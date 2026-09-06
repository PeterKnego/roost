//! Upload and paste: the two POST endpoints, their Origin checks, size and
//! part-count limits, and destination handling.

mod common;
use common::*;

/// The check the whole GET-only amendment is traded against, so it gets the
/// treatment `ws_rejects_foreign_and_missing_origin` already gives the socket.
/// It asserts the *file was not written*, not merely the status: a 403 returned
/// after the write would still be a drive-by write.
#[test]
fn upload_refuses_a_foreign_or_absent_origin_without_writing() {
    let (d, port) = fixture_named("up_origin");
    let (ct, body) = multipart(&[("evil.txt", b"x")]);

    let (s1, _) = post(port, "/upload/up_origin", Some("https://evil.example.com"), &ct, &body);
    assert_eq!(s1, 403, "a foreign origin must not reach the upload endpoint");

    let (s2, _) = post(port, "/upload/up_origin", None, &ct, &body);
    assert_eq!(s2, 403, "a request with no Origin must be refused");

    assert!(
        !d.path().join("up_origin/evil.txt").exists(),
        "a refused upload must not have written the file"
    );
}

#[test]
fn upload_writes_every_part_and_reports_per_file() {
    let (d, port) = fixture_named("up_multi");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::write(d.path().join("up_multi/taken.txt"), b"original").unwrap();
    let (ct, body) = multipart(&[("a.txt", b"AAA"), ("taken.txt", b"BBB"), ("c.txt", b"CCC")]);

    let (status, resp) = post(port, "/upload/up_multi", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "a partial failure is still a well-formed request");

    assert_eq!(std::fs::read(d.path().join("up_multi/a.txt")).unwrap(), b"AAA");
    assert_eq!(std::fs::read(d.path().join("up_multi/c.txt")).unwrap(), b"CCC");
    assert_eq!(
        std::fs::read(d.path().join("up_multi/taken.txt")).unwrap(),
        b"original",
        "the colliding part must not have overwritten anything"
    );
    assert!(resp.contains("taken.txt") && resp.contains("already exists"), "response: {resp}");
    // The neighbours must be reported as successes, or a caller cannot tell
    // which of the three failed — and this is what pins that a rejected part is
    // still drained, since c.txt comes after the failure.
    assert!(resp.contains(r#"{"name":"a.txt","ok":true}"#), "response: {resp}");
    assert!(resp.contains(r#"{"name":"c.txt","ok":true}"#), "response: {resp}");
}

#[test]
fn upload_refuses_more_parts_than_the_limit() {
    let (d, port) = fixture_named("up_parts");
    let origin = format!("http://127.0.0.1:{port}");
    let names: Vec<String> = (0..20).map(|i| format!("f{i}.txt")).collect();
    let parts: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
    let (ct, body) = multipart(&parts);

    let (status, resp) = post(port, "/upload/up_parts", Some(&origin), &ct, &body);
    assert_eq!(status, 413);
    assert!(resp.contains("too many files"), "the parts cap must name itself: {resp}");
    assert!(!d.path().join("up_parts/f19.txt").exists());
}

/// A different cap with a different message. Two tests that both passed because
/// the same limit fired would say nothing about the other.
#[test]
fn upload_refuses_a_body_past_the_aggregate_limit() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_MAX_UPLOAD", "4096");
    let (d, port) = fixture_named("up_bytes");
    let origin = format!("http://127.0.0.1:{port}");
    let big = vec![b'x'; 8192];
    let (ct, body) = multipart(&[("big.bin", &big)]);

    let (status, resp) = post(port, "/upload/up_bytes", Some(&origin), &ct, &body);
    std::env::remove_var("ROOST_MAX_UPLOAD");

    assert_eq!(status, 413);
    assert!(resp.contains("too large"), "the size cap must name itself: {resp}");
    assert!(!d.path().join("up_bytes/big.bin").exists());
    let leftovers: Vec<String> = std::fs::read_dir(d.path().join("up_bytes"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("roost.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "a cap breach left a partial file: {leftovers:?}");
}

#[test]
fn upload_refuses_a_hidden_destination() {
    let (d, port) = fixture_named("up_hidden");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::create_dir_all(d.path().join("up_hidden/.git")).unwrap();
    let (ct, body) = multipart(&[("config", b"[core]")]);
    let (status, resp) = post(port, "/upload/up_hidden?dir=.git", Some(&origin), &ct, &body);
    assert_eq!(status, 200);
    assert!(resp.contains("not visible in the tree"), "response: {resp}");
    assert!(!d.path().join("up_hidden/.git/config").exists());
}

#[test]
fn upload_lands_in_the_named_subdirectory() {
    let (d, port) = fixture_named("up_sub");
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::create_dir_all(d.path().join("up_sub/src")).unwrap();
    let (ct, body) = multipart(&[("logo.png", b"PNG")]);
    let (status, resp) = post(port, "/upload/up_sub?dir=src", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "response: {resp}");
    assert_eq!(std::fs::read(d.path().join("up_sub/src/logo.png")).unwrap(), b"PNG");
}

/// With `ROOST_CMD=cat` the PTY echoes what is written to it, so the terminal
/// socket is a direct view of the injected bytes. Asserting the markers — not
/// merely that the session survived — is the point: CLAUDE.md records a test
/// whose subject was a call it never actually verified.
#[test]
fn a_pasted_image_injects_a_bracketed_path_into_the_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", state.path());
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");

    // Attaching creates the session; the paste needs a live one.
    let mut term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();

    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, resp) = post_when_session_ready(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "response: {resp}");

    let mut seen = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !seen.contains("\u{1b}[201~") {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            // ws_connect sets a 5s read timeout, so an idle gap surfaces as a
            // would-block rather than a death. Retrying until the deadline is
            // the difference between this test waiting and this test failing
            // for a reason that has nothing to do with pasting.
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("terminal socket died waiting for the paste: {e}"),
        }
    }
    // The PTY echoes with ECHOCTL, which renders the ESC byte as the two
    // printable characters `^[` — so the raw \x1b never appears here, and
    // asserting on it would fail against a perfectly correct injection. What
    // does survive is the rest of each marker, which nothing else would produce.
    assert!(seen.contains("[200~"), "missing the opening marker: {seen:?}");
    assert!(seen.contains("[201~"), "missing the closing marker: {seen:?}");
    assert!(seen.contains(".png"), "the injected path must carry an image extension: {seen:?}");
    assert!(
        seen.contains(&state.path().join("pasted").to_string_lossy().to_string()),
        "the path must be absolute and under the state dir, not in the project: {seen:?}"
    );
    std::env::remove_var("ROOST_CMD");
}

/// Differs from an accepted paste only in its *content* — same filename, same
/// live session — which is what makes it a control on the sniffing rather than
/// on the plumbing. It needs a live session because liveness is checked first,
/// deliberately: a paste is refused before its bytes are accepted.
#[test]
fn a_paste_of_a_non_image_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("ROOST_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", state.path());
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let _term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();

    // A BMP: a real image the *clipboard* route would take, refused here
    // because the receiver cannot read `.bmp` from a path.
    let (ct, body) = multipart(&[("clip.png", b"BM\0\0\0\0\0\0\0\0\0\0")]);
    let (status, resp) = post_when_session_ready(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 400, "response: {resp}");
    assert!(resp.contains("PNG"), "the error must name what is accepted: {resp}");
    assert!(
        std::fs::read_dir(state.path().join("pasted")).map(|d| d.count()).unwrap_or(0) <= 1,
        "a refused paste must not have left an image behind"
    );
    std::env::remove_var("ROOST_CMD");
}

#[test]
fn a_paste_onto_a_dead_session_is_an_error_not_a_silent_success() {
    let (_d, port) = fixture_named("paste_dead");
    let origin = format!("http://127.0.0.1:{port}");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, resp) = post(port, "/paste/paste_dead/nosuch", Some(&origin), &ct, &body);
    assert_eq!(status, 404);
    assert!(resp.contains("no such session"), "unexpected error: {resp}");
}

#[test]
fn a_paste_refuses_a_foreign_origin() {
    let (_d, port) = fixture_named("paste_origin");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, _) =
        post(port, "/paste/paste_origin/shell", Some("https://evil.example.com"), &ct, &body);
    assert_eq!(status, 403, "the paste endpoint needs the same gate as the upload one");
}

/// The property the old `http::tests::rejects_non_get` used to guarantee at the
/// parser: a request carrying a body must not reach the fragment routes. POST is
/// now parsed, so this is what stands in its place — and it asserts on the
/// *fragment content* rather than the status, because a route that ran and then
/// returned an error status would still have run.
#[test]
fn post_to_an_ordinary_path_does_not_reach_the_router() {
    let (_d, port) = fixture_named("post_router");
    let origin = format!("http://127.0.0.1:{port}");
    let (ct, body) = multipart(&[("x.txt", b"x")]);
    // A *valid* Origin, so this tests routing rather than tripping the origin
    // gate first — otherwise it would pass for a reason unrelated to its name.
    let (status, text) = post(port, "/frag/post_router/tree", Some(&origin), &ct, &body);
    assert_eq!(status, 404, "an ordinary path must not answer a POST");
    assert!(
        !text.contains("<ul class=\"tree\""),
        "the tree fragment was rendered for a POST: {text}"
    );
}
