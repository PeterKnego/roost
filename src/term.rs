//! Terminal websocket: one connection = one attachment to a session in
//! `session`. The session owns the PTY and outlives this connection.
use crate::session;
use std::net::TcpStream;
use std::path::PathBuf;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{accept_hdr_with_config, Message, WebSocket};

/// See wsconn.rs's identical constant for the rationale: bound below
/// tungstenite's 64 MiB default so an oversized frame is refused at the
/// protocol layer rather than buffered, with headroom above the 2 MB text
/// cap for framing overhead. This socket never carries file text, but there
/// is no reason its cap should be looser than the one that does.
const MAX_FRAME_BYTES: usize = crate::workspace::MAX_TEXT_BYTES * 4;

pub fn handle_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut path = String::new();
    // WebSocket handshakes bypass the same-origin policy: without this check any
    // page the user visits can open this socket and get a shell. See spec §Security.
    let allowed = crate::config::allowed_origins();
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    let accepted = accept_hdr_with_config(
        stream,
        |req: &WsRequest, resp: WsResponse| {
            path = req.uri().path().to_string();
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("resh: rejected ws origin={origin:?} (set allowed_origins)");
                return Err(tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("origin not allowed".to_string()))
                    .expect("static 403"));
            }
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws_read) = accepted else { return };

    // /ws/{project}/term/{name} — {project} may itself be multi-segment
    // (a nested rel path), so split from the right off the two fixed
    // trailing segments ("term", {name}) rather than assuming project is
    // segs[0], same rationale as lib.rs's route_ws for `_workspace`.
    let rest = path.trim_start_matches("/ws/");
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 || segs[segs.len() - 2] != "term" {
        let _ = ws_read.close(None);
        return;
    }
    let name = segs[segs.len() - 1];
    let project = segs[..segs.len() - 2].join("/");
    // Every failure below closes the socket, which the client sees only as a
    // terminal that never starts — no error text reaches the UI, because a
    // closed socket carries none. So each one must say why on the way out, or
    // a user whose terminal silently refuses to start (and whoever reads
    // `journalctl --user -u resh` afterwards) has nothing at all to go
    // on. Diagnosing an intermittent "live_sessions stayed empty" needed
    // exactly this and did not have it.
    let Some(dir) = crate::projects::resolve_project(roots, &project) else {
        eprintln!("resh: term socket refused — project {project:?} does not resolve under the roots");
        let _ = ws_read.close(None);
        return;
    };
    // A close in flight is SIGKILLing every one of this project's sessions on
    // a background thread, and `kill_and_unlink` kills whatever holds a
    // socket path — including a session spawned *after* it took its process
    // snapshot. `hub::do_start_terminal` refuses the `StartTerminal` intent
    // for that reason, but that only stops a browser that asks first: this
    // connect is what actually spawns the PTY, and a mirrored tab already
    // showing a terminal reconnects straight here with no intent at all. So
    // the same guard has to exist on this path.
    //
    // Not airtight, and cannot be from here: a close starting in the moment
    // between this check and `attach` below still races. Closing that
    // properly would mean holding the hub lock across `attach` (a PTY spawn
    // — blocking I/O under a lock, which CLAUDE.md forbids outright). What
    // remains is a microsecond-scale window instead of the whole ~100ms+
    // kill, and the losing case degrades to what it already was.
    //
    // Safe to touch a hub lock *here*, unlike the refresh block further down
    // (see its placement comment): no subscriber exists until `attach`
    // returns, so waiting on a busy hub can only delay this terminal's own
    // start — there is no queue yet that could fill and get this tab dropped
    // mid-connect.
    if crate::hub::Hub::is_closing(&project) {
        eprintln!("resh: term socket refused — project {project:?} is closing");
        let _ = ws_read.close(None);
        return;
    }
    // Must run before `attach`, not after: `attach` builds the spawned
    // shell's environment (`session::session_env`) from
    // `ide::port_for(project)`, which on a brand-new project's first-ever
    // terminal has nothing to find yet — the only other thing that starts
    // the ide listener is `Hub::for_project`, and there is no earlier
    // `_workspace` connection to have called that already. Caught by driving
    // a real `claude` through this exact path (see this task's report): the
    // very first shell spawned in a fresh project came up with no
    // `CLAUDE_CODE_SSE_PORT` at all.
    //
    // Calls `ide::for_project` directly rather than the full
    // `Hub::for_project` (which also still runs, unchanged, further down):
    // `ide`'s registry is deliberately its own, separate from the hub's (see
    // its doc comment) precisely so a caller can guarantee the listener
    // exists without paying for `Hub::new`'s own blocking I/O (wsstate load,
    // buffer reconciliation) too. That distinction is load-bearing, not
    // just an optimisation — an earlier version of this fix called the full
    // `Hub::for_project` here and broke five unrelated integration tests
    // that open this websocket and immediately assume `attach` has already
    // run server-side (e.g. an immediately-following `POST /paste`, which
    // got a genuine "no such session" 404). That race already existed
    // before this task (the handshake completes, and so the client's
    // connect call returns, before `attach` is ever called) — it was just
    // narrow enough not to lose in practice. `Hub::new`'s I/O was enough
    // extra width to lose it routinely under load; `ide::for_project`
    // alone (TCP bind, token, one small file write) was not.
    //
    // **Not redundant with `Hub::for_project`, and deleting it silently kills
    // IDE integration for every reopened project.** The comment above frames
    // this as a first-terminal race, which reads like something the hub's own
    // call already covers once a `_workspace` socket exists. It does not, past
    // the first close: `CloseProject` calls `ide::stop`, which removes the
    // *ide* registry entry, but nothing ever removes the project from
    // `hub::REGISTRY` — so `Hub::for_project`'s `or_insert_with` closure, the
    // only other caller of `ide::for_project`, can never run again for that
    // project for the life of the process. After one close this line is the
    // *only* path that can rebuild the listener, and the failure is invisible:
    // shells still spawn, they just spawn with no `CLAUDE_CODE_SSE_PORT` and
    // no lock file, so `claude` silently comes up with no IDE at all.
    // `tests/integration.rs::reopening_a_closed_project_rebuilds_its_ide_listener`
    // is what fails if this goes.
    crate::ide::for_project(&project, dir.clone());
    let att = match session::attach(&project, name, &dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("resh: term socket refused — attach {project}/{name} failed: {e}");
            let _ = ws_read.close(None);
            return;
        }
    };
    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);
    let rx = att.rx;
    // recv_timeout rather than recv: a session can be silent for hours, and
    // this thread is the only one that ever writes to this socket. Without a
    // periodic write, a peer that vanished without TCP noticing (a slept
    // laptop, a moved network) is never discovered — the read loop below
    // blocks forever, and the attachment goes on holding a `sizes` entry that
    // clamps the PTY to its dead geometry for every live client, since
    // `min_geometry` takes the smallest attachment. A failed write is
    // positive evidence the peer is gone; silence is evidence of nothing,
    // which is why nothing here times an attachment out on suspicion.
    let ping_every = crate::config::ping_interval();
    let out = std::thread::spawn(move || {
        loop {
            let msg = match rx.recv_timeout(ping_every) {
                Ok(chunk) => Message::Binary(chunk.into()),
                // Browsers answer a Ping without involving page JavaScript,
                // and the reply lands in the read loop below, which already
                // ignores frames it has no use for.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Message::Ping(Vec::new().into()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if ws_write.send(msg).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    // `StartTerminal` (hub.rs) refreshes `live_sessions` before the PTY
    // actually exists — the real spawn happens here, via `attach` — so
    // without this, every mirrored browser (and the tab's own client, on
    // its next `State`) would keep seeing "not live" until some unrelated
    // intent happened to touch the hub. No later task wires attach back to
    // the hub, so this connection does it directly: look the hub up
    // (`for_project` does create the Hub and can start the project's
    // filesystem watcher if this happens to be the first connection, but
    // that setup runs off-thread, so it doesn't block here), lock it just
    // long enough to refresh and broadcast, then release before entering
    // the blocking read loop below.
    //
    // Placement matters: this must run *after* `out` is spawned, not
    // before. `out` is what drains this connection's own subscriber
    // channel (`att.rx`, a bounded 64-slot queue); a hub critical section
    // can legitimately run for seconds (the `gitio` calls under the hub
    // lock carry a 15s deadline), and nothing else reads that channel. If
    // this block ran first, output arriving on a busy session (e.g. a
    // build or Claude streaming) plus the scrollback replay could fill the
    // queue while this waited on the hub mutex; the pump thread's
    // `retain(|_, tx| tx.try_send(..).is_ok())` permanently drops any
    // subscriber whose queue fills, killing this tab's socket before it
    // ever started reading. With `out` already running first, the channel
    // is being drained the whole time this contends for the hub lock.
    {
        let hub = crate::hub::Hub::for_project(&project, dir.clone());
        let mut h = crate::hub::Hub::lock(&hub);
        h.refresh_live_sessions();
        let ev = h.snapshot_event(&String::new());
        h.broadcast(&ev);
    }

    loop {
        match ws_read.read() {
            Ok(Message::Binary(b)) => {
                if session::write_input(&att.key, &b).is_err() {
                    break;
                }
            }
            Ok(Message::Text(t)) => {
                if let Some(sz) = t.strip_prefix("resize:") {
                    if let Some((c, r)) = sz.split_once('x') {
                        if let (Ok(cols), Ok(rows)) = (c.parse(), r.parse()) {
                            session::resize(&att.key, att.id, cols, rows);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    session::detach(&att.key, att.id); // detach only; the session survives
    let _ = out.join();
}
