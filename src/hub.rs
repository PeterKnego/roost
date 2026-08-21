//! One Hub per project: owns the Workspace, the subscriber list, and the
//! dispatch from intent to either a pure transition or a file operation.
//! Everything the sockets do goes through here, so mirroring is automatic.
use crate::proto::{Event, Intent, Mode, Tab};
use crate::workspace::{self, Workspace};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

pub type ConnId = String;

pub struct Hub {
    pub project: String,
    pub dir: std::path::PathBuf,
    pub ws: Workspace,
    pub subs: HashMap<ConnId, Sender<String>>,
    next_id: u64,
    /// Paths resh itself just wrote, with the resulting hash. The watcher
    /// (Task 8) drops matching events so a save does not echo back.
    pub self_writes: HashMap<String, u64>,
    /// Set once a filesystem watcher has been spawned for this hub, so
    /// `for_project` starts at most one watcher per project even though it
    /// runs on every connection.
    pub watching: bool,
    /// A handle back to this hub's own `Arc<Mutex<Hub>>`, set only by
    /// `for_project` (see there) — lets `do_close_project` spawn a thread
    /// that re-locks this same hub *later*, after the blocking work is
    /// done, rather than doing that work with the lock held (CLAUDE.md's
    /// hard "never hold a lock across blocking I/O" constraint). Left as
    /// `Weak::new()` (never upgrades) for a bare `Hub::new()` built
    /// directly, which this file's own unit tests do — those fall back to
    /// running a close synchronously, matching their assumption that the
    /// broadcast messages are ready immediately after `handle()` returns.
    self_ref: std::sync::Weak<Mutex<Hub>>,
    /// Set under the hub lock before `do_close_project` spawns its
    /// session-killing thread, cleared by that thread once it re-locks to
    /// broadcast. Without this, the window between spawning and re-locking
    /// is unguarded: `StartTerminal` would create a fresh session while the
    /// close is still killing the project's *old* ones, and
    /// `kill_and_unlink` would then find and SIGKILL that brand-new
    /// session's master too — the terminal the user just started dies and
    /// is reported as already closed. It also collapses a rapid double
    /// `CloseProject` into the first attempt rather than spawning a second,
    /// redundant thread whose `ended: 0` would overwrite the true count.
    closing: bool,
    /// Set by the notice intents, drained by the socket layer after the hub
    /// lock is released. `broadcast_all` locks every hub, including this one,
    /// and `Mutex` is not reentrant — so the broadcast cannot happen inside
    /// `handle`.
    pub notices_dirty: bool,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<Hub>>>>> = OnceLock::new();

impl Hub {
    pub fn new(project: &str, dir: std::path::PathBuf) -> Hub {
        let (ws, warn) = crate::wsstate::load(project);
        if let Some(w) = warn {
            eprintln!("resh: {w}");
        }
        let mut hub = Hub {
            project: project.to_string(),
            dir,
            ws,
            subs: HashMap::new(),
            next_id: 0,
            self_writes: HashMap::new(),
            watching: false,
            self_ref: std::sync::Weak::new(),
            closing: false,
            notices_dirty: false,
        };
        // A freshly loaded hub must report reality, not whatever the
        // persisted layout happened to say last time: sessions may have
        // died (or kept running under dtach) since the last save, and
        // `.git` may have appeared or vanished on disk in the meantime.
        hub.refresh_live_sessions();
        hub.reconcile_buffers_with_disk();
        hub
    }

    /// Restored buffers describe what was true when resh last wrote the state
    /// file; the disk may have moved since, and nothing records that — the
    /// state file cannot, because staleness is a fact about the file, not
    /// about the buffer. Left unanswered, a buffer whose file changed during
    /// a restart comes back looking current, and the first sign of trouble is
    /// a conflict banner at save time.
    ///
    /// The two cases follow the live rules exactly (`file_changed_externally`):
    /// a buffer with unsaved text is *flagged*, never overwritten, and a saved
    /// one follows the file.
    ///
    /// A file that cannot be read is a third outcome and is left alone. "I
    /// could not look" is not "it changed" — flagging on a failed read would
    /// put a warning on a file nobody touched, and adopting on one would
    /// discard unsaved work.
    ///
    /// Called from `new`, which runs under the process-global registry lock,
    /// so the I/O here is deliberately bounded: at most `MAX_BUFFERS` files,
    /// whose contents the state file this call just parsed already carried —
    /// so it at most doubles reading that `new` was doing anyway.
    fn reconcile_buffers_with_disk(&mut self) {
        let dir = &self.dir;
        for (rel, b) in self.ws.buffers.iter_mut() {
            let Ok(disk) = crate::projects::safe_resolve(dir, rel)
                .and_then(|p| crate::projects::read_text_file(&p))
            else {
                continue;
            };
            let hash = workspace::hash_text(&disk);
            if b.dirty {
                b.stale = hash != b.base_hash;
            } else if hash != b.base_hash {
                b.text = disk;
                b.base_hash = hash;
                b.stale = false;
            }
        }
    }

    /// One hub per project, shared by every connection to it. Also the place
    /// a project's filesystem watcher is started: the first connection to
    /// see a fresh hub spawns it, and `watching` makes that idempotent so a
    /// second connection racing in does not start a second watcher.
    pub fn for_project(project: &str, dir: std::path::PathBuf) -> Arc<Mutex<Hub>> {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
        let arc = map
            .entry(project.to_string())
            .or_insert_with(|| {
                // `new_cyclic` hands back a `Weak` to the not-yet-finished
                // `Arc` while still constructing its contents, which is
                // exactly what's needed to give the hub a handle to itself
                // (see `self_ref`'s doc comment) without a chicken-and-egg
                // problem — `Hub::new` itself stays untouched (and every
                // other, non-`for_project` caller of it, chiefly this
                // file's own unit tests, keeps getting an empty `self_ref`).
                Arc::new_cyclic(|weak| {
                    let mut hub = Hub::new(project, dir.clone());
                    hub.self_ref = weak.clone();
                    Mutex::new(hub)
                })
            })
            .clone();
        // The registry lock is dropped by the time we get here (this is
        // after the `entry` call completes, and `map` is not touched again),
        // so locking the hub next cannot deadlock against another thread
        // that holds the hub lock and wants the registry lock.
        drop(map);
        // Test-and-set inside one critical section on the hub lock: two
        // connections racing into a brand-new project must not both decide
        // "not watching yet" and both spawn a watcher. That would be more
        // than wasteful — two watcher threads racing on the same
        // `self_writes` entry means one of them finds the token already
        // consumed by the other and re-broadcasts a save's own content back
        // at the author, exactly the cursor-fight self-write suppression
        // exists to prevent. `watching` is set to true here, *before*
        // spawning, so the flag itself is the lock; the actual spawn still
        // happens with no hub lock held, since it walks the filesystem.
        let need_watcher = {
            let mut h = Hub::lock(&arc);
            if h.watching {
                false
            } else {
                h.watching = true;
                true
            }
        };
        if need_watcher {
            let ms: u64 = std::env::var("RESH_DEBOUNCE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300);
            // `watch::spawn` walks the tree and registers OS watches, which
            // on a large project (thousands of directories) can take long
            // enough that the connection thread calling `for_project` would
            // never get to send the client's first `State` snapshot — this
            // is the bug reported live: the workspace pane stayed empty
            // because nothing was ever sent. Doing the setup on its own
            // thread means `for_project` returns immediately regardless of
            // project size. `watching` is already set (above, under the hub
            // lock), so a second connection racing in here sees
            // `need_watcher == false` and does not spawn a second one.
            let arc2 = arc.clone();
            let project2 = project.to_string();
            std::thread::spawn(move || {
                let ok = crate::watch::spawn(&project2, dir, arc2.clone(), std::time::Duration::from_millis(ms));
                let mut h = Hub::lock(&arc2);
                h.ws.watch_degraded = !ok;
                if !ok {
                    // Clients already served a snapshot saw watch_degraded
                    // still false, since setup hadn't finished yet — tell
                    // them now, or the UI never learns watching failed.
                    let ev = h.snapshot_event(&String::new());
                    h.broadcast(&ev);
                }
            });
        }
        arc
    }

    /// Lock a hub, recovering from poisoning. A panic in one connection thread
    /// must not take the project down for every other browser.
    pub fn lock(h: &Arc<Mutex<Hub>>) -> std::sync::MutexGuard<'_, Hub> {
        h.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True when this project has a live hub currently mid-`CloseProject`
    /// (see `closing`). Exists for `term.rs`: refusing the `StartTerminal`
    /// *intent* while a close is in flight only stops a browser that asks
    /// first, and the actual PTY spawn happens when something connects to
    /// `/ws/{project}/term/{name}` — which a mirrored tab already showing a
    /// terminal does on reconnect, with no intent of its own.
    ///
    /// Deliberately does *not* create a hub the way `for_project` does: a
    /// project with no hub at all cannot be closing, and building one here
    /// would also start a filesystem watcher as a side effect of a mere
    /// question. The registry guard is dropped before the hub is locked, for
    /// the same reason `for_project` drops it: a thread holding a hub lock
    /// may want the registry lock.
    pub fn is_closing(project: &str) -> bool {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let arc = {
            let map = reg.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(project) {
                Some(a) => a.clone(),
                None => return false,
            }
        };
        // Bound, not returned inline: as a tail expression the guard would
        // outlive `arc` and this would not compile.
        let closing = Hub::lock(&arc).closing;
        closing
    }

    /// This workspace's tree-visibility override, or `None` when it is still
    /// following the config file. Answers from the registry without creating
    /// a hub, exactly like [`Hub::is_closing`] and for the same reason: a
    /// project nobody has opened has no override, and building a hub to ask
    /// would start a filesystem watcher as a side effect of a question the
    /// tree fragment asks on every render.
    pub fn show_hidden(project: &str) -> Option<bool> {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let arc = {
            let map = reg.lock().unwrap_or_else(|e| e.into_inner());
            map.get(project)?.clone()
        };
        // Bound rather than returned inline, so the guard drops before `arc`.
        let over = Hub::lock(&arc).ws.show_hidden;
        over
    }

    pub fn subscribe(&mut self) -> (ConnId, Receiver<String>) {
        self.next_id += 1;
        let id = format!("c{}", self.next_id);
        let (tx, rx) = channel();
        self.subs.insert(id.clone(), tx);
        (id, rx)
    }

    pub fn unsubscribe(&mut self, id: &ConnId) {
        self.subs.remove(id);
    }

    /// Send to everyone; prune receivers that have gone away. That pruning is
    /// how a closed socket is noticed — there is no separate reaper.
    pub fn broadcast(&mut self, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    pub fn broadcast_except(&mut self, skip: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|id, tx| id == skip || tx.send(msg.clone()).is_ok());
    }

    pub fn send_to(&mut self, id: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        if let Some(tx) = self.subs.get(id) {
            if tx.send(msg).is_err() {
                self.subs.remove(id);
            }
        }
    }

    pub fn snapshot_event(&self, origin: &ConnId) -> Event {
        Event::State { version: self.ws.version, origin: origin.clone(), ws: self.ws.view() }
    }

    fn persist(&mut self) {
        if let Err(e) = crate::wsstate::save(&self.project.clone(), &self.ws) {
            eprintln!("resh: state save failed: {e}");
        }
    }

    pub fn handle(&mut self, from: &ConnId, intent: Intent) {
        match &intent {
            Intent::RequestState => {
                // Safety net for anything that changed `live_sessions`/`is_git`
                // without going through a hub intent — chiefly a terminal
                // websocket's own `session::attach`, which normally pushes a
                // refresh itself (see term.rs) but a client that reconnects
                // and asks for state directly should not have to wait for
                // some unrelated intent to arrive first. Cheap now that this
                // is `session::live_names` rather than `list_sessions` (no
                // `ps` fork per session).
                self.refresh_live_sessions();
                let ev = self.snapshot_event(from);
                self.send_to(from, &ev);
                return;
            }
            Intent::MarkNoticeRead { id } => {
                crate::notify::mark_read(*id);
                // Everyone, not just the caller: read state is global, so a
                // second browser's badge must not keep counting it. The
                // actual broadcast happens outside `handle` (see
                // `notices_dirty`) because `broadcast_all` locks every hub,
                // including this one, which is already locked here.
                self.notices_dirty = true;
                return;
            }
            Intent::MarkAllNoticesRead => {
                crate::notify::mark_all_read();
                // Same reasoning as MarkNoticeRead above: dirty-flag, don't
                // broadcast_all from in here.
                self.notices_dirty = true;
                return;
            }
            Intent::ClearNotices => {
                crate::notify::clear();
                self.notices_dirty = true;
                return;
            }
            Intent::EditBuffer { rel, text } => {
                // Text goes to everyone *but* the author, so their cursor survives.
                let ev = Event::BufferText {
                    rel: rel.clone(),
                    text: text.clone(),
                    origin: from.clone(),
                };
                if let Err(e) = workspace::apply_layout(&mut self.ws, &intent) {
                    let ev = Event::Error { msg: e };
                    self.send_to(from, &ev);
                    return;
                }
                self.ws.version += 1;
                self.broadcast_except(from, &ev);
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
                return;
            }
            Intent::SaveBuffer { rel, force } => return self.do_save(from, rel.clone(), *force),
            Intent::CreateFile { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::create_file(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::CreateDir { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::create_dir(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::DeleteFile { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::delete(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::RenamePath { from: f, to } => {
                let dir = self.dir.clone();
                let (old, new) = (f.clone(), to.clone());
                let r = crate::fileops::rename(&dir, f, to);
                return self.do_rename(from, r, &old, &new);
            }
            Intent::StartTerminal { session } => return self.do_start_terminal(from, session.clone()),
            Intent::InitGit => return self.do_init_git(from),
            Intent::CloseProject => return self.do_close_project(from),
            Intent::EndSession { session } => return self.do_end_session(from, session.clone()),
            Intent::NewTerminal { pane } => return self.do_new_terminal(from, *pane),
            _ => {}
        }
        // CloseTab removes the tab from `self.ws` inside `apply_layout`
        // below, so the rel it pointed at (if any) must be captured before
        // that call, not after.
        let closing_rel: Option<String> = match &intent {
            Intent::CloseTab { pane, idx } => self
                .ws
                .panes
                .get(*pane as usize)
                .and_then(|p| p.tabs.get(*idx))
                .and_then(|t| match t {
                    Tab::File { rel, .. } => Some(rel.clone()),
                    _ => None,
                }),
            _ => None,
        };
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                // Entering Edit mode is the server's cue to become this
                // buffer's owner: read the file now, so base_hash/base_mtime
                // reflect what's actually on disk. Without this, a buffer
                // opened purely client-side (the old /frag/raw flow) never
                // got a real base_hash and every first save reported a
                // conflict against content it never compared against.
                match &intent {
                    Intent::SetMode { rel, mode: Mode::Edit } => self.open_for_edit(from, rel),
                    // Dispatched off the COERCED tab, not off the intent:
                    // apply_layout may have forced Edit to Preview, and
                    // reading the file anyway would seed a buffer for a tab
                    // that has no editor. That buffer is not harmless — the
                    // read stores String::from_utf8_lossy, so its text is not
                    // the file's bytes, and a later save would write U+FFFD
                    // over the original.
                    Intent::OpenTab { tab, .. } => {
                        if let Tab::File { rel, mode: Mode::Edit } = workspace::coerce_tab(tab) {
                            self.open_for_edit(from, &rel)
                        }
                    }
                    // Closing a File tab is the only way a buffer is ever
                    // freed short of the conflict banner's "discard mine" —
                    // without this, every buffer a user opens accumulates
                    // (up to MAX_BUFFERS) and is persisted with its full
                    // text to disk forever, including secrets like a .env
                    // opened once in Edit.
                    Intent::CloseTab { .. } => {
                        if let Some(rel) = &closing_rel {
                            self.maybe_drop_buffer(rel);
                        }
                    }
                    // Discarding means "show me what is on disk", not "show me
                    // nothing". The tab stays open in Edit, and a tab in Edit
                    // whose buffer is gone renders an empty textarea — for
                    // certain on the next reload, since a connecting client is
                    // only sent text for buffers that exist (see wsconn). Until
                    // then it would go on showing the very text just discarded.
                    //
                    // Only when a tab still points at it: CloseBuffer is also
                    // how a buffer is freed outright, and re-reading there
                    // would resurrect every buffer forever, defeating both the
                    // MAX_BUFFERS cap and the reason a once-opened .env should
                    // not stay in the state file.
                    Intent::CloseBuffer { rel } => {
                        let still_open = self.ws.panes.iter().any(|p| {
                            p.tabs.iter().any(
                                |t| matches!(t, Tab::File { rel: r, mode: Mode::Edit } if r == rel),
                            )
                        });
                        if still_open {
                            self.open_for_edit(from, rel);
                        }
                    }
                    _ => {}
                }
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(false) => {}
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// Reads a file into its buffer the moment a tab enters Edit mode, and
    /// tells every client what's in it. Skips the disk read when the buffer
    /// is already dirty: reactivating an in-progress edit (the tab getting
    /// reopened, e.g. by a second browser) must never clobber unsaved text
    /// with what's on disk — only `SaveBuffer`/`CloseBuffer` may do that.
    fn open_for_edit(&mut self, from: &ConnId, rel: &str) {
        let already_dirty = self.ws.buffers.get(rel).map(|b| b.dirty).unwrap_or(false);
        if !already_dirty {
            if !self.ws.buffers.contains_key(rel) && self.ws.buffers.len() >= workspace::MAX_BUFFERS {
                self.send_to(from, &Event::Error { msg: "too many open buffers".into() });
                return;
            }
            match crate::projects::safe_resolve(&self.dir, rel)
                .and_then(|p| crate::projects::read_text_file(&p))
            {
                Ok(text) => {
                    let hash = workspace::hash_text(&text);
                    let mtime =
                        std::fs::metadata(self.dir.join(rel)).ok().and_then(|m| m.modified().ok());
                    let b = self.ws.buffers.entry(rel.to_string()).or_default();
                    b.text = text;
                    b.base_hash = hash;
                    b.base_mtime = mtime;
                    b.dirty = false;
                    b.stale = false;
                }
                Err(e) => {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
            }
        }
        let text = self.ws.buffers.get(rel).map(|b| b.text.clone()).unwrap_or_default();
        // No author: everyone applies it, including the client that just
        // switched to Edit — otherwise its own echo-suppression rule would
        // drop this and the editor would open blank (the bug this fixes).
        self.broadcast(&Event::BufferText { rel: rel.to_string(), text, origin: String::new() });
    }

    /// A file with an open buffer changed on disk. Clean buffers follow the
    /// file so you watch Claude's edits land live; dirty buffers are only
    /// flagged stale, so unsaved work is never overwritten by a background
    /// writer.
    ///
    /// Returns false when the file could not be read — almost always because
    /// it was deleted. `classify` routes an open buffer's path to `Buffer`,
    /// not `Tree`, so without this the caller's tree pane would keep listing
    /// a file that no longer exists until some unrelated event happened to
    /// arrive and trigger a refresh. Callers must treat `false` as a tree
    /// change too.
    pub fn file_changed_externally(&mut self, base: &std::path::Path, rel: &str) -> bool {
        let Ok(disk) = std::fs::read_to_string(base.join(rel)) else { return false };
        let disk_hash = workspace::hash_text(&disk);
        if crate::watch::is_self_write(&mut self.self_writes, rel, disk_hash) {
            return true; // our own save; broadcasting it would echo back at the author
        }
        let Some(b) = self.ws.buffers.get_mut(rel) else { return true };
        if b.dirty {
            b.stale = true;
            let ev = Event::BufferStale { rel: rel.to_string() };
            self.broadcast(&ev);
        } else {
            b.text = disk.clone();
            b.base_hash = disk_hash;
            b.stale = false;
            let ev = Event::BufferText {
                rel: rel.to_string(),
                text: disk,
                origin: String::new(), // no author: everyone applies it
            };
            self.broadcast(&ev);
        }
        self.ws.version += 1;
        self.broadcast(&Event::FileChanged { rel: rel.to_string() });
        true
    }

    fn do_fileop(&mut self, from: &ConnId, r: Result<std::path::PathBuf, String>) {
        match r {
            Ok(_) => self.broadcast(&Event::TreeChanged),
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// A rename is a fileop like the others, but unlike create/delete it can
    /// leave in-memory state pointing at a path that no longer exists: an
    /// open Edit tab's buffer is keyed by the old rel, and every tab
    /// referencing it still shows the old path. Left alone, the next save
    /// against that tab fails `safe_resolve` with "not found" — silently,
    /// per the Error-banner gap this branch fixes alongside (A2) — and the
    /// unsaved text becomes permanently unsaveable. So a successful rename
    /// also rekeys buffers/tabs and broadcasts `State`, not just `TreeChanged`.
    fn do_rename(&mut self, from: &ConnId, r: Result<std::path::PathBuf, String>, old: &str, new: &str) {
        match r {
            Ok(_) => {
                self.rekey_after_rename(old, new);
                self.ws.version += 1;
                self.broadcast(&Event::TreeChanged);
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// Move the buffer and rewrite every tab's rel from `old` to `new` after
    /// a filesystem rename succeeds. A rename can move a whole subtree (a
    /// directory), so this rewrites not just an exact match on `old` but
    /// every rel that has `old` as a `/`-boundary prefix — `has_prefix_boundary`
    /// is what keeps that from also matching an unrelated sibling like
    /// renaming "src" from clobbering "src2/x.rs".
    fn rekey_after_rename(&mut self, old: &str, new: &str) {
        if let Some(buf) = self.ws.buffers.remove(old) {
            self.ws.buffers.insert(new.to_string(), buf);
        }
        let nested: Vec<String> =
            self.ws.buffers.keys().filter(|k| has_prefix_boundary(k, old)).cloned().collect();
        for k in nested {
            if let Some(buf) = self.ws.buffers.remove(&k) {
                self.ws.buffers.insert(format!("{new}{}", &k[old.len()..]), buf);
            }
        }
        for p in self.ws.panes.iter_mut() {
            for t in p.tabs.iter_mut() {
                if let Tab::File { rel, .. } | Tab::Diff { rel: Some(rel) } = t {
                    if rel == old {
                        *rel = new.to_string();
                    } else if has_prefix_boundary(rel, old) {
                        *rel = format!("{new}{}", &rel[old.len()..]);
                    }
                }
            }
        }
    }

    /// Drop a File tab's buffer once closing a tab leaves nothing else
    /// pointing at it — but never while it's dirty. Discarding unsaved text
    /// on close would violate the exact crash-safety property `Buffer`
    /// exists for; only an explicit `SaveBuffer`/`CloseBuffer` (the conflict
    /// banner's "discard mine") may do that.
    fn maybe_drop_buffer(&mut self, rel: &str) {
        let still_open = self.ws.panes.iter().any(|p| {
            p.tabs.iter().any(|t| match t {
                Tab::File { rel: r, .. } => r == rel,
                Tab::Diff { rel: Some(r) } => r == rel,
                _ => false,
            })
        });
        if still_open {
            return;
        }
        if self.ws.buffers.get(rel).is_some_and(|b| !b.dirty) {
            self.ws.buffers.remove(rel);
        }
    }

    fn do_save(&mut self, from: &ConnId, rel: String, force: bool) {
        let Some(buf) = self.ws.buffers.get(&rel).cloned() else {
            let ev = Event::Error { msg: format!("no buffer for {rel}") };
            return self.send_to(from, &ev);
        };
        let dir = self.dir.clone();
        match crate::fileops::save(&dir, &rel, &buf.text, buf.base_hash, force) {
            Ok(crate::fileops::SaveOutcome::Written) => {
                let hash = workspace::hash_text(&buf.text);
                if let Some(b) = self.ws.buffers.get_mut(&rel) {
                    b.dirty = false;
                    b.stale = false;
                    b.base_hash = hash;
                    b.base_mtime = std::fs::metadata(dir.join(&rel)).ok().and_then(|m| m.modified().ok());
                }
                self.self_writes.insert(rel.clone(), hash);
                self.ws.version += 1;
                self.broadcast(&Event::SaveOk { rel: rel.clone() });
                self.broadcast(&Event::FileChanged { rel });
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(crate::fileops::SaveOutcome::Conflict { disk_text }) => {
                let diff_html = crate::render::diff_html(&crate::textdiff::unified(&disk_text, &buf.text));
                let ev = Event::SaveConflict { rel, diff_html };
                self.send_to(from, &ev);
            }
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// Recompute what is actually running/present. Cheap enough to call
    /// after any intent that could change it, and the single source of
    /// truth for the client's placeholder-versus-attach decision. Uses the
    /// *raw* project string — `session::live_names` does its own
    /// `storage_key` encoding internally to build its map keys, so passing
    /// an already-encoded key here would double-encode and silently match
    /// zero sessions. Deliberately `session::live_names`, not
    /// `list_sessions`: the latter forks a `ps` per session *while holding
    /// the global session-registry mutex*, and this is called from
    /// `Hub::new` — which itself runs under the process-global hub-registry
    /// lock (`for_project`'s `or_insert_with`) — so that cost would stall
    /// every other project's connection setup, not just this one's.
    pub fn refresh_live_sessions(&mut self) {
        self.ws.live_sessions = crate::session::live_names(&self.project);
        // Any enclosing work tree counts, not just this directory's own `.git`
        // — see `gitio::is_inside_work_tree`. A nested project has no `.git` of
        // its own, and offering `git init` there embeds a repository inside its
        // parent.
        self.ws.is_git = crate::gitio::is_inside_work_tree(&self.dir);
    }

    /// The client's websocket connect to `/ws/{project}/term/{name}` is what
    /// actually spawns the PTY (via `session::attach`); this intent only
    /// validates the name, enforces the per-project cap, and tells every
    /// mirrored client the tab is now live. Spawning here too would double-
    /// spawn.
    fn do_start_terminal(&mut self, from: &ConnId, session: String) {
        if !crate::session::valid_name(&session) {
            let ev = Event::Error { msg: format!("invalid session name: {session}") };
            return self.send_to(from, &ev);
        }
        // I4: a close in flight is killing every one of this project's
        // sessions on a background thread. Starting a new one now would
        // race it — the browser's own follow-up connect to
        // `/ws/{project}/term/{session}` (term.rs, not this intent) is what
        // actually spawns the PTY, and if that lands while `kill_project`
        // is still running, `kill_and_unlink` can find and SIGKILL the
        // *new* session's master too: the terminal the user just started
        // dies and is reported as already closed. Refusing the intent here
        // stops the browser from ever issuing that connect.
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        // `live_names`, not `list_sessions` — see refresh_live_sessions's
        // comment; the cap check needs only a count, not per-session `ps` ages.
        let live = crate::session::live_names(&self.project).len();
        if !crate::session::has_session(&self.project, &session)
            && live >= crate::session::MAX_SESSIONS_PER_PROJECT
        {
            let ev = Event::Error { msg: "too many terminal sessions".into() };
            return self.send_to(from, &ev);
        }
        self.ws.version += 1;
        self.broadcast(&Event::TerminalStarted { session });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
    }

    /// `git init` takes no user-supplied arguments and always runs with the
    /// project directory as cwd — this is a fixed, server-chosen command,
    /// not something a client can steer. Routed through `gitio::run_git`
    /// rather than a bare `Command::output()` so it inherits the same 15s
    /// deadline-and-kill every other git invocation in this project gets:
    /// this runs under the hub lock, so a hanging git (stalled network
    /// mount, a slow hook) would otherwise freeze every websocket on the
    /// project permanently.
    fn do_init_git(&mut self, from: &ConnId) {
        let (ok, msg) = match crate::gitio::init(&self.dir) {
            Ok(out) => (true, out.trim().to_string()),
            Err(e) => (false, e),
        };
        self.broadcast(&Event::GitInit { ok, msg });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
    }

    /// Ends every running terminal session for this project, but keeps the
    /// saved layout so reopening restores panes and tabs. Refused outright
    /// while any buffer is dirty: unsaved text is the one piece of state
    /// that cannot be reconstructed, so a resource-cleanup operation must
    /// never destroy it. `CloseRefused` goes only to the requesting client
    /// (`send_to`) — a conflict here is that client's business, not
    /// something every mirrored browser needs to hear about.
    ///
    /// The actual killing runs off this hub's own lock. `session::kill_project`
    /// confirms every session's `dtach` master is really dead — a bounded
    /// poll that can take up to ~500ms *per session*, times up to
    /// `MAX_SESSIONS_PER_PROJECT` (16) — and shells out to `ps`/`kill` along
    /// the way. `wsconn.rs` calls `handle` (and so this) with the hub
    /// already locked, and every other websocket on this project (terminal
    /// I/O in `term.rs`, the filesystem watcher) needs that same lock, so
    /// running the kill inline would freeze all of them for the whole
    /// duration — `CLAUDE.md` makes "never hold a lock across blocking I/O"
    /// a hard constraint, and this project has already shipped one deadlock
    /// of exactly this shape. So this spawns a thread that does the killing
    /// unlocked, then re-locks only long enough to broadcast the result.
    ///
    /// That thread needs a handle back to this same hub, which only exists
    /// when this `Hub` was built through `for_project` (`self_ref` upgrades)
    /// — the real, production path, and what every integration test goes
    /// through too. A bare `Hub::new()`, which this file's own unit tests
    /// use directly, has no such handle; those tests assert on broadcast
    /// messages immediately after `handle()` returns, so that path falls
    /// back to running the close synchronously, preserving that assumption
    /// exactly rather than turning dozens of unrelated tests into pollers.
    ///
    /// `closing` (I4) is set here, under the lock, before the thread is
    /// even spawned — not inside it — so the window in which a fresh
    /// `StartTerminal` could race the kill is closed from the very start of
    /// this call, and a second `CloseProject` arriving before the first
    /// finishes is a no-op rather than a redundant second kill pass whose
    /// `ended: 0` would overwrite the true count. Cleared only once the
    /// thread re-locks to broadcast (or, on the synchronous fallback,
    /// implicitly — it never gets set in the first place).
    ///
    /// Broadcasts an empty origin (`snapshot_event(&String::new())`), like
    /// every other broadcast in this file (`do_init_git`, `do_start_terminal`,
    /// `term.rs`) — *not* `from`: this `State` now arrives asynchronously,
    /// after `handle` has already returned, so a later `RequestState` could
    /// otherwise mistake it for the answer to its own request (see
    /// `tests/integration.rs`'s `fresh_state` doc comment, which already
    /// documents this as the invariant).
    /// Ends one terminal session and clears its tabs everywhere.
    ///
    /// The layout change is applied and broadcast *now*, while the kill runs on
    /// a background thread: `session::end_session` confirms the dtach master is
    /// really dead, a bounded poll that shells out to `ps`/`kill`. Doing that
    /// inline would hold the hub lock across it and freeze terminal output for
    /// every session in the project — the same constraint `do_close_project`
    /// already spawns a thread for.
    fn do_end_session(&mut self, from: &ConnId, session: String) {
        if !crate::session::valid_name(&session) {
            let ev = Event::Error { msg: format!("invalid session name: {session}") };
            return self.send_to(from, &ev);
        }
        // A close in flight is already killing every session here. Racing it
        // would report an end for a session `kill_project` is also ending.
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        let intent = Intent::EndSession { session: session.clone() };
        if let Ok(true) = workspace::apply_layout(&mut self.ws, &intent) {
            self.ws.version += 1;
        }
        let project = self.project.clone();
        match self.self_ref.upgrade() {
            Some(hub_arc) => {
                let thread_project = project.clone();
                let thread_session = session.clone();
                let spawned =
                    std::thread::Builder::new().name("end-session".into()).spawn(move || {
                        // No panic may escape a socket thread; one here must not
                        // strand the hub lock and leave every later intent stuck.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::session::end_session(&thread_project, &thread_session)
                        }))
                        .unwrap_or_else(|_| {
                            eprintln!(
                                "resh: end_session panicked ending {thread_project}/{thread_session}"
                            );
                            false
                        });
                        let mut h = Hub::lock(&hub_arc);
                        h.refresh_live_sessions();
                        let snap = h.snapshot_event(&String::new());
                        h.broadcast(&snap);
                    });
                if let Err(e) = spawned {
                    eprintln!("resh: could not spawn end-session thread for {project}: {e}");
                    crate::session::end_session(&project, &session);
                    self.refresh_live_sessions();
                }
            }
            None => {
                crate::session::end_session(&project, &session);
                self.refresh_live_sessions();
            }
        }
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
        self.persist();
    }

    /// Opens a terminal on a server-allocated name. See
    /// `session::next_free_name` for why the client must not choose it.
    fn do_new_terminal(&mut self, from: &ConnId, pane: crate::proto::PaneId) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        // Names already on a tab but not yet connected are in no registry —
        // `next_free_name`'s `also_taken` is what stops two quick clicks from
        // both being handed `term`.
        let on_tabs: Vec<String> = self
            .ws
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match t {
                Tab::Terminal { session } => Some(session.clone()),
                _ => None,
            })
            .collect();
        let Some(name) = crate::session::next_free_name(&self.project, &on_tabs) else {
            let ev = Event::Error { msg: "too many terminal sessions".into() };
            return self.send_to(from, &ev);
        };
        let intent = Intent::OpenTab { pane, tab: Tab::Terminal { session: name.clone() } };
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                self.broadcast(&Event::TerminalStarted { session: name });
                self.refresh_live_sessions();
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(false) => {}
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    fn do_close_project(&mut self, from: &ConnId) {
        let dirty: Vec<String> =
            self.ws.buffers.iter().filter(|(_, b)| b.dirty).map(|(r, _)| r.clone()).collect();
        if !dirty.is_empty() {
            let ev = Event::CloseRefused { dirty };
            return self.send_to(from, &ev);
        }
        if self.closing {
            return;
        }
        let project = self.project.clone();
        match self.self_ref.upgrade() {
            Some(hub_arc) => {
                self.closing = true;
                // I3: thread creation itself can fail (fork/EAGAIN — the
                // same process-table pressure C1 already contemplates), and
                // a panic from that would escape `handle` through
                // `wsconn::handle`, which has no `catch_unwind` — killing
                // the browser's workspace socket mid-session. `Builder::spawn`
                // returns a `Result` instead of panicking, so a failure here
                // is just an `Err` to handle, not a crash to survive.
                // Cloned rather than moved into the closure below: `Err`
                // from `spawn` means the closure — and whatever it
                // captured — was never run, but it was still consumed by
                // the `spawn` call itself, so the error path below needs
                // its own owned copy to log which project failed to close.
                let thread_project = project.clone();
                let spawned = std::thread::Builder::new().name("close-project".into()).spawn(
                    move || {
                        // No panic may escape a socket thread. One here
                        // (from anything session::kill_project calls) must
                        // not strand ProjectClosed/State unsent forever
                        // with the hub lock never reacquired and `closing`
                        // stuck true — that would leave the UI showing a
                        // close that silently never finished, and refuse
                        // every later StartTerminal/CloseProject forever.
                        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::session::kill_project(&thread_project)
                        }))
                        .unwrap_or_else(|_| {
                            eprintln!(
                                "resh: kill_project panicked while closing {thread_project}; reporting 0 ended"
                            );
                            0
                        });
                        let mut h = Hub::lock(&hub_arc);
                        h.closing = false;
                        h.ws.version += 1;
                        h.broadcast(&Event::ProjectClosed { ended });
                        h.refresh_live_sessions();
                        let snap = h.snapshot_event(&String::new());
                        h.broadcast(&snap);
                    },
                );
                if let Err(e) = spawned {
                    self.closing = false;
                    eprintln!("resh: could not spawn close-project thread for {project}: {e}");
                    let ev = Event::Error { msg: "could not close the project; try again".into() };
                    self.send_to(from, &ev);
                }
            }
            None => {
                let ended = crate::session::kill_project(&project);
                self.ws.version += 1;
                self.broadcast(&Event::ProjectClosed { ended });
                self.refresh_live_sessions();
                let snap = self.snapshot_event(&String::new());
                self.broadcast(&snap);
            }
        }
    }
}

/// Send an event to every connected client of every project. Notices are
/// machine-wide: a browser on one project must still learn that another one
/// wants attention.
///
/// The registry lock is dropped before any hub lock is taken. `for_project`
/// already established that order (registry, then hub); taking them the other
/// way round here would deadlock against a connection racing in.
pub fn broadcast_all(ev: &Event) {
    let Some(reg) = REGISTRY.get() else { return };
    let hubs: Vec<Arc<Mutex<Hub>>> = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    };
    for h in hubs {
        Hub::lock(&h).broadcast(ev);
    }
}

/// Record a parsed sequence and tell every client. Called from the PTY pump
/// thread, which holds no lock at this point and must never panic.
pub fn publish(project: &str, session: &str, p: crate::osc::Parsed) {
    if let Some(notice) = crate::notify::record(project, session, p) {
        broadcast_all(&Event::Notice { notice });
    }
}

/// True when `prefix` names a directory containing `path` — i.e. `path` is
/// `prefix` followed by a `/` and more. A plain `starts_with` would wrongly
/// match "src2/x.rs" against prefix "src"; this requires the `/` boundary.
fn has_prefix_boundary(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len() && path.starts_with(prefix) && path.as_bytes()[prefix.len()] == b'/'
}

/// A minimal unified-diff rendering of disk vs buffer. Uses the existing
/// classifier in `render`, so the conflict view looks like every other diff.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, Mode, Tab};

    // Helper: drain whatever a receiver has without blocking.
    fn drain(rx: &Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(m) = rx.try_recv() {
            out.push(m);
        }
        out
    }

    #[test]
    fn a_mutation_reaches_every_subscriber() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&b, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "a.txt".into(), mode: Mode::Preview },
        });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(to_a.iter().any(|m| m.contains(r#""t":"State""#)), "the other client must mirror");
        assert!(to_b.iter().any(|m| m.contains(r#""t":"State""#)), "originator sees it too");
        assert!(to_a.iter().any(|m| m.contains("a.txt")));
    }

    #[test]
    fn buffer_text_is_not_echoed_to_its_author() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&a, Intent::EditBuffer { rel: "a.txt".into(), text: "typed".into() });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(
            !to_a.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "echoing text back stomps the author's cursor"
        );
        assert!(to_b.iter().any(|m| m.contains("typed")), "other clients must receive the text");
        // Guard against `broadcast_except`'s retain predicate being inverted:
        // that bug would also make `to_a` empty (by pruning `a` outright)
        // and would otherwise pass the assertions above undetected.
        assert!(
            to_a.iter().any(|m| m.contains(r#""t":"State""#)),
            "author must survive broadcast_except"
        );
        assert_eq!(h.subs.len(), 2, "skipping the originator must not prune it");
    }

    #[test]
    fn version_advances_on_change_only() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        drain(&rx);
        drain(&rx_other);
        let before = h.ws.version;
        h.handle(&c, Intent::ActivateTab { pane: proto::MIDDLE, idx: 9 }); // invalid
        assert_eq!(h.ws.version, before, "a rejected intent must not bump the version");
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"Error""#)));
        // An Error is the requesting client's business, not a broadcast: a
        // second subscriber must see nothing, or `send_to` could silently
        // regress into `broadcast` without any test catching it.
        assert!(
            !drain(&rx_other).iter().any(|m| m.contains(r#""t":"Error""#)),
            "an Error must go only to the client that sent the bad intent"
        );
    }

    #[test]
    fn save_conflict_is_reported_and_the_file_is_untouched() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        // buffer opened against different content => base_hash mismatch
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        drain(&rx);
        drain(&rx_other);
        h.handle(&c, Intent::SaveBuffer { rel: "a.txt".into(), force: false });
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"SaveConflict""#)));
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "on disk\n");
        // A conflict is the saving client's business, not everyone's: with
        // only one subscriber, `send_to` and `broadcast` are indistinguishable.
        assert!(
            !drain(&rx_other).iter().any(|m| m.contains(r#""t":"SaveConflict""#)),
            "a save conflict must go only to the client that tried to save"
        );
    }

    /// Regression for the bug reported live: a buffer with unsaved text came
    /// back from the state file with `base_hash` recomputed from *its own
    /// text*, so the save path compared the disk against the user's edit
    /// instead of against the content that edit was based on. Every save then
    /// reported a conflict, the file was never written, and the still-dirty
    /// buffer was persisted again — wedging that file permanently: the user
    /// saw their edits survive restarts while the file on disk never changed.
    ///
    /// The disk is deliberately left untouched across the restart here: with
    /// nothing having changed, a save has nothing to conflict *with*, so a
    /// conflict can only come from a manufactured base.
    /// "discard mine" on the conflict banner sends CloseBuffer, which dropped
    /// the buffer and stopped there — while the tab stayed open in Edit. The
    /// text a client is holding is only re-sent for buffers that *exist*
    /// (wsconn's connect path), so the discarded editor came back blank on
    /// the next reload, and until then went on showing the text that was
    /// supposedly discarded.
    ///
    /// Discarding means "show me what is on disk", so the file is re-read.
    #[test]
    fn discarding_a_buffer_reloads_the_file_rather_than_leaving_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("discard_probe", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.txt".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        drain(&rx);

        h.handle(&c, Intent::CloseBuffer { rel: "a.txt".into() });

        let b = h.ws.buffers.get("a.txt").expect("the tab is still open in Edit, so it needs text");
        assert_eq!(b.text, "on disk\n", "discarding shows the file, not the discarded edit");
        assert!(!b.dirty, "a freshly read buffer is not dirty");
        assert_eq!(b.base_hash, workspace::hash_text("on disk\n"), "and it knows its base");
        // The client that clicked must see it now, not on its next reload.
        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"BufferText""#) && m.contains("on disk")),
            "the reload has to reach the browser that discarded; got {msgs:?}"
        );
    }

    /// The other half: closing a buffer whose tab is gone must still free it.
    /// Without this the fix above would resurrect every buffer forever, and
    /// the MAX_BUFFERS cap along with the "a .env opened once is persisted
    /// with its contents" problem would both come back.
    #[test]
    fn discarding_a_buffer_with_no_tab_left_frees_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("discard_notab_probe", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        assert!(h.ws.buffers.contains_key("a.txt"));
        drain(&rx);

        h.handle(&c, Intent::CloseBuffer { rel: "a.txt".into() });
        assert!(!h.ws.buffers.contains_key("a.txt"), "no tab points at it, so it must go");
    }

    /// A restart reloads buffers from the state file, which records no
    /// staleness — it is a fact about the disk, not about the buffer. So the
    /// hub has to work it out at startup, or a buffer whose file changed
    /// while resh was down comes back looking clean and current, and the
    /// first warning is a conflict banner at save time.
    #[test]
    fn a_restart_notices_a_file_that_changed_while_it_was_down() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("dirty.txt"), "on disk\n").unwrap();
        std::fs::write(d.path().join("clean.txt"), "on disk\n").unwrap();
        std::fs::write(d.path().join("quiet.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("reconcile_probe", d.path().to_path_buf());
        let (c, _rx) = h.subscribe();
        for rel in ["dirty.txt", "clean.txt", "quiet.txt"] {
            h.handle(
                &c,
                Intent::OpenTab {
                    pane: proto::MIDDLE,
                    tab: Tab::File { rel: rel.into(), mode: Mode::Edit },
                },
            );
        }
        h.handle(&c, Intent::EditBuffer { rel: "dirty.txt".into(), text: "mine\n".into() });
        h.handle(&c, Intent::EditBuffer { rel: "quiet.txt".into(), text: "mine\n".into() });
        drop(h);

        // While it is "down": one file moves under an unsaved buffer, one
        // moves under a saved one, and one does not move at all.
        std::fs::write(d.path().join("dirty.txt"), "somebody else\n").unwrap();
        std::fs::write(d.path().join("clean.txt"), "somebody else\n").unwrap();

        let h2 = Hub::new("reconcile_probe", d.path().to_path_buf());
        let dirty = &h2.ws.buffers["dirty.txt"];
        assert!(dirty.stale, "an unsaved buffer whose file moved must come back flagged");
        assert_eq!(dirty.text, "mine\n", "and must keep the unsaved text, never adopt the file");

        // The live rule for a *clean* buffer is that it follows the file
        // (file_changed_externally); a restart must not be the one case where
        // it silently shows content the file no longer has.
        let clean = &h2.ws.buffers["clean.txt"];
        assert_eq!(clean.text, "somebody else\n", "a saved buffer follows the file");
        assert_eq!(clean.base_hash, workspace::hash_text("somebody else\n"));
        assert!(!clean.stale, "following the file is not staleness");

        // The discriminating case: flagging everything would pass both of the
        // assertions above.
        assert!(!h2.ws.buffers["quiet.txt"].stale, "a file nobody touched is not stale");
        assert_eq!(h2.ws.buffers["quiet.txt"].text, "mine\n");
    }

    /// "I could not read the file" is not "the file changed". A buffer whose
    /// file cannot be read must come back untouched rather than flagged on a
    /// guess — and must not take the hub's startup down with it.
    #[test]
    fn a_file_that_cannot_be_read_leaves_its_buffer_alone() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("gone.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("unreadable_probe", d.path().to_path_buf());
        let (c, _rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "gone.txt".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "gone.txt".into(), text: "mine\n".into() });
        drop(h);
        std::fs::remove_file(d.path().join("gone.txt")).unwrap();

        let h2 = Hub::new("unreadable_probe", d.path().to_path_buf());
        let b = &h2.ws.buffers["gone.txt"];
        assert_eq!(b.text, "mine\n", "unsaved work survives a file we cannot read");
        assert!(!b.stale, "cannot tell is not the same as changed");
    }

    #[test]
    fn a_dirty_buffer_still_saves_after_a_restart() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();

        let mut h = Hub::new("restart_probe", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.txt".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        assert!(h.ws.buffers["a.txt"].dirty, "the edit must have landed as unsaved text");
        drop(rx);
        drop(h);

        // The restart. Same project name and state dir, so this is the same
        // workspace coming back off disk.
        let mut h2 = Hub::new("restart_probe", d.path().to_path_buf());
        let (c2, rx2) = h2.subscribe();
        drain(&rx2);
        assert_eq!(h2.ws.buffers["a.txt"].text, "mine\n", "unsaved text is crash-safe");
        assert_eq!(
            h2.ws.buffers["a.txt"].base_hash,
            workspace::hash_text("on disk\n"),
            "the restored base must be what the edit was made against, not the edit itself"
        );

        h2.handle(&c2, Intent::SaveBuffer { rel: "a.txt".into(), force: false });
        let msgs = drain(&rx2);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"SaveOk""#)),
            "an untouched disk cannot conflict with anything; got {msgs:?}"
        );
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "mine\n");
    }

    #[test]
    fn set_mode_edit_reads_the_file_so_the_first_save_does_not_conflict() {
        // Regression for the bug reported live: switching to Edit used to
        // leave base_hash at its Default::default() of 0, which never
        // matches any real file, so the very first save always reported a
        // conflict and the file was never written.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "a.txt".into(), mode: Mode::Preview } },
        );
        drain(&rx);
        h.handle(&c, Intent::SetMode { rel: "a.txt".into(), mode: Mode::Edit });
        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| {
                m.contains(r#""t":"BufferText""#) && m.contains("on disk") && m.contains(r#""origin":"""#)
            }),
            "entering Edit must push the disk text with an empty origin, or the \
             requester's own echo rule drops it and the editor opens blank; got {msgs:?}"
        );
        assert_eq!(h.ws.buffers["a.txt"].base_hash, workspace::hash_text("on disk\n"));
        assert!(!h.ws.buffers["a.txt"].dirty, "a freshly-read buffer must not be marked dirty");

        // The whole point: an unmodified save against a real base_hash must
        // succeed, not conflict, now that the buffer actually knows what's
        // on disk.
        h.handle(&c, Intent::SaveBuffer { rel: "a.txt".into(), force: false });
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"SaveOk""#)));
    }

    /// The Edit-mode disk read has to follow the tab that actually landed.
    /// `apply_layout` coerces `OpenTab{mode:Edit}` on a PNG to Preview, but
    /// this dispatch used to match the *intent*, so the read still ran and
    /// seeded a buffer for a tab with no editor. Not cosmetic: the read
    /// stores `String::from_utf8_lossy`, so that buffer's text is not the
    /// file's bytes — every non-UTF-8 byte has become U+FFFD — and a save
    /// against it rewrites the image with the replacement characters.
    ///
    /// The .txt half is the discriminating half: without it this passes with
    /// the dispatch deleted outright.
    ///
    /// Confirmed by restoring the pre-fix arm
    /// `Intent::OpenTab { tab: Tab::File { rel, mode: Mode::Edit }, .. } =>
    /// self.open_for_edit(from, rel)` and running this test: it failed at
    /// "a coerced tab must not get a buffer" — the buffer was there, holding
    /// the lossy text of the PNG.
    #[test]
    fn a_coerced_open_does_not_read_the_file_it_could_not_edit() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        // Real PNG magic: bytes that are not valid UTF-8, so from_utf8_lossy
        // would visibly corrupt them.
        std::fs::write(d.path().join("shot.png"), [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
            .unwrap();
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "shot.png".into(), mode: Mode::Edit },
            },
        );
        assert!(
            !h.ws.buffers.contains_key("shot.png"),
            "a coerced tab must not get a buffer: {:?}",
            h.ws.buffers.keys().collect::<Vec<_>>()
        );

        // A tab that really did open in Edit must still be read, or this test
        // would pass with the dispatch removed altogether.
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.txt".into(), mode: Mode::Edit },
            },
        );
        assert_eq!(h.ws.buffers["a.txt"].text, "on disk\n");
    }

    #[test]
    fn set_mode_edit_does_not_clobber_an_already_dirty_buffer() {
        // Reactivating an in-progress edit (e.g. the tab reopened from a
        // second browser) must not silently discard unsaved text by
        // re-reading the file out from under it.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "a.txt".into(), mode: Mode::Edit } },
        );
        drain(&rx);
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "unsaved work".into() });
        drain(&rx);
        assert!(h.ws.buffers["a.txt"].dirty);

        h.handle(&c, Intent::SetMode { rel: "a.txt".into(), mode: Mode::Edit });
        let msgs = drain(&rx);
        assert_eq!(h.ws.buffers["a.txt"].text, "unsaved work", "dirty text must survive");
        assert!(h.ws.buffers["a.txt"].dirty, "SetMode must not clear dirty for unsaved work");
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"BufferText""#) && m.contains("unsaved work")),
            "the requester must still get the current (unsaved) text, not blank; got {msgs:?}"
        );
    }

    #[test]
    fn closing_a_clean_file_tab_drops_its_buffer_but_a_dirty_one_survives() {
        // A1: nothing else in the app frees a buffer on the common path, so
        // without this every file ever opened in Edit lingers — with its
        // full text — in the persisted state file forever, including things
        // like a .env opened once.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("clean.txt"), "on disk\n").unwrap();
        std::fs::write(d.path().join("dirty.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("a1_close_buffer", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "clean.txt".into(), mode: Mode::Edit } },
        );
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "dirty.txt".into(), mode: Mode::Edit } },
        );
        drain(&rx);
        assert!(h.ws.buffers.contains_key("clean.txt"));
        assert!(h.ws.buffers.contains_key("dirty.txt"));

        h.handle(&c, Intent::EditBuffer { rel: "dirty.txt".into(), text: "unsaved".into() });
        drain(&rx);
        assert!(h.ws.buffers["dirty.txt"].dirty);

        // Tabs are [clean.txt, dirty.txt] in that pane; close index 0.
        h.handle(&c, Intent::CloseTab { pane: proto::MIDDLE, idx: 0 });
        assert!(
            !h.ws.buffers.contains_key("clean.txt"),
            "a closed, clean, unreferenced buffer must not linger"
        );
        assert!(h.ws.buffers.contains_key("dirty.txt"), "closing a different tab must not touch it");

        h.handle(&c, Intent::CloseTab { pane: proto::MIDDLE, idx: 0 });
        assert!(
            h.ws.buffers.contains_key("dirty.txt"),
            "unsaved work must survive closing its tab — only an explicit \
             save or the conflict banner's 'discard mine' may drop it"
        );
    }

    #[test]
    fn closing_a_tab_still_referenced_elsewhere_keeps_the_buffer() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("shared.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("a1_shared_buffer", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        // A Diff tab for the same rel, in a different pane, still counts as
        // "references that rel" even though it doesn't read the buffer
        // directly — closing the Edit tab must not drop the buffer under it.
        h.handle(&c, Intent::OpenTab { pane: proto::LEFT_TOP, tab: Tab::Diff { rel: Some("shared.txt".into()) } });
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "shared.txt".into(), mode: Mode::Edit } },
        );
        drain(&rx);
        assert!(h.ws.buffers.contains_key("shared.txt"));

        h.handle(&c, Intent::CloseTab { pane: proto::MIDDLE, idx: 0 });
        assert!(
            h.ws.buffers.contains_key("shared.txt"),
            "another pane still references this rel; the buffer must survive"
        );
    }

    #[test]
    fn rename_rekeys_the_buffer_and_the_open_tab_so_a_later_save_still_works() {
        // A3: before this fix, a rename left the buffer and every tab
        // pointing at the old rel — only TreeChanged was broadcast — so the
        // next save against that tab failed safe_resolve with "not found",
        // silently (per A2), and unsaved text became permanently unsaveable.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("old.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("a3_rename_file", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "old.txt".into(), mode: Mode::Edit } },
        );
        drain(&rx);
        h.handle(&c, Intent::EditBuffer { rel: "old.txt".into(), text: "unsaved work".into() });
        drain(&rx);
        assert!(h.ws.buffers["old.txt"].dirty);

        h.handle(&c, Intent::RenamePath { from: "old.txt".into(), to: "new.txt".into() });
        let msgs = drain(&rx);
        assert!(msgs.iter().any(|m| m.contains(r#""t":"TreeChanged""#)));
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"State""#)),
            "a rename must also broadcast State, or clients keep showing the tab at the old path"
        );

        assert!(!h.ws.buffers.contains_key("old.txt"), "the old key must not linger");
        assert_eq!(h.ws.buffers["new.txt"].text, "unsaved work", "unsaved text must survive the rename");
        assert!(h.ws.buffers["new.txt"].dirty);
        assert_eq!(
            h.ws.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "new.txt".into(), mode: Mode::Edit },
            "the open tab must follow the file, not keep pointing at a name that no longer exists"
        );

        // The whole point: a save against the new rel must actually work —
        // before the fix this failed with "not found" because the buffer
        // and tab still pointed at "old.txt".
        h.handle(&c, Intent::SaveBuffer { rel: "new.txt".into(), force: true });
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"SaveOk""#)));
        assert_eq!(std::fs::read_to_string(d.path().join("new.txt")).unwrap(), "unsaved work");
    }

    #[test]
    fn renaming_a_directory_rewrites_buffers_and_tabs_for_the_whole_subtree() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        // A sibling that merely starts with the same characters must not be
        // touched — this is the `/`-boundary case `has_prefix_boundary` exists for.
        std::fs::create_dir(d.path().join("src2")).unwrap();
        std::fs::write(d.path().join("src2/b.rs"), "fn b() {}\n").unwrap();

        let mut h = Hub::new("a3_rename_dir", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "src/a.rs".into(), mode: Mode::Edit } },
        );
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::RIGHT, tab: Tab::File { rel: "src2/b.rs".into(), mode: Mode::Edit } },
        );
        drain(&rx);
        h.handle(&c, Intent::EditBuffer { rel: "src/a.rs".into(), text: "unsaved a".into() });
        drain(&rx);

        h.handle(&c, Intent::RenamePath { from: "src".into(), to: "lib".into() });
        drain(&rx);

        assert!(!h.ws.buffers.contains_key("src/a.rs"));
        assert_eq!(h.ws.buffers["lib/a.rs"].text, "unsaved a");
        assert_eq!(
            h.ws.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "lib/a.rs".into(), mode: Mode::Edit }
        );
        // default_layout seeds RIGHT with a Terminal tab already, so the
        // opened file lands at index 1, not 0.
        assert_eq!(
            h.ws.panes[proto::RIGHT as usize].tabs[1],
            Tab::File { rel: "src2/b.rs".into(), mode: Mode::Edit },
            "\"src2\" is not \"src\" followed by a '/' boundary and must be untouched"
        );
        assert!(h.ws.buffers.contains_key("src2/b.rs"));
    }

    #[test]
    fn for_project_returns_promptly_on_a_large_tree() {
        // The bug reported live: `for_project` used to walk the entire
        // project tree, registering an OS watch per directory, on the
        // connection thread — before the client's first `State` snapshot
        // could be sent. On a project with thousands of directories that
        // made the workspace pane stay empty indefinitely. Watcher setup
        // must now happen off this thread, so `for_project` returns almost
        // immediately no matter how large the tree is.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sd = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", sd.path());

        let d = tempfile::tempdir().unwrap();
        // A few thousand directories is enough to make the old synchronous
        // walk take a noticeable amount of time without making the test
        // itself slow.
        for i in 0..4000 {
            std::fs::create_dir(d.path().join(format!("dir{i}"))).unwrap();
        }

        let start = std::time::Instant::now();
        let _hub = Hub::for_project("large_tree_project", d.path().to_path_buf());
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "for_project must return promptly regardless of project size; took {elapsed:?}"
        );

        std::env::remove_var("RESH_STATE_DIR");
    }

    #[test]
    fn mark_all_notices_read_flags_dirty_and_updates_the_store() {
        // Regression for the missing "mark all read" plumbing: the panel
        // needs a way to clear every notice at once without one round trip
        // per id, and — like MarkNoticeRead — it must flag notices_dirty
        // rather than call broadcast_all itself, since that would try to
        // lock this very hub a second time.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        crate::notify::reset_for_test();
        crate::notify::record("proj", "claude", crate::osc::Parsed { title: None, body: "one".into() });
        crate::notify::record("proj", "shell", crate::osc::Parsed { title: None, body: "two".into() });

        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        assert!(!h.notices_dirty);
        h.handle(&c, Intent::MarkAllNoticesRead);
        assert!(h.notices_dirty, "the socket layer relies on this flag to broadcast Notices");
        assert!(
            crate::notify::list().iter().all(|n| n.read),
            "the store itself must be updated, not just the dirty flag"
        );
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drop(rx_b);
        h.handle(&a, Intent::Resize { sizes: proto::Sizes::default() });
        assert_eq!(h.subs.len(), 1, "a closed socket must not accumulate");
        drop(rx_a);
    }

    /// The + button no longer asks for a name, so the server must hand out an
    /// unused one. `term` then `term1`: without the `also_taken` check, the
    /// second click sees no *live* session yet (the PTY only spawns when the
    /// browser connects to /ws/.../term/<name>) and hands out `term` twice.
    // Two subscribers, deliberately: with one, `broadcast` and `send_to` are
    // indistinguishable, and a toggle that reached only the clicking browser
    // would look correct in every single-client test while the other tab sat
    // showing a tree that no longer matches what the server renders.
    #[test]
    fn toggling_hidden_files_reaches_every_client_and_survives_a_reload() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("show_hidden_mirror", d.path().to_path_buf());
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);
        let before = h.ws.version;

        h.handle(&b, Intent::SetShowHidden { on: true });

        assert_eq!(h.ws.show_hidden, Some(true));
        assert!(h.ws.version > before, "a mirrored change must bump the version");
        for (who, msgs) in [("the other client", drain(&rx_a)), ("the clicking client", drain(&rx_b))] {
            assert!(
                msgs.iter().any(|m| m.contains(r#""show_hidden":true"#)),
                "{who} must receive the new value; got {msgs:?}"
            );
        }

        // Persisted, not just held in memory: a reload builds a fresh hub.
        let (reloaded, _) = crate::wsstate::load("show_hidden_mirror");
        assert_eq!(reloaded.show_hidden, Some(true), "the toggle must survive a restart");
        std::env::remove_var("RESH_STATE_DIR");
    }

    // The tree fragment asks this on every render, including for projects
    // nobody has opened. Answering must not build a hub — that would start a
    // filesystem watcher as a side effect of a question.
    #[test]
    fn asking_for_the_override_never_creates_a_hub() {
        assert_eq!(Hub::show_hidden("no_such_project_asked_about"), None);
        let registered = REGISTRY
            .get()
            .map(|r| r.lock().unwrap_or_else(|e| e.into_inner()).contains_key("no_such_project_asked_about"))
            .unwrap_or(false);
        assert!(!registered, "asking must not have registered a hub");
    }

    #[test]
    fn new_terminal_allocates_successive_unused_names() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("newterm_names", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // default_layout seeds RIGHT with a Terminal tab; clear it so the
        // names under test are the only ones in play.
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT });
        drain(&rx);

        let names: Vec<String> = h.ws.panes[proto::RIGHT as usize]
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Terminal { session } => Some(session.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["term", "term1"], "each click must get its own shell");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Ending a session is what makes closing a tab reclaim a slot; the tab
    /// must go from every pane, and every mirrored browser must be told.
    #[test]
    fn end_session_clears_the_tab_everywhere_and_broadcasts() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("endsession_hub", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_c2, rx_other) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::Terminal { session: "term".into() } },
        );
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::RIGHT, tab: Tab::Terminal { session: "term".into() } },
        );
        drain(&rx);
        drain(&rx_other);

        h.handle(&c, Intent::EndSession { session: "term".into() });

        let remaining: Vec<&Tab> = h.ws.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            !remaining.iter().any(|t| matches!(t, Tab::Terminal { session } if session == "term")),
            "the ended session must not keep a tab in any pane"
        );
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(
            msgs_other.iter().any(|m| m.contains(r#""t":"State""#)),
            "a second browser must be told its terminal tab is gone"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    #[test]
    fn close_project_is_refused_while_a_buffer_is_dirty() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "disk\n").unwrap();
        let mut h = Hub::new("closeproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // Second, mirrored client: proves CloseRefused is this requester's
        // business only. Without this subscriber, `send_to` accidentally
        // regressing into `broadcast` (leaking one client's conflict to
        // every mirrored browser) would pass undetected — see
        // `version_advances_on_change_only` and `save_conflict_is_reported...`
        // for the two earlier occurrences of this exact gap in this file.
        let (_other, rx_other) = h.subscribe();
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "unsaved".into() });
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}

        h.handle(&c, Intent::CloseProject);
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"CloseRefused""#) && m.contains("a.txt")),
            "unsaved work must block a close and name the file"
        );
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"ProjectClosed""#)),
            "nothing may be ended while work is unsaved"
        );
        assert!(
            !msgs_other.iter().any(|m| m.contains(r#""t":"CloseRefused""#)),
            "a close conflict is the requester's business, not every mirrored browser's"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    #[test]
    fn close_project_with_clean_buffers_reports_what_it_ended() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("closeclean", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // A second subscriber must also learn the project closed — this is
        // the mirror-everyone-but-CloseRefused half of the routing contract;
        // without it, `broadcast` accidentally regressing into `send_to`
        // (mirrors never learning the project closed) would pass undetected.
        let (_other, rx_other) = h.subscribe();
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}
        h.handle(&c, Intent::CloseProject);
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(msgs.iter().any(|m| m.contains(r#""t":"ProjectClosed""#)));
        assert!(
            msgs_other.iter().any(|m| m.contains(r#""t":"ProjectClosed""#)),
            "ProjectClosed must be broadcast, not sent only to the requester"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    // I2: `handle(CloseProject)` must return promptly, spawning the actual
    // session-killing rather than doing it inline under the hub lock —
    // `session::kill_project` confirms every session's dtach master is
    // really dead, a bounded poll that shells out to `ps`/`kill` and can
    // take real wall time even for one session. Built through `for_project`
    // (not a bare `Hub::new()`, which deliberately takes the synchronous
    // fallback — see `do_close_project`'s doc comment) so `self_ref`
    // resolves and the code path under test is the one production and every
    // WS-level test actually uses.
    //
    // Measures `handle`'s own elapsed time directly, rather than inferring
    // it from client-visible WebSocket response ordering (tried first,
    // rejected: each connection's outgoing messages are pipelined through
    // their own writer path, so response arrival order isn't a reliable
    // proxy for how long the hub lock was actually held — confirmed by
    // reverting the fix and finding that version of the test still passed).
    //
    // The 50ms bound was chosen by measuring both sides directly: reverting
    // to the pre-fix inline `kill_project` call (one real session) measured
    // 100-125ms across five runs on this machine, since even a fast SIGKILL
    // still pays for at least two `ps` subprocess spawns and one `kill`
    // spawn; the fixed code's own work here (check dirty, clone two
    // `String`s, spawn a thread) does no process I/O and measures well
    // under 1ms. 50ms sits with wide margin on both sides of that gap.
    #[test]
    fn close_project_returns_promptly_without_blocking_on_session_killing() {
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_CMD"); // real dtach: a `cat` client has no master to wait on
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let project_dir = tempfile::tempdir().unwrap();

        let hub = Hub::for_project("closepromptly", project_dir.path().to_path_buf());
        let (c, _rx) = Hub::lock(&hub).subscribe();

        let Ok(_att) = crate::session::attach("closepromptly", "shell", project_dir.path()) else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("RESH_STATE_DIR");
            return;
        };

        let start = std::time::Instant::now();
        Hub::lock(&hub).handle(&c, Intent::CloseProject);
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "handle(CloseProject) must return promptly rather than kill sessions inline \
             under the hub lock — took {elapsed:?}"
        );

        std::env::remove_var("RESH_STATE_DIR");
    }

    // I4: without a guard, the window between do_close_project spawning its
    // kill thread and that thread finishing is unprotected — a
    // StartTerminal landing in it would let the browser's follow-up connect
    // to /ws/{project}/term/{session} (term.rs) spawn a brand-new session
    // while kill_project is still tearing down the old ones, and
    // kill_and_unlink would then find and SIGKILL that new session's master
    // too. Reliable without polling or sleeping: handle(CloseProject)
    // itself returns in well under 1ms (see close_project_returns_promptly,
    // above — it only spawns a thread), while the real kill it kicks off
    // takes on the order of 100ms (real ps/kill subprocess spawns), so the
    // very next `handle` call on the same thread is certain to land while
    // `closing` is still true.
    #[test]
    fn start_terminal_is_refused_while_a_close_is_in_flight() {
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_CMD"); // real dtach: needs a kill that takes real wall time
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let project_dir = tempfile::tempdir().unwrap();

        let hub = Hub::for_project("closeinflight", project_dir.path().to_path_buf());
        let (c, rx) = Hub::lock(&hub).subscribe();

        let Ok(_att) = crate::session::attach("closeinflight", "shell", project_dir.path()) else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("RESH_STATE_DIR");
            return;
        };
        while rx.try_recv().is_ok() {}

        Hub::lock(&hub).handle(&c, Intent::CloseProject);
        // The same window, as `term.rs` sees it: refusing the intent only
        // stops a browser that asks first, and it is the connect to
        // /ws/{project}/term/{name} that actually spawns the PTY — a mirrored
        // tab reconnects straight there with no intent at all. That path has
        // nothing but this to check.
        assert!(
            Hub::is_closing("closeinflight"),
            "term.rs's own guard must be able to observe the in-flight close"
        );
        assert!(
            !Hub::is_closing("no-such-project-ever"),
            "a project with no hub cannot be closing"
        );
        // Asking must not *create* a hub the way `for_project` does — that
        // would start a filesystem watcher as a side effect of a question,
        // for a directory nobody asked about.
        let created = REGISTRY
            .get()
            .map(|r| {
                r.lock().unwrap_or_else(|e| e.into_inner()).contains_key("no-such-project-ever")
            })
            .unwrap_or(false);
        assert!(!created, "is_closing must not register a hub for a project it was merely asked about");
        Hub::lock(&hub).handle(&c, Intent::StartTerminal { session: "fresh".into() });

        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("closing")),
            "StartTerminal must be refused while a close is in flight; got: {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)),
            "a refused StartTerminal must not also announce the tab as started; got: {msgs:?}"
        );

        std::env::remove_var("RESH_STATE_DIR");
    }

    #[test]
    fn start_terminal_rejects_an_invalid_session_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("startproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // A second, mirrored client must never see this client's own Error.
        let (_other, rx_other) = h.subscribe();
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}
        h.handle(&c, Intent::StartTerminal { session: "bad name;rm".into() });
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(msgs.iter().any(|m| m.contains(r#""t":"Error""#)));
        assert!(!msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)));
        assert!(
            !msgs_other.iter().any(|m| m.contains(r#""t":"Error""#)),
            "an invalid session name is the requester's business, not every mirrored browser's"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    #[test]
    fn init_git_creates_a_repo_and_flips_is_git_for_every_client() {
        // Entirely unverified before this test: the handler could be
        // deleted and every other test would still pass. GitInit and the
        // is_git flip are exactly what the terminal/tree placeholder in the
        // client depends on to know a repo now exists.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("initgit", d.path().to_path_buf());
        assert!(!h.ws.is_git, "a fresh project must not already look like a git repo");
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}

        h.handle(&c, Intent::InitGit);
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"GitInit""#) && m.contains(r#""ok":true"#)),
            "got {msgs:?}"
        );
        assert!(
            msgs_other.iter().any(|m| m.contains(r#""t":"GitInit""#) && m.contains(r#""ok":true"#)),
            "GitInit must be broadcast, not sent only to the requester"
        );
        assert!(d.path().join(".git").exists(), "git init must actually create the repo on disk");
        assert!(h.ws.is_git, "refresh_live_sessions must flip is_git once .git exists");
        std::env::remove_var("RESH_STATE_DIR");
    }
}
