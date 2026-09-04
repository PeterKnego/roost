# A Claude that outlived roost should say so — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When Alt+K finds no Claude on the IDE socket, say what is actually true — a Claude that predates this roost cannot reconnect on its own — and stop handing a project's ephemeral IDE port to a different project after a restart.

**Architecture:** Two independent halves. The *primary* half is a message: the `/proc` walk that already finds Claudes by `ROOST_PROJECT`/`ROOST_SESSION` also reads `CLAUDE_CODE_SSE_PORT` from the same bytes, and `ide::notify_selected` turns one sentence into three. The *secondary* half is a persisted port: a new `src/ideport.rs` records the port each project last bound in `<state>/ide/<storage_key>.port`, and `ide::start_in` tries to take it again before falling back to an OS-assigned one.

**Tech Stack:** Rust, no async, no new dependencies. `std::net::TcpListener`, `std::fs` for `/proc` and the record. Deno + Chromium for the restart-survival test.

**Spec:** `docs/superpowers/specs/2026-09-04-stable-ide-port-design.md`

## Global Constraints

- **`cargo test`, never `cargo test --release`.** Run it as `cargo test -- --test-threads=1`: a bare `cargo test` hangs on this project.
- **Build from one checkout.** This host points every cargo workspace at a shared `target-dir` and `build.rs` bakes absolute asset paths into a generated table. Work in `/home/claude/projects/roost` directly; never a second checkout or a git worktree.
- **Absence of evidence is not evidence of absence.** Every `/proc` read has three outcomes: read cleanly, absent, and *could not read*. In this plan that rule lands on a **message** rather than a kill: "roost could not read this Claude's port" and "this Claude's port is stale" are different sentences, and neither is "no Claude is connected". `src/idesess.rs`'s `Sess` enum and the new `src/procsess.rs`'s `Sid` are the in-repo models.
- **Never hold a lock across blocking I/O.** `ide::start_in` runs with no lock held and `for_project_in` deliberately drops the registry lock across it — the port-record read and write go inside `start_in`, not inside a lock.
- **No panics may escape a socket thread.** `notify_selected` is reached from a websocket intent; `start_in` from a terminal connect.
- **`ROOST_STATE_DIR` is the state root** (`wsstate::state_dir()`), and on this host it points at `~/.local/state/resh`, not the `~/.local/state/roost` default. Never hardcode either.
- **Project storage keys are percent-encoded** (`projects::storage_key`) — `karpie%2Fsrc`, never a raw `/` in a filename.
- **Style:** module-level `//!` doc explaining *why*; implementation first, `#[cfg(test)] mod tests` at the bottom of the same file; comments give rationale, not mechanics.
- **Test comment convention:** revert-checked tests record the **assertion message** they were observed to fail with, never a `line:column` — a comment written above an assertion shifts that assertion's line number, so a number measured before writing the comment is stale by construction. This cost two fix rounds on the previous branch.

## Decisions taken from the spec's open questions

1. **`/ide` is NOT named in the message.** The spec says to measure it or leave it out. It was not measured: the CLI does register `/ide` (`{type:"local-jsx", name:"ide", description:"Manage IDE integrations and show status"}` in the 2.1.260 bundle), but whether it re-runs discovery and reconnects a session whose port went stale was never observed. The two repairs the message *does* name are measured — probe B on 2026-09-04 showed a newly started `claude` with a stale port connects via the workspace-path fallback, so both "start a new terminal" and "restart claude in that one" work. *Reviewer: overturning this means running the measurement first, not adding the word.*
2. **The banner describes the repair, it does not perform it.** No button. The ✻ new-terminal path already exists and an error banner that spawns shells is a new kind of thing.
3. **The overview gets no new glyph.** Out of scope for this plan; the spec lists it as an open question and nothing here depends on it.
4. **The port record is not swept.** One small file per project, written at most once per changed binding. `reconcile` already sweeps the state dir on a throttle and this can ride along later if it ever matters.

## File Structure

| File | Responsibility |
|---|---|
| `src/ideport.rs` *(new)* | The file that remembers which port a project last bound. Read, atomic write, nothing else. Sibling to `idelock.rs`, which owns the *other* file — the one in Claude's own `~/.claude/ide` that advertises the live listener. Different directory, different reader, different lifetime, so a separate module. |
| `src/lib.rs` *(modify)* | Register the new module, alphabetically. |
| `src/ide.rs` *(modify: `start_in` at :103, `notify_selected` at :433)* | Bind the recorded port when there is one; turn one refusal sentence into three. |
| `src/claudes.rs` *(modify: `try_claude_terminals` at :83 and the types around it)* | Carry `CLAUDE_CODE_SSE_PORT` out of the walk it already does, without changing what `tick` treats as a change. |
| `src/routes.rs` *(modify: :252-257)* | Follows the scan's type change; no behaviour change. |
| `tests/browser/ide.mjs` *(modify)* | Restart survival: a terminal opened before a restart still names the live listener afterwards. The one thing no Rust test can reach. |

---

### Task 1: `ideport` — remember which port a project bound

**Files:**
- Create: `src/ideport.rs`
- Modify: `src/lib.rs` (add `pub mod ideport;` between `pub mod idelock;` and `pub mod idesess;`)
- Test: `src/ideport.rs` (`#[cfg(test)] mod tests` at the bottom)

**Interfaces:**
- Consumes: `crate::projects::storage_key`, `crate::wsstate::state_dir`.
- Produces:
  - `pub fn recorded_in(dir: &std::path::Path, project: &str) -> Option<u16>`
  - `pub fn record_in(dir: &std::path::Path, project: &str, port: u16)`

  Task 2 calls **only these two**, passing `wsstate::state_dir().join("ide")` itself from `ide::start_in`. Do **not** add `recorded(project)` / `record(project, port)` convenience wrappers: nothing would call them, and this project builds with zero warnings, so a `dead_code` warning is a build regression. The `_in` shape is also what keeps tests out of the real state dir.

**Why atomic:** `registry::write_origin` already establishes the pattern in this repo and its doc comment explains it — a temp file with a **pid-unique** name, then `rename`, because two roosts sharing one `ROOST_STATE_DIR` is a supported configuration and a shared temp name hands the truncate-then-write window straight back.

- [ ] **Step 1: Write the failing test**

Create `src/ideport.rs` with the module doc, stub items, and the tests:

```rust
//! Which TCP port a project's IDE listener last bound.
//!
//! `ide::start_in` binds port 0 and takes whatever the OS gives it, so every
//! roost start hands every project a different port. That is invisible until
//! you notice what else it means: `session_env` bakes `CLAUDE_CODE_SSE_PORT`
//! into a shell at spawn time and dtach sessions outlive roost, so a
//! surviving shell holds a port number that a *later* roost start can hand to
//! a different project. Claude Code matches a lock file by port **before** it
//! tries to match by path (`gBt` in the 2.1.260 bundle: `else if (v.port ===
//! r) R = true`), so that Claude would connect to the other project's
//! listener, with the other project's token, and never compare a path at all.
//!
//! Remembering the port removes the draw. This is state roost writes about
//! itself, not configuration: no config key, no per-project override.
//!
//! Advisory, never authoritative. A recorded port that cannot be bound is not
//! an error — the caller falls back to an OS-assigned one and records that
//! instead. Two roosts sharing a `ROOST_STATE_DIR` will alternate, which is
//! the correct outcome for a hint.
use std::path::{Path, PathBuf};

fn record_path(dir: &Path, project: &str) -> PathBuf {
    dir.join(format!("{}.port", crate::projects::storage_key(project)))
}

pub fn recorded_in(_dir: &Path, _project: &str) -> Option<u16> {
    None
}

pub fn record_in(_dir: &Path, _project: &str, _port: u16) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_port_reads_back() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 45123);
        assert_eq!(recorded_in(d.path(), "karpie"), Some(45123));
    }

    #[test]
    fn no_record_is_none_and_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(recorded_in(d.path(), "never-seen"), None);
        assert!(!d.path().join("never-seen.port").exists());
    }

    #[test]
    fn a_nested_project_key_is_percent_encoded_not_a_directory() {
        // `karpie/src` must not become a `karpie/` subdirectory, and must not
        // collide with a project literally named `karpie%2Fsrc`.
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie/src", 40001);
        assert_eq!(recorded_in(d.path(), "karpie/src"), Some(40001));
        assert!(d.path().join("karpie%2Fsrc.port").exists());
        assert!(!d.path().join("karpie").exists());
    }

    #[test]
    fn a_later_record_replaces_an_earlier_one() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 1111);
        record_in(d.path(), "karpie", 2222);
        assert_eq!(recorded_in(d.path(), "karpie"), Some(2222));
    }

    #[test]
    fn an_unparseable_record_is_none_not_a_panic() {
        // A hand-edited or truncated file must degrade to "no hint", never
        // abort a project from opening. Asserted on the value, not merely
        // that it did not panic.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path()).unwrap();
        std::fs::write(d.path().join("karpie.port"), b"not a port\n").unwrap();
        assert_eq!(recorded_in(d.path(), "karpie"), None);
        std::fs::write(d.path().join("karpie.port"), b"99999999\n").unwrap();
        assert_eq!(recorded_in(d.path(), "karpie"), None, "out of u16 range is not a port");
    }

    #[test]
    fn port_zero_is_never_recorded_and_never_returned() {
        // 0 means "let the OS choose". Recording it would make the next start
        // "restore" a meaningless hint, and returning it would make the caller
        // bind ephemeral while believing it restored something.
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 0);
        assert_eq!(recorded_in(d.path(), "karpie"), None);
        std::fs::write(d.path().join("other.port"), b"0\n").unwrap();
        assert_eq!(recorded_in(d.path(), "other"), None);
    }

    #[test]
    fn writing_leaves_no_temp_file_behind() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 45123);
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "karpie.port")
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib ideport -- --test-threads=1
```

Expected: `a_recorded_port_reads_back`, `a_nested_project_key_is_percent_encoded_not_a_directory` and `a_later_record_replaces_an_earlier_one` FAIL (`left: None, right: Some(...)` / a missing file). The other four pass vacuously against the stub — that is expected and is why they are not the only tests.

- [ ] **Step 3: Write the implementation**

```rust
/// The recorded port, or `None` when there is no usable hint.
///
/// Every failure is `None`: no file, an unreadable one, a value that is not a
/// number, one outside `u16`, or `0`. A hint roost cannot make sense of is
/// worth exactly as much as no hint, and the caller's fallback is the same in
/// both cases — so unlike the `/proc` readers elsewhere in this crate there is
/// no third outcome to preserve here. Nothing destructive hangs off it.
pub fn recorded_in(dir: &Path, project: &str) -> Option<u16> {
    let raw = std::fs::read_to_string(record_path(dir, project)).ok()?;
    match raw.trim().parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(p) => Some(p),
    }
}

/// Records `port`, atomically. Best-effort: a failure means the next start
/// falls back to an OS-assigned port, which is exactly today's behaviour.
///
/// Temp file with a **pid-unique** name, then `rename` — `registry::write_origin`'s
/// pattern and for its reason: two roosts sharing one `ROOST_STATE_DIR` is a
/// supported configuration, and a shared temp name would let one process's
/// `rename` publish the other's half-written file.
pub fn record_in(dir: &Path, project: &str, port: u16) {
    // 0 is "the OS chooses" and is never a hint worth keeping.
    if port == 0 {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{}.port.tmp.{}", crate::projects::storage_key(project), std::process::id()));
    if std::fs::write(&tmp, format!("{port}\n")).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, record_path(dir, project)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}
```

Note the temp name starts with `.` so a stranded one is a dotfile, matching `write_origin`'s reasoning about enumerations that skip dotfiles. `writing_leaves_no_temp_file_behind` will therefore also catch a stranded temp — `read_dir` returns dotfiles.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib ideport -- --test-threads=1
```

Expected: 7 passed.

- [ ] **Step 5: Revert-check, message-only comment**

Write the comment first with a placeholder, then measure, then fill it in. Revert: change `Ok(0) | Err(_) => None` to `Err(_) => None, Ok(p) => Some(p)` (i.e. let 0 through). Run `cargo test --lib ideport -- --test-threads=1`, observe `port_zero_is_never_recorded_and_never_returned` fail, and record the **assertion message** — not a line:column. Restore and re-run green.

- [ ] **Step 6: Commit**

```bash
git add src/ideport.rs src/lib.rs
git commit -m "ideport: remember which port a project's IDE listener bound"
```

---

### Task 2: `ide::start_in` takes the recorded port when it can

**Files:**
- Modify: `src/ide.rs` — `start_in` (around :103)
- Test: `src/ide.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ideport::{recorded, record}` from Task 1.
- Produces: no signature change. `start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String>` keeps its shape; `Ide::port` keeps its meaning ("the port actually bound").

**The rule, from the spec:** no record → bind 0, record what was bound. Record present → try `bind(("127.0.0.1", recorded))`; **on any failure fall back to 0** and record the new port. Never fail a project over it — `for_project_in` already treats a failed listener as a degraded convenience, not an error.

- [ ] **Step 1: Write the failing test**

Add to `ide.rs`'s test module. Read the existing tests first — several already build an `Ide` against a temp lock dir, and `idelock::set_ide_dir_for_test` / `isolate_ide_dir_for_test` exist so tests never touch the real `~/.claude/ide`.

```rust
/// The port a project bound is offered back to it on the next start, which
/// is what keeps a surviving shell's `CLAUDE_CODE_SSE_PORT` pointing at this
/// project rather than at whichever project the OS hands the number to next.
#[test]
fn a_second_start_rebinds_the_port_the_first_one_recorded() {
    let lockdir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let first = start_in_with_ports(lockdir.path(), &state.path().join("ide"), "portsame", PathBuf::from("/tmp"))
        .expect("first listener");
    let port = first.port;
    assert_ne!(port, 0, "an OS-assigned port must be read back after bind");
    first.request_shutdown();
    // Wait for the port to be released before asking for it again; a bind
    // racing the old listener's close would fall back and hide the feature.
    for _ in 0..40 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let second = start_in_with_ports(lockdir.path(), &state.path().join("ide"), "portsame", PathBuf::from("/tmp"))
        .expect("second listener");
    assert_eq!(second.port, port, "the recorded port must be taken again");
    second.request_shutdown();
}

/// The fallback, which is the whole reason the record is advisory: something
/// else owns the recorded port, so the project gets an OS-assigned one and
/// the record is updated to match reality.
#[test]
fn a_recorded_port_that_is_taken_falls_back_and_re_records() {
    let lockdir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // Squat a real port and record it as this project's.
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = squatter.local_addr().unwrap().port();
    crate::ideport::record_in(&state.path().join("ide"), "portbusy", taken);
    let ide = start_in_with_ports(lockdir.path(), &state.path().join("ide"), "portbusy", PathBuf::from("/tmp"))
        .expect("a taken port must degrade, never fail the project");
    assert_ne!(ide.port, taken, "must not claim to have bound a port someone else holds");
    assert_ne!(ide.port, 0);
    assert_eq!(
        crate::ideport::recorded_in(&state.path().join("ide"), "portbusy"),
        Some(ide.port),
        "the record must be corrected to the port actually bound"
    );
    ide.request_shutdown();
    drop(squatter);
}

/// A lock file always advertises the port actually bound — never the hint.
#[test]
fn the_lock_file_names_the_port_actually_bound() {
    let lockdir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    crate::ideport::record_in(&state.path().join("ide"), "portlock", 1);  // privileged, will fail
    let ide = start_in_with_ports(lockdir.path(), &state.path().join("ide"), "portlock", PathBuf::from("/tmp"))
        .expect("a hint that cannot be bound must not fail the project");
    assert!(lockdir.path().join(format!("{}.lock", ide.port)).exists());
    ide.request_shutdown();
}
```

Port 1 is used deliberately: binding it needs privilege this process does not have, so it exercises the failure path without depending on another process. If the test host ever runs as root this test would bind successfully and assert nothing — note that in the test's own comment.

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib ide:: -- --test-threads=1
```

Expected: FAIL to compile — `cannot find function \`start_in_with_ports\``.

- [ ] **Step 3: Write the implementation**

Give `start_in` a state-dir-parameterised sibling so tests never touch the real state dir, exactly as `start_in` is `dir`-parameterised for the lock dir:

```rust
pub fn start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    start_in_with_ports(dir, &crate::wsstate::state_dir().join("ide"), project, workspace)
}

/// `start_in`, with the port-record directory injected. Production calls
/// `start_in`; tests call this so they never write into the real state dir —
/// the same reason `idelock::set_ide_dir_for_test` exists for the lock dir.
pub(crate) fn start_in_with_ports(
    dir: &Path,
    ports: &Path,
    project: &str,
    workspace: PathBuf,
) -> Result<Arc<Ide>, String> {
    // The recorded port is a hint, not a requirement: taking it again is what
    // keeps a surviving shell's baked `CLAUDE_CODE_SSE_PORT` pointing at *this*
    // project instead of at whichever project the OS hands the number to next.
    // Anything at all can be holding it by now — another roost, another
    // service, or this project's own listener from a restart that has not
    // finished closing — so a failure is ordinary and silent.
    let listener = crate::ideport::recorded_in(ports, project)
        .and_then(|p| TcpListener::bind(("127.0.0.1", p)).ok())
        .map_or_else(|| TcpListener::bind(("127.0.0.1", 0)), Ok)
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // Recorded after the bind, from `local_addr`, never from the hint: the
    // record has to say what is true, and on the fallback path the hint is
    // exactly what was not true.
    crate::ideport::record_in(ports, project, port);
    let token = idelock::new_token()?;
    // … the rest of the existing body, unchanged, from `let lock = …` on.
}
```

Read the existing `start_in` body and keep everything from `idelock::new_token()` onward exactly as it is.

- [ ] **Step 4: Run the whole suite**

```bash
cargo test -- --test-threads=1
```

Expected: all pass. **The `ide` module's existing tests are the ones to watch**: several build listeners in the same process, and they now share a port-record directory unless each supplies its own. If any existing test starts failing intermittently, that is real information about test isolation — investigate it rather than adding a retry.

- [ ] **Step 5: Revert-check, message-only comment**

Comment first with a placeholder, then measure. Revert: drop the `recorded_in(...).and_then(...)` and bind `0` unconditionally. Run `cargo test --lib ide:: -- --test-threads=1`, observe `a_second_start_rebinds_the_port_the_first_one_recorded` fail, record the real assertion message. Restore, re-run green.

- [ ] **Step 6: Commit**

```bash
git add src/ide.rs
git commit -m "ide: take the recorded port again when it is still free"
```

---

### Task 3: the `/proc` walk carries the port it already read past

**Files:**
- Modify: `src/claudes.rs` (`try_claude_terminals` at :83, `names_for`, `changed_projects`, `tick`, `fake_proc`, `claude_evidence_with_scan`)
- Modify: `src/routes.rs` (:252-257 — follows the type, no behaviour change)
- Test: `src/claudes.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
  pub struct ClaudeProc { pub project: String, pub session: String, pub sse_port: Option<u16> }
  pub fn try_claude_terminals(proc_root: &Path) -> Option<Vec<ClaudeProc>>
  pub fn claude_terminals(proc_root: &Path) -> Vec<ClaudeProc>
  pub fn sse_port_in(scan: &[ClaudeProc], project: &str, session: &str) -> Option<Option<u16>>
  ```
  `sse_port_in`'s doubled `Option` is deliberate and Task 4 depends on it: the outer is "is there a Claude in this terminal at all", the inner is "could roost read its port". Those are different questions and Task 4 renders a different sentence for each.

**The trap this task must not spring.** `tick` calls `changed_projects(&cell, &fresh)` to wake only the projects whose Claudes changed. If the port becomes part of what is compared, a port change counts as a project change — and every project's Claudes would appear to change on the first tick after a roost restart, waking every open workspace for nothing. **`changed_projects` must keep comparing `(project, session)` pairs only.** Read it before you change the type, and add the test below that pins it.

- [ ] **Step 1: Write the failing test**

Add to `claudes.rs`'s test module, alongside the existing `a_running_claude_process_names_its_terminal`:

```rust
#[test]
fn the_walk_reads_the_sse_port_from_the_same_environ() {
    let d = tempfile::tempdir().unwrap();
    let mk = |pid: u32, comm: &str, env: &str| {
        let p = d.path().join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
    };
    mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\nCLAUDE_CODE_SSE_PORT=41011\n");
    // No port at all: a claude started before the integration existed, or with
    // the kill switch off. Distinct from a port roost could not parse.
    mk(200, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term4\n");
    mk(300, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term5\nCLAUDE_CODE_SSE_PORT=notanumber\n");
    let scan = claude_terminals(d.path());
    assert_eq!(sse_port_in(&scan, "karpie", "term3"), Some(Some(41011)));
    assert_eq!(sse_port_in(&scan, "karpie", "term4"), Some(None), "present, port unknown");
    assert_eq!(sse_port_in(&scan, "karpie", "term5"), Some(None), "unparseable is unknown, not absent");
    assert_eq!(sse_port_in(&scan, "karpie", "term9"), None, "no Claude in that terminal at all");
    assert_eq!(sse_port_in(&scan, "other", "term3"), None, "another project's terminal");
}

#[test]
fn a_port_change_alone_does_not_wake_a_project() {
    // `tick` broadcasts to every project `changed_projects` names, and a roost
    // restart changes every surviving Claude's port relative to the live
    // listener. If the port entered the comparison, the first tick after every
    // restart would wake every open workspace for no visible change.
    let before = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), sse_port: Some(1111) }];
    let after = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), sse_port: Some(2222) }];
    assert!(changed_projects(&before, &after).is_empty());
    // …but a terminal appearing or leaving still does.
    let gone: Vec<ClaudeProc> = vec![];
    assert_eq!(changed_projects(&before, &gone), vec!["karpie".to_string()]);
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib claudes -- --test-threads=1
```

Expected: FAIL to compile — `cannot find struct \`ClaudeProc\``, `cannot find function \`sse_port_in\``.

- [ ] **Step 3: Write the implementation**

Replace the `(String, String)` tuple with `ClaudeProc` throughout `claudes.rs`, parse `CLAUDE_CODE_SSE_PORT` in the same `environ` loop that already reads `ROOST_PROJECT`/`ROOST_SESSION`, and add:

```rust
/// This terminal's Claude's `CLAUDE_CODE_SSE_PORT`, if there is a Claude
/// there at all.
///
/// Two `Option`s, and they answer different questions. `None` means no Claude
/// in that terminal. `Some(None)` means a Claude is there but roost could not
/// read a port from it — an environment without the variable (the integration
/// was off when it started), or a value that is not a `u16`. `Some(Some(p))`
/// is a Claude whose port roost knows. `ide::notify_selected` says something
/// different for each, which is the whole reason this is not flattened.
pub fn sse_port_in(scan: &[ClaudeProc], project: &str, session: &str) -> Option<Option<u16>> {
    scan.iter().find(|c| c.project == project && c.session == session).map(|c| c.sse_port)
}
```

`changed_projects` currently builds `HashSet<&(String, String)>` and takes their
`symmetric_difference`, so if it were handed `ClaudeProc`s it would compare the
port too. Project the pairs out explicitly:

```rust
/// Projects whose set of (terminal, Claude) pairs differs between two scans.
///
/// Compares `(project, session)` only, never the port. `tick` broadcasts to
/// every project this names, and a roost restart changes the port of every
/// surviving Claude relative to the new listener — so including it here would
/// wake every open workspace on the first tick after every restart, for a
/// change no client renders. The port is carried on `ClaudeProc` for
/// `ide::notify_selected` to read on an Alt+K, not for this.
fn changed_projects(old: &[ClaudeProc], new: &[ClaudeProc]) -> Vec<String> {
    let key = |c: &ClaudeProc| (c.project.clone(), c.session.clone());
    let a: HashSet<(String, String)> = old.iter().map(key).collect();
    let b: HashSet<(String, String)> = new.iter().map(key).collect();
    let mut out: Vec<String> = a.symmetric_difference(&b).map(|(p, _)| p.clone()).collect();
    out.sort();
    out.dedup();
    out
}
```

`routes.rs:252-257` needs only to follow the type; `claude_evidence_with_scan` and `names_for` read `.project` / `.session` where they read `.0` / `.1` today.

- [ ] **Step 4: Run the whole suite**

```bash
cargo test -- --test-threads=1
```

Expected: all pass, including `hub`'s snapshot test, which seeds this cache through `fake_proc` — update `fake_proc` to build `ClaudeProc`s and keep its existing signature working for callers that do not care about ports.

- [ ] **Step 5: Revert-check, message-only comment**

Comment first with a placeholder, then measure. Two reverts, run separately:
1. Make `changed_projects` compare whole `ClaudeProc`s including the port; observe `a_port_change_alone_does_not_wake_a_project` fail; record the assertion message.
2. Make the `environ` parse fold an unparseable port into "absent" the same way as a missing one — observe that `the_walk_reads_the_sse_port_from_the_same_environ` still passes (both are `Some(None)`), so **do not** write a revert-check comment claiming that revert fails. Note in the report that this one is deliberately not discriminated: the two cases are rendered identically by design, and the spec's third message row covers both.

- [ ] **Step 6: Commit**

```bash
git add src/claudes.rs src/routes.rs
git commit -m "claudes: carry each Claude's SSE port out of the walk we already do"
```

---

### Task 4: three answers where there was one

**Files:**
- Modify: `src/ide.rs` — `notify_selected` (around :433) and `mention_to`
- Test: `src/ide.rs`

**Interfaces:**
- Consumes: `claudes::sse_port_in`, `claudes::claude_terminals` (or the cached scan) from Task 3; `ide::port_for` for the live port.
- Produces: a pure function so the wording is testable without a socket:
  ```rust
  pub(crate) fn no_connection_message(
      session: Option<&str>,
      scan_port: Option<Option<u16>>,   // from claudes::sse_port_in
      live_port: Option<u16>,
  ) -> String
  ```

**The four cases, from the spec's table:**

| `scan_port` | meaning | message |
|---|---|---|
| `None` | no Claude in that terminal | `no Claude is connected to this project` — unchanged, it is true |
| `Some(Some(p))`, `p != live` | stale port | `Claude in "term1" predates this roost (it has port 41011, this roost is on 46793) and cannot reconnect on its own — start a new terminal, or restart claude in that one` |
| `Some(Some(p))`, `p == live` | port matches, still no socket | `Claude is running in "term1" but is not connected to roost` |
| `Some(None)` | port unreadable | `Claude is running in "term1" but is not connected to roost` |

The last two share a sentence deliberately: roost cannot distinguish them in a way that changes what the user should do. **Do not name `/ide`** — see the decisions section; it is not measured.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_no_connection_message_says_which_of_the_four_things_happened() {
    // Asserted on the rendered sentence, not an intermediate: CLAUDE.md
    // records a message-formatting defect that every test in its module was
    // structurally unable to see, because they all asserted on the same
    // intermediate string.
    let m = no_connection_message(Some("term1"), None, Some(46793));
    assert_eq!(m, "no Claude is connected to this project");

    let m = no_connection_message(Some("term1"), Some(Some(41011)), Some(46793));
    assert!(m.contains("term1") && m.contains("41011") && m.contains("46793"), "got: {m}");
    assert!(m.contains("predates this roost"), "got: {m}");
    assert!(m.contains("start a new terminal") && m.contains("restart claude"), "got: {m}");
    assert!(!m.contains("/ide"), "the /ide repair is unmeasured and must not be advised: {m}");

    let same = no_connection_message(Some("term1"), Some(Some(46793)), Some(46793));
    let unknown = no_connection_message(Some("term1"), Some(None), Some(46793));
    assert_eq!(same, unknown, "roost cannot tell these apart in a way the user could act on");
    assert!(same.contains("is not connected to roost"), "got: {same}");
    assert!(!same.contains("predates"), "a matching port is not a stale one: {same}");
}

#[test]
fn the_unchanged_row_is_pinned_so_a_fix_to_the_others_cannot_rewrite_it() {
    // The one case that must NOT change wording: no Claude anywhere. Existing
    // tests match on "no Claude", and rewriting it into a guess is the defect
    // this whole change exists to remove.
    assert_eq!(
        no_connection_message(None, None, None),
        "no Claude is connected to this project"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib ide:: -- --test-threads=1
```

Expected: FAIL to compile — `cannot find function \`no_connection_message\``.

- [ ] **Step 3: Write the implementation**

Add `no_connection_message` as a pure function, then call it from `notify_selected`'s `total == 0` branch (`ide.rs:449`), passing `claudes::sse_port_in(&claudes::claude_terminals(Path::new("/proc")), project, session)` and `port_for(project)`. Take the scan **outside** the `conns()` lock — `notify_selected` decides everything inside one lock acquisition today, and a `/proc` walk under that lock is the "never hold a lock across blocking I/O" rule this project has already broken once.

Keep the existing multi-Claude branches (`no Claude is running in terminal "x" (N connected)`, and the "N Claudes are connected — click the terminal you mean") exactly as they are; only the `total == 0` sentence changes.

- [ ] **Step 4: Run the whole suite**

```bash
cargo test -- --test-threads=1
```

Existing tests match on the string "no Claude" — confirm which ones, and confirm they still pass rather than adjusting them.

- [ ] **Step 5: Revert-check, message-only comment**

Comment first with a placeholder, then measure. Revert: make `no_connection_message` return the old single sentence in every case. Observe which assertions fail, record the real messages. Restore, re-run green.

- [ ] **Step 6: Commit**

```bash
git add src/ide.rs
git commit -m "ide: a Claude that outlived this roost gets its own sentence"
```

---

### Task 5: restart survival, in a real browser against real dtach

**Files:**
- Modify: `tests/browser/ide.mjs`
- Test: the same file

**Interfaces:** none — nothing later depends on this.

`tests/browser/ide.mjs` is 734 lines and already drives a real roost, a real dtach-backed terminal (which is what provisions the ide listener, via `term.rs`'s `ide::for_project` call) and a real browser, with a hand-rolled fake Claude on the wire because Deno's `WebSocket` constructor cannot set the custom auth header. Read its header comment before adding to it — the section you add needs none of that fake-Claude machinery, only the terminal and the lock file.

**Why this task exists:** every test above runs against a fake `/proc` or an in-process listener. The property that actually matters — *a terminal opened before a roost restart still names the live listener afterwards* — needs a real dtach master surviving a real restart, and CLAUDE.md's substitution table records that `ROOST_CMD=cat` leaves no master to survive one. `tests/browser/claudeterm.mjs` already calls `startRoost` twice in one file, so restarting inside a test is an established pattern to copy.

- [ ] **Step 1: Write the failing test**

Read `tests/browser/ide.mjs` and `tests/browser/harness.mjs` first, then add a section following the file's own conventions:

- Start roost, open the project, create a terminal, and read the port from its dtach master's environment:
  `tr '\0' '\n' < /proc/<master pid>/environ | grep CLAUDE_CODE_SSE_PORT`
  (find the master via `ps -Ao pid,args=` matching the fixture's socket path — **never** `pkill -f`).
- Assert that port equals the port in the project's `~/.claude/ide/*.lock` — i.e. the setup state, before negating anything.
- `await roost.close()`, then `startRoost` again on a **new** HTTP port with the same `stateDir` and `roots`, and re-open the page so the listener is rebuilt.
- Assert the new lock file names the **same** IDE port as before, and that the surviving dtach master's baked `CLAUDE_CODE_SSE_PORT` therefore still matches the live listener.

Assert the number, not merely that a lock file exists. A test satisfied by any port is exactly the vacuous shape the previous branch found twice in its own plan.

- [ ] **Step 2: Confirm the binary under test is from this checkout**

```bash
grep -o '/home/[^"]*static' $(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/build/roost-*/out/assets_table.rs | head -1
```

Must print a path under `/home/claude/projects/roost`. Then `cargo build`.

- [ ] **Step 3: Run it**

```bash
deno run -A tests/browser/ide.mjs
```

Expected: all ok. A `SKIP: no chromium found` line is not a pass — Chromium is at `/snap/bin/chromium` on this host.

- [ ] **Step 4: Revert-check**

Revert Task 2 (bind `0` unconditionally in `start_in_with_ports`), `cargo build`, re-run. The new section MUST fail on the "same IDE port after restart" assertion. Record the real FAIL line. Restore with `cp` — **never `git checkout`** — rebuild, re-run green.

**If it passes under the revert, stop and report BLOCKED.** The section would not be exercising the mechanism.

- [ ] **Step 5: Full suite and browser sweep**

```bash
time cargo test -- --test-threads=1
deno run -A tests/browser/ide.mjs
```

`tests/browser/README.md` records that these flake under contention — re-run a failing file alone before calling it a regression, and say in the report which runs you did.

- [ ] **Step 6: Commit**

```bash
git add tests/browser/ide.mjs
git commit -m "test: a terminal that outlives a restart still names the live IDE port"
```

---

## What this plan deliberately does not do

- **It does not advise `/ide`.** Unmeasured; see the decisions section.
- **It does not add a button to the banner**, or a new overview glyph.
- **It does not make a running Claude reconnect.** Nothing roost can do makes the CLI redial; that is why the message is the primary fix.
- **It does not touch the token, the Origin rules, or `ide.rs`'s inverted handshake.** CVE-2025-52882 is what that inversion is for; a stable port changes which number is bound and nothing about who may connect.
- **It does not change `CLAUDE_CODE_SSE_PORT`'s role.** The port shortcut stays; it is made durable instead. Probe B showed the workspace-path fallback already covers a fresh Claude.

## Self-review notes

- **Spec coverage.** Primary fix (the three-way message) → Tasks 3 and 4. Secondary fix (stable port: where, how, rule) → Tasks 1 and 2. Testing section: "port record round-trips and a taken port falls back and rewrites the record" → Tasks 1 and 2; "restart survival needs the real-dtach harness" → Task 5; "the message is a pure function… including the row that must not change" → Task 4's two tests. "What this is not" → the section above.
- **Open at the end of this plan:** the spec's question 1 (should the banner offer the action) and question 3 (an overview glyph for running-but-disconnected) are both declined here and remain open. Question 4 (bounding the port record) is declined as not worth a sweep. The `/ide` measurement is a real follow-up if anyone wants that sentence.
