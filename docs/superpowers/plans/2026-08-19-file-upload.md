# File Upload and Terminal Image Paste Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a browser put bytes into a resh workspace — dragged or pasted into the file tree, or pasted as an image onto a terminal tab, where it becomes a real image attachment for the Claude running in that shell.

**Architecture:** Two new websocket intents carry base64 payloads over the existing `_workspace` socket; no HTTP route changes and no new dependency. `UploadFile` decodes and writes into the project through a new byte-typed `fileops::write_bytes`, which reuses the existing confinement and three-outcome existence checks. `PasteImage` writes a scratch file *outside* the repo and injects its absolute path into the session's PTY wrapped in bracketed-paste markers.

**Tech Stack:** Rust, no async runtime, thread-per-connection. `tungstenite` for websockets. Plain JS with no framework on the client. `cargo test`, never `--release`.

**Spec:** `docs/superpowers/specs/2026-08-19-file-upload-design.md` — read it before Task 1. The plan argues from the spec; where they disagree, the spec wins and the plan is wrong.

## Global Constraints

These come from `CLAUDE.md` and apply to every task below. They are not style preferences.

- **HTTP stays GET-only.** No task here adds a route or a method. Every state change is a websocket intent.
- **Every filesystem path is confined before use** — `projects::safe_resolve` for existing targets, `projects::safe_resolve_parent` for anything being created.
- **Never hold a lock across blocking I/O.** This project shipped one deadlock that way. Task 6 is where this bites; read its note before writing code.
- **Absence of evidence is not evidence of absence.** `Path::exists()`/`is_dir()` are banned before anything destructive; use `symlink_metadata` and treat "cannot tell" as a third outcome. `fileops::must_not_exist` already does this — reuse it, do not re-derive it.
- **No panics may escape a socket or watcher thread.**
- **Module-level `//!` doc explaining *why* the module exists**; `#[cfg(test)] mod tests` at the bottom of the same file; comments give rationale, not mechanics.
- **Caps, after this change:** `MAX_UPLOAD_BYTES` 8 MB (opaque bytes in), `MAX_TEXT_BYTES` 2 MB (editor buffers, unchanged), `MAX_FRAME_BYTES` 12 MB (socket). Exact values in Task 1.
- **Tests must be able to fail.** For every negative test, assert on *why* — the message, not `is_err()`. Before committing any task, revert your implementation, run the test, watch it fail, restore. That is not a thought experiment; it is the step that has caught vacuous tests here twice.

---

### Task 1: Three caps, decoupled, with a coherence test

The spec's cap decision, done first because every later task's limits refer to it. The subtlety: `MAX_FRAME_BYTES` is currently *derived* from `MAX_TEXT_BYTES`, so raising the frame ceiling the obvious way would quadruple the editor's text cap as a side effect.

**Files:**
- Modify: `src/fileops.rs:6` (add `MAX_UPLOAD_BYTES` beside `MAX_WRITE_BYTES`)
- Modify: `src/wsconn.rs:13-19` (the `MAX_FRAME_BYTES` comment and constant)
- Modify: `CLAUDE.md` (the hard-constraints caps line)
- Test: `src/wsconn.rs` (bottom, `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `fileops::MAX_UPLOAD_BYTES: usize` (8_000_000), used by Tasks 3, 4, 6, 7.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `src/wsconn.rs`. It asserts the *relationship*, which is the thing that silently breaks when someone edits one number:

```rust
#[cfg(test)]
mod tests {
    use super::MAX_FRAME_BYTES;

    /// The failure this prevents is invisible from the client: an upload
    /// inside its own documented limit gets refused by tungstenite before any
    /// resh code runs, so there is no `Event::Error` to show — the socket just
    /// closes. Asserting the two constants' values separately would not catch
    /// it; only the relationship does.
    #[test]
    fn the_frame_ceiling_clears_a_largest_legal_upload() {
        let encoded = (crate::fileops::MAX_UPLOAD_BYTES + 2) / 3 * 4;
        let envelope = 4096; // {"t":"UploadFile","rel":"…","data_b64":"…"}
        assert!(
            MAX_FRAME_BYTES >= encoded + envelope,
            "frame ceiling {MAX_FRAME_BYTES} cannot carry a {} byte upload \
             (base64 {encoded} + envelope {envelope})",
            crate::fileops::MAX_UPLOAD_BYTES
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_frame_ceiling_clears`
Expected: FAIL — `MAX_UPLOAD_BYTES` does not exist yet (compile error), which is the correct first failure.

- [ ] **Step 3: Add the upload cap**

In `src/fileops.rs`, beside the existing `MAX_WRITE_BYTES`:

```rust
const MAX_WRITE_BYTES: usize = 2_000_000;

/// Uploads are opaque bytes passing through; a buffer is text the server holds,
/// diffs, and mirrors to every client on every keystroke. They answer different
/// questions, so they get different numbers — 2 MB would refuse the 4K
/// screenshot this feature exists to carry.
pub const MAX_UPLOAD_BYTES: usize = 8_000_000;
```

- [ ] **Step 4: Undo the derivation in `wsconn.rs`**

Replace the existing constant and rewrite its comment — the old one explains a derivation that is about to be wrong:

```rust
/// Sized for the largest *upload*, not the largest text buffer. An
/// `EditBuffer` is capped at `workspace::MAX_TEXT_BYTES` of text and its JSON
/// escaping fits several times over; an `UploadFile` carries
/// `fileops::MAX_UPLOAD_BYTES` base64-encoded, which is ~10.7 MB before the
/// envelope. Deliberately *not* derived from either constant: the previous
/// `MAX_TEXT_BYTES * 4` meant raising this ceiling for uploads would have
/// quadrupled the editor's text cap as a side effect. `wsconn`'s tests assert
/// the relationship this must keep.
const MAX_FRAME_BYTES: usize = 12_000_000;
```

- [ ] **Step 5: Run the test and the suite**

Run: `cargo test`
Expected: PASS, including the new test and every existing one. If `workspace::apply_layout`'s text-cap tests changed behaviour, you raised the wrong constant — `MAX_TEXT_BYTES` must still be 2_000_000.

- [ ] **Step 6: Amend the constraint in CLAUDE.md**

The hard-constraints list says the caps are `1 MB scrollback, 2 MB file cap for reads *and* writes`. That becomes false with this change. Edit that line to:

```markdown
- Caps: ≤16 sessions per project, ≤50 open buffers, 1 MB scrollback, 2 MB file
  cap for reads and for buffer writes, 8 MB for uploads (opaque bytes, never
  opened as text).
```

- [ ] **Step 7: Commit**

```bash
git add src/fileops.rs src/wsconn.rs CLAUDE.md
git commit -m "caps: separate the upload ceiling from the text and frame limits"
```

---

### Task 2: A base64 decoder

No base64 crate is in `Cargo.toml` and this project does not add dependencies casually. The decoder is small, pure, and the one piece that can be tested exhaustively without a socket.

**Files:**
- Create: `src/b64.rs`
- Modify: `src/lib.rs` (add `pub mod b64;`)
- Test: `src/b64.rs` (bottom)

**Interfaces:**
- Consumes: nothing.
- Produces: `b64::decode(s: &str) -> Result<Vec<u8>, String>` and `b64::encoded_len(bytes: usize) -> usize`, both used by Tasks 4 and 6.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_rfc_vectors_including_both_padding_lengths() {
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(decode("Zg==").unwrap(), b"f");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    /// PNG magic, the payload this actually carries.
    #[test]
    fn round_trips_bytes_that_are_not_utf8() {
        let png = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(decode("iVBORw0KGgo=").unwrap(), png);
    }

    /// Each negative case asserts the *reason*. `is_err()` alone would pass
    /// against a decoder that rejected everything, including valid input.
    #[test]
    fn rejects_a_length_that_is_not_a_multiple_of_four() {
        let e = decode("Zm9vY").unwrap_err();
        assert!(e.contains("multiple of 4"), "unexpected message: {e}");
    }

    #[test]
    fn rejects_a_character_outside_the_alphabet() {
        let e = decode("Zm9v!!!!").unwrap_err();
        assert!(e.contains("invalid base64 character"), "unexpected message: {e}");
    }

    #[test]
    fn rejects_padding_before_the_final_chunk() {
        let e = decode("Zg==Zg==").unwrap_err();
        assert!(e.contains("padding before"), "unexpected message: {e}");
    }

    #[test]
    fn rejects_padding_inside_a_chunk() {
        let e = decode("Z=9v").unwrap_err();
        assert!(e.contains("padding inside"), "unexpected message: {e}");
    }

    #[test]
    fn encoded_len_matches_what_decode_accepts() {
        for n in 0..64usize {
            let encoded = encoded_len(n);
            assert_eq!(encoded % 4, 0, "encoded length must be a whole number of chunks");
            assert!(encoded * 3 / 4 >= n, "encoded length must cover {n} bytes");
        }
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib b64`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write the decoder**

```rust
//! Base64 for upload payloads. Hand-rolled because uploads are the only thing
//! in resh that needs it and this project does not take a dependency for forty
//! lines — the same reasoning that leaves the HTTP layer hand-rolled.
//!
//! Strict on purpose: it rejects non-canonical input (padding in the middle,
//! characters outside the alphabet) rather than skipping what it does not
//! understand. A lenient decoder turns a corrupted upload into a *plausible*
//! file, and this one's output gets written to the user's disk.

const INVALID: u8 = 255;

fn sextet(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => INVALID,
    }
}

/// Encoded length of `bytes` bytes, padding included. Used to refuse an
/// oversized payload *before* allocating a decode buffer for it.
pub const fn encoded_len(bytes: usize) -> usize {
    (bytes + 2) / 3 * 4
}

pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    if b.len() % 4 != 0 {
        return Err(format!("base64 length {} is not a multiple of 4", b.len()));
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (i, chunk) in b.chunks_exact(4).enumerate() {
        let pad = chunk.iter().filter(|&&c| c == b'=').count();
        let last = (i + 1) * 4 == b.len();
        if pad > 0 && !last {
            return Err("base64 padding before the final chunk".into());
        }
        if pad > 2 {
            return Err("base64 chunk is entirely padding".into());
        }
        let mut acc: u32 = 0;
        for (n, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                // Padding is only legal in the trailing positions.
                if n < 4 - pad {
                    return Err("base64 padding inside a chunk".into());
                }
                acc <<= 6;
                continue;
            }
            let v = sextet(c);
            if v == INVALID {
                return Err(format!("invalid base64 character {:?}", c as char));
            }
            acc = (acc << 6) | v as u32;
        }
        out.push((acc >> 16) as u8);
        if pad < 2 {
            out.push((acc >> 8) as u8);
        }
        if pad < 1 {
            out.push(acc as u8);
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`, alongside the other `pub mod` lines, in alphabetical position:

```rust
pub mod b64;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib b64`
Expected: PASS, all seven.

- [ ] **Step 6: Prove the negative tests can fail**

Delete the `if pad > 0 && !last` check, run `cargo test --lib b64`, confirm `rejects_padding_before_the_final_chunk` fails, then restore it. Do the same for the `n < 4 - pad` check. A negative test that passes with its guard removed is testing nothing.

- [ ] **Step 7: Commit**

```bash
git add src/b64.rs src/lib.rs
git commit -m "b64: a strict decoder for upload payloads"
```

---

### Task 3: `fileops::write_bytes`

The byte-typed sibling of `save`. Most of the safety already exists in this module — `must_not_exist` implements the three-outcome rule, `safe_resolve_parent` confines and validates the final component. This task adds bytes, a cap, and a pid-unique temp name.

**Files:**
- Modify: `src/fileops.rs:15-34` (`atomic_write` takes bytes and a unique temp name)
- Modify: `src/fileops.rs:37-57` (`save` passes `text.as_bytes()`)
- Modify: `src/fileops.rs` (add `write_bytes` after `create_dir`)
- Test: `src/fileops.rs` (bottom, existing `mod tests`)

**Interfaces:**
- Consumes: `fileops::MAX_UPLOAD_BYTES` (Task 1).
- Produces: `fileops::write_bytes(project_dir: &Path, rel: &str, data: &[u8]) -> Result<PathBuf, String>`, used by Tasks 4 and 5.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` in `src/fileops.rs`:

```rust
    /// Reaches the confinement check rather than failing earlier for an
    /// unrelated reason: the parent (`..`) exists, so this cannot pass on
    /// ENOENT. That exact hole is why a symlink escape once survived review
    /// here, so the assertion is on the message, not on `is_err()`.
    #[test]
    fn write_bytes_refuses_a_traversal_and_says_why() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        let e = write_bytes(&proj, "../escape.png", b"x").unwrap_err();
        assert!(e.contains("path outside project"), "unexpected message: {e}");
        assert!(
            !d.path().join("escape.png").exists(),
            "a refused upload must not have written outside the project"
        );
    }

    /// Asserting `is_err()` alone would also pass against an implementation
    /// that truncated the file and *then* failed, which is the outcome this
    /// test exists to forbid.
    #[test]
    fn write_bytes_refuses_a_collision_and_leaves_the_original_intact() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.png"), b"original").unwrap();
        let e = write_bytes(d.path(), "a.png", b"replacement").unwrap_err();
        assert!(e.contains("already exists"), "unexpected message: {e}");
        assert_eq!(
            std::fs::read(d.path().join("a.png")).unwrap(),
            b"original",
            "a refused upload must not have touched the existing bytes"
        );
    }

    /// "Cannot tell" is a third outcome, never folded into "nothing is there".
    #[test]
    #[cfg(unix)]
    fn write_bytes_refuses_when_it_cannot_tell_whether_the_target_exists() {
        use std::os::unix::fs::PermissionsExt;
        if nix_is_root() {
            return; // mode bits do not apply to root; the fixture cannot enter its own precondition
        }
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = write_bytes(d.path(), "locked/a.png", b"x");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = r.unwrap_err();
        assert!(
            e.contains("no such directory") || e.contains("cannot check"),
            "an unreadable parent must be refused as unknown, not treated as absent: {e}"
        );
    }

    #[test]
    fn write_bytes_refuses_an_oversize_payload() {
        let d = tempfile::tempdir().unwrap();
        let big = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let e = write_bytes(d.path(), "big.bin", &big).unwrap_err();
        assert!(e.contains("too large"), "unexpected message: {e}");
    }

    #[test]
    fn write_bytes_refuses_a_filename_with_a_control_character() {
        let d = tempfile::tempdir().unwrap();
        let e = write_bytes(d.path(), "ev\nil.png", b"x").unwrap_err();
        assert!(e.contains("invalid filename"), "unexpected message: {e}");
    }

    #[test]
    fn write_bytes_writes_non_utf8_bytes_and_leaves_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let png = [0x89u8, 0x50, 0x4e, 0x47, 0xff, 0xfe];
        let p = write_bytes(d.path(), "a.png", &png).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), png);
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("resh.tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    fn nix_is_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib fileops`
Expected: FAIL — `write_bytes` does not exist.

- [ ] **Step 3: Generalise `atomic_write` to bytes**

Change the signature and the temp name. The pid makes two processes writing the same destination unable to collide on the temp file — `CLAUDE.md` asks for this wherever a reader might see a half-written file, and the watcher is exactly such a reader:

```rust
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.{}.resh.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("buf"),
        std::process::id()
    ));
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
```

The rest of the function is unchanged. In `save`, the call becomes:

```rust
    atomic_write(&abs, text.as_bytes())?;
```

- [ ] **Step 4: Add `write_bytes`**

After `create_dir` in `src/fileops.rs`:

```rust
/// Rejects what `safe_resolve_parent` does not already: it validates the final
/// component for traversal, but the name here arrives from a browser's
/// `DataTransfer` — attacker-influenced even though the socket checks `Origin`
/// — so control characters and separators in either direction are refused too.
fn valid_upload_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("upload has no filename".into());
    }
    if name.len() > 255 {
        return Err(format!("invalid filename: {} bytes is too long", name.len()));
    }
    if name.contains('/') || name.contains('\\') || name.chars().any(|c| c.is_control()) {
        return Err(format!("invalid filename: {name:?}"));
    }
    Ok(())
}

/// The byte-typed sibling of `save`. No base hash, because an upload has no
/// buffer it was opened from, and no overwrite: a drop that lands on an
/// existing name is refused rather than resolved, because the burden of proof
/// is on destroying.
pub fn write_bytes(project_dir: &Path, rel: &str, data: &[u8]) -> Result<PathBuf, String> {
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(format!("file too large ({} bytes)", data.len()));
    }
    valid_upload_name(rel.rsplit('/').next().unwrap_or(""))?;
    let abs = safe_resolve_parent(project_dir, rel)?;
    must_not_exist(&abs, rel)?;
    atomic_write(&abs, data)?;
    Ok(abs)
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS. The whole suite, not just `fileops` — `atomic_write` changed under `save`.

- [ ] **Step 6: Prove the collision test can fail**

Replace `must_not_exist(&abs, rel)?;` with nothing, run `cargo test --lib write_bytes_refuses_a_collision`, confirm it fails on the *bytes* assertion, restore. This is the revert-and-watch step from `CLAUDE.md`; do it rather than reasoning about it.

- [ ] **Step 7: Commit**

```bash
git add src/fileops.rs
git commit -m "fileops: write_bytes, the byte-typed sibling of save"
```

---

### Task 4: `Intent::UploadFile` and its handler

Wires the decoder to the writer. The hub already has the shape this needs — `do_fileop` takes a `Result<PathBuf, String>`, broadcasts `TreeChanged` on success and sends `Event::Error` to the originating connection on failure.

**Files:**
- Modify: `src/proto.rs:41-74` (the `Intent` enum)
- Modify: `src/hub.rs:297-310` (beside `CreateFile`/`CreateDir`)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `b64::decode`, `b64::encoded_len`, `fileops::write_bytes`, `fileops::MAX_UPLOAD_BYTES`.
- Produces: the wire intent `{"t":"UploadFile","rel":"<path>","data_b64":"<payload>"}`, consumed by Task 7.

- [ ] **Step 1: Write the failing test**

In `tests/integration.rs`. This goes through a real socket rather than calling `write_bytes` directly, because only the full path exercises the frame ceiling from Task 1:

```rust
#[test]
fn a_largest_legal_upload_survives_the_whole_path() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (d, port) = fixture();
    let mut ws = workspace_ws(port, "proj");

    // Exactly at the cap: the size that fails if MAX_FRAME_BYTES is ever
    // re-derived from MAX_TEXT_BYTES, and the reason this test is not a unit
    // test of write_bytes.
    let payload = vec![0x41u8; resh::fileops::MAX_UPLOAD_BYTES];
    let b64 = base64_encode(&payload);
    ws.send(tungstenite::Message::Text(
        serde_json::json!({"t": "UploadFile", "rel": "big.bin", "data_b64": b64}).to_string(),
    ))
    .unwrap();

    wait_for_event(&mut ws, "TreeChanged");
    let landed = std::fs::read(d.path().join("proj").join("big.bin")).unwrap();
    assert_eq!(landed.len(), resh::fileops::MAX_UPLOAD_BYTES);
    assert_eq!(landed, payload, "the bytes that landed must be the bytes sent");
}

#[test]
fn an_upload_onto_an_existing_name_is_refused_without_touching_it() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (d, port) = fixture();
    std::fs::write(d.path().join("proj").join("taken.png"), b"original").unwrap();
    let mut ws = workspace_ws(port, "proj");

    ws.send(tungstenite::Message::Text(
        serde_json::json!({"t": "UploadFile", "rel": "taken.png", "data_b64": "eA=="}).to_string(),
    ))
    .unwrap();

    let msg = wait_for_event(&mut ws, "Error");
    assert!(msg.contains("already exists"), "unexpected error: {msg}");
    assert_eq!(
        std::fs::read(d.path().join("proj").join("taken.png")).unwrap(),
        b"original"
    );
}
```

Add these helpers near `ws_connect` if they do not already exist:

```rust
fn workspace_ws(
    port: u16,
    project: &str,
) -> tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>> {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws/{project}/_workspace")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("origin", format!("http://127.0.0.1:{port}").parse().unwrap());
    let (ws, _) = tungstenite::connect(req).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    }
    ws
}

/// Reads until an event of kind `t` arrives, returning its raw JSON. Fails the
/// test on timeout rather than returning — a helper that returns `Option` here
/// would let a caller `assert!(x.is_none())` and pass on its own read timeout,
/// which is a documented past failure in this suite.
fn wait_for_event(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    t: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match ws.read() {
            Ok(tungstenite::Message::Text(s)) => {
                if s.contains(&format!("\"t\":\"{t}\"")) {
                    return s.to_string();
                }
            }
            Ok(_) => {}
            Err(e) => panic!("socket died waiting for {t}: {e}"),
        }
    }
    panic!("timed out waiting for a {t} event");
}

fn base64_encode(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --test integration upload`
Expected: FAIL — the server ignores an unknown intent, so this times out in `wait_for_event`.

- [ ] **Step 3: Add the intent**

In `src/proto.rs`, in the `Intent` enum after `CreateDir`:

```rust
    /// Writes bytes into the project. Distinct from `SaveBuffer` on two counts:
    /// no base hash, because an upload has no buffer it was opened from, and
    /// bytes rather than text, because the payload need not be UTF-8.
    UploadFile { rel: String, data_b64: String },
```

- [ ] **Step 4: Handle it**

In `src/hub.rs`, in the `handle` match beside `CreateFile`:

```rust
            Intent::UploadFile { rel, data_b64 } => {
                // Checked on the *encoded* length, before decoding: the point
                // is to refuse without allocating a buffer an attacker sized.
                if data_b64.len() > crate::b64::encoded_len(crate::fileops::MAX_UPLOAD_BYTES) {
                    let ev = Event::Error { msg: "upload too large".into() };
                    return self.send_to(from, &ev);
                }
                let dir = self.dir.clone();
                let r = crate::b64::decode(data_b64)
                    .and_then(|bytes| crate::fileops::write_bytes(&dir, rel, &bytes));
                return self.do_fileop(from, r);
            }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS. Note `fileops` needs to be reachable as `resh::fileops` from the integration test — if it is not already `pub mod` in `src/lib.rs`, make it so in this step.

- [ ] **Step 6: Prove the frame ceiling is really exercised**

Temporarily set `MAX_FRAME_BYTES` back to `crate::workspace::MAX_TEXT_BYTES * 4`, run `cargo test --test integration a_largest_legal_upload`, and confirm it fails (the socket closes rather than delivering). Restore. This is what makes Task 1's coherence test more than a tautology.

- [ ] **Step 7: Commit**

```bash
git add src/proto.rs src/hub.rs src/lib.rs tests/integration.rs
git commit -m "upload: an UploadFile intent that writes bytes into a project"
```

---

### Task 5: Image sniffing and the scratch path

Where a pasted image lands, and under what name. The extension is load-bearing: the receiving side reads the *filename*, not the content, so a PNG saved as `.dat` degrades silently into pasted text.

**Files:**
- Create: `src/paste.rs`
- Modify: `src/lib.rs` (add `pub mod paste;`)
- Test: `src/paste.rs` (bottom)

**Interfaces:**
- Consumes: `wsstate::state_dir()`, `projects::storage_key()`, `fileops::write_bytes`.
- Produces: `paste::write_scratch_image(project: &str, data: &[u8]) -> Result<PathBuf, String>`, used by Task 6.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0];
    const GIF: &[u8] = b"GIF89a__";
    const WEBP: &[u8] = b"RIFF\0\0\0\0WEBPVP8 ";

    #[test]
    fn sniffs_each_accepted_format() {
        assert_eq!(extension_of(PNG), Some("png"));
        assert_eq!(extension_of(JPEG), Some("jpg"));
        assert_eq!(extension_of(GIF), Some("gif"));
        assert_eq!(extension_of(WEBP), Some("webp"));
    }

    /// A BMP is a real image the *clipboard* route would accept, and it is
    /// still refused here: the receiving side's filename regex does not cover
    /// `.bmp`, so writing one would produce a file that silently arrives as
    /// text. Refusing is the honest outcome.
    #[test]
    fn refuses_formats_the_receiver_cannot_read_from_a_path() {
        assert_eq!(extension_of(b"BM\0\0\0\0\0\0"), None);
        assert_eq!(extension_of(b"not an image at all"), None);
        assert_eq!(extension_of(&[]), None);
    }

    #[test]
    fn writes_under_the_state_dir_with_the_sniffed_extension() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let p = write_scratch_image("proj", PNG).unwrap();
        assert!(p.is_absolute(), "the receiver only reads absolute paths");
        assert_eq!(p.extension().unwrap(), "png");
        assert!(
            p.starts_with(d.path().join("pasted")),
            "scratch images must not land in the project: {p:?}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), PNG);
    }

    /// Two pastes in the same second must not collide — the second would be
    /// refused by write_bytes' no-overwrite rule and the user would see a
    /// paste do nothing.
    #[test]
    fn two_pastes_in_a_row_get_distinct_names() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let a = write_scratch_image("proj", PNG).unwrap();
        let b = write_scratch_image("proj", PNG).unwrap();
        assert_ne!(a, b);
    }

    /// A nested project's `/` must not become a directory separator or a
    /// literal slash in a filename — the same reason wsstate keys by
    /// storage_key rather than the raw project string.
    #[test]
    fn a_nested_project_key_is_encoded_not_split() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let p = write_scratch_image("karpie/src", PNG).unwrap();
        let dir = p.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        assert!(!dir.contains('/'), "project key leaked a separator: {dir}");
        assert_eq!(dir, crate::projects::storage_key("karpie/src"));
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib paste`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write the module**

```rust
//! Scratch storage for images pasted onto a terminal.
//!
//! These files are deliberately *outside* the project. resh already keeps its
//! own state out of the working tree so that using it never shows up in `git
//! status`; a paste directory inside the repo would undo that on the first
//! screenshot.
//!
//! The extension is not cosmetic. The receiving end of the paste — the program
//! running in the shell — decides whether a pasted path is an image by looking
//! at its *filename*, not at its bytes, so a correct PNG written as `.dat`
//! silently arrives as text instead of as an image. That is why this module
//! sniffs the content and refuses anything it cannot name correctly, rather
//! than trusting a MIME type from the browser.
use std::path::{Path, PathBuf};

/// The formats the receiver recognises from a path. BMP is deliberately absent:
/// the clipboard route accepts it, the path route does not.
pub fn extension_of(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

fn scratch_dir(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("pasted").join(crate::projects::storage_key(project))
}

/// Writes `data` to a fresh file under the scratch directory and returns its
/// absolute path. Absolute because the receiver's relative-path branch falls
/// back to reading a clipboard that does not exist on a headless host.
pub fn write_scratch_image(project: &str, data: &[u8]) -> Result<PathBuf, String> {
    let ext = extension_of(data)
        .ok_or("clipboard image is not a PNG, JPEG, GIF or WebP")?;
    let dir = scratch_dir(project);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create paste directory: {e}"))?;

    // Counter, not a timestamp: two pastes inside the same second would collide,
    // and a collision is refused by write_bytes rather than resolved — so the
    // user's second paste would appear to do nothing.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for n in 0..1000 {
        let name = if n == 0 {
            format!("{stamp}.{ext}")
        } else {
            format!("{stamp}-{n}.{ext}")
        };
        let candidate = dir.join(&name);
        if candidate.symlink_metadata().is_ok() {
            continue;
        }
        return write_into(&dir, &name, data);
    }
    Err("too many pasted images in the same second".into())
}

/// Reuses `fileops::write_bytes` for the cap, the atomic write and the
/// three-outcome existence check, with the scratch directory standing in for
/// the project root. The confinement is trivially satisfied here (the name has
/// no separators) but going through one writer keeps the caps in one place.
fn write_into(dir: &Path, name: &str, data: &[u8]) -> Result<PathBuf, String> {
    crate::fileops::write_bytes(dir, name, data)
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`:

```rust
pub mod paste;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib paste`
Expected: PASS, all five.

- [ ] **Step 6: Prove the sniffing test can fail**

Change `extension_of` to `Some("png")` unconditionally, run `cargo test --lib paste`, confirm `refuses_formats_the_receiver_cannot_read_from_a_path` fails, restore.

- [ ] **Step 7: Commit**

```bash
git add src/paste.rs src/lib.rs
git commit -m "paste: scratch storage for pasted images, named by sniffed format"
```

---

### Task 6: `Intent::PasteImage` and the PTY injection

The task with the lock hazard. Read this note before writing code.

**The hub lock is not the registry lock.** `session::write_input` takes the registry lock only long enough to clone the writer `Arc` and drops it before writing — but `Hub::handle` runs with the *hub* lock held by `wsconn::handle`, and a PTY write blocks indefinitely if the child stops draining stdin. Doing the write inline would hold the hub lock across blocking I/O, stalling every connection for that project: the same shape as the deadlock `CLAUDE.md` records as already shipped. So the decode and the sniff happen inline (CPU only), and the file write plus the PTY write happen on a spawned thread, mirroring `do_close_project`.

**Files:**
- Modify: `src/proto.rs` (the `Intent` enum)
- Modify: `src/hub.rs` (the `handle` match, and a new `do_paste_image` beside `do_start_terminal`)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `b64::decode`, `paste::write_scratch_image`, `session::valid_name`, `session::has_session`, `session::key_for`, `session::write_input`.
- Produces: the wire intent `{"t":"PasteImage","session":"term","data_b64":"<payload>"}`, consumed by Task 7.

- [ ] **Step 1: Write the failing test**

In `tests/integration.rs`. With `RESH_CMD=cat` the PTY echoes what is written to it, so the terminal socket is a direct view of the injected bytes — this asserts the *exact* sequence rather than that the session survived:

```rust
#[test]
fn a_pasted_image_injects_a_bracketed_path_into_the_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", state.path());
    let (_d, port) = fixture();

    // Attaching creates the session; the paste needs a live one.
    let mut term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    let mut ws = workspace_ws(port, "proj");

    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    ws.send(tungstenite::Message::Text(
        serde_json::json!({
            "t": "PasteImage", "session": "shell", "data_b64": base64_encode(&png)
        })
        .to_string(),
    ))
    .unwrap();

    let mut seen = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !seen.contains("\u{1b}[201~") {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(e) => panic!("terminal socket died: {e}"),
        }
    }

    // Each assertion covers a property established empirically in the spec's
    // appendix; drop any one of them and the paste silently degrades to text.
    assert!(seen.contains("\u{1b}[200~"), "missing the opening bracketed-paste marker: {seen:?}");
    assert!(seen.contains("\u{1b}[201~"), "missing the closing bracketed-paste marker: {seen:?}");
    assert!(seen.contains(".png"), "the injected path must carry an image extension: {seen:?}");
    assert!(
        seen.contains(&state.path().join("pasted").to_string_lossy().to_string()),
        "the injected path must be absolute and under the state dir: {seen:?}"
    );
}

#[test]
fn a_paste_onto_a_dead_session_is_an_error_not_a_silent_success() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_d, port) = fixture();
    let mut ws = workspace_ws(port, "proj");
    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    ws.send(tungstenite::Message::Text(
        serde_json::json!({
            "t": "PasteImage", "session": "nosuch", "data_b64": base64_encode(&png)
        })
        .to_string(),
    ))
    .unwrap();
    let msg = wait_for_event(&mut ws, "Error");
    assert!(msg.contains("no such session"), "unexpected error: {msg}");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --test integration paste`
Expected: FAIL — both time out; the intent does not exist.

- [ ] **Step 3: Add the intent**

In `src/proto.rs`, after `UploadFile`:

```rust
    /// Writes an image outside the repo and pastes its path into a live
    /// session. Deliberately not `UploadFile` plus a client-side terminal
    /// write: the scratch path is server-chosen, and a client that could name
    /// a path outside the project would hold an arbitrary-write primitive.
    PasteImage { session: String, data_b64: String },
```

- [ ] **Step 4: Handle it**

In the `handle` match:

```rust
            Intent::PasteImage { session, data_b64 } => {
                return self.do_paste_image(from, session.clone(), data_b64)
            }
```

And the method, beside `do_start_terminal`:

```rust
    /// Validates and decodes inline, then does both blocking steps — the
    /// scratch write and the PTY write — on a spawned thread.
    ///
    /// `session::write_input` drops the *registry* lock before writing, but
    /// this runs with the *hub* lock held by `wsconn::handle`, and a write to a
    /// PTY whose child has stopped draining stdin blocks indefinitely. Inline,
    /// that would hold the hub lock across blocking I/O and stall every
    /// connection for this project — the shape of the deadlock CLAUDE.md
    /// records as already shipped once.
    fn do_paste_image(&mut self, from: &ConnId, session: String, data_b64: &str) {
        if !crate::session::valid_name(&session) {
            let ev = Event::Error { msg: format!("invalid session name: {session}") };
            return self.send_to(from, &ev);
        }
        if data_b64.len() > crate::b64::encoded_len(crate::fileops::MAX_UPLOAD_BYTES) {
            let ev = Event::Error { msg: "pasted image too large".into() };
            return self.send_to(from, &ev);
        }
        if !crate::session::has_session(&self.project, &session) {
            let ev = Event::Error { msg: format!("no such session: {session}") };
            return self.send_to(from, &ev);
        }
        let bytes = match crate::b64::decode(data_b64) {
            Ok(b) => b,
            Err(e) => {
                let ev = Event::Error { msg: e };
                return self.send_to(from, &ev);
            }
        };
        // Sniffed here rather than on the thread so an unsupported format is
        // reported to the connection that asked, synchronously.
        if crate::paste::extension_of(&bytes).is_none() {
            let ev = Event::Error { msg: "clipboard image is not a PNG, JPEG, GIF or WebP".into() };
            return self.send_to(from, &ev);
        }

        let project = self.project.clone();
        let hub = self.self_ref.upgrade();
        let conn = from.clone();
        // Builder::spawn rather than thread::spawn: thread creation can fail
        // (fork/EAGAIN), and a panic from that would escape `handle` through
        // `wsconn::handle`, which has no catch_unwind — killing the browser's
        // workspace socket mid-session.
        let spawned = std::thread::Builder::new().name("paste-image".into()).spawn(move || {
            let report = |msg: String| {
                if let Some(h) = &hub {
                    let mut h = Hub::lock(h);
                    h.send_to(&conn, &Event::Error { msg });
                }
            };
            let path = match crate::paste::write_scratch_image(&project, &bytes) {
                Ok(p) => p,
                Err(e) => return report(e),
            };
            // Bracketed-paste markers are load-bearing: the same path arriving
            // as raw characters is inserted as literal text instead of being
            // read as an image. See the spec's evidence appendix.
            let mut payload = Vec::with_capacity(path.as_os_str().len() + 12);
            payload.extend_from_slice(b"\x1b[200~");
            payload.extend_from_slice(path.to_string_lossy().as_bytes());
            payload.extend_from_slice(b"\x1b[201~");
            let key = crate::session::key_for(&project, &session);
            if let Err(e) = crate::session::write_input(&key, &payload) {
                report(format!("paste failed: {e}"));
            }
        });
        if spawned.is_err() {
            let ev = Event::Error { msg: "cannot start paste worker".into() };
            self.send_to(from, &ev);
        }
    }
```

Note the `handle` arm passes `data_b64` as `&str`; adjust the borrow to match how neighbouring arms destructure (`Intent::SaveBuffer` is the closest example).

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the marker assertions can fail, and time the suite**

Remove the two `extend_from_slice` marker lines so only the bare path is written, run `cargo test --test integration a_pasted_image`, and confirm it fails on the *marker* assertion rather than passing because the session survived. Restore.

Then run `time cargo test` twice. A lock-ordering regression hangs rather than fails, so compare the wall-clock times: a run that suddenly takes tens of seconds longer is the signal, not the pass count.

- [ ] **Step 7: Commit**

```bash
git add src/proto.rs src/hub.rs tests/integration.rs
git commit -m "paste: inject a pasted image's path into its terminal"
```

---

### Task 7: The client, and the docs

Drop and paste in the browser, plus the documentation this makes stale.

**Files:**
- Modify: `static/app.js` (new section near the divider-drag handlers around line 697)
- Modify: `README.md` (the feature list)
- Modify: `docs/backlog.md:24-32` (four items this closes)

**Interfaces:**
- Consumes: the wire intents from Tasks 4 and 6; the existing `send(intent)` and `showError(msg)` helpers.
- Produces: nothing further.

- [ ] **Step 1: Add the upload client**

In `static/app.js`. Listeners are delegated at document level because the tree is an htmx fragment replaced wholesale on `TreeChanged` — which an upload itself triggers, so per-row listeners would die on the first successful drop:

```js
// Uploads: a file dropped or pasted onto the tree, or an image pasted onto a
// terminal. Delegated at document level, not bound per row: the tree is an
// htmx fragment replaced wholesale on TreeChanged, and an upload triggers
// exactly that — per-row listeners would not survive their own first success.
const MAX_UPLOAD = 8_000_000; // must match fileops::MAX_UPLOAD_BYTES

function b64encode(buf) {
  const bytes = new Uint8Array(buf);
  let s = "";
  // Chunked: String.fromCharCode(...bytes) overflows the argument limit and
  // throws on anything above a few hundred KB.
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) s += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  return btoa(s);
}

// The destination for a drop: the nearest row with a data-rel. A directory
// row contributes itself, a file row its parent. Null means the drop was not
// on the tree at all, which is what keeps the destination unambiguous.
function dropDir(target) {
  const el = target && target.closest && target.closest("[data-rel]");
  if (!el) return null;
  const rel = el.dataset.rel;
  if (el.tagName === "DETAILS") return rel;
  return rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
}

function focusedSession() {
  const host = document.activeElement && document.activeElement.closest(".termhost");
  return host ? host.dataset.session : null;
}

async function uploadFiles(files, dir) {
  for (const f of files) {
    if (f.size > MAX_UPLOAD) {
      showError(`${f.name} is too large (${f.size} bytes, limit ${MAX_UPLOAD})`);
      continue;
    }
    const data_b64 = b64encode(await f.arrayBuffer());
    send({ t: "UploadFile", rel: dir ? `${dir}/${f.name}` : f.name, data_b64 });
  }
}

async function pasteImage(file, session) {
  if (file.size > MAX_UPLOAD) {
    showError(`pasted image is too large (${file.size} bytes, limit ${MAX_UPLOAD})`);
    return;
  }
  send({ t: "PasteImage", session, data_b64: b64encode(await file.arrayBuffer()) });
}

// Without preventDefault on dragover the browser navigates to the dropped
// file instead of delivering a drop event.
document.addEventListener("dragover", (e) => {
  if (dropDir(e.target) !== null) e.preventDefault();
});

document.addEventListener("drop", (e) => {
  const dir = dropDir(e.target);
  if (dir === null) return;
  if (!e.dataTransfer || !e.dataTransfer.files.length) return;
  e.preventDefault();
  uploadFiles(e.dataTransfer.files, dir);
});

document.addEventListener("paste", (e) => {
  const files = e.clipboardData && e.clipboardData.files;
  if (!files || !files.length) return;
  const session = focusedSession();
  if (session) {
    // Only images are meaningful here; a non-image file pasted onto a terminal
    // falls through to xterm's own text handling rather than being uploaded
    // somewhere the user did not point at.
    const img = [...files].find((f) => f.type.startsWith("image/"));
    if (!img) return;
    e.preventDefault();
    pasteImage(img, session);
    return;
  }
  const dir = dropDir(document.activeElement) ?? dropDir(e.target);
  if (dir === null) return;
  e.preventDefault();
  uploadFiles(files, dir);
});
```

- [ ] **Step 2: Verify in a real browser**

Not optional and not substitutable — synthetic events prove plumbing only, and `CLAUDE.md` records four defects that a green suite could not have caught. Against the dev instance (`https://resh.<tailnet>.ts.net:8445`, or `http://127.0.0.1:8555`):

1. Drag a real file from a desktop file manager onto a **file** row; confirm it lands in that file's directory and the tree refreshes without a manual reload.
2. Drag one onto a **directory** row; confirm it lands inside that directory.
3. Drop one **outside the tree**; confirm nothing happens and the browser does not navigate away from the workspace.
4. Drop a file whose name already exists; confirm a visible error and that the original file is unchanged on disk.
5. Copy a file in the OS file manager, click a tree row, paste; confirm it uploads.
6. Copy an image (screenshot), focus a terminal running `claude`, paste; confirm it arrives as an image attachment rather than as a path in the prompt.
7. With two browsers open on the same project, confirm an upload in one appears in the other's tree.

- [ ] **Step 3: Run the suite on the Linux host**

Per the dev/prod substitution table, a macOS-only run has hidden real defects here before:

```bash
ssh <host> 'cd ~/projects/resh && cargo test'
```

- [ ] **Step 4: Update the README**

In the feature list, after the "All state lives on the server" paragraph:

```markdown
**Files go in through the browser.** Drag a file from the desktop onto the file
tree, or copy one and paste it there, and it lands in that directory — over the
same websocket everything else uses, since the HTTP side stays GET-only. Paste a
screenshot onto a terminal and it reaches the program running there as an image,
not as a path.
```

- [ ] **Step 5: Close the backlog items**

In `docs/backlog.md`, the four speculative v3-spec entries at lines 24-32 are now decided. Replace the three upload/paste ones with a single line recording the outcome, and leave the tab-reordering entry alone — it is unrelated to this work:

```markdown
- Drag-n-drop upload, copy-paste of file content, and pasting images into the
  claude terminal — **shipped**, see
  `docs/superpowers/specs/2026-08-19-file-upload-design.md`. Dropping or pasting
  a file into the tree uploads it; pasting an image onto a terminal hands the
  program a real image. Directories, download/drag-out, and a host clipboard
  bridge are the recorded non-goals.
```

- [ ] **Step 6: Commit**

```bash
git add static/app.js README.md docs/backlog.md
git commit -m "upload: drop and paste files from the browser"
```

---

## Self-Review

**Spec coverage.** Every section of the spec maps to a task: the multipart rejection is Task 4's shape (no HTTP touched); the three caps are Task 1; byte writing and confinement are Task 3; scratch location and extension gating are Task 5; the injection and its three empirical properties are Task 6; the client's delegation, resolution, and `dragover` handling are Task 7. The spec's testing section is distributed — confinement, collision, cannot-tell, bad base64, oversize, extension refusal, injection bytes, cap coherence, and the timed concurrency run each appear as a step. Browser verification and the Linux-host run are Task 7 steps 2 and 3.

**Three open questions remain open**, as the spec left them, and each has a defensible default baked in that a reviewer can overrule cheaply: collisions refuse (Task 3), scratch files are never pruned (Task 5), and dotfiles are permitted since `valid_upload_name` does not special-case a leading dot (Task 3). If review decides otherwise, only the named function changes.

**Type consistency.** `write_bytes(&Path, &str, &[u8]) -> Result<PathBuf, String>` is defined in Task 3 and called in Tasks 4 and 5 with those types. `decode(&str) -> Result<Vec<u8>, String>` and `encoded_len(usize) -> usize` are defined in Task 2 and used in Tasks 4 and 6. `write_scratch_image(&str, &[u8]) -> Result<PathBuf, String>` and `extension_of(&[u8]) -> Option<&'static str>` are defined in Task 5 and used in Task 6. `MAX_UPLOAD_BYTES` is defined once in Task 1 and referenced by name everywhere else, including the client's `MAX_UPLOAD`, which carries a comment pointing at it.

**Known rough edge for the implementer:** Task 6's `handle` arm and `do_paste_image` must agree on whether `data_b64` arrives as `&str` or `String`. `Intent::SaveBuffer`'s arm is the pattern to copy. This is a compile error, not a silent bug, so it is safe to leave to the implementer.
