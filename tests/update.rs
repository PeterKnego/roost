//! The update choices over a real websocket: `Later` reaches every page,
//! and a stale `Skip` is refused to its sender. #65 step 4.
//!
//! Uses `ws_connect_path`, not `ws_connect`: the latter hard-codes the
//! *terminal* PTY socket (`/ws/proj/term/shell`), a different protocol from
//! the Intent/Event JSON `DeferUpdate`/`SkipUpdate`/`State` live on
//! (`/ws/proj/_workspace`) — the same substitution `tests/settings.rs`
//! already made for this class of test. See drift.md's Task 9 row.
mod common;
use common::*;
use tungstenite::Message;

fn update_of(state_json: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(state_json).unwrap();
    v["ws"]["settings"]["update"].clone()
}

// `apply_defer`/`apply_skip` write `choices.json` under `ROOST_STATE_DIR`,
// which is process-global — set here, under `WS_TEST_LOCK`, the same lock
// (and the same reasoning) `tests/settings.rs` uses for tests that mutate
// it, so this test's choice never lands in a developer's real
// `~/.local/state/roost`.
//
// Deleting the wsconn divert makes `DeferUpdate` reach `Hub::handle`'s
// defensive arm instead of `apply_defer`: the arm replies with an `Error`,
// not a `State`, so `read_until(&mut a, r#""t":"State""#)` after the send
// times out at its 15s deadline — a hang-shaped failure.
#[test]
fn later_reaches_every_page_and_a_stale_skip_is_refused_to_its_sender() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());

    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let first = read_until(&mut a, r#""t":"State""#);
    let _ = read_until(&mut b, r#""t":"State""#);
    assert_eq!(update_of(&first)["deferred_until"], 0, "nothing deferred yet");

    a.send(Message::Text(r#"{"t":"DeferUpdate"}"#.into())).unwrap();
    let sa = update_of(&read_until(&mut a, r#""t":"State""#));
    let sb = update_of(&read_until(&mut b, r#""t":"State""#));
    assert!(sa["deferred_until"].as_u64().unwrap() > 0, "the sender's page sees the deferral: {sa}");
    assert_eq!(sa["deferred_until"], sb["deferred_until"], "and so does every other page, at once");

    // A version no check has seen: refused, named, and only to the sender.
    // (This test binary never runs a real version check — see
    // `common::disable_version_check` — so `crate::version::current()` has
    // no `latest` at all, and `Update` itself would be refused the same way
    // before ever reaching a download: this test's server has no
    // compiled-in key either, see `keys/roost.pub`.)
    a.send(Message::Text(r#"{"t":"SkipUpdate","version":"1.2.3"}"#.into())).unwrap();
    let err = read_until(&mut a, r#""t":"Error""#);
    assert!(err.contains("1.2.3 is not the version the last check saw"), "{err}");
    // No snapshot was broadcast for a refused skip: b's next message is
    // the answer to its own RequestState, and its skip is still empty.
    b.send(Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    let sb = update_of(&read_until(&mut b, r#""t":"State""#));
    assert_eq!(sb["skipped"], "", "{sb}");

    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}

// `Update` itself, diverted the same way. This test binary is a checkout
// with no compiled-in key (`update::public_key()` is `None` — `keys/roost.pub`
// does not exist in this tree), so `update::eligible()` refuses before any
// network call or exec is attempted, and the refusal must reach the
// requester as an `UpdateProgress{phase:"refused"}`. That message is
// distinct from `Hub::handle`'s defensive-arm `Error` ("update intents are
// handled before the hub") — asserting its *absence* is what proves this
// frame came from the real divert into `update::start`, not from a second
// dispatch site that fell through to the arm meant only as a backstop.
#[test]
fn update_with_no_compiled_in_key_is_refused_to_its_sender_alone() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());

    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);

    a.send(Message::Text(r#"{"t":"Update"}"#.into())).unwrap();
    let got = read_until(&mut a, r#""t":"UpdateProgress""#);
    assert!(got.contains(r#""phase":"refused""#), "{got}");
    assert!(!got.contains("update intents are handled before the hub"), "the divert took it, not the defensive arm: {got}");

    let _ = a.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}
