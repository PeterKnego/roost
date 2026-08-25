# Worktree Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire `resh peers`; make ✻ in a project that already has a Claude offer a new worktree instead; show each worktree's state in the switcher and offer removal only on positive evidence it is finished.

**Architecture:** Detection uses two signals resh already owns (a terminal's parked Claude launch, kept on the `Session` record from now on, and IDE-socket connections) folded into a three-valued `ClaudeEvidence`. Worktree creation, state and removal are pure-ish functions in `worktree.rs` that take a git runner, so every branch is unit-tested against a fake runner *and* against real git; the hub wires them in with the same "never hold the hub lock across git" thread pattern `do_end_session` uses. The browser opens the new tab synchronously on the click and navigates it when `WorktreeReady` arrives.

**Tech Stack:** Rust (std only — the uuid is minted from `/dev/urandom`, no new crate), `serde`, real `git` in tests via `tempfile`, Deno + headless Chromium for the browser test (`tests/browser/harness.mjs`).

**Spec:** `docs/superpowers/specs/2026-08-25-worktree-launch-design.md`

## Global Constraints

- `cargo test`, never `cargo test --release`. Build from this checkout only (shared target dir — see `CLAUDE.md` "Build from one checkout").
- Every new test must be revert-checked: apply the broken version, run, read the failure, restore, and say so in the test's comment.
- Never hold a hub or session lock across a `git` call or any blocking I/O.
- Destruction requires positive evidence: `symlink_metadata` + `Err(NotFound)` is "gone"; any other `Err` is "cannot tell, do nothing".
- Every filesystem path is confined: `projects::safe_resolve_parent` for the worktree path (it does not exist yet).
- Session names match `^[A-Za-z0-9_-]{1,32}$`; the minted uuid must match `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` before it is typed into a shell.
- All HTML built in `render.rs`, everything interpolated escaped with `esc`.
- Config key `worktree_prompt` is **global-only** (`~/.config/resh/config.toml`); default `true`.
- Worktree name is `claude-N`, N ∈ 1..=64, path `.claude/worktrees/claude-N`, branch `claude-N`.
- Commit with explicit paths (`git add <files>`), never `git add -A` — the user keeps uncommitted notes in the tree.
- Hub tests set env under `wsstate::STATE_ENV_LOCK` then `session::SESSION_ENV_LOCK`, in that order, with `RESH_CMD=cat` and a temp `RESH_STATE_DIR` — copy the fixture at `src/hub.rs:2616-2630`.

---

## File map

| File | Change |
|---|---|
| `src/peers.rs`, `docs/peers.md` | deleted |
| `src/cli.rs`, `src/main.rs`, `src/lib.rs`, `docs/deploy.md`, `docs/backlog.md` | peers references removed; deploy step + config row added |
| `src/launch.rs` | `keystrokes(launch, session_id)`, `valid_session_id`, `new_session_id` |
| `src/session.rs` | `LaunchRequest`; launch kept on `Session`; `launched_names` |
| `src/term.rs` | types the new keystrokes |
| `src/claudes.rs` (new) | `ClaudeEvidence`, `evidence_from`, `claude_evidence` |
| `src/ide.rs` | `connected_sessions(project)` |
| `src/config.rs` | `worktree_prompt()` |
| `src/proto.rs` | `NewTerminal.force`, `NewWorktree`, `RemoveWorktree`, `ClaudeHere`, `WorktreeReady` |
| `src/worktree.rs` | `create`, `state`, `remove`, `.base` helpers |
| `src/hub.rs` | prompt in `do_new_terminal`; `do_new_worktree`; `do_remove_worktree` |
| `src/registry.rs` | `WorktreeStatus` on `ProjectStatus`; `known_projects_with_state`; orphan `.base` reaping |
| `src/routes.rs` | `/frag/_worktrees?state=1` |
| `src/render.rs` | state fields and remove control in `worktrees_strip` |
| `static/app.js`, `static/style.css` | prompt, tab opening, `?launch=` consumption, switcher state + remove |
| `tests/browser/worktree-launch.mjs`, `tests/browser/harness.mjs`, `tests/browser/README.md` | browser test |

---

### Task 1: Retire `resh peers`

**Files:**
- Delete: `src/peers.rs`, `docs/peers.md`
- Modify: `src/lib.rs:19`, `src/cli.rs:216-300` (the `run_peers` fn and its imports), `src/main.rs:5,15-19,30-45`, `docs/deploy.md:40-41,63`, `docs/backlog.md:247`

**Interfaces:**
- Produces: nothing; later tasks must not reference `crate::peers`.

- [ ] **Step 1: Delete the module and its doc**

```bash
git rm -q src/peers.rs docs/peers.md
```

- [ ] **Step 2: Remove the wiring**

In `src/lib.rs` delete the line `pub mod peers;`.

In `src/main.rs` delete the arm `Some("peers") => std::process::exit(resh::cli::run_peers(&args[1..])),`. In the "no project roots configured" message replace the two lines

```
             Or list them in ~/.config/resh/config.toml, which callers that do not\n\
             inherit the unit's environment (such as `resh peers`) also read:\n\
```
with
```
             Or list them in ~/.config/resh/config.toml:\n\
```
In the roots-conflict message replace `config \`roots\` (used by \`resh peers\` and anything without this environment): {cfg:?}` with `config \`roots\` (used by anything without this environment): {cfg:?}`.

In `src/cli.rs` delete `pub fn run_peers` entirely (from its doc comment through its closing brace). `errlog` stays — `main.rs` still records roots conflicts through it.

- [ ] **Step 3: Build and fix whatever still names it**

Run: `cargo build 2>&1 | grep -E "peers|error" | head`
Expected: no output. If a test module references `peers`, delete that test — it tested the deleted module.

- [ ] **Step 4: Docs**

`docs/deploy.md:40-41`: delete `and \`resh peers\` (see [\`docs/peers.md\`](peers.md))` so the sentence ends at the notifications link. `docs/deploy.md:63`: replace `which today means \`resh peers\`` with `which today means nothing shipped with resh, but the key stays so a second instance's tooling can read it`. Add, under the *Upgrading* heading (create one after the environment table if absent):

```markdown
## Upgrading from a build that shipped `resh peers`

`resh peers` is gone (spec `docs/superpowers/specs/2026-08-25-worktree-launch-design.md`).
Remove its `SessionStart` entry from `~/.claude/settings.json` on every host that
had it; left in place it prints `command not found` at every session start —
loud, harmless, and the reason this note exists.
```

`docs/backlog.md`, at the end of the *Peer sessions* section (after the last bullet, before the next `###`), add:

```markdown
**Retired 2026-08-25.** Replaced by steering a second Claude into its own
worktree — `docs/superpowers/specs/2026-08-25-worktree-launch-design.md`,
whose opening section records the repro that ended it: resuming a session
still open in another process, and the hook warning a session about itself.
```

- [ ] **Step 5: Run the suite**

Run: `cargo test 2>&1 | tail -3`
Expected: `test result: ok` for every binary; no `peers` in the output.

- [ ] **Step 6: Commit**

```bash
git add -u src/peers.rs docs/peers.md src/lib.rs src/main.rs src/cli.rs docs/deploy.md docs/backlog.md
git commit -m "peers: retire resh peers — its premise (coordinate Claudes sharing a directory) is replaced by steering the second one into a worktree"
```

---

### Task 2: `--session-id` on the launch line

**Files:**
- Modify: `src/launch.rs:36-40` (`keystrokes`), tests at `src/launch.rs:193-197`

**Interfaces:**
- Produces: `pub fn keystrokes(launch: Launch, session_id: Option<&str>) -> Vec<u8>`; `pub fn valid_session_id(s: &str) -> bool`; `pub fn new_session_id() -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Replace the existing `claude_is_typed_as_the_command_and_enter` test with:

```rust
    #[test]
    fn claude_is_typed_with_its_session_id_and_enter() {
        // Revert-checked: with `keystrokes` returning the old `claude\r`
        // this fails on the `--session-id` assertion.
        let id = "0123abcd-0123-4abc-8abc-0123456789ab";
        assert_eq!(
            keystrokes(Launch::Claude, Some(id)),
            format!("claude --session-id {id}\r").into_bytes()
        );
    }

    #[test]
    fn without_an_id_the_bare_command_is_typed() {
        assert_eq!(keystrokes(Launch::Claude, None), b"claude\r".to_vec());
    }

    #[test]
    fn a_malformed_id_is_never_typed() {
        // The id lands on a command line. Anything that is not exactly a
        // uuid falls back to the bare command — the metacharacter must be
        // absent from what is typed, not merely quoted.
        let bad = "0123abcd-0123-4abc-8abc-0123456789ab; rm -rf ~";
        let typed = keystrokes(Launch::Claude, Some(bad));
        assert_eq!(typed, b"claude\r".to_vec());
        assert!(!String::from_utf8_lossy(&typed).contains(';'));
        assert!(!valid_session_id(bad));
        assert!(!valid_session_id("0123ABCD-0123-4abc-8abc-0123456789ab"), "uppercase is not the form claude prints");
        assert!(valid_session_id("0123abcd-0123-4abc-8abc-0123456789ab"));
    }

    #[test]
    fn a_minted_id_is_a_valid_v4_uuid() {
        let id = new_session_id().expect("/dev/urandom is readable on a test host");
        assert!(valid_session_id(&id), "{id}");
        assert_eq!(&id[14..15], "4", "version nibble: {id}");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "variant nibble: {id}");
        assert_ne!(id, new_session_id().unwrap(), "two mints differ");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test launch:: 2>&1 | grep -E "^error|test result" | head -3`
Expected: compile error — `keystrokes` takes one argument; `valid_session_id` and `new_session_id` not found.

- [ ] **Step 3: Implement**

Replace `keystrokes` in `src/launch.rs`:

```rust
/// The bytes typed into the shell for a launch. `\r` is Enter on a PTY.
///
/// `session_id` is typed only when it is exactly a lowercase v4-shaped uuid:
/// it lands on a command line, and the validation is the whole boundary
/// between "resh chose this id" and "something typed a shell command". A
/// malformed id degrades to the bare program, which still starts.
pub fn keystrokes(launch: Launch, session_id: Option<&str>) -> Vec<u8> {
    match (launch, session_id) {
        (Launch::Claude, Some(id)) if valid_session_id(id) => {
            format!("claude --session-id {id}\r").into_bytes()
        }
        (Launch::Claude, _) => b"claude\r".to_vec(),
    }
}

/// `8-4-4-4-12` lowercase hex, and nothing else.
pub fn valid_session_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
    })
}

/// A fresh v4 uuid from `/dev/urandom`. `None` when the kernel would not
/// give sixteen bytes — the launch then goes without an id rather than with
/// a weak one.
pub fn new_session_id() -> Option<String> {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}
```

`use std::io::Read;` is already imported at the top of the file. `term.rs:163` will not compile yet — that is Task 3's job; for this task make it compile by passing `None`: `crate::launch::keystrokes(l, None)`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test launch:: 2>&1 | grep "test result"`
Expected: `test result: ok` with the four new tests counted.

- [ ] **Step 5: Revert-check**

Temporarily change the `Some(id) if valid_session_id(id)` arm to `Some(id)` (drop the guard); run `cargo test launch::a_malformed`; it must fail on the `contains(';')` assertion. Restore. Record the result in the test's comment if it differs from the one written.

- [ ] **Step 6: Commit**

```bash
git add src/launch.rs src/term.rs
git commit -m "launch: type claude --session-id <uuid> — the id resh mints is the one thing about a session it can know without reading Claude's files"
```

---

### Task 3: Keep the launch on the session record

**Files:**
- Modify: `src/session.rs:82-102` (`Session`), `:104-120` (`Attachment`), `:128-155` (pending launch), `:220-232` and `:363` (attach), add `launched_names`
- Modify: `src/term.rs:160-166`
- Modify: `src/hub.rs:1181` and the test at `:2616-2655`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct LaunchRequest { pub launch: crate::proto::Launch, pub session_id: Option<String> }
  pub fn set_launch(project: &str, name: &str, launch: Option<LaunchRequest>)
  pub struct Attachment { …, pub launch: Option<LaunchRequest> }
  /// Names of this project's sessions (in this process's map) that were spawned with a launch.
  pub fn launched_names(project: &str) -> Vec<(String, LaunchRequest)>
  ```

- [ ] **Step 1: Write the failing test** (in `src/session.rs`'s `mod tests`, next to the existing attach tests around `:962`)

```rust
    #[test]
    fn a_launched_session_stays_known_as_launched_for_its_lifetime() {
        // Revert-checked: with `launched` never stored on the Session this
        // fails on the first assertion with an empty Vec.
        let _s = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let req = LaunchRequest { launch: crate::proto::Launch::Claude, session_id: Some("0123abcd-0123-4abc-8abc-0123456789ab".into()) };
        set_launch("launched", "term", Some(req.clone()));
        let a = attach("launched", "term", d.path()).unwrap();
        assert_eq!(a.launch, Some(req.clone()), "the spawning attach carries it");
        let _plain = attach("launched", "term1", d.path()).unwrap();
        assert_eq!(launched_names("launched"), vec![("term".to_string(), req)], "only the launched one, still after the launch was consumed");
        let again = attach("launched", "term", d.path()).unwrap();
        assert_eq!(again.launch, None, "a reattach types nothing");
        assert_eq!(launched_names("launched").len(), 1, "…and does not forget");
        kill_project("launched");
        assert!(launched_names("launched").is_empty(), "gone with the session");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test session::a_launched_session 2>&1 | grep -E "^error|panicked|test result" | head -3`
Expected: compile error — `LaunchRequest` / `launched_names` not found.

- [ ] **Step 3: Implement**

In `src/session.rs`:

```rust
/// What a ✻ click asked a terminal to start, and the session id resh chose
/// for it. Kept on the `Session` after the keystrokes are typed — it is the
/// only record that this terminal was handed `claude`, and `claudes.rs`
/// reads it to answer "is a Claude already here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub launch: crate::proto::Launch,
    pub session_id: Option<String>,
}
```

`PENDING_LAUNCH` becomes `Mutex<HashMap<String, LaunchRequest>>`; `set_launch`'s parameter becomes `launch: Option<LaunchRequest>`. `Attachment.launch: Option<LaunchRequest>`. Add to `struct Session`:

```rust
    /// Set on the spawn that consumed a parked launch; `None` for a plain
    /// shell. Survives the typing of the keystrokes on purpose.
    launched: Option<LaunchRequest>,
```

In `attach`, where the `Session` is inserted into the map, set `launched: launch.clone()`. Add:

```rust
/// Sessions of `project` this process spawned with a launch. A map scan
/// only, like `live_names`: it runs under the hub on every ✻ click.
pub fn launched_names(project: &str) -> Vec<(String, LaunchRequest)> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<(String, LaunchRequest)> = map
        .iter()
        .filter_map(|(k, s)| Some((k.strip_prefix(&prefix)?.to_string(), s.launched.clone()?)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
```

`src/term.rs:162-166`:

```rust
    if let Some(l) = att.launch {
        let typed = crate::launch::keystrokes(l.launch, l.session_id.as_deref());
        if let Err(e) = session::write_input(&att.key, &typed) {
            eprintln!("resh: could not start {:?} in {project}/{name}: {e}", l.launch);
        }
    }
```

`src/hub.rs:1181`:

```rust
        crate::session::set_launch(
            &self.project,
            &name,
            launch.map(|l| crate::session::LaunchRequest { launch: l, session_id: crate::launch::new_session_id() }),
        );
```

In the hub test `a_claude_terminal_parks_its_launch_on_the_name_it_was_given`, change `assert_eq!(first.launch, Some(proto::Launch::Claude), …)` to `assert_eq!(first.launch.as_ref().map(|l| l.launch), Some(proto::Launch::Claude), …)` and add after it: `assert!(first.launch.as_ref().and_then(|l| l.session_id.as_deref()).is_some_and(crate::launch::valid_session_id), "the hub minted an id");`.

- [ ] **Step 4: Run**

Run: `cargo test session:: hub::a_claude_terminal 2>&1 | grep "test result"`
Expected: ok.

- [ ] **Step 5: Revert-check** — comment out `launched: launch.clone()` (set `None`), run the new test, confirm the first `launched_names` assertion fails, restore.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs src/term.rs src/hub.rs
git commit -m "session: keep the launch on the record — that a terminal was handed claude is the evidence the ✻ prompt needs"
```

---

### Task 4: `ClaudeEvidence`

**Files:**
- Create: `src/claudes.rs`
- Modify: `src/lib.rs` (add `pub mod claudes;`), `src/ide.rs` (add `connected_sessions`), `src/idesess.rs:23` (derive `Clone` on `Sess` if absent)

**Interfaces:**
- Produces:
  ```rust
  pub enum ClaudeEvidence { Present(Vec<String>), Absent, Unknown }
  pub fn evidence_from(launched: &[String], connected: &[crate::idesess::Sess], ide_on: bool) -> ClaudeEvidence
  pub fn claude_evidence(project: &str) -> ClaudeEvidence
  // ide.rs
  pub fn connected_sessions(project: &str) -> Vec<crate::idesess::Sess>
  ```

- [ ] **Step 1: Write the failing tests** (`src/claudes.rs`, bottom)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::idesess::Sess;

    #[test]
    fn an_ide_connection_alone_is_present_and_names_its_terminal() {
        assert_eq!(evidence_from(&[], &[Sess::In("term2".into())], true), ClaudeEvidence::Present(vec!["term2".into()]));
    }

    #[test]
    fn a_launched_terminal_alone_is_present_even_with_ide_off() {
        // Revert-checked: returning `Unknown` whenever `!ide_on` fails here.
        assert_eq!(evidence_from(&["term".into()], &[], false), ClaudeEvidence::Present(vec!["term".into()]));
    }

    #[test]
    fn a_connection_resh_cannot_place_is_still_present_but_unnamed() {
        assert_eq!(evidence_from(&[], &[Sess::Unknown], true), ClaudeEvidence::Present(vec![]));
    }

    #[test]
    fn nothing_with_ide_on_is_absent() {
        // Asserted on the variant: `!= Present` would also pass for Unknown.
        assert_eq!(evidence_from(&[], &[], true), ClaudeEvidence::Absent);
    }

    #[test]
    fn nothing_with_ide_off_is_unknown() {
        // Revert-checked: dropping the `ide_on` branch yields Absent here.
        assert_eq!(evidence_from(&[], &[], false), ClaudeEvidence::Unknown);
    }

    #[test]
    fn a_terminal_seen_both_ways_is_named_once() {
        assert_eq!(
            evidence_from(&["term".into()], &[Sess::In("term".into()), Sess::Outside], true),
            ClaudeEvidence::Present(vec!["term".into()])
        );
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test claudes:: 2>&1 | grep -E "^error" | head -2`
Expected: module not found.

- [ ] **Step 3: Implement**

`src/claudes.rs`:

```rust
//! What resh can say about Claudes running in a project, from what resh
//! itself observed — never from Claude's own session files.
//!
//! Two signals: a terminal resh typed `claude` into (`session::launched_names`)
//! and a connection on the project's IDE socket (`ide::connected_sessions`).
//! Three answers, not two: with the IDE integration switched off, a `claude`
//! typed by hand into a plain terminal is invisible, so "found nothing" is
//! not "nothing there". Only `Present` may change what a button does.

use crate::idesess::Sess;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeEvidence {
    /// Terminal names resh could attribute, sorted, deduplicated. May be
    /// empty when the only evidence is a connection it could not place.
    Present(Vec<String>),
    Absent,
    Unknown,
}

pub fn evidence_from(launched: &[String], connected: &[Sess], ide_on: bool) -> ClaudeEvidence {
    let mut names: Vec<String> = launched.to_vec();
    let mut any = !launched.is_empty();
    for s in connected {
        match s {
            Sess::In(n) => { names.push(n.clone()); any = true; }
            Sess::Unknown => any = true,
            // Positively in another project's terminal: not evidence here.
            Sess::Outside => {}
        }
    }
    if any {
        names.sort();
        names.dedup();
        return ClaudeEvidence::Present(names);
    }
    if ide_on { ClaudeEvidence::Absent } else { ClaudeEvidence::Unknown }
}

pub fn claude_evidence(project: &str) -> ClaudeEvidence {
    let launched: Vec<String> =
        crate::session::launched_names(project).into_iter().map(|(n, _)| n).collect();
    evidence_from(&launched, &crate::ide::connected_sessions(project), crate::config::ide_enabled())
}
```

`src/ide.rs`, next to `set_session` (`:402`):

```rust
/// Which terminal each of this project's connected Claudes is in — cloned
/// out under the lock, so the caller (the hub, on a ✻ click) holds nothing
/// of ours while it decides.
pub fn connected_sessions(project: &str) -> Vec<crate::idesess::Sess> {
    let map = conns().lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).map(|v| v.iter().map(|t| t.session.clone()).collect()).unwrap_or_default()
}
```

`Sess` needs `Clone` — add `#[derive(Debug, Clone, PartialEq, Eq)]` to it in `src/idesess.rs` if it lacks any of those.

- [ ] **Step 4: Run** — `cargo test claudes:: ide:: 2>&1 | grep "test result"` → ok.

- [ ] **Step 5: Commit**

```bash
git add src/claudes.rs src/lib.rs src/ide.rs src/idesess.rs
git commit -m "claudes: three-valued evidence of a Claude in a project, from the launch record and the IDE socket"
```

---

### Task 5: The ✻ prompt and the `worktree_prompt` key

**Files:**
- Modify: `src/proto.rs:51-55` (Launch derives), `:86-94` (`NewTerminal`), `Event` enum after `:217`
- Modify: `src/config.rs:11-20` (RawConfig), after `:221` (new fn), tests
- Modify: `src/hub.rs:434`, `:1150-1160`, every `Intent::NewTerminal {` literal in tests
- Modify: `docs/deploy.md` config table

**Interfaces:**
- Produces: `Intent::NewTerminal { pane, launch, force: bool }`; `Event::ClaudeHere { pane: PaneId, terminals: Vec<String> }`; `config::worktree_prompt() -> bool`.
- Consumes: `claudes::claude_evidence`.

- [ ] **Step 1: Write the failing tests**

`src/config.rs` tests:

```rust
    #[test]
    fn worktree_prompt_is_on_unless_the_global_config_says_off() {
        // Revert-checked: `unwrap_or(false)` fails the first assertion.
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("config.toml");
        assert!(worktree_prompt_from(&g), "absent file: on");
        std::fs::write(&g, "worktree_prompt = false\n").unwrap();
        assert!(!worktree_prompt_from(&g));
        std::fs::write(&g, "this is not toml\n").unwrap();
        assert!(worktree_prompt_from(&g), "unparseable: on, a typo must not change a button");
    }
```

`src/hub.rs` tests (copy the fixture from `a_claude_terminal_parks_its_launch_on_the_name_it_was_given`; two subscribers so `send_to` and `broadcast` are distinguishable):

```rust
    #[test]
    fn a_second_claude_click_gets_a_prompt_that_only_the_clicker_sees() {
        // Revert-checked: with the evidence check removed, `a` receives
        // State + TerminalStarted and the ClaudeHere assertion fails;
        // with `send_to` swapped for `broadcast`, the `b` assertion fails.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("prompt_second", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        let (b, rxb) = h.subscribe();
        for p in h.ws.panes.iter_mut() { p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. })); p.active = 0; }
        // First ✻: allocates `term`; spawn it the way a browser would, so the
        // launch is consumed and recorded on the session.
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false });
        let _att = crate::session::attach("prompt_second", "term", d.path()).unwrap();
        assert_eq!(crate::session::launched_names("prompt_second").len(), 1, "fixture: a launched terminal exists");
        drain(&rxa); drain(&rxb);
        let version = h.ws.version;

        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false });
        let got = rxa.try_recv().expect("the clicker hears back");
        assert!(got.contains(r#""t":"ClaudeHere""#) && got.contains(r#""terminals":["term"]"#), "{got}");
        assert!(rxb.try_recv().is_err(), "nobody else hears anything");
        assert_eq!(h.ws.version, version, "no layout change");
        assert_eq!(crate::session::live_names("prompt_second").len(), 1, "no session allocated");

        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: true });
        assert!(h.ws.version > version, "force opens a terminal");
        assert!(rxb.try_recv().is_ok_and(|m| m.contains(r#""t":"State""#)), "…which everyone sees");
        crate::session::kill_project("prompt_second");
    }

    #[test]
    fn a_plus_click_never_prompts() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("prompt_plus", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        for p in h.ws.panes.iter_mut() { p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. })); p.active = 0; }
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: Some(proto::Launch::Claude), force: false });
        let _att = crate::session::attach("prompt_plus", "term", d.path()).unwrap();
        drain(&rxa);
        let version = h.ws.version;
        h.handle(&a, Intent::NewTerminal { pane: proto::RIGHT, launch: None, force: false });
        assert!(h.ws.version > version, "a plain shell opens beside a Claude");
        crate::session::kill_project("prompt_plus");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test config::worktree_prompt hub::a_second_claude hub::a_plus_click 2>&1 | grep -E "^error" | head -3`
Expected: `force` is not a field; `worktree_prompt_from` not found.

- [ ] **Step 3: Implement**

`src/proto.rs`: give `Launch` `Serialize` too (`#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]`, `#[serde(rename_all = "lowercase")]`). In `NewTerminal` add:

```rust
        /// Skip the "a Claude is already here" prompt. Sent by the prompt's
        /// own "start here anyway" button. Not a boundary — typing `claude`
        /// was always one keystroke away.
        #[serde(default)]
        force: bool,
```

In `Event`, after `TerminalStarted`:

```rust
    /// A ✻ click in a project resh has positive evidence a Claude is
    /// already running in. Sent to the clicker only; nothing was opened.
    ClaudeHere { pane: PaneId, terminals: Vec<String> },
```

`src/config.rs`: add `worktree_prompt: Option<bool>,` to `RawConfig`; add after `ide_enabled_from`:

```rust
/// Whether ✻ offers a worktree when a Claude is already in the project.
/// Global only: it changes what a button does everywhere, and a checkout
/// must not get to decide that. Absent, unreadable or unparseable mean on.
pub fn worktree_prompt() -> bool {
    worktree_prompt_from(&global_config_path())
}

fn worktree_prompt_from(global: &Path) -> bool {
    std::fs::read_to_string(global)
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.worktree_prompt)
        .unwrap_or(true)
}
```

`src/hub.rs`: dispatch arm `Intent::NewTerminal { pane, launch, force } => return self.do_new_terminal(from, *pane, *launch, *force),`. `do_new_terminal` gains `force: bool` and, right after the `closing` check:

```rust
        if launch == Some(crate::proto::Launch::Claude) && !force && crate::config::worktree_prompt() {
            if let crate::claudes::ClaudeEvidence::Present(terminals) =
                crate::claudes::claude_evidence(&self.project)
            {
                // The clicker only: nothing changed for anyone else, and a
                // prompt is a question, not a state.
                let ev = Event::ClaudeHere { pane, terminals };
                return self.send_to(from, &ev);
            }
        }
```

Update every existing `Intent::NewTerminal { pane: …, launch: … }` literal in `src/hub.rs` tests to add `force: false` (`grep -n "Intent::NewTerminal {" src/hub.rs`). The `proto.rs:265` decode test stays valid — `force` defaults.

`docs/deploy.md` config table: add a row `| \`worktree_prompt\` | ✻ offers a new worktree when a Claude is already in the project | \`true\` | global only |` matching the table's existing columns.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep "test result"` → all ok.

- [ ] **Step 5: Revert-checks** — (a) delete the evidence block, run `hub::a_second_claude`, confirm the `ClaudeHere` assertion fails; (b) change `send_to` to `broadcast` in that block, confirm the `rxb` assertion fails. Restore both.

- [ ] **Step 6: Commit**

```bash
git add src/proto.rs src/config.rs src/hub.rs docs/deploy.md
git commit -m "hub: a second ✻ in a project with a Claude asks instead of opening — the clicker only, force to skip"
```

---

### Task 6: `worktree::create`

**Files:**
- Modify: `src/worktree.rs` (new fns after `list`; tests at the bottom)

**Interfaces:**
- Produces:
  ```rust
  pub type GitRunner<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, String>;
  pub fn real_git(repo: &Path, args: &[&str]) -> Result<String, String>   // gitio::run_git(repo, args, false)
  pub struct Created { pub name: String, pub path: PathBuf, pub base: String }
  pub fn create(repo: &Path, state_dir: &Path, wt_key_of: &dyn Fn(&str) -> String, run: GitRunner) -> Result<Created, String>
  pub fn base_file(state_dir: &Path, wt_key: &str) -> PathBuf          // {state}/worktrees/{wt_key}.base
  pub fn read_base(state_dir: &Path, wt_key: &str) -> Option<String>
  pub fn write_base(state_dir: &Path, wt_key: &str, base: &str) -> Result<(), String>
  pub const MAX_WORKTREES: u32 = 64;
  ```
  `wt_key_of(name)` maps `claude-N` to the storage key of its project URL; the hub supplies `|n| storage_key(&format!("{project}/.claude/worktrees/{n}"))`.

- [ ] **Step 1: Write the failing tests**

```rust
    fn repo_with_commit(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        repo
    }
    fn key_of(n: &str) -> String { format!("repo%2F.claude%2Fworktrees%2F{n}") }

    #[test]
    fn create_mints_the_next_free_name_and_records_the_base() {
        // Revert-checked: writing `.base` after `worktree add` instead of
        // before passes this test but fails `base_is_written_before_git_runs`.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let state = root.path().join("state");
        let c1 = create(&repo, &state, &key_of, &real_git).unwrap();
        assert_eq!(c1.name, "claude-1");
        assert_eq!(c1.path, repo.join(".claude/worktrees/claude-1"));
        assert_eq!(c1.base, "main");
        assert!(c1.path.join("a.txt").is_file(), "checked out");
        assert_eq!(read_base(&state, &key_of("claude-1")).as_deref(), Some("main"));
        assert!(list(&repo).iter().any(|w| w.branch == "claude-1" && !w.is_main));
        let c2 = create(&repo, &state, &key_of, &real_git).unwrap();
        assert_eq!(c2.name, "claude-2");
    }

    #[test]
    fn a_branch_without_a_directory_still_takes_its_number() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        real_git(&repo, &["branch", "claude-1"]).unwrap();
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(c.name, "claude-2");
    }

    #[test]
    fn a_directory_without_a_branch_still_takes_its_number() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        std::fs::create_dir_all(repo.join(".claude/worktrees/claude-1")).unwrap();
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(c.name, "claude-2");
    }

    #[test]
    fn a_failed_branch_check_refuses_rather_than_skipping() {
        // "Could not tell whether claude-1 exists" must not become claude-2.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let flaky = |r: &Path, args: &[&str]| -> Result<String, String> {
            if args.first() == Some(&"branch") { Err("fatal: index locked".into()) } else { real_git(r, args) }
        };
        let err = create(&repo, &root.path().join("state"), &key_of, &flaky).unwrap_err();
        assert!(err.contains("cannot tell") && err.contains("claude-1"), "{err}");
        assert!(list(&repo).len() == 1, "nothing was created");
    }

    #[test]
    fn base_is_written_before_git_runs_and_removed_when_git_fails() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let state = root.path().join("state");
        let seen = std::cell::Cell::new(false);
        let failing = |r: &Path, args: &[&str]| -> Result<String, String> {
            if args.first() == Some(&"worktree") {
                seen.set(read_base(&state, &key_of("claude-1")).is_some());
                Err("fatal: disk full".into())
            } else { real_git(r, args) }
        };
        let err = create(&repo, &state, &key_of, &failing).unwrap_err();
        assert!(err.contains("disk full"), "{err}");
        assert!(seen.get(), ".base existed when git ran");
        assert!(read_base(&state, &key_of("claude-1")).is_none(), "…and is gone after git failed");
    }

    #[test]
    fn a_linked_worktree_cannot_create_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        let err = create(&c.path, &root.path().join("state"), &key_of, &real_git).unwrap_err();
        assert!(err.contains("main checkout"), "{err}");
    }

    #[test]
    fn a_non_repository_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let err = create(root.path(), &root.path().join("state"), &key_of, &real_git).unwrap_err();
        assert!(err.contains("not a git repository"), "{err}");
    }

    #[test]
    fn write_base_is_atomic_and_read_base_ignores_a_torn_file() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        write_base(&state, "k", "main").unwrap();
        assert_eq!(read_base(&state, "k").as_deref(), Some("main"));
        assert!(std::fs::read_dir(state.join("worktrees")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".tmp")), "no temp file left");
        std::fs::write(base_file(&state, "torn"), "").unwrap();
        assert_eq!(read_base(&state, "torn"), None, "empty is not a base");
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test worktree:: 2>&1 | grep -E "^error" | head -2` → `create` not found.

- [ ] **Step 3: Implement** (after `list` in `src/worktree.rs`)

```rust
pub type GitRunner<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, String>;

/// The production runner: the 15 s-deadline `gitio::run_git`, exit 0 only.
pub fn real_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    crate::gitio::run_git(repo, args, false)
}

pub const MAX_WORKTREES: u32 = 64;

pub struct Created {
    pub name: String,
    pub path: PathBuf,
    /// The branch (or commit, when detached) the worktree was cut from.
    pub base: String,
}

pub fn base_file(state_dir: &Path, wt_key: &str) -> PathBuf {
    state_dir.join("worktrees").join(format!("{wt_key}.base"))
}

/// `None` for absent, unreadable, or empty: an empty base is not a base,
/// and "ahead unknown" is the direction that failure must fall.
pub fn read_base(state_dir: &Path, wt_key: &str) -> Option<String> {
    let s = std::fs::read_to_string(base_file(state_dir, wt_key)).ok()?;
    let s = s.trim_end_matches('\n');
    if s.is_empty() { None } else { Some(s.to_string()) }
}

/// Temp file with a pid-unique name, then rename: a reader never sees half.
pub fn write_base(state_dir: &Path, wt_key: &str, base: &str) -> Result<(), String> {
    let path = base_file(state_dir, wt_key);
    let dir = path.parent().ok_or("no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{wt_key}.base.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{base}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })
}

/// Mint `claude-N`, record its base, `git worktree add`. Every failure
/// returns before anything later runs; a failed check is a refusal, never a
/// skip to N+1 — "I could not tell whether claude-1 exists" is not "it does".
pub fn create(
    repo: &Path,
    state_dir: &Path,
    wt_key_of: &dyn Fn(&str) -> String,
    run: GitRunner,
) -> Result<Created, String> {
    if !crate::gitio::is_inside_work_tree(repo) {
        return Err("not a git repository".into());
    }
    let canon = repo.canonicalize().map_err(|e| format!("cannot resolve project directory: {e}"))?;
    let ws = list(repo);
    if ws.is_empty() {
        return Err("git did not answer (worktree list)".into());
    }
    let me = ws.iter().find(|w| w.path.canonicalize().ok().as_deref() == Some(canon.as_path()));
    match me {
        Some(w) if w.is_main => {}
        _ => return Err("start worktrees from the main checkout".into()),
    }
    let mut name = None;
    for n in 1..=MAX_WORKTREES {
        let cand = format!("claude-{n}");
        let out = run(repo, &["branch", "--list", &cand])
            .map_err(|e| format!("cannot tell whether branch {cand} exists: {e}"))?;
        if !out.trim().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(repo.join(".claude/worktrees").join(&cand)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { name = Some(cand); break; }
            Ok(_) => continue,
            Err(e) => return Err(format!("cannot tell whether .claude/worktrees/{cand} exists: {e}")),
        }
    }
    let name = name.ok_or_else(|| format!("too many worktrees ({MAX_WORKTREES})"))?;
    std::fs::create_dir_all(repo.join(".claude/worktrees")).map_err(|e| e.to_string())?;
    let path = crate::projects::safe_resolve_parent(repo, &format!(".claude/worktrees/{name}"))?;
    let base = match run(repo, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => run(repo, &["rev-parse", "HEAD"]).map_err(|e| format!("cannot read HEAD: {e}"))?.trim().to_string(),
    };
    let key = wt_key_of(&name);
    write_base(state_dir, &key, &base)?;
    let rel = format!(".claude/worktrees/{name}");
    if let Err(e) = run(repo, &["worktree", "add", "-b", &name, &rel, "HEAD"]) {
        let _ = std::fs::remove_file(base_file(state_dir, &key));
        return Err(format!("git worktree add failed: {e}"));
    }
    Ok(Created { name, path, base })
}
```

`create`'s `.claude/worktrees` pre-creation must not be counted as "the directory exists" by the mint loop — it creates the *parent*, and the loop checks the child; keep that order.

- [ ] **Step 4: Run** — `cargo test worktree:: 2>&1 | grep "test result"` → ok, 8 new tests counted.

- [ ] **Step 5: Revert-check** — swap the order of `write_base` and the `worktree add` call; `base_is_written_before_git_runs…` must fail on `seen`; restore.

- [ ] **Step 6: Commit**

```bash
git add src/worktree.rs
git commit -m "worktree: create claude-N — mint on positive evidence only, record the base before git runs"
```

---

### Task 7: `Intent::NewWorktree` in the hub

**Files:**
- Modify: `src/proto.rs` (`Intent::NewWorktree`, `Event::WorktreeReady`), `src/hub.rs` (dispatch + `do_new_worktree`), `src/hub.rs` tests

**Interfaces:**
- Produces: `Intent::NewWorktree { launch: Option<Launch> }`; `Event::WorktreeReady { url: String, launch: Option<Launch> }`; `Hub::do_new_worktree(&mut self, from: &ConnId, launch: Option<Launch>)`.
- Consumes: `worktree::create`, `worktree::real_git`, `registry::known_projects`, `hub::broadcast_all`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn new_worktree_creates_claude_1_announces_it_and_answers_the_clicker() {
        // Revert-checked: with `WorktreeReady` sent via `broadcast`, the
        // `rxb` assertion fails; with the `ProjectsChanged` broadcast
        // removed, the `projects` assertion fails.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let mut h = Hub::new("repo", repo.clone());
        let (a, rxa) = h.subscribe();
        let (b, rxb) = h.subscribe();
        drain(&rxa); drain(&rxb);
        h.handle(&a, Intent::NewWorktree { launch: Some(proto::Launch::Claude) });
        // Hub::new has no self_ref, so the work ran synchronously (the
        // do_end_session convention) and the reply is already queued.
        let got = rxa.try_recv().expect("clicker answered");
        assert!(got.contains(r#""t":"WorktreeReady""#) && got.contains(r#""url":"repo/.claude/worktrees/claude-1""#) && got.contains(r#""launch":"claude""#), "{got}");
        assert!(rxb.try_recv().is_err(), "the reply is the clicker's alone");
        assert!(repo.join(".claude/worktrees/claude-1/a.txt").is_file());
        assert_eq!(crate::worktree::read_base(&crate::wsstate::state_dir(), "repo%2F.claude%2Fworktrees%2Fclaude-1").as_deref(), Some("main"));
    }

    #[test]
    fn new_worktree_in_a_non_repository_is_an_error_to_the_clicker() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("plain", d.path().to_path_buf());
        let (a, rxa) = h.subscribe();
        drain(&rxa);
        h.handle(&a, Intent::NewWorktree { launch: None });
        let got = rxa.try_recv().unwrap();
        assert!(got.contains(r#""t":"Error""#) && got.contains("not a git repository"), "{got}");
    }
```

`ProjectsChanged` goes through `broadcast_all`, which reaches hubs registered in the global map, not a bare `Hub::new` — assert it the way `src/hub.rs:2873` does if a global-registered hub fixture exists there; otherwise leave that assertion to the browser test and say so in the test comment.

- [ ] **Step 2: Run to verify it fails** — `cargo test hub::new_worktree 2>&1 | grep -E "^error" | head -2` → variant not found.

- [ ] **Step 3: Implement**

`src/proto.rs` `Intent`:

```rust
    /// Create `.claude/worktrees/claude-N` off this project's HEAD. No name
    /// from the browser: the server mints it, so nothing typed reaches a
    /// path or a command line. `launch` is echoed back in `WorktreeReady`
    /// so the new tab knows what to start.
    NewWorktree {
        #[serde(default)]
        launch: Option<Launch>,
    },
```

`Event`:

```rust
    /// The worktree exists and is registered. Sent to the clicker only —
    /// it is the one holding the tab to navigate.
    WorktreeReady { url: String, launch: Option<Launch> },
```

`src/hub.rs` dispatch: `Intent::NewWorktree { launch } => return self.do_new_worktree(from, *launch),`. Then:

```rust
    /// `git worktree add` off the hub lock, the `do_end_session` shape: the
    /// work runs on a thread when this hub has a self-handle, synchronously
    /// (unit tests, bare `Hub::new`) when it does not, and in both cases
    /// the hub is re-locked only to deliver the answer.
    fn do_new_worktree(&mut self, from: &ConnId, launch: Option<crate::proto::Launch>) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        let project = self.project.clone();
        let dir = self.dir.clone();
        let from = from.clone();
        let work = move || -> Result<String, String> {
            let key_of = |n: &str| crate::projects::storage_key(&format!("{project}/.claude/worktrees/{n}"));
            let c = crate::worktree::create(&dir, &crate::wsstate::state_dir(), &key_of, &crate::worktree::real_git)?;
            Ok(format!("{project}/.claude/worktrees/{}", c.name))
        };
        let finish = move |h: &mut Hub, r: Result<String, String>| match r {
            Ok(url) => {
                broadcast_all(&Event::ProjectsChanged { project: url.clone() });
                let ev = Event::WorktreeReady { url, launch };
                h.send_to(&from, &ev);
            }
            Err(msg) => {
                let ev = Event::Error { msg };
                h.send_to(&from, &ev);
            }
        };
        match self.self_ref.upgrade() {
            Some(arc) => {
                let spawned = std::thread::Builder::new().name("new-worktree".into()).spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        .unwrap_or_else(|_| Err("worktree creation panicked".into()));
                    let mut h = Hub::lock(&arc);
                    finish(&mut h, r);
                });
                if spawned.is_err() {
                    let ev = Event::Error { msg: "could not start worktree creation".into() };
                    self.send_to(&from, &ev);
                }
            }
            None => {
                let r = work();
                finish(self, r);
            }
        }
    }
```

If `Hub::lock` is not the name of the existing helper used at `src/hub.rs:1120`, use whatever `do_end_session` uses.

- [ ] **Step 4: Run** — `cargo test hub:: 2>&1 | grep "test result"` → ok.

- [ ] **Step 5: Revert-check** — change `h.send_to(&from, &ev)` for `WorktreeReady` to `h.broadcast(&ev)`; the `rxb` assertion must fail; restore.

- [ ] **Step 6: Commit**

```bash
git add src/proto.rs src/hub.rs
git commit -m "hub: NewWorktree — create off the lock, announce to every page, answer the clicker with the url to open"
```

---

### Task 8: Worktree state in the switcher

**Files:**
- Modify: `src/worktree.rs` (`State`, `state`), `src/registry.rs:10-38` (`ProjectStatus.wt`), every `ProjectStatus {` literal (`grep -n "ProjectStatus {" src/*.rs`), new `known_projects_with_state`, `src/routes.rs:80-84`, `src/render.rs:812-885`, `static/style.css:86-94`

**Interfaces:**
- Produces:
  ```rust
  // worktree.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct State { pub dirty: Option<bool>, pub ahead: Option<u32> }
  pub fn state(path: &Path, base: &str, run: GitRunner) -> State
  // registry.rs
  #[derive(Debug, Clone)]
  pub struct WorktreeStatus { pub claude: crate::claudes::ClaudeEvidence, pub dirty: Option<bool>, pub ahead: Option<u32>, pub base: String, pub base_recorded: bool }
  pub struct ProjectStatus { …, pub wt: Option<WorktreeStatus> }
  pub fn known_projects_with_state(roots: &[PathBuf]) -> Vec<ProjectStatus>
  pub fn removable(w: &WorktreeStatus, live: usize) -> bool
  ```

- [ ] **Step 1: Write the failing tests**

`src/worktree.rs`:

```rust
    #[test]
    fn state_reads_dirty_and_ahead_against_the_recorded_base() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(state(&c.path, "main", &real_git), State { dirty: Some(false), ahead: Some(0) });
        std::fs::write(c.path.join("new.txt"), "y").unwrap();
        assert_eq!(state(&c.path, "main", &real_git).dirty, Some(true));
        real_git(&c.path, &["add", "."]).unwrap();
        real_git(&c.path, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "wt"]).unwrap();
        assert_eq!(state(&c.path, "main", &real_git), State { dirty: Some(false), ahead: Some(1) });
        // Merged into the base: ahead drops to 0 even though main moved.
        real_git(&repo, &["merge", "-q", "--no-edit", "claude-1"]).unwrap();
        assert_eq!(state(&c.path, "main", &real_git).ahead, Some(0));
    }

    #[test]
    fn state_reports_unknown_not_clean_when_git_does_not_answer() {
        // Revert-checked: `unwrap_or(false)` / `unwrap_or(0)` fail here.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let dead = |_: &Path, _: &[&str]| -> Result<String, String> { Err("timeout".into()) };
        assert_eq!(state(&repo, "main", &dead), State { dirty: None, ahead: None });
        assert_eq!(state(&repo, "no-such-base", &real_git).ahead, None, "a bad base is unknown, not zero");
    }
```

`src/render.rs` tests:

```rust
    #[test]
    fn a_worktree_row_shows_its_state_and_offers_removal_only_when_clean() {
        // Revert-checked: rendering the control whenever `ahead == Some(0)`
        // alone fails the dirty case; `?` for None fails if None renders as `—`.
        use crate::registry::{ProjectStatus, WorktreeStatus};
        use crate::claudes::ClaudeEvidence;
        let mk = |wt: WorktreeStatus, live: usize| vec![
            ProjectStatus { key: "r".into(), url: "r".into(), live: 1, oldest_age_secs: None, has_layout: true, branch: "main".into(), parent: None, reachable: true, wt: None },
            ProjectStatus { key: "r%2F.claude%2Fworktrees%2Fclaude-1".into(), url: "r/.claude/worktrees/claude-1".into(), live, oldest_age_secs: None, has_layout: false, branch: "claude-1".into(), parent: Some("r".into()), reachable: true, wt: Some(wt) },
        ];
        let clean = WorktreeStatus { claude: ClaudeEvidence::Absent, dirty: Some(false), ahead: Some(0), base: "main".into(), base_recorded: true };
        let out = worktrees_strip("r", &mk(clean.clone(), 0));
        assert!(out.contains("0 ahead") && out.contains("class=\"wtremove\"") && out.contains("data-key=\"r%2F.claude%2Fworktrees%2Fclaude-1\""), "{out}");
        let dirty = WorktreeStatus { dirty: Some(true), ..clean.clone() };
        let out = worktrees_strip("r", &mk(dirty, 0));
        assert!(out.contains("dirty") && !out.contains("wtremove"), "{out}");
        let unknown = WorktreeStatus { dirty: None, ..clean.clone() };
        let out = worktrees_strip("r", &mk(unknown, 0));
        assert!(out.contains("title=\"git did not answer") && !out.contains("wtremove"), "{out}");
        let present = WorktreeStatus { claude: ClaudeEvidence::Present(vec!["term".into()]), ..clean.clone() };
        let out = worktrees_strip("r", &mk(present, 0));
        assert!(out.contains("✻") && !out.contains("wtremove"), "{out}");
        let out = worktrees_strip("r", &mk(clean.clone(), 1));
        assert!(!out.contains("wtremove"), "a live terminal blocks removal: {out}");
        let unrecorded = WorktreeStatus { base_recorded: false, ..clean };
        let out = worktrees_strip("r", &mk(unrecorded, 0));
        assert!(out.contains("measured against main, the main worktree's branch"), "{out}");
    }
```

`src/routes.rs` tests, next to `the_worktrees_fragment_is_routed`:

```rust
    #[test]
    fn the_worktrees_fragment_computes_state_only_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        assert!(frag_route(&roots, "/frag/_worktrees?current=nosuch&state=1").contains("id=\"wtlabel\""));
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test worktree::state render::a_worktree_row routes::the_worktrees_fragment_computes 2>&1 | grep -E "^error" | head -3`.

- [ ] **Step 3: Implement**

`src/worktree.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub dirty: Option<bool>,
    pub ahead: Option<u32>,
}

/// Two git calls, each three-valued. `None` is "git did not answer" and is
/// never rendered or acted on as clean.
pub fn state(path: &Path, base: &str, run: GitRunner) -> State {
    let dirty = run(path, &["status", "--porcelain"]).ok().map(|o| !o.trim().is_empty());
    let ahead = run(path, &["rev-list", "--count", &format!("{base}..HEAD")])
        .ok()
        .and_then(|o| o.trim().parse::<u32>().ok());
    State { dirty, ahead }
}
```

`src/registry.rs`: add `WorktreeStatus` and `pub wt: Option<WorktreeStatus>` to `ProjectStatus` (doc: *"Populated only by `known_projects_with_state`, only for reachable linked worktrees."*); add `wt: None` to every literal. This groups the spec's four fields (`claude`, `dirty`, `ahead`, `base`) into one optional struct so a main-worktree entry carries no half-filled state — same content, one `Option` instead of four. Add:

```rust
/// Can this worktree be offered for removal? Every axis positively clean;
/// a single unknown says no. The hub re-derives this at the moment of the
/// intent — the row is a hint, not an authorisation.
pub fn removable(w: &WorktreeStatus, live: usize) -> bool {
    live == 0
        && w.claude == crate::claudes::ClaudeEvidence::Absent
        && w.dirty == Some(false)
        && w.ahead == Some(0)
}

/// `known_projects` plus per-worktree state. Costs two git calls per linked
/// worktree, so it is requested only when the switcher panel opens.
pub fn known_projects_with_state(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    let mut ps = known_projects(roots);
    let state_dir = crate::wsstate::state_dir();
    let main_branch: std::collections::HashMap<String, String> =
        ps.iter().filter(|p| p.parent.is_none()).map(|p| (p.key.clone(), p.branch.clone())).collect();
    // A worktree's directory comes from git's own listing of its parent, not
    // from `resolve_project`: that refuses dot segments (`.claude/…`), and
    // `is_vouched_worktree` is an exception to *naming* only.
    let mut dirs: std::collections::HashMap<String, PathBuf> = Default::default();
    for p in ps.iter().filter(|p| p.parent.is_none()) {
        if let Some(pd) = crate::projects::resolve_project(roots, &p.url) {
            for w in crate::worktree::list(&pd).into_iter().filter(|w| !w.is_main) {
                if let Ok(c) = w.path.canonicalize() {
                    dirs.insert(w.branch.clone(), c);
                }
            }
        }
    }
    for p in ps.iter_mut() {
        let Some(parent) = p.parent.as_deref() else { continue };
        if !p.reachable { continue }
        let Some(dir) = dirs.get(&p.branch).cloned() else { continue };
        let (base, base_recorded) = match crate::worktree::read_base(&state_dir, &p.key) {
            Some(b) => (b, true),
            None => (main_branch.get(parent).cloned().unwrap_or_default(), false),
        };
        let st = if base.is_empty() {
            crate::worktree::State { dirty: None, ahead: None }
        } else {
            crate::worktree::state(&dir, &base, &crate::worktree::real_git)
        };
        p.wt = Some(WorktreeStatus {
            claude: crate::claudes::claude_evidence(&p.url),
            dirty: st.dirty,
            ahead: st.ahead,
            base,
            base_recorded,
        });
    }
    ps
}
```

`src/routes.rs:80-84`:

```rust
        ["frag", "_worktrees"] => {
            let current = req.query.get("current").map(String::as_str).unwrap_or("");
            let ps = if req.query.get("state").map(String::as_str) == Some("1") {
                registry::known_projects_with_state(roots)
            } else {
                registry::known_projects(roots)
            };
            http::html(w, &render::worktrees_strip(current, &ps));
        }
```

`src/render.rs` `worktrees_strip`: after computing `branch` for a row, build `state_html`:

```rust
        let state_html = match &p.wt {
            None => String::new(),
            Some(w) => {
                let claude = match &w.claude {
                    crate::claudes::ClaudeEvidence::Present(_) => "<span class=\"wtf on\" title=\"a Claude is running here\">✻</span>".to_string(),
                    crate::claudes::ClaudeEvidence::Absent => "<span class=\"wtf\" title=\"no Claude here\">—</span>".to_string(),
                    crate::claudes::ClaudeEvidence::Unknown => "<span class=\"wtf\" title=\"IDE integration is off, so resh cannot tell\">?</span>".to_string(),
                };
                let dirty = match w.dirty {
                    Some(true) => "<span class=\"wtf on\">dirty</span>".to_string(),
                    Some(false) => "<span class=\"wtf\">clean</span>".to_string(),
                    None => "<span class=\"wtf\" title=\"git did not answer (status)\">?</span>".to_string(),
                };
                let against = if w.base_recorded {
                    format!("measured against {}, recorded when resh created this worktree", esc(&w.base))
                } else {
                    format!("measured against {}, the main worktree's branch — resh did not create this worktree", esc(&w.base))
                };
                let ahead = match w.ahead {
                    Some(n) => format!("<span class=\"wtf{}\" title=\"{against}. A squash-merged branch stays ahead forever; remove it by hand.\">{n} ahead</span>", if n > 0 { " on" } else { "" }),
                    None => "<span class=\"wtf\" title=\"git did not answer (rev-list), or no base is known\">?</span>".to_string(),
                };
                let remove = if crate::registry::removable(w, p.live) {
                    format!(" <button class=\"wtremove\" data-key=\"{}\" title=\"remove this worktree and its branch\">✕</button>", esc(&p.key))
                } else {
                    String::new()
                };
                format!(" · {claude} {dirty} {ahead}{remove}")
            }
        };
```

and append `{state_html}` inside the `<a class="{cls}" …>…</a>` row after `{branch}`. The `<button>` inside an `<a>` is invalid HTML — render the row as `<span class="wtrow"><a …>…</a>{state_html}</span>` instead, with `.wtrow { display: flex; align-items: center; gap: 6px; }` in `style.css`, plus `.wtf { color: var(--muted); font-size: 12px; } .wtf.on { color: var(--fg); } .wtremove { font: inherit; font-size: 12px; cursor: pointer; background: none; border: 1px solid var(--muted); border-radius: 3px; }`.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep "test result"` → all ok.

- [ ] **Step 5: Revert-check** — in `removable`, drop the `dirty == Some(false)` clause; the render test's dirty case must fail; restore.

- [ ] **Step 6: Commit**

```bash
git add src/worktree.rs src/registry.rs src/routes.rs src/render.rs static/style.css
git commit -m "switcher: each worktree's state — Claude, dirty, ahead of its recorded base — and a remove control only when all three are positively clean"
```

---

### Task 9: `Intent::RemoveWorktree`

**Files:**
- Modify: `src/worktree.rs` (`remove`), `src/proto.rs` (`RemoveWorktree`), `src/hub.rs` (dispatch + `do_remove_worktree` + tests)

**Interfaces:**
- Produces: `worktree::remove(repo: &Path, path: &Path, branch: &str, run: GitRunner) -> Result<Option<String>, String>` (Ok(Some(note)) when the worktree went but the branch was kept); `Intent::RemoveWorktree { key: String }`.

- [ ] **Step 1: Write the failing tests**

`src/worktree.rs`:

```rust
    #[test]
    fn remove_takes_the_worktree_and_the_branch_when_git_agrees() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(remove(&repo, &c.path, "claude-1", &real_git).unwrap(), None);
        assert!(matches!(std::fs::symlink_metadata(&c.path), Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        assert!(real_git(&repo, &["branch", "--list", "claude-1"]).unwrap().trim().is_empty());
    }

    #[test]
    fn remove_keeps_an_unmerged_branch_and_says_so() {
        // git's own `-d` refusal is the gate here, not ours.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        std::fs::write(c.path.join("n.txt"), "y").unwrap();
        real_git(&c.path, &["add", "."]).unwrap();
        real_git(&c.path, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "wt"]).unwrap();
        let note = remove(&repo, &c.path, "claude-1", &real_git).unwrap().expect("a note");
        assert!(note.contains("claude-1") && note.contains("unmerged"), "{note}");
        assert!(!real_git(&repo, &["branch", "--list", "claude-1"]).unwrap().trim().is_empty(), "branch kept");
    }

    #[test]
    fn remove_refuses_a_dirty_worktree_without_force() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        std::fs::write(c.path.join("n.txt"), "y").unwrap();
        let err = remove(&repo, &c.path, "claude-1", &real_git).unwrap_err();
        assert!(err.contains("worktree remove"), "{err}");
        assert!(c.path.join("n.txt").is_file(), "nothing touched");
    }
```

`src/hub.rs` — a fixture plus four refusals, each with exactly one condition dirty, then success:

```rust
    /// A repo with one resh-created worktree, both registered under a temp
    /// state dir; returns (hub for the repo, worktree url, worktree dir).
    fn repo_with_worktree(root: &Path) -> (Hub, String, PathBuf) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let key_of = |n: &str| crate::projects::storage_key(&format!("repo/.claude/worktrees/{n}"));
        let c = crate::worktree::create(&repo, &crate::wsstate::state_dir(), &key_of, &crate::worktree::real_git).unwrap();
        (Hub::new("repo", repo), "repo/.claude/worktrees/claude-1".into(), c.path)
    }
    const WT_KEY: &str = "repo%2F.claude%2Fworktrees%2Fclaude-1";

    fn refusal_of(h: &mut Hub, a: &ConnId, rx: &Receiver<String>) -> String {
        h.handle(a, Intent::RemoveWorktree { key: WT_KEY.into() });
        let got = rx.try_recv().expect("an answer");
        assert!(got.contains(r#""t":"Error""#), "expected a refusal, got {got}");
        got
    }

    #[test]
    fn remove_refuses_a_worktree_with_a_live_terminal() {
        // Revert-checked: with the live check deleted this passes the
        // worktree to git, which removes it — the `is_dir` assertion fails.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        let _att = crate::session::attach(&url, "term", &dir).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("live terminal"), "{got}");
        assert!(dir.is_dir());
        crate::session::kill_project(&url);
    }

    #[test]
    fn remove_refuses_a_worktree_where_a_claude_was_launched() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        crate::session::set_launch(&url, "term", Some(crate::session::LaunchRequest { launch: proto::Launch::Claude, session_id: None }));
        let _att = crate::session::attach(&url, "term", &dir).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        // Names the more specific reason even though "live terminal" is also true.
        assert!(got.contains("Claude"), "{got}");
        assert!(dir.is_dir());
        crate::session::kill_project(&url);
    }

    #[test]
    fn remove_refuses_a_dirty_worktree() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        std::fs::write(dir.join("n.txt"), "y").unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("uncommitted"), "{got}");
        assert!(dir.join("n.txt").is_file());
    }

    #[test]
    fn remove_refuses_a_worktree_with_commits_ahead() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        std::fs::write(dir.join("n.txt"), "y").unwrap();
        crate::worktree::real_git(&dir, &["add", "."]).unwrap();
        crate::worktree::real_git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "wt"]).unwrap();
        let got = refusal_of(&mut h, &a, &rx);
        assert!(got.contains("1 commit"), "{got}");
        assert!(dir.is_dir());
    }

    #[test]
    fn remove_takes_a_clean_idle_worktree_and_its_state() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, _url, dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        let state = crate::wsstate::state_dir();
        std::fs::write(state.join(format!("{WT_KEY}.json")), "{}").unwrap();
        h.handle(&a, Intent::RemoveWorktree { key: WT_KEY.into() });
        let got = rx.try_recv().unwrap();
        assert!(!got.contains(r#""t":"Error""#), "{got}");
        assert!(matches!(std::fs::symlink_metadata(&dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        assert!(crate::worktree::read_base(&state, WT_KEY).is_none(), ".base gone");
        assert!(!state.join(format!("{WT_KEY}.json")).exists(), "layout gone");
    }

    #[test]
    fn remove_refuses_a_key_that_is_not_a_worktree_of_this_project() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", root.path().join("state"));
        std::env::set_var("RESH_ROOTS", root.path());
        let (mut h, _url, _dir) = repo_with_worktree(root.path());
        let (a, rx) = h.subscribe(); drain(&rx);
        h.handle(&a, Intent::RemoveWorktree { key: "repo".into() });
        let got = rx.try_recv().unwrap();
        assert!(got.contains("not a worktree of this project"), "the main checkout is never removable: {got}");
    }
```

The `RESH_ROOTS` env var: check `projects::roots()` reads it (`src/projects.rs:77`); these tests need the worktree url to resolve. If a `PROJECTS_ENV_LOCK` or similar exists for that variable, take it in the same order as the other tests that set it.

- [ ] **Step 2: Run to verify they fail** — `cargo test worktree::remove hub::remove_ 2>&1 | grep -E "^error" | head -3`.

- [ ] **Step 3: Implement**

`src/worktree.rs`:

```rust
/// `git worktree remove` without `--force`, then `git branch -d` without
/// `-D`. git's own refusals are gates that share no code with the caller's
/// checks: a dirty tree stops step one, an unmerged branch stops step two.
/// A kept branch is `Ok(Some(note))` — a branch is cheap, a lost commit is
/// not, and the caller reports it rather than retrying harder.
pub fn remove(repo: &Path, path: &Path, branch: &str, run: GitRunner) -> Result<Option<String>, String> {
    let p = path.to_string_lossy();
    run(repo, &["worktree", "remove", &p]).map_err(|e| format!("git worktree remove refused: {e}"))?;
    match run(repo, &["branch", "-d", branch]) {
        Ok(_) => Ok(None),
        Err(e) => Ok(Some(format!("worktree removed; branch {branch} kept: git reports it unmerged ({})", e.trim()))),
    }
}
```

`src/proto.rs` `Intent`:

```rust
    /// Remove a finished worktree of this project. The server re-derives
    /// every "finished" check at this moment; the row that offered the
    /// button is a hint, not an authorisation.
    RemoveWorktree { key: String },
```

`src/hub.rs` dispatch: `Intent::RemoveWorktree { key } => return self.do_remove_worktree(from, key.clone()),`. Then, same thread shape as `do_new_worktree`:

```rust
    fn do_remove_worktree(&mut self, from: &ConnId, key: String) {
        if self.closing {
            let ev = Event::Error { msg: "project is closing; try again in a moment".into() };
            return self.send_to(from, &ev);
        }
        let repo = self.dir.clone();
        let from = from.clone();
        let work = move || -> Result<Option<String>, String> {
            let url = crate::registry::decode_key(&key);
            // The key names a path under some root; git's listing of *this*
            // repo says whether that path is one of its linked worktrees.
            // `resolve_project` is not used: it refuses dot segments, and a
            // key that resolves to anything else is not ours to remove.
            let roots = crate::projects::roots();
            let canon = roots.iter()
                .find_map(|r| r.join(&url).canonicalize().ok())
                .ok_or_else(|| format!("{url}: not a worktree of this project"))?;
            let ws = crate::worktree::list(&repo);
            let w = ws.iter()
                .find(|w| !w.is_main && w.path.canonicalize().ok().as_deref() == Some(canon.as_path()))
                .ok_or_else(|| format!("{url}: not a worktree of this project"))?;
            let dir = canon.clone();
            let name = w.branch.clone();
            // Each check names itself. Order: the most specific first, so a
            // launched Claude is reported as such and not as "a terminal".
            match crate::claudes::claude_evidence(&url) {
                crate::claudes::ClaudeEvidence::Present(_) => return Err(format!("{name}: a Claude is running there")),
                crate::claudes::ClaudeEvidence::Unknown => return Err(format!("{name}: cannot tell whether a Claude is running there (IDE integration is off)")),
                crate::claudes::ClaudeEvidence::Absent => {}
            }
            if !crate::session::live_names(&url).is_empty() {
                return Err(format!("{name} has a live terminal"));
            }
            let state_dir = crate::wsstate::state_dir();
            let base = crate::worktree::read_base(&state_dir, &key)
                .or_else(|| ws.iter().find(|w| w.is_main).map(|m| m.branch.clone()))
                .ok_or_else(|| format!("{name}: no base to measure against"))?;
            let st = crate::worktree::state(&dir, &base, &crate::worktree::real_git);
            match st.dirty {
                Some(false) => {}
                Some(true) => return Err(format!("{name} has uncommitted changes")),
                None => return Err(format!("{name}: git did not answer (status)")),
            }
            match st.ahead {
                Some(0) => {}
                Some(n) => return Err(format!("{name} is {n} commit{} ahead of {base}", if n == 1 { "" } else { "s" })),
                None => return Err(format!("{name}: git did not answer (rev-list)")),
            }
            let note = crate::worktree::remove(&repo, &dir, &name, &crate::worktree::real_git)?;
            // Only after both git steps: resh's records about a thing that no longer exists.
            let _ = std::fs::remove_file(crate::worktree::base_file(&state_dir, &key));
            let _ = std::fs::remove_file(state_dir.join(format!("{key}.json")));
            Ok(note)
        };
        let finish = move |h: &mut Hub, r: Result<Option<String>, String>| {
            match r {
                Ok(note) => {
                    broadcast_all(&Event::ProjectsChanged { project: h.project.clone() });
                    if let Some(n) = note {
                        let ev = Event::Error { msg: n };
                        h.send_to(&from, &ev);
                    } else {
                        let ev = Event::ProjectsChanged { project: h.project.clone() };
                        h.send_to(&from, &ev);
                    }
                }
                Err(msg) => {
                    let ev = Event::Error { msg };
                    h.send_to(&from, &ev);
                }
            }
        };
        match self.self_ref.upgrade() {
            Some(arc) => {
                let spawned = std::thread::Builder::new().name("remove-worktree".into()).spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        .unwrap_or_else(|_| Err("worktree removal panicked".into()));
                    let mut h = Hub::lock(&arc);
                    finish(&mut h, r);
                });
                if spawned.is_err() {
                    let ev = Event::Error { msg: "could not start worktree removal".into() };
                    self.send_to(&from, &ev);
                }
            }
            None => { let r = work(); finish(self, r); }
        }
    }
```

The kept-branch note goes out as `Error` deliberately: it is shown as a banner, and the person should read it.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep "test result"` → all ok.

- [ ] **Step 5: Revert-check** — delete the `live_names` check; `remove_refuses_a_worktree_with_a_live_terminal` must fail on `dir.is_dir()`; restore. Delete the `st.ahead` match; `…with_commits_ahead` must fail on `"1 commit"`; restore.

- [ ] **Step 6: Commit**

```bash
git add src/worktree.rs src/proto.rs src/hub.rs
git commit -m "hub: RemoveWorktree — four checks re-derived at the click, then git's own two refusals; nothing is ever swept"
```

---

### Task 10: Reap orphan `.base` files

**Files:**
- Modify: `src/registry.rs` `reconcile_with` (after the socket sweep), tests

- [ ] **Step 1: Write the failing test** (near the existing reconcile tests; use the same `reconcile_with(&roots, &snapshot)` entry the neighbours use)

```rust
    #[test]
    fn a_base_file_for_a_worktree_that_no_longer_exists_is_reaped_and_one_that_does_is_kept() {
        // Revert-checked: `Path::exists()` instead of `symlink_metadata`
        // still passes this pair — so the unreadable case below is the one
        // that discriminates; without it this test could not fail on the
        // "cannot tell" branch.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::env::set_var("RESH_STATE_DIR", &state);
        let roots = vec![root.path().to_path_buf()];
        std::fs::create_dir_all(root.path().join("repo/.claude/worktrees/claude-1")).unwrap();
        let live = crate::projects::storage_key("repo/.claude/worktrees/claude-1");
        let gone = crate::projects::storage_key("repo/.claude/worktrees/claude-2");
        crate::worktree::write_base(&state, &live, "main").unwrap();
        crate::worktree::write_base(&state, &gone, "main").unwrap();
        // A base whose directory cannot be looked at: parent unreadable.
        let blocked = crate::projects::storage_key("repo/.claude/worktrees/blocked/deep");
        std::fs::create_dir_all(root.path().join("repo/.claude/worktrees/blocked")).unwrap();
        crate::worktree::write_base(&state, &blocked, "main").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path().join("repo/.claude/worktrees/blocked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        reconcile_with(&roots, &|| Some(String::new()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path().join("repo/.claude/worktrees/blocked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(crate::worktree::read_base(&state, &live).is_some(), "kept");
        assert!(crate::worktree::read_base(&state, &gone).is_none(), "reaped");
        assert!(crate::worktree::read_base(&state, &blocked).is_some(), "cannot tell: kept");
    }
```

If the test runs as root (permissions do not block), the `blocked` assertion is vacuous — guard it with `if nix_is_root() { return }` using `std::env::var("USER") == Ok("root".into())` and say so in a comment. Match the `snapshot` closure signature to `SnapshotFn`'s actual type at `src/registry.rs:585`.

- [ ] **Step 2: Run to verify it fails** — the `gone` assertion fails (no reaping yet).

- [ ] **Step 3: Implement** — at the end of `reconcile_with`, before `report` is returned:

```rust
    // resh's own worktree markers: a `.base` whose directory is *positively*
    // gone (NotFound, nothing else) is a file about nothing. Same rule as
    // `.origin`: unreadable is "cannot tell", and cannot-tell keeps.
    if roots_ok {
        if let Ok(rd) = std::fs::read_dir(crate::wsstate::state_dir().join("worktrees")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(key) = name.strip_suffix(".base") else { continue };
                if key.starts_with('.') { continue } // a temp file mid-write
                let url = decode_key(key);
                let mut candidate: Option<PathBuf> = None;
                for r in roots {
                    let p = r.join(&url);
                    match std::fs::symlink_metadata(&p) {
                        Ok(_) => { candidate = Some(p); break; }
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(_) => { candidate = Some(p); break; } // cannot tell: keep
                    }
                }
                if candidate.is_none() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
```

- [ ] **Step 4: Run** — `cargo test registry:: 2>&1 | grep "test result"` → ok.

- [ ] **Step 5: Revert-check** — change the `Err(_) =>` arm to `continue`; the `blocked` assertion must fail (when not root); restore.

- [ ] **Step 6: Commit**

```bash
git add src/registry.rs
git commit -m "registry: reap a .base whose worktree is positively gone — NotFound only, unreadable keeps"
```

---

### Task 11: The page — prompt, tab, `?launch=`, switcher controls

**Files:**
- Modify: `static/app.js:194` (State case), `:310` (event cases), `:691-702` (`newTerminal`), `:2083-2097` (switcher), `static/style.css`
- Modify: `tests/browser/harness.mjs` (add `attachTarget`), `tests/browser/README.md`
- Create: `tests/browser/worktree-launch.mjs`

**Interfaces:**
- Consumes: `ClaudeHere{pane, terminals}`, `WorktreeReady{url, launch}`, `NewTerminal{…, force}`, `NewWorktree{launch}`, `RemoveWorktree{key}`, `/frag/_worktrees?current=…&state=1`, the `.wtremove[data-key]` control.

- [ ] **Step 1: Write the browser test** — `tests/browser/worktree-launch.mjs`

```js
//! ✻ in a project that already has a Claude: the prompt, the worktree, the tab.
//!
//! Only a real browser can show three of the things the spec promises: that
//! a second ✻ opens *nothing* (asserted on the State snapshot's tabs, never on
//! event order — client-visible ordering is pipelined per connection and was
//! proved non-discriminating once, see README trap 2); that "new worktree"
//! ends in a second browser tab on the worktree project whose first terminal
//! typed `claude --session-id …` by itself; and that the switcher's remove
//! control appears only once that terminal is gone, then removes the
//! directory and the branch.
//!
//! The click on "Start in a new worktree" is a real CDP mouse click, not
//! `element.click()` from Runtime.evaluate: `window.open` needs a user
//! gesture, and this is the same path a person's click takes.
//!
//! Fake `claude`, as in claudeterm.mjs, printing its argv so `--session-id`
//! is observable.
//!
//! Run: deno run -A tests/browser/worktree-launch.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startResh, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const fx = await fixture();
const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(fakebin, { recursive: true });
// Stays running until stdin closes, so the terminal keeps "a launched Claude" alive.
await Deno.writeFile(`${fakebin}/claude`, enc.encode(`#!/bin/sh\necho "FAKE-CLAUDE-STARTED argv=$*"\ncat >/dev/null\n`), { mode: 0o755 });
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(`#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`), { mode: 0o755 });
const git = async (dir, ...args) => {
  const o = await new Deno.Command("git", { args: ["-C", dir, ...args], stdout: "piped", stderr: "piped" }).output();
  return { ok: o.success, out: new TextDecoder().decode(o.stdout), err: new TextDecoder().decode(o.stderr) };
};
const projDir = `${fx.roots}/${fx.project}`;
await git(projDir, "config", "user.email", "t@t"); await git(projDir, "config", "user.name", "t");
await git(projDir, "add", "."); await git(projDir, "commit", "-qm", "init");

const browser = await startBrowser(profileDir(repoRoot));
let page, page2, resh;
const helpers = `window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
    for (let i = 0; i < b.length; i++) o += b.getLine(i).translateToString(true) + "\\n"; return o; };
  window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);`;
const clickReal = async (pg, selector) => {
  const r = JSON.parse(await pg.evalIn(`JSON.stringify(document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect())`));
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  await pg.cmd("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
};

try {
  resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shell } });
  await until(async () => (await (await fetch(`http://127.0.0.1:${resh.port}/${fx.project}`)).text()).includes('data-launches="claude"'), 15, "claude offered");
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(helpers);

  console.log("A. first ✻ types claude --session-id");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 1, 20, "one terminal"), "a terminal opened");
  const first = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`))[0];
  await until(() => evalIn(`terms.has(${JSON.stringify(first)})`), 30, "attached");
  const started = await until(async () => /FAKE-CLAUDE-STARTED argv=--session-id [0-9a-f-]{36}/.test(await evalIn(`__txt(${JSON.stringify(first)})`)), 60, "claude with an id");
  ok(started, "claude was started with --session-id <uuid>");

  console.log("\nB. second ✻ asks instead of opening");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  ok(await until(() => evalIn(`!!document.querySelector('.pane[data-pane="3"] .claudehere')`), 10, "the prompt"), "the prompt appeared in the pane");
  await sleep(1500);
  ok(JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 1, "and no terminal was opened (State snapshot still has one)");
  ok((await evalIn(`document.querySelector('.claudehere').textContent`)).includes(first), `it names the terminal (${first})`);

  console.log("\nC. start in a new worktree → a second tab on claude-1 with claude started");
  const before = (await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json()).length;
  await clickReal(page, ".claudehere .wt-new");
  const targets = await until(async () => {
    const l = await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();
    const t = l.find((x) => x.url.includes(".claude/worktrees/claude-1"));
    return t || (l.length > before ? null : false);
  }, 30, "a tab on the worktree");
  ok(!!targets, "a second browser tab opened on the worktree project");
  const wt = await git(projDir, "worktree", "list", "--porcelain");
  ok(wt.out.includes(`${projDir}/.claude/worktrees/claude-1`) && wt.out.includes("refs/heads/claude-1"), "git lists the worktree and its branch");
  if (targets) {
    page2 = await attachTarget(targets.webSocketDebuggerUrl);
    await until(() => page2.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "worktree app.js");
    await page2.evalIn(helpers);
    ok(!(await page2.evalIn("location.search")), "the ?launch= parameter was consumed");
    ok(await until(async () => JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).length === 1, 20, "worktree terminal"), "the worktree opened its own terminal");
    const s2 = JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`))[0];
    await until(() => page2.evalIn(`terms.has(${JSON.stringify(s2)})`), 30, "attached");
    ok(await until(async () => /FAKE-CLAUDE-STARTED argv=--session-id/.test(await page2.evalIn(`__txt(${JSON.stringify(s2)})`)), 60, "claude in the worktree"), "…with claude already typed into it");
  }

  console.log("\nD. start here anyway");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  await until(() => evalIn(`!!document.querySelector('.pane[data-pane="3"] .claudehere')`), 10, "the prompt again");
  await evalIn(`document.querySelector('.claudehere .wt-here').click()`);
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 2, 20, "a second terminal here"), "force opens a second terminal in the original project");

  console.log("\nE. the switcher shows state, and removal waits for the terminal to end");
  await evalIn(`document.getElementById("wtbtn").click()`);
  ok(await until(() => evalIn(`(document.getElementById("wtstrip").textContent || "").includes("0 ahead")`), 15, "state in the switcher"), "the worktree row shows 0 ahead");
  ok(!(await evalIn(`!!document.querySelector("#wtstrip .wtremove")`)), "no remove control while its Claude terminal is attached");
  if (page2) {
    const s2 = JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`))[0];
    await page2.evalIn(`window.confirm = () => true; send({ t: "EndSession", session: ${JSON.stringify(s2)} })`);
    await until(async () => JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).length === 0, 20, "worktree terminal ended");
  }
  await evalIn(`document.getElementById("wtbtn").click(); document.getElementById("wtbtn").click()`);
  ok(await until(() => evalIn(`!!document.querySelector("#wtstrip .wtremove")`), 15, "the remove control"), "the remove control appears once the worktree is idle and clean");
  await evalIn(`window.confirm = () => true; document.querySelector("#wtstrip .wtremove").click()`);
  ok(await until(async () => !(await git(projDir, "worktree", "list", "--porcelain")).out.includes("claude-1"), 20, "worktree gone"), "clicking it removes the worktree");
  ok((await git(projDir, "branch", "--list", "claude-1")).out.trim() === "", "…and its branch");
  ok(await until(async () => { try { await Deno.stat(`${projDir}/.claude/worktrees/claude-1`); return false; } catch { return true; } }, 5, "directory gone"), "the directory is gone");
} finally {
  page?.close(); page2?.close();
  browser.close();
  if (resh) await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
```

Add to `tests/browser/harness.mjs`: split `openPage` so the CDP session setup can attach to a target the page itself opened. `openPage` keeps its signature and behaviour:

```js
export async function openPage(cdpPort, url) {
  const t = await (await fetch(`http://127.0.0.1:${cdpPort}/json/new?${url}`, { method: "PUT" })).json();
  return attachTarget(t.webSocketDebuggerUrl);
}

/// The rest of the old `openPage` body, verbatim, from `const ws = new
/// WebSocket(…)` through `return { cmd, evalIn, close }`. A tab opened by
/// `window.open` from inside a page is listed by `/json/list` with its own
/// debugger url; this is how a test drives it.
export async function attachTarget(webSocketDebuggerUrl) {
  const ws = new WebSocket(webSocketDebuggerUrl);
  // … unchanged from the current openPage: onopen, pending map, cmd with
  // its 30 s timeout, evalIn, Runtime.enable, close …
}
```

- [ ] **Step 2: Run it to see it fail** — `deno run -A tests/browser/worktree-launch.mjs 2>&1 | tail -20` → section B's "prompt appeared" fails (no `.claudehere`), the rest cascade. If it says SKIP for no browser, install per the README before continuing; this task cannot be verified without one.

- [ ] **Step 3: Implement `app.js`**

*State case* (`:194`): at the end of the `case "State":` block add, once:

```js
      // A tab opened by "start in a new worktree" arrives with ?launch=…;
      // consume it exactly once, after the first State, then strip it so a
      // reload does not start a second program.
      if (pendingLaunch && !pendingLaunchSent) {
        pendingLaunchSent = true;
        newTerminal(3, pendingLaunch);
        history.replaceState(null, "", location.pathname);
      }
```

and near the top of the file (after `const PROJECT = …`):

```js
const pendingLaunch = (() => {
  const l = new URLSearchParams(location.search).get("launch");
  return l && LAUNCHES.includes(l) ? l : null;
})();
let pendingLaunchSent = false;
// The blank tab opened synchronously on the "new worktree" click, navigated
// when WorktreeReady arrives. Opened on the click because a window.open after
// a websocket round trip is not reliably inside the user gesture.
let pendingTab = null;
```

*Event cases* (after `TerminalStarted`):

```js
    case "ClaudeHere":
      showClaudeHere(ev.pane, ev.terminals);
      break;
    case "WorktreeReady": {
      const url = "/" + projectPath(ev.url) + (ev.launch ? `?launch=${encodeURIComponent(ev.launch)}` : "");
      if (pendingTab && !pendingTab.closed) {
        pendingTab.location = url;
      } else {
        showBanner(`opened ${ev.url.split("/").pop()} — `);
        const a = document.createElement("a");
        a.href = url; a.target = "_blank"; a.textContent = "click to go there";
        document.querySelector(".error-banner:last-of-type b")?.append(a);
      }
      pendingTab = null;
      document.body.dispatchEvent(new Event("projects"));
      break;
    }
```

*The prompt* (next to `showBanner`):

```js
// The "a Claude is already here" prompt. Per-browser and transient: it is a
// question to the person who clicked, not a state of the project.
function showClaudeHere(pane, terminals) {
  document.querySelectorAll(".claudehere").forEach((n) => n.remove());
  const box = document.createElement("div");
  box.className = "conflict claudehere";
  const b = document.createElement("b");
  b.textContent = terminals.length
    ? `A Claude is already working in this project (${terminals.join(", ")}).`
    : "A Claude is already working in this project.";
  const wt = document.createElement("button");
  wt.className = "wt-new";
  wt.textContent = "Start in a new worktree";
  wt.onclick = () => {
    pendingTab = window.open("about:blank");
    send({ t: "NewWorktree", launch: "claude" });
    box.remove();
  };
  const here = document.createElement("button");
  here.className = "wt-here";
  here.textContent = "Start here anyway";
  here.onclick = () => { send({ t: "NewTerminal", pane, launch: "claude", force: true }); box.remove(); };
  const dismiss = document.createElement("button");
  dismiss.textContent = "dismiss";
  dismiss.onclick = () => box.remove();
  box.append(b, wt, here, dismiss);
  const host = document.querySelector(`.pane[data-pane="${pane}"]`) || document.body;
  host.prepend(box);
}
```

*Switcher* (`:2083-2097`): when opening, fetch the stateful fragment, and handle the remove control:

```js
  wtBtn.onclick = () => {
    wtPanel.hidden = !wtPanel.hidden;
    if (!wtPanel.hidden && window.htmx) {
      // State costs two git calls per worktree; ask only while looking.
      htmx.ajax("GET", `/frag/_worktrees?current=${encodeURIComponent(document.body.dataset.key || "")}&state=1`, "#wtstrip");
    }
  };
  wtPanel.onclick = (e) => {
    const rm = e.target.closest(".wtremove");
    if (rm) {
      e.preventDefault();
      const key = rm.dataset.key;
      const name = rm.closest(".wtrow")?.textContent.trim().split(/\s+/)[1] || key;
      if (confirm(`Remove worktree ${name} and its branch? resh re-checks that it is clean, idle and merged first.`)) send({ t: "RemoveWorktree", key });
      return;
    }
    if (e.target.closest("a")) wtPanel.hidden = true;
  };
```

`document.body.dataset.key` must exist: check `render::index_page`'s `<body …>` for the attribute that carries the current key (it passes `qkey` into the strip URL at `render.rs:1020`); if there is no such data attribute, add `data-key="{qkey}"` there.

`static/style.css`: `.claudehere { margin: 8px; } .claudehere button { margin: 8px 8px 0 0; font: inherit; cursor: pointer; } .claudehere .wt-new { font-weight: 600; }`.

- [ ] **Step 4: Run** — `deno run -A tests/browser/worktree-launch.mjs 2>&1 | tail -25` → `all ok`. Then the whole browser set the README lists, to catch regressions in `claudeterm.mjs` (its `FAKE-CLAUDE-STARTED` line now carries `argv=…`; its `sse=` assertion must still hold — adjust its fake to print both if needed).

- [ ] **Step 5: Revert-checks, recorded in the file header** — (a) remove the `force: true` from `.wt-here`'s intent: section D must fail (the prompt reappears, no second terminal); (b) make `showClaudeHere` also call `newTerminal(pane, "claude")`: section B's "no terminal was opened" must fail; (c) drop `history.replaceState`: section C's "parameter was consumed" must fail. Restore each; write the three results into the `//!` header.

- [ ] **Step 6: README** — add `worktree-launch.mjs` to the list in `tests/browser/README.md`'s *What a run does* with one line: *"the ✻ prompt, worktree creation into a second tab, switcher state and removal; needs a real CDP click for `window.open`"*.

- [ ] **Step 7: Commit**

```bash
git add static/app.js static/style.css tests/browser/worktree-launch.mjs tests/browser/harness.mjs tests/browser/README.md
git commit -m "app: the ✻ prompt, a worktree in a new tab with claude typed in, and switcher state with removal — browser-tested end to end"
```

---

### Task 12: Deploy verification and the by-hand checks the spec leaves open

**Files:**
- Modify: `docs/deploy.md` (record the run)

- [ ] **Step 1: Suite on the Linux host** — `cargo test 2>&1 | grep "test result"`, then every `tests/browser/*.mjs` the README lists. Time the Rust run; a hang is a lock-order defect, not a slow test.

- [ ] **Step 2: Deploy** per `docs/deploy.md`, and confirm the running binary changed (the doc's check).

- [ ] **Step 3: By hand, with the real `claude`, in a real browser on the deployed instance** — the two things no test covers:
  1. ✻ in a project with a live Claude → prompt → new worktree → the new tab's Claude starts and shows its `--session-id` in `ps -o args= -p <pid>`.
  2. In the switcher, the worktree row shows `✻` while that Claude runs; `/exit` it, `EndSession` the terminal, reopen the switcher: remove control present; remove; `git worktree list` no longer shows it.

- [ ] **Step 4: Record** both results with the date under the *Upgrading* note added in Task 1, and remove the `SessionStart` hook entry from `~/.claude/settings.json` on the host (the deploy step the spec lists).

- [ ] **Step 5: Commit**

```bash
git add docs/deploy.md
git commit -m "deploy: record the by-hand worktree-launch checks and the hook removal"
```
