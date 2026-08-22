use std::net::TcpListener;
use std::path::PathBuf;

/// One temp directory, shared by every test in this binary, for any real
/// ide lock file opening a terminal writes (`ide::for_project` ->
/// `idelock::ide_dir()`). Set once and idempotently — see
/// `idelock::set_ide_dir_for_test`'s doc comment for why a directory shared
/// across tests, not one per test, is the right shape — so `cargo test`
/// never touches the real `~/.claude/ide` (Task 5 review, finding 2).
fn isolate_ide_dir_for_tests() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    resh::idelock::set_ide_dir_for_test(d.path().to_path_buf());
}

fn start(roots: Vec<PathBuf>) -> u16 {
    isolate_ide_dir_for_tests();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || resh::serve(listener, roots));
    port
}

fn fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("proj")).unwrap();
    std::fs::write(d.path().join("proj/hello.md"), "# Hello\n").unwrap();
    std::fs::create_dir(d.path().join("proj/.resh")).unwrap();
    std::fs::write(d.path().join("proj/.resh/config.toml"), "theme = \"light\"\n").unwrap();
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
fn fixture_named(project: &str) -> (tempfile::TempDir, u16) {
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
fn two_project_fixture(a: &str, b: &str) -> (tempfile::TempDir, u16) {
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
fn get_full(port: u16, path: &str) -> (u16, String, String) {
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
fn multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----reshtestboundary";
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
fn post(port: u16, path: &str, origin: Option<&str>, ctype: &str, body: &[u8]) -> (u16, String) {
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
fn post_when_session_ready(
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

/// The check the whole GET-only amendment is traded against, so it gets the
/// treatment `ws_rejects_foreign_and_missing_origin` already gives the socket.
/// It asserts the *file was not written*, not merely the status: a 403 returned
/// after the write would still be a drive-by write.
#[test]
fn upload_refuses_a_foreign_or_absent_origin_without_writing() {
    let (d, port) = fixture_named("up_origin");
    let (ct, body) = multipart(&[("evil.txt", b"x")]);

    let (s1, _) = post(port, "/upload/up_origin", Some("https://evil.example.com"), &ct, &body);
    assert_eq!(s1, 403, "a foreign origin must not reach the upload endpoint");

    let (s2, _) = post(port, "/upload/up_origin", None, &ct, &body);
    assert_eq!(s2, 403, "a request with no Origin must be refused");

    assert!(
        !d.path().join("up_origin/evil.txt").exists(),
        "a refused upload must not have written the file"
    );
}

#[test]
fn upload_writes_every_part_and_reports_per_file() {
    let (d, port) = fixture_named("up_multi");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::write(d.path().join("up_multi/taken.txt"), b"original").unwrap();
    let (ct, body) = multipart(&[("a.txt", b"AAA"), ("taken.txt", b"BBB"), ("c.txt", b"CCC")]);

    let (status, resp) = post(port, "/upload/up_multi", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "a partial failure is still a well-formed request");

    assert_eq!(std::fs::read(d.path().join("up_multi/a.txt")).unwrap(), b"AAA");
    assert_eq!(std::fs::read(d.path().join("up_multi/c.txt")).unwrap(), b"CCC");
    assert_eq!(
        std::fs::read(d.path().join("up_multi/taken.txt")).unwrap(),
        b"original",
        "the colliding part must not have overwritten anything"
    );
    assert!(resp.contains("taken.txt") && resp.contains("already exists"), "response: {resp}");
    // The neighbours must be reported as successes, or a caller cannot tell
    // which of the three failed — and this is what pins that a rejected part is
    // still drained, since c.txt comes after the failure.
    assert!(resp.contains(r#"{"name":"a.txt","ok":true}"#), "response: {resp}");
    assert!(resp.contains(r#"{"name":"c.txt","ok":true}"#), "response: {resp}");
}

#[test]
fn upload_refuses_more_parts_than_the_limit() {
    let (d, port) = fixture_named("up_parts");
    let origin = format!("http://127.0.0.1:{port}");
    let names: Vec<String> = (0..20).map(|i| format!("f{i}.txt")).collect();
    let parts: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
    let (ct, body) = multipart(&parts);

    let (status, resp) = post(port, "/upload/up_parts", Some(&origin), &ct, &body);
    assert_eq!(status, 413);
    assert!(resp.contains("too many files"), "the parts cap must name itself: {resp}");
    assert!(!d.path().join("up_parts/f19.txt").exists());
}

/// A different cap with a different message. Two tests that both passed because
/// the same limit fired would say nothing about the other.
#[test]
fn upload_refuses_a_body_past_the_aggregate_limit() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_MAX_UPLOAD", "4096");
    let (d, port) = fixture_named("up_bytes");
    let origin = format!("http://127.0.0.1:{port}");
    let big = vec![b'x'; 8192];
    let (ct, body) = multipart(&[("big.bin", &big)]);

    let (status, resp) = post(port, "/upload/up_bytes", Some(&origin), &ct, &body);
    std::env::remove_var("RESH_MAX_UPLOAD");

    assert_eq!(status, 413);
    assert!(resp.contains("too large"), "the size cap must name itself: {resp}");
    assert!(!d.path().join("up_bytes/big.bin").exists());
    let leftovers: Vec<String> = std::fs::read_dir(d.path().join("up_bytes"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("resh.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "a cap breach left a partial file: {leftovers:?}");
}

#[test]
fn upload_refuses_a_hidden_destination() {
    let (d, port) = fixture_named("up_hidden");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::create_dir_all(d.path().join("up_hidden/.git")).unwrap();
    let (ct, body) = multipart(&[("config", b"[core]")]);
    let (status, resp) = post(port, "/upload/up_hidden?dir=.git", Some(&origin), &ct, &body);
    assert_eq!(status, 200);
    assert!(resp.contains("not visible in the tree"), "response: {resp}");
    assert!(!d.path().join("up_hidden/.git/config").exists());
}

#[test]
fn upload_lands_in_the_named_subdirectory() {
    let (d, port) = fixture_named("up_sub");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::create_dir_all(d.path().join("up_sub/src")).unwrap();
    let (ct, body) = multipart(&[("logo.png", b"PNG")]);
    let (status, resp) = post(port, "/upload/up_sub?dir=src", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "response: {resp}");
    assert_eq!(std::fs::read(d.path().join("up_sub/src/logo.png")).unwrap(), b"PNG");
}

/// `RESH_MAX_UPLOAD` is process-global, so any test that writes it serialises.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// With `RESH_CMD=cat` the PTY echoes what is written to it, so the terminal
/// socket is a direct view of the injected bytes. Asserting the markers — not
/// merely that the session survived — is the point: CLAUDE.md records a test
/// whose subject was a call it never actually verified.
#[test]
fn a_pasted_image_injects_a_bracketed_path_into_the_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", state.path());
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");

    // Attaching creates the session; the paste needs a live one.
    let mut term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();

    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, resp) = post_when_session_ready(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "response: {resp}");

    let mut seen = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !seen.contains("\u{1b}[201~") {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            // ws_connect sets a 5s read timeout, so an idle gap surfaces as a
            // would-block rather than a death. Retrying until the deadline is
            // the difference between this test waiting and this test failing
            // for a reason that has nothing to do with pasting.
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("terminal socket died waiting for the paste: {e}"),
        }
    }
    // The PTY echoes with ECHOCTL, which renders the ESC byte as the two
    // printable characters `^[` — so the raw \x1b never appears here, and
    // asserting on it would fail against a perfectly correct injection. What
    // does survive is the rest of each marker, which nothing else would produce.
    assert!(seen.contains("[200~"), "missing the opening marker: {seen:?}");
    assert!(seen.contains("[201~"), "missing the closing marker: {seen:?}");
    assert!(seen.contains(".png"), "the injected path must carry an image extension: {seen:?}");
    assert!(
        seen.contains(&state.path().join("pasted").to_string_lossy().to_string()),
        "the path must be absolute and under the state dir, not in the project: {seen:?}"
    );
    std::env::remove_var("RESH_CMD");
}

/// Differs from an accepted paste only in its *content* — same filename, same
/// live session — which is what makes it a control on the sniffing rather than
/// on the plumbing. It needs a live session because liveness is checked first,
/// deliberately: a paste is refused before its bytes are accepted.
#[test]
fn a_paste_of_a_non_image_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", state.path());
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let _term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();

    // A BMP: a real image the *clipboard* route would take, refused here
    // because the receiver cannot read `.bmp` from a path.
    let (ct, body) = multipart(&[("clip.png", b"BM\0\0\0\0\0\0\0\0\0\0")]);
    let (status, resp) = post_when_session_ready(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 400, "response: {resp}");
    assert!(resp.contains("PNG"), "the error must name what is accepted: {resp}");
    assert!(
        std::fs::read_dir(state.path().join("pasted")).map(|d| d.count()).unwrap_or(0) <= 1,
        "a refused paste must not have left an image behind"
    );
    std::env::remove_var("RESH_CMD");
}

#[test]
fn a_paste_onto_a_dead_session_is_an_error_not_a_silent_success() {
    let (_d, port) = fixture_named("paste_dead");
    let origin = format!("http://127.0.0.1:{port}");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, resp) = post(port, "/paste/paste_dead/nosuch", Some(&origin), &ct, &body);
    assert_eq!(status, 404);
    assert!(resp.contains("no such session"), "unexpected error: {resp}");
}

#[test]
fn a_paste_refuses_a_foreign_origin() {
    let (_d, port) = fixture_named("paste_origin");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, _) =
        post(port, "/paste/paste_origin/shell", Some("https://evil.example.com"), &ct, &body);
    assert_eq!(status, 403, "the paste endpoint needs the same gate as the upload one");
}

/// The property the old `http::tests::rejects_non_get` used to guarantee at the
/// parser: a request carrying a body must not reach the fragment routes. POST is
/// now parsed, so this is what stands in its place — and it asserts on the
/// *fragment content* rather than the status, because a route that ran and then
/// returned an error status would still have run.
#[test]
fn post_to_an_ordinary_path_does_not_reach_the_router() {
    let (_d, port) = fixture_named("post_router");
    let origin = format!("http://127.0.0.1:{port}");
    let (ct, body) = multipart(&[("x.txt", b"x")]);
    // A *valid* Origin, so this tests routing rather than tripping the origin
    // gate first — otherwise it would pass for a reason unrelated to its name.
    let (status, text) = post(port, "/frag/post_router/tree", Some(&origin), &ct, &body);
    assert_eq!(status, 404, "an ordinary path must not answer a POST");
    assert!(
        !text.contains("<ul class=\"tree\""),
        "the tree fragment was rendered for a POST: {text}"
    );
}

#[test]
fn the_service_worker_is_served_from_the_root_scope() {
    let (_d, port) = fixture();
    let (status, ctype, body) = get_full(port, "/sw.js");
    assert_eq!(status, 200, "sw.js must be at the root, or its scope cannot cover /{{project}}");
    assert!(ctype.contains("javascript"), "wrong content-type: {ctype}");
    assert!(body.contains("notificationclick"), "not the service worker: {body:.120}");
}

#[test]
fn index_lists_projects() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("proj"));
}

#[test]
fn workspace_page_applies_project_settings() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/proj"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("/static/themes/light.css")); // .resh config read per request
    assert!(body.contains("data-project=\"proj\""));
}

#[test]
fn fragments_render_and_errors_become_hints() {
    let (_d, port) = fixture();
    let base = format!("http://127.0.0.1:{port}");
    let tree = ureq::get(&format!("{base}/frag/proj/tree")).call().unwrap().into_string().unwrap();
    assert!(tree.contains("hello.md"));
    let file = ureq::get(&format!("{base}/frag/proj/file?path=hello.md"))
        .call().unwrap().into_string().unwrap();
    assert!(file.contains("<h1>Hello</h1>"));
    // escape attempt: 200 + hint, and definitely no file content
    let esc = ureq::get(&format!("{base}/frag/proj/file?path=../../../etc/passwd"))
        .call().unwrap().into_string().unwrap();
    assert!(esc.contains("class=\"hint\""));
    assert!(!esc.contains("root:"));
}

/// A project with a real nested subdirectory, for the multi-segment
/// workspace URL / directory-picker tests below.
fn nested_fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("karpie/sub")).unwrap();
    std::fs::write(d.path().join("karpie/sub/inner.rs"), "fn main() {}").unwrap();
    std::fs::write(d.path().join("karpie/top.txt"), "top").unwrap();
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

#[test]
fn multi_segment_workspace_url_resolves_the_nested_directory() {
    let (_d, port) = nested_fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/karpie/sub"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("data-project=\"karpie/sub\""));
}

#[test]
fn frag_route_resolves_a_nested_projects_fragment_kind() {
    let (_d, port) = nested_fixture();
    let tree = ureq::get(&format!("http://127.0.0.1:{port}/frag/karpie/sub/tree"))
        .call().unwrap().into_string().unwrap();
    assert!(tree.contains("inner.rs"));
}

// Guards the route-ordering fix in routes.rs: `_projects` has too few
// segments to match the general `["frag", rest @ ..] if rest.len() >= 2`
// arm, so without its own arm sitting ahead of the catch-all, this request
// falls through to `serve_workspace` and tries to open a project literally
// named "frag/_projects" instead of serving the cross-project fragment.
// Deleting the `["frag", "_projects"]` arm, or moving it after the
// catch-all, is exactly what this test would catch.
#[test]
fn frag_projects_route_serves_the_cross_project_strip() {
    let (_d, port) = fixture();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/frag/_projects?current=x"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_string().unwrap();
    assert!(body.contains("class=\"projstrip\""));
}

#[test]
fn picker_at_shows_a_directorys_children_marked_distinctly() {
    let (_d, port) = nested_fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/?at=karpie"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("class=\"dir\" data-rel=\"karpie/sub\""));
    assert!(body.contains("class=\"file\""));
    assert!(body.contains("top.txt"));
    assert!(body.contains("crumb-current\">karpie"));
}

#[test]
fn picker_at_outside_the_roots_falls_back_to_the_top_level_not_a_leak() {
    let (_d, port) = nested_fixture();
    // `at` is fully attacker-controlled query text; a traversal attempt or
    // a bogus rel must never surface foreign directory content — it must
    // fall back to the same safe top level opening the page fresh would show.
    for at in ["../../etc", "nonexistent", "/etc"] {
        let body = ureq::get(&format!("http://127.0.0.1:{port}/?at={at}"))
            .call().unwrap().into_string().unwrap();
        assert!(!body.contains("passwd"), "leaked for at={at}: {body}");
        assert!(body.contains("crumb-current\">resh"), "did not fall back for at={at}");
        assert!(body.contains("data-rel=\"karpie\""), "top level missing for at={at}");
        // The fallback is silent to the URL (still `at=""`) but must not be
        // silent to the reader — a notice explains why they landed at the
        // top level instead of where `?at=` pointed.
        assert!(body.contains("showing the top level"), "no notice for at={at}");
    }
}

#[test]
fn nested_project_websockets_connect() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    std::env::set_var("RESH_CMD", "cat");
    let (_d, port) = nested_fixture();

    // routes::route's `[project, rest @ ..]` change is only half the fix —
    // lib.rs's route_ws and term.rs's handle_ws each had their own
    // single-segment-project assumption to update, or a nested workspace
    // page would render while its sockets silently failed to connect.
    let mut ws = ws_connect_path(port, "/ws/karpie/sub/_workspace").unwrap();
    let state = read_until(&mut ws, r#""t":"State""#);
    assert!(state.contains(r#""t":"State""#));
    let _ = ws.close(None);

    let mut term = ws_connect_path(port, "/ws/karpie/sub/term/shell").unwrap();
    term.send(tungstenite::Message::Binary(b"hi\r".to_vec())).unwrap();
    let mut seen = String::new();
    for _ in 0..60 {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("hi") {
            break;
        }
    }
    assert!(seen.contains("hi"), "nested project's terminal must echo through the PTY");
    let _ = term.close(None);

    std::env::remove_var("RESH_STATE_DIR");
    std::env::remove_var("RESH_CMD");
}

#[test]
fn unknown_pages_are_404() {
    let (_d, port) = fixture();
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/no-such-project")).call().is_err());
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/frag/proj/nope")).call().is_err());
}

#[test]
fn static_assets_served_with_type() {
    let (_d, port) = fixture();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/static/vendor/highlight.min.js"))
        .call().unwrap();
    assert!(resp.content_type().starts_with("text/javascript"));
}

#[cfg(unix)]
#[test]
fn theme_css_symlink_escaping_the_project_is_refused() {
    // A cloned repo controls .resh/theme.css. If the fragment handler
    // did a bare fs::read of that path, a symlink planted there pointing at
    // e.g. ~/.ssh/id_rsa would be served straight to the browser as
    // text/css. serve_frag must resolve it through safe_resolve like every
    // other file read, so the escape is refused the same way path
    // traversal already is.
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("themeleak")).unwrap();
    std::fs::create_dir(d.path().join("themeleak/.resh")).unwrap();
    let secret = d.path().join("secret.txt");
    std::fs::write(&secret, "top secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, d.path().join("themeleak/.resh/theme.css")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    match ureq::get(&format!("http://127.0.0.1:{port}/frag/themeleak/theme.css")).call() {
        Err(ureq::Error::Status(code, r)) => {
            assert_eq!(code, 404);
            assert!(!r.into_string().unwrap().contains("top secret"));
        }
        Ok(r) => panic!("symlink escape must not be served; got {:?}", r.into_string()),
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}

/// The `serve_theme` unit-test helper in routes.rs calls `serve_project_theme`
/// directly, which never proves the URL actually reaches it — the fragment
/// router splits on the *last* path segment for every other fragment kind,
/// and a first cut of this route's dispatch arm required two-or-more
/// segments after "theme" and so could never match, 404ing every request as
/// "no such project" while every direct-call unit test stayed green. This
/// test goes over real HTTP through the router, the only way to catch that.
#[test]
fn frag_theme_directory_serves_presentation_and_refuses_code_over_http() {
    let (d, port) = fixture_named("themedir");
    let t = d.path().join("themedir/.resh/theme");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("style.css"), "body{color:red}").unwrap();
    std::fs::write(t.join("app.js"), "alert('pwned')").unwrap();

    let css = ureq::get(&format!("http://127.0.0.1:{port}/frag/themedir/theme/style.css"))
        .call()
        .unwrap();
    assert_eq!(css.status(), 200);
    assert_eq!(css.header("Content-Security-Policy"), Some("sandbox"));
    let body = css.into_string().unwrap();
    assert!(body.contains("body{color:red}"));

    match ureq::get(&format!("http://127.0.0.1:{port}/frag/themedir/theme/app.js")).call() {
        Err(ureq::Error::Status(code, r)) => {
            assert_eq!(code, 404);
            assert!(!r.into_string().unwrap().contains("pwned"));
        }
        Ok(r) => panic!("a project may never serve code; got {:?}", r.into_string()),
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}

/// A first version of the theme-directory router dispatch keyed off "does
/// any path segment say theme" rather than "is the last segment a real
/// fragment kind". A project is legitimately multi-segment
/// (`resolve_project` accepts nested rels, see
/// `multi_segment_workspace_url_resolves_the_nested_directory` /
/// `frag_route_resolves_a_nested_projects_fragment_kind`), so a project
/// literally named ".../theme" made that version hijack every one of its
/// ordinary fragments into a theme-asset lookup under its *parent*
/// project instead — the workspace page would render, and every pane
/// would 404. This has to go over real HTTP: a test that reaches
/// `serve_frag`/`serve_project_theme` directly, bypassing `route()`'s
/// dispatch, is exactly what let the dead-route bug and this one both
/// ship green.
#[test]
fn nested_project_named_theme_still_serves_ordinary_fragments_over_http() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("a/theme")).unwrap();
    std::fs::write(d.path().join("a/theme/inner.rs"), "fn main() {}").unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let body = ureq::get(&format!("http://127.0.0.1:{port}/frag/a/theme/tree"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(body.contains("inner.rs"), "project a/theme's own tree, not a 404: {body}");
}

#[test]
fn diff_traversal_path_is_rejected_with_hint() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/frag/proj/diff?path=../../../etc/passwd"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("class=\"hint\""));
    assert!(!body.contains("root:"));
}

#[test]
fn tree_dir_traversal_is_rejected_with_hint_and_leaks_no_listing() {
    // `dir` is network-supplied and must be confined through
    // `safe_resolve` the same way `file`'s `path` is (see routes.rs) —
    // a `dir` that escapes the project must never render a listing.
    let (d, port) = fixture();
    std::fs::write(d.path().join("secret.txt"), "top secret\n").unwrap();
    let base = format!("http://127.0.0.1:{port}");
    // ".." from the project dir resolves to the tempdir root, which holds
    // `secret.txt` alongside `proj` — a real, canonicalizable escape.
    let body = ureq::get(&format!("{base}/frag/proj/tree?dir=.."))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("class=\"hint\""));
    assert!(!body.contains("secret.txt"));
    assert!(!body.contains("<li"));
}

#[test]
fn tree_dir_lazily_returns_a_subdirectorys_children() {
    let (d, port) = fixture();
    std::fs::create_dir(d.path().join("proj/sub")).unwrap();
    std::fs::write(d.path().join("proj/sub/inner.txt"), "").unwrap();
    let base = format!("http://127.0.0.1:{port}");
    // the root render shows `sub` closed, without its child inlined
    let root = ureq::get(&format!("{base}/frag/proj/tree")).call().unwrap().into_string().unwrap();
    assert!(root.contains("data-rel=\"sub\""));
    assert!(!root.contains("inner.txt"));
    // the lazy fetch for that same directory returns exactly its children
    let sub = ureq::get(&format!("{base}/frag/proj/tree?dir=sub")).call().unwrap().into_string().unwrap();
    assert!(sub.contains("inner.txt"));
}

// The header toggle has to beat the config file in *both* directions, or the
// control is one-way: a project with `show_hidden = true` in its config must
// still be able to turn dot entries off from the UI. Driven end to end — the
// intent over the websocket, the listing over HTTP — because the two reach the
// filter by different routes (hub state vs. registry peek) and a wiring that
// only worked one way would still pass a unit test of either half.
//
// Own project name: this test writes into the project directory and mutates
// hub state (see `fixture_named`).
#[test]
fn the_header_toggle_overrides_the_config_file_in_both_directions() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (d, port) = fixture_named("toggleproj");
    let proj = d.path().join("toggleproj");
    std::fs::write(proj.join(".gitignore"), "target\n").unwrap();
    let url = format!("http://127.0.0.1:{port}/frag/toggleproj/tree");
    let tree = || ureq::get(&url).call().unwrap().into_string().unwrap();

    // Config says hide (it says nothing, which is the same thing).
    assert!(!tree().contains(".gitignore"), "hidden by default");

    let mut c = ws_connect_path(port, "/ws/toggleproj/_workspace").unwrap();
    read_until(&mut c, r#""t":"State""#);
    c.send(tungstenite::Message::Text(r#"{"t":"SetShowHidden","on":true}"#.into())).unwrap();
    read_until(&mut c, r#""show_hidden":true"#);
    let shown = tree();
    assert!(shown.contains(r#"data-rel=".gitignore""#), "the toggle must beat the config file");
    assert!(shown.contains(r#"data-rel="hello.md""#), "ordinary rows are unaffected");

    // Now the other direction: config on, toggle off.
    std::fs::create_dir(proj.join(".resh")).unwrap();
    std::fs::write(proj.join(".resh/config.toml"), "show_hidden = true").unwrap();
    c.send(tungstenite::Message::Text(r#"{"t":"SetShowHidden","on":false}"#.into())).unwrap();
    read_until(&mut c, r#""show_hidden":false"#);
    let hidden = tree();
    assert!(!hidden.contains(".gitignore"), "an explicit off must beat show_hidden = true");
    assert!(hidden.contains(r#"data-rel="hello.md""#), "and must not empty the tree instead");

    let _ = c.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

// The setting has to survive the whole request path — config cascade, route,
// renderer — and it is per project, so the test asserts the same server serves
// the hidden row only once that project's `.resh/config.toml` asks for it. A
// filter wired to a constant would pass one half and fail the other.
// Own project name: this test writes into the project directory (see
// `fixture_named`).
#[test]
fn show_hidden_is_read_per_project_on_every_tree_request() {
    let (d, port) = fixture_named("hiddenproj");
    let proj = d.path().join("hiddenproj");
    std::fs::write(proj.join(".gitignore"), "target\n").unwrap();
    let url = format!("http://127.0.0.1:{port}/frag/hiddenproj/tree");
    let before = ureq::get(&url).call().unwrap().into_string().unwrap();
    assert!(before.contains("data-rel=\"hello.md\""), "ordinary rows render");
    assert!(!before.contains(".gitignore"), "hidden by default");

    // Settings are re-read per request, so no restart between these two.
    std::fs::create_dir(proj.join(".resh")).unwrap();
    std::fs::write(proj.join(".resh/config.toml"), "show_hidden = true").unwrap();
    let after = ureq::get(&url).call().unwrap().into_string().unwrap();
    assert!(after.contains("data-rel=\".gitignore\""), "the setting took effect");
    assert!(after.contains("data-rel=\".resh\""), "including the config dir itself");
    // The lazy-expand endpoint reads the same setting, not a cached one.
    let lazy = ureq::get(&format!("{url}?dir=")).call().unwrap().into_string().unwrap();
    assert!(lazy.contains("data-rel=\".gitignore\""));
}

#[test]
fn tree_dir_with_empty_rel_returns_the_root_listing() {
    // app.js reconciles the root level of the tree in place on every
    // TreeChanged (see refreshTree/reconcileList) instead of re-fetching
    // the whole `tree_fragment`, so a brand-new root-level file doesn't
    // wait for a reload while an existing open subdirectory doesn't
    // collapse. It does that by hitting `dir=` (empty rel) — the same
    // lazy-fetch endpoint a subdirectory expansion uses — which must
    // resolve to the project root itself, not 404 or error.
    let (d, port) = fixture();
    std::fs::create_dir(d.path().join("proj/sub")).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let root_dir = ureq::get(&format!("{base}/frag/proj/tree?dir=")).call().unwrap().into_string().unwrap();
    assert!(!root_dir.contains("class=\"hint\""));
    assert!(root_dir.contains("data-rel=\"hello.md\""));
    assert!(root_dir.contains("data-rel=\"sub\""));
}

// RESH_CMD is process-global; both ws tests set it, and if they ran in
// parallel one could overwrite the other's value mid-connect. Serialize them.
static WS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Connect with an explicit Origin. The server rejects handshakes without one
/// (spec §Security), so every legitimate ws client must supply it.
fn ws_connect(
    port: u16,
    origin: Option<&str>,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    use tungstenite::client::IntoClientRequest;
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

#[test]
fn ws_rejects_foreign_and_missing_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let (_d, port) = fixture();
    // The drive-by attack: a page the user visits opens this socket for a shell.
    assert!(
        ws_connect(port, Some("https://evil.example.com")).is_err(),
        "foreign origin must not reach the shell"
    );
    // Non-browser clients send no Origin at all.
    assert!(ws_connect(port, None).is_err(), "missing origin must be rejected");
    // Loopback still works without any configuration.
    assert!(ws_connect(port, Some("http://127.0.0.1:8444")).is_ok());
}

#[test]
fn terminal_ws_echoes_through_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    ws.send(tungstenite::Message::Text("resize:100x30".into())).unwrap();
    ws.send(tungstenite::Message::Binary(b"hello\r".to_vec())).unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match ws.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("hello") {
            break;
        }
    }
    assert!(seen.contains("hello"), "PTY echo not received; got: {seen:?}");
    let _ = ws.close(None);
}

/// Waits for the server to genuinely close the socket, distinguishing that
/// from the client's own read timeout: a timeout means the server may be
/// hanging and must fail the test, not be mistaken for a close.
fn assert_ws_closes(
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

#[test]
fn ws_closes_when_child_exits_first() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "true"); // exits immediately
    let (_d, port) = fixture();
    // Own session name: the process-global registry may already hold a live
    // "proj/shell" session from another test in this binary (e.g.
    // terminal_ws_echoes_through_pty's `cat`), in which case RESH_CMD
    // would never be consulted for a fresh spawn and this test would prove
    // nothing about a child exiting first.
    let mut ws = ws_connect_path(port, "/ws/proj/term/exiter").unwrap();
    // child exited at spawn; the server must close/shutdown the socket rather than hang
    assert_ws_closes(&mut ws, "ws_closes_when_child_exits_first");
}

#[test]
fn child_exit_delivers_a_close_frame_not_a_bare_eof() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    std::env::set_var("RESH_CMD", "true"); // exits immediately
    let (_d, port) = fixture_named("closeproj");
    let mut ws = ws_connect_path(port, "/ws/closeproj/term/exiter").unwrap();

    // Deliberately stricter than assert_ws_closes, which also accepts a bare
    // EOF. The browser turns that distinction into `wasClean`, and app.js's
    // connectTerm reconnects on an unclean close *only* — because a terminal
    // socket that dies with the laptop must heal itself, while one the server
    // closed on purpose must not, since session::attach creates the session
    // when it is absent. If this close frame were ever lost, every `exit`
    // would look like a network drop and silently fork a fresh shell.
    let mut saw = Vec::new();
    for _ in 0..50 {
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) => {
                std::env::remove_var("RESH_STATE_DIR");
                return;
            }
            Ok(m) => saw.push(format!("{m:?}")),
            Err(e) => {
                std::env::remove_var("RESH_STATE_DIR");
                panic!(
                    "child exit must close the socket with a Close frame, not {e:?}; \
                     frames seen first: {saw:?}"
                );
            }
        }
    }
    std::env::remove_var("RESH_STATE_DIR");
    panic!("no Close frame within the read budget; frames seen: {saw:?}");
}

/// Reads until a Ping arrives, or fails saying what came instead.
///
/// Deliberately specific: a socket that merely stays *open* proves nothing
/// here, because an idle socket stays open on its own. The whole point of the
/// ping is that bytes are periodically pushed at a peer that may no longer
/// exist, so only an actual Ping frame is evidence.
fn expect_ping(
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

#[test]
fn an_idle_terminal_socket_is_pinged() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    std::env::set_var("RESH_CMD", "cat"); // reads stdin, writes nothing unprompted
    std::env::set_var("RESH_PING_SECS", "1");
    let (_d, port) = fixture_named("pingterm");
    let mut ws = ws_connect_path(port, "/ws/pingterm/term/idle").unwrap();
    // Nothing is sent from either side after the handshake. Without the
    // ping this socket would sit silent forever, which is exactly how a
    // dead peer's attachment goes on holding a `sizes` entry — and the PTY
    // takes the *minimum* geometry across attachments, so a stale one
    // clamps the terminal for every live client.
    expect_ping(&mut ws, "idle terminal socket");
    std::env::remove_var("RESH_PING_SECS");
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn an_idle_workspace_socket_is_pinged() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_PING_SECS", "1");
    let (_d, port) = fixture_named("pingws");
    let mut ws = ws_connect_path(port, "/ws/pingws/_workspace").unwrap();
    // This socket matters more than the terminal one: hub::subscribe hands
    // out an *unbounded* channel, so a subscriber nobody drains accumulates
    // every broadcast in memory for as long as the process lives.
    expect_ping(&mut ws, "idle workspace socket");
    std::env::remove_var("RESH_PING_SECS");
}

#[test]
fn two_terminal_clients_mirror_one_session() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/term/shell").unwrap();
    let mut b = ws_connect_path(port, "/ws/proj/term/shell").unwrap();
    a.send(tungstenite::Message::Binary(b"mirrored\r".to_vec().into())).unwrap();

    for ws in [&mut a, &mut b] {
        let mut seen = String::new();
        for _ in 0..60 {
            match ws.read() {
                Ok(tungstenite::Message::Binary(x)) => seen.push_str(&String::from_utf8_lossy(&x)),
                Ok(_) => {}
                Err(_) => break,
            }
            if seen.contains("mirrored") {
                break;
            }
        }
        assert!(seen.contains("mirrored"), "both attachments must see the output");
    }
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

/// A browser that connects *after* a proposal opened must be shown what it is
/// being asked to approve. `Event::Proposal` goes out once, live; `State`
/// names the tab but carries neither side of the diff. Without the connect-time
/// replay, the second browser draws an empty proposal tab it can still click
/// Accept on — agreeing to a change nobody showed it.
///
/// The assertion is on the *text*, not on the tab: a test that only checked
/// for `"k":"Proposal"` in the snapshot cannot tell a rendered proposal from
/// a blank one, which is the whole defect.
///
/// Revert-checked twice. Removing the `proposal_replay` loop from `wsconn`'s
/// connect path failed this test — `never saw "\"t\":\"Proposal\"" within
/// the deadline`, after genuinely waiting out `read_until`'s full 15s rather
/// than passing vacuously. Separately, keeping the loop but making
/// `open_proposal_tab` store nothing failed the same way. The hub-level unit
/// test for `proposal_replay` kept passing through both, which is exactly why
/// this one exists. Then restored.
#[test]
fn a_browser_that_connects_after_a_proposal_is_shown_both_sides_of_it() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    // The first connection is what builds the hub this proposal is opened on.
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    resh::hub::open_proposal("proj", "late-1", "hello.md", "what is there", "what claude wants");

    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let seen = read_until(&mut b, r#""t":"Proposal""#);
    assert!(
        seen.contains("what is there") && seen.contains("what claude wants"),
        "the late browser was shown a proposal tab with no content: {seen}"
    );
    assert!(seen.contains(r#""rel":"hello.md""#), "and it must name the file: {seen}");

    // The hub registry is process-global and keyed by project name, so this
    // proposal would otherwise sit in "proj"'s layout for every later test in
    // this binary.
    resh::hub::close_proposal("proj", "late-1");
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

/// The connect path must emit content before the tab that renders it, the
/// same order the live path already used (`Hub::open_proposal_tab` broadcasts
/// `Event::Proposal` before the `State` that follows it). Before this task,
/// `wsconn::handle` sent the connect-time snapshot (`State`, which names
/// every open `Tab::Proposal`) *before* replaying `proposal_replay`'s
/// `Event::Proposal`s — so a client had to handle both orders, and a
/// straightforward implementation of the client's "keyed by id" map (see
/// static/app.js's `proposals`/`tabKey`) would draw an accept-able blank tab
/// for exactly one frame.
///
/// This only checks the very first frame after connecting — not merely that
/// both eventually arrive (`a_browser_that_connects_after_a_proposal_is_
/// shown_both_sides_of_it`, above, already covers that) — because ordering
/// is exactly what a "did both arrive" assertion cannot see.
///
/// Revert-checked: swapping wsconn's two `send_to` calls back (`State` before
/// the `proposal_replay` loop) fails this test — `the first frame after
/// connecting must be the proposal's content, not "State": {"t":"State",...`
/// — while `a_browser_that_connects_after_a_proposal_is_shown_both_sides_of_it`
/// keeps passing, which is exactly why this test exists alongside it.
/// Restored.
#[test]
fn a_late_browsers_first_frame_is_the_proposals_content_not_its_tab() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("proposal-order");
    let mut a = ws_connect_path(port, "/ws/proposal-order/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    resh::hub::open_proposal(
        "proposal-order",
        "order-1",
        "hello.md",
        "what is there",
        "what claude wants",
    );

    let mut b = ws_connect_path(port, "/ws/proposal-order/_workspace").unwrap();
    let first = loop {
        match b.read().unwrap() {
            tungstenite::Message::Text(t) => break t.to_string(),
            _ => continue,
        }
    };
    assert!(
        first.contains(r#""t":"Proposal""#),
        "the first frame after connecting must be the proposal's content, not \"State\": {first}"
    );

    resh::hub::close_proposal("proposal-order", "order-1");
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn invalid_session_name_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let (_d, port) = fixture();
    // "bad%20name" is rejected because '%' is itself outside valid_name's
    // charset — req.uri().path() is never percent-decoded, so the server
    // never even sees a space there. "bad.name" exercises a character that
    // survives undecoded, proving the rejection isn't an artifact of that.
    for path in ["/ws/proj/term/bad%20name", "/ws/proj/term/bad.name"] {
        let mut ws = ws_connect_path(port, path).unwrap();
        // the server closes immediately rather than spawning anything
        assert_ws_closes(&mut ws, path);
    }
}

fn ws_connect_path(
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
fn read_until(
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
fn extract_origin(json: &str) -> String {
    let key = r#""origin":""#;
    let start = json.find(key).expect("frame has no origin field") + key.len();
    let rest = &json[start..];
    let end = rest.find('"').expect("unterminated origin field");
    rest[..end].to_string()
}

/// The + button sends no name at all now, so allocation is entirely the
/// server's. Driven over the real socket because the client half — dropping
/// the `prompt()` — is not reachable from a unit test.
#[test]
fn new_terminal_names_itself_and_ending_one_clears_only_its_own_tab() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    // Its own project, not the shared "proj": Hub is a process-global registry
    // keyed by project name, so a sibling test's tabs would otherwise show up
    // here and the seeded-`term` baseline below would be reading their state.
    let (_d, port) = fixture_named("newterm");
    let mut a = ws_connect_path(port, "/ws/newterm/_workspace").unwrap();
    // default_layout already seeds a `term` tab, so the first click must skip
    // to `term1` — the case that proves names on tabs are treated as taken
    // even before any PTY exists for them.
    let init = read_until(&mut a, r#""t":"State""#);
    assert!(init.contains(r#""session":"term""#), "the seeded terminal is the baseline");

    a.send(tungstenite::Message::Text(r#"{"t":"NewTerminal","pane":3}"#.into())).unwrap();
    let seen = read_until(&mut a, r#""session":"term1""#);
    assert!(seen.contains(r#""session":"term1""#));

    a.send(tungstenite::Message::Text(r#"{"t":"NewTerminal","pane":3}"#.into())).unwrap();
    let seen = read_until(&mut a, r#""session":"term2""#);
    assert!(
        seen.contains(r#""session":"term2""#),
        "a second click must not hand out a name it already gave away"
    );

    a.send(tungstenite::Message::Text(r#"{"t":"EndSession","session":"term1"}"#.into())).unwrap();
    // Read until a snapshot that no longer mentions term1; the ending itself
    // happens on a background thread, so more than one State can arrive.
    let mut cleared = String::new();
    for _ in 0..20 {
        let msg = read_until(&mut a, r#""t":"State""#);
        if !msg.contains(r#""session":"term1""#) {
            cleared = msg;
            break;
        }
    }
    assert!(!cleared.is_empty(), "a snapshot without the ended session must arrive");
    assert!(cleared.contains(r#""session":"term2""#), "siblings must survive");
    assert!(cleared.contains(r#""session":"term""#), "siblings must survive");

    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn workspace_state_mirrors_between_two_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    // a's own initial snapshot carries a's connection id in `origin`; capture
    // it now so we can later prove the *mirrored* event names this same id,
    // rather than just asserting on ordering-dependent text like "hello.md".
    let a_init = read_until(&mut a, r#""t":"State""#);
    let a_id = extract_origin(&a_init);

    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    // Hub is a process-global registry keyed by project name (see hub.rs), so
    // "proj" can carry state left behind by another test in this binary.
    // Prove b's own snapshot starts clean, or a stale "hello.md" from a
    // previous test could make the assertion below pass for the wrong
    // reason — off b's own state, without exercising mirroring at all.
    let b_init = read_until(&mut b, r#""t":"State""#);
    assert!(!b_init.contains("hello.md"), "b's own snapshot must not already contain hello.md");

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();

    // the *other* browser must learn about it without asking
    let seen = read_until(&mut b, "hello.md");
    assert!(seen.contains(r#""t":"State""#));
    // ...and it must be attributed to *a*, the client that actually acted —
    // not something b could have produced from its own state.
    assert!(
        seen.contains(&format!(r#""origin":"{a_id}""#)),
        "mirrored event must carry the originating client's id ({a_id}), got: {seen}"
    );
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn workspace_socket_rejects_foreign_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_d, port) = fixture();
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws/proj/_workspace").into_client_request().unwrap();
    req.headers_mut().insert("origin", "https://evil.example.com".parse().unwrap());
    assert!(tungstenite::connect(req).is_err(), "the write socket must not be cross-origin");
}

#[test]
fn workspace_socket_rejects_missing_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_d, port) = fixture();
    use tungstenite::client::IntoClientRequest;
    // No Origin header at all: term.rs's socket already rejects this
    // (ws_rejects_foreign_and_missing_origin); the socket that can write
    // files needs the identical guarantee.
    let req = format!("ws://127.0.0.1:{port}/ws/proj/_workspace").into_client_request().unwrap();
    assert!(tungstenite::connect(req).is_err(), "missing origin must be rejected");
}

#[test]
fn workspace_socket_malformed_json_is_reported_not_fatal() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut ws = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let _ = read_until(&mut ws, r#""t":"State""#); // the initial snapshot

    ws.send(tungstenite::Message::Text("not json".into())).unwrap();
    let err = read_until(&mut ws, r#""t":"Error""#);
    assert!(err.contains(r#""t":"Error""#));

    // the socket must still be alive: a well-formed intent afterward still works
    ws.send(tungstenite::Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    let state = read_until(&mut ws, r#""t":"State""#);
    assert!(state.contains(r#""t":"State""#));

    let _ = ws.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn external_edit_updates_a_clean_buffer_live() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    std::env::set_var("RESH_DEBOUNCE_MS", "10");
    // Its OWN project name, not the shared "proj". `Hub` is a process-global
    // registry keyed by project name, so a "proj" hub created by any earlier
    // test outlives that test's TempDir — and this test would then bind to it,
    // leaving the watcher registered on a deleted directory while the writer
    // thread below rewrites a file in *this* test's fresh one. No event ever
    // arrives and the wait times out. That is exactly what `fixture_named`'s
    // doc comment warns about for "any test whose server-side code touches the
    // filesystem", and it made this the first casualty of a ~1-in-6 whole-suite
    // flake on Linux while passing 20/20 in isolation, where no other test is
    // there to create the shared hub first.
    let (d, port) = fixture_named("extedit");
    let mut a = ws_connect_path(port, "/ws/extedit/_workspace").unwrap();
    a.send(tungstenite::Message::Text(
        "{\"t\":\"EditBuffer\",\"rel\":\"hello.md\",\"text\":\"# Hello\\n\"}".into(),
    ))
    .unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"SaveBuffer","rel":"hello.md","force":true}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "SaveOk"); // buffer is now clean

    // Claude, in the next pane, rewrites the file. The watcher now spins up
    // on a background thread (the large-project fix: `for_project` must
    // return promptly regardless of tree size, so it can no longer walk and
    // register OS watches inline before answering this connection) — so
    // there's a short, expected window right after connecting where the
    // watcher isn't live yet. Keep rewriting the file in the background
    // instead of writing once, so the test doesn't depend on winning that
    // race on the first try.
    let hello_path = d.path().join("extedit/hello.md");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let writer = std::thread::spawn(move || {
        let mut n = 0u32;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            std::fs::write(&hello_path, format!("# Rewritten by Claude {n}\n")).unwrap();
            n += 1;
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let seen = read_until(&mut a, "Rewritten by Claude");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = writer.join();
    assert!(seen.contains(r#""t":"BufferText""#), "a clean buffer must follow the file");

    // The hub for "extedit" still outlives this test (Hub is process-global),
    // but the name is now unique to this test, so nothing else can inherit it.
    // Closing the buffer anyway keeps the hub's state tidy for a rerun within
    // the same binary.
    a.send(tungstenite::Message::Text(r#"{"t":"CloseBuffer","rel":"hello.md"}"#.into())).unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    let _ = a.close(None);
    std::env::remove_var("RESH_STATE_DIR");
    std::env::remove_var("RESH_DEBOUNCE_MS");
}

#[test]
fn set_mode_edit_then_save_writes_the_file() {
    // End-to-end regression for the live-verified bug: SetMode{Edit} must
    // make the server read the file (setting a real base_hash) before the
    // client ever calls SaveBuffer, or every first save reports a conflict
    // and the file on disk never changes.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (d, port) = fixture_named("editproj1");
    let mut a = ws_connect_path(port, "/ws/editproj1/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#); // a's own initial snapshot

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "hello.md");

    a.send(tungstenite::Message::Text(
        r#"{"t":"SetMode","rel":"hello.md","mode":"Edit"}"#.into(),
    ))
    .unwrap();
    // The server must push the disk content with an empty origin — a
    // non-empty origin equal to a's own id would be dropped client-side by
    // the echo rule and the editor would open blank.
    let text_ev = read_until(&mut a, r#""t":"BufferText""#);
    assert!(text_ev.contains("# Hello"), "got: {text_ev}");
    assert!(text_ev.contains(r#""origin":"""#), "must be authorless; got: {text_ev}");

    a.send(tungstenite::Message::Text(
        "{\"t\":\"EditBuffer\",\"rel\":\"hello.md\",\"text\":\"# Hello, edited\\n\"}".into(),
    ))
    .unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"SaveBuffer","rel":"hello.md","force":false}"#.into(),
    ))
    .unwrap();
    // force:false is the whole point of this test: only a correct base_hash
    // (set by SetMode's disk read) lets an *unforced* save through.
    let saved = read_until(&mut a, r#""t":"SaveOk""#);
    assert!(saved.contains(r#""t":"SaveOk""#));
    assert_eq!(
        std::fs::read_to_string(d.path().join("editproj1/hello.md")).unwrap(),
        "# Hello, edited\n",
        "the file on disk must actually change"
    );

    let _ = a.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn reconnect_replays_buffer_text_for_open_edit_buffers() {
    // A client that (re)connects onto a layout with an already-open Edit
    // buffer gets metadata-only State — never text — so without a replay,
    // that editor renders permanently blank until someone happens to edit
    // the same file again.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("editproj2");
    let mut a = ws_connect_path(port, "/ws/editproj2/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);
    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "hello.md");
    a.send(tungstenite::Message::Text(
        r#"{"t":"SetMode","rel":"hello.md","mode":"Edit"}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, r#""t":"BufferText""#);

    // A second connection joins after the buffer already exists — this is
    // the reconnect-onto-existing-state path, not the original open.
    let mut b = ws_connect_path(port, "/ws/editproj2/_workspace").unwrap();
    let b_text = read_until(&mut b, r#""t":"BufferText""#);
    assert!(b_text.contains("hello.md") && b_text.contains("# Hello"), "got: {b_text}");

    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
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
fn discard_pending(
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
fn fresh_state(
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
fn wait_for_state_containing(
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
fn wait_for_live_session(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    own_id: &str,
    session: &str,
) -> String {
    wait_for_state_containing(ws, own_id, &format!("\"live_sessions\":[\"{session}\"]"))
}

// This is the single behavioral promise of the whole projects feature:
// opening a project (fetching its page, opening its workspace socket —
// everything a browser does on arrival) must not itself start a shell.
// Before Tasks 3-4, the default layout shipped a Terminal tab, mounting it
// connected a socket, and connecting spawned a shell, so merely *looking*
// at a project forked a bash nobody used — the mechanism behind nine
// orphaned shells for deleted directories in production.
#[test]
fn opening_a_project_spawns_no_terminal_session() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    // A name unique to this test: the session registry (session.rs's
    // `SESSIONS`) is a process-global map keyed by project name that
    // outlives any one test's TempDir, and several other tests in this
    // binary attach a real "proj/shell" session and only ever detach it
    // (never kill it — see ws_closes_when_child_exits_first's comment).
    // Reusing "proj" here would let a leftover session from a test that
    // happened to run first make this assertion pass for the wrong reason.
    let (_d, port) = fixture_named("spawncheck");

    // Fetch the workspace page and open a workspace socket — everything a
    // browser does on arrival except starting a terminal.
    let body = ureq::get(&format!("http://127.0.0.1:{port}/spawncheck"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(body.contains("data-project"));
    let mut ws = ws_connect_path(port, "/ws/spawncheck/_workspace").unwrap();
    // This socket's own connection id, from its initial snapshot's `origin`
    // — needed so `fresh_state` can tell its own RequestState answer apart
    // from any other State frame that happens to arrive (see fresh_state's
    // doc comment for why that distinction matters).
    let init = read_until(&mut ws, r#""t":"State""#);
    let my_id = extract_origin(&init);
    let state = fresh_state(&mut ws, &my_id);
    assert!(
        state.contains(r#""live_sessions":[]"#),
        "merely opening a project must not spawn a shell; got: {state}"
    );

    // Prove the assertion above is not vacuous, i.e. that it would have
    // failed had a session really been spawned: the identical
    // RequestState/State path, against the identical project, does report a
    // session once a terminal socket genuinely attaches one. Without this,
    // "live_sessions":[] could just as well mean the field is hardcoded
    // empty, or State ignores live_sessions entirely, as it could mean
    // nothing was spawned.
    let mut term = ws_connect_path(port, "/ws/spawncheck/term/shell").unwrap();
    let live_state = wait_for_live_session(&mut ws, &my_id, "shell");
    assert!(
        live_state.contains(r#""live_sessions":["shell"]"#),
        "attaching a real terminal must make it show up live; got: {live_state}"
    );

    let _ = term.close(None);
    let _ = ws.close(None);
    std::env::remove_var("RESH_STATE_DIR");
    std::env::remove_var("RESH_CMD");
}

// CloseProject must end *every* session belonging to one project, report
// exactly how many, and leave every other project's sessions running.
//
// closealpha deliberately holds *two* sessions (closebeta holds one): with
// only one session per project, a `kill_project` that reports the right
// count but kills the wrong session (e.g. counts correctly while acting on
// a different project's key) — or one that only ever removes the *first*
// matching key rather than all of them — would still report "ended":1 and
// still leave closealpha showing empty. Two sessions on the project being
// closed forces the count, the completeness ("all of them", not just one),
// and the isolation to all be genuinely exercised at once; a
// single-project, single-session version of this test could pass with any
// of those three broken.
#[test]
fn close_project_ends_sessions_and_isolates_other_projects() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = two_project_fixture("closealpha", "closebeta");

    // Starting a terminal is what creates a session: connect its socket.
    let mut term_a1 = ws_connect_path(port, "/ws/closealpha/term/shell").unwrap();
    let mut term_a2 = ws_connect_path(port, "/ws/closealpha/term/build").unwrap();
    let mut term_b = ws_connect_path(port, "/ws/closebeta/term/shell").unwrap();

    let mut ws_a = ws_connect_path(port, "/ws/closealpha/_workspace").unwrap();
    let mut ws_b = ws_connect_path(port, "/ws/closebeta/_workspace").unwrap();
    // Each socket's own connection id, needed so `fresh_state` can tell its
    // own RequestState answer apart from any other State frame that
    // happens to arrive (see fresh_state's doc comment).
    let a_init = read_until(&mut ws_a, r#""t":"State""#);
    let a_id = extract_origin(&a_init);
    let b_init = read_until(&mut ws_b, r#""t":"State""#);
    let b_id = extract_origin(&b_init);

    // Wait for all three attaches to land before closing, so "ended"
    // reflects sessions that genuinely exist rather than racing term.rs's
    // attach. `session::live_names` sorts, so both of closealpha's land in
    // one deterministic snapshot: ["build","shell"].
    wait_for_state_containing(&mut ws_a, &a_id, r#""live_sessions":["build","shell"]"#);
    wait_for_live_session(&mut ws_b, &b_id, "shell");

    ws_a.send(tungstenite::Message::Text(r#"{"t":"CloseProject"}"#.into())).unwrap();
    let closed = read_until(&mut ws_a, r#""t":"ProjectClosed""#);
    assert!(closed.contains(r#""ended":2"#), "expected both of closealpha's sessions ended; got: {closed}");

    // closealpha itself must now report no live sessions — *both* gone, not
    // just the one a naive "remove the first matching key" fix would catch.
    let state_a = wait_for_state_containing(&mut ws_a, &a_id, r#""live_sessions":[]"#);
    assert!(
        state_a.contains(r#""live_sessions":[]"#),
        "closealpha must have no sessions left after CloseProject; got: {state_a}"
    );

    // closebeta's session must be untouched — proof this was project-scoped,
    // not a global kill that happened to only be observed from one project.
    // A single `fresh_state` call (not a polling wait) is deliberate: if
    // isolation were broken, the session would already be gone by now, and
    // polling for it to reappear would just make a broken test hang until
    // its deadline instead of failing promptly.
    let state_b = fresh_state(&mut ws_b, &b_id);
    assert!(
        state_b.contains(r#""live_sessions":["shell"]"#),
        "closing closealpha must not touch closebeta's session; got: {state_b}"
    );

    let _ = term_a1.close(None);
    let _ = term_a2.close(None);
    let _ = term_b.close(None);
    let _ = ws_a.close(None);
    let _ = ws_b.close(None);
    std::env::remove_var("RESH_STATE_DIR");
    std::env::remove_var("RESH_CMD");
}

/// Minimal, test-only "is anything holding this path" check via `ps`.
/// Deliberately not resh's own internal machinery (private to its
/// `registry` module, and hardened for production against inputs this
/// test's own known, plain tempdir paths don't need) — just enough to prove
/// a real process is or isn't there.
fn any_process_holds(path: &std::path::Path) -> bool {
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

// The end-to-end reproduction of the bug this task exists to fix, over a
// real WebSocket connection exactly like a browser's: closing a project
// with `RESH_CMD=cat` (every other close-project test, including the
// one just above) cannot exercise this at all, because a `cat` child has no
// detached dtach master to leave behind — that gap is exactly why the rest
// of this suite never caught it. Real, unoverridden `dtach` forks a master
// that immediately detaches and reparents to init; killing only the
// in-process client (the whole of what CloseProject used to do) is then
// just a *detach*, leaving that master and the user's shell running with a
// live socket. This proves the fix at the OS level, not just the in-memory
// session map the old, buggy code also lied through.
#[test]
fn close_project_ends_the_real_dtach_master_not_just_the_client() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("RESH_CMD");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("realclose");

    let mut term = ws_connect_path(port, "/ws/realclose/term/shell").unwrap();
    let mut ws = ws_connect_path(port, "/ws/realclose/_workspace").unwrap();
    let init = read_until(&mut ws, r#""t":"State""#);
    let my_id = extract_origin(&init);
    wait_for_live_session(&mut ws, &my_id, "shell");

    let sock =
        sd.path().join("sock").join(resh::projects::storage_key("realclose")).join("shell");
    // Poll rather than a fixed sleep: dtach's own fork-and-detach takes an
    // unpredictable, usually-small amount of wall time to complete. The
    // budget is deliberately far larger than the ~50ms this normally needs,
    // because the loop exits the moment the condition holds — so a generous
    // ceiling costs nothing on a fast run, while a tight one turns a loaded
    // machine (a concurrent `cargo test`, a cold `ps`) into a setup-assert
    // panic that reads as a genuine product failure.
    let mut waited = 0;
    while !(sock.exists() && any_process_holds(&sock)) && waited < 200 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        waited += 1;
    }
    assert!(sock.exists(), "test setup: dtach must have created its socket");
    assert!(
        any_process_holds(&sock),
        "test setup: a detached dtach master must be observable before CloseProject runs \
         — otherwise this test would prove nothing"
    );

    ws.send(tungstenite::Message::Text(r#"{"t":"CloseProject"}"#.into())).unwrap();
    let closed = read_until(&mut ws, r#""t":"ProjectClosed""#);
    assert!(closed.contains(r#""ended":1"#), "expected the one session reported ended; got: {closed}");

    assert!(
        !any_process_holds(&sock),
        "the dtach master — and, through it, the shell — must actually be dead, \
         not merely the in-process client CloseProject used to kill alone"
    );
    assert!(!sock.exists(), "the socket must be removed only once the holding process is confirmed gone");

    let _ = term.close(None);
    let _ = ws.close(None);
    std::env::remove_var("RESH_STATE_DIR");
}

#[test]
fn http_rejects_rebinding_host() {
    use std::io::{Read, Write};
    let (_d, port) = fixture();
    // DNS rebinding: the browser resolves a hostile name to 127.0.0.1, so the
    // page becomes same-origin and CORS no longer protects these reads.
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: evil.example.com\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 403"), "got: {}", &resp[..resp.len().min(60)]);
}

#[test]
fn a_notice_reaches_a_client_watching_a_different_project() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("alpha")).unwrap();
    std::fs::create_dir_all(d.path().join("beta")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let mut a = ws_connect_path(port, "/ws/alpha/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/beta/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    read_until(&mut b, r#""t":"State""#);

    // Published against alpha; beta's client must still see it.
    resh::hub::publish(
        "alpha",
        "claude",
        resh::osc::Parsed { title: Some("build".into()), body: "green".into() },
    );

    let seen_b = read_until(&mut b, r#""t":"Notice""#);
    assert!(seen_b.contains(r#""project":"alpha""#), "beta got: {seen_b}");
    assert!(seen_b.contains("green"), "beta got: {seen_b}");
    let seen_a = read_until(&mut a, r#""t":"Notice""#);
    assert!(seen_a.contains("green"), "alpha got: {seen_a}");
}

#[test]
fn notices_are_replayed_on_connect_and_read_state_mirrors() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    resh::hub::publish(
        "proj",
        "claude",
        resh::osc::Parsed { title: None, body: "waiting for you".into() },
    );

    // A client connecting *after* the fact still learns about it.
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let replay = read_until(&mut a, r#""t":"Notices""#);
    assert!(replay.contains("waiting for you"), "connect replay missing it: {replay}");
    let id: u64 = {
        // The notice store is process-global across the whole integration
        // binary and never reset between tests, so the *first* "id": in the
        // replay can belong to a notice some other test left behind, not
        // this one — grabbing that id would still happen to make this test
        // fail if mark_read broke (any id works for that), but it would not
        // be testing the notice this test actually published. Anchor on the
        // matched body instead: `id` is the first field on `Notice` (see
        // proto.rs's struct field order), so the nearest `"id":` preceding
        // this specific body belongs to this specific notice.
        let key = r#""id":"#;
        let body_pos = replay.find("waiting for you").expect("body missing from replay");
        let start = replay[..body_pos].rfind(key).expect("no id preceding the matched body") + key.len();
        replay[start..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
    };

    // Read state is global: b marks read, a must be told.
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    read_until(&mut b, r#""t":"Notices""#);
    b.send(tungstenite::Message::Text(format!(r#"{{"t":"MarkNoticeRead","id":{id}}}"#))).unwrap();
    let after = read_until(&mut a, r#""read":true"#);
    assert!(after.contains(r#""t":"Notices""#), "a was not re-sent the list: {after}");
}

#[test]
fn an_escape_sequence_from_a_terminal_becomes_a_notice() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", sd.path());

    // A single-token command: RESH_CMD splits on whitespace.
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("emit.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '\\033]777;notify;Build done;42 tests passed\\007'\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("RESH_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("notifyproj");
    let mut ctrl = ws_connect_path(port, "/ws/notifyproj/_workspace").unwrap();
    read_until(&mut ctrl, r#""t":"State""#);
    // Attaching the terminal socket is what spawns the session and its pump.
    let mut term = ws_connect_path(port, "/ws/notifyproj/term/claude").unwrap();

    let seen = read_until(&mut ctrl, r#""t":"Notice""#);
    assert!(seen.contains("Build done"), "title missing: {seen}");
    assert!(seen.contains("42 tests passed"), "body missing: {seen}");
    // Attribution comes from the pump's own identity, not from the payload.
    assert!(seen.contains(r#""session":"claude""#), "session missing: {seen}");
    assert!(seen.contains(r#""project":"notifyproj""#), "project missing: {seen}");

    let _ = term.close(None);
    std::env::remove_var("RESH_CMD");
}

#[test]
fn a_terminal_child_can_discover_that_notifications_exist() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("env.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"NOTIFY=$RESH_NOTIFY PROJ=$RESH_PROJECT SESS=$RESH_SESSION\"\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("RESH_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("envproj");
    let mut term = ws_connect_path(port, "/ws/envproj/term/envprobe").unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("NOTIFY=") {
            break;
        }
    }
    assert!(seen.contains("NOTIFY=1"), "capability flag missing: {seen:?}");
    assert!(seen.contains("PROJ=envproj"), "project missing: {seen:?}");
    assert!(seen.contains("SESS=envprobe"), "session missing: {seen:?}");
    let _ = term.close(None);
    std::env::remove_var("RESH_CMD");
}

/// Connects the terminal socket directly, with **no** prior `_workspace`
/// connection — the one thing a real browser always does first (loading the
/// project page), but a reconnecting client or a raw client is not
/// guaranteed to. That ordering is exactly what exposed a real bug while
/// implementing this: `term.rs` used to call `session::attach` (which reads
/// `ide::port_for`) *before* anything had ever started this project's ide
/// listener, so the very first terminal in a brand-new project came up with
/// no `CLAUDE_CODE_SSE_PORT` at all. Revert-checked: reverting `term.rs`'s
/// `ide::for_project` call back out (so only the pre-existing, later
/// `Hub::for_project` call remains) reproduces exactly this — see this
/// task's report for the observed failure.
#[test]
fn a_fresh_projects_first_terminal_already_carries_the_ide_port() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("sseport.sh");
    std::fs::write(&script, "#!/bin/sh\necho \"SSEPORT=$CLAUDE_CODE_SSE_PORT\"\nsleep 5\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("RESH_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("sseportproj");
    // No `_workspace` connection anywhere above this line: this project's
    // hub, and so its ide listener, has never been touched before.
    let mut term = ws_connect_path(port, "/ws/sseportproj/term/sseprobe").unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("SSEPORT=") {
            break;
        }
    }
    let ide_port: Option<u16> = seen
        .split("SSEPORT=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|p| p.trim().parse().ok());
    assert!(
        matches!(ide_port, Some(p) if p != 0),
        "expected a nonzero CLAUDE_CODE_SSE_PORT in the spawned shell's env; got: {seen:?}"
    );
    let _ = term.close(None);
    std::env::remove_var("RESH_CMD");
    // Best-effort cleanup: this writes a real lock file into the shared
    // test ide directory (`isolate_ide_dir_for_tests`), and nothing else
    // ever closes "sseportproj"'s project to remove it. Not load-bearing —
    // the whole directory is a `TempDir` that removes itself when this test
    // binary exits — but tidy, and it matches the same rule
    // `idelock::Lock`'s own `Drop` follows: `remove_file`, never a
    // directory scan.
    if let Some(p) = ide_port {
        let _ = std::fs::remove_file(resh::idelock::ide_dir().join(format!("{p}.lock")));
    }
}
