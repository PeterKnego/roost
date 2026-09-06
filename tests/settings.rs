//! Project settings: the show-hidden header/config-file interaction and
//! writing Claude hook settings.

mod common;
use common::*;

// The header toggle has to beat the config file in *both* directions, or the
// control is one-way: a project with `show_hidden = true` in its config must
// still be able to turn dot entries off from the UI. Driven end to end — the
// intent over the websocket, the listing over HTTP — because the two reach the
// filter by different routes (hub state vs. registry peek) and a wiring that
// only worked one way would still pass a unit test of either half.
//
// Own project name: this test writes into the project directory and mutates
// hub state (see `fixture_named`).
#[test]
fn the_header_toggle_overrides_the_config_file_in_both_directions() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (d, port) = fixture_named("toggleproj");
    let proj = d.path().join("toggleproj");
    std::fs::write(proj.join(".gitignore"), "target\n").unwrap();
    let url = format!("http://127.0.0.1:{port}/frag/toggleproj/tree");
    let tree = || ureq::get(&url).call().unwrap().into_string().unwrap();

    // Config says hide (it says nothing, which is the same thing).
    assert!(!tree().contains(".gitignore"), "hidden by default");

    let mut c = ws_connect_path(port, "/ws/toggleproj/_workspace").unwrap();
    read_until(&mut c, r#""t":"State""#);
    c.send(tungstenite::Message::Text(r#"{"t":"SetShowHidden","on":true}"#.into())).unwrap();
    read_until(&mut c, r#""show_hidden":true"#);
    let shown = tree();
    assert!(shown.contains(r#"data-rel=".gitignore""#), "the toggle must beat the config file");
    assert!(shown.contains(r#"data-rel="hello.md""#), "ordinary rows are unaffected");

    // Now the other direction: config on, toggle off.
    std::fs::create_dir(proj.join(".roost")).unwrap();
    std::fs::write(proj.join(".roost/config.toml"), "show_hidden = true").unwrap();
    c.send(tungstenite::Message::Text(r#"{"t":"SetShowHidden","on":false}"#.into())).unwrap();
    read_until(&mut c, r#""show_hidden":false"#);
    let hidden = tree();
    assert!(!hidden.contains(".gitignore"), "an explicit off must beat show_hidden = true");
    assert!(hidden.contains(r#"data-rel="hello.md""#), "and must not empty the tree instead");

    let _ = c.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

// The bell's switch: an intent over the workspace socket writes the
// project's local Claude settings, and the next State reports what the
// file says — read back from disk, not echoed from memory.
//
// Own project name: this test writes into the project directory and mutates
// hub state (see `fixture_named`).
//
// Revert-checked: replacing the `snapshot_event` match with
// `ws.claude_hooks = Some(false);` panicked at
// `never saw "\"claude_hooks\":true" within the deadline` (the snapshot
// never reported the write). Restored. Replacing `set(&self.dir, *on)`
// with `Ok::<(), String>(())` in the `handle` arm panicked with the same
// `never saw "\"claude_hooks\":true" within the deadline` — the stub never
// writes the file, and `snapshot_event`'s real disk read of
// `claudehooks::state` correctly keeps reporting `false`, so the client
// never sees `true` and the test times out at that read_until rather than
// reaching the file-content assertions below it. Restored.
#[test]
fn set_claude_hooks_writes_the_local_settings_and_state_reports_the_file() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (d, port) = fixture_named("hooksproj");
    let proj = d.path().join("hooksproj");
    let file = proj.join(".claude/settings.local.json");

    let mut c = ws_connect_path(port, "/ws/hooksproj/_workspace").unwrap();
    let first = read_until(&mut c, r#""t":"State""#);
    assert!(first.contains(r#""claude_hooks":false"#), "no file yet is off: {first}");

    c.send(tungstenite::Message::Text(r#"{"t":"SetClaudeHooks","on":true}"#.into())).unwrap();
    read_until(&mut c, r#""claude_hooks":true"#);
    let text = std::fs::read_to_string(&file).expect("the file was written");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "roost claude-hook", "{text}");
    assert_eq!(v["hooks"]["Notification"][0]["hooks"][0]["command"], "roost claude-hook", "{text}");

    // Hub::claude_hooks is a cache, not a fresh read on every snapshot (see
    // its doc comment): a hand edit to the file, followed by an unrelated
    // intent that rebuilds a snapshot for its own reason, must still report
    // the cached value — only an explicit RequestState (or a fresh
    // connection) re-reads.
    //
    // Revert-checked: dropping the cache (making `snapshot_event` always
    // recompute) failed the "still true" assertion below with
    // `left: false` in the frame text instead of `true` — SetShowHidden's
    // snapshot picked up the hand-edited file immediately rather than
    // waiting for the RequestState that follows.
    let mut hand_edited: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    hand_edited["hooks"].as_object_mut().unwrap().remove("Stop");
    std::fs::write(&file, serde_json::to_string_pretty(&hand_edited).unwrap()).unwrap();
    c.send(tungstenite::Message::Text(r#"{"t":"SetShowHidden","on":true}"#.into())).unwrap();
    let still_cached = read_until(&mut c, r#""t":"State""#);
    assert!(
        still_cached.contains(r#""claude_hooks":true"#),
        "the unrelated rebuild must reuse the cached value, not the just-edited file: {still_cached}"
    );
    c.send(tungstenite::Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    read_until(&mut c, r#""claude_hooks":false"#);

    c.send(tungstenite::Message::Text(r#"{"t":"SetClaudeHooks","on":false}"#.into())).unwrap();
    read_until(&mut c, r#""claude_hooks":false"#);
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(v.get("hooks").is_none(), "{v}");

    // Unknown is reported as null and the intent is refused, file untouched.
    std::fs::write(&file, "{ broken").unwrap();
    c.send(tungstenite::Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    read_until(&mut c, r#""claude_hooks":null"#);
    c.send(tungstenite::Message::Text(r#"{"t":"SetClaudeHooks","on":true}"#.into())).unwrap();
    let err = read_until(&mut c, r#""t":"Error""#);
    assert!(err.contains("settings.local.json"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ broken");

    let _ = c.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

// The setting has to survive the whole request path — config cascade, route,
// renderer — and it is per project, so the test asserts the same server serves
// the hidden row only once that project's `.roost/config.toml` asks for it. A
// filter wired to a constant would pass one half and fail the other.
// Own project name: this test writes into the project directory (see
// `fixture_named`).
#[test]
fn show_hidden_is_read_per_project_on_every_tree_request() {
    let (d, port) = fixture_named("hiddenproj");
    let proj = d.path().join("hiddenproj");
    std::fs::write(proj.join(".gitignore"), "target\n").unwrap();
    let url = format!("http://127.0.0.1:{port}/frag/hiddenproj/tree");
    let before = ureq::get(&url).call().unwrap().into_string().unwrap();
    assert!(before.contains("data-rel=\"hello.md\""), "ordinary rows render");
    assert!(!before.contains(".gitignore"), "hidden by default");

    // Settings are re-read per request, so no restart between these two.
    std::fs::create_dir(proj.join(".roost")).unwrap();
    std::fs::write(proj.join(".roost/config.toml"), "show_hidden = true").unwrap();
    let after = ureq::get(&url).call().unwrap().into_string().unwrap();
    assert!(after.contains("data-rel=\".gitignore\""), "the setting took effect");
    assert!(after.contains("data-rel=\".roost\""), "including the config dir itself");
    // The lazy-expand endpoint reads the same setting, not a cached one.
    let lazy = ureq::get(&format!("{url}?dir=")).call().unwrap().into_string().unwrap();
    assert!(lazy.contains("data-rel=\".gitignore\""));
}
