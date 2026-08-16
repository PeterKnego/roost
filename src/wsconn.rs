//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
use crate::hub::Hub;
use crate::proto;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

pub fn handle(stream: TcpStream, project: &str, dir: PathBuf) {
    // WebSocket handshakes bypass the same-origin policy: without this check
    // any page the user visits can drive this socket, and this socket can
    // write files. See spec §Security and src/term.rs's identical check.
    let allowed = crate::config::allowed_origins();
    let accepted = accept_hdr(stream, |req: &WsRequest, resp: WsResponse| {
        let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
        if !crate::origin::origin_allowed(origin, &allowed) {
            eprintln!("deadlight: rejected workspace ws origin={origin:?} (set allowed_origins)");
            let body = Some("origin not allowed".to_string());
            return Err(tungstenite::http::Response::builder()
                .status(403)
                .body(body)
                .expect("static 403 response"));
        }
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    // Obtained *before* any hub lock is taken, and the registry lock inside
    // for_project is released before this call returns: a socket thread must
    // never hold a hub lock while acquiring the registry lock, or two threads
    // opening different projects could deadlock on each other's locks.
    let hub: Arc<Mutex<Hub>> = Hub::for_project(project, dir);
    let (id, rx) = {
        let mut h = Hub::lock(&hub);
        h.subscribe()
    };

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);

    // Drains the subscriber channel outside any hub lock: the channel recv
    // blocks indefinitely between events, and blocking while holding the hub
    // lock would stall every other connection to this project.
    let writer = std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            if ws_write.send(Message::Text(msg.into())).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    // Send the current state immediately so a fresh tab renders without asking.
    {
        let mut h = Hub::lock(&hub);
        let ev = h.snapshot_event(&id);
        h.send_to(&id, &ev);
    }

    loop {
        match ws_read.read() {
            Ok(Message::Text(t)) => {
                let mut h = Hub::lock(&hub);
                match proto::decode(&t) {
                    Ok(intent) => h.handle(&id, intent),
                    Err(e) => {
                        let ev = proto::Event::Error { msg: e };
                        h.send_to(&id, &ev);
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }

    {
        let mut h = Hub::lock(&hub);
        h.unsubscribe(&id);
    }
    let _ = writer.join();
}
