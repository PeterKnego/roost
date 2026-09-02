//! Terminal session registry. resh owns the PTY; dtach owns survival
//! across a resh restart. Multiple attachments to one session mirror.
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_SESSIONS_PER_PROJECT: usize = 16;
// Bound on queued-but-unread chunks per subscriber. A client that falls this
// far behind its own terminal (frozen tab, dead socket the kernel hasn't
// noticed yet) is functionally gone; better to drop it than let the queue
// grow without bound.
const SUB_CHANNEL_CAP: usize = 64;

/// Session names land in a dtach socket path and a command line. Anything
/// outside this set is a path-traversal or argument-injection vector.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Defense in depth for `attach`, not the primary gate: both call sites
/// (wsconn.rs, term.rs) already resolve `project` through
/// `projects::resolve_project` before ever reaching here, but this function
/// also builds a socket path and a session-registry key from it, so it
/// re-validates independently rather than trusting a caller never to
/// regress. Mirrors resolve_project's *syntactic* checks (no empty,
/// absolute, or leading-dot segment) — it has no `roots` to canonicalize
/// against, and doesn't need one, since the filesystem-confinement half of
/// that check already happened before this project ever had a live `dir`
/// to spawn a shell in. A nested project (`karpie/src`) is legitimate here.
///
/// One dot-segment exception, mirroring `resolve_project`'s own worktree
/// vouching: the sole dot-segment shape resh itself ever mints is
/// `.claude/worktrees/<name>` (`worktree::create`, `do_new_worktree`'s
/// `WorktreeReady.url`), so that literal shape is let through without
/// `roots` to re-derive real vouching — this was already vouched for once,
/// by `resolve_project`, before this project had a `dir` to reach here with.
/// Without this, no worktree's terminal can ever start: `attach` rejects
/// the project string before it gets anywhere near a shell.
fn valid_project(project: &str) -> bool {
    if project.is_empty() {
        return false;
    }
    let segs: Vec<&str> = project.split('/').collect();
    if segs.iter().any(|s| s.is_empty()) {
        return false;
    }
    segs.iter().enumerate().all(|(i, s)| {
        !s.starts_with('.')
            || (i > 0 && *s == ".claude" && segs.get(i + 1) == Some(&"worktrees") && i + 3 == segs.len())
    })
}

/// `{project}/{name}` rather than a flattened `{project}-{name}`: project and
/// session names can each contain `-`, so a flat join is ambiguous (project
/// `a` + session `b-c` collides with project `a-b` + session `c`). `name`
/// can't contain `/` (`valid_name` forbids it), but `project` now can — it
/// may be a nested rel path like `karpie/src` — so `project` is run through
/// `storage_key` first to hide its own `/` before this join, or a nested
/// project's directory structure would leak into (and collide with) this
/// one's, and the on-disk directory layout this join doubles as would gain
/// a directory level nothing else expects.
/// The dtach socket path for `project/name` — public for the overview,
/// which needs to find the *master* holding it (see `registry::pids_holding_path`).
pub fn socket_path(project: &str, name: &str) -> PathBuf {
    sock_path(project, name)
}

fn sock_path(project: &str, name: &str) -> PathBuf {
    crate::wsstate::state_dir()
        .join("sock")
        .join(crate::projects::storage_key(project))
        .join(name)
}

pub fn default_command(project: &str, name: &str) -> Vec<String> {
    if let Ok(c) = std::env::var("ROOST_CMD") {
        if !c.trim().is_empty() {
            return c.split_whitespace().map(String::from).collect();
        }
    }
    let sock = sock_path(project, name);
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    vec![
        "dtach".into(),
        "-A".into(),
        sock.to_string_lossy().into_owned(),
        "-E".into(), // no escape character
        "-r".into(),
        "winch".into(), // repaint full-screen apps on attach
        "-z".into(), // no suspend key
        shell,
        "-l".into(),
    ]
}

pub fn min_geometry(sizes: &HashMap<u64, (u16, u16)>) -> Option<(u16, u16)> {
    let cols = sizes.values().map(|(c, _)| *c).min()?;
    let rows = sizes.values().map(|(_, r)| *r).min()?;
    Some((cols, rows))
}

struct Session {
    // Arc<Mutex<..>> rather than a bare writer: write_input must be able to
    // release the *registry* lock before doing a blocking write to the
    // child (see write_input's comment), so the writer needs its own lock
    // that outlives the registry critical section.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // Only for age lookup via `ps`; not used for signaling (child.kill()
    // does that). 0 is a legitimate "unknown" sentinel, not a real pid, so
    // callers must treat a 0 age as "unknown" rather than "brand new".
    child_pid: u32,
    /// Output, filed under the screen it was written on, plus which screen
    /// that is. Not a plain byte log: a client attaching mid-app has to be
    /// told which buffer the frames belong in, and the app said so once,
    /// hours ago. See `screen`.
    screens: crate::screen::Screens,
    subs: HashMap<u64, SyncSender<Vec<u8>>>,
    sizes: HashMap<u64, (u16, u16)>,
    next_id: u64,
    /// Set on the spawn that consumed a parked launch; `None` for a plain
    /// shell. Survives the typing of the keystrokes on purpose.
    launched: Option<LaunchRequest>,
}

pub struct Attachment {
    pub id: u64,
    pub key: String,
    pub rx: Receiver<Vec<u8>>,
    /// True when this attach spawned the session's process rather than
    /// joining one already pumping. `term.rs` uses it to nudge every
    /// project's ◆ panel only when the roster can actually have changed —
    /// a mirrored tab reconnecting to a live session must not make every
    /// browser on the machine refetch it.
    pub spawned: bool,
    /// Set only on the attach that actually spawned the shell, and only when
    /// a `NewTerminal` asked for it (`set_launch`). The caller types the
    /// program in (`launch::keystrokes`) once the socket is up. Reattaches,
    /// mirroring browsers and a later respawn of the same name get `None`:
    /// the entry is consumed at spawn, so one click starts one claude.
    pub launch: Option<LaunchRequest>,
}

/// What a ✻ click asked a terminal to start, and the session id resh chose
/// for it. Kept on the `Session` after the keystrokes are typed — it is the
/// only record that this terminal was handed `claude`, and `claudes.rs`
/// reads it to answer "is a Claude already here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub launch: crate::proto::Launch,
    pub session_id: Option<String>,
}

/// Permission for the *next* attach on this key to spawn a shell, placed by
/// whichever intent asked for a terminal and consumed once.
///
/// A distinct type rather than `Option<LaunchRequest>` because those are not
/// the same question. `do_new_terminal` records a launch of `None` on every
/// allocation, so "no launch requested" and "no terminal was ever asked for"
/// had exactly one representation — and the second one is now the difference
/// between spawning a shell and refusing to.
#[derive(Debug, Clone, Default)]
pub struct Reservation {
    pub launch: Option<LaunchRequest>,
}

/// Everything a spawn or a close has to agree about, behind **one** mutex.
///
/// The three fields were three separate pieces of state, and the close raced
/// the spawn because the guard that was supposed to order them
/// (`Hub::is_closing`) took the *hub* mutex while the spawn is serialised by
/// this one. A check on a different lock from the operation it guards orders
/// nothing at all: a connect could pass it and spawn arbitrarily later, which
/// is how a closed project kept ending up with one live shell.
///
/// Merged here so `attach` and `kill_project` cannot interleave: whichever
/// takes this lock first wins, and *both* orders are correct. Close first —
/// reservations are gone and `closing` is set, so the attach refuses. Attach
/// first — it has inserted into `map` before releasing, so the close's own
/// scan sees it and kills it. There is no third outcome and no timing in it.
#[derive(Default)]
struct Registry {
    map: HashMap<String, Session>,
    /// Keyed exactly like `map` (`{storage_key}/{name}`).
    reservations: HashMap<String, Reservation>,
    /// Storage keys of projects mid-close. Not a bool per session: a close
    /// has to refuse names that do not exist yet, which is the whole point.
    closing: std::collections::HashSet<String>,
}

static SESSIONS: OnceLock<Mutex<Registry>> = OnceLock::new();

fn sessions() -> &'static Mutex<Registry> {
    SESSIONS.get_or_init(|| Mutex::new(Registry::default()))
}

/// Reserves `project/name`, so the next attach on it is allowed to spawn, and
/// records what that spawn should type into its shell.
///
/// Called on *every* allocation, not just the ones that launch something —
/// which is why the launch is `Option` *inside* the reservation rather than
/// the reservation being optional. A name handed out for a ✻ click whose tab
/// was closed before any browser attached leaves its entry behind, and the
/// plain `+` click handed the same name next must not inherit that launch;
/// it must still be allowed to spawn, so it needs a reservation of its own
/// with no launch in it.
pub fn reserve(project: &str, name: &str, launch: Option<LaunchRequest>) {
    let key = format!("{}/{}", crate::projects::storage_key(project), name);
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.reservations.insert(key, Reservation { launch });
}

/// Authorises a spawn *without* disturbing a launch already parked for this
/// name.
///
/// `reserve` overwrites, which is right when an intent is deciding what the
/// next shell should be. It is wrong for anything that only wants to permit
/// the spawn: overwriting a `✻` click's launch with `None` leaves a plain
/// shell and a test that still passes every assertion except the one about
/// the launch. Both the lib and integration harnesses hit exactly that.
pub fn reserve_if_absent(project: &str, name: &str) {
    let key = format!("{}/{}", crate::projects::storage_key(project), name);
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.reservations.entry(key).or_default();
}

/// Drops a reservation without spawning — a tab closed before any browser
/// attached. Without it a name stays permanently spawnable.
pub fn release(project: &str, name: &str) {
    let key = format!("{}/{}", crate::projects::storage_key(project), name);
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.reservations.remove(&key);
}

/// Opens a close: marks the project closing, drops every reservation it has,
/// and takes its live sessions out of the map — killing each one's child — in
/// a single acquisition of the one lock `attach` also needs.
///
/// Returning the names is not a convenience. `kill_project` used to take this
/// lock, scan, release, and only then work through the sockets; a spawn could
/// land in between and appear in neither the scan nor the socket directory.
/// Draining under the same lock that now refuses new spawns closes that by
/// construction: after this returns, this project's set of sessions cannot
/// grow again until `end_close`.
pub fn begin_close(project: &str) -> Vec<String> {
    let skey = crate::projects::storage_key(project);
    let prefix = format!("{skey}/");
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.closing.insert(skey);
    guard.reservations.retain(|k, _| !k.starts_with(&prefix));
    let keys: Vec<String> = guard.map.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    let mut names = Vec::with_capacity(keys.len());
    for k in keys {
        if let Some(mut s) = guard.map.remove(&k) {
            let _ = s.child.kill();
            let _ = s.child.wait();
            if let Some(name) = k.strip_prefix(&prefix) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Closes a close. Until this runs every attach on the project is refused,
/// including one for a session that still exists — it is about to stop.
pub fn end_close(project: &str) {
    let skey = crate::projects::storage_key(project);
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    guard.closing.remove(&skey);
}

/// What `Registry::decide` concluded about one connect.
enum Decision {
    /// The session is already running here; this is a second viewer.
    Join,
    /// Authorised to spawn, carrying whatever the reservation asked for.
    Spawn(Reservation),
    /// Nothing local says yes; ask the filesystem whether a session survived
    /// a restart. Deliberately *not* decided under the lock — see `attach`.
    Probe,
    Refuse(AttachRefusal),
}

/// Whether a socket on disk still has a process behind it.
///
/// Four states, not two, and the fourth is the point: `Unknown` is "the
/// process listing could not be trusted", which must never be folded into
/// "nothing holds it" here — that answer leads to a spawn.
enum SocketState {
    Absent,
    Held,
    Unheld,
    Unknown,
}

/// `symlink_metadata`, not `exists()`: the latter collapses "not there" and
/// "cannot look" into one `false` and follows symlinks, which is the exact
/// shape of several defects this project has already shipped.
fn probe_socket(sock: &Path) -> SocketState {
    match std::fs::symlink_metadata(sock) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketState::Absent,
        Err(_) => return SocketState::Unknown,
        Ok(_) => {}
    }
    match crate::registry::holders_snapshot() {
        None => SocketState::Unknown,
        Some(snap) if !crate::registry::pids_holding_path(&snap, sock).is_empty() => {
            SocketState::Held
        }
        Some(_) => SocketState::Unheld,
    }
}

impl Registry {
    /// The authorisation rule, in one place and under one lock, so it can be
    /// tested without a PTY and cannot drift from what `attach` does.
    ///
    /// Order matters: `Closing` outranks even an existing session, because a
    /// session that exists during a close is one that is about to stop.
    fn decide(&mut self, skey: &str, key: &str) -> Decision {
        if self.closing.contains(skey) {
            return Decision::Refuse(AttachRefusal::Closing);
        }
        if self.map.contains_key(key) {
            return Decision::Join;
        }
        if let Some(r) = self.reservations.remove(key) {
            return Decision::Spawn(r);
        }
        Decision::Probe
    }
}

/// Why an attach was refused. Typed rather than a `String` so `term.rs` can
/// answer each differently and a negative test can assert on *which* refusal
/// it got — this codebase has shipped confinement tests that passed on the
/// wrong error entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRefusal {
    /// The project is mid-close. Outranks every other rule.
    Closing,
    /// No session, no reservation, no live socket to rejoin. `attach` used to
    /// create here, which made every terminal websocket a session factory and
    /// a close something a client could race.
    NotReserved,
    /// A socket is on disk but whether anything holds it could not be
    /// determined. Refused rather than guessed: guessing "nothing holds it"
    /// falls through to a spawn, and a spawn nobody asked for is an orphan
    /// with no tab and no way back to it.
    Unverifiable,
}

impl std::fmt::Display for AttachRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closing => "the project is closing",
            Self::NotReserved => "that session has ended",
            Self::Unverifiable => "could not determine whether that session is still running",
        })
    }
}

/// The environment a resh shell is spawned with, factored out of `attach` so
/// it can be asserted on directly rather than through a real PTY spawn.
fn session_env(
    project: &str,
    name: &str,
    ide_port: Option<u16>,
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    // How a process in this terminal — Claude, mainly — discovers that it
    // can raise a notification at all, and what to attribute it to. A
    // model can answer "can I notify?" from its own environment rather
    // than having to be told in a prompt.
    env.insert("ROOST_NOTIFY".into(), "1".into());
    env.insert("ROOST_PROJECT".into(), project.to_string());
    env.insert("ROOST_SESSION".into(), name.to_string());
    // Claude Code matches a lock file by port before it tries to match by
    // path, so this makes a claude started here connect without any path
    // comparison at all — which sidesteps every worktree, symlink and
    // canonicalisation question in one line. Absent, not empty, when there
    // is no listener: an empty value would tell Claude Code to try port 0
    // rather than to skip the SSE-port shortcut entirely.
    if let Some(p) = ide_port {
        env.insert("CLAUDE_CODE_SSE_PORT".into(), p.to_string());
    }
    env
}

/// Attach to a session, creating it if needed. The new subscriber is sent the
/// session's screen immediately — its scrollback, and the switch that says
/// which buffer a running full-screen app is painting on — so a reconnecting
/// browser sees where it was rather than a log it cannot place.
///
/// Locking discipline: the registry mutex is held only for the short,
/// non-blocking bookkeeping steps (map lookups, inserting a new Session,
/// registering a subscriber, and — since this task — reading the ide port
/// via `session_env`/`ide::port_for`, itself just a `HashMap` lookup under
/// `ide`'s own registry lock). It is never held across a blocking read or
/// write — the pump thread below re-acquires the lock fresh on every loop
/// iteration, only around the read()-independent fan-out, so `attach` can
/// never block behind a PTY that has nothing to say.
pub fn attach(project: &str, name: &str, dir: &Path) -> Result<Attachment, String> {
    if !valid_name(name) {
        return Err("invalid session name".into());
    }
    if !valid_project(project) {
        return Err("invalid project name".into());
    }
    // storage_key, not the raw project string: `project` may be a nested
    // rel path (`karpie/src`), and its `/` must not be mistaken for this
    // key's own `project/name` separator — see storage_key's doc comment.
    // Without this, project "karpie" + session "src" and project
    // "karpie/src" + session "shell" (a real, distinct pair) would produce
    // keys "karpie/src" and "karpie/src/shell" that alias under the
    // `starts_with` prefix check just below, inflating one project's
    // session cap with another's sessions.
    let skey = crate::projects::storage_key(project);
    let key = format!("{skey}/{name}");

    // Authorised first, in its own acquisition, because deciding whether this
    // connect may *create* a session is the whole invariant — see `Registry`.
    // The probe branch is the only one that touches the filesystem, and it
    // does so with the lock released: a `ps` fork under this mutex would
    // stall every other session's output, which is the deadlock class
    // CLAUDE.md already records shipping once.
    let decision = {
        let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        guard.decide(&skey, &key)
    };
    let reservation = match decision {
        Decision::Join => None,
        Decision::Spawn(r) => Some(r),
        Decision::Refuse(why) => return Err(why.to_string()),
        Decision::Probe => {
            // Restart survival, and the only case that has to ask the
            // filesystem: this process never attached, so the map is empty,
            // but the dtach master is alive and `dtach -A` will rejoin it.
            match probe_socket(&sock_path(project, name)) {
                SocketState::Held => Some(Reservation::default()),
                SocketState::Absent | SocketState::Unheld => {
                    return Err(AttachRefusal::NotReserved.to_string())
                }
                SocketState::Unknown => return Err(AttachRefusal::Unverifiable.to_string()),
            }
        }
    };

    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    // Re-checked after the probe released the lock: a close may have started
    // in between, and it outranks an authorisation taken a moment ago.
    if guard.closing.contains(&skey) {
        return Err(AttachRefusal::Closing.to_string());
    }
    let map = &mut guard.map;
    let live_for_project = map.keys().filter(|k| k.starts_with(&format!("{skey}/"))).count();
    if !map.contains_key(&key) && live_for_project >= MAX_SESSIONS_PER_PROJECT {
        return Err("too many terminal sessions".into());
    }

    let spawned = !map.contains_key(&key);
    let mut launch = None;
    if spawned {
        let cmd = default_command(project, name);
        if cmd.is_empty() {
            return Err("empty command".into());
        }
        // The reservation was consumed by `decide` above, before the spawn can
        // fail, rather than after: a spawn that fails leaves no session, and
        // the browser's retry would otherwise find the entry still there and
        // type claude into a shell nobody asked to be a claude shell. One
        // click, one chance.
        launch = reservation.and_then(|r| r.launch);
        // dtach -A refuses to create a socket in a directory that doesn't
        // exist yet; nothing else creates it. 0o700: this directory grants
        // shell access to whoever can connect to a socket in it. Gated on
        // the command actually being dtach so ROOST_CMD-overridden test
        // runs (which never touch sock_path) don't create directories under
        // a real, unconfigured $HOME.
        if cmd[0] == "dtach" {
            if let Some(sock_dir) = sock_path(project, name).parent() {
                std::fs::create_dir_all(sock_dir).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(sock_dir, std::fs::Permissions::from_mode(0o700));
                }
                // Records the resolved absolute directory this session was
                // created under, so a *later* reconcile pass — possibly run
                // by a differently-configured process sharing this same
                // ROOST_STATE_DIR — can confirm "genuinely gone" against
                // this exact path rather than guessing from whether it
                // resolves under *its own* roots. `dir` is already the
                // caller's resolved path (every caller resolves the project
                // through `projects::resolve_project` before calling here),
                // so no re-resolution is needed. See
                // `registry::confirmed_gone`'s doc comment for why this
                // matters.
                crate::registry::record_origin(project, dir);
            }
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cb = CommandBuilder::new(&cmd[0]);
        cb.args(&cmd[1..]);
        cb.cwd(dir);
        for (k, v) in session_env(project, name, crate::ide::port_for(project)) {
            cb.env(k, v);
        }
        let child = pair.slave.spawn_command(cb).map_err(|e| e.to_string())?;
        // Best-effort: some platforms/backends can decline to report a pid.
        // 0 degrades list_sessions' age lookup to "unknown" rather than
        // panicking or failing the attach outright.
        let child_pid = child.process_id().unwrap_or(0);
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        map.insert(
            key.clone(),
            Session {
                writer: Arc::new(Mutex::new(writer)),
                master: pair.master,
                child,
                child_pid,
                screens: crate::screen::Screens::new(),
                subs: HashMap::new(),
                sizes: HashMap::new(),
                next_id: 0,
                launched: launch.clone(),
            },
        );
        let pump_key = key.clone();
        let pump_project = project.to_string();
        let pump_session = name.to_string();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // Parser state lives here, not on `Session`: scanning then needs
            // no lock at all, and a sequence split across two reads still
            // parses.
            let mut osc = crate::osc::Parser::new();
            let mut screen = crate::screen::Scanner::new();
            loop {
                // The blocking read happens with the lock released: only the
                // fan-out after a chunk arrives needs the registry.
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // Scanned before the lock is taken (pure CPU), and
                        // published after it is dropped: `publish` locks the
                        // hub registry, and holding the session registry
                        // across that would invert a lock order and risk the
                        // deadlock this project has already shipped once.
                        let notices = osc.feed(&buf[..n]);
                        let switches = screen.feed(&buf[..n]);
                        {
                            let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
                            let map = &mut guard.map;
                            let Some(s) = map.get_mut(&pump_key) else { break };
                            // What goes out is what `ingest` returns, not the
                            // raw read: it is the same bytes except for the
                            // one case it has to reconcile (see its doc).
                            let chunk = s.screens.ingest(&buf[..n], &switches);
                            // try_send, not send: a subscriber whose queue is
                            // full (frozen tab, dead socket) is dropped rather
                            // than backing up the whole fan-out (I4).
                            s.subs.retain(|_, tx| tx.try_send(chunk.clone()).is_ok());
                        }
                        for p in notices {
                            crate::hub::publish(&pump_project, &pump_session, p);
                        }
                    }
                }
            }
            // PTY closed: drop the session so the next attach respawns it.
            {
                let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
                let map = &mut guard.map;
                if let Some(mut s) = map.remove(&pump_key) {
                    let _ = s.child.kill();
                    let _ = s.child.wait();
                }
            }
            // A shell that ended on its own (`exit`, or its dtach master
            // dying) changes the running-projects roster without any intent
            // having asked for it, so nothing in hub.rs knows to say so.
            // After the registry lock is released, for the same lock-order
            // reason `publish` above runs outside it. Also fires when a
            // kill path took the process down, redundantly and possibly
            // before that path has unlinked the socket — harmless, since
            // those paths send their own nudge once they are done.
            crate::hub::broadcast_all(&crate::proto::Event::ProjectsChanged {
                project: pump_project.clone(),
            });
        });
    }

    let s = map.get_mut(&key).ok_or("session vanished")?;
    s.next_id += 1;
    let id = s.next_id;
    let (tx, rx) = sync_channel(SUB_CHANNEL_CAP);
    let replay = s.screens.replay();
    if !replay.is_empty() {
        let _ = tx.try_send(replay);
    }
    s.subs.insert(id, tx);
    Ok(Attachment { id, key, rx, spawned, launch })
}

/// Takes the registry lock only long enough to clone the writer's `Arc`,
/// then drops it before the blocking write. Holding the registry lock across
/// I/O to a child would deadlock: if the child stops draining stdin (e.g.
/// it's itself blocked writing output faster than the browser reads it), the
/// write blocks indefinitely, and the pump thread — the only thing that can
/// unblock it by draining output — needs that same lock to make progress.
pub fn write_input(key: &str, data: &[u8]) -> Result<(), String> {
    let writer = {
        let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let map = &mut guard.map;
        let s = map.get_mut(key).ok_or("no such session")?;
        s.writer.clone()
    };
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    w.write_all(data).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

pub fn resize(key: &str, id: u64, cols: u16, rows: u16) {
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let map = &mut guard.map;
    let Some(s) = map.get_mut(key) else { return };
    s.sizes.insert(id, (cols, rows));
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

/// Detach only. The PTY keeps running and dtach keeps the session alive, so
/// reopening the same name reattaches.
pub fn detach(key: &str, id: u64) {
    let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let map = &mut guard.map;
    let Some(s) = map.get_mut(key) else { return };
    s.subs.remove(&id);
    s.sizes.remove(&id);
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

/// The `SESSIONS` map key. Callers pass the *raw* slashed project form (e.g.
/// `karpie/src`), matching how they already invoke `attach` — but this
/// function encodes it via `storage_key` internally, because that's what
/// `attach` itself keys the map with. A raw, unencoded key would let project
/// `karpie` + session `src` (raw key `karpie/src`) alias with project
/// `karpie/src` + session `shell` under the `starts_with(&format!("{project}/"))`
/// prefix checks in `list_sessions`/`kill_project`, inflating one project's
/// session cap and visibility with another project's sessions — the exact
/// ambiguity `attach` already guards against (see `sock_path`'s comment).
pub fn key_for(project: &str, name: &str) -> String {
    format!("{}/{name}", crate::projects::storage_key(project))
}

pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    /// `None` when the OS could not tell us — a pid `ps` cannot read, or the
    /// 0 sentinel `child_pid` carries when portable-pty gave no pid. Not `0`:
    /// an unknown age reported as zero claims the shell just started, which is
    /// the opposite of the truth and wrong on exactly the question a session
    /// age is asked for. See CLAUDE.md, "Absence of evidence is not evidence
    /// of absence" — the same rule, in a rendering rather than a destructive
    /// position.
    pub age_secs: Option<u64>,
    pub attached: usize,
}

/// Elapsed seconds for a pid, via `ps -o etime=`. Age is read from the OS
/// rather than recorded in memory because dtach sessions outlive resh —
/// an in-process timestamp would reset on every restart and report a
/// days-old shell as brand new.
///
/// `etime` rather than the brief's suggested `etimes`: this macOS `ps`
/// (BSD-derived) rejects `etimes` outright ("keyword not found") and only
/// offers `etime`, GNU `ps` on Linux accepts either. `etime` prints
/// `[[dd-]hh:]mm:ss` instead of a raw second count, so it needs parsing —
/// see `parse_etime`.
pub fn process_age_secs(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_etime(String::from_utf8_lossy(&out.stdout).trim())
}

/// Parses `ps -o etime=` output: `mm:ss`, `hh:mm:ss`, or `dd-hh:mm:ss`.
/// Malformed input (unexpected pid format, a future ps that changes shape)
/// yields None rather than panicking — callers already treat None as
/// "unknown".
fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, mins, secs) = match parts.as_slice() {
        [h, m, s] => (h.parse::<u64>().ok()?, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        _ => return None,
    };
    Some(((days * 24 + hours) * 60 + mins) * 60 + secs)
}

/// All sessions for one project. `project` is the raw form (see `key_for`);
/// the prefix is built from its `storage_key` so nested projects match the
/// keys `attach` actually inserted, rather than silently matching nothing.
///
/// The `ps` fork for each session's age happens **after** the registry guard
/// is dropped, and this is load-bearing rather than tidy. This function runs
/// on a request path — `registry::known_projects` calls it once per project on
/// every picker load, workspace load and strip refresh — while the PTY pump
/// re-takes this same `SESSIONS` mutex for every chunk of terminal output. Ages
/// resolved under the guard meant one page load froze terminal output for every
/// session in every project for as long as up to `MAX_SESSIONS_PER_PROJECT`
/// subprocess spawns took. `live_names` below has always described this hazard;
/// this function used to be the thing it was describing. See CLAUDE.md: never
/// hold a lock across blocking I/O — this project has already shipped one
/// deadlock of that shape.
pub fn list_sessions(project: &str) -> Vec<SessionInfo> {
    // The lock-only scan lives in `live_rows`; the guard is already dropped
    // by the time it returns, so the `ps` fork below is outside it too.
    live_rows(project)
        .into_iter()
        .map(|(name, pid, attached)| SessionInfo {
            name,
            pid,
            age_secs: process_age_secs(pid),
            attached,
        })
        .collect()
}

/// (name, pid, attached) for a project's live sessions — the lock-only scan
/// `list_sessions` used to do inline, now shared with the overview's
/// all-sessions view (which joins these against one `ages_snapshot` instead
/// of forking `ps` per session). No `ps` here at all: see `list_sessions`'s
/// doc comment on why the fork must happen after the guard is dropped.
pub fn live_rows(project: &str) -> Vec<(String, u32, usize)> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let mut found: Vec<(String, u32, usize)> = {
        let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let map = &guard.map;
        map.iter()
            .filter_map(|(k, s)| {
                let name = k.strip_prefix(&prefix)?;
                Some((name.to_string(), s.child_pid, s.subs.len()))
            })
            .collect()
    }; // guard dropped here, before any I/O
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// `live_rows` plus the sockets on disk. The in-memory map is only what
/// THIS process attached; dtach sessions outlive resh, so after a restart
/// every surviving session is missing from it while its socket is still on
/// disk and its shell still running — the bug `live_names` documents, seen
/// on the overview as a right pane listing two sessions under a left pane
/// marking four projects live. Sockets are the restart-proof truth; a
/// socket-only row carries pid 0 (the existing "unknown" sentinel) and 0
/// attached. Separate from `live_rows` so `list_sessions` keeps its
/// in-memory contract (its callers count "sessions this process holds").
pub fn session_rows(project: &str) -> Vec<(String, u32, usize)> {
    let mut found = live_rows(project);
    for name in socket_names(project) {
        if !found.iter().any(|(n, _, _)| *n == name) {
            found.push((name, 0, 0));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// pid → elapsed seconds, from ONE `ps -Aww -o pid=,etime=` for the whole
/// host — so the overview's all-projects view costs one fork rather than one
/// per session, unlike `list_sessions`'s per-project `process_age_secs`.
/// Failure (missing `ps`, non-zero exit) is an empty map: every age then
/// renders "unknown", never `0` — see CLAUDE.md, "Absence of evidence is not
/// evidence of absence".
pub fn ages_snapshot() -> HashMap<u32, u64> {
    match std::process::Command::new("ps").args(["-Aww", "-o", "pid=,etime="]).output() {
        Ok(o) if o.status.success() => parse_ages(&String::from_utf8_lossy(&o.stdout)),
        _ => HashMap::new(),
    }
}

/// The age of a session is the age of the oldest process holding its socket
/// — the dtach master, which outlives resh. `child_pid` is only the client
/// *this* resh spawned, so after a restart it reports "10m" for an 18-hour
/// shell (the time since resh reattached). `None` when no holder is known
/// or none has an age: unknown, never `0`.
pub fn oldest_age_of(pids: &[u32], ages: &HashMap<u32, u64>) -> Option<u64> {
    pids.iter().filter_map(|p| ages.get(p).copied()).max()
}

/// Parses `ps -Aww -o pid=,etime=` output: one `pid etime` pair per line,
/// split on the first run of whitespace. A line that doesn't parse (an
/// unexpected column, a truncated read) is skipped rather than panicking —
/// one bad row from a live `ps` shouldn't blank every other session's age.
pub fn parse_ages(out: &str) -> HashMap<u32, u64> {
    let mut m = HashMap::new();
    for line in out.lines() {
        let t = line.trim();
        let Some((pid_s, etime_s)) = t.split_once(char::is_whitespace) else { continue };
        let (Ok(pid), Some(age)) = (pid_s.trim().parse::<u32>(), parse_etime(etime_s.trim())) else { continue };
        m.insert(pid, age);
    }
    m
}

/// Just the names of a project's live sessions — no `ps` invocation, unlike
/// `list_sessions`. `Hub::refresh_live_sessions` calls this on every
/// terminal-affecting intent (and from `Hub::new`, which runs under the
/// hub *and* the process-global hub-registry lock via `for_project`), so it
/// must stay a plain map scan: `list_sessions`'s per-session `ps` fork,
/// done while holding this same `SESSIONS` mutex, would otherwise stall
/// every other project's connection setup for the duration of up to
/// `MAX_SESSIONS_PER_PROJECT` subprocess spawns.
pub fn live_names(project: &str) -> Vec<String> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let mut out: Vec<String> = {
        let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let map = &guard.map;
        map.keys().filter_map(|k| k.strip_prefix(&prefix)).map(str::to_string).collect()
    };
    // The in-memory map is only what THIS process attached. dtach sessions
    // outlive resh, so after a restart every surviving session is absent
    // from it while its socket is still on disk and its shell still running.
    // Reporting only the map made the workspace claim "No terminal sessions are
    // running" for a project holding two, and — because `kill_project` walked
    // the same map — made Close Project silently end nothing. Observed in
    // production: the dialog said nothing was running, accepting it changed
    // nothing, and the button looked dead.
    //
    // A directory listing, not a `ps`: this is still called from `Hub::new`
    // under the process-global hub-registry lock, so it must stay fork-free
    // (see the caller's comment). `reconcile` guarantees a socket still present
    // is process-backed, within its own bounded staleness window.
    out.extend(socket_names(project));
    out.sort();
    out.dedup();
    out
}

/// Sessions of `project` this process spawned with a launch. A map scan
/// only, like `live_names`: it runs under the hub on every ✻ click.
pub fn launched_names(project: &str) -> Vec<(String, LaunchRequest)> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let map = &guard.map;
    let mut out: Vec<(String, LaunchRequest)> = map
        .iter()
        .filter_map(|(k, s)| Some((k.strip_prefix(&prefix)?.to_string(), s.launched.clone()?)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Session names with a socket on disk for this project. Dotfiles are skipped:
/// `.origin` is metadata about the project key, not a session.
fn socket_names(project: &str) -> Vec<String> {
    let dir = crate::wsstate::state_dir().join("sock").join(crate::projects::storage_key(project));
    // An unreadable socket dir reads as "no sessions" here. Display-only:
    // this feeds listings, never `reconcile`'s reaping, so the fold hides a
    // row for one refresh and destroys nothing — the exception CLAUDE.md's
    // table allows, stated so it is not mistaken for the destructive kind.
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

pub fn has_session(project: &str, name: &str) -> bool {
    let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let map = &guard.map;
    map.contains_key(&key_for(project, name))
}

/// Ends every session belonging to one project. This is the only way to end
/// a session from the UI; detaching a tab deliberately leaves it running.
/// Prefix built from `storage_key` for the same reason as `list_sessions`.
///
/// Killing `s.child` alone — the whole of what this function used to do —
/// only ends dtach's *client*, the process resh itself spawned. In
/// `-A` mode `dtach` forks a *master* that immediately detaches and
/// reparents to init; killing the client is therefore just a *detach*, not
/// an end. The master and the user's shell survive with a live socket,
/// unreachable through resh and invisibly still running — exactly the
/// failure "Close Project" exists to prevent, silently doing what closing a
/// tab already does while telling the user the session ended. So this now
/// also kills whatever still holds each session's socket path (the master
/// included) via `registry::kill_and_unlink` — the identical helper
/// `reconcile` uses to reap a session whose project has been deleted, since
/// both need the same "kill, confirm, then unlink — leave the socket in
/// place on any failure" behavior.
///
/// `ended` counts only sessions actually confirmed ended (client killed
/// *and* every socket-holding process confirmed dead), not merely removed
/// from the in-memory map, so a caller reporting this number (e.g.
/// `ProjectClosed`) never overstates what happened.
///
/// The registry lock is held only long enough to remove each matching
/// entry and kill its in-process client — never across the socket
/// kill-and-poll below, which can block for up to ~500ms per session and
/// shells out to `ps`/`kill`. Holding it across that would freeze every
/// other project's terminal traffic (any operation needing this same lock)
/// for the duration, the identical hazard `write_input`'s doc comment
/// already describes for a blocking write.
/// Ends whatever still holds `project/name`'s socket — the dtach master
/// included — and unlinks it. Shared by [`end_session`] and [`kill_project`]
/// so the two cannot drift: both need the same "kill, confirm, then unlink,
/// but leave the socket in place if something survived" handling, and a
/// survivor must stay discoverable rather than be silently forgotten.
fn end_socket(project: &str, name: &str, who: &str) -> bool {
    if crate::registry::kill_and_unlink(&sock_path(project, name)) {
        return true;
    }
    eprintln!(
        "resh: {who} could not fully end session {project}/{name} — a process survived the kill attempt; its socket was left in place so it stays discoverable"
    );
    false
}

/// Ends one session: the narrow case of [`kill_project`], for a user closing
/// a single terminal tab.
///
/// Killing this process's own client is only a *detach* — in `-A` mode dtach
/// forks a master that reparents to init — so the socket work below is what
/// actually ends the shell, and it runs whether or not this process ever
/// attached. That second part is why a session which outlived a restart (on
/// disk, absent from the in-memory map) is still endable.
///
/// Returns whether the session is confirmed gone.
pub fn end_session(project: &str, name: &str) -> bool {
    if !valid_name(name) || !valid_project(project) {
        return false;
    }
    let key = format!("{}/{}", crate::projects::storage_key(project), name);
    {
        let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let map = &mut guard.map;
        if let Some(mut s) = map.remove(&key) {
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
    } // lock released before any blocking socket work — see `attach`
    end_socket(project, name, "End session")
}

/// The first unused `term`, `term1`, `term2`, … for this project, or `None`
/// once [`MAX_SESSIONS_PER_PROJECT`] names are live.
///
/// Allocation is deliberately server-side. A client can only see the sessions
/// it has tabs for, while `live_names` also sees detached ones and those that
/// outlived a restart; picking a name from the tab strip alone would sooner or
/// later choose a name that is still alive, and since attaching creates only
/// when absent, "new terminal" would silently drop the user into an old shell
/// with its scrollback replayed.
///
/// `also_taken` covers the gap between opening the tab and the browser's
/// follow-up connect to `/ws/{project}/term/{name}`, which is what actually
/// spawns the PTY (see `term.rs`): until that lands the name is in no
/// registry, so two quick clicks would otherwise both be handed `term`.
pub fn next_free_name(project: &str, also_taken: &[String]) -> Option<String> {
    let live = live_names(project);
    let taken = |n: &str| live.iter().any(|l| l == n) || also_taken.iter().any(|l| l == n);
    // One more candidate than the cap: with MAX names taken, every candidate
    // below is taken and the cap is what refuses — not an exhausted range.
    (0..=MAX_SESSIONS_PER_PROJECT)
        .map(|i| if i == 0 { "term".to_string() } else { format!("term{i}") })
        .find(|n| !taken(n))
        .filter(|_| live.len() < MAX_SESSIONS_PER_PROJECT)
}

pub fn kill_project(project: &str) -> usize {
    // `begin_close` does the map drain, and marks the project closing in the
    // same acquisition — so from here on no attach can add to what this
    // function is about to walk. That ordering is the whole fix: the scan
    // used to release the lock before the socket work below, leaving a window
    // in which a spawn landed in neither the scan nor the socket directory.
    //
    // `end_close` is paired here, at the bottom of this function, rather than
    // left to the caller. The refusal has to outlast the *killing* — a
    // browser reconnecting mid-kill must not be authorised by the very
    // absence the kill is creating — and this function is exactly that
    // span. Leaving it to callers made the invariant depend on every one of
    // them remembering, and the first thing that forgot left the project
    // permanently unable to open a terminal again, which is a worse failure
    // than the one being prevented.
    let removed_names: Vec<String> = begin_close(project);

    // Sessions that outlived a restart are not in the map above, but their
    // sockets are on disk — and they are exactly the long-running shells a user
    // reaches for Close Project to end. Walking only the map made this a no-op
    // for them: the confirmation reported nothing running and nothing was
    // killed. `kill_and_unlink` ends whatever holds each socket, which is the
    // dtach master, so it works whether or not this process ever attached.
    let mut all_names = removed_names;
    for name in socket_names(project) {
        if !all_names.contains(&name) {
            all_names.push(name);
        }
    }

    let mut ended = 0;
    for name in &all_names {
        if end_socket(project, name, "Close Project") {
            ended += 1;
        }
    }
    end_close(project);
    // Not removing the now-possibly-empty sock/<project>/ directory here:
    // `registry::reconcile` already does that unconditionally on every
    // pass, and it runs on every subsequent project listing or header-strip
    // load (`known_projects` calls it, throttled but not indefinitely
    // deferred), so a second removal here would be pure duplication, not a
    // correctness requirement — the directory disappears on the very next
    // such load regardless.
    ended
}

/// Reserve, then attach — the production sequence, for tests that used to get
/// a session merely by connecting.
///
/// Not a convenience: `attach` no longer creates on its own, so a test that
/// calls it bare is asserting against a connect that production would refuse.
/// Every test below that wants a *running* session goes through here, which
/// is also what keeps the refusal tests honest — they call `attach` directly,
/// and that difference is now meaningful rather than incidental.
#[cfg(test)]
pub fn reserve_and_attach(project: &str, name: &str, dir: &Path) -> Result<Attachment, String> {
    // `or_default`, not `reserve`: a test that has already reserved a *launch*
    // for this name must keep it. Overwriting it here silently turned every
    // "the launch rides the attach that spawns" test into a test of a plain
    // shell — six of them, all still green on the launch assertions they were
    // no longer exercising, until the launch itself was asserted.
    reserve_if_absent(project, name);
    attach(project, name, dir)
}

// Serializes tests that mutate the process-global ROOST_CMD env var.
// cargo runs a binary's tests in parallel threads by default, and an env var
// is process-wide state, so two such tests interleaving would have one see
// the other's value mid-test (or after its cleanup already ran). This
// project shipped exactly that flakiness once before; every test below that
// touches ROOST_CMD takes this lock for its whole body.
//
// Lock order, whenever a test needs both this and `wsstate::STATE_ENV_LOCK`
// (ROOST_STATE_DIR): **STATE_ENV_LOCK first, SESSION_ENV_LOCK second**,
// everywhere, no exceptions. Two tests taking them in opposite orders can
// deadlock under `cargo test`'s parallel threads, and a deadlock does not
// fail — it *hangs*, so no number of green runs can rule it out. It would
// present as stuck CI, never as a red test. The order itself is arbitrary;
// what matters is that it is total, and the majority of call sites
// (`registry`'s and `hub`'s tests, which acquire the state dir first because
// their fixtures are built inside it) already used this one.
#[cfg(test)]
pub static SESSION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Revert-checked (Task 5, Step 5): removing the `if let Some(p) =
    // ide_port` block from `session_env` — so `CLAUDE_CODE_SSE_PORT` is
    // never inserted at all — failed only this test. See this task's report
    // for the exact command and output.
    fn a_spawned_shell_is_told_which_port_to_connect_to() {
        // Without this a claude in a resh terminal has to path-match, which
        // is exactly the comparison that goes wrong for worktrees.
        let env = session_env("alpha", "main", Some(5599));
        assert_eq!(env.get("CLAUDE_CODE_SSE_PORT").map(String::as_str), Some("5599"));
        assert_eq!(env.get("ROOST_PROJECT").map(String::as_str), Some("alpha"));
    }

    #[test]
    fn a_project_without_a_listener_gets_no_port_variable() {
        let env = session_env("alpha", "main", None);
        assert!(!env.contains_key("CLAUDE_CODE_SSE_PORT"), "an empty value would be worse than absence");
    }

    #[test]
    fn session_names_are_strictly_validated() {
        assert!(valid_name("shell"));
        assert!(valid_name("claude-2"));
        assert!(valid_name("A_b-9"));
        // these land in a socket path and a command line
        assert!(!valid_name(""));
        assert!(!valid_name("../../etc/passwd"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a;rm -rf /"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(33)));
        assert!(valid_name(&"x".repeat(32)));
    }

    #[test]
    fn valid_project_accepts_nested_and_rejects_bad_segments() {
        assert!(valid_project("proj"));
        assert!(valid_project("karpie/src"));
        assert!(!valid_project(""));
        assert!(!valid_project("/abs"));
        assert!(!valid_project("a//b"));
        assert!(!valid_project(".hidden"));
        assert!(!valid_project("a/../b"));
    }

    // Revert-checked: reverting to the blanket `!s.starts_with('.')` check
    // (deleting the worktree exception) fails the first assertion here —
    // observed: `assertion failed: valid_project("repo/.claude/worktrees/claude-1")`.
    // `resolve_project` (projects.rs) already grew this exception for real
    // vouched worktrees; this function's own doc comment claims to mirror
    // resolve_project's syntactic checks, and had silently drifted out of
    // sync with it.
    #[test]
    fn valid_project_accepts_only_the_exact_worktree_shape_as_a_dot_segment() {
        assert!(valid_project("repo/.claude/worktrees/claude-1"), "the one shape resh itself mints");
        assert!(valid_project("karpie/src/.claude/worktrees/claude-2"), "a nested project's worktree too");
        assert!(!valid_project("repo/.claude"), "the parent dot-dir alone is still not a project");
        assert!(!valid_project("repo/.claude/worktrees"), "missing the worktree name");
        assert!(!valid_project("repo/.claude/worktrees/claude-1/extra"), "not a deeper path under it");
        assert!(!valid_project(".claude/worktrees/claude-1"), "not at the top level either");
        assert!(!valid_project("repo/.git"), "no exception for any other dotfile");
    }

    #[test]
    fn storage_key_keeps_a_nested_projects_session_key_unambiguous() {
        // project "karpie" with session "src", vs. project "karpie/src"
        // with session "shell": before storage_key was used here, both
        // produced keys under the "karpie/" prefix, so the second's
        // sessions counted against the first's MAX_SESSIONS_PER_PROJECT cap.
        let key_a = format!("{}/{}", crate::projects::storage_key("karpie"), "src");
        let key_b = format!("{}/{}", crate::projects::storage_key("karpie/src"), "shell");
        assert!(!key_b.starts_with(&format!("{}/", crate::projects::storage_key("karpie"))));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn default_command_wraps_dtach_with_no_ui() {
        // Reads ROOST_CMD, so it is a participant in the env-var race the
        // lock's comment describes, even though it never sets the variable:
        // unlocked, it fails whenever it interleaves with a `ROOST_CMD=cat`
        // test, and more such tests make it lose more often.
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let c = default_command("proj", "shell");
        assert_eq!(c[0], "dtach");
        assert!(c.contains(&"-E".to_string()), "no escape character");
        assert!(c.contains(&"-z".to_string()), "no suspend key");
        assert!(c.iter().any(|a| a.contains("proj/shell")), "socket is per project+session");
    }

    #[test]
    fn env_override_replaces_the_command() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        assert_eq!(default_command("proj", "shell"), vec!["cat".to_string()]);
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn smallest_attachment_geometry_wins() {
        let mut sizes = HashMap::new();
        sizes.insert(1u64, (100u16, 40u16));
        sizes.insert(2u64, (80u16, 24u16));
        sizes.insert(3u64, (120u16, 50u16));
        assert_eq!(min_geometry(&sizes), Some((80, 24)), "nobody may see clipped output");
        assert_eq!(min_geometry(&HashMap::new()), None);
    }

    #[test]
    fn key_for_is_the_map_key_shape() {
        assert_eq!(key_for("karpie", "shell"), "karpie/shell");
        // Caller passes the raw slashed project form (as `attach` callers
        // do); key_for encodes it via storage_key internally so the key
        // matches what `attach` actually inserted into the map.
        assert_eq!(key_for("karpie/src", "claude"), "karpie%2Fsrc/claude");
    }

    #[test]
    fn process_age_of_our_own_process_is_small_and_present() {
        // Our own pid is guaranteed to exist; a fresh test process is young.
        let me = std::process::id();
        let age = process_age_secs(me).expect("our own process must be readable");
        assert!(age < 60 * 60, "own process age looked wrong: {age}");
        // A pid that cannot exist yields None rather than panicking.
        assert!(process_age_secs(0).is_none() || process_age_secs(4_294_967_294).is_none());
    }

    #[test]
    fn end_session_ends_one_and_leaves_its_siblings_alone() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        reserve_and_attach("endproj", "term", d.path()).unwrap();
        reserve_and_attach("endproj", "term1", d.path()).unwrap();
        reserve_and_attach("otherend", "term", d.path()).unwrap();

        assert!(end_session("endproj", "term"));

        let names: Vec<String> = list_sessions("endproj").into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["term1"], "only the named session may end");
        assert_eq!(list_sessions("otherend").len(), 1, "another project must be untouched");

        kill_project("endproj");
        kill_project("otherend");
        std::env::remove_var("ROOST_CMD");
    }

    /// The whole point of ending by name: a rubbish name must not be able to
    /// escape its project, and must not report success for work it never did.
    #[test]
    fn end_session_refuses_an_invalid_name_rather_than_reporting_success() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!end_session("endproj", "../../etc/passwd"));
        assert!(!end_session("endproj", ""));
    }

    /// Counting is over `live_names`, which sees sessions this process never
    /// attached to — so a name that is merely *detached* is still taken, and
    /// handing it out would silently reattach the user to an old shell rather
    /// than giving them a new one.
    #[test]
    fn next_free_name_counts_up_and_skips_names_already_live() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();

        assert_eq!(next_free_name("nameproj", &[]).as_deref(), Some("term"));

        reserve_and_attach("nameproj", "term", d.path()).unwrap();
        assert_eq!(next_free_name("nameproj", &[]).as_deref(), Some("term1"));

        reserve_and_attach("nameproj", "term1", d.path()).unwrap();
        assert_eq!(next_free_name("nameproj", &[]).as_deref(), Some("term2"));

        // A gap is reused: term1 ending frees the name below term2.
        end_session("nameproj", "term1");
        assert_eq!(next_free_name("nameproj", &[]).as_deref(), Some("term1"));

        // `also_taken` covers the tab-opened-but-not-yet-connected window,
        // where a name is in no registry yet — two quick clicks must not both
        // be handed the same one.
        assert_eq!(
            next_free_name("nameproj", &["term1".to_string()]).as_deref(),
            Some("term2"),
            "a name already claimed by an open tab is not free"
        );

        kill_project("nameproj");
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn next_free_name_gives_up_at_the_session_cap() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        for i in 0..MAX_SESSIONS_PER_PROJECT {
            let n = if i == 0 { "term".to_string() } else { format!("term{i}") };
            reserve_and_attach("capproj", &n, d.path()).unwrap();
        }
        assert_eq!(live_names("capproj").len(), MAX_SESSIONS_PER_PROJECT);
        assert_eq!(next_free_name("capproj", &[]), None, "the cap refuses, it does not wrap");

        end_session("capproj", "term");
        assert_eq!(
            next_free_name("capproj", &[]).as_deref(),
            Some("term"),
            "ending one must free a slot — otherwise closing tabs can never escape the cap"
        );

        kill_project("capproj");
        std::env::remove_var("ROOST_CMD");
    }

    /// Rule 1, and the reason the whole `Registry` exists: while a project is
    /// closing every attach is refused — including one for a session that is
    /// still in the map, because that session is about to stop existing.
    ///
    /// Deterministic on purpose. The hub-level version of this had to catch
    /// the close still in flight, which is a race; here the state is set
    /// directly, so the assertion is on the rule rather than on who won.
    #[test]
    fn a_close_refuses_every_attach_until_it_finishes() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let live = reserve_and_attach("closerule", "term", d.path()).expect("setup: a live session");
        drop(live);

        let names = begin_close("closerule");
        assert_eq!(names, vec!["term".to_string()], "the drain must report what it took");

        // The existing session's own name, and a fresh one: both refused, and
        // both *as closing* — the reason is the assertion, since "refused
        // because it has ended" would be a different rule firing by luck.
        for n in ["term", "brandnew"] {
            let Err(e) = attach("closerule", n, d.path()) else {
                panic!("attach({n}) must be refused while the project is closing")
            };
            assert_eq!(e, AttachRefusal::Closing.to_string(), "attach({n}) refused for the wrong reason");
        }
        // Even a reservation does not buy past a close.
        reserve("closerule", "reserved", None);
        let Err(e) = attach("closerule", "reserved", d.path()) else {
            panic!("a reservation must not buy past a close")
        };
        assert_eq!(e, AttachRefusal::Closing.to_string());

        end_close("closerule");
        // And the refusal lifts, or a closed project could never be reopened.
        reserve("closerule", "after", None);
        assert!(reserve_and_attach("closerule", "after", d.path()).is_ok(), "the close must lift");
        kill_project("closerule");
        std::env::remove_var("ROOST_CMD");
    }

    /// The other order, which is the half that used to leak. A spawn that
    /// takes the lock before the close must be *visible* to it — the close
    /// drains the map under the same lock, so a session that got in is in the
    /// list the close returns and gets killed, rather than surviving in
    /// neither the scan nor the socket directory.
    #[test]
    fn an_attach_that_wins_the_lock_is_still_seen_by_the_close() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let a = reserve_and_attach("winrace", "term", d.path()).expect("setup");
        let b = reserve_and_attach("winrace", "term1", d.path()).expect("setup");
        drop(a);
        drop(b);

        let mut names = begin_close("winrace");
        names.sort();
        assert_eq!(
            names,
            vec!["term".to_string(), "term1".to_string()],
            "a close must take every session that got in before it"
        );
        end_close("winrace");
        std::env::remove_var("ROOST_CMD");
    }

    /// The invariant in one test: a connect with no reservation creates
    /// nothing. This is what `term.rs` used to do on every websocket, which
    /// made a close something a client could race by simply reconnecting.
    #[test]
    fn a_connect_with_no_reservation_creates_nothing() {
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = tempfile::tempdir().unwrap();

        let Err(e) = attach("noreservation", "term", d.path()) else {
            panic!("an unreserved connect must not create a session")
        };
        assert_eq!(e, AttachRefusal::NotReserved.to_string());
        assert!(live_names("noreservation").is_empty(), "and must leave nothing behind");

        // A reservation is single use: it authorises one spawn, not a name
        // that is spawnable forever. Without this the fix would only move the
        // hole rather than close it.
        reserve("noreservation", "term", None);
        let ok = attach("noreservation", "term", d.path()).expect("the reservation authorises one");
        drop(ok);
        kill_project("noreservation");
        let Err(e) = attach("noreservation", "term", d.path()) else {
            panic!("a consumed reservation must not authorise a second spawn")
        };
        assert_eq!(e, AttachRefusal::NotReserved.to_string(), "the reservation must be consumed");

        std::env::remove_var("ROOST_STATE_DIR");
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn a_pending_launch_rides_the_attach_that_spawns_and_no_other() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        reserve(
            "launchproj",
            "term",
            Some(LaunchRequest { launch: crate::proto::Launch::Claude, session_id: None }),
        );

        let first = reserve_and_attach("launchproj", "term", d.path()).unwrap();
        assert_eq!(
            first.launch,
            Some(LaunchRequest { launch: crate::proto::Launch::Claude, session_id: None }),
            "the attach that spawns carries it"
        );
        let mirror = reserve_and_attach("launchproj", "term", d.path()).unwrap();
        assert_eq!(mirror.launch, None, "a second browser on the same shell must not retype it");

        // Consumed at spawn: when the shell exits and the name is respawned
        // later, the old click must not start claude again.
        kill_project("launchproj");
        let respawn = reserve_and_attach("launchproj", "term", d.path()).unwrap();
        assert_eq!(respawn.launch, None);
        kill_project("launchproj");
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn reallocating_a_name_without_a_launch_clears_one_nobody_connected_for() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        // The ✻ click whose tab was closed before any browser attached.
        reserve(
            "launchproj2",
            "term",
            Some(LaunchRequest { launch: crate::proto::Launch::Claude, session_id: None }),
        );
        // The plain + click that got the same name back.
        reserve("launchproj2", "term", None);
        let a = reserve_and_attach("launchproj2", "term", d.path()).unwrap();
        assert_eq!(a.launch, None, "a stale launch must not leak into an unrelated terminal");
        kill_project("launchproj2");
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn a_launched_session_stays_known_as_launched_for_its_lifetime() {
        // Revert-checked: with `launched` never stored on the Session this
        // fails on the first assertion with an empty Vec.
        let _s = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let req = LaunchRequest { launch: crate::proto::Launch::Claude, session_id: Some("0123abcd-0123-4abc-8abc-0123456789ab".into()) };
        reserve("launched", "term", Some(req.clone()));
        let a = reserve_and_attach("launched", "term", d.path()).unwrap();
        assert_eq!(a.launch, Some(req.clone()), "the spawning attach carries it");
        let _plain = reserve_and_attach("launched", "term1", d.path()).unwrap();
        assert_eq!(launched_names("launched"), vec![("term".to_string(), req)], "only the launched one, still after the launch was consumed");
        let again = reserve_and_attach("launched", "term", d.path()).unwrap();
        assert_eq!(again.launch, None, "a reattach types nothing");
        assert_eq!(launched_names("launched").len(), 1, "…and does not forget");
        kill_project("launched");
        assert!(launched_names("launched").is_empty(), "gone with the session");
    }

    #[test]
    fn listing_and_killing_are_scoped_to_one_project() {
        // ROOST_CMD is process-global; hold the lock for the whole body.
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        // Two projects, so we can prove kill_project does not spill over.
        reserve_and_attach("listproj", "shell", d.path()).unwrap();
        reserve_and_attach("listproj", "claude", d.path()).unwrap();
        reserve_and_attach("otherproj", "shell", d.path()).unwrap();

        let mut names: Vec<String> = list_sessions("listproj").into_iter().map(|s| s.name).collect();
        names.sort();
        assert_eq!(names, vec!["claude", "shell"]);
        assert!(has_session("listproj", "shell"));
        assert!(!has_session("listproj", "nope"));

        let ended = kill_project("listproj");
        assert_eq!(ended, 2);
        assert!(list_sessions("listproj").is_empty(), "all of the project's sessions must go");
        assert_eq!(list_sessions("otherproj").len(), 1, "another project must be untouched");

        kill_project("otherproj");
        std::env::remove_var("ROOST_CMD");
    }

    /// `list_sessions` forks `ps` once per session, and it runs on a request
    /// path (`registry::known_projects`, on every picker load, workspace load
    /// and strip refresh) while the PTY pump re-takes this same mutex for every
    /// chunk of terminal output. Resolving ages under the guard therefore froze
    /// terminal output for every session in every project for the duration of
    /// those forks — the "never hold a lock across blocking I/O" constraint this
    /// project has already shipped one deadlock against.
    ///
    /// Measured as a **ratio**, not against a fixed millisecond budget: how
    /// long a competing thread waits to acquire the registry mutex, against how
    /// long the whole `list_sessions` call takes. Under the fix the wait is a
    /// map scan (near zero) while the call is dominated by forks; under the bug
    /// the two are the same number, because the forks happen inside the
    /// critical section. That self-calibrates to whatever the machine's fork
    /// cost is, which a fixed threshold cannot — and a fixed threshold would
    /// also have passed with the bug in place, since simply waiting out a
    /// holder is not the behaviour under test.
    #[test]
    fn list_sessions_does_not_hold_the_registry_lock_across_its_ps_forks() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        // Enough sessions that the forks dominate the call and the ratio has
        // something to measure.
        let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
        for name in names {
            reserve_and_attach("lockproj", name, d.path()).unwrap();
        }
        // Warm up, so the measurement is not about first-fork cost.
        assert_eq!(list_sessions("lockproj").len(), names.len());

        // The contender acquires *repeatedly* until told to stop, rather than
        // timing one acquisition. A single timed acquire can pass with the bug
        // fully in place: if the scheduler preempts this thread between the
        // start signal and `list_sessions` taking the lock, the contender
        // acquires uncontended, measures ~0, and the assertion succeeds. That
        // is a flaky *pass*, which CLAUDE.md names as this codebase's dominant
        // failure mode — a test that cannot fail is worse than no test.
        //
        // Looping pins two independent properties that the bug breaks together
        // and a scheduling accident cannot satisfy: no single acquisition waits
        // for a large fraction of the call, *and* the lock was genuinely
        // available many times during it. Under the bug the contender blocks
        // once for the whole fork run, so it fails both — one long wait, and
        // far too few acquisitions.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let contender = std::thread::spawn(move || {
            started_rx.recv().unwrap();
            let mut acquisitions = 0usize;
            let mut max_wait = std::time::Duration::ZERO;
            while !stop_for_thread.load(std::sync::atomic::Ordering::Relaxed) {
                let t = std::time::Instant::now();
                let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
                max_wait = max_wait.max(t.elapsed());
                drop(guard);
                acquisitions += 1;
                std::thread::yield_now();
            }
            (acquisitions, max_wait)
        });

        started_tx.send(()).unwrap();
        let start = std::time::Instant::now();
        let listed = list_sessions("lockproj");
        let total = start.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (acquisitions, max_wait) = contender.join().unwrap();

        assert_eq!(listed.len(), names.len(), "the listing itself must still be correct");
        assert!(
            max_wait * 2 < total,
            "a competing thread's longest wait for the registry lock was {max_wait:?} out of a \
             {total:?} list_sessions call — the forks are happening inside the critical section, \
             which stalls every terminal's output on this process for their duration"
        );
        assert!(
            acquisitions >= 5,
            "a competing thread got the registry lock only {acquisitions} time(s) during a \
             {total:?} list_sessions call — it is held across the `ps` forks, not just the map scan"
        );

        kill_project("lockproj");
        std::env::remove_var("ROOST_CMD");
    }

    /// Revert-checked: `.min()` instead of `.max()` fails with
    /// `left: Some(30) right: Some(64000)` — the master is the oldest holder.
    #[test]
    fn session_age_is_the_oldest_holder_never_zero_when_unknown() {
        let mut ages = HashMap::new();
        ages.insert(10u32, 64_000u64); // the master
        ages.insert(11u32, 30u64);     // this resh's fresh client
        assert_eq!(oldest_age_of(&[10, 11], &ages), Some(64_000));
        assert_eq!(oldest_age_of(&[99], &ages), None, "no known holder: unknown, not 0");
        assert_eq!(oldest_age_of(&[], &ages), None);
    }

    #[test]
    fn parse_ages_reads_pid_and_etime_columns() {
        // `ps -o pid=,etime=` output: pid, whitespace, etime, one row per line.
        // Revert-checked: hardcoding the inserted age to 0 instead of
        // `parse_etime`'s result fails this with
        // `left: Some(0)\n right: Some(310)`; restored.
        let out = "  123 05:10\n 4567 1-02:03:04\n89 00:42\n";
        let m = parse_ages(out);
        assert_eq!(m.get(&123), Some(&310)); // 5m10s
        assert_eq!(m.get(&4567), Some(&(93784))); // 1d2h3m4s
        assert_eq!(m.get(&89), Some(&42));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn parse_ages_skips_a_malformed_line_rather_than_panicking() {
        // Revert-checked: an `unwrap` on the split fails this instead of skipping.
        let m = parse_ages("nonsense\n  7 03:00\n");
        assert_eq!(m.get(&7), Some(&180));
        assert_eq!(m.len(), 1);
    }

    /// After a resh restart the in-memory map is empty while the sockets
    /// on disk still name every surviving shell. Revert-checked: without the
    /// socket-floor merge this failed with `left: [] right: [("term7", 0, 0)]`.
    #[test]
    fn session_rows_includes_sessions_known_only_from_their_sockets() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let sockdir = d.path().join("sock").join(crate::projects::storage_key("ghostproj"));
        std::fs::create_dir_all(&sockdir).unwrap();
        std::fs::write(sockdir.join("term7"), b"").unwrap();
        std::fs::write(sockdir.join(".origin"), b"/x\n").unwrap();
        let rows = session_rows("ghostproj");
        assert_eq!(rows, vec![("term7".to_string(), 0u32, 0usize)], "socket-only row, pid 0 sentinel: {rows:?}");
        assert!(live_rows("ghostproj").is_empty(), "live_rows stays in-memory only (list_sessions' contract)");
    }

    // The "no ps" property is structural (live_rows has no ps call); this asserts the scan's data is real.
    #[test]
    fn live_rows_lists_a_projects_sessions_without_forking_ps() {
        let _s = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let _a = reserve_and_attach("ovproj", "term", d.path()).unwrap();
        let _b = reserve_and_attach("ovproj", "term1", d.path()).unwrap();
        let rows = live_rows("ovproj");
        assert_eq!(rows.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>(), vec!["term", "term1"]);
        // Revert-checked: hardcoding live_rows to emit pid 0 per row fails this
        // with `live_rows must carry real pids: [("term", 0, 1), ("term1", 0, 1)]`; restored.
        assert!(rows.iter().all(|(_, pid, _)| *pid != 0), "live_rows must carry real pids: {rows:?}");
        // `attach` itself registers a subscriber, so each fresh session already shows 1 attached.
        assert_eq!(rows.iter().map(|(_, _, attached)| *attached).collect::<Vec<_>>(), vec![1, 1]);
        kill_project("ovproj");
        std::env::remove_var("ROOST_CMD");
    }

    /// Minimal, test-only "is anything holding this path" check via `ps`.
    /// Deliberately not `registry`'s own `socket_has_process` (module-
    /// private, and hardened against regex metacharacters / spaces in a way
    /// this test's own known, plain tempdir paths don't need) — just enough
    /// to prove a real process is or isn't there.
    fn any_process_holds(path: &std::path::Path) -> bool {
        let target = path.to_string_lossy();
        let out = std::process::Command::new("ps")
            .args(["-Ao", "args="])
            .output()
            .expect("ps must be runnable for this check to mean anything");
        // C1's exact shape, inside the test that guards C1: treating a
        // failed or empty `ps` as `false` ("nothing holds it") would let
        // the central `assert!(!any_process_holds(&sock))` below pass
        // vacuously on a broken `ps`, proving nothing. Panic instead — a
        // test that can't verify what it's asserting must not report a
        // pass.
        assert!(
            out.status.success() && !out.stdout.is_empty(),
            "ps failed or returned nothing; this test cannot trust its own assertions right now"
        );
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(target.as_ref()))
    }

    // The regression this whole task exists to fix. ROOST_CMD=cat (used
    // everywhere else in this file, including the test just above) cannot
    // reproduce it at all: a `cat` child has no detached master to leave
    // behind, so killing it really does end the "session" outright — that
    // gap is exactly why a fully-passing suite never caught this bug in the
    // first place. This test uses the real, unoverridden `dtach` command
    // (a runtime prerequisite of this project) and checks OS-level process
    // state directly, not just the in-memory session map, since the map
    // alone is exactly what lied about this before the fix.
    #[test]
    fn kill_project_ends_the_detached_dtach_master_and_shell_not_just_the_client() {
        // STATE before SESSION — the one total order the whole suite uses;
        // see SESSION_ENV_LOCK's doc comment. This site had them inverted,
        // which no amount of green runs could have revealed.
        let _g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let dir = tempfile::tempdir().unwrap();

        let attach_result = reserve_and_attach("realdtach", "shell", dir.path());
        let Ok(_att) = attach_result else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("ROOST_STATE_DIR");
            return;
        };

        let sock = sock_path("realdtach", "shell");
        // Poll rather than a fixed sleep: dtach's own fork-and-detach takes
        // an unpredictable, usually-small amount of wall time to complete.
        // Budget deliberately far above the ~50ms this normally needs — the
        // loop exits as soon as the condition holds, so a generous ceiling is
        // free on a fast run, while a tight one lets a loaded machine turn a
        // setup assert into what looks like a real product failure.
        let mut waited = 0;
        while !(sock.exists() && any_process_holds(&sock)) && waited < 200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            waited += 1;
        }
        assert!(sock.exists(), "test setup: dtach must have created its socket");
        assert!(
            any_process_holds(&sock),
            "test setup: a detached dtach master must be observable before kill_project runs \
             — otherwise this test would prove nothing"
        );

        let ended = kill_project("realdtach");

        assert_eq!(ended, 1, "the session must be reported as fully ended, not merely detached");
        assert!(
            !any_process_holds(&sock),
            "the dtach master — and, through it, the shell — must actually be dead, \
             not merely the in-process client this function used to kill alone"
        );
        assert!(!sock.exists(), "the socket must be removed only once the holding process is confirmed gone");

        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A session that outlived a resh restart: its dtach master and shell
    /// are running and its socket is on disk, but this process never attached
    /// to it, so the in-memory map knows nothing about it.
    ///
    /// Both halves used to walk that map alone, so for exactly the sessions
    /// dtach exists to preserve, the workspace reported "No terminal sessions
    /// are running" and Close Project ended nothing — observed in production as
    /// a Close button that appeared dead. Simulated here by starting a real
    /// dtach the way resh does and then clearing the map, which is what a
    /// restart leaves behind.
    ///
    /// `ROOST_CMD=cat` cannot express this at all: a `cat` child leaves no
    /// detached master, so there is nothing to survive.
    #[test]
    fn a_session_that_outlived_a_restart_is_listed_and_can_be_closed() {
        let _g1 = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g2 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_CMD");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let dir = tempfile::tempdir().unwrap();

        let Ok(att) = reserve_and_attach("survivor", "shell", dir.path()) else {
            eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
            std::env::remove_var("ROOST_STATE_DIR");
            return;
        };
        let sock = sock_path("survivor", "shell");
        let mut waited = 0;
        while !(sock.exists() && any_process_holds(&sock)) && waited < 200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            waited += 1;
        }
        assert!(any_process_holds(&sock), "test setup: a detached master must exist");

        // Forget it, exactly as a restart would: the socket and its master
        // survive, the map does not.
        drop(att);
        sessions().lock().unwrap_or_else(|e| e.into_inner()).map.clear();
        assert!(
            !has_session("survivor", "shell"),
            "test setup: the map must be empty, or this is not the restart case"
        );

        assert_eq!(
            live_names("survivor"),
            vec!["shell".to_string()],
            "a surviving session must still be listed, or the UI claims nothing is running \
             and offers no way to end it"
        );

        let ended = kill_project("survivor");
        assert_eq!(ended, 1, "Close Project must end a session it never attached to");
        assert!(
            !any_process_holds(&sock),
            "the surviving master and its shell must actually be dead"
        );
        assert!(!sock.exists(), "and its socket removed once the holder is confirmed gone");

        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn nested_project_sessions_are_found_and_never_alias_the_flat_prefix() {
        // Regression for the aliasing hazard key_for's doc comment describes:
        // project "nest" + session "sub" (raw key "nest/sub") and project
        // "nest/sub" + session "shell" (encoded key "nest%2Fsub/shell") both
        // start with "nest/" as *text*, but must never be confused by any
        // prefix-based lookup. Every other test here uses a flat project
        // name, so storage_key is the identity function and this bug is
        // invisible to them — that's exactly why this dedicated case exists.
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let d = tempfile::tempdir().unwrap();

        reserve_and_attach("nest/sub", "shell", d.path()).unwrap();
        reserve_and_attach("nest", "sub", d.path()).unwrap();

        assert!(has_session("nest/sub", "shell"));
        let nested_names: Vec<String> =
            list_sessions("nest/sub").into_iter().map(|s| s.name).collect();
        assert_eq!(nested_names, vec!["shell"]);

        // "nest"'s own session ("sub") must not be shadowed or duplicated by
        // "nest/sub"'s.
        let flat_names: Vec<String> = list_sessions("nest").into_iter().map(|s| s.name).collect();
        assert_eq!(flat_names, vec!["sub"]);

        // Ending project "nest" must leave "nest/sub"'s session running.
        let ended = kill_project("nest");
        assert_eq!(ended, 1);
        assert!(
            has_session("nest/sub", "shell"),
            "kill_project(\"nest\") must not end nest/sub's session"
        );

        kill_project("nest/sub");
        std::env::remove_var("ROOST_CMD");
    }

    #[test]
    fn a_terminal_carries_the_roost_environment_contract() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // `env` prints the child's environment to its stdout, which is the PTY —
        // so it arrives back through this attachment's own subscriber channel.
        std::env::set_var("ROOST_CMD", "env");
        // The child inherits this process's environment, and on the author's
        // host this suite runs *inside* a roost terminal — so the old and new
        // prefixes may both already be present, exported by the terminal we
        // are in rather than by the code under test. Clear them so the
        // assertions below see only what `session_env` exports.
        // concat! keeps the old prefix out of the whole-repo sweeps that
        // verify it is gone; this loop needs the literal old prefix, not
        // the renamed one, to actually clear a legacy terminal's exports.
        for prefix in [concat!("RESH", "_"), "ROOST_"] {
            for name in ["NOTIFY", "PROJECT", "SESSION"] {
                std::env::remove_var(format!("{prefix}{name}"));
            }
        }
        let d = tempfile::tempdir().unwrap();
        let att = reserve_and_attach("envproj", "shell", d.path()).expect("attach");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = String::new();
        while std::time::Instant::now() < deadline {
            match att.rx.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(chunk) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                    if seen.contains("ROOST_SESSION") {
                        break;
                    }
                }
                Err(_) => {}
            }
        }
        kill_project("envproj");
        std::env::remove_var("ROOST_CMD");

        assert!(seen.contains("ROOST_NOTIFY=1"), "child env lacked ROOST_NOTIFY: {seen:?}");
        assert!(seen.contains("ROOST_PROJECT=envproj"), "child env lacked ROOST_PROJECT: {seen:?}");
        assert!(seen.contains("ROOST_SESSION=shell"), "child env lacked ROOST_SESSION: {seen:?}");
        // concat! keeps the literal out of the whole-repo sweeps that verify
        // the old prefix is gone.
        assert!(
            !seen.contains(concat!("RESH", "_")),
            "a terminal still exports the old prefix, so hooks would see both: {seen:?}"
        );
    }
}
