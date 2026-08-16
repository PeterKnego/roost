//! Terminal websocket: one connection = one attachment to a session in
//! `session`. The session owns the PTY and outlives this connection.
use crate::session;
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
            return Err(tungstenite::http::Response::builder()
                .status(403)
                .body(Some("origin not allowed".to_string()))
                .expect("static 403"));
        }
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    // /ws/{project}/term/{name}
    let rest = path.trim_start_matches("/ws/");
    let segs: Vec<&str> = rest.split('/').collect();
    let (Some(project), Some(&"term"), Some(name)) =
        (segs.first().copied(), segs.get(1), segs.get(2).copied())
    else {
        let _ = ws_read.close(None);
        return;
    };
    let Some(dir) = crate::projects::resolve_project(roots, project) else {
        let _ = ws_read.close(None);
        return;
    };
    let att = match session::attach(project, name, &dir) {
        Ok(a) => a,
        Err(_) => {
            let _ = ws_read.close(None);
            return;
        }
    };

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);
    let rx = att.rx;
    let out = std::thread::spawn(move || {
        while let Ok(chunk) = rx.recv() {
            if ws_write.send(Message::Binary(chunk.into())).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

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
