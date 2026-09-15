//! Terminal sessions: multiple clients on one session, proposals shown to
//! late joiners, tab naming, Claude auto-typing, and dtach master cleanup.

mod common;
use common::*;

#[test]
fn two_terminal_clients_mirror_one_session() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_term(port, "/ws/proj/term/shell").unwrap();
    let mut b = ws_connect_term(port, "/ws/proj/term/shell").unwrap();
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
    std::env::remove_var("ROOST_STATE_DIR");
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
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    // The first connection is what builds the hub this proposal is opened on.
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    roost::hub::open_proposal("proj", "late-1", "hello.md", "what is there", "what claude wants");

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
    roost::hub::close_proposal("proj", "late-1");
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
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
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("proposal-order");
    let mut a = ws_connect_path(port, "/ws/proposal-order/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    roost::hub::open_proposal(
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

    roost::hub::close_proposal("proposal-order", "order-1");
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

/// The + button sends no name at all now, so allocation is entirely the
/// server's. Driven over the real socket because the client half — dropping
/// the `prompt()` — is not reachable from a unit test.
#[test]
fn new_terminal_names_itself_and_ending_one_clears_only_its_own_tab() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
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

    std::env::remove_var("ROOST_STATE_DIR");
}

/// `TerminalStarted` tells a browser "attach to this session now", and the
/// browser only obeys for a session it has a tab for (app.js gates it, so a
/// mirroring tab never opens a PTY socket for a terminal it does not show).
/// For `NewTerminal` the tab is born in the same handler — so the snapshot
/// that carries it has to reach the browser *first*, or every browser,
/// including the one that clicked, drops the event and lands on the "press
/// Enter" placeholder. That was the + button's behaviour before this test:
/// one click for the tab, a keypress for the shell.
#[test]
fn a_new_terminals_started_event_follows_the_snapshot_that_carries_its_tab() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("newtermorder");
    let mut a = ws_connect_path(port, "/ws/newtermorder/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    a.send(tungstenite::Message::Text(r#"{"t":"NewTerminal","pane":3}"#.into())).unwrap();
    // Both frames mention the new name; collect them in arrival order.
    let mut order = Vec::new();
    for _ in 0..10 {
        let msg = read_until(&mut a, r#""session":"term1""#);
        if msg.contains(r#""t":"TerminalStarted""#) {
            order.push("started");
            break;
        }
        if msg.contains(r#""t":"State""#) {
            order.push("state");
        }
    }
    assert_eq!(
        order,
        vec!["state", "started"],
        "the browser must hold the tab before it is told to attach to it"
    );
    std::env::remove_var("ROOST_STATE_DIR");
}

/// The ✻ button end to end: the intent names no session, the server allocates
/// one, and the program is typed into that session's PTY by the socket that
/// spawns it — not by the hub, which has no PTY yet, and not by the client,
/// which a mirroring browser could race. `ROOST_CMD=cat` makes the shell echo
/// its input, so the keystrokes come back out as proof they went in.
#[test]
fn a_claude_terminal_has_claude_typed_into_it_once_its_shell_exists() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("claudeterm");
    let mut a = ws_connect_path(port, "/ws/claudeterm/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    a.send(tungstenite::Message::Text(r#"{"t":"NewTerminal","pane":3,"launch":"claude"}"#.into())).unwrap();
    // A ✻ click is handed `claude`, not the next free `termN`: the name says
    // what the terminal is for, so the tab strip reports which one has an
    // agent in it. `default_layout`'s seeded `term` is untouched.
    read_until(&mut a, r#""session":"claude""#);

    let mut t = ws_connect_term(port, "/ws/claudeterm/term/claude").unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match t.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        // `keystrokes` now types the minted session id along with the
        // program (Task 3), so the PTY sees `claude --session-id <uuid>`
        // rather than the bare `claude` this test used to look for.
        if seen.contains("claude --session-id ") && seen.contains('\r') {
            break;
        }
    }
    assert!(seen.contains("claude --session-id "), "claude + Enter must reach the PTY; got: {seen:?}");
    let id = seen
        .split("claude --session-id ")
        .nth(1)
        .and_then(|rest| rest.split(['\r', '\n']).next())
        .unwrap_or("");
    assert!(roost::launch::valid_session_id(id), "the typed id must be a valid session id; got: {id:?}");

    // The plain + button on the same project stays a plain shell: nothing is
    // typed into it, so nothing comes back out.
    // `term1`, not `term2`: the ✻ above took `claude`, so the plain + is
    // handed the next free name in its own sequence rather than in a single
    // shared one. `term` is `default_layout`'s seeded tab.
    a.send(tungstenite::Message::Text(r#"{"t":"NewTerminal","pane":3}"#.into())).unwrap();
    read_until(&mut a, r#""session":"term1""#);
    let mut p = ws_connect_term(port, "/ws/claudeterm/term/term1").unwrap();
    p.send(tungstenite::Message::Binary(b"marker\r".to_vec())).unwrap();
    let mut plain = String::new();
    for _ in 0..100 {
        match p.read() {
            Ok(tungstenite::Message::Binary(b)) => plain.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if plain.contains("marker") {
            break;
        }
    }
    // `marker` arriving proves the socket is live and echoing, so the absence
    // of `claude` before it is absence, not a socket that never answered.
    assert!(plain.contains("marker"), "the plain terminal must echo; got: {plain:?}");
    assert!(!plain.contains("claude"), "+ must not type claude; got: {plain:?}");

    let _ = t.close(None);
    let _ = p.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
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
    std::env::set_var("ROOST_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("sseportproj");
    // No `_workspace` connection anywhere above this line: this project's
    // hub, and so its ide listener, has never been touched before.
    let mut term = ws_connect_term(port, "/ws/sseportproj/term/sseprobe").unwrap();
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
    std::env::remove_var("ROOST_CMD");
    // Best-effort cleanup: this writes a real lock file into the shared
    // test ide directory (`isolate_ide_dir_for_tests`), and nothing else
    // ever closes "sseportproj"'s project to remove it. Not load-bearing —
    // the whole directory is a `TempDir` that removes itself when this test
    // binary exits — but tidy, and it matches the same rule
    // `idelock::Lock`'s own `Drop` follows: `remove_file`, never a
    // directory scan.
    if let Some(p) = ide_port {
        let _ = std::fs::remove_file(roost::idelock::ide_dir().join(format!("{p}.lock")));
    }
}

/// A child that declines the hangup must not survive `kill_and_unlink`.
///
/// This is the measured defect (2026-09-04): `kill -9` on the dtach master
/// closes the pty, the kernel `SIGHUP`s the slave side, the foreground process
/// and a plain background child die — and a `trap "" HUP` child does not. It
/// reparents to init and keeps the project directory as its cwd, while the
/// socket goes unheld and roost reports the session ended.
///
/// Deliberately a real `dtach`, not `ROOST_CMD=cat`: with `cat` there is no
/// master, no pty and no hangup, so the whole mechanism is absent and the test
/// would pass against the unfixed code.
///
/// Revert-checked: with the session sweep removed (delete the
/// `kill_sessions(proc_root, &targets);` calls, replace `session_or_socket_alive(...)`
/// with `socket_has_process_with(sock_path, snapshot_fn)` in `kill_and_unlink_with`)
/// this fails — test panicked with "the HUP-ignoring child survived kill_and_unlink".
/// Restored with `cp` from a pre-edit backup.
#[test]
fn a_hup_ignoring_child_does_not_survive_kill_and_unlink() {
    if std::process::Command::new("dtach").arg("-h").output().is_err() {
        eprintln!("skipping: dtach not installed");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("term");
    // A unique name so the assertion cannot match another test's process, and
    // so a survivor is identifiable in `ps` if this fails.
    let marker = format!("roost_hup_survivor_{}", std::process::id());
    let script = d.path().join("inner.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nbash -c 'trap \"\" HUP; exec -a {marker} sleep 600' &\nexec sleep 600\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ok = std::process::Command::new("dtach")
        .args(["-n", sock.to_str().unwrap(), "-E", "-r", "winch", "-z"])
        .arg(&script)
        .status()
        .unwrap()
        .success();
    assert!(ok, "dtach -n must create the session this test is about");
    std::thread::sleep(std::time::Duration::from_millis(800));

    let alive = |m: &str| {
        let out = std::process::Command::new("ps").args(["-Ao", "args="]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(m))
    };
    // Asserts the setup state it later negates: without this the test would
    // pass just as well if the child had never started.
    assert!(alive(&marker), "the HUP-ignoring child must be running before the kill");

    let confirmed = roost::registry::kill_and_unlink(&sock);
    std::thread::sleep(std::time::Duration::from_millis(500));

    assert!(!alive(&marker), "the HUP-ignoring child survived kill_and_unlink");
    assert!(confirmed, "and the session must be reported as confirmed ended");
    assert!(!sock.exists(), "and its socket unlinked");
}

/// The restart the sidecar exists for, driven end to end against a real
/// `dtach` master this process never spawned.
///
/// Deliberately not `ROOST_CMD=cat`: with `cat` there is no master and no
/// socket, so `attach` never reaches the `Decision::Probe` branch — the one
/// whose doc says "this process never attached, but the dtach master is alive
/// and `dtach -A` will rejoin it". That branch *is* the restart, and it is the
/// only path on which a restored mode table can be observed. A `cat`
/// substitution would make this test pass against completely unfixed code,
/// which is the dev/prod trap CLAUDE.md records four instances of.
///
/// Revert-checked: deleting the `sc.restore(&crate::modes::load(project, name))`
/// line in `session::attach`'s `Screens` construction makes this fail with
/// "the restored contract never arrived".
#[test]
fn a_session_that_outlived_roost_gets_its_mode_contract_back() {
    // `ROOST_CMD` and `ROOST_STATE_DIR` are process-global and this file's
    // other tests set them; without the lock a `remove_var` here can strip
    // `ROOST_CMD=cat` out from under a test running on another thread, which
    // then spawns a real login shell and fails its echo assertion. That is
    // the flake class CLAUDE.md records — one test's state reaching into
    // another's — and `tests/common/mod.rs` spells out the same reason.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::process::Command::new("dtach").arg("-h").output().is_err() {
        eprintln!("skipping: dtach not installed");
        return;
    }
    std::env::remove_var("ROOST_CMD");
    roost::wsstate::set_state_dir_for_test();

    let d = tempfile::tempdir().unwrap();
    // A rel key, not a path: `valid_project` rejects an absolute one.
    let project = format!("modeproj{}", std::process::id() % 100_000);
    let name = "restarted".to_string();
    let sock = roost::session::socket_path(&project, &name);
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&sock);

    // A master roost did not spawn — exactly what a restart leaves behind.
    let ok = std::process::Command::new("dtach")
        .args(["-n", sock.to_str().unwrap(), "-E", "-r", "winch", "-z", "sleep", "600"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "dtach -n must create the session this test is about");
    assert!(common::wait_for_path(&sock), "and its socket must appear");

    // What the previous roost recorded before it went away.
    roost::modes::save(&project, &name, &[(2004, true), (1000, true)]);
    assert!(
        roost::modes::path_for(&project, &name).exists(),
        "the sidecar must exist, or this test proves nothing about reading it"
    );

    let att = roost::session::attach(&project, &name, d.path()).expect("attach must rejoin");
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match att.rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(chunk) => {
                seen.extend_from_slice(&chunk);
                if seen.windows(8).any(|w| w == b"\x1b[?2004h") {
                    break;
                }
            }
            Err(_) => {}
        }
    }
    let _ = roost::registry::kill_and_unlink(&sock);

    assert!(
        seen.windows(8).any(|w| w == b"\x1b[?2004h"),
        "the restored contract never arrived — a paste here submits its first line. Got {:?}",
        String::from_utf8_lossy(&seen)
    );
    assert!(
        seen.windows(8).any(|w| w == b"\x1b[?1000h"),
        "and mouse reporting with it: {:?}",
        String::from_utf8_lossy(&seen)
    );
    // The refusal, on the same path: nothing may put this client on the
    // alternate screen, because no live app declared it to *this* process.
    assert!(
        !seen.windows(8).any(|w| w == b"\x1b[?1049h"),
        "the screen bit must never be restored: {:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// The writing half, against a real shell: a mode an app declares must reach
/// the sidecar without anyone asking it to.
///
/// Revert-checked: removing the `modes::save` call from the pump makes this
/// fail with "the pump never recorded the contract".
#[test]
fn the_pump_records_a_declared_mode_where_the_next_roost_will_find_it() {
    // `ROOST_CMD` and `ROOST_STATE_DIR` are process-global and this file's
    // other tests set them; without the lock a `remove_var` here can strip
    // `ROOST_CMD=cat` out from under a test running on another thread, which
    // then spawns a real login shell and fails its echo assertion. That is
    // the flake class CLAUDE.md records — one test's state reaching into
    // another's — and `tests/common/mod.rs` spells out the same reason.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::process::Command::new("dtach").arg("-h").output().is_err() {
        eprintln!("skipping: dtach not installed");
        return;
    }
    std::env::remove_var("ROOST_CMD");
    roost::wsstate::set_state_dir_for_test();

    let d = tempfile::tempdir().unwrap();
    let project = format!("modewrite{}", std::process::id() % 100_000);
    let name = "declaring".to_string();
    let sock = roost::session::socket_path(&project, &name);
    let sidecar = roost::modes::path_for(&project, &name);
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&sidecar);
    let _ = std::fs::remove_file(&sock);

    // Created with `dtach -n` rather than by `attach`, because session
    // *creation* is reservation-gated (see the `Decision` enum) and a bare
    // `attach` is refused. Rejoining an existing master is the path this
    // needs anyway, and it is a real shell so the mode really is declared by
    // a process rather than injected into the ring by the test.
    let ok = std::process::Command::new("dtach")
        .args(["-n", sock.to_str().unwrap(), "-E", "-r", "winch", "-z", "sh"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "dtach -n must create the session this test is about");
    assert!(common::wait_for_path(&sock), "and its socket must appear");

    let att = roost::session::attach(&project, &name, d.path()).expect("attach must rejoin");

    // Mouse reporting is the marker, not bracketed paste, and the difference
    // matters. `/bin/sh` is bash on several distros, and bash 4.4+ emits
    // `?2004h` at every prompt — so on those hosts the sidecar already holds
    // `2004 1` before this test types anything, the main assertion is
    // satisfied by the shell rather than by the `printf` under test, and it
    // passes while proving nothing. No shell declares mouse reporting.
    //
    // The pre-state is cleared rather than asserted absent, for the same
    // reason: the shell may legitimately have written one already.
    let _ = std::fs::remove_file(&sidecar);

    // The app declares its contract, the way Claude does at startup.
    roost::session::write_input(&att.key, b"printf '\\033[?1000h\\033[?1006h'\n")
        .expect("the shell must accept input");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut body = String::new();
    while std::time::Instant::now() < deadline {
        if let Ok(t) = std::fs::read_to_string(&sidecar) {
            if t.contains("1000 1") {
                body = t;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = roost::registry::kill_and_unlink(&sock);

    assert!(body.contains("1000 1"), "the pump never recorded the contract; got {body:?}");
    assert!(body.contains("1006 1"), "and the coordinate encoding with it; got {body:?}");
    // 25 is tracked in memory and deliberately never written: it is the one
    // mode a repainting app changes constantly, so persisting it would put a
    // disk write on every frame for a value that repairs itself in a second.
    assert!(!body.contains("25 "), "cursor visibility must not be written; got {body:?}");
}

/// A brand-new session must never be handed a dead app's mode contract.
///
/// The sidecar routinely outlives its session: when the user types `exit`,
/// **dtach unlinks its own socket**, so neither `kill_and_unlink` nor
/// `reconcile`'s dead-socket arm runs and the file is orphaned. Verified on
/// this host — `dtach -n <sock> … bash -c 'exit 0'` leaves no socket behind.
/// `next_free_name` then hands the same name out again, and the restore was
/// gated on `spawned`, which is true for a genuinely new session too.
///
/// What that cost the user: run Claude in `term`, exit it, exit the shell,
/// click + — the fresh `bash` was replayed the dead Claude's table, so mouse
/// reporting was asserted at a shell prompt and every click typed
/// `\x1b[<0;12;5M` junk while the wheel stopped scrolling.
///
/// Revert-checked: dropping the `if rejoining` guard in `session::attach`
/// fails this with the stale table replayed into the new session.
#[test]
fn a_new_session_does_not_inherit_an_orphaned_mode_contract() {
    if std::process::Command::new("dtach").arg("-h").output().is_err() {
        eprintln!("skipping: dtach not installed");
        return;
    }
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("ROOST_CMD");
    roost::wsstate::set_state_dir_for_test();

    let d = tempfile::tempdir().unwrap();
    let project = format!("orphan{}", std::process::id() % 100_000);
    let name = "reused".to_string();
    let sock = roost::session::socket_path(&project, &name);
    let sidecar = roost::modes::path_for(&project, &name);
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&sock);

    // Exactly the state a shell that exited on its own leaves: a contract on
    // disk and no socket at all.
    // Mouse reporting, not bracketed paste. `save` refuses when the socket is
    // absent (it must not resurrect a sidecar for an ended session), so the
    // orphan is written directly — which is what dtach's own unlink leaves
    // behind.
    //
    // 2004 deliberately is *not* asserted on below: bash emits `?2004h` at
    // its own prompt, so it appears in this session's output whether or not
    // anything was restored, and an assertion on it fails against correct
    // code. The first draft of this test did exactly that. No shell declares
    // mouse reporting, so 1000 is the only byte here that can only have come
    // from a restore.
    std::fs::write(&sidecar, b"1000 1\n1006 1\n").unwrap();
    assert!(sidecar.exists(), "setup: an orphaned contract is on disk");
    assert!(!sock.exists(), "setup: and its session is gone");

    // A new session, same name. Reserved first, because creation is
    // reservation-gated — this is the `Decision::Spawn` path, which is
    // exactly the one that must not restore.
    roost::session::reserve(&project, &name, None);
    let att = roost::session::attach(&project, &name, d.path()).expect("a new session spawns");

    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match att.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(chunk) => seen.extend_from_slice(&chunk),
            Err(_) => {}
        }
    }
    let _ = roost::registry::kill_and_unlink(&sock);

    let has = |needle: &[u8]| seen.windows(needle.len()).any(|w| w == needle);
    assert!(
        !has(b"\x1b[?1000h"),
        "a fresh shell must not be told to report mouse events: {:?}",
        String::from_utf8_lossy(&seen)
    );
    assert!(
        !has(b"\x1b[?1006h"),
        "nor its coordinate encoding: {:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// The orphan itself is collected, so the sidecars do not accumulate for the
/// life of the state dir — and so an emptied key directory can be removed.
#[test]
fn an_orphaned_sidecar_is_swept_but_a_live_one_is_not() {
    let d = tempfile::tempdir().unwrap();
    let key = d.path().join("proj");
    std::fs::create_dir_all(&key).unwrap();

    // A live session: socket present.
    std::fs::write(key.join("live"), b"").unwrap();
    std::fs::write(key.join(".modes.live"), b"2004 1\n").unwrap();
    // An orphan: contract, no socket.
    std::fs::write(key.join(".modes.dead"), b"2004 1\n").unwrap();
    // An interrupted write.
    std::fs::write(key.join(".modes.dead.tmp.999"), b"2004 1\n").unwrap();

    roost::modes::sweep_orphans(&key);

    assert!(key.join(".modes.live").exists(), "a held session keeps its contract");
    assert!(!key.join(".modes.dead").exists(), "an orphan is collected");
    assert!(!key.join(".modes.dead.tmp.999").exists(), "and so is an interrupted write");
    assert!(key.join("live").exists(), "the socket itself is untouched");
}
