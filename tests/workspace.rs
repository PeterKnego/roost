//! The workspace (buffer/editor) socket: state mirroring, live external
//! edits, saving, and reconnect replay.

mod common;
use common::*;

#[test]
fn workspace_state_mirrors_between_two_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    // a's own initial snapshot carries a's connection id in `origin`; capture
    // it now so we can later prove the *mirrored* event names this same id,
    // rather than just asserting on ordering-dependent text like "hello.md".
    let a_init = read_until(&mut a, r#""t":"State""#);
    let a_id = extract_origin(&a_init);

    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    // Hub is a process-global registry keyed by project name (see hub.rs), so
    // "proj" can carry state left behind by another test in this binary.
    // Prove b's own snapshot starts clean, or a stale "hello.md" from a
    // previous test could make the assertion below pass for the wrong
    // reason — off b's own state, without exercising mirroring at all.
    let b_init = read_until(&mut b, r#""t":"State""#);
    assert!(!b_init.contains("hello.md"), "b's own snapshot must not already contain hello.md");

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();

    // The *other* browser must learn about it without asking. Read to the
    // next State frame, not to the first frame naming the file: a fresh hub
    // broadcasts the buffer's BufferText before the State that carries the
    // tab, and matching on "hello.md" caught that one. This test only ever
    // passed in the monolithic binary because an earlier test had left a hub
    // for "proj" pointing at its own deleted tempdir, so the disk read
    // failed silently and no BufferText was ever sent — a vacuous pass
    // exposed the day the suite was split into one binary per area.
    let seen = read_until(&mut b, r#""t":"State""#);
    assert!(seen.contains("hello.md"), "the mirrored State must carry the tab: {seen}");
    // ...and it must be attributed to *a*, the client that actually acted —
    // not something b could have produced from its own state.
    assert!(
        seen.contains(&format!(r#""origin":"{a_id}""#)),
        "mirrored event must carry the originating client's id ({a_id}), got: {seen}"
    );
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

#[test]
fn external_edit_updates_a_clean_buffer_live() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    std::env::set_var("ROOST_DEBOUNCE_MS", "10");
    // Its OWN project name, not the shared "proj". `Hub` is a process-global
    // registry keyed by project name, so a "proj" hub created by any earlier
    // test outlives that test's TempDir — and this test would then bind to it,
    // leaving the watcher registered on a deleted directory while the writer
    // thread below rewrites a file in *this* test's fresh one. No event ever
    // arrives and the wait times out. That is exactly what `fixture_named`'s
    // doc comment warns about for "any test whose server-side code touches the
    // filesystem", and it made this the first casualty of a ~1-in-6 whole-suite
    // flake on Linux while passing 20/20 in isolation, where no other test is
    // there to create the shared hub first.
    let (d, port) = fixture_named("extedit");
    let mut a = ws_connect_path(port, "/ws/extedit/_workspace").unwrap();
    a.send(tungstenite::Message::Text(
        "{\"t\":\"EditBuffer\",\"rel\":\"hello.md\",\"text\":\"# Hello\\n\"}".into(),
    ))
    .unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"SaveBuffer","rel":"hello.md","force":true}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "SaveOk"); // buffer is now clean

    // Claude, in the next pane, rewrites the file. The watcher now spins up
    // on a background thread (the large-project fix: `for_project` must
    // return promptly regardless of tree size, so it can no longer walk and
    // register OS watches inline before answering this connection) — so
    // there's a short, expected window right after connecting where the
    // watcher isn't live yet. Keep rewriting the file in the background
    // instead of writing once, so the test doesn't depend on winning that
    // race on the first try.
    let hello_path = d.path().join("extedit/hello.md");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let writer = std::thread::spawn(move || {
        let mut n = 0u32;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            std::fs::write(&hello_path, format!("# Rewritten by Claude {n}\n")).unwrap();
            n += 1;
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let seen = read_until(&mut a, "Rewritten by Claude");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = writer.join();
    assert!(seen.contains(r#""t":"BufferText""#), "a clean buffer must follow the file");

    // The hub for "extedit" still outlives this test (Hub is process-global),
    // but the name is now unique to this test, so nothing else can inherit it.
    // Closing the buffer anyway keeps the hub's state tidy for a rerun within
    // the same binary.
    a.send(tungstenite::Message::Text(r#"{"t":"CloseBuffer","rel":"hello.md"}"#.into())).unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    let _ = a.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
    std::env::remove_var("ROOST_DEBOUNCE_MS");
}

#[test]
fn set_mode_edit_then_save_writes_the_file() {
    // End-to-end regression for the live-verified bug: SetMode{Edit} must
    // make the server read the file (setting a real base_hash) before the
    // client ever calls SaveBuffer, or every first save reports a conflict
    // and the file on disk never changes.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (d, port) = fixture_named("editproj1");
    let mut a = ws_connect_path(port, "/ws/editproj1/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#); // a's own initial snapshot

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "hello.md");

    a.send(tungstenite::Message::Text(
        r#"{"t":"SetMode","rel":"hello.md","mode":"Edit"}"#.into(),
    ))
    .unwrap();
    // The server must push the disk content with an empty origin — a
    // non-empty origin equal to a's own id would be dropped client-side by
    // the echo rule and the editor would open blank.
    let text_ev = read_until(&mut a, r#""t":"BufferText""#);
    assert!(text_ev.contains("# Hello"), "got: {text_ev}");
    assert!(text_ev.contains(r#""origin":"""#), "must be authorless; got: {text_ev}");

    a.send(tungstenite::Message::Text(
        "{\"t\":\"EditBuffer\",\"rel\":\"hello.md\",\"text\":\"# Hello, edited\\n\"}".into(),
    ))
    .unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"SaveBuffer","rel":"hello.md","force":false}"#.into(),
    ))
    .unwrap();
    // force:false is the whole point of this test: only a correct base_hash
    // (set by SetMode's disk read) lets an *unforced* save through.
    let saved = read_until(&mut a, r#""t":"SaveOk""#);
    assert!(saved.contains(r#""t":"SaveOk""#));
    assert_eq!(
        std::fs::read_to_string(d.path().join("editproj1/hello.md")).unwrap(),
        "# Hello, edited\n",
        "the file on disk must actually change"
    );

    let _ = a.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

#[test]
fn reconnect_replays_buffer_text_for_open_edit_buffers() {
    // A client that (re)connects onto a layout with an already-open Edit
    // buffer gets metadata-only State — never text — so without a replay,
    // that editor renders permanently blank until someone happens to edit
    // the same file again.
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("editproj2");
    let mut a = ws_connect_path(port, "/ws/editproj2/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);
    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "hello.md");
    a.send(tungstenite::Message::Text(
        r#"{"t":"SetMode","rel":"hello.md","mode":"Edit"}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, r#""t":"BufferText""#);

    // A second connection joins after the buffer already exists — this is
    // the reconnect-onto-existing-state path, not the original open.
    let mut b = ws_connect_path(port, "/ws/editproj2/_workspace").unwrap();
    let b_text = read_until(&mut b, r#""t":"BufferText""#);
    assert!(b_text.contains("hello.md") && b_text.contains("# Hello"), "got: {b_text}");

    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}
