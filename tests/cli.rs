//! Command-line entry points: `--version`, bad `--port`, and the
//! `claude-hook` subcommand.

mod common;

/// `roost claude-hook` as `main.rs` actually dispatches it — the real
/// built binary, not `cli::run_claude_hook` called in-process — asserting
/// what a Claude Code hook entry actually sees: exit 0 and empty stdout in
/// every one of the three ways it can have nothing to say. Claude Code
/// reads a hook's stdout as a decision and treats nonzero exit as a
/// failure shown in the transcript, so either one leaking here is user-
/// visible in every Claude session on the project, not just a wrong log
/// line.
///
/// Revert-checked: changing `run_claude_hook`'s first `return 0` (the
/// missing-`ROOST_NOTIFY` path) to `return 1` failed case (a)'s
/// `out.status.success()` assertion — `left: false` where a Claude
/// running outside a roost terminal must see silent success. Restored.
/// `roost --version` used to fall into the server path: an unrecognised
/// first argument parsed as the port, failed, defaulted to 8444, and either
/// started a server nobody asked for or panicked at the bind where one
/// already ran. Both halves here: the flag prints the crate version and
/// exits 0; a non-numeric port is refused with exit 2 rather than defaulted
/// past. The bad-port child is waited on with a deadline, because the old
/// behaviour on an idle host is a server that never exits — a hang, not a
/// failure, is what the deadline turns into a failure.
///
/// Revert-check (2026-09-05): with `unwrap_or(8444)` restored, the bad-port
/// half failed — on this host with exit 101 at the bind (8444 busy), and the
/// version half with the same panic. On an idle host the deadline fires.
#[test]
fn version_flag_prints_the_version_and_a_bad_port_is_refused() {
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_roost");
    for flag in ["--version", "-V"] {
        let out = Command::new(bin).arg(flag).output().expect("run roost --version");
        assert!(out.status.success(), "{flag}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("roost {}\n", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
    let roots = tempfile::tempdir().unwrap();
    let mut child = Command::new(bin)
        .arg("notaport")
        .env("ROOST_ROOTS", roots.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn roost notaport");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("`roost notaport` did not exit: it started a server instead of refusing the argument");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let out = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("notaport") && err.contains("port"), "stderr must name the bad argument: {err}");
}

#[test]
fn claude_hook_subcommand_exits_zero_and_silent_in_every_hands_off_case() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_roost");

    let run = |mut cmd: Command, stdin: &[u8]| -> std::process::Output {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn roost claude-hook");
        // The write handle is a temporary: it is dropped (closing the pipe,
        // signalling EOF) at the end of this statement, before
        // `wait_with_output` blocks on the child. The result is ignored on
        // purpose: case (a) exits without reading stdin, so a child that
        // wins the race closes the pipe first and this write sees
        // BrokenPipe — a flake, not a failure, since exit status and stdout
        // are what the assertions below check.
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().expect("wait for roost claude-hook")
    };

    // (a) No ROOST_NOTIFY: this Claude is not running in a roost terminal
    // (the project is also used outside roost), so the hook is a true
    // no-op regardless of what is on stdin.
    let mut cmd = Command::new(bin);
    cmd.arg("claude-hook").env_remove("ROOST_NOTIFY");
    let out = run(cmd, br#"{"hook_event_name":"Stop"}"#);
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");

    // (b) ROOST_NOTIFY set, stdin not JSON at all: a parse failure must not
    // surface as a hook failure either.
    let mut cmd = Command::new(bin);
    cmd.arg("claude-hook").env("ROOST_NOTIFY", "1");
    let out = run(cmd, b"not json");
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");

    // (c) ROOST_NOTIFY set and a valid Stop event, but the process has no
    // controlling terminal — the shape a subagent's hook runs in, and also
    // what this test binary itself may already be running under. `setsid`
    // gives the child its own session with no controlling terminal at all,
    // which is the case that matters. Deliberately *not* combined with
    // `process_group(0)`: util-linux `setsid` forks and exits 0 at once when
    // it is already a group leader, discarding the child's status, which
    // made the success assertion below unable to fail (verified: `setsid sh
    // -c 'exit 3'` reports 3 normally and 0 in its own process group).
    // `--wait` makes setsid report the child's status either way.
    // Verified this can fail: with `std::process::exit(1)` inserted in
    // `run_claude_hook`'s no-terminal arm, the success assertion below
    // panicked; before `--wait` replaced `process_group(0)` it stayed green.
    let mut cmd = Command::new("setsid");
    cmd.arg("--wait").arg(bin).arg("claude-hook").env("ROOST_NOTIFY", "1");
    let out = run(cmd, br#"{"hook_event_name":"Stop","last_assistant_message":"Done."}"#);
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    // A test runner with no terminal anywhere in its *ancestry* legitimately
    // hits the very same no-terminal path on its own, with no `setsid`
    // involved — so this only checks stderr's *content* when there is any,
    // not that it is empty. Which makes the branch environment-dependent:
    // it is dead on a dev box inside a roost terminal (the walk finds the
    // ancestor pty, stderr is empty) and live on CI, where nothing in the
    // ancestry has one. So it must not hold its own copy of the wording —
    // it did, and drifted, and CI alone saw it. Assert on the constant the
    // binary prints.
    if !out.stderr.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(roost::cli::NO_TERMINAL_NOTICE), "{stderr}");
    }
}

/// The wiring no unit test reaches: `roost claude-hook` really being handed a
/// payload on stdin, by the real binary, with the environment a hook inherits.
///
/// The parse and the write have their own tests in `claudesess`. What only the
/// real binary can show is the *ordering* in `cli::run_claude_hook` — the
/// record has to be taken before `hook_message`'s gate, because `SessionStart`
/// produces no notification and returns `None` there, and `SessionStart` is
/// the one event carrying the id of a session that may never finish a turn.
/// Record after the gate and every recoverable session is the one you lose.
#[test]
fn the_hook_records_a_session_start_even_though_it_notifies_nothing() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_roost");
    let state = tempfile::tempdir().expect("state dir");
    let project = "hookrec";

    let fire = |event: &str, id: &str| {
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"{id}",
                 "transcript_path":"/tmp/{id}.jsonl","cwd":"/tmp"}}"#
        );
        let mut child = Command::new(bin)
            .arg("claude-hook")
            .env("ROOST_NOTIFY", "1")
            .env("ROOST_STATE_DIR", state.path())
            .env("ROOST_PROJECT", project)
            .env("ROOST_SESSION", "term")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn roost claude-hook");
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
        let out = child.wait_with_output().expect("wait");
        // Exit 0 and nothing on stdout, always: Claude Code reads hook stdout
        // as a decision, and a non-zero exit shows an error in the transcript.
        assert!(out.status.success(), "{event}: {out:?}");
        assert!(out.stdout.is_empty(), "{event}: {out:?}");
    };

    let marker = state.path().join("claude").join(project).join("term.json");
    assert!(!marker.exists(), "setup: nothing recorded yet");

    fire("SessionStart", "aaaa1111-2222-3333-4444-555555555555");
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).expect("SessionStart was recorded")).unwrap();
    assert_eq!(v["session_id"], "aaaa1111-2222-3333-4444-555555555555");
    assert_eq!(v["event"], "SessionStart", "and it knows which event taught it that");

    // A later Stop for the same terminal replaces it, so the record follows
    // the session actually running rather than the first one ever seen.
    fire("Stop", "bbbb1111-2222-3333-4444-555555555555");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(v["session_id"], "bbbb1111-2222-3333-4444-555555555555");
    assert_eq!(v["event"], "Stop");
}

/// A hook that is not running in a roost terminal records nothing, and the
/// name it would have recorded under comes from an environment a user can set
/// by hand.
#[test]
fn a_hook_outside_a_roost_terminal_records_nothing() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_roost");
    let state = tempfile::tempdir().expect("state dir");

    let fire = |session: &str| {
        let mut child = Command::new(bin)
            .arg("claude-hook")
            .env("ROOST_NOTIFY", "1")
            .env("ROOST_STATE_DIR", state.path())
            .env("ROOST_PROJECT", "hooknone")
            .env("ROOST_SESSION", session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let _ = child.stdin.take().unwrap().write_all(
            br#"{"hook_event_name":"Stop","session_id":"cccc1111-2222","transcript_path":"/tmp/c.jsonl"}"#,
        );
        assert!(child.wait_with_output().expect("wait").status.success());
    };

    // Asserts the state it negates: a well-formed name does record, so the
    // refusals below are not a binary that records nothing at all.
    fire("term");
    assert!(
        state.path().join("claude").join("hooknone").join("term.json").exists(),
        "setup: a well-formed terminal name records"
    );

    fire("../escape");
    assert!(
        !state.path().join("claude").join("escape.json").exists(),
        "a terminal name that is not a session name writes nothing, and nothing outside the directory"
    );
}
