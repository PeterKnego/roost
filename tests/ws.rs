//! Websocket handshake and framing behavior common to terminal and
//! workspace sockets: Origin checks, pings, close frames, and malformed
//! input.

mod common;
use common::*;

#[test]
fn ws_rejects_foreign_and_missing_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
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

/// The reader half of these sockets can no longer write, so the `Pong` that
/// tungstenite queues on it is discarded — the read loop has to forward the
/// reply through the one writer instead. If that forwarding is dropped, the
/// structural fix silently turns roost into a server that never answers a
/// ping, and nothing else in the suite notices.
///
/// Revert-checked: deleting the `Ok(Message::Ping(p))` arm in `term.rs` makes
/// this fail on the 5s read timeout with "no Pong came back"; deleting the
/// same arm in `wsconn.rs` fails its sibling below the same way.
/// The discriminating test for the fix itself, as opposed to for the gate
/// type or for pings still working.
///
/// Before this fix, the reader answered a `Ping` itself, from its own
/// `WebSocket` over a second descriptor — that reply is what could splice
/// into a frame the writer thread was part-way through. The fix mutes the
/// reader and forwards the reply through the one writer. If the muting is
/// removed but the forwarding kept, *both* halves answer and the peer gets
/// **two** Pongs for one Ping — which is exactly the observable signature of
/// two writers on this socket.
///
/// So: exactly one Pong means exactly one writer.
///
/// Revert-checked: deleting `gate.close()` from `wsconn.rs` fails this with
/// "2 Pongs came back for one Ping", and the sibling tests all stay green —
/// they cannot see the difference.
#[test]
fn one_ping_gets_exactly_one_pong_because_only_one_half_can_write() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    ws.send(tungstenite::Message::Ping(b"once".to_vec().into())).unwrap();

    // Read for a fixed window rather than stopping at the first Pong: the
    // whole point is to catch a *second* one, so stopping early would make
    // this pass against the very bug it exists for.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    let mut pongs = 0usize;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
    }
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Pong(p)) => {
                assert_eq!(p.to_vec(), b"once".to_vec(), "a Pong must echo the Ping's payload");
                pongs += 1;
            }
            Ok(_) => continue,
            // The read timeout expiring is the loop's clock, not a failure.
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    assert_eq!(
        pongs, 1,
        "{pongs} Pongs came back for one Ping — more than one means both the \
         reader and the writer answered, which is the two-writer bug itself"
    );
}

/// The terminal socket's half of the test above — `term.rs` has its own
/// `gate.close()`, and a test that only covered `wsconn.rs` would leave the
/// socket that matters more (its writer is pumping PTY output continuously,
/// so it is nearly always mid-frame) unguarded.
///
/// Revert-checked: deleting `gate.close()` from `term.rs` fails this with
/// "2 Pongs came back for one Ping".
#[test]
fn one_ping_to_a_terminal_gets_exactly_one_pong() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    ws.send(tungstenite::Message::Ping(b"once".to_vec().into())).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    let mut pongs = 0usize;
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Pong(p)) => {
                assert_eq!(p.to_vec(), b"once".to_vec(), "a Pong must echo the Ping's payload");
                pongs += 1;
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    assert_eq!(
        pongs, 1,
        "{pongs} Pongs came back for one Ping — more than one means both the \
         reader and the writer answered, which is the two-writer bug itself"
    );
}

#[test]
fn a_terminal_socket_still_answers_a_ping_after_the_reader_stops_writing() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    ws.send(tungstenite::Message::Ping(b"marco".to_vec().into())).unwrap();
    // The socket also carries PTY output and the server's own pings, so read
    // past whatever else arrives rather than assuming the Pong is first.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Pong(p)) => {
                got = Some(p.to_vec());
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert_eq!(
        got.as_deref(),
        Some(&b"marco"[..]),
        "no Pong came back: the reader cannot write and nothing forwarded the reply"
    );
}

#[test]
fn a_workspace_socket_still_answers_a_ping_after_the_reader_stops_writing() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    ws.send(tungstenite::Message::Ping(b"polo".to_vec().into())).unwrap();
    // State and Notices arrive first on this socket, so the Pong is never
    // the first frame.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Pong(p)) => {
                got = Some(p.to_vec());
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert_eq!(
        got.as_deref(),
        Some(&b"polo"[..]),
        "no Pong came back: the reader cannot write and nothing forwarded the reply"
    );
}

#[test]
fn terminal_ws_echoes_through_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
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
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "true"); // exits immediately
    let (_d, port) = fixture();
    // Own session name: the process-global registry may already hold a live
    // "proj/shell" session from another test in this binary (e.g.
    // terminal_ws_echoes_through_pty's `cat`), in which case ROOST_CMD
    // would never be consulted for a fresh spawn and this test would prove
    // nothing about a child exiting first.
    let mut ws = ws_connect_term(port, "/ws/proj/term/exiter").unwrap();
    // child exited at spawn; the server must close/shutdown the socket rather than hang
    assert_ws_closes(&mut ws, "ws_closes_when_child_exits_first");
}

#[test]
fn child_exit_delivers_a_close_frame_not_a_bare_eof() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    std::env::set_var("ROOST_CMD", "true"); // exits immediately
    let (_d, port) = fixture_named("closeproj");
    let mut ws = ws_connect_term(port, "/ws/closeproj/term/exiter").unwrap();

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
                std::env::remove_var("ROOST_STATE_DIR");
                return;
            }
            Ok(m) => saw.push(format!("{m:?}")),
            Err(e) => {
                std::env::remove_var("ROOST_STATE_DIR");
                panic!(
                    "child exit must close the socket with a Close frame, not {e:?}; \
                     frames seen first: {saw:?}"
                );
            }
        }
    }
    std::env::remove_var("ROOST_STATE_DIR");
    panic!("no Close frame within the read budget; frames seen: {saw:?}");
}

#[test]
fn an_idle_terminal_socket_is_pinged() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    std::env::set_var("ROOST_CMD", "cat"); // reads stdin, writes nothing unprompted
    std::env::set_var("ROOST_PING_SECS", "1");
    let (_d, port) = fixture_named("pingterm");
    let mut ws = ws_connect_term(port, "/ws/pingterm/term/idle").unwrap();
    // Nothing is sent from either side after the handshake. Without the
    // ping this socket would sit silent forever, which is exactly how a
    // dead peer's attachment goes on holding a `sizes` entry — and the PTY
    // takes the *minimum* geometry across attachments, so a stale one
    // clamps the terminal for every live client.
    expect_ping(&mut ws, "idle terminal socket");
    std::env::remove_var("ROOST_PING_SECS");
    std::env::remove_var("ROOST_STATE_DIR");
}

#[test]
fn an_idle_workspace_socket_is_pinged() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_PING_SECS", "1");
    let (_d, port) = fixture_named("pingws");
    let mut ws = ws_connect_path(port, "/ws/pingws/_workspace").unwrap();
    // This socket matters more than the terminal one: hub::subscribe hands
    // out an *unbounded* channel, so a subscriber nobody drains accumulates
    // every broadcast in memory for as long as the process lives.
    expect_ping(&mut ws, "idle workspace socket");
    std::env::remove_var("ROOST_PING_SECS");
}

#[test]
fn invalid_session_name_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
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

/// A connect for a session that does not exist creates nothing, and says so
/// with a *clean* close.
///
/// Both halves matter and they are separate failures. If it created, a
/// terminal websocket would still be a session factory and a close would
/// still be raceable. If it refused but the Close frame never reached the
/// browser, `app.js` would read 1006 as a dead connection and reconnect on a
/// backoff — re-entering the same door until one attempt landed somewhere it
/// was allowed, which is the shape of the bug this whole change is about.
///
/// Revert-checked: dropping the `NotReserved` rule so `attach` creates again
/// fails the first assertion, naming the session it should not have made.
///
/// The flush in `term.rs` after `close(None)` is **not** pinned by this test —
/// removing it leaves this green. It is kept as defensive correctness (a Close
/// that is only enqueued may never be written), but nothing here demonstrates
/// that it matters, and this comment says so rather than implying a proof the
/// revert did not produce.
#[test]
fn an_unreserved_terminal_connect_creates_nothing_and_closes_cleanly() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = fixture();

    // Deliberately `ws_connect_path`, not `ws_connect_term`: the point is a
    // connect with no reservation behind it.
    let mut ws = ws_connect_path(port, "/ws/proj/term/nosuchsession").unwrap();
    let mut closed = false;
    let mut read_err = None;
    for _ in 0..20 {
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) => {
                closed = true;
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                read_err = Some(e.to_string());
                break;
            }
        }
    }
    // Creation is asserted first, deliberately. When `attach` creates again
    // the socket simply stays open, so the loop above ends on a read timeout
    // — and asserting the close frame first reported that as "no Close
    // frame", which diagnoses the wrong half. Checked by reverting: with the
    // `NotReserved` rule removed this now fails here, naming the session it
    // should not have made.
    assert!(
        !roost::session::live_names("proj").iter().any(|n| n == "nosuchsession"),
        "an unreserved connect must not create the session it was refused: {:?}",
        roost::session::live_names("proj")
    );
    assert!(closed, "the refusal must arrive as a Close frame; read ended with {read_err:?}");
    std::env::remove_var("ROOST_CMD");
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
    std::env::set_var("ROOST_STATE_DIR", sd.path());
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
    std::env::remove_var("ROOST_STATE_DIR");
}

/// Reads until the Close frame and hands it back, so a test can assert on
/// *why* the socket closed rather than only that it did. A bare EOF or a read
/// error is a failure here, not a close: `assert_ws_closes` accepts those, and
/// that leniency is exactly what let a reasonless refusal pass as "closed".
fn read_close_frame(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Option<tungstenite::protocol::CloseFrame<'static>> {
    let mut saw = Vec::new();
    for _ in 0..50 {
        match ws.read() {
            Ok(tungstenite::Message::Close(f)) => return f,
            Ok(m) => saw.push(format!("{m:?}")),
            Err(e) => panic!("expected a Close frame, read ended with {e:?}; frames first: {saw:?}"),
        }
    }
    panic!("no Close frame within the read budget; frames seen: {saw:?}");
}

/// #123: a terminal whose command is not installed says so, instead of
/// closing with nothing and reading as "session ended" — the same words the
/// tab shows after a deliberate `exit`, so nothing hinted that anything was
/// wrong, let alone what to install.
///
/// Asserts on the reason text and the code, not on the close: the refusal
/// already closed cleanly before this fix, so a close-only test was green
/// against the bug.
///
/// Revert-checked: restoring `close(None)` in `term.rs` fails this with
/// "the refusal must carry a Close frame with a reason".
#[test]
fn a_missing_command_names_itself_in_the_close_frame() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    std::env::set_var("ROOST_CMD", "roost-no-such-command-123");
    let (_d, port) = fixture_named("missingcmd");
    let mut ws = ws_connect_term(port, "/ws/missingcmd/term/nocmd").unwrap();
    let frame = read_close_frame(&mut ws);
    std::env::remove_var("ROOST_CMD");
    std::env::remove_var("ROOST_STATE_DIR");

    let frame = frame.expect("the refusal must carry a Close frame with a reason, not an empty close");
    assert_eq!(
        frame.code,
        tungstenite::protocol::frame::coding::CloseCode::Error,
        "a failed spawn is a server-side error (1011), not a normal close: {frame:?}"
    );
    assert!(
        frame.reason.contains("roost-no-such-command-123") && frame.reason.contains("not installed"),
        "the reason must name the missing command and say it is not installed: {:?}",
        frame.reason
    );
    assert!(
        !roost::session::live_names("missingcmd").iter().any(|n| n == "nocmd"),
        "a spawn that never happened must leave no session behind"
    );
}

/// A bare command name is looked up on `PATH`, and only there.
///
/// portable-pty's own lookup tries `cwd/<name>` *before* `PATH`, and a
/// terminal's cwd is inside the project — so a checkout carrying an
/// executable named `dtach` at its root had it run in place of the real one
/// the moment a terminal opened. That is repository code executing on a
/// click that promises a shell.
///
/// Watched failing before the fix: "the project's own roost-cwd-probe ran".
/// It covers only a name *absent* from PATH, where the lookup refuses before
/// any spawn — so it stays green if the spawn is handed the bare name again.
/// The test below is the one that sees that.
#[test]
fn a_command_is_not_run_from_the_project_directory() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (d, port) = fixture_named("cwdcmd");
    let marker = d.path().join("ran");
    let probe = d.path().join("cwdcmd/roost-cwd-probe");
    std::fs::write(&probe, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("ROOST_CMD", "roost-cwd-probe");
    let mut ws = ws_connect_term(port, "/ws/cwdcmd/term/probe").unwrap();
    let frame = read_close_frame(&mut ws);
    std::env::remove_var("ROOST_CMD");
    std::env::remove_var("ROOST_STATE_DIR");

    assert!(
        std::fs::symlink_metadata(&marker).is_err(),
        "the project's own roost-cwd-probe ran: a bare command must resolve on PATH only"
    );
    // And it was refused as missing, rather than failing some other way that
    // would also have left no marker.
    let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
    assert!(reason.contains("not installed"), "expected a not-installed refusal, got {reason:?}");
}

/// The case the one above cannot reach, and the real one: a command that *is*
/// installed, shadowed by a same-named executable in the project — a checkout
/// carrying its own `dtach`. There the lookup succeeds, so what protects the
/// user is that the spawn is handed the resolved absolute path; handed the
/// bare name, portable-pty tries `cwd/<name>` first and runs the project's.
///
/// `true` stands in for `dtach` because it is on every PATH this suite runs
/// under and exits at once, so the socket closes without a shell to tend.
///
/// Revert-checked: `CommandBuilder::new(&cmd[0])` in place of the resolved
/// path fails this, and only this, of the three tests here.
#[test]
fn an_installed_command_is_not_shadowed_by_one_in_the_project() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (d, port) = fixture_named("shadowcmd");
    let marker = d.path().join("ran");
    let shadow = d.path().join("shadowcmd/true");
    std::fs::write(&shadow, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("ROOST_CMD", "true");
    let mut ws = ws_connect_term(port, "/ws/shadowcmd/term/shadow").unwrap();
    assert_ws_closes(&mut ws, "an_installed_command_is_not_shadowed_by_one_in_the_project");
    std::env::remove_var("ROOST_CMD");
    std::env::remove_var("ROOST_STATE_DIR");
    assert!(
        std::fs::symlink_metadata(&marker).is_err(),
        "the project's own `true` ran in place of the one on PATH"
    );
}
