# resh — one writer per socket

Closes a frame-splicing defect on the workspace and terminal sockets: the read
thread and the writer thread each hold a `WebSocket` over a `try_clone`d
descriptor of the same connection, and tungstenite writes control-frame replies
from *whichever* object read them. A reply written while the writer thread is
mid-frame splices into that frame.

`src/ide.rs` already avoids this by having exactly one writer; its module doc
names these two sites as carrying the same shape. This is that follow-up.

## What actually triggers it

Worth stating precisely, because it decides how much the fix may cost.

tungstenite 0.24 queues a reply on `read()` for exactly two inbound control
frames (`protocol/mod.rs`):

| Inbound | Queues a write? | Returned to the caller? |
|---|---|---|
| `Ping` (`:605-611`) | yes — `set_additional(Frame::pong(..))` | yes, `Message::Ping` |
| `Close` (`:601`, `:693`) | yes — `set_additional(reply)` | yes, `Message::Close` |
| `Pong` (`:613`) | **no** | yes, `Message::Pong` |

and flushes the queue at the top of the next `read()` (`:395`).

**resh is the pinger.** Both writer threads send `Message::Ping` on a
`recv_timeout` tick (`wsconn.rs:193`, `term.rs:162`), and `term.rs`'s own
comment records that browsers answer without involving page JavaScript. So the
frames these read loops receive are *Pongs*, which queue nothing.

That leaves two real triggers:

- **A `Close` at teardown**, which is routine — every closed tab sends one. The
  spliced frame is the last one the peer sees, so in practice the browser is
  discarding it anyway. This is the common case and it is nearly harmless.
- **A `Ping` from a client that chose to send one.** The browser JavaScript API
  cannot, but these sockets accept any loopback `Origin`, so a local process can
  connect and ping deliberately — repeatedly, mid-frame, to corrupt terminal
  output or workspace events at will.

So: low severity, not zero, and the second trigger is a choice someone else
makes rather than a race resh loses on its own.

## Why the `ide.rs` fix does not transplant

`src/ide.rs` collapsed to one thread with a 200 ms read-timeout poll. That works
there because the traffic is low-volume request/response.

It cannot be copied here. A terminal's writer pumps PTY output continuously, and
a poll interval becomes added latency on every keystroke echo — 200 ms is
unusable, and a 10 ms poll trades a teardown-time defect for ~100 wakeups per
second per terminal, forever. The workspace socket has the same problem in
milder form: mirrored typing would lag the poll.

The property that matters is not "one thread". It is **no two writers may be
inside a frame at once**.

## What changes

Two threads stay. Two changes make the single-writer property structural.

**1. The read half stops being able to write at all.** Its `WebSocket` is built
over a wrapper that delegates `read` to the socket and discards `write`/`flush`.
tungstenite's auto-replies are then queued, "written" into nothing, and dropped.
Nothing else ever writes from that object — the read loops never call `send`.

**2. The reply the read half owed is sent by the writer instead.** Both `Ping`
and `Close` are handed back to the caller as well as queued, so the read loop
already sees everything it needs; it forwards the reply through the same
`WebSocket` the writer thread uses, which lives behind an
`Arc<Mutex<WebSocket<TcpStream>>>`.

The mutex is held across a whole `send()`, not across individual byte writes —
that is the point. These sockets are blocking, so `send()` queues and flushes
one frame before returning, and a frame therefore cannot be interrupted by the
other thread. Locking the underlying stream per `write` call would *not* be
enough: tungstenite's `WriteBuffer` can drain a frame in several `write` calls,
and a lock at that granularity still permits a splice between them.

Cost: one uncontended mutex acquisition per PTY chunk and per workspace event.
No added latency, no idle wakeups, no change to either loop's shape.

## What this does not do

- **It does not touch `src/ide.rs`.** That socket is already single-writer, and
  its 200 ms poll is right for its traffic.
- **It does not make the sockets non-blocking**, add a poll interval, or
  introduce an async runtime.
- **It does not change the ping cadence** or `config::ping_interval`.

## The trap in testing this

A test that sends a `Ping` and checks the connection still works proves nothing:
the splice needs the writer to be *mid-frame* when the reply is written, so a
quiet socket passes with the bug fully present. Two properties are testable
without racing:

- **Structural**: the read half cannot write. Give it a wrapper that *records*
  instead of discarding, drive a `Ping` and a `Close` through the read loop, and
  assert nothing was written by that object — that fails against today's code,
  which writes the pong there.
- **Behavioural**: a `Ping` still gets a `Pong`, and a `Close` still gets a
  `Close`, over the real socket. That fails if the forwarding is dropped, which
  is what the structural change would otherwise silently break.

The interleaving itself stays untested, and this document says so rather than
implying a green suite covers it.
