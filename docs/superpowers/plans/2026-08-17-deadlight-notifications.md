# deadlight Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a process in a deadlight terminal — Claude, primarily — raise a
notification that reaches the user's OS even when the deadlight tab is in the
background, and that clicks back to the exact terminal that needs attention.

**Architecture:** An escape sequence emitted into the PTY is parsed by the
existing pump thread, stored in a bounded persisted server-side ring, and
broadcast to every connected workspace socket across all projects. The browser
shows it through a service worker, so a later Web Push addition needs no
rewrite of the client.

**Tech Stack:** Rust 2021, no new crates. serde/serde_json (already present),
portable-pty, tungstenite. Frontend: plain JS, no framework, no build step.

**Spec:** `docs/superpowers/specs/2026-08-17-deadlight-notifications-design.md`

## Global Constraints

Copied from `CLAUDE.md` and the spec. Every task's requirements include these.

- **Never hold a lock across blocking I/O.** The notice store's mutex is a leaf
  lock: never held across a broadcast, a filesystem write, or a hub lock.
- **No panics may escape a socket, watcher, or PTY pump thread.** The parser
  and the store must degrade, never `unwrap` on external input.
- **HTTP stays GET-only.** `/sw.js` is a GET static asset. No new verbs.
- **All HTML is built in Rust in `render.rs`; escape everything interpolated**
  via `esc()`. In JS, notice text uses `textContent`, never `innerHTML`.
- **Module-level `//!` doc** explaining *why* the module exists, on every new
  module. Comments give rationale, not mechanics.
- **Implementation first, `#[cfg(test)] mod tests` at the bottom of the same
  file.** Integration tests go in `tests/integration.rs`.
- **`cargo test`, never `cargo test --release`.**
- **Session names match `^[A-Za-z0-9_-]{1,32}$`** (`session::valid_name`).
- Caps, exact values: 100 retained notices globally; title 100 chars; body 500
  chars; 10 notices per session per rolling 60s; 4096-byte in-flight OSC
  sequence.
- **Tests must be able to fail.** Before committing a test, ask whether
  deleting the code under test would break it. Negative tests assert on *why*,
  not just `is_err()`.
- Work happens in the worktree at
  `/Users/peter/Projects/deadlight/.claude/worktrees/notifications` on branch
  `notifications`. Bash starts in the main checkout — use absolute paths or
  `cd` first.

---

### Task 1: OSC parser

**Files:**
- Create: `src/osc.rs`
- Modify: `src/lib.rs` (add `pub mod osc;`)
- Test: `src/osc.rs` (`#[cfg(test)] mod tests` at the bottom)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub struct Parsed { pub title: Option<String>, pub body: String }
  pub struct Parser { /* private */ }
  impl Parser {
      pub fn new() -> Parser;
      pub fn feed(&mut self, chunk: &[u8]) -> Vec<Parsed>;
      #[cfg(test)] pub fn buffered_len(&self) -> usize;
  }
  pub const MAX_SEQUENCE: usize = 4096;
  pub const MAX_TITLE: usize = 100;
  pub const MAX_BODY: usize = 500;
  ```

Background for the implementer: an OSC ("operating system command") escape
sequence looks like `ESC ] <payload> <terminator>`, where `ESC` is `0x1b`, `]`
is `0x5d`, and the terminator is either `BEL` (`0x07`) or `ST` (`ESC \`, i.e.
`0x1b 0x5c`). Terminals use them for out-of-band messages — window titles,
clipboard, and notifications. We accept two payload shapes:

- `777;notify;TITLE;BODY` — the urxvt/kitty/dunst convention
- `9;BODY` — the iTerm2 one-arg form

Only the *first three* `;` in the 777 form are structural, so a `;` inside
BODY stays literal.

- [ ] **Step 1: Write the failing tests**

Create `src/osc.rs` with only the test module and the type signatures it
needs, so the file compiles once the implementation lands.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn one(bytes: &[u8]) -> Parsed {
        let mut p = Parser::new();
        let mut out = p.feed(bytes);
        assert_eq!(out.len(), 1, "expected exactly one notice from {bytes:?}");
        out.pop().unwrap()
    }

    #[test]
    fn parses_the_777_form() {
        let n = one(b"\x1b]777;notify;Build done;42 tests passed\x07");
        assert_eq!(n.title.as_deref(), Some("Build done"));
        assert_eq!(n.body, "42 tests passed");
    }

    #[test]
    fn parses_the_iterm_form_without_a_title() {
        let n = one(b"\x1b]9;needs your input\x07");
        assert_eq!(n.title, None);
        assert_eq!(n.body, "needs your input");
    }

    #[test]
    fn accepts_st_as_well_as_bel() {
        let n = one(b"\x1b]9;done\x1b\\");
        assert_eq!(n.body, "done");
    }

    #[test]
    fn semicolons_inside_the_body_are_literal() {
        let n = one(b"\x1b]777;notify;t;a;b;c\x07");
        assert_eq!(n.title.as_deref(), Some("t"));
        assert_eq!(n.body, "a;b;c", "only the first three ';' are structural");
    }

    // The reason this parser is stateful at all. A stateless implementation
    // passes every test above and fails this one.
    #[test]
    fn a_sequence_split_at_any_offset_still_parses() {
        let seq = b"\x1b]777;notify;Title here;Body here\x07";
        for cut in 1..seq.len() {
            let mut p = Parser::new();
            let mut got = p.feed(&seq[..cut]);
            got.extend(p.feed(&seq[cut..]));
            assert_eq!(got.len(), 1, "split at {cut} produced {} notices", got.len());
            assert_eq!(got[0].body, "Body here", "split at {cut}");
            assert_eq!(got[0].title.as_deref(), Some("Title here"), "split at {cut}");
        }
    }

    #[test]
    fn surrounding_terminal_output_passes_through_without_confusing_the_parser() {
        let mut p = Parser::new();
        let got = p.feed(b"$ cargo test\r\n\x1b]9;done\x07ok\r\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "done");
        assert_eq!(p.buffered_len(), 0, "trailing plain output must not be buffered");
    }

    // The bug being prevented is unbounded accumulation, not a spurious
    // notice — so assert the buffer drained, not merely that nothing came out.
    #[test]
    fn an_overlong_sequence_is_abandoned_and_drains_the_buffer() {
        let mut p = Parser::new();
        let mut junk = Vec::from(&b"\x1b]777;notify;t;"[..]);
        junk.extend(std::iter::repeat(b'x').take(MAX_SEQUENCE + 100));
        let got = p.feed(&junk);
        assert!(got.is_empty(), "overlong sequence must not emit");
        assert_eq!(p.buffered_len(), 0, "overlong sequence must not stay buffered");
    }

    #[test]
    fn a_newline_abandons_an_in_flight_sequence() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]777;notify;t;oops\nplain text\r\n");
        assert!(got.is_empty(), "real OSC never contains a newline");
        assert_eq!(p.buffered_len(), 0);
    }

    #[test]
    fn binary_noise_containing_esc_bracket_yields_nothing_and_no_state() {
        let mut p = Parser::new();
        let noise: Vec<u8> = (0u8..=255).chain(b"\x1b]".iter().copied()).chain(0u8..=255).collect();
        let got = p.feed(&noise);
        assert!(got.is_empty(), "binary noise must not produce notices");
        assert_eq!(p.buffered_len(), 0, "noise contains newlines, which must drain the buffer");
    }

    // Assert the sanitised *result*, not that sanitising was attempted.
    // Non-ESC control bytes: an ESC inside a sequence ends it (see the next
    // test), so no ESC can ever reach `sanitise` to be stripped.
    #[test]
    fn control_characters_are_stripped_from_the_fields() {
        let n = one(b"\x1b]777;notify;Ti\x01tle;bo\x02dy\x1b\\");
        assert_eq!(n.title.as_deref(), Some("Title"), "control byte left in the title");
        assert_eq!(n.body, "body");
    }

    // Real terminals end an OSC at any ESC that is not the ST pair `ESC \`.
    // Following that convention is also what keeps this parser bounded: there
    // is no state in which it consumes input without either buffering it
    // against MAX_SEQUENCE or ending the sequence outright. Any "skip the
    // ANSI sequence and carry on" cleverness reintroduces exactly that
    // unbounded state — see the wedge test below.
    #[test]
    fn an_esc_inside_a_sequence_abandons_it() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]777;notify;Ti\x1b[31mtle;body\x07");
        assert!(got.is_empty(), "ESC inside a sequence must abandon it");
        assert_eq!(p.buffered_len(), 0);
    }

    // No look-ahead: a sequence ends at its own first terminator, and a later
    // unrelated ST elsewhere in the chunk must not retroactively demote it.
    #[test]
    fn the_first_terminator_wins_even_if_another_appears_later() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]9;first\x07 trailing \x1b\\ more \x1b]9;second\x07");
        assert_eq!(got.len(), 2, "each sequence ends at its own first terminator");
        assert_eq!(got[0].body, "first");
        assert_eq!(got[1].body, "second");
    }

    // Regression: a stray CSI intro inside a sequence must not put the parser
    // into a state that consumes unbounded input, swallows the next
    // sequence's prefix, or fabricates a notice by merging two sequences.
    #[test]
    fn an_unterminated_csi_cannot_wedge_the_parser() {
        let mut p = Parser::new();
        let mut junk = Vec::from(&b"\x1b]9;AAA\x1b["[..]);
        junk.extend(std::iter::repeat(b'9').take(MAX_SEQUENCE * 10));
        junk.extend_from_slice(b"\x1b]9;real\x07");
        let got = p.feed(&junk);
        assert_eq!(got.len(), 1, "exactly the well-formed sequence, got {got:?}");
        assert_eq!(got[0].body, "real", "no bytes from the abandoned sequence may leak in");
        assert_eq!(p.buffered_len(), 0);
    }

    #[test]
    fn oversized_fields_are_truncated_to_the_caps() {
        let mut seq = Vec::from(&b"\x1b]777;notify;"[..]);
        seq.extend(std::iter::repeat(b'T').take(300));
        seq.push(b';');
        seq.extend(std::iter::repeat(b'B').take(900));
        seq.push(0x07);
        let n = one(&seq);
        assert_eq!(n.title.as_deref().unwrap().chars().count(), MAX_TITLE);
        assert_eq!(n.body.chars().count(), MAX_BODY);
    }

    #[test]
    fn invalid_utf8_does_not_panic_and_yields_a_lossy_body() {
        let n = one(b"\x1b]9;caf\xff\x07");
        assert!(n.body.starts_with("caf"), "got {:?}", n.body);
    }

    #[test]
    fn two_sequences_in_one_chunk_both_parse() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]9;one\x07 middle \x1b]9;two\x07");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].body, "one");
        assert_eq!(got[1].body, "two");
    }

    #[test]
    fn an_unrelated_osc_code_is_ignored() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]0;window title\x07"); // OSC 0 = set title
        assert!(got.is_empty(), "OSC 0 is not a notification");
        assert_eq!(p.buffered_len(), 0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test osc
```

Expected: compile error — `Parser`, `Parsed`, `MAX_SEQUENCE` not found. That
is a legitimate "fails first"; do not proceed until you have seen it.

- [ ] **Step 3: Implement the parser**

Write this above the test module in `src/osc.rs`:

```rust
//! Parses desktop-notification escape sequences out of a PTY byte stream.
//!
//! Terminals carry out-of-band messages as OSC sequences (`ESC ] … BEL`), and
//! notification sequences are the convention every other terminal already
//! implements — which is why deadlight accepts them rather than inventing an
//! ingress of its own: anything that can already notify iTerm2 or kitty
//! notifies deadlight unchanged, with no knowledge that deadlight exists.
//!
//! Stateful because a sequence can straddle a read boundary; the pump reads
//! 8 KiB at a time and a notification is under no obligation to arrive whole.
//! Every bound here exists because this parser is fed attacker-influenced
//! bytes — anything written to a terminal, including `cat` of a hostile file.

pub const MAX_SEQUENCE: usize = 4096;
pub const MAX_TITLE: usize = 100;
pub const MAX_BODY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub title: Option<String>,
    pub body: String,
}

#[derive(Default)]
pub struct Parser {
    /// Bytes of an in-flight sequence, payload only (after `ESC ]`).
    buf: Vec<u8>,
    /// Inside a sequence, i.e. `ESC ]` seen and no terminator yet.
    in_seq: bool,
    /// Last byte was `ESC` — could open a sequence, or close one as `ESC \`.
    esc: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    #[cfg(test)]
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Parsed> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.in_seq {
                // ESC inside a sequence is only meaningful as the ST pair
                // `ESC \`; a bare ESC starts a new sequence, abandoning this
                // one, which is what a real terminal does too.
                if self.esc {
                    self.esc = false;
                    if b == b'\\' {
                        self.finish(&mut out);
                        continue;
                    }
                    if b == b']' {
                        self.buf.clear();
                        continue;
                    }
                    self.reset();
                    continue;
                }
                match b {
                    0x1b => self.esc = true,
                    0x07 => self.finish(&mut out),
                    // Real OSC never spans a line. Bailing here is what stops
                    // `cat` of a binary file from parking bytes in `buf`
                    // until MAX_SEQUENCE, once per stray `ESC ]`.
                    b'\n' | b'\r' => self.reset(),
                    _ => {
                        self.buf.push(b);
                        if self.buf.len() > MAX_SEQUENCE {
                            self.reset();
                        }
                    }
                }
            } else if self.esc {
                self.esc = false;
                if b == b']' {
                    self.in_seq = true;
                    self.buf.clear();
                } else if b == 0x1b {
                    self.esc = true; // ESC ESC — still pending
                }
            } else if b == 0x1b {
                self.esc = true;
            }
        }
        out
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.in_seq = false;
        self.esc = false;
    }

    fn finish(&mut self, out: &mut Vec<Parsed>) {
        let payload = String::from_utf8_lossy(&self.buf).into_owned();
        self.reset();
        if let Some(p) = parse_payload(&payload) {
            out.push(p);
        }
    }
}

/// `777;notify;TITLE;BODY` or `9;BODY`. Anything else is some other OSC code
/// (window title, clipboard) and is not ours.
fn parse_payload(payload: &str) -> Option<Parsed> {
    if let Some(rest) = payload.strip_prefix("777;notify;") {
        // splitn(2): only the delimiter between TITLE and BODY is structural,
        // so a body may contain ';' without escaping.
        let mut it = rest.splitn(2, ';');
        let title = it.next().unwrap_or("");
        let body = it.next().unwrap_or("");
        return Some(Parsed {
            title: Some(sanitise(title, MAX_TITLE)),
            body: sanitise(body, MAX_BODY),
        });
    }
    if let Some(body) = payload.strip_prefix("9;") {
        return Some(Parsed { title: None, body: sanitise(body, MAX_BODY) });
    }
    None
}

/// Strips C0/C1 controls and truncates. The text reaches a browser and an OS
/// notification centre, and it arrived over a terminal, so it is untrusted:
/// leaving an ESC in would let a notification re-colour or reposition
/// whatever renders it.
fn sanitise(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(*c, '\u{80}'..='\u{9f}'))
        .take(max)
        .collect()
}
```

Add to `src/lib.rs` alongside the other `pub mod` lines:

```rust
pub mod osc;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test osc
```

Expected: PASS, 17 tests.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/osc.rs src/lib.rs
git commit -m "osc: parse notification escape sequences out of a PTY stream"
```

---

### Task 2: Notice store

**Files:**
- Create: `src/notify.rs`
- Modify: `src/lib.rs` (add `pub mod notify;`)
- Test: `src/notify.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `osc::Parsed` from Task 1. `wsstate::state_dir()` for the
  persistence location, and `wsstate::STATE_ENV_LOCK` in tests.
- Produces:
  ```rust
  pub struct Notice {
      pub id: u64, pub project: String, pub session: String,
      pub title: String, pub body: String, pub at: u64, pub read: bool,
  }
  pub const MAX_NOTICES: usize = 100;
  pub const RATE_LIMIT_PER_MIN: usize = 10;

  pub fn record(project: &str, session: &str, p: crate::osc::Parsed) -> Option<Notice>;
  pub fn list() -> Vec<Notice>;
  pub fn mark_read(id: u64);
  pub fn clear();
  pub fn load();
  #[cfg(test)] pub fn reset_for_test();
  ```

`record` is the store half only: it assigns the id, applies the rate limit,
evicts, persists, and returns the notice to broadcast (or `None` if rate
limited). Task 4 adds the broadcast on top. Splitting it this way keeps the
store's mutex a leaf lock — it is never held while a hub lock is taken.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::Parsed;

    fn parsed(body: &str) -> Parsed {
        Parsed { title: Some("t".into()), body: body.into() }
    }

    /// Every test here mutates process-global state (the store and
    /// DEADLIGHT_STATE_DIR); cargo runs tests in parallel threads.
    fn setup() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        let g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        reset_for_test();
        (g, d)
    }

    #[test]
    fn record_assigns_increasing_ids_and_keeps_server_side_attribution() {
        let (_g, _d) = setup();
        let a = record("karpie", "claude", parsed("one")).unwrap();
        let b = record("deadlight", "shell", parsed("two")).unwrap();
        assert!(b.id > a.id, "ids must increase: {} then {}", a.id, b.id);
        assert_eq!(a.project, "karpie");
        assert_eq!(a.session, "claude");
        assert_eq!(b.project, "deadlight");
        assert!(!a.read);
        assert_eq!(list().len(), 2);
    }

    #[test]
    fn the_ring_evicts_oldest_first() {
        let (_g, _d) = setup();
        for i in 0..MAX_NOTICES + 5 {
            // A fresh session name each time, or the rate limiter would fire.
            record("p", &format!("s{i}"), parsed(&format!("n{i}")));
        }
        let all = list();
        assert_eq!(all.len(), MAX_NOTICES, "ring must stay capped");
        assert_eq!(all.first().unwrap().body, "n5", "oldest five must be gone");
        assert_eq!(all.last().unwrap().body, format!("n{}", MAX_NOTICES + 4));
    }

    #[test]
    fn the_rate_limiter_admits_the_cap_then_rejects_and_counts() {
        let (_g, _d) = setup();
        for i in 0..RATE_LIMIT_PER_MIN {
            assert!(record("p", "loop", parsed(&format!("n{i}"))).is_some(), "notice {i} rejected");
        }
        assert!(record("p", "loop", parsed("over")).is_none(), "the 11th must be dropped");
        assert!(record("p", "loop", parsed("over2")).is_none());
        // Another session is unaffected — the limit is per session, not global.
        assert!(record("p", "other", parsed("fine")).is_some());
        assert_eq!(list().len(), RATE_LIMIT_PER_MIN + 1);
    }

    #[test]
    fn suppressed_notices_are_reported_on_the_next_admitted_one() {
        let (_g, _d) = setup();
        for i in 0..RATE_LIMIT_PER_MIN {
            record("p", "loop", parsed(&format!("n{i}")));
        }
        record("p", "loop", parsed("dropped1"));
        record("p", "loop", parsed("dropped2"));
        expire_window_for_test("p", "loop");
        let n = record("p", "loop", parsed("back")).unwrap();
        assert!(n.body.contains("back"), "original body must survive: {:?}", n.body);
        assert!(n.body.contains('2'), "must report 2 suppressed: {:?}", n.body);
    }

    #[test]
    fn mark_read_and_clear_change_what_list_returns() {
        let (_g, _d) = setup();
        let a = record("p", "s1", parsed("one")).unwrap();
        record("p", "s2", parsed("two"));
        mark_read(a.id);
        let all = list();
        assert!(all.iter().find(|n| n.id == a.id).unwrap().read);
        assert!(!all.iter().find(|n| n.id != a.id).unwrap().read, "only the named id");
        clear();
        assert!(list().is_empty());
    }

    #[test]
    fn state_survives_a_reload_including_ids_and_read_flags() {
        let (_g, _d) = setup();
        let a = record("karpie", "claude", parsed("survive me")).unwrap();
        record("karpie", "shell", parsed("second"));
        mark_read(a.id);
        reset_for_test(); // drops the in-memory store, keeps the file
        assert!(list().is_empty(), "reset must really empty the store, or this proves nothing");
        load();
        let all = list();
        assert_eq!(all.len(), 2, "both notices must come back");
        assert_eq!(all[0].id, a.id, "ids must survive");
        assert!(all[0].read, "read flag must survive");
        assert_eq!(all[0].body, "survive me");
        // A new notice must not reuse a persisted id.
        let c = record("karpie", "third", parsed("after reload")).unwrap();
        assert!(c.id > all[1].id, "id counter must resume past the loaded max");
    }

    #[test]
    fn a_corrupt_state_file_is_ignored_rather_than_fatal() {
        let (_g, d) = setup();
        std::fs::write(d.path().join("notifications.json"), b"{ not json").unwrap();
        load(); // must not panic
        assert!(list().is_empty());
        assert!(record("p", "s", parsed("still works")).is_some());
    }

    #[test]
    fn expired_rate_limit_windows_are_evicted() {
        let (_g, _d) = setup();
        record("p", "gone", parsed("one")).unwrap();
        assert_eq!(window_count(), 1);
        expire_window_for_test("p", "gone");
        record("p", "here", parsed("two")).unwrap();
        assert_eq!(window_count(), 1, "the expired window must be evicted, not accumulate");

        // But an expired window still holding an undelivered suppression
        // count must survive, or the count it exists to report is lost.
        for i in 0..RATE_LIMIT_PER_MIN {
            record("p", "loud", parsed(&format!("n{i}")));
        }
        record("p", "loud", parsed("dropped"));
        expire_window_for_test("p", "loud");
        record("p", "other", parsed("three")).unwrap(); // triggers a prune
        let n = record("p", "loud", parsed("back")).unwrap();
        assert!(n.body.contains('1'), "suppression count must survive the prune: {:?}", n.body);
    }

    #[test]
    fn persisted_state_is_not_readable_by_other_users() {
        let (_g, d) = setup();
        record("p", "s", parsed("private terminal output")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = std::fs::metadata(d.path().join("notifications.json")).unwrap();
            assert_eq!(f.permissions().mode() & 0o077, 0, "notifications.json is group/world readable");
            let dir = std::fs::metadata(d.path()).unwrap();
            assert_eq!(dir.permissions().mode() & 0o077, 0, "state dir is group/world readable");
        }
    }

    #[test]
    fn a_missing_title_falls_back_to_the_session_name() {
        let (_g, _d) = setup();
        let n = record("p", "claude", Parsed { title: None, body: "hi".into() }).unwrap();
        assert_eq!(n.title, "claude");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test notify::
```

Expected: compile error — `record`, `Notice`, `reset_for_test` not found.

- [ ] **Step 3: Implement the store**

```rust
//! Server-side notice store. Bounded, persisted, and global rather than
//! per-project: what needs your attention is a property of the machine, not
//! of whichever project happens to be on screen, and a browser sitting on one
//! project must still learn that another one wants something.
//!
//! Persistence — not just in-memory queueing — is what makes a notice raised
//! at 3am survive a deadlight restart rather than only a closed tab.
//!
//! The mutex here is a leaf lock: taken for bookkeeping, released before any
//! broadcast or hub lock. `record` deliberately does not broadcast for that
//! reason; see `notify_and_broadcast` in hub.rs.
use crate::osc::Parsed;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_NOTICES: usize = 100;
pub const RATE_LIMIT_PER_MIN: usize = 10;
const WINDOW_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub id: u64,
    pub project: String,
    pub session: String,
    pub title: String,
    pub body: String,
    pub at: u64,
    pub read: bool,
}

#[derive(Default)]
struct Window {
    started: u64,
    count: usize,
    suppressed: usize,
}

#[derive(Default)]
struct Store {
    notices: VecDeque<Notice>,
    next_id: u64,
    windows: HashMap<String, Window>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path() -> std::path::PathBuf {
    crate::wsstate::state_dir().join("notifications.json")
}

/// Assign an id, apply the rate limit, evict, persist. Returns the notice to
/// broadcast, or `None` when the session is over its limit. Never panics:
/// this runs on the PTY pump thread, which must keep pumping regardless.
pub fn record(project: &str, session: &str, p: Parsed) -> Option<Notice> {
    let ts = now();
    let notice = {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        // Bound the rate-limit map by activity rather than by uptime. An
        // entry whose window has expired and that carries no undelivered
        // suppression count has nothing left to say. Without this, the map
        // gains one permanent entry per session name ever seen — and this
        // server's whole job is spinning worktree sessions up and down, so
        // that is unbounded in practice, not merely in theory. Everything
        // else in deadlight is capped; this must be too.
        s.windows.retain(|_, w| ts.saturating_sub(w.started) < WINDOW_SECS || w.suppressed > 0);
        let key = format!("{project}/{session}");
        let w = s.windows.entry(key).or_default();
        if ts.saturating_sub(w.started) >= WINDOW_SECS {
            // A new window resets the allowance but carries the suppression
            // count across. Counting drops exists solely to tell the user
            // they happened, and the next admitted notice is the first
            // chance to say so — zeroing here would silently discard the
            // very thing the count is for.
            let carried = w.suppressed;
            *w = Window { started: ts, count: 0, suppressed: carried };
        }
        if w.count >= RATE_LIMIT_PER_MIN {
            // Dropped, but counted: a runaway loop must not be able to evict
            // the ring, and the user should learn that it happened.
            w.suppressed += 1;
            return None;
        }
        w.count += 1;
        let suppressed = std::mem::take(&mut w.suppressed);

        s.next_id += 1;
        let mut body = p.body;
        if suppressed > 0 {
            body = format!("{body} ({suppressed} suppressed)");
        }
        let n = Notice {
            id: s.next_id,
            project: project.to_string(),
            session: session.to_string(),
            title: p.title.unwrap_or_else(|| session.to_string()),
            body,
            at: ts,
            read: false,
        };
        s.notices.push_back(n.clone());
        while s.notices.len() > MAX_NOTICES {
            s.notices.pop_front();
        }
        n
    };
    persist();
    Some(notice)
}

pub fn list() -> Vec<Notice> {
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    s.notices.iter().cloned().collect()
}

pub fn mark_read(id: u64) {
    {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = s.notices.iter_mut().find(|n| n.id == id) {
            n.read = true;
        }
    }
    persist();
}

pub fn clear() {
    {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        s.notices.clear();
    }
    persist();
}

/// Read-then-rename, like wsstate::save: a crash mid-write must leave the old
/// file intact rather than a half-written one. Failures are logged, never
/// propagated — notifications are best-effort, and a full disk must not take
/// down a terminal.
fn persist() {
    let snapshot: Vec<Notice> = list();
    let dir = crate::wsstate::state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return eprintln!("deadlight: notifications dir: {e}");
    }
    // The same hardening wsstate::save applies, and for a sharper reason:
    // a notice body is terminal output, so it must not be readable by other
    // local users. Done here rather than relying on some earlier
    // wsstate::save having already tightened the directory — a notice can
    // be recorded before any workspace has ever been saved.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let Ok(json) = serde_json::to_string(&snapshot) else { return };
    let tmp = path().with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
        return eprintln!("deadlight: notifications write: {e}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path()) {
        eprintln!("deadlight: notifications rename: {e}");
    }
}

/// Called once at startup. A corrupt or absent file leaves an empty store —
/// losing notification history is not worth failing a boot over.
pub fn load() {
    let Ok(text) = std::fs::read_to_string(path()) else { return };
    let Ok(list) = serde_json::from_str::<Vec<Notice>>(&text) else {
        return eprintln!("deadlight: notifications.json unreadable, starting empty");
    };
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    // Resume the counter past the highest persisted id, or a reboot would
    // hand out ids that already name a different notice.
    s.next_id = list.iter().map(|n| n.id).max().unwrap_or(0);
    s.notices = list.into();
}

/// Lets the eviction test observe the map that is supposed to stay bounded.
#[cfg(test)]
pub fn window_count() -> usize {
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    s.windows.len()
}

#[cfg(test)]
pub fn reset_for_test() {
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    *s = Store::default();
}

/// Ages out a session's rate-limit window so a test can cross it without
/// sleeping 60 seconds.
#[cfg(test)]
pub fn expire_window_for_test(project: &str, session: &str) {
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = s.windows.get_mut(&format!("{project}/{session}")) {
        w.started = 0;
    }
}
```

Add `pub mod notify;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test notify::
```

Expected: PASS, 10 tests.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/notify.rs src/lib.rs
git commit -m "notify: bounded persisted notice store with a per-session rate limit"
```

---

### Task 3: Wire types

**Files:**
- Modify: `src/proto.rs`
- Test: `src/proto.rs` (extend the existing `mod tests`)

**Interfaces:**
- Consumes: `notify::Notice` from Task 2.
- Produces:
  ```rust
  Event::Notice  { notice: crate::notify::Notice }
  Event::Notices { list: Vec<crate::notify::Notice> }
  Intent::MarkNoticeRead { id: u64 }
  Intent::ClearNotices
  ```

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/proto.rs`:

```rust
    #[test]
    fn decodes_the_notice_intents() {
        let i = decode(r#"{"t":"MarkNoticeRead","id":7}"#).unwrap();
        assert!(matches!(i, Intent::MarkNoticeRead { id: 7 }));
        let i = decode(r#"{"t":"ClearNotices"}"#).unwrap();
        assert!(matches!(i, Intent::ClearNotices));
    }

    #[test]
    fn encodes_a_notice_with_its_attribution() {
        let n = crate::notify::Notice {
            id: 3,
            project: "karpie/src".into(),
            session: "claude".into(),
            title: "done".into(),
            body: "42 tests".into(),
            at: 1_700_000_000,
            read: false,
        };
        let s = encode(&Event::Notice { notice: n.clone() });
        assert!(s.contains(r#""t":"Notice""#));
        // Attribution must reach the client, or it cannot route a click.
        assert!(s.contains(r#""project":"karpie/src""#), "got {s}");
        assert!(s.contains(r#""session":"claude""#), "got {s}");
        assert!(s.contains(r#""id":3"#), "got {s}");
        let s = encode(&Event::Notices { list: vec![n] });
        assert!(s.contains(r#""t":"Notices""#));
        assert!(s.contains(r#""list":["#), "got {s}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test proto::
```

Expected: compile error — no `Intent::MarkNoticeRead`, no `Event::Notice`.

- [ ] **Step 3: Add the variants**

In `src/proto.rs`, add to `enum Intent`:

```rust
    MarkNoticeRead { id: u64 },
    ClearNotices,
```

and to `enum Event`:

```rust
    /// One live notice. Deliberately not folded into `WorkspaceView`: that
    /// snapshot goes out on every workspace change, and history does not
    /// belong on that path.
    Notice { notice: crate::notify::Notice },
    /// The whole store — every project's notices, not just this client's —
    /// sent on connect and after any read-state change, so no two browsers
    /// disagree about the badge count.
    Notices { list: Vec<crate::notify::Notice> },
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test proto::
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/proto.rs
git commit -m "proto: Notice/Notices events and the two notice intents"
```

---

### Task 4: Cross-project broadcast and intent handling

**Files:**
- Modify: `src/hub.rs`
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `notify::{record, list, mark_read, clear}` (Task 2), the events
  and intents from Task 3.
- Produces:
  ```rust
  // in hub.rs, free functions (not methods — they touch every hub, not one)
  pub fn broadcast_all(ev: &crate::proto::Event);
  pub fn publish(project: &str, session: &str, p: crate::osc::Parsed);
  ```
  `publish` is what the PTY pump calls in Task 5.

The locking rule this task exists to respect: `broadcast_all` clones the
registry's `Arc`s under the registry lock, **drops it**, and only then locks
each hub. Locking a hub while holding the registry lock would invert the order
`for_project` already established, and deadlock against it.

- [ ] **Step 1: Write the failing test**

Add to `tests/integration.rs`. Note the two clients are on **different
projects** — a single-client test cannot tell cross-project broadcast from
plain same-project delivery, which is the exact failure mode this repo has
shipped before.

```rust
#[test]
fn a_notice_reaches_a_client_watching_a_different_project() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("alpha")).unwrap();
    std::fs::create_dir_all(d.path().join("beta")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let mut a = ws_connect_path(port, "/ws/alpha/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/beta/_workspace").unwrap();
    read_until(&mut a, r#""t":"State""#);
    read_until(&mut b, r#""t":"State""#);

    // Published against alpha; beta's client must still see it.
    deadlight::hub::publish(
        "alpha",
        "claude",
        deadlight::osc::Parsed { title: Some("build".into()), body: "green".into() },
    );

    let seen_b = read_until(&mut b, r#""t":"Notice""#);
    assert!(seen_b.contains(r#""project":"alpha""#), "beta got: {seen_b}");
    assert!(seen_b.contains("green"), "beta got: {seen_b}");
    let seen_a = read_until(&mut a, r#""t":"Notice""#);
    assert!(seen_a.contains("green"), "alpha got: {seen_a}");
}

#[test]
fn notices_are_replayed_on_connect_and_read_state_mirrors() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    deadlight::hub::publish(
        "proj",
        "claude",
        deadlight::osc::Parsed { title: None, body: "waiting for you".into() },
    );

    // A client connecting *after* the fact still learns about it.
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let replay = read_until(&mut a, r#""t":"Notices""#);
    assert!(replay.contains("waiting for you"), "connect replay missing it: {replay}");
    let id: u64 = {
        let key = r#""id":"#;
        let start = replay.find(key).expect("no id in replay") + key.len();
        replay[start..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
    };

    // Read state is global: b marks read, a must be told.
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    read_until(&mut b, r#""t":"Notices""#);
    b.send(tungstenite::Message::Text(format!(r#"{{"t":"MarkNoticeRead","id":{id}}}"#).into())).unwrap();
    let after = read_until(&mut a, r#""read":true"#);
    assert!(after.contains(r#""t":"Notices""#), "a was not re-sent the list: {after}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --test integration notice
```

Expected: compile error — `deadlight::hub::publish` not found.

- [ ] **Step 3: Implement broadcast and intent handling**

In `src/hub.rs`, add these free functions after the `impl Hub` block:

```rust
/// Send an event to every connected client of every project. Notices are
/// machine-wide: a browser on one project must still learn that another one
/// wants attention.
///
/// The registry lock is dropped before any hub lock is taken. `for_project`
/// already established that order (registry, then hub); taking them the other
/// way round here would deadlock against a connection racing in.
pub fn broadcast_all(ev: &Event) {
    let Some(reg) = REGISTRY.get() else { return };
    let hubs: Vec<Arc<Mutex<Hub>>> = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    };
    for h in hubs {
        Hub::lock(&h).broadcast(ev);
    }
}

/// Record a parsed sequence and tell every client. Called from the PTY pump
/// thread, which holds no lock at this point and must never panic.
pub fn publish(project: &str, session: &str, p: crate::osc::Parsed) {
    if let Some(notice) = crate::notify::record(project, session, p) {
        broadcast_all(&Event::Notice { notice });
    }
}
```

In `Hub::handle`, add two arms to the `match &intent`, next to
`Intent::RequestState`:

```rust
            Intent::MarkNoticeRead { id } => {
                crate::notify::mark_read(*id);
                // Everyone, not just the caller: read state is global, so a
                // second browser's badge must not keep counting it.
                broadcast_all(&Event::Notices { list: crate::notify::list() });
                return;
            }
            Intent::ClearNotices => {
                crate::notify::clear();
                broadcast_all(&Event::Notices { list: crate::notify::list() });
                return;
            }
```

**Careful:** these arms run with the hub's own lock held (the caller locked it
to dispatch), and `broadcast_all` locks every hub — including this one, which
is not reentrant. Do **not** call `broadcast_all` from inside `handle`.
Instead, have `handle` return and let the socket layer do it. Implement it as:
`handle` sets a flag on the hub, and `wsconn` broadcasts after unlocking.

Simplest correct version — add a field to `Hub`:

```rust
    /// Set by the notice intents, drained by the socket layer after the hub
    /// lock is released. `broadcast_all` locks every hub, including this one,
    /// and `Mutex` is not reentrant — so the broadcast cannot happen inside
    /// `handle`.
    pub notices_dirty: bool,
```

initialise it to `false` in `Hub::new`, and set `self.notices_dirty = true;`
in both arms instead of calling `broadcast_all`.

Then in `src/wsconn.rs`, the read loop currently holds its guard `h` for the
whole match arm (`src/wsconn.rs:114-123`). Replace that arm with:

```rust
            Ok(Message::Text(t)) => {
                let dirty = {
                    let mut h = Hub::lock(&hub);
                    match proto::decode(&t) {
                        Ok(intent) => h.handle(&id, intent),
                        Err(e) => {
                            let ev = proto::Event::Error { msg: e };
                            h.send_to(&id, &ev);
                        }
                    }
                    std::mem::take(&mut h.notices_dirty)
                };
                // Outside the block above, so this hub's lock is released:
                // broadcast_all locks every hub including this one, and a
                // Mutex is not reentrant.
                if dirty {
                    crate::hub::broadcast_all(&proto::Event::Notices {
                        list: crate::notify::list(),
                    });
                }
            }
```

Also send the store on connect. In `wsconn.rs`, inside the existing
`let (id, rx) = { … }` block that subscribes and sends the initial snapshot
(`src/wsconn.rs:66-90`), immediately after `h.send_to(&id, &ev);` for the
snapshot:

```rust
        // Replay the whole store to a fresh client: a notice raised while no
        // browser was open is exactly the case this feature exists for. Sent
        // inside the same lock acquisition as the snapshot, for the reason
        // the existing comment above gives — releasing in between lets a
        // foreign broadcast land first.
        let ev = proto::Event::Notices { list: crate::notify::list() };
        h.send_to(&id, &ev);
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --test integration notice
```

Expected: PASS, 2 tests. Then run the whole suite to confirm nothing regressed:

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/hub.rs src/wsconn.rs tests/integration.rs
git commit -m "hub: cross-project notice broadcast, connect replay, read-state mirroring"
```

---

### Task 5: PTY ingress and child environment

**Files:**
- Modify: `src/session.rs` (the pump thread in `attach`, and the
  `CommandBuilder` env block)
- Modify: `src/lib.rs` (call `notify::load()` at startup, in `serve`)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `osc::Parser` (Task 1), `hub::publish` (Task 4).
- Produces: no new public API. Child processes gain `DEADLIGHT_NOTIFY=1`,
  `DEADLIGHT_PROJECT=<project>`, `DEADLIGHT_SESSION=<name>`.

- [ ] **Step 1: Write the failing test**

Add to `tests/integration.rs`. This goes through the real pump rather than
calling `publish` directly — a test that called `publish` would pass even if
the parser were never wired in.

`DEADLIGHT_CMD` splits on whitespace, so it must name a single path token; an
inline `sh -c 'printf …'` would be torn apart by that split. Hence the script
file.

Both tests use `fixture_named` with a project of their own, and distinct
session names, rather than the shared `fixture()` and a shared `"claude"`.
Two process-global registries outlive any one test: `Hub`'s, keyed by project
name (see `fixture_named`'s own doc comment), and `session::SESSIONS`, keyed
by `{project}/{session}`. Two tests sharing both keys means the second
silently attaches to the first's still-running child and reads the wrong
script's output — passing or failing depending on test order.

```rust
#[test]
fn an_escape_sequence_from_a_terminal_becomes_a_notice() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());

    // A single-token command: DEADLIGHT_CMD splits on whitespace.
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
    std::env::set_var("DEADLIGHT_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("notifyproj");
    let mut ctrl = ws_connect_path(port, "/ws/notifyproj/_workspace").unwrap();
    read_until(&mut ctrl, r#""t":"State""#);
    // Attaching the terminal socket is what spawns the session and its pump.
    let mut term = ws_connect_path(port, "/ws/notifyproj/term/claude").unwrap();

    let seen = read_until(&mut ctrl, r#""t":"Notice""#);
    assert!(seen.contains("Build done"), "title missing: {seen}");
    assert!(seen.contains("42 tests passed"), "body missing: {seen}");
    // Attribution comes from the pump's own identity, not from the payload.
    assert!(seen.contains(r#""session":"claude""#), "session missing: {seen}");
    assert!(seen.contains(r#""project":"notifyproj""#), "project missing: {seen}");

    let _ = term.close(None);
    std::env::remove_var("DEADLIGHT_CMD");
}

#[test]
fn a_terminal_child_can_discover_that_notifications_exist() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("env.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"NOTIFY=$DEADLIGHT_NOTIFY PROJ=$DEADLIGHT_PROJECT SESS=$DEADLIGHT_SESSION\"\nsleep 5\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("DEADLIGHT_CMD", script.to_str().unwrap());

    let (_d, port) = fixture_named("envproj");
    let mut term = ws_connect_path(port, "/ws/envproj/term/envprobe").unwrap();
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
    std::env::remove_var("DEADLIGHT_CMD");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --test integration escape_sequence discover
```

Expected: both fail — no `Notice` ever arrives; the env vars are empty.

- [ ] **Step 3: Wire the parser into the pump**

In `src/session.rs`, in `attach`, extend the `CommandBuilder` env block that
already sets `TERM`:

```rust
        cb.env("TERM", "xterm-256color");
        // How a process in this terminal — Claude, mainly — discovers that it
        // can raise a notification at all, and what to attribute it to. A
        // model can answer "can I notify?" from its own environment rather
        // than having to be told in a prompt.
        cb.env("DEADLIGHT_NOTIFY", "1");
        cb.env("DEADLIGHT_PROJECT", project);
        cb.env("DEADLIGHT_SESSION", name);
```

Then, in the pump thread, capture the identity and hold the parser as a local:

```rust
        let pump_key = key.clone();
        let pump_project = project.to_string();
        let pump_session = name.to_string();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // Parser state lives here, not on `Session`: scanning then needs
            // no lock at all, and a sequence split across two reads still
            // parses.
            let mut osc = crate::osc::Parser::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // Scanned before the lock is taken (pure CPU), and
                        // published after it is dropped: `publish` locks the
                        // hub registry, and holding the session registry
                        // across that would invert a lock order and risk the
                        // deadlock this project has already shipped once.
                        let notices = osc.feed(&buf[..n]);
                        {
                            let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
                            let Some(s) = map.get_mut(&pump_key) else { break };
                            push_scrollback(&mut s.scrollback, &buf[..n]);
                            let chunk = buf[..n].to_vec();
                            s.subs.retain(|_, tx| tx.try_send(chunk.clone()).is_ok());
                        }
                        for p in notices {
                            crate::hub::publish(&pump_project, &pump_session, p);
                        }
                    }
                }
            }
            // ... existing teardown unchanged ...
        });
```

Note the added block braces around the locked section: the guard must drop
before `publish` runs. The original code had no braces because the guard lived
to the end of the match arm.

In `src/lib.rs`, in `serve`, before the accept loop:

```rust
    // Notices raised while no browser was connected are the point of the
    // store; load them before anything can connect.
    crate::notify::load();
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test
```

Expected: all green, including the two new tests.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/session.rs src/lib.rs tests/integration.rs
git commit -m "session: parse notification sequences from the PTY; advertise the capability in the child env"
```

---

### Task 6: `deadlight notify` subcommand

**Files:**
- Modify: `src/main.rs`
- Create: `src/cli.rs`
- Modify: `src/lib.rs` (add `pub mod cli;`)
- Test: `src/cli.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing (it emits bytes; it does not link the store).
- Produces:
  ```rust
  pub fn notify_sequence(title: &str, body: &str) -> String;
  pub fn run_notify(args: &[String]) -> i32;  // process exit code
  ```

Why a subcommand at all: it is more discoverable than a `printf`, and it is
what a Claude Code hook invokes. It runs *inside* the PTY, so it needs no IPC
— printing the sequence is the whole mechanism.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_is_the_777_form_the_parser_accepts() {
        let s = notify_sequence("Build done", "42 tests");
        assert_eq!(s, "\x1b]777;notify;Build done;42 tests\x07");
        // The real check: our own parser must accept what we emit.
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("Build done"));
        assert_eq!(got[0].body, "42 tests");
    }

    #[test]
    fn a_semicolon_in_the_title_cannot_forge_a_body_boundary() {
        // Only the body may contain ';' — a title that does would otherwise
        // shift the parse.
        let s = notify_sequence("a;b", "body");
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("a"), "title must be sanitised, not trusted");
        assert_eq!(got[0].body, "b;body");
    }

    #[test]
    fn a_missing_body_is_allowed() {
        let s = notify_sequence("just a title", "");
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "");
    }

    #[test]
    fn no_arguments_is_an_error_not_a_silent_success() {
        assert_ne!(run_notify(&[]), 0, "a hook with no title must fail loudly");
    }
}
```

Note the second test documents a real wart: a `;` in the title shifts the
split. Rather than inventing an escaping scheme, the sequence is emitted
as-is and the parser's own splitting decides. If that proves annoying in
practice, strip `;` from the title in `notify_sequence` — but do it as a
follow-up with its own test, not silently here.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test cli::
```

Expected: compile error — `notify_sequence` not found.

- [ ] **Step 3: Implement**

Create `src/cli.rs`:

```rust
//! The `deadlight notify` subcommand.
//!
//! It runs *inside* the terminal deadlight is already reading, so there is no
//! IPC here and no socket to connect to: printing the escape sequence to the
//! controlling terminal IS the mechanism. That is also why this never binds a
//! port or touches the notice store.
use std::io::Write;

/// `/dev/tty` rather than stdout, because the intended caller is a Claude
/// Code hook and Claude Code captures hook stdout — a hook printing to stdout
/// would be swallowed before it ever reached the PTY. `/dev/tty` is the
/// controlling terminal, which is the PTY deadlight owns.
fn tty() -> Option<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open("/dev/tty").ok()
}

pub fn notify_sequence(title: &str, body: &str) -> String {
    format!("\x1b]777;notify;{title};{body}\x07")
}

pub fn run_notify(args: &[String]) -> i32 {
    let Some(title) = args.first() else {
        eprintln!("usage: deadlight notify <title> [body]");
        return 2;
    };
    let body = args.get(1).map(String::as_str).unwrap_or("");
    let seq = notify_sequence(title, body);
    if let Some(mut f) = tty() {
        if f.write_all(seq.as_bytes()).is_ok() && f.flush().is_ok() {
            return 0;
        }
    }
    // Fall back to stdout for the interactive case where it is a tty anyway.
    let mut out = std::io::stdout();
    if out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok() {
        return 0;
    }
    // Loud, not silent: a misconfigured hook that quietly did nothing would
    // look exactly like a feature that does not work.
    eprintln!("deadlight notify: no controlling terminal and stdout unavailable");
    1
}
```

Replace `src/main.rs`:

```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // One subcommand only. Everything else keeps the historical contract:
    // the single argument is the port.
    if args.first().map(String::as_str) == Some("notify") {
        std::process::exit(deadlight::cli::run_notify(&args[1..]));
    }
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("deadlight listening on http://127.0.0.1:{port}");
    deadlight::serve(listener, deadlight::projects::roots());
}
```

Add `pub mod cli;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test cli::
```

Expected: PASS, 4 tests.

- [ ] **Step 5: Verify the /dev/tty assumption by hand**

This is the one inferred assumption in the design, and it is cheap to check
now rather than after the client is built. In a deadlight terminal:

```bash
cargo run --quiet -- notify "hand check" "does this arrive"
```

A notice should appear in the server log path — at minimum, confirm no error
on stderr. Then, from a Claude Code session running in a deadlight terminal,
configure a `Stop` hook running the same command and confirm it fires. If hook
output cannot reach the terminal, stop and report: the escape sequence and the
subcommand still work, and only the automatic-on-stop convenience needs
rethinking.

- [ ] **Step 6: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/cli.rs src/main.rs src/lib.rs
git commit -m "cli: deadlight notify writes the escape sequence to the controlling terminal"
```

---

### Task 7: Service worker and OS notifications

**Files:**
- Create: `static/sw.js`
- Modify: `src/routes.rs` (serve `/sw.js` from the root scope)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `Event::Notice` shape from Task 3 (`{t, notice:{id, project,
  session, title, body, at, read}}`).
- Produces: a service worker at origin scope. The page posts it
  `{kind: 'notify', notice}`; it posts the page `{kind: 'focus', project,
  session}` on click.

**The path matters.** A service worker's scope is capped by its own URL, so
one served from `/static/sw.js` could only control `/static/*` and could never
focus a workspace tab. It must be served from `/sw.js`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_service_worker_is_served_from_the_root_scope() {
    let (_d, port) = fixture();
    let (status, ctype, body) = get_full(port, "/sw.js");
    assert_eq!(status, 200, "sw.js must be at the root, or its scope cannot cover /{{project}}");
    assert!(ctype.contains("javascript"), "wrong content-type: {ctype}");
    assert!(body.contains("notificationclick"), "not the service worker: {body:.120}");
}
```

If `get_full` (status + content-type + body) does not already exist in
`tests/integration.rs`, add it next to the existing helpers:

```rust
fn get_full(port: u16, path: &str) -> (u16, String, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = ureq::get(&url).call();
    match resp {
        Ok(r) => {
            let status = r.status();
            let ctype = r.header("content-type").unwrap_or("").to_string();
            (status, ctype, r.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(code, r)) => {
            let ctype = r.header("content-type").unwrap_or("").to_string();
            (code, ctype, r.into_string().unwrap_or_default())
        }
        Err(e) => panic!("request failed: {e}"),
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --test integration service_worker
```

Expected: FAIL — `/sw.js` currently falls through to the workspace arm and
404s as "no such project".

- [ ] **Step 3: Serve it and write it**

In `src/routes.rs`, add an arm to the `match segs.as_slice()` **before** the
`[project, rest @ ..]` arm — order matters, that arm is a catch-all:

```rust
        // Root scope, not /static/sw.js: a service worker may only control
        // URLs under its own path, and this one has to focus and navigate
        // workspace tabs at /{project}.
        ["sw.js"] => serve_static(w, "sw.js"),
```

Create `static/sw.js`:

```js
// Shows OS notifications and routes clicks back to the right window.
//
// The page could call new Notification() directly and skip this file
// entirely — the reason it does not is that a service worker is the only
// thing that can later receive a Web Push message with no tab open. Doing it
// here now means adding push is a `push` listener, not a rewrite.

// Without claim(), a freshly registered worker does not control the page
// until the next reload, and clients.matchAll would find nothing to focus.
self.addEventListener("install", (e) => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  const m = e.data;
  if (!m || m.kind !== "notify") return;
  const n = m.notice;
  // Guard the payload even though only first-party page code posts here: an
  // undefined `notice` would throw a TypeError inside the worker, and an
  // uncaught throw in this context is invisible from the page.
  if (!n || !n.title) return;
  // waitUntil because showNotification is async and this is an
  // ExtendableMessageEvent: without it the browser may terminate the worker
  // between this dispatch and the notification actually appearing, and the
  // notice is lost with nothing to show for it.
  e.waitUntil(
    self.registration.showNotification(n.title, {
      body: n.body,
      // One notification per session: a chatty session replaces its own
      // rather than stacking twenty.
      tag: `${n.project}/${n.session}`,
      data: { project: n.project, session: n.session },
      renotify: true,
    })
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const { project, session } = e.notification.data || {};
  // Deliberate bail-out with no waitUntil: there is nothing to focus or
  // navigate to, and close() above already ran synchronously.
  if (!project) return;
  const target = `/${project}#session=${encodeURIComponent(session || "")}`;
  e.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
      // Prefer a window already on this project: focusing it needs no
      // navigation, so nothing in that tab is disturbed.
      const onProject = wins.find((c) => {
        try { return new URL(c.url).pathname === `/${project}`; } catch { return false; }
      });
      if (onProject) {
        return onProject.focus().then((c) => {
          (c || onProject).postMessage({ kind: "focus", project, session });
        });
      }
      // Otherwise reuse any deadlight window rather than opening a new tab.
      if (wins.length) {
        return wins[0].focus().then((c) => (c || wins[0]).navigate(target));
      }
      return self.clients.openWindow(target);
    })
  );
});
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --test integration service_worker
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add static/sw.js src/routes.rs tests/integration.rs
git commit -m "sw: service worker shows notices and focuses the window that needs attention"
```

---

### Task 8: Notification centre, badges, and click routing

**Files:**
- Modify: `src/render.rs` (bell markup in `<header>`, panel container)
- Modify: `static/app.js`
- Modify: `static/style.css`
- Test: `src/render.rs` (`#[cfg(test)] mod tests`) and by hand in a browser

**Interfaces:**
- Consumes: `Event::Notice` and `Event::Notices` (Task 3), the service worker
  message protocol (Task 7).
- Produces: no Rust API. DOM ids other code may rely on: `#bell`,
  `#bellcount`, `#noticepanel`.

- [ ] **Step 1: Write the failing test**

Add to `src/render.rs`'s test module:

The signature is `workspace_page(project: &str, s: &Settings, has_theme_css:
bool) -> String` (`src/render.rs:289`).

```rust
    #[test]
    fn the_workspace_page_carries_the_notification_centre() {
        let s = crate::config::Settings::default();
        let html = workspace_page("proj", &s, false);
        assert!(html.contains(r#"id="bell""#), "no bell button");
        assert!(html.contains(r#"id="bellcount""#), "no unread badge");
        assert!(html.contains(r#"id="noticepanel""#), "no panel container");
        // The panel is filled from JS with textContent; it must ship empty,
        // or notice text would be interpolated into HTML somewhere.
        assert!(html.contains(r#"<div id="noticepanel" hidden></div>"#), "panel must ship empty");
    }
```

Confirm `Settings` derives `Default` before writing this; if it does not,
build one the way `render.rs`'s existing tests do.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --lib render
```

Expected: FAIL — no bell in the header.

- [ ] **Step 3: Add the markup**

In `src/render.rs`, in the `<header>` of `workspace_page`, before the refresh
button:

```html
  <button id="bell" title="notifications (n)">🔔<span id="bellcount"></span></button>
```

and immediately after `</header>`:

```html
<div id="noticepanel" hidden></div>
```

The panel ships empty and is filled from JS with `textContent`: notice text
arrived over a terminal and is untrusted, so it must never be interpolated
into HTML.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test --lib render
```

Expected: PASS.

- [ ] **Step 5: Implement the client**

Append to `static/app.js`:

```js
// ---- notifications ----------------------------------------------------
// Notices are machine-wide, so this list spans projects; only the tab dot
// below is scoped to what is on screen.
let notices = [];
let swReg = null;
const baseTitle = document.title;

// A secure context is required for both service workers and the Notification
// API. localhost and `tailscale serve` HTTPS qualify; plain http:// to a
// tailnet IP does not — there the panel still works and OS notifications
// simply are not offered.
const canNotify = () => window.isSecureContext && "Notification" in window;

if (canNotify() && "serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").then(
    (r) => { swReg = r; },
    (e) => console.warn("deadlight: service worker registration failed", e)
  );
  navigator.serviceWorker.addEventListener("message", (e) => {
    if (e.data && e.data.kind === "focus") focusSession(e.data.session);
  });
}

function unread() { return notices.filter((n) => !n.read).length; }

function renderNotices() {
  const n = unread();
  const count = document.getElementById("bellcount");
  if (count) count.textContent = n ? String(n) : "";
  // The only cue that works from a background tab with no permission granted.
  document.title = n ? `(${n}) ${baseTitle}` : baseTitle;
  setFavicon(n > 0);

  const panel = document.getElementById("noticepanel");
  if (!panel || panel.hidden) return;
  panel.replaceChildren();
  if (!notices.length) {
    const empty = document.createElement("div");
    empty.className = "notice-empty";
    empty.textContent = "no notifications";
    panel.appendChild(empty);
  }
  for (const x of [...notices].reverse()) {
    const row = document.createElement("div");
    row.className = "notice" + (x.read ? " read" : "");
    const who = document.createElement("span");
    who.className = "notice-who";
    // Attribution is server truth; the message text is not. Both go in as
    // textContent regardless.
    who.textContent = `${x.project} · ${x.session}`;
    const title = document.createElement("span");
    title.className = "notice-title";
    title.textContent = x.title;
    const body = document.createElement("span");
    body.className = "notice-body";
    body.textContent = x.body;
    const when = document.createElement("span");
    when.className = "notice-when";
    when.textContent = ago(x.at);
    row.append(who, title, body, when);
    row.onclick = () => openNotice(x);
    panel.appendChild(row);
  }
  const foot = document.createElement("div");
  foot.className = "notice-foot";
  if (canNotify() && Notification.permission !== "granted") {
    const b = document.createElement("button");
    b.textContent = "Enable OS notifications";
    // Requested from a click, never on load: browsers penalise spontaneous
    // permission prompts, and an unprompted one is worse than none.
    b.onclick = (e) => { e.stopPropagation(); Notification.requestPermission().then(renderNotices); };
    foot.appendChild(b);
  } else if (!canNotify()) {
    const s = document.createElement("span");
    s.textContent = "OS notifications need a secure context (https or localhost)";
    foot.appendChild(s);
  }
  const clear = document.createElement("button");
  clear.textContent = "Clear";
  clear.onclick = (e) => { e.stopPropagation(); send({ t: "ClearNotices" }); };
  foot.appendChild(clear);
  panel.appendChild(foot);
}

function ago(secs) {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - secs);
  if (d < 60) return `${d}s`;
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h`;
  return `${Math.floor(d / 86400)}d`;
}

// A badged favicon, drawn rather than shipped as a second asset so it follows
// whatever the page's icon already is.
function setFavicon(badged) {
  let link = document.querySelector("link#dlfav");
  if (!link) {
    link = document.createElement("link");
    link.id = "dlfav";
    link.rel = "icon";
    document.head.appendChild(link);
  }
  const svg = badged
    ? `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><text y="13" font-size="13">◆</text><circle cx="12.5" cy="3.5" r="3.5" fill="#e5534b"/></svg>`
    : `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><text y="13" font-size="13">◆</text></svg>`;
  link.href = "data:image/svg+xml," + encodeURIComponent(svg);
}

function openNotice(x) {
  if (!x.read) send({ t: "MarkNoticeRead", id: x.id });
  if (x.project !== PROJECT) {
    location.href = `/${x.project}#session=${encodeURIComponent(x.session)}`;
    return;
  }
  focusSession(x.session);
}

// Activate the terminal tab for `session`, opening it if it is not on screen.
// Both paths are ordinary intents, so every connected client follows.
function focusSession(session) {
  if (!session || !SESSION_RE.test(session) || !state) return;
  for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal" && t.session === session);
    if (ti >= 0) {
      send({ t: "ActivateTab", pane: pi, idx: ti });
      attention.delete(session);
      renderNotices();
      return;
    }
  }
  send({ t: "OpenTab", pane: 3, tab: { k: "Terminal", session } });
}

// Sessions in THIS project with unseen notices — the tab-strip dot. A session
// in another project has no tab here; that is what the bell badge is for.
const attention = new Set();

function onNotice(n) {
  notices.push(n);
  if (n.project === PROJECT) attention.add(n.session);
  if (canNotify() && Notification.permission === "granted") {
    if (swReg) swReg.active && swReg.active.postMessage({ kind: "notify", notice: n });
    else new Notification(n.title, { body: n.body, tag: `${n.project}/${n.session}` });
  }
  renderNotices();
  render();
}
```

Add the two event cases to `onEvent`'s switch:

```js
    case "Notice": onNotice(ev.notice); break;
    case "Notices":
      notices = ev.list;
      // Read state is global, so a notice someone else cleared must stop
      // pulling at this client's attention too.
      for (const s of [...attention]) {
        if (!notices.some((n) => n.project === PROJECT && n.session === s && !n.read)) attention.delete(s);
      }
      renderNotices();
      render();
      break;
```

Wire the bell, next to the existing `refreshBtn` handler at the bottom:

```js
const bell = document.getElementById("bell");
if (bell) {
  bell.onclick = () => {
    const p = document.getElementById("noticepanel");
    p.hidden = !p.hidden;
    renderNotices();
  };
}
setFavicon(false);

// A notification click can land on a cold load; consume the fragment once and
// clear it so a later reload does not re-focus.
if (location.hash.startsWith("#session=")) {
  const want = decodeURIComponent(location.hash.slice("#session=".length));
  history.replaceState(null, "", location.pathname);
  const tryFocus = () => { if (state) focusSession(want); else setTimeout(tryFocus, 100); };
  tryFocus();
}
```

In `render()`, where a terminal tab's element is built, add the dot. Find the
tab-strip construction and add, for tabs where `t.k === "Terminal" &&
attention.has(t.session)`, the class `attn` on the tab element. Read the
existing `render()` to place this correctly — do not guess at the variable
names.

Append to `static/style.css`:

```css
#bell { position: relative; }
#bellcount:not(:empty) {
  position: absolute; top: -2px; right: -4px;
  min-width: 14px; padding: 0 3px;
  border-radius: 7px; background: #e5534b; color: #fff;
  font-size: 10px; line-height: 14px; text-align: center;
}
#noticepanel {
  position: absolute; top: var(--header-h); right: 8px; z-index: 20;
  width: 340px; max-height: 60vh; overflow-y: auto;
  background: var(--bg, #1c1c1c); border: 1px solid var(--border, #333);
  border-radius: 4px; padding: 4px;
}
.notice { display: grid; grid-template-columns: 1fr auto; gap: 2px 6px; padding: 6px; cursor: pointer; border-bottom: 1px solid var(--border, #333); }
.notice:hover { background: var(--hover, #262626); }
.notice.read { opacity: 0.55; }
.notice-who { grid-column: 1; font-size: 11px; opacity: 0.7; }
.notice-when { grid-column: 2; grid-row: 1; font-size: 11px; opacity: 0.7; }
.notice-title { grid-column: 1 / -1; font-weight: 600; }
.notice-body { grid-column: 1 / -1; font-size: 12px; white-space: pre-wrap; word-break: break-word; }
.notice-empty, .notice-foot { padding: 8px; font-size: 12px; opacity: 0.8; display: flex; gap: 8px; align-items: center; }
.tab.attn::after { content: "●"; color: #e5534b; margin-left: 4px; font-size: 9px; vertical-align: middle; }
```

Adapt the CSS variable names and the `.tab` selector to whatever
`static/style.css` already uses — read it first.

- [ ] **Step 6: Run the full suite**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test
```

Expected: all green.

- [ ] **Step 7: Verify in a real browser — required, not optional**

This repo has shipped features that a green suite said were fine and a browser
proved were completely broken. Start it and check every item:

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
DEADLIGHT_ROOTS="$HOME/Projects" cargo run --quiet 8445
# open http://127.0.0.1:8445/ — loopback, not a tailnet IP (which 403s)
```

- [ ] Open a terminal tab, run `printf '\033]777;notify;hello;from the terminal\007'` — a row appears in the panel and the bell badge increments.
- [ ] The browser tab title shows `(1)` and the favicon gains a dot.
- [ ] Click *Enable OS notifications*, accept, fire another — an OS notification appears.
- [ ] Background the tab, fire another — the OS notification still appears.
- [ ] Click the OS notification — the deadlight tab is focused and the firing terminal tab becomes active.
- [ ] Open two projects in two tabs. Fire from project A while looking at B — B's bell badge increments and names project A. Clicking the OS notification focuses A's tab specifically, not B's.
- [ ] With the terminal tab closed, click a notice — the tab opens and activates.
- [ ] The tab-strip dot appears on the firing session's tab and clears when it is activated.
- [ ] Mark read in one browser — a second browser's badge drops too.
- [ ] Kill and restart deadlight — the notices are still listed.
- [ ] `cat` a binary file in a terminal — no spurious notifications, no visible corruption.

Fix anything that fails before committing.

- [ ] **Step 8: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add src/render.rs static/app.js static/style.css
git commit -m "ui: notification centre, tab/favicon badges, and click-to-focus routing"
```

---

### Task 9: Documentation

**Files:**
- Create: `docs/notifications.md`
- Modify: `README.md`, `docs/deploy.md`, `docs/backlog.md`
- Test: none (prose)

- [ ] **Step 1: Write `docs/notifications.md`**

```markdown
# Notifications

deadlight raises an OS notification when something in a terminal wants your
attention — Claude finishing a task, or asking for a decision — and clicking
it focuses the browser tab and terminal that fired.

## Triggering one

Any process in a deadlight terminal can fire one. Three equivalent ways:

```bash
deadlight notify "Build done" "42 tests passed"
printf '\033]777;notify;Build done;42 tests passed\007'
printf '\033]9;Build done\007'          # iTerm2 one-arg form, no body
```

The sequences are the ones kitty, urxvt/dunst, and iTerm2 already use, so
anything that can already notify those notifies deadlight unchanged.

## Discovering that it is available

Every terminal deadlight spawns carries:

| Variable | Meaning |
|---|---|
| `DEADLIGHT_NOTIFY` | `1` when notifications are available |
| `DEADLIGHT_PROJECT` | the project this terminal belongs to |
| `DEADLIGHT_SESSION` | this terminal's session name |

So a script — or a model — can check `[ -n "$DEADLIGHT_NOTIFY" ]` before
trying.

## Firing automatically from Claude Code

Add to the project's `.claude/settings.json` to be notified when Claude
finishes a turn or needs permission:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "deadlight notify \"Claude\" \"finished\"" }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "deadlight notify \"Claude\" \"needs your input\"" }] }
    ]
  }
}
```

`deadlight notify` writes to `/dev/tty` rather than stdout, because Claude
Code captures hook stdout — a hook printing to stdout would be swallowed
before reaching the terminal.

## In the browser

A bell in the header shows an unread count across **all** projects; the
browser tab title and favicon carry the same count. Notices persist across a
deadlight restart, so one raised overnight is still there in the morning.

OS notifications need a secure context — `localhost` or `tailscale serve`
HTTPS. Over plain `http://` to a tailnet IP the panel still works but the OS
cannot be asked. Permission is requested from the panel's button, never
automatically.

## Limits

| Thing | Limit |
|---|---|
| Retained notices | 100 |
| Title / body | 100 / 500 characters |
| Per session | 10 per minute, then suppressed and counted |

Text arriving over a terminal is untrusted — `cat` of a hostile file could
emit one — so it is stripped of control characters and always attributed to
the project and session deadlight itself observed, never to anything the
message claims about itself.
```

- [ ] **Step 2: Update the surrounding docs**

- `README.md`: add a bullet to "What it does" describing notifications, and
  correct the Quick start line "The only CLI argument is the port" — there is
  now a `notify` subcommand.
- `docs/deploy.md`: note that OS notifications require the HTTPS
  (`tailscale serve`) or localhost origin, and that `notifications.json` joins
  the state directory.
- `docs/backlog.md`: move Web Push, the relay sink, per-project mute, and the
  picker-page centre in from the spec's Future work.

- [ ] **Step 3: Verify the docs match the code**

Re-read `docs/notifications.md` against the implementation. Every command,
variable name, and limit in it must be one that exists. A doc that describes a
flag nobody implemented is worse than no doc.

- [ ] **Step 4: Commit**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
git add docs/notifications.md README.md docs/deploy.md docs/backlog.md
git commit -m "docs: notifications — sequences, env vars, hook config, limits"
```

---

### Task 10: Full verification

**Files:** none — this task only runs things.

- [ ] **Step 1: Full suite, debug profile**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test
```

Expected: all green. Record the count; it should be roughly 154 + 30.

- [ ] **Step 2: Clippy and formatting**

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications
cargo clippy --all-targets 2>&1 | tail -20
cargo fmt --check
```

Fix anything reported.

- [ ] **Step 3: Confirm the whole suite still passes with the store populated**

The notice store is process-global; a stale one could mask an ordering bug.

```bash
cd /Users/peter/Projects/deadlight/.claude/worktrees/notifications && cargo test -- --test-threads=1
```

Expected: green. If it passes single-threaded but fails in parallel (or vice
versa), that is a real bug in the test isolation, not a flake to retry.

- [ ] **Step 4: Linux host**

Per `CLAUDE.md`, the macOS/Linux split has hidden real defects before. If the
Linux host is reachable, `ssh` in, pull the branch, and `cargo test`. If it is
not reachable, say so explicitly in the completion report rather than
implying it was covered.

- [ ] **Step 5: Report**

State plainly: tests run and their counts, browser checks performed from Task
8 Step 7, whether the Linux run happened, and whether the `/dev/tty` hook
assumption from Task 6 Step 5 held. Do not report the feature as verified on
any axis that was not actually exercised.

---

## Self-Review

**Spec coverage.** Walked each spec section against the tasks:

| Spec section | Task |
|---|---|
| The notice (shape, server-side attribution) | 2 |
| Ingress, both wire forms, bounds, sanitising | 1 |
| Stream not modified; parser placement in the pump | 5 |
| Store, ring, persistence, rate limit | 2 |
| Egress, cross-project, new events/intents | 3, 4 |
| Connect replay, global read state | 4 |
| Service worker, click targeting, `clients.claim` | 7 |
| Attention cues (bell, title, favicon, tab dot) | 8 |
| Degradation on insecure context | 8 |
| Env vars | 5 |
| `deadlight notify` | 6 |
| Documentation and hook config | 9 |
| Threat model mitigations | 1 (sanitise, bounds), 2 (rate limit), 4 (attribution), 8 (`textContent`) |
| Testing section, all named cases | 1, 2, 4, 5, 7, 8, 10 |

No gaps.

**Type consistency.** `Parsed { title: Option<String>, body: String }` is
produced in Task 1 and consumed unchanged in 2, 4, 5, 6. `Notice`'s seven
fields are defined in Task 2 and used identically in the Task 3 encode test,
the Task 4 integration assertions, and the Task 8 client. `record` returns
`Option<Notice>` in Task 2 and is matched with `if let Some(..)` in Task 4.
`publish(project, session, Parsed)` is defined in Task 4 and called with that
arity in Task 5.

**Known wart, deliberately left in.** Task 4 Step 3 first shows the obvious
implementation of the intent arms and then rejects it, because
`broadcast_all` locks every hub including the one whose lock `handle` already
holds. The corrected version routes through a `notices_dirty` flag. The
rejected version is left visible in the plan on purpose: an implementer who
writes the natural thing will otherwise ship a self-deadlock.

**One thing an implementer must not take on faith.** Task 8 Step 5 says to
read the existing `render()` before adding the tab dot, and to adapt the CSS
variable names. Those are the two places the plan describes an edit without
being able to quote the surrounding code, so they are called out rather than
guessed at.
