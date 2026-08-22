pub mod assets;
pub mod cli;
pub mod config;
pub mod fileops;
pub mod gitio;
pub mod hub;
pub mod http;
pub mod ide;
pub mod idecwd;
pub mod idelock;
pub mod notify;
pub mod origin;
pub mod osc;
pub mod paste;
pub mod projects;
pub mod proto;
pub mod registry;
pub mod render;
pub mod routes;
pub mod screen;
pub mod session;
pub mod term;
pub mod textdiff;
pub mod upload;
pub mod watch;
pub mod workspace;
pub mod worktree;
pub mod wsconn;
pub mod wsio;
pub mod wsstate;

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

pub fn serve(listener: TcpListener, roots: Vec<PathBuf>) {
    // Notices raised while no browser was connected are the point of the
    // store; load them before anything can connect.
    crate::notify::load();
    // Sessions outlive resh, so the registry must be rebuilt from disk
    // and live processes rather than assumed empty.
    let report = registry::reconcile(&roots);
    if report.dead_sockets > 0 || report.gone_projects > 0 {
        eprintln!(
            "resh: startup reap — {} dead sockets, {} sessions for missing projects",
            report.dead_sockets, report.gone_projects
        );
    }
    // An explicit operator setting, so silence here would look like "my edits
    // do nothing". The optional user directory is different: absent is normal
    // and says nothing, so it warns about neither.
    if let Some(d) = std::env::var_os("RESH_STATIC") {
        let p = std::path::Path::new(&d);
        if !p.is_dir() {
            eprintln!(
                "resh: RESH_STATIC={} is not a readable directory; serving embedded assets",
                p.display()
            );
        }
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let roots = roots.clone();
        std::thread::spawn(move || {
            if is_ws(&stream) {
                route_ws(stream, &roots);
            } else {
                routes::handle(stream, &roots);
            }
        });
    }
}

/// `/ws/{project}/_workspace` and `/ws/{project}/term/{name}` are peeked
/// apart here so each gets its own handler; both re-check Origin themselves.
fn route_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut buf = [0u8; 512];
    // Poll like is_ws does, but wait for the whole request line (CRLF), not
    // just a fixed byte count: a short peek can land mid-path (e.g.
    // "GET /ws/proj/_worksp"), truncating "_workspace" and silently
    // misrouting the connection to the wrong handler.
    let mut n = 0usize;
    for _ in 0..50 {
        match stream.peek(&mut buf) {
            Ok(k) => {
                n = k;
                if buf[..n].windows(2).any(|w| w == b"\r\n") || n == buf.len() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return,
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let Some(target) = head.split_whitespace().nth(1) else { return };
    let segs: Vec<&str> =
        target.trim_start_matches("/ws/").split('/').filter(|s| !s.is_empty()).collect();
    // The project identifier can now be a nested rel path (e.g.
    // /ws/karpie/src/_workspace), so it's no longer just `segs[0]` — split
    // from the right off the fixed trailing marker instead, the same way
    // routes.rs's frag route does. This can misfire only if a real project
    // directory is itself named "_workspace" at exactly this position;
    // unlike static/ws/frag (checked by projects::RESERVED because they sit
    // on the plain-HTTP URL surface too), "_workspace" isn't reserved
    // there, since it belongs only to this websocket surface.
    let is_workspace_request =
        segs.len() >= 2 && segs[segs.len() - 1] == "_workspace";
    if is_workspace_request {
        let project = segs[..segs.len() - 1].join("/");
        if let Some(dir) = projects::resolve_project(roots, &project) {
            return wsconn::handle(stream, &project, dir);
        }
        return;
    }
    // Anything else (including a well-formed .../term/{name}) is
    // term::handle_ws's own job to parse and validate for real; this
    // function's only other responsibility is routing _workspace.
    term::handle_ws(stream, roots);
}

/// Peek the first bytes without consuming them: websocket requests go to
/// tungstenite with the request intact; everything else to the HTTP parser.
fn is_ws(stream: &TcpStream) -> bool {
    let mut buf = [0u8; 8];
    for _ in 0..50 {
        match stream.peek(&mut buf) {
            Ok(n) if n >= 8 => return &buf[..8] == b"GET /ws/",
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => return false,
        }
    }
    false
}
