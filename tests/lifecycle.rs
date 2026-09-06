//! Project open/close lifecycle: no session spawned just from opening, and
//! closing a project ending its real sessions without touching others.

mod common;
use common::*;

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
    std::env::set_var("ROOST_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
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
    let mut term = ws_connect_term(port, "/ws/spawncheck/term/shell").unwrap();
    let live_state = wait_for_live_session(&mut ws, &my_id, "shell");
    assert!(
        live_state.contains(r#""live_sessions":["shell"]"#),
        "attaching a real terminal must make it show up live; got: {live_state}"
    );

    let _ = term.close(None);
    let _ = ws.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
    std::env::remove_var("ROOST_CMD");
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
    std::env::set_var("ROOST_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = two_project_fixture("closealpha", "closebeta");

    // Starting a terminal is what creates a session: connect its socket.
    let mut term_a1 = ws_connect_term(port, "/ws/closealpha/term/shell").unwrap();
    let mut term_a2 = ws_connect_term(port, "/ws/closealpha/term/build").unwrap();
    let mut term_b = ws_connect_term(port, "/ws/closebeta/term/shell").unwrap();

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
    std::env::remove_var("ROOST_STATE_DIR");
    std::env::remove_var("ROOST_CMD");
}

// The end-to-end reproduction of the bug this task exists to fix, over a
// real WebSocket connection exactly like a browser's: closing a project
// with `ROOST_CMD=cat` (every other close-project test, including the
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
    std::env::remove_var("ROOST_CMD");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("realclose");

    let mut term = ws_connect_term(port, "/ws/realclose/term/shell").unwrap();
    let mut ws = ws_connect_path(port, "/ws/realclose/_workspace").unwrap();
    let init = read_until(&mut ws, r#""t":"State""#);
    let my_id = extract_origin(&init);
    wait_for_live_session(&mut ws, &my_id, "shell");

    let sock =
        sd.path().join("sock").join(roost::projects::storage_key("realclose")).join("shell");
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
    std::env::remove_var("ROOST_STATE_DIR");
}

/// After a `CloseProject`, `term.rs`'s direct `ide::for_project` call is the
/// *only* thing left that can rebuild the project's IDE listener.
///
/// `ide::stop` removes the project from `ide`'s registry, but nothing ever
/// removes it from `hub::REGISTRY` — so `Hub::for_project`'s `or_insert_with`
/// closure, the only other caller of `ide::for_project`, can never run again
/// for that project. `term.rs`'s comment describes that line as a
/// first-terminal race fix, which reads like something the hub already covers;
/// deleting it as redundant would leave every reopened project with no lock
/// file, no `CLAUDE_CODE_SSE_PORT`, and a `claude` that silently comes up with
/// no IDE — while every other test in this suite stays green.
///
/// Reconnecting the `_workspace` socket after the close is the control, and it
/// is what makes this discriminating: it proves the listener is *not* coming
/// back through the hub, so the assertion that follows can only be satisfied
/// by `term.rs`. Asserting on the lock file rather than only on
/// `ide::port_for` is deliberate too — the lock file is what `claude` actually
/// scans, and a registry entry whose lock file was never rewritten would
/// advertise nothing to anyone.
///
/// Revert-checked twice, and the second one is the one that matters.
///
/// Deleting `crate::ide::for_project(&project, dir.clone());` from `term.rs`
/// outright failed this test — `reopening the project must rebuild its ide
/// listener: term.rs's ide::for_project call is the only path left that can,
/// and nothing else does`, after genuinely waiting out the full 5s poll — but
/// it also failed five *existing* integration tests, so that break was already
/// covered and says nothing about why this test needs to exist.
///
/// The break that isolates it is the actual mistake the comment in `term.rs`
/// guards against: replacing that line with
/// `crate::hub::Hub::for_project(&project, dir.clone())` — "the hub already
/// does this, so this call is redundant". A fresh project still gets its
/// listener (`or_insert_with` runs), so
/// `a_fresh_projects_first_terminal_already_carries_the_ide_port` and all 59
/// other integration tests stay green; a *reopened* one never does, and this
/// test was the only failure in the suite, with the message above. Then
/// restored.
#[test]
fn reopening_a_closed_project_rebuilds_its_ide_listener() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("idereopen");
    let ide_dir = roost::idelock::ide_dir();

    // Opening the workspace socket is what builds the hub, and the hub's
    // `or_insert_with` is what starts the listener the first time.
    let mut ws = ws_connect_path(port, "/ws/idereopen/_workspace").unwrap();
    let _ = read_until(&mut ws, r#""t":"State""#);
    let first_port = wait_for_ide_port("idereopen").expect("test setup: no ide listener was ever started");
    let first_lock = ide_dir.join(format!("{first_port}.lock"));
    assert!(first_lock.exists(), "test setup: the first listener must have written a lock file");

    ws.send(tungstenite::Message::Text(r#"{"t":"CloseProject"}"#.into())).unwrap();
    let _ = read_until(&mut ws, r#""t":"ProjectClosed""#);
    // `ide::stop` runs before `ProjectClosed` is broadcast, so this is ordered,
    // not raced.
    assert!(
        roost::ide::port_for("idereopen").is_none(),
        "CloseProject must have stopped the ide listener, or the rest of this test proves nothing"
    );
    assert!(!first_lock.exists(), "and removed exactly the lock file it wrote");

    // The control: the hub entry survived the close, so this reconnect takes
    // `for_project`'s hit path and its `or_insert_with` never runs again.
    let mut ws2 = ws_connect_path(port, "/ws/idereopen/_workspace").unwrap();
    let _ = read_until(&mut ws2, r#""t":"State""#);
    assert!(
        roost::ide::port_for("idereopen").is_none(),
        "a reconnected _workspace socket must NOT be what brings the listener back — \
         if it is, this test has stopped covering term.rs and the comment there is wrong"
    );

    // Reopening a terminal is the path that must rebuild it.
    let mut term = ws_connect_term(port, "/ws/idereopen/term/shell").unwrap();
    let second_port = wait_for_ide_port("idereopen").expect(
        "reopening the project must rebuild its ide listener: term.rs's ide::for_project \
         call is the only path left that can, and nothing else does",
    );
    let second_lock = ide_dir.join(format!("{second_port}.lock"));
    assert!(
        wait_for_path(&second_lock),
        "the rebuilt listener must advertise itself in a lock file — that file is what \
         `claude` scans, and a registry entry alone reaches nobody"
    );

    let _ = term.close(None);
    let _ = ws.close(None);
    let _ = ws2.close(None);
    roost::ide::stop("idereopen");
    std::env::remove_var("ROOST_STATE_DIR");
    std::env::remove_var("ROOST_CMD");
}
