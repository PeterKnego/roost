//! The broker between `roost mcp` and the user's real browser.
//!
//! Claude Code shows the model only two of the IDE socket's tools (see the
//! spec, `docs/superpowers/specs/2026-10-12-browser-bridge-design.md`), so
//! browser tools come from a separate stdio server, `roost mcp`, which
//! reaches this process over `state/hook/browser.sock`. A call is parked here
//! and relayed to one roost tab whose page has the roost extension's content
//! script; the tab's answer comes back as a `BridgeReply` intent.
//!
//! The extension never connects to roost itself. Riding an already
//! Origin-checked workspace socket is what keeps roost at two authentication
//! rules — an extension socket would send `Origin: chrome-extension://…`,
//! which fits neither (#117). The cost is that a roost tab must be open, and
//! a call with none fails at once instead of waiting.
//!
//! The extension, not this module, decides whether a call is allowed: the
//! consent prompt, the grants and unsafe mode all live in extension storage,
//! where script in a roost page cannot reach them.
use crate::proto::Event;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Parked calls per *asking* project, like `ide::MAX_PENDING`: one Claude in
/// a loop must not make every other project's calls bounce.
pub const MAX_PENDING: usize = 16;
/// The extension gives a human 120 s to answer its consent prompt, and the
/// action itself then gets the rest. `roost mcp` waits a little longer than
/// this, so the server always answers before the client gives up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(180);
/// `chrome_type`'s text is the only large argument a request carries.
const MAX_REQUEST: usize = 256 * 1024;
/// Total budget for reading one request. `roost mcp` writes its line at once;
/// a peer that trickles is not one of ours.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Connection threads alive at once, process-wide, as on `hook.sock`.
const MAX_CONNECTIONS: usize = 16;
/// A project label is shown to a person in the consent window, and nothing
/// else is done with it; bounded so it cannot be a wall of text.
const MAX_LABEL: usize = 256;

pub const NO_RELAY: &str =
    "no roost tab with the roost browser extension is open — open roost in Chrome with the extension installed";
const RELAY_CLOSED: &str = "the roost tab relaying to your browser closed before it answered";

/// One workspace connection whose page has the extension's content script.
struct Relay {
    project: String,
    conn: String,
    /// Larger is more recent. A counter, not a clock, so two hellos in the
    /// same millisecond still order.
    active: u64,
}

fn relays() -> &'static Mutex<Vec<Relay>> {
    static R: OnceLock<Mutex<Vec<Relay>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

static NEXT_ACTIVE: AtomicU64 = AtomicU64::new(1);

/// A page's content script answered (or its window took focus). Registers
/// the connection as a relay, or makes it the most recent one. `active ==
/// false` registers without promoting — a background tab reloading must not
/// steal calls from the tab the user is looking at.
pub fn hello(project: &str, conn: &str, active: bool) {
    let mut r = relays().lock().unwrap_or_else(|e| e.into_inner());
    let n = NEXT_ACTIVE.fetch_add(1, Ordering::Relaxed);
    match r.iter_mut().find(|x| x.project == project && x.conn == conn) {
        Some(x) if active => x.active = n,
        Some(_) => {}
        None => r.push(Relay { project: project.into(), conn: conn.into(), active: if active { n } else { 0 } }),
    }
}

/// The workspace connection closed. Every call it was relaying fails now,
/// with a reason, rather than at the timeout three minutes later.
pub fn gone(project: &str, conn: &str) {
    relays().lock().unwrap_or_else(|e| e.into_inner()).retain(|x| !(x.project == project && x.conn == conn));
    // Separate lock scope: `relays` and `calls` are never held together.
    let orphaned: Vec<Parked> = {
        let mut c = calls().lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> =
            c.iter().filter(|(_, p)| p.relay_project == project && p.relay_conn == conn).map(|(k, _)| k.clone()).collect();
        ids.into_iter().filter_map(|k| c.remove(&k)).collect()
    };
    for p in orphaned {
        let _ = p.reply.send(Msg::Done(Err(RELAY_CLOSED.into())));
    }
}

/// Relays, most recent first, copied out so no lock is held while sending.
fn relay_order() -> Vec<(String, String)> {
    let r = relays().lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<&Relay> = r.iter().collect();
    v.sort_by(|a, b| b.active.cmp(&a.active));
    v.into_iter().map(|x| (x.project.clone(), x.conn.clone())).collect()
}

struct Parked {
    asker: String,
    relay_project: String,
    relay_conn: String,
    reply: Sender<Msg>,
}

fn calls() -> &'static Mutex<HashMap<String, Parked>> {
    static C: OnceLock<Mutex<HashMap<String, Parked>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A process-wide counter: an id travels to a browser and back, and nothing
/// the caller sent should be able to choose it.
static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

/// A browser tool call, as `roost mcp` sends it.
#[derive(Debug, Clone)]
pub struct Call {
    pub project: String,
    pub session: String,
    pub pid: u32,
    pub tool: String,
    pub args: serde_json::Value,
}

/// How a call reaches one connection: `hub::send_to_conn` in production, a
/// recording closure in tests. `false` means that connection is gone.
pub type SendFn<'a> = &'a dyn Fn(&str, &str, &Event) -> bool;

/// How long a relay tab has to say it received a call. The content script
/// acknowledges before it even asks the worker, so this measures only "is
/// that page alive": a tab navigated away into the back/forward cache, or
/// frozen in the background, keeps its socket open and answers nothing, and
/// without this a call sent there waited the full three minutes.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// What a parked call's channel carries.
enum Msg {
    /// The relay tab's content script has the call.
    Ack,
    Done(Result<serde_json::Value, String>),
}

/// Relays `call` to the most recent relay tab and waits for its answer.
/// Blocks the calling socket thread, never a lock.
///
/// `key` is passed in rather than loaded here so tests never touch the real
/// state directory; `serve_request` loads it.
pub fn call_blocking(call: Call, key: &str, timeout: Duration, send: SendFn) -> Result<serde_json::Value, String> {
    call_blocking_with(call, key, timeout, ACK_TIMEOUT, send)
}

fn call_blocking_with(
    call: Call,
    key: &str,
    timeout: Duration,
    ack_timeout: Duration,
    send: SendFn,
) -> Result<serde_json::Value, String> {
    if !crate::session::valid_name(&call.session) {
        return Err(format!("invalid terminal name {:?}", call.session));
    }
    if call.project.is_empty() || call.project.len() > MAX_LABEL || call.project.chars().any(char::is_control) {
        return Err("invalid project".into());
    }
    let id = NEXT_CALL.fetch_add(1, Ordering::Relaxed).to_string();
    let payload = serde_json::json!({
        "call": id,
        "project": call.project,
        "session": call.session,
        "pid": call.pid,
        "tool": call.tool,
        "args": call.args,
        "issued": now_millis(),
    })
    .to_string();
    let sig = sign(key, &payload);
    let deadline = Instant::now() + timeout;
    // Kept for the whole call, so the channel never reads as disconnected:
    // every way a parked call ends sends on it explicitly.
    let (tx, rx) = channel();
    let mut tried = 0;
    for (rp, rc) in relay_order() {
        {
            let mut c = calls().lock().unwrap_or_else(|e| e.into_inner());
            if c.values().filter(|p| p.asker == call.project).count() >= MAX_PENDING {
                return Err(format!("too many browser calls waiting in {}", call.project));
            }
            c.insert(
                id.clone(),
                Parked { asker: call.project.clone(), relay_project: rp.clone(), relay_conn: rc.clone(), reply: tx.clone() },
            );
        }
        tried += 1;
        let ev = Event::BridgeCall { call: id.clone(), payload: payload.clone(), sig: sig.clone() };
        if !send(&rp, &rc, &ev) {
            // That connection is gone without its teardown having run yet;
            // unpark and try the next one rather than wait on nobody.
            unpark(&id);
            gone(&rp, &rc);
            continue;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(ack_timeout.min(left)) {
            Ok(Msg::Ack) => return wait_done(&id, &rx, deadline, timeout),
            // An answer is also proof of life (a page from before acks).
            Ok(Msg::Done(r)) => return r,
            Err(_) => {
                if unpark(&id) {
                    // Alive on the server, silent in the browser: drop it so
                    // the next call does not pick it either, and try the next.
                    gone(&rp, &rc);
                    continue;
                }
                // An answer or a teardown removed it first and owns the
                // reply, which is on its way.
                return wait_done(&id, &rx, deadline, timeout);
            }
        }
    }
    Err(if tried == 0 {
        NO_RELAY.into()
    } else {
        "no roost tab relaying to your browser responded — reload roost in Chrome".into()
    })
}

fn unpark(id: &str) -> bool {
    calls().lock().unwrap_or_else(|e| e.into_inner()).remove(id).is_some()
}

fn wait_done(
    id: &str,
    rx: &std::sync::mpsc::Receiver<Msg>,
    deadline: Instant,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Ack) => continue,
            Ok(Msg::Done(r)) => return r,
            Err(RecvTimeoutError::Timeout) if unpark(id) => {
                return Err(format!("your browser did not answer within {} s", timeout.as_secs()))
            }
            // Removed by an answer or a teardown between the timeout and the
            // lock: that side owns the reply and always sends it.
            Err(RecvTimeoutError::Timeout) => loop {
                if let Ok(Msg::Done(r)) = rx.recv() {
                    return r;
                }
            },
            Err(RecvTimeoutError::Disconnected) => return Err(RELAY_CLOSED.into()),
        }
    }
}

/// The relay tab's content script has the call. Not removed: the answer is
/// still to come. Ignored from any connection the call was not sent to.
pub fn ack(project: &str, conn: &str, call: &str) {
    let c = calls().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = c.get(call) {
        if p.relay_project == project && p.relay_conn == conn {
            let _ = p.reply.send(Msg::Ack);
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn key_path() -> std::path::PathBuf {
    crate::hooksock::socket_dir().join("browser.key")
}

/// The key the extension verifies every call with, created on first use.
///
/// It is what stops script in a roost page from inventing calls: that page
/// relays every `BridgeCall` but never sees this file, which is read only
/// here and by `roost browser-key`, and never served over HTTP. It lives in
/// the same 0700 directory as the sockets.
///
/// Read on every call rather than cached, so deleting the file to rotate the
/// key takes effect without a restart. Three outcomes, not two: a file that
/// cannot be read is an error, never a reason to write a new key over it —
/// that would silently unpair every browser.
pub fn load_or_create_key() -> Result<String, String> {
    key_in(&crate::hooksock::socket_dir())
}

fn key_in(dir: &std::path::Path) -> Result<String, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = dir.join("browser.key");
    let valid = |t: &str| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit());
    match std::fs::read_to_string(&path) {
        Ok(t) if valid(t.trim()) => return Ok(t.trim().to_string()),
        Ok(_) => return Err(format!("{} is not a roost browser key; delete it to make a new one", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    crate::hooksock::verify_socket_dir(dir)?;
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .map_err(|e| format!("no CSPRNG: {e}"))?;
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    // Written whole under a pid-unique name, then linked into place: a link
    // fails if the name exists, so two creators racing cannot replace each
    // other's key, and a reader never sees half a file.
    let tmp = dir.join(format!(".browser.key.{}.tmp", std::process::id()));
    let r = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(hex.as_bytes()))
        .and_then(|_| std::fs::hard_link(&tmp, &path));
    let _ = std::fs::remove_file(&tmp);
    match r {
        Ok(()) => Ok(hex),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => key_in(dir),
        Err(e) => Err(format!("cannot write {}: {e}", path.display())),
    }
}

/// HMAC-SHA256 of `payload`, hex. The key material is the key's hex text
/// itself, which is what the extension imports too — one less encoding for
/// the two sides to disagree on.
pub fn sign(key: &str, payload: &str) -> String {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.as_bytes());
    ring::hmac::sign(&k, payload.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// A relay tab's answer. Accepted only from the connection the call was sent
/// to: any other page on any project holding a call id must not be able to
/// answer for the browser that was asked.
pub fn deliver(project: &str, conn: &str, call: &str, result: Result<serde_json::Value, String>) -> Result<(), String> {
    let p = {
        let mut c = calls().lock().unwrap_or_else(|e| e.into_inner());
        match c.get(call) {
            Some(p) if p.relay_project == project && p.relay_conn == conn => c.remove(call).ok_or("just matched")?,
            Some(_) => return Err("that browser call was not sent to this tab".into()),
            None => return Err("no such browser call — it was answered, timed out or withdrawn".into()),
        }
    };
    p.reply.send(Msg::Done(result)).map_err(|_| "the asking Claude went away".to_string())
}

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    project: String,
    session: String,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    since: u64,
    #[serde(default)]
    wait: u64,
    #[serde(default)]
    text: String,
}

/// Everything a request can ask for, minus the socket.
fn serve_request(req: Request, send: SendFn) -> Result<serde_json::Value, String> {
    match req.op.as_str() {
        "chrome" => call_blocking(
            Call { project: req.project, session: req.session, pid: req.pid, tool: req.tool, args: req.args },
            // A key that cannot be loaded means a call the extension could
            // not verify, so this fails here rather than in the browser.
            &load_or_create_key()?,
            CALL_TIMEOUT,
            send,
        ),
        "handoff_read" => {
            let me = format!("claude:{}", req.session);
            let msgs = crate::handoff::wait_since(&req.project, req.since, &me, Duration::from_secs(req.wait));
            Ok(serde_json::json!({ "messages": msgs }))
        }
        "handoff_send" => {
            if !crate::session::valid_name(&req.session) {
                return Err(format!("invalid terminal name {:?}", req.session));
            }
            let m = crate::handoff::post(&req.project, &format!("claude:{}", req.session), &req.text)?;
            crate::hub::broadcast_to_project(&req.project, &Event::Handoff { messages: vec![m.clone()], full: false });
            Ok(serde_json::json!({ "id": m.id }))
        }
        other => Err(format!("unknown op {other:?}")),
    }
}

/// Reads one line within [`REQUEST_DEADLINE`] and [`MAX_REQUEST`] — a total
/// deadline across reads, the same reason `hooksock` gives for not trusting
/// a per-read timeout.
fn read_line(mut s: &UnixStream) -> Result<String, String> {
    let started = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let left = REQUEST_DEADLINE.checked_sub(started.elapsed()).ok_or("request timed out")?;
        let _ = s.set_read_timeout(Some(left.min(Duration::from_secs(1)).max(Duration::from_millis(1))));
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.iter().position(|&c| c == b'\n') {
                    buf.truncate(pos);
                    break;
                }
                if buf.len() > MAX_REQUEST {
                    return Err("request too large".into());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    if buf.len() > MAX_REQUEST {
        return Err("request too large".into());
    }
    String::from_utf8(buf).map_err(|_| "request is not UTF-8".to_string())
}

fn write_reply(mut s: &UnixStream, r: Result<serde_json::Value, String>) {
    let v = match r {
        Ok(result) => serde_json::json!({ "ok": true, "result": result }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let mut line = v.to_string();
    line.push('\n');
    let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = s.write_all(line.as_bytes());
}

fn serve_conn(s: UnixStream, send: SendFn) {
    let r = read_line(&s).and_then(|line| {
        if line.trim().is_empty() {
            return Err("no request".into());
        }
        let req: Request = serde_json::from_str(&line).map_err(|e| format!("unreadable request: {e}"))?;
        serve_request(req, send)
    });
    write_reply(&s, r);
}

fn production_send(project: &str, conn: &str, ev: &Event) -> bool {
    crate::hub::send_to_conn(project, conn, ev)
}

/// Binds `browser.sock` and spawns its accept thread. Never fatal: without it
/// only the browser tools are missing.
pub fn start() -> Result<(), String> {
    let l = crate::hooksock::bind_private("browser.sock")?;
    std::thread::Builder::new()
        .name("browsersock".into())
        .spawn(move || serve_listener(l, MAX_CONNECTIONS, &production_send))
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn socket_path() -> std::path::PathBuf {
    crate::hooksock::socket_dir().join("browser.sock")
}

fn serve_listener(l: UnixListener, cap: usize, send: &'static (dyn Fn(&str, &str, &Event) -> bool + Sync)) {
    // Constructed on the accept thread and moved into the closure, so a
    // failed spawn still frees its slot — `hooksock::serve_incoming` has the
    // whole story.
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    // SAFETY: getuid takes no arguments, reads no memory, and cannot fail.
    let me = unsafe { libc::getuid() };
    let in_flight = Arc::new(AtomicUsize::new(0));
    for s in l.incoming() {
        let Ok(s) = s else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if crate::hooksock::peer_uid(&s) != Some(me) {
            continue;
        }
        if in_flight.load(Ordering::SeqCst) >= cap {
            write_reply(&s, Err("busy".into()));
            continue;
        }
        in_flight.fetch_add(1, Ordering::SeqCst);
        let guard = Guard(in_flight.clone());
        let _ = std::thread::Builder::new().name("browsersock-conn".into()).spawn(move || {
            let _guard = guard;
            // Fed bytes by a Claude-spawned process: no panic may escape, and
            // the peer still gets an answer if one does.
            let s2 = s.try_clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_conn(s, send)));
            if r.is_err() {
                if let Ok(s2) = s2 {
                    write_reply(&s2, Err("roost failed while handling the call".into()));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test uses its own project names; the registries are process-wide
    /// and tests run in parallel.
    /// The relay registry is process-wide and "most recent" is a global
    /// order, so every test that registers or routes holds this — otherwise
    /// a parallel test's hello becomes the relay this one's call goes to.
    static SERIAL: Mutex<()> = Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    const K: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn call(project: &str) -> Call {
        Call { project: project.into(), session: "term".into(), pid: 42, tool: "chrome_tabs".into(), args: serde_json::json!({}) }
    }

    fn call_id(ev: &Event) -> String {
        match ev {
            Event::BridgeCall { call, .. } => call.clone(),
            other => panic!("not a BridgeCall: {other:?}"),
        }
    }

    /// Two relays, so "sent to the most recent one" and "sent to all" differ.
    /// Revert-checked: sorting `relay_order` ascending sends to `old` and
    /// fails on the first assertion.
    #[test]
    fn a_call_goes_to_the_most_recent_relay_only_and_its_answer_comes_back() {
        let _g = serial();
        hello("br-route", "old", true);
        hello("br-route-other", "new", true);
        let sent: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let s2 = sent.clone();
        let t = std::thread::spawn(move || {
            let send = move |p: &str, c: &str, ev: &Event| {
                s2.lock().unwrap().push((p.into(), c.into(), call_id(ev)));
                true
            };
            call_blocking(call("br-route-asker"), K, Duration::from_secs(5), &send)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let (p, c, id) = loop {
            if let Some(x) = sent.lock().unwrap().first().cloned() {
                break x;
            }
            assert!(Instant::now() < deadline, "nothing was sent");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((p.as_str(), c.as_str()), ("br-route-other", "new"));
        assert_eq!(sent.lock().unwrap().len(), 1, "sent to one relay, not every one");
        let wrong = deliver("br-route", "old", &id, Ok(serde_json::json!("forged"))).unwrap_err();
        assert!(wrong.contains("not sent to this tab"), "{wrong}");
        deliver("br-route-other", "new", &id, Ok(serde_json::json!({"tabs": []}))).unwrap();
        assert_eq!(t.join().unwrap().unwrap(), serde_json::json!({"tabs": []}));
        gone("br-route", "old");
        gone("br-route-other", "new");
    }

    /// Revert-checked: with `gone`'s orphan sweep removed, the caller waits
    /// its full timeout and fails on the elapsed bound.
    #[test]
    fn a_relay_closing_fails_its_calls_at_once() {
        let _g = serial();
        hello("br-gone", "c1", true);
        let start = Instant::now();
        let t = std::thread::spawn(|| call_blocking(call("br-gone"), K, Duration::from_secs(10), &|_, _, _| true));
        std::thread::sleep(Duration::from_millis(100));
        gone("br-gone", "c1");
        let e = t.join().unwrap().unwrap_err();
        assert!(e.contains("closed"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_relay_that_cannot_be_reached_is_skipped_for_the_next() {
        let _g = serial();
        hello("br-skip-live", "live", true);
        hello("br-skip-dead", "dead", true);
        let tried: Arc<Mutex<Vec<String>>> = Arc::default();
        let tr = tried.clone();
        let t = std::thread::spawn(move || {
            let send = move |p: &str, _: &str, ev: &Event| {
                tr.lock().unwrap().push(p.to_string());
                if p == "br-skip-live" {
                    let id = call_id(ev);
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(50));
                        deliver("br-skip-live", "live", &id, Ok(serde_json::json!(1))).unwrap();
                    });
                    true
                } else {
                    false
                }
            };
            call_blocking(call("br-skip"), K, Duration::from_secs(5), &send)
        });
        assert_eq!(t.join().unwrap().unwrap(), serde_json::json!(1));
        assert_eq!(tried.lock().unwrap().first().map(String::as_str), Some("br-skip-dead"));
        assert!(!relay_order().iter().any(|(p, _)| p == "br-skip-dead"), "the dead relay was dropped");
        gone("br-skip-live", "live");
    }

    /// A relay that takes the call and never acknowledges it — a tab in the
    /// back/forward cache — is dropped and the next one asked. Revert-checked:
    /// waiting for the answer directly instead of the ack hangs on `silent`
    /// until the call's own timeout and fails on the elapsed bound.
    #[test]
    fn a_relay_that_never_acknowledges_is_dropped_for_the_next() {
        let _g = serial();
        hello("br-ack-live", "live", true);
        hello("br-ack-silent", "silent", true);
        let start = Instant::now();
        let t = std::thread::spawn(move || {
            let send = move |p: &str, _: &str, ev: &Event| {
                if p == "br-ack-live" {
                    let id = call_id(ev);
                    std::thread::spawn(move || {
                        ack("br-ack-live", "live", &id);
                        deliver("br-ack-live", "live", &id, Ok(serde_json::json!("from live"))).unwrap();
                    });
                }
                true
            };
            call_blocking_with(call("br-ack"), K, Duration::from_secs(10), Duration::from_millis(200), &send)
        });
        assert_eq!(t.join().unwrap().unwrap(), serde_json::json!("from live"));
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        assert!(!relay_order().iter().any(|(p, _)| p == "br-ack-silent"), "the silent relay was dropped");
        gone("br-ack-live", "live");
    }

    #[test]
    fn no_relay_is_an_immediate_refusal_that_names_the_fix() {
        let _g = serial();
        // The send closure refuses every relay another test may have left,
        // so this is "none reachable", which is the same answer.
        let e = call_blocking(call("br-none"), K, Duration::from_secs(5), &|_, _, _| false).unwrap_err();
        assert!(e.contains("extension"), "{e}");
    }

    #[test]
    fn the_asking_project_is_capped() {
        let _g = serial();
        hello("br-cap", "c", true);
        let mut ts = Vec::new();
        for _ in 0..MAX_PENDING {
            ts.push(std::thread::spawn(|| {
                call_blocking(call("br-cap-asker"), K, Duration::from_secs(10), &|p, _, _| p == "br-cap")
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls().lock().unwrap().values().filter(|p| p.asker == "br-cap-asker").count() < MAX_PENDING {
            assert!(Instant::now() < deadline, "the calls never parked");
            std::thread::sleep(Duration::from_millis(10));
        }
        let e = call_blocking(call("br-cap-asker"), K, Duration::from_secs(1), &|p, _, _| p == "br-cap").unwrap_err();
        assert!(e.contains("too many"), "{e}");
        gone("br-cap", "c");
        for t in ts {
            assert!(t.join().unwrap().is_err());
        }
    }

    #[test]
    fn a_bad_terminal_name_is_refused_before_anything_is_sent() {
        let mut c = call("br-name");
        c.session = "../x".into();
        let e = call_blocking(c, K, Duration::from_secs(1), &|_, _, _| panic!("must not send")).unwrap_err();
        assert!(e.contains("invalid terminal name"), "{e}");
    }

    #[test]
    fn a_background_hello_registers_without_taking_over() {
        let _g = serial();
        hello("br-bg-front", "f", true);
        hello("br-bg-back", "b", false);
        let order = relay_order();
        let f = order.iter().position(|(p, _)| p == "br-bg-front").unwrap();
        let b = order.iter().position(|(p, _)| p == "br-bg-back").unwrap();
        assert!(f < b, "{order:?}");
        gone("br-bg-front", "f");
        gone("br-bg-back", "b");
    }

    /// The page relays the payload text untouched, so the signature must be
    /// over exactly that text and the text must name who is asking — the
    /// extension shows those fields in its consent window.
    #[test]
    fn a_relayed_call_is_signed_over_its_payload() {
        let _g = serial();
        hello("br-sig", "c", true);
        let got: Arc<Mutex<Option<Event>>> = Arc::default();
        let g2 = got.clone();
        let t = std::thread::spawn(move || {
            let send = move |_: &str, _: &str, ev: &Event| {
                *g2.lock().unwrap() = Some(ev.clone());
                true
            };
            call_blocking(call("br-sig-asker"), K, Duration::from_millis(300), &send)
        });
        // Nobody answers; only what was sent matters here.
        assert!(t.join().unwrap().is_err());
        let Some(Event::BridgeCall { payload, sig, .. }) = got.lock().unwrap().clone() else { panic!("nothing sent") };
        assert_eq!(sig, sign(K, &payload));
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!((v["project"].as_str(), v["session"].as_str(), v["pid"].as_u64()), (Some("br-sig-asker"), Some("term"), Some(42)));
        assert!(v["issued"].as_u64().unwrap() > 1_600_000_000_000, "issued is unix milliseconds");
        gone("br-sig", "c");
    }

    /// RFC 4231 test case 2, so the signature is HMAC-SHA256 and not merely
    /// some stable hash — the extension's WebCrypto verify has to agree.
    #[test]
    fn sign_is_standard_hmac_sha256() {
        assert_eq!(
            sign("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn a_key_is_created_once_and_then_reused() {
        let d = tempfile_dir("key-once");
        let a = key_in(&d).unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(key_in(&d).unwrap(), a, "a second call reads the same key");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.join("browser.key")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// The destructive direction: a key file that is there but wrong must
    /// never be replaced, or every paired browser is silently unpaired.
    #[test]
    fn a_damaged_key_is_refused_not_replaced() {
        let d = tempfile_dir("key-bad");
        std::fs::write(d.join("browser.key"), "not a key").unwrap();
        let e = key_in(&d).unwrap_err();
        assert!(e.contains("not a roost browser key"), "{e}");
        assert_eq!(std::fs::read_to_string(d.join("browser.key")).unwrap(), "not a key");
    }

    fn tempfile_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("roost-browser-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
        d
    }
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn the_socket_answers_one_request_with_one_line() {
        let (a, b) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_conn(b, &|_, _, _| false));
        (&a).write_all(b"{\"op\":\"nope\",\"project\":\"p\",\"session\":\"s\"}\n").unwrap();
        let mut out = String::new();
        (&a).read_to_string(&mut out).unwrap();
        t.join().unwrap();
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("unknown op"), "{v}");
    }
}
