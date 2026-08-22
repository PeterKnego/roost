//! The socket Claude Code connects to, and the reason its rules differ from
//! every other socket in this codebase.
//!
//! resh is the *server* here. The extension model has the IDE listening and
//! `claude` connecting out to it, which is what lets the integration work for
//! a Claude attached to a dtach session resh did not spawn.
//!
//! The client is a Bun process, not a browser, so it sends no `Origin` — it
//! sends a token from the lock file. `origin.rs` refuses a handshake with no
//! Origin because "every browser sends one, so its absence means a non-browser
//! client, which has no business here." On this socket that reasoning runs
//! backwards: a browser is the only thing that sends one, and a browser has no
//! business here. Both sockets are right; the rules are opposites.
//!
//! That is not a stylistic point. Claude Code's own extensions shipped this
//! socket unauthenticated and Origin-blind through version 1.0.23, and because
//! WebSocket handshakes bypass the same-origin policy, any web page could scan
//! localhost, connect, and read files — CVE-2025-52882, fixed in 1.0.24 by the
//! lock-file token this module implements.
use crate::idelock;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::accept_hdr_with_config;

/// An `openDiff` carries a whole file, capped elsewhere at 2 MB; this is the
/// coarse backstop against an oversized frame being buffered at all.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub struct Ide {
    pub port: u16,
    pub token: String,
    /// Removed on drop, and only ever the path we wrote.
    _lock: idelock::Lock,
}

/// Length is not secret — the token is a fixed 32 hex chars — but the bytes
/// are, so the comparison must not stop at the first difference.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    // Port 0: the OS picks, and the lock file must advertise what was actually
    // bound, not what was asked for.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = idelock::new_token()?;
    let lock = idelock::write_in(dir, port, &token, &workspace)?;
    let ide = Arc::new(Ide { port, token: token.clone(), _lock: lock });
    let project = project.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let token = token.clone();
            let project = project.clone();
            let workspace = workspace.clone();
            std::thread::spawn(move || {
                // A panic here must not take the process down with it: this
                // thread is fed attacker-influenced bytes.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_conn(stream, &token, &project, &workspace);
                }));
            });
        }
    });
    Ok(ide)
}

pub fn start(project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    start_in(&idelock::ide_dir(), project, workspace)
}

fn serve_conn(stream: TcpStream, token: &str, _project: &str, _workspace: &Path) {
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    let accepted = accept_hdr_with_config(
        stream,
        |req: &WsRequest, mut resp: WsResponse| {
            let deny = |why: &str| {
                eprintln!("resh: rejected ide ws handshake ({why})");
                tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("forbidden".to_string()))
                    .expect("static 403 response")
            };
            // See the module doc: on this socket an Origin is disqualifying.
            if req.headers().get("origin").is_some() {
                return Err(deny("carries an Origin, so it is a browser"));
            }
            let got = req
                .headers()
                .get("x-claude-code-ide-authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !ct_eq(got.as_bytes(), token.as_bytes()) {
                return Err(deny("token mismatch"));
            }
            // The CLI asks for the `mcp` subprotocol; a server that does not
            // echo it back is not guaranteed to be accepted by the client.
            resp.headers_mut().insert(
                "sec-websocket-protocol",
                tungstenite::http::HeaderValue::from_static("mcp"),
            );
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };
    // Task 4 replaces this with the JSON-RPC loop.
    while ws.read().is_ok() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    /// `impl IntoClientRequest for http::Request<()>` in tungstenite 0.24 is a
    /// bare pass-through (`Ok(self)`), and `generate_request` then *requires*
    /// Host/Connection/Upgrade/Sec-WebSocket-Version/Sec-WebSocket-Key to
    /// already be on the request — it does not add them. So this helper adds
    /// the boilerplate RFC 6455 headers itself, on top of the custom ones
    /// under test, rather than relying on the client to fill them in.
    fn connect(port: u16, token: Option<&str>, origin: Option<&str>) -> Result<(), String> {
        let mut b = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Protocol", "mcp");
        if let Some(t) = token {
            b = b.header("X-Claude-Code-Ide-Authorization", t);
        }
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        let req = b.body(()).unwrap().into_client_request().unwrap();
        tungstenite::connect(req).map(|_| ()).map_err(|e| e.to_string())
    }

    fn started() -> (tempfile::TempDir, tempfile::TempDir, Arc<Ide>) {
        let lockdir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = start_in(lockdir.path(), "proj", ws.path().to_path_buf()).unwrap();
        (lockdir, ws, ide)
    }

    #[test]
    fn the_right_token_and_no_origin_connects() {
        let (_l, _w, ide) = started();
        connect(ide.port, Some(&ide.token), None).expect("the CLI's own shape must be accepted");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&"0".repeat(32)), None)
            .expect_err("a guessed token must not connect");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_missing_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, None, None).expect_err("no token, no socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_handshake_carrying_an_origin_is_refused_even_with_the_right_token() {
        // This is CVE-2025-52882 in one assertion. A browser is the only
        // thing that sends Origin, WebSocket handshakes bypass the
        // same-origin policy, and this socket can read files. A page that
        // somehow learned the token still must not get in.
        //
        // Note this is the *inverse* of origin.rs's rule for the workspace
        // socket, which refuses a handshake with no Origin. Both are right.
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&ide.token), Some("https://evil.example"))
            .expect_err("a browser must never reach this socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_loopback_origin_is_refused_too() {
        // The workspace socket allows loopback origins. This one does not:
        // resh's own page has no business here either, and allowing it would
        // reopen the hole for anything that can forge an origin.
        let (_l, _w, ide) = started();
        assert!(connect(ide.port, Some(&ide.token), Some("http://127.0.0.1:8444")).is_err());
    }

    #[test]
    fn starting_advertises_the_port_it_actually_bound() {
        let (lockdir, ws, ide) = started();
        let f = lockdir.path().join(format!("{}.lock", ide.port));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        assert_eq!(v["authToken"], ide.token.as_str());
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
        assert_ne!(ide.port, 0, "an OS-assigned port must be read back after bind");
    }

    #[test]
    fn constant_time_compare_answers_correctly_including_on_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }
}
