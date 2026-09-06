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
