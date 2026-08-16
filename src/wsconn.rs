//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
use crate::hub::{ConnId, Hub};
use crate::proto;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

/// Unsubscribes on drop, not just on the happy path: if `Hub::handle` ever
/// panics, unwinding runs this instead of the tail of `handle` below, so the
/// subscriber's `Sender` still leaves `hub.subs`. Without it a panic mid-loop
/// would leave the writer thread's `rx.recv()` blocked forever on a sender
/// nobody reads for anymore — a zombie half-connection that accumulates.
struct UnsubGuard {
    hub: Arc<Mutex<Hub>>,
    id: ConnId,
}

impl Drop for UnsubGuard {
    fn drop(&mut self) {
        Hub::lock(&self.hub).unsubscribe(&self.id);
    }
}

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
    // Guards the subscription from here on: if we return early (the
    // try_clone below fails) or the read loop below panics, this still runs.
    let unsub = UnsubGuard { hub: hub.clone(), id: id.clone() };

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

    // Unsubscribe *before* joining: the writer's rx.recv() only returns once
    // this connection's Sender is gone from hub.subs, and that removal is
    // what UnsubGuard::drop performs. Joining first would deadlock waiting
    // for a thread that is itself waiting on us.
    drop(unsub);
    let _ = writer.join();
}
