//! Terminal session registry. deadlight owns the PTY; dtach owns survival
//! across a deadlight restart. Multiple attachments to one session mirror.
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_SCROLLBACK: usize = 1_000_000;
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
/// regress. Mirrors resolve_project's *syntactic* checks only (no empty,
/// absolute, or leading-dot segment) — it has no `roots` to canonicalize
/// against, and doesn't need one, since the filesystem-confinement half of
/// that check already happened before this project ever had a live `dir`
/// to spawn a shell in. A nested project (`karpie/src`) is legitimate here.
fn valid_project(project: &str) -> bool {
    !project.is_empty() && project.split('/').all(|s| !s.is_empty() && !s.starts_with('.'))
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
fn sock_path(project: &str, name: &str) -> PathBuf {
    crate::wsstate::state_dir()
        .join("sock")
        .join(crate::projects::storage_key(project))
        .join(name)
}

pub fn default_command(project: &str, name: &str) -> Vec<String> {
    if let Ok(c) = std::env::var("DEADLIGHT_CMD") {
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

pub fn push_scrollback(ring: &mut VecDeque<u8>, data: &[u8]) {
    ring.extend(data.iter().copied());
    while ring.len() > MAX_SCROLLBACK {
        ring.pop_front();
    }
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
    scrollback: VecDeque<u8>,
    subs: HashMap<u64, SyncSender<Vec<u8>>>,
    sizes: HashMap<u64, (u16, u16)>,
    next_id: u64,
}

pub struct Attachment {
    pub id: u64,
    pub key: String,
    pub rx: Receiver<Vec<u8>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Attach to a session, creating it if needed. The new subscriber is sent the
/// scrollback immediately so a reconnecting browser sees where it was.
///
/// Locking discipline: the registry mutex is held only for the short,
/// non-blocking bookkeeping steps (map lookups, inserting a new Session,
/// registering a subscriber). It is never held across a blocking read or
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
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let live_for_project = map.keys().filter(|k| k.starts_with(&format!("{skey}/"))).count();
    if !map.contains_key(&key) && live_for_project >= MAX_SESSIONS_PER_PROJECT {
        return Err("too many terminal sessions".into());
    }

    if !map.contains_key(&key) {
        let cmd = default_command(project, name);
        if cmd.is_empty() {
            return Err("empty command".into());
        }
        // dtach -A refuses to create a socket in a directory that doesn't
        // exist yet; nothing else creates it. 0o700: this directory grants
        // shell access to whoever can connect to a socket in it. Gated on
        // the command actually being dtach so DEADLIGHT_CMD-overridden test
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
            }
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cb = CommandBuilder::new(&cmd[0]);
        cb.args(&cmd[1..]);
        cb.cwd(dir);
        cb.env("TERM", "xterm-256color");
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
                scrollback: VecDeque::new(),
                subs: HashMap::new(),
                sizes: HashMap::new(),
                next_id: 0,
            },
        );
        let pump_key = key.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                // The blocking read happens with the lock released: only the
                // fan-out after a chunk arrives needs the registry.
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
                        let Some(s) = map.get_mut(&pump_key) else { break };
                        push_scrollback(&mut s.scrollback, &buf[..n]);
                        let chunk = buf[..n].to_vec();
                        // try_send, not send: a subscriber whose queue is full
                        // (frozen tab, dead socket) is dropped rather than
                        // backing up the whole fan-out (I4).
                        s.subs.retain(|_, tx| tx.try_send(chunk.clone()).is_ok());
                    }
                }
            }
            // PTY closed: drop the session so the next attach respawns it.
            let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut s) = map.remove(&pump_key) {
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        });
    }

    let s = map.get_mut(&key).ok_or("session vanished")?;
    s.next_id += 1;
    let id = s.next_id;
    let (tx, rx) = sync_channel(SUB_CHANNEL_CAP);
    let replay: Vec<u8> = s.scrollback.iter().copied().collect();
    if !replay.is_empty() {
        let _ = tx.try_send(replay);
    }
    s.subs.insert(id, tx);
    Ok(Attachment { id, key, rx })
}

/// Takes the registry lock only long enough to clone the writer's `Arc`,
/// then drops it before the blocking write. Holding the registry lock across
/// I/O to a child would deadlock: if the child stops draining stdin (e.g.
/// it's itself blocked writing output faster than the browser reads it), the
/// write blocks indefinitely, and the pump thread — the only thing that can
/// unblock it by draining output — needs that same lock to make progress.
pub fn write_input(key: &str, data: &[u8]) -> Result<(), String> {
    let writer = {
        let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let s = map.get_mut(key).ok_or("no such session")?;
        s.writer.clone()
    };
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    w.write_all(data).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

pub fn resize(key: &str, id: u64, cols: u16, rows: u16) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = map.get_mut(key) else { return };
    s.sizes.insert(id, (cols, rows));
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

/// Detach only. The PTY keeps running and dtach keeps the session alive, so
/// reopening the same name reattaches.
pub fn detach(key: &str, id: u64) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
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
    pub age_secs: u64,
    pub attached: usize,
}

/// Elapsed seconds for a pid, via `ps -o etime=`. Age is read from the OS
/// rather than recorded in memory because dtach sessions outlive deadlight —
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
pub fn list_sessions(project: &str) -> Vec<SessionInfo> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<SessionInfo> = map
        .iter()
        .filter_map(|(k, s)| {
            let name = k.strip_prefix(&prefix)?;
            let pid = s.child_pid;
            Some(SessionInfo {
                name: name.to_string(),
                pid,
                age_secs: process_age_secs(pid).unwrap_or(0),
                attached: s.subs.len(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn has_session(project: &str, name: &str) -> bool {
    let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    map.contains_key(&key_for(project, name))
}

/// Ends every session belonging to one project. This is the only way to end
/// a session from the UI; detaching a tab deliberately leaves it running.
/// Prefix built from `storage_key` for the same reason as `list_sessions`.
pub fn kill_project(project: &str) -> usize {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let keys: Vec<String> = map.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    let mut ended = 0;
    for k in keys {
        if let Some(mut s) = map.remove(&k) {
            let _ = s.child.kill();
            let _ = s.child.wait();
            ended += 1;
        }
    }
    ended
}

// Serializes tests that mutate the process-global DEADLIGHT_CMD env var.
// cargo runs a binary's tests in parallel threads by default, and an env var
// is process-wide state, so two such tests interleaving would have one see
// the other's value mid-test (or after its cleanup already ran). This
// project shipped exactly that flakiness once before; every test below that
// touches DEADLIGHT_CMD takes this lock for its whole body.
#[cfg(test)]
pub static SESSION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

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
        let c = default_command("proj", "shell");
        assert_eq!(c[0], "dtach");
        assert!(c.contains(&"-E".to_string()), "no escape character");
        assert!(c.contains(&"-z".to_string()), "no suspend key");
        assert!(c.iter().any(|a| a.contains("proj/shell")), "socket is per project+session");
    }

    #[test]
    fn env_override_replaces_the_command() {
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("DEADLIGHT_CMD", "cat");
        assert_eq!(default_command("proj", "shell"), vec!["cat".to_string()]);
        std::env::remove_var("DEADLIGHT_CMD");
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
    fn scrollback_ring_is_bounded() {
        let mut ring = VecDeque::new();
        for _ in 0..(MAX_SCROLLBACK / 10 + 100) {
            push_scrollback(&mut ring, &[b'x'; 10]);
        }
        assert!(ring.len() <= MAX_SCROLLBACK);
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
    fn listing_and_killing_are_scoped_to_one_project() {
        // DEADLIGHT_CMD is process-global; hold the lock for the whole body.
        let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("DEADLIGHT_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        // Two projects, so we can prove kill_project does not spill over.
        attach("listproj", "shell", d.path()).unwrap();
        attach("listproj", "claude", d.path()).unwrap();
        attach("otherproj", "shell", d.path()).unwrap();

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
        std::env::remove_var("DEADLIGHT_CMD");
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
        std::env::set_var("DEADLIGHT_CMD", "cat");
        let d = tempfile::tempdir().unwrap();

        attach("nest/sub", "shell", d.path()).unwrap();
        attach("nest", "sub", d.path()).unwrap();

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
        std::env::remove_var("DEADLIGHT_CMD");
    }
}
