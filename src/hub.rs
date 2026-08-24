//! One Hub per project: owns the Workspace, the subscriber list, and the
//! dispatch from intent to either a pure transition or a file operation.
//! Everything the sockets do goes through here, so mirroring is automatic.
use crate::proto::{Event, Intent, Mode, Tab};
use crate::workspace::{self, Workspace};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

pub type ConnId = String;

/// The two sides of one open proposal. Kept whole rather than as a rendered
/// diff: `routes.rs`'s `/frag/{project}/proposal` fragment renders from this
/// on every fetch (so a browser's Accept edit-box, once Task 9 adds one,
/// always compares against the same text the diff was built from), and the
/// accepting path has to be able to send back text the user edited, which a
/// rendered diff cannot supply either way.
#[derive(Clone)]
pub struct ProposalSides {
    pub rel: String,
    pub old_text: String,
    pub new_text: String,
}

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
    /// Both sides of every proposal whose tab is currently open, keyed by the
    /// same id as the tab.
    ///
    /// This is what a browser that connects *after* a proposal opened is
    /// shown. Without it, `Event::Proposal` goes out exactly once, to whoever
    /// happened to be connected, and a second browser renders a
    /// `Tab::Proposal` it has no content for — and can still click Accept,
    /// which is agreeing to a change nobody was shown. This project's whole
    /// conflict-guard stance is that a human sees what they are agreeing to.
    ///
    /// Deliberately on the `Hub` and not in `Workspace`: it is not layout, and
    /// it must never reach the state file — a proposal does not survive a
    /// restart (see `workspace::drop_dead_tabs`). Bounded by `ide::MAX_PENDING`
    /// per project, since an entry exists only while a pending request does.
    pub proposals: HashMap<String, ProposalSides>,
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
            proposals: HashMap::new(),
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
    /// so the I/O here has to stay bounded — but not by `MAX_BUFFERS` itself:
    /// that cap only ever applies to *dirty* buffers on the live insert path
    /// (`open_buffer_for`), so nothing stops a session from accumulating far
    /// more clean ones by previewing files. What actually bounds `self.ws.buffers`
    /// here is `wsstate::load`, which this hub was just built from: every
    /// dirty buffer it restored, plus at most `MAX_BUFFERS` clean ones. Every
    /// one of those files' contents the state file this call just parsed
    /// already carried, so this at most doubles the reading `new` was doing
    /// anyway — it does not, on its own, bound the *count*.
    fn reconcile_buffers_with_disk(&mut self) {
        let dir = &self.dir;
        for (rel, b) in self.ws.buffers.iter_mut() {
            let Ok(disk) = crate::projects::safe_resolve(dir, rel)
                .and_then(|p| crate::projects::read_text_file(&p))
            else {
                continue;
            };
            let hash = workspace::hash_text(&disk);
            if b.dirty() {
                b.stale = hash != b.base_hash;
            } else if hash != b.base_hash {
                // The file moved under a clean buffer, which holds nothing to
                // update — the new content just is the disk, so the buffer
                // only needs its base hash to agree with it.
                b.content = workspace::Content::Clean;
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
                    // Started here, inside the "only on a fresh entry" arm,
                    // rather than on first connection: the lock file must
                    // exist before a terminal is spawned, since the spawn
                    // reads the port out of the ide registry
                    // (session::session_env). This runs exactly once per
                    // project — a reconnecting browser or a second terminal
                    // takes the `entry` hit path above and never reaches
                    // this closure at all — so it never hands out a second
                    // listener. It runs with the hub *registry* lock held,
                    // same as `Hub::new` just above it on this same line:
                    // that call already does blocking file I/O (wsstate,
                    // buffer reconciliation) under this same lock for the
                    // same one-time-setup reason, so this does not introduce
                    // a new class of hold. Degrades silently on failure (see
                    // `ide::for_project_in`): IDE integration is a
                    // convenience, never a reason to fail opening a project.
                    crate::ide::for_project(project, dir.clone());
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
            Intent::NewTerminal { pane, launch } => return self.do_new_terminal(from, *pane, *launch),
            Intent::OpenPath { text } => return self.do_open_path(from, text.clone()),
            Intent::MentionPath { rel, line_start, line_end, session } => {
                return self.do_mention_path(
                    from,
                    rel.clone(),
                    *line_start,
                    *line_end,
                    session.clone(),
                )
            }
            Intent::ShareSelection { rel, text, start_line, start_col, end_line, end_col } => {
                return self.do_share_selection(
                    from,
                    rel.clone(),
                    text.clone(),
                    (*start_line, *start_col),
                    (*end_line, *end_col),
                )
            }
            Intent::AnswerProposal { id, accept, text } => {
                return self.do_answer_proposal(from, id.clone(), *accept, text.clone())
            }
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
        // Same reason as `closing_rel` above: `apply_layout` has removed the
        // tab by the time the answer has to be sent, so the id it carried has
        // to be read out first.
        let closing_proposal: Option<String> = match &intent {
            Intent::CloseTab { pane, idx } => self
                .ws
                .panes
                .get(*pane as usize)
                .and_then(|p| p.tabs.get(*idx))
                .and_then(|t| match t {
                    Tab::Proposal { id } => Some(id.clone()),
                    _ => None,
                }),
            _ => None,
        };
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                // Entering Edit mode is the server's cue to become this
                // buffer's owner: read the file now, so base_hash
                // reflects what's actually on disk. Without this, a buffer
                // opened purely client-side (the old /frag/raw flow) never
                // got a real base_hash and every first save reported a
                // conflict against content it never compared against.
                match &intent {
                    Intent::SetMode { rel, mode: Mode::Edit } => {
                        self.open_buffer_for(from, rel, Mode::Edit, true)
                    }
                    // Dispatched off the COERCED tab, not off the intent:
                    // apply_layout may have forced Edit to Preview, and
                    // reading an image anyway would seed a buffer for it. That
                    // buffer is not harmless — the read stores
                    // String::from_utf8_lossy, so its text is not the file's
                    // bytes, and a later save (however unreachable through the
                    // UI) would write U+FFFD over the original. A File tab in
                    // either mode otherwise gets a buffer: it is what records
                    // the base a previewed file was opened against, and what
                    // makes the watcher find it (Workspace::open_file_rels is
                    // keyed off tabs, not buffers, so this is not what shows a
                    // preview — only what lets one later become an edit
                    // without a fresh disk read).
                    Intent::OpenTab { tab, .. } => {
                        if let Tab::File { rel, mode } = workspace::coerce_tab(tab) {
                            // The buffer decides, not the tab. apply_layout
                            // returns Ok(true) both for a new tab and for a
                            // bare activation of one already open (find_tab
                            // hit), and this guard does not tell those apart:
                            // whenever a clean, non-stale buffer already
                            // exists for this rel, the read is skipped — a
                            // genuinely new tab on a rel some other pane
                            // already has open included. That is the intended
                            // reading, because what the read would produce is
                            // a function of the rel, not of the tab: the
                            // buffer's base still agrees with the file, so
                            // there is nothing to learn and nothing new to
                            // tell anyone — every subscriber already has this
                            // text, from the read that first created the
                            // buffer or from wsconn's connect-time replay for
                            // anyone who joined since. Without the guard,
                            // clicking back to a tab costs a fresh disk read
                            // (up to 2 MB, under the hub lock) plus a
                            // whole-file broadcast, for a pane whose content
                            // Preview doesn't even take from the buffer (it's
                            // fetched over HTTP). A dirty or stale buffer
                            // still needs the real call: dirty because a
                            // second browser reopening an in-progress edit
                            // needs its own re-broadcast, stale because "the
                            // file moved under this" is exactly a reason to
                            // look again.
                            let reactivating_a_settled_buffer = self
                                .ws
                                .buffers
                                .get(&rel)
                                .is_some_and(|b| !b.dirty() && !b.stale);
                            if !reactivating_a_settled_buffer && !crate::routes::refuses_text_edit(&rel)
                            {
                                self.open_buffer_for(from, &rel, mode, false)
                            }
                        }
                    }
                    // Closing a File tab is the only way a buffer is ever
                    // freed short of the conflict banner's "discard mine" —
                    // without this, every file a user so much as previews
                    // would keep a buffer (and its base_hash) around
                    // forever. Harmless for a clean one — wsstate::save skips
                    // its text — but still worth reclaiming: it's what a .env
                    // opened once in Edit would otherwise persist its full
                    // text under for as long as the tab stayed open.
                    Intent::CloseTab { .. } => {
                        if let Some(rel) = &closing_rel {
                            self.maybe_drop_buffer(rel);
                        }
                        // Closing the tab *is* an answer — the spec's third
                        // row, "reject, or close the tab". Without this, the
                        // tab vanishes and Claude stays blocked on a request
                        // with nothing left in the UI that could ever answer
                        // it.
                        if let Some(id) = &closing_proposal {
                            if let Err(e) =
                                crate::ide::answer(&self.project, id, crate::ide::Answer::Rejected)
                            {
                                eprintln!("resh: closing proposal {id}: {e}");
                            }
                            // `apply_layout` removed the tab, not the content
                            // behind it.
                            self.proposals.remove(id);
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
                            self.open_buffer_for(from, rel, Mode::Edit, false);
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

    /// Establishes a buffer's base — reads the file, records its hash — the
    /// moment a File tab opens, in either mode, and tells every
    /// client what's in it. Not just an Edit-mode thing despite the old name:
    /// a previewed file needs a base too, since that is what lets it flip to
    /// Edit later without a fresh disk read, and it is what the watcher's
    /// stale/conflict tracking keys off. Skips the disk read when the buffer
    /// is already dirty: reactivating an in-progress edit (the tab getting
    /// reopened, e.g. by a second browser) must never clobber unsaved text
    /// with what's on disk — only `SaveBuffer`/`CloseBuffer` may do that.
    ///
    /// Callers keep images out: hashing `read_text_file`'s lossy-UTF-8 bytes
    /// would be meaningless, and the watcher reaches them through
    /// `Workspace::open_file_rels`, which is keyed off tabs, not buffers.
    ///
    /// `mode` decides only what a *failed* read is worth saying. In Edit the
    /// read is the editor's whole content, so a refusal has to be reported or
    /// the user gets a blank textarea with no explanation. In Preview the read
    /// only records a base for a later edit — the pane itself is painted from
    /// `/frag/raw` over HTTP — so a `.pdf`, a `.zip` or a 3 MB log previews
    /// perfectly well with no buffer at all, and a banner on that everyday
    /// tree click would be reporting a failure the user neither caused nor
    /// can act on.
    /// Puts every Edit tab on `rel` back into Preview. Returns whether any
    /// tab actually moved — false means nothing referenced it in Edit, in
    /// which case the caller still owes the client an explanation.
    fn demote_to_preview(&mut self, rel: &str) -> bool {
        let mut moved = false;
        for p in self.ws.panes.iter_mut() {
            for t in p.tabs.iter_mut() {
                if let crate::proto::Tab::File { rel: r, mode } = t {
                    if r == rel && *mode == Mode::Edit {
                        *mode = Mode::Preview;
                        moved = true;
                    }
                }
            }
        }
        moved
    }

    /// `requested` distinguishes a user pressing ✎ from a tab defaulting into
    /// Edit because that is how text files open now. Both demote to Preview
    /// when the file turns out not to be readable as text; only the first is
    /// worth a banner, because only there did somebody ask for something the
    /// system could not do.
    fn open_buffer_for(&mut self, from: &ConnId, rel: &str, mode: Mode, requested: bool) {
        let already_dirty = self.ws.buffers.get(rel).map(|b| b.dirty()).unwrap_or(false);
        // The text to broadcast below. A freshly-read buffer holds nothing
        // (Content::Clean), so it cannot be read back out of the buffer the
        // way a stored `text` field would allow — the disk read's own local
        // `text` is threaded through directly instead. `None` here means
        // already_dirty was true, so the fallback after the `if` picks up
        // the buffer's own unsaved edit.
        let mut freshly_read: Option<String> = None;
        if !already_dirty {
            // Mirrors workspace.rs's EditBuffer cap: the ceiling is on unsaved
            // edits, not on how many files have ever been looked at. A clean
            // buffer here (previewed, or read but never typed into) must
            // never itself trip the cap or count against it.
            if !self.ws.buffers.get(rel).map(|b| b.dirty()).unwrap_or(false)
                && self.ws.buffers.values().filter(|b| b.dirty()).count() >= workspace::MAX_BUFFERS
            {
                self.send_to(from, &Event::Error { msg: "too many unsaved files".into() });
                return;
            }
            match crate::projects::safe_resolve(&self.dir, rel)
                .and_then(|p| crate::projects::read_text_file(&p))
            {
                Ok(text) => {
                    let hash = workspace::hash_text(&text);
                    let b = self.ws.buffers.entry(rel.to_string()).or_default();
                    b.content = workspace::Content::Clean;
                    b.base_hash = hash;
                    b.stale = false;
                    freshly_read = Some(text);
                }
                Err(e) => {
                    // A file the editor cannot hold must not leave a tab
                    // sitting in Edit over it: the textarea would be empty,
                    // and the only thing telling the user why would be a
                    // banner. Since a text file now opens in Edit by default,
                    // this is no longer an edge case — it is what clicking a
                    // .zip, or a log past the 2 MB cap, does. Fall back to the
                    // preview, which renders its own explanation of why the
                    // file is not text.
                    //
                    // Reported only when the mode was *asked for*. A tab that
                    // merely defaulted into Edit and quietly landed in Preview
                    // is the system working; a ⌘-toggle into Edit that could
                    // not be honoured is worth a banner.
                    if mode == Mode::Edit {
                        // Always demote: an empty textarea over a file that
                        // is not empty is how work gets overwritten, and that
                        // is what a tab left in Edit here would be.
                        if self.demote_to_preview(rel) {
                            self.ws.version += 1;
                            let snap = self.snapshot_event(from);
                            self.broadcast(&snap);
                            self.persist();
                        }
                        // Explain only what somebody asked for. A click that
                        // defaulted into Edit and quietly landed in Preview is
                        // the system working; a ✎ that could not be honoured
                        // is not, and the preview alone does not say why.
                        if requested {
                            self.send_to(from, &Event::Error { msg: e });
                        }
                    }
                    return;
                }
            }
        }
        let text = freshly_read.unwrap_or_else(|| {
            // already_dirty was true: no disk read happened above, so the
            // text to re-broadcast is whatever unsaved edit the buffer holds.
            self.ws.buffers.get(rel).and_then(|b| b.edited_text()).unwrap_or_default().to_string()
        });
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
    /// Three outcomes, not two, because a failed `read_to_string` is not
    /// evidence the file is gone (CLAUDE.md):
    ///
    /// - readable as text: the buffer follows it (or is flagged stale) and
    ///   every tab hears `FileChanged`;
    /// - *there* but not readable as text — a PNG, a 3 MB log, a permissions
    ///   failure, anything `read_to_string` refuses: no buffer's content can
    ///   be touched, but the file on screen still changed, so `FileChanged`
    ///   goes out anyway. That event is what re-fetches a previewed image's
    ///   fragment and with it the cache key on its `<img src>`; folding this
    ///   case into "deleted" left the browser showing the old picture forever.
    /// - genuinely absent (`symlink_metadata` says `NotFound`): returns
    ///   false. `classify` routes an open buffer's path to `Buffer`, not
    ///   `Tree`, so without this the caller's tree pane would keep listing a
    ///   file that no longer exists until some unrelated event happened to
    ///   arrive and trigger a refresh. Callers must treat `false` as a tree
    ///   change too.
    pub fn file_changed_externally(&mut self, base: &std::path::Path, rel: &str) -> bool {
        let path = base.join(rel);
        let disk = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            // Ask separately whether the path is there at all, and take only
            // `NotFound` as "gone" — `symlink_metadata` so a dangling symlink
            // answers about the link, not its missing target. Any other
            // metadata error means we could not look, which is not the same
            // as absence, so it lands on the "still there" side: a spurious
            // re-mount costs a repaint, a spurious "deleted" costs the tab.
            Err(_) => match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
                _ => None,
            },
        };
        let Some(disk) = disk else {
            // Present, but nothing here can read it as text. No buffer's
            // content is touched — there is nothing trustworthy to put in one
            // — and `FileChanged` is what makes app.js re-fetch the fragment
            // for every tab showing this rel; the image tab's cache key rides
            // in that fragment (`render::image_fragment`, keyed on the file's
            // mtime), so the re-fetch is the whole mechanism. The version bump
            // keeps the workspace counter honest about a change having
            // happened, exactly as the readable path below does.
            self.ws.version += 1;
            self.broadcast(&Event::FileChanged { rel: rel.to_string() });
            return true;
        };
        let disk_hash = workspace::hash_text(&disk);
        if crate::watch::is_self_write(&mut self.self_writes, rel, disk_hash) {
            return true; // our own save; broadcasting it would echo back at the author
        }
        // No buffer is not "nothing to tell anyone": a file open in Preview
        // has no buffer and still has a pane showing it. The buffer branches
        // below update what a *buffer* holds; the broadcast at the end is
        // what every open tab needs either way.
        if let Some(b) = self.ws.buffers.get_mut(rel) {
            if b.dirty() {
                b.stale = true;
                let ev = Event::BufferStale { rel: rel.to_string() };
                self.broadcast(&ev);
            } else {
                // A clean buffer holds nothing, so following the file is
                // just staying Clean — the disk text goes straight into the
                // broadcast below via the `disk` local, not through the
                // buffer.
                b.content = workspace::Content::Clean;
                b.base_hash = disk_hash;
                b.stale = false;
                let ev = Event::BufferText {
                    rel: rel.to_string(),
                    text: disk,
                    origin: String::new(), // no author: everyone applies it
                };
                self.broadcast(&ev);
            }
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
        if self.ws.buffers.get(rel).is_some_and(|b| !b.dirty()) {
            self.ws.buffers.remove(rel);
        }
    }

    fn do_save(&mut self, from: &ConnId, rel: String, force: bool) {
        let Some(buf) = self.ws.buffers.get(&rel).cloned() else {
            let ev = Event::Error { msg: format!("no buffer for {rel}") };
            return self.send_to(from, &ev);
        };
        // Nothing to write is not an error the user caused: ⌘S on a file that
        // was opened and never edited is a reasonable thing to press, and the
        // answer is that it is already saved. `edited_text().unwrap_or_default()`
        // would happily write an empty string over the file for exactly this
        // case, which is the shape this guard exists to close.
        let Some(text) = buf.edited_text().map(|t| t.to_string()) else {
            self.send_to(from, &Event::SaveOk { rel: rel.clone() });
            return;
        };
        let dir = self.dir.clone();
        match crate::fileops::save(&dir, &rel, &text, buf.base_hash, force) {
            Ok(crate::fileops::SaveOutcome::Written) => {
                let hash = workspace::hash_text(&text);
                if let Some(b) = self.ws.buffers.get_mut(&rel) {
                    b.content = workspace::Content::Clean;
                    b.stale = false;
                    b.base_hash = hash;
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
                let diff_html = crate::render::diff_html(&crate::textdiff::unified(&disk_text, &text));
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
                        {
                            let mut h = Hub::lock(&hub_arc);
                            h.refresh_live_sessions();
                            let snap = h.snapshot_event(&String::new());
                            h.broadcast(&snap);
                        }
                        // Ending a project's last session empties its row in
                        // every other project's ◆ panel; same placement
                        // rules as `do_close_project`'s nudge.
                        broadcast_all(&Event::ProjectsChanged { project: thread_project });
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
    fn do_new_terminal(
        &mut self,
        from: &ConnId,
        pane: crate::proto::PaneId,
        launch: Option<crate::proto::Launch>,
    ) {
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
        // Unconditionally, `None` included: this name may have been handed
        // out before to a ✻ click whose tab was closed before any browser
        // attached, and a plain + must not inherit that click. The shell is
        // spawned by whichever browser attaches first, so the request waits
        // in `session` until then.
        crate::session::set_launch(&self.project, &name, launch);
        let intent = Intent::OpenTab { pane, tab: Tab::Terminal { session: name.clone() } };
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                self.refresh_live_sessions();
                let snap = self.snapshot_event(from);
                // Snapshot first, then the start: a browser attaches on
                // `TerminalStarted` only for a session it has a tab for, and
                // the tab is in this snapshot. Sent the other way round, every
                // browser dropped the event and showed the "press Enter"
                // placeholder for a terminal the click had already asked for.
                self.broadcast(&snap);
                self.broadcast(&Event::TerminalStarted { session: name });
                self.persist();
            }
            Ok(false) => {}
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// A terminal link resolves *before* anything reaches the layout.
    ///
    /// `OpenTab` validates nothing: `apply_layout` pushes the tab straight in
    /// and the resulting snapshot goes to every connected browser. Since a
    /// path scraped out of terminal output is a guess, opening optimistically
    /// would leave a dead tab in everyone's window for one person's false
    /// positive — so the guess is settled here, and only a real file is
    /// allowed to become an `OpenTab`.
    ///
    /// Building that `OpenTab` rather than reaching into the panes directly is
    /// what makes a `.png` from a terminal coerce exactly as one clicked in
    /// the tree does, and what gets tab de-duplication (`find_tab`) for free.
    fn do_open_path(&mut self, from: &ConnId, text: String) {
        let rel = match crate::projects::resolve_terminal_path(&self.dir, &text) {
            Ok(rel) => rel,
            Err(msg) => {
                let ev = Event::PathRefused { text, msg };
                return self.send_to(from, &ev);
            }
        };
        // Re-enter `handle` with a synthesised `OpenTab` rather than calling
        // `apply_layout` here directly: that is the only way this path gets
        // `open_buffer_for` (and so a real buffer, watcher coverage, and a
        // clean flip to Edit later) for free, and the only way it stays in
        // lockstep with a tab opened from the file tree. Safe to recurse
        // once: `OpenTab` matches none of the early-return arms at the top
        // of `handle`, so it falls straight through to the single
        // `apply_layout` call and the single broadcast/persist that follow —
        // no double broadcast, no double persist, and the refusal path above
        // (which returns before this point) is untouched.
        let intent = Intent::OpenTab {
            pane: crate::proto::MIDDLE,
            tab: Tab::File { rel, mode: Mode::Preview },
        };
        self.handle(from, intent)
    }

    /// Resolves `rel` against this project's directory before it ever
    /// reaches `ide::mention_to` — `safe_resolve` is the confinement
    /// boundary, and a path a browser sent must never be trusted past it.
    /// Failure (an escaping path, an invalid session name, or no Claude
    /// connected) is reported with `send_to`, never `broadcast`: only the
    /// browser that pressed the key should see the refusal.
    fn do_mention_path(
        &mut self,
        from: &ConnId,
        rel: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
        session: Option<String>,
    ) {
        let abs = match crate::projects::safe_resolve(&self.dir, &rel) {
            Ok(p) => p,
            Err(e) => {
                let ev = Event::Error { msg: e };
                return self.send_to(from, &ev);
            }
        };
        // Refused rather than dropped to `None`: silently ignoring an
        // unusable name would degrade an aimed mention into an unaimed one,
        // which is a different request. Truncated in the message because the
        // value is unvalidated wire input and an error banner is not the
        // place to render four kilobytes of it.
        if let Some(s) = &session {
            if !crate::session::valid_name(s) {
                let shown: String = s.chars().take(32).collect();
                let ev = Event::Error {
                    msg: format!("mention: invalid session name {shown:?}"),
                };
                return self.send_to(from, &ev);
            }
        }
        // A half-specified range (one bound present, the other absent) is
        // refused rather than guessed at. `Option::zip` — the previous
        // shape of this line — silently turned it into `None`, i.e. a
        // whole-file mention: a different answer than what was asked for,
        // not the one asked for with a gap filled in. Defaulting the
        // missing bound to the one given would be just as much a guess in
        // the other direction. Both are the same "I could not determine X"
        // folded into a definite answer that CLAUDE.md's absence-of-evidence
        // rule exists to prevent — this is a wire-protocol input, not
        // internal state, but the principle is the same one.
        let lines = match (line_start, line_end) {
            (Some(a), Some(b)) => Some((a, b)),
            (None, None) => None,
            _ => {
                let ev = Event::Error {
                    msg: "mention: line_start and line_end must both be set or both be absent".into(),
                };
                return self.send_to(from, &ev);
            }
        };
        if let Err(e) = crate::ide::mention_to(&self.project, session.as_deref(), &abs, lines) {
            let ev = Event::Error { msg: e };
            self.send_to(from, &ev);
        }
    }

    /// The editor's current selection, arriving on a 200ms debounce from
    /// `static/app.js` rather than a deliberate keystroke like
    /// `MentionPath`'s Alt+K. Resolved and confined against the project the
    /// same way `do_mention_path` resolves `rel` — a path off the wire is
    /// never trusted past `safe_resolve` — and a confinement failure is
    /// surfaced to the asking client exactly like `do_mention_path`'s: it is
    /// not routine, and is worth knowing about.
    ///
    /// What is *not* surfaced is `ide::selection_changed`'s own refusal:
    /// sharing being off (the ordinary state for almost every project, since
    /// the default is off) and no Claude being attached are both routine,
    /// expected outcomes on a signal that fires on every debounced selection
    /// change — flashing the error banner for either would train users to
    /// ignore it. The visible signal that sharing is on is the header
    /// indicator `render.rs` renders from `Settings::share_selection`, not a
    /// banner here.
    fn do_share_selection(
        &mut self,
        from: &ConnId,
        rel: String,
        text: String,
        start: (u32, u32),
        end: (u32, u32),
    ) {
        let abs = match crate::projects::safe_resolve(&self.dir, &rel) {
            Ok(p) => p,
            Err(e) => {
                let ev = Event::Error { msg: e };
                return self.send_to(from, &ev);
            }
        };
        let _ = crate::ide::selection_changed(&self.project, &abs, &text, start, end);
    }

    /// The human's Accept/Reject, which **is** the permission answer Claude
    /// is blocked on.
    ///
    /// Lock order, and it only goes one way: this runs with the hub lock
    /// held and takes `ide`'s pending lock inside it. `ide::open_diff` takes
    /// the pending lock and *releases* it before it ever asks for a hub lock
    /// (see `ide::open_proposal`'s caller), so there is no cycle to close.
    /// Both directions must stay that way.
    fn do_answer_proposal(&mut self, from: &ConnId, id: String, accept: bool, text: Option<String>) {
        let a = match (accept, text) {
            // A rejection carrying text is still a rejection: the text is
            // only ever read on the accepting path, so there is no way for a
            // client to smuggle content past a "no".
            (false, _) => crate::ide::Answer::Rejected,
            (true, Some(t)) => crate::ide::Answer::AcceptedEdited(t),
            (true, None) => crate::ide::Answer::Accepted,
        };
        // Answer first, then close the tab: the removal from the pending map
        // inside `answer` is what decides which of two browsers won, and the
        // loser must find the proposal already gone rather than be told it
        // succeeded because its tab was still on screen.
        let failed = crate::ide::answer(&self.project, &id, a).err();
        self.close_proposal_tab(&id);
        if let Some(e) = failed {
            // To the clicker only. Every other browser's tab simply
            // disappears, which is the truth — the proposal was answered.
            let ev = Event::Error { msg: format!("that proposal is no longer open: {e}") };
            self.send_to(from, &ev);
        }
    }

    /// Opens the tab and hands both sides of the diff to every browser on
    /// this project. Called from an `ide` connection thread, which holds no
    /// lock of its own by this point.
    ///
    /// The proposal is deliberately not persisted (`persist` is not called):
    /// it would only be dropped again by `workspace::drop_dead_tabs` on the
    /// next load, and a whole file body has no business in the state file.
    pub fn open_proposal_tab(&mut self, id: &str, rel: &str, old_text: &str, new_text: &str) {
        let Some(p) = self.ws.panes.get_mut(crate::proto::MIDDLE as usize) else {
            // A hand-edited state file could in principle leave fewer panes
            // than the layout has. Never index blind from a socket thread.
            eprintln!("resh: no middle pane to open a proposal in");
            return;
        };
        p.tabs.push(Tab::Proposal { id: id.to_string() });
        p.active = p.tabs.len() - 1;
        self.proposals.insert(
            id.to_string(),
            ProposalSides {
                rel: rel.to_string(),
                old_text: old_text.to_string(),
                new_text: new_text.to_string(),
            },
        );
        self.ws.version += 1;
        // Content before the tab that renders it: a client that learned about
        // the tab first would have a frame with nothing to draw in it.
        let ev = Event::Proposal {
            id: id.to_string(),
            rel: rel.to_string(),
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        };
        self.broadcast(&ev);
        let snap = self.snapshot_event(&String::new());
        self.broadcast(&snap);
    }

    /// Every open proposal, as the events that draw it. Sent to a connection
    /// as it subscribes, for the same reason `BufferText` is: `State` carries
    /// tabs but no content, so without this a reconnecting browser renders an
    /// unanswerable blank.
    ///
    /// `wsconn` sends these *before* the snapshot's `State`, matching the
    /// live path in `open_proposal_tab` (content before the tab that renders
    /// it), so a client only ever has to handle one order. This is safe
    /// because `Event::Proposal` carries no `origin` field — the
    /// origin-latching rule that forces `State` to go out inside the same
    /// lock acquisition as `subscribe` does not apply to it.
    pub fn proposal_replay(&self) -> Vec<Event> {
        self.proposals
            .iter()
            .map(|(id, p)| Event::Proposal {
                id: id.clone(),
                rel: p.rel.clone(),
                old_text: p.old_text.clone(),
                new_text: p.new_text.clone(),
            })
            .collect()
    }

    /// Drops one proposal's tab from every pane. A proposal is a single
    /// question, so a second browser showing the same tab must lose it too —
    /// the same reasoning `EndSession` gives for clearing a terminal's tabs
    /// everywhere rather than by (pane, idx).
    pub fn close_proposal_tab(&mut self, id: &str) {
        let mut removed = false;
        for p in self.ws.panes.iter_mut() {
            while let Some(i) =
                p.tabs.iter().position(|t| matches!(t, Tab::Proposal { id: x } if x == id))
            {
                p.tabs.remove(i);
                // Not `min(len-1)` alone: closing a tab to the *left* of the
                // active one shifts the active tab down by one, and clamping
                // without that shift silently activates a different tab.
                if p.active > i {
                    p.active -= 1;
                }
                p.active = p.active.min(p.tabs.len().saturating_sub(1));
                removed = true;
            }
        }
        // Removed whether or not a tab was found: the content has no reason
        // to outlive the question, and a leaked entry would be replayed to
        // every later connection as a proposal with no tab.
        self.proposals.remove(id);
        if !removed {
            return;
        }
        self.ws.version += 1;
        let snap = self.snapshot_event(&String::new());
        self.broadcast(&snap);
    }

    fn do_close_project(&mut self, from: &ConnId) {
        let dirty: Vec<String> =
            self.ws.buffers.iter().filter(|(_, b)| b.dirty()).map(|(r, _)| r.clone()).collect();
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
                        // Outside the catch_unwind above, not inside it: a
                        // panic in kill_project must not skip stopping the
                        // ide listener, or a closed project keeps
                        // authenticating connections against a token no
                        // longer advertised in any lock file. stop() itself
                        // cannot panic (no unwrap, no I/O it doesn't guard).
                        crate::ide::stop(&thread_project);
                        {
                            let mut h = Hub::lock(&hub_arc);
                            h.closing = false;
                            h.ws.version += 1;
                            h.broadcast(&Event::ProjectClosed { ended });
                            h.refresh_live_sessions();
                            let snap = h.snapshot_event(&String::new());
                            h.broadcast(&snap);
                        }
                        // Every *other* project's ◆ panel, which nothing
                        // above reaches. After the block, not inside it:
                        // `broadcast_all` locks every hub including this
                        // one. And after `kill_project`, not before — the
                        // nudge makes every browser refetch the roster, and
                        // the roster counts socket files, so a nudge sent
                        // while they were still being unlinked would have
                        // them re-read the very state they were told had
                        // changed.
                        broadcast_all(&Event::ProjectsChanged { project: thread_project });
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
                crate::ide::stop(&project);
                self.ws.version += 1;
                self.broadcast(&Event::ProjectClosed { ended });
                self.refresh_live_sessions();
                let snap = self.snapshot_event(&String::new());
                self.broadcast(&snap);
            }
        }
    }
}

/// The hub for a project that is already open, without creating one.
///
/// `REGISTRY.get()`, not `get_or_init`: everything below is a question asked
/// from an `ide` connection thread, and building a hub as a side effect of a
/// question would start a filesystem watcher for a project nobody opened —
/// the same reasoning `Hub::is_closing` and `Hub::show_hidden` give.
///
/// The registry guard is dropped before the caller ever locks the hub, so the
/// registry-then-hub order `for_project` established is preserved.
fn open_hub(project: &str) -> Option<Arc<Mutex<Hub>>> {
    let reg = REGISTRY.get()?;
    let map = reg.lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).cloned()
}

/// Whether resh is holding unsaved edits to `rel`.
///
/// `openDiff` asks this before parking a proposal: accepting one would let
/// Claude write over text the user has typed and not saved, which is the one
/// thing the whole conflict guard exists to prevent.
///
/// `false` for a project with no hub is a fact, not a failed check — a
/// project nobody has opened holds no buffers at all, so there is nothing
/// here to be uncertain about.
pub fn has_dirty_buffer(project: &str, rel: &str) -> bool {
    let Some(arc) = open_hub(project) else { return false };
    let dirty = Hub::lock(&arc).ws.buffers.get(rel).is_some_and(|b| b.dirty());
    dirty
}

/// Opens a proposal tab for a change Claude has asked permission to make.
/// The free-function half of `Hub::open_proposal_tab`, so `ide.rs` never
/// takes a hub lock itself.
///
/// Silently does nothing when the project has no hub. That is not a case
/// production reaches: the `ide` listener a proposal arrives on is created
/// by `Hub::for_project`, which puts the hub in the registry first and never
/// removes it. `ide.rs`'s own unit tests do build a listener with no hub, and
/// they assert on the parked request rather than on the tab.
pub fn open_proposal(project: &str, id: &str, rel: &str, old_text: &str, new_text: &str) {
    if let Some(arc) = open_hub(project) {
        Hub::lock(&arc).open_proposal_tab(id, rel, old_text, new_text);
    }
}

/// Withdraws a proposal's tab — Claude sent `close_tab`, so the question is
/// gone whether or not anyone was looking at it.
pub fn close_proposal(project: &str, id: &str) {
    if let Some(arc) = open_hub(project) {
        Hub::lock(&arc).close_proposal_tab(id);
    }
}

/// Reads out one open proposal's content by id, for `routes.rs`'s
/// `/frag/{project}/proposal` fragment — the free-function half, so that
/// route never takes a hub lock itself, matching `has_dirty_buffer`/
/// `open_proposal` above.
///
/// `None` covers two different situations the caller does not need to tell
/// apart: a project with no hub yet, and an id that has already been
/// answered or withdrawn by the time this browser's fetch lands — both
/// render the same "this proposal is no longer open" fragment.
pub fn proposal_by_id(project: &str, id: &str) -> Option<ProposalSides> {
    let arc = open_hub(project)?;
    let sides = Hub::lock(&arc).proposals.get(id).cloned();
    sides
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

    /// The three tests below that call `Hub::for_project` directly (not a
    /// bare `Hub::new()`) reach `ide::for_project` -> `idelock::ide_dir()`
    /// through it. Without this, `cargo test --lib` would write real lock
    /// files into the developer's actual `~/.claude/ide` (Task 5 review,
    /// finding 2) — the same isolation `tests/integration.rs` applies for
    /// the same reason, via its own `isolate_ide_dir_for_tests`.
    fn isolate_ide_dir_for_tests() {
        crate::idelock::isolate_ide_dir_for_test();
    }

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

    /// A clean buffer holds no text, so a save that read one would write an
    /// empty string over the file. Cmd-S on an untouched file reaches here
    /// via pushEdit, so this is a real path and not a hypothetical one.
    #[test]
    fn saving_a_clean_buffer_writes_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let path = d.path().join("a.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "a.rs".into(), mode: Mode::Edit } },
        );
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        drain(&rx);

        h.handle(&c, Intent::SaveBuffer { rel: "a.rs".into(), force: false });

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        // mtime too: an identical rewrite is still a write, and it would make
        // the watcher fire and every other client re-fetch.
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before,
            "an unedited save must not touch the file at all"
        );
        assert!(
            drain(&rx).iter().any(|m| m.contains(r#""t":"SaveOk""#)),
            "the client still needs to hear it was saved"
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
        // A clean buffer holds nothing of its own to compare against
        // "on disk\n" here; the BufferText assertion below is what actually
        // proves the reload happened, by checking what reached the browser.
        assert_eq!(b.edited_text(), None, "discarding shows the file, not the discarded edit");
        assert!(!b.dirty(), "a freshly read buffer is not dirty");
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
        assert_eq!(dirty.edited_text(), Some("mine\n"), "and must keep the unsaved text, never adopt the file");

        // The live rule for a *clean* buffer is that it follows the file
        // (file_changed_externally); a restart must not be the one case where
        // it silently shows content the file no longer has.
        let clean = &h2.ws.buffers["clean.txt"];
        // A clean buffer holds nothing of its own — base_hash and
        // !stale below are what prove reconcile actually caught up with the
        // new disk content on the clean path, distinct from the dirty path
        // above.
        assert_eq!(clean.edited_text(), None, "a clean buffer holds no text of its own");
        assert_eq!(clean.base_hash, workspace::hash_text("somebody else\n"));
        assert!(!clean.stale, "following the file is not staleness");

        // The discriminating case: flagging everything would pass both of the
        // assertions above.
        assert!(!h2.ws.buffers["quiet.txt"].stale, "a file nobody touched is not stale");
        assert_eq!(h2.ws.buffers["quiet.txt"].edited_text(), Some("mine\n"));
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
        assert_eq!(b.edited_text(), Some("mine\n"), "unsaved work survives a file we cannot read");
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
        assert!(h.ws.buffers["a.txt"].dirty(), "the edit must have landed as unsaved text");
        drop(rx);
        drop(h);

        // The restart. Same project name and state dir, so this is the same
        // workspace coming back off disk.
        let mut h2 = Hub::new("restart_probe", d.path().to_path_buf());
        let (c2, rx2) = h2.subscribe();
        drain(&rx2);
        assert_eq!(h2.ws.buffers["a.txt"].edited_text(), Some("mine\n"), "unsaved text is crash-safe");
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
        assert!(!h.ws.buffers["a.txt"].dirty(), "a freshly-read buffer must not be marked dirty");

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
    /// self.open_buffer_for(from, rel)` and running this test: it failed at
    /// "a coerced tab must not get a buffer" — the buffer was there, holding
    /// the lossy text of the PNG.
    /// With Edit as the default mode for text files, clicking a `.zip` or a
    /// log past the 2 MB cap sends `OpenTab{Edit}` for a file the editor
    /// cannot hold. Leaving the tab in Edit puts an empty textarea over it,
    /// and an empty textarea over a file that exists is how work gets
    /// overwritten. The tab must land in Preview instead, which renders its
    /// own explanation.
    ///
    /// Reverting the Err arm to `send_to(Error)` fails this at the mode
    /// assertion — the tab stays in Edit.
    #[test]
    fn a_file_the_editor_cannot_hold_falls_back_to_preview() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        // A NUL byte is what read_text_file refuses as binary. `.bin` is off
        // NO_TEXT_EDIT_EXT on purpose, so coerce_tab cannot be what saves us
        // here — this is the read failing, not the extension list.
        std::fs::write(d.path().join("blob.bin"), b"a\0b").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "blob.bin".into(), mode: Mode::Edit },
            },
        );

        assert_eq!(
            h.ws.panes[proto::MIDDLE as usize].tabs.last(),
            Some(&Tab::File { rel: "blob.bin".into(), mode: Mode::Preview }),
            "an unreadable file's tab must not stay in Edit"
        );
        assert!(!h.ws.buffers.contains_key("blob.bin"), "and it gets no buffer");
        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"State""#)),
            "the client has to be told the mode moved, or its tab strip disagrees with the server"
        );
    }

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
        // A freshly-opened, unedited buffer holds nothing of its own;
        // base_hash and !dirty() are what prove the read actually happened.
        assert_eq!(h.ws.buffers["a.txt"].base_hash, workspace::hash_text("on disk\n"));
        assert!(!h.ws.buffers["a.txt"].dirty());
    }

    /// The sibling of the coercion case above: an image requested directly in
    /// Preview (no coercion involved — Preview is already what it gets) must
    /// still get no buffer. `refuses_text_edit` gates the OpenTab dispatch on
    /// the *rel*, not the mode, precisely so this holds; a buffer for an
    /// image is the shape an earlier defect used to truncate files.
    #[test]
    fn a_directly_previewed_image_gets_no_buffer() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("shot.png"), [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
            .unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "shot.png".into(), mode: Mode::Preview },
            },
        );
        assert!(
            !h.ws.buffers.contains_key("shot.png"),
            "a previewed image must not get a buffer: {:?}",
            h.ws.buffers.keys().collect::<Vec<_>>()
        );
    }

    /// Every open file has a buffer, which is what puts a previewed file in
    /// the watcher's list — and it holds nothing until it is edited.
    #[test]
    fn opening_a_file_in_preview_creates_a_buffer_holding_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("read.md"), "hello\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
            },
        );

        let b = h.ws.buffers.get("read.md").expect("a previewed file has a buffer");
        assert_eq!(b.edited_text(), None, "and holds nothing");
        assert_eq!(b.base_hash, workspace::hash_text("hello\n"), "with a base taken at open time");
    }

    /// Clicking back to a tab that is already open still runs through
    /// `apply_layout`'s `OpenTab` arm and still returns `Ok(true)` (it just
    /// moves `active`), so without a guard this would re-read the file and
    /// re-broadcast its text to everyone on every activation — a 2 MB read
    /// under the hub lock for a pane whose content a browser fetches over
    /// HTTP, not from this broadcast.
    #[test]
    fn reactivating_an_already_open_clean_tab_does_not_reread_or_rebroadcast() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("read.md"), "hello\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
            },
        );
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
            },
        );
        let msgs = drain(&rx);
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "reactivating an unchanged, already-open tab must not resend its text; got {msgs:?}"
        );
        // What this test discriminates is the guard, not the arm: delete
        // `reactivating_a_settled_buffer` and the assertion above fails on a
        // resent BufferText. Deleting the whole OpenTab arm would leave the
        // test green — the State broadcast asserted here comes from
        // apply_layout, not from that arm — so read this second assertion as
        // nothing more than proof the intent was dispatched at all.
        assert!(msgs.iter().any(|m| m.contains(r#""t":"State""#)), "got {msgs:?}");
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
        assert!(h.ws.buffers["a.txt"].dirty());

        h.handle(&c, Intent::SetMode { rel: "a.txt".into(), mode: Mode::Edit });
        let msgs = drain(&rx);
        assert_eq!(h.ws.buffers["a.txt"].edited_text(), Some("unsaved work"), "dirty text must survive");
        assert!(h.ws.buffers["a.txt"].dirty(), "SetMode must not clear dirty for unsaved work");
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
        assert!(h.ws.buffers["dirty.txt"].dirty());

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
        assert!(h.ws.buffers["old.txt"].dirty());

        h.handle(&c, Intent::RenamePath { from: "old.txt".into(), to: "new.txt".into() });
        let msgs = drain(&rx);
        assert!(msgs.iter().any(|m| m.contains(r#""t":"TreeChanged""#)));
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"State""#)),
            "a rename must also broadcast State, or clients keep showing the tab at the old path"
        );

        assert!(!h.ws.buffers.contains_key("old.txt"), "the old key must not linger");
        assert_eq!(h.ws.buffers["new.txt"].edited_text(), Some("unsaved work"), "unsaved text must survive the rename");
        assert!(h.ws.buffers["new.txt"].dirty());
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
        assert_eq!(h.ws.buffers["lib/a.rs"].edited_text(), Some("unsaved a"));
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
        isolate_ide_dir_for_tests();
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

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None });
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

    /// The ✻ button is the + button plus a program to type in. The hub only
    /// parks the request against the name it allocates; the shell it reaches
    /// is the one `session::attach` spawns for that name later.
    #[test]
    fn a_claude_terminal_parks_its_launch_on_the_name_it_was_given() {
        // STATE first, SESSION second — the order session.rs's lock comment fixes.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("newterm_launch", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude) });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None });
        drain(&rx);
        let first = crate::session::attach("newterm_launch", "term", d.path()).unwrap();
        assert_eq!(first.launch, Some(proto::Launch::Claude), "✻ got `term`, so `term` starts claude");
        let second = crate::session::attach("newterm_launch", "term1", d.path()).unwrap();
        assert_eq!(second.launch, None, "+ got `term1`, which stays a plain shell");
        crate::session::kill_project("newterm_launch");

        // The stale case: ✻ allocates `term2`, its tab is closed before any
        // browser attaches, and + is then handed `term2` back. The click that
        // made it a claude shell is gone, so the shell must be plain.
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude) });
        let idx = h.ws.panes[proto::RIGHT as usize]
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Terminal { session } if session == "term2"))
            .expect("✻ was handed term2");
        h.handle(&c, Intent::CloseTab { pane: proto::RIGHT, idx });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None });
        drain(&rx);
        let reused = crate::session::attach("newterm_launch", "term2", d.path()).unwrap();
        assert_eq!(reused.launch, None, "a reallocated name must not inherit the old click");
        crate::session::kill_project("newterm_launch");
        std::env::remove_var("RESH_CMD");
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
        isolate_ide_dir_for_tests();
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

    // The ◆ "running projects" panel in every *other* project's browser is a
    // server-rendered fragment that only ever refetches on that tab's own
    // triggers (page load, ⟳, opening the panel, its own ProjectClosed). So
    // closing project A from A's tab left B's tab showing "● A" — and badge
    // "1" — for as long as B stayed open. Reproduced in a real browser before
    // this existed: A's socket gone from disk and A redirected to `/`, B's
    // open panel still listing A. The roster is machine-wide in exactly the
    // way notices are, so the fix is the same shape: a `broadcast_all` nudge
    // once the close has actually finished.
    //
    // Asserted on B's subscriber, not A's: A's own clients already get
    // ProjectClosed and refetch on that, so a nudge that only reached A would
    // pass a single-hub test and fix nothing. And deliberately with *no*
    // session in A: a killed session's pump thread sends its own nudge as it
    // winds down (see `a_shell_exiting_on_its_own_...` below), which would
    // satisfy this assertion whether or not the close thread said anything.
    // With nothing to kill, the close thread is the only possible source.
    // Watched fail with its `broadcast_all` removed: B's receiver timed out.
    #[test]
    fn closing_a_project_tells_every_other_projects_clients_the_roster_changed() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let hub_a = Hub::for_project("roster-a", dir_a.path().to_path_buf());
        let hub_b = Hub::for_project("roster-b", dir_b.path().to_path_buf());
        let (ca, rxa) = Hub::lock(&hub_a).subscribe();
        let (_cb, rxb) = Hub::lock(&hub_b).subscribe();
        while rxb.try_recv().is_ok() {}

        Hub::lock(&hub_a).handle(&ca, Intent::CloseProject);
        // The close runs on its own thread (see close_project_returns_promptly
        // above); ProjectClosed on A's subscriber is the signal that it is
        // done. 10s is wide margin over the real ps/kill spawns it makes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let m = rxa.recv_timeout(left).expect("A's close must report ProjectClosed");
            if m.contains(r#""t":"ProjectClosed""#) {
                break;
            }
        }

        // Attributed by project name, because `broadcast_all` reaches every
        // hub in the registry — including ones belonging to tests running
        // concurrently, whose own sessions ending would otherwise be
        // indistinguishable from this close and let this pass vacuously.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let m = rxb
                .recv_timeout(left)
                .expect("project B's clients must be told that A's close changed the roster");
            if m.contains(r#""t":"ProjectsChanged""#) && m.contains(r#""project":"roster-a""#) {
                break;
            }
        }

        std::env::remove_var("RESH_STATE_DIR");
    }

    // The other way a project leaves the roster: its shell exits (`exit`,
    // ctrl-d, or its dtach master dying) with no intent ever asking. The
    // only place that notices is the PTY pump as it winds down, so that is
    // where the nudge has to come from; nothing in `handle` runs. `RESH_CMD=
    // true` is a shell that exits on its own the instant it starts, without
    // dtach — which is the point: the socket-file side is `registry`'s job,
    // this pins that the *event* is sent at all. Watched fail with the pump's
    // `broadcast_all` removed: B's receiver timed out.
    #[test]
    fn a_shell_exiting_on_its_own_tells_every_project_the_roster_changed() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "true");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        // Only B has a hub: the project whose shell exits need not have one
        // (its tab can be long gone), and the nudge must still reach the others.
        let hub_b = Hub::for_project("roster-exit-b", dir_b.path().to_path_buf());
        let (_cb, rxb) = Hub::lock(&hub_b).subscribe();
        while rxb.try_recv().is_ok() {}

        let att = crate::session::attach("roster-exit", "shell", dir_a.path())
            .expect("attach with RESH_CMD=true spawns `true`");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let m = rxb
                .recv_timeout(left)
                .expect("a shell ending on its own must tell other projects the roster changed");
            if m.contains(r#""t":"ProjectsChanged""#) && m.contains(r#""project":"roster-exit""#) {
                break;
            }
        }
        drop(att);

        std::env::remove_var("RESH_CMD");
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
        isolate_ide_dir_for_tests();
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

    /// `file_changed_externally` used to return before its broadcast whenever
    /// the rel had no buffer:
    ///
    ///     let Some(b) = self.ws.buffers.get_mut(rel) else { return true };
    ///
    /// so nothing downstream ever heard that the file on screen had changed.
    ///
    /// The fixture has to be a tab that really has no buffer *and* whose file
    /// `read_to_string` can read, or the early return is never reached and
    /// this test pins nothing. An ordinary previewed `.md` is no longer such a
    /// case — a Preview open creates a clean buffer now — so this uses the
    /// case that survives it: a text file past `MAX_FILE_BYTES`, which
    /// `read_text_file` refuses (no buffer) and plain `read_to_string` reads
    /// happily. A tailed 3 MB log in the preview pane is exactly that file.
    /// Reverting the `if let Some(b)` back to the early return fails this
    /// test.
    #[test]
    fn an_external_change_to_a_bufferless_previewed_file_is_broadcast() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let big = "before\n".repeat(400_000); // > 2 MB: read_text_file refuses it
        std::fs::write(d.path().join("huge.log"), &big).unwrap();
        let mut h = Hub::new("previewproj", d.path().to_path_buf());
        let (a, rx) = h.subscribe();
        drain(&rx);

        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "huge.log".into(), mode: Mode::Preview },
        });
        drain(&rx);
        assert!(
            !h.ws.buffers.contains_key("huge.log"),
            "fixture is void unless the oversize file really has no buffer"
        );

        std::fs::write(d.path().join("huge.log"), big.replace("before", "after")).unwrap();
        assert!(h.file_changed_externally(d.path(), "huge.log"));

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"FileChanged""#) && m.contains("huge.log")),
            "a previewed file's change must reach the browser, got {msgs:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The image case, which is the one the cache-busting URL was built for:
    /// `read_to_string` fails on a PNG *every* time, so folding that failure
    /// into "the file was deleted" meant `FileChanged` never went out, the
    /// client never re-fetched the fragment, and the browser went on showing
    /// the picture that was replaced.
    /// Reverting to `let Ok(disk) = read_to_string(..) else { return false }`
    /// fails this test.
    #[test]
    fn an_external_change_to_an_unreadable_file_still_broadcasts_filechanged() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        // Real PNG magic: not valid UTF-8, so read_to_string genuinely fails
        // here rather than the test relying on the extension.
        std::fs::write(d.path().join("pic.png"), b"\x89PNG\r\n\x1a\nfirst").unwrap();
        let mut h = Hub::new("imgproj", d.path().to_path_buf());
        let (a, rx) = h.subscribe();
        drain(&rx);

        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "pic.png".into(), mode: Mode::Preview },
        });
        drain(&rx);
        let version_before = h.ws.version;

        std::fs::write(d.path().join("pic.png"), b"\x89PNG\r\n\x1a\nsecond bytes").unwrap();
        assert!(
            h.file_changed_externally(d.path(), "pic.png"),
            "a file that is still there must not be reported as deleted"
        );

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"FileChanged""#) && m.contains("pic.png")),
            "a changed image must reach the browser, got {msgs:?}"
        );
        // The re-mount itself rides on FileChanged (the img's cache key lives
        // in the fragment, keyed on mtime); this pins the other half — a real
        // change still moves the workspace version, as the readable path does.
        assert!(h.ws.version > version_before, "a real change must move the version");
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "nothing readable came back, so no buffer text may be invented, got {msgs:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// A genuinely deleted file is the third outcome and must still say so:
    /// the watcher turns `false` into the tree refresh that drops the row.
    #[test]
    fn a_deleted_file_still_reports_false() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("gone.md"), "here\n").unwrap();
        let mut h = Hub::new("delproj", d.path().to_path_buf());
        std::fs::remove_file(d.path().join("gone.md")).unwrap();
        assert!(!h.file_changed_externally(d.path(), "gone.md"));
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Clicking a `.pdf`, a `.zip` or a 3 MB log in the tree sends
    /// `OpenTab{mode: Preview}`, whose read `read_text_file` refuses. The
    /// preview pane renders anyway (it fetches over HTTP), so the refusal is
    /// not something the user did or can act on — an error banner there is
    /// pure noise on an everyday click. Dropping the `mode == Mode::Edit`
    /// guard in `open_buffer_for` fails this test.
    #[test]
    fn a_preview_open_of_an_unreadable_file_is_silent() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        // .bin is off TEXT_EXTENSIONS, so the NUL sniff really refuses it, and
        // off NO_TEXT_EDIT_EXT, so nothing coerces the mode out from under the
        // Edit half of this pair below.
        std::fs::write(d.path().join("blob.bin"), b"\x00\x01binary").unwrap();
        let mut h = Hub::new("binproj", d.path().to_path_buf());
        let (a, rx) = h.subscribe();
        drain(&rx);

        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "blob.bin".into(), mode: Mode::Preview },
        });

        let msgs = drain(&rx);
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"Error""#)),
            "previewing a binary file must not raise a banner, got {msgs:?}"
        );
        assert!(
            !h.ws.buffers.contains_key("blob.bin"),
            "a file that could not be read must leave no buffer behind"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The other direction, and the reason the guard is on the mode rather
    /// than on `open_buffer_for` as a whole: in Edit the same refusal means
    /// the editor cannot work, and saying nothing would leave an empty
    /// textarea over a file that is not empty. Making the Err arm
    /// unconditionally silent fails this test.
    #[test]
    fn an_edit_open_of_an_unreadable_file_reports_the_reason() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("blob.bin"), b"\x00\x01binary").unwrap();
        let mut h = Hub::new("binproj2", d.path().to_path_buf());
        let (a, rx) = h.subscribe();
        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "blob.bin".into(), mode: Mode::Preview },
        });
        drain(&rx);

        h.handle(&a, Intent::SetMode { rel: "blob.bin".into(), mode: Mode::Edit });

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("binary file")),
            "switching to Edit on an unreadable file must say why, got {msgs:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    // These four tests reach `apply_layout`'s `Ok(true)` branch (opening or
    // reusing a tab), which calls `self.persist()`. Every other test in this
    // file that persists scopes `RESH_STATE_DIR` under `STATE_ENV_LOCK` first
    // — without it, `wsstate::save` writes to this host's real default state
    // directory under the project's storage key, which is exactly the kind
    // of test-run side effect this codebase's own testing culture warns
    // against. Confirmed by running without the guard: a stray `linktwice`
    // tab count of 3 (not the expected 2) showed up from a prior run's
    // leftover persisted file.
    #[test]
    fn open_path_opens_the_file_it_names() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("linkopen", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();

        let abs = dir.path().join("src/a.rs");
        h.handle(&conn, Intent::OpenPath { text: format!("{}:42", abs.display()) });

        // The rel, not the count. "a tab opened" passes for the wrong file.
        let tabs = &h.ws.panes[proto::MIDDLE as usize].tabs;
        assert!(
            tabs.iter().any(|t| matches!(t, Tab::File { rel, mode: Mode::Preview } if rel == "src/a.rs")),
            "expected a Preview tab for src/a.rs, got {tabs:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Two subscribers, deliberately. With one, `send_to` and `broadcast` are
    /// indistinguishable and this test would pass with the privacy removed —
    /// which is on CLAUDE.md's own list of tests that passed for the wrong
    /// reason.
    ///
    /// A refusal must be *inert* to everyone but the asker, not just silent
    /// about `PathRefused` specifically: the other subscriber must receive
    /// nothing at all, and `ws.version` must not move. Checking only for the
    /// literal string `"PathRefused"` in the other subscriber's inbox would
    /// let a leaked no-op `State` bump (version++, snapshot, broadcast,
    /// persist — exactly what a refusal must never trigger) sail straight
    /// through, since that event contains no such string. See Revert 4 below.
    ///
    /// Revert 1 (Step 5): changing the `Err` arm's `self.send_to(from, &ev)`
    /// to `self.broadcast(&ev)` failed this test on the second assertion, with
    /// the actual panic message:
    /// `a refusal leaked to a second browser: ["{\"t\":\"PathRefused\",
    ///  \"text\":\"src/gone.rs\",\"msg\":\"not found: No such file or
    ///  directory (os error 2)\"}"]`
    ///
    /// Revert 4 (fix round 1): injecting, at the top of the `Err` arm before
    /// the early return —
    /// ```ignore
    /// self.ws.version += 1;
    /// let snap = self.snapshot_event(from);
    /// self.broadcast(&snap);
    /// self.persist();
    /// ```
    /// — failed this test on the "nothing at all" assertion, with the actual
    /// panic message:
    /// `a refusal must leave the other subscriber's inbox empty, got:
    ///  ["{\"t\":\"State\",\"version\":1,\"origin\":\"c1\",\"ws\":{...}}"]`
    /// (the version-unchanged assertion below it would also have caught this
    /// independently, since the injection bumps `ws.version` — but the inbox
    /// check runs first and fires first).
    #[test]
    fn open_path_refusal_reaches_only_the_client_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        let mut h = Hub::new("linkrefuse", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        let version_before = h.ws.version;

        h.handle(&asker, Intent::OpenPath { text: "src/gone.rs".into() });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains("PathRefused")),
            "the asking client got no refusal: {got:?}"
        );
        // Not just "no PathRefused": nothing at all. A leaked `State` bump
        // contains no literal "PathRefused" and would pass a substring check.
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(
            others.is_empty(),
            "a refusal must leave the other subscriber's inbox empty, got: {others:?}"
        );
        assert_eq!(
            h.ws.version, version_before,
            "a refusal must not bump ws.version — that's what gates a broadcasted snapshot"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Confinement runs before the notification ever reaches `ide::mention`:
    /// a path that escapes the project must be refused by `safe_resolve`
    /// itself, not silently forwarded — there being no listening Claude in
    /// this test would otherwise make an escape indistinguishable from an
    /// ordinary "resolved fine, nobody's connected" refusal.
    ///
    /// Revert-checked: swapping the arm order so `ide::mention` runs first
    /// against the raw `rel` (bypassing `safe_resolve` entirely) failed this
    /// test — the refusal's message became `"no Claude is connected to this
    /// project"`, tripping the `!m.contains("no Claude")` assertion, i.e. the
    /// escaping path silently made it past confinement. Restored.
    #[test]
    fn mention_path_outside_the_project_is_refused_before_reaching_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        let mut h = Hub::new("mentionescape", dir.path().to_path_buf());
        let (asker, rx) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "../../etc/passwd".into(),
            line_start: None,
            line_end: None,
            // session: None — this test is about path confinement, not routing.
            session: None,
        });

        let got: Vec<String> = rx.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && !m.contains("no Claude")),
            "an escaping path must be refused by safe_resolve, not treated as \
             'resolved but no Claude connected': {got:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Two subscribers, for the reason CLAUDE.md records and the sibling
    /// `open_path_refusal_reaches_only_the_client_that_asked` test above
    /// gives: with one subscriber, `send_to` and `broadcast` cannot be told
    /// apart.
    ///
    /// Revert-checked: changing both `Event::Error` arms in `do_mention_path`
    /// from `self.send_to(from, &ev)` to `self.broadcast(&ev)` failed this
    /// test — `others` was no longer empty (`a refusal must leave the other
    /// subscriber's inbox empty, got: ["{\"t\":\"Error\",\"msg\":\"no Claude
    /// is connected to this project\"}"]`) — then restored.
    #[test]
    fn mention_path_refusal_reaches_only_the_client_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("mentionrefuse", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        // No fake Claude is registered for "mentionrefuse", so this resolves
        // fine and then refuses at `ide::mention_to` for lack of a connection.
        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: None,
            line_end: None,
            // session: None — this test is about the refusal reaching only
            // the asker, not about which terminal it was aimed at.
            session: None,
        });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(got.iter().any(|m| m.contains("no Claude")), "the asking client got no refusal: {got:?}");
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(others.is_empty(), "a refusal must leave the other subscriber's inbox empty, got: {others:?}");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// A half-specified line range must be refused by name, not silently
    /// turned into a whole-file mention (the old `Option::zip` behavior) or
    /// a guessed single line. The refusal message is checked specifically —
    /// not just `contains("Error")` — because "no Claude is connected"
    /// (from `ide::mention`, reached only if this check is skipped) would
    /// also satisfy a looser assertion, and this test exists to prove the
    /// rejection happens *before* `ide::mention`, on this exact input shape.
    ///
    /// Revert-checked: reverting the match arm back to
    /// `let lines = line_start.zip(line_end);` failed this test — the
    /// asker's inbox held only `{"t":"Error","msg":"no Claude is connected
    /// to this project"}` (from `ide::mention`, since `zip` silently turned
    /// the half-specified range into `None`), so `got.iter().any(|m|
    /// m.contains("line_start") ...)` was false — then restored.
    #[test]
    fn mention_path_with_a_half_specified_line_range_is_refused_not_guessed() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("mentionhalfrange", dir.path().to_path_buf());
        let (asker, rx) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: Some(5),
            line_end: None,
            // session: None — this test is about the line-range check, which
            // runs before routing is ever consulted.
            session: None,
        });

        let got: Vec<String> = rx.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains("line_start") && m.contains("line_end")),
            "a half-specified range must be refused by name, not silently degraded: {got:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// A session name off the wire is refused before it reaches `ide`, and
    /// the refusal goes only to the client that asked — the same rule the
    /// sibling path-confinement test pins. Two subscribers, because with one
    /// `send_to` and `broadcast` are indistinguishable.
    #[test]
    fn an_invalid_session_name_is_refused_and_only_the_asker_hears() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("mentionsess", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: None,
            line_end: None,
            session: Some("../../etc/passwd".into()),
        });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("session")),
            "an invalid session name must be refused by name, not treated as 'no Claude': {got:?}"
        );
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(others.is_empty(), "a refusal must reach only the asker, got: {others:?}");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Confinement runs before `do_share_selection` ever reaches
    /// `ide::selection_changed`, mirroring `mention_path_outside_the_project_
    /// is_refused_before_reaching_claude` above — with one extra wrinkle this
    /// intent has and `MentionPath` does not: `do_share_selection` swallows
    /// `ide::selection_changed`'s own refusal (sharing off, no Claude
    /// attached — both routine on a signal that fires on every debounced
    /// selection change), so an escaping path silently passing confinement
    /// would look *identical* to the routine case: no `Error` reaches
    /// anyone either way. Only a confinement failure is surfaced, which is
    /// exactly what makes it possible to tell "refused before reaching ide"
    /// apart from "reached ide and ide stayed quiet" from outside the hub.
    ///
    /// Revert-checked: replacing the `safe_resolve` match (and its
    /// `Event::Error` arm) with a bare `self.dir.join(&rel)` — the same
    /// "skip confinement, build the path anyway" shape
    /// `mention_path_outside_the_project_is_refused_before_reaching_claude`'s
    /// own revert uses — failed both this test and the sibling
    /// `share_selection_refusal_reaches_only_the_client_that_asked` below:
    /// the asker's inbox went from one `Error` containing "outside project"
    /// to empty, since the un-confined path (this test's project has no
    /// Claude attached and has not opted in) then fails silently inside
    /// `ide::selection_changed` itself, which `do_share_selection` swallows
    /// on purpose. Restored.
    #[test]
    fn share_selection_outside_the_project_is_refused_before_reaching_ide() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        let mut h = Hub::new("shareselectionescape", dir.path().to_path_buf());
        let (asker, rx) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../../etc/passwd".into(),
            text: "root:x:0:0".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 10,
        });

        let got: Vec<String> = rx.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("outside project")),
            "an escaping path must be refused by safe_resolve before ide::selection_changed \
             ever sees it: {got:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Two subscribers, for the reason CLAUDE.md records: with one, a leak to
    /// every subscriber and a reply to only the asker are indistinguishable.
    /// This is the privacy property that matters most for this intent in
    /// particular — a selection is file content, and it must never reach a
    /// browser that did not select it.
    ///
    /// Revert-checked: changing the confinement failure's `self.send_to(from,
    /// &ev)` to `self.broadcast(&ev)` failed this test — `others` held the
    /// same `Error` the asker got — then restored.
    #[test]
    fn share_selection_refusal_reaches_only_the_client_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        let mut h = Hub::new("shareselectionrefuse", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../../etc/passwd".into(),
            text: "secret".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 1,
        });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(got.iter().any(|m| m.contains("outside project")), "the asking client got no refusal: {got:?}");
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(others.is_empty(), "a refusal must leave the other subscriber's inbox empty, got: {others:?}");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The ordinary case — a project that has not opted in, which is every
    /// project by default — must produce no visible effect at all: no
    /// `Error` (see `do_share_selection`'s doc comment for why that noise is
    /// deliberately swallowed), no broadcast to any subscriber, and no
    /// `ws.version` bump (nothing here should ever cause a `State` to go
    /// out). Without this a future edit that starts broadcasting the
    /// selection to every browser in the project — the exact silent
    /// exfiltration this whole feature exists to prevent — would slip past
    /// every other test in this file, none of which inspect `rx_other` on the
    /// success path.
    ///
    /// Revert-checked: changing the final line to `self.broadcast(&Event::
    /// Error { msg: "test".into() })` unconditionally after the
    /// `ide::selection_changed` call failed this test on the `others.is_
    /// empty()` assertion — then restored.
    #[test]
    fn a_selection_change_on_an_unopted_in_project_reaches_nobody() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("shareselectionoff", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        let version_before = h.ws.version;

        h.handle(&asker, Intent::ShareSelection {
            rel: "a.rs".into(),
            text: "fn main".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 7,
        });

        assert!(rx_asker.try_iter().collect::<Vec<_>>().is_empty(), "an opted-out project must stay silent to the asker too");
        assert!(rx_other.try_iter().collect::<Vec<_>>().is_empty(), "and to every other subscriber");
        assert_eq!(h.ws.version, version_before, "nothing here should ever bump ws.version");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Revert 2 (Step 5): moving `apply_layout` above `resolve_terminal_path`
    /// (using the raw `text` as the rel) failed this test with the actual
    /// panic:
    /// `assertion \`left == right\` failed: a refused path still added a tab
    ///   left: 1
    ///  right: 0`
    #[test]
    fn open_path_refuses_without_touching_the_layout() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        let mut h = Hub::new("linknotab", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();
        let before = h.ws.panes[proto::MIDDLE as usize].tabs.len();

        h.handle(&conn, Intent::OpenPath { text: "../../etc/passwd".into() });

        // The whole reason resolution happens before the layout changes: a
        // dead tab would land in every connected browser's window.
        assert_eq!(
            h.ws.panes[proto::MIDDLE as usize].tabs.len(),
            before,
            "a refused path still added a tab"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// Clicking the same path twice must not stack tabs.
    ///
    /// This is the assertion that proves the handler goes THROUGH
    /// `apply_layout` rather than pushing a tab itself, and it is deliberately
    /// not an image-coercion test: `OpenPath` always asks for `Preview`, and
    /// `coerce_tab` only rewrites `Edit`→`Preview`, so an image assertion here
    /// would hold identically with `apply_layout` bypassed — passing for the
    /// wrong reason. De-duplication lives in `find_tab`, which only
    /// `apply_layout` reaches.
    ///
    /// Revert 3 (Step 5): replacing the `apply_layout` call with a direct
    /// `self.ws.panes[proto::MIDDLE as usize].tabs.push(...)` failed this
    /// test on the count, exactly as expected — the actual panic:
    /// `assertion \`left == right\` failed: opening the same path twice
    ///  stacked tabs: [File { rel: "src/a.rs", mode: Preview }, File { rel:
    ///  "src/a.rs", mode: Preview }]
    ///   left: 2
    ///  right: 1`
    #[test]
    fn open_path_reuses_the_tab_it_already_opened() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), b"x").unwrap();
        let mut h = Hub::new("linktwice", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();

        h.handle(&conn, Intent::OpenPath { text: "src/a.rs".into() });
        h.handle(&conn, Intent::OpenPath { text: "src/a.rs:9".into() });

        let pane = &h.ws.panes[proto::MIDDLE as usize];
        let hits = pane
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::File { rel, .. } if rel == "src/a.rs"))
            .count();
        assert_eq!(hits, 1, "opening the same path twice stacked tabs: {:?}", pane.tabs);
        assert!(
            matches!(pane.tabs.get(pane.active), Some(Tab::File { rel, .. }) if rel == "src/a.rs"),
            "the second open did not activate the existing tab: {pane:?}"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    // --- Task 7: proposal tabs ---

    /// A `Hub` with a private state directory, for the proposal tests below.
    /// They all persist on some path or other, and none of them should write
    /// into this host's real state directory.
    /// Points `RESH_STATE_DIR` at the shared stable test directory rather
    /// than at this test's `TempDir`.
    ///
    /// Not a `STATE_ENV_LOCK` guard: holding that across the test body
    /// deadlocks, because `std::sync::Mutex` is not reentrant and code these
    /// tests reach takes the same lock again. And pointing the global at a
    /// per-test `TempDir` is what leaked — the variable is read on every
    /// `state_dir()` call, so a concurrently-running test writes into this
    /// directory while it is being removed, `TempDir::drop` ignores the
    /// failed removal, and the directory survives silently. A stable path
    /// has nothing to race and nothing to accumulate; the project directory
    /// stays a `TempDir`, since nothing outside this test writes there.
    fn proposal_hub(project: &str) -> (Hub, tempfile::TempDir) {
        crate::wsstate::set_state_dir_for_test();
        let dir = tempfile::tempdir().unwrap();
        (Hub::new(project, dir.path().to_path_buf()), dir)
    }

    /// Two subscribers, deliberately: with one, `broadcast` and `send_to` are
    /// indistinguishable, and a proposal that reached only the browser that
    /// happened to be first would leave the other one showing a tab it has no
    /// content for.
    ///
    /// Revert-checked: broadcasting the `State` snapshot *before* the
    /// `Proposal` event failed this test — `panicked ... "the content must
    /// arrive before the tab that draws it"` — then restored. Also checked by
    /// dropping the `Proposal` broadcast entirely, which failed on "no
    /// Proposal event reached ...".
    #[test]
    fn a_proposal_sends_both_sides_to_every_browser_before_the_tab_that_draws_them() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut h, _d) = proposal_hub("proposal-broadcast");
        let (_a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();

        h.open_proposal_tab("p-1", "src/a.rs", "on disk", "proposed");

        for (who, rx) in [("a", &rx_a), ("b", &rx_b)] {
            let got: Vec<String> = rx.try_iter().collect();
            let at_proposal = got.iter().position(|m| m.contains(r#""t":"Proposal""#));
            let at_state = got.iter().position(|m| m.contains(r#""t":"State""#));
            let at_proposal =
                at_proposal.unwrap_or_else(|| panic!("no Proposal event reached {who}: {got:?}"));
            let at_state =
                at_state.unwrap_or_else(|| panic!("no State event reached {who}: {got:?}"));
            assert!(at_proposal < at_state, "the content must arrive before the tab that draws it");
            let ev = &got[at_proposal];
            // Both sides, or there is nothing to diff: the new side has never
            // been written anywhere, and the old side is what makes an
            // overwrite distinguishable from a creation.
            assert!(ev.contains(r#""old_text":"on disk""#), "{who} got {ev}");
            assert!(ev.contains(r#""new_text":"proposed""#), "{who} got {ev}");
            assert!(ev.contains(r#""rel":"src/a.rs""#), "{who} got {ev}");
        }
        assert!(
            h.ws.panes[proto::MIDDLE as usize]
                .tabs
                .iter()
                .any(|t| matches!(t, Tab::Proposal { id } if id == "p-1")),
            "the tab itself must exist too"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// One proposal is one question, so its tab has to go everywhere it was
    /// drawn — the same reasoning `EndSession` gives for terminal tabs.
    ///
    /// Revert-checked: restricting `close_proposal_tab`'s loop to the middle
    /// pane failed this test — `panicked ... "a proposal tab survived in
    /// another pane: [Terminal { session: \"term\" }, Proposal { id: \"p-1\" }]
    /// (pane 3)"` — then restored. Separately, deleting the `if p.active > i`
    /// shift failed the last assertion — `left: Some(Changes) / right:
    /// Some(Tree)`.
    #[test]
    fn closing_a_proposal_clears_it_from_every_pane_and_keeps_the_active_tab() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut h, _d) = proposal_hub("proposal-close");
        let mid = proto::MIDDLE as usize;
        let right = proto::RIGHT as usize;
        // Three tabs, with the active one *between* the proposal and the
        // end: with only two, clamping to `len - 1` lands on the right tab
        // by accident and the index shift below cannot be observed at all.
        // Checked — the two-tab version of this test passed with the shift
        // deleted.
        h.ws.panes[mid].tabs =
            vec![Tab::Proposal { id: "p-1".into() }, Tab::Tree, Tab::Changes];
        h.ws.panes[mid].active = 1;
        h.ws.panes[right].tabs.push(Tab::Proposal { id: "p-1".into() });

        h.close_proposal_tab("p-1");

        for (i, p) in h.ws.panes.iter().enumerate() {
            assert!(
                !p.tabs.iter().any(|t| matches!(t, Tab::Proposal { .. })),
                "a proposal tab survived in another pane: {:?} (pane {i})",
                p.tabs
            );
        }
        assert_eq!(
            h.ws.panes[mid].tabs.get(h.ws.panes[mid].active),
            Some(&Tab::Tree),
            "closing the tab to the left of the active one must not activate a different tab"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The whole point of the feature: the click **is** the permission
    /// answer, and a proposal the user rewrote before accepting must travel
    /// back as the content Claude writes.
    ///
    /// Revert-checked: making the `(true, Some(t))` arm produce
    /// `Answer::Accepted` (dropping the user's text) failed this test —
    /// `left: String("TAB_CLOSED") / right: "FILE_SAVED"` — then restored.
    /// That is the defect that would make Claude write the version the user
    /// rejected.
    #[test]
    fn accepting_an_edited_proposal_sends_the_users_own_text_back_and_closes_the_tab() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut h, _d) = proposal_hub("proposal-accept");
        let (tx, rx) = channel();
        let id = crate::ide::park_for_test("proposal-accept", "p1", tx);
        h.open_proposal_tab(&id, "a.rs", "old", "claude's version");
        let (conn, _crx) = h.subscribe();

        h.handle(
            &conn,
            Intent::AnswerProposal {
                id: id.clone(),
                accept: true,
                text: Some("the human's version".into()),
            },
        );

        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "FILE_SAVED");
        assert_eq!(v["result"]["content"][1]["text"], "the human's version");
        assert!(
            !h.ws.panes.iter().any(|p| p.tabs.iter().any(|t| matches!(t, Tab::Proposal { .. }))),
            "an answered proposal's tab must not linger"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// A rejection carrying text is still a rejection. Without this, a client
    /// could get content past a "no" simply by sending it alongside.
    ///
    /// Revert-checked: reordering the match so `(_, Some(t))` produced
    /// `AcceptedEdited` regardless of `accept` failed this test — `left:
    /// String("FILE_SAVED") / right: "DIFF_REJECTED"` — then restored.
    #[test]
    fn rejecting_ignores_any_text_that_came_with_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut h, _d) = proposal_hub("proposal-reject");
        let (tx, rx) = channel();
        let id = crate::ide::park_for_test("proposal-reject", "p1", tx);
        h.open_proposal_tab(&id, "a.rs", "old", "new");
        let (conn, _crx) = h.subscribe();

        h.handle(
            &conn,
            Intent::AnswerProposal {
                id: id.clone(),
                accept: false,
                text: Some("smuggled".into()),
            },
        );

        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "DIFF_REJECTED");
        assert!(v["result"]["content"].get(1).is_none(), "a rejection carries no content: {v}");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The spec's third row: "reject, or close the tab". Without it the tab
    /// vanishes and Claude stays blocked on a request nothing left in the UI
    /// could ever answer.
    ///
    /// Revert-checked: deleting the `closing_proposal` arm from `CloseTab`
    /// failed this test — `called Result::unwrap() on an Err value: Timeout`,
    /// after waiting out the full 2s budget — i.e. Claude was never answered
    /// at all. Then restored.
    #[test]
    fn closing_a_proposal_tab_by_hand_rejects_it_rather_than_stranding_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut h, _d) = proposal_hub("proposal-closetab");
        let (tx, rx) = channel();
        let id = crate::ide::park_for_test("proposal-closetab", "p1", tx);
        h.open_proposal_tab(&id, "a.rs", "old", "new");
        let (conn, _crx) = h.subscribe();
        let mid = proto::MIDDLE as usize;
        let idx = h.ws.panes[mid]
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Proposal { .. }))
            .expect("the proposal tab must be there to close");

        h.handle(&conn, Intent::CloseTab { pane: proto::MIDDLE, idx });

        let v: serde_json::Value = serde_json::from_str(
            &rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "DIFF_REJECTED");
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// `openDiff` asks this before parking anything: accepting a proposal for
    /// a file the user has unsaved edits to would discard them silently.
    ///
    /// Goes through `Hub::for_project`, not `Hub::new`, because the registry
    /// lookup is half of what is under test — `ide.rs` has no hub of its own
    /// to ask.
    ///
    /// Revert-checked: making `has_dirty_buffer` return
    /// `buffers.contains_key(rel)` instead of testing `dirty()` failed this
    /// test — `panicked ... "a clean buffer is not unsaved work"` — then
    /// restored.
    #[test]
    fn has_dirty_buffer_is_about_unsaved_text_and_about_this_project_only() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        isolate_ide_dir_for_tests();
        let hub = Hub::for_project("dirtyprobe", d.path().to_path_buf());
        assert!(!has_dirty_buffer("dirtyprobe", "a.rs"), "no buffer at all is not unsaved work");
        {
            let mut h = Hub::lock(&hub);
            h.ws.buffers.insert("a.rs".into(), workspace::Buffer::default());
        }
        assert!(!has_dirty_buffer("dirtyprobe", "a.rs"), "a clean buffer is not unsaved work");
        {
            let mut h = Hub::lock(&hub);
            h.ws.buffers.insert(
                "a.rs".into(),
                workspace::Buffer {
                    content: workspace::Content::Edited("typed".into()),
                    ..workspace::Buffer::default()
                },
            );
        }
        assert!(has_dirty_buffer("dirtyprobe", "a.rs"));
        assert!(!has_dirty_buffer("dirtyprobe", "b.rs"), "and only about the file asked about");
        assert!(
            !has_dirty_buffer("dirtyprobe-not-open", "a.rs"),
            "a project with no hub holds no buffers, which is a fact and not a failed check"
        );
        std::env::remove_var("RESH_STATE_DIR");
    }

    /// The free functions `ide.rs` actually calls, exercised through the
    /// registry they look their hub up in — `open_proposal_tab` being right
    /// is worth nothing if `open_proposal` routes to the wrong project.
    ///
    /// Two projects, deliberately: with one, "opened it in the project that
    /// asked" and "opened it in whatever hub was handy" are the same
    /// observation.
    ///
    /// Revert-checked: making `open_hub` ignore the key it was given (taking
    /// the highest-sorting registry entry instead, which is deterministically
    /// the *other* project here) failed this test — `panicked ... "the
    /// proposal never reached the project that asked"` — then restored.
    #[test]
    fn open_and_close_proposal_act_on_the_project_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        isolate_ide_dir_for_tests();
        let a = Hub::for_project("proposal-route-a", d.path().to_path_buf());
        let b = Hub::for_project("proposal-route-b", d.path().to_path_buf());
        let has_tab = |h: &Arc<Mutex<Hub>>| {
            Hub::lock(h)
                .ws
                .panes
                .iter()
                .any(|p| p.tabs.iter().any(|t| matches!(t, Tab::Proposal { id } if id == "p-7")))
        };

        open_proposal("proposal-route-a", "p-7", "a.rs", "old", "new");
        assert!(has_tab(&a), "the proposal never reached the project that asked");
        assert!(!has_tab(&b), "a proposal opened in the wrong project");

        close_proposal("proposal-route-a", "p-7");
        assert!(!has_tab(&a), "close_proposal left the tab behind");
        std::env::remove_var("RESH_STATE_DIR");
    }
}
