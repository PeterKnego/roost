//! Notices: per-project delivery, replay on connect, and terminal escape
//! sequences surfacing as notices.

mod common;
use common::*;

/// Notices used to go to every project's clients. They no longer do: a
/// browser tab is opened on one project key, so a notice from elsewhere has
/// no tab there to focus, no honest place in a panel headed "for this
/// project", and — the behaviour that prompted the change — nothing to do on
/// a click but navigate the tab away from the project its user is working in.
///
/// Two connected clients on two projects, deliberately: with one, scoped and
/// unscoped delivery are indistinguishable. Alpha's assertion carries the
/// "still delivered" half — without it, a `publish` that reached nobody at
/// all would satisfy beta's silence just as well — and it is read *first*,
/// so beta's frame would already be queued behind it if the broadcast had
/// gone wide. Revert-checked: with `publish` restored to `broadcast_all`,
/// beta receives alpha's Notice frame and the assertion below fires.
#[test]
fn a_notice_reaches_only_its_own_projects_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("alpha")).unwrap();
    std::fs::create_dir_all(d.path().join("beta")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let mut a = ws_connect_path(port, "/ws/alpha/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/beta/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    read_until(&mut b, r#""t":"State""#);

    roost::hub::publish(
        "alpha",
        "claude",
        roost::osc::Parsed { title: Some("build".into()), body: "green".into() },
    );

    let seen_a = read_until(&mut a, r#""t":"Notice""#);
    assert!(seen_a.contains(r#""project":"alpha""#), "alpha got: {seen_a}");
    assert!(seen_a.contains("green"), "alpha got: {seen_a}");

    let leaked = read_none(&mut b, r#""t":"Notice""#, std::time::Duration::from_millis(500));
    assert!(
        leaked.is_none(),
        "beta's tab must not be sent alpha's notice, got: {leaked:?}"
    );
}

#[test]
fn notices_are_replayed_on_connect_and_read_state_mirrors() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    roost::hub::publish(
        "proj",
        "claude",
        roost::osc::Parsed { title: None, body: "waiting for you".into() },
    );

    // A client connecting *after* the fact still learns about it.
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let replay = read_until(&mut a, r#""t":"Notices""#);
    assert!(replay.contains("waiting for you"), "connect replay missing it: {replay}");
    let id: u64 = {
        // The notice store is process-global across the whole integration
        // binary and never reset between tests, so the *first* "id": in the
        // replay can belong to a notice some other test left behind, not
        // this one — grabbing that id would still happen to make this test
        // fail if mark_read broke (any id works for that), but it would not
        // be testing the notice this test actually published. Anchor on the
        // matched body instead: `id` is the first field on `Notice` (see
        // proto.rs's struct field order), so the nearest `"id":` preceding
        // this specific body belongs to this specific notice.
        let key = r#""id":"#;
        let body_pos = replay.find("waiting for you").expect("body missing from replay");
        let start = replay[..body_pos].rfind(key).expect("no id preceding the matched body") + key.len();
        replay[start..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
    };

    // Read state is global: b marks read, a must be told.
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    read_until(&mut b, r#""t":"Notices""#);
    b.send(tungstenite::Message::Text(format!(r#"{{"t":"MarkNoticeRead","id":{id}}}"#))).unwrap();
    let after = read_until(&mut a, r#""read":true"#);
    assert!(after.contains(r#""t":"Notices""#), "a was not re-sent the list: {after}");
}

#[test]
fn an_escape_sequence_from_a_terminal_becomes_a_notice() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());

    // A single-token command: ROOST_CMD splits on whitespace.
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("emit.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '\\033]777;notify;Build done;42 tests passed\\007'\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("ROOST_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("notifyproj");
    let mut ctrl = ws_connect_path(port, "/ws/notifyproj/_workspace").unwrap();
    read_until(&mut ctrl, r#""t":"State""#);
    // Attaching the terminal socket is what spawns the session and its pump.
    let mut term = ws_connect_term(port, "/ws/notifyproj/term/claude").unwrap();

    let seen = read_until(&mut ctrl, r#""t":"Notice""#);
    assert!(seen.contains("Build done"), "title missing: {seen}");
    assert!(seen.contains("42 tests passed"), "body missing: {seen}");
    // Attribution comes from the pump's own identity, not from the payload.
    assert!(seen.contains(r#""session":"claude""#), "session missing: {seen}");
    assert!(seen.contains(r#""project":"notifyproj""#), "project missing: {seen}");

    let _ = term.close(None);
    std::env::remove_var("ROOST_CMD");
}

#[test]
fn a_terminal_child_can_discover_that_notifications_exist() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("env.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"NOTIFY=$ROOST_NOTIFY PROJ=$ROOST_PROJECT SESS=$ROOST_SESSION\"\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("ROOST_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("envproj");
    let mut term = ws_connect_term(port, "/ws/envproj/term/envprobe").unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("NOTIFY=") {
            break;
        }
    }
    assert!(seen.contains("NOTIFY=1"), "capability flag missing: {seen:?}");
    assert!(seen.contains("PROJ=envproj"), "project missing: {seen:?}");
    assert!(seen.contains("SESS=envprobe"), "session missing: {seen:?}");
    let _ = term.close(None);
    std::env::remove_var("ROOST_CMD");
}
