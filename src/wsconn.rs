//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
use crate::hub::{ConnId, Hub};
use crate::proto;
use crate::workspace::{Buffer, Content};
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
    let (id, rx, dir, snapshot) = {
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
        // can have up to MAX_BUFFERS open buffers, and a disk read is up to
        // 2 MB each, which would stall the watcher thread and every other
        // connection to this project for the whole scan.
        //
        // Only the rel, its base_hash, and (if dirty) its own edited text are
        // collected here — one clone of the text at most, not the two a full
        // `Buffer` clone followed by `.to_string()` would cost. base_hash is
        // what lets the second lock below tell whether this rel's state
        // moved while the lock was down, which it must: this connection is
        // already subscribed (see `subscribe` above), so a peer's `EditBuffer`
        // in that window reaches it too, queued on its channel behind
        // whatever this replay sends. Sending stale disk text after that
        // would land in front of it in the client's own apply order and a
        // save from this connection would then write the stale text back
        // over the peer's newer edit.
        let dir = h.dir.clone();
        let snapshot: Vec<(String, u64, Option<String>)> = h
            .ws
            .buffers
            .iter()
            .map(|(rel, b)| (rel.clone(), b.base_hash, b.edited_text().map(|t| t.to_string())))
            .collect();
        (id, rx, dir, snapshot)
    };
    // Disk reads happen with the hub lock already dropped. Only a rel that
    // was Clean at snapshot time needs one — an edited buffer's text is
    // already in hand and reading its file too would be pure waste.
    let disk_reads: Vec<Option<String>> = snapshot
        .iter()
        .map(|(rel, _, edited)| if edited.is_some() { None } else { read_disk_text(&dir, rel) })
        .collect();
    {
        let mut h = Hub::lock(&hub);
        for ((rel, base_hash, edited), disk_text) in snapshot.into_iter().zip(disk_reads) {
            // Resolved against the hub's *current* state, not the snapshot:
            // see `resolve_replay` for why an existence check alone is not
            // enough, and what it costs (nothing — the paths that matter
            // here read live data already in memory, not the disk).
            let current = h.ws.buffers.get(&rel);
            let Some(text) = resolve_replay(base_hash, &edited, &disk_text, current) else {
                continue;
            };
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

/// Phase 2's actual I/O: one Clean buffer's file, or `None` on a refused or
/// failed read. Never called for a buffer that was dirty at snapshot time —
/// its text is already in hand and reading its file too would be pure waste.
///
/// `None` here is deliberate, not an oversight: `read_text_file` refuses a
/// file as *policy* — too large, or a NUL byte marking it binary — not only
/// on genuine I/O failure, and a clean buffer's file can cross either
/// threshold after the buffer was already opened. Collapsing that into
/// `Some(String::new())` would hand the browser an empty-but-editable
/// textarea for a file that was never actually empty; the next keystroke or
/// an unconditional `pushEdit` save would then write "" over it, straight
/// past `do_save`'s clean-buffer guard, because the buffer really is dirty
/// by then. Sending nothing for this `rel` is recoverable — the next
/// `SetMode`/`OpenTab` re-reads the file — where a blanked editor is not.
fn read_disk_text(dir: &Path, rel: &str) -> Option<String> {
    crate::projects::safe_resolve(dir, rel).and_then(|p| crate::projects::read_text_file(&p)).ok()
}

/// What a rel should replay, resolved against the hub's *current* state
/// rather than trusted from the snapshot phase 1 took before the lock was
/// dropped for disk I/O.
///
/// An existence check alone is not enough: this connection is already
/// subscribed by the time the lock comes back (see `subscribe` at the call
/// site), so a peer's `EditBuffer` reaching the hub during that window is
/// already queued on this connection's own channel, waiting behind whatever
/// this function sends. Handing back a disk read or a snapshot text that is
/// now stale would land *behind* that queued message in send order but,
/// once app.js applies it, end up as the buffer's on-screen content anyway —
/// and a save from this connection would then write that stale text back
/// over the peer's newer edit. See CLAUDE.md's "Absence of evidence is not
/// evidence of absence": `contains_key` alone treated "not gone" as "still
/// what phase 1 saw," which is the same category of mistake with buffer
/// *content* standing in for buffer *existence*.
///
/// `snapshot_base_hash` and `snapshot_edited` are what phase 1 collected;
/// `disk_text` is phase 2's read (only ever attempted when `snapshot_edited`
/// was `None`); `current` is the same rel looked up again under the second
/// lock.
fn resolve_replay(
    snapshot_base_hash: u64,
    snapshot_edited: &Option<String>,
    disk_text: &Option<String>,
    current: Option<&Buffer>,
) -> Option<String> {
    let current = current?; // vanished while unlocked: nothing to resurrect
    match &current.content {
        // A live edit is always the freshest text there is, and cloning it
        // costs no I/O — whether it is the same edit phase 1 saw or a newer
        // one a peer made while the lock was down. This is the branch that
        // closes the stale-overwrite bug: a disk read or a phase-1 snapshot
        // must never be allowed to land on top of it.
        Content::Edited(t) => Some(t.clone()),
        Content::Clean if current.base_hash == snapshot_base_hash => {
            // Nothing about this buffer's disk-relevant state moved since
            // phase 1. If it was Clean then too, the phase-2 read (if any
            // succeeded) is still accurate. If it was dirty then, an
            // unchanged base_hash with Clean now means an in-place undo back
            // to the base text (`Buffer::set_text`) — no disk read was ever
            // scheduled for that case, so there is nothing to forward.
            disk_text.clone()
        }
        Content::Clean => match snapshot_edited {
            // Dirty at snapshot time, Clean now, with base_hash moved: the
            // only path that produces exactly this is `do_save` (hub.rs) —
            // it updates base_hash but, unlike `EditBuffer` and
            // `file_changed_externally`, never re-broadcasts BufferText, so
            // skipping here (the naive "any change is stale" rule) would
            // blank the editor on every save a peer makes during this
            // window. The snapshot's own edited text is what was almost
            // certainly just written; even where a further edit slipped in
            // between phase 1 and the save, this can never overwrite
            // anything silently — a save from this connection is checked
            // against the server's *current* base_hash, not against this
            // text, so a mismatch conflicts instead of clobbering.
            Some(t) => Some(t.clone()),
            // Clean at snapshot time too, with base_hash moved: the only
            // path that changes base_hash on an already-Clean buffer is
            // `file_changed_externally`, which broadcasts the new
            // BufferText itself (already queued on this connection's
            // channel). The phase-2 read reflects the disk from *before*
            // that change, so forwarding it here would be stale on arrival.
            None => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(content: Content, base_hash: u64) -> Buffer {
        Buffer { content, base_hash, ..Buffer::default() }
    }

    #[test]
    fn read_disk_text_returns_the_files_contents() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "on disk\n").unwrap();
        assert_eq!(read_disk_text(d.path(), "a.rs"), Some("on disk\n".to_string()));
    }

    /// The case the review that added this replay path called out: a clean
    /// buffer whose file `read_text_file` refuses. Constructed the way that
    /// refusal is really reached in production — a NUL byte, which
    /// `read_text_file` treats as "this is binary" — not a permissions
    /// trick, since that path is equally real but this one is the one
    /// `file_changed_externally` can actually leave behind (it keeps a
    /// buffer clean and updates base_hash using plain `read_to_string`, so a
    /// later binary write is invisible to it until this read tries the file
    /// back).
    #[test]
    fn read_disk_text_refuses_a_binary_file() {
        let d = tempfile::tempdir().unwrap();
        // Not on TEXT_EXTENSIONS's allow-list, or the NUL-byte sniff in
        // read_text_file is skipped and this fixture would not actually
        // exercise the refusal this test is about.
        std::fs::write(d.path().join("a.bin"), b"a\0b").unwrap();
        assert_eq!(
            read_disk_text(d.path(), "a.bin"),
            None,
            "a refused read must not become an empty, editable buffer"
        );
    }

    #[test]
    fn unchanged_edited_buffer_replays_its_own_text() {
        let edited = Some("unsaved\n".to_string());
        let current = buf(Content::Edited("unsaved\n".into()), 7);
        assert_eq!(resolve_replay(7, &edited, &None, Some(&current)), Some("unsaved\n".to_string()));
    }

    #[test]
    fn unchanged_clean_buffer_forwards_the_disk_read() {
        let disk = Some("on disk\n".to_string());
        let current = buf(Content::Clean, 7);
        assert_eq!(resolve_replay(7, &None, &disk, Some(&current)), Some("on disk\n".to_string()));
    }

    /// The Critical this fix closes: a peer's `EditBuffer` landed while the
    /// lock was down, so the buffer this connection now sees is Edited with
    /// *different* text than either the snapshot or the (irrelevant, since
    /// it was never even read) disk. The live text must win.
    ///
    /// Reverting `resolve_replay`'s call site to a plain `contains_key`
    /// check (the pre-review code) and rerunning `cargo test` fails this
    /// test: `contains_key` only asks "does the buffer still exist," so the
    /// stale snapshot/disk text passed in here would be sent instead of the
    /// peer's newer edit. See the task report for the actual revert-and-run
    /// output.
    #[test]
    fn a_buffer_edited_while_unlocked_replays_the_new_edit_not_the_stale_one() {
        let stale_snapshot = Some("stale\n".to_string());
        let stale_disk = Some("also stale\n".to_string());
        let current = buf(Content::Edited("a peer's newer edit\n".into()), 7);
        assert_eq!(
            resolve_replay(7, &stale_snapshot, &stale_disk, Some(&current)),
            Some("a peer's newer edit\n".to_string()),
            "the buffer's current text must win over anything collected before the lock was dropped"
        );
    }

    /// The Important this fix also closes: a peer's `SaveBuffer` landed
    /// while the lock was down. `do_save` moves the buffer from Edited to
    /// Clean and bumps base_hash, but — unlike `EditBuffer` and
    /// `file_changed_externally` — never re-broadcasts BufferText, so a
    /// blanket "base_hash changed means skip" would blank this editor.
    #[test]
    fn a_buffer_saved_while_unlocked_replays_the_snapshots_edited_text() {
        let edited = Some("about to be saved\n".to_string());
        // base_hash moved from 7 (what phase 1 saw) to 9 (post-save).
        let current = buf(Content::Clean, 9);
        assert_eq!(
            resolve_replay(7, &edited, &None, Some(&current)),
            Some("about to be saved\n".to_string()),
            "a save during the unlocked window must not blank the editor"
        );
    }

    /// A buffer that was already Clean and stayed Clean, but whose file
    /// changed externally while the lock was down: `file_changed_externally`
    /// already broadcast the new text itself, so resending the phase-2 read
    /// — which reflects the disk from *before* that change — would be
    /// stale on arrival.
    #[test]
    fn a_clean_buffer_whose_file_changed_externally_is_not_resent() {
        let current = buf(Content::Clean, 9); // base_hash moved from 7
        assert_eq!(resolve_replay(7, &None, &Some("stale disk read\n".to_string()), Some(&current)), None);
    }

    #[test]
    fn a_buffer_closed_while_unlocked_is_not_resurrected() {
        assert_eq!(resolve_replay(7, &Some("text\n".to_string()), &None, None), None);
    }
}
