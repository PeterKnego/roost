pub mod config;
pub mod gitio;
pub mod http;
pub mod origin;
pub mod projects;
pub mod proto;
pub mod render;
pub mod routes;
pub mod term;
pub mod workspace;
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
                term::handle_ws(stream, &roots);
            } else {
                routes::handle(stream, &roots);
            }
        });
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
