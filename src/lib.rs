pub mod config;
pub mod fileops;
pub mod gitio;
pub mod hub;
pub mod http;
pub mod origin;
pub mod projects;
pub mod proto;
pub mod render;
pub mod routes;
pub mod session;
pub mod term;
pub mod watch;
pub mod workspace;
pub mod wsconn;
pub mod wsstate;

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

pub fn serve(listener: TcpListener, roots: Vec<PathBuf>) {
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
    let segs: Vec<&str> = target.trim_start_matches("/ws/").split('/').collect();
    let Some(project) = segs.first().copied().filter(|s| !s.is_empty()) else { return };
    let Some(dir) = projects::resolve_project(roots, project) else { return };
    match segs.get(1).copied() {
        Some("_workspace") => wsconn::handle(stream, project, dir),
        _ => term::handle_ws(stream, roots),
    }
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
