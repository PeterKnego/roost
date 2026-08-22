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
use crate::idecwd::{self, Cwd};
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

pub struct Conn {
    /// Claude's working directory, learned from `ide_connected`'s pid. `None`
    /// until it connects, or when resh could not read it — those are different
    /// situations with the same representation here only because both mean
    /// "do not trust a path against it yet".
    pub cwd: Option<PathBuf>,
    pub workspace: PathBuf,
    pub project: String,
    /// This connection's writer channel. Unused until Task 6 gives the
    /// connection a writer thread, and carried from the start because the
    /// connection owns its identity and its output from the moment it exists.
    pub reply: std::sync::mpsc::Sender<String>,
    pub closed: bool,
}

impl Conn {
    pub fn new(project: &str, workspace: PathBuf, reply: std::sync::mpsc::Sender<String>) -> Self {
        Conn { cwd: None, workspace, project: project.to_string(), reply, closed: false }
    }
}

fn err(id: &serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn ok(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn text_result(s: &str) -> serde_json::Value {
    serde_json::json!({"content": [{"type": "text", "text": s}]})
}

fn dispatch(msg: &serde_json::Value, conn: &mut Conn) -> Option<serde_json::Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg.get("id").cloned();

    // A message with no id is a notification: answering one is a protocol
    // error, not a harmless extra.
    let Some(id) = id else {
        if method == "ide_connected" {
            let pid = msg["params"]["pid"].as_u64().unwrap_or(0) as u32;
            match idecwd::cwd_of(pid) {
                Cwd::At(p) => conn.cwd = Some(p),
                // Gone and Unknown both leave cwd unset, and neither closes
                // the connection here: the socket itself is the evidence that
                // something is on the other end, and it is more trustworthy
                // than a /proc lookup that just failed.
                Cwd::Gone | Cwd::Unknown => {}
            }
        }
        return None;
    };

    match method {
        "initialize" => Some(ok(
            &id,
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "resh", "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "tools/list" => Some(ok(
            &id,
            serde_json::json!({"tools": [{
                "name": "getDiagnostics",
                "description": "Get language diagnostics from the editor",
                "inputSchema": {
                    "type": "object",
                    "properties": {"uri": {"type": "string"}},
                },
            }]}),
        )),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            match name {
                // resh has no language server. An empty list is the honest
                // answer and is what Claude sees when nothing is wrong — so
                // if a `cargo check` bridge ever lands, it lands here.
                "getDiagnostics" => Some(ok(&id, text_result("[]"))),
                other => Some(err(&id, -32601, format!("resh does not implement {other}"))),
            }
        }
        "ping" => Some(ok(&id, serde_json::json!({}))),
        other => Some(err(&id, -32601, format!("unknown method {other}"))),
    }
}

fn serve_conn(stream: TcpStream, token: &str, project: &str, workspace: &Path) {
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
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let mut conn = Conn::new(project, workspace.to_path_buf(), reply_tx);
    let _ = reply_rx; // drained by the writer thread from Task 6 onward
    loop {
        let Ok(msg) = ws.read() else { break };
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(reply) = dispatch(&v, &mut conn) {
            if ws.send(tungstenite::Message::Text(reply.to_string())).is_err() {
                break;
            }
        }
        if conn.closed {
            break;
        }
    }
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

    /// Same request-building as `connect`, but keeps the handshake response
    /// instead of discarding it — needed to assert on a response *header*
    /// (the echoed subprotocol), which `connect`'s callers never inspect.
    fn connect_response(
        port: u16,
        token: Option<&str>,
        origin: Option<&str>,
    ) -> Result<tungstenite::http::Response<Option<Vec<u8>>>, String> {
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
        tungstenite::connect(req).map(|(_, resp)| resp).map_err(|e| e.to_string())
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
        let err = connect(ide.port, Some(&ide.token), Some("http://127.0.0.1:8444"))
            .expect_err("a loopback origin must be refused too");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
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
    fn the_server_echoes_the_mcp_subprotocol_the_client_asked_for() {
        // tungstenite's client does not itself verify the negotiated
        // subprotocol, so a missing echo would not show up as a connection
        // failure anywhere else in this file — it has to be checked directly
        // against the handshake response's own headers.
        let (_l, _w, ide) = started();
        let resp = connect_response(ide.port, Some(&ide.token), None)
            .expect("the right token and no origin must connect");
        assert_eq!(
            resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()),
            Some("mcp"),
            "the server must echo back the `mcp` subprotocol the client asked for"
        );
    }

    #[test]
    fn constant_time_compare_answers_correctly_including_on_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }

    fn rpc(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[test]
    // Revert-checked: changing `"resh"` to `"BROKEN"` in dispatch's
    // "initialize" arm failed only this test — `assertion `left == right`
    // failed / left: String("BROKEN") / right: "resh"` — then restored.
    fn initialize_answers_with_resh_as_the_server_name() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(1, "initialize", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 1);
        assert_eq!(out["result"]["serverInfo"]["name"], "resh");
        assert!(out["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn the_tool_list_offers_diagnostics_and_never_offers_code_execution() {
        // executeCode is one of only two tools the CLI makes visible to the
        // model, and it is arbitrary code execution reachable from this
        // socket. Adding it to the list is the defect this asserts against.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(2, "tools/list", serde_json::json!({})), &mut c).unwrap();
        let names: Vec<String> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["getDiagnostics".to_string()]);
    }

    #[test]
    // An empty success would read to Claude as "ran, produced nothing".
    // Revert-checked: adding an `"executeCode" => Some(ok(&id,
    // text_result("")))` arm ahead of the error fallthrough failed only this
    // test — `left: Null / right: -32601` on `out["error"]["code"]`, i.e. the
    // error object was simply absent — proving this test actually
    // distinguishes a refusal from an empty success rather than passing
    // vacuously. Then restored.
    fn calling_execute_code_is_a_method_error_not_an_empty_success() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(
            &rpc(3, "tools/call", serde_json::json!({"name": "executeCode", "arguments": {"code": "1"}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["error"]["code"], -32601);
        assert!(
            out["error"]["message"].as_str().unwrap().contains("executeCode"),
            "the refusal must name what was refused: {}", out["error"]["message"]
        );
        assert!(out.get("result").is_none());
    }

    #[test]
    // Revert-checked: changing the "getDiagnostics" arm's `text_result("[]")`
    // to `text_result("[{\"bogus\":true}]")` failed only this test —
    // `left: String("[{\"bogus\":true}]") / right: "[]"` — then restored.
    fn diagnostics_answers_empty_rather_than_failing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(
            &rpc(4, "tools/call", serde_json::json!({"name": "getDiagnostics", "arguments": {}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["result"]["content"][0]["type"], "text");
        assert_eq!(out["result"]["content"][0]["text"], "[]");
    }

    #[test]
    // Revert-checked: making the notification branch `return
    // Some(ok(&Value::Null, json!({})))` instead of `return None` failed
    // this test — `panicked ... "notifications get no response"` — exercising
    // the `is_none()` assertion. The sibling pid test below shares the same
    // notification branch and failed alongside it for the same reason
    // (`assertion failed: dispatch(&note, &mut c).is_none()`); both are
    // legitimate hits on the one break, not evidence of a false positive.
    // Then restored.
    fn ide_connected_resolves_the_senders_directory_and_is_not_answered() {
        // A notification has no id, so a reply would be a protocol error.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", std::env::current_dir().unwrap(), tx);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected",
            "params": {"pid": std::process::id()}
        });
        assert!(dispatch(&note, &mut c).is_none(), "notifications get no response");
        assert_eq!(
            c.cwd.as_ref().unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    // Revert-checked: changing the `Cwd::Gone | Cwd::Unknown => {}` arm to
    // `=> conn.closed = true` failed only this test — `panicked ... "but the
    // connection stays open"` — proving the tri-state is load-bearing, not
    // decorative. Then restored.
    fn ide_connected_from_an_unreadable_pid_leaves_the_connection_usable() {
        // Cwd::Unknown must not disconnect. Folding it into "gone" would kill
        // a live Claude because a check failed.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": u32::MAX}
        });
        assert!(dispatch(&note, &mut c).is_none());
        assert!(c.cwd.is_none(), "no directory was learned");
        assert!(!c.closed, "but the connection stays open");
    }

    #[test]
    // Revert-checked: changing the unknown-method arm to `err(&Value::Null,
    // ...)` instead of `err(&id, ...)` failed only this test —
    // `left: Null / right: 9` on `out["id"]` — then restored.
    fn an_unknown_method_is_a_method_error_carrying_the_request_id() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(9, "nonsense/method", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 9);
        assert_eq!(out["error"]["code"], -32601);
    }
}
