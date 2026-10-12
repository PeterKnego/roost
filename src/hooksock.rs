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
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_REQUEST: u64 = 64 * 1024;
/// Total budget for reading one request, from the first byte of the
/// connection to the last — not a per-`read()` timeout. R20b: a peer that
/// trickles one byte every few seconds would never trip a short per-read
/// timeout and could hold a connection (and a slot under `MAX_CONNECTIONS`)
/// open indefinitely.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// CLAUDE.md's `ide::MAX_PENDING` is per-project; this is process-wide,
/// because unlike a parked `openDiff` proposal this socket has no project
/// to scope by — Task 8's client has not even told us which project it
/// wants yet when it first connects. Bounds the number of connection
/// threads this socket can have alive at once, not just how big one of
/// them can get.
const MAX_CONNECTIONS: usize = 16;

/// How long the accept loop waits after a failed `accept` before the next.
/// See the `Err` arm in `serve_incoming`.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

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
/// mirroring `roost worktree-hook`'s own one-shot lifecycle (Task 8). Uses
/// the production `REQUEST_DEADLINE`; `serve_one_with_deadline` is the
/// same thing with the deadline as a parameter, so a test can drive it with
/// a short one instead of actually waiting ten seconds.
pub fn serve_one(stream: UnixStream, ask: &dyn Fn(AskRequest) -> AskReply) {
    serve_one_with_deadline(stream, ask, REQUEST_DEADLINE);
}

/// Reads byte-by-byte-ish (in small chunks) rather than handing the whole
/// read to one `read_line` call, because the deadline this exists to
/// enforce (R20b) is a *total* budget across however many reads a trickling
/// peer forces this into — a single `read_line` call only ever sees the one
/// timeout it was given at the start, so it cannot re-check a running total
/// partway through.
fn serve_one_with_deadline(stream: UnixStream, ask: &dyn Fn(AskRequest) -> AskReply, deadline: Duration) {
    let started = Instant::now();
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            return reply(&stream, &AskReply::Refuse { refuse: "request timed out".into() });
        }
        // Re-derived from the running total on every iteration, not a fixed
        // timeout set once: that is the one thing that makes this a total
        // deadline instead of a per-read one. Capped at 1s so a peer that
        // never sends anything at all is re-checked against `deadline`
        // promptly rather than only once, right at the end.
        let per_read = (deadline - elapsed).min(Duration::from_secs(1));
        let _ = (&stream).set_read_timeout(Some(per_read));
        match (&stream).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(pos) = chunk[..n].iter().position(|&c| c == b'\n') {
                    // R21a: the cap below only ran on a newline-free chunk,
                    // so a chunk that happened to *contain* the newline
                    // was accepted unconditionally, however large it made
                    // the total — up to a whole chunk (4096 bytes) over
                    // `MAX_REQUEST`. Checked here too, against the exact
                    // size this line would have *including* the newline,
                    // before ever accepting it.
                    if (buf.len() + pos + 1) as u64 > MAX_REQUEST {
                        return reply(
                            &stream,
                            &AskReply::Refuse { refuse: "request too large".into() },
                        );
                    }
                    buf.extend_from_slice(&chunk[..=pos]);
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() as u64 >= MAX_REQUEST {
                    return reply(&stream, &AskReply::Refuse { refuse: "request too large".into() });
                }
            }
            // A per-read timeout firing is not a failure, only a prompt to
            // re-check the total deadline above — `WouldBlock` is what a
            // nonblocking-style timeout reports on some platforms,
            // `TimedOut` on others; treat both the same way. R21c:
            // `Interrupted` (EINTR, a signal landing mid-syscall) is
            // likewise not a failure and must retry the same way — folding
            // it into the catch-all `Err(_) => break` below would read an
            // ordinary, transient signal as "the peer is gone".
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            // Any other read error (peer reset, etc.) is treated like EOF:
            // whatever is in `buf` so far (often nothing) is final.
            Err(_) => break,
        }
    }
    let line = String::from_utf8_lossy(&buf).into_owned();
    if line.trim().is_empty() {
        // A silent peer (nothing written before EOF or the deadline, or the
        // connection closed with nothing sent) is not garbage — it is
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
    // Finding 3: on a fresh install, nothing before this call has created
    // `state_dir()` itself — `wsstate::save` only creates it lazily, on a
    // project's first save, which may never have happened yet. Creating
    // roost's own state root is not destructive (unlike anything this
    // module does to `state/hook` itself), so no positive-evidence dance
    // is needed here the way there is below for the socket file.
    //
    // R21b: `recursive(true)` + `.mode(0o700)` rather than
    // `create_dir_all` followed by an unconditional `set_permissions` (the
    // shape `wsstate::save` uses) — `DirBuilder`'s recursive creation only
    // ever calls `mkdir` for a path segment that does not already exist,
    // so the mode is applied solely to whatever this call actually
    // creates. A directory that was already there — however it got its
    // mode, including one an operator deliberately widened — is never
    // touched, matching CLAUDE.md: only what this call created is ours to
    // set the mode on.
    use std::os::unix::fs::DirBuilderExt;
    let state = crate::wsstate::state_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&state)
        .map_err(|e| format!("create {}: {e}", state.display()))?;

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
            // (`DirBuilderExt` is already imported above, for the state
            // root's own `DirBuilder` call.)
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
        Ok(md) => {
            // Finding 1: on Linux, `connect()` to a non-socket returns
            // `ECONNREFUSED` — the identical errno a genuinely dead socket
            // gives. Without this check, any regular file an operator (or
            // a bad restore) left at `hook.sock` would read as "stale" and
            // get unlinked below, which is destruction from a guess, not
            // from evidence (CLAUDE.md). `is_socket` is checked *before*
            // ever calling `connect`, so a non-socket is refused outright
            // and `connect`'s `ConnectionRefused` is only ever reached for
            // something that was already confirmed to be a socket.
            use std::os::unix::fs::FileTypeExt;
            if !md.file_type().is_socket() {
                return Err(format!("{} exists and is not a socket", p.display()));
            }
            match UnixStream::connect(&p) {
                // Someone answers: another roost holds it. Never unlink that.
                Ok(_) => return Err(format!("{} is in use by another roost", p.display())),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    // Nothing is listening — a previous roost's socket
                    // file, left behind because SIGTERM unwinds nothing.
                    // Safe to remove now: this is a confirmed socket (the
                    // check above), and a refused connection to it is
                    // positive evidence it is a dead end, not a guess from
                    // its mere existence.
                    std::fs::remove_file(&p)
                        .map_err(|e| format!("cannot remove stale {}: {e}", p.display()))?;
                }
                Err(e) => return Err(format!("cannot tell whether {} is in use: {e}", p.display())),
            }
        }
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
        .spawn(move || serve_listener(l, MAX_CONNECTIONS, me, |req| crate::hub::ask_worktree_blocking(req)))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The accept loop's core, pulled out of `start()` so a test can drive it
/// against a small `cap` and a throwaway listener instead of the
/// production `MAX_CONNECTIONS` (16) and the real hook socket. `ask` is
/// taken by value and wrapped in an `Arc` internally (rather than taking
/// `&dyn Fn` the way `serve_one` does) so each connection thread can hold
/// its own clone of the `Arc` for as long as it runs, including past the
/// point where this function — and whatever borrowed it — might return.
fn serve_listener<F>(l: UnixListener, cap: usize, me: u32, ask: F)
where
    F: Fn(AskRequest) -> AskReply + Send + Sync + 'static,
{
    serve_incoming(l.incoming(), cap, me, ask)
}

/// [`serve_listener`] over any source of accepted connections, so a test
/// can hand it the accept *errors* a real listener only produces under
/// descriptor exhaustion.
fn serve_incoming<I, F>(incoming: I, cap: usize, me: u32, ask: F)
where
    I: Iterator<Item = std::io::Result<UnixStream>>,
    F: Fn(AskRequest) -> AskReply + Send + Sync + 'static,
{
    // A drop guard that frees one slot. Round 1 built this *inside* the
    // spawned closure's body (`let _guard = Guard(counter);` as the first
    // statement run once the thread actually started). That is wrong: if
    // `Builder::spawn` itself fails — thread exhaustion, exactly the
    // condition `cap` exists to defend against — the closure it was given
    // is simply dropped, unrun. None of its *statements* ever execute, so
    // no `Guard` value is ever constructed, so nothing decrements, and the
    // slot leaks forever. Sixteen such failures (one per `MAX_CONNECTIONS`
    // slot) would wedge this socket at permanent "busy".
    //
    // Constructing the guard here, on the accept thread, and only then
    // moving the already-built value into the closure's *captured
    // environment* fixes it: a closure that fails to spawn is dropped like
    // any other owned value going out of scope, which runs the destructors
    // of everything it captured — including this `Guard` — whether or not
    // the closure's body ever ran. This specific path (forcing
    // `Builder::spawn` to fail) has no portable way to trigger from a
    // test, so this is a structural fix verified by inspection; the test
    // below instead exercises the release mechanism end to end (a
    // normally-completed connection frees its slot for the next one).
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    let ask = Arc::new(ask);
    // R20a: bounds how many connection threads this socket can have alive
    // at once, process-wide (there is exactly one hook socket per roost).
    // Incremented and compared only here, on this single accept thread —
    // `l.incoming()` yields one connection at a time from one thread, so
    // there is no concurrent writer to race against the check below; the
    // only other mutator is each connection thread's own decrement on
    // exit, which only ever moves the count down.
    let in_flight = Arc::new(AtomicUsize::new(0));
    // `.flatten()` would also work, but the explicit `continue` on `Err`
    // makes it visible that one bad `accept` must not end the loop —
    // CLAUDE.md: no panic, and no silent exit either, may leave this
    // process's only hook listener dead.
    for s in incoming {
        let Ok(s) = s else {
            // Backed off, not retried at once (final review h). The errors
            // that persist — EMFILE/ENFILE with every descriptor in use —
            // fail every call immediately, so a bare `continue` spins this
            // thread at 100 % until a descriptor frees up, starving the very
            // connection threads that would free one. A tenth of a second
            // costs a hook nothing it would notice.
            std::thread::sleep(ACCEPT_BACKOFF);
            continue;
        };
        if !peer_uid(&s).is_some_and(|u| peer_allowed(u, me)) {
            continue;
        }
        if in_flight.load(Ordering::SeqCst) >= cap {
            // Refused on this thread, synchronously, rather than handed to
            // a connection thread that would just immediately hit the same
            // cap: busy means busy right now, not "queued behind a budget
            // that may not free up before this peer gives up anyway".
            reply(&s, &AskReply::Refuse { refuse: "busy".into() });
            continue; // `s` drops here, closing the connection.
        }
        in_flight.fetch_add(1, Ordering::SeqCst);
        // Built now, before `spawn` is even called — see the comment on
        // `Guard` above for why that ordering is the entire fix.
        let guard = Guard(in_flight.clone());
        let ask = ask.clone();
        let _ = std::thread::Builder::new().name("hooksock-conn".into()).spawn(move || {
            let _guard = guard;
            // A panic here must not take the process down: this thread is
            // fed bytes from a Claude-spawned process, and CLAUDE.md
            // requires no panic escape a socket thread regardless of how
            // trusted that peer is.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve_one_with_deadline(s, &*ask, REQUEST_DEADLINE);
            }));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    /// Final-review h (M7): an accept error backs off before the next
    /// accept. A persistent one — EMFILE, with every descriptor held —
    /// returns at once on every call, so a bare `continue` spins a core at
    /// 100 % for as long as the condition lasts, which is exactly when the
    /// process can least afford it.
    ///
    /// Revert-checked (final fix wave): with the `ACCEPT_BACKOFF` sleep
    /// removed from the `Err` arm, three errors are consumed in
    /// microseconds and this fails on "accept errors were retried without
    /// backing off".
    #[test]
    fn an_accept_error_backs_off_before_the_next_accept() {
        let errors = std::iter::repeat_with(|| Err(std::io::Error::from_raw_os_error(24))).take(3);
        let started = std::time::Instant::now();
        serve_incoming(errors, 1, 0, |_| AskReply::Refuse { refuse: "unused".into() });
        let took = started.elapsed();
        assert!(
            took >= ACCEPT_BACKOFF * 3,
            "accept errors were retried without backing off: 3 errors in {took:?}"
        );
    }

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

    // `serve_one` runs with the production 10s total deadline, so this test
    // drives the lower-level `serve_one_with_deadline` directly with a short
    // one instead — fix round 1 (R20b) moved from a caller-set per-read
    // timeout to a total-request deadline tracked inside `serve_one` itself,
    // so there is no longer an outside knob to turn down for a fast test.
    //
    // Revert-checked (fix round 1): replacing the `elapsed >= deadline`
    // early-return and the elapsed-aware `per_read` computation in
    // `serve_one_with_deadline` with a fixed 1s-per-read, no-total-check
    // version (see the next test's check too — both exercise the same
    // code) makes this test hang indefinitely: killed with
    // `timeout 25 cargo test --lib hooksock::tests::a_silent_peer -- --test-threads=1`
    // (exit code 124, no completed output), where the correct version
    // finishes in well under 2s. Restored from backup.
    #[test]
    fn a_silent_peer_does_not_hang_the_connection_thread() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let start = std::time::Instant::now();
        let t = std::thread::spawn(move || {
            serve_one_with_deadline(b, &|_| AskReply::Path { path: "/x".into() }, Duration::from_millis(500))
        });
        a.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        let _ = BufReader::new(&a).read_line(&mut line);
        t.join().unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "a silent peer must be refused at the deadline, took {:?}",
            start.elapsed()
        );
        assert!(line.contains("refuse"), "{line}");
    }

    // R20b: the deadline bounds the *whole* request, not each individual
    // read — a peer that keeps the connection alive by trickling a byte
    // every so often must still be cut off once the total time is up. A
    // naive "reset a fresh per-read timeout on every successful read" bug
    // would let this peer run forever one trickle at a time; this is the
    // test that distinguishes the two.
    //
    // `a` (not a clone) is kept open by the test itself until after the
    // server thread has already finished, deliberately outliving the
    // writer thread's own clone of it: a first draft of this test moved
    // `a` straight into the writer thread, so it closed as soon as the
    // trickle loop ended, handing the server a natural EOF shortly after
    // ~2.4s regardless of whether the deadline logic worked at all — the
    // test passed even against the broken code below, for the wrong
    // reason, because nothing actually depended on the deadline firing.
    //
    // Revert-checked (fix round 1): replacing the `elapsed >= deadline`
    // early-return and the elapsed-aware `per_read` computation in
    // `serve_one_with_deadline` with a fixed 1s-per-read, no-total-check
    // version makes this test hang indefinitely — killed with
    // `timeout 25 cargo test --lib hooksock::tests::a_trickling_peer -- --test-threads=1`
    // (exit code 124, no completed output), where the correct version
    // finishes in well under 1s. Restored from backup.
    //
    // R21d: tightened from `< 3s` to `< 1.8s` against the same 2.4s
    // trickle, and now asserts the reply text itself — a per-read-timeout
    // implementation (one that resets a fresh, full budget on every
    // successful read, the exact bug this test exists to catch) would
    // only ever time out somewhere past the 2.4s trickle finishes, not
    // within 1.8s, so the old `< 3s` bound was loose enough that a
    // sufficiently fast per-read implementation variant could in principle
    // have slipped under it.
    #[test]
    fn a_trickling_peer_is_refused_by_the_total_deadline_not_per_read() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let start = std::time::Instant::now();
        let t = std::thread::spawn(move || {
            serve_one_with_deadline(b, &|_| AskReply::Path { path: "/x".into() }, Duration::from_millis(900))
        });
        // One byte every 400ms: always well inside a single read's own
        // timeout, but six of them add up to 2.4s against a 900ms budget.
        // Writes from a *clone* — `a` itself stays open in this function
        // (see the comment above) so the connection has no other way to
        // end besides the deadline actually firing.
        let writer = a.try_clone().unwrap();
        let trickle = std::thread::spawn(move || {
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(400));
                let _ = (&writer).write_all(b"x");
            }
        });
        t.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_millis(1800),
            "the TOTAL deadline must win even though every individual read keeps succeeding, took {:?}",
            start.elapsed()
        );
        a.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut line = String::new();
        BufReader::new(&a).read_line(&mut line).unwrap();
        assert!(line.contains("timed out"), "{line}");
        // Cleanup, not part of the timed assertion above: lets the trickle
        // thread finish rather than leaking it, now that the server side
        // (and thus every write after this point) is already gone.
        drop(a);
        trickle.join().unwrap();
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

    // Finding 1: on Linux, `connect()` to a non-socket returns
    // ECONNREFUSED — the exact same errno a dead *socket* gives. Treating
    // that as "stale, safe to delete" was the bug: it let `start()` unlink
    // and overwrite any ordinary file an operator (or a bad backup/restore)
    // happened to leave at `hook.sock`. CLAUDE.md: destruction requires
    // positive evidence, and "connect refused" is not evidence the path is
    // a dead socket unless it was first confirmed to *be* a socket at all.
    //
    // Revert-checked (fix round 1): restoring the pre-fix stale-socket
    // branch (matching on `Ok(_)` instead of `Ok(md)` with an
    // `md.file_type().is_socket()` guard before `connect`) makes this test
    // fail: `start()` returns `Ok(())` instead of `Err`, because the
    // planted regular file is unlinked and a fresh socket is bound in its
    // place — `expect_err` panics with "a non-socket at hook.sock must be
    // refused: ()", and the planted content is gone. Restored from backup.
    #[test]
    fn a_regular_file_at_the_socket_path_is_refused_not_deleted() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(socket_dir()).unwrap();
        std::fs::write(socket_path(), b"not a socket, please leave me alone").unwrap();

        let err = start().expect_err("a non-socket at hook.sock must be refused");
        assert!(err.contains("not a socket"), "{err}");
        assert_eq!(
            std::fs::read(socket_path()).unwrap(),
            b"not a socket, please leave me alone",
            "the planted file must survive untouched"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // Finding 2: `peer_uid` is the entire authentication story on this
    // socket (see the module doc), and had no test exercising a real
    // kernel-reported credential at all — every other test calls
    // `peer_allowed` directly on hand-picked numbers.
    #[test]
    fn peer_uid_reads_our_uid_on_a_real_socket() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
        let me = unsafe { libc::getuid() };
        assert_eq!(peer_uid(&a), Some(me));
    }

    // Finding 2: the actual stale-socket-replacement branch, exercised
    // through `start()` end to end rather than only unit-tested via
    // `symlink_metadata`/`connect` in isolation.
    //
    // `start()`'s accept thread is left running, bound inside this test's
    // own tempdir; it blocks forever on `l.incoming()` once the test
    // function returns, which leaks the thread but is harmless — nothing
    // else references that tempdir, and the thread does nothing but wait.
    #[test]
    fn start_replaces_a_dead_socket() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(socket_dir()).unwrap();
        // Bind and drop: `UnixListener::drop` does not unlink its path, so
        // this leaves exactly what a previous roost's SIGTERM-killed
        // process would — a socket file with nothing listening on it.
        {
            let _dead = UnixListener::bind(socket_path()).unwrap();
        }

        let res = start();
        assert!(res.is_ok(), "{res:?}");
        let conn = UnixStream::connect(socket_path());
        assert!(conn.is_ok(), "the replacement socket must be live: {conn:?}");

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // Finding 2: the "someone is actually listening, never touch it"
    // branch, also exercised end to end.
    #[test]
    fn start_refuses_a_live_socket_and_leaves_it() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(socket_dir()).unwrap();
        let held = UnixListener::bind(socket_path()).unwrap();

        let err = start().expect_err("a live socket must never be replaced");
        assert!(err.contains("in use"), "{err}");

        // The held listener must still be exactly as it was: able to
        // accept a connection, because `start()` must not have touched it.
        let conn = UnixStream::connect(socket_path());
        assert!(conn.is_ok(), "the original listener must still be live: {conn:?}");
        drop(held);

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // Finding 3: nothing before `hooksock::start()` in a fresh install's
    // startup path creates `state_dir()` itself — `wsstate` only creates it
    // lazily, on the first save. `DirBuilder::create` is not recursive, so
    // `start()` must create its own way down to `state/hook` rather than
    // assume `state/` already exists.
    //
    // Revert-checked (fix round 1): removing the `create_dir_all(state_dir())`
    // call at the top of `start()` makes this fail —
    // `DirBuilder::new().mode(0o700).create(&dir)` returns `NotFound`
    // (its parent, `state_dir()`, does not exist), so `start()` returns
    // `Err("create .../state/hook: No such file or directory (os error 2)")`
    // instead of `Ok`, and `res.is_ok()` panics. Restored from backup.
    #[test]
    fn start_creates_the_state_dir_itself_on_a_fresh_install() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("not-yet-created").join("deeper");
        std::env::set_var("ROOST_STATE_DIR", &state);
        assert!(std::fs::symlink_metadata(&state).is_err(), "setup: must not exist yet");

        let res = start();
        assert!(res.is_ok(), "{res:?}");

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::symlink_metadata(socket_dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // R21b: the state root this call created (not only the `hook`
        // subdirectory under it) must come out 0700 too.
        let state_mode = std::fs::symlink_metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(state_mode, 0o700, "a freshly created state root must be 0700");

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // R21b: a pre-existing state root — one `start()` did not create —
    // must never have its mode changed, including down to something
    // tighter. `wsstate::save` chmods `state_dir()` unconditionally on
    // every save; copying that shape into `hooksock::start()` would mean
    // every roost restart silently tightens a directory an operator may
    // have deliberately widened (CLAUDE.md: only what this call created is
    // ours to touch).
    //
    // Revert-checked (fix round 2): replacing the `DirBuilder::new()
    // .recursive(true).mode(0o700).create(&state)` call with
    // `wsstate::save`'s shape (`create_dir_all` followed by an
    // unconditional `set_permissions(..., 0o700)`) makes this test fail —
    // the pre-existing 0o755 root is chmod'd down to 0o700, so
    // `assert_eq!(mode, 0o755, ...)` fails with `0o700 != 0o755`.
    // Restored from backup.
    #[test]
    fn start_never_chmods_a_pre_existing_state_root() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("already_here");
        std::fs::create_dir_all(&state).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("ROOST_STATE_DIR", &state);

        let res = start();
        assert!(res.is_ok(), "{res:?}");

        let mode = std::fs::symlink_metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "a pre-existing state root must never be chmod'd, even tighter");

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // R20a: a 17th (here, a 2nd past a cap of 1) connection must be refused
    // rather than queued or allowed to pile up, closing it instead of
    // leaving it to the deadline. Uses `serve_listener` directly with a
    // small injectable cap so this does not need to open sixteen real
    // sockets — but it exercises the real `AtomicUsize` counter and its
    // drop guard, not a mock: `held` occupies the one slot for real, by
    // actually being accepted and handed to a real connection thread that
    // is actually blocked on a real read.
    //
    // The accept thread leaks into the background once this test returns
    // (same as `start_replaces_a_dead_socket`'s note) — harmless, bound to
    // this test's own tempdir.
    //
    // Revert-checked (fix round 1): changing the cap check in
    // `serve_listener` from `in_flight.load(Ordering::SeqCst) >= cap` to
    // `false` (i.e. never refuse) makes this test fail: the second
    // connection is accepted and spawned instead of refused, so it blocks
    // on its own read up to `REQUEST_DEADLINE` (10s) with nothing written
    // back yet — the client's 5s read times out first, and
    // `read_line(...).unwrap()` panics with a `Kind(TimedOut)`/`WouldBlock`
    // I/O error rather than the `line.contains("busy")` assertion ever
    // running. Restored from backup.
    #[test]
    fn connections_over_the_cap_are_refused_busy_and_closed() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("cap.sock");
        let l = UnixListener::bind(&path).unwrap();
        // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
        let me = unsafe { libc::getuid() };
        std::thread::spawn(move || {
            serve_listener(l, 1, me, |r| AskReply::Path { path: format!("/x/{}", r.name) })
        });

        // Occupies the one slot: accepted, handed to a connection thread,
        // and never writes anything, so it stays "in flight" for the rest
        // of this test.
        let held = UnixStream::connect(&path).unwrap();

        let over = UnixStream::connect(&path).unwrap();
        over.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        BufReader::new(&over).read_line(&mut line).unwrap();
        assert!(line.contains("refuse") && line.contains("busy"), "{line}");

        drop(held);
    }

    // R20a: round 1's `Guard` was constructed *inside* the spawned
    // closure's body, so a thread that fails to spawn at all (thread
    // exhaustion — exactly the condition the cap exists to defend
    // against) drops the closure unrun, meaning the `Guard` statement
    // never executes and nothing ever decrements: sixteen such failures
    // would wedge this socket at permanent "busy". That specific path
    // (forcing `Builder::spawn` to fail) has no portable way to trigger
    // from a test, so this instead exercises the release mechanism itself
    // end to end: connection A completes normally and connection B, after
    // A is done, must not be refused.
    //
    // Revert-checked (fix round 2): making `Guard::drop` a no-op (deleting
    // its `fetch_sub` call) makes this test fail — B stays "busy" for the
    // full 2s poll budget below and the test panics with "the slot never
    // freed after A finished: still busy after 2s". Restored from backup.
    #[test]
    fn a_finished_connection_frees_its_slot() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("slot.sock");
        let l = UnixListener::bind(&path).unwrap();
        // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
        let me = unsafe { libc::getuid() };
        std::thread::spawn(move || {
            serve_listener(l, 1, me, |r| AskReply::Path { path: format!("/x/{}", r.name) })
        });

        let mut a = UnixStream::connect(&path).unwrap();
        a.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        writeln!(
            a,
            r#"{{"op":"worktree_create","project":"p","session":"term","name":"fox","cwd":"/p","session_id":"s"}}"#
        )
        .unwrap();
        let mut a_line = String::new();
        BufReader::new(&a).read_line(&mut a_line).unwrap();
        assert_eq!(a_line.trim(), r#"{"path":"/x/fox"}"#, "setup: A must actually finish");
        drop(a);

        // The reply above only proves A's write happened — its connection
        // thread still has to return (and its guard drop) afterward,
        // which is a separate, very short window. Polling, bounded to 2s
        // total, absorbs that ordinary scheduling gap without papering
        // over a real leak: a real leak would never clear within the
        // bound, so the loop still fails, just slightly later rather than
        // on the very first attempt.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut b = UnixStream::connect(&path).unwrap();
            b.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
            writeln!(
                b,
                r#"{{"op":"worktree_create","project":"p","session":"term","name":"bar","cwd":"/p","session_id":"s2"}}"#
            )
            .unwrap();
            let mut b_line = String::new();
            let _ = BufReader::new(&b).read_line(&mut b_line);
            if !b_line.contains("busy") {
                assert!(b_line.contains("bar"), "{b_line}");
                return;
            }
            if Instant::now() >= deadline {
                panic!("the slot never freed after A finished: still busy after 2s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // R21a: the size cap only ran on a newline-free chunk, so a chunk that
    // happened to *contain* the newline was accepted unconditionally,
    // however large it made the total line — up to a whole read-chunk
    // (4096 bytes) over `MAX_REQUEST`. This test controls the chunking
    // directly (two separate writes, with a pause between them) rather
    // than relying on how one big `write_all` happens to split across
    // reads, so it reproduces the defect regardless of the internal chunk
    // size: the first write fills `buf` to just under the cap with no
    // newline in it; the second, small write arrives as its own read and
    // contains the newline partway through — a case the old code accepted
    // outright and the fix must still catch.
    //
    // Revert-checked (fix round 2): removing the
    // `(buf.len() + pos + 1) as u64 > MAX_REQUEST` check (so only the
    // no-newline branch's cap applies) makes this test fail — the reply
    // is the normal, non-refusing one built from the request's `name`
    // field, not a "too large" refusal, so `line.contains("too large")`
    // is false. Restored from backup.
    #[test]
    fn a_newline_that_crosses_the_max_request_boundary_is_still_refused() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            serve_one(b, &|r| AskReply::Path { path: format!("/x/{}", r.name) })
        });
        let mut a2 = a.try_clone().unwrap();
        // Just under the cap, no newline: lands in `buf` via the
        // no-newline branch without ever tripping its own `>= MAX_REQUEST`
        // check.
        let filler = vec![b'x'; MAX_REQUEST as usize - 50];
        a2.write_all(&filler).unwrap();
        // Give the reader time to drain the filler as its own read(s)
        // before the second write arrives, so the newline below is
        // guaranteed to land in a *separate* chunk from the filler rather
        // than however the kernel happens to coalesce one big write.
        std::thread::sleep(Duration::from_millis(100));
        // 100 bytes + the newline: buf.len() (MAX_REQUEST - 50) + 100 is
        // comfortably over MAX_REQUEST, with the newline inside this one
        // chunk.
        a2.write_all(&vec![b'y'; 100]).unwrap();
        a2.write_all(b"\n").unwrap();
        let mut line = String::new();
        let _ = BufReader::new(a).read_line(&mut line);
        t.join().unwrap();
        assert!(line.contains("refuse") && line.contains("too large"), "{line}");
    }

    // R21a: the two tests the finding named directly — the exact boundary
    // in each direction. (These two, run alone, do not reliably reproduce
    // the specific defect above: `MAX_REQUEST` happens to be an exact
    // multiple of this module's 4096-byte read-chunk size, so a single
    // `write_all` of `MAX_REQUEST + 1` bytes gets caught by the
    // *no-newline* branch's own cap after exactly `MAX_REQUEST` bytes,
    // before the chunk containing the newline is ever read — the test
    // above is the one that isolates the actual crossing-chunk defect.
    // These are still worth having as plain boundary-correctness checks.)
    fn padded_request(total_len_with_newline: usize) -> String {
        let mut obj = serde_json::json!({
            "op": "worktree_create",
            "project": "p",
            "session": "term",
            "name": "fox",
            "cwd": "/p",
            "session_id": "s",
            "pad": ""
        });
        let base_len = serde_json::to_string(&obj).unwrap().len() + 1; // + '\n'
        assert!(total_len_with_newline >= base_len, "helper: total_len too small to pad down to");
        obj["pad"] = serde_json::Value::String("x".repeat(total_len_with_newline - base_len));
        let line = serde_json::to_string(&obj).unwrap();
        assert_eq!(line.len() + 1, total_len_with_newline, "helper built the wrong length");
        line
    }

    #[test]
    fn a_request_of_exactly_max_request_bytes_is_accepted() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            serve_one(b, &|r| AskReply::Path { path: format!("/x/{}", r.name) })
        });
        let mut a2 = a.try_clone().unwrap();
        writeln!(a2, "{}", padded_request(MAX_REQUEST as usize)).unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert!(line.contains("/x/fox"), "{line}");
    }

    #[test]
    fn a_request_one_byte_over_max_request_is_refused_too_large() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            serve_one(b, &|r| AskReply::Path { path: format!("/x/{}", r.name) })
        });
        let mut a2 = a.try_clone().unwrap();
        writeln!(a2, "{}", padded_request(MAX_REQUEST as usize + 1)).unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert!(line.contains("refuse") && line.contains("too large"), "{line}");
    }
}
