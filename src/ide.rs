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
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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
    /// Set by `request_shutdown` (called from `stop`), and checked by the
    /// accept loop between connections. A flag alone cannot wake a thread
    /// already parked in `accept(2)` inside `TcpListener::incoming()` — see
    /// `request_shutdown` for how it actually gets observed.
    stopped: Arc<AtomicBool>,
}

impl Ide {
    /// Task 3's review flagged that dropping the `Arc<Ide>` removed the lock
    /// file but left the accept loop running forever, authenticating against
    /// a token no longer advertised anywhere. Not exploitable (the token
    /// stays unguessable), but `stop` is where a real shutdown belongs.
    ///
    /// `TcpListener::incoming()` blocks in the kernel until a connection
    /// arrives, so setting `stopped` by itself would not be noticed until
    /// some *other* client happened to connect next — which might be never.
    /// A loopback self-connect delivers exactly one more item to
    /// `incoming()`, which is what lets the loop actually observe the flag
    /// and exit. That self-connection is never served: the loop checks
    /// `stopped` before it ever matches on the accepted stream, so it just
    /// gets dropped. Once the loop breaks, `listener` (moved into the
    /// thread's closure) drops too, which is what actually closes the port.
    fn request_shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Bounded, not just best-effort: Task 5's review (finding 1) caught
        // this connect running *inside* `stop`'s registry-lock critical
        // section, which made a stalled loopback connect a global stall —
        // every `attach` in the process reads `ide::port_for` while holding
        // the *sessions* lock (see `session::attach`'s doc comment), so one
        // wedged `TcpStream::connect` here would have wedged every session
        // in every project, the exact historical defect CLAUDE.md's "never
        // hold a lock across blocking I/O" constraint was written from.
        // `stop` no longer holds that lock across this call at all (see
        // `stop`), but the connect itself still gets its own timeout rather
        // than staying unbounded: an untimed connect here would still be a
        // real, if now solitary, way for `stop` itself to hang, and nothing
        // requires this self-connect to succeed for shutdown to be correct
        // (see the comment below).
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        // If this fails or times out (e.g. the listener already closed for
        // some other reason) the accept loop still exits on the next real
        // connection or at process exit. Never panic here — this runs on
        // the CloseProject path and must not take a browser socket down
        // with it.
        let _ = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500));
    }
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
    let stopped = Arc::new(AtomicBool::new(false));
    let ide = Arc::new(Ide { port, token: token.clone(), _lock: lock, stopped: stopped.clone() });
    let project = project.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            // Checked before the stream is even matched on: the self-connect
            // `request_shutdown` makes to wake this loop must never be
            // handed to `serve_conn`, only used to get back here and exit.
            if stopped.load(Ordering::SeqCst) {
                break;
            }
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

/// One listener per project, not one per connection to it — mirrors
/// `Hub::for_project`'s registry, and is deliberately a *separate* map: a
/// `claude` reconnecting to a project resh already has open must not be
/// handed a second listener (a second port, a second lock file, and a stale
/// one left behind that Claude Code would just as happily match against).
static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Ide>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Arc<Ide>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `dir`-parameterised for tests (see `Task 4`'s `start_in`); production
/// code should call `for_project`.
pub fn for_project_in(dir: &Path, project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    // Fast path only touches the map: no I/O, so the lock is held for a
    // lookup and a clone, nothing more.
    {
        let map = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(project) {
            return Some(existing.clone());
        }
    }
    // The registry lock is released before `start_in`'s blocking I/O (a TCP
    // bind, a urandom read, a file write, a thread spawn) — CLAUDE.md's
    // "never hold a lock across blocking I/O" is not a style preference
    // here: this call sits directly ahead of `session::attach` on the
    // terminal-open path (see `term.rs`), which every project opened during
    // a test run — or in production, every terminal in every project —
    // passes through. Holding one process-global mutex across that I/O
    // would serialize every concurrent "first terminal in a new project"
    // in the whole process behind whichever one happened to go first;
    // dropping the lock here lets unrelated projects build their listeners
    // in parallel, which is exactly what fixed a real, reproducible
    // regression this task introduced (five integration tests started
    // failing "no such session" once this call sat, lock held, ahead of
    // `attach` — see this task's report).
    let built = start_in(dir, project, workspace);
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = map.get(project) {
        let existing = existing.clone();
        // Another caller for the same project won the race while this one
        // was building its own listener. Task 5's review (finding 3)
        // pointed out this is not the rare case the first draft of this
        // comment claimed — a page load opens the `_workspace` socket
        // (`hub.rs`) and a terminal socket (`term.rs`) for the same project
        // near-simultaneously, which is the ordinary reconnect flow, not an
        // edge case. So the loser's `Ide` is shut down properly, the same
        // way `stop` does it: released from the registry lock first (its
        // own `request_shutdown` call, and the lock-file removal its final
        // drop triggers, are both real syscalls), rather than just letting
        // it leak a live accept thread until process exit.
        drop(map);
        if let Ok(losing_ide) = built {
            losing_ide.request_shutdown();
        }
        return Some(existing);
    }
    match built {
        Ok(ide) => {
            map.insert(project.to_string(), ide.clone());
            Some(ide)
        }
        Err(e) => {
            // Degraded, never fatal: IDE integration is a convenience, and a
            // read-only home directory (or any other reason the lock file
            // could not be written) must not stop a project from opening.
            eprintln!("resh: IDE integration unavailable for {project}: {e}");
            None
        }
    }
}

pub fn for_project(project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    for_project_in(&idelock::ide_dir(), project, workspace)
}

pub fn port_for(project: &str) -> Option<u16> {
    let map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).map(|i| i.port)
}

/// Removing the project's entry drops the last `Arc<Ide>` this module holds,
/// which drops the `Lock` inside it — removing exactly the lock file this
/// listener wrote, never a directory scan. `request_shutdown` (below) is
/// what stops the *listener*; without it the accept loop would keep running,
/// unadvertised, until the process exits. Must not disturb any other
/// project's entry: removed by key, never by clearing the map.
///
/// Task 5's review (finding 1, Critical): the registry lock used to stay
/// held across both `request_shutdown`'s connect *and* the removed `Arc`'s
/// own drop (which runs `idelock::Lock::drop`'s `remove_file`) — two real
/// syscalls, one process-global mutex, exactly the "never hold a lock
/// across blocking I/O" constraint this codebase has already shipped one
/// deadlock from. `session::attach` reads `ide::port_for` while holding the
/// *sessions* lock, so a stalled connect here would have stalled every
/// `attach` in every project, not just this one's. `map.remove` now returns
/// the `Arc` out of the locked block entirely — the lock is released before
/// `ide` (and everything its drop does) is ever touched.
pub fn stop(project: &str) {
    let ide = {
        let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
        map.remove(project)
    };
    if let Some(ide) = ide {
        ide.request_shutdown();
        // `ide` drops here, well after the registry lock above was
        // released — that drop is what removes the lock file.
    }
}

/// Live Claude connections per project. A `Sender` per connection, drained by
/// that connection's writer thread — the notification path must never block
/// on a socket write, and must never hold this lock while doing one.
///
/// Each entry carries a small connection id alongside its `Sender`.
/// `std::sync::mpsc::Sender` — unlike tokio's or crossbeam's — exposes no
/// `same_channel`/identity check at all, so a `ConnGuard` has no way to find
/// "its" entry in a `Vec<Sender<String>>` once other connections have
/// registered and deregistered around it; the id is what makes removal
/// correct regardless of registration/deregistration order.
static CONNS: OnceLock<Mutex<HashMap<String, Vec<(u64, std::sync::mpsc::Sender<String>)>>>> =
    OnceLock::new();

fn conns() -> &'static Mutex<HashMap<String, Vec<(u64, std::sync::mpsc::Sender<String>)>>> {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_CONN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Deregisters this connection's writer channel even if `serve_conn`'s read
/// loop panics — the same guarantee `wsconn.rs`'s `UnsubGuard` gives its
/// subscriber list. Without it a panicking connection would leave a dead
/// `Sender` in `CONNS` forever, and every later `mention` for this project
/// would "succeed" into a void: the send itself never fails (the writer
/// thread on the other end is what actually delivers it, and that thread
/// exited when its own channel disconnected, so nothing is left to act on
/// the send).
struct ConnGuard {
    project: String,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut map = conns().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = map.get_mut(&self.project) {
            v.retain(|(id, _)| *id != self.id);
            if v.is_empty() {
                map.remove(&self.project);
            }
        }
    }
}

/// Tells every Claude connected to `project` that the user pointed at `abs`
/// (and optionally a line range within it). A notification, not a request —
/// see `dispatch`'s id handling: an `at_mentioned` carrying an `id` would
/// make the CLI wait for a response that will never come.
pub fn mention(project: &str, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String> {
    let mut params = serde_json::json!({"filePath": abs.to_string_lossy()});
    if let Some((a, b)) = lines {
        params["lineStart"] = serde_json::json!(a);
        params["lineEnd"] = serde_json::json!(b);
    }
    let msg = serde_json::json!({"jsonrpc": "2.0", "method": "at_mentioned", "params": params})
        .to_string();
    // Cloned out under the lock, sent outside it: CLAUDE.md's "never hold a
    // lock across blocking I/O" — a channel send only blocks if the writer
    // thread's queue capacity is exceeded (it is unbounded here, so it never
    // does), but the principle is the same one that bit `stop`, so this
    // follows the same shape regardless.
    let targets: Vec<_> = {
        let map = conns().lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).cloned().unwrap_or_default()
    };
    if targets.is_empty() {
        return Err("no Claude is connected to this project".into());
    }
    for (_, t) in &targets {
        let _ = t.send(msg.clone());
    }
    Ok(())
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
    // Built before the handshake, not after: registration happens inside the
    // handshake callback below (see the comment there for why that ordering
    // is load-bearing, not cosmetic).
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let reg_tx = reply_tx.clone();
    let reg_project = project.to_string();
    let accepted = accept_hdr_with_config(
        stream,
        move |req: &WsRequest, mut resp: WsResponse| {
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
            // Registered here, inside the handshake callback, and not after
            // `accept_hdr_with_config` returns: this callback runs and
            // returns *before* tungstenite writes the handshake response
            // bytes to the client. Registering after the call returned lost
            // the race in practice — a fake client's `connect()` can return
            // and this thread can call `mention` before the accepting
            // thread gets scheduled again, so `mention` found nothing and
            // `a_mention_is_the_notification_the_cli_expects` failed with
            // "no Claude is connected to this project" (see this task's
            // report for the actual failure output). Registering here
            // instead makes "the client observed a successful handshake"
            // happen-after "this connection is in `CONNS`", not merely
            // usually-before.
            let mut map = conns().lock().unwrap_or_else(|e| e.into_inner());
            map.entry(reg_project.clone()).or_default().push((conn_id, reg_tx.clone()));
            Ok(resp)
        },
        Some(config),
    );
    // Built right after the handshake attempt, before checking whether it
    // actually succeeded: the callback above can register this connection
    // and then have the handshake still fail as a whole (e.g. a write error
    // flushing the response tungstenite already decided to send) — an
    // "accepted, but then failed anyway" outcome that would otherwise leave
    // a registration nothing ever cleans up. Constructing the guard
    // unconditionally, before the `Ok`/`Err` branch, means its `Drop` always
    // runs on every exit from here on, including that one; for a handshake
    // that was denied before ever reaching the registration line (a bad
    // token, an Origin), removing an id that was never inserted is a no-op.
    let guard = ConnGuard { project: project.to_string(), id: conn_id };
    let Ok(mut ws) = accepted else { return };
    let mut conn = Conn::new(project, workspace.to_path_buf(), reply_tx.clone());
    // Only conn's and the registry's own clones should keep the channel
    // open from here — this one has done its job.
    drop(reply_tx);

    // Splits read and write the way wsconn.rs and term.rs do: a writer
    // thread owns `ws_write` exclusively and drains `reply_rx`, so every
    // outbound message — a direct reply *and* an out-of-band `at_mentioned`
    // pushed by `mention` from another thread — goes through one writer,
    // never two threads racing to write the same socket.
    let Ok(write_half) = ws.get_ref().try_clone() else { return };
    let mut ws_write: tungstenite::WebSocket<TcpStream> =
        tungstenite::WebSocket::from_raw_socket(write_half, tungstenite::protocol::Role::Server, None);
    let writer = std::thread::spawn(move || {
        while let Ok(msg) = reply_rx.recv() {
            if ws_write.send(tungstenite::Message::Text(msg)).is_err() {
                break;
            }
        }
    });

    loop {
        let Ok(msg) = ws.read() else { break };
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(reply) = dispatch(&v, &mut conn) {
            if conn.reply.send(reply.to_string()).is_err() {
                break;
            }
        }
        if conn.closed {
            break;
        }
    }

    // Drop `conn` (its own reply clone) and the guard (deregisters, and
    // drops its clone too) before joining: `reply_rx.recv()` above only
    // returns once every `Sender` clone for this connection is gone, and
    // joining first would deadlock waiting for a thread that is itself
    // waiting on us — the same ordering `wsconn.rs`'s `handle` uses.
    drop(conn);
    drop(guard);
    let _ = writer.join();
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

    // --- Task 5: the per-project registry ---
    //
    // `REGISTRY` is process-global, so a project name shared across two
    // tests hands them the same listener and their pass/fail depends on
    // which one runs first — CLAUDE.md records exactly this shape of flake.
    // Every test below therefore gets a project name unique to it.

    /// Each test gets its own lock directory, so tests that run concurrently
    /// cannot see each other's lock files. Deliberately not an env var:
    /// `CLAUDE_CONFIG_DIR` is process-global, and CLAUDE.md records a
    /// "~1-in-8 flake" that was one test reaping another's state.
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn one_listener_per_project_not_one_per_connection() {
        let (dir, ws, _) = dirs();
        let a = for_project_in(dir.path(), "reuse-alpha", ws.path().to_path_buf()).unwrap();
        let again = for_project_in(dir.path(), "reuse-alpha", ws.path().to_path_buf()).unwrap();
        assert_eq!(a.port, again.port, "a second call must reuse the listener");
    }

    #[test]
    fn two_projects_get_two_ports_and_two_lock_files() {
        let (dir, wa, wb) = dirs();
        // One listener listing both roots would let a claude in beta be
        // handed alpha's socket: the CLI takes the first workspaceFolder that
        // contains its cwd.
        let a = for_project_in(dir.path(), "twoports-alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "twoports-beta", wb.path().to_path_buf()).unwrap();
        assert_ne!(a.port, b.port);
        assert!(dir.path().join(format!("{}.lock", a.port)).exists());
        assert!(dir.path().join(format!("{}.lock", b.port)).exists());
    }

    #[test]
    fn stopping_a_project_removes_its_lock_and_leaves_the_others() {
        let (dir, wa, wb) = dirs();
        let a = for_project_in(dir.path(), "stop-alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "stop-beta", wb.path().to_path_buf()).unwrap();
        let (a_port, b_port) = (a.port, b.port);
        // `for_project_in` hands back its own `Arc` clone, separate from the
        // one the registry map holds; `stop` can only drop the map's clone.
        // Production code (`hub.rs`) never keeps one of these around past
        // the initial call, so dropping it here is what makes this test
        // match how `stop` is actually used, rather than asserting against
        // a reference this test itself is still pinning alive.
        drop(a);
        drop(b);
        stop("stop-alpha");
        assert!(!dir.path().join(format!("{a_port}.lock")).exists());
        assert!(
            dir.path().join(format!("{b_port}.lock")).exists(),
            "closing one project must not unregister another"
        );
    }

    #[test]
    fn a_failure_to_write_the_lock_file_does_not_fail_the_project() {
        let (dir, ws, _) = dirs();
        // IDE integration is a convenience. A read-only home directory must
        // degrade it, never stop a project opening.
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "").unwrap();
        assert!(for_project_in(&blocked, "lockfail-alpha", ws.path().to_path_buf()).is_none());
    }

    #[test]
    // Task 3's review: dropping the `Arc<Ide>` removed the lock file but
    // left the accept loop running, authenticating against a token nothing
    // advertises any more. `stop` must actually terminate the listener, not
    // just forget about it.
    //
    // Revert-checked: emptying `request_shutdown`'s body (so `stop` still
    // removes the lock file and the registry entry, but never signals the
    // accept thread) failed this test — `panicked ... "port {port} is still
    // accepting connections after stop()"`, after genuinely waiting out the
    // 5s poll deadline rather than passing vacuously — then restored.
    fn stop_actually_closes_the_listening_socket() {
        let (dir, ws, _) = dirs();
        let ide = for_project_in(dir.path(), "stopclose-alpha", ws.path().to_path_buf()).unwrap();
        let port = ide.port;
        drop(ide); // the registry still holds its own Arc; this must not matter
        stop("stopclose-alpha");
        // The accept thread only observes the shutdown once it loops back
        // around `incoming()` and drops the listener — not synchronously
        // inside `stop` — so poll rather than assert immediately.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Err(_) => return, // refused: the listener is gone
                Ok(_) if std::time::Instant::now() >= deadline => {
                    panic!("port {port} is still accepting connections after stop()")
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

    // --- Task 6: at_mentioned ---
    //
    // `CONNS` is process-global like `REGISTRY`, so every test below (and
    // every fake client it connects) gets a project name unique to it — the
    // same discipline Task 5's tests already established.

    /// Test-only: the send half of each fake client's socket, so a test can
    /// speak as Claude without owning the socket itself.
    static SENDERS: OnceLock<Mutex<HashMap<String, std::sync::mpsc::Sender<String>>>> =
        OnceLock::new();

    fn senders() -> &'static Mutex<HashMap<String, std::sync::mpsc::Sender<String>>> {
        SENDERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Connects a client the way the CLI does and returns (frames it
    /// receives, the live Ide). Holding the returned `Arc<Ide>` matters: it
    /// owns the lock file, and dropping it unregisters the project.
    fn connected_fake_client_for(project: &str) -> (std::sync::mpsc::Receiver<String>, Arc<Ide>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = for_project_in(dir.path(), project, ws.path().to_path_buf()).unwrap();
        // `IntoClientRequest for http::Request<()>` is a pass-through: it adds
        // nothing. `generate_request` requires Host, Connection, Upgrade,
        // Sec-WebSocket-Version and Sec-WebSocket-Key to be present already and
        // errors without the key (tungstenite 0.24 handshake/client.rs:111-135),
        // so a hand-built request must carry all five itself.
        let req = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{}/", ide.port))
            .header("Host", format!("127.0.0.1:{}", ide.port))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
            .header("Sec-WebSocket-Protocol", "mcp")
            .header("X-Claude-Code-Ide-Authorization", &ide.token)
            .body(())
            .unwrap()
            .into_client_request()
            .unwrap();
        let (mut sock, _) = tungstenite::connect(req).expect("the fake client must connect");
        let (tx, rx) = std::sync::mpsc::channel();
        // The test drives sends through this channel too, so one thread owns
        // the socket and the test never races itself.
        let (send_tx, send_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || loop {
            while let Ok(out) = send_rx.try_recv() {
                if sock.send(tungstenite::Message::Text(out)).is_err() {
                    return;
                }
            }
            match sock.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    if tx.send(t).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        });
        senders().lock().unwrap().insert(project.to_string(), send_tx);
        (rx, ide, dir)
    }

    /// Revert-checked: adding `"id": 1` to `mention`'s constructed message
    /// (turning the notification into something shaped like a request)
    /// failed only this test, on the `id.is_none()` assertion — the actual
    /// panic: `"a notification must carry no id or the CLI will wait for a
    /// reply it never gets"`. Then restored.
    #[test]
    fn a_mention_is_the_notification_the_cli_expects() {
        let (rx, ide, _d) = connected_fake_client_for("mention-shape");
        mention("mention-shape", Path::new("/w/src/hub.rs"), Some((12, 40))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["method"], "at_mentioned");
        assert_eq!(v["params"]["filePath"], "/w/src/hub.rs");
        assert_eq!(v["params"]["lineStart"], 12);
        assert_eq!(v["params"]["lineEnd"], 40);
        assert!(v.get("id").is_none(), "a notification must carry no id or the CLI will wait for a reply it never gets");
        let _ = ide;
    }

    /// Revert-checked: changing `mention` to always set `lineStart`/`lineEnd`
    /// (defaulting to `(0, 0)` when `lines` is `None`) failed only this test —
    /// `assertion failed: v["params"].get("lineStart").is_none()` — then
    /// restored. Without this test a whole-file mention would silently start
    /// carrying a bogus line range the CLI would render as a real selection.
    #[test]
    fn a_whole_file_mention_carries_no_line_numbers() {
        let (rx, _ide, _d) = connected_fake_client_for("mention-wholefile");
        mention("mention-wholefile", Path::new("/w/README.md"), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert!(v["params"].get("lineStart").is_none());
    }

    /// Revert-checked: disabling `mention`'s `targets.is_empty()` check (so
    /// an empty target list falls through to `Ok(())` instead of refusing)
    /// failed this test — `called Result::unwrap_err() on an Ok value: ()` —
    /// and, for the same reason, also failed the sibling
    /// `a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude`
    /// test below, which exercises the same empty-target path from the other
    /// direction (a connection that *was* registered and then deregistered,
    /// rather than one that was never registered at all). Both are
    /// legitimate hits on the one break, not evidence of a false positive —
    /// see CLAUDE.md's note on the `ide_connected` notification test for the
    /// same pattern. Then restored.
    #[test]
    fn mentioning_with_no_claude_connected_is_an_error_not_a_panic() {
        // The tree's keybinding is always available; Claude is not. This must
        // surface as a refusal the UI can show, never as a socket-thread panic.
        let err = mention("nobody-here", Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("no Claude"), "the message must say what is missing: {err}");
    }

    /// Revert-checked (Task 6's brief, Step 6): changing `for t in &targets`
    /// to send only to `targets.first()` failed only this test, and only on
    /// `rx2` — `rx1.recv_timeout` still succeeded (it got the first sender),
    /// `rx2.recv_timeout` timed out. With a single subscriber "send to all"
    /// and "send to the first" are indistinguishable, which is exactly the
    /// trap CLAUDE.md records; that is why this test uses two clients. Then
    /// restored.
    #[test]
    fn a_mention_reaches_every_connected_claude_not_just_the_first() {
        // Two terminals, two claudes, one project. Sending to only the first
        // is indistinguishable from sending to all with a single subscriber —
        // which is exactly the trap CLAUDE.md records.
        let (rx1, _a, _d1) = connected_fake_client_for("mention-fanout");
        let (rx2, _b, _d2) = connected_fake_client_for("mention-fanout");
        mention("mention-fanout", Path::new("/w/x.rs"), None).unwrap();
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }

    /// A dead connection (its read loop already broke, panicked or not) must
    /// not leave a phantom target in `CONNS` — `mention` would then report
    /// success into a void nothing will ever deliver. Exercises `ConnGuard`
    /// directly rather than tearing down a real socket, so the assertion is
    /// about the registry's own bookkeeping, not about socket teardown timing.
    ///
    /// Revert-checked: emptying `ConnGuard::drop`'s body (so registering still
    /// happens but deregistering does not) failed this test — `mention`
    /// returned `Ok(())` instead of the expected `Err`, because the dead
    /// sender was still counted as a live target. Then restored.
    #[test]
    fn a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude() {
        let project = "mention-guard-drop";
        let (tx, _rx) = std::sync::mpsc::channel::<String>();
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut map = conns().lock().unwrap();
            map.entry(project.to_string()).or_default().push((id, tx));
        }
        let guard = ConnGuard { project: project.to_string(), id };
        // Sanity: the registration really did take effect before the drop
        // this test is actually about.
        assert!(mention(project, Path::new("/w/x.rs"), None).is_ok());
        drop(guard);
        let err = mention(project, Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("no Claude"), "the registry entry must be gone once the guard drops: {err}");
    }
}
