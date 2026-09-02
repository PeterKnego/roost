//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
//!
//! Only the writer thread ever writes. tungstenite answers an inbound `Ping`
//! or `Close` from the object that *read* it, which would put a reply on the
//! wire while the writer is part-way through a frame and splice the two
//! together; `wsio::Gate` takes that ability away from the reader once the
//! writer exists, and the reader forwards the reply instead. See
//! `docs/superpowers/specs/2026-08-22-one-writer-per-socket-design.md`.
use crate::hub::{ConnId, Hub};
use crate::proto;
use crate::workspace::{hash_text, Buffer, Content};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use crate::wsio::{Gate, GatedStream};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// One query, start to finish, with the hub lock held only at the two ends.
///
/// Split out of the worker loop so tests can drive it directly — in
/// particular the one that proves the lock is free while it runs.
pub(crate) fn run_search(
    hub: &Arc<Mutex<Hub>>,
    id: &ConnId,
    q: &str,
    seq: u64,
    latest: &Arc<AtomicU64>,
) {
    if latest.load(Ordering::SeqCst) != seq {
        return; // already stale before it started
    }
    // (1) Lock, copy, unlock. The guard is bound inside the block so it is
    // dropped before anything below touches the filesystem.
    let snap = {
        let h = Hub::lock(hub);
        h.search_snapshot()
    };

    // (2) No lock held from here to the end of the walk. `for_project` reads
    // config files, which is why it is on this side of the boundary.
    let settings = crate::config::for_project(&snap.dir);
    let filter = settings.tree_filter_with(snap.show_hidden_override);
    let cancelled = || latest.load(Ordering::SeqCst) != seq;
    let query = crate::search::Query {
        text: q,
        filter,
        sessions: &snap.sessions,
        // Contents are the expensive half; paths and sessions answer from the
        // first keystroke, contents once the query is specific enough to be
        // worth reading every file for.
        contents: q.chars().count() >= 3,
    };
    let results = crate::search::run(&snap.dir, &query, &cancelled);

    // (3) Lock again, only to reply. A query the user typed past is dropped
    // here rather than rendered over what they are looking at now.
    if cancelled() {
        return;
    }
    let ev = crate::proto::Event::SearchResults { seq, results };
    Hub::lock(hub).send_to(id, &ev);
}

/// A connection's search worker: one thread, reused for every query it sends.
///
/// One thread rather than one per query, because a fast typist would
/// otherwise spawn a thread per keystroke — and `latest` means the queued
/// ones mostly have nothing left to do by the time they are picked up.
struct Searcher {
    tx: std::sync::mpsc::Sender<(String, u64)>,
    latest: Arc<AtomicU64>,
}

impl Searcher {
    fn submit(&self, q: String, seq: u64) {
        // Published before the send, so a query already in the queue sees it
        // and abandons itself rather than answering after this one.
        self.latest.store(seq, Ordering::SeqCst);
        let _ = self.tx.send((q, seq));
    }
}

fn spawn_searcher(hub: Arc<Mutex<Hub>>, id: ConnId) -> Searcher {
    let (tx, rx) = std::sync::mpsc::channel::<(String, u64)>();
    let latest = Arc::new(AtomicU64::new(0));
    let latest2 = latest.clone();
    std::thread::spawn(move || {
        while let Ok((q, seq)) = rx.recv() {
            // A panic here would silently take search away from this browser
            // for the life of the connection, with nothing in the UI to say
            // so — CLAUDE.md's "no panics may escape a socket thread", in the
            // one place where the escape would be invisible rather than loud.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_search(&hub, &id, &q, seq, &latest2);
            }));
            if r.is_err() {
                let ev = crate::proto::Event::Error { msg: "search failed".into() };
                Hub::lock(&hub).send_to(&id, &ev);
            }
        }
    });
    Searcher { tx, latest }
}

pub fn handle(stream: TcpStream, project: &str, dir: PathBuf) {
    // WebSocket handshakes bypass the same-origin policy: without this check
    // any page the user visits can drive this socket, and this socket can
    // write files. See spec §Security and src/term.rs's identical check.
    let allowed = crate::config::allowed_origins();
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    // Open through the handshake — the 101 and any refusal go out on this
    // object — and closed below, the moment a second writer exists.
    let gate = Gate::open();
    let accepted = accept_hdr_with_config(
        GatedStream::new(stream, gate.clone()),
        |req: &WsRequest, resp: WsResponse| {
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("roost: rejected workspace ws origin={origin:?} (set allowed_origins)");
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
        // Before the snapshot, and inside the same lock acquisition as
        // subscribing above (releasing in between would let a foreign
        // broadcast land first — see the comment on that lock acquisition).
        // `State` names the proposal tabs but carries neither side of the
        // diff, so a browser that connected after a proposal opened would
        // otherwise draw an empty tab it could still click Accept on —
        // approving a change it never showed anyone. Content before the tab
        // that renders it, exactly as the live path in
        // `Hub::open_proposal_tab` sends it: `Event::Proposal` carries no
        // `origin` field, so unlike `State` below, nothing here depends on
        // this being the *first* thing sent.
        for ev in h.proposal_replay() {
            h.send_to(&id, &ev);
        }
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
        // replay has to come from disk — but not under this lock: a disk
        // read is up to 2 MB each, which would stall the watcher thread and
        // every other connection to this project for the whole scan. Note
        // this is *not* bounded by MAX_BUFFERS: that cap only ever applies
        // to dirty buffers on the live insert path (hub::open_buffer_for),
        // so a session that has previewed many files can hold arbitrarily
        // more clean ones than that — every one of them gets a disk read
        // here on every reconnect.
        //
        // Only the rel, its base_hash, and (if dirty) its own edited text are
        // collected here. For a dirty buffer this still clones its text
        // twice before it reaches the client — once here, once more where
        // `resolve_replay` hands back the *current* live text below — but
        // that second copy is the point: it is what lets phase 3 send the
        // buffer's state as of the second lock instead of the one collected
        // here, which is what closes the stale-overwrite bug this whole
        // restructuring is about. What this avoids is the *third* copy the
        // original version paid: cloning a whole `Buffer` here and then
        // `.to_string()`-ing its text again downstream. base_hash is what
        // lets the second lock below tell whether this rel's state moved
        // while the lock was down, which it must: this connection is
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
    // Built here — the first statement after the lock is released — not below
    // the replay, so it covers everything between the subscription and the
    // read loop: the disk reads, `replay_buffer`, and the `try_clone` early
    // return. It used to sit after all of that, and this branch widened the
    // unguarded window by inserting the `proposal_replay` send into it.
    // `ide.rs` builds its `ConnGuard` ahead of the handshake for the same
    // reason.
    //
    // It cannot go one statement earlier, *inside* the block above:
    // `UnsubGuard::drop` calls `Hub::lock`, locals unwind in reverse
    // declaration order, so a panic in there would drop the guard while that
    // block's own `MutexGuard` is still alive and re-lock the same mutex on
    // the same thread. A leaked subscriber is recoverable; a socket thread
    // wedged holding the hub lock takes the whole project with it.
    let unsub = UnsubGuard { hub: hub.clone(), id: id.clone() };
    // Disk reads happen with the hub lock already dropped. Only a rel that
    // was Clean at snapshot time needs one — an edited buffer's text is
    // already in hand and reading its file too would be pure waste.
    // `None` means no read was even attempted (the buffer was dirty, so its
    // own text replays); `Some(Err(..))` is a read that ran and failed, which
    // the client has to be told about.
    let disk_reads: Vec<Option<Result<String, String>>> = snapshot
        .iter()
        .map(|(rel, _, edited)| if edited.is_some() { None } else { Some(read_disk_text(&dir, rel)) })
        .collect();
    {
        let mut h = Hub::lock(&hub);
        for ((rel, base_hash, edited), disk_text) in snapshot.into_iter().zip(disk_reads) {
            replay_buffer(&mut h, &id, rel, base_hash, edited, disk_text);
        }
    }

    let Ok(write_half) = ws_read.get_ref().try_clone_inner() else { return };
    // From here on this connection has two threads and exactly one writer.
    gate.close();
    // Shared so the read thread can send the control replies it no longer
    // writes itself. The lock is held across a whole `send`, which is the
    // point: these sockets are blocking, so `send` queues and flushes one
    // frame before returning, and no other frame can be spliced into it.
    // Locking the stream per `write` call instead would not do — a frame can
    // drain in several calls, and a splice fits between them.
    //
    // This is a lock held across blocking I/O, which CLAUDE.md forbids. The
    // rule is about a *shared* lock stalling unrelated work: the registry
    // held across a PTY write wedged every session in every project. This one
    // guards the socket being written, its only two contenders are this
    // connection's own threads, and the wait is bounded by one frame.
    let ws_write: Arc<Mutex<WebSocket<TcpStream>>> =
        Arc::new(Mutex::new(WebSocket::from_raw_socket(write_half, Role::Server, None)));
    let ws_ctl = ws_write.clone();

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
            if ws_write.lock().unwrap_or_else(|e| e.into_inner()).send(out).is_err() {
                break;
            }
        }
        let mut w = ws_write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.close(None);
        let _ = w.get_ref().shutdown(std::net::Shutdown::Both);
    });

    let searcher = spawn_searcher(hub.clone(), id.clone());

    loop {
        match ws_read.read() {
            Ok(Message::Text(t)) => {
                // Decoded *before* the hub lock, so a Search can be diverted
                // to its worker. Every other intent is handled under the lock
                // as it always was; a search is the only one that performs
                // unbounded blocking I/O, and holding this lock across that
                // would stall every other browser on this project. See
                // CLAUDE.md's "never hold a lock across blocking I/O".
                let decoded = proto::decode(&t);
                if let Ok(proto::Intent::Search { q, seq }) = decoded {
                    searcher.submit(q, seq);
                    continue;
                }
                let dirty = {
                    let mut h = Hub::lock(&hub);
                    match decoded {
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
            // The reply tungstenite queued on this object went into the
            // gate's bit bucket, so it is sent here instead, by the one
            // writer. A browser cannot send a Ping from page JavaScript, but
            // anything else on loopback can, and a server that stops
            // answering pings looks dead to it.
            Ok(Message::Ping(p)) => {
                let _ = ws_ctl.lock().unwrap_or_else(|e| e.into_inner()).send(Message::Pong(p));
            }
            Ok(Message::Close(c)) => {
                let _ = ws_ctl.lock().unwrap_or_else(|e| e.into_inner()).send(Message::Close(c));
                break;
            }
            Err(_) => break,
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

/// Phase 2's actual I/O: one Clean buffer's file, or the reason it could not
/// be read. Never called for a buffer that was dirty at snapshot time — its
/// text is already in hand and reading its file too would be pure waste.
///
/// Returning `Err` rather than an empty string is deliberate:
/// `read_text_file` refuses a file as *policy* — too large, or a NUL byte
/// marking it binary — not only on genuine I/O failure, and a clean buffer's
/// file can cross either threshold after the buffer was already opened.
/// Collapsing that into `Ok(String::new())` would hand the browser an
/// empty-but-editable textarea for a file that was never actually empty; the
/// next keystroke or an unconditional `pushEdit` save would then write "" over
/// it, straight past `do_save`'s clean-buffer guard, because the buffer really
/// is dirty by then.
///
/// The error message is carried rather than dropped because nothing else will
/// tell the user: the tab still renders, in Edit, over a file this connection
/// has no text for, and no later event re-reads it on its own — `OpenTab` on
/// an already-open tab whose buffer is clean and settled deliberately skips
/// the read (hub.rs's reactivation guard), and `SetMode` only fires if the
/// user happens to toggle modes. `replay_buffer` turns it into an `Error`
/// event naming the file.
fn read_disk_text(dir: &Path, rel: &str) -> Result<String, String> {
    crate::projects::safe_resolve(dir, rel).and_then(|p| crate::projects::read_text_file(&p))
}

/// Phase 3's resolve-and-send for one rel, pulled out of `handle`'s loop so
/// a test can exercise the actual wiring: `hub.ws.buffers.get(&rel)` reads
/// the hub's state *now*, under this second lock, not a reconstruction of
/// what phase 1 collected. `resolve_replay`'s own tests only ever see
/// hand-built `current` values, so they would stay green even if this call
/// site regressed to pass the phase-1 snapshot back in here instead — which
/// would silently defeat the whole fix. See
/// `replay_buffer_reads_the_hubs_current_state_not_the_snapshot` below.
fn replay_buffer(hub: &mut Hub, id: &ConnId, rel: String, base_hash: u64, edited: Option<String>, disk: Option<Result<String, String>>) {
    let read_err = match &disk {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    };
    let disk_text = match disk {
        Some(Ok(t)) => Some(t),
        _ => None,
    };
    let (resolved, still_clean) = {
        let current = hub.ws.buffers.get(&rel);
        // Only a buffer that is still clean is at risk here: one that turned
        // dirty while the lock was down replays its own live text below, and
        // one that vanished has no tab left to be wrong about.
        let still_clean = current.is_some_and(|b| !b.dirty());
        (resolve_replay(base_hash, &edited, &disk_text, current), still_clean)
    };
    match resolved {
        Some(text) => {
            let ev = proto::Event::BufferText { rel, text, origin: String::new() };
            hub.send_to(id, &ev);
        }
        // Silence would leave `mountEditor` seeding from a `texts` map with
        // no entry for this rel: an empty textarea over a file that is not
        // empty, whose base_hash still matches the untouched disk, so the
        // first keystroke saves the blank straight over it. Say so instead,
        // to this connection only — every other subscriber has its own copy
        // and its own read.
        None if still_clean => {
            if let Some(e) = read_err {
                let ev = proto::Event::Error { msg: format!("{rel}: {e}") };
                hub.send_to(id, &ev);
            }
        }
        None => {}
    }
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
        // base_hash moved since the snapshot, and the buffer is Clean now.
        // More than one thing produces exactly this shape — not only
        // `do_save` after an edit, but also a *discard*: `CloseBuffer` while
        // the tab is still open drops the buffer (hub.rs's CloseBuffer arm),
        // and `open_buffer_for` reinserts it Clean, freshly read from disk,
        // with a new base_hash (see the discard test at hub.rs ~1196). And a
        // peer can edit *further* before saving, all within this window, so
        // even a genuine save does not guarantee `snapshot_edited`'s text is
        // what actually landed on disk.
        //
        // Every one of those producers already re-broadcasts BufferText
        // itself — `open_buffer_for` unconditionally, `EditBuffer` to every
        // other subscriber, both already queued on this connection's own
        // channel — so `None` is the safe default: a fresher BufferText is
        // on its way regardless. The one case worth resurrecting without
        // waiting for that broadcast, and without another disk read, is when
        // the snapshot's edited text hashes to exactly `current.base_hash`:
        // that is what "saved unchanged" looks like, byte for byte, and
        // nothing else does. Anything looser reproduces the two bugs review
        // caught here — replaying a just-discarded edit on top of the
        // reload that discarded it, or replaying a pre-save snapshot that a
        // real save's own conflict check would not have stopped, because
        // `do_save` (hub.rs) checks the write against the *server's*
        // base_hash, not against any text a client happens to be showing.
        Content::Clean => match snapshot_edited {
            Some(t) if hash_text(t) == current.base_hash => Some(t.clone()),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a project of `n` files of `lines` lines each, so a walk takes
    /// long enough to observe. Not a fixed sleep for the walk itself: its
    /// duration is the thing under test.
    ///
    /// The size is calibrated, not arbitrary. The first version of this
    /// fixture (4000 one-line files) walked in 10 ms, and the lock-held
    /// implementation of `run_search` passed this test — see the comment on
    /// the test below. Sized down again afterwards, because this runs on
    /// every `cargo test --lib` and the deploy host is modest. Measured here
    /// at 12000 files x 40 lines (~19 MB, ~40 ms to build): a walk of
    /// 150-210 ms, which is 163 polls of a 1 ms loop against a threshold of
    /// 10, and a superseded walk that stops 3 ms after being told to.
    fn wide_project(n: usize, lines: usize) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let body = "some ordinary line of source text here\n".repeat(lines);
        for i in 0..n {
            let sub = d.path().join(format!("d{}", i % 40));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join(format!("f{i}.rs")), &body).unwrap();
        }
        d
    }

    /// A query is one browser's business. With a single subscriber this test
    /// could not tell `send_to` from `broadcast` — CLAUDE.md lists that exact
    /// trap — so there are two, and the second must hear nothing.
    #[test]
    fn results_go_only_to_the_connection_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("needle.rs"), "x\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx_asker) = Hub::lock(&hub).subscribe();
        let (_other, rx_other) = Hub::lock(&hub).subscribe();
        while rx_asker.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}

        let latest = Arc::new(AtomicU64::new(1));
        run_search(&hub, &asker, "needle", 1, &latest);

        let mut mine = vec![];
        while let Ok(m) = rx_asker.try_recv() { mine.push(m); }
        let mut theirs = vec![];
        while let Ok(m) = rx_other.try_recv() { theirs.push(m); }

        assert!(
            mine.iter().any(|m| m.contains(r#""t":"SearchResults""#) && m.contains("needle.rs")),
            "the asker must get its results, got {mine:?}"
        );
        assert!(
            !theirs.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "another browser must not see this query's results, got {theirs:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The hard constraint, as a test. `wsconn` dispatches every other intent
    /// under the hub lock; a search must not, or every browser on the project
    /// stalls for the length of the walk.
    ///
    /// Non-vacuous by construction: it asserts the lock was free *while the
    /// search was still running*, not merely at some point. The result
    /// arriving is what ends the poll loop, so every success counted below
    /// provably happened before the walk finished.
    ///
    /// Two details are load-bearing, and both were found by actually applying
    /// a lock-held `run_search` and watching this test pass anyway:
    ///
    /// 1. The start barrier. Without it the poll loop wins one `try_lock` in
    ///    the window between `thread::spawn` returning and the spawned thread
    ///    reaching its first `Hub::lock` — the lock-held version scored
    ///    exactly `locked_during = 1` on a 10 ms walk, which `> 0` accepted.
    ///    Sleeping first gives the walker time to take the lock it means to
    ///    hold, so a lock-held walk owns it for every poll that follows.
    /// 2. The fixture size. A 10 ms walk leaves ~10 polls, which is too few
    ///    to tell "free throughout" from "free by luck". The fixture walks
    ///    for ~170 ms here, so the correct implementation scores 163 against
    ///    a threshold of 10.
    ///
    /// The barrier cannot mask the defect it was added to expose, which is
    /// the obvious worry about sleeping before you look. If a lock-held walk
    /// finished inside those 20 ms, its result would already be in the
    /// channel, the loop's `rx.try_recv()` would succeed on the first check,
    /// the body would never run, and `locked_during` would be 0 — a failure,
    /// not a pass. The barrier can only ever cost this test samples it would
    /// have counted, never manufacture one.
    #[test]
    fn the_hub_lock_is_free_while_a_search_walks() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = wide_project(12_000, 40);
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path().join("state"));

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}

        let latest = Arc::new(AtomicU64::new(1));
        let h2 = hub.clone();
        let l2 = latest.clone();
        let id2 = asker.clone();
        let walker = std::thread::spawn(move || run_search(&h2, &id2, "needle", 1, &l2));

        // The start barrier (see 1. above). Not a stand-in for the walk's
        // duration — it only ensures the walk has begun before the first
        // poll, so that a lock a lock-held walk is holding is observed as
        // held.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Poll for the lock while the walk is in flight.
        let mut locked_during = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while rx.try_recv().is_err() && std::time::Instant::now() < deadline {
            if let Ok(g) = hub.try_lock() {
                locked_during += 1;
                drop(g);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        walker.join().unwrap();

        // Sustained availability, not a single lucky sample: the correct
        // implementation scores ~500 here, and a lock-held one scores 0.
        assert!(
            locked_during > 10,
            "the hub lock was free only {locked_during} times while the search ran — \
             it is being held across the walk"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A query the user typed past *before it started* must not be answered.
    ///
    /// Only covers the pre-flight check: `latest` is already 5 when the call
    /// is made, so `run_search` returns before it ever walks. The mid-walk
    /// case — the one that decides whether a fast typist's superseded walk
    /// abandons or runs to completion — is a different test, below.
    #[test]
    fn a_superseded_query_sends_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("needle.rs"), "x\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}

        // The connection has moved on to seq 5; the in-flight seq 1 is stale.
        let latest = Arc::new(AtomicU64::new(5));
        run_search(&hub, &asker, "needle", 1, &latest);

        let mut got = vec![];
        while let Ok(m) = rx.try_recv() { got.push(m); }
        assert!(
            !got.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "a superseded query must not answer, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A query superseded *while the walk is running* stops walking.
    ///
    /// The three other search tests here all pass with the cancellation
    /// wiring gutted: `a_superseded_query_sends_nothing` returns at the
    /// pre-flight check and never reaches the walk, so it survives deleting
    /// either check individually and survives replacing the `cancelled`
    /// closure `run_search` hands to `search::run` with `|| false`. Nothing
    /// exercised the closure that `search::run` actually polls at every
    /// directory boundary — which is the only thing that makes a superseded
    /// walk stop rather than finish and then decline to send.
    ///
    /// So the load-bearing assertion here is the *timing* one, and it comes
    /// first: "no SearchResults arrived" is equally true of a walk that ran
    /// to the very end and then dropped its answer. The walk is timed on this
    /// host rather than compared against a hard-coded duration, because the
    /// fixture's cost varies by machine and filesystem and only the ratio is
    /// meaningful.
    #[test]
    fn a_query_superseded_mid_walk_abandons_the_walk() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = wide_project(12_000, 40);
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path().join("state"));

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}

        // Baseline: one uninterrupted walk over this very fixture, to learn
        // what "ran to completion" costs here. Asserting it answered is also
        // what proves the fixture is walkable at all — a fixture that failed
        // to walk would make the interrupted run below fast for the wrong
        // reason.
        let latest = Arc::new(AtomicU64::new(1));
        let t0 = std::time::Instant::now();
        run_search(&hub, &asker, "needle", 1, &latest);
        let full = t0.elapsed();
        let mut base = vec![];
        while let Ok(m) = rx.try_recv() { base.push(m); }
        assert!(
            base.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "the baseline walk must have completed and answered, got {base:?}"
        );

        // The same walk, superseded once it is provably under way.
        let latest2 = Arc::new(AtomicU64::new(3));
        let h2 = hub.clone();
        let l2 = latest2.clone();
        let id2 = asker.clone();
        let walker = std::thread::spawn(move || run_search(&h2, &id2, "needle", 3, &l2));
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Timed from the store, not from the spawn, so the barrier sleep is
        // not counted as walking: this measures only how long the walk kept
        // going after it was superseded.
        let superseded_at = std::time::Instant::now();
        latest2.store(4, Ordering::SeqCst);
        walker.join().unwrap();
        let after = superseded_at.elapsed();

        assert!(
            after < full / 2,
            "the walk kept going {after:?} after being superseded, against a full walk of \
             {full:?} — it is running to the end and only then declining to answer"
        );
        let mut got = vec![];
        while let Ok(m) = rx.try_recv() { got.push(m); }
        assert!(
            !got.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "a superseded query must not answer, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    fn buf(content: Content, base_hash: u64) -> Buffer {
        Buffer { content, base_hash, ..Buffer::default() }
    }

    #[test]
    fn read_disk_text_returns_the_files_contents() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "on disk\n").unwrap();
        assert_eq!(read_disk_text(d.path(), "a.rs"), Ok("on disk\n".to_string()));
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
        // On the message, not just `is_err`: a refusal that arrived for some
        // other reason (a bad resolve, say) would prove nothing about the
        // policy this test is here to pin.
        assert_eq!(
            read_disk_text(d.path(), "a.bin"),
            Err("binary file".to_string()),
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

    /// The Important round 1 closed: a peer's `SaveBuffer` landed while the
    /// lock was down and saved the snapshot's own text unchanged. `do_save`
    /// moves the buffer from Edited to Clean and bumps base_hash to match
    /// exactly what got written — unlike `EditBuffer` and
    /// `file_changed_externally`, it never re-broadcasts BufferText, so a
    /// blanket "base_hash changed means skip" would blank this editor.
    #[test]
    fn a_buffer_saved_unchanged_while_unlocked_replays_the_snapshots_edited_text() {
        let edited = Some("about to be saved\n".to_string());
        // What do_save leaves behind when it saves exactly this text: Clean,
        // base_hash set to that text's own hash.
        let current = buf(Content::Clean, hash_text("about to be saved\n"));
        assert_eq!(
            resolve_replay(7, &edited, &None, Some(&current)),
            Some("about to be saved\n".to_string()),
            "a save-unchanged during the unlocked window must not blank the editor"
        );
    }

    /// The Critical round 2 found in round 1's own fix: a discard during the
    /// unlocked window. `CloseBuffer` with the tab still open (hub.rs) drops
    /// the buffer and `open_buffer_for` reinserts it Clean, freshly re-read
    /// from disk — a base_hash that has no reason to match the hash of the
    /// text that was just discarded. The old "dirty at snapshot, Clean now"
    /// branch treated that as "must be a save" and replayed the discarded
    /// edit right on top of the reload that discarded it.
    #[test]
    fn a_buffer_discarded_while_unlocked_replays_nothing() {
        let edited = Some("the edit that got discarded\n".to_string());
        // What the discard reloaded from disk — unrelated to the discarded
        // text, so its hash does not match.
        let current = buf(Content::Clean, hash_text("on disk all along\n"));
        assert_eq!(
            resolve_replay(7, &edited, &None, Some(&current)),
            None,
            "a discard must not resurrect the text it just discarded"
        );
    }

    /// The other half of round 2's Critical: a peer edits *further* and
    /// *then* saves, all within the unlocked window. The snapshot's edited
    /// text is not what ended up on disk, and — unlike the Critical
    /// `a_buffer_edited_while_unlocked_...` test above — the buffer is Clean
    /// by the time this connection looks again, so the "now Edited" branch
    /// does not catch it. Replaying the snapshot here is exactly the case
    /// the round-2 review flagged: a later save from *this* connection would
    /// be checked against the server's live base_hash, which already
    /// matches the peer's save, so it would go through silently and
    /// overwrite the peer's actual work.
    #[test]
    fn a_buffer_edited_further_then_saved_while_unlocked_replays_nothing() {
        let edited = Some("first draft\n".to_string());
        let current = buf(Content::Clean, hash_text("first draft, then more, then saved\n"));
        assert_eq!(
            resolve_replay(7, &edited, &None, Some(&current)),
            None,
            "the snapshot's text is not what was actually saved, so it must not be replayed"
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

    /// Regression for the exact gap round 2 named: every test above calls
    /// `resolve_replay` with a hand-built `current`, so none of them would
    /// notice if the real call site stopped passing the hub's live buffer
    /// and passed a reconstruction of the phase-1 snapshot instead — a
    /// change that would defeat this whole fix while every test above
    /// stayed green. This drives `replay_buffer` itself against a real
    /// `Hub`, mutating its buffer *after* the "snapshot" to stand in for the
    /// unlocked window, the way `handle`'s phase 2 actually works.
    #[test]
    fn replay_buffer_reads_the_hubs_current_state_not_the_snapshot() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = crate::hub::Hub::new("replay_wiring_probe", d.path().to_path_buf());
        let (id, rx) = h.subscribe();

        // What phase 1 would have collected: this rel was dirty, holding
        // "stale". Then, standing in for a peer's `EditBuffer` landing while
        // phase 2's disk reads were in flight, the hub's *real* buffer moves
        // on to different text — `replay_buffer` is handed the same stale
        // snapshot values phase 1 would have passed regardless.
        h.ws.buffers.insert(
            "a.txt".into(),
            Buffer { content: Content::Edited("fresher\n".into()), base_hash: 1, ..Buffer::default() },
        );

        replay_buffer(&mut h, &id, "a.txt".into(), 1, Some("stale\n".to_string()), None);

        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("fresher") && !m.contains("stale")),
            "replay_buffer must read the hub's current buffer, not resend the stale snapshot it was handed; got {msgs:?}"
        );
    }

    /// The failed-read case, from the other end: sending nothing at all is
    /// what leaves `mountEditor` seeding an empty textarea from a `texts` map
    /// with no entry for this rel — over a file that is not empty, and whose
    /// base_hash still matches the untouched disk, so the first keystroke
    /// saves the blank over it. Nothing else re-reads on its own either: the
    /// reactivation guard in hub.rs skips the read for exactly this buffer
    /// (clean, not stale).
    ///
    /// Two subscribers, because with one, `send_to` and `broadcast` are
    /// indistinguishable and a regression to a project-wide error banner for
    /// one connection's failed read would pass unnoticed. Deleting the
    /// `None if still_clean` arm fails this test.
    #[test]
    fn a_clean_buffers_failed_replay_read_is_reported_to_that_connection() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        // Binary by the NUL sniff, so the read really is refused as policy —
        // the way a file that turned binary under an open clean buffer looks.
        std::fs::write(d.path().join("a.bin"), b"a\0b").unwrap();
        let mut h = crate::hub::Hub::new("replay_failed_read_probe", d.path().to_path_buf());
        let (id, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        h.ws.buffers.insert("a.bin".into(), buf(Content::Clean, 7));

        let read = read_disk_text(&d.path().to_path_buf(), "a.bin");
        assert!(read.is_err(), "fixture is void unless the read actually fails");
        replay_buffer(&mut h, &id, "a.bin".into(), 7, None, Some(read));

        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("a.bin")),
            "a replay read that failed must name the file it failed on; got {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "and must not invent text for it; got {msgs:?}"
        );
        let other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(other.is_empty(), "one connection's failed read is not everyone's; got {other:?}");
        std::env::remove_var("ROOST_STATE_DIR");
    }
}
