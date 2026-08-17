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
    /// Paths deadlight itself just wrote, with the resulting hash. The watcher
    /// (Task 8) drops matching events so a save does not echo back.
    pub self_writes: HashMap<String, u64>,
    /// Set once a filesystem watcher has been spawned for this hub, so
    /// `for_project` starts at most one watcher per project even though it
    /// runs on every connection.
    pub watching: bool,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<Hub>>>>> = OnceLock::new();

impl Hub {
    pub fn new(project: &str, dir: std::path::PathBuf) -> Hub {
        let (ws, warn) = crate::wsstate::load(project);
        if let Some(w) = warn {
            eprintln!("deadlight: {w}");
        }
        let mut hub = Hub {
            project: project.to_string(),
            dir,
            ws,
            subs: HashMap::new(),
            next_id: 0,
            self_writes: HashMap::new(),
            watching: false,
        };
        // A freshly loaded hub must report reality, not whatever the
        // persisted layout happened to say last time: sessions may have
        // died (or kept running under dtach) since the last save, and
        // `.git` may have appeared or vanished on disk in the meantime.
        hub.refresh_live_sessions();
        hub
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
            .or_insert_with(|| Arc::new(Mutex::new(Hub::new(project, dir.clone()))))
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
            let ms: u64 = std::env::var("DEADLIGHT_DEBOUNCE_MS")
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
            eprintln!("deadlight: state save failed: {e}");
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
                    Intent::OpenTab { tab: Tab::File { rel, mode: Mode::Edit }, .. } => {
                        self.open_for_edit(from, rel)
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
                let diff_html = crate::render::diff_html(&conflict_diff(&disk_text, &buf.text));
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
        self.ws.is_git = self.dir.join(".git").exists();
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
    fn do_close_project(&mut self, from: &ConnId) {
        let dirty: Vec<String> =
            self.ws.buffers.iter().filter(|(_, b)| b.dirty).map(|(r, _)| r.clone()).collect();
        if !dirty.is_empty() {
            let ev = Event::CloseRefused { dirty };
            return self.send_to(from, &ev);
        }
        let ended = crate::session::kill_project(&self.project);
        self.ws.version += 1;
        self.broadcast(&Event::ProjectClosed { ended });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
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
fn conflict_diff(disk: &str, buf: &str) -> String {
    let mut out = String::from("--- a/disk\n+++ b/your buffer\n@@ conflict @@\n");
    for l in disk.lines() {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in buf.lines() {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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

    #[test]
    fn set_mode_edit_reads_the_file_so_the_first_save_does_not_conflict() {
        // Regression for the bug reported live: switching to Edit used to
        // leave base_hash at its Default::default() of 0, which never
        // matches any real file, so the very first save always reported a
        // conflict and the file was never written.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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

    #[test]
    fn set_mode_edit_does_not_clobber_an_already_dirty_buffer() {
        // Reactivating an in-progress edit (e.g. the tab reopened from a
        // second browser) must not silently discard unsaved text by
        // re-reading the file out from under it.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());

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

        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drop(rx_b);
        h.handle(&a, Intent::Resize { sizes: proto::Sizes::default() });
        assert_eq!(h.subs.len(), 1, "a closed socket must not accumulate");
        drop(rx_a);
    }

    #[test]
    fn close_project_is_refused_while_a_buffer_is_dirty() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn close_project_with_clean_buffers_reports_what_it_ended() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn start_terminal_rejects_an_invalid_session_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn init_git_creates_a_repo_and_flips_is_git_for_every_client() {
        // Entirely unverified before this test: the handler could be
        // deleted and every other test would still pass. GitInit and the
        // is_git flip are exactly what the terminal/tree placeholder in the
        // client depends on to know a repo now exists.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }
}
