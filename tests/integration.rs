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
    let mut req = format!("ws://127.0.0.1:{port}/ws/proj").into_client_request().unwrap();
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

#[test]
fn ws_closes_when_child_exits_first() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    std::env::set_var("DEADLIGHT_CMD", "true"); // exits immediately
    let (_d, port) = fixture();
    let mut ws = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    // child exited at spawn; the server must close/shutdown the socket rather than hang
    let mut closed = false;
    for _ in 0..50 {
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) | Err(_) => { closed = true; break; }
            Ok(_) => {}
        }
    }
    assert!(closed, "socket did not close after child exit");
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

#[test]
fn workspace_state_mirrors_between_two_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();

    // the *other* browser must learn about it without asking
    let seen = read_until(&mut b, "hello.md");
    assert!(seen.contains(r#""t":"State""#));
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
