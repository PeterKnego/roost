# Claude Notification Hooks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A user switches "Claude finished" and "Claude needs you"
notifications on or off per project from the bell, roost writes exactly its
own two hook entries into that project's `.claude/settings.local.json`, and
the hook they run turns Claude Code's event JSON into a notification while
staying silent outside a roost terminal.

**Architecture:** A new `roost claude-hook` subcommand in `src/cli.rs`
(pure mapping from hook JSON to title and body, then the existing sink
logic). A new `src/claudehooks.rs` that reads the settings file into one of
three states and rewrites it around roost-owned entries with temp-and-rename.
One new intent and one new `WorkspaceView` field carry it to the client,
where `static/app.js` marks the bell and adds a row with the switch to the
notice panel.

**Tech Stack:** Rust with `serde_json` (gaining the `preserve_order`
feature; its `indexmap` dependency is already in the lockfile), plain JS in
`static/app.js`, CSS in `static/style.css`, Deno browser tests through
`tests/browser/harness.mjs`.

**Spec:** `docs/superpowers/specs/2026-09-03-claude-notification-hooks-design.md`

## Global Constraints

- **Only `.claude/settings.local.json` under the project is ever written.**
  Never `.claude/settings.json`, never `~/.claude/settings.json`.
- **roost owns an entry iff its `command` is exactly `roost claude-hook`.**
  Every other byte of the file's content survives a write, including
  foreign hooks on the same events, other events, other keys, and key
  order.
- **Three read states:** `Present` (both events carry a roost entry),
  `Absent` (file missing, or fewer than both events carry one), `Unknown`
  (unreadable other than `NotFound`, or not a JSON object with an
  object-or-absent `hooks`). `Unknown` refuses every write and leaves the
  file byte-for-byte untouched.
- **Writes are temp-then-rename** in the same directory, preserving an
  existing file's mode, with `settings.local.json.bak` written once before
  the first replacement and never overwritten.
- **The hook command always exits 0** and prints nothing to stdout; the
  sequence goes to `/dev/tty` through the existing `choose_sink`. Silent
  when `ROOST_NOTIFY` is unset, on invalid or unhandled input, and on the
  `Nowhere` sink (one stderr line in that last case).
- **Exact entries written:** for each of `Notification` and `Stop`, one
  group `{ "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] }`.
- **Run the Rust suite as `cargo test -- --test-threads=1`.** A bare
  `cargo test` hangs on this project.
- **Revert-check every new test** and record the observed failure in the
  test's doc comment.
- **Build from this checkout only.** If `cargo build` fails with
  `include_bytes!` naming another directory, run `cargo clean -p roost`.
- **Never `git add -A`.** The working tree carries files that are not this
  plan's (`README-new.md`); stage the paths each task names.

---

## File Structure

- `src/cli.rs` — gains `hook_message(v: &serde_json::Value) -> Option<(String, String)>`
  (pure) and `run_claude_hook() -> i32`. Same file as `run_notify` because
  both are "emit one sequence to the sink".
- `src/main.rs` — one new dispatch arm.
- `src/claudehooks.rs` (new) — `HookState`, `state(project_dir)`,
  `set(project_dir, on)`, and the private merge. Owns everything about the
  settings file; nothing else in the tree touches it.
- `src/lib.rs` — `pub mod claudehooks;`.
- `Cargo.toml` — `serde_json = { version = "1", features = ["preserve_order"] }`.
- `src/proto.rs` — `Intent::SetClaudeHooks { on: bool }`,
  `WorkspaceView.claude_hooks: Option<bool>`.
- `src/workspace.rs` — `view()` fills `claude_hooks: None`.
- `src/hub.rs` — the intent arm; `snapshot_event` fills `claude_hooks`.
- `static/app.js` — bell mark, panel row, confirmation, intent.
- `static/style.css` — the two marks and the row.
- `tests/integration.rs` — intent over the workspace socket.
- `tests/browser/claudehooks.mjs` (new), `tests/browser/README.md`.
- `docs/notifications.md`, `README.md`, `CLAUDE.md`.

---

### Task 1: `roost claude-hook`

**Files:**
- Modify: `src/cli.rs` (append after `run_notify`; tests in its `mod tests`)
- Modify: `src/main.rs:5-8` (dispatch)

**Interfaces:**
- Consumes: `notify_sequence`, `tty`, `choose_sink`, `Sink` (all in `cli.rs`), `crate::osc::sanitise`.
- Produces:
  ```rust
  pub fn hook_message(v: &serde_json::Value) -> Option<(String, String)>
  pub fn run_claude_hook() -> i32   // reads stdin, honours ROOST_NOTIFY, always 0
  ```

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/cli.rs`:

```rust
    fn msg(json: &str) -> Option<(String, String)> {
        hook_message(&serde_json::from_str(json).unwrap())
    }

    /// The table from the spec, one row per assertion, so a row that
    /// changes fails by name.
    #[test]
    fn hook_message_maps_each_handled_event() {
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#),
            Some(("Claude needs you".into(), "wants permission to run a tool".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#),
            Some(("Claude needs you".into(), "is waiting for your input".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"agent_needs_input"}"#),
            Some(("Claude needs you".into(), "an agent needs your input".into()))
        );
        for t in ["elicitation_dialog", "elicitation_url_dialog"] {
            assert_eq!(
                msg(&format!(r#"{{"hook_event_name":"Notification","notification_type":"{t}"}}"#)),
                Some(("Claude needs you".into(), "is asking a question".into())),
                "{t}"
            );
        }
        assert_eq!(
            msg(r#"{"hook_event_name":"Stop","last_assistant_message":"Done.\nSecond line."}"#),
            Some(("Claude finished".into(), "Done.".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Stop"}"#),
            Some(("Claude finished".into(), String::new()))
        );
    }

    /// Everything else is silence: unhandled types and events, and input
    /// that is JSON but not an object.
    #[test]
    fn hook_message_is_none_for_everything_else() {
        assert_eq!(msg(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"Notification","notification_type":"agent_completed"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"Notification"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"SubagentStop","last_assistant_message":"x"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"PreToolUse"}"#), None);
        assert_eq!(msg(r#"{}"#), None);
        assert_eq!(msg(r#"[1,2]"#), None);
    }

    /// A glance, not a transcript: first line, at most 120 characters,
    /// control characters stripped by the same sanitiser `notify` uses.
    #[test]
    fn stop_body_is_the_first_line_capped_and_sanitised() {
        let long = "x".repeat(300);
        let (_, body) = msg(&format!(r#"{{"hook_event_name":"Stop","last_assistant_message":"{long}"}}"#)).unwrap();
        assert_eq!(body.chars().count(), 120);
        let (_, body) = msg(r#"{"hook_event_name":"Stop","last_assistant_message":"a\u001b[31mb\tc"}"#).unwrap();
        // `\u001b` and `\t` are JSON escapes, so serde hands `hook_message`
        // a real ESC and a real tab; the sanitiser must strip both.
        assert!(!body.contains('\u{1b}') && !body.contains('\t'), "{body:?}");
        assert!(body.starts_with('a'), "{body:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test hook_message stop_body -- --test-threads=1`
Expected: compile error, `cannot find function hook_message`.

- [ ] **Step 3: Write the mapping and the runner**

Append after `run_notify` in `src/cli.rs`:

```rust
/// What a Claude Code hook event says to the user, or `None` for the
/// events and notification types this command deliberately ignores.
///
/// Pure, so the whole table is one unit test. The event shapes are Claude
/// Code's documented hook input: `hook_event_name` on every event,
/// `notification_type` on `Notification`, `last_assistant_message` on
/// `Stop`. Anything not matched here is silence, not an error: a hook fires
/// for every event it is registered on, and the ones registered are only
/// `Notification` and `Stop`, but a future Claude Code may send types this
/// table has never heard of.
pub fn hook_message(v: &serde_json::Value) -> Option<(String, String)> {
    let event = v.get("hook_event_name")?.as_str()?;
    let (title, body) = match event {
        "Notification" => {
            let body = match v.get("notification_type")?.as_str()? {
                "permission_prompt" => "wants permission to run a tool",
                "idle_prompt" => "is waiting for your input",
                "agent_needs_input" => "an agent needs your input",
                "elicitation_dialog" | "elicitation_url_dialog" => "is asking a question",
                _ => return None,
            };
            ("Claude needs you", body.to_string())
        }
        "Stop" => {
            // First line only, then a hard cap: a notification is a glance.
            // `sanitise` strips control characters and applies the parser's
            // own cap; the 120 here is tighter on purpose.
            let last = v.get("last_assistant_message").and_then(|m| m.as_str()).unwrap_or("");
            let line = last.lines().next().unwrap_or("");
            let clean = crate::osc::sanitise(line, crate::osc::MAX_BODY);
            ("Claude finished", clean.chars().take(120).collect())
        }
        _ => return None,
    };
    Some((title.to_string(), body))
}

/// The `roost claude-hook` subcommand: Claude Code pipes the event as JSON on
/// stdin; this turns it into one notification, or nothing.
///
/// Always exits 0. A `Stop` hook that exits non-zero shows an error in the
/// transcript, and none of the ways this can have nothing to do is the
/// user's mistake: a Claude run outside roost (no `ROOST_NOTIFY`), an event
/// the table ignores, or a subagent's hook with no terminal (the `Nowhere`
/// sink). `roost notify` keeps its loud exit 1 for the hand-written case;
/// this command is installed into a project's settings by the bell and has
/// to be silent wherever that project is opened without roost.
pub fn run_claude_hook() -> i32 {
    use std::io::Read;
    if std::env::var_os("ROOST_NOTIFY").is_none() {
        return 0;
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else { return 0 };
    let Some((title, body)) = hook_message(&v) else { return 0 };
    let seq = notify_sequence(&title, &body);
    let mut tty_file = tty();
    match choose_sink(tty_file.is_some(), std::io::stdout().is_terminal()) {
        Sink::Tty => {
            if let Some(f) = tty_file.as_mut() {
                let _ = f.write_all(seq.as_bytes()).and_then(|_| f.flush());
            }
        }
        // Not stdout even when it is a terminal: Claude Code reads hook
        // stdout as a decision, and a sequence there would be parsed as one.
        Sink::Stdout | Sink::Nowhere => {
            eprintln!("roost claude-hook: no controlling terminal to notify through; nothing sent");
        }
    }
    0
}
```

In `src/main.rs`, extend the dispatch:

```rust
    match args.first().map(String::as_str) {
        Some("notify") => std::process::exit(roost::cli::run_notify(&args[1..])),
        Some("claude-hook") => std::process::exit(roost::cli::run_claude_hook()),
        _ => {}
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test hook_message stop_body -- --test-threads=1`
Expected: 3 passed.

- [ ] **Step 5: Exercise the binary end to end**

The exit-0-everywhere rule lives in `run_claude_hook`, which reads real
stdin and the real environment, so pin it with the built binary rather
than a unit test:

```bash
cargo build -q
B="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/roost"
echo '{"hook_event_name":"Stop","last_assistant_message":"hi"}' | env -u ROOST_NOTIFY "$B" claude-hook; echo "no ROOST_NOTIFY: exit $?"
echo 'not json' | ROOST_NOTIFY=1 "$B" claude-hook; echo "bad json: exit $?"
echo '{"hook_event_name":"Stop"}' | ROOST_NOTIFY=1 setsid "$B" claude-hook > /dev/null; echo "nowhere: exit $?"
```

Expected: three lines each ending `exit 0`; the third also prints the
one-line stderr notice. (`setsid` detaches from the controlling terminal so
`/dev/tty` cannot open, which is the `Nowhere` case.) Paste the output into
the report.

- [ ] **Step 6: Revert-check**

Change `"idle_prompt" => "is waiting for your input"` to return `None`
and run: `hook_message_maps_each_handled_event` fails on the idle row.
Restore. Change `clean.chars().take(120)` to `take(500)`:
`stop_body_is_the_first_line_capped_and_sanitised` fails on the count.
Restore. Record both in the doc comments.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "cli: roost claude-hook turns Claude Code's hook JSON into a notification, silently elsewhere"
```

---

### Task 2: The settings file reader and writer

**Files:**
- Create: `src/claudehooks.rs`
- Modify: `src/lib.rs` (add `pub mod claudehooks;` in alphabetical position among the `pub mod` lines)
- Modify: `Cargo.toml:39` (`serde_json = { version = "1", features = ["preserve_order"] }`)

**Interfaces:**
- Consumes: `crate::projects::safe_resolve_parent(project_dir: &Path, rel: &str) -> Result<PathBuf, String>`.
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum HookState { Present, Absent, Unknown(String) }
  pub fn state(project_dir: &Path) -> HookState
  pub fn set(project_dir: &Path, on: bool) -> Result<(), String>
  pub const COMMAND: &str = "roost claude-hook";
  ```

- [ ] **Step 1: Write the failing tests**

Create `src/claudehooks.rs` with only the module doc and the test module
for now:

```rust
//! The two hook entries roost owns in a project's `.claude/settings.local.json`.
//!
//! Claude Code raises no notification of its own; a hook has to run
//! `roost claude-hook`. This module is the only code that reads or writes
//! that file, and it touches exactly the entries whose command is that
//! string — a user's other hooks, other keys and their order survive every
//! write. It writes the *local* settings file, the one Claude Code keeps
//! personal and gitignored, never the committed one and never the global
//! one: a clone of the repo must not inherit a hook that runs roost.
//!
//! Reading has three outcomes, not two. A file that exists but cannot be
//! parsed is `Unknown`, and `Unknown` refuses to write: rewriting a file we
//! could not read is how a hand-edited settings file gets destroyed.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn proj() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
    fn read(p: &Path) -> String {
        std::fs::read_to_string(p.join(".claude/settings.local.json")).unwrap()
    }
    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.join(".claude")).unwrap();
        std::fs::write(p.join(".claude/settings.local.json"), s).unwrap();
    }

    const OURS: &str = r#"{ "type": "command", "command": "roost claude-hook", "timeout": 5 }"#;

    #[test]
    fn a_missing_file_is_absent_and_enable_writes_exactly_the_two_entries() {
        let d = proj();
        assert_eq!(state(d.path()), HookState::Absent);
        set(d.path(), true).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"hooks":{{"Notification":[{{"hooks":[{OURS}]}}],"Stop":[{{"hooks":[{OURS}]}}]}}}}"#
        )).unwrap();
        let got: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(got, expected);
        assert!(read(d.path()).ends_with("}\n"), "two-space pretty JSON with a trailing newline");
        assert_eq!(state(d.path()), HookState::Present);
    }

    #[test]
    fn enable_is_idempotent_byte_for_byte() {
        let d = proj();
        set(d.path(), true).unwrap();
        let once = read(d.path());
        set(d.path(), true).unwrap();
        assert_eq!(read(d.path()), once);
    }

    /// Foreign content survives: other hooks on Stop, an unrelated event,
    /// unrelated keys, and their order.
    #[test]
    fn enable_keeps_every_foreign_byte_of_content_and_key_order() {
        let d = proj();
        write(d.path(), r#"{
  "permissions": { "allow": ["Bash(ls *)"] },
  "hooks": {
    "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ],
    "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "lint" } ] } ]
  },
  "zeta": 1
}
"#);
        set(d.path(), true).unwrap();
        let s = read(d.path());
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls *)");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done", "foreign Stop group first");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], "roost claude-hook", "ours appended");
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(v["hooks"]["Notification"][0]["hooks"][0]["command"], "roost claude-hook");
        assert_eq!(v["zeta"], 1);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["permissions", "hooks", "zeta"], "top-level order kept");
        let hooks: Vec<&str> = v["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(hooks, ["Stop", "PreToolUse", "Notification"], "event order kept, new one last");
        assert_eq!(state(d.path()), HookState::Present);
    }

    #[test]
    fn disable_removes_only_ours_and_prunes_what_it_empties() {
        let d = proj();
        write(d.path(), &format!(r#"{{
  "hooks": {{
    "Stop": [ {{ "hooks": [ {{ "type": "command", "command": "say done" }}, {OURS} ] }} ],
    "Notification": [ {{ "hooks": [ {OURS} ] }} ]
  }},
  "other": true
}}
"#));
        assert_eq!(state(d.path()), HookState::Present);
        set(d.path(), false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(), 1, "foreign entry kept");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert!(v["hooks"].get("Notification").is_none(), "an emptied event is dropped");
        assert_eq!(v["other"], true);
        assert_eq!(state(d.path()), HookState::Absent);

        // Nothing but ours: `hooks` itself goes.
        let d = proj();
        set(d.path(), true).unwrap();
        set(d.path(), false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert!(v.get("hooks").is_none(), "{v}");
    }

    #[test]
    fn one_event_present_is_absent_and_enable_adds_only_the_missing_one() {
        let d = proj();
        write(d.path(), &format!(r#"{{"hooks":{{"Stop":[{{"hooks":[{OURS}]}}]}}}}"#));
        assert_eq!(state(d.path()), HookState::Absent);
        set(d.path(), true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1, "not duplicated");
        assert_eq!(v["hooks"]["Notification"].as_array().unwrap().len(), 1);
        assert_eq!(state(d.path()), HookState::Present);
    }

    /// Unknown refuses both directions and touches nothing.
    #[test]
    fn invalid_json_is_unknown_and_never_written() {
        let d = proj();
        write(d.path(), "{ not json");
        assert!(matches!(state(d.path()), HookState::Unknown(_)));
        let e = set(d.path(), true).unwrap_err();
        assert!(e.contains("settings.local.json"), "{e}");
        assert!(set(d.path(), false).is_err());
        assert_eq!(read(d.path()), "{ not json");
        assert!(!d.path().join(".claude/settings.local.json.bak").exists(), "no backup of a refused write");

        let d = proj();
        write(d.path(), r#"{"hooks": 5}"#);
        assert!(matches!(state(d.path()), HookState::Unknown(_)), "hooks that is not an object");
        let d = proj();
        write(d.path(), r#"[]"#);
        assert!(matches!(state(d.path()), HookState::Unknown(_)), "a top-level array");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        write(d.path(), "{}");
        let p = d.path().join(".claude/settings.local.json");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file anyway; then this test cannot
        // arrange the condition and says so rather than passing vacuously.
        let arranged = std::fs::read_to_string(&p).is_err();
        let s = state(d.path());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        if !arranged {
            eprintln!("skipped: running as a user that can read a mode-000 file");
            return;
        }
        assert!(matches!(s, HookState::Unknown(_)), "{s:?}");
    }

    #[test]
    fn the_backup_is_written_once_and_never_overwritten() {
        let d = proj();
        write(d.path(), r#"{"pristine": true}"#);
        set(d.path(), true).unwrap();
        let bak = d.path().join(".claude/settings.local.json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"pristine": true}"#);
        set(d.path(), false).unwrap();
        set(d.path(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"pristine": true}"#, "still the pre-roost file");

        // A file that did not exist has nothing to back up.
        let d = proj();
        set(d.path(), true).unwrap();
        assert!(!d.path().join(".claude/settings.local.json.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writes_are_atomic_and_keep_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        write(d.path(), "{}");
        let p = d.path().join(".claude/settings.local.json");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        set(d.path(), true).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join(".claude")).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test claudehooks -- --test-threads=1`
Expected: compile errors, `cannot find function state` and `set`, `HookState`.
(Add `pub mod claudehooks;` to `src/lib.rs` first so the module is compiled
at all; the errors must come from the missing items, not a missing module.)

- [ ] **Step 3: Write the module**

Insert above the test module in `src/claudehooks.rs`:

```rust
use std::path::{Path, PathBuf};

/// The command roost installs. Ownership is this exact string, nothing
/// looser: a user who writes their own `roost notify` hook keeps it.
pub const COMMAND: &str = "roost claude-hook";
const EVENTS: [&str; 2] = ["Notification", "Stop"];
const REL: &str = ".claude/settings.local.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookState {
    /// Both events carry a roost entry.
    Present,
    /// No file, or fewer than both events carry one.
    Absent,
    /// The file exists but could not be read or is not the shape this
    /// module knows how to rewrite. The string says why, for the UI.
    Unknown(String),
}

fn path(project_dir: &Path) -> PathBuf {
    project_dir.join(REL)
}

/// The parsed document, `None` for a missing file, `Err` for `Unknown`.
fn load(project_dir: &Path) -> Result<Option<serde_json::Value>, String> {
    let p = path(project_dir);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{REL}: cannot read: {e}")),
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{REL}: not valid JSON: {e}"))?;
    if !v.is_object() {
        return Err(format!("{REL}: top level is not an object"));
    }
    if let Some(h) = v.get("hooks") {
        if !h.is_object() {
            return Err(format!("{REL}: \"hooks\" is not an object"));
        }
    }
    Ok(Some(v))
}

fn is_ours(entry: &serde_json::Value) -> bool {
    entry.get("command").and_then(|c| c.as_str()) == Some(COMMAND)
}

fn our_entry() -> serde_json::Value {
    serde_json::json!({ "type": "command", "command": COMMAND, "timeout": 5 })
}

/// Whether `event`'s array in `hooks` holds a roost entry in any group.
fn event_has_ours(hooks: &serde_json::Value, event: &str) -> bool {
    hooks
        .get(event)
        .and_then(|a| a.as_array())
        .map_or(false, |groups| {
            groups.iter().any(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array())
                    .map_or(false, |entries| entries.iter().any(is_ours))
            })
        })
}

pub fn state(project_dir: &Path) -> HookState {
    match load(project_dir) {
        Err(why) => HookState::Unknown(why),
        Ok(None) => HookState::Absent,
        Ok(Some(v)) => {
            let hooks = v.get("hooks").cloned().unwrap_or(serde_json::Value::Null);
            if EVENTS.iter().all(|e| event_has_ours(&hooks, e)) {
                HookState::Present
            } else {
                HookState::Absent
            }
        }
    }
}

/// Adds or removes roost's entries in `doc`, touching nothing else.
fn merge(doc: &mut serde_json::Value, on: bool) {
    let obj = doc.as_object_mut().expect("load guarantees an object");
    if on {
        let hooks = obj
            .entry("hooks")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        for event in EVENTS {
            if event_has_ours(hooks, event) {
                continue;
            }
            let groups = hooks
                .as_object_mut()
                .expect("load guarantees an object")
                .entry(event)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(arr) = groups.as_array_mut() {
                // A group of our own, never an entry inside a foreign group:
                // disabling then removes it without deciding what to do with
                // a group we share.
                arr.push(serde_json::json!({ "hooks": [our_entry()] }));
            }
        }
        return;
    }
    let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else { return };
    for event in EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(|a| a.as_array_mut()) else { continue };
        for g in groups.iter_mut() {
            if let Some(entries) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                entries.retain(|e| !is_ours(e));
            }
        }
        groups.retain(|g| {
            g.get("hooks").and_then(|h| h.as_array()).map_or(true, |e| !e.is_empty())
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
}

/// Enables or disables roost's hooks in the project's local settings.
///
/// Refuses on `Unknown`: the file is left byte-for-byte alone. Otherwise
/// writes temp-then-rename in the same directory (a crash mid-write leaves
/// the old file intact, and a reader never sees a half-written one), keeps
/// an existing file's mode, and copies the pre-roost file to `.bak` the
/// first time it replaces one — never again, so the backup stays the state
/// before roost touched anything.
pub fn set(project_dir: &Path, on: bool) -> Result<(), String> {
    let mut doc = load(project_dir)?.unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    merge(&mut doc, on);
    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    text.push('\n');

    // Confined like every other creation path: the parent is canonicalised
    // and the final component validated, because the file may not exist yet.
    let dir = crate::projects::safe_resolve_parent(project_dir, ".claude")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{REL}: cannot create .claude: {e}"))?;
    let target = crate::projects::safe_resolve_parent(project_dir, REL)?;
    // `symlink_metadata`, not `exists()`: "cannot look" must not read as
    // "absent" and skip the backup of a file that is there.
    let existing = match std::fs::symlink_metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{REL}: cannot stat: {e}")),
    };

    // The pre-roost file, kept once and never overwritten.
    if existing.is_some() {
        let bak = dir.join("settings.local.json.bak");
        if !bak.exists() {
            std::fs::copy(&target, &bak).map_err(|e| format!("{REL}: cannot write backup: {e}"))?;
        }
    }

    let tmp = dir.join(format!("settings.local.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("{REL}: cannot write: {e}"))?;
    if let Some(meta) = &existing {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{REL}: cannot replace: {e}")
    })
}
```

Then in `Cargo.toml` change the `serde_json` line to
`serde_json = { version = "1", features = ["preserve_order"] }`, and add
`pub mod claudehooks;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test claudehooks -- --test-threads=1`
Expected: 9 passed (8 on a non-unix host).

Then confirm key order really is preserved by the feature, not by luck:
the `enable_keeps_every_foreign_byte_of_content_and_key_order` test's two
order assertions are the check.

- [ ] **Step 5: Revert-check**

One at a time, restoring between:

1. In `merge`, replace `entries.retain(|e| !is_ours(e))` with
   `entries.clear()`: `disable_removes_only_ours_and_prunes_what_it_empties`
   fails on "foreign entry kept".
2. In `load`, replace the `NotFound` arm with `Err(...)`:
   `a_missing_file_is_absent_and_enable_writes_exactly_the_two_entries`
   fails on the first assertion (Unknown instead of Absent).
3. In `set`, drop the `if !bak.exists()` guard:
   `the_backup_is_written_once_and_never_overwritten` fails on "still the
   pre-roost file".
4. In `state`, replace `.all(` with `.any(`:
   `one_event_present_is_absent_and_enable_adds_only_the_missing_one` fails
   on the first assertion.
5. Remove `features = ["preserve_order"]` from `Cargo.toml`:
   `enable_keeps_every_foreign_byte_of_content_and_key_order` fails on
   "top-level order kept" (serde_json's default map is sorted). Restore.

Record each in its test's doc comment.

- [ ] **Step 6: Run the whole suite and commit**

Run: `cargo test -- --test-threads=1`. Expected: green.

```bash
git add src/claudehooks.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "claudehooks: read three states and rewrite the local Claude settings around roost's own entries"
```

---

### Task 3: Intent, snapshot field, hub arm, integration test

**Files:**
- Modify: `src/proto.rs` (`Intent` enum near line 141; `WorkspaceView` near line 206)
- Modify: `src/workspace.rs` (`view()`; the `WorkspaceView { … }` literal)
- Modify: `src/hub.rs` (`handle`, an arm next to `ClearNotices` around line 414; `snapshot_event` around line 340)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `claudehooks::{state, set, HookState}`.
- Produces: `Intent::SetClaudeHooks { on: bool }`, `WorkspaceView.claude_hooks: Option<bool>` (serialised as `"claude_hooks": true|false|null`).

- [ ] **Step 1: Write the failing integration test**

In `tests/integration.rs`, after
`the_header_toggle_overrides_the_config_file_in_both_directions`:

```rust
// The bell's switch: an intent over the workspace socket writes the
// project's local Claude settings, and the next State reports what the
// file says — read back from disk, not echoed from memory.
//
// Own project name: this test writes into the project directory and mutates
// hub state (see `fixture_named`).
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
```

`read_until(ws, needle) -> String` returns the text of the frame that
matched, which is what the two `contains` assertions read.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test integration set_claude_hooks -- --test-threads=1`
Expected: fails at the first `claude_hooks` assertion (the field does not
exist yet), or the intent is rejected as unknown.

- [ ] **Step 3: Add the intent and the field**

`src/proto.rs`, in `Intent` after `SetShowHidden { on: bool }`:

```rust
    /// Enable or disable roost's two Claude Code hooks in this project's
    /// `.claude/settings.local.json`. Applies to the project, not the
    /// connection: every browser on it sees the new state.
    SetClaudeHooks { on: bool },
```

In `WorkspaceView` after `show_hidden`:

```rust
    /// Whether roost's Claude hooks are installed in the project's local
    /// Claude settings: `Some(true)` present, `Some(false)` absent, `None`
    /// when the file cannot be read or parsed. Derived in
    /// `hub::snapshot_event` by reading the file, never stored: the file is
    /// the truth and a hand edit must show on the next snapshot.
    pub claude_hooks: Option<bool>,
```

`src/workspace.rs`, in `view()`'s `WorkspaceView { … }` literal, add
`claude_hooks: None,` (the hub fills it).

- [ ] **Step 4: Fill it in the snapshot and handle the intent**

`src/hub.rs`, in `snapshot_event`, next to the `claude_sessions` line:

```rust
        ws.claude_hooks = match crate::claudehooks::state(&self.dir) {
            crate::claudehooks::HookState::Present => Some(true),
            crate::claudehooks::HookState::Absent => Some(false),
            crate::claudehooks::HookState::Unknown(_) => None,
        };
```

In `handle`, after the `ClearNotices` arm:

```rust
            Intent::SetClaudeHooks { on } => {
                // A file write under the hub lock, like CreateFile below:
                // one small file, and the state it changes is what every
                // client of this project is about to be sent.
                if let Err(e) = crate::claudehooks::set(&self.dir, *on) {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
                self.ws.version += 1;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                return;
            }
```

`self.ws.version += 1` follows the other state-changing arms, so a client
that ever keys on the version sees a change. `self.dir` is the project
directory (`PathBuf`) the hub already holds.

- [ ] **Step 5: Run the test to verify it passes, then the suite**

Run: `cargo test --test integration set_claude_hooks -- --test-threads=1`
Expected: PASS.
Run: `cargo test -- --test-threads=1`. Expected: green (a `WorkspaceView`
literal elsewhere in tests may need `claude_hooks: None` added; the
compiler names them).

- [ ] **Step 6: Revert-check**

Replace the `snapshot_event` match with `ws.claude_hooks = Some(false);`:
the integration test fails at `read_until(… "claude_hooks":true …)` with
a timeout. Restore. Replace `set(&self.dir, *on)` with `Ok::<(), String>(())`:
it fails at "the file was written". Restore. Record both in the test's
comment.

- [ ] **Step 7: Commit**

```bash
git add src/proto.rs src/workspace.rs src/hub.rs tests/integration.rs
git commit -m "hub: SetClaudeHooks writes the project's local Claude settings, and State reports what the file says"
```

---

### Task 4: The bell mark, the panel row, and the browser test

**Files:**
- Modify: `static/app.js` (`renderNotices` around line 2415; the State handler that assigns `state`; the bell's `title`)
- Modify: `static/style.css` (after the `#bell` rules around line 635)
- Modify: `src/render.rs` (the bell button, `title="notifications (n)"` around line 1639: no change to the markup is needed; the attribute is set from JS)
- Create: `tests/browser/claudehooks.mjs`
- Modify: `tests/browser/README.md` (run list, after the `dotfiles.mjs` line)

**Interfaces:**
- Consumes: `state.claude_hooks` (`true` | `false` | `null`), `send({ t: "SetClaudeHooks", on })`, `showError(msg)` for the `Error` event (already wired).
- Produces: `data-claude-hooks="on|off|unknown"` on `#bell`; a `.hookrow` element as the notice panel's first child with a `.hookrow button` for the switch and `.hookrow .confirm` for the confirmation.

- [ ] **Step 1: Write the browser test**

```js
//! The bell's Claude-hooks switch: the mark on the bell, the row in the
//! panel, the confirmation, and the write it ends in.
//!
//! Worth a browser test for the reason dotfiles.mjs is: the intent and the
//! file rewrite are covered server-side (claudehooks.rs, integration.rs),
//! and all of that can be right while the row sends nothing, the mark draws
//! the wrong state, or a second browser on the project never learns of the
//! flip. Three client paths get their own assertion: the browser that
//! clicked, one that did not, and the unknown state, which must show a
//! reason and no button.
//!
//! The four traps in README.md apply: every assertion names an element or
//! a file, and the file is read from disk, not inferred from the DOM.
//!
//! Revert-the-fix, watched fail and restored:
//!   1. Made the Enable button send nothing (commented out the send in
//!      app.js). "the settings file now holds both events" failed.
//!   2. Removed the `data-claude-hooks` assignment. "the bell is marked
//!      off before enabling" failed.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const settings = `${fx.roots}/proj/.claude/settings.local.json`;
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${roost.port}/proj`;

const wire = (page) => {
  const { evalIn } = page;
  const ready = () => until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app");
  const mark = () => evalIn(`document.getElementById("bell").dataset.claudeHooks`);
  const openPanel = async () => {
    await evalIn(`document.getElementById("noticepanel").hidden = true`);
    await evalIn(`document.getElementById("bell").click()`);
    await until(() => evalIn(`!document.getElementById("noticepanel").hidden`), 5, "panel");
  };
  const rowText = () => evalIn(`(document.querySelector("#noticepanel .hookrow") || {}).textContent || ""`);
  const buttonText = () => evalIn(`(document.querySelector("#noticepanel .hookrow button") || {}).textContent || ""`);
  // Through the real elements, never send(): a button wired to nothing is
  // exactly the defect this file exists to catch.
  const clickButton = () => evalIn(`(() => { const b = document.querySelector("#noticepanel .hookrow button"); if (!b) return false; b.click(); return true; })()`);
  const confirmYes = () => evalIn(`(() => { const b = [...document.querySelectorAll("#noticepanel .hookrow .confirm button")].find((x) => /^(Enable|Disable)$/.test(x.textContent)); if (!b) return false; b.click(); return true; })()`);
  const confirmText = () => evalIn(`(document.querySelector("#noticepanel .hookrow .confirm") || {}).textContent || ""`);
  return { evalIn, ready, mark, openPanel, rowText, buttonText, clickButton, confirmYes, confirmText, close: page.close };
};

let one, two;
try {
  one = wire(await openPage(browser.port, url));
  two = wire(await openPage(browser.port, url));
  ok(await one.ready() && await two.ready(), "two pages are up on the same project");

  ok(await one.mark() === "off", "the bell is marked off before enabling");
  await one.openPanel();
  ok(/Claude notifications for this project: off/.test(await one.rowText()), "the panel's first row says off");
  ok(await one.buttonText() === "Enable", "and offers Enable");

  ok(await one.clickButton(), "Enable is clickable");
  ok(/settings\.local\.json/.test(await one.confirmText()), "the confirmation names the file it will write");
  ok(await one.confirmYes(), "and can be confirmed");

  const written = await until(async () => {
    try { const v = JSON.parse(await Deno.readTextFile(settings)); return !!(v.hooks && v.hooks.Stop && v.hooks.Notification); } catch { return false; }
  }, 10, "settings file");
  ok(written, "the settings file now holds both events");
  ok(await until(() => one.mark().then((m) => m === "on"), 5, "mark on"), "the clicking browser's bell is unmarked (on)");
  ok(await until(() => two.mark().then((m) => m === "on"), 5, "mirror"), "the other browser's bell followed");

  await one.openPanel();
  ok(await one.buttonText() === "Disable", "the row now offers Disable");
  ok(await one.clickButton() && await one.confirmYes(), "Disable, confirmed");
  const emptied = await until(async () => {
    try { const v = JSON.parse(await Deno.readTextFile(settings)); return !v.hooks; } catch { return false; }
  }, 10, "hooks removed");
  ok(emptied, "the file no longer holds hooks");
  ok(await until(() => one.mark().then((m) => m === "off"), 5, "mark off"), "the bell is marked off again");

  // Unknown: a file roost cannot parse shows a reason and no button.
  await Deno.writeTextFile(settings, "{ broken");
  await one.evalIn(`send({ t: "RequestState" })`);
  ok(await until(() => one.mark().then((m) => m === "unknown"), 5, "unknown"), "an unparseable file marks the bell unknown");
  await one.openPanel();
  ok(/cannot tell/.test(await one.rowText()), "the row says it cannot tell");
  ok(await one.buttonText() === "", "and offers no button");
} finally {
  try { await one?.close?.(); } catch {}
  try { await two?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
```

- [ ] **Step 2: Run it to verify it fails**

Run: `deno run -A tests/browser/claudehooks.mjs`
Expected: "the bell is marked off before enabling" FAILs (no attribute yet)
and the rest cascade.

- [ ] **Step 3: The client**

In `static/app.js`, find where a `State` event assigns `state` and
re-renders (search for `renderNotices()` calls after state assignment, or
the handler for `ev.t === "State"`). After the assignment add a call
`renderClaudeHooks();`. Then add, just above `function renderNotices()`:

```js
// The bell's Claude-hooks state. Three values, never two: `null` means the
// server could not read or parse the settings file, and that gets a reason
// and no button, not a guess.
let hookConfirm = null; // "on" | "off" while a confirmation is showing
function hookState() {
  if (!state || state.claude_hooks === undefined) return "unknown";
  return state.claude_hooks === null ? "unknown" : (state.claude_hooks ? "on" : "off");
}
function renderClaudeHooks() {
  const bell = document.getElementById("bell");
  if (!bell) return;
  const s = hookState();
  bell.dataset.claudeHooks = s;
  const word = { on: "on", off: "off", unknown: "cannot tell" }[s];
  bell.title = `notifications (n) · Claude notifications for this project: ${word}`;
}
// The panel's first row: state, and the switch behind a one-line
// confirmation, because it writes into a file roost does not own.
function hookRow() {
  const row = document.createElement("div");
  row.className = "hookrow";
  const s = hookState();
  const label = document.createElement("span");
  if (s === "unknown") {
    label.textContent = "Claude notifications: cannot tell — .claude/settings.local.json could not be read or parsed";
    row.appendChild(label);
    return row;
  }
  label.textContent = `Claude notifications for this project: ${s}`;
  row.appendChild(label);
  if (hookConfirm) {
    const c = document.createElement("span");
    c.className = "confirm";
    const q = document.createElement("span");
    q.textContent = hookConfirm === "on"
      ? "Write two hooks to .claude/settings.local.json? "
      : "Remove roost's hooks from .claude/settings.local.json? ";
    const yes = document.createElement("button");
    yes.textContent = hookConfirm === "on" ? "Enable" : "Disable";
    yes.onclick = (e) => { e.stopPropagation(); send({ t: "SetClaudeHooks", on: hookConfirm === "on" }); hookConfirm = null; renderNotices(); };
    const no = document.createElement("button");
    no.textContent = "Cancel";
    no.onclick = (e) => { e.stopPropagation(); hookConfirm = null; renderNotices(); };
    c.append(q, yes, no);
    row.appendChild(c);
    return row;
  }
  const b = document.createElement("button");
  b.textContent = s === "on" ? "Disable" : "Enable";
  b.onclick = (e) => { e.stopPropagation(); hookConfirm = s === "on" ? "off" : "on"; renderNotices(); };
  row.appendChild(b);
  return row;
}
```

In `renderNotices`, right after `panel.replaceChildren();`, add
`panel.appendChild(hookRow());` so the row is always first. Also call
`renderClaudeHooks()` once at the end of `renderNotices` so the mark and
the tooltip stay in step with the row.

In `static/style.css`, after the `#bell { position: relative; }` rule:

```css
/* The Claude-hooks mark. On is the quiet state and draws nothing. */
#bell[data-claude-hooks="off"]::after,
#bell[data-claude-hooks="unknown"]::after {
  content: "✻"; position: absolute; right: 2px; bottom: 0; font-size: 9px; line-height: 1;
  color: var(--muted); text-decoration: line-through;
}
#bell[data-claude-hooks="unknown"]::after { content: "?"; text-decoration: none; color: var(--warn); }
#noticepanel .hookrow { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; padding: 6px 8px; border-bottom: 1px solid var(--border); color: var(--fg); }
#noticepanel .hookrow .confirm { display: flex; gap: 6px; align-items: center; }
```

Use only tokens the five themes already define (`--muted`, `--warn`,
`--border`, `--fg`); see the transient-messages plan's constraint on new
tokens.

- [ ] **Step 4: Run the browser test to verify it passes**

Run: `deno run -A tests/browser/claudehooks.mjs`
Expected: `all passed`. Run one browser test at a time on this host.

- [ ] **Step 5: Revert-check**

The two reverts named in the file's header, one at a time, watched to
fail, restored; then a green run. Update the header text to the failures
actually observed.

- [ ] **Step 6: README run line and commit**

In `tests/browser/README.md`, after the `dotfiles.mjs` line:

```
deno run -A tests/browser/claudehooks.mjs # the bell's Claude-hooks switch: mark, row, confirmation, the file it writes, and the mirror
```

```bash
git add static/app.js static/style.css tests/browser/claudehooks.mjs tests/browser/README.md
git commit -m "bell: the Claude-hooks mark and the switch, behind a confirmation that names the file"
```

---

### Task 5: Docs

**Files:**
- Modify: `docs/notifications.md` (section "Firing automatically from Claude Code", line 51 on)
- Modify: `README.md` (the "Desktop notifications" paragraph)
- Modify: `CLAUDE.md` ("Hard constraints" list, after the raw-HTML bullet)

- [ ] **Step 1: notifications.md**

Replace the "Firing automatically from Claude Code" section's opening (the
paragraph and the JSON snippet, keeping the "Existing hooks and scripts
must be updated by hand" paragraph that follows) with:

```markdown
## Firing automatically from Claude Code

Open the bell in a project's workspace. Its first row says whether Claude
notifications are on for that project and offers the switch. Enabling
writes two hook entries into the project's `.claude/settings.local.json`
(the personal, gitignored one; the committed `settings.json` and the
global `~/.claude/settings.json` are never touched), each running
`roost claude-hook`:

```json
{
  "hooks": {
    "Notification": [ { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] } ],
    "Stop":         [ { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] } ]
  }
}
```

Disabling removes exactly those entries and nothing else; other hooks,
other keys and their order survive. The first time roost replaces an
existing file it copies it to `settings.local.json.bak` and never
overwrites that copy. A file roost cannot parse shows on the bell as
"cannot tell" and is never written.

`roost claude-hook` reads the event Claude Code pipes on stdin and raises:

| Event | Title | Body |
|---|---|---|
| `Notification` · `permission_prompt` | Claude needs you | wants permission to run a tool |
| `Notification` · `idle_prompt` | Claude needs you | is waiting for your input |
| `Notification` · `agent_needs_input` | Claude needs you | an agent needs your input |
| `Notification` · `elicitation_dialog`, `elicitation_url_dialog` | Claude needs you | is asking a question |
| `Stop` | Claude finished | the first line of Claude's last message |

Anything else is ignored. Unlike `roost notify`, this command always exits
0 and is silent when `ROOST_NOTIFY` is unset: the same project is used
outside roost, and a `Stop` hook that exits non-zero shows an error in
every such session. The snippet above is also what to paste by hand into
any other settings file if you want the hooks somewhere roost will not
write.
```

- [ ] **Step 2: README.md**

Replace the "Desktop notifications" paragraph's first two sentences with:

```markdown
A process in a terminal raises a notification with one escape sequence or
`roost notify`. For Claude, the bell has a switch: on, and a Claude Code
hook in that project raises one when a turn finishes or Claude is waiting
on you. It shows as a bell across every project and, in a secure context,
as an OS notification that clicks back to the terminal that raised it.
```

Keep the trailing "See [docs/notifications.md]…" sentence.

- [ ] **Step 3: CLAUDE.md**

After the raw-HTML bullet in "Hard constraints", add one bullet in the
file's voice:

```markdown
- **roost writes exactly one Claude settings file**, the project's
  `.claude/settings.local.json`, through `claudehooks::set`, and in it only
  entries whose command is exactly `roost claude-hook`. Never the committed
  `settings.json` (a clone would inherit a hook that runs roost) and never
  `~/.claude/settings.json`. A file it cannot parse is `Unknown`, which
  refuses to write: rewriting a settings file we could not read is how a
  hand-edited one gets destroyed. And `roost claude-hook` always exits 0
  and is silent without `ROOST_NOTIFY`, unlike `roost notify`'s deliberate
  loud exit 1 — a `Stop` hook that exits non-zero shows an error in every
  Claude session run outside roost in the same checkout.
```

- [ ] **Step 4: Commit**

```bash
git add docs/notifications.md README.md CLAUDE.md
git commit -m "docs: the bell's Claude-hooks switch, roost claude-hook, and the one settings file roost writes"
```

---

### Task 6: Verify on the live instance

**Files:** none changed.

- [ ] **Step 1: Suite and both browser tests**

```bash
cargo test -- --test-threads=1
deno run -A tests/browser/claudehooks.mjs
deno run -A tests/browser/dotfiles.mjs
```

Expected: all green.

- [ ] **Step 2: Real hook, real Claude, real notification**

After deploying per `docs/deploy.md` (build, install, restart, confirm the
running binary's hash), open this repository's workspace in a browser,
enable the switch from the bell, confirm `.claude/settings.local.json` in
this checkout holds both events, then in a roost terminal in this project
run `claude` and ask it something trivial. When it finishes, the bell
should show one "Claude finished" notice attributed to this project and
session. Report what appeared; if nothing did, `cat ~/.local/state/resh/notify/notices.json`
and the Claude transcript's hook output are the two places to look.
