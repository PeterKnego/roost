//! The channel `roost worktree-hook` asks the server over.
//!
//! `roost claude-hook` never talks to the server — it writes to its tty and
//! to files. Approving a worktree needs a round trip (hook → hub → a browser
//! → back), so this is the one place a Claude-spawned process reaches roost.
//!
//! A Unix socket, not HTTP: CLAUDE.md fixes the HTTP state-change surface at
//! two Origin-checked POSTs, and an endpoint that creates directories and
//! moves terminals is exactly what a drive-by page would want. No browser can
//! open a Unix socket. The trust boundary is the same as `state/` itself:
//! file mode 0600 in a 0700 directory, and the peer's uid from `SO_PEERCRED`
//! must be ours.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct AskRequest {
    pub op: String,
    pub project: String,
    pub session: String,
    pub name: String,
    pub cwd: String,
    pub session_id: String,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum AskReply {
    Path { path: String },
    Refuse { refuse: String },
}

/// The socket's own directory, kept private so a stray socket file is never
/// dropped straight into `state/` where every other project's files live.
/// Not `pub(crate)`: tests outside this module have no business constructing
/// paths under it either.
pub fn socket_dir() -> PathBuf {
    crate::wsstate::state_dir().join("hook")
}

pub fn socket_path() -> PathBuf {
    socket_dir().join("hook.sock")
}

pub fn peer_allowed(peer_uid: u32, our_uid: u32) -> bool {
    peer_uid == our_uid
}

/// `SO_PEERCRED` is the thing a Unix socket has that a TCP one does not: the
/// kernel's own record of who is on the other end, unforgeable by whatever
/// that process writes. Reading it is the entire authentication story here —
/// no token, because the directory and file mode already keep everyone but
/// us from connecting at all, and this is the belt to that belt-and-braces.
fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` is zeroed and `len` is set to its exact size, so the
    // kernel writes at most that many bytes into a valid, owned buffer; the
    // return code is checked before any field of `cred` is read.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

fn reply(mut s: &UnixStream, r: &AskReply) {
    if let Ok(mut line) = serde_json::to_string(r) {
        line.push('\n');
        let _ = s.write_all(line.as_bytes());
    }
}

/// Reads exactly one line-delimited JSON request and writes exactly one
/// line-delimited JSON reply, then returns — one request per connection,
/// mirroring `roost worktree-hook`'s own one-shot lifecycle (Task 8). The
/// read is bounded by `MAX_REQUEST` so an oversized or never-terminated
/// line cannot buffer unbounded attacker-controlled bytes in this process;
/// the caller (`start()`) bounds how long the read may take at all.
pub fn serve_one(stream: UnixStream, ask: &dyn Fn(AskRequest) -> AskReply) {
    let mut line = String::new();
    let n = BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line).unwrap_or(0);
    if n as u64 >= MAX_REQUEST && !line.ends_with('\n') {
        return reply(&stream, &AskReply::Refuse { refuse: "request too large".into() });
    }
    if line.trim().is_empty() {
        // A silent peer (nothing written before the read timeout fires, or
        // the connection closed with nothing sent) is not garbage — it is
        // simply nothing, and deserves its own reason rather than whatever
        // message an empty string happens to produce from a JSON parser.
        return reply(&stream, &AskReply::Refuse { refuse: "no request".into() });
    }
    let req: AskRequest = match serde_json::from_str(line.trim_end()) {
        Ok(r) => r,
        Err(e) => {
            return reply(&stream, &AskReply::Refuse { refuse: format!("unreadable request: {e}") })
        }
    };
    if req.op != "worktree_create" {
        return reply(&stream, &AskReply::Refuse { refuse: format!("unknown op {:?}", req.op) });
    }
    reply(&stream, &ask(req));
}

/// `symlink_metadata`'s three-way split (CLAUDE.md: absence of evidence is
/// not evidence of absence) applied to the socket directory itself: `Err`
/// other than `NotFound` means "cannot tell", not "absent", and must refuse
/// rather than risk creating a second directory over something unreadable.
/// Checked every `start()`, not only at creation, because another process
/// (or a hand-edit) could have widened the mode since this roost last ran.
fn verify_socket_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot stat {}: {e}", dir.display()))?;
    // `symlink_metadata`, not `metadata`: a symlink here would mean this
    // directory's contents — and thus the socket's real location — are
    // decided by whatever that link points at, not by this 0700 check.
    if !md.is_dir() {
        return Err(format!("{} is not a real directory", dir.display()));
    }
    // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
    let me = unsafe { libc::getuid() };
    if md.uid() != me {
        return Err(format!("{} is not owned by us (owner uid {})", dir.display(), md.uid()));
    }
    let mode = md.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} has mode {mode:03o}, which grants group or other access; refusing rather than chmod-ing it",
            dir.display()
        ));
    }
    Ok(())
}

/// Binds the hook socket and spawns its accept thread. Never fatal to the
/// caller: a project with an unwritable state dir still runs, just without
/// `roost worktree-hook` able to reach it (Task 8 falls back).
pub fn start() -> Result<(), String> {
    let dir = socket_dir();
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No umask dance here (see CLAUDE.md / R19): umask is
            // process-global, so flipping it around this `create` would
            // hand every other thread's concurrent `mkdir` the same
            // tightened mask for the width of this call — including ones
            // that *want* their usual mode. `DirBuilderExt::mode` passes
            // 0o700 straight to `mkdir`, and a typical umask only clears
            // bits that 0o700 does not set in the first place, so this
            // reliably produces 0700 without touching process-global state.
            // `verify_socket_dir` below is the actual guarantee, not this.
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        Err(e) => return Err(format!("cannot stat {}: {e}", dir.display())),
        Ok(_) => {}
    }
    verify_socket_dir(&dir)?;

    let p = socket_path();
    match std::fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot stat {}: {e}", p.display())),
        Ok(_) => match UnixStream::connect(&p) {
            // Someone answers: another roost holds it. Never unlink that.
            Ok(_) => return Err(format!("{} is in use by another roost", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                // Nothing is listening — a previous roost's socket file,
                // left behind because SIGTERM unwinds nothing. Safe to
                // remove: a refused connection is positive evidence the
                // path is a dead end, not a guess from its mere existence.
                std::fs::remove_file(&p)
                    .map_err(|e| format!("cannot remove stale {}: {e}", p.display()))?;
            }
            Err(e) => return Err(format!("cannot tell whether {} is in use: {e}", p.display())),
        },
    }

    let l = UnixListener::bind(&p).map_err(|e| format!("bind {}: {e}", p.display()))?;
    use std::os::unix::fs::PermissionsExt;
    // Belt and braces after the directory's own 0700: a window exists
    // between `bind` (which creates the file at whatever the directory's
    // default would be) and this call, but that window is inside a 0700
    // directory nothing but us can even traverse into, so it is not the
    // same hole a world-readable directory would make it.
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {}: {e}", p.display()))?;

    // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
    let me = unsafe { libc::getuid() };
    std::thread::Builder::new()
        .name("hooksock".into())
        .spawn(move || {
            // `.flatten()` would also work, but the explicit `continue` on
            // `Err` makes it visible that one bad `accept` must not end the
            // loop — CLAUDE.md: no panic, and no silent exit either, may
            // leave this process's only hook listener dead.
            for s in l.incoming() {
                let Ok(s) = s else { continue };
                if !peer_uid(&s).is_some_and(|u| peer_allowed(u, me)) {
                    continue;
                }
                let _ = std::thread::Builder::new().name("hooksock-conn".into()).spawn(move || {
                    // Bounds only the request read, per the brief: the wait
                    // on `ask` (Task 9) is a separate, much longer budget.
                    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                    // A panic here must not take the process down: this
                    // thread is fed bytes from a Claude-spawned process,
                    // and CLAUDE.md requires no panic escape a socket
                    // thread regardless of how trusted that peer is.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        serve_one(s, &|req| crate::hub::ask_worktree_blocking(req));
                    }));
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn a_foreign_uid_is_refused() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
    }

    #[test]
    fn one_request_one_reply() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            serve_one(b, &|r| AskReply::Path { path: format!("/x/{}", r.name) })
        });
        let mut a2 = a.try_clone().unwrap();
        writeln!(
            a2,
            r#"{{"op":"worktree_create","project":"p","session":"term","name":"fox","cwd":"/p","session_id":"s"}}"#
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert_eq!(line.trim(), r#"{"path":"/x/fox"}"#);
    }

    // Revert-checked: removing the `MAX_REQUEST` bound in `serve_one` (so
    // the 70 KiB line reaches `serde_json::from_str` uncapped) does not
    // make this test fail to refuse — it still reaches the parser and
    // still refuses — but with a different reason:
    // `{"refuse":"unreadable request: expected value at line 1 column 1"}`
    // rather than "too large", so the `line.contains("too large")`
    // assertion fails. Restored from backup.
    #[test]
    fn an_oversized_request_is_refused_without_asking() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asked2 = asked.clone();
        let t = std::thread::spawn(move || {
            serve_one(b, &move |_| {
                asked2.store(true, std::sync::atomic::Ordering::SeqCst);
                AskReply::Path { path: "/x".into() }
            })
        });
        let mut a2 = a.try_clone().unwrap();
        let _ = a2.write_all(&vec![b'x'; 70 * 1024]);
        let _ = a2.write_all(b"\n");
        let mut line = String::new();
        let _ = BufReader::new(a).read_line(&mut line);
        t.join().unwrap();
        assert!(!asked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(line.contains("refuse") && line.contains("too large"), "{line}");
    }

    #[test]
    fn garbage_is_refused_with_a_reason() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_one(b, &|_| AskReply::Path { path: "/x".into() }));
        let mut a2 = a.try_clone().unwrap();
        writeln!(a2, "not json").unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert!(line.contains("refuse") && line.contains("request"), "{line}");
    }

    // The timeout is set by the caller (`start()`) on the accepted stream,
    // not inside `serve_one` — so this test sets it itself to mimic that,
    // then checks a silent peer does not wedge the connection thread.
    // Revert-checked: dropping the `b.set_read_timeout` line below makes
    // this test hang (observed: killed after 2+ minutes with no output,
    // where the timed version finishes in ~5s) because `read_line` blocks
    // forever on a peer that never writes and never closes.
    #[test]
    fn a_silent_peer_does_not_hang_the_connection_thread() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        b.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let start = std::time::Instant::now();
        let t = std::thread::spawn(move || serve_one(b, &|_| AskReply::Path { path: "/x".into() }));
        a.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let mut line = String::new();
        let _ = BufReader::new(a).read_line(&mut line);
        t.join().unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(8),
            "the read must time out well under 8s, took {:?}",
            start.elapsed()
        );
    }

    // R19: a pre-existing, wrongly-permissioned socket directory must refuse
    // to start rather than silently chmod or delete it, and the bad mode
    // must be named in the error so an operator can find it.
    //
    // Revert-checked: commenting out the `verify_socket_dir(&dir)?;` call in
    // `start()` makes this fail — `start()` returns `Ok` and binds inside
    // the 0o755 directory anyway, so `expect_err` panics with
    // "a group/other-readable directory must be refused: ()". Restored
    // from backup.
    #[test]
    fn a_wrongly_permissioned_dir_refuses_to_start_and_is_left_alone() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let dir = socket_dir();
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = start().expect_err("a group/other-readable directory must be refused");
        assert!(err.contains("mode"), "{err}");

        let mode = std::fs::symlink_metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "start() must never chmod a directory it did not create");

        std::env::remove_var("ROOST_STATE_DIR");
    }
}
