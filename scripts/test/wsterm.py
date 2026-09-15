#!/usr/bin/env python3
"""Open a roost terminal over a real websocket and type one command into it.

`container.sh` needs to prove that a PTY opens inside the container with no
capabilities, and the only way to start a terminal is the websocket — which is
the point: it is the surface that spawns a shell, and a test that reached the
shell some other way would be testing something roost does not do.

Hand-rolled RFC 6455 rather than a dependency, because this runs from a shell
script whose only other requirement is python3, and the client half of the
protocol needed here is one masked frame.

Two things are load-bearing and neither is obvious:

* **The workspace socket is opened first, and kept open.** Connecting to
  `/ws/{project}/term/{name}` only *attaches*; it spawns a shell only when a
  reservation exists, and reservations are placed by `Hub::new` — which happens
  when the workspace socket connects. Without it this exits cleanly having
  started nothing.
* **Origin is sent.** Every browser-facing socket refuses a handshake that
  carries none, deliberately (CLAUDE.md), so a client that omitted it would be
  testing the refusal rather than the terminal.

Usage: wsterm.py <host:port> <project> <shell command>
"""
import base64
import os
import socket
import sys
import time


def handshake(hostport, path):
    host, port = hostport.split(":")
    s = socket.create_connection((host, int(port)), timeout=15)
    key = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {hostport}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Origin: http://{hostport}\r\n"
        "\r\n"
    )
    s.sendall(req.encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = s.recv(4096)
        if not chunk:
            raise SystemExit(f"handshake for {path}: connection closed with no reply")
        buf += chunk
    if b" 101 " not in buf.splitlines()[0]:
        raise SystemExit(f"handshake for {path} refused: {buf.splitlines()[0]!r}")
    return s


def send(sock, payload, opcode):
    """One masked client frame. `opcode` is 1 for text, 2 for binary.

    The distinction is not cosmetic and cost this script its first run:
    `term.rs` reads **Binary** frames as keystrokes and **Text** frames as the
    resize channel (`resize:<cols>x<rows>`), so a command sent as text is
    silently dropped — no error, no keystrokes, and a session that looks
    perfectly healthy because it is.
    """
    n = len(payload)
    header = bytearray([0x80 | opcode])  # FIN + opcode
    mask_bit = 0x80
    if n < 126:
        header.append(mask_bit | n)
    elif n < (1 << 16):
        header.append(mask_bit | 126)
        header += n.to_bytes(2, "big")
    else:
        header.append(mask_bit | 127)
        header += n.to_bytes(8, "big")
    mask = os.urandom(4)
    header += mask
    sock.sendall(bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))


def main():
    if len(sys.argv) < 4:
        raise SystemExit("usage: wsterm.py <host:port> <project> <command>")
    hostport, project, command = sys.argv[1], sys.argv[2], " ".join(sys.argv[3:])
    ws = handshake(hostport, f"/ws/{project}/_workspace")
    # The hub is built on connect; the terminal reservation comes with it.
    time.sleep(1.0)
    term = handshake(hostport, f"/ws/{project}/term/term")
    # A real size before anything is typed. A browser sends one immediately and
    # a shell in a 0x0 terminal is a poor thing to reason about.
    send(term, b"resize:120x40", 1)
    # The shell needs to reach its first prompt before it will read a line.
    time.sleep(1.5)
    send(term, (command + "\r").encode(), 2)
    # Long enough for the shell to run it and for the effect to be on disk.
    time.sleep(2.5)
    term.close()
    ws.close()
    print("ok")


if __name__ == "__main__":
    main()
