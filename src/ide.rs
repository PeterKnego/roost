//! The socket Claude Code connects to, and the reason its rules differ from
//! every other socket in this codebase.
//!
//! resh is the *server* here. The extension model has the IDE listening and
//! `claude` connecting out to it, which is what lets the integration work for
//! a Claude attached to a dtach session resh did not spawn.
//!
//! The client is a Bun process, not a browser, so it sends no `Origin` — it
//! sends a token from the lock file. `origin.rs` refuses a handshake with no
//! Origin because "every browser sends one, so its absence means a non-browser
//! client, which has no business here." On this socket that reasoning runs
//! backwards: a browser is the only thing that sends one, and a browser has no
//! business here. Both sockets are right; the rules are opposites.
//!
//! That is not a stylistic point. Claude Code's own extensions shipped this
//! socket unauthenticated and Origin-blind through version 1.0.23, and because
//! WebSocket handshakes bypass the same-origin policy, any web page could scan
//! localhost, connect, and read files — CVE-2025-52882, fixed in 1.0.24 by the
//! lock-file token this module implements.
use crate::idecwd::{self, Cwd};
use crate::idelock;
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::accept_hdr_with_config;

/// An `openDiff` carries a whole file. Both of its sides are capped at
/// `projects::MAX_FILE_BYTES` (2 MB) by `open_diff` itself; this is only the
/// coarse backstop against an oversized frame being buffered at all, and is
/// deliberately looser than the real limit rather than a second copy of it.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub struct Ide {
    pub port: u16,
    pub token: String,
    /// Removed on drop, and only ever the path we wrote.
    _lock: idelock::Lock,
    /// Set by `request_shutdown` (called from `stop`), and checked by the
    /// accept loop between connections. A flag alone cannot wake a thread
    /// already parked in `accept(2)` inside `TcpListener::incoming()` — see
    /// `request_shutdown` for how it actually gets observed.
    stopped: Arc<AtomicBool>,
}

impl Ide {
    /// Task 3's review flagged that dropping the `Arc<Ide>` removed the lock
    /// file but left the accept loop running forever, authenticating against
    /// a token no longer advertised anywhere. Not exploitable (the token
    /// stays unguessable), but `stop` is where a real shutdown belongs.
    ///
    /// `TcpListener::incoming()` blocks in the kernel until a connection
    /// arrives, so setting `stopped` by itself would not be noticed until
    /// some *other* client happened to connect next — which might be never.
    /// A loopback self-connect delivers exactly one more item to
    /// `incoming()`, which is what lets the loop actually observe the flag
    /// and exit. That self-connection is never served: the loop checks
    /// `stopped` before it ever matches on the accepted stream, so it just
    /// gets dropped. Once the loop breaks, `listener` (moved into the
    /// thread's closure) drops too, which is what actually closes the port.
    fn request_shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Bounded, not just best-effort: Task 5's review (finding 1) caught
        // this connect running *inside* `stop`'s registry-lock critical
        // section, which made a stalled loopback connect a global stall —
        // every `attach` in the process reads `ide::port_for` while holding
        // the *sessions* lock (see `session::attach`'s doc comment), so one
        // wedged `TcpStream::connect` here would have wedged every session
        // in every project, the exact historical defect CLAUDE.md's "never
        // hold a lock across blocking I/O" constraint was written from.
        // `stop` no longer holds that lock across this call at all (see
        // `stop`), but the connect itself still gets its own timeout rather
        // than staying unbounded: an untimed connect here would still be a
        // real, if now solitary, way for `stop` itself to hang, and nothing
        // requires this self-connect to succeed for shutdown to be correct
        // (see the comment below).
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        // If this fails or times out (e.g. the listener already closed for
        // some other reason) the accept loop still exits on the next real
        // connection or at process exit. Never panic here — this runs on
        // the CloseProject path and must not take a browser socket down
        // with it.
        let _ = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500));
    }
}

/// Length is not secret — the token is a fixed 32 hex chars — but the bytes
/// are, so the comparison must not stop at the first difference.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    // Port 0: the OS picks, and the lock file must advertise what was actually
    // bound, not what was asked for.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = idelock::new_token()?;
    let lock = idelock::write_in(dir, port, &token, &workspace)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let ide = Arc::new(Ide { port, token: token.clone(), _lock: lock, stopped: stopped.clone() });
    let project = project.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            // Checked before the stream is even matched on: the self-connect
            // `request_shutdown` makes to wake this loop must never be
            // handed to `serve_conn`, only used to get back here and exit.
            if stopped.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let token = token.clone();
            let project = project.clone();
            let workspace = workspace.clone();
            std::thread::spawn(move || {
                // A panic here must not take the process down with it: this
                // thread is fed attacker-influenced bytes.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_conn(stream, &token, &project, &workspace);
                }));
            });
        }
    });
    Ok(ide)
}

pub fn start(project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    start_in(&idelock::ide_dir(), project, workspace)
}

/// One listener per project, not one per connection to it — mirrors
/// `Hub::for_project`'s registry, and is deliberately a *separate* map: a
/// `claude` reconnecting to a project resh already has open must not be
/// handed a second listener (a second port, a second lock file, and a stale
/// one left behind that Claude Code would just as happily match against).
static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Ide>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Arc<Ide>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `dir`-parameterised for tests (see `Task 4`'s `start_in`); production
/// code should call `for_project`.
pub fn for_project_in(dir: &Path, project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    // Fast path only touches the map: no I/O, so the lock is held for a
    // lookup and a clone, nothing more.
    {
        let map = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(project) {
            return Some(existing.clone());
        }
    }
    // The registry lock is released before `start_in`'s blocking I/O (a TCP
    // bind, a urandom read, a file write, a thread spawn) — CLAUDE.md's
    // "never hold a lock across blocking I/O" is not a style preference
    // here: this call sits directly ahead of `session::attach` on the
    // terminal-open path (see `term.rs`), which every project opened during
    // a test run — or in production, every terminal in every project —
    // passes through. Holding one process-global mutex across that I/O
    // would serialize every concurrent "first terminal in a new project"
    // in the whole process behind whichever one happened to go first;
    // dropping the lock here lets unrelated projects build their listeners
    // in parallel, which is exactly what fixed a real, reproducible
    // regression this task introduced (five integration tests started
    // failing "no such session" once this call sat, lock held, ahead of
    // `attach` — see this task's report).
    let built = start_in(dir, project, workspace);
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = map.get(project) {
        let existing = existing.clone();
        // Another caller for the same project won the race while this one
        // was building its own listener. Task 5's review (finding 3)
        // pointed out this is not the rare case the first draft of this
        // comment claimed — a page load opens the `_workspace` socket
        // (`hub.rs`) and a terminal socket (`term.rs`) for the same project
        // near-simultaneously, which is the ordinary reconnect flow, not an
        // edge case. So the loser's `Ide` is shut down properly, the same
        // way `stop` does it: released from the registry lock first (its
        // own `request_shutdown` call, and the lock-file removal its final
        // drop triggers, are both real syscalls), rather than just letting
        // it leak a live accept thread until process exit.
        drop(map);
        if let Ok(losing_ide) = built {
            losing_ide.request_shutdown();
        }
        return Some(existing);
    }
    match built {
        Ok(ide) => {
            map.insert(project.to_string(), ide.clone());
            Some(ide)
        }
        Err(e) => {
            // Degraded, never fatal: IDE integration is a convenience, and a
            // read-only home directory (or any other reason the lock file
            // could not be written) must not stop a project from opening.
            eprintln!("resh: IDE integration unavailable for {project}: {e}");
            None
        }
    }
}

pub fn for_project(project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    for_project_in(&idelock::ide_dir(), project, workspace)
}

pub fn port_for(project: &str) -> Option<u16> {
    let map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).map(|i| i.port)
}

/// Removing the project's entry drops the last `Arc<Ide>` this module holds,
/// which drops the `Lock` inside it — removing exactly the lock file this
/// listener wrote, never a directory scan. `request_shutdown` (below) is
/// what stops the *listener*; without it the accept loop would keep running,
/// unadvertised, until the process exits. Must not disturb any other
/// project's entry: removed by key, never by clearing the map.
///
/// Task 5's review (finding 1, Critical): the registry lock used to stay
/// held across both `request_shutdown`'s connect *and* the removed `Arc`'s
/// own drop (which runs `idelock::Lock::drop`'s `remove_file`) — two real
/// syscalls, one process-global mutex, exactly the "never hold a lock
/// across blocking I/O" constraint this codebase has already shipped one
/// deadlock from. `session::attach` reads `ide::port_for` while holding the
/// *sessions* lock, so a stalled connect here would have stalled every
/// `attach` in every project, not just this one's. `map.remove` now returns
/// the `Arc` out of the locked block entirely — the lock is released before
/// `ide` (and everything its drop does) is ever touched.
pub fn stop(project: &str) {
    let ide = {
        let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
        map.remove(project)
    };
    if let Some(ide) = ide {
        ide.request_shutdown();
        // `ide` drops here, well after the registry lock above was
        // released — that drop is what removes the lock file.
    }
}

/// Live Claude connections per project. A `Sender` per connection, drained by
/// that connection's writer thread — the notification path must never block
/// on a socket write, and must never hold this lock while doing one.
///
/// Each entry carries a small connection id alongside its `Sender`.
/// `std::sync::mpsc::Sender` — unlike tokio's or crossbeam's — exposes no
/// `same_channel`/identity check at all, so a `ConnGuard` has no way to find
/// "its" entry in a `Vec<Sender<String>>` once other connections have
/// registered and deregistered around it; the id is what makes removal
/// correct regardless of registration/deregistration order.
static CONNS: OnceLock<Mutex<HashMap<String, Vec<(u64, std::sync::mpsc::Sender<String>)>>>> =
    OnceLock::new();

fn conns() -> &'static Mutex<HashMap<String, Vec<(u64, std::sync::mpsc::Sender<String>)>>> {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_CONN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Deregisters this connection's writer channel even if `serve_conn`'s read
/// loop panics — the same guarantee `wsconn.rs`'s `UnsubGuard` gives its
/// subscriber list. Without it a panicking connection would leave a dead
/// `Sender` in `CONNS` forever, and every later `mention` for this project
/// would "succeed" into a void: the send itself never fails (the writer
/// thread on the other end is what actually delivers it, and that thread
/// exited when its own channel disconnected, so nothing is left to act on
/// the send).
struct ConnGuard {
    project: String,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        {
            let mut map = conns().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = map.get_mut(&self.project) {
                v.retain(|(id, _)| *id != self.id);
                if v.is_empty() {
                    map.remove(&self.project);
                }
            }
        }
        // Scoped so the `CONNS` lock is released before the pending one is
        // taken, and the hub lock after that: conns -> pending -> hub, one
        // way only, which is the order `open_diff` and `do_answer_proposal`
        // already establish between the last two.
        reclaim_pending_for(&self.project, self.id);
    }
}

/// Drops every proposal parked by a connection that has gone away.
///
/// Without this the click path still self-heals — `answer` finds a dead
/// channel and says so, and the hub closes the tab — but only if a human
/// ever clicks. A tab that is simply ignored holds its `PENDING` row for the
/// life of the process, so a few CLI restarts silently exhaust the project's
/// `MAX_PENDING` slots and every later `openDiff` is refused with no visible
/// cause. That is the worst kind of failure this codebase collects.
///
/// The requests themselves are dropped unanswered rather than replied to:
/// the socket they came in on is gone, so there is nothing to answer to.
fn reclaim_pending_for(project: &str, conn_id: u64) {
    let orphaned: Vec<String> = {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, p)| p.project == project && p.conn_id == conn_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &ids {
            map.remove(k);
        }
        ids
    };
    // Outside the lock: this takes a hub lock and mirrors to every browser.
    for k in &orphaned {
        crate::hub::close_proposal(project, k);
    }
}

/// Sends `msg` — a complete JSON-RPC notification, never a request — to every
/// Claude connected to `project`. Factored out of `mention`, and shared with
/// `selection_changed`: both are one-way, fire-and-forget notices that differ
/// only in payload shape, and both need the identical fan-out.
///
/// Cloned out under the lock, sent outside it: CLAUDE.md's "never hold a lock
/// across blocking I/O" — a channel send only blocks if the writer thread's
/// queue capacity is exceeded (it is unbounded here, so it never does), but
/// the principle is the same one that bit `stop`, so this follows the same
/// shape regardless.
fn notify_all(project: &str, msg: &serde_json::Value) -> Result<(), String> {
    let msg = msg.to_string();
    let targets: Vec<_> = {
        let map = conns().lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).cloned().unwrap_or_default()
    };
    if targets.is_empty() {
        return Err("no Claude is connected to this project".into());
    }
    for (_, t) in &targets {
        let _ = t.send(msg.clone());
    }
    Ok(())
}

/// Tells every Claude connected to `project` that the user pointed at `abs`
/// (and optionally a line range within it). A notification, not a request —
/// see `dispatch`'s id handling: an `at_mentioned` carrying an `id` would
/// make the CLI wait for a response that will never come.
pub fn mention(project: &str, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String> {
    let mut params = serde_json::json!({"filePath": abs.to_string_lossy()});
    if let Some((a, b)) = lines {
        params["lineStart"] = serde_json::json!(a);
        params["lineEnd"] = serde_json::json!(b);
    }
    let msg = serde_json::json!({"jsonrpc": "2.0", "method": "at_mentioned", "params": params});
    notify_all(project, &msg)
}

/// Ships the editor's current selection to Claude as ambient context — the
/// one notification in this module that carries file *contents* with no
/// explicit user gesture behind it. `mention` and `openDiff` both require a
/// deliberate act (Alt+K, or Claude itself proposing a change); a selection
/// changes on its own as someone reads a file. That is exactly why this
/// checks `Settings::share_selection` on every call rather than once at
/// connect time: a project can turn sharing off mid-session, and the very
/// next selection must honor that, not the one that was true when Claude
/// connected.
///
/// `project_dir` is the project's own directory — what `config::for_project`
/// needs to find `.resh/config.toml` — not the ide lock directory `Ide`
/// itself is keyed by; the hub already holds it as `Hub::dir`.
pub fn selection_changed(
    project: &str,
    project_dir: &Path,
    abs: &Path,
    text: &str,
    start: (u32, u32),
    end: (u32, u32),
) -> Result<(), String> {
    if !crate::config::for_project(project_dir).share_selection {
        return Err("selection sharing is off for this project".into());
    }
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "selection_changed",
        "params": {
            "filePath": abs.to_string_lossy(),
            "text": text,
            "selection": {
                "start": {"line": start.0, "character": start.1},
                "end":   {"line": end.0,   "character": end.1},
            },
        },
    });
    notify_all(project, &msg)
}

pub struct Conn {
    /// This connection's id in `CONNS`. Carried so a proposal parked from
    /// here can be reclaimed when this connection dies — see `ConnGuard`.
    pub id: u64,
    /// Claude's working directory, learned from `ide_connected`'s pid. `None`
    /// until it connects, or when resh could not read it — those are different
    /// situations with the same representation here only because both mean
    /// "do not trust a path against it yet".
    pub cwd: Option<PathBuf>,
    pub workspace: PathBuf,
    pub project: String,
    /// This connection's outbound channel, drained by its own read loop (see
    /// `serve_conn`: there is no writer thread — two writers over one fd let
    /// tungstenite's auto-Pong interleave mid-frame). Cloning it is how a
    /// *different* thread gets a frame onto this socket, which is exactly
    /// what a parked `openDiff` needs: the answer is written minutes later,
    /// from whichever browser thread the human happened to click in.
    pub reply: std::sync::mpsc::Sender<String>,
    pub closed: bool,
}

impl Conn {
    pub fn new(
        project: &str,
        workspace: PathBuf,
        reply: std::sync::mpsc::Sender<String>,
        id: u64,
    ) -> Self {
        Conn { id, cwd: None, workspace, project: project.to_string(), reply, closed: false }
    }
}

/// Enough for any real review session, and in the spirit of the existing
/// ≤16-session and ≤50-buffer caps.
///
/// It is a memory bound as well as a sanity one: a parked proposal keeps both
/// sides of its diff alive in the hub (`Hub::proposals`, which is what lets a
/// browser connecting later see what it is being asked to approve), and the
/// hub's map only ever holds an entry per live pending row. Two file bodies
/// per proposal, sixteen proposals per project. Over the cap, refuse rather
/// than queue: `DIFF_REJECTED` is a refusal Claude is already required to
/// handle.
pub(crate) const MAX_PENDING: usize = 16;

/// What a human said about one proposal. The three arms are not three
/// shades of the same thing: they map onto the three responses the CLI
/// accepts, and it treats them as different instructions.
pub enum Answer {
    Accepted,
    /// The user rewrote the proposal before accepting it. The text travels
    /// back so Claude writes *this*, not what it proposed.
    AcceptedEdited(String),
    Rejected,
}

/// An `openDiff` that has been asked and not yet answered.
///
/// The whole point of this struct is that the request is *parked*: `dispatch`
/// returns `None` for an `openDiff` and this holds everything needed to
/// answer it later, from another thread. Blocking the read loop instead would
/// mean Claude could not send `close_tab` to withdraw the proposal, and a
/// user who walked away would wedge the socket for good.
struct Pending {
    /// Answers are scoped to the project that parked them: an id is a map key
    /// within one process, and nothing else must be able to name it.
    project: String,
    /// The `CONNS` id of the connection that asked. A parked request outlives
    /// nothing: when that connection dies the question is unanswerable, and
    /// leaving the row behind would silently consume one of this project's
    /// `MAX_PENDING` slots forever (see `ConnGuard::drop`).
    conn_id: u64,
    /// The id from Claude's request, echoed back verbatim. Kept as a `Value`
    /// because JSON-RPC allows a string or a number and the reply must be the
    /// same shape it arrived as.
    rpc_id: serde_json::Value,
    /// Claude's own name for the tab, which is what `close_tab` addresses.
    /// Deliberately not the map key — see `new_pending_id`.
    tab_name: String,
    /// A clone of the connection's outbound channel (`Conn::reply`), which is
    /// how a browser thread writes onto a socket owned by a connection thread.
    reply: std::sync::mpsc::Sender<String>,
}

static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A map key within one process — never persisted, never security-relevant.
/// A counter rather than randomness so test assertions are deterministic; a
/// collision after a restart is impossible because pending proposals do not
/// survive one.
///
/// Deliberately not Claude's `tab_name`: that is chosen by the client, and
/// two connections to the same project would collide on it.
fn new_pending_id(project: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{project}-{n}")
}

/// Answer a parked `openDiff`. This is what a browser's Accept/Reject
/// actually does — the click *is* the permission answer.
pub fn answer(project: &str, id: &str, a: Answer) -> Result<(), String> {
    // Removed, not read: the removal *is* the "first answer wins" rule, and
    // doing it under the lock means two browsers cannot both win. A
    // read-then-remove would let both pass the check and both send.
    let p = {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        match map.get(id) {
            Some(p) if p.project == project => map.remove(id).ok_or("just matched")?,
            Some(_) => return Err("proposal belongs to another project".into()),
            None => return Err("no such proposal — it was already answered or withdrawn".into()),
        }
    };
    let content = match a {
        // Not `FILE_SAVED`: an unedited acceptance leaves Claude's own tool
        // input correct, and resh must not write the file (see `open_diff`).
        // These three strings are the CLI's whole vocabulary here — anything
        // else raises `Not accepted` on its side.
        Answer::Accepted => serde_json::json!([{"type": "text", "text": "TAB_CLOSED"}]),
        Answer::Rejected => serde_json::json!([{"type": "text", "text": "DIFF_REJECTED"}]),
        Answer::AcceptedEdited(text) => serde_json::json!([
            {"type": "text", "text": "FILE_SAVED"},
            {"type": "text", "text": text},
        ]),
    };
    let msg = serde_json::json!({
        "jsonrpc": "2.0", "id": p.rpc_id, "result": {"content": content}
    })
    .to_string();
    // Outside the lock: this is a channel send feeding a socket write, and a
    // proposal's lock must never be held across anything that touches I/O.
    p.reply.send(msg).map_err(|_| "Claude disconnected before answering".to_string())
}

/// Parks a proposal with no socket behind it, so `hub`'s own tests can drive
/// the intent that answers one without standing up a listener and a fake
/// client. The `reply` channel stands in for the connection's.
#[cfg(test)]
pub(crate) fn park_for_test(
    project: &str,
    tab: &str,
    reply: std::sync::mpsc::Sender<String>,
) -> String {
    let id = new_pending_id(project);
    pending().lock().unwrap_or_else(|e| e.into_inner()).insert(
        id.clone(),
        Pending {
            project: project.to_string(),
            // No real connection behind it, and 0 is never handed out by
            // `NEXT_CONN_ID` (which starts at 1), so this can never be
            // reclaimed by a live connection's guard.
            conn_id: 0,
            rpc_id: serde_json::json!(1),
            tab_name: tab.to_string(),
            reply,
        },
    );
    id
}

/// The pending id for a project's tab name, for tests that need to answer a
/// proposal they parked over a real socket.
#[cfg(test)]
fn pending_id_for_tab(project: &str, tab: &str) -> Option<String> {
    let map = pending().lock().unwrap_or_else(|e| e.into_inner());
    map.iter()
        .find(|(_, p)| p.project == project && p.tab_name == tab)
        .map(|(k, _)| k.clone())
}

/// Confine an absolute path Claude sent, which may name a file that does not
/// exist yet.
///
/// `openDiff` carries `old_file_path` off the wire, so it is a hint and never
/// a boundary. Two things make this harder than `resolve_terminal_path`'s
/// single `safe_resolve` call: the target may legitimately not exist (Claude
/// proposing a *new* file), and `projects::abs_to_rel` needs it to exist,
/// because `canonicalize` is what resolves the symlinks the comparison rests
/// on. So the *parent* — which does exist either way — is what gets
/// canonicalised and confined, and the final component is validated
/// separately. That is exactly the split `safe_resolve_parent` already
/// exists for.
///
/// Returns the project-relative path and, when it is there, its real
/// location on disk.
fn confined_target(workspace: &Path, path: &str) -> Result<(String, Option<PathBuf>), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("not an absolute path: {path}"));
    }
    let name = p.file_name().and_then(|n| n.to_str()).ok_or("that is a directory, not a file")?;
    let parent = p.parent().ok_or("that is the filesystem root, not a file")?;
    // The parent, canonicalised against the project root. A symlinked
    // directory that leaves the project is refused here.
    let parent_rel = crate::projects::abs_to_rel(workspace, parent)?;
    let rel = if parent_rel.is_empty() { name.to_string() } else { format!("{parent_rel}/{name}") };
    // Re-confined through the project's own boundary rather than trusting the
    // string just built: this re-canonicalises the parent and rejects a final
    // component that is empty, `.`, `..` or contains a separator.
    let target = crate::projects::safe_resolve_parent(workspace, &rel)?;
    // Three outcomes, not two. "I could not look" must not be read as "it is
    // not there": that would show a whole-file rewrite as a *creation*, with
    // an empty left-hand side, to the human being asked to approve it.
    match std::fs::symlink_metadata(&target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((rel, None)),
        Err(e) => Err(format!("cannot tell whether {rel} exists: {e}")),
        Ok(m) if m.file_type().is_symlink() => {
            // Only the final component can still be a symlink at this point,
            // and a symlink inside the project pointing out of it is not the
            // project's file — `safe_resolve` is what refuses it.
            let real = crate::projects::safe_resolve(workspace, &rel)?;
            Ok((rel, Some(real)))
        }
        Ok(_) => Ok((rel, Some(target))),
    }
}

/// Park a change Claude is asking permission to make, and answer nothing yet.
///
/// **resh must not write the file.** On acceptance the CLI continues its own
/// tool call with the content this answer returns; a write here would be a
/// second write, and would leave `hub.self_writes` hashing content that never
/// reached disk.
fn open_diff(
    id: &serde_json::Value,
    args: &serde_json::Value,
    conn: &Conn,
) -> Option<serde_json::Value> {
    let path = args["old_file_path"].as_str().unwrap_or("");
    let new_raw = args["new_file_contents"].as_str().unwrap_or("");
    let tab_name = args["tab_name"].as_str().unwrap_or("proposal").to_string();

    // The two sides of one proposal get the same ceiling. The *old* side comes
    // through `projects::read_text_file`, which refuses anything over
    // `MAX_FILE_BYTES`; without this check the side Claude sends was bounded
    // only by `MAX_FRAME_BYTES`, four times larger. That gap is not academic:
    // `Hub::proposals` retains both sides for the life of the tab, and
    // `Event::Proposal` broadcasts a full copy to every connected browser, so
    // an 8 MB `new_file_contents` is 8 MB held plus 8 MB per browser — for a
    // file resh would have refused to *read* at a quarter of the size.
    // Checked before the path work below so an oversized frame costs no
    // filesystem calls, and before the `to_string` that would copy it again.
    if new_raw.len() as u64 > crate::projects::MAX_FILE_BYTES {
        return Some(err(
            id,
            -32602,
            format!(
                "new_file_contents is {} bytes, over the {} byte limit",
                new_raw.len(),
                crate::projects::MAX_FILE_BYTES
            ),
        ));
    }
    let new_text = new_raw.to_string();

    // Confined against the project, never against `conn.cwd`. The cwd is the
    // directory Claude is *in*, which may be a subdirectory of the project (or
    // a worktree outside it entirely); resolving against it would either
    // produce a rel the hub cannot open — the hub's paths are relative to the
    // project root — or widen the boundary past the project resh is serving.
    // It stays useful for what it was learned for (knowing which project a
    // connection belongs to), not for confinement.
    let (rel, abs) = match confined_target(&conn.workspace, path) {
        Ok(v) => v,
        Err(e) => return Some(err(id, -32602, format!("path outside project: {e}"))),
    };

    // A missing file is not an error: an `openDiff` for a file that does not
    // exist yet is Claude creating one, and the left-hand side is simply
    // empty. A file that exists but cannot be read is a different answer —
    // `confined_target` has already refused the case where resh could not
    // even tell, and an unreadable or oversized one is refused here rather
    // than drawn as an empty original.
    let old_text = match &abs {
        None => String::new(),
        Some(p) => match crate::projects::read_text_file(p) {
            Ok(t) => t,
            Err(e) => return Some(err(id, -32603, format!("cannot show a diff of {rel}: {e}"))),
        },
    };

    // resh may be holding unsaved edits to this very file. Accepting would
    // discard them silently, which is the one thing the whole conflict-guard
    // exists to prevent.
    if crate::hub::has_dirty_buffer(&conn.project, &rel) {
        return Some(err(id, -32002, format!("{rel} has unsaved changes in resh")));
    }

    let pid = new_pending_id(&conn.project);
    {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        // Per project, not global: `PENDING` is process-wide, and a Claude in
        // a loop in one project must not be able to make every other
        // project's proposals start bouncing.
        if map.values().filter(|p| p.project == conn.project).count() >= MAX_PENDING {
            return Some(ok(id, text_result("DIFF_REJECTED")));
        }
        map.insert(
            pid.clone(),
            Pending {
                project: conn.project.clone(),
                conn_id: conn.id,
                rpc_id: id.clone(),
                tab_name,
                reply: conn.reply.clone(),
            },
        );
    }

    // Outside the lock: this opens a tab and mirrors it to every browser, and
    // a pending proposal's lock is held for minutes if it is held at all.
    // Taking the hub lock only *after* releasing the pending one is also what
    // keeps the lock order total — see `Hub::do_answer_proposal`.
    crate::hub::open_proposal(&conn.project, &pid, &rel, &old_text, &new_text);

    // No reply yet, and crucially the read loop is not blocked: Claude must
    // still be able to send `close_tab`, and a user who walked away must not
    // wedge the socket.
    None
}

/// Claude withdrawing a proposal it no longer needs an answer to — the user
/// answered the permission prompt in the terminal instead, say.
///
/// The parked request is dropped without a reply on purpose: the side that
/// asked to close the tab is the side that was waiting, and it has already
/// stopped waiting.
fn close_tab(
    id: &serde_json::Value,
    args: &serde_json::Value,
    conn: &Conn,
) -> Option<serde_json::Value> {
    let name = args["tab_name"].as_str().unwrap_or("");
    let withdrawn: Vec<String> = {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, p)| p.project == conn.project && p.tab_name == name)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &ids {
            map.remove(k);
        }
        ids
    };
    // Outside the lock, for the same reason `open_diff` is.
    for k in &withdrawn {
        crate::hub::close_proposal(&conn.project, k);
    }
    Some(ok(id, text_result("TAB_CLOSED")))
}

fn err(id: &serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn ok(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn text_result(s: &str) -> serde_json::Value {
    serde_json::json!({"content": [{"type": "text", "text": s}]})
}

fn dispatch(msg: &serde_json::Value, conn: &mut Conn) -> Option<serde_json::Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg.get("id").cloned();

    // A message with no id is a notification: answering one is a protocol
    // error, not a harmless extra.
    let Some(id) = id else {
        if method == "ide_connected" {
            let pid = msg["params"]["pid"].as_u64().unwrap_or(0) as u32;
            match idecwd::cwd_of(pid) {
                Cwd::At(p) => conn.cwd = Some(p),
                // Gone and Unknown both leave cwd unset, and neither closes
                // the connection here: the socket itself is the evidence that
                // something is on the other end, and it is more trustworthy
                // than a /proc lookup that just failed.
                Cwd::Gone | Cwd::Unknown => {}
            }
        }
        return None;
    };

    match method {
        "initialize" => Some(ok(
            &id,
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "resh", "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "tools/list" => Some(ok(
            &id,
            serde_json::json!({"tools": [{
                "name": "getDiagnostics",
                "description": "Get language diagnostics from the editor",
                "inputSchema": {
                    "type": "object",
                    "properties": {"uri": {"type": "string"}},
                },
            }]}),
        )),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            match name {
                // resh has no language server. An empty list is the honest
                // answer and is what Claude sees when nothing is wrong — so
                // if a `cargo check` bridge ever lands, it lands here.
                "getDiagnostics" => Some(ok(&id, text_result("[]"))),
                // Returns `None`: the answer comes from a browser thread,
                // minutes from now. See `open_diff`.
                "openDiff" => open_diff(&id, &msg["params"]["arguments"], conn),
                "close_tab" => close_tab(&id, &msg["params"]["arguments"], conn),
                other => Some(err(&id, -32601, format!("resh does not implement {other}"))),
            }
        }
        "ping" => Some(ok(&id, serde_json::json!({}))),
        other => Some(err(&id, -32601, format!("unknown method {other}"))),
    }
}

fn serve_conn(stream: TcpStream, token: &str, project: &str, workspace: &Path) {
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    // Built before the handshake, not after: registration happens inside the
    // handshake callback below (see the comment there for why that ordering
    // is load-bearing, not cosmetic).
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    // Constructed before `accept_hdr_with_config` is even called, not after
    // it returns: both fields it needs (`project`, `conn_id`) already exist
    // by this point, and the callback below can register this connection
    // and then have tungstenite panic somewhere *inside* the handshake
    // machinery afterward — a case the previous version of this function
    // missed, since its guard did not exist yet when that could happen.
    // Building the guard this early means its `Drop` runs on every exit
    // from here on, panic included, so a registration can never outlive
    // this function without a guard in scope to remove it — the same
    // reasoning `wsconn.rs:145` gives for creating its own `UnsubGuard`
    // before, not after, the call whose failure it needs to survive.
    //
    // Revert-checked by hand, not with a permanent test (there is no clean
    // way to make tungstenite itself panic mid-handshake from a test): with
    // this declaration moved back to just before `let Ok(mut ws) =
    // accepted...` (its original position) and a `panic!()` temporarily
    // injected into the callback right after the `map.entry(...).push(...)`
    // line below, a fake client's connect attempt drove the callback to
    // register and then panic; the panic unwound through `serve_conn` with
    // no `guard` yet in scope, and a follow-up `mention(project, ...)`
    // returned `Ok(())` — a leaked, undeliverable target. Moving this
    // declaration back to here (unchanged) with the same injected panic
    // still in place made the same `mention` call correctly return
    // `Err("no Claude is connected to this project")`. Both the panic and
    // the temporary test used to observe this were removed afterward.
    let guard = ConnGuard { project: project.to_string(), id: conn_id };
    let reg_tx = reply_tx.clone();
    let reg_project = project.to_string();
    let accepted = accept_hdr_with_config(
        stream,
        move |req: &WsRequest, mut resp: WsResponse| {
            let deny = |why: &str| {
                eprintln!("resh: rejected ide ws handshake ({why})");
                tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("forbidden".to_string()))
                    .expect("static 403 response")
            };
            // See the module doc: on this socket an Origin is disqualifying.
            if req.headers().get("origin").is_some() {
                return Err(deny("carries an Origin, so it is a browser"));
            }
            let got = req
                .headers()
                .get("x-claude-code-ide-authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !ct_eq(got.as_bytes(), token.as_bytes()) {
                return Err(deny("token mismatch"));
            }
            // The CLI asks for the `mcp` subprotocol; a server that does not
            // echo it back is not guaranteed to be accepted by the client.
            resp.headers_mut().insert(
                "sec-websocket-protocol",
                tungstenite::http::HeaderValue::from_static("mcp"),
            );
            // Registered here, inside the handshake callback, and not after
            // `accept_hdr_with_config` returns: this callback runs and
            // returns *before* tungstenite writes the handshake response
            // bytes to the client. Registering after the call returned lost
            // the race in practice — a fake client's `connect()` can return
            // and this thread can call `mention` before the accepting
            // thread gets scheduled again, so `mention` found nothing and
            // `a_mention_is_the_notification_the_cli_expects` failed with
            // "no Claude is connected to this project" (see this task's
            // report for the actual failure output). Registering here
            // instead makes "the client observed a successful handshake"
            // happen-after "this connection is in `CONNS`", not merely
            // usually-before.
            let mut map = conns().lock().unwrap_or_else(|e| e.into_inner());
            map.entry(reg_project.clone()).or_default().push((conn_id, reg_tx.clone()));
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };

    // Everything from here on runs on this one thread through this one
    // `WebSocket`, and that is deliberate, not merely the simplest option.
    // The first cut of this function split read and write into a read half
    // and a writer thread owning a second `WebSocket` built from a
    // `try_clone`d file descriptor of the same socket — the shape
    // `wsconn.rs` and `term.rs` already use. Review caught that this socket
    // cannot tolerate it: tungstenite auto-replies to a Ping by queuing a
    // Pong and writing+flushing it on the *next* call to `read`/`write`/
    // `flush` on whichever `WebSocketContext` is asked (see tungstenite
    // 0.24's `protocol/mod.rs::read`, which calls `self.flush(stream)`
    // whenever `additional_send`/`unflushed_additional` is set) — so a Ping
    // arriving while the writer thread was mid-write on an `at_mentioned`
    // would have the read half's Pong reply interleave, frame-for-frame,
    // with it, corrupting the JSON-RPC stream Claude is parsing. There is no
    // config flag in this tungstenite version to suppress that auto-reply or
    // redirect it elsewhere (`WebSocketConfig` has no such field, and
    // `read`'s auto-reply is unconditional whenever a Ping/Close was
    // queued) — the only way to guarantee no two writers ever touch this
    // fd is to have only one. `wsconn.rs` and `term.rs` carry the identical
    // two-descriptor shape; see this task's report for why they are left
    // alone here rather than folded into this fix.
    //
    // A short read timeout is what lets one thread still deliver an
    // out-of-band `at_mentioned` (queued on `reply_rx` from some other
    // connection's hub thread, via `mention`) without a second writer: this
    // loop wakes on the timeout even when the CLI sends nothing, drains the
    // channel, and writes from the one object that owns the fd — bounding
    // an out-of-band notification's latency to at most one `POLL` interval
    // instead of "whenever the CLI next happens to send a frame."
    const POLL: std::time::Duration = std::time::Duration::from_millis(200);
    if ws.get_ref().set_read_timeout(Some(POLL)).is_err() {
        // Degrades rather than fails the connection outright, matching the
        // rest of this module's "IDE integration is a convenience" stance:
        // a `mention` for this connection now waits for the CLI's own next
        // frame (a periodic `ping` at minimum) instead of at most `POLL`.
        eprintln!("resh: could not set a read timeout on an ide connection; mentions to it may be delayed");
    }

    let mut conn = Conn::new(project, workspace.to_path_buf(), reply_tx.clone(), conn_id);
    // Only conn's and the registry's own clones should keep the channel
    // open from here — this one has done its job.
    drop(reply_tx);

    loop {
        // Drains anything queued for this connection from another thread —
        // in practice, only `mention`. A direct request/response reply is
        // written synchronously below instead, in the same call that
        // produced it (dispatch runs on this thread already, so there is no
        // reason to round-trip it through the channel). Either way, this
        // loop is the only place `ws.send`/`ws.read` is ever called for
        // this connection, so there is never a second writer to interleave
        // with — see the comment above for why that has to be true here.
        while let Ok(msg) = reply_rx.try_recv() {
            if ws.send(tungstenite::Message::Text(msg)).is_err() {
                drop(conn);
                drop(guard);
                return;
            }
        }
        let msg = match ws.read() {
            Ok(m) => m,
            // The `POLL` timeout expiring with nothing to read: loop back
            // around to drain `reply_rx` again rather than treating this as
            // a dead connection. Both `WouldBlock` and `TimedOut` are
            // checked because which one a timed-out read surfaces as is
            // platform-dependent (see `TcpStream::set_read_timeout`'s docs).
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        };
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(reply) = dispatch(&v, &mut conn) {
            if ws.send(tungstenite::Message::Text(reply.to_string())).is_err() {
                break;
            }
        }
        if conn.closed {
            break;
        }
    }

    // No writer thread to join any more — this thread was the only writer
    // throughout, which is the whole point of the restructuring above.
    drop(conn);
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    /// `impl IntoClientRequest for http::Request<()>` in tungstenite 0.24 is a
    /// bare pass-through (`Ok(self)`), and `generate_request` then *requires*
    /// Host/Connection/Upgrade/Sec-WebSocket-Version/Sec-WebSocket-Key to
    /// already be on the request — it does not add them. So this helper adds
    /// the boilerplate RFC 6455 headers itself, on top of the custom ones
    /// under test, rather than relying on the client to fill them in.
    fn connect(port: u16, token: Option<&str>, origin: Option<&str>) -> Result<(), String> {
        let mut b = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Protocol", "mcp");
        if let Some(t) = token {
            b = b.header("X-Claude-Code-Ide-Authorization", t);
        }
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        let req = b.body(()).unwrap().into_client_request().unwrap();
        tungstenite::connect(req).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Same request-building as `connect`, but keeps the handshake response
    /// instead of discarding it — needed to assert on a response *header*
    /// (the echoed subprotocol), which `connect`'s callers never inspect.
    fn connect_response(
        port: u16,
        token: Option<&str>,
        origin: Option<&str>,
    ) -> Result<tungstenite::http::Response<Option<Vec<u8>>>, String> {
        let mut b = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Protocol", "mcp");
        if let Some(t) = token {
            b = b.header("X-Claude-Code-Ide-Authorization", t);
        }
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        let req = b.body(()).unwrap().into_client_request().unwrap();
        tungstenite::connect(req).map(|(_, resp)| resp).map_err(|e| e.to_string())
    }

    fn started() -> (tempfile::TempDir, tempfile::TempDir, Arc<Ide>) {
        let lockdir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = start_in(lockdir.path(), "proj", ws.path().to_path_buf()).unwrap();
        (lockdir, ws, ide)
    }

    #[test]
    fn the_right_token_and_no_origin_connects() {
        let (_l, _w, ide) = started();
        connect(ide.port, Some(&ide.token), None).expect("the CLI's own shape must be accepted");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&"0".repeat(32)), None)
            .expect_err("a guessed token must not connect");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_missing_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, None, None).expect_err("no token, no socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_handshake_carrying_an_origin_is_refused_even_with_the_right_token() {
        // This is CVE-2025-52882 in one assertion. A browser is the only
        // thing that sends Origin, WebSocket handshakes bypass the
        // same-origin policy, and this socket can read files. A page that
        // somehow learned the token still must not get in.
        //
        // Note this is the *inverse* of origin.rs's rule for the workspace
        // socket, which refuses a handshake with no Origin. Both are right.
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&ide.token), Some("https://evil.example"))
            .expect_err("a browser must never reach this socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_loopback_origin_is_refused_too() {
        // The workspace socket allows loopback origins. This one does not:
        // resh's own page has no business here either, and allowing it would
        // reopen the hole for anything that can forge an origin.
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&ide.token), Some("http://127.0.0.1:8444"))
            .expect_err("a loopback origin must be refused too");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn starting_advertises_the_port_it_actually_bound() {
        let (lockdir, ws, ide) = started();
        let f = lockdir.path().join(format!("{}.lock", ide.port));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        assert_eq!(v["authToken"], ide.token.as_str());
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
        assert_ne!(ide.port, 0, "an OS-assigned port must be read back after bind");
    }

    #[test]
    fn the_server_echoes_the_mcp_subprotocol_the_client_asked_for() {
        // tungstenite's client does not itself verify the negotiated
        // subprotocol, so a missing echo would not show up as a connection
        // failure anywhere else in this file — it has to be checked directly
        // against the handshake response's own headers.
        let (_l, _w, ide) = started();
        let resp = connect_response(ide.port, Some(&ide.token), None)
            .expect("the right token and no origin must connect");
        assert_eq!(
            resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()),
            Some("mcp"),
            "the server must echo back the `mcp` subprotocol the client asked for"
        );
    }

    #[test]
    fn constant_time_compare_answers_correctly_including_on_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }

    fn rpc(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[test]
    // Revert-checked: changing `"resh"` to `"BROKEN"` in dispatch's
    // "initialize" arm failed only this test — `assertion `left == right`
    // failed / left: String("BROKEN") / right: "resh"` — then restored.
    fn initialize_answers_with_resh_as_the_server_name() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let out = dispatch(&rpc(1, "initialize", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 1);
        assert_eq!(out["result"]["serverInfo"]["name"], "resh");
        assert!(out["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn the_tool_list_offers_diagnostics_and_never_offers_code_execution() {
        // executeCode is one of only two tools the CLI makes visible to the
        // model, and it is arbitrary code execution reachable from this
        // socket. Adding it to the list is the defect this asserts against.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let out = dispatch(&rpc(2, "tools/list", serde_json::json!({})), &mut c).unwrap();
        let names: Vec<String> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["getDiagnostics".to_string()]);
    }

    #[test]
    // An empty success would read to Claude as "ran, produced nothing".
    // Revert-checked: adding an `"executeCode" => Some(ok(&id,
    // text_result("")))` arm ahead of the error fallthrough failed only this
    // test — `left: Null / right: -32601` on `out["error"]["code"]`, i.e. the
    // error object was simply absent — proving this test actually
    // distinguishes a refusal from an empty success rather than passing
    // vacuously. Then restored.
    fn calling_execute_code_is_a_method_error_not_an_empty_success() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let out = dispatch(
            &rpc(3, "tools/call", serde_json::json!({"name": "executeCode", "arguments": {"code": "1"}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["error"]["code"], -32601);
        assert!(
            out["error"]["message"].as_str().unwrap().contains("executeCode"),
            "the refusal must name what was refused: {}", out["error"]["message"]
        );
        assert!(out.get("result").is_none());
    }

    #[test]
    // Revert-checked: changing the "getDiagnostics" arm's `text_result("[]")`
    // to `text_result("[{\"bogus\":true}]")` failed only this test —
    // `left: String("[{\"bogus\":true}]") / right: "[]"` — then restored.
    fn diagnostics_answers_empty_rather_than_failing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let out = dispatch(
            &rpc(4, "tools/call", serde_json::json!({"name": "getDiagnostics", "arguments": {}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["result"]["content"][0]["type"], "text");
        assert_eq!(out["result"]["content"][0]["text"], "[]");
    }

    #[test]
    // Revert-checked: making the notification branch `return
    // Some(ok(&Value::Null, json!({})))` instead of `return None` failed
    // this test — `panicked ... "notifications get no response"` — exercising
    // the `is_none()` assertion. The sibling pid test below shares the same
    // notification branch and failed alongside it for the same reason
    // (`assertion failed: dispatch(&note, &mut c).is_none()`); both are
    // legitimate hits on the one break, not evidence of a false positive.
    // Then restored.
    fn ide_connected_resolves_the_senders_directory_and_is_not_answered() {
        // A notification has no id, so a reply would be a protocol error.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", std::env::current_dir().unwrap(), tx, 1);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected",
            "params": {"pid": std::process::id()}
        });
        assert!(dispatch(&note, &mut c).is_none(), "notifications get no response");
        assert_eq!(
            c.cwd.as_ref().unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    // Revert-checked: changing the `Cwd::Gone | Cwd::Unknown => {}` arm to
    // `=> conn.closed = true` failed only this test — `panicked ... "but the
    // connection stays open"` — proving the tri-state is load-bearing, not
    // decorative. Then restored.
    fn ide_connected_from_an_unreadable_pid_leaves_the_connection_usable() {
        // Cwd::Unknown must not disconnect. Folding it into "gone" would kill
        // a live Claude because a check failed.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": u32::MAX}
        });
        assert!(dispatch(&note, &mut c).is_none());
        assert!(c.cwd.is_none(), "no directory was learned");
        assert!(!c.closed, "but the connection stays open");
    }

    #[test]
    // Revert-checked: changing the unknown-method arm to `err(&Value::Null,
    // ...)` instead of `err(&id, ...)` failed only this test —
    // `left: Null / right: 9` on `out["id"]` — then restored.
    fn an_unknown_method_is_a_method_error_carrying_the_request_id() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx, 1);
        let out = dispatch(&rpc(9, "nonsense/method", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 9);
        assert_eq!(out["error"]["code"], -32601);
    }

    // --- Task 5: the per-project registry ---
    //
    // `REGISTRY` is process-global, so a project name shared across two
    // tests hands them the same listener and their pass/fail depends on
    // which one runs first — CLAUDE.md records exactly this shape of flake.
    // Every test below therefore gets a project name unique to it.

    /// Each test gets its own lock directory, so tests that run concurrently
    /// cannot see each other's lock files. Deliberately not an env var:
    /// `CLAUDE_CONFIG_DIR` is process-global, and CLAUDE.md records a
    /// "~1-in-8 flake" that was one test reaping another's state.
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn one_listener_per_project_not_one_per_connection() {
        let (dir, ws, _) = dirs();
        let a = for_project_in(dir.path(), "reuse-alpha", ws.path().to_path_buf()).unwrap();
        let again = for_project_in(dir.path(), "reuse-alpha", ws.path().to_path_buf()).unwrap();
        assert_eq!(a.port, again.port, "a second call must reuse the listener");
    }

    #[test]
    fn two_projects_get_two_ports_and_two_lock_files() {
        let (dir, wa, wb) = dirs();
        // One listener listing both roots would let a claude in beta be
        // handed alpha's socket: the CLI takes the first workspaceFolder that
        // contains its cwd.
        let a = for_project_in(dir.path(), "twoports-alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "twoports-beta", wb.path().to_path_buf()).unwrap();
        assert_ne!(a.port, b.port);
        assert!(dir.path().join(format!("{}.lock", a.port)).exists());
        assert!(dir.path().join(format!("{}.lock", b.port)).exists());
    }

    #[test]
    fn stopping_a_project_removes_its_lock_and_leaves_the_others() {
        let (dir, wa, wb) = dirs();
        let a = for_project_in(dir.path(), "stop-alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "stop-beta", wb.path().to_path_buf()).unwrap();
        let (a_port, b_port) = (a.port, b.port);
        // `for_project_in` hands back its own `Arc` clone, separate from the
        // one the registry map holds; `stop` can only drop the map's clone.
        // Production code (`hub.rs`) never keeps one of these around past
        // the initial call, so dropping it here is what makes this test
        // match how `stop` is actually used, rather than asserting against
        // a reference this test itself is still pinning alive.
        drop(a);
        drop(b);
        stop("stop-alpha");
        assert!(!dir.path().join(format!("{a_port}.lock")).exists());
        assert!(
            dir.path().join(format!("{b_port}.lock")).exists(),
            "closing one project must not unregister another"
        );
    }

    #[test]
    fn a_failure_to_write_the_lock_file_does_not_fail_the_project() {
        let (dir, ws, _) = dirs();
        // IDE integration is a convenience. A read-only home directory must
        // degrade it, never stop a project opening.
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "").unwrap();
        assert!(for_project_in(&blocked, "lockfail-alpha", ws.path().to_path_buf()).is_none());
    }

    #[test]
    // Task 3's review: dropping the `Arc<Ide>` removed the lock file but
    // left the accept loop running, authenticating against a token nothing
    // advertises any more. `stop` must actually terminate the listener, not
    // just forget about it.
    //
    // Revert-checked: emptying `request_shutdown`'s body (so `stop` still
    // removes the lock file and the registry entry, but never signals the
    // accept thread) failed this test — `panicked ... "port {port} is still
    // accepting connections after stop()"`, after genuinely waiting out the
    // 5s poll deadline rather than passing vacuously — then restored.
    fn stop_actually_closes_the_listening_socket() {
        let (dir, ws, _) = dirs();
        let ide = for_project_in(dir.path(), "stopclose-alpha", ws.path().to_path_buf()).unwrap();
        let port = ide.port;
        drop(ide); // the registry still holds its own Arc; this must not matter
        stop("stopclose-alpha");
        // The accept thread only observes the shutdown once it loops back
        // around `incoming()` and drops the listener — not synchronously
        // inside `stop` — so poll rather than assert immediately.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Err(_) => return, // refused: the listener is gone
                Ok(_) if std::time::Instant::now() >= deadline => {
                    panic!("port {port} is still accepting connections after stop()")
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

    // --- Task 6: at_mentioned ---
    //
    // `CONNS` is process-global like `REGISTRY`, so every test below (and
    // every fake client it connects) gets a project name unique to it — the
    // same discipline Task 5's tests already established.

    /// What a test asks its fake client's socket thread to do. `Close` exists
    /// because "Claude went away" is a case with its own cleanup path, and the
    /// only honest way to reach it is to actually drop the socket.
    enum ClientCmd {
        Text(String),
        Close,
    }

    /// Test-only: the send half of each fake client's socket, so a test can
    /// speak as Claude without owning the socket itself.
    static SENDERS: OnceLock<Mutex<HashMap<String, std::sync::mpsc::Sender<ClientCmd>>>> =
        OnceLock::new();

    fn senders() -> &'static Mutex<HashMap<String, std::sync::mpsc::Sender<ClientCmd>>> {
        SENDERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Connects a client the way the CLI does and returns (frames it
    /// receives, the live Ide, the lock dir, the workspace dir). Holding the
    /// returned `Arc<Ide>` matters: it owns the lock file, and dropping it
    /// unregisters the project.
    ///
    /// Both temp dirs are returned, not just the lock dir: the workspace one
    /// (`ws` below) used to be dropped — and so deleted — before this
    /// function even returned, since it was never in the returned tuple.
    /// Harmless for Task 6 (nothing here reads the workspace directory back),
    /// but Task 7's `openDiff` fixtures write into it and need it to still
    /// exist for the life of the test.
    fn connected_fake_client_for(
        project: &str,
    ) -> (std::sync::mpsc::Receiver<String>, Arc<Ide>, tempfile::TempDir, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = for_project_in(dir.path(), project, ws.path().to_path_buf()).unwrap();
        // `IntoClientRequest for http::Request<()>` is a pass-through: it adds
        // nothing. `generate_request` requires Host, Connection, Upgrade,
        // Sec-WebSocket-Version and Sec-WebSocket-Key to be present already and
        // errors without the key (tungstenite 0.24 handshake/client.rs:111-135),
        // so a hand-built request must carry all five itself.
        let req = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{}/", ide.port))
            .header("Host", format!("127.0.0.1:{}", ide.port))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Protocol", "mcp")
            .header("X-Claude-Code-Ide-Authorization", &ide.token)
            .body(())
            .unwrap()
            .into_client_request()
            .unwrap();
        let (mut sock, _) = tungstenite::connect(req).expect("the fake client must connect");
        // A read timeout, for the same reason `serve_conn` sets one: this
        // thread is the only owner of the socket, so it can only notice
        // something queued for sending between reads. Without it the loop
        // below parks in `read()` on the very first pass and every send a
        // test makes sits in the channel forever — which is exactly what
        // happened when Task 7's tests became the first ones to send
        // anything at all (Task 6's only ever received).
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = sock.get_ref() {
            s.set_read_timeout(Some(std::time::Duration::from_millis(20)))
                .expect("a fake client must be able to poll its own socket");
        } else {
            panic!("the fake client must be on a plain loopback socket");
        }
        let (tx, rx) = std::sync::mpsc::channel();
        // The test drives sends through this channel too, so one thread owns
        // the socket and the test never races itself.
        let (send_tx, send_rx) = std::sync::mpsc::channel::<ClientCmd>();
        std::thread::spawn(move || loop {
            while let Ok(out) = send_rx.try_recv() {
                match out {
                    ClientCmd::Text(t) => {
                        if sock.send(tungstenite::Message::Text(t)).is_err() {
                            return;
                        }
                    }
                    // Returning drops `sock`, which closes the TCP connection
                    // — the same thing a `claude` that exited does.
                    ClientCmd::Close => return,
                }
            }
            match sock.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    if tx.send(t).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                // The poll expiring with nothing to read: loop back around to
                // drain `send_rx`, rather than treating it as a dead socket.
                // Both kinds are checked because which one a timed-out read
                // surfaces as is platform-dependent.
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return,
            }
        });
        senders().lock().unwrap().insert(project.to_string(), send_tx);
        (rx, ide, dir, ws)
    }

    /// Speaks as Claude on a fake client's socket. The client thread owns the
    /// socket, so a test never races itself on it.
    fn send_from_claude(project: &str, v: &serde_json::Value) {
        let tx = {
            let map = senders().lock().unwrap_or_else(|e| e.into_inner());
            map.get(project).cloned().expect("connect a fake client for this project first")
        };
        tx.send(ClientCmd::Text(v.to_string()))
            .expect("the fake client's socket thread must still be alive");
    }

    /// Hangs up a fake client's socket, the way a `claude` process exiting
    /// does. The server notices on its own read loop and drops its `ConnGuard`.
    fn disconnect_fake_client(project: &str) {
        let tx = {
            let map = senders().lock().unwrap_or_else(|e| e.into_inner());
            map.get(project).cloned().expect("connect a fake client for this project first")
        };
        let _ = tx.send(ClientCmd::Close);
    }

    /// Distinct ids per request, so a test that sends several can tell the
    /// answers apart.
    fn next_rpc_id() -> i64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
        NEXT.fetch_add(1, Ordering::Relaxed) as i64
    }

    fn send_open_diff(project: &str, path: &str, contents: &str, tab: &str) {
        send_from_claude(
            project,
            &rpc(
                next_rpc_id(),
                "tools/call",
                serde_json::json!({
                    "name": "openDiff",
                    "arguments": {
                        "old_file_path": path,
                        "new_file_path": path,
                        "new_file_contents": contents,
                        "tab_name": tab,
                    }
                }),
            ),
        );
    }

    /// Sends an `openDiff` and waits for it to be *parked* — which is the
    /// thing under test everywhere below: a request that produced a pending
    /// entry and no reply. Polls rather than sleeping a fixed amount, and
    /// fails loudly rather than returning a made-up id.
    fn open_diff_from_claude(project: &str, path: &str, contents: &str, tab: &str) -> String {
        send_open_diff(project, path, contents, tab);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(id) = pending_id_for_tab(project, tab) {
                return id;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "openDiff for {tab:?} never parked a pending request"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    // Revert-checked: making `Answer::Accepted` emit `DIFF_REJECTED` failed
    // only this test — `left: String("DIFF_REJECTED") / right: "TAB_CLOSED"`
    // — then restored. These three strings are the CLI's entire vocabulary
    // for this call and it treats them as opposite instructions, so each
    // needs its own assertion.
    fn accepting_unchanged_answers_tab_closed() {
        let proj = "diff-1";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        std::fs::write(&f, "original").unwrap();
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "new contents", "proposal-1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "TAB_CLOSED");
        assert!(v["result"]["content"].get(1).is_none(), "an unedited accept carries no content");
    }

    #[test]
    // The second element is how "the user changed my proposal before
    // accepting" reaches Claude. Dropping it makes Claude write the text the
    // user rejected.
    //
    // Revert-checked: making `Answer::AcceptedEdited` emit only the
    // `FILE_SAVED` element failed this test — `left: Null / right: "the
    // human's version"` on `content[1]["text"]` — and its hub-level sibling
    // `accepting_an_edited_proposal_sends_the_users_own_text_back_...` for
    // the same reason. Then restored.
    fn accepting_an_edited_proposal_answers_file_saved_and_the_edited_text() {
        let proj = "diff-2";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let pending =
            open_diff_from_claude(proj, f.to_str().unwrap(), "claude's version", "proposal-1");
        answer(proj, &pending, Answer::AcceptedEdited("the human's version".into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "FILE_SAVED");
        assert_eq!(v["result"]["content"][1]["text"], "the human's version");
    }

    #[test]
    // Revert-checked: making `Answer::Rejected` emit `TAB_CLOSED` failed this
    // test — `left: String("TAB_CLOSED") / right: "DIFF_REJECTED"` — along
    // with the two hub tests that reach the same arm. Then restored. Accept
    // and reject are opposite instructions to the CLI, so a test that only
    // checked "an answer arrived" would be worthless.
    fn rejecting_answers_diff_rejected_and_is_not_the_same_as_accepting() {
        let proj = "diff-3";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "x", "proposal-1");
        answer(proj, &pending, Answer::Rejected).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "DIFF_REJECTED");
    }

    #[test]
    // The CLI applies the edit with the content this answer returns
    // (`updatedInput`). A write here would be a second write, and would leave
    // hub.self_writes hashing content that never reached disk.
    //
    // Revert-checked: adding `if let Some(p) = &abs { let _ =
    // std::fs::write(p, &new_text); }` to `open_diff` — the natural place a
    // second write would creep in — failed only this test — `left:
    // "proposed" / right: "original"` — then restored.
    fn resh_never_writes_the_file_itself() {
        let proj = "diff-4";
        let (_rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        std::fs::write(&f, "original").unwrap();
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "proposed", "p1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");
    }

    #[test]
    // If openDiff blocked the read loop, Claude could not send close_tab to
    // withdraw it, and a user who walked away would wedge the socket.
    //
    // Revert-checked: making `open_diff` block on a channel receive
    // (`let (_t, r) = std::sync::mpsc::channel::<()>(); let _ = r.recv();`)
    // instead of returning `None` failed this test — `called
    // Result::unwrap() on an Err value: Timeout` on the `recv_timeout` —
    // then restored. Worth knowing for whoever repeats it: that break also
    // *hangs* the rest of this module's tests rather than failing them,
    // because the blocked read loop is also the only thing that drains this
    // connection's reply channel, so `cargo test --lib ide::` never
    // terminates and has to be run against this one test alone. That is the
    // shape CLAUDE.md warns about — a defect that hangs instead of failing —
    // and it is why this test asserts against a bounded `recv_timeout`
    // rather than a bare `recv`.
    fn the_read_loop_is_not_blocked_while_a_proposal_is_pending() {
        let proj = "diff-5";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let _pending = open_diff_from_claude(proj, f.to_str().unwrap(), "x", "p1");
        send_from_claude(proj, &rpc(77, "ping", serde_json::json!({})));
        let v: serde_json::Value =
            serde_json::from_str(&rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap())
                .unwrap();
        assert_eq!(v["id"], 77, "a later request must be answered while a proposal is open");
    }

    #[test]
    // Two browsers mirror this state; both can click.
    //
    // Revert-checked: making `answer` read the entry without removing it
    // (cloning the `Sender` and the id out under the lock and leaving the map
    // untouched) failed only this test — `panicked ... "first answer wins"` —
    // then restored.
    fn a_second_answer_to_one_proposal_is_refused() {
        let proj = "diff-6";
        let (_rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "x", "p1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        let e = answer(proj, &pending, Answer::Rejected).expect_err("first answer wins");
        assert!(e.contains("already answered"), "the refusal must say why: {e}");
    }

    #[test]
    // An id is only meaningful inside the project that parked it. Without the
    // project check, one project's browser could answer another's proposal by
    // guessing a counter value.
    //
    // Revert-checked: dropping the `p.project == project` guard (matching on
    // `Some(_)` alone) failed only this test — `panicked ... "an id from
    // another project must not be answerable"` — then restored.
    fn a_proposal_cannot_be_answered_from_another_project() {
        let (_rx, _ide, _d, ws) = connected_fake_client_for("diff-scope-a");
        let (_rx2, _ide2, _d2, _ws2) = connected_fake_client_for("diff-scope-b");
        let f = ws.path().join("a.rs");
        let pending = open_diff_from_claude("diff-scope-a", f.to_str().unwrap(), "x", "p1");
        let e = answer("diff-scope-b", &pending, Answer::Accepted)
            .expect_err("an id from another project must not be answerable");
        assert!(e.contains("another project"), "the refusal must say why: {e}");
        // And the real owner can still answer it: the refusal must not have
        // consumed the proposal on its way out.
        answer("diff-scope-a", &pending, Answer::Accepted).unwrap();
    }

    #[test]
    // The spec's cap, in the spirit of the existing <=16-session and
    // <=50-buffer caps. Queueing would let a Claude in a loop hold unbounded
    // content in resh's memory; DIFF_REJECTED is a refusal Claude already
    // knows how to handle.
    //
    // Revert-checked twice, because the cap has two halves. Making the check
    // never trip (`if false`) failed this test — `no answer at all: Timeout`,
    // after genuinely waiting out the 10s `recv_timeout` rather than passing
    // vacuously — since a queued 17th proposal answers nothing at all.
    // Separately, keeping the refusal but parking the request alongside it
    // failed the second assertion — `a refused proposal must not also be
    // parked`. Then restored.
    fn pending_proposals_are_capped_and_the_overflow_is_refused_not_queued() {
        let proj = "diff-cap";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let path = f.to_str().unwrap().to_string();
        for i in 0..MAX_PENDING {
            open_diff_from_claude(proj, &path, "x", &format!("tab-{i}"));
        }
        send_from_claude(
            proj,
            &rpc(
                999,
                "tools/call",
                serde_json::json!({
                    "name": "openDiff",
                    "arguments": {
                        "old_file_path": path, "new_file_path": path,
                        "new_file_contents": "one too many", "tab_name": "overflow",
                    }
                }),
            ),
        );
        let v = loop {
            let v: serde_json::Value = serde_json::from_str(
                &rx.recv_timeout(std::time::Duration::from_secs(10)).expect("no answer at all"),
            )
            .unwrap();
            if v["id"] == 999 {
                break v;
            }
        };
        assert_eq!(
            v["result"]["content"][0]["text"], "DIFF_REJECTED",
            "over the cap, answer immediately rather than parking the request"
        );
        assert!(
            pending_id_for_tab(proj, "overflow").is_none(),
            "a refused proposal must not also be parked"
        );
    }

    #[test]
    // Create a real file at the escape target, or this errors with ENOENT
    // before reaching the confinement check and proves nothing.
    //
    // Revert-checked: replacing `confined_target`'s body with a bare
    // `Ok((path.trim_start_matches('/').to_string(), None))` failed this test
    // — `called Result::unwrap() on an Err value: Timeout`, because the
    // escape was parked instead of refused and so answered nothing — along
    // with all five `confined_target` unit tests below. Then restored.
    fn a_path_outside_the_project_is_refused_without_opening_a_tab() {
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        std::fs::write(&victim, "not yours").unwrap();
        let proj = "diff-7";
        let (rx, _ide, _d, _ws) = connected_fake_client_for(proj);
        send_open_diff(proj, victim.to_str().unwrap(), "x", "p1");
        let v: serde_json::Value =
            serde_json::from_str(&rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap())
                .unwrap();
        assert!(v["error"].is_object(), "expected a refusal, got {v}");
        assert!(pending_id_for_tab(proj, "p1").is_none(), "and nothing may be parked");
    }

    #[test]
    // The *old* side of a proposal goes through `projects::read_text_file` and
    // is refused over `MAX_FILE_BYTES`; the new side used to be bounded only by
    // `MAX_FRAME_BYTES`, 4x larger — and both sides are retained by
    // `Hub::proposals` for the tab's life and broadcast whole to every browser.
    //
    // The at-the-limit control is what makes this discriminating: without it,
    // a refusal for *any* reason (a bad path, an unwritable fixture, the frame
    // cap) would pass the first half. Same project, same file, same code path,
    // one byte apart — so the only thing that can distinguish them is the cap.
    //
    // Revert-checked: deleting the `new_raw.len()` guard from `open_diff`
    // failed this test — `no answer to the oversized openDiff: Timeout`, after
    // genuinely waiting out the full 10s `recv_timeout` rather than passing
    // vacuously. The failure lands there rather than on the `is_object`
    // assertion because without the guard the oversized request is *parked*,
    // and a parked request answers nothing at all. Then restored.
    fn an_oversized_new_file_contents_is_refused_and_names_the_limit() {
        let proj = "diff-big";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        std::fs::write(&f, "original").unwrap();
        let path = f.to_str().unwrap().to_string();

        let too_big = "x".repeat(crate::projects::MAX_FILE_BYTES as usize + 1);
        send_from_claude(
            proj,
            &rpc(
                4242,
                "tools/call",
                serde_json::json!({
                    "name": "openDiff",
                    "arguments": {
                        "old_file_path": path, "new_file_path": path,
                        "new_file_contents": too_big, "tab_name": "oversized",
                    }
                }),
            ),
        );
        let v = loop {
            let v: serde_json::Value = serde_json::from_str(
                &rx.recv_timeout(std::time::Duration::from_secs(10))
                    .expect("no answer to the oversized openDiff"),
            )
            .unwrap();
            if v["id"] == 4242 {
                break v;
            }
        };
        assert!(v["error"].is_object(), "oversized new_file_contents must be refused, got {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains(&crate::projects::MAX_FILE_BYTES.to_string()),
            "the refusal must name the limit so Claude can act on it, got {msg:?}"
        );
        assert!(
            pending_id_for_tab(proj, "oversized").is_none(),
            "a refused proposal must not also be parked"
        );

        // Exactly at the limit — one byte less than the refusal above, and
        // otherwise identical in every respect: this one parks normally.
        let at_limit = "y".repeat(crate::projects::MAX_FILE_BYTES as usize);
        let pending = open_diff_from_claude(proj, &path, &at_limit, "at-limit");
        assert!(
            answer(proj, &pending, Answer::Rejected).is_ok(),
            "a proposal exactly at the limit is a normal proposal"
        );
    }

    #[test]
    // Revert-checked: making `close_tab` answer `TAB_CLOSED` without removing
    // anything from the pending map failed only this test — `panicked ...
    // "close_tab never withdrew the proposal"`, after waiting out its full 5s
    // deadline — then restored.
    fn close_tab_withdraws_a_pending_proposal() {
        let proj = "diff-8";
        let (_rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let f = ws.path().join("a.rs");
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "x", "proposal-1");
        send_from_claude(
            proj,
            &rpc(
                5,
                "tools/call",
                serde_json::json!({"name": "close_tab", "arguments": {"tab_name": "proposal-1"}}),
            ),
        );
        // The withdrawal happens on the connection thread, so wait for it
        // rather than assuming this thread lost the race.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pending_id_for_tab(proj, "proposal-1").is_some() {
            assert!(std::time::Instant::now() < deadline, "close_tab never withdrew the proposal");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            answer(proj, &pending, Answer::Accepted).is_err(),
            "a withdrawn proposal cannot be answered"
        );
    }

    /// A proposal nobody answers must not outlive the socket that asked for
    /// it. The click path already self-heals (`answer` finds a dead channel),
    /// but only if a human clicks: an ignored tab would hold its slot for the
    /// life of the process, and a few CLI restarts would then leave every
    /// `openDiff` refused with nothing visible to explain it.
    ///
    /// Goes through a real hub, not just the pending map, because the tab is
    /// half of what leaks — and through a real socket close, because the id
    /// carried in `Pending` has to be the one the guard was built with. A test
    /// that parked its own row with a hand-picked id would pass even if
    /// `open_diff` recorded the wrong connection entirely.
    ///
    /// Revert-checked: removing the `reclaim_pending_for` call from
    /// `ConnGuard::drop` failed this test — `panicked ... "a dead
    /// connection's proposal still holds its slot"` after the full 5s
    /// deadline. Separately, keeping the call but dropping its
    /// `hub::close_proposal` loop failed the last assertion — `panicked ...
    /// "the tab outlived the connection that asked"`. Then restored.
    #[test]
    fn a_dead_connection_reclaims_its_pending_proposals_and_their_tabs() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let proj = "diff-orphan";
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        // The shared stable directory, not a per-test `TempDir`:
        // `set_ide_dir_for_test` sets a process-global `OnceLock`, so a
        // directory handed to it outlives this test and every later write
        // recreates it after the `TempDir` is gone — one leaked directory per
        // test process. `hub`'s helper owns the stable path; reuse it.
        idelock::isolate_ide_dir_for_test();
        // The hub first: it is what creates this project's ide listener, so
        // the fake client below joins that one rather than a second.
        let hub = crate::hub::Hub::for_project(proj, d.path().to_path_buf());
        let (_rx, _ide, _l, _w) = connected_fake_client_for(proj);
        let f = d.path().join("a.rs");
        std::fs::write(&f, "on disk").unwrap();

        let id = open_diff_from_claude(proj, f.to_str().unwrap(), "proposed", "p1");
        assert!(
            crate::hub::Hub::lock(&hub).proposals.contains_key(&id),
            "the proposal must be open before this test can say anything about it going away"
        );

        disconnect_fake_client(proj);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pending_id_for_tab(proj, "p1").is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "a dead connection's proposal still holds its slot"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let h = crate::hub::Hub::lock(&hub);
        assert!(
            !h.ws.panes.iter().any(|p| p.tabs.iter().any(|t| matches!(t, crate::proto::Tab::Proposal { .. }))),
            "the tab outlived the connection that asked"
        );
        assert!(h.proposals.is_empty(), "and so did the content behind it");
        drop(h);
        std::env::remove_var("RESH_STATE_DIR");
    }

    // --- confinement, which `openDiff` cannot express over the wire ---
    //
    // Every one of these creates a real file at the target, so a failure is
    // the confinement check refusing and never an ENOENT on the way to it —
    // the trap CLAUDE.md records as the reason a symlink escape once survived
    // review.

    #[test]
    fn an_existing_file_inside_the_project_resolves_to_its_rel_and_its_real_path() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir(ws.path().join("src")).unwrap();
        let f = ws.path().join("src/a.rs");
        std::fs::write(&f, "hi").unwrap();
        let (rel, abs) = confined_target(ws.path(), f.to_str().unwrap()).unwrap();
        assert_eq!(rel, "src/a.rs");
        assert_eq!(abs.unwrap().canonicalize().unwrap(), f.canonicalize().unwrap());
    }

    #[test]
    // An openDiff for a file that does not exist yet is Claude creating one.
    // Refusing it would make "write a new file" the one edit resh cannot
    // show, and the left-hand side of the diff is simply empty.
    fn a_file_that_does_not_exist_yet_is_confined_but_has_no_disk_side() {
        let ws = tempfile::tempdir().unwrap();
        let (rel, abs) =
            confined_target(ws.path(), ws.path().join("new.rs").to_str().unwrap()).unwrap();
        assert_eq!(rel, "new.rs");
        assert!(abs.is_none(), "nothing on disk to diff against yet");
    }

    #[test]
    // A symlink inside the project pointing out of it is not the project's
    // file. `Path::exists()` following it is the eleventh defect in
    // CLAUDE.md's table, in a different module.
    fn a_symlink_out_of_the_project_is_refused_even_though_its_target_exists() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        // The target really exists: without this the refusal below could be a
        // plain ENOENT and would prove nothing about confinement.
        std::fs::write(&victim, "not yours").unwrap();
        let link = ws.path().join("link.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(std::fs::read_to_string(&link).is_ok(), "the escape target must be reachable");
        let e = confined_target(ws.path(), link.to_str().unwrap())
            .expect_err("a symlink out of the project must be refused");
        assert!(e.contains("outside"), "the refusal must say why: {e}");
    }

    #[test]
    fn a_traversal_through_a_real_directory_outside_the_project_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        std::fs::write(&victim, "not yours").unwrap();
        // `<ws>/../<outside-dir-name>/secret.txt` — a real, readable file,
        // reached by a path that starts inside the project. Both temp dirs
        // share a parent, which is what makes one `..` enough.
        let sneaky = format!(
            "{}/../{}/secret.txt",
            ws.path().display(),
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        assert!(std::fs::read_to_string(&sneaky).is_ok(), "the escape target must be reachable");
        // The message, not just `is_err()`: this path exists on disk and is
        // readable, so an `Err` here could still be a bug elsewhere in
        // `confined_target` rather than the confinement doing its job.
        //
        // Revert-checked: changing `abs_to_rel`'s outside-the-project message
        // to a bare "refused" failed this assertion — `the refusal must say
        // why: refused` — while `is_err()` alone would have stayed green.
        // Then restored.
        let e = confined_target(ws.path(), &sneaky).expect_err("traversal must not escape");
        assert!(e.contains("outside"), "the refusal must say why: {e}");
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_joined_onto_the_project() {
        // openDiff sends absolute paths. A relative one is a client resh does
        // not understand, and guessing at a base for it is how a confinement
        // check gets skipped.
        let ws = tempfile::tempdir().unwrap();
        // The file exists and the rel is the one that *would* work, so the
        // refusal below can only be about the path not being absolute.
        //
        // Revert-checked: deleting the `is_absolute` guard failed this
        // assertion — `the refusal must say why, and must not be an
        // incidental ENOENT: no such file` — which is the exact trap
        // CLAUDE.md names: the bare `is_err()` this replaced was passing on
        // an ENOENT that never reached a confinement check. Then restored.
        std::fs::write(ws.path().join("a.rs"), "hi").unwrap();
        for bad in ["a.rs", "./a.rs", ""] {
            let e = confined_target(ws.path(), bad)
                .expect_err("a relative path must be refused, not joined onto the project");
            assert!(
                e.contains("not an absolute path"),
                "the refusal must say why, and must not be an incidental ENOENT: {e}"
            );
        }
    }

    /// Revert-checked: adding `"id": 1` to `mention`'s constructed message
    /// (turning the notification into something shaped like a request)
    /// failed only this test, on the `id.is_none()` assertion — the actual
    /// panic: `"a notification must carry no id or the CLI will wait for a
    /// reply it never gets"`. Then restored.
    #[test]
    fn a_mention_is_the_notification_the_cli_expects() {
        let (rx, ide, _d, _ws) = connected_fake_client_for("mention-shape");
        mention("mention-shape", Path::new("/w/src/hub.rs"), Some((12, 40))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["method"], "at_mentioned");
        assert_eq!(v["params"]["filePath"], "/w/src/hub.rs");
        assert_eq!(v["params"]["lineStart"], 12);
        assert_eq!(v["params"]["lineEnd"], 40);
        assert!(v.get("id").is_none(), "a notification must carry no id or the CLI will wait for a reply it never gets");
        let _ = ide;
    }

    /// Revert-checked: changing `mention` to always set `lineStart`/`lineEnd`
    /// (defaulting to `(0, 0)` when `lines` is `None`) failed only this test —
    /// `assertion failed: v["params"].get("lineStart").is_none()` — then
    /// restored. Without this test a whole-file mention would silently start
    /// carrying a bogus line range the CLI would render as a real selection.
    #[test]
    fn a_whole_file_mention_carries_no_line_numbers() {
        let (rx, _ide, _d, _ws) = connected_fake_client_for("mention-wholefile");
        mention("mention-wholefile", Path::new("/w/README.md"), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert!(v["params"].get("lineStart").is_none());
    }

    /// Revert-checked: disabling `mention`'s `targets.is_empty()` check (so
    /// an empty target list falls through to `Ok(())` instead of refusing)
    /// failed this test — `called Result::unwrap_err() on an Ok value: ()` —
    /// and, for the same reason, also failed the sibling
    /// `a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude`
    /// test below, which exercises the same empty-target path from the other
    /// direction (a connection that *was* registered and then deregistered,
    /// rather than one that was never registered at all). Both are
    /// legitimate hits on the one break, not evidence of a false positive —
    /// see CLAUDE.md's note on the `ide_connected` notification test for the
    /// same pattern. Then restored.
    #[test]
    fn mentioning_with_no_claude_connected_is_an_error_not_a_panic() {
        // The tree's keybinding is always available; Claude is not. This must
        // surface as a refusal the UI can show, never as a socket-thread panic.
        let err = mention("nobody-here", Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("no Claude"), "the message must say what is missing: {err}");
    }

    /// Revert-checked (Task 6's brief, Step 6): changing `for t in &targets`
    /// to send only to `targets.first()` failed only this test, and only on
    /// `rx2` — `rx1.recv_timeout` still succeeded (it got the first sender),
    /// `rx2.recv_timeout` timed out. With a single subscriber "send to all"
    /// and "send to the first" are indistinguishable, which is exactly the
    /// trap CLAUDE.md records; that is why this test uses two clients. Then
    /// restored.
    #[test]
    fn a_mention_reaches_every_connected_claude_not_just_the_first() {
        // Two terminals, two claudes, one project. Sending to only the first
        // is indistinguishable from sending to all with a single subscriber —
        // which is exactly the trap CLAUDE.md records.
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("mention-fanout");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("mention-fanout");
        mention("mention-fanout", Path::new("/w/x.rs"), None).unwrap();
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }

    /// A dead connection (its read loop already broke, panicked or not) must
    /// not leave a phantom target in `CONNS` — `mention` would then report
    /// success into a void nothing will ever deliver. Exercises `ConnGuard`
    /// directly rather than tearing down a real socket, so the assertion is
    /// about the registry's own bookkeeping, not about socket teardown timing.
    ///
    /// Revert-checked: emptying `ConnGuard::drop`'s body (so registering still
    /// happens but deregistering does not) failed this test — `mention`
    /// returned `Ok(())` instead of the expected `Err`, because the dead
    /// sender was still counted as a live target. Then restored.
    #[test]
    fn a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude() {
        let project = "mention-guard-drop";
        let (tx, _rx) = std::sync::mpsc::channel::<String>();
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut map = conns().lock().unwrap();
            map.entry(project.to_string()).or_default().push((id, tx));
        }
        let guard = ConnGuard { project: project.to_string(), id };
        // Sanity: the registration really did take effect before the drop
        // this test is actually about.
        assert!(mention(project, Path::new("/w/x.rs"), None).is_ok());
        drop(guard);
        let err = mention(project, Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("no Claude"), "the registry entry must be gone once the guard drops: {err}");
    }

    /// The struct is `Settings`, not `Config`. The default must be off, or a
    /// highlighted line of `.env` leaves the host the moment someone selects
    /// it, with no config file ever written.
    ///
    /// Revert-checked: flipping `Settings::default()`'s `share_selection` to
    /// `true` failed this test (`assertion failed: !cfg.share_selection`) —
    /// and, for the same reason, also failed
    /// `config::tests::share_selection_defaults_off_and_either_layer_can_
    /// turn_it_on` and `render::tests::share_selection_is_off_by_default_
    /// and_the_indicator_appears_only_when_it_is_on`, three legitimate hits
    /// on the one default. Then restored.
    #[test]
    fn selection_sharing_is_off_unless_a_project_opts_in() {
        let cfg = crate::config::Settings::default();
        assert!(!cfg.share_selection, "a highlighted line of .env must not leave the host by default");
    }

    /// Writes `share_selection = true` into the fake client's own project
    /// directory (`connected_fake_client_for`'s `ws`, which is what
    /// `Hub::dir` — and so this call's `project_dir` — actually is) so the
    /// opt-in check inside `selection_changed` passes.
    fn opt_in(ws: &Path) {
        std::fs::create_dir_all(ws.join(".resh")).unwrap();
        std::fs::write(ws.join(".resh/config.toml"), "share_selection = true\n").unwrap();
    }

    #[test]
    // Revert-checked: constructing `selection_changed`'s message with
    // `"method": "at_mentioned"` (the neighboring notification's method name,
    // a plausible copy-paste) failed only this test's first assertion —
    // `left: "at_mentioned" / right: "selection_changed"` — then restored.
    fn a_shared_selection_is_the_notification_the_cli_expects() {
        let proj = "selection-shape";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        opt_in(ws.path());
        selection_changed(proj, ws.path(), Path::new("/w/a.rs"), "let x = 1;", (10, 4), (10, 14))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["method"], "selection_changed");
        assert_eq!(v["params"]["text"], "let x = 1;");
        assert_eq!(v["params"]["selection"]["start"]["line"], 10);
        assert_eq!(v["params"]["selection"]["start"]["character"], 4);
        assert_eq!(v["params"]["selection"]["end"]["line"], 10);
        assert_eq!(v["params"]["selection"]["end"]["character"], 14);
        assert!(v.get("id").is_none(), "a notification must carry no id or the CLI will wait for a reply it never gets");
    }

    /// Both halves matter: an assertion that only checks the return value
    /// cannot tell "refused" from "refused after sending" — a bug that sent
    /// the text and *then* returned an error would still look green with
    /// only the `is_err()` half.
    ///
    /// Revert-checked: moving the `share_selection` check to *after*
    /// `notify_all` (so the notification goes out before the refusal is
    /// noticed) failed only the second assertion here — `rx.try_recv()`
    /// returned `Ok("secret"...)` instead of the expected `Err` — while the
    /// first assertion (`is_err()`) stayed green throughout, which is exactly
    /// the false-confidence this test exists to catch. Then restored.
    #[test]
    fn nothing_is_sent_when_the_project_has_not_opted_in() {
        let proj = "selection-optout";
        let (rx, _ide, _d, ws) = connected_fake_client_for(proj);
        // No .resh/config.toml written: share_selection defaults to false.
        let err = selection_changed(proj, ws.path(), Path::new("/w/a.rs"), "secret", (1, 0), (1, 6))
            .unwrap_err();
        assert!(err.contains("off"), "the refusal must say sharing is off: {err}");
        // A deterministic ordering barrier, not a wall-clock bound: an
        // earlier version of this test waited out a fixed `recv_timeout` and
        // hoped nothing arrived in that window, which a busy host can pass
        // for the wrong reason (see this test's revert-check history below —
        // the first version of that check used a non-blocking `try_recv`,
        // which raced the delivery outright and missed a real leak). Instead,
        // send a second, unrelated notification through the same `notify_all`
        // fan-out right after the refused call and assert it is the *first*
        // frame this connection ever receives. If the refused selection had
        // reached the wire, it would already be queued ahead of this marker
        // — the assertion is then a fact about order, not about timing, and
        // it blocks on `rx.recv()` rather than racing a clock either way.
        //
        // Revert-checked: moving the opt-in check to run after `notify_all`
        // (the same break the earlier `recv_timeout` version was written
        // against) still fails this version — `assertion left == right
        // failed ... got {"...\"method\":\"selection_changed\"...\"text\":
        // \"secret\"...} first` — deterministically, not on a timing
        // window. Restored.
        mention(proj, Path::new("/w/marker.rs"), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(
            v["method"], "at_mentioned",
            "the socket must stay silent — the marker mention should be the first frame, got {v} first"
        );
    }

    #[test]
    // Revert-checked: reusing `mention`'s `no Claude` message text for
    // `selection_changed`'s not-opted-in refusal (so both errors read "no
    // Claude is connected to this project") failed this test — it asserts on
    // the *reason*, not just that an error occurred, so a caller cannot
    // mistake "nobody is listening" for "this project has not opted in" (the
    // two have different remedies: attach a Claude, versus edit a config
    // file). It also failed the sibling `nothing_is_sent_when_the_project_
    // has_not_opted_in` test above, which checks the same "off" wording for
    // its own reason (proving the refusal happened for the right cause, not
    // just that one happened) — both are legitimate hits on the one break,
    // not evidence either test is redundant. Then restored.
    fn selection_sharing_off_is_a_different_refusal_than_no_claude_connected() {
        let proj = "selection-optout-reason";
        let (_rx, _ide, _d, ws) = connected_fake_client_for(proj);
        let err = selection_changed(proj, ws.path(), Path::new("/w/a.rs"), "x", (0, 0), (0, 1))
            .unwrap_err();
        assert!(err.contains("off") && !err.contains("no Claude"), "got: {err}");
    }

    #[test]
    // Revert-checked: deleting `notify_all`'s `targets.is_empty()` check (so
    // an empty target list falls through to `Ok(())`) failed this test along
    // with the three pre-existing tests over the same shared function —
    // `mentioning_with_no_claude_connected_is_an_error_not_a_panic`,
    // `a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude`,
    // and hub.rs's `mention_path_refusal_reaches_only_the_client_that_asked`
    // — all legitimate hits on the one break in shared code, not evidence of
    // a false positive (see CLAUDE.md's note on this exact pattern). Restored.
    fn selection_sharing_with_no_claude_connected_is_an_error_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        opt_in(d.path());
        let err = selection_changed("selection-nobody-here", d.path(), Path::new("/w/x.rs"), "x", (0, 0), (0, 1))
            .unwrap_err();
        assert!(err.contains("no Claude"), "the message must say what is missing: {err}");
    }

    #[test]
    // Same fanout guarantee `mention` already has, and the same reason it
    // needs two subscribers to prove: with one, "send to all" and "send to
    // the first" are indistinguishable.
    //
    // Revert-checked: changing `notify_all`'s `for (_, t) in &targets` to
    // `targets.first()` failed this test alongside the pre-existing
    // `a_mention_reaches_every_connected_claude_not_just_the_first` — both
    // legitimate hits on the one break in the now-shared fan-out loop.
    // Restored.
    fn a_shared_selection_reaches_every_connected_claude_not_just_the_first() {
        let proj = "selection-fanout";
        let (rx1, _a, _d1, w1) = connected_fake_client_for(proj);
        opt_in(w1.path());
        let (rx2, _b, _d2, _w2) = connected_fake_client_for(proj);
        selection_changed(proj, w1.path(), Path::new("/w/x.rs"), "x", (0, 0), (0, 1)).unwrap();
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }
}
