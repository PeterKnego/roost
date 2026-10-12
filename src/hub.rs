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
    /// Paths roost itself just wrote, with the resulting hash. The watcher
    /// (Task 8) drops matching events so a save does not echo back.
    pub self_writes: HashMap<String, u64>,
    /// Set once a filesystem watcher has been spawned for this hub, so
    /// `for_project` starts at most one watcher per project even though it
    /// runs on every connection.
    pub watching: bool,
    /// Resident name → its home, for sessions re-homed into this project
    /// (`home.rs`). Rebuilt by `refresh_live_sessions`, never persisted: it
    /// is derived from the records and the sockets, and a saved copy could
    /// only ever be stale.
    pub session_homes: std::collections::BTreeMap<String, String>,
    /// Native (never resident) live session names this project has a
    /// recorded Claude session id for (`claudesess::recorded`) — what the
    /// `+` dialog's fork choices are built from. Rebuilt by
    /// `refresh_live_sessions`, the same way and for the same reason as
    /// `session_homes` just above: a small local file read per live
    /// session, cheap at the per-project cap of 16, but `snapshot_event`
    /// runs on every debounced `EditBuffer` and would turn "cheap once" into
    /// a filesystem read per keystroke if it looked here instead. Native
    /// only (never a resident's name) — forking means continuing the
    /// conversation of a *terminal on this page*, and a resident's record
    /// lives, and is forkable, at its own home's refresh instead.
    pub forkable: Vec<String>,
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
    /// Cached result of `claudehooks::state(&self.dir)`. Outer `None` means
    /// "not computed since the last invalidation"; `snapshot_event` fills it
    /// on demand and reuses it otherwise. Without this, a filesystem read
    /// under the hub lock ran on every debounced keystroke — `snapshot_event`
    /// is called from nearly every intent arm, including `EditBuffer` — which
    /// is exactly the per-keystroke filesystem cost `WorkspaceView::show_hidden`'s
    /// doc comment refuses for the same reason. It is set back to `None` in
    /// exactly three places: the `SetClaudeHooks` arm (the write it just made
    /// must be reflected immediately), the `RequestState` arm (a client's
    /// explicit "tell me the truth now"), and `wsconn`'s initial snapshot for
    /// a newly connecting client. That set covers every case CLAUDE.md's
    /// design promises a refresh for — reconnect, an explicit refresh, and
    /// the toggle's own write — while a hand edit made between those events
    /// (Claude Code rewriting the file, say) is *not* one of them: it shows
    /// up at the next refresh or reconnect, not sooner, exactly as the spec's
    /// "Live following is deliberately not added" says.
    pub claude_hooks: Option<Option<bool>>,
    /// Same cache, same three invalidation sites, for
    /// `claudehooks::Kind::Worktree` instead of `Kind::Notify`. Kept as a
    /// separate field rather than keying one cache by `Kind` because the two
    /// are read and invalidated together everywhere that touches either —
    /// a map would buy nothing and cost a lookup at both sites.
    pub worktree_hooks: Option<Option<bool>>,
    /// Cached result of `config::settings_view(&self.dir)`, same shape and
    /// same reason as `claude_hooks` above: that call is a `load` of both
    /// config files plus a `raw_setting` read per key per file — on the
    /// order of twenty file reads — and `snapshot_event` runs on every
    /// debounced `EditBuffer`, so an uncached call there is exactly the
    /// per-keystroke filesystem cost `claude_hooks` already exists to avoid.
    /// Invalidated at the same three sites as `claude_hooks` (the
    /// `SetClaudeHooks` arm, the `RequestState` arm, and `wsconn`'s initial
    /// snapshot for a newly connecting client — see `claude_hooks`'s doc
    /// comment for why each of those is a "tell the truth now" moment), plus
    /// the `SetSetting` arm after a successful write, which is the one this
    /// cache adds. A hand edit to a config file made between those events is
    /// not reflected sooner, same as `claude_hooks`; the settings dialog
    /// opening will ask for a fresh `RequestState` itself (a later task).
    pub settings: Option<crate::proto::SettingsView>,
    /// Worktree asks from Claude's hook that no browser has answered yet
    /// (`ask_worktree_blocking`). Whoever *removes* an entry owns its
    /// `reply` sender and is the only one who may answer it — the answer
    /// handler, or the asker's own timeout — so exactly one reply is ever
    /// sent, by construction rather than by a flag. Never persisted: the
    /// hook connection it answers does not survive a restart either.
    asks: Vec<PendingAsk>,
}

/// How long Claude's worktree ask waits for a browser before it is refused.
/// Inside the hook client's own `cli::REPLY_TIMEOUT` (90 s), so the server
/// always answers before the client gives up — a reply the client never
/// reads would leave a worktree Claude was never told about.
pub const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// At most this many unanswered asks per project (R22). Each one holds a
/// hook connection thread (bounded process-wide by `hooksock`) and a dialog
/// in every browser; per project, like `ide::MAX_PENDING`, so one Claude in
/// a loop cannot starve the others.
pub const MAX_PENDING_ASKS: usize = 4;

/// Process-wide, so an id never repeats across projects or hub rebuilds, and
/// never derived from the request: the id travels to every browser and back,
/// and nothing Claude sent should be able to choose it.
static NEXT_ASK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

const NO_TAB: &str = "no roost tab is open on this project to approve it";

struct PendingAsk {
    id: String,
    session: String,
    name: String,
    reply: Sender<crate::hooksock::AskReply>,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<Hub>>>>> = OnceLock::new();

/// Terminal tabs the restored layout asks for whose shell is positively gone.
///
/// roost survives its own restart — dtach masters reparent to init, and
/// `KillMode=process` keeps systemd from taking them — so after a restart
/// these tabs simply reattach. A *reboot* is different: no process outlives a
/// kernel, and the tabs come back pointing at nothing.
///
/// Both used to render the same "Press Enter to start a terminal"
/// placeholder, so a reboot that lost a long-running Claude looked exactly
/// like a tab nobody had used yet. Naming them is the difference.
///
/// Only positive evidence counts. `socket_names_checked` returning `None`
/// means the socket directory could not be read, and telling a user their
/// shell is gone on the strength of a failed `read_dir` would be a false
/// claim about a shell that is still running — the same conflation
/// CLAUDE.md's table is entirely about, here in its "says something untrue"
/// form rather than its destructive one.
/// Parks a launch on every lost terminal roost had itself launched an agent
/// in, so the shell that replaces it comes back running the same thing.
///
/// Step 3 of #17, and everything careful about it is in `relaunch`'s module
/// doc: off unless asked for, global-only, the launch kind and never a resume.
/// One more restraint lives here rather than there — this runs when a project
/// is *opened*, not at boot. #17 asks for boot and this deliberately does
/// less: a boot-time relaunch is the version with nobody in front of it, and
/// doing it here keeps a person at the screen while an agent starts.
///
/// It arms rather than spawns. A `reserve` is permission for the next attach,
/// which happens when the browser mounts the tab and asks — so nothing starts
/// in a project nobody opened, and a tab the user closes first never fires.
fn rearm_launches(lost: &[String], project: &str) {
    rearm_launches_with(lost, project, crate::config::relaunch())
}

/// The reserving half, with the setting passed in.
///
/// Split out for the same reason `launches_to_rearm` was — a test that had to
/// write the global config would race every other test for one environment
/// variable — but for a different property: `launches_to_rearm` decides *which*
/// terminals rearm, and this decides *what they are handed*. Since #18 step 2
/// there is a second thing they could be handed, and #17 says they must not be;
/// that is only assertable if the reserving can be driven directly.
fn rearm_launches_with(lost: &[String], project: &str, enabled: bool) {
    for (name, launch) in launches_to_rearm(lost, project, enabled) {
        // `session: None` on purpose. #17: resuming would continue a
        // conversation whose last turn may have been mid-edit, which it calls
        // worse rather than better. A fresh one, in the same terminal.
        crate::session::reserve(
            project,
            &name,
            Some(crate::session::LaunchRequest { launch, session: None }),
        );
    }
}

/// The decision, separated from the reserving so it can be tested without the
/// process-global config path.
///
/// `enabled` is passed in rather than read here for the same reason: a test
/// that had to write `~/.config/roost/config.toml` to check "off means
/// nothing happens" would be racing every other test in the binary for one
/// environment variable — and "off means nothing happens" is the property this
/// whole feature's safety rests on.
fn launches_to_rearm(
    lost: &[String],
    project: &str,
    enabled: bool,
) -> Vec<(String, crate::proto::Launch)> {
    if !enabled {
        return Vec::new();
    }
    lost.iter()
        .filter_map(|n| crate::relaunch::recorded(project, n).map(|l| (n.clone(), l)))
        .collect()
}

fn lost_terminal_sessions(ws: &crate::workspace::Workspace, project: &str) -> Vec<String> {
    let Some(on_disk) = crate::session::socket_names_checked(project) else {
        return Vec::new(); // could not look: claim nothing
    };
    let mut out: Vec<String> = ws
        .panes
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter_map(|t| match t {
            crate::proto::Tab::Terminal { session, .. } => Some(session.clone()),
            _ => None,
        })
        // Three conditions, and `sessions_seen` is the one that makes this a
        // claim rather than a guess: roost watched a shell run in that session
        // and wrote the fact down. The default layout ships an empty `term`
        // tab, so without it every new project announced the loss of a shell
        // it had never started.
        .filter(|s| ws.sessions_seen.contains(s))
        .filter(|s| !on_disk.contains(s) && !ws.live_sessions.contains(s))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Which of the lost sessions roost recorded a Claude for.
///
/// Absence is *no record*, never a claim that no Claude ran there — the whole
/// reason `claudesess::recorded` returns an `Option` (see its module doc).
/// Nothing destructive keys on this, so an unreadable record costs an offer
/// that is not made, which is the safe direction.
fn resumable_terminal_sessions(lost: &[String], project: &str) -> Vec<String> {
    lost.iter()
        .filter(|s| crate::claudesess::recorded(project, s).is_some())
        .cloned()
        .collect()
}

/// Whether `project` may still end `kill_target`/`name` right now — for a
/// resident (`kill_target != project`), only when the re-home record,
/// re-read at this exact moment, still names `project` as the owner; for a
/// native session (`kill_target == project`), only when that same re-read
/// says `Here` — never assumed from `kill_target == project` alone.
///
/// R16, Important 1 (resident branch): `do_end_session` reads
/// `kill_target` from `session_homes` once, under the hub lock, before the
/// kill itself runs — on its own thread, or inline when none was spawned —
/// so a "Move back" or a re-point landing in between would otherwise
/// SIGKILL a session this owner no longer has any claim to.
///
/// Fix round 2, New Issue 2 (native branch): `session_homes` holds
/// residents *of* this project, so it says nothing about a *native*
/// session that was itself re-homed *away* — `kill_target == project`
/// still holds for it, with no record anywhere upstream to have refused
/// on its behalf (the end-session confirm dialog's own staleness window,
/// or a second browser, can land `EndSession` after exactly that move).
/// Positive evidence is required on both branches now, the same rule
/// `session::kill_project_inner` already applies to its own kills
/// (`session.rs` ~1419-1430 for the native case, ~1436 for residents):
/// `Elsewhere` and `Unknown` both refuse, never just "not `Elsewhere` of
/// someone else."
fn may_end(kill_target: &str, name: &str, project: &str) -> bool {
    if kill_target == project {
        matches!(crate::home::owner_of(project, name), crate::home::Owner::Here)
    } else {
        matches!(crate::home::owner_of(kill_target, name), crate::home::Owner::Elsewhere(o) if o == project)
    }
}

impl Hub {
    pub fn new(project: &str, dir: std::path::PathBuf) -> Hub {
        let (ws, warn) = crate::wsstate::load(project);
        if let Some(w) = warn {
            eprintln!("roost: {w}");
        }
        let mut hub = Hub {
            project: project.to_string(),
            dir,
            ws,
            subs: HashMap::new(),
            next_id: 0,
            self_writes: HashMap::new(),
            watching: false,
            session_homes: Default::default(),
            forkable: Vec::new(),
            self_ref: std::sync::Weak::new(),
            closing: false,
            notices_dirty: false,
            proposals: HashMap::new(),
            claude_hooks: None,
            worktree_hooks: None,
            settings: None,
            asks: Vec::new(),
        };
        // A freshly loaded hub must report reality, not whatever the
        // persisted layout happened to say last time: sessions may have
        // died (or kept running under dtach) since the last save, and
        // `.git` may have appeared or vanished on disk in the meantime.
        hub.refresh_live_sessions();
        // Once, here, and only here: this is the single moment the restored
        // layout can be compared against what actually survived. A tab whose
        // shell is gone is indistinguishable a second later, because by then
        // the user may simply not have started it yet.
        hub.ws.lost_sessions = lost_terminal_sessions(&hub.ws, project);
        hub.ws.resumable_sessions = resumable_terminal_sessions(&hub.ws.lost_sessions, project);
        rearm_launches(&hub.ws.lost_sessions, project);
        hub.reconcile_buffers_with_disk();
        // The restored layout's terminal tabs are as much a request for those
        // sessions as a fresh click is — including `default_layout`'s own
        // `term`, which is every project's first terminal and goes through no
        // intent at all.
        hub.sync_reservations();
        hub
    }

    /// Re-reads the layout from disk, discarding what this hub holds.
    ///
    /// Exists for exactly one caller: a restore (#18 step 3) has just replaced
    /// the state file underneath a hub that was built from the old one. Without
    /// this the restore appears to work and then silently reverts — the hub
    /// still holds the previous layout in memory, and the next intent that
    /// touches it calls `wsstate::save` and writes that layout straight back
    /// over the restored file. Nothing anywhere would say so.
    ///
    /// Deliberately narrow and deliberately not a general "reload" the UI can
    /// reach: it drops in-memory buffers, which for a *dirty* buffer means
    /// dropping unsaved text. That is safe here only because `restore::run`
    /// refuses while any session of the project is live and the caller sends
    /// the fresh snapshot immediately; it would not be safe as a button.
    pub fn reload_from_disk(&mut self) {
        let project = self.project.clone();
        let (ws, warn) = crate::wsstate::load(&project);
        if let Some(w) = warn {
            eprintln!("roost: {w}");
        }
        self.ws = ws;
        // The same three steps `new` runs for the same reason: a restored
        // layout describes what was true on another machine, and reality here
        // is whatever actually survived.
        self.refresh_live_sessions();
        self.ws.lost_sessions = lost_terminal_sessions(&self.ws, &project);
        self.ws.resumable_sessions =
            resumable_terminal_sessions(&self.ws.lost_sessions, &project);
        self.reconcile_buffers_with_disk();
        self.sync_reservations();
    }

    /// Restored buffers describe what was true when roost last wrote the state
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
            let ms: u64 = std::env::var("ROOST_DEBOUNCE_MS")
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

    /// This workspace's tree-visibility override, or `None` when it is still
    /// following the config file. Answers from the registry without creating
    /// a hub, for the reason `show_hidden` gives below: a
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

    /// Everything a search needs, copied out under the lock so the walk can
    /// run without it.
    ///
    /// Deliberately does *not* resolve the config here: `config::for_project`
    /// reads files, and this method's whole purpose is that its caller can
    /// drop the lock immediately. The worker resolves the settings itself,
    /// off-lock, from `dir`.
    pub fn search_snapshot(&self) -> crate::search::Snapshot {
        crate::search::Snapshot {
            dir: self.dir.clone(),
            show_hidden_override: self.ws.show_hidden,
            // `ws.live_sessions`, never `session::list_sessions`: that forks
            // a `ps` per session while holding the global session-registry
            // mutex (see `refresh_live_sessions`), which would reintroduce
            // exactly the stall this snapshot exists to avoid.
            sessions: self.ws.live_sessions.clone(),
        }
    }

    pub fn snapshot_event(&mut self, origin: &ConnId) -> Event {
        let mut ws = self.ws.view();
        // Derived here rather than kept in `WsState`: `wsstate::save` persists
        // that struct, and which terminal is running a Claude is true of the
        // running host, not of the saved layout. Reading it back from disk
        // would mark tabs from a previous boot.
        ws.claude_sessions = crate::claudes::cached_sessions(&self.project);
        // Same reasoning: a fact about which sockets are actually live right
        // now, not about the saved layout, so it is never persisted and is
        // filled fresh on every snapshot from the in-memory `session_homes`
        // `refresh_live_sessions` just rebuilt.
        ws.session_homes = self.session_homes.clone();
        // Same reasoning again: `refresh_live_sessions` just rebuilt this
        // from the live, native session set, and copying it is free where
        // recomputing it here — on every debounced `EditBuffer` — would not
        // be (see `Hub::forkable`'s own doc comment).
        ws.forkable = self.forkable.clone();
        // See the doc comment on `claude_hooks` for why this is cached
        // rather than read on every call.
        let cached = *self.claude_hooks.get_or_insert_with(|| {
            match crate::claudehooks::state(&self.dir) {
                crate::claudehooks::HookState::Present => Some(true),
                crate::claudehooks::HookState::Absent => Some(false),
                crate::claudehooks::HookState::Unknown(_) => None,
            }
        });
        ws.claude_hooks = cached;
        // Same cache, same reason, for `Kind::Worktree`.
        let worktree_cached = *self.worktree_hooks.get_or_insert_with(|| {
            match crate::claudehooks::state_of(&self.dir, crate::claudehooks::Kind::Worktree) {
                crate::claudehooks::HookState::Present => Some(true),
                crate::claudehooks::HookState::Absent => Some(false),
                crate::claudehooks::HookState::Unknown(_) => None,
            }
        });
        ws.worktree_hooks = worktree_cached;
        // See the doc comment on `settings` for why this is cached rather
        // than read on every call.
        ws.settings = self.settings.get_or_insert_with(|| crate::config::settings_view(&self.dir)).clone();
        Event::State { version: self.ws.version, origin: origin.clone(), ws }
    }

    /// Every terminal tab in this layout is reserved, so the browser that
    /// mounts it may spawn its shell.
    ///
    /// A tab *is* the server's record that this session is wanted — that is
    /// what a layout is. Reservations that only came from `NewTerminal` were
    /// not enough, because the two commonest terminals in the product never
    /// go through it: `Workspace::default_layout` seeds pane 3 with `term`,
    /// so a fresh project's first terminal comes straight from the layout,
    /// and a restored layout's tabs do the same on every reload. Without this
    /// both were refused and no terminal could be started at all — caught by
    /// `reconnect.mjs`, which failed at "start a terminal", not by any Rust
    /// test, because none of them mounts a tab.
    ///
    /// Additive: `reserve_if_absent` never disturbs a launch already parked
    /// for a name (`✻` reserves before the tab exists). Nothing here removes
    /// reservations — `do_close_project` drops the whole project's in one
    /// go, which is the case that matters.
    ///
    /// `home: None` tabs only: a resident's reservation belongs under its
    /// *home*, not here — that is where the browser's own follow-up connect
    /// actually lands (it dials `tab.home`), and reserving the name under
    /// this project too would park a second, never-consumed reservation at
    /// an address nothing ever spawns through.
    fn sync_reservations(&self) {
        for p in &self.ws.panes {
            for t in &p.tabs {
                if let Tab::Terminal { session, home: None } = t {
                    crate::session::reserve_if_absent(&self.project, session);
                }
            }
        }
    }

    fn persist(&mut self) {
        if let Err(e) = crate::wsstate::save(&self.project.clone(), &self.ws) {
            eprintln!("roost: state save failed: {e}");
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
                // This is the explicit "tell me the truth now": one of the
                // three places `claude_hooks` (and, for the same reason,
                // `settings`) is invalidated (see their doc comments) — a
                // hand edit to the settings file made since the last
                // snapshot must show up here even though it does not show
                // up on every keystroke.
                self.claude_hooks = None;
                self.worktree_hooks = None;
                self.settings = None;
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
                // Scoped to this hub's project, because the panel the button
                // sits in is: "Mark all read" can only mean the rows it is
                // under.
                crate::notify::mark_all_read_in(&self.project);
                // Same reasoning as MarkNoticeRead above: dirty-flag, don't
                // broadcast from in here.
                self.notices_dirty = true;
                return;
            }
            Intent::ClearNotices => {
                // Scoped for a sharper reason than the button above: an
                // unscoped `clear()` from a panel showing one project
                // destroys history the user cannot see and does not know is
                // there. See `notify::clear_in`.
                crate::notify::clear_in(&self.project);
                self.notices_dirty = true;
                return;
            }
            Intent::SetClaudeHooks { on } => {
                // A file write under the hub lock, like CreateFile below:
                // one small file, and the state it changes is what every
                // client of this project is about to be sent.
                if let Err(e) = crate::claudehooks::set(&self.dir, *on) {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
                self.ws.version += 1;
                // The write above just changed what the file says; the
                // cached read from before it must not be reused for the
                // broadcast that reports it (see `claude_hooks`'s doc
                // comment — this is one of its three invalidation sites,
                // matched here by `settings` for the same reason).
                self.claude_hooks = None;
                self.settings = None;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                return;
            }
            Intent::SetWorktreeHooks { on } => {
                // Sibling of SetClaudeHooks immediately above, same shape,
                // for `Kind::Worktree` instead of `Kind::Notify`.
                if let Err(e) = crate::claudehooks::set_kind(&self.dir, crate::claudehooks::Kind::Worktree, *on) {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
                self.ws.version += 1;
                // Same invalidation as SetClaudeHooks, for the same reason:
                // the write just made must not be shadowed by a cached read
                // taken before it.
                self.worktree_hooks = None;
                self.settings = None;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                return;
            }
            Intent::SetSetting { scope, key, value } => {
                // A file write under the hub lock, like SetClaudeHooks: one
                // small file, and what it says next is what every client of
                // this project is about to be sent. The global file takes a
                // process-wide lock inside set_setting as well.
                if let Err(e) = crate::config::set_setting(*scope, &self.dir, key, value.as_ref()) {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
                // The write above just changed what the config files say;
                // the cached read from before it must not be reused for the
                // broadcast that reports it (see `settings`'s doc comment —
                // this is the one invalidation site that field adds beyond
                // `claude_hooks`'s three).
                self.settings = None;
                // Bumped before the `show_hidden` block's `persist()` below,
                // matching every other site in this file: the state written
                // to disk must carry the version that describes it, not the
                // one before.
                self.ws.version += 1;
                if key == "show_hidden" {
                    // The header toggle's override outranks the file in both
                    // directions (workspace.rs). Writing the file from the
                    // dialog is the one gesture that means "follow the file
                    // again"; without this a person who set the file to
                    // true and sees no change has no way to find out why.
                    self.ws.show_hidden = None;
                    self.persist();
                }
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
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
            Intent::StartTerminal { session, resume } => {
                return self.do_start_terminal(from, session.clone(), *resume)
            }
            Intent::InitGit => return self.do_init_git(from),
            Intent::CloseProject => return self.do_close_project(from),
            Intent::EndSession { session } => return self.do_end_session(from, session.clone()),
            Intent::NewWorktree { name, launch } => {
                return self.do_new_worktree(from, name.clone(), launch.clone())
            }
            Intent::RemoveWorktree { key } => return self.do_remove_worktree(from, key.clone()),
            Intent::NewTerminal { pane, launch, force, resume, fork } => {
                return self.do_new_terminal(from, *pane, *launch, *force, resume.clone(), fork.clone())
            }
            Intent::OpenPath { text } => return self.do_open_path(from, text.clone()),
            Intent::OpenAtLine { pane, rel, line } => {
                return self.do_open_at_line(from, *pane, rel.clone(), *line)
            }
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
            Intent::WorktreeAnswer { id, approve } => {
                return self.do_worktree_answer(from, id.clone(), *approve)
            }
            // A search performs an unbounded filesystem walk, so `wsconn.rs`
            // diverts it to a worker thread *before* this lock is taken —
            // that divert is the entire enforcement of CLAUDE.md's "never
            // hold a lock across blocking I/O" for this feature. Enumerated
            // here rather than left to the `_` arm below so that reordering
            // the divert, or adding a second dispatch site, fails loudly
            // instead of quietly running the walk under the hub lock and
            // stalling every browser on the project with nothing to say so.
            Intent::Search { .. } => {
                unreachable!("Search is diverted in wsconn before this lock is taken")
            }
            // Same reasoning as `Search` above, and the same deliberate panic
            // rather than a fall-through: a restore reads an archive the size
            // of an upload and writes the state directory, so running it under
            // this lock would stall every browser on the project. Named here
            // so that adding a second dispatch site fails loudly.
            Intent::RestoreWorkspace { .. } => {
                unreachable!("RestoreWorkspace is diverted in wsconn before this lock is taken")
            }
            // Diverted in wsconn for the same reason as the two above: git
            // under this lock would stall every browser on the project.
            Intent::RevertPreview { .. } | Intent::Revert { .. } => {
                unreachable!("revert intents are diverted in wsconn before this lock is taken")
            }
            // Meant to be diverted in wsconn like the ones above: a download
            // must not run under this lock, and a choice is a file write that
            // every hub then hears about. But unlike those arms this one is
            // reachable — the intents exist before their divert does, and any
            // page `Origin` admits can forge one — so it refuses rather than
            // panics: a panic here kills the socket thread holding this lock.
            // Kept after the divert lands, as the answer to a second dispatch
            // site. Logged to stderr only, not `errlog::record`: that appends
            // a file under this lock, once per forged frame, with no bound.
            Intent::Update | Intent::DeferUpdate | Intent::SkipUpdate { .. } => {
                eprintln!("roost: {}: update intent reached the hub; refused", self.project);
                self.send_to(from, &Event::Error { msg: "update intents are handled before the hub".into() });
                return;
            }
            _ => {}
        }
        // R16's invariant (at most one live terminal tab per name in an
        // owner) means a client that still thinks a resident's name is
        // native is simply wrong, not offering a legitimate second tab:
        // `focusSession`'s own-state miss and a session search result row
        // (app.js) both send a bare `OpenTab{tab:{k:"Terminal", session}}`
        // with no `home` at all, because neither has any way to know it —
        // a search hit is a session *name*, and `focusSession`'s pre-check
        // is a race away from missing a tab that just arrived. Filling in
        // `home` here, before `apply_layout`, is what makes `find_tab`
        // match and focus the resident's actual tab instead of creating a
        // second, native one and reserving `(this project, session)` for a
        // name this project's own `sock/` can never hold (R16 point 3).
        //
        // R17 (fix round 2): normalised unconditionally, not only when the
        // intent arrives with `home: None` — a forged `OpenTab` naming a
        // `home` of its own has no more claim to it than a bare one does.
        // `session_homes` is the only source of truth for what `home`
        // means on this page; nothing a client sends can override it.
        let intent = match intent {
            Intent::OpenTab { pane, tab: Tab::Terminal { session, home: _ } } => {
                let home = self.session_homes.get(&session).cloned();
                Intent::OpenTab { pane, tab: Tab::Terminal { session, home } }
            }
            other => other,
        };
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
        // A terminal tab closed before any browser attached leaves a
        // reservation behind — permission for the *next* attach on that name
        // to spawn, carrying whichever launch the click asked for. It used to
        // be cleaned up by accident: the next allocation reserved over it, and
        // names were handed out in one sequence, so `term2` came back round.
        // Now that ✻ is given `claude`/`claude2` and + is given `term`/`term1`
        // they no longer collide, and nothing overwrote it.
        //
        // Released explicitly instead, which is what `session::release` is
        // for. Read before `apply_layout`, which has removed the tab by the
        // time the answer is sent.
        let closing_session: Option<String> = match &intent {
            Intent::CloseTab { pane, idx } => self
                .ws
                .panes
                .get(*pane as usize)
                .and_then(|p| p.tabs.get(*idx))
                .and_then(|t| match t {
                    Tab::Terminal { session, .. } => Some(session.clone()),
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
        if let Some(name) = closing_session.as_deref() {
            crate::session::release(&self.project, name);
        }
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                // A layout change can add a terminal tab (OpenTab, MoveTab
                // between panes); the reservation follows it. Cheap and
                // idempotent, so it runs on every change rather than being
                // enumerated per intent — an intent list is the kind of thing
                // that silently stops covering a case someone adds later.
                self.sync_reservations();
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
                                eprintln!("roost: closing proposal {id}: {e}");
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

    /// A file that is positively gone from under whatever has it open.
    ///
    /// This is the *proven* absence — `symlink_metadata` said `NotFound`, not
    /// "I could not look" — so acting on it is safe in the way CLAUDE.md's
    /// third-outcome rule requires. What is not safe is the state the watcher
    /// used to leave behind: an external `mv` deletes the old path, nothing
    /// touched the tab, and the tab went on addressing it in Edit. The editor
    /// is seeded from the `BufferText` this method's caller would have sent —
    /// no readable file, no push — so the next mount paints an *empty*
    /// textarea under the old filename, labelled "saved". `open_buffer_for`
    /// already refuses to create exactly that state on the open path ("always
    /// demote"); this applies the same rule when the file leaves on its own.
    ///
    /// Always returns `false`, because that is what "still there?" means to
    /// the watcher, which turns it into the tree refresh that drops the row.
    ///
    /// The tab itself is never closed. A `git checkout` or a rebase removes
    /// files it is about to put back, and a wrongly closed tab is not
    /// recoverable from the UI the way a wrongly demoted one is — a demoted
    /// tab re-renders itself the moment the file returns, because a Preview
    /// pane fetches its own fragment.
    fn file_vanished(&mut self, rel: &str) -> bool {
        // A dirty buffer is now the only copy of that text anywhere. Demoting
        // it would hide the user's unsaved work behind a "not found" page and
        // dropping it would delete the work outright, so it is marked stale
        // instead — the same flag a file changing underneath sets, and the one
        // the client uses to pause autosave, which matters more here than
        // there: every autosave from now on writes to a path that is gone.
        if self.ws.buffers.get(rel).map(|b| b.dirty()).unwrap_or(false) {
            if let Some(b) = self.ws.buffers.get_mut(rel) {
                b.stale = true;
            }
            let ev = Event::BufferStale { rel: rel.to_string() };
            self.broadcast(&ev);
            self.broadcast(&Event::FileChanged { rel: rel.to_string() });
            return false;
        }
        // Nothing was unsaved, so the buffer holds only a base hash for a file
        // that no longer exists. Keeping it would have every later reconnect
        // re-read a path it cannot read (`wsconn`'s replay reads one file per
        // buffer, every time) to learn the same thing again.
        let dropped = self.ws.buffers.remove(rel).is_some();
        let demoted = self.demote_to_preview(rel);
        if dropped || demoted {
            self.ws.version += 1;
            let snap = self.snapshot_event(&String::new());
            self.broadcast(&snap);
            self.persist();
        }
        // Every pane showing this rel, not just the one that owns the buffer:
        // a Preview pane paints from a fetched fragment and otherwise goes on
        // showing the deleted file's contents until something makes it look
        // again. Sent after the snapshot so a demoted tab has already been
        // told which mode it is in.
        self.broadcast(&Event::FileChanged { rel: rel.to_string() });
        false
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
    /// - genuinely absent (`symlink_metadata` says `NotFound`): hands off to
    ///   `file_vanished`, which settles what happens to the tab and the
    ///   buffer, and returns false. `classify` routes an open buffer's path to
    ///   `Buffer`, not `Tree`, so without this the caller's tree pane would
    ///   keep listing a file that no longer exists until some unrelated event
    ///   happened to arrive and trigger a refresh. Callers must treat `false`
    ///   as a tree change too.
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
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return self.file_vanished(rel)
                }
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
            // Diverged, not merely touched. A rename fires an event for the
            // destination without changing a byte, and flagging that would
            // tell the user their file had been rewritten underneath them by
            // the act of moving it — and pause autosave until they resolved a
            // conflict that does not exist. Equal hashes mean a save from this
            // buffer would not even reach the conflict check.
            if b.dirty() {
                if disk_hash != b.base_hash {
                    b.stale = true;
                    let ev = Event::BufferStale { rel: rel.to_string() };
                    self.broadcast(&ev);
                }
                // Equal hashes: nothing to say and nothing to do. Falling
                // through to the clean branch below would reset this buffer to
                // Clean and broadcast the disk text over the user's unsaved
                // edit, which is why this is a nested `if` and not an `&&`.
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

    /// A rename roost did not perform — `mv` from a terminal, `git mv`, a
    /// Claude's own edit tool. `watch.rs` only calls this when the kernel
    /// paired the two halves itself (inotify's rename cookie, surfaced by
    /// `notify` as one `Modify(Name(Both))` event carrying both paths), so
    /// this is told where the file went rather than inferring it. A move out
    /// of the project never pairs, and correctly stays a deletion.
    ///
    /// Returns whether anything was actually following that path. The guard is
    /// not an optimisation: every atomic save is a rename (`atomic_write`
    /// writes a temp file and renames over the target) and so is most of what
    /// git does to its index, so without it a build or a commit would bump the
    /// workspace version, persist the layout, and wake every browser on the
    /// project several times a second.
    pub fn follow_rename(&mut self, old: &str, new: &str) -> bool {
        let referenced = self.ws.buffers.keys().any(|k| k == old || has_prefix_boundary(k, old))
            || self.ws.panes.iter().flat_map(|p| p.tabs.iter()).any(|t| match t {
                Tab::File { rel, .. } | Tab::Diff { rel: Some(rel) } => {
                    rel == old || has_prefix_boundary(rel, old)
                }
                _ => false,
            });
        if !referenced {
            return false;
        }
        // Handles a directory rename too, by `/`-boundary prefix: renaming
        // `src` moves every open tab under it. That is the same method roost's
        // own rename intent uses, so both paths cannot drift apart.
        self.rekey_after_rename(old, new);
        self.ws.version += 1;
        let snap = self.snapshot_event(&String::new());
        self.broadcast(&snap);
        self.persist();
        // Only a dirty buffer. A clean one holds no text of its own and the
        // batch's own classification of the destination re-reads it from disk a
        // moment later; an unsaved edit has no other source, and app.js keys its
        // editor text by rel, so without this the editor re-mounts at the new
        // name with nothing to put in it — the empty-editor failure this line of
        // work started from, arriving through the rename door.
        //
        // Sent after the snapshot because that is the natural order, not because
        // it is required: measured by sending it first, and the client is fine
        // either way. It mounts the editor empty and fills it in whenever the
        // text lands (`mountEditor` documents exactly that), and the `texts`
        // pruning every State does cannot drop this one — the State that moves
        // the tab lists the buffer under its new rel, so the new rel is in the
        // set the prune keeps.
        let edited = self.ws.buffers.get(new).and_then(|b| b.edited_text()).map(|t| t.to_string());
        if let Some(text) = edited {
            self.broadcast(&Event::BufferText {
                rel: new.to_string(),
                text,
                origin: String::new(), // no author: every client applies it
            });
        }
        true
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
        // `owned_live_names`, which is `live_names` adjusted by the re-home
        // records and is fork-free for the same reason.
        let owned = crate::session::owned_live_names(&self.project);
        self.session_homes =
            owned.iter().filter_map(|(n, h)| Some((n.clone(), h.clone()?))).collect();
        self.ws.live_sessions = owned.into_iter().map(|(n, _)| n).collect();
        self.ws.live_sessions.dedup();
        // A shell that is running again is no longer lost. Without this the
        // note would survive the user starting a replacement in that very
        // tab, which is the one moment it stops being true.
        let live = &self.ws.live_sessions;
        self.ws.lost_sessions.retain(|s| !live.contains(s));
        // And with it the offer to resume: a tab with a shell in it again is
        // not a tab waiting to be restored. Kept a subset of `lost_sessions`
        // rather than filtered on `live` independently, so the two can never
        // drift into a placeholder that offers a resume it no longer shows a
        // loss for.
        let lost = self.ws.lost_sessions.clone();
        self.ws.resumable_sessions.retain(|s| lost.contains(s));

        // A resident with no tab of its own yet. Nothing that opens a
        // terminal tab (`OpenTab`, `StartTerminal`, a restored layout) ever
        // names a session this project never spawned, so this refresh — the
        // moment `session_homes` above just learned about it — is the only
        // place that can give a freshly re-homed terminal a tab on its
        // *owner*'s page.
        //
        // R16 — the invariant superseding R15 part 2: within an owner, a
        // terminal name is held by at most one live session, native or
        // resident, ever. R15 part 2 let a native and a resident tab of the
        // same name coexist, which looked right in isolation but broke
        // everything else that is keyed by name alone — `EndSession`
        // (closing the native tab would `apply_layout`'s name-only
        // `EndSession` match drop *both*, while the resident's shell at its
        // home kept running, orphaned), the client's own `terms` Map (one
        // entry per session name — a second tab of the same name reuses the
        // first's live socket, so both tabs would show the same shell), and
        // reservations/`lost_sessions`/`live_sessions` (also name-keyed).
        // Two tabs sharing a name was never representable anywhere outside
        // this one loop.
        //
        // `raw_live` (the raw sockets under *this* project, `live_names` —
        // not `self.ws.live_sessions`) is what "really live" has to mean
        // here: `live_sessions` is `owned_live_names`'s output, which
        // already carries a resident's own name as "live" for the client's
        // attach-vs-placeholder decision — using it here would make the
        // resident's own arrival look like evidence its name is natively
        // live, so a dead native tab would never be retargeted at all.
        let raw_live = crate::session::live_names(&self.project);
        for (name, home) in self.session_homes.clone() {
            let already_resident_tabbed = self.ws.panes.iter().flat_map(|p| p.tabs.iter()).any(|t| {
                matches!(t, Tab::Terminal { session: s, home: Some(h) } if s == &name && h == &home)
            });
            if already_resident_tabbed {
                continue;
            }
            let native_exists = self
                .ws
                .panes
                .iter()
                .flat_map(|p| p.tabs.iter())
                .any(|t| matches!(t, Tab::Terminal { session: s, home: None } if s == &name));
            if native_exists {
                if raw_live.contains(&name) {
                    // The invariant says this cannot happen — a resident and
                    // a live native session cannot share a name — but a
                    // race between a browser's `StartTerminal` and this
                    // refresh could, in principle, hand this project a live
                    // native `name` the instant before a resident of the
                    // same name is also recorded. Refusing to add a second
                    // tab is what the invariant requires; logging rather
                    // than panicking is what a socket/watcher thread
                    // (CLAUDE.md) requires.
                    eprintln!(
                        "roost: {} has a live native terminal {name} and a resident of the same name at {home} — keeping the native one, not tabbing the resident",
                        self.project
                    );
                    // Fix round 2, New Issue 1: the hidden resident must
                    // not stay routable through `session_homes` while its
                    // name is shadowed — `do_end_session`'s `kill_target`
                    // and the `OpenTab` home-fill-in above both read it,
                    // and leaving this entry in would let a click on the
                    // *visible* native tab's × SIGKILL the *hidden*
                    // resident at its home (its record truly names this
                    // owner, so `may_end` would pass), or let a bare
                    // `OpenTab` for `name` push a second tab beside the
                    // native one. The on-disk record is left alone — only
                    // this hub's in-memory view of `name` reverts to
                    // "native" — so the next refresh can pick the resident
                    // back up the moment the native session actually ends.
                    self.session_homes.remove(&name);
                    continue;
                }
                // Retarget in place — covers both a never-started
                // placeholder (`default_layout`'s own seeded `term`, the
                // primary approve flow: a Claude in the main checkout's own
                // `term` asks for a worktree) and a seen-but-dead native tab
                // alike, since neither is "really" this name any more than
                // the resident now claiming it. No longer lost, and no
                // longer offering a resume: the name just became a live
                // tab again, the same way a replacement shell starting in
                // that very spot already clears both below.
                for pane in self.ws.panes.iter_mut() {
                    for t in pane.tabs.iter_mut() {
                        if let Tab::Terminal { session: s, home: tab_home } = t {
                            if tab_home.is_none() && s == &name {
                                *tab_home = Some(home.clone());
                            }
                        }
                    }
                }
                self.ws.lost_sessions.retain(|s| s != &name);
                self.ws.resumable_sessions.retain(|s| s != &name);
                self.ws.version += 1;
                continue;
            }
            // No tab at all, native or resident: give it one, at `RIGHT` —
            // the pane `NewTerminal`/`focusSession` use for a terminal with
            // no home of its own, so a resident lands exactly where a plain
            // `+` click would have put it.
            let intent = Intent::OpenTab {
                pane: crate::proto::RIGHT,
                tab: Tab::Terminal { session: name, home: Some(home) },
            };
            if let Ok(true) = workspace::apply_layout(&mut self.ws, &intent) {
                self.ws.version += 1;
            }
        }

        // The mirror image, found on the *home*'s own refresh: a native tab
        // (`home: None`) whose session is still truly alive here
        // (`raw_live`, computed above) but absent from `self.ws.live_sessions`
        // just moved, rather than ended — that absence is exactly what
        // `owned_live_names`'s own `Elsewhere`-and-resolves filter means.
        // The owner's own refresh (above) is what gives it a tab there
        // instead, so this page no longer has any business showing one.
        let project = self.project.clone();
        let mut rehomed: Vec<(String, String)> = Vec::new();
        for pane in &self.ws.panes {
            for t in &pane.tabs {
                if let Tab::Terminal { session, home: None } = t {
                    if rehomed.iter().any(|(s, _)| s == session) {
                        continue;
                    }
                    if raw_live.contains(session) && !self.ws.live_sessions.contains(session) {
                        rehomed
                            .push((session.clone(), crate::home::effective_owner(&project, session)));
                    }
                }
            }
        }
        for (session, owner) in rehomed {
            let ended = workspace::apply_layout(
                &mut self.ws,
                &Intent::EndSession { session: session.clone() },
            );
            if let Ok(true) = ended {
                self.ws.version += 1;
            }
            self.broadcast(&Event::Rehomed { session, owner });
        }

        // Any enclosing work tree counts, not just this directory's own `.git`
        // — see `gitio::is_inside_work_tree`. A nested project has no `.git` of
        // its own, and offering `git init` there embeds a repository inside its
        // parent.
        self.ws.is_git = crate::gitio::is_inside_work_tree(&self.dir);
        // The `+` dialog's fork choices: native (never resident — see the
        // field's own doc comment) live sessions this project has a
        // recorded Claude session id for. `raw_live`, not
        // `self.ws.live_sessions`, for the same reason the rehoming check
        // above uses it: the latter already folds a resident's own name in
        // as "live", and a resident is forked from its home's refresh, not
        // this one.
        self.forkable = raw_live
            .iter()
            .filter(|name| crate::claudesess::recorded(&self.project, name).is_some())
            .cloned()
            .collect();
    }

    /// The client's websocket connect to `/ws/{project}/term/{name}` is what
    /// actually spawns the PTY (via `session::attach`); this intent only
    /// validates the name, enforces the per-project cap, and tells every
    /// mirrored client the tab is now live. Spawning here too would double-
    /// spawn.
    fn do_start_terminal(&mut self, from: &ConnId, session: String, resume: bool) {
        if !crate::session::valid_name(&session) {
            let ev = Event::Error { msg: format!("invalid session name: {session}") };
            return self.send_to(from, &ev);
        }
        // A lost tab's "press Enter" placeholder retries its old name, which
        // is the other way (besides a fresh `+`/`✻` click, already guarded
        // by `do_new_terminal`'s `also_taken`) a browser can ask for a name
        // already in play on this page. `self.session_homes` is exactly
        // that: a resident's name, taken here though this project's own
        // `sock/` has no such socket. Spawning anyway would land the
        // browser in a brand-new, unrelated shell under the name a Claude
        // at the resident's home is still running in.
        if let Some(home) = self.session_homes.get(&session) {
            let ev = Event::Error {
                msg: format!("terminal {session} is in use by a terminal moved here from {home}"),
            };
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
        // After every refusal above, so nothing is parked for a start that
        // was not allowed to happen — otherwise a refused resume would sit
        // there and be typed into whatever the user started next in this tab.
        //
        // `reserve`, not `reserve_if_absent`: this intent *is* the decision
        // about what the next shell runs, which is the case that function's
        // doc reserves it for.
        //
        // A `resume` with no record still starts a plain terminal rather than
        // failing. The button is only offered where a record exists, so this
        // is a stale client or a record removed in between — and "I have
        // nothing recorded" is not "no Claude ran here" (`claudesess`), so the
        // honest answer is a shell, not an error about a session.
        if resume {
            if let Some(rec) = crate::claudesess::recorded(&self.project, &session) {
                crate::session::reserve(
                    &self.project,
                    &session,
                    Some(crate::session::LaunchRequest {
                        launch: crate::proto::Launch::Claude,
                        session: Some(crate::launch::ClaudeSession::Resume(rec.session_id)),
                    }),
                );
            }
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
        // A resident's dtach master lives at its *home*, never here
        // (`home.rs`'s module doc): ending it must kill the real session
        // there, or this would report success while leaving the shell —
        // and any Claude in it — running untouched. `self.session_homes` is
        // exactly `do_new_terminal`'s `also_taken` source: a name in it is
        // a resident's, not a native tab's.
        let kill_target = self.session_homes.get(&session).cloned().unwrap_or_else(|| project.clone());
        match self.self_ref.upgrade() {
            Some(hub_arc) => {
                let thread_project = project.clone();
                let thread_kill_target = kill_target.clone();
                let thread_session = session.clone();
                let thread_from = from.clone();
                let spawned =
                    std::thread::Builder::new().name("end-session".into()).spawn(move || {
                        // R16, Important 1: re-checked right here, as close
                        // to the kill as the lock-free gap between reading
                        // `kill_target` and this thread actually running
                        // allows — see `may_end`'s own doc comment for why.
                        if !may_end(&thread_kill_target, &thread_session, &thread_project) {
                            eprintln!(
                                "roost: End Session refused {thread_kill_target}/{thread_session}: no longer owned by {thread_project}"
                            );
                            let mut h = Hub::lock(&hub_arc);
                            let ev = Event::Error {
                                msg: format!(
                                    "{thread_session} moved since this was asked; nothing was ended"
                                ),
                            };
                            h.send_to(&thread_from, &ev);
                            h.refresh_live_sessions();
                            let snap = h.snapshot_event(&String::new());
                            h.broadcast(&snap);
                            return;
                        }
                        // No panic may escape a socket thread; one here must not
                        // strand the hub lock and leave every later intent stuck.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::session::end_session(&thread_kill_target, &thread_session)
                        }))
                        .unwrap_or_else(|_| {
                            eprintln!(
                                "roost: end_session panicked ending {thread_kill_target}/{thread_session}"
                            );
                            false
                        });
                        {
                            let mut h = Hub::lock(&hub_arc);
                            h.refresh_live_sessions();
                            let snap = h.snapshot_event(&String::new());
                            h.broadcast(&snap);
                        }
                        // The kill just happened at `thread_kill_target`, not
                        // `thread_project`, for a resident — so that other
                        // project's own browsers, if any are open, must hear
                        // about it too. `open_hub`, not `for_project`: a
                        // home with nobody connected has no hub to refresh
                        // and nothing waiting to be told.
                        if thread_kill_target != thread_project {
                            if let Some(home_hub) = open_hub(&thread_kill_target) {
                                let mut hh = Hub::lock(&home_hub);
                                hh.refresh_live_sessions();
                                let snap = hh.snapshot_event(&String::new());
                                hh.broadcast(&snap);
                            }
                        }
                        // Ending a project's last session empties its row in
                        // every other project's ◆ panel; same placement
                        // rules as `do_close_project`'s nudge.
                        broadcast_all(&Event::ProjectsChanged { project: thread_project });
                    });
                if let Err(e) = spawned {
                    eprintln!("roost: could not spawn end-session thread for {project}: {e}");
                    if may_end(&kill_target, &session, &project) {
                        crate::session::end_session(&kill_target, &session);
                    } else {
                        eprintln!(
                            "roost: End Session refused {kill_target}/{session}: no longer owned by {project}"
                        );
                        self.send_to(from, &Event::Error {
                            msg: format!("{session} moved since this was asked; nothing was ended"),
                        });
                    }
                    self.refresh_live_sessions();
                }
            }
            None => {
                if may_end(&kill_target, &session, &project) {
                    crate::session::end_session(&kill_target, &session);
                } else {
                    eprintln!(
                        "roost: End Session refused {kill_target}/{session}: no longer owned by {project}"
                    );
                    self.send_to(from, &Event::Error {
                        msg: format!("{session} moved since this was asked; nothing was ended"),
                    });
                }
                self.refresh_live_sessions();
            }
        }
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
        self.persist();
    }

    /// `git worktree add` off the hub lock, the `do_end_session` shape: the
    /// work runs on a thread when this hub has a self-handle, synchronously
    /// (unit tests, bare `Hub::new`) when it does not, and in both cases
    /// the hub is re-locked only to deliver the answer.
    ///
    /// `Fork`'s session id is resolved **before** any thread is spawned, not
    /// inside `work`: the refusal (no recorded Claude session) is a fact
    /// about this project right now, cheap to check (a small local file,
    /// `claudesess`'s own read), and must reach the clicker without waiting
    /// on a worktree that will not be created. Resolving it late would also
    /// let the terminal's record change between the click and the git work
    /// with no way to say which version of it was honoured.
    fn do_new_worktree(
        &mut self,
        from: &ConnId,
        name: Option<String>,
        launch: Option<crate::proto::WorktreeLaunch>,
    ) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        // What the new worktree's first terminal should start, and the
        // Claude session id to fork — resolved now, never typed, never
        // carried as anything but an id (CLAUDE.md: the server owns what
        // lands on a command line).
        let (reply_launch, fork_id) = match &launch {
            Some(crate::proto::WorktreeLaunch::Claude) => (Some(crate::proto::Launch::Claude), None),
            Some(crate::proto::WorktreeLaunch::Fork { session }) => {
                match crate::claudesess::recorded(&self.project, session) {
                    Some(rec) => (Some(crate::proto::Launch::Claude), Some(rec.session_id)),
                    None => {
                        let msg = format!("no recorded Claude session for {session}");
                        return self.send_to(from, &Event::Error { msg });
                    }
                }
            }
            Some(crate::proto::WorktreeLaunch::Shell) | None => (None, None),
        };
        let project = self.project.clone();
        let dir = self.dir.clone();
        let from = from.clone();
        // A separate clone for `finish`: the spawn-failure arm below still
        // needs `from` after `finish` (a `move` closure) has taken its own
        // copy, since only one of the two ever runs but the borrow checker
        // cannot see that.
        let finish_from = from.clone();
        let work = move || -> Result<String, String> {
            let key_of = |n: &str| crate::projects::storage_key(&format!("{project}/.claude/worktrees/{n}"));
            // Checked before any name is even chosen: a non-repository (or
            // an unanswerable `git branch --list`) would otherwise make
            // every generated candidate look "taken" and exhaust
            // `wtname::generate`'s retries, reporting "could not find a
            // free worktree name" instead of the real reason. `create_named`
            // makes this same check again — redundant on the happy path,
            // but it is what actually confines the generator's own git
            // calls to a real repository.
            if !crate::gitio::is_inside_work_tree(&dir) {
                return Err("not a git repository".into());
            }
            let resolved = match name {
                Some(n) => n,
                None => {
                    // `worktree::name_taken` is the same two checks
                    // `create_named` itself makes, asked ahead of it so a
                    // name this loop would reject is never even offered to
                    // it — see that function's own doc comment for why an
                    // uncertain answer counts as taken too.
                    let taken = |cand: &str| crate::worktree::name_taken(&dir, &crate::worktree::real_git, cand);
                    crate::wtname::generate(&taken)
                        .ok_or_else(|| "could not find a free worktree name".to_string())?
                }
            };
            let c = crate::worktree::create_named(&dir, &crate::wsstate::state_dir(), &resolved, &key_of, &crate::worktree::real_git)?;
            Ok(format!("{project}/.claude/worktrees/{}", c.name))
        };
        // Delivers the reply under the hub lock and hands back the url on
        // success, but does *not* call `broadcast_all` itself: that locks
        // every registered hub, including this one, and this closure runs
        // while this hub's own lock is held (see both call sites below) —
        // calling `broadcast_all` from in here deadlocked every time,
        // confirmed with a `for_project`-registered hub timing out on
        // `recv_timeout` before this was split apart. The caller does the
        // broadcast only after that lock is released, the same ordering
        // `do_end_session` uses for its own `broadcast_all` call.
        let finish = move |h: &mut Hub, r: Result<String, String>| -> Option<String> {
            match r {
                Ok(url) => {
                    let ev = Event::WorktreeReady { url: url.clone(), launch: reply_launch, fork: fork_id };
                    h.send_to(&finish_from, &ev);
                    Some(url)
                }
                Err(msg) => {
                    let ev = Event::Error { msg };
                    h.send_to(&finish_from, &ev);
                    None
                }
            }
        };
        match self.self_ref.upgrade() {
            Some(arc) => {
                let spawned = std::thread::Builder::new().name("new-worktree".into()).spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        .unwrap_or_else(|_| Err("worktree creation panicked".into()));
                    let url = {
                        let mut h = Hub::lock(&arc);
                        finish(&mut h, r)
                    };
                    if let Some(url) = url {
                        broadcast_all(&Event::ProjectsChanged { project: url });
                    }
                });
                if spawned.is_err() {
                    let ev = Event::Error { msg: "could not start worktree creation".into() };
                    self.send_to(&from, &ev);
                }
            }
            None => {
                let r = work();
                if let Some(url) = finish(self, r) {
                    broadcast_all(&Event::ProjectsChanged { project: url });
                }
            }
        }
    }

    /// A browser's answer to a parked worktree ask. Runs under the hub lock,
    /// so it only settles the ask: removes it (taking ownership of its reply
    /// sender), closes every browser's dialog, and either refuses or hands
    /// the git work to a thread — the `do_new_worktree` shape. An `id` that
    /// is no longer pending was answered by someone else or timed out; the
    /// first answer won, and this one is dropped without a word.
    fn do_worktree_answer(&mut self, from: &ConnId, id: String, approve: bool) {
        let Some(i) = self.asks.iter().position(|a| a.id == id) else { return };
        let ask = self.asks.remove(i);
        self.broadcast(&Event::WorktreeAskClosed { id });
        if !approve {
            let _ = ask.reply.send(ask_refuse("refused in roost"));
            return;
        }
        if self.closing {
            let _ = ask.reply.send(ask_refuse("project is closing"));
            return;
        }
        let (project, dir, clicker) = (self.project.clone(), self.dir.clone(), from.clone());
        // The closure owns `ask`, so a failed spawn drops its sender unsent;
        // this clone is how that one failure still answers.
        let fallback = ask.reply.clone();
        let spawned = std::thread::Builder::new()
            .name("worktree-ask".into())
            .spawn(move || approve_ask(project, dir, clicker, ask));
        if spawned.is_err() {
            let msg = "could not start worktree creation";
            let _ = fallback.send(ask_refuse(msg));
            self.send_to(from, &Event::Error { msg: msg.into() });
        }
    }

    /// `git worktree remove` (no `--force`), then `git branch -d` (no
    /// `-D`) off the hub lock, the `do_new_worktree` shape. Four independent
    /// gates are re-derived here — Claude evidence, a live terminal, dirty,
    /// ahead — rather than trusted from whatever state the row that offered
    /// the button was rendered from; see CLAUDE.md's "Absence of evidence is
    /// not evidence of absence": this is the destructive path that section
    /// exists for. git's own two refusals (a dirty tree, an unmerged
    /// branch) are gates this function does not duplicate — a `state`-based
    /// check makes sure this project never even tries.
    ///
    /// The worktree is found from this repo's own `git worktree list`, not
    /// `projects::roots()`/`resolve_project`: every worktree roost creates
    /// lives under `.claude/worktrees/{name}` inside this project's own
    /// directory, so asking git which linked worktrees it has and matching
    /// their storage key is exact, and the main checkout — never a non-main
    /// entry in that list — can never match.
    fn do_remove_worktree(&mut self, from: &ConnId, key: String) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        let repo = self.dir.clone();
        let project = self.project.clone();
        let from = from.clone();
        // A separate clone for `finish`: the spawn-failure arm below still
        // needs `from` after `finish` (a `move` closure) has taken its own
        // copy, since only one of the two ever runs but the borrow checker
        // cannot see that (same shape as `do_new_worktree`'s `finish_from`).
        let finish_from = from.clone();
        let work = move || -> Result<Option<String>, String> {
            let decoded = crate::registry::decode_key(&key);
            let repo_canon = repo
                .canonicalize()
                .map_err(|e| format!("cannot resolve project directory: {e}"))?;
            let ws = crate::worktree::list(&repo);
            let mut target: Option<(std::path::PathBuf, String, String)> = None;
            for w in ws.iter().filter(|w| !w.is_main) {
                let Ok(canon) = w.path.canonicalize() else { continue };
                let Ok(rel) = canon.strip_prefix(&repo_canon) else { continue };
                let url = format!("{project}/{}", rel.display());
                if crate::projects::storage_key(&url) == key {
                    target = Some((canon, w.branch.clone(), url));
                    break;
                }
            }
            let (dir, name, url) =
                target.ok_or_else(|| format!("{decoded}: not a worktree of this project"))?;
            // Each check names itself. Order: the most specific first, so a
            // launched Claude is reported as such and not as "a terminal".
            match crate::claudes::claude_evidence(&url) {
                crate::claudes::ClaudeEvidence::Present(_) => {
                    return Err(format!("{name}: a Claude is running there"))
                }
                crate::claudes::ClaudeEvidence::Unknown => {
                    return Err(format!(
                        "{name}: cannot tell whether a Claude is running there (IDE integration is off)"
                    ))
                }
                crate::claudes::ClaudeEvidence::Absent => {}
            }
            // `owned_live_names`, not `live_names`: a resident's dtach
            // socket is still under its *home*, so `live_names(&url)` alone
            // would miss a running Claude re-homed into this worktree and
            // let `git worktree remove` delete the directory out from
            // under it.
            if !crate::session::owned_live_names(&url).is_empty() {
                return Err(format!("{name} has a live terminal"));
            }
            let state_dir = crate::wsstate::state_dir();
            let base = crate::worktree::read_base(&state_dir, &key)
                .or_else(|| ws.iter().find(|w| w.is_main).map(|m| m.branch.clone()))
                .ok_or_else(|| format!("{name}: no base to measure against"))?;
            let st = crate::worktree::state(&dir, &base, &crate::worktree::real_git);
            match st.dirty {
                Some(false) => {}
                Some(true) => return Err(format!("{name} has uncommitted changes")),
                None => return Err(format!("{name}: git did not answer (status)")),
            }
            match st.ahead {
                Some(0) => {}
                Some(n) => {
                    return Err(format!(
                        "{name} is {n} commit{} ahead of {base}",
                        if n == 1 { "" } else { "s" }
                    ))
                }
                None => return Err(format!("{name}: git did not answer (rev-list)")),
            }
            let note = crate::worktree::remove(&repo, &dir, &name, &crate::worktree::real_git)?;
            // Only after both git steps succeed: roost's own records about a
            // thing that, by this point, no longer exists.
            let _ = std::fs::remove_file(crate::worktree::base_file(&state_dir, &key));
            let _ = std::fs::remove_file(state_dir.join(format!("{key}.json")));
            Ok(note)
        };
        // `finish` runs under the hub lock and only talks to the clicker.
        // `broadcast_all` locks EVERY registered hub — this one included —
        // so it must run after the guard is dropped (Task 7 found the
        // self-deadlock; `do_new_worktree`/`do_end_session` have the same
        // ordering).
        let finish = move |h: &mut Hub, r: Result<Option<String>, String>| -> bool {
            match r {
                Ok(note) => {
                    let ev = match note {
                        Some(n) => Event::Error { msg: n },
                        None => Event::ProjectsChanged { project: h.project.clone() },
                    };
                    h.send_to(&finish_from, &ev);
                    true
                }
                Err(msg) => {
                    let ev = Event::Error { msg };
                    h.send_to(&finish_from, &ev);
                    false
                }
            }
        };
        let project_for_broadcast = self.project.clone();
        match self.self_ref.upgrade() {
            Some(arc) => {
                let spawned = std::thread::Builder::new().name("remove-worktree".into()).spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        .unwrap_or_else(|_| Err("worktree removal panicked".into()));
                    let changed = {
                        let mut h = Hub::lock(&arc);
                        finish(&mut h, r)
                    }; // guard dropped here, before any other hub is locked
                    if changed {
                        broadcast_all(&Event::ProjectsChanged { project: project_for_broadcast });
                    }
                });
                if spawned.is_err() {
                    let ev = Event::Error { msg: "could not start worktree removal".into() };
                    self.send_to(&from, &ev);
                }
            }
            None => {
                let r = work();
                let changed = finish(self, r);
                if changed {
                    broadcast_all(&Event::ProjectsChanged { project: project_for_broadcast });
                }
            }
        }
    }

    /// Opens a terminal on a server-allocated name. See
    /// `session::next_free_name` for why the client must not choose it.
    fn do_new_terminal(
        &mut self,
        from: &ConnId,
        pane: crate::proto::PaneId,
        launch: Option<crate::proto::Launch>,
        force: bool,
        resume: Option<String>,
        fork: Option<String>,
    ) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        if launch == Some(crate::proto::Launch::Claude) && !force && crate::config::worktree_prompt() {
            if let crate::claudes::ClaudeEvidence::Present(terminals) =
                crate::claudes::claude_evidence(&self.project)
            {
                // The clicker only: nothing changed for anyone else, and a
                // prompt is a question, not a state.
                let ev = Event::ClaudeHere { pane, terminals };
                return self.send_to(from, &ev);
            }
        }
        // Names already on a tab but not yet connected are in no registry —
        // `next_free_name`'s `also_taken` is what stops two quick clicks from
        // both being handed `term`.
        //
        // Residents too: re-homed here, they show on this page under their
        // name, though this project's own `sock/` has no such socket.
        let on_tabs: Vec<String> = self
            .ws
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match t {
                Tab::Terminal { session, .. } => Some(session.clone()),
                _ => None,
            })
            .chain(self.session_homes.keys().cloned())
            .collect();
        // The prefix says what the terminal is for. A ✻ click gets `claude`,
        // `claude2`, … so the strip says which tab has an agent in it; a plain
        // + keeps `term`. #52's `prloop` gets its own for the same reason —
        // that one is pushing commits, and "which tab is that" should not need
        // a guess.
        let prefix = match launch.as_ref().copied() {
            Some(crate::proto::Launch::Claude) => "claude",
            Some(crate::proto::Launch::PrLoop) => "prloop",
            None => "term",
        };
        let Some(name) = crate::session::next_free_name_with(&self.project, &on_tabs, prefix) else {
            let ev = Event::Error { msg: "too many terminal sessions".into() };
            return self.send_to(from, &ev);
        };
        // Unconditionally, `None` included: this name may have been handed
        // out before to a ✻ click whose tab was closed before any browser
        // attached, and a plain + must not inherit that click. The shell is
        // spawned by whichever browser attaches first, so the request waits
        // in `session` until then.
        crate::session::reserve(
            &self.project,
            &name,
            launch.map(|l| crate::session::LaunchRequest {
                launch: l,
                // A chosen row wins over a fresh mint, but only after the
                // server has confirmed this project really has that
                // conversation — the menu is a hint. A `resume` that does not
                // survive that check falls through to a fresh launch rather
                // than failing: the user asked for a Claude, and the worst
                // case is they get a new one instead of an old one.
                // `fork` takes priority: it is the worktree page's own first
                // terminal asking to continue a conversation this project
                // just resolved for it (do_new_worktree), never a value from
                // the ✻ history menu — `resume` is that one. Re-validated
                // here against the class `claudesess` stores under, the same
                // boundary `launch::keystrokes` crosses again before typing
                // anything (CLAUDE.md: the server owns what lands on a
                // command line, checked at every hop that carries an id).
                session: fork
                    .filter(|id| {
                        l == crate::proto::Launch::Claude && crate::claudesess::valid_session_id(id)
                    })
                    .map(crate::launch::ClaudeSession::Fork)
                    .or_else(|| {
                        resume
                            .filter(|id| {
                                l == crate::proto::Launch::Claude && crate::claudehist::has(&self.dir, id)
                            })
                            .map(crate::launch::ClaudeSession::Resume)
                    })
                    .or_else(|| {
                        crate::launch::new_session_id().map(crate::launch::ClaudeSession::Fresh)
                    }),
            }),
        );
        let intent = Intent::OpenTab { pane, tab: Tab::Terminal { session: name.clone(), home: None } };
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                // A layout change can add a terminal tab (OpenTab, MoveTab
                // between panes); the reservation follows it. Cheap and
                // idempotent, so it runs on every change rather than being
                // enumerated per intent — an intent list is the kind of thing
                // that silently stops covering a case someone adds later.
                self.sync_reservations();
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
        let line = crate::projects::trailing_line(&text);
        let intent = Intent::OpenTab {
            pane: crate::proto::MIDDLE,
            tab: Tab::File { rel: rel.clone(), mode: Mode::Preview },
        };
        self.handle(from, intent);
        // The line the link named. Until this existed the client stripped it
        // and flashed "line 42 — opening file", because, as app.js put it,
        // "the viewer has no line addressing to spend it on". It does now.
        if let Some(line) = line {
            let ev = Event::RevealLine { rel, line };
            self.broadcast(&ev);
        }
    }

    /// Open `rel` and tell every browser to scroll it to `line`.
    ///
    /// The confinement check is here for the reason `do_open_path` gives:
    /// `apply_layout` validates nothing, and this `rel` came off a wire. That
    /// a search produced it is no guarantee — a client can send this intent
    /// with anything in it.
    ///
    /// Refusal wording mirrors `resolve_terminal_path`'s own split (it keeps
    /// "confidently outside" distinct from "canonicalize failed for some
    /// other reason", so a file merely deleted since a search ran is never
    /// reported as having escaped the project) — but, unlike that function,
    /// never interpolates `rel` into the escape-case message. `rel` is
    /// attacker-controlled at this point, and echoing it back into the one
    /// event this client receives would hand it right back out. The
    /// non-escape case has nothing to leak: `rel` already resolved to
    /// somewhere inside this project, the same trust level `do_open_path`
    /// already gives a terminal link's path in its own refusal.
    fn do_open_at_line(&mut self, from: &ConnId, pane: crate::proto::PaneId, rel: String, line: u32) {
        if let Err(msg) = crate::projects::safe_resolve(&self.dir, &rel) {
            let text = if msg.starts_with("path outside project") {
                "path outside project".to_string()
            } else {
                format!("couldn't open {rel}")
            };
            let ev = Event::Error { msg: text };
            return self.send_to(from, &ev);
        }
        // Edit, not Preview: a content hit was matched against the file's
        // source, and a rendered markdown preview has no line 412 to land on.
        // `coerce_tab` still demotes anything that cannot be edited as text.
        let want = Tab::File { rel: rel.clone(), mode: Mode::Edit };
        // Checked before the `OpenTab` below reactivates it: `apply_layout`'s
        // `OpenTab` arm only applies `mode` when it creates a fresh tab
        // (see `find_tab`'s early return in that arm) — reactivating an
        // already-open tab leaves whatever mode it already had (Preview,
        // from a tree click or a terminal link) untouched. A content hit
        // needs Edit regardless, so that case gets an explicit `SetMode`
        // below, the same path the ✎ toggle uses.
        let already_open = self.ws.find_tab(&want).is_some();
        let intent = Intent::OpenTab { pane, tab: want };
        self.handle(from, intent);
        // `pane` is client-controlled and never bounds-checked (`do_open_path`
        // sidesteps this by hardcoding `MIDDLE`); an out-of-range value makes
        // `apply_layout` fail with "no pane N", which `handle` has already
        // reported to `from` above. Confirming the tab actually exists before
        // broadcasting `RevealLine` — rather than assuming the `OpenTab` above
        // succeeded — means a failed open never tells every other browser to
        // scroll a tab that was never created.
        if self.ws.find_tab(&Tab::File { rel: rel.clone(), mode: Mode::Edit }).is_none() {
            return;
        }
        if already_open {
            self.handle(from, Intent::SetMode { rel: rel.clone(), mode: Mode::Edit });
        }
        let ev = Event::RevealLine { rel, line };
        self.broadcast(&ev);
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
        // The named terminal's Claude is registered wherever its socket
        // actually is — its home, for a resident, never this project
        // (`self.project` may itself be the *owner* here, which has no
        // conns() entry at all for a session that lives elsewhere). `abs`
        // is already a real absolute path, not project-relative, so
        // sending it unchanged to the home's connection needs no
        // re-relativising the way `ide.rs`'s proposal delivery does.
        //
        // A resident goes through `mention_to_exact`, never plain
        // `mention_to`: the home's `conns()` entry can also hold its own
        // native terminals or a different resident, and `mention_to`'s
        // `Sess::Unknown` match / lone-connection fallback — right for a
        // project asking about its *own* ambiguity — would otherwise let
        // an Alt+K aimed at one resident land on an unrelated Claude at the
        // same home. A session that is *not* a resident takes the exact
        // same path as before re-homing existed (self.project, lenient).
        let result = match session.as_deref().and_then(|s| self.session_homes.get(s).map(|h| (h.clone(), s))) {
            Some((home, s)) => crate::ide::mention_to_exact(&home, s, &abs, lines),
            None => crate::ide::mention_to(&self.project, session.as_deref(), &abs, lines),
        };
        if let Err(e) = result {
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
        // Unaimed, unlike `do_mention_path`: every connected Claude gets
        // this, so every one of this project's *residents* must also be
        // reached at their own home — their connection has no entry under
        // `self.project` at all. Grouped by home (several residents can
        // share one) and delivered through the session-filtered
        // `selection_changed_for_sessions`, never plain `selection_changed`
        // (which is `notify_all`): a home's `conns()` entry can also hold
        // its own native terminals' Claudes, or a resident re-homed into a
        // *different* worktree, and this selection is about neither.
        let mut by_home: std::collections::HashMap<&String, Vec<String>> = Default::default();
        for (name, home) in &self.session_homes {
            by_home.entry(home).or_default().push(name.clone());
        }
        for (home, names) in by_home {
            let _ = crate::ide::selection_changed_for_sessions(home, &names, &abs, &text, start, end);
        }
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
            eprintln!("roost: no middle pane to open a proposal in");
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
                        // One sweep. The second one (and `CLOSE_SETTLE`)
                        // existed to catch a shell that was still forking
                        // when the first read the map and the socket
                        // directory; `session::begin_close` now marks the
                        // project closing in the same acquisition that drains
                        // it, so there is nothing left for a second pass to
                        // find. A timing hack kept beside an invariant only
                        // invites the next reader to trust the wrong one.
                        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::session::kill_project(&thread_project)
                        }))
                        .unwrap_or_else(|_| {
                            eprintln!(
                                "roost: kill_project panicked while closing {thread_project}; reporting 0 ended"
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
                    eprintln!("roost: could not spawn close-project thread for {project}: {e}");
                    let ev = Event::Error { msg: "could not close the project; try again".into() };
                    self.send_to(from, &ev);
                    // Nothing was ended, so nothing may be cleared: returning
                    // here is what keeps the tab drop below on the committed
                    // path only. Dropping the tabs of shells that are still
                    // running would hide them from the one browser that can
                    // still reach them.
                    return;
                }
                // After the spawn, not before: the thread blocks on the hub
                // lock this call still holds, so it cannot observe the
                // half-applied layout, and a failed spawn has already
                // returned above. It broadcasts the cleared layout itself,
                // in the `State` that follows its `ProjectClosed`.
                self.drop_terminal_tabs();
            }
            None => {
                let ended = crate::session::kill_project(&project);
                crate::ide::stop(&project);
                // Before the version bump, so the one bump covers the kill
                // and the layout change together — two bumps for a single
                // intent would make a client's `version` check see a state it
                // never received.
                self.drop_terminal_tabs();
                self.ws.version += 1;
                self.broadcast(&Event::ProjectClosed { ended });
                self.refresh_live_sessions();
                let snap = self.snapshot_event(&String::new());
                self.broadcast(&snap);
            }
        }
    }

    /// Removes every terminal tab and persists the result — the layout half
    /// of a close, which `session::kill_project` knows nothing about.
    ///
    /// Called only once a close is committed. Persisting is the point, not an
    /// afterthought: the sessions really were ended before this existed, but
    /// the tabs pointing at them stayed in the saved layout, so reopening the
    /// project restored a tab per dead session on every reload — reported as
    /// "I close the project but still see terminals under it".
    fn drop_terminal_tabs(&mut self) {
        if let Ok(true) = workspace::apply_layout(&mut self.ws, &Intent::CloseProject) {
            self.ws.version += 1;
            self.persist();
        }
    }
}

/// The hub for a project that is already open, without creating one.
///
/// `REGISTRY.get()`, not `get_or_init`: everything below is a question asked
/// from an `ide` connection thread, and building a hub as a side effect of a
/// question would start a filesystem watcher for a project nobody opened —
/// the same reasoning `Hub::show_hidden` gives.
///
/// The registry guard is dropped before the caller ever locks the hub, so the
/// registry-then-hub order `for_project` established is preserved.
fn open_hub(project: &str) -> Option<Arc<Mutex<Hub>>> {
    let reg = REGISTRY.get()?;
    let map = reg.lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).cloned()
}

fn ask_refuse(msg: impl Into<String>) -> crate::hooksock::AskReply {
    crate::hooksock::AskReply::Refuse { refuse: msg.into() }
}

/// Claude's worktree hook asking the browsers of its project (spec §3).
/// Blocks the calling (socket) thread for up to [`ASK_TIMEOUT`], never a
/// hub lock: `hooksock.rs`'s accept loop spawns one thread per connection
/// precisely so this can block without stalling any other hook request.
pub fn ask_worktree_blocking(req: crate::hooksock::AskRequest) -> crate::hooksock::AskReply {
    ask_worktree_blocking_with(req, ASK_TIMEOUT)
}

/// [`ask_worktree_blocking`] with the wait injected, so a test can watch a
/// timeout in milliseconds. A panic anywhere in here becomes a refusal: the
/// hook must always get an answer, and the default answer is no.
pub fn ask_worktree_blocking_with(
    req: crate::hooksock::AskRequest,
    timeout: std::time::Duration,
) -> crate::hooksock::AskReply {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ask_inner(req, timeout)))
        .unwrap_or_else(|_| ask_refuse("roost failed while asking; nothing was created"))
}

fn ask_inner(req: crate::hooksock::AskRequest, timeout: std::time::Duration) -> crate::hooksock::AskReply {
    use std::sync::mpsc::RecvTimeoutError;
    // Both land in a path and a git argument later, so they are checked
    // before anything else is looked at, let alone shown to a person.
    if !crate::session::valid_name(&req.session) {
        return ask_refuse(format!("invalid terminal name {:?}", req.session));
    }
    if let Err(e) = crate::wtname::check(&req.name) {
        return ask_refuse(e);
    }
    // Off every lock: a dot-segment URL makes this ask git.
    if crate::projects::resolve_project(&crate::projects::roots(), &req.project).is_none() {
        return ask_refuse(format!("{} is not a project roost serves", req.project));
    }
    // Looked up, never created: a project nobody has open has no browser to
    // ask, and building a hub would start a watcher for nobody.
    let Some(arc) = open_hub(&req.project) else { return ask_refuse(NO_TAB) };
    let id = NEXT_ASK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string();
    let (tx, rx) = channel();
    {
        let mut h = Hub::lock(&arc);
        if h.subs.is_empty() {
            return ask_refuse(NO_TAB);
        }
        if h.asks.iter().any(|a| a.session == req.session) {
            return ask_refuse("an earlier request from this terminal is still waiting");
        }
        if h.asks.len() >= MAX_PENDING_ASKS {
            return ask_refuse(format!("too many worktree requests waiting in {}", req.project));
        }
        h.asks.push(PendingAsk { id: id.clone(), session: req.session.clone(), name: req.name.clone(), reply: tx });
        h.broadcast(&Event::WorktreeAsk { id: id.clone(), session: req.session, name: req.name });
        // `broadcast` is what prunes closed sockets, so a subscriber list
        // that was all dead connections only shows itself now. Nobody saw
        // the dialog; waiting 60 s for nobody is worse than saying so.
        if h.subs.is_empty() {
            h.asks.retain(|a| a.id != id);
            return ask_refuse(NO_TAB);
        }
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => {
            let expired = {
                let mut h = Hub::lock(&arc);
                match h.asks.iter().position(|a| a.id == id) {
                    Some(i) => {
                        // Taking it out of `asks` is taking the right to
                        // answer it; the sender is dropped unsent and this
                        // thread answers instead.
                        drop(h.asks.remove(i));
                        h.broadcast(&Event::WorktreeAskClosed { id });
                        true
                    }
                    None => false,
                }
            };
            if expired {
                ask_refuse("nobody approved it in roost within 60 s")
            } else {
                // An answer removed it between the timeout and the lock, so
                // that answer owns the reply and is already on its way —
                // possibly a `Path`, with a record written. Refusing here
                // instead would leave the terminal re-homed and Claude
                // where it was. Unbounded by design: the approving thread
                // always sends (its panics become a refusal), and the hook
                // client bounds its own wait.
                rx.recv().unwrap_or_else(|_| ask_refuse("roost lost the request; check the worktree switcher"))
            }
        }
        Err(RecvTimeoutError::Disconnected) => ask_refuse("roost lost the request; check the worktree switcher"),
    }
}

/// The approve path, off every lock (spec §3): create → R16 gate → record →
/// (Task 10: hooks) → refresh both hubs → reply `Path` → `WorktreeReady` to
/// the clicker → `ProjectsChanged`. Each step returns before the next on
/// failure, and the record is written only once a worktree exists and the
/// gate has passed.
fn approve_ask(project: String, dir: std::path::PathBuf, clicker: ConnId, ask: PendingAsk) {
    let PendingAsk { session, name, reply, .. } = ask;
    // `created` is the owner URL once git has made the worktree, so the
    // switcher is told about it even when a later step refuses.
    let mut created: Option<String> = None;
    let work = std::panic::AssertUnwindSafe(|| -> Result<String, String> {
        let key_of = |n: &str| crate::projects::storage_key(&format!("{project}/.claude/worktrees/{n}"));
        let c = crate::worktree::create_named(
            &dir,
            &crate::wsstate::state_dir(),
            &name,
            &key_of,
            &crate::worktree::real_git,
        )?;
        let owner = format!("{project}/.claude/worktrees/{}", c.name);
        created = Some(owner.clone());
        // From here on a failure leaves the worktree on disk. That is
        // deliberate: creating destroyed nothing, and removing it would be
        // the destructive step this codebase demands positive evidence for —
        // an empty worktree in the switcher is the honest leftover.
        //
        // R16: within one owner a name is held by at most one live session.
        // A brand-new worktree has none, but the record is what would make
        // two of them share a name, so it is checked rather than assumed.
        if crate::session::live_names(&owner).contains(&session) {
            return Err(format!("{} already has a terminal named {session}", c.name));
        }
        crate::home::record(&project, &session, &owner).map_err(|e| {
            format!("{} was created, but roost could not record that {session} moved there: {e}", c.name)
        })?;
        // Install the worktree hook into the new worktree's own
        // `.claude/settings.local.json`, mirroring whatever the main
        // checkout has: a worktree Claude never ran `roost worktree-hook`
        // from `dir` to enable it, so it inherits the setting instead of
        // starting cold every time. Off the hub lock — `dir` and `c.path`
        // are plain paths, and `claudehooks::set_kind` does its own file
        // I/O with no lock involved.
        //
        // Errors are logged and swallowed, not propagated: the worktree and
        // its `home::record` already exist by this point, so failing the
        // whole approve now would report a worktree creation failure for a
        // worktree that in fact exists — the one thing CLAUDE.md's
        // destruction table is about, in its "says something untrue" form.
        // A fresh Claude started there without the hook just starts cold,
        // same as any project that never had it installed.
        if crate::claudehooks::state_of(&dir, crate::claudehooks::Kind::Worktree)
            == crate::claudehooks::HookState::Present
        {
            if let Err(e) = crate::claudehooks::set_kind(&c.path, crate::claudehooks::Kind::Worktree, true) {
                eprintln!("roost: could not install the worktree hook into {}: {e}", c.path.display());
            }
        }
        // Both hubs learn about the move before Claude does, so a
        // `StartTerminal`/`NewTerminal` racing the reply already sees the
        // resident. One lock at a time; never two.
        for p in [&project, &owner] {
            if let Some(arc) = open_hub(p) {
                let mut h = Hub::lock(&arc);
                h.refresh_live_sessions();
                let snap = h.snapshot_event(&String::new());
                h.broadcast(&snap);
            }
        }
        Ok(c.path.to_string_lossy().into_owned())
    });
    let r = std::panic::catch_unwind(work).unwrap_or_else(|_| Err("worktree creation panicked".into()));
    let ev = match &r {
        Ok(_) => Event::WorktreeReady { url: created.clone().unwrap_or_default(), launch: None, fork: None },
        Err(msg) => Event::Error { msg: msg.clone() },
    };
    // Re-locked only to deliver: the clicker holds the blank tab
    // `WorktreeReady` navigates (or hears why there is nothing to open).
    // Before the reply rather than after, so by the time Claude is told
    // anything the clicker's tab is already on its way — the owner hub was
    // refreshed above, so that page shows the resident when it loads.
    if let Some(arc) = open_hub(&project) {
        Hub::lock(&arc).send_to(&clicker, &ev);
    }
    let _ = reply.send(match r {
        Ok(path) => crate::hooksock::AskReply::Path { path },
        Err(msg) => ask_refuse(msg),
    });
    // After every lock is released: `broadcast_all` locks each hub, this
    // project's included (see `do_new_worktree`).
    if let Some(owner) = created {
        broadcast_all(&Event::ProjectsChanged { project: owner });
        broadcast_all(&Event::ProjectsChanged { project });
    }
}

/// Whether `project` has a hub at all — `ide.rs`'s `open_diff` needs this
/// before deciding to deliver a proposal to an owner: parking it there with
/// no hub to receive it would make `open_proposal` silently do nothing
/// (see its own doc comment) while Claude waits forever and the row still
/// holds one of that project's `MAX_PENDING` slots.
pub fn is_open(project: &str) -> bool {
    open_hub(project).is_some()
}

/// Whether roost is holding unsaved edits to `rel`.
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

/// Push a fresh settings snapshot to every project's clients after a
/// process-wide fact changed — an update choice, which is not about any
/// project, so every open page must drop or keep its mark at once. Same
/// lock order as `broadcast_all`: registry, then hub, never both.
///
/// **The caller must hold no hub lock** — this locks every hub in turn, the
/// caller's own included, and a `Mutex` is not reentrant. Call it where
/// `wsconn` calls `broadcast_to_project`: after the block that held the lock.
///
/// The settings view is built *between* the two short hub locks, not under
/// either: it reads both config files and the choices file, and a hub lock
/// held across that would stall every browser on the project for a choice
/// made on another one. It is used for this one event and not left in the
/// cache, because it was computed outside the lock — a `SetSetting` landing
/// in that window would otherwise be hidden by a stale cached view until the
/// next invalidation. The next snapshot recomputes, exactly as after any
/// other invalidation.
pub fn broadcast_settings_all() {
    let Some(reg) = REGISTRY.get() else { return };
    let hubs: Vec<Arc<Mutex<Hub>>> = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    };
    for h in hubs {
        let dir = Hub::lock(&h).dir.clone();
        let fresh = crate::config::settings_view(&dir);
        let mut g = Hub::lock(&h);
        g.settings = Some(fresh);
        let ev = g.snapshot_event(&String::new());
        g.settings = None;
        g.broadcast(&ev);
    }
}

/// Push a fresh snapshot to one project's clients. Unlike every other
/// snapshot push, nothing in the workspace changed — what the snapshot
/// *derives* did (see `claudes::tick`). A project nobody has open has no hub
/// and needs nothing, so a miss here is the normal case, not an error.
pub fn broadcast_state_for(project: &str) {
    let Some(reg) = REGISTRY.get() else { return };
    // The registry lock is released before the hub lock is taken, as in
    // `broadcast_all`: holding both is how this project's one shipped
    // deadlock happened.
    let hub = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).cloned()
    };
    let Some(h) = hub else { return };
    let mut g = Hub::lock(&h);
    let ev = g.snapshot_event(&String::new());
    g.broadcast(&ev);
}

/// Send an event to the clients of one project and nobody else. Same lock
/// order as `broadcast_all` — registry, then hub — but it clones a single
/// `Arc` rather than the whole map.
pub fn broadcast_to_project(project: &str, ev: &Event) {
    let Some(reg) = REGISTRY.get() else { return };
    let hub = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).cloned()
    };
    let Some(h) = hub else { return };
    Hub::lock(&h).broadcast(ev);
}

/// Record a parsed sequence and tell that project's clients. Called from the
/// PTY pump thread, which holds no lock at this point and must never panic.
///
/// Scoped rather than machine-wide: a browser tab is opened on exactly one
/// project key — and a worktree is a key of its own — so a notice from
/// another project has nothing to attach to there. It cannot be shown in a
/// panel that is about this project, and routing a click on it away from the
/// project the user is working in is the behaviour that prompted the change.
///
/// `project` here is always the session's *home* — the pump has no notion
/// of re-homing — so delivery is to `effective_owner(project, session)`
/// instead: a re-homed session's tab lives in the owner's workspace now,
/// and that is the panel a notice from it must land in. Computed before any
/// lock is taken (CLAUDE.md: never hold a lock across blocking I/O, and this
/// is a handful of small file reads/stats, never git).
pub fn publish(project: &str, session: &str, p: crate::osc::Parsed) {
    let target = crate::home::effective_owner(project, session);
    // Recorded under `target`, not `project`: `Notice.project` is what
    // `notify::list_for` filters a reload or a mark-all-read/clear against,
    // and a re-homed session's tab is the owner's now — filing it under the
    // home would make it invisible to everything but a browser that stayed
    // subscribed across this very call.
    if let Some(notice) = crate::notify::record(&target, session, p) {
        broadcast_to_project(&target, &Event::Notice { notice });
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
    use super::launches_to_rearm;

    #[test]
    fn nothing_relaunches_unless_the_setting_asks_for_it() {
        // The property this whole feature's safety rests on, and the reason
        // the decision is a separate function: #17 is careful about relaunch
        // because it starts an agent in a checkout whose state it does not
        // know, so an install that never opted in must see no difference at
        // all — even with a launch recorded and the tab lost, which is exactly
        // the state that would otherwise fire.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("rearm{}", std::process::id());
        crate::relaunch::record(project.as_str(), "term", crate::proto::Launch::Claude);
        let lost = vec!["term".to_string()];

        // Asserts the state it then negates: with the setting on this *is* a
        // relaunch, so the empty result below is the setting talking and not a
        // missing marker.
        assert_eq!(
            launches_to_rearm(&lost, &project, true),
            vec![("term".to_string(), crate::proto::Launch::Claude)],
            "setup: opted in, a recorded launch comes back"
        );
        assert!(
            launches_to_rearm(&lost, &project, false).is_empty(),
            "opted out, nothing is armed"
        );
    }

    /// #18 step 2: the offer is made only where there is something to
    /// resume, and never for a tab that merely has no shell.
    ///
    /// Revert-checked: dropping the `recorded(..).is_some()` filter makes the
    /// second assertion fail with `["ghost", "plain"]` — every lost terminal
    /// offering to resume a conversation that does not exist.
    #[test]
    fn only_a_lost_terminal_with_a_recorded_claude_is_offered_as_resumable() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::wsstate::set_state_dir_for_test();
        let project = format!("resumable{}", std::process::id());
        crate::claudesess::record(
            &project,
            "ghost",
            &crate::claudesess::Recorded {
                session_id: "864ee734-e3ab-434e-8278-745a850b16ad".into(),
                transcript_path: None,
                event: "SessionStart".into(),
            },
        );
        let lost = vec!["ghost".to_string(), "plain".to_string()];
        // Asserts the setup it then narrows: without this, an empty result
        // below would be just as true of a state directory nothing was ever
        // written to, which is how a filter test passes vacuously.
        assert!(
            crate::claudesess::recorded(&project, "ghost").is_some(),
            "setup: `ghost` has a record"
        );
        assert_eq!(
            resumable_terminal_sessions(&lost, &project),
            vec!["ghost".to_string()],
            "`plain` lost its shell too, but there is nothing recorded to resume"
        );
        crate::claudesess::forget(&project, "ghost");
    }

    /// The record outlives only a session nobody ended, which is what makes
    /// "resumable" a subset of "lost" rather than a second, independent list.
    ///
    /// This is the assertion the subset property actually needs: the function
    /// above takes `lost` as its input, so it cannot *not* be a subset there —
    /// a test at that level would prove nothing. The place it can drift is
    /// `refresh_live_sessions`, where a shell coming back has to take the
    /// offer with it.
    ///
    /// Revert-checked: removing the `resumable_sessions.retain` leaves
    /// `["ghost"]` after the session goes live — a placeholder offering to
    /// resume in a tab that is no longer showing a placeholder at all.
    #[test]
    fn a_terminal_that_has_a_shell_again_stops_offering_a_resume() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("resumelive{}", std::process::id());
        let mut h = Hub::new(&project, d.path().to_path_buf());
        h.ws.lost_sessions = vec!["ghost".into()];
        h.ws.resumable_sessions = vec!["ghost".into()];

        h.refresh_live_sessions();
        assert_eq!(
            h.ws.resumable_sessions,
            vec!["ghost".to_string()],
            "setup: with no shell running, the offer stands"
        );

        // A real session under that name, which is what the client sees when
        // the user starts the terminal.
        crate::session::reserve_and_attach(&project, "ghost", d.path()).unwrap();
        h.refresh_live_sessions();
        assert!(h.ws.lost_sessions.is_empty(), "setup: it is no longer lost");
        assert!(
            h.ws.resumable_sessions.is_empty(),
            "and the resume offer goes with it: {:?}",
            h.ws.resumable_sessions
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
    }

    /// The whole point of the feature, asserted where it lands: the shell the
    /// next attach spawns is handed `--resume <the recorded id>`.
    ///
    /// Revert-checked: with the `if resume` block deleted from
    /// `do_start_terminal`, `att.launch` is `None` and this fails on
    /// "the resume must be parked for the next attach".
    #[test]
    fn starting_a_terminal_with_resume_parks_the_recorded_conversation() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("resumestart{}", std::process::id());
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";
        crate::claudesess::record(
            &project,
            "term",
            &crate::claudesess::Recorded {
                session_id: id.into(),
                transcript_path: None,
                event: "Stop".into(),
            },
        );
        let mut h = Hub::new(&project, d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::StartTerminal { session: "term".into(), resume: true });
        let att = crate::session::reserve_and_attach(&project, "term", d.path()).unwrap();
        let l = att.launch.as_ref().expect("the resume must be parked for the next attach");
        assert_eq!(l.launch, proto::Launch::Claude);
        assert_eq!(
            l.session.as_ref(),
            Some(&crate::launch::ClaudeSession::Resume(id.to_string())),
            "the id must be the recorded one, and it must be a Resume, not a Fresh"
        );
        // The end of the road: what actually reaches the shell. Asserted here
        // as well as in `launch`'s own tests because this is the only place
        // the recorded id and the typing meet — a `Fresh` here would type
        // `--session-id`, start an empty conversation, and look identical
        // until the user noticed the history was gone.
        assert_eq!(
            crate::launch::keystrokes(l.launch, l.session.as_ref()),
            format!("claude --resume {id}\r").into_bytes()
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
    }

    /// The two ways a resume must *not* happen, in the shape a stale client
    /// or a plain Enter produces.
    ///
    /// Revert-checked: moving the `if resume` block above the refusal checks —
    /// or dropping the `resume` guard so any start resumes — fails the second
    /// case with a parked `Resume`, which is a terminal that silently
    /// continues an old conversation when the user asked for a shell.
    #[test]
    fn a_plain_start_and_a_resume_with_nothing_recorded_both_give_a_bare_shell() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("resumenone{}", std::process::id());
        let mut h = Hub::new(&project, d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        // Asked to resume, nothing recorded: still a terminal, never an error
        // about a session — "nothing recorded" is not "no Claude ran here".
        h.handle(&c, Intent::StartTerminal { session: "term".into(), resume: true });
        let att = crate::session::reserve_and_attach(&project, "term", d.path()).unwrap();
        assert_eq!(att.launch, None, "no record, so nothing to type");
        crate::session::kill_project(&project);

        // Recorded, but the user pressed Enter rather than clicking Resume.
        crate::claudesess::record(
            &project,
            "term2",
            &crate::claudesess::Recorded {
                session_id: "864ee734-e3ab-434e-8278-745a850b16ad".into(),
                transcript_path: None,
                event: "Stop".into(),
            },
        );
        h.handle(&c, Intent::StartTerminal { session: "term2".into(), resume: false });
        let att = crate::session::reserve_and_attach(&project, "term2", d.path()).unwrap();
        assert_eq!(att.launch, None, "Enter starts a shell, even where a resume was available");
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
    }

    /// The ✻ menu's chosen row, end to end: the intent carries an id, the
    /// server confirms this project has that conversation, and the shell that
    /// spawns is handed `--resume <it>`.
    ///
    /// Revert-checked: dropping the `claudehist::has` filter still passes this
    /// one — the id *is* valid here — which is why the refusal has its own test
    /// below rather than being an extra assertion on this one.
    #[test]
    fn a_chosen_conversation_from_the_menu_is_what_the_new_terminal_resumes() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let project = format!("menupick{}", std::process::id());
        let dir = d.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";
        let tdir = crate::claudehist::transcript_dir(&dir).unwrap();
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join(format!("{id}.jsonl")), "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}").unwrap();

        let mut h = Hub::new(&project, dir.clone());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);
        h.handle(&c, Intent::NewTerminal {
            pane: proto::RIGHT,
            launch: Some(proto::Launch::Claude),
            force: true,
            resume: Some(id.to_string()),
            fork: None,
        });
        // `claude`, not `term`: develop gave the ✻ click its own name sequence,
        // so the reservation this parks is on the name the click was allocated.
        let att = crate::session::reserve_and_attach(&project, "claude", &dir).unwrap();
        let l = att.launch.as_ref().expect("the menu's pick must be parked");
        assert_eq!(
            l.session.as_ref(),
            Some(&crate::launch::ClaudeSession::Resume(id.to_string()))
        );
        assert_eq!(
            crate::launch::keystrokes(l.launch, l.session.as_ref()),
            format!("claude --resume {id}\r").into_bytes()
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
        match prev_home { Some(v) => std::env::set_var("HOME", v), None => std::env::remove_var("HOME") }
    }

    /// A row the project does not have. The menu is a hint, not an
    /// authorisation — the same rule `RemoveWorktree` states about its own —
    /// so a stale, forged or cross-project id must not become a `--resume`.
    ///
    /// It falls through to a *fresh* Claude rather than failing: the user asked
    /// for one, and the worst outcome should be a new conversation instead of
    /// an old one, not a button that does nothing.
    ///
    /// Revert-checked: removing the `claudehist::has` filter fails this with
    /// `Some(Resume("aaaa1111-…"))` against the expected `Fresh`.
    #[test]
    fn an_id_this_project_does_not_have_falls_through_to_a_fresh_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let project = format!("menuforged{}", std::process::id());
        let dir = d.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();

        let mut h = Hub::new(&project, dir.clone());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);
        h.handle(&c, Intent::NewTerminal {
            pane: proto::RIGHT,
            launch: Some(proto::Launch::Claude),
            force: true,
            resume: Some("aaaa1111-2222-3333-4444-555555555555".to_string()),
            fork: None,
        });
        // `claude`, not `term`: develop gave the ✻ click its own name sequence,
        // so the reservation this parks is on the name the click was allocated.
        let att = crate::session::reserve_and_attach(&project, "claude", &dir).unwrap();
        let l = att.launch.as_ref().expect("a Claude was still asked for");
        assert!(
            matches!(l.session.as_ref(), Some(crate::launch::ClaudeSession::Fresh(_))),
            "an unauthorised id must not resume, and must not stop the launch: {:?}",
            l.session
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
        match prev_home { Some(v) => std::env::set_var("HOME", v), None => std::env::remove_var("HOME") }
    }

    /// Fix round 1, finding 3. `NewTerminal.fork` end to end, the same shape
    /// as `a_chosen_conversation_from_the_menu_is_what_the_new_terminal_resumes`
    /// above but for a fork rather than a plain resume: the id lands in
    /// `ClaudeSession::Fork`, not `Resume`, and `keystrokes` types
    /// `--fork-session` after it. No `claudehist`/`HOME` setup needed here —
    /// `fork` is validated by shape alone (`claudesess::valid_session_id`),
    /// never by "does this project have that conversation" (see this file's
    /// own `NewTerminal.fork` doc comment for why that is not a missing
    /// authorisation check).
    ///
    /// Revert-checked: swapping `do_new_terminal`'s `fork.filter(...).map(ClaudeSession::Fork)`
    /// for `ClaudeSession::Resume` fails the second assertion — observed:
    /// left: `Some(Fork("864ee734-…"))`, right: `Some(Resume("864ee734-…"))`.
    /// Restored.
    #[test]
    fn new_terminal_with_a_valid_fork_parks_a_fork_launch() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("forkterm{}", std::process::id());
        let dir = d.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";

        let mut h = Hub::new(&project, dir.clone());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);
        h.handle(&c, Intent::NewTerminal {
            pane: proto::RIGHT,
            launch: Some(proto::Launch::Claude),
            force: true,
            resume: None,
            fork: Some(id.to_string()),
        });
        let att = crate::session::reserve_and_attach(&project, "claude", &dir).unwrap();
        let l = att.launch.as_ref().expect("a fork must still ask for a Claude");
        assert_eq!(l.session.as_ref(), Some(&crate::launch::ClaudeSession::Fork(id.to_string())));
        assert_eq!(
            crate::launch::keystrokes(l.launch, l.session.as_ref()),
            format!("claude --resume {id} --fork-session\r").into_bytes()
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The same shape as `an_id_this_project_does_not_have_falls_through_to_a_fresh_claude`,
    /// for `fork`'s own validation: a shape-invalid id (one that could read
    /// as a command-line option) must not reach `--resume`/`--fork-session`
    /// and must not stop the launch — it degrades to a fresh Claude, same
    /// as an unauthorised `resume` does.
    ///
    /// Revert-checked: dropping `do_new_terminal`'s `claudesess::valid_session_id`
    /// guard on `fork` (filtering only on `l == Launch::Claude`, passing the
    /// id straight to `ClaudeSession::Fork` otherwise) fails this test here,
    /// at the hub layer — observed: `panicked ... an invalid fork id must
    /// not stop the launch: Some(Fork("--dangerously-skip-permissions"))`.
    /// `launch.rs`'s own `usable()` guard would still have degraded it at
    /// typing time, but this pins that the *parked* `ClaudeSession` is
    /// already `Fresh`, not a `Fork` carrying an unusable id waiting to be
    /// degraded later. Restored.
    #[test]
    fn new_terminal_with_an_invalid_fork_falls_back_to_a_fresh_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("forktermbad{}", std::process::id());
        let dir = d.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();

        let mut h = Hub::new(&project, dir.clone());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);
        h.handle(&c, Intent::NewTerminal {
            pane: proto::RIGHT,
            launch: Some(proto::Launch::Claude),
            force: true,
            resume: None,
            fork: Some("--dangerously-skip-permissions".into()),
        });
        let att = crate::session::reserve_and_attach(&project, "claude", &dir).unwrap();
        let l = att.launch.as_ref().expect("a Claude was still asked for");
        assert!(
            matches!(l.session.as_ref(), Some(crate::launch::ClaudeSession::Fresh(_))),
            "an invalid fork id must not stop the launch: {:?}",
            l.session
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// #17's rule, now that there is something for it to rule out.
    ///
    /// `relaunch` restarts the agent roost launched; it must hand it a *fresh*
    /// conversation, because resuming would continue one whose last turn may
    /// have been mid-edit. Before #18 step 2 that was true because roost had
    /// no recorded id to pass; it is now true only because this code does not
    /// pass it, which is exactly the kind of property that rots silently.
    ///
    /// Revert-checked: changing `rearm_launches_with` to look the record up
    /// and park a `Resume` fails here with
    /// `Some(Resume("864ee734-…"))` against the expected `None`.
    #[test]
    fn a_relaunched_agent_starts_a_fresh_conversation_even_with_one_recorded() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = format!("rearmfresh{}", std::process::id());
        crate::relaunch::record(&project, "term", proto::Launch::Claude);
        crate::claudesess::record(
            &project,
            "term",
            &crate::claudesess::Recorded {
                session_id: "864ee734-e3ab-434e-8278-745a850b16ad".into(),
                transcript_path: None,
                event: "Stop".into(),
            },
        );
        // Both records present, and the setting on — so anything the code
        // *could* pass through is available to it.
        assert!(crate::claudesess::recorded(&project, "term").is_some(), "setup: a conversation exists");

        rearm_launches_with(&[String::from("term")], &project, true);
        let att = crate::session::reserve_and_attach(&project, "term", d.path()).unwrap();
        let l = att.launch.as_ref().expect("setup: the relaunch was armed");
        assert_eq!(l.launch, proto::Launch::Claude);
        assert_eq!(l.session, None, "#17: the launch kind only, never a resume");
        assert_eq!(
            crate::launch::keystrokes(l.launch, l.session.as_ref()),
            b"claude\r".to_vec(),
            "a bare claude reaches the shell"
        );
        crate::session::kill_project(&project);
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn a_lost_tab_roost_never_launched_anything_in_is_left_alone() {
        // Most terminals are plain shells. Relaunching one would be starting
        // something the user never asked roost to start — and `default_layout`
        // ships a `term` tab in every project, so this is the common case
        // rather than an edge.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("rearmplain{}", std::process::id());
        assert!(
            launches_to_rearm(&[String::from("term")], &project, true).is_empty(),
            "no recorded launch, nothing to relaunch"
        );
    }

    use super::lost_terminal_sessions;

    fn ws_with_terminals(names: &[&str]) -> crate::workspace::Workspace {
        let mut w = crate::workspace::Workspace::default_layout();
        for p in w.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, crate::proto::Tab::Terminal { .. }));
        }
        w.panes[crate::proto::RIGHT as usize].tabs = names
            .iter()
            .map(|n| crate::proto::Tab::Terminal { session: (*n).to_string(), home: None })
            .collect();
        w
    }

    #[test]
    fn a_terminal_tab_with_no_shell_left_is_named_as_lost() {
        // The point of the whole field: after a reboot a restored Terminal tab
        // rendered the same "Press Enter to start a terminal" placeholder as a
        // tab nobody had ever used, so losing a long-running Claude was
        // indistinguishable from never having started one.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("lostsess{}", std::process::id());
        let dir = crate::wsstate::state_dir()
            .join("sock")
            .join(crate::projects::storage_key(&project));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("alive"), b"").unwrap();

        let mut w = ws_with_terminals(&["alive", "gone"]);
        w.live_sessions = vec!["alive".into()];
        w.sessions_seen = vec!["alive".into(), "gone".into()];
        assert_eq!(
            lost_terminal_sessions(&w, &project),
            vec!["gone".to_string()],
            "only the tab with no socket and no live session"
        );
    }

    #[test]
    fn a_tab_that_never_had_a_shell_is_not_called_lost() {
        // The inverse of the mistake this codebase keeps making, and a bug
        // this feature actually shipped in its first draft: instead of
        // concluding "gone" from a failed check, it concluded "gone" from no
        // check at all. `default_layout` puts an empty `term` tab in the right
        // pane, so on a brand-new project — no socket directory, nothing ever
        // started — every user was told their shell had not survived a reboot
        // that never happened. Found by a browser test, not by review.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("neverran{}", std::process::id());
        let _ = std::fs::remove_dir_all(
            crate::wsstate::state_dir().join("sock").join(crate::projects::storage_key(&project)),
        );

        let fresh = crate::workspace::Workspace::default_layout();
        assert!(
            fresh.panes[crate::proto::RIGHT as usize]
                .tabs
                .iter()
                .any(|t| matches!(t, crate::proto::Tab::Terminal { .. })),
            "setup: the default layout must actually carry a terminal tab, or \
             this test passes by describing a layout that no longer exists"
        );
        assert_eq!(
            lost_terminal_sessions(&fresh, &project),
            Vec::<String>::new(),
            "a tab whose shell was never started has lost nothing"
        );

        // And the same layout, once roost has recorded seeing a shell there,
        // *is* a loss. Asserting both against one fixture is what stops the
        // check above from being satisfied by a function that returns nothing.
        let mut seen = fresh;
        seen.sessions_seen = vec!["term".into()];
        assert_eq!(
            lost_terminal_sessions(&seen, &project),
            vec!["term".to_string()],
            "but the same tab, after roost saw a shell in it, is"
        );
    }

    #[test]
    fn an_unreadable_socket_directory_claims_nothing() {
        // The rule this codebase is built around, in its "says something
        // untrue" form rather than its destructive one: a failed `read_dir`
        // must not become "your shell is gone" about a shell that is still
        // running. `socket_names_checked` answers `None` there, and `None`
        // means claim nothing.
        //
        // Driven through a state dir that positively cannot be read, rather
        // than by mocking: removing search permission is what an EACCES on a
        // real deployment looks like. Skipped as root, which ignores the
        // permission bits — this project has been bitten by that before.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if unsafe { libc_geteuid() } == 0 {
                eprintln!("skipping: running as root ignores the permission bits");
                return;
            }
            crate::wsstate::set_state_dir_for_test();
            let project = format!("unreadable{}", std::process::id());
            let dir = crate::wsstate::state_dir()
                .join("sock")
                .join(crate::projects::storage_key(&project));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("gone"), b"").unwrap();
            // Asserts the state it later negates: readable, the tab *is* lost.
            let mut w = ws_with_terminals(&["ghost"]);
            w.live_sessions = vec![];
            w.sessions_seen = vec!["ghost".into()];
            assert_eq!(
                lost_terminal_sessions(&w, &project),
                vec!["ghost".to_string()],
                "setup: while the directory is readable, the claim is made"
            );

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let claimed = lost_terminal_sessions(&w, &project);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                claimed.is_empty(),
                "an unreadable directory must claim nothing, got {claimed:?}"
            );
        }
    }

    extern "C" {
        #[link_name = "geteuid"]
        fn libc_geteuid_raw() -> u32;
    }
    unsafe fn libc_geteuid() -> u32 {
        libc_geteuid_raw()
    }

    use super::*;
    use crate::proto::{self, Mode, Tab, WorktreeLaunch};

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

    /// Whether any pane carries a terminal tab for `session`, regardless of
    /// whether it is native or a resident's.
    fn has_terminal_tab(h: &Hub, session: &str) -> bool {
        h.ws.panes.iter().flat_map(|p| p.tabs.iter()).any(
            |t| matches!(t, Tab::Terminal { session: s, .. } if s == session),
        )
    }

    /// The `home` carried by `session`'s terminal tab, if any pane has one —
    /// `None` both when there is no such tab and when the tab is native, a
    /// distinction this test module never needs to tell apart on its own
    /// (every caller already knows which it is asking about).
    fn terminal_tab_home(h: &Hub, session: &str) -> Option<String> {
        for p in &h.ws.panes {
            for t in &p.tabs {
                if let Tab::Terminal { session: s, home } = t {
                    if s == session {
                        return home.clone();
                    }
                }
            }
        }
        None
    }

    /// Every terminal tab in the workspace, across every pane, as
    /// `(session, home)` pairs — the fully faithful version
    /// `terminal_tab_home` cannot be (R15): since a native tab and a
    /// resident's tab may legally share a session name and differ only in
    /// `home`, a test that needs to tell the two apart, or count them,
    /// cannot settle for "the first match".
    fn terminal_tabs(h: &Hub) -> Vec<(String, Option<String>)> {
        h.ws
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match t {
                Tab::Terminal { session, home } => Some((session.clone(), home.clone())),
                _ => None,
            })
            .collect()
    }

    /// Two hubs sharing one live session: `home` is where its dtach socket
    /// actually lives, `owner` is a worktree under it recorded as where the
    /// tab now shows. Plain `Hub::new`, not `for_project` — this is about
    /// `refresh_live_sessions`'s own bookkeeping, which needs neither a
    /// watcher thread nor an IDE lock file, and both hubs are handed back as
    /// `Arc<Mutex<Hub>>` only so the test can `Hub::lock` either one the way
    /// a real connection would.
    ///
    /// The home's own tab for `session` is added by hand
    /// (`Workspace::default_layout`'s own seeded `term` tab is a different
    /// name on purpose, so it cannot be mistaken for this fixture's subject)
    /// — nothing that opens a tab runs here, because what is under test is
    /// what happens to a tab that is already there once re-homing is
    /// recorded, not how the tab got there.
    fn two_hubs_with_session(
        root: &std::path::Path,
        session: &str,
    ) -> (Arc<Mutex<Hub>>, Arc<Mutex<Hub>>, String, String) {
        let home = format!("rehome-{session}-home");
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.join(&home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();
        crate::session::reserve_and_attach(&home, session, &home_dir).unwrap();
        let mut home_hub = Hub::new(&home, home_dir);
        home_hub.ws.panes[proto::RIGHT as usize]
            .tabs
            .push(Tab::Terminal { session: session.to_string(), home: None });
        let owner_hub = Hub::new(&owner, owner_dir);
        (Arc::new(Mutex::new(home_hub)), Arc::new(Mutex::new(owner_hub)), home, owner)
    }

    /// Two hubs and two connections, so a broadcast to the wrong project is
    /// visible — a single-subscriber fixture cannot tell `send_to` from
    /// `broadcast`, and cannot tell "sent to the right hub" from "sent to
    /// whichever hub happened to be open".
    ///
    /// Revert-checked (Step 5): removing the tab-drop half of
    /// `refresh_live_sessions` (the loop that calls `EndSession` on a native
    /// tab whose session just resolved elsewhere) fails at the first
    /// assertion — observed: `assertion failed: !has_terminal_tab(&Hub::lock(&home_hub), "term3")`.
    #[test]
    fn rehoming_moves_the_tab_and_tells_the_home() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let (home_hub, owner_hub, home, owner) = two_hubs_with_session(root.path(), "term3");
        crate::home::record(&home, "term3", &owner).unwrap();

        let (_ah, rx_ah) = Hub::lock(&home_hub).subscribe();
        let (_ao, rx_ao) = Hub::lock(&owner_hub).subscribe();
        drain(&rx_ah);
        drain(&rx_ao);

        Hub::lock(&home_hub).refresh_live_sessions();
        Hub::lock(&owner_hub).refresh_live_sessions();

        assert!(!has_terminal_tab(&Hub::lock(&home_hub), "term3"), "the home must drop its own tab for a session that moved away");
        assert_eq!(
            terminal_tab_home(&Hub::lock(&owner_hub), "term3"),
            Some(home.clone()),
            "the owner must gain a tab naming the resident's real home"
        );

        let home_msgs = drain(&rx_ah);
        assert!(
            home_msgs.iter().any(|m| m.contains(r#""t":"Rehomed""#) && m.contains("term3") && m.contains(&owner)),
            "the home's own browser must be told where its tab went: {home_msgs:?}"
        );
        let owner_msgs = drain(&rx_ao);
        assert!(
            !owner_msgs.iter().any(|m| m.contains(r#""t":"Rehomed""#)),
            "the owner did not lose a tab, so it must not hear Rehomed: {owner_msgs:?}"
        );

        crate::session::kill_project(&home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R15: this is the primary approve flow, not an edge case — a Claude in
    /// the main checkout's own `term` asks for a worktree, roost creates a
    /// brand-new worktree project, and `Workspace::default_layout` seeds
    /// that fresh project's own `Terminal{session:"term", home:None}`
    /// placeholder before anything else happens to it. `term` is also
    /// `next_free_name`'s own first pick, so this is not a contrived name
    /// collision; it is the name every first terminal gets.
    ///
    /// Revert-checked: reverting the retarget-in-place loop to the
    /// session-only `already_tabbed` check (the pre-R15 body) fails —
    /// observed: `assertion 'left == right' failed\n  left: [("term",
    /// None)]\n right: [("term", Some("r15-approve-home"))]` — the resident
    /// never gets a tab at all, because the bare placeholder already
    /// "counts" as one.
    #[test]
    fn a_fresh_worktree_placeholder_term_becomes_the_resident_term() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        // Setup sanity, independent of the hub under test: `default_layout`
        // really does seed exactly this placeholder, or this test would
        // pass by describing a layout that no longer exists.
        assert_eq!(
            crate::workspace::Workspace::default_layout().panes[proto::RIGHT as usize].tabs,
            vec![Tab::Terminal { session: "term".into(), home: None }]
        );

        let home = "r15-approve-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        crate::session::reserve_and_attach(home, "term", &home_dir).unwrap();
        crate::home::record(home, "term", &owner).unwrap();

        // `Hub::new` runs `refresh_live_sessions` as part of construction
        // (see its own doc comment) — the exact path under test, on a
        // worktree that has never been opened before and so starts from
        // `default_layout`'s own placeholder, with the resident already on
        // record by the time it does.
        let owner_hub = Hub::new(&owner, owner_dir);

        assert_eq!(
            terminal_tabs(&owner_hub),
            vec![("term".to_string(), Some(home.to_string()))],
            "the placeholder must become the resident's tab in place, not sit beside a second one"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R16 supersedes R15 part 2: a native tab this project's own session
    /// actually used (`sessions_seen`, the same evidence
    /// `lost_terminal_sessions` trusts), but whose session is not live now,
    /// is retargeted to the resident in place exactly like a never-seen
    /// placeholder — the invariant (at most one live terminal tab per name
    /// in an owner) has no "but it has history" exception, because a dead
    /// tab's history belongs to a name, not to whichever session happens
    /// to be using it next. The arriving resident must also clear it from
    /// `lost_sessions`: a name that just became a live tab again is not
    /// still waiting to be restored.
    ///
    /// Revert-checked: reintroducing a `!seen.contains(s)` guard on the
    /// retarget (inside the still-current `native_exists` branch, which
    /// unconditionally clears `lost_sessions`/`resumable_sessions` and
    /// `continue`s regardless) means the tab is left native *and* no
    /// resident tab is ever added — observed: `assertion 'left == right'
    /// failed\n  left: [("term", None)]\n right: [("term",
    /// Some("r16-seen-dead-home"))]`. The resident is silently dropped
    /// rather than ending up beside the native tab, because this branch no
    /// longer falls through to the "push a new one" case below it.
    #[test]
    fn a_seen_but_dead_native_term_tab_becomes_the_arriving_resident() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r16-seen-dead-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        // Built with no live session anywhere under the owner and no
        // re-home record yet, so this is the owner's own history being set
        // up, not a side effect of the resident's arrival. `sessions_seen`
        // and `lost_sessions` are set directly (the established pattern in
        // this file — see `a_tab_that_never_had_a_shell_is_not_called_lost`)
        // rather than earned through a real reboot.
        let mut owner_hub = Hub::new(&owner, owner_dir);
        owner_hub.ws.sessions_seen = vec!["term".to_string()];
        owner_hub.ws.lost_sessions = vec!["term".to_string()];

        crate::session::reserve_and_attach(home, "term", &home_dir).unwrap();
        crate::home::record(home, "term", &owner).unwrap();
        owner_hub.refresh_live_sessions();

        assert_eq!(
            terminal_tabs(&owner_hub),
            vec![("term".to_string(), Some(home.to_string()))],
            "the seen-but-dead native tab must become the resident's, not sit beside a second one"
        );
        assert!(
            !owner_hub.ws.lost_sessions.contains(&"term".to_string()),
            "a name that just gained a live tab is no longer lost"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R16's invariant breach case: a live native session and a recorded
    /// resident cannot legitimately share a name, but a race (a browser's
    /// own `StartTerminal` landing between two refreshes) could — driven
    /// directly here rather than actually won, since winning it for real
    /// would make this test flaky by construction. The native tab must
    /// survive untouched and the resident must not get a tab at all; the
    /// *on-disk* record is left alone (not forgotten), so it is still
    /// there to retry once the native session actually ends — but (fix
    /// round 2, New Issue 1) `session_homes`, the in-memory router
    /// `do_end_session` and the `OpenTab` home-fill-in both read, must not
    /// keep naming the hidden resident's home for this name, or a click on
    /// the *visible* native tab's × routes straight to the *hidden*
    /// resident's kill.
    ///
    /// Revert-checked: dropping the `raw_live.contains(&name)` breach guard
    /// (so retargeting proceeds regardless) fails — observed: `assertion
    /// 'left == right' failed\n  left: [("term", Some("r16-live-clash-home"))]\n
    /// right: [("term", None)]` — the live native tab is silently
    /// overwritten to point at the resident, which is exactly the bug this
    /// guard exists to prevent (closing it would then SIGKILL the home's
    /// real session).
    #[test]
    fn a_live_native_term_blocks_a_second_tab_for_a_resident_of_the_same_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r16-live-clash-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        crate::session::reserve_and_attach(&owner, "term", &owner_dir).unwrap();
        crate::session::reserve_and_attach(home, "term", &home_dir).unwrap();
        crate::home::record(home, "term", &owner).unwrap();

        let owner_hub = Hub::new(&owner, owner_dir);

        assert_eq!(
            terminal_tabs(&owner_hub),
            vec![("term".to_string(), None)],
            "the live native tab must survive untouched; the resident must not get a second tab of the same name"
        );
        assert_eq!(
            owner_hub.session_homes.get("term"),
            None,
            "the hidden resident must not stay routable through session_homes while its name is shadowed"
        );
        assert!(
            matches!(crate::home::owner_of(home, "term"), crate::home::Owner::Elsewhere(o) if o == owner),
            "the on-disk record itself must be left alone, so the resident can be retried once the native session ends"
        );

        crate::session::kill_project(&owner);
        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 2, New Issue 1: the breach branch above keeps the native
    /// tab visible and the resident hidden — but before this fix,
    /// `session_homes["term"]` still named the resident's home, so
    /// `EndSession` on the *visible* native tab's × read `kill_target` from
    /// there and SIGKILLed the *hidden* resident instead (`may_end` passed,
    /// because the on-disk record genuinely names this owner). This is
    /// `a_live_native_term_blocks_a_second_tab_for_a_resident_of_the_same_name`'s
    /// same breach fixture, carried one step further: actually end the
    /// session a user clicking the one visible tab would mean.
    ///
    /// Real dtach on both ends: this has to prove which *process* died, not
    /// just which map entry changed.
    ///
    /// Revert-checked: dropping the `self.session_homes.remove(&name)` call
    /// in the breach branch (restoring the New-Issue-1 defect) fails at
    /// this test's own setup assertion — observed: `assertion 'left ==
    /// right' failed\n  left: Some("r17-breach-home")\n right: None` —
    /// `session_homes["term"]` still names the resident's home, which is
    /// exactly the defect `EndSession` on the visible tab would then route
    /// through to SIGKILL the hidden resident instead.
    #[test]
    fn end_session_on_the_visible_native_tab_of_a_breach_pair_ends_only_the_native_one() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r17-breach-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        let att_owner = crate::session::reserve_and_attach(&owner, "term", &owner_dir)
            .expect("/usr/bin/dtach is a runtime prerequisite elsewhere in this project");
        let att_home = crate::session::reserve_and_attach(home, "term", &home_dir)
            .expect("/usr/bin/dtach is a runtime prerequisite elsewhere in this project");
        let owner_sock = crate::session::socket_path(&owner, "term");
        let home_sock = crate::session::socket_path(home, "term");
        for sock in [&owner_sock, &home_sock] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !any_process_holds(sock) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(any_process_holds(sock), "setup: both real dtach masters must be up before this test can mean anything");
        }
        drop(att_owner);
        drop(att_home);

        crate::home::record(home, "term", &owner).unwrap();

        // The breach: a live native `term` at the owner, and a resident of
        // the same name recorded there. `Hub::for_project`'s own
        // construction hits the breach branch in `refresh_live_sessions`
        // immediately — the fix under test is that it must not leave the
        // hidden resident routable through `session_homes`.
        let owner_hub = Hub::for_project(&owner, owner_dir.clone());
        assert_eq!(
            Hub::lock(&owner_hub).session_homes.get("term"),
            None,
            "setup: the breach must not leave the hidden resident routable through session_homes"
        );
        let (c, _rx) = Hub::lock(&owner_hub).subscribe();

        Hub::lock(&owner_hub).handle(&c, Intent::EndSession { session: "term".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while any_process_holds(&owner_sock) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            !any_process_holds(&owner_sock),
            "the visible native tab's End Session must end the native session"
        );
        assert!(
            any_process_holds(&home_sock),
            "the hidden resident at its home must survive — End Session on the native tab must not reach it"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R16's Important 1: `do_end_session` reads `kill_target` from a
    /// `session_homes` snapshot under the hub lock, but the kill itself
    /// runs later, on its own thread, off that lock — so a "Move back" or
    /// a re-point landing in the gap must not go on to SIGKILL a session
    /// this owner no longer has any claim to. Driven deterministically:
    /// the record is forgotten *after* the hub's own snapshot (asserted
    /// below) and *without* a refresh in between, which is what a real race
    /// would also leave behind, just without the timing.
    ///
    /// Real dtach, not `ROOST_CMD=cat`: the home's session has to be a
    /// process `any_process_holds` can actually find surviving (or not).
    ///
    /// Revert-checked: replacing the `may_end` re-check with the pre-fix
    /// unconditional kill reproduces exactly the bug this guards against —
    /// observed: `assertion failed: the clicker must be told nothing was
    /// ended` (no `Error` ever arrives — the kill just happens) and the
    /// follow-up `any_process_holds` assertion on the home's socket fails
    /// too once that one is reached.
    #[test]
    fn end_session_of_a_resident_rehomed_away_since_is_refused() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r16-raced-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        let att = crate::session::reserve_and_attach(home, "term3", &home_dir)
            .expect("/usr/bin/dtach is a runtime prerequisite elsewhere in this project");
        let sock = crate::session::socket_path(home, "term3");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !any_process_holds(&sock) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(any_process_holds(&sock), "setup: the real dtach master must be up before this test can mean anything");
        drop(att);

        crate::home::record(home, "term3", &owner).unwrap();

        let owner_hub = Hub::for_project(&owner, owner_dir.clone());
        assert_eq!(
            Hub::lock(&owner_hub).session_homes.get("term3"),
            Some(&home.to_string()),
            "setup: the owner hub must see term3 as a resident before the record changes"
        );
        let (c, rx) = Hub::lock(&owner_hub).subscribe();
        drain(&rx);

        // The race, driven directly: the record changes between the
        // snapshot just asserted and the kill thread's own re-read, with no
        // refresh in between to let the hub notice.
        crate::home::forget(home, "term3");

        Hub::lock(&owner_hub).handle(&c, Intent::EndSession { session: "term3".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got_refusal = false;
        while std::time::Instant::now() < deadline {
            if drain(&rx).iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("moved since this was asked")) {
                got_refusal = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(got_refusal, "the clicker must be told nothing was ended");
        assert!(
            any_process_holds(&sock),
            "the home's real session must survive a refused End Session"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 2, New Issue 2: `may_end`'s native branch used to return
    /// `true` the instant `kill_target == project`, with no record check —
    /// but `session_homes` holds residents *of* this project, so it says
    /// nothing about a *native* session that was itself re-homed *away*.
    /// `EndSession{n}` sent at `n`'s own home, after `n` moved to a
    /// worktree under it, must not fall through to an unconditional native
    /// kill — that would SIGKILL the Claude that now belongs to the
    /// worktree and forget its record out from under it. Reachable via the
    /// End-session confirm dialog's own staleness window (app.js) or a
    /// second browser that has not yet heard the `Rehomed` event.
    ///
    /// `Hub::for_project`, the thread path: the fix has to hold off the hub
    /// lock exactly where the resident branch already does, not just in a
    /// synchronous fallback.
    ///
    /// Revert-checked: reverting `may_end`'s native branch to
    /// `kill_target == project` (no record check) reproduces the bug —
    /// observed: no `Error` ever reaches `rx` (`got_refusal` stays false)
    /// and `any_process_holds` on the home's own socket fails once that
    /// assertion is reached — the kill proceeds unconditionally.
    #[test]
    fn end_session_on_a_home_refuses_a_native_name_rehomed_away() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r17-nativekill-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        let att = crate::session::reserve_and_attach(home, "term", &home_dir)
            .expect("/usr/bin/dtach is a runtime prerequisite elsewhere in this project");
        let sock = crate::session::socket_path(home, "term");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !any_process_holds(&sock) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(any_process_holds(&sock), "setup: the real dtach master must be up before this test can mean anything");
        drop(att);

        // Re-homed away from its own home, to a worktree under it — this
        // project's own `term` is no longer its to end, even though
        // `session_homes` (residents *of* this project) has nothing to say
        // about it either way: it is neither a resident here nor, any
        // more, truly native.
        crate::home::record(home, "term", &owner).unwrap();

        let home_hub = Hub::for_project(home, home_dir.clone());
        assert!(
            Hub::lock(&home_hub).session_homes.get("term").is_none(),
            "setup: term is not a resident of its own home"
        );
        let (c, rx) = Hub::lock(&home_hub).subscribe();
        drain(&rx);

        Hub::lock(&home_hub).handle(&c, Intent::EndSession { session: "term".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got_refusal = false;
        while std::time::Instant::now() < deadline {
            if drain(&rx).iter().any(|m| m.contains(r#""t":"Error""#)) {
                got_refusal = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(got_refusal, "the clicker must be told nothing was ended");
        assert!(
            any_process_holds(&sock),
            "a session re-homed away must survive an End Session issued at its own home"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R16's Important 2: a client that still thinks a resident's name is
    /// native (`focusSession`'s own-state miss, or a session search result
    /// row — both send a bare `OpenTab{tab:{k:"Terminal", session}}` with
    /// no `home`, since neither has any way to know it) must still land on
    /// the resident's actual tab, not push a second, native one and
    /// reserve this project's own copy of a name its `sock/` can never
    /// hold.
    ///
    /// Revert-checked: removing the `OpenTab` home-filling rewrite in
    /// `handle` reproduces the bug — observed: `assertion 'left == right'
    /// failed\n  left: [("term3", Some("r16-homeless-home")), ("term3", None)]\n
    /// right: [("term3", Some("r16-homeless-home"))]` — a second, native
    /// tab is pushed into `RIGHT` instead of the existing resident tab
    /// being focused (the reservation assertion would fail too, for the
    /// same reason, if this one were reached first).
    #[test]
    fn a_homeless_open_tab_for_a_resident_focuses_its_tab() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("r16-homeless-owner", d.path().to_path_buf());
        // Clear every default-layout terminal tab; the only `term` tab in
        // play is the resident's own, set up directly, since this test is
        // about the `OpenTab` rewrite in `handle`, not about how a resident
        // normally gets its tab (`refresh_live_sessions`, tested elsewhere).
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
        }
        h.session_homes.insert("term3".into(), "r16-homeless-home".into());
        h.ws.panes[proto::LEFT_BOTTOM as usize]
            .tabs
            .push(Tab::Terminal { session: "term3".into(), home: Some("r16-homeless-home".into()) });
        // LEFT_BOTTOM is now [Changes (default_layout's own, active), the
        // resident's tab] — active still on Changes, so focusing the
        // resident is a real, observable change.
        assert_eq!(h.ws.panes[proto::LEFT_BOTTOM as usize].active, 0, "setup: active starts on Changes");
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::RIGHT, tab: Tab::Terminal { session: "term3".into(), home: None } },
        );

        assert_eq!(
            terminal_tabs(&h),
            vec![("term3".to_string(), Some("r16-homeless-home".to_string()))],
            "still exactly one terminal tab, the resident's own"
        );
        assert_eq!(
            h.ws.panes[proto::LEFT_BOTTOM as usize].active, 1,
            "the resident's tab must become active, not be ignored in favour of a push elsewhere"
        );
        assert!(
            !crate::session::has_reservation_for_test("r16-homeless-owner", "term3"),
            "a home-less OpenTab for a resident must not reserve this project's own name for it"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R17 (fix round 2, folded from the same lines as Important 2): the
    /// `OpenTab` home-fill-in normalises `home` unconditionally now, not
    /// only when the intent arrives with `home: None` — a forged
    /// `OpenTab{Terminal{n, home: Some(x)}}` for a name that is actually
    /// native has no more claim to `x` than a bare one would, and
    /// `session_homes` is the only source of truth for what `home` means
    /// on this page.
    ///
    /// Revert-checked: narrowing the `match` back to `home: None` (the
    /// round-1 body) lets the forged `home` through unchanged — observed:
    /// `assertion 'left == right' failed\n  left: [("term",
    /// Some("elsewhere")), ("term", None)]\n right: [("term", None)]` — a
    /// second tab is pushed under the forged home instead of the existing
    /// native one being focused.
    #[test]
    fn a_forged_home_on_open_tab_is_normalized_to_the_truth() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("r17-forged-owner", d.path().to_path_buf());
        // "term" is native here (default_layout's own placeholder) and not
        // a resident of anything — `session_homes` has no entry for it.
        assert!(h.session_homes.get("term").is_none(), "setup");
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::Terminal { session: "term".into(), home: Some("elsewhere".into()) },
            },
        );

        assert_eq!(
            terminal_tabs(&h),
            vec![("term".to_string(), None)],
            "still exactly one tab, and it must not have taken the forged home at face value"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// R16 point 1's placement rule: a resident with no native tab at all
    /// to retarget lands in `RIGHT` — the pane `NewTerminal`/`focusSession`
    /// use for a terminal with no home of its own — not `LEFT_TOP`, R15's
    /// placement.
    ///
    /// Revert-checked: changing the push's `pane` back to
    /// `crate::proto::LEFT_TOP` fails — observed: `assertion 'left ==
    /// right' failed\n  left: Some(Terminal { session: "term", home: None
    /// })\n right: Some(Terminal { session: "term3", home:
    /// Some("r16-right-pane-home") })` — `RIGHT`'s last tab is back to
    /// `default_layout`'s own placeholder, because the resident landed in
    /// `LEFT_TOP` instead.
    #[test]
    fn a_resident_tab_is_added_to_the_right_pane() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "r16-right-pane-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        // A name distinct from the owner's own default-layout placeholder
        // ("term"), so there is no native tab here at all to retarget —
        // the case this test is actually about.
        crate::session::reserve_and_attach(home, "term3", &home_dir).unwrap();
        crate::home::record(home, "term3", &owner).unwrap();

        let owner_hub = Hub::new(&owner, owner_dir);

        assert_eq!(
            owner_hub.ws.panes[proto::RIGHT as usize].tabs.last(),
            Some(&Tab::Terminal { session: "term3".to_string(), home: Some(home.to_string()) }),
            "a resident with no native tab must land in RIGHT, the pane NewTerminal/focusSession use"
        );
        assert!(
            !owner_hub.ws.panes[proto::LEFT_TOP as usize]
                .tabs
                .iter()
                .any(|t| matches!(t, Tab::Terminal { .. })),
            "and must not land in LEFT_TOP, R15's placement"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn a_mutation_reaches_every_subscriber() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
    /// while roost was down comes back looking clean and current, and the
    /// first warning is a conflict banner at save time.
    #[test]
    fn a_restart_notices_a_file_that_changed_while_it_was_down() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::set_var("ROOST_STATE_DIR", sd.path());

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

        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        crate::notify::reset_for_test();
        crate::notify::record("proj", "claude", crate::osc::Parsed { title: None, body: "one".into() });
        crate::notify::record("proj", "shell", crate::osc::Parsed { title: None, body: "two".into() });
        // A third project's notice, so this test also pins that the button
        // is scoped: "Mark all read" sits in a panel showing one project and
        // cannot mean rows the user is not being shown.
        crate::notify::record("other", "shell", crate::osc::Parsed { title: None, body: "not mine".into() });

        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        assert!(!h.notices_dirty);
        h.handle(&c, Intent::MarkAllNoticesRead);
        assert!(h.notices_dirty, "the socket layer relies on this flag to broadcast Notices");
        let all = crate::notify::list();
        assert!(
            all.iter().filter(|n| n.project == "proj").all(|n| n.read),
            "the store itself must be updated, not just the dirty flag: {all:?}"
        );
        assert!(
            all.iter().filter(|n| n.project == "other").all(|n| !n.read),
            "another project's unread notices must survive this button: {all:?}"
        );
    }

    // Two registered hubs with a subscriber each, deliberately: `publish`
    // used to call `broadcast_all`, so with a single hub the scoped and
    // unscoped versions are indistinguishable and this passes either way —
    // CLAUDE.md's "single subscriber" trap. B's receiver is what carries the
    // whole claim.
    //
    // A browser tab is opened on one project key and a worktree is a key of
    // its own, so `roost` and `roost/.claude/worktrees/claude-1` here are as
    // separate as two unrelated projects; that pair is the case the panel
    // actually gets wrong, not two strangers.
    //
    // Watched fail with `publish` restored to `broadcast_all`: B's receiver
    // gets the Notice and the `is_err()` assertion below fires.
    #[test]
    fn a_notice_reaches_its_own_projects_clients_and_nobody_elses() {
        isolate_ide_dir_for_tests();
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        crate::notify::reset_for_test();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let hub_a = Hub::for_project("notice-scope-a", dir_a.path().to_path_buf());
        let hub_b =
            Hub::for_project("notice-scope-a/.claude/worktrees/w1", dir_b.path().to_path_buf());
        let (_ca, rxa) = Hub::lock(&hub_a).subscribe();
        let (_cb, rxb) = Hub::lock(&hub_b).subscribe();
        drain(&rxa);
        drain(&rxb);

        publish(
            "notice-scope-a",
            "term1",
            crate::osc::Parsed { title: None, body: "for a only".into() },
        );

        // A's client must actually receive it, or the "nobody else" half
        // below would also hold for a publish that reached no one at all.
        let got = rxa
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the notice's own project must receive it");
        assert!(got.contains(r#""t":"Notice""#), "expected a Notice, got {got}");
        assert!(got.contains("for a only"), "wrong notice: {got}");

        // Not merely "no Notice": nothing at all should arrive, and a
        // timeout is the only outcome that proves the broadcast never
        // reached this hub.
        let leaked = rxb.recv_timeout(std::time::Duration::from_millis(300));
        assert!(
            leaked.is_err(),
            "a worktree's tab must not be sent its parent project's notice, got {leaked:?}"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `publish` runs on a session's pump thread, where the `project` it is
    /// handed is always the session's *home* (the pump has no notion of
    /// re-homing at all). A re-homed session's own tab lives in the
    /// *owner*'s workspace now, so a notice raised there must be routed to
    /// the owner hub, not the home's — the same "nobody else" shape as
    /// `a_notice_reaches_its_own_projects_clients_and_nobody_elses` above,
    /// but for the re-homing case rather than plain project scoping. Two
    /// real hubs with a subscriber each, deliberately (CLAUDE.md's
    /// single-subscriber trap): with one subscriber, routing to the home
    /// and broadcasting to everyone are indistinguishable.
    ///
    /// `ROOST_ROOTS` and the real `.claude/worktrees/w1` directory are what
    /// let the production, fork-free `home::resolves` (not an injected
    /// stub) confirm the owner before `publish` routes to it — without
    /// them the record would be real but unresolved, and the notice would
    /// fall back to home, proving nothing about the routing this test is
    /// for.
    ///
    /// Revert-checked: dropping the `effective_owner` remap in `publish`
    /// (routing to `project` — the home — unconditionally) fails the first
    /// assertion — observed: `panicked ... the owner's project must
    /// receive it: Timeout` — because the notice lands at home instead
    /// (confirmed separately that the home *does* receive it in that case).
    #[test]
    fn a_rehomed_sessions_notice_reaches_its_owner_not_its_home() {
        isolate_ide_dir_for_tests();
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::env::set_var("ROOST_ROOTS", d.path());
        crate::notify::reset_for_test();

        let home = "notice-rehome-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = d.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        let hub_home = Hub::for_project(home, home_dir.clone());
        let hub_owner = Hub::for_project(&owner, owner_dir.clone());
        let (_ca, rxa) = Hub::lock(&hub_home).subscribe();
        let (_cb, rxb) = Hub::lock(&hub_owner).subscribe();
        drain(&rxa);
        drain(&rxb);

        crate::home::record(home, "term3", &owner).unwrap();

        publish(home, "term3", crate::osc::Parsed { title: None, body: "for the owner".into() });

        let got = rxb
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the owner's project must receive it");
        assert!(got.contains(r#""t":"Notice""#), "expected a Notice, got {got}");
        assert!(got.contains("for the owner"), "wrong notice: {got}");

        let leaked = rxa.recv_timeout(std::time::Duration::from_millis(300));
        assert!(
            leaked.is_err(),
            "a re-homed session's notice must not also land at home, got {leaked:?}"
        );

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The sibling test above proves *live* delivery; this proves the
    /// *persisted* store agrees. `notify::record` still stored
    /// `Notice.project` as the raw `project` argument until this fix —
    /// `list_for`, which every reload and the mark-all-read/clear buttons
    /// go through, filters on that field. So a browser that reloads the
    /// owner's tab (rather than staying subscribed across the `publish`
    /// call) would see no notice at all, and the home's mark-all-read could
    /// never reach it either.
    ///
    /// Revert-checked: reverting `publish` to call
    /// `notify::record(project, …)` (the home, not `target`) fails the
    /// first assertion — observed: `panicked ... []` — the notice is
    /// filed under the home instead, invisible to `list_for(&owner)`.
    #[test]
    fn a_rehomed_sessions_notice_persists_under_its_owner_not_its_home() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::env::set_var("ROOST_ROOTS", d.path());
        crate::notify::reset_for_test();

        let home = "notice-persist-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = d.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        std::fs::create_dir_all(home_dir.join(".claude/worktrees/w1")).unwrap();

        crate::home::record(home, "term3", &owner).unwrap();

        publish(home, "term3", crate::osc::Parsed { title: None, body: "persisted for owner".into() });

        let owner_notices = crate::notify::list_for(&owner);
        assert!(
            owner_notices.iter().any(|n| n.body.contains("persisted for owner")),
            "the notice must be filed under the owner: {owner_notices:?}"
        );
        let home_notices = crate::notify::list_for(home);
        assert!(home_notices.is_empty(), "and not under the home: {home_notices:?}");

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drop(rx_b);
        h.handle(&a, Intent::Resize { sizes: proto::Sizes::default() });
        assert_eq!(h.subs.len(), 1, "a closed socket must not accumulate");
        drop(rx_a);
    }

    /// `do_fileop` is the shared tail of CreateFile, CreateDir and DeleteFile,
    /// and its two halves go to different audiences: a success is a tree change
    /// every client must see, a refusal is the asking client's own business.
    ///
    /// Two subscribers, deliberately: with one, `broadcast` and `send_to` are
    /// indistinguishable, so a refusal broadcast to everyone — an error banner
    /// on a tab whose user did nothing — would pass a single-client test.
    #[test]
    fn a_fileop_broadcasts_success_and_tells_only_the_asker_about_a_refusal() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let proj = d.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let mut h = Hub::new("fileop_audience", proj.clone());
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&b, Intent::CreateFile { rel: "notes.txt".into() });
        assert!(proj.join("notes.txt").is_file(), "the file itself was not created");
        for (who, msgs) in [("the other client", drain(&rx_a)), ("the asking client", drain(&rx_b))] {
            assert!(
                msgs.iter().any(|m| m.contains("TreeChanged")),
                "{who} must see the tree change; got {msgs:?}"
            );
        }

        // The same path again, which `must_not_exist` refuses.
        h.handle(&b, Intent::CreateFile { rel: "notes.txt".into() });
        let (to_a, to_b) = (drain(&rx_a), drain(&rx_b));
        // On the reason, not merely on "an error happened": a refusal that
        // named the wrong path, or said nothing useful, is the failure this
        // function exists to avoid.
        assert!(
            to_b.iter().any(|m| m.contains("already exists") && m.contains("notes.txt")),
            "the asker must be told why; got {to_b:?}"
        );
        assert!(
            to_a.is_empty(),
            "a client that did nothing must hear nothing about someone else's refusal; got {to_a:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Two subscribers, deliberately: with one, `broadcast` and `send_to`
    /// are indistinguishable, and a setting that reached only the saving
    /// browser would look right in every single-client test.
    ///
    /// Revert-checked twice: dropping the `if key == "show_hidden"` block in
    /// `handle` fails the override assertion (`left: Some(false), right:
    /// None`); changing the arm's final `self.broadcast(&snap)` to
    /// `self.send_to(from, &snap)` fails "the other client must see the new
    /// effective value; got []". Both restored.
    #[test]
    fn set_setting_writes_the_project_file_and_every_client_sees_the_new_value() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::env::set_var("ROOST_CONFIG", d.path().join("global.toml"));
        let proj = d.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let mut h = Hub::new("set_setting", proj.clone());
        h.ws.show_hidden = Some(false); // the header toggle's override
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);
        let before = h.ws.version;

        h.handle(&b, Intent::SetSetting {
            scope: crate::proto::Scope::Project,
            key: "show_hidden".into(),
            value: Some(crate::proto::SettingValue::Bool(true)),
        });

        let file = std::fs::read_to_string(proj.join(".roost/config.toml")).unwrap();
        assert!(file.contains("show_hidden = true"), "{file}");
        assert_eq!(h.ws.show_hidden, None, "writing the file clears the toggle's override");
        assert!(h.ws.version > before);
        for (who, msgs) in [("the other client", drain(&rx_a)), ("the saving client", drain(&rx_b))] {
            assert!(
                msgs.iter().any(|m| m.contains(r#""key":"show_hidden""#) && m.contains(r#""effective":true"#)),
                "{who} must see the new effective value; got {msgs:?}"
            );
        }

        // A refused write: error to the sender only, file untouched.
        h.handle(&b, Intent::SetSetting {
            scope: crate::proto::Scope::Project,
            key: "allowed_origins".into(),
            value: Some(crate::proto::SettingValue::List(vec!["https://evil".into()])),
        });
        let to_b = drain(&rx_b);
        assert!(to_b.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("allowed_origins")), "{to_b:?}");
        assert!(drain(&rx_a).is_empty(), "a refusal is not broadcast");
        assert!(!std::fs::read_to_string(proj.join(".roost/config.toml")).unwrap().contains("evil"));
        std::env::remove_var("ROOST_CONFIG");
    }

    /// `Hub::settings` exists for the same reason `claude_hooks` does: reading
    /// it fresh on every snapshot would mean the ~20 file reads inside
    /// `settings_view` running on every debounced `EditBuffer`.
    ///
    /// Revert-check: with `self.settings` removed and `snapshot_event` calling
    /// `crate::config::settings_view(&self.dir)` directly on every snapshot
    /// (no cache), the first assertion below fails — the hand-edited `nord`
    /// shows up immediately, on the `SetShowHidden` snapshot, instead of only
    /// after the next invalidation. Restored.
    #[test]
    fn settings_in_the_snapshot_are_cached_until_invalidated() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::env::set_var("ROOST_CONFIG", d.path().join("global.toml"));
        let proj = d.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let mut h = Hub::new("settings_cache", proj.clone());
        let (a, rx_a) = h.subscribe();
        drain(&rx_a);

        // First RequestState: populates the cache with the default theme —
        // no project config file exists yet.
        h.handle(&a, Intent::RequestState);
        let first = drain(&rx_a);
        assert!(
            first.iter().any(|m| m.contains(r#""key":"theme""#) && m.contains(r#""effective":"darcula""#)),
            "{first:?}"
        );

        // A hand edit, bypassing `SetSetting` entirely — the same kind of
        // out-of-band change `claude_hooks`'s doc comment says is not
        // reflected sooner than the next invalidation.
        std::fs::create_dir_all(proj.join(".roost")).unwrap();
        std::fs::write(proj.join(".roost/config.toml"), "theme = \"nord\"\n").unwrap();

        // SetShowHidden snapshots (every layout intent does) but is not one
        // of `settings`'s invalidation sites — it goes through `apply_layout`
        // and the generic `_ => {}` arm, never touching `self.settings`.
        h.handle(&a, Intent::SetShowHidden { on: true });
        let stale = drain(&rx_a);
        assert!(
            stale.iter().any(|m| m.contains(r#""key":"theme""#) && m.contains(r#""effective":"darcula""#)),
            "a snapshot that does not invalidate must still report the cached value; got {stale:?}"
        );
        assert!(
            !stale.iter().any(|m| m.contains(r#""effective":"nord""#)),
            "the hand edit must not appear before an invalidation; got {stale:?}"
        );

        // RequestState is one of the invalidation sites: the hand edit must
        // now show.
        h.handle(&a, Intent::RequestState);
        let fresh = drain(&rx_a);
        assert!(
            fresh.iter().any(|m| m.contains(r#""key":"theme""#) && m.contains(r#""effective":"nord""#)),
            "{fresh:?}"
        );
        std::env::remove_var("ROOST_CONFIG");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("newterm_names", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // default_layout seeds RIGHT with a Terminal tab; clear it so the
        // names under test are the only ones in play.
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        drain(&rx);

        let names: Vec<String> = h.ws.panes[proto::RIGHT as usize]
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Terminal { session, .. } => Some(session.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["term", "term1"], "each click must get its own shell");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A resident's name is taken in its owner, though the owner's own
    /// `sock/` has no such socket — two tabs called `term` on one page.
    ///
    /// Revert-checked (Task 4): without the `session_homes` chain in
    /// `do_new_terminal`'s `also_taken`, this allocates `["term"]`.
    #[test]
    fn new_terminal_in_an_owner_skips_a_residents_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("owner_names/.claude/worktrees/worktree-x", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        // What `refresh_live_sessions` computes from a re-home record; set
        // directly because resolving the owner reads the configured roots.
        h.session_homes.insert("term".into(), "owner_names".into());
        drain(&rx);

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        drain(&rx);

        let names: Vec<String> = h.ws.panes[proto::RIGHT as usize]
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Terminal { session, .. } => Some(session.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["term1"], "the resident already shows as `term` on this page");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The ✻ button is the + button plus a program to type in. The hub only
    /// parks the request against the name it allocates; the shell it reaches
    /// is the one `session::attach` spawns for that name later.
    #[test]
    fn a_claude_terminal_parks_its_launch_on_the_name_it_was_given() {
        // STATE first, SESSION second — the order session.rs's lock comment fixes.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("newterm_launch", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        drain(&rx);

        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false, resume: None, fork: None });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        drain(&rx);
        // ✻ is allocated `claude`, not whichever `termN` was free: a strip
        // reading `term, term1, term2` says nothing about which tab has an
        // agent in it. The plain + that followed keeps `term`.
        let first = crate::session::reserve_and_attach("newterm_launch", "claude", d.path()).unwrap();
        assert_eq!(
            first.launch.as_ref().map(|l| l.launch),
            Some(proto::Launch::Claude),
            "✻ got `claude`, so `claude` starts claude"
        );
        assert!(
            first.launch.as_ref().and_then(|l| l.session.as_ref()).is_some_and(|s| {
                matches!(s, crate::launch::ClaudeSession::Fresh(id) if crate::launch::valid_session_id(id))
            }),
            "the hub minted an id"
        );
        let second = crate::session::reserve_and_attach("newterm_launch", "term", d.path()).unwrap();
        assert_eq!(second.launch, None, "+ got `term`, which stays a plain shell");
        crate::session::kill_project("newterm_launch");

        // The stale case: ✻ allocates `claude2` (the first `claude` is taken),
        // its tab is closed before any browser attaches, and a later ✻ is
        // handed `claude2` back. The click that made it a claude shell is
        // gone, so its own reservation must not survive the close.
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false, resume: None, fork: None });
        let idx = h.ws.panes[proto::RIGHT as usize]
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Terminal { session, .. } if session == "claude2"))
            .expect("✻ was handed claude2");
        h.handle(&c, Intent::CloseTab { pane: proto::RIGHT, idx });
        h.handle(&c, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        drain(&rx);
        // The plain + that followed took `term1`; `claude2` is free again, and
        // attaching it directly is what a browser that never saw the close
        // would do.
        let reused = crate::session::reserve_and_attach("newterm_launch", "claude2", d.path()).unwrap();
        assert_eq!(reused.launch, None, "a reallocated name must not inherit the old click");
        crate::session::kill_project("newterm_launch");
        std::env::remove_var("ROOST_CMD");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn a_second_claude_click_gets_a_prompt_that_only_the_clicker_sees() {
        // Revert-checked: with the evidence check removed, `a` receives
        // State + TerminalStarted and the ClaudeHere assertion fails;
        // with `send_to` swapped for `broadcast`, the `b` assertion fails.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("prompt_second", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        let (_b, rxb) = h.subscribe();
        for p in h.ws.panes.iter_mut() { p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. })); p.active = 0; }
        // First ✻: allocates `claude`; spawn it the way a browser would, so
        // the launch is consumed and recorded on the session.
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false, resume: None, fork: None });
        let _att = crate::session::reserve_and_attach("prompt_second", "claude", d.path()).unwrap();
        assert_eq!(crate::session::launched_names("prompt_second").len(), 1, "fixture: a launched terminal exists");
        drain(&rxa); drain(&rxb);
        let version = h.ws.version;

        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false, resume: None, fork: None });
        let got = rxa.try_recv().expect("the clicker hears back");
        // `claude`, not `term`: a ✻ click is allocated a name that says what
        // the terminal is for.
        assert!(got.contains(r#""t":"ClaudeHere""#) && got.contains(r#""terminals":["claude"]"#), "{got}");
        assert!(rxb.try_recv().is_err(), "nobody else hears anything");
        assert_eq!(h.ws.version, version, "no layout change");
        assert_eq!(crate::session::live_names("prompt_second").len(), 1, "no session allocated");

        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: true, resume: None, fork: None });
        assert!(h.ws.version > version, "force opens a terminal");
        assert!(rxb.try_recv().is_ok_and(|m| m.contains(r#""t":"State""#)), "…which everyone sees");
        crate::session::kill_project("prompt_second");
    }

    #[test]
    // Revert-checked: dropping the `launch == Some(Claude)` clause in
    // `do_new_terminal` fails this at the "a plain shell opens beside a
    // Claude" assertion (src/hub.rs:2734) — a `+` click gets prompted too.
    fn a_plus_click_never_prompts() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("prompt_plus", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        for p in h.ws.panes.iter_mut() { p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. })); p.active = 0; }
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false, resume: None, fork: None });
        let _att = crate::session::reserve_and_attach("prompt_plus", "term", d.path()).unwrap();
        drain(&rxa);
        let version = h.ws.version;
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false, resume: None, fork: None });
        assert!(h.ws.version > version, "a plain shell opens beside a Claude");
        crate::session::kill_project("prompt_plus");
    }

    /// Ending a session is what makes closing a tab reclaim a slot; the tab
    /// must go from every pane, and every mirrored browser must be told.
    #[test]
    fn end_session_clears_the_tab_everywhere_and_broadcasts() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("endsession_hub", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_c2, rx_other) = h.subscribe();
        for p in h.ws.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::Terminal { session: "term".into(), home: None } },
        );
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::RIGHT, tab: Tab::Terminal { session: "term".into(), home: None } },
        );
        drain(&rx);
        drain(&rx_other);

        h.handle(&c, Intent::EndSession { session: "term".into() });

        let remaining: Vec<&Tab> = h.ws.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            !remaining.iter().any(|t| matches!(t, Tab::Terminal { session, .. } if session == "term")),
            "the ended session must not keep a tab in any pane"
        );
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(
            msgs_other.iter().any(|m| m.contains(r#""t":"State""#)),
            "a second browser must be told its terminal tab is gone"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Minimal, test-only "is anything holding this path" check via `ps` —
    /// the same shape as `session.rs`'s own private helper of the same
    /// name, duplicated rather than exposed: both exist only to prove a real
    /// process is or isn't there for one test's own known, plain tempdir
    /// path, and a shared `pub(crate)` version would widen a process-table
    /// scrape into the crate's real interface for no caller that needs it.
    fn any_process_holds(path: &std::path::Path) -> bool {
        let target = path.to_string_lossy();
        let out = std::process::Command::new("ps")
            .args(["-Ao", "args="])
            .output()
            .expect("ps must be runnable for this check to mean anything");
        assert!(
            out.status.success() && !out.stdout.is_empty(),
            "ps failed or returned nothing; this test cannot trust its own assertions right now"
        );
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(target.as_ref()))
    }

    /// Carried from Task 4: before this, `do_end_session` always ended
    /// `self.project`/`session` — for a resident that kills nothing at all
    /// (the owner's own `sock/` has no socket by that name) while still
    /// reporting success, leaving the real shell at `home`, and any Claude
    /// in it, running untouched forever.
    ///
    /// Real dtach, not `ROOST_CMD=cat`: a `cat` client has no detached
    /// master to leave behind, so the exact bug this guards against —
    /// ending the client without ending the master — cannot reproduce under
    /// the substitute at all (see `session::
    /// kill_project_ends_the_detached_dtach_master_and_shell_not_just_the_client`,
    /// which exists for the identical reason). `/usr/bin/dtach` is a runtime
    /// prerequisite elsewhere in this project, so a failure to attach here
    /// is a real environment defect worth failing loudly on, not a reason
    /// to skip.
    ///
    /// `Hub::for_project`, not a bare `Hub::new()`: `do_end_session`'s
    /// thread-spawning path only runs once `self_ref` resolves (see its own
    /// doc comment), and that threaded path — kill off the hub lock, then
    /// re-lock to broadcast — is the one this test exists to exercise.
    #[test]
    fn end_session_on_a_resident_ends_the_real_session_at_its_home() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "endresident_home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        let att = crate::session::reserve_and_attach(home, "term3", &home_dir)
            .expect("/usr/bin/dtach is a runtime prerequisite elsewhere in this project");
        let sock = crate::session::socket_path(home, "term3");
        // dtach's own fork-and-detach takes an unpredictable, usually-small
        // amount of wall time to complete — the same poll
        // `kill_project_ends_the_detached_dtach_master_...` uses, for the
        // same reason.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !any_process_holds(&sock) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(any_process_holds(&sock), "setup: the real dtach master must be up before this test can mean anything");
        drop(att);

        crate::home::record(home, "term3", &owner).unwrap();

        let owner_hub = Hub::for_project(&owner, owner_dir.clone());
        assert_eq!(
            Hub::lock(&owner_hub).session_homes.get("term3"),
            Some(&home.to_string()),
            "setup: the owner hub must see term3 as a resident"
        );
        let (c, _rx) = Hub::lock(&owner_hub).subscribe();

        Hub::lock(&owner_hub).handle(&c, Intent::EndSession { session: "term3".into() });

        // The kill runs on its own thread, off the hub lock (see
        // `do_end_session`'s doc comment) — give it a moment to actually
        // finish before checking what it did.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while any_process_holds(&sock) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            !any_process_holds(&sock),
            "ending a resident from the owner's page must kill the real session at its home"
        );
        assert!(
            matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here),
            "the re-home record must be forgotten once the session actually ends"
        );

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The half of a close the user actually sees afterwards. `kill_project`
    /// really does end the shells (see `close_project_..._reports_what_it_ended`),
    /// but the tabs pointing at them lived on in the layout — and the layout
    /// is persisted, so reopening the closed project re-rendered a terminal
    /// tab per dead session, on every reload, forever.
    ///
    /// Asserts on the reloaded-from-disk workspace, not just `h.ws`: the
    /// in-memory half alone would pass with `persist()` missing, which is
    /// exactly the "even after reload" the report was about.
    #[test]
    fn close_project_clears_the_terminal_tabs_of_the_sessions_it_ended() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "disk\n").unwrap();
        let mut h = Hub::new("closetabs", d.path().to_path_buf());
        let (c, _rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::Terminal { session: "term1".into(), home: None } },
        );
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.txt".into(), mode: crate::proto::Mode::Edit },
            },
        );
        let before: Vec<&Tab> = h.ws.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            before.iter().filter(|t| matches!(t, Tab::Terminal { .. })).count() >= 2,
            "test setup: there must be terminal tabs to lose: {before:?}"
        );

        h.handle(&c, Intent::CloseProject);

        let live: Vec<&Tab> = h.ws.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            !live.iter().any(|t| matches!(t, Tab::Terminal { .. })),
            "a closed project may keep no terminal tab: {live:?}"
        );
        // What a reload actually reads. `load` returns the same workspace the
        // next `Hub::new` for this project would start from.
        let (reloaded, warn) = crate::wsstate::load("closetabs");
        assert!(warn.is_none(), "the state file must have been written cleanly: {warn:?}");
        let saved: Vec<&Tab> = reloaded.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            !saved.iter().any(|t| matches!(t, Tab::Terminal { .. })),
            "reopening a closed project must not bring its dead terminals back: {saved:?}"
        );
        // Closing ends shells; it does not close the files being worked on.
        // Without this, wiping the layout outright would pass.
        assert!(
            saved.iter().any(|t| matches!(t, Tab::File { rel, .. } if rel == "a.txt")),
            "an open file tab must survive the close: {saved:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn close_project_is_refused_while_a_buffer_is_dirty() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn close_project_with_clean_buffers_reports_what_it_ended() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_CMD"); // real dtach: a `cat` client has no master to wait on
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project_dir = tempfile::tempdir().unwrap();

        let hub = Hub::for_project("closepromptly", project_dir.path().to_path_buf());
        let (c, _rx) = Hub::lock(&hub).subscribe();

        let Ok(_att) = crate::session::reserve_and_attach("closepromptly", "shell", project_dir.path()) else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("ROOST_STATE_DIR");
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

        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // The update intents exist before their wsconn divert does, and any page
    // `Origin` admits can send one, so reaching `handle` must be a refusal to
    // the sender, not a panic that kills the socket thread. Two connections,
    // so a `broadcast` of the refusal would show up on the bystander.
    //
    // Watched fail with the arm put back to `unreachable!`: the test panicked
    // with "update intents are diverted in wsconn before this lock is taken".
    #[test]
    fn an_update_intent_reaching_the_hub_is_refused_to_its_sender_only() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let dir = tempfile::tempdir().unwrap();
        let hub = Hub::for_project("update-intent-refused", dir.path().to_path_buf());
        let (asker, rx) = Hub::lock(&hub).subscribe();
        let (_other, rx_other) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}

        for intent in [Intent::Update, Intent::DeferUpdate, Intent::SkipUpdate { version: "9.9.9".into() }] {
            let label = format!("{intent:?}");
            Hub::lock(&hub).handle(&asker, intent);
            let m = rx.try_recv().unwrap_or_else(|_| panic!("{label}: the sender must be told"));
            assert!(m.contains(r#""t":"Error""#), "{label}: got {m}");
            assert!(m.contains("update intents are handled before the hub"), "{label}: got {m}");
            assert!(rx.try_recv().is_err(), "{label}: exactly one reply, no state push");
            assert!(rx_other.try_recv().is_err(), "{label}: the bystander hears nothing");
        }
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // An update choice is process-wide, so every project's open page has to
    // hear it at once — and through the settings *cache*, which is the trap:
    // each hub below is primed with a snapshot taken before the choice, so a
    // broadcast that merely re-sent `snapshot_event` would re-send the old
    // cached view and every assertion here would time out.
    //
    // Run on its own thread and waited on with a timeout, because the
    // failure this function is most exposed to is a lock-order deadlock,
    // and a deadlock hangs rather than fails.
    //
    // Watched fail with `g.settings = Some(fresh)` removed (the cache primed
    // before the choice is re-sent): A's receiver timed out on "skipped".
    #[test]
    fn an_update_choice_reaches_every_projects_clients_past_the_settings_cache() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "version_check = false\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &cfg);
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let hub_a = Hub::for_project("update-choice-a", dir_a.path().to_path_buf());
        let hub_b = Hub::for_project("update-choice-b", dir_b.path().to_path_buf());
        let (_ca, rxa) = Hub::lock(&hub_a).subscribe();
        let (_cb, rxb) = Hub::lock(&hub_b).subscribe();
        for h in [&hub_a, &hub_b] {
            let mut g = Hub::lock(h);
            let _ = g.snapshot_event(&String::new());
            assert!(g.settings.as_ref().is_some_and(|v| v.update.skipped.is_empty()), "primed before the choice");
        }
        while rxa.try_recv().is_ok() {}
        while rxb.try_recv().is_ok() {}

        let choice = crate::update::Choices { skipped: Some("7.7.7".into()), deferred_until: None };
        crate::update::write_choices_to(&crate::update::choices_path(), &choice).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            broadcast_settings_all();
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("broadcast_settings_all must return, not deadlock");

        // Filtered by content, because `broadcast_settings_all` reaches every
        // hub in the registry, including concurrently running tests' hubs,
        // whose snapshots would otherwise satisfy a bare "got a State".
        for (name, rx) in [("A", &rxa), ("B", &rxb)] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let m = rx.recv_timeout(left).unwrap_or_else(|_| panic!("{name}'s clients must hear the skip"));
                if m.contains(r#""t":"State""#) && m.contains(r#""skipped":"7.7.7""#) {
                    break;
                }
            }
        }
        std::env::remove_var("ROOST_CONFIG");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // The other way a project leaves the roster: its shell exits (`exit`,
    // ctrl-d, or its dtach master dying) with no intent ever asking. The
    // only place that notices is the PTY pump as it winds down, so that is
    // where the nudge has to come from; nothing in `handle` runs. `ROOST_CMD=
    // true` is a shell that exits on its own the instant it starts, without
    // dtach — which is the point: the socket-file side is `registry`'s job,
    // this pins that the *event* is sent at all. Watched fail with the pump's
    // `broadcast_all` removed: B's receiver timed out.
    #[test]
    fn a_shell_exiting_on_its_own_tells_every_project_the_roster_changed() {
        isolate_ide_dir_for_tests();
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "true");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        // Only B has a hub: the project whose shell exits need not have one
        // (its tab can be long gone), and the nudge must still reach the others.
        let hub_b = Hub::for_project("roster-exit-b", dir_b.path().to_path_buf());
        let (_cb, rxb) = Hub::lock(&hub_b).subscribe();
        while rxb.try_recv().is_ok() {}

        let att = crate::session::reserve_and_attach("roster-exit", "shell", dir_a.path())
            .expect("attach with ROOST_CMD=true spawns `true`");

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

        std::env::remove_var("ROOST_CMD");
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_CMD"); // real dtach: needs a kill that takes real wall time
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project_dir = tempfile::tempdir().unwrap();

        let hub = Hub::for_project("closeinflight", project_dir.path().to_path_buf());
        let (c, rx) = Hub::lock(&hub).subscribe();

        let Ok(_att) = crate::session::reserve_and_attach("closeinflight", "shell", project_dir.path()) else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("ROOST_STATE_DIR");
            return;
        };
        while rx.try_recv().is_ok() {}

        Hub::lock(&hub).handle(&c, Intent::CloseProject);
        // What `term.rs` used to check here — `Hub::is_closing` — is gone: it
        // took the *hub* mutex while the spawn it guarded is serialised by
        // the *session* mutex, so it ordered nothing. The connect side is now
        // asserted where it is deterministic, in `session`'s
        // `a_close_refuses_every_attach_until_it_finishes` and
        // `an_attach_that_wins_the_lock_is_still_seen_by_the_close`. Asserting
        // it here needed the close to still be in flight at the moment of the
        // call, which is a race: with the second sweep gone the close now
        // finishes first often enough that this observed `NotReserved`
        // instead — a correct refusal, for the wrong reason to assert on.
        //
        // What remains here is the *intent* half, which is the hub's own job
        // and is not timing-dependent.
        Hub::lock(&hub).handle(&c, Intent::StartTerminal { session: "fresh".into(), resume: false });

        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("closing")),
            "StartTerminal must be refused while a close is in flight; got: {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)),
            "a refused StartTerminal must not also announce the tab as started; got: {msgs:?}"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn start_terminal_rejects_an_invalid_session_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("startproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // A second, mirrored client must never see this client's own Error.
        let (_other, rx_other) = h.subscribe();
        while rx.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}
        h.handle(&c, Intent::StartTerminal { session: "bad name;rm".into(), resume: false });
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let msgs_other: Vec<String> = std::iter::from_fn(|| rx_other.try_recv().ok()).collect();
        assert!(msgs.iter().any(|m| m.contains(r#""t":"Error""#)));
        assert!(!msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)));
        assert!(
            !msgs_other.iter().any(|m| m.contains(r#""t":"Error""#)),
            "an invalid session name is the requester's business, not every mirrored browser's"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Ruling 5 (carried from Task 4): `StartTerminal` restarts a lost tab
    /// under its old name, which is the *other* way a browser can ask for a
    /// name already in play — `do_new_terminal`'s `also_taken` only guards
    /// the fresh-click path. Without this guard a name a resident holds on
    /// this page would pass straight to `session::reserve`, and the
    /// browser's own follow-up connect to `/ws/{project}/term/{session}`
    /// would create a brand-new, unrelated shell under the name a Claude at
    /// the resident's home is still running in.
    ///
    /// `session_homes` is set directly, the same as
    /// `new_terminal_in_an_owner_skips_a_residents_name` above: resolving a
    /// real re-home record reads the configured project roots, which this
    /// test does not need to set up just to exercise the guard.
    ///
    /// Revert-checked: deleting the guard lets this reach `TerminalStarted`
    /// instead — observed: `assertion failed:
    /// msgs.iter().any(|m| m.contains("\"t\":\"Error\"") && m.contains("moved here from"))`.
    #[test]
    fn start_terminal_refuses_a_name_held_by_a_resident() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("startrefuse_owner", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);
        h.session_homes.insert("term3".into(), "startrefuse_home".into());

        h.handle(&c, Intent::StartTerminal { session: "term3".into(), resume: false });

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("moved here from")),
            "{msgs:?}"
        );
        assert!(!msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)));
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn init_git_creates_a_repo_and_flips_is_git_for_every_client() {
        // Entirely unverified before this test: the handler could be
        // deleted and every other test would still pass. GitInit and the
        // is_git flip are exactly what the terminal/tree placeholder in the
        // client depends on to know a repo now exists.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }


    /// A rename roost did not perform. The tab has to move with the file, and
    /// unsaved work has to arrive at the new name — app.js keys its editor
    /// text by rel, so a tab that changes rel with nothing sent for the new one
    /// mounts an empty textarea over the user's edit, which is the failure this
    /// whole line of work began with.
    #[test]
    fn an_external_rename_moves_the_tab_and_its_unsaved_work() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("old.rs"), "fn one() {}\n").unwrap();
        let mut h = Hub::new("renameproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "old.rs".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "old.rs".into(), text: "unsaved work\n".into() });
        drain(&rx);

        std::fs::rename(d.path().join("old.rs"), d.path().join("new.rs")).unwrap();
        assert!(h.follow_rename("old.rs", "new.rs"), "something referenced the old path");

        let tab = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .find_map(|t| match t {
                Tab::File { rel, mode } => Some((rel.clone(), *mode)),
                _ => None,
            })
            .expect("the tab is still open");
        assert_eq!(tab.0, "new.rs", "the tab addresses where the file went");
        assert_eq!(tab.1, Mode::Edit, "and stays in the mode it was in — nothing here is unreadable");
        assert!(!h.ws.buffers.contains_key("old.rs"), "nothing is left under the old key");
        assert_eq!(
            h.ws.buffers.get("new.rs").and_then(|b| b.edited_text()),
            Some("unsaved work\n"),
            "the unsaved edit moves with the file"
        );

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"State""#)),
            "the browser has to be told the tab moved; got {msgs:?}"
        );
        // Presence, deliberately not order. The obvious worry is that app.js
        // prunes its `texts` map against every State's buffer list, so a
        // BufferText arriving first would be deleted by the State behind it —
        // but the State that moves the tab lists the buffer under the *new*
        // rel, so the prune keeps it. Checked rather than assumed: sending the
        // text first leaves `renamed.mjs` fully green, so an ordering assertion
        // here would only be a trap for whoever refactors this next.
        assert!(
            msgs.iter()
                .any(|m| m.contains(r#""t":"BufferText""#) && m.contains("unsaved work")),
            "the text has to reach the browser under the new rel; got {msgs:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Every atomic save is a rename (`fileops::atomic_write`) and so is most
    /// of what git does to its index. Following one that nothing has open must
    /// not touch the workspace at all — a version bump per temp file would
    /// persist the layout and wake every browser several times a second during
    /// a build.
    #[test]
    fn a_rename_nobody_has_open_leaves_the_workspace_alone() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("quietrename", d.path().to_path_buf());
        let (_c, rx) = h.subscribe();
        drain(&rx);
        let version_before = h.ws.version;

        assert!(!h.follow_rename(".tmp.1234", "notes.rs"), "nothing referenced the old path");

        assert_eq!(h.ws.version, version_before, "an unrelated rename is not a workspace change");
        let msgs = drain(&rx);
        assert!(msgs.is_empty(), "and nothing is broadcast for it; got {msgs:?}");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A dirty buffer is flagged stale when the file underneath it changes,
    /// which is what pauses autosave and raises the banner. But "changed" has
    /// to mean *diverged*: a rename fires an event for the destination while
    /// leaving the bytes identical, and flagging that would tell the user their
    /// file had been rewritten underneath them by the act of moving it.
    #[test]
    fn a_file_still_matching_the_buffers_base_has_not_diverged_from_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "on disk\n").unwrap();
        let mut h = Hub::new("nodiverge", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.rs".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "a.rs".into(), text: "mine\n".into() });
        drain(&rx);

        // Same bytes the buffer was opened against, touched by something else.
        assert!(h.file_changed_externally(d.path(), "a.rs"));
        assert!(
            !h.ws.buffers["a.rs"].stale,
            "the file matches this buffer's base exactly; a save would not even conflict"
        );
        // The assertion that catches the tempting way to write the guard. As
        // `&&` on the existing condition, a non-divergent event falls into the
        // clean-buffer branch, which resets the buffer and broadcasts the disk
        // text — silently discarding the edit while leaving `stale` false, so
        // the check above passes.
        assert_eq!(
            h.ws.buffers["a.rs"].edited_text(),
            Some("mine\n"),
            "and the unsaved edit is still there — not quietly replaced by the disk text"
        );

        // The control: a real divergence must still be caught, or the assertion
        // above is just testing that staleness never happens.
        std::fs::write(d.path().join("a.rs"), "somebody else\n").unwrap();
        assert!(h.file_changed_externally(d.path(), "a.rs"));
        assert!(h.ws.buffers["a.rs"].stale, "different bytes on disk is exactly what stale is for");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A genuinely deleted file is the third outcome and must still say so:
    /// the watcher turns `false` into the tree refresh that drops the row.
    #[test]
    fn a_deleted_file_still_reports_false() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("gone.md"), "here\n").unwrap();
        let mut h = Hub::new("delproj", d.path().to_path_buf());
        std::fs::remove_file(d.path().join("gone.md")).unwrap();
        assert!(!h.file_changed_externally(d.path(), "gone.md"));
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// …and reporting `false` is not all it owes. A file that vanished under
    /// an open Edit tab leaves that tab addressing a path with nothing behind
    /// it: `mountEditor` seeds its textarea from the `BufferText` the server
    /// pushes, no push arrives for a file that cannot be read, and the editor
    /// mounts *empty* — over a file that was not empty, wearing the old name
    /// and a "saved" label. That is what an external `mv` looks like from the
    /// browser, and it is the exact state `open_buffer_for` refuses to create
    /// on the open path ("always demote"); the watcher path just never applied
    /// the same rule.
    ///
    /// Preview is honest here where Edit cannot be: it fetches its own
    /// fragment and renders the server's "not found" instead of a blank
    /// editor.
    #[test]
    fn a_vanished_file_does_not_leave_an_edit_tab_over_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("gone.rs"), "fn one() {}\n").unwrap();
        let mut h = Hub::new("vanishproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "gone.rs".into(), mode: Mode::Edit },
            },
        );
        assert!(h.ws.buffers.contains_key("gone.rs"), "opening in Edit reads the file into a buffer");
        drain(&rx);

        std::fs::remove_file(d.path().join("gone.rs")).unwrap();
        assert!(!h.file_changed_externally(d.path(), "gone.rs"), "gone is still gone");

        let mode = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .find_map(|t| match t {
                Tab::File { rel, mode } if rel == "gone.rs" => Some(*mode),
                _ => None,
            })
            .expect("the tab itself stays — closing it would throw away the user's layout");
        assert_eq!(mode, Mode::Preview, "an Edit tab over a file that is not there paints an empty editor");
        assert!(
            !h.ws.buffers.contains_key("gone.rs"),
            "and a clean buffer for a file that no longer exists holds nothing worth keeping — \
             every later reconnect would re-read it and fail"
        );
        // Without this the browser only learns on its next reload, which is
        // where the empty editor was actually seen.
        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"State""#)),
            "the demotion has to reach the browser that is looking at it; got {msgs:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The other half, and the one that must not follow the rule above: a
    /// *dirty* buffer is the only copy of that text in the world once the file
    /// is gone. Demoting it to Preview would hide the user's unsaved work
    /// behind a "not found" page, and dropping the buffer would delete it
    /// outright. Mark it stale instead — which is what pauses autosave
    /// client-side, so nothing tries to write it back to a path that vanished.
    #[test]
    fn a_vanished_file_keeps_unsaved_work_and_says_so() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("gone.rs"), "fn one() {}\n").unwrap();
        let mut h = Hub::new("vanishdirty", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(
            &c,
            Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "gone.rs".into(), mode: Mode::Edit },
            },
        );
        h.handle(&c, Intent::EditBuffer { rel: "gone.rs".into(), text: "unsaved work\n".into() });
        drain(&rx);

        std::fs::remove_file(d.path().join("gone.rs")).unwrap();
        assert!(!h.file_changed_externally(d.path(), "gone.rs"));

        let b = h.ws.buffers.get("gone.rs").expect("unsaved work must survive the file's deletion");
        assert_eq!(b.edited_text(), Some("unsaved work\n"), "and survive it unaltered");
        assert!(b.stale, "the buffer no longer matches anything on disk, and must say so");
        let mode = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .find_map(|t| match t {
                Tab::File { rel, mode } if rel == "gone.rs" => Some(*mode),
                _ => None,
            })
            .expect("the tab stays");
        assert_eq!(mode, Mode::Edit, "Preview would hide the only copy of the user's text");
        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"BufferStale""#)),
            "autosave pauses on BufferStale; without it the client keeps writing to a dead path; got {msgs:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // These four tests reach `apply_layout`'s `Ok(true)` branch (opening or
    // reusing a tab), which calls `self.persist()`. Every other test in this
    // file that persists scopes `ROOST_STATE_DIR` under `STATE_ENV_LOCK` first
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Confinement runs before the notification ever reaches `ide::mention_to`:
    /// a path that escapes the project must be refused by `safe_resolve`
    /// itself, not silently forwarded — there being no listening Claude in
    /// this test would otherwise make an escape indistinguishable from an
    /// ordinary "resolved fine, nobody's connected" refusal.
    ///
    /// Revert-checked: swapping the arm order so `ide::mention_to` runs first
    /// against the raw `rel` (bypassing `safe_resolve` entirely) failed this
    /// test — the refusal's message became `"no Claude is connected to this
    /// project"`, tripping the `!m.contains("no Claude")` assertion, i.e. the
    /// escaping path silently made it past confinement. Restored.
    #[test]
    fn mention_path_outside_the_project_is_refused_before_reaching_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Finding 6: an Alt+K aimed at a re-homed terminal, sent from the
    /// *owner*'s hub (where that terminal's tab actually lives now). The
    /// Claude it should reach is registered under the terminal's *home* —
    /// `self.project` here is the owner, which has no `conns()` entry for
    /// it at all — so `do_mention_path` must look the session up in
    /// `self.session_homes` and route there instead.
    ///
    /// `register_fake_conn_for_test` stands in for a real IDE connection;
    /// `term9` is an unrelated session under the same home, registered to
    /// prove this is a *routed* mention (one session picked out), not a
    /// broadcast that happens to work.
    ///
    /// Revert-checked: reverting `target_project` to always be
    /// `self.project.clone()` (the pre-fix-round body) fails — observed:
    /// `panicked ... the resident's own Claude must receive the mention:
    /// Timeout`. (The mention is sent to `conns()[owner]`, which has no
    /// entry at all, so `ide::mention_to` returns "no Claude is connected
    /// to this project" and the refusal reaches `rx_asker` instead.)
    #[test]
    fn mention_from_a_rehomed_terminal_reaches_its_claude_at_home() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "mention-rehome-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::write(owner_dir.join("a.rs"), "fn main() {}").unwrap();

        // `session_homes` only lists a resident whose terminal is actually
        // live (`owned_live_names`) — a real dtach session at the home, not
        // just the re-home record, the same discipline
        // `remove_refuses_a_worktree_with_a_live_resident` follows.
        crate::session::reserve_and_attach(home, "term3", &home_dir).unwrap();
        crate::home::record(home, "term3", &owner).unwrap();

        // The resident's Claude is registered at its home, never the owner.
        let rx_home = crate::ide::register_fake_conn_for_test(home, "term3");
        // An unrelated session under the same home: proof this is routed,
        // not broadcast.
        let rx_other = crate::ide::register_fake_conn_for_test(home, "term9");

        let mut h = Hub::new(&owner, owner_dir.clone());
        assert_eq!(
            h.session_homes.get("term3"),
            Some(&home.to_string()),
            "setup: the owner hub must see term3 as a resident"
        );
        let (asker, rx_asker) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: None,
            line_end: None,
            session: Some("term3".into()),
        });

        let got = rx_home
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the resident's own Claude must receive the mention");
        assert!(got.contains("at_mentioned"), "{got}");
        assert!(rx_other.try_recv().is_err(), "an unrelated session's Claude must not receive it");
        assert!(
            rx_asker.try_iter().collect::<Vec<_>>().is_empty(),
            "no refusal, since the mention actually reached someone"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Ruling R13a: a resident's Alt+K must not fall back across projects.
    /// `notify_selected`'s two ambiguity heuristics — matching
    /// `Sess::Unknown`, and a lone connection winning regardless of its
    /// session — are right when a project asks about its own connections
    /// (an `Unknown` or a single Claude there could plausibly be the one
    /// meant). Routed to a resident's *home* instead, they are wrong: an
    /// `Unknown`/lone connection there is some other terminal entirely, and
    /// term3 (the resident) has no connection at all in this fixture. The
    /// mention must reach nobody, not that unrelated connection, and the
    /// asker must see the same "no Claude" refusal `do_mention_path`
    /// already gives for an ordinary (non-re-homed) miss.
    ///
    /// Revert-checked: calling `ide::mention_to` (which goes through
    /// `notify_selected`, the a48248c body) instead of `mention_to_exact`
    /// for the resident case fails the first assertion — observed:
    /// `panicked ... the lone Unknown connection must not receive a
    /// mention aimed at a different session` (the lone-connection fallback
    /// delivers to it, since it is the only connection under the home —
    /// confirmed separately that `rx_unknown.recv_timeout` succeeds there).
    #[test]
    fn an_aimed_mention_to_a_resident_does_not_fall_back_to_an_unrelated_home_connection() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());

        let home = "mention-fallback-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::write(owner_dir.join("a.rs"), "fn main() {}").unwrap();

        crate::session::reserve_and_attach(home, "term3", &home_dir).unwrap();
        crate::home::record(home, "term3", &owner).unwrap();

        // No connection at all for term3; the home's only connection is one
        // roost could not place, the single-project heuristic this test
        // exists to refuse.
        let rx_unknown = crate::ide::register_fake_unknown_conn_for_test(home);

        let mut h = Hub::new(&owner, owner_dir.clone());
        assert_eq!(h.session_homes.get("term3"), Some(&home.to_string()), "setup");
        let (asker, rx_asker) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: None,
            line_end: None,
            session: Some("term3".into()),
        });

        assert!(
            rx_unknown.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the lone Unknown connection must not receive a mention aimed at a different session"
        );
        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(got.iter().any(|m| m.contains(r#""t":"Error""#)), "the asker must see a refusal: {got:?}");

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Finding 6's other half: a shared selection is unaimed — every
    /// connected Claude gets it — so from the owner's hub it must reach not
    /// only any native connection under the owner itself but every
    /// *resident's* connection too, each at its own home.
    ///
    /// Also pins the fix-round-2 half of finding 6: `notify_all(home)` (the
    /// a48248c body) sends to *every* `Target` under the home — the home's
    /// own native terminals, and any other resident re-homed into a
    /// *different* worktree — not just this owner's own resident. `term9`
    /// (a native home session) and `term5` (a resident of a different
    /// owner, `w2`) must receive nothing.
    ///
    /// Revert-checked: removing the `self.session_homes.values()` loop from
    /// `do_share_selection` (the pre-fix-round body, which only ever calls
    /// `ide::selection_changed(&self.project, ...)`) fails — observed:
    /// `panicked ... the resident's Claude must receive the shared
    /// selection: Timeout`.
    ///
    /// Revert-checked again (fix round 2, and this is the real RED a48248c
    /// itself produces): calling `ide::selection_changed(home, ...)`
    /// (`notify_all`, a48248c's actual body) instead of the session-
    /// filtered `selection_changed_for_sessions` fails the `term9`
    /// assertion — observed: `panicked ... term9 (a native home session)
    /// must not receive a re-homed selection` (the `recv_timeout` on
    /// `rx_native` succeeds instead of timing out: `notify_all` sent to
    /// every `Target` under the home, term9 included).
    #[test]
    fn a_shared_selection_from_the_owner_also_reaches_its_residents_claude() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        let cfg = root.path().join("global.toml");
        std::fs::write(&cfg, "share_selection = true\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &cfg);

        let home = "shareselection-rehome-home";
        let owner = format!("{home}/.claude/worktrees/w1");
        let owner2 = format!("{home}/.claude/worktrees/w2");
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::write(owner_dir.join("a.rs"), "fn main() {}").unwrap();

        crate::session::reserve_and_attach(home, "term3", &home_dir).unwrap();
        crate::home::record(home, "term3", &owner).unwrap();
        let rx_home = crate::ide::register_fake_conn_for_test(home, "term3");
        // A native home session — never re-homed at all.
        let rx_native = crate::ide::register_fake_conn_for_test(home, "term9");
        // A resident of a *different* owner under the same home.
        crate::home::record(home, "term5", &owner2).unwrap();
        let rx_other_owner = crate::ide::register_fake_conn_for_test(home, "term5");

        let mut h = Hub::new(&owner, owner_dir.clone());
        assert_eq!(h.session_homes.get("term3"), Some(&home.to_string()), "setup");
        assert!(h.session_homes.get("term9").is_none(), "setup: term9 is native, not a resident of this owner");
        assert!(h.session_homes.get("term5").is_none(), "setup: term5 belongs to owner2, not this owner");
        let (asker, _rx_asker) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "a.rs".into(),
            text: "fn main".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 7,
        });

        let got = rx_home
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the resident's Claude must receive the shared selection");
        assert!(got.contains("selection_changed"), "{got}");
        assert!(
            rx_native.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "term9 (a native home session) must not receive a re-homed selection"
        );
        assert!(
            rx_other_owner.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "term5 (a different owner's resident) must not receive it either"
        );

        crate::session::kill_project(home);
        std::env::remove_var("ROOST_CONFIG");
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A half-specified line range must be refused by name, not silently
    /// turned into a whole-file mention (the old `Option::zip` behavior) or
    /// a guessed single line. The refusal message is checked specifically —
    /// not just `contains("Error")` — because "no Claude is connected"
    /// (from `ide::mention_to`, reached only if this check is skipped) would
    /// also satisfy a looser assertion, and this test exists to prove the
    /// rejection happens *before* `ide::mention_to`, on this exact input shape.
    ///
    /// Revert-checked: reverting the match arm back to
    /// `let lines = line_start.zip(line_end);` failed this test — the
    /// asker's inbox held only `{"t":"Error","msg":"no Claude is connected
    /// to this project"}` (from `ide::mention_to`, since `zip` silently turned
    /// the half-specified range into `None`), so `got.iter().any(|m|
    /// m.contains("line_start") ...)` was false — then restored.
    #[test]
    fn mention_path_with_a_half_specified_line_range_is_refused_not_guessed() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A session name off the wire is refused before it reaches `ide`, and
    /// the refusal goes only to the client that asked — the same rule the
    /// sibling path-confinement test pins. Two subscribers, because with one
    /// `send_to` and `broadcast` are indistinguishable.
    #[test]
    fn an_invalid_session_name_is_refused_and_only_the_asker_hears() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        // The project root sits INSIDE a parent temp dir, with a real file
        // beside it. `../secret.txt` therefore always names something that
        // exists and is always outside the project — where the old
        // `../../etc/passwd` only reached a real file when the temp dir
        // happened to sit two levels above `/etc`, so `safe_resolve` returned
        // `not found` and the confinement arm this test exists for was never
        // executed. Same shape as `projects.rs`'s
        // `terminal_path_refuses_a_real_file_outside_the_project`.
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("shareselectionescape", root);
        let (asker, rx) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../secret.txt".into(),
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("shareselectionrefuse", root);
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../secret.txt".into(),
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", dir.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // --- Task 7: proposal tabs ---

    /// A `Hub` with a private state directory, for the proposal tests below.
    /// They all persist on some path or other, and none of them should write
    /// into this host's real state directory.
    /// Points `ROOST_STATE_DIR` at the shared stable test directory rather
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `openDiff` asks this before parking anything: accepting a proposal for
    /// a file the user has unsaved edits to would discard them silently.
    ///
    /// The snapshot is the only path by which the browser learns which tabs
    /// are running a Claude, so this covers the wiring, not the detection
    /// (`claudes.rs` owns that). Asserts on the *names*, not just that the
    /// list is non-empty: marking the wrong tab is the failure that matters,
    /// and a length check could not see it.
    ///
    /// Revert-checked: dropping the `ws.claude_sessions = ...` line from
    /// `snapshot_event` failed here with
    /// `left: [] right: ["term7"]`, then restored.
    #[test]
    fn a_snapshot_names_the_terminals_running_a_claude() {
        let _s = crate::claudes::SCAN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        isolate_ide_dir_for_tests();

        // Seeded before the hub exists, so `tick`'s broadcast finds no hub to
        // wake and this tests the snapshot path alone.
        let proc_root = d.path().join("proc");
        crate::claudes::fake_proc(&proc_root, &[(100, "claude-tab-snap", "term7")]);
        crate::claudes::tick(&proc_root);

        let hub = Hub::for_project("claude-tab-snap", d.path().to_path_buf());
        let ev = Hub::lock(&hub).snapshot_event(&String::new());
        let crate::proto::Event::State { ws, .. } = ev else { panic!("not a State event") };
        assert_eq!(ws.claude_sessions, vec!["term7".to_string()]);

        std::fs::remove_dir_all(proc_root.join("100")).unwrap();
        crate::claudes::tick(&proc_root); // leave the shared cache empty
    }

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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
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
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
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
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn new_worktree_creates_the_named_worktree_announces_it_and_answers_the_clicker() {
        // Revert-checked: with `WorktreeReady` sent via `broadcast` instead
        // of `send_to`, the `rxb` assertion below fails ("the reply is the
        // clicker's alone") — confirmed, then restored.
        //
        // `broadcast_all` reaches hubs registered in the global map, which a
        // bare `Hub::new` (used here, as `do_end_session`'s convention
        // requires for the synchronous branch) never is — so this test
        // cannot observe the `ProjectsChanged` broadcast itself; that is
        // left to the browser test.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let mut h = Hub::new("repo", repo.clone());
        let (a, rxa) = h.subscribe();
        let (_b, rxb) = h.subscribe();
        drain(&rxa); drain(&rxb);
        h.handle(&a, Intent::NewWorktree { name: Some("fox".into()), launch: Some(WorktreeLaunch::Claude) });
        // Hub::new has no self_ref, so the work ran synchronously (the
        // do_end_session convention) and the reply is already queued.
        let got = rxa.try_recv().expect("clicker answered");
        assert!(got.contains(r#""t":"WorktreeReady""#) && got.contains(r#""url":"repo/.claude/worktrees/worktree-fox""#) && got.contains(r#""launch":"claude""#), "{got}");
        assert!(rxb.try_recv().is_err(), "the reply is the clicker's alone");
        assert!(repo.join(".claude/worktrees/worktree-fox/a.txt").is_file());
        assert_eq!(crate::worktree::read_base(&crate::wsstate::state_dir(), "repo%2F.claude%2Fworktrees%2Fworktree-fox").as_deref(), Some("main"));
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn new_worktree_in_a_non_repository_is_an_error_to_the_clicker() {
        // Revert-checked: swallowing the `Err` branch in `finish` instead of
        // sending an `Error` event fails this test's `unwrap()` with
        // `Err(Empty)` (nothing was ever sent) — confirmed, then restored.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("plain", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        drain(&rxa);
        h.handle(&a, Intent::NewWorktree { name: None, launch: None });
        let got = rxa.try_recv().unwrap();
        assert!(got.contains(r#""t":"Error""#) && got.contains("not a git repository"), "{got}");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// An empty name is the dialog's "let roost pick one" — `wtname::generate`
    /// with `taken` asking git/the filesystem the same two things
    /// `create_named` itself checks, so a name this loop would have rejected
    /// is never even offered to it.
    ///
    /// Revert-checked: making the `None` arm `return Err("no name given".into())`
    /// before ever calling `wtname::generate` makes this fail — observed:
    /// `panicked ... {"t":"Error","msg":"no name given"}` (the assertion's own
    /// message is `got`, so the panic prints the reply, not an expression).
    /// Restored.
    #[test]
    fn new_worktree_with_no_name_generates_one() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let mut h = Hub::new("repo", repo.clone());
        let (a, rxa) = h.subscribe();
        drain(&rxa);
        h.handle(&a, Intent::NewWorktree { name: None, launch: None });
        let got = rxa.try_recv().expect("clicker answered");
        assert!(got.contains(r#""t":"WorktreeReady""#), "{got}");
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let url = v["url"].as_str().expect("a url").to_string();
        let generated = url.rsplit('/').next().unwrap();
        assert!(generated.starts_with("worktree-") && generated.len() > "worktree-".len(), "{generated}");
        assert!(crate::wtname::valid(generated.trim_start_matches("worktree-")), "{generated}");
        assert!(repo.join(".claude/worktrees").join(generated).is_dir(), "the generated name's worktree must actually exist");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `WorktreeLaunch::Fork` resolves its session id from `claudesess`
    /// before any git work runs, and echoes the resolved **id**, never the
    /// terminal name the dialog offered — R4.
    #[test]
    fn new_worktree_forks_a_recorded_session_and_echoes_its_id() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";
        crate::claudesess::record(
            "repo",
            "term",
            &crate::claudesess::Recorded { session_id: id.into(), transcript_path: None, event: "Stop".into() },
        );
        let mut h = Hub::new("repo", repo.clone());
        let (a, rxa) = h.subscribe();
        drain(&rxa);
        h.handle(&a, Intent::NewWorktree {
            name: Some("fox".into()),
            launch: Some(WorktreeLaunch::Fork { session: "term".into() }),
        });
        let got = rxa.try_recv().expect("clicker answered");
        assert!(
            got.contains(r#""t":"WorktreeReady""#)
                && got.contains(r#""launch":"claude""#)
                && got.contains(&format!(r#""fork":"{id}""#)),
            "{got}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `Fork`'s whole safety property: a terminal name offered by a stale
    /// dialog, or simply typed by hand, must not create a worktree and must
    /// not type anything — it must refuse, by name, before any git work runs.
    ///
    /// Revert-checked: dropping the `claudesess::recorded` lookup (treating
    /// every `Fork` as if a session were always found, with a fabricated
    /// id) makes this fail at the `"t":"Error"` assertion — confirmed with
    /// `got` instead containing a `"t":"WorktreeReady"` for a worktree that
    /// should never have been created. Restored.
    #[test]
    fn fork_without_a_recorded_session_is_refused() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", tmp.path().join("state"));
        let (mut h, _home, _dir) = repo_with_worktree(tmp.path());
        let (a, rx) = conn(&mut h);
        h.handle(&a, Intent::NewWorktree {
            name: Some("fox".into()),
            launch: Some(WorktreeLaunch::Fork { session: "term9".into() }),
        });
        let got = rx.try_recv().expect("an answer");
        assert!(
            got.contains(r#""t":"Error""#) && got.contains("term9") && got.contains("no recorded Claude session"),
            "{got}"
        );
        // Nothing was created: the only worktree in this repo is the
        // fixture's own.
        assert_eq!(crate::worktree::list(&h.dir).len(), 2, "no second worktree");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 1 — the hub-side half of it. The hook that
    /// records a Claude session (`roost claude-hook`, `claudesess::record_from_hook`)
    /// runs as a separate subprocess and writes straight to disk; it tells
    /// the hub nothing when it does, so `Hub::forkable` — rebuilt only by
    /// `refresh_live_sessions` — stays stale until something asks for the
    /// truth again. `RequestState` is that something (same reasoning its
    /// own doc comment gives for `claude_hooks`/`worktree_hooks`): this
    /// pins that it actually recomputes `forkable` too, which is what the
    /// client's `openWorktreeDialog`'s own `requestFreshState()` leans on.
    ///
    /// Revert-checked: commenting out `RequestState`'s own
    /// `self.refresh_live_sessions()` call makes the second assertion fail
    /// — observed: `panicked ... assertion failed: got.contains("\"forkable\":[\"term\"]")`,
    /// with the second reply still carrying `"forkable":[]` — confirming
    /// this test was already green *because* that call exists, not by
    /// accident. Restored.
    #[test]
    fn request_state_recomputes_forkable_after_a_hook_writes_its_record() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let project = "forkable-fresh";
        let dir = d.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        crate::session::reserve_and_attach(project, "term", &dir).unwrap();
        let mut h = Hub::new(project, dir);
        let (a, rx) = conn(&mut h);
        h.handle(&a, Intent::RequestState);
        let got = rx.try_recv().expect("an answer");
        assert!(got.contains(r#""forkable":[]"#), "no record yet: {got}");
        // The hook writes its record now, exactly as the real `roost
        // claude-hook` subprocess does — off every hub lock, telling this
        // hub nothing directly.
        crate::claudesess::record(project, "term", &crate::claudesess::Recorded {
            session_id: "864ee734-e3ab-434e-8278-745a850b16ad".into(),
            transcript_path: None,
            event: "Stop".into(),
        });
        h.handle(&a, Intent::RequestState);
        let got = rx.try_recv().expect("a second answer");
        assert!(got.contains(r#""forkable":["term"]"#), "{got}");
        crate::session::kill_project(project);
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 3. `forkable` must name a native live session
    /// with a record, exclude a native live session without one, and
    /// exclude a *resident* re-homed in even if a record exists under this
    /// project's own name — forking means continuing a conversation of a
    /// terminal on this page, and a resident's own record is forkable from
    /// its home's refresh instead (see `Hub::forkable`'s doc comment).
    ///
    /// The third case is deliberately adversarial: a resident's claudesess
    /// record naturally lives under its true home (`ROOST_PROJECT` is set
    /// at the shell's spawn, which never moves), so it could never
    /// naturally collide with this project's own name. Writing it there
    /// anyway is what actually distinguishes "`forkable` is built from
    /// `raw_live`" from "`forkable` is built from `self.ws.live_sessions`"
    /// — `self.ws.live_sessions` folds a resident in as live
    /// (`owned_live_names`), so if `forkable` ever read that list instead,
    /// this exact record would leak through; `raw_live` structurally never
    /// contains a resident's name at all, record or no record.
    ///
    /// Revert-checked: changing `refresh_live_sessions`'s `self.forkable`
    /// computation from `raw_live.iter()...` to
    /// `self.ws.live_sessions.iter()...` makes the first assertion fail —
    /// observed: `assertion failed: forkable == ["recorded"]`, left:
    /// `["recorded", "term3"]`. Restored.
    #[test]
    fn forkable_lists_recorded_native_live_sessions_only() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        // `home::resolves` only recognises the worktree shape
        // (`{home}/.claude/worktrees/{name}`) as a real owner — it is not a
        // general cross-project relationship — so `owner` has to be a
        // worktree-shaped child of `home`, not an unrelated project, or the
        // resident below is never folded into `owner`'s own `live_sessions`
        // at all and this test would prove nothing.
        let home = "forkable-home";
        let owner_url = format!("{home}/.claude/worktrees/w1");
        let owner = owner_url.as_str();
        let home_dir = root.path().join(home);
        let owner_dir = home_dir.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&owner_dir).unwrap();

        crate::session::reserve_and_attach(owner, "recorded", &owner_dir).unwrap();
        crate::session::reserve_and_attach(owner, "plain", &owner_dir).unwrap();
        crate::claudesess::record(owner, "recorded", &crate::claudesess::Recorded {
            session_id: "864ee734-e3ab-434e-8278-745a850b16ad".into(),
            transcript_path: None,
            event: "Stop".into(),
        });
        // Native to `home`, re-homed into `owner` — `owner`'s own
        // `live_sessions` folds it in as live; see the adversarial-record
        // note above for why the record is written under `owner` here.
        crate::session::reserve_and_attach(home, "term3", &home_dir).unwrap();
        crate::home::record(home, "term3", owner).unwrap();
        crate::claudesess::record(owner, "term3", &crate::claudesess::Recorded {
            session_id: "aaaa1111-2222-3333-4444-555555555555".into(),
            transcript_path: None,
            event: "Stop".into(),
        });

        let mut h = Hub::new(owner, owner_dir);
        let (a, rx) = conn(&mut h);
        h.handle(&a, Intent::RequestState);
        let snap = rx.try_recv().expect("an answer");
        let v: serde_json::Value = serde_json::from_str(&snap).unwrap();
        let forkable: Vec<String> = v["ws"]["forkable"]
            .as_array().expect("forkable is an array")
            .iter().map(|x| x.as_str().unwrap().to_string()).collect();
        assert_eq!(forkable, vec!["recorded".to_string()], "{snap}");
        // Setup check: if `term3` were not actually live on the owner's own
        // page, excluding it from `forkable` would prove nothing.
        assert!(
            v["ws"]["live_sessions"].as_array().unwrap().iter().any(|x| x == "term3"),
            "setup: term3 must be live on the owner's page: {snap}"
        );

        crate::session::kill_project(owner);
        crate::session::kill_project(home);
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `Hub::new` (used by the two tests above) never has a `self_ref`, so
    /// they never exercise the threaded branch's own hub re-lock at all —
    /// this is the one test in the file that goes through `for_project`
    /// and therefore a hub that really is in `REGISTRY`. That distinction
    /// matters: `broadcast_all` locks every registered hub, including this
    /// one, so calling it while still holding *this* hub's own lock (taken
    /// in the spawned thread to deliver the reply) is a guaranteed
    /// self-deadlock on `std::sync::Mutex`, which is not reentrant.
    ///
    /// Caught in self-review, not by the brief's own tests: moving
    /// `broadcast_all` back inside the locked section (the shape the task
    /// brief's literal code used) reproduces it — `recv_timeout` times out
    /// instead of ever receiving `WorktreeReady`. Confirmed, then restored
    /// to the fixed ordering (`broadcast_all` only after the lock guard is
    /// dropped, the same ordering `do_end_session` already uses for its own
    /// `broadcast_all` call).
    #[test]
    fn new_worktree_on_a_registered_hub_does_not_deadlock_broadcasting_projects_changed() {
        isolate_ide_dir_for_tests();
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let hub = Hub::for_project("worktree-registered", repo.clone());
        let (a, rxa) = Hub::lock(&hub).subscribe();
        drain(&rxa);
        Hub::lock(&hub).handle(&a, Intent::NewWorktree { name: None, launch: None });
        let got = rxa.recv_timeout(std::time::Duration::from_secs(5));
        assert!(got.is_ok(), "timed out waiting for WorktreeReady -- broadcast_all deadlocked against this hub's own lock");
        assert!(got.unwrap().contains(r#""t":"WorktreeReady""#));
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A repo with one roost-created worktree, both registered under a temp
    /// state dir; returns (hub for the repo, worktree url, worktree dir).
    fn repo_with_worktree(root: &std::path::Path) -> (Hub, String, std::path::PathBuf) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let key_of = |n: &str| crate::projects::storage_key(&format!("repo/.claude/worktrees/{n}"));
        let c = crate::worktree::create_named(&repo, &crate::wsstate::state_dir(), "pier", &key_of, &crate::worktree::real_git).unwrap();
        (Hub::new("repo", repo), "repo/.claude/worktrees/worktree-pier".into(), c.path)
    }
    const WT_KEY: &str = "repo%2F.claude%2Fworktrees%2Fworktree-pier";

    fn refusal_of(h: &mut Hub, a: &ConnId, rx: &Receiver<String>) -> String {
        h.handle(a, Intent::RemoveWorktree { key: WT_KEY.into() });
        let got = rx.try_recv().expect("an answer");
        assert!(got.contains(r#""t":"Error""#), "expected a refusal, got {got}");
        got
    }

    // ---- Task 9: the worktree ask ----------------------------------------

    /// Puts a bare `Hub::new` into `REGISTRY` with its `self_ref` set, the
    /// two things `for_project` does that the ask path depends on — without
    /// the watcher thread and IDE listener `for_project` also starts, which
    /// these tests neither need nor want left running.
    fn register_for_test(hub: Hub) -> Arc<Mutex<Hub>> {
        let project = hub.project.clone();
        let arc = Arc::new_cyclic(|w| {
            let mut hub = hub;
            hub.self_ref = w.clone();
            Mutex::new(hub)
        });
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        reg.lock().unwrap_or_else(|e| e.into_inner()).insert(project, arc.clone());
        arc
    }

    fn unregister_for_test(project: &str) {
        if let Some(reg) = REGISTRY.get() {
            reg.lock().unwrap_or_else(|e| e.into_inner()).remove(project);
        }
    }

    fn init_repo(repo: &std::path::Path) {
        std::fs::create_dir_all(repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(repo).args(args).status().unwrap().success());
        }
    }

    /// A registered hub for `root/project` (a plain directory) with `session`
    /// live in it and nobody connected. The caller holds both env locks and
    /// has pointed `ROOST_ROOTS`/`ROOST_STATE_DIR` at `root`.
    fn registered_hub_with_session(root: &std::path::Path, project: &str, session: &str) -> Arc<Mutex<Hub>> {
        let dir = root.join(project);
        std::fs::create_dir_all(&dir).unwrap();
        crate::session::reserve_and_attach(project, session, &dir).unwrap();
        register_for_test(Hub::new(project, dir))
    }

    /// The same, in a real git repository; also returns the repository's
    /// canonical path, which is what `create_named` reports back.
    fn registered_hub_in_git_repo_with_session(
        root: &std::path::Path,
        project: &str,
        session: &str,
    ) -> (Arc<Mutex<Hub>>, std::path::PathBuf) {
        let repo = root.join(project);
        init_repo(&repo);
        crate::session::reserve_and_attach(project, session, &repo).unwrap();
        let hub = register_for_test(Hub::new(project, repo.clone()));
        (hub, repo.canonicalize().unwrap())
    }

    fn req(project: &str, session: &str, name: &str) -> crate::hooksock::AskRequest {
        crate::hooksock::AskRequest {
            op: "worktree_create".into(),
            project: project.into(),
            session: session.into(),
            name: name.into(),
            cwd: String::new(),
            session_id: "sid-1".into(),
        }
    }

    fn conn(h: &mut Hub) -> (ConnId, Receiver<String>) {
        let (id, rx) = h.subscribe();
        drain(&rx);
        (id, rx)
    }

    /// Waits (≤5 s) for a `WorktreeAsk` on `rx` and returns its id. Every
    /// other message is discarded.
    fn wait_for_ask(rx: &Receiver<String>) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let m = rx.recv_timeout(left).expect("no WorktreeAsk within 5 s");
            let v: serde_json::Value = serde_json::from_str(&m).unwrap();
            if v["t"] == "WorktreeAsk" {
                return v["id"].as_str().unwrap().to_string();
            }
        }
    }

    fn ask_env(root: &std::path::Path) {
        std::env::set_var("ROOST_CMD", "cat");
        std::env::set_var("ROOST_STATE_DIR", root.join("state"));
        std::env::set_var("ROOST_ROOTS", root);
    }

    fn ask_env_clear(projects: &[&str]) {
        for p in projects {
            crate::session::kill_project(p);
            unregister_for_test(p);
        }
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    fn spawn_ask(project: &str, session: &str, name: &str) -> std::thread::JoinHandle<crate::hooksock::AskReply> {
        let r = req(project, session, name);
        std::thread::spawn(move || ask_worktree_blocking(r))
    }

    use crate::hooksock::AskReply;

    #[test]
    fn an_ask_with_no_browser_is_refused_at_once() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-nobrowser";
        let _hub = registered_hub_with_session(root.path(), p, "term3");
        let r = ask_worktree_blocking(req(p, "term3", "fox"));
        assert_eq!(r, AskReply::Refuse { refuse: "no roost tab is open on this project to approve it".into() });
        // And a real project with no hub at all gets the same answer — the
        // hub is never created by asking.
        std::fs::create_dir_all(root.path().join("ask-nobrowser-nohub")).unwrap();
        let r = ask_worktree_blocking(req("ask-nobrowser-nohub", "term3", "fox"));
        assert_eq!(r, AskReply::Refuse { refuse: "no roost tab is open on this project to approve it".into() });
        assert!(!is_open("ask-nobrowser-nohub"), "asking must never create a hub");
        ask_env_clear(&[p]);
    }

    #[test]
    fn approve_creates_records_and_replies_path() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-approve";
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let (_b, rxb) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id: id.clone(), approve: true });
        let r = t.join().unwrap();
        let owner = format!("{p}/.claude/worktrees/worktree-fox");
        assert_eq!(r, AskReply::Path { path: repo.join(".claude/worktrees/worktree-fox").to_string_lossy().into() });
        assert!(
            matches!(crate::home::owner_of(p, "term3"), crate::home::Owner::Elsewhere(o) if o == owner),
            "the re-home record names the new worktree"
        );
        // The clicker's `pendingTab` is navigated by `WorktreeReady`, which
        // is delivered before the `Path` reply returns — so it is queued by
        // now, and only the clicker has it.
        let got_a = drain(&rxa);
        assert!(
            got_a.iter().any(|m| m.contains(r#""t":"WorktreeReady""#) && m.contains(&format!(r#""url":"{owner}""#))),
            "the clicker must get WorktreeReady for {owner}: {got_a:?}"
        );
        let got_b = drain(&rxb);
        assert!(!got_b.iter().any(|m| m.contains("WorktreeReady")), "WorktreeReady is the clicker's alone: {got_b:?}");
        assert!(
            got_b.iter().any(|m| m.contains(r#""t":"WorktreeAskClosed""#) && m.contains(&id)),
            "the other browser's dialog is closed: {got_b:?}"
        );
        ask_env_clear(&[p, &owner]);
    }

    /// Task 10: when the main checkout has `Kind::Worktree` installed, a
    /// freshly approved worktree inherits it — its own
    /// `.claude/settings.local.json` gets the `WorktreeCreate` entry, not
    /// the main checkout's file, which must stay untouched.
    ///
    /// Verified this can fail: commenting out the install call in
    /// `approve_ask` (the Task 10 marker) fails the first assertion with
    /// `No such file or directory (os error 2)` reading the new worktree's
    /// settings file — nothing is installed there at all. Restored.
    #[test]
    fn approve_installs_the_worktree_hook_when_the_main_checkout_has_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-hook-on";
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        crate::claudehooks::set_kind(&repo, crate::claudehooks::Kind::Worktree, true).unwrap();
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        let r = t.join().unwrap();
        let AskReply::Path { path } = r else { panic!("expected Path, got {r:?}") };
        let owner = format!("{p}/.claude/worktrees/worktree-fox");
        let wt_settings = std::path::Path::new(&path).join(".claude/settings.local.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&wt_settings).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"], "roost worktree-hook",
            "the new worktree's own settings file carries the hook: {v}"
        );
        assert_eq!(
            crate::claudehooks::state_of(&repo, crate::claudehooks::Kind::Worktree),
            crate::claudehooks::HookState::Present,
            "installing into the worktree must not disturb the main checkout's own file"
        );
        ask_env_clear(&[p, &owner]);
    }

    /// The other half: a main checkout that never enabled `Kind::Worktree`
    /// leaves the new worktree with no settings file at all — approve must
    /// not conjure one into existence just by running.
    #[test]
    fn approve_installs_nothing_when_the_main_checkout_lacks_the_hook() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-hook-off";
        let (hub, _repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        let r = t.join().unwrap();
        let AskReply::Path { path } = r else { panic!("expected Path, got {r:?}") };
        let owner = format!("{p}/.claude/worktrees/worktree-fox");
        assert!(
            !std::path::Path::new(&path).join(".claude/settings.local.json").exists(),
            "no settings file at all when the main checkout never had the hook"
        );
        ask_env_clear(&[p, &owner]);
    }

    /// Revert-checked: answering without removing the ask (so the second
    /// answer finds it still pending and runs a second create) failed 5/5
    /// runs — observed both `assertion failed: matches!(t.join().unwrap(),
    /// AskReply::Path { .. })` (the second, refused create answered Claude
    /// first) and `the second answer was acted on: [.. two
    /// WorktreeAskClosed ..]`. The brief's worktree count alone stayed green
    /// under that revert, because `create_named` refuses the taken name.
    #[test]
    fn second_answer_to_an_ask_is_ignored() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-second";
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let (b, rxb) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id: id.clone(), approve: true });
        Hub::lock(&hub).handle(&b, Intent::WorktreeAnswer { id, approve: true });
        assert!(matches!(t.join().unwrap(), AskReply::Path { .. }));
        // A second create (if one ran) is on its own thread; give it the
        // time a create takes before looking.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(crate::worktree::list(&repo).len(), 2, "created exactly once");
        // The worktree count alone cannot see a second run: `create_named`
        // refuses the now-taken name, so a second attempt leaves the count
        // at 2 too. What it *does* leave is an answer to the second clicker
        // and a second close. Neither may exist.
        let got_b = drain(&rxb);
        assert!(
            !got_b.iter().any(|m| m.contains(r#""t":"Error""#) || m.contains(r#""t":"WorktreeReady""#)),
            "the second answer was acted on: {got_b:?}"
        );
        assert_eq!(
            got_b.iter().filter(|m| m.contains(r#""t":"WorktreeAskClosed""#)).count(),
            1,
            "closed once: {got_b:?}"
        );
        ask_env_clear(&[p, &format!("{p}/.claude/worktrees/worktree-fox")]);
    }

    #[test]
    fn refuse_writes_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-refuse";
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: false });
        assert_eq!(t.join().unwrap(), AskReply::Refuse { refuse: "refused in roost".into() });
        assert_eq!(crate::worktree::list(&repo).len(), 1);
        assert!(matches!(crate::home::owner_of(p, "term3"), crate::home::Owner::Here));
        ask_env_clear(&[p]);
    }

    /// Revert-checked: dropping the timeout's `WorktreeAskClosed` broadcast
    /// fails with `the expired ask's dialogs are closed: [WorktreeAsk only]`.
    #[test]
    fn an_unanswered_ask_is_refused_and_closed() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-timeout";
        let (hub, _repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        let (_a, rxa) = conn(&mut Hub::lock(&hub));
        let r = ask_worktree_blocking_with(req(p, "term3", "fox"), std::time::Duration::from_millis(200));
        assert_eq!(r, AskReply::Refuse { refuse: "nobody approved it in roost within 60 s".into() });
        let got = drain(&rxa);
        let id = got
            .iter()
            .find_map(|m| {
                let v: serde_json::Value = serde_json::from_str(m).ok()?;
                (v["t"] == "WorktreeAsk").then(|| v["id"].as_str().unwrap().to_string())
            })
            .expect("the ask was broadcast");
        assert!(
            got.iter().any(|m| m.contains(r#""t":"WorktreeAskClosed""#) && m.contains(&format!(r#""id":"{id}""#))),
            "the expired ask's dialogs are closed: {got:?}"
        );
        assert!(Hub::lock(&hub).asks.is_empty(), "the expired ask is gone");
        ask_env_clear(&[p]);
    }

    /// Revert-checked: recording before `create_named` fails here with
    /// `no record without a worktree` — a terminal re-homed into a worktree
    /// git refused to make.
    #[test]
    fn a_failed_create_writes_no_record() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-failcreate";
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        // The name is taken: the branch already exists.
        assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(["branch", "worktree-fox"]).status().unwrap().success());
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        let r = t.join().unwrap();
        assert!(matches!(&r, AskReply::Refuse { refuse } if refuse.contains("branch worktree-fox already exists")), "{r:?}");
        assert!(matches!(crate::home::owner_of(p, "term3"), crate::home::Owner::Here), "no record without a worktree");
        let got = drain(&rxa);
        assert!(got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("already exists")), "the clicker hears why: {got:?}");
        ask_env_clear(&[p]);
    }

    /// R22. Revert-checked: with the cap check disabled the fifth ask is
    /// parked like the others and this fails with `left: Refuse { refuse:
    /// "nobody approved it in roost within 60 s" }` against the cap's text.
    #[test]
    fn a_fifth_ask_from_one_project_is_refused() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-cap";
        let hub = registered_hub_with_session(root.path(), p, "term");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let waiters: Vec<_> = (1..=MAX_PENDING_ASKS).map(|i| spawn_ask(p, &format!("term{i}"), "fox")).collect();
        let ids: Vec<String> = (0..MAX_PENDING_ASKS).map(|_| wait_for_ask(&rxa)).collect();
        let t0 = std::time::Instant::now();
        // A short wait, so that with the cap gone this fails in a second
        // with the timeout's text rather than hanging for 60.
        let r = ask_worktree_blocking_with(req(p, "term9", "fox"), std::time::Duration::from_secs(1));
        assert_eq!(r, AskReply::Refuse { refuse: format!("too many worktree requests waiting in {p}") });
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "refused at once, not after a wait");
        // One per session still holds alongside it.
        let r = ask_worktree_blocking(req(p, "term1", "fox"));
        assert!(matches!(&r, AskReply::Refuse { .. }), "{r:?}");
        for id in ids {
            Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: false });
        }
        for w in waiters {
            assert_eq!(w.join().unwrap(), AskReply::Refuse { refuse: "refused in roost".into() });
        }
        ask_env_clear(&[p]);
    }

    #[test]
    fn a_second_ask_from_one_terminal_is_refused_while_the_first_waits() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-onepersession";
        let hub = registered_hub_with_session(root.path(), p, "term3");
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        let r = ask_worktree_blocking(req(p, "term3", "otter"));
        assert_eq!(r, AskReply::Refuse { refuse: "an earlier request from this terminal is still waiting".into() });
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: false });
        t.join().unwrap();
        ask_env_clear(&[p]);
    }

    #[test]
    fn an_invalid_session_or_name_is_refused_before_anything() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-invalid";
        let hub = registered_hub_with_session(root.path(), p, "term3");
        let (_a, rxa) = conn(&mut Hub::lock(&hub));
        let r = ask_worktree_blocking(req(p, "../x", "fox"));
        assert!(matches!(&r, AskReply::Refuse { refuse } if refuse.contains("terminal name")), "{r:?}");
        let r = ask_worktree_blocking(req(p, "term3", "-rf"));
        assert!(matches!(&r, AskReply::Refuse { refuse } if refuse.contains("must match")), "{r:?}");
        assert!(drain(&rxa).iter().all(|m| !m.contains("WorktreeAsk")), "nothing was asked");
        ask_env_clear(&[p]);
    }

    /// R16. Revert-checked: with the gate disabled this fails with `left:
    /// Path { path: ".../ask-r16/.claude/worktrees/worktree-fox" }` — Claude
    /// told to move, and a second live `term3` in one owner.
    #[test]
    fn the_record_gate_refuses_a_name_live_in_the_owner() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-r16";
        let owner = format!("{p}/.claude/worktrees/worktree-fox");
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        // A live `term3` already keyed under the owner URL — the worktree
        // itself cannot exist yet (create would refuse), so its cwd is the
        // repository; the session map is keyed by URL, not by directory.
        crate::session::reserve_and_attach(&owner, "term3", &repo).unwrap();
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        let r = t.join().unwrap();
        assert_eq!(r, AskReply::Refuse { refuse: "worktree-fox already has a terminal named term3".into() });
        assert!(matches!(crate::home::owner_of(p, "term3"), crate::home::Owner::Here), "no record past the gate");
        // Creating destroys nothing: the worktree stays.
        assert!(repo.join(".claude/worktrees/worktree-fox").is_dir());
        ask_env_clear(&[p, &owner]);
    }

    /// Asserted in this thread straight after the reply, no sleep: the
    /// refresh must have happened *before* `Path` was sent.
    ///
    /// Revert-checked twice: removing the refresh fails with `left: None,
    /// right: Some("ask-refresh")`; moving it to after the reply fails the
    /// same way 5/5 runs (that variant is a race the asker almost always
    /// wins, so it is evidence, not proof).
    #[test]
    fn approve_refreshes_the_owner_hub_before_replying() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        ask_env(root.path());
        let p = "ask-refresh";
        let owner = format!("{p}/.claude/worktrees/worktree-fox");
        let (hub, repo) = registered_hub_in_git_repo_with_session(root.path(), p, "term3");
        // The owner's hub is open before the worktree exists — a tab on a
        // URL that does not resolve yet, exactly as a fast browser would be.
        let owner_hub = register_for_test(Hub::new(&owner, repo.join(".claude/worktrees/worktree-fox")));
        assert!(Hub::lock(&owner_hub).session_homes.is_empty());
        let (a, rxa) = conn(&mut Hub::lock(&hub));
        let t = spawn_ask(p, "term3", "fox");
        let id = wait_for_ask(&rxa);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        assert!(matches!(t.join().unwrap(), AskReply::Path { .. }));
        assert_eq!(
            Hub::lock(&owner_hub).session_homes.get("term3"),
            Some(&p.to_string()),
            "the owner hub knows its resident before Claude is told to move"
        );
        assert!(
            !Hub::lock(&hub).ws.live_sessions.contains(&"term3".to_string()),
            "and the home hub has let go of it"
        );
        ask_env_clear(&[p, &owner]);
    }

    #[test]
    fn remove_refuses_a_worktree_with_a_live_terminal() {
        // Revert-checked: with the live check deleted this passes the
        // worktree straight to git, which removes it — `refusal_of`'s own
        // assertion that the answer was an `Error` fails first (removal
        // *succeeds*, so no later assertion in this test even runs).
        // Observed: `panicked ... expected a refusal, got
        // {"t":"ProjectsChanged","project":"repo"}`.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        let _att = crate::session::reserve_and_attach(&url, "term", &dir).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("live terminal"), "{got}");
        assert!(dir.is_dir());
        crate::session::kill_project(&url);
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A resident's socket lives under its *home* ("repo"), never under the
    /// worktree it was re-homed into — so the removal gate must count
    /// `owned_live_names(&url)`, not `live_names(&url)`, or a running
    /// re-homed Claude's terminal would be invisible to it and `git
    /// worktree remove` would delete the directory out from under it.
    ///
    /// `ROOST_ROOTS` is set so `home::resolves` (fork-free, no git) can
    /// confirm the worktree dir under "repo" is real; without it the
    /// resident would fall back to showing at home and this test would
    /// prove nothing about the gate.
    ///
    /// Revert-checked: reverting the gate's `owned_live_names` back to
    /// `live_names` fails `refusal_of`'s own assertion that the answer was
    /// an `Error` — removal *succeeds* — observed: `panicked ... expected a
    /// refusal, got {"t":"ProjectsChanged","project":"repo"}`.
    #[test]
    fn remove_refuses_a_worktree_with_a_live_resident() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        let (mut h, url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe();
        drain(&rx);
        // The resident's dtach socket is under its home, "repo" — re-homing
        // never moves it (see home.rs's module doc) — while the record
        // names the worktree as its owner.
        let home_dir = h.dir.clone();
        let _att = crate::session::reserve_and_attach("repo", "term3", &home_dir).unwrap();
        crate::home::record("repo", "term3", &url).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("terminal"), "{got}");
        assert!(dir.is_dir());
        crate::session::kill_project("repo");
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn remove_refuses_a_worktree_where_a_claude_was_launched() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        crate::session::reserve(&url, "term", Some(crate::session::LaunchRequest { launch: proto::Launch::Claude, session: None }));
        let _att = crate::session::reserve_and_attach(&url, "term", &dir).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        // Names the more specific reason even though "live terminal" is also true.
        //
        // Revert-checked: deleting the `claude_evidence` match falls through
        // to the live-terminal gate instead, which still refuses — but with
        // the wrong, less specific reason. Observed: `panicked ...
        // {"t":"Error","msg":"claude-1 has a live terminal"}`, failing
        // `got.contains("Claude")`. Restored.
        assert!(got.contains("Claude"), "{got}");
        assert!(dir.is_dir());
        crate::session::kill_project(&url);
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn remove_refuses_a_dirty_worktree() {
        // Revert-checked: deleting the `st.dirty` match still refuses — git
        // itself is the backstop here, since a dirty tree also fails
        // `git worktree remove` without `--force` — but with a different
        // message than ours ("uncommitted"), so the assertion below already
        // discriminates the hub's own gate from git's. Observed: `panicked
        // ... {"t":"Error","msg":"git worktree remove refused: fatal:
        // '.../.claude/worktrees/claude-1' contains modified or untracked
        // files, use --force to delete it"}`, failing
        // `got.contains("uncommitted")`. Restored.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        std::fs::write(dir.join("n.txt"), "y").unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("uncommitted"), "{got}");
        assert!(dir.join("n.txt").is_file());
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn remove_refuses_a_worktree_with_commits_ahead() {
        // Revert-checked: with the `st.ahead` match deleted, `remove` runs
        // and git itself refuses the unmerged branch, so this still gets an
        // `Error` — but the wrong one, and the worktree is gone. Observed:
        // `panicked ... {"t":"Error","msg":"worktree removed; branch
        // claude-1 kept: git reports it unmerged (...)"}`, failing the
        // `got.contains("1 commit")` assertion.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        std::fs::write(dir.join("n.txt"), "y").unwrap();
        crate::worktree::real_git(&dir, &["add", "."]).unwrap();
        crate::worktree::real_git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "wt"]).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("1 commit"), "{got}");
        assert!(dir.is_dir());
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn remove_takes_a_clean_idle_worktree_and_its_state() {
        // Revert-checked: flipping the dirty match's `Some(false) => {}` arm
        // to also refuse (simulating any one gate wrongly always firing)
        // turns this success case into a refusal. Observed: `panicked ...
        // {"t":"Error","msg":"claude-1 has uncommitted changes"}`, failing
        // the `!got.contains(r#""t":"Error""#)` assertion. Restored.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        let state = crate::wsstate::state_dir();
        std::fs::write(state.join(format!("{WT_KEY}.json")), "{}").unwrap();
        h.handle(&a, Intent::RemoveWorktree { key: WT_KEY.into() });
        let got = rx.try_recv().unwrap();
        assert!(!got.contains(r#""t":"Error""#), "{got}");
        assert!(matches!(std::fs::symlink_metadata(&dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        assert!(crate::worktree::read_base(&state, WT_KEY).is_none(), ".base gone");
        assert!(!state.join(format!("{WT_KEY}.json")).exists(), "layout gone");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn remove_refuses_a_key_that_is_not_a_worktree_of_this_project() {
        // The key the *unfiltered* `ws.iter()` loop would mint for the main
        // checkout itself: its canonicalized path strips to an empty `rel`
        // against the canonicalized repo, so `format!("{project}/{}", "")`
        // is "repo/" and its storage key is "repo%2F" — not "repo". Using
        // plain "repo" here would pass whether or not the `!w.is_main`
        // filter exists (it never collides with anything the loop mints
        // either way), so it wouldn't discriminate the filter this test is
        // named for.
        //
        // Revert-checked: deleting `.filter(|w| !w.is_main)` at the
        // `for w in ws.iter()...` loop makes this key match the main
        // checkout, so identification succeeds and the flow reaches the
        // dirty gate — which then refuses for an unrelated reason (the
        // freshly-created `.claude/worktrees/claude-1` sits as an untracked
        // directory in the main checkout's own `git status`). Observed:
        // `panicked ... the main checkout is never removable:
        // {"t":"Error","msg":"main has uncommitted changes"}` — no longer
        // containing "not a worktree of this project". Restored.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        let (mut h, _url, _dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        let key = crate::projects::storage_key("repo/");
        h.handle(&a, Intent::RemoveWorktree { key });
        let got = rx.try_recv().unwrap();
        assert!(got.contains("not a worktree of this project"), "the main checkout is never removable: {got}");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn open_at_line_opens_the_file_and_reveals_the_line() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        drain(&rx);
        drain(&rx_other);

        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 2 });

        let mine = drain(&rx);
        assert!(mine.iter().any(|m| m.contains(r#""t":"State""#) && m.contains("a.rs")), "{mine:?}");
        assert!(
            mine.iter().any(|m| m.contains(r#""t":"RevealLine""#) && m.contains(r#""line":2"#)),
            "{mine:?}"
        );
        // Broadcast, not send_to: a second browser mirroring this tab follows
        // it to the same line. Asserted explicitly so the choice cannot flip
        // silently in either direction.
        assert!(
            drain(&rx_other).iter().any(|m| m.contains(r#""t":"RevealLine""#)),
            "a mirroring browser must be told where to scroll"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn open_at_line_opening_a_file_twice_does_not_open_a_second_tab() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 1 });
        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 2 });

        let tabs = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::File { rel, .. } if rel == "a.rs"))
            .count();
        assert_eq!(tabs, 1, "a second hit in an open file must re-scroll it, not clone the tab");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `rel` arrives from a browser. `apply_layout` validates nothing, so the
    /// confinement check has to be here — the same reasoning `OpenPath`
    /// carries. Asserts on the *message*, not merely that something failed:
    /// an intent rejected for the wrong reason would pass an `is_err`-style
    /// check while leaving the escape open.
    #[test]
    fn open_at_line_refuses_a_path_outside_the_project() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("proj", root);
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine {
            pane: proto::MIDDLE,
            rel: "../secret.txt".into(),
            line: 1,
        });

        let got = drain(&rx);
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("outside project")),
            "must refuse by confinement and say so, got {got:?}"
        );
        // `hub.rs`'s confinement arm replaces the message with the bare
        // "path outside project" instead of echoing the caller's path. This
        // assertion is what holds that in place.
        assert!(
            !got.iter().any(|m| m.contains("secret.txt")),
            "nothing outside the project may reach the layout, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn a_terminal_link_with_a_line_number_reveals_that_line() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenPath { text: "a.rs:3".into() });

        let got = drain(&rx);
        assert!(got.iter().any(|m| m.contains(r#""t":"State""#) && m.contains("a.rs")), "{got:?}");
        assert!(
            got.iter().any(|m| m.contains(r#""t":"RevealLine""#) && m.contains(r#""line":3"#)),
            "a link that named a line must land on it, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn a_terminal_link_without_a_line_number_reveals_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenPath { text: "a.rs".into() });

        assert!(
            !drain(&rx).iter().any(|m| m.contains(r#""t":"RevealLine""#)),
            "a plain path must not scroll anywhere"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 1: `apply_layout`'s `OpenTab` arm only applies
    /// `mode` when it creates a fresh tab — reactivating an already-open one
    /// just flips `pane.active` and leaves the stored tab's mode alone. A
    /// file opened from the tree or a terminal link lands in Preview; taking
    /// a content hit inside it must still land the reader in Edit, since
    /// that's the whole reason `do_open_at_line` asks for Edit in the first
    /// place (a rendered preview has no line 412 to scroll to).
    ///
    /// Revert-checked: removing the `if already_open { self.handle(...,
    /// SetMode...) }` call reproduces the bug this test exists to catch —
    /// see the fix-round-1 report for the verbatim failure.
    #[test]
    fn open_at_line_switches_an_already_open_preview_tab_to_edit() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.md"), "one\ntwo\nthree\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(
            &c,
            Intent::OpenTab { pane: proto::MIDDLE, tab: Tab::File { rel: "a.md".into(), mode: Mode::Preview } },
        );
        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.md".into(), line: 2 });

        let tabs: Vec<_> = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::File { rel, .. } if rel == "a.md"))
            .collect();
        assert_eq!(tabs.len(), 1, "must reuse the tab, not clone it");
        assert!(
            matches!(tabs[0], Tab::File { mode: Mode::Edit, .. }),
            "a content hit must flip an already-open Preview tab to Edit, got {:?}",
            tabs[0]
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 2: `pane` is a client-controlled `u8` that
    /// nothing bounds-checks before this point (`do_open_path` sidesteps the
    /// question entirely by hardcoding `MIDDLE`). An out-of-range pane makes
    /// the `OpenTab` this method issues fail — reported to the asker alone —
    /// and the fix confirms the tab exists before broadcasting `RevealLine`,
    /// so a second browser is never told to scroll a tab that was never
    /// created.
    ///
    /// Revert-checked: removing the post-`OpenTab` `find_tab` guard (so
    /// `RevealLine` always broadcasts) reproduces the leak this test exists
    /// to catch — see the fix-round-1 report for the verbatim failure.
    #[test]
    fn open_at_line_with_an_out_of_range_pane_reports_an_error_and_broadcasts_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        drain(&rx);
        drain(&rx_other);

        h.handle(&c, Intent::OpenAtLine { pane: 99, rel: "a.rs".into(), line: 1 });

        let got = drain(&rx);
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#)),
            "the asker must be told the pane was rejected, got {got:?}"
        );
        assert!(
            !drain(&rx_other).iter().any(|m| m.contains(r#""t":"RevealLine""#)),
            "nobody may be told to scroll a tab that was never created"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 3: `safe_resolve` keeps "confidently outside the
    /// project" distinct from "canonicalize failed for any other reason"
    /// (missing file, permission denied, ...) — collapsing both into a flat
    /// refusal would report a file merely deleted since a search ran as
    /// having escaped the project, the exact false-claim shape CLAUDE.md's
    /// eleven-defect table describes.
    ///
    /// Revert-checked: collapsing the two branches back to `.is_err()`
    /// reproduces the false claim this test exists to catch — see the
    /// fix-round-1 report for the verbatim failure.
    #[test]
    fn open_at_line_on_a_missing_but_in_bounds_path_is_not_reported_as_outside() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        // Never created: `gone.rs` resolves inside the project but isn't there.
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "gone.rs".into(), line: 1 });

        let got = drain(&rx);
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && !m.contains("outside")),
            "a missing-but-confined path must not be reported as outside the project, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A restore replaces the state file underneath a hub that was built from
    /// the old one. Without `reload_from_disk` the restore appears to work and
    /// then silently reverts: the hub still holds the previous layout, and the
    /// next intent that touches it saves that layout back over the restored
    /// file. Nothing anywhere says so — no error, no banner, and a state file
    /// whose contents are correct right up until the next click.
    ///
    /// Found while wiring #18 step 3, not by a test — which is why this one
    /// exists. Revert-checked by deleting the `reload_from_disk()` call from
    /// the restore arm in `wsconn`: the second assertion fails with the hub
    /// still reporting the pre-restore `show_hidden`, and the file on disk is
    /// back to the pre-restore contents after the save.
    #[test]
    fn a_layout_replaced_on_disk_is_picked_up_rather_than_written_back_over() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        let proj = d.path().join("reloadproj");
        std::fs::create_dir_all(&proj).unwrap();

        // The hub as it was before the restore.
        let mut h = Hub::new("reloadproj", proj.clone());
        h.ws.show_hidden = Some(false);
        crate::wsstate::save("reloadproj", &h.ws).unwrap();
        assert_eq!(h.ws.show_hidden, Some(false), "setup");

        // What a restore does: write a different layout to the same path,
        // behind the hub's back.
        let mut restored = h.ws.clone();
        restored.show_hidden = Some(true);
        crate::wsstate::save("reloadproj", &restored).unwrap();
        assert_eq!(
            h.ws.show_hidden,
            Some(false),
            "setup: the hub has not noticed, which is the whole problem"
        );

        h.reload_from_disk();
        assert_eq!(h.ws.show_hidden, Some(true), "the hub must pick the restored layout up");

        // And the restored value survives the next save — the moment the
        // silent revert used to happen.
        crate::wsstate::save("reloadproj", &h.ws).unwrap();
        let (after, _) = crate::wsstate::load("reloadproj");
        assert_eq!(after.show_hidden, Some(true), "the restored layout was written back over");
        std::env::remove_var("ROOST_STATE_DIR");
    }
}
