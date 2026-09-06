//! The roots socket: adding a root and its Origin check.

mod common;
use common::*;

/// `/ws/_roots`: one exchange, Origin-checked. A missing Origin is refused at
/// the handshake like every browser-facing socket; a loopback Origin gets one
/// reply and the file changes; a second connection sees the new root.
///
/// Revert-checked twice. Restoring `let roots = roots.clone();` in `serve`'s
/// accept loop (in place of `projects::roots()`) fails the last assertion —
/// "roots are re-read per connection" — because the front page then keeps
/// answering with the roots list `serve` started with. Removing the Origin
/// check from `handle_ws`'s handshake callback (accepting unconditionally)
/// fails the first assertion — "a handshake without Origin must be refused".
/// Both restored.
#[test]
fn roots_socket_adds_a_root_and_refuses_a_handshake_without_origin() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let global = d.path().join("config.toml");
    std::env::set_var("ROOST_CONFIG", &global);
    std::env::remove_var("ROOST_ROOTS");
    let dir = d.path().join("projects");
    std::fs::create_dir(&dir).unwrap();
    let port = start(vec![]); // empty root list: `start` clears ROOST_ROOTS so `roots_from` falls through to `global`
    // No Origin: refused.
    assert!(ws_connect_roots(port, None).is_err(), "a handshake without Origin must be refused");
    // Loopback Origin: one exchange.
    let mut ws = ws_connect_roots(port, Some(&format!("http://127.0.0.1:{port}"))).unwrap();
    ws.send(tungstenite::Message::Text(format!(r#"{{"t":"AddRoot","path":"{}"}}"#, dir.display()).into())).unwrap();
    let reply = ws.read().unwrap().into_text().unwrap();
    assert!(
        reply.contains(r#""t":"Roots""#) && reply.contains(&dir.canonicalize().unwrap().display().to_string()),
        "{reply}"
    );
    assert!(std::fs::read_to_string(&global).unwrap().contains("roots = ["), "the global file gained the list");
    // The next request sees it: the front page's roots label names it.
    let page = ureq::get(&format!("http://127.0.0.1:{port}/")).call().unwrap().into_string().unwrap();
    assert!(page.contains(&dir.canonicalize().unwrap().display().to_string()), "roots are re-read per connection");
    std::env::remove_var("ROOST_CONFIG");
}
