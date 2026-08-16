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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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

fn read_until(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    needle: &str,
) -> String {
    for _ in 0..40 {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                if t.contains(needle) {
                    return t.to_string();
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("never saw {needle:?}");
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
    let (_d, port) = fixture();
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws/proj/_workspace").into_client_request().unwrap();
    req.headers_mut().insert("origin", "https://evil.example.com".parse().unwrap());
    assert!(tungstenite::connect(req).is_err(), "the write socket must not be cross-origin");
}

#[test]
fn workspace_socket_rejects_missing_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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

    // Claude, in the next pane, rewrites the file
    std::fs::write(d.path().join("proj/hello.md"), "# Rewritten by Claude\n").unwrap();
    let seen = read_until(&mut a, "Rewritten by Claude");
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
    let _g = WS_TEST_LOCK.lock().unwrap();
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
