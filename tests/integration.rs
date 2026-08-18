use std::net::TcpListener;
use std::path::PathBuf;

fn start(roots: Vec<PathBuf>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || deadlight::serve(listener, roots));
    port
}

fn fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("proj")).unwrap();
    std::fs::write(d.path().join("proj/hello.md"), "# Hello\n").unwrap();
    std::fs::create_dir(d.path().join("proj/.deadlight")).unwrap();
    std::fs::write(d.path().join("proj/.deadlight/config.toml"), "theme = \"light\"\n").unwrap();
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
    assert!(body.contains("/static/themes/light.css")); // .deadlight config read per request
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
        assert!(body.contains("crumb-current\">deadlight"), "did not fall back for at={at}");
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
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    std::env::set_var("DEADLIGHT_CMD", "cat");
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

    std::env::remove_var("DEADLIGHT_STATE_DIR");
    std::env::remove_var("DEADLIGHT_CMD");
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
    // A cloned repo controls .deadlight/theme.css. If the fragment handler
    // did a bare fs::read of that path, a symlink planted there pointing at
    // e.g. ~/.ssh/id_rsa would be served straight to the browser as
    // text/css. serve_frag must resolve it through safe_resolve like every
    // other file read, so the escape is refused the same way path
    // traversal already is.
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("themeleak")).unwrap();
    std::fs::create_dir(d.path().join("themeleak/.deadlight")).unwrap();
    let secret = d.path().join("secret.txt");
    std::fs::write(&secret, "top secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, d.path().join("themeleak/.deadlight/theme.css")).unwrap();
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

// DEADLIGHT_CMD is process-global; both ws tests set it, and if they ran in
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
    std::env::set_var("DEADLIGHT_CMD", "cat");
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
    std::env::set_var("DEADLIGHT_CMD", "cat");
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
    std::env::set_var("DEADLIGHT_CMD", "true"); // exits immediately
    let (_d, port) = fixture();
    // Own session name: the process-global registry may already hold a live
    // "proj/shell" session from another test in this binary (e.g.
    // terminal_ws_echoes_through_pty's `cat`), in which case DEADLIGHT_CMD
    // would never be consulted for a fresh spawn and this test would prove
    // nothing about a child exiting first.
    let mut ws = ws_connect_path(port, "/ws/proj/term/exiter").unwrap();
    // child exited at spawn; the server must close/shutdown the socket rather than hang
    assert_ws_closes(&mut ws, "ws_closes_when_child_exits_first");
}

#[test]
fn two_terminal_clients_mirror_one_session() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn invalid_session_name_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("DEADLIGHT_CMD", "cat");
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

#[test]
fn workspace_state_mirrors_between_two_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
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
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn external_edit_updates_a_clean_buffer_live() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    std::env::set_var("DEADLIGHT_DEBOUNCE_MS", "10");
    let (d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
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
    let hello_path = d.path().join("proj/hello.md");
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

    // Hub is a process-global registry keyed by project name (see hub.rs),
    // so "proj" outlives this test for the rest of the binary's run. Close
    // the buffer we opened, or its leftover entry pollutes every later
    // test's State snapshot for "proj" (see the comment in
    // workspace_state_mirrors_between_two_clients, which already guards
    // against exactly this class of cross-test leakage).
    a.send(tungstenite::Message::Text(r#"{"t":"CloseBuffer","rel":"hello.md"}"#.into())).unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    let _ = a.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
    std::env::remove_var("DEADLIGHT_DEBOUNCE_MS");
}

#[test]
fn set_mode_edit_then_save_writes_the_file() {
    // End-to-end regression for the live-verified bug: SetMode{Edit} must
    // make the server read the file (setting a real base_hash) before the
    // client ever calls SaveBuffer, or every first save reports a conflict
    // and the file on disk never changes.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn reconnect_replays_buffer_text_for_open_edit_buffers() {
    // A client that (re)connects onto a layout with an already-open Edit
    // buffer gets metadata-only State — never text — so without a replay,
    // that editor renders permanently blank until someone happens to edit
    // the same file again.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
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
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
    std::env::remove_var("DEADLIGHT_CMD");
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
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
    std::env::remove_var("DEADLIGHT_CMD");
}

/// Minimal, test-only "is anything holding this path" check via `ps`.
/// Deliberately not deadlight's own internal machinery (private to its
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
// with `DEADLIGHT_CMD=cat` (every other close-project test, including the
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
    std::env::remove_var("DEADLIGHT_CMD");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("realclose");

    let mut term = ws_connect_path(port, "/ws/realclose/term/shell").unwrap();
    let mut ws = ws_connect_path(port, "/ws/realclose/_workspace").unwrap();
    let init = read_until(&mut ws, r#""t":"State""#);
    let my_id = extract_origin(&init);
    wait_for_live_session(&mut ws, &my_id, "shell");

    let sock =
        sd.path().join("sock").join(deadlight::projects::storage_key("realclose")).join("shell");
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
    std::env::remove_var("DEADLIGHT_STATE_DIR");
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
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("alpha")).unwrap();
    std::fs::create_dir_all(d.path().join("beta")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let mut a = ws_connect_path(port, "/ws/alpha/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/beta/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    read_until(&mut b, r#""t":"State""#);

    // Published against alpha; beta's client must still see it.
    deadlight::hub::publish(
        "alpha",
        "claude",
        deadlight::osc::Parsed { title: Some("build".into()), body: "green".into() },
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
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    deadlight::hub::publish(
        "proj",
        "claude",
        deadlight::osc::Parsed { title: None, body: "waiting for you".into() },
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
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());

    // A single-token command: DEADLIGHT_CMD splits on whitespace.
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
    std::env::set_var("DEADLIGHT_CMD", script.to_str().unwrap());

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
    std::env::remove_var("DEADLIGHT_CMD");
}

#[test]
fn a_terminal_child_can_discover_that_notifications_exist() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("env.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"NOTIFY=$DEADLIGHT_NOTIFY PROJ=$DEADLIGHT_PROJECT SESS=$DEADLIGHT_SESSION\"\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("DEADLIGHT_CMD", script.to_str().unwrap());

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
    std::env::remove_var("DEADLIGHT_CMD");
}
