//! Terminal websocket: bridges a browser tab to a PTY running
//! `zellij attach --create {project}`. One connection = one zellij client;
//! zellij owns all session state. Two pump directions over one TcpStream:
//! tungstenite over try_clone'd halves (frames are independent per direction).
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

pub fn handle_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut path = String::new();
    // WebSocket handshakes bypass the same-origin policy: without this check any
    // page the user visits can open this socket and get a shell. See spec §Security.
    let allowed = crate::config::allowed_origins();
    let accepted = accept_hdr(stream, |req: &WsRequest, resp: WsResponse| {
        path = req.uri().path().to_string();
        let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
        if !crate::origin::origin_allowed(origin, &allowed) {
            eprintln!("deadlight: rejected ws origin={origin:?} (set allowed_origins)");
            let body = Some("origin not allowed".to_string());
            return Err(tungstenite::http::Response::builder()
                .status(403)
                .body(body)
                .expect("static 403 response"));
        }
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    let project = match path.strip_prefix("/ws/") {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            let _ = ws_read.close(None);
            return;
        }
    };
    let Some(dir) = crate::projects::resolve_project(roots, &project) else {
        let _ = ws_read.close(None);
        return;
    };

    let cmd: Vec<String> = match std::env::var("DEADLIGHT_CMD") {
        Ok(c) => c.split_whitespace().map(String::from).collect(),
        Err(_) => vec!["zellij".into(), "attach".into(), "--create".into(), project.clone()],
    };
    if cmd.is_empty() {
        let _ = ws_read.close(None);
        return;
    }

    let pty = native_pty_system();
    let Ok(pair) = pty.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
    else {
        return;
    };
    let mut cb = CommandBuilder::new(&cmd[0]);
    cb.args(&cmd[1..]);
    cb.cwd(&dir);
    cb.env("TERM", "xterm-256color");
    let Ok(mut child) = pair.slave.spawn_command(cb) else { return };
    drop(pair.slave);
    let cleanup = |child: &mut Box<dyn portable_pty::Child + Send + Sync>| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let Ok(mut pty_reader) = pair.master.try_clone_reader() else {
        cleanup(&mut child);
        return;
    };
    let Ok(mut pty_writer) = pair.master.take_writer() else {
        cleanup(&mut child);
        return;
    };
    let master = pair.master;

    let Ok(write_half) = ws_read.get_ref().try_clone() else {
        cleanup(&mut child);
        return;
    };
    // Known accepted risk: tungstenite auto-queues Pongs on the read socket, so a
    // ping-originating client could interleave a Pong mid-frame with out_thread's
    // writes on the cloned socket. Browsers cannot send Pings and tailscale serve
    // does not originate them; worst case is one corrupted frame + auto-reconnect.
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);

    // PTY -> browser
    let out_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_write.send(Message::Binary(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    // browser -> PTY (this thread)
    loop {
        match ws_read.read() {
            Ok(Message::Binary(b)) => {
                if pty_writer.write_all(&b).is_err() {
                    break;
                }
            }
            Ok(Message::Text(t)) => {
                if let Some(sz) = t.strip_prefix("resize:") {
                    if let Some((c, r)) = sz.split_once('x') {
                        if let (Ok(cols), Ok(rows)) = (c.parse(), r.parse()) {
                            let _ = master.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    // browser gone: kill our zellij client (detach); the session survives
    let _ = child.kill();
    let _ = child.wait();
    let _ = out_thread.join();
}
