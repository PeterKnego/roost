# Mention Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Alt+K mentions the file in the active tab — Preview as well as Edit — and the `at_mentioned` notification reaches only the Claude running in the terminal you are looking at.

**Architecture:** A connected Claude tells resh its pid (`ide_connected`). resh already exports `RESH_SESSION`/`RESH_PROJECT` into every shell it spawns, so reading `/proc/<pid>/environ` says which resh terminal that Claude is in. That answer is stored on the connection registry at connect time, and the mention fan-out filters on it instead of broadcasting.

**Tech Stack:** Rust (no async runtime, thread per connection), `serde_json`, `tungstenite`; plain browser JS with no framework; Deno + Chromium DevTools Protocol for browser tests.

**Spec:** `docs/superpowers/specs/2026-08-24-mention-routing-design.md`

## Global Constraints

Copied from the spec and CLAUDE.md. Every task's requirements implicitly include this section.

- **`cargo test`, never `cargo test --release`.**
- **This worktree has its own `.cargo/config.toml`** pointing `target-dir` at a local `target/`. Do not delete it and do not build from another checkout: `build.rs` bakes *absolute* asset paths, so a shared target dir leaves the binary built from the wrong tree while cargo reports `Fresh resh`.
- **Stage explicit paths.** Never `git add -A` in this repo — it once swept the user's uncommitted backlog notes into an unrelated commit.
- **Three outcomes, never two.** A failed read is not a negative result. `Err(_)` from `/proc` means *cannot tell*, and must never be folded into "not in a session".
- **Module-level `//!` doc explaining *why* the module exists.** Implementation first, `#[cfg(test)] mod tests` at the bottom of the same file. Comments give rationale, not mechanics.
- **Would this test fail if I deleted the code it covers?** Where a step says *revert-check*, actually apply the broken version, run it, read the failure, and restore — then record the observed failure text in the test's own comment.
- **Session names match `^[A-Za-z0-9_-]{1,32}$`** (`session::valid_name`, `src/session.rs:19-23`).
- **The absolute path travels.** Claude computes the relative path itself; never pre-relativise `filePath`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/idesess.rs` *(new)* | Pure: pid → which resh session that process runs in. No knowledge of connections or routing. Mirrors `idecwd.rs`. |
| `src/ide.rs` *(modify)* | Registry carries each connection's session; `mention_to` selects targets. `notify_all` stays, unchanged, for `selection_changed`. |
| `src/proto.rs` *(modify)* | `Intent::MentionPath` gains `session: Option<String>`. |
| `src/hub.rs` *(modify)* | `do_mention_path` validates the session name and calls `mention_to`. |
| `static/app.js` *(modify)* | Alt+K fires from Preview tabs, names the active terminal, and reports an empty target. |
| `tests/browser/ide.mjs` *(modify)* | Section F: the Preview trigger, end to end against a real Chromium and a real fake Claude. |

Task order is dependency order. Tasks 1–3 are server-side and independently testable; Task 4 connects the wire; Tasks 5–6 are the client.

---

### Task 1: `src/idesess.rs` — which session a pid runs in

**Files:**
- Create: `src/idesess.rs`
- Modify: `src/lib.rs` (add the module declaration)
- Test: `src/idesess.rs` (bottom of the same file, per house style)

**Interfaces:**
- Consumes: `crate::session::valid_name(&str) -> bool` (`src/session.rs:19`).
- Produces: `pub enum Sess { In(String), Outside, Unknown }` and
  `pub fn session_of(pid: u32, project: &str) -> Sess`,
  `pub fn session_of_in(proc_root: &Path, pid: u32, project: &str) -> Sess`.
  Task 2 stores `Sess` on the connection registry; Task 3 matches on it.

- [ ] **Step 1: Write the failing tests**

Create `src/idesess.rs` containing *only* the test module for now, so the first run fails on a missing implementation rather than on a missing file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a fake `/proc` whose pid 4242 has the given NUL-separated
    /// environment. Returns the TempDir so the caller keeps it alive — a
    /// dropped TempDir removes the fixture out from under the test.
    fn fake_proc(vars: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let pdir = d.path().join("4242");
        std::fs::create_dir(&pdir).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        for v in vars {
            buf.extend_from_slice(v.as_bytes());
            buf.push(0);
        }
        std::fs::write(pdir.join("environ"), &buf).unwrap();
        d
    }

    #[test]
    fn a_claude_in_a_resh_terminal_reports_its_session() {
        let d = fake_proc(&["PATH=/usr/bin", "RESH_PROJECT=karpie", "RESH_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::In("main".into()));
    }

    /// The distinction this whole enum exists for. A clean environment is
    /// evidence; an unreadable one is not.
    #[test]
    fn a_claude_started_outside_resh_is_outside() {
        let d = fake_proc(&["PATH=/usr/bin", "HOME=/home/x"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Outside);
    }

    #[test]
    fn an_unreadable_environ_is_unknown_not_outside() {
        // The pid directory exists but has no environ. Folding this into
        // Outside is how a live Claude silently stops receiving mentions.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("4242")).unwrap();
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    #[test]
    fn a_missing_proc_entry_is_unknown_not_outside() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    #[test]
    fn a_missing_proc_filesystem_is_unknown() {
        // Not Linux, or a container without /proc.
        let d = tempfile::tempdir().unwrap();
        let absent = d.path().join("no-proc-here");
        assert_eq!(session_of_in(&absent, 4242, "karpie"), Sess::Unknown);
    }

    /// Session names are unique within a project, not across them: `main`
    /// exists in most projects that have one.
    ///
    /// This is defence in depth, not the only barrier — and the spec's
    /// framing of it was too strong. `CONNS` is keyed by project, so a
    /// mention for project A cannot reach a connection registered under B no
    /// matter what this returns. What the RESH_PROJECT test actually buys is
    /// a correct answer for a Claude whose environment says one project
    /// while it is connected to another's socket (lock-file discovery by
    /// path, rather than the `CLAUDE_CODE_SSE_PORT` shortcut `session_env`
    /// sets). Rare, but "rare" is not "impossible", and `Outside` is the
    /// honest answer for it.
    #[test]
    fn the_same_session_name_in_another_project_is_outside() {
        let d = fake_proc(&["RESH_PROJECT=other", "RESH_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Outside);
    }

    /// A name that fails valid_name cannot be matched against anything, so
    /// it is "cannot tell", not "not here". Outside would exclude this
    /// connection from a mention; Unknown leaves it eligible.
    #[test]
    fn an_invalid_session_name_is_unknown_not_in() {
        let d = fake_proc(&["RESH_PROJECT=karpie", "RESH_SESSION=../../etc/passwd"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    /// A partially scrubbed environment: the session is named but the
    /// project is not, so the name cannot be trusted to mean this project.
    #[test]
    fn a_session_without_a_project_is_unknown() {
        let d = fake_proc(&["RESH_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    /// environ is NUL-separated, not newline-separated, and a value may
    /// itself contain '='. Splitting on the wrong byte or on every '='
    /// silently truncates the name.
    #[test]
    fn a_value_containing_an_equals_sign_survives() {
        let d = fake_proc(&["RESH_PROJECT=karpie", "RESH_SESSION=a-b", "OTHER=x=y=z"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::In("a-b".into()));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib idesess`
Expected: FAIL — the module is not declared in `lib.rs` yet, so this is a compile error (`failed to resolve: use of undeclared crate or module`). That is the correct first failure.

- [ ] **Step 3: Declare the module**

In `src/lib.rs`, add `mod idesess;` alongside the existing `mod idecwd;` declaration, keeping the list's existing alphabetical order (`idecwd` then `idelock` — `idesess` goes after `idelock`).

- [ ] **Step 4: Write the implementation**

Insert above the `#[cfg(test)] mod tests` block in `src/idesess.rs`:

```rust
//! Which resh terminal a process is running in, from its pid.
//!
//! `session_env` (`session.rs`) exports `RESH_PROJECT` and `RESH_SESSION`
//! into every shell resh spawns, originally so a program in that terminal
//! could attribute a `RESH_NOTIFY` notification to its session. A `claude`
//! started in that terminal inherits both, through dtach and through the
//! shell — which makes the same two variables the answer to the opposite
//! question: given a connected Claude's pid, which of this project's
//! terminals is it sitting in?
//!
//! That question has no answer in the IDE protocol. `ide_connected` carries
//! a pid and nothing else, so resh asks the kernel — the same move, for the
//! same reason, that `idecwd.rs` makes for the working directory.
//!
//! Three outcomes, not two. "I could not read this process's environment" is
//! not "this process is not in a resh terminal". Only the second is evidence,
//! and only the second may exclude a connection from a mention: a mention
//! that reaches one Claude too many is recoverable, one that reaches none
//! looks like a broken keystroke.
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Sess {
    /// `RESH_SESSION` read, the name is valid, and `RESH_PROJECT` names the
    /// project being asked about.
    In(String),
    /// The environment read cleanly and positively places this process
    /// outside this project's terminals — either no resh variables at all,
    /// or a different project's. Evidence, so it may exclude.
    Outside,
    /// resh could not tell. Never a reason to exclude a connection.
    Unknown,
}

pub fn session_of_in(proc_root: &Path, pid: u32, project: &str) -> Sess {
    // Read the whole thing: environ is a few KB and there is no way to seek
    // to a variable. A read error is the "cannot tell" case and is the only
    // failure this function can have — the parse below cannot fail.
    let raw = match std::fs::read(proc_root.join(pid.to_string()).join("environ")) {
        Ok(b) => b,
        Err(_) => return Sess::Unknown,
    };
    let mut session: Option<&str> = None;
    let mut proj: Option<&str> = None;
    // NUL-separated, and a *value* may contain '=' — so this splits on the
    // first '=' via strip_prefix on the full key, never on every '='.
    for entry in raw.split(|b| *b == 0) {
        let Ok(s) = std::str::from_utf8(entry) else { continue };
        if let Some(v) = s.strip_prefix("RESH_SESSION=") {
            session = Some(v);
        } else if let Some(v) = s.strip_prefix("RESH_PROJECT=") {
            proj = Some(v);
        }
    }
    match (session, proj) {
        // Both present and this is the project: the only case that can name
        // a terminal. An unusable name is "cannot tell", not "not here" —
        // Outside would exclude the connection, which is the wrong direction
        // for a value resh failed to make sense of.
        (Some(s), Some(p)) if p == project => {
            if crate::session::valid_name(s) { Sess::In(s.to_string()) } else { Sess::Unknown }
        }
        // A clean environment with neither variable: resh did not spawn this
        // process. Positive evidence.
        (None, None) => Sess::Outside,
        // Positively in a different project's terminal. Also evidence.
        (_, Some(_)) => Sess::Outside,
        // One variable without the other. Something scrubbed the
        // environment partially; the name cannot be trusted to mean this
        // project, and resh cannot tell what it does mean.
        (Some(_), None) => Sess::Unknown,
    }
}

pub fn session_of(pid: u32, project: &str) -> Sess {
    session_of_in(Path::new("/proc"), pid, project)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib idesess`
Expected: PASS, 9 tests.

- [ ] **Step 6: Revert-check the `Unknown` branch**

This is the assertion the whole module exists for, so prove it can fail. Temporarily change `Err(_) => return Sess::Unknown` to `Err(_) => return Sess::Outside`, then run `cargo test --lib idesess`.

Expected: `an_unreadable_environ_is_unknown_not_outside` and `a_missing_proc_entry_is_unknown_not_outside` and `a_missing_proc_filesystem_is_unknown` all fail. Read the failure text, restore the line, re-run to confirm green, and record the observed failure in a comment above `an_unreadable_environ_is_unknown_not_outside`, in the style `src/ide.rs:2126` uses:

```rust
    /// Revert-checked: changing the `Err(_)` arm to `Sess::Outside` failed
    /// this test — `assertion `left == right` failed: left: Outside, right:
    /// Unknown` — then restored.
```

- [ ] **Step 7: Commit**

```bash
git add src/idesess.rs src/lib.rs
git commit -m "idesess: which resh terminal a pid is running in, or that we cannot tell"
```

---

### Task 2: carry the session on the connection registry

**Files:**
- Modify: `src/ide.rs:289-294` (the `CONNS` type and its accessor)
- Modify: `src/ide.rs:311-321` (`ConnGuard::drop`)
- Modify: `src/ide.rs:370-383` (`notify_all`'s iteration)
- Modify: `src/ide.rs:811-822` (the `ide_connected` arm)
- Modify: `src/ide.rs:941` (the registration push)
- Modify: `src/ide.rs:2191-2194` (the one test that pushes into `CONNS` by hand)
- Test: `src/ide.rs` tests module

**Interfaces:**
- Consumes: `crate::idesess::{Sess, session_of}` from Task 1.
- Produces: `struct Target { id: u64, reply: Sender<String>, session: Sess }` as the `CONNS` value element, and `fn set_session(project: &str, id: u64, sess: Sess)`. Task 3 reads `Target::session`.

This task changes no behaviour: `mention` still reaches every connection. It only makes the session *known*. Keeping it separate is what lets Task 3's routing tests fail for the right reason.

- [ ] **Step 1: Write the failing tests**

Add to `src/ide.rs`'s tests module:

```rust
    /// The registry must record what `ide_connected` learned, or Task 3's
    /// routing has nothing to filter on. Asserted against the registry
    /// directly rather than through a mention, so a failure here says
    /// "not recorded" rather than "not delivered".
    #[test]
    fn a_connection_records_the_session_it_was_told_about() {
        let project = "sess-record";
        let (tx, _rx) = std::sync::mpsc::channel::<String>();
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut map = conns().lock().unwrap();
            map.entry(project.to_string())
                .or_default()
                .push(Target { id, reply: tx, session: crate::idesess::Sess::Unknown });
        }
        let _guard = ConnGuard { project: project.to_string(), id };
        set_session(project, id, crate::idesess::Sess::In("term3".into()));
        let map = conns().lock().unwrap();
        let t = map[project].iter().find(|t| t.id == id).expect("the connection is registered");
        assert_eq!(t.session, crate::idesess::Sess::In("term3".into()));
    }

    /// A pid that cannot exist gives a deterministic Unknown from the real
    /// /proc, which is what makes this assertion stable on any host: the
    /// point is that `ide_connected` writes the answer through to the
    /// registry at all, and that a failed lookup lands as Unknown rather
    /// than Outside. The three-outcome logic itself is covered by
    /// `idesess.rs`'s fixture tests.
    #[test]
    fn ide_connected_records_unknown_for_a_pid_that_cannot_exist() {
        let project = "sess-connected";
        let (tx, _rx) = std::sync::mpsc::channel::<String>();
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut map = conns().lock().unwrap();
            map.entry(project.to_string())
                .or_default()
                .push(Target { id, reply: tx.clone(), session: crate::idesess::Sess::Outside });
        }
        let _guard = ConnGuard { project: project.to_string(), id };
        let mut conn = Conn::new(project, std::path::PathBuf::from("/w"), tx, id);
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": u32::MAX}
        });
        assert!(dispatch(&msg, &mut conn).is_none(), "a notification must not be answered");
        let map = conns().lock().unwrap();
        let t = map[project].iter().find(|t| t.id == id).unwrap();
        assert_eq!(
            t.session,
            crate::idesess::Sess::Unknown,
            "a pid resh cannot read must land as Unknown, not left at its previous value"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ide::tests::a_connection_records_the_session -- --exact` and `cargo test --lib ide::tests::ide_connected_records_unknown`
Expected: FAIL to compile — `cannot find struct Target`, `cannot find function set_session`.

- [ ] **Step 3: Replace the registry tuple with a named struct**

At `src/ide.rs:289`, replace the `CONNS` static, its accessor, and add `Target` above them:

```rust
/// One registered connection, as the fan-out sees it.
///
/// A struct rather than the `(u64, Sender<String>)` tuple this used to be:
/// routing a mention needs to know which resh terminal each Claude is in,
/// and that fact has to live where the fan-out can read it. Putting it on
/// `Conn` instead — where `cwd` lives — would hide it from `notify_selected`,
/// and a second map keyed by conn id is a second lifetime to get right whose
/// failure mode is a dead connection claiming a terminal forever.
struct Target {
    id: u64,
    reply: std::sync::mpsc::Sender<String>,
    /// Learned from `ide_connected`'s pid. `Unknown` until that notification
    /// arrives — which is correct rather than merely convenient: between the
    /// handshake and `ide_connected` resh genuinely cannot tell, and
    /// `Unknown` is the value that leaves the connection eligible.
    session: crate::idesess::Sess,
}

static CONNS: OnceLock<Mutex<HashMap<String, Vec<Target>>>> = OnceLock::new();

fn conns() -> &'static Mutex<HashMap<String, Vec<Target>>> {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}
```

Leave the existing doc comment above `CONNS` (`src/ide.rs:285-288`, explaining why removal is keyed by id) in place — it is still true and still load-bearing.

- [ ] **Step 4: Update the four sites that touch the tuple shape**

`ConnGuard::drop` (`src/ide.rs:316`):

```rust
                v.retain(|t| t.id != self.id);
```

`notify_all` (`src/ide.rs:380-382`) — the clone-out and the send. It must keep cloning out under the lock and sending outside it:

```rust
    let targets: Vec<std::sync::mpsc::Sender<String>> = {
        let map = conns().lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).map(|v| v.iter().map(|t| t.reply.clone()).collect()).unwrap_or_default()
    };
    if targets.is_empty() {
        return Err("no Claude is connected to this project".into());
    }
    for t in &targets {
        let _ = t.send(msg.clone());
    }
    Ok(())
```

The registration push (`src/ide.rs:941`):

```rust
            map.entry(reg_project.clone()).or_default().push(Target {
                id: conn_id,
                reply: reg_tx.clone(),
                session: crate::idesess::Sess::Unknown,
            });
```

The hand-rolled push in `a_dropped_connection_deregisters_so_a_later_mention_reports_no_claude` (`src/ide.rs:2194`):

```rust
            map.entry(project.to_string())
                .or_default()
                .push(Target { id, reply: tx, session: crate::idesess::Sess::Unknown });
```

- [ ] **Step 5: Add `set_session` and call it from `ide_connected`**

Add next to `notify_all`:

```rust
/// Records what `ide_connected` learned about this connection's terminal.
///
/// Separate from registration because the two happen at different times and
/// cannot be merged: registration runs inside the handshake callback (see
/// `serve_conn`), before any message has been read, so the pid does not
/// exist yet. Looked up by id for the same reason removal is — the position
/// in the Vec is not stable across other connections coming and going.
fn set_session(project: &str, id: u64, sess: crate::idesess::Sess) {
    let mut map = conns().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = map.get_mut(project) {
        if let Some(t) = v.iter_mut().find(|t| t.id == id) {
            t.session = sess;
        }
    }
}
```

Extend the `ide_connected` arm (`src/ide.rs:811-822`) so it records both facts from the one pid:

```rust
        if method == "ide_connected" {
            let pid = msg["params"]["pid"].as_u64().unwrap_or(0) as u32;
            match idecwd::cwd_of(pid) {
                Cwd::At(p) => conn.cwd = Some(p),
                // Gone and Unknown both leave cwd unset, and neither closes
                // the connection here: the socket itself is the evidence that
                // something is on the other end, and it is more trustworthy
                // than a /proc lookup that just failed.
                Cwd::Gone | Cwd::Unknown => {}
            }
            // Written through to CONNS rather than kept on `conn`, because
            // the mention fan-out reads the registry, not this struct. Every
            // outcome is recorded, Unknown included — leaving the previous
            // value in place on a failed lookup would let a stale answer
            // outlive the evidence for it.
            set_session(&conn.project, conn.id, crate::idesess::session_of(pid, &conn.project));
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS — 589 + 2 new = 591 lib, 67 integration, 0 failed. `a_mention_reaches_every_connected_claude_not_just_the_first` must still pass here; this task changes no routing.

- [ ] **Step 7: Commit**

```bash
git add src/ide.rs
git commit -m "ide: the registry records which terminal each Claude connected from"
```

---

### Task 3: `ide::mention_to` — send to one Claude

**Files:**
- Modify: `src/ide.rs` (add `notify_selected` and `mention_to`; `mention` becomes a wrapper)
- Modify: `src/ide.rs:2166-2175` (`a_mention_reaches_every_connected_claude_not_just_the_first` — its contract changes)
- Test: `src/ide.rs` tests module

**Interfaces:**
- Consumes: `Target`, `set_session` from Task 2; `Sess` from Task 1.
- Produces: `pub fn mention_to(project: &str, session: Option<&str>, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String>`. Task 4 calls it.

**`notify_all` is not replaced.** `selection_changed` keeps using it: an ambient selection is not aimed at a terminal, and narrowing it is a different decision nobody has made.

- [ ] **Step 1: Rewrite the fan-out test to the new contract**

`a_mention_reaches_every_connected_claude_not_just_the_first` (`src/ide.rs:2166`) asserts the *old* behaviour and must now assert the new one. Replace it and its doc comment with:

```rust
    /// Two terminals, two claudes, one project, and no terminal named. The
    /// old contract was "reach both"; the new one is "refuse", because a
    /// mention that reaches a Claude the user was not looking at interrupts
    /// work they did not ask about. Two clients, not one — with a single
    /// subscriber `send to the chosen one` and `send to all` are
    /// indistinguishable, the trap CLAUDE.md records.
    #[test]
    fn an_unaimed_mention_with_two_claudes_is_refused_not_broadcast() {
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("mention-ambiguous");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("mention-ambiguous");
        let err = mention_to("mention-ambiguous", None, Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("2 "), "the message must say how many are connected: {err}");
        assert!(
            rx1.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the first Claude must receive nothing"
        );
        assert!(
            rx2.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the second Claude must receive nothing"
        );
    }

    /// The fan-out itself still exists and still works — `selection_changed`
    /// uses it. Without this, deleting `notify_all` outright would leave the
    /// suite green.
    #[test]
    fn notify_all_still_reaches_every_connected_claude() {
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("notify-fanout");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("notify-fanout");
        notify_all("notify-fanout", &serde_json::json!({"jsonrpc": "2.0", "method": "ping"}))
            .unwrap();
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }

    /// The case the feature exists for: two Claudes, one named. The
    /// assertion that matters is the *empty* inbox, not the full one.
    #[test]
    fn a_named_mention_reaches_only_that_terminals_claude() {
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("mention-aimed");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("mention-aimed");
        // Label the two registered connections in registration order.
        {
            let mut map = conns().lock().unwrap();
            let v = map.get_mut("mention-aimed").expect("both clients registered");
            assert_eq!(v.len(), 2, "the test needs two distinct connections");
            v[0].session = crate::idesess::Sess::In("term1".into());
            v[1].session = crate::idesess::Sess::In("term2".into());
        }
        mention_to("mention-aimed", Some("term2"), Path::new("/w/x.rs"), None).unwrap();
        assert!(
            rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
            "the named terminal's Claude must receive it"
        );
        assert!(
            rx1.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the other terminal's Claude must receive nothing"
        );
    }

    /// resh could not read one connection's environment. Excluding it would
    /// silently drop the mention; including it costs one extra notification.
    /// The conservative direction is the one this asserts.
    #[test]
    fn a_connection_resh_cannot_place_stays_eligible() {
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("mention-unknown");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("mention-unknown");
        {
            let mut map = conns().lock().unwrap();
            let v = map.get_mut("mention-unknown").unwrap();
            v[0].session = crate::idesess::Sess::Outside;
            v[1].session = crate::idesess::Sess::Unknown;
        }
        mention_to("mention-unknown", Some("term9"), Path::new("/w/x.rs"), None).unwrap();
        assert!(
            rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
            "an Unknown connection must still be reached"
        );
        assert!(
            rx1.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "an Outside connection must not be, once a terminal is named"
        );
    }

    /// One Claude and a terminal name nothing claims: the lone connection is
    /// still the only sensible target, and refusing here would break the
    /// ordinary single-Claude case whenever the environ read failed.
    #[test]
    fn a_lone_claude_receives_a_mention_it_does_not_claim() {
        let (rx, _a, _d, _w) = connected_fake_client_for("mention-lone");
        {
            let mut map = conns().lock().unwrap();
            map.get_mut("mention-lone").unwrap()[0].session = crate::idesess::Sess::Outside;
        }
        mention_to("mention-lone", Some("term7"), Path::new("/w/x.rs"), None).unwrap();
        assert!(rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }

    /// Two Claudes, both positively placed elsewhere. Unlike the lone case
    /// above there is nothing to fall back to, so this must refuse rather
    /// than pick one.
    #[test]
    fn a_mention_for_a_terminal_no_claude_is_in_is_refused() {
        let (rx1, _a, _d1, _w1) = connected_fake_client_for("mention-nomatch");
        let (rx2, _b, _d2, _w2) = connected_fake_client_for("mention-nomatch");
        {
            let mut map = conns().lock().unwrap();
            let v = map.get_mut("mention-nomatch").unwrap();
            v[0].session = crate::idesess::Sess::In("term1".into());
            v[1].session = crate::idesess::Sess::In("term2".into());
        }
        let err = mention_to("mention-nomatch", Some("term9"), Path::new("/w/x.rs"), None)
            .unwrap_err();
        assert!(err.contains("term9"), "the message must name the terminal asked for: {err}");
        assert!(rx1.recv_timeout(std::time::Duration::from_millis(300)).is_err());
        assert!(rx2.recv_timeout(std::time::Duration::from_millis(300)).is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ide::tests`
Expected: FAIL to compile — `cannot find function mention_to`.

- [ ] **Step 3: Write the implementation**

Add beside `notify_all`, and change `mention`:

```rust
/// Sends `msg` to the connections a mention is aimed at, or reports why
/// none were chosen. Never falls back to the whole fan-out: reaching a
/// Claude the user was not looking at is the failure this exists to prevent,
/// and it is invisible when it happens.
///
/// Cloned out under the lock, sent outside it, for the reason `notify_all`
/// gives.
fn notify_selected(
    project: &str,
    session: Option<&str>,
    msg: &serde_json::Value,
) -> Result<(), String> {
    let msg = msg.to_string();
    // Every decision is made inside the one lock scope, and only the sends
    // happen outside it. Taking the lock a second time for the lone-connection
    // fallback would let a connection arrive or die between the two looks, so
    // the fallback would act on a different registry than the filter saw.
    let chosen: Vec<std::sync::mpsc::Sender<String>> = {
        let map = conns().lock().unwrap_or_else(|e| e.into_inner());
        let all: &[Target] = map.get(project).map(|v| v.as_slice()).unwrap_or(&[]);
        let total = all.len();
        if total == 0 {
            // Unchanged wording: existing tests match on "no Claude".
            return Err("no Claude is connected to this project".into());
        }
        let matched: Vec<std::sync::mpsc::Sender<String>> = match session {
            // No terminal named, so nothing to match on; the lone-connection
            // case below is the only way an unaimed mention gets delivered.
            None => Vec::new(),
            Some(want) => all
                .iter()
                .filter(|t| match &t.session {
                    crate::idesess::Sess::In(s) => s == want,
                    // resh could not place this Claude, so it cannot rule it
                    // out. One notification too many is recoverable; none
                    // looks like a broken keystroke.
                    crate::idesess::Sess::Unknown => true,
                    crate::idesess::Sess::Outside => false,
                })
                .map(|t| t.reply.clone())
                .collect(),
        };
        if !matched.is_empty() {
            matched
        } else if total == 1 {
            // One Claude is unambiguous whatever resh managed to learn about
            // where it lives — including nothing at all. This is what keeps
            // the ordinary single-Claude case working when the environ read
            // failed, or when Claude was started outside resh entirely.
            all.iter().map(|t| t.reply.clone()).collect()
        } else {
            return Err(match session {
                Some(want) => format!(
                    "no Claude is running in terminal \"{want}\" ({total} connected to this project)"
                ),
                None => format!(
                    "{total} Claudes are connected to this project — click the terminal you mean, then press Alt+K"
                ),
            });
        }
    };
    for t in &chosen {
        let _ = t.send(msg.clone());
    }
    Ok(())
}

/// Tells the Claude in `session`'s terminal that the user pointed at `abs`
/// (and optionally a line range within it). A notification, not a request —
/// see `dispatch`'s id handling: an `at_mentioned` carrying an `id` would
/// make the CLI wait for a response that will never come.
///
/// `session` is `None` when the browser had no terminal tab in focus.
pub fn mention_to(
    project: &str,
    session: Option<&str>,
    abs: &Path,
    lines: Option<(u32, u32)>,
) -> Result<(), String> {
    let mut params = serde_json::json!({"filePath": abs.to_string_lossy()});
    if let Some((a, b)) = lines {
        params["lineStart"] = serde_json::json!(a);
        params["lineEnd"] = serde_json::json!(b);
    }
    let msg = serde_json::json!({"jsonrpc": "2.0", "method": "at_mentioned", "params": params});
    notify_selected(project, session, &msg)
}

/// An unaimed mention. Kept so existing callers and tests that do not care
/// about terminals read the same as before.
pub fn mention(project: &str, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String> {
    mention_to(project, None, abs, lines)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS. `mention_shape` and `mention_wholefile` (`src/ide.rs:2115`, `2133`) each use one client and must still pass unchanged — they exercise the `None` + `total == 1` path.

- [ ] **Step 5: Revert-check the privacy assertion**

Change `notify_selected`'s call in `mention_to` to `notify_all(project, &msg)` and run
`cargo test --lib ide::tests::a_named_mention_reaches_only_that_terminals_claude -- --exact`.

Expected: FAIL on `the other terminal's Claude must receive nothing`. Restore, re-run green, and record the observed failure in that test's doc comment.

- [ ] **Step 6: Commit**

```bash
git add src/ide.rs
git commit -m "ide: a mention goes to the terminal it was aimed at, or nowhere"
```

---

### Task 4: thread the session through the wire

**Files:**
- Modify: `src/proto.rs:115-119` (the `MentionPath` variant)
- Modify: `src/hub.rs:436-438` (the dispatch arm)
- Modify: `src/hub.rs:1242-1274` (`do_mention_path`)
- Modify: `src/hub.rs:3315`, `:3352`, `:3384` (three struct literals)
- Test: `src/proto.rs` and `src/hub.rs` tests modules

**Interfaces:**
- Consumes: `ide::mention_to` from Task 3; `session::valid_name`.
- Produces: `Intent::MentionPath { rel, line_start, line_end, session }` on the wire. Task 5 sends it.

- [ ] **Step 1: Write the failing tests**

In `src/proto.rs`'s tests module:

```rust
    /// The field is optional on the wire so an older client's payload still
    /// parses. Pinning it means a later `#[serde(deny_unknown_fields)]` or a
    /// dropped `default` cannot silently start rejecting them.
    #[test]
    fn a_mention_without_a_session_still_decodes() {
        let i = decode(r#"{"t":"MentionPath","rel":"a.rs","line_start":null,"line_end":null}"#)
            .unwrap();
        assert!(matches!(i, Intent::MentionPath { session: None, .. }));
    }

    #[test]
    fn a_mention_carries_the_terminal_it_is_aimed_at() {
        let i = decode(
            r#"{"t":"MentionPath","rel":"a.rs","line_start":null,"line_end":null,"session":"term2"}"#,
        )
        .unwrap();
        assert!(matches!(i, Intent::MentionPath { session: Some(s), .. } if s == "term2"));
    }
```

In `src/hub.rs`'s tests module, beside the existing mention tests:

```rust
    /// A session name off the wire is refused before it reaches `ide`, and
    /// the refusal goes only to the client that asked — the same rule the
    /// sibling path-confinement test pins. Two subscribers, because with one
    /// `send_to` and `broadcast` are indistinguishable.
    #[test]
    fn an_invalid_session_name_is_refused_and_only_the_asker_hears() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", dir.path().join("state"));
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("mentionsess", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::MentionPath {
            rel: "a.rs".into(),
            line_start: None,
            line_end: None,
            session: Some("../../etc/passwd".into()),
        });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("session")),
            "an invalid session name must be refused by name, not treated as 'no Claude': {got:?}"
        );
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(others.is_empty(), "a refusal must reach only the asker, got: {others:?}");
        std::env::remove_var("RESH_STATE_DIR");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib proto:: hub::tests::an_invalid_session_name`
Expected: FAIL to compile — `struct variant Intent::MentionPath has no field named session`.

- [ ] **Step 3: Add the field**

`src/proto.rs:119`, keeping the existing doc comment above it and extending it:

```rust
    /// A file or selection the user wants Claude to look at. Resolved
    /// server-side and sent as `at_mentioned`, not pasted into a terminal:
    /// a paste lands in whatever state the terminal is in and competes with
    /// whatever Claude is doing at that instant.
    ///
    /// `session` is the terminal the browser had in focus, and it is what
    /// makes this reach one Claude rather than all of them. `None` means the
    /// browser had no terminal tab active — a real answer, not a missing
    /// one, and the server refuses rather than guessing when more than one
    /// Claude is connected. Optional on the wire so an older client's
    /// payload still parses.
    MentionPath {
        rel: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
        #[serde(default)]
        session: Option<String>,
    },
```

- [ ] **Step 4: Update the dispatch arm and `do_mention_path`**

`src/hub.rs:436-438`:

```rust
            Intent::MentionPath { rel, line_start, line_end, session } => {
                return self.do_mention_path(
                    from,
                    rel.clone(),
                    *line_start,
                    *line_end,
                    session.clone(),
                )
            }
```

`src/hub.rs:1242` — new parameter, validation, and the call. Everything between the `safe_resolve` block and the `lines` match is unchanged:

```rust
    fn do_mention_path(
        &mut self,
        from: &ConnId,
        rel: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
        session: Option<String>,
    ) {
        let abs = match crate::projects::safe_resolve(&self.dir, &rel) {
            Ok(p) => p,
            Err(e) => {
                let ev = Event::Error { msg: e };
                return self.send_to(from, &ev);
            }
        };
        // Refused rather than dropped to `None`: silently ignoring an
        // unusable name would degrade an aimed mention into an unaimed one,
        // which is a different request. Truncated in the message because the
        // value is unvalidated wire input and an error banner is not the
        // place to render four kilobytes of it.
        if let Some(s) = &session {
            if !crate::session::valid_name(s) {
                let shown: String = s.chars().take(32).collect();
                let ev = Event::Error {
                    msg: format!("mention: invalid session name {shown:?}"),
                };
                return self.send_to(from, &ev);
            }
        }
        // ... the existing `lines` match is unchanged ...
        if let Err(e) = crate::ide::mention_to(&self.project, session.as_deref(), &abs, lines) {
            let ev = Event::Error { msg: e };
            self.send_to(from, &ev);
        }
    }
```

- [ ] **Step 5: Add `session: None` to the three existing struct literals**

Rust requires every field on a struct-variant literal, so `#[serde(default)]` does not help here. At `src/hub.rs:3315`, `:3352` and `:3384`, add `session: None` to each `Intent::MentionPath { .. }`. Say why in each, since each is a different routing case:

```rust
        // session: None — this test is about path confinement, not routing.
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS, 0 failed.

- [ ] **Step 7: Commit**

```bash
git add src/proto.rs src/hub.rs
git commit -m "mention: carry the terminal the browser had in focus"
```

---

### Task 5: the client fires from Preview and names the terminal

**Files:**
- Modify: `static/app.js:1747-1758` (`mentionTarget`)
- Modify: `static/app.js:1776-1784` (the Alt+K handler)
- Modify: `static/app.js:2188` (`focusSession` — record the last focus)
- Test: covered by Task 6; no Rust test can reach this file.

**Interfaces:**
- Consumes: `Intent::MentionPath`'s `session` field from Task 4; the existing `showError` (`app.js:1871`), `mentionSelection` (`app.js:1761`), `MIDDLE`/`RIGHT` pane constants.
- Produces: `mentionTarget()` now returns `{rel, mode} | null`; `activeTerminalSession()` returns `string | null`.

**Do not touch `saveTarget()` (`app.js:1716-1727`).** It is a line-for-line twin with the same two clauses, and the temptation to factor them together is wrong: saving is meaningful only for a tab backed by a textarea, so *its* `Edit` and `editors.has` tests are load-bearing where `mentionTarget`'s are not.

- [ ] **Step 1: Rewrite `mentionTarget`**

```js
// Which tab a mention keystroke means, and in which mode — same "focused
// editor first, else the visible MIDDLE/RIGHT File tab" rule saveTarget uses
// above, for the same reason: focus is often on the body, not a textarea,
// right after a reconnect or a click elsewhere on the page.
//
// Two clauses that look alike are doing different jobs here. `mode === "Edit"`
// is what this function deliberately stops requiring — a Preview tab is a
// perfectly good thing to point Claude at. `editors.has(rel)` stays, but only
// on the Edit branch: `editors` holds textareas, so a Preview tab never has an
// entry, and keeping that test unconditional would leave this returning null
// for every Preview tab — the feature would look implemented and do nothing.
function mentionTarget() {
  const el = document.activeElement;
  if (el && el.classList && el.classList.contains("editor")) {
    for (const [rel, ta] of editors) if (ta === el) return { rel, mode: "Edit" };
  }
  for (const p of [MIDDLE, RIGHT]) {
    const pane = state && state.panes && state.panes[p];
    const tab = pane && pane.tabs[pane.active];
    if (!tab || tab.k !== "File") continue;
    if (tab.mode === "Edit") {
      // An Edit tab whose textarea has not mounted yet is not a target; fall
      // through to the other pane, exactly as this did before.
      if (editors.has(tab.rel)) return { rel: tab.rel, mode: "Edit" };
      continue;
    }
    return { rel: tab.rel, mode: tab.mode };
  }
  return null;
}
```

- [ ] **Step 2: Add `activeTerminalSession` and record the last focus**

Directly above `mentionTarget`, add:

```js
// The terminal a mention is aimed at. Recorded on focus rather than derived
// from the layout alone: two panes can each hold an active Terminal tab, and
// the layout says nothing about which one the user last looked at.
let lastFocusedSession = null;

function activeTerminalSession() {
  if (!state) return null;
  const live = [];
  for (const pane of state.panes) {
    const tab = pane.tabs[pane.active];
    if (tab && tab.k === "Terminal") live.push(tab.session);
  }
  if (!live.length) return null;
  // The remembered one only counts while it is still an active tab somewhere;
  // otherwise a closed terminal would keep claiming every mention.
  if (live.includes(lastFocusedSession)) return lastFocusedSession;
  return live[0];
}
```

In `focusSession` (`app.js:2188`), record the focus immediately after the existing guard, so every path that activates a terminal — tab click, notice click, `connectControl`'s deferred focus — updates it through the one funnel they all already share:

```js
function focusSession(session) {
  if (!session || !SESSION_RE.test(session) || !state) return;
  lastFocusedSession = session;
  markSessionNoticesRead(session);
```

- [ ] **Step 3: Rewrite the Alt+K handler**

```js
// Alt+K, matching the extensions' own binding. The selection's line range
// travels; the text does not (that is ShareSelection, and it is opt-in).
document.addEventListener("keydown", (e) => {
  if (!e.altKey || e.key.toLowerCase() !== "k") return;
  const target = mentionTarget();
  if (target === null) {
    // Alt+K is Meta-k in readline, so a keystroke aimed at a shell must not
    // raise a banner about tabs. Only a keystroke with nowhere to go and no
    // terminal under it is worth reporting.
    if (e.target && e.target.closest && e.target.closest(".xterm")) return;
    // Silence here is indistinguishable from a broken binding, which is how
    // this was reported in the first place.
    showError("Alt+K mentions the file in the active tab — open a file first.");
    return;
  }
  e.preventDefault();
  // A Preview tab has no textarea and no source-line mapping, so it mentions
  // the whole file. See the spec's "Why a preview carries no line range".
  const sel = target.mode === "Edit"
    ? mentionSelection(target.rel)
    : { startLine: null, endLine: null };
  send({
    t: "MentionPath",
    rel: target.rel,
    line_start: sel.startLine,
    line_end: sel.endLine,
    session: activeTerminalSession(),
  });
});
```

- [ ] **Step 4: Verify no other caller of `mentionTarget` broke**

Run: `grep -n "mentionTarget" static/app.js`
Expected: three hits — the definition, the Alt+K handler, and the `selectionchange` listener in the selection-sharing block (`app.js:~1795`). **That third one returns a bare `rel` today and must be updated**, or `ShareSelection` starts sending `[object Object]` as a path:

```js
    const target = mentionTarget(); // same "which tab" rule Alt+K uses
    if (target === null || target.mode !== "Edit") return;
    const sel = shareSelectionSnapshot(target.rel);
```

The `mode !== "Edit"` guard is new and correct: `shareSelectionSnapshot` reads a textarea, so a Preview tab has nothing for it to send.

- [ ] **Step 5: Check it compiles and the Rust suite is unaffected**

Run: `cargo test`
Expected: PASS, 0 failed — no Rust test reaches `app.js`, so this only confirms nothing else regressed. **A green suite here proves nothing about this task.** Task 6 is the real verification.

- [ ] **Step 6: Commit**

```bash
git add static/app.js
git commit -m "app: Alt+K fires from a preview and names the terminal it is aimed at"
```

---

### Task 6: browser test — the Preview trigger, end to end

**Files:**
- Modify: `tests/browser/ide.mjs` (new section F, after section E at line ~444)

**Interfaces:**
- Consumes: the existing harness in that file — `evalIn`, `until`, `poll`, `ok`, `cmd`, `claude.log`, `projectDir`.

Read `tests/browser/README.md` first: it lists four traps that make a browser test pass while asserting nothing, and this test can fall into two of them.

- [ ] **Step 1: Add the markdown fixture**

Beside the existing fixtures (`tests/browser/ide.mjs:190-203`):

```js
const PREVIEW_FILE = "preview-me.md";
```

and next to the other `writeTextFile` calls:

```js
await Deno.writeTextFile(`${projectDir}/${PREVIEW_FILE}`, "# Heading\n\nSome prose.\n");
```

- [ ] **Step 2: Write the failing test**

Append after section E:

```js
console.log("\nF. Alt+K mentions a markdown file from its Preview tab");
// The regression this guards: mentionTarget used to require mode === "Edit"
// AND editors.has(rel). A Preview tab satisfies neither, so Alt+K silently
// did nothing. "No error appeared" is equally true of a handler that
// returned null, so this asserts the notification itself arrived AND that it
// names this file — per tests/browser/README.md's trap list.
await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(PREVIEW_FILE)}, mode: "Preview" } })`);
ok(await until(() => evalIn(`!!document.querySelector("article.markdown-body")`), 10, "the rendered preview"),
   "the markdown file opens in a preview, not an editor");
ok(await evalIn(`!document.querySelector("textarea.editor")`),
   "and there is no textarea — otherwise this is testing the Edit path by accident");

// Select a run of rendered text. It must NOT produce a line range: rendered
// markdown has no source-line mapping, and a guessed range is worse than none.
await evalIn(`(() => {
  const p = document.querySelector("article.markdown-body p");
  const r = document.createRange();
  r.selectNodeContents(p);
  const s = window.getSelection();
  s.removeAllRanges();
  s.addRange(r);
})()`);
for (const type of ["rawKeyDown", "keyUp"]) {
  await cmd("Input.dispatchKeyEvent", {
    type, modifiers: 1 /* Alt */, key: "k", code: "KeyK",
    windowsVirtualKeyCode: 75, nativeVirtualKeyCode: 75,
  });
}
const previewed = await poll(
  () => claude.log.find((m) => m.method === "at_mentioned" && (m.params?.filePath || "").endsWith(PREVIEW_FILE)),
  10, "the at_mentioned notification for the previewed file",
);
ok(previewed, "Alt+K in a Preview tab reaches Claude");
ok(previewed.params.lineStart === undefined && previewed.params.lineEnd === undefined,
   `a preview selection carries no line range, got ${JSON.stringify(previewed.params)}`);
```

- [ ] **Step 3: Run it against a real browser and watch it fail**

Run: `deno run -A tests/browser/ide.mjs`
Expected: section F FAILS at "Alt+K in a Preview tab reaches Claude" if run against the pre-Task-5 `app.js`. If you are running it after Task 5, confirm the failure mode instead by temporarily restoring the old single-line `mentionTarget` loop body — the whole point of this test is that it can distinguish the two.

- [ ] **Step 4: Run it against the implemented client**

Run: `deno run -A tests/browser/ide.mjs`
Expected: every section passes, F included.

- [ ] **Step 5: Run the full browser suite**

Run: `deno run -A tests/browser/reconnect.mjs` and `deno run -A tests/browser/upload.mjs`, plus the rest of `tests/browser/`.
Expected: all green. `app.js` changed, and per CLAUDE.md that file is reachable by no Rust test.

- [ ] **Step 6: Full suite and commit**

```bash
cargo test
git add tests/browser/ide.mjs
git commit -m "browser test: a preview tab mentions its file, with no line range"
```

---

## Wrap-up

- [ ] **Rebase onto master.** Another session has been committing there throughout (`24cdab4` and later). `git fetch` is not needed — same repo — but do `git rebase master` from this worktree and re-run `cargo test` plus the browser suite afterwards. As of writing, none of `proto.rs`, `hub.rs`, `ide.rs` or `app.js` had changed on master since `868ad4f`, but that was true at plan time, not at merge time — check again.
- [ ] **Remove the `docs/backlog.md:21` entry.** It is the request this plan implements; the backlog's own header says to pull items out when they are picked up. The markdown-line-range deferral added in `ea6d0b3` stays.
- [ ] **Do not commit `.cargo/config.toml`.** It is worktree infrastructure. Committing it would follow the branch into master and redirect the main checkout away from the shared build cache.
