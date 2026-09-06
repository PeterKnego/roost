#![allow(dead_code)]
//! Shared helpers for the integration test binaries under `tests/`: fixtures,
//! HTTP/websocket connection helpers, and polling utilities. Every `tests/*.rs`
//! binary that needs one declares `mod common;` and pulls it in with
//! `use common::*;` — not every binary uses every helper, hence the blanket
//! `dead_code` allow above rather than scattering it per item.

use std::net::TcpListener;
use std::path::PathBuf;

/// One temp directory, shared by every test in this binary — now one binary
/// per file under `tests/`, so "this binary" means one of those files, not
/// all 71 tests at once; it is still one shared directory per process, just
/// no longer shared across every test in the whole suite — for any real
/// ide lock file opening a terminal writes (`ide::for_project` ->
/// `idelock::ide_dir()`). Set once and idempotently — see
/// `idelock::set_ide_dir_for_test`'s doc comment for why a directory shared
/// across tests, not one per test, is the right shape — so `cargo test`
/// never touches the real `~/.claude/ide` (Task 5 review, finding 2).
///
/// Same reasoning applies to `ide::start_in`'s port record
/// (`ideport::ports_dir()`): its own `cfg!(test)` fallback only covers this
/// crate's own unit tests, not this binary, which links `roost` as an
/// ordinary dependency with no `cfg(test)`. Measured directly: before this
/// call existed, running this suite once left `pingws.port` and
/// `sseportproj.port` behind in the developer's real
/// `~/.local/state/roost/ide/` — the same class of leak Task 5 found on the
/// lock-file side.
pub fn isolate_ide_dir_for_tests() {
    roost::idelock::isolate_ide_dir_for_test();
    let who = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let p = std::env::temp_dir().join(format!("roost-test-ideport-integration-{who}"));
    roost::ideport::set_ports_dir_for_test(p);
}

/// `serve`'s accept loop now re-reads `projects::roots()` on every connection
/// instead of using its `startup_roots` argument after boot (so a root added
/// from the front page is visible to the very next request, not just after a
/// restart) — see `lib.rs`. `ROOST_ROOTS` is the source that wins over the
/// config file (`projects::roots_from`), so setting it here, process-wide, is
/// what makes every connection *this* test makes see exactly the roots it
/// asked for rather than whatever the developer's real global config says.
/// Safe under this suite's mandatory `--test-threads=1`: test bodies run one
/// at a time, and each sets this before making its own requests, so an
/// earlier test's still-listening (but now idle) server thread never reads it
/// concurrently. An empty list clears the var instead of setting it to `""`,
/// because `roots_from` treats a set-but-empty value as unset and falls
/// through to the config file — setting it to empty would be indistinguishable
/// from that, not from "no roots".
pub fn start(roots: Vec<PathBuf>) -> u16 {
    isolate_ide_dir_for_tests();
    if roots.is_empty() {
        std::env::remove_var("ROOST_ROOTS");
    } else {
        std::env::set_var(
            "ROOST_ROOTS",
            roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":"),
        );
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || roost::serve(listener, roots));
    port
}

pub fn fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("proj")).unwrap();
    std::fs::write(d.path().join("proj/hello.md"), "# Hello\n").unwrap();
    std::fs::create_dir(d.path().join("proj/.roost")).unwrap();
    std::fs::write(d.path().join("proj/.roost/config.toml"), "theme = \"light\"\n").unwrap();
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

/// Like `fixture()`, but under a project name unique to the caller instead
/// of the shared "proj". `Hub` is a process-global registry keyed by project
/// name (see hub.rs) that outlives any single test's `TempDir`: once some
/// other test's "proj" hub exists, every later connection to "proj" reuses
/// that *same* Hub — including its `dir`, which points at a directory that
/// test's TempDir has since deleted. Any test whose server-side code touches
/// the filesystem (not just in-memory buffer state) needs its own project
/// name to avoid silently reading/writing through a stale path.
pub fn fixture_named(project: &str) -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join(project)).unwrap();
    std::fs::write(d.path().join(project).join("hello.md"), "# Hello\n").unwrap();
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

/// Two sibling projects under one root, for tests that must prove isolation
/// *between* projects. A test that only ever looks at one project cannot
/// catch `kill_project` (or anything else project-scoped) degenerating to
/// "affect everything" — it would still pass.
pub fn two_project_fixture(a: &str, b: &str) -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    for name in [a, b] {
        std::fs::create_dir(d.path().join(name)).unwrap();
        std::fs::write(d.path().join(name).join("hello.md"), "# Hello\n").unwrap();
    }
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

/// Existing tests call `ureq::get(...)` inline and only need the body; this
/// is for the cases that also need to assert on status and content-type.
pub fn get_full(port: u16, path: &str) -> (u16, String, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = ureq::get(&url).call();
    match resp {
        Ok(r) => {
            let status = r.status();
            let ctype = r.header("content-type").unwrap_or("").to_string();
            (status, ctype, r.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(code, r)) => {
            let ctype = r.header("content-type").unwrap_or("").to_string();
            (code, ctype, r.into_string().unwrap_or_default())
        }
        Err(e) => panic!("request failed: {e}"),
    }
}

/// Builds a `multipart/form-data` body by hand. Each part is a file part named
/// `file`, which is what the client sends.
pub fn multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----roosttestboundary";
    let mut body = Vec::new();
    for (name, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A raw socket rather than `ureq`: these tests must control the `Origin`
/// header exactly, *including omitting it*, which a client library will not let
/// you do reliably. Returns (status, whole response text).
pub fn post(port: u16, path: &str, origin: Option<&str>, ctype: &str, body: &[u8]) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: {ctype}\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(o) = origin {
        head.push_str(&format!("Origin: {o}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut resp = Vec::new();
    let _ = s.read_to_end(&mut resp);
    let text = String::from_utf8_lossy(&resp).to_string();
    let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, text)
}

/// `POST /paste/{project}/{session}` needs a session that is already in
/// `session::sessions()`, but connecting the terminal websocket only
/// *starts* `attach` — the WS handshake completes (and so the client's
/// `ws_connect` call returns) before the server has necessarily called it,
/// let alone had it finish spawning the PTY. That gap has always existed;
/// it only became wide enough to lose routinely once opening a terminal
/// started guaranteeing the project's ide listener exists first (real
/// I/O — a TCP bind, a token, a lock file — genuinely ahead of the spawn,
/// not merely slow test scheduling). Retrying is safe: a "no such session"
/// 404 is refused before any paste content is touched, so it leaves nothing
/// behind to double up on the next attempt. Poll rather than sleep-once,
/// per this file's own idiom elsewhere (see `any_process_holds`'s callers):
/// bounded, and exits the moment the condition holds.
pub fn post_when_session_ready(
    port: u16,
    path: &str,
    origin: Option<&str>,
    ctype: &str,
    body: &[u8],
) -> (u16, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (status, resp) = post(port, path, origin, ctype, body);
        let session_not_ready_yet = status == 404 && resp.contains("no such session");
        if !session_not_ready_yet || std::time::Instant::now() >= deadline {
            return (status, resp);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `ROOST_MAX_UPLOAD` is process-global, so any test that writes it serialises.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A project with a real nested subdirectory, for the multi-segment
/// workspace URL / directory-picker tests below.
pub fn nested_fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("karpie/sub")).unwrap();
    std::fs::write(d.path().join("karpie/sub/inner.rs"), "fn main() {}").unwrap();
    std::fs::write(d.path().join("karpie/top.txt"), "top").unwrap();
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

// ROOST_CMD is process-global; both ws tests set it, and if they ran in
// parallel one could overwrite the other's value mid-connect. Serialize them.
pub static WS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Connect with an explicit Origin. The server rejects handshakes without one
/// (spec §Security), so every legitimate ws client must supply it.
pub fn ws_connect(
    port: u16,
    origin: Option<&str>,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    use tungstenite::client::IntoClientRequest;
    // Reserved first, for the reason `ws_connect_term` gives: a bare connect
    // no longer creates a session. Unconditional, including for the tests that
    // expect a refusal — the Origin check runs long before `attach`, so those
    // still fail exactly where they are meant to.
    roost::session::reserve_if_absent("proj", "shell");
    let mut req = format!("ws://127.0.0.1:{port}/ws/proj/term/shell").into_client_request().unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    let (ws, _resp) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
    Ok(ws)
}

/// Waits for the server to genuinely close the socket, distinguishing that
/// from the client's own read timeout: a timeout means the server may be
/// hanging and must fail the test, not be mistaken for a close.
pub fn assert_ws_closes(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    context: &str,
) {
    for _ in 0..50 {
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) => return,
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                return;
            }
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return;
            }
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                panic!("{context}: timed out waiting for the socket to close");
            }
            Ok(_) => {}
            Err(e) => panic!("{context}: unexpected error while waiting for close: {e:?}"),
        }
    }
    panic!("{context}: socket did not close within the read budget");
}

/// Reads until a Ping arrives, or fails saying what came instead.
///
/// Deliberately specific: a socket that merely stays *open* proves nothing
/// here, because an idle socket stays open on its own. The whole point of the
/// ping is that bytes are periodically pushed at a peer that may no longer
/// exist, so only an actual Ping frame is evidence.
pub fn expect_ping(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    context: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut seen: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Ping(_)) => return,
            Ok(m) => seen.push(format!("{m:?}").chars().take(40).collect()),
            // The read timeout is a poll interval, not a failure: nothing has
            // arrived yet, and the ping is on a wall-clock schedule.
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
            Err(e) => panic!("{context}: socket died before any ping: {e:?} (saw {seen:?})"),
        }
    }
    panic!("{context}: no ping within the deadline; frames seen: {seen:?}");
}

/// Connect to a terminal socket the way a browser actually does: *after* an
/// intent reserved the name.
///
/// `session::attach` no longer creates a session on a bare connect — that was
/// what made every terminal websocket a session factory and let a client race
/// a close. Production always gets here via `NewTerminal`, which reserves; a
/// test that connects cold is exercising a path the server now refuses, so it
/// has to reserve too. Tests that mean to assert the *refusal* call
/// `ws_connect_path` directly, and that difference is now meaningful.
pub fn ws_connect_term(
    port: u16,
    path: &str,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    if let Some(rest) = path.strip_prefix("/ws/") {
        if let Some((project, name)) = rest.rsplit_once("/term/") {
            roost::session::reserve_if_absent(project, name);
        }
    }
    ws_connect_path(port, path)
}

pub fn ws_connect_path(
    port: u16,
    path: &str,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}{path}").into_client_request().unwrap();
    req.headers_mut().insert("origin", "http://127.0.0.1:8444".parse().unwrap());
    let (ws, _r) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
    Ok(ws)
}

/// Waits for a frame containing `needle`, bounded by wall time rather than by a
/// message count.
///
/// The read timeout set above is a *poll interval*, not a failure: `ws.read()`
/// returning `TimedOut`/`WouldBlock` only means nothing has arrived yet. The
/// previous version treated any `Err` as fatal (`Err(_) => break`) and panicked
/// immediately, so a single transient timeout ended the wait even when the event
/// was about to arrive. That made the filesystem-watch tests flaky on Linux
/// specifically — inotify plus the watcher's own debounce can put the broadcast
/// past the first poll, where macOS's FSEvents timing happened to land inside it
/// (observed at ~1 run in 6 on the deploy host, always starting with
/// `external_edit_updates_a_clean_buffer_live`).
///
/// A real socket error still fails, and now says so instead of being reported as
/// "never saw ..." — the two are different diagnoses and were indistinguishable
/// before. The bound still fails a genuinely absent event, just on a deadline.
pub fn read_until(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    needle: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                if t.contains(needle) {
                    return t.to_string();
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => panic!("read_until({needle:?}): socket error rather than a timeout: {e:?}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never saw {needle:?} within the deadline"
        );
    }
}

/// Pulls the `origin` field out of an `Event::State` frame's JSON: the
/// connection id of whichever client's action produced this snapshot. Used
/// to prove a mirrored event really came from the *other* browser, not from
/// the reading client's own initial snapshot.
pub fn extract_origin(json: &str) -> String {
    let key = r#""origin":""#;
    let start = json.find(key).expect("frame has no origin field") + key.len();
    let rest = &json[start..];
    let end = rest.find('"').expect("unterminated origin field");
    rest[..end].to_string()
}

/// Reads and discards every frame currently queued on `ws`, using a short
/// read timeout to detect "nothing left queued" rather than blocking on the
/// socket's normal multi-second one. Meant to be called right before
/// sending a fresh request, so nothing already sitting in the queue — a
/// stale broadcast, e.g. — can later be mistaken for that request's answer.
///
/// A read error here is only "nothing queued right now" when it's a genuine
/// timeout; anything else (the socket actually closing) must not be
/// swallowed as if it were an empty queue, or a stale frame received
/// earlier could keep standing in as authoritative right up until the real
/// fault would otherwise have surfaced. `assert_ws_closes` above draws the
/// same distinction for the same reason.
pub fn discard_pending(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) {
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
    }
    loop {
        match ws.read() {
            Ok(_) => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => break,
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) =>
            {
                break; // the expected exit: nothing left queued right now
            }
            Err(e) => panic!("discard_pending: unexpected error while sweeping the queue: {e:?}"),
        }
    }
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
}

/// Sends `RequestState` and returns the answer correlated to `own_id` — this
/// socket's own connection id, read from the `origin` field of its initial
/// snapshot (see `extract_origin`).
///
/// A plain "read the next `State` frame" is not sound here. Two distinct
/// races land extra `State` frames in this socket's queue that have nothing
/// to do with the request this call is about to send: (1) anything already
/// queued from before the send — e.g. a broadcast that arrived while this
/// call's caller was doing something else — and (2) term.rs's own
/// post-attach broadcast, which fires from the *connecting* thread only
/// after `session::attach` returns; since a client's `connect()` call
/// returns as soon as the handshake completes, well before that, a
/// workspace socket can end up subscribed — and this function's own
/// `RequestState` can get answered — before that broadcast lands, so it
/// arrives *after* as a genuine surprise. Broadcasts always carry an empty
/// `origin` (`h.snapshot_event(&String::new())` — see term.rs and
/// `do_close_project`), so filtering on `origin == own_id` rejects both
/// cases at once: only `RequestState`'s own handler stamps the requester's
/// id into the `State` it sends back (`self.snapshot_event(from)`), making
/// that the one frame guaranteed to answer *this* request.
///
/// This is not a hypothetical: an earlier version of this file used "read
/// the next matching frame" and it let the isolation test in
/// `close_project_ends_sessions_and_isolates_other_projects` pass while the
/// server was secretly killing the other project's session too — see that
/// test's mutation-testing note in the task report.
pub fn fresh_state(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    own_id: &str,
) -> String {
    discard_pending(ws);
    ws.send(tungstenite::Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                if t.contains(r#""t":"State""#) && extract_origin(&t) == own_id {
                    return t.to_string();
                }
                // Some other frame — a stray broadcast, or a State whose
                // origin isn't ours: not the answer to our request, keep
                // waiting for it rather than accepting this one.
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
            Err(e) => panic!("fresh_state: unexpected error waiting for our own response: {e:?}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no State frame correlated to {own_id:?} arrived within the deadline"
        );
    }
}

/// Polls `fresh_state` until the snapshot contains `needle`, or panics after
/// a deadline. Used both to wait for a session to go live and to wait for
/// `CloseProject`'s effect to be visible, so a test never depends on
/// guessing how many broadcasts to skip past.
pub fn wait_for_state_containing(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    own_id: &str,
    needle: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = fresh_state(ws, own_id);
        if state.contains(needle) {
            return state;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "state never contained {needle:?} within the deadline; last: {state}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Waits for `session` to appear in `live_sessions` specifically — not just
/// anywhere in the frame: a `State` with an open Terminal tab also carries
/// `"session":"shell"` in its pane/tab data even while `live_sessions` is
/// still empty, and a bare `contains("shell")` would match that instead.
pub fn wait_for_live_session(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    own_id: &str,
    session: &str,
) -> String {
    wait_for_state_containing(ws, own_id, &format!("\"live_sessions\":[\"{session}\"]"))
}

/// Minimal, test-only "is anything holding this path" check via `ps`.
/// Deliberately not roost's own internal machinery (private to its
/// `registry` module, and hardened for production against inputs this
/// test's own known, plain tempdir paths don't need) — just enough to prove
/// a real process is or isn't there.
pub fn any_process_holds(path: &std::path::Path) -> bool {
    let target = path.to_string_lossy();
    let out = std::process::Command::new("ps")
        .args(["-Ao", "args="])
        .output()
        .expect("ps must be runnable for this check to mean anything");
    // C1's exact shape, inside the test that guards C1: treating a failed
    // or empty `ps` as `false` ("nothing holds it") would let the central
    // `assert!(!any_process_holds(&sock))` pass vacuously on a broken `ps`,
    // proving nothing. Panic instead — a test that can't verify what it's
    // asserting must not report a pass.
    assert!(
        out.status.success() && !out.stdout.is_empty(),
        "ps failed or returned nothing; this test cannot trust its own assertions right now"
    );
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(target.as_ref()))
}

/// The ide listener is built on the connection thread, after the handshake has
/// already returned to the client, so every check of it has to be a bounded
/// poll rather than an immediate read.
pub fn wait_for_ide_port(project: &str) -> Option<u16> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(p) = roost::ide::port_for(project) {
            return Some(p);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn wait_for_path(p: &std::path::Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if p.exists() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Reads for `window`, returning the first frame containing `needle`, or
/// `None` if none arrived. The mirror image of `read_until`: that one proves
/// something happened, this one proves something did not. Shortens the
/// socket's read timeout so the loop actually polls within the window rather
/// than blocking past it on the 5s default `ws_connect_path` sets.
pub fn read_none(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    needle: &str,
    window: std::time::Duration,
) -> Option<String> {
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
    }
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) if t.contains(needle) => return Some(t.to_string()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => panic!("read_none({needle:?}): socket error rather than a timeout: {e:?}"),
        }
    }
    None
}

/// Connect to `/ws/_roots`, the way `ws_connect` connects to a terminal: a
/// fixed path, an optional `Origin`. A separate helper rather than a `path`
/// parameter on `ws_connect` because that one also reserves a terminal name
/// before connecting (see its doc comment) — a concern this socket has none
/// of.
pub fn ws_connect_roots(
    port: u16,
    origin: Option<&str>,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws/_roots").into_client_request().unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    let (ws, _resp) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
    Ok(ws)
}
