# Alternate screen survives an attachment — implementation plan

> **For agentic workers:** implement task-by-task, running the tests named in
> each task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A browser attaching to a session where a full-screen app is already
running lands on the same screen buffer the app is on, so the app's exit
restores the pre-app screen instead of painting over its own leftover frame.

**Architecture:** A new `screen` module owns everything about *which* screen a
session is on. A pure byte scanner (pump thread, no lock — the same discipline
`osc::Parser` already follows) reports alternate-screen switches; a `Screens`
value (behind the registry lock, on `Session`) routes output into one ring per
buffer and builds the replay for a new attachment, synthesizing the switch
sequence that the ring can no longer be trusted to carry.

**Tech Stack:** Rust, no new dependencies. `deno` + Chromium for the browser
test that proves the chain end to end.

**Spec:** `docs/superpowers/specs/2026-08-20-alternate-screen-design.md`

## Global Constraints

- `cargo test`, never `cargo test --release`.
- Implementation first, `#[cfg(test)] mod tests` at the bottom of the same file.
- Module-level `//!` doc explaining *why* the module exists.
- Comments give rationale, not mechanics.
- Every bound exists because this parser is fed attacker-influenced bytes —
  anything written to a terminal, including `cat` of a hostile file.
- Never hold a lock across blocking I/O.
- A test that cannot fail is worse than no test: for each one, revert the fix,
  run it, read the failure, restore.

---

### Task 1: the scanner

**Files:**
- Create: `src/screen.rs`
- Modify: `src/lib.rs` (add `pub mod screen;`)
- Test: bottom of `src/screen.rs`

**Interfaces:**
- Produces: `screen::Scanner::new()`, `Scanner::feed(&mut self, &[u8]) -> Vec<Switch>`,
  `Switch { start: usize, end: usize, mode: u16, enter: bool }`.
  `start`/`end` are offsets into the chunk just fed; a sequence that began in
  an earlier chunk reports `start: 0`. `mode` is 47, 1047 or 1049 — whichever
  the app actually used.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_enter_and_an_exit_are_both_seen() {
    let mut s = Scanner::new();
    let sw = s.feed(b"before\x1b[?1049hafter");
    assert_eq!(sw.len(), 1);
    assert!(sw[0].enter);
    assert_eq!(sw[0].mode, 1049);
    assert_eq!(&b"before\x1b[?1049h"[sw[0].start..], b"\x1b[?1049h");
    assert_eq!(sw[0].end, 6 + 8);
    assert_eq!(s.feed(b"\x1b[?1049l")[0].enter, false);
}

#[test]
fn a_sequence_split_across_reads_is_still_seen() {
    // The pump reads 8 KiB at a time and a switch is under no obligation to
    // arrive whole. This is the case that makes the scanner stateful.
    let mut s = Scanner::new();
    assert!(s.feed(b"\x1b[?10").is_empty());
    let sw = s.feed(b"49hframe");
    assert_eq!(sw.len(), 1);
    assert!(sw[0].enter);
    assert_eq!((sw[0].start, sw[0].end), (0, 3));
}

#[test]
fn a_query_is_not_a_switch() {
    // DECRQM: `\e[?1049$p` asks whether the mode is set. Reading it as a set
    // would drop a client onto the alternate screen because something asked
    // a question about it.
    let mut s = Scanner::new();
    assert!(s.feed(b"\x1b[?1049$p").is_empty());
}

#[test]
fn a_mode_riding_along_with_others_is_still_seen() {
    let mut s = Scanner::new();
    let sw = s.feed(b"\x1b[?1049;1000h");
    assert_eq!(sw.len(), 1);
    assert_eq!(sw[0].mode, 1049);
}

#[test]
fn the_older_spellings_are_seen_too() {
    for (bytes, mode) in [(&b"\x1b[?47h"[..], 47u16), (&b"\x1b[?1047h"[..], 1047)] {
        let sw = Scanner::new().feed(bytes);
        assert_eq!(sw.len(), 1, "{bytes:?}");
        assert_eq!(sw[0].mode, mode);
    }
}

#[test]
fn a_switch_printed_inside_a_string_sequence_is_text_not_a_switch() {
    // A window title is a string sequence, and its payload is whatever the
    // program felt like. Scanning for `[?1049h` without tracking string state
    // lets any process that can set a title move a browser onto the alternate
    // screen.
    let mut s = Scanner::new();
    assert!(s.feed(b"\x1b]0;[?1049h\x07").is_empty());
    // ...and the parser is not stuck: a real one right after still lands.
    assert_eq!(s.feed(b"\x1b[?1049h").len(), 1);
}

#[test]
fn an_unbounded_parameter_run_is_dropped_rather_than_buffered() {
    let mut s = Scanner::new();
    let mut junk = vec![b'1'; 100_000];
    junk.splice(0..0, *b"\x1b[?");
    assert!(s.feed(&junk).is_empty());
    assert!(s.buffered_len() <= MAX_PARAMS, "buffered {}", s.buffered_len());
    junk.push(b'h');
    assert!(s.feed(b"h").is_empty(), "an over-long sequence stays dropped");
    assert_eq!(s.feed(b"\x1b[?1049h").len(), 1, "and the next real one is seen");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test screen::`
Expected: FAIL — `screen` does not exist.

- [ ] **Step 3: Implement `src/screen.rs`'s scanner**

State machine: `Ground -> Esc -> Csi(params, intermediates) -> dispatch`, plus
a string state for OSC/DCS/SOS/PM/APC so their payloads are text. Dispatch only
on a final `h`/`l` with a `?` private marker and no intermediate bytes.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test screen::`

- [ ] **Step 5: Commit**

```bash
git add src/screen.rs src/lib.rs
git commit -m "screen: scan the stream for alternate-screen switches"
```

---

### Task 2: two rings and the replay

**Files:**
- Modify: `src/screen.rs`
- Test: bottom of `src/screen.rs`

**Interfaces:**
- Produces: `screen::Screens::new()`,
  `Screens::ingest(&mut self, chunk: &[u8], switches: &[Switch]) -> Vec<u8>`
  (returns the bytes to fan out to attached clients),
  `Screens::replay(&self) -> Vec<u8>`, `screen::MAX_SCROLLBACK`.
- `MAX_SCROLLBACK` and `push_scrollback` move here from `session.rs`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_client_attaching_mid_app_is_put_on_the_alternate_screen() {
    // The whole bug: the app declares the alternate screen once, and that
    // declaration ages out of a 1 MB ring long before the app exits.
    let mut sc = Screens::new();
    feed(&mut sc, b"shell history\n");
    feed(&mut sc, b"\x1b[?1049hframe");
    let replay = sc.replay();
    let at = replay.windows(8).position(|w| w == b"\x1b[?1049h").expect("a switch in the replay");
    assert!(replay[..at].ends_with(b"shell history\n"), "normal screen first");
    assert!(replay[at + 8..].ends_with(b"frame"), "then the alternate screen's content");
}

#[test]
fn the_app_does_not_evict_the_screen_it_will_return_to() {
    // One shared ring means a full-screen app pushes the user's own shell
    // history out of the replay within a minute or two of running.
    let mut sc = Screens::new();
    feed(&mut sc, b"shell history\n");
    feed(&mut sc, b"\x1b[?1049h");
    let frame = vec![b'f'; 10_000];
    for _ in 0..(MAX_SCROLLBACK / 10_000 + 10) {
        feed(&mut sc, &frame);
    }
    let replay = sc.replay();
    assert!(
        replay.windows(13).any(|w| w == b"shell history"),
        "the pre-app screen survived an app that outran the ring"
    );
}

#[test]
fn leaving_the_alternate_screen_drops_its_content_and_its_switch() {
    let mut sc = Screens::new();
    feed(&mut sc, b"shell history\n");
    feed(&mut sc, b"\x1b[?1049hframe\x1b[?1049l");
    let replay = sc.replay();
    assert!(!replay.windows(5).any(|w| w == b"frame"), "the frame is gone");
    assert!(!replay.windows(7).any(|w| w == b"\x1b[?1049"), "and so is every switch");
    assert!(replay.ends_with(b"shell history\n"));
}

#[test]
fn a_second_run_does_not_show_the_first_runs_frame() {
    let mut sc = Screens::new();
    feed(&mut sc, b"\x1b[?1049hfirst\x1b[?1049l");
    feed(&mut sc, b"\x1b[?1049hsecond");
    let replay = sc.replay();
    assert!(!replay.windows(5).any(|w| w == b"first"));
    assert!(replay.ends_with(b"second"));
}

#[test]
fn an_exit_with_no_entry_this_process_is_reconciled_rather_than_forwarded() {
    // After a restart the ring is empty and the app's entry is unrecoverable,
    // so every attached client is on the normal buffer. Forwarding the exit
    // verbatim is what paints the banner over the leftover frame.
    let mut sc = Screens::new();
    let out = feed(&mut sc, b"\x1b[?1049lResume this session with:");
    assert!(!out.windows(8).any(|w| w == b"\x1b[?1049"), "the exit was not forwarded");
    assert!(out.starts_with(b"\x1b[H\x1b[2J"), "the screen was cleared instead: {out:?}");
    assert!(out.ends_with(b"Resume this session with:"));
}

#[test]
fn an_exit_after_an_entry_is_forwarded_untouched() {
    // The reconciliation above is only ever right while the entry is unknown.
    // An app that leaves and re-enters must not have its screen cleared.
    let mut sc = Screens::new();
    feed(&mut sc, b"\x1b[?1049h");
    let out = feed(&mut sc, b"\x1b[?1049lback");
    assert_eq!(out, b"\x1b[?1049lback".to_vec());
}
```

with, above them:

```rust
/// The pump's two steps in one call: scan outside the lock, apply inside it.
fn feed(sc: &mut Screens, chunk: &[u8]) -> Vec<u8> {
    let switches = sc.test_scanner().feed(chunk);
    sc.ingest(chunk, &switches)
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test screen::`

- [ ] **Step 3: Implement `Screens`**

Two `VecDeque<u8>` rings, each bounded by `MAX_SCROLLBACK`; a `Buffer` state of
`Unknown | Normal | Alt(u16)`; `ingest` walks the switches, routing each span
into the ring for the buffer that was active while it was written, then applies
the switch (entering clears the alternate ring, leaving drops it). `replay`
emits the normal ring, then — when on the alternate screen — a synthesized
`ESC[?<mode>h` followed by the alternate ring.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test screen::`

- [ ] **Step 5: Commit**

```bash
git add src/screen.rs
git commit -m "screen: one ring per buffer, and a replay that re-declares the screen"
```

---

### Task 3: wire it into the session, and prove it in a browser

**Files:**
- Modify: `src/session.rs` (the `Session` struct, the pump thread, `attach`)
- Modify: `CLAUDE.md` (the caps line: the ring is now per buffer)
- Test: `tests/browser/altscreen.mjs` (already written; flips to green)

**Interfaces:**
- Consumes: `screen::Screens`, `screen::Scanner`.
- `Session::scrollback: VecDeque<u8>` becomes `Session::screens: Screens`.
  `session::MAX_SCROLLBACK` and `session::push_scrollback` are gone; the
  `scrollback_ring_is_bounded` test moves to `screen.rs`.

- [ ] **Step 1: Wire the pump**

The scanner is per-pump-thread state next to `osc::Parser`, scanned before the
lock is taken; the routing happens under it, and the fan-out sends what
`ingest` returns rather than the raw chunk.

- [ ] **Step 2: Wire `attach`**

`let replay = s.screens.replay();` in place of collecting the old ring.

- [ ] **Step 3: Run the Rust suite**

Run: `cargo test`
Expected: PASS, with no reference to `push_scrollback` left in `session.rs`.

- [ ] **Step 4: Run the browser reproduction**

Run: `deno run -A tests/browser/altscreen.mjs`
Expected: ALL PASS — in particular "the replay put the browser back on the
alternate screen the app is still on" and "the pre-app screen came back".

- [ ] **Step 5: Prove the test could fail**

Revert the fix (`git stash` the `session.rs` wiring alone), re-run the browser
test, confirm the same three assertions fail, restore. A green run against a
reverted fix means the test is measuring something else.

- [ ] **Step 6: Run the rest of the browser suite**

Run: `deno run -A tests/browser/reconnect.mjs`
Expected: ALL PASS — the replay path it covers is the one this task rewrote.

- [ ] **Step 7: Commit**

```bash
git add src/session.rs CLAUDE.md tests/browser/altscreen.mjs docs/superpowers
git commit -m "session: replay the screen a client is attaching to, not just the bytes"
```
