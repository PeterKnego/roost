//! The lock file that tells Claude Code this project has an IDE socket.
//!
//! Claude Code discovers IDEs by scanning `~/.claude/ide/*.lock`; the filename
//! is the port and the contents carry the token that authenticates the
//! socket. Two properties are load-bearing and neither is obvious.
//!
//! The write is atomic because the CLI *deletes* any lock file it cannot
//! parse. A half-written file therefore does not degrade the integration, it
//! silently unregisters it — so a reader must never see a partial one.
//!
//! Removal only ever touches the path this process wrote. The directory is
//! shared with every other IDE on the host: a sweep of "stale-looking"
//! entries would unlink a live IntelliJ's registration the moment a check
//! failed, which is exactly the class of defect CLAUDE.md's table is about.
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Test-only redirect for `ide_dir()`. Not `#[cfg(test)]`: `tests/integration.rs`
/// links this crate as an ordinary dependency (no `cfg(test)`), so a
/// cfg-gated item would be invisible to it — the override has to be a real,
/// always-compiled item that test code opts into via `set_ide_dir_for_test`.
///
/// Task 5's review (finding 2): without this, `cargo test` wrote real lock
/// files into the developer's actual `~/.claude/ide` — a directory shared
/// with every other real IDE on the host, whose stale-entry cleanup is the
/// `claude` CLI's job, not this codebase's, so test debris left there
/// becomes the CLI's problem. Deliberately a `OnceLock` set at most once to
/// one directory shared by every test in the process, not a per-test value
/// and not an env var: a per-test directory would need its own
/// `STATE_ENV_LOCK`-style global lock to avoid one test's `set` racing
/// another's `ide_dir()` read, for no benefit — `ide`'s own registry is
/// already keyed by project name, and lock filenames are already keyed by
/// port, so many unrelated test "projects" sharing one directory cannot
/// collide there either.
static TEST_IDE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Redirects `ide_dir()` for the remainder of this process. Idempotent — a
/// second call (from another test, or another test module) is a silent
/// no-op — so any number of call sites can each set it defensively without
/// coordinating who goes first.
/// Points `ide_dir()` at one stable, reused directory for this user.
///
/// Prefer this over handing `set_ide_dir_for_test` a `tempfile::TempDir`:
/// the override is a process-global `OnceLock`, so the directory outlives
/// whichever test supplied it, and every later write recreates it after that
/// `TempDir` has been removed — one leaked directory per test process, which
/// is how 154 of them accumulated in /tmp before this was measured. A stable
/// path keeps the isolation (nothing here touches the real `~/.claude/ide`)
/// without the accumulation. Idempotent, so every test can call it.
pub fn isolate_ide_dir_for_test() {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let d = DIR.get_or_init(|| {
        let who = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        let p = std::env::temp_dir().join(format!("resh-test-ide-{who}"));
        let _ = std::fs::create_dir_all(&p);
        p
    });
    set_ide_dir_for_test(d.clone());
}

pub fn set_ide_dir_for_test(dir: PathBuf) {
    let _ = TEST_IDE_DIR.set(dir);
}

/// `$CLAUDE_CONFIG_DIR/ide` when set — the CLI honours the same override, so
/// a user who relocated their Claude config still finds us.
pub fn ide_dir() -> PathBuf {
    if let Some(d) = TEST_IDE_DIR.get() {
        return d.clone();
    }
    // A test that forgets to call `set_ide_dir_for_test` must not silently
    // write into the developer's real `~/.claude/ide` — a directory shared
    // with every other IDE on the host. That already happened once on this
    // branch and left 17 stale lock files behind. `cfg!(test)` only covers
    // this crate's own `cargo test` (the lib-test binary); `tests/integration.rs`
    // links this crate as an ordinary dependency with no `cfg(test)`, so an
    // integration test that forgot the same call still falls through to the
    // real directory below — that gap is closed separately, by
    // `tests/integration.rs` calling `set_ide_dir_for_test` itself, not by
    // this check.
    if cfg!(test) {
        panic!(
            "ide_dir() called with no override set — call \
             idelock::set_ide_dir_for_test(tempdir) first, or this test would \
             write real lock files into ~/.claude/ide"
        );
    }
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d).join("ide");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/ide")
}

/// 128 bits from the OS CSPRNG, hex-encoded — the same shape the CLI's own
/// extensions use. `/dev/urandom` rather than a dependency: this is the only
/// randomness resh needs, and the deploy target is Linux.
pub fn new_token() -> Result<String, String> {
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| format!("no CSPRNG: {e}"))?;
    let mut buf = [0u8; 16];
    f.read_exact(&mut buf).map_err(|e| format!("short read from /dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn temp_name(port: u16) -> String {
    // Pid-unique: two resh instances share this directory, and a shared temp
    // name lets one truncate the other's in-flight write.
    format!(".{}.{}.resh.tmp", port, std::process::id())
}

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Exactly this path. Never a scan of the directory.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What a sweep did, so startup can say it rather than doing it silently.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed: usize,
    /// Left alone, for any reason — not ours, still live, or unreadable.
    pub kept: usize,
}

/// Is this pid running? Three outcomes, not two — the same rule `idecwd`
/// applies to `/proc`, for the same reason: "I could not look" must never
/// become "it is gone", because the branch that follows deletes something.
fn pid_state(proc_root: &Path, pid: u32) -> Option<bool> {
    match std::fs::symlink_metadata(proc_root.join(pid.to_string())) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Distinguish "no such process" from "no /proc at all".
            match std::fs::symlink_metadata(proc_root) {
                Ok(_) => Some(false),
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

/// Is something accepting connections on this loopback port?
fn port_busy(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}

/// Removes the lock files a *previous* resh left behind.
///
/// systemd stops resh with `SIGTERM`, which unwinds nothing, so `Lock::drop`
/// never runs and every restart strands one lock file per open project. The
/// `claude` CLI does reap stale entries by pid, but leaving our debris for it
/// to clear puts this codebase's mess in a directory it shares with every
/// other IDE on the host — so resh cleans up after itself.
///
/// Three conditions, all required, because this directory is not ours:
/// the file says `ideName: resh`, its pid is *known* dead, and nothing is
/// listening on the port it advertises. Any doubt at all — an unparseable
/// file, an unreadable `/proc`, a port that answers — and the file stays.
/// A stale row is recoverable; deleting a live IntelliJ's registration is not,
/// and neither is deleting the lock of a resh that is still serving.
///
/// Deliberately *not* the CLI's rule: it unlinks lock files it cannot parse.
/// That is right for the client, which owns the directory's hygiene; it is
/// wrong for us, because a file we cannot read is a file we cannot claim.
pub fn sweep_strays_in(dir: &Path, proc_root: &Path) -> SweepReport {
    let mut r = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Cannot list it — that is not evidence of anything.
        return r;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Some(port) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".lock"))
            .and_then(|n| n.parse::<u16>().ok())
        else {
            r.kept += 1;
            continue;
        };
        let ours_and_dead = (|| {
            let text = std::fs::read_to_string(&path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            if v.get("ideName").and_then(|x| x.as_str()) != Some("resh") {
                return Some(false);
            }
            let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
            // `Some(true)` means alive, `None` means we could not tell.
            match pid_state(proc_root, pid) {
                Some(false) => Some(true),
                _ => Some(false),
            }
        })()
        .unwrap_or(false);
        if ours_and_dead && !port_busy(port) {
            match std::fs::remove_file(&path) {
                Ok(()) => r.removed += 1,
                Err(_) => r.kept += 1,
            }
        } else {
            r.kept += 1;
        }
    }
    r
}

pub fn sweep_strays() -> SweepReport {
    sweep_strays_in(&ide_dir(), Path::new("/proc"))
}

pub fn write_in(dir: &Path, port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "pid": std::process::id(),
        "workspaceFolders": [workspace.to_string_lossy()],
        "ideName": "resh",
        "transport": "ws",
        "authToken": token,
    })
    .to_string();
    let tmp = dir.join(temp_name(port));
    let path = dir.join(format!("{port}.lock"));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(Lock { path })
}

pub fn write(port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    write_in(&ide_dir(), port, token, workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dead pid that cannot be reused during the test: pick one, confirm
    /// it is absent, and fabricate the /proc root so nothing depends on the
    /// host's real process table.
    fn fake_proc(alive: &[u32]) -> tempfile::TempDir {
        let p = tempfile::tempdir().unwrap();
        for pid in alive {
            std::fs::create_dir(p.path().join(pid.to_string())).unwrap();
        }
        p
    }

    fn write_lock(dir: &Path, port: u16, ide: &str, pid: u32) -> std::path::PathBuf {
        let f = dir.join(format!("{port}.lock"));
        std::fs::write(
            &f,
            serde_json::json!({"pid": pid, "workspaceFolders": ["/w"], "ideName": ide,
                               "transport": "ws", "authToken": "x"})
            .to_string(),
        )
        .unwrap();
        f
    }

    #[test]
    fn a_stray_of_ours_is_removed() {
        let d = tempfile::tempdir().unwrap();
        let proc = fake_proc(&[]);
        let f = write_lock(d.path(), 5501, "resh", 4242);
        let r = sweep_strays_in(d.path(), proc.path());
        assert_eq!(r, SweepReport { removed: 1, kept: 0 });
        assert!(!f.exists());
    }

    #[test]
    fn another_ides_lock_is_never_touched() {
        // The whole reason this is three conditions and not one. This
        // directory is shared with every real IDE on the host, and a live
        // IntelliJ's registration looks exactly like ours apart from the
        // name. Dropping the `ideName` check makes this the only failing
        // test — verified by doing it.
        let d = tempfile::tempdir().unwrap();
        let proc = fake_proc(&[]);
        let f = write_lock(d.path(), 5502, "IntelliJ IDEA", 4242);
        let r = sweep_strays_in(d.path(), proc.path());
        assert_eq!(r, SweepReport { removed: 0, kept: 1 });
        assert!(f.exists(), "a foreign lock must survive even when its pid is dead");
    }

    #[test]
    fn a_live_resh_keeps_its_lock() {
        // Deleting a serving instance's lock unregisters a working
        // integration: `claude` stops finding it, silently.
        let d = tempfile::tempdir().unwrap();
        let proc = fake_proc(&[4242]);
        let f = write_lock(d.path(), 5503, "resh", 4242);
        let r = sweep_strays_in(d.path(), proc.path());
        assert_eq!(r, SweepReport { removed: 0, kept: 1 });
        assert!(f.exists());
    }

    #[test]
    fn a_lock_we_cannot_parse_is_left_alone() {
        // "I could not read it" is not "it is mine and it is dead". The CLI
        // does delete unparseable locks — that is its directory to keep tidy,
        // and the opposite rule for us: a file we cannot read is a file we
        // cannot claim.
        let d = tempfile::tempdir().unwrap();
        let proc = fake_proc(&[]);
        let f = d.path().join("5504.lock");
        std::fs::write(&f, "{ this is not json").unwrap();
        let r = sweep_strays_in(d.path(), proc.path());
        assert_eq!(r, SweepReport { removed: 0, kept: 1 });
        assert!(f.exists());
    }

    #[test]
    fn an_unreadable_proc_means_we_cannot_tell_so_nothing_is_removed() {
        // `pid_state` returns None, which must not collapse into "dead".
        let d = tempfile::tempdir().unwrap();
        let absent = d.path().join("no-proc-here");
        let f = write_lock(d.path(), 5505, "resh", 4242);
        let r = sweep_strays_in(d.path(), &absent);
        assert_eq!(r, SweepReport { removed: 0, kept: 1 });
        assert!(f.exists());
    }

    #[test]
    fn a_dead_pid_whose_port_still_answers_is_left_alone() {
        // The port outlived the pid in the lock — something is serving on it,
        // so the advertised endpoint is real even if the recorded pid is not.
        let d = tempfile::tempdir().unwrap();
        let proc = fake_proc(&[]);
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let f = write_lock(d.path(), port, "resh", 4242);
        let r = sweep_strays_in(d.path(), proc.path());
        assert_eq!(r, SweepReport { removed: 0, kept: 1 });
        assert!(f.exists(), "a port that answers means the endpoint is live");
    }

    #[test]
    fn an_unreadable_directory_is_not_an_empty_one() {
        // Reports nothing rather than claiming a clean sweep of a directory
        // it never managed to list.
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("gone");
        assert_eq!(sweep_strays_in(&missing, Path::new("/proc")), SweepReport::default());
    }

    #[test]
    fn a_token_is_thirty_two_hex_chars_and_not_a_constant() {
        let a = new_token().expect("/dev/urandom must be readable");
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32, "128 bits, hex-encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Reverting to a fixed token passes every other test in this file;
        // only this assertion fails.
        assert_ne!(a, b, "two tokens must differ or the CSPRNG is not being read");
    }

    #[test]
    fn the_lock_file_carries_what_the_cli_reads_out_of_it() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        assert_eq!(lock.path().file_name().unwrap(), "5599.lock", "the CLI parses the port out of the filename");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(lock.path()).unwrap()).unwrap();
        assert_eq!(v["pid"], serde_json::json!(std::process::id()));
        assert_eq!(v["transport"], "ws");
        assert_eq!(v["authToken"], "cafe");
        assert_eq!(v["ideName"], "resh");
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
    }

    #[test]
    fn writing_leaves_no_temp_file_behind_and_replaces_an_existing_one() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("5599.lock"), "stale garbage").unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["5599.lock".to_string()], "a temp file must not survive the write");
        assert!(std::fs::read_to_string(lock.path()).unwrap().contains("cafe"));
    }

    #[test]
    fn the_temp_name_is_unique_per_process_so_two_resh_instances_cannot_collide() {
        // Two processes writing the same port is impossible (the OS assigned
        // it), but two processes writing *different* ports into one directory
        // is the normal case, and a shared temp name would let one truncate
        // the other's in-flight write.
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert!(temp_name(5599).contains(&std::process::id().to_string()));
        assert_ne!(temp_name(5599), temp_name(5600));
        let _ = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
    }

    #[test]
    fn dropping_removes_our_lock_and_leaves_a_strangers_alone() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        // A real IntelliJ's registration, in the directory we share with it.
        let foreign = d.path().join("4711.lock");
        std::fs::write(&foreign, "{}").unwrap();
        {
            let _lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
            assert!(d.path().join("5599.lock").exists());
        }
        assert!(!d.path().join("5599.lock").exists(), "our own lock must go on drop");
        // Reverting `Drop` to a directory sweep passes the line above and
        // fails this one. That sweep is the defect this test exists for.
        assert!(foreign.exists(), "resh must never delete a lock file it did not write");
    }

    #[test]
    fn a_missing_directory_is_created_rather_than_failing_the_project_open() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let nested = d.path().join("claude/ide");
        let lock = write_in(&nested, 5599, "cafe", ws.path()).unwrap();
        assert!(lock.path().exists());
    }
}
