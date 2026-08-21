//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
use crate::hub::{ConnId, Hub};
use crate::proto;
use crate::workspace::Buffer;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{accept_hdr_with_config, Message, WebSocket};

/// tungstenite defaults to a 64 MiB max message; an `EditBuffer` intent is
/// capped at `workspace::MAX_TEXT_BYTES` of *text*, but the frame carrying
/// it is JSON (the text gets escaped, plus the envelope), so the protocol
/// ceiling needs headroom above that — this is a coarse backstop against an
/// oversized frame being buffered at all, not the precise limit. The
/// precise, friendly-error limit is enforced in `workspace::apply_layout`.
const MAX_FRAME_BYTES: usize = crate::workspace::MAX_TEXT_BYTES * 4;

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
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    let accepted = accept_hdr_with_config(
        stream,
        |req: &WsRequest, resp: WsResponse| {
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("resh: rejected workspace ws origin={origin:?} (set allowed_origins)");
                let body = Some("origin not allowed".to_string());
                return Err(tungstenite::http::Response::builder()
                    .status(403)
                    .body(body)
                    .expect("static 403 response"));
            }
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws_read) = accepted else { return };

    // Obtained *before* any hub lock is taken, and the registry lock inside
    // for_project is released before this call returns: a socket thread must
    // never hold a hub lock while acquiring the registry lock, or two threads
    // opening different projects could deadlock on each other's locks.
    let hub: Arc<Mutex<Hub>> = Hub::for_project(project, dir);
    let (id, rx, dir, open_buffers) = {
        let mut h = Hub::lock(&hub);
        let (id, rx) = h.subscribe();
        // Subscribing and sending this connection's own initial snapshot
        // happen inside one lock acquisition, not two: releasing the lock in
        // between would let another connection's broadcast land in this
        // subscriber's channel first (subs.send is fifo per-channel, but
        // *which* message gets sent first across threads depends on lock
        // order, not send order). A client that received a foreign
        // connection's State first would latch that id as its own origin
        // and then silently drop that peer's own BufferText forever.
        let ev = h.snapshot_event(&id);
        h.send_to(&id, &ev);
        // Replay the whole store to a fresh client: a notice raised while no
        // browser was open is exactly the case this feature exists for. Sent
        // inside the same lock acquisition as the snapshot, for the reason
        // the existing comment above gives — releasing in between lets a
        // foreign broadcast land first.
        let ev = proto::Event::Notices { list: crate::notify::list() };
        h.send_to(&id, &ev);
        // State is metadata-only — it never carries buffer text — so a
        // client reconnecting onto a layout with open Edit buffers would
        // otherwise render them blank forever: nothing else re-sends
        // BufferText for a buffer that isn't actively being typed into.
        // A clean buffer holds nothing of its own, so its half of this
        // replay has to come from disk — but not under this lock: a project
        // can have up to MAX_BUFFERS open buffers, and `replay_text`'s disk
        // read is up to 2 MB each, which would stall the watcher thread and
        // every other connection to this project for the whole scan. Only
        // the rels and their `Buffer`s are collected here (cheap — a clean
        // one holds nothing and an edited one is the String this same
        // replay would clone anyway); the disk reads happen after the lock
        // below is dropped.
        let dir = h.dir.clone();
        let open_buffers: Vec<(String, Buffer)> =
            h.ws.buffers.iter().map(|(rel, b)| (rel.clone(), b.clone())).collect();
        (id, rx, dir, open_buffers)
    };
    // Disk reads happen with the hub lock already dropped: replay_text only
    // touches disk for a clean buffer (an edited one returns its own text
    // with no I/O), but a project can have up to MAX_BUFFERS of them.
    let resolved: Vec<(String, Option<String>)> = open_buffers
        .iter()
        .map(|(rel, b)| (rel.clone(), replay_text(b, &dir, rel)))
        .collect();
    {
        let mut h = Hub::lock(&hub);
        for (rel, text) in resolved {
            // A failed read (too large, binary, or a transient I/O error —
            // read_text_file refuses those as policy, not just on real I/O
            // failure) must not become "the file is empty": that text would
            // reach the browser as a real, editable buffer, and a save from
            // it would write "" over a file that was never actually empty.
            // Sending nothing here is recoverable — the next SetMode/OpenTab
            // re-reads the file — where a blanked editor is not.
            let Some(text) = text else { continue };
            // The buffer may have closed (or the connection itself gone)
            // while its file was being read with no lock held; resending its
            // text now would resurrect a buffer the client already closed
            // instead of leaving it gone.
            if !h.ws.buffers.contains_key(&rel) {
                continue;
            }
            let ev = proto::Event::BufferText { rel, text, origin: String::new() };
            h.send_to(&id, &ev);
        }
    }
    // Guards the subscription from here on: if we return early (the
    // try_clone below fails) or the read loop below panics, this still runs.
    let unsub = UnsubGuard { hub: hub.clone(), id: id.clone() };

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);

    // Drains the subscriber channel outside any hub lock: the channel recv
    // blocks indefinitely between events, and blocking while holding the hub
    // lock would stall every other connection to this project.
    // recv_timeout rather than recv, for the reason term.rs's writer gives:
    // without a periodic write nothing ever discovers a peer that vanished
    // without TCP noticing. It matters more here than there — `subscribe`
    // hands out an *unbounded* channel, so a subscriber nobody drains
    // accumulates every broadcast this project makes for the life of the
    // process, where a terminal subscriber's bounded queue at least fills and
    // drops itself once the session produces output.
    let ping_every = crate::config::ping_interval();
    let writer = std::thread::spawn(move || {
        loop {
            let out = match rx.recv_timeout(ping_every) {
                Ok(msg) => Message::Text(msg.into()),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Message::Ping(Vec::new().into()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if ws_write.send(out).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    loop {
        match ws_read.read() {
            Ok(Message::Text(t)) => {
                let dirty = {
                    let mut h = Hub::lock(&hub);
                    match proto::decode(&t) {
                        Ok(intent) => h.handle(&id, intent),
                        Err(e) => {
                            let ev = proto::Event::Error { msg: e };
                            h.send_to(&id, &ev);
                        }
                    }
                    std::mem::take(&mut h.notices_dirty)
                };
                // Outside the block above, so this hub's lock is released:
                // broadcast_all locks every hub including this one, and a
                // Mutex is not reentrant.
                if dirty {
                    crate::hub::broadcast_all(&proto::Event::Notices {
                        list: crate::notify::list(),
                    });
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

/// What to replay for one open buffer on connect/reconnect, or nothing.
///
/// `None` on a failed read is deliberate, not an oversight: `read_text_file`
/// refuses a file as *policy* — too large, or a NUL byte marking it binary —
/// not only on genuine I/O failure, and a clean buffer's file can cross
/// either threshold after the buffer was already opened. Collapsing that
/// into `Some(String::new())` would hand the browser an empty-but-editable
/// textarea for a file that was never actually empty; the next keystroke or
/// an unconditional `pushEdit` save would then write "" over it, straight
/// past `do_save`'s clean-buffer guard, because the buffer really is dirty
/// by then. Sending nothing for this `rel` is recoverable — the next
/// `SetMode`/`OpenTab` re-reads the file — where a blanked editor is not.
fn replay_text(b: &Buffer, dir: &Path, rel: &str) -> Option<String> {
    match b.edited_text() {
        Some(t) => Some(t.to_string()),
        None => crate::projects::safe_resolve(dir, rel)
            .and_then(|p| crate::projects::read_text_file(&p))
            .ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Content;

    #[test]
    fn an_edited_buffer_replays_its_own_text_without_touching_disk() {
        let b = Buffer { content: Content::Edited("unsaved\n".into()), ..Buffer::default() };
        // A directory that does not exist as a project root: if this read
        // disk at all, safe_resolve would fail on it and the test would
        // still (accidentally) pass with None — the assertion on Some below
        // is what actually catches that regression.
        let dir = PathBuf::from("/nonexistent/does-not-exist");
        assert_eq!(replay_text(&b, &dir, "a.rs"), Some("unsaved\n".to_string()));
    }

    #[test]
    fn a_clean_buffer_whose_file_reads_fine_replays_the_disk_text() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "on disk\n").unwrap();
        let b = Buffer::default(); // Content::Clean
        assert_eq!(replay_text(&b, d.path(), "a.rs"), Some("on disk\n".to_string()));
    }

    /// The case the reviewer's Critical was about: a clean buffer whose file
    /// `read_text_file` refuses. Constructed the way that refusal is really
    /// reached in production — a NUL byte, which read_text_file treats as
    /// "this is binary" — not a permissions trick, since that path is
    /// equally real but this one is the one `file_changed_externally` can
    /// actually leave behind (it keeps a buffer clean and updates base_hash
    /// using plain `read_to_string`, so a later binary write is invisible to
    /// it until this replay tries to read the file back).
    #[test]
    fn a_clean_buffer_whose_file_is_refused_replays_nothing() {
        let d = tempfile::tempdir().unwrap();
        // Not on TEXT_EXTENSIONS's allow-list, or the NUL-byte sniff in
        // read_text_file is skipped and this fixture would not actually
        // exercise the refusal this test is about.
        std::fs::write(d.path().join("a.bin"), b"a\0b").unwrap();
        let b = Buffer::default(); // Content::Clean
        assert_eq!(
            replay_text(&b, d.path(), "a.bin"),
            None,
            "a refused read must not become an empty, editable buffer"
        );
    }
}
