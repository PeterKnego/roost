# Close Project ends the session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Close Project (and every other path through `registry::kill_and_unlink`) end the whole terminal session, so a child that handles SIGHUP — Claude Code, a `trap "" HUP` helper — cannot survive it, and so the count roost reports is true.

**Architecture:** A new small module `src/procsess.rs` answers one `/proc` question — "which session is this pid in, and who else is in it" — mirroring the existing single-question `/proc` readers `idesess.rs` and `idecwd.rs`. `registry::kill_and_unlink_with` derives the slave-side session(s) from the socket holders *before* it kills them, kills the holders as it does today, then sweeps those sessions, and confirms on both before unlinking.

**Tech Stack:** Rust, no async, no new dependencies. `std::fs` for `/proc`, `std::process::Command` for `kill(1)` (matching the existing code). `tempfile` for fake-`/proc` fixtures (already a dev-dependency). Deno + Chromium for the browser test.

**Spec:** `docs/superpowers/specs/2026-09-04-close-project-ends-the-session-design.md`

## Global Constraints

- **`cargo test`, never `cargo test --release`.** Run it as `cargo test -- --test-threads=1`: a bare `cargo test` hangs on this project (lib + integration tests in parallel).
- **Build from one checkout.** This host points every cargo workspace at a shared `target-dir` and `build.rs` bakes absolute asset paths into a generated table. Do not build this repo from a second checkout or a git worktree — work in `/home/claude/projects/roost` directly.
- **Absence of evidence is not evidence of absence.** Every `/proc` read has three outcomes: read cleanly, `ENOENT` (gone), and *could not read* (unknown). Unknown must never be folded into "gone" on a path that gates a kill or a confirmation. This is the constraint the whole plan exists to honour; `src/idesess.rs`'s `Sess` enum is the in-repo model to copy.
- **Never hold a lock across blocking I/O.** All the work here runs in `registry`, which holds no lock; keep it that way.
- **No panics may escape a socket or watcher thread.** `kill_and_unlink` is reached from `session::kill_project` (a websocket intent) and from `registry::reconcile` (a background sweep). No `unwrap()` on anything derived from `/proc` or from a subprocess.
- **Style:** module-level `//!` doc explaining *why* the module exists; implementation first, `#[cfg(test)] mod tests` at the bottom of the same file; comments give rationale, not mechanics.
- **Test comment convention:** every test in this repo that was revert-checked carries a `// Revert-checked: <what was broken> fails here — test panicked with <the actual message>` comment. You must produce that message by actually running the reverted code, never by predicting it.

## Decisions taken from the spec's open questions

Two of the spec's open questions are settled here so the tasks are unambiguous. Both are flagged for the reviewer to overturn.

1. **SIGKILL only; no SIGTERM grace period.** The holders are already `kill -9`'d today, Close Project is an explicit destructive gesture, and a grace period adds latency to a path that already polls up to ~500ms per session plus a second thing that can hang. Claude Code writes its transcript incrementally, so little is lost. *Reviewer: overturning this adds one step to Task 4 and a delay constant.*
2. **The sweep lives inside `kill_and_unlink_with`, so `end_session` and `reconcile` inherit it.** All three are "end this session" operations and the spec's point is that ending a session means ending the session. The shared implementation is what the existing doc comment says prevents the two call sites drifting; adding a "shallow" mode would reintroduce exactly that drift. *Reviewer: overturning this means a parameter, not a second function.*

## File Structure

| File | Responsibility |
|---|---|
| `src/procsess.rs` *(new)* | One `/proc` question: the session a pid belongs to, that session's members, a pid's children, and which sessions a set of socket holders leads. No killing, no policy. |
| `src/lib.rs` *(modify, line 22)* | Register the new module. |
| `src/registry.rs` *(modify, `kill_and_unlink_with` at :320, its public wrapper at :377, its `reconcile_with` call site at :846)* | Derive targets before the kill, sweep them after, confirm on both, keep the "leave it in place on any doubt" behaviour. |
| `tests/integration.rs` *(modify)* | The real-`dtach` test: a HUP-ignoring child must not survive. This is the one the unit tests cannot substitute for. |
| `tests/browser/closeproject.mjs` *(modify)* | Section G: the same thing end-to-end through the UI, plus the `ended` count roost reports. |

---

### Task 1: `procsess::session_of` — read a pid's session id safely

**Files:**
- Create: `src/procsess.rs`
- Modify: `src/lib.rs` (add `pub mod procsess;` in alphabetical order, between `pub mod paste;` and `pub mod projects;`)
- Test: `src/procsess.rs` (`#[cfg(test)] mod tests` at the bottom, per house style)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum Sid { In(u32), Gone, Unknown }` and `pub fn session_of(proc_root: &std::path::Path, pid: u32) -> Sid`. Tasks 2 and 3 depend on both.

**Why this is its own task:** the `/proc/<pid>/stat` parse has a trap that has bitten this kind of code everywhere — field 2 is `comm`, which is parenthesised and may contain spaces *and* parentheses, so any whitespace-indexed field access is wrong for a process named `my prog` or `foo)bar`. It deserves its own gate.

- [ ] **Step 1: Write the failing test**

Create `src/procsess.rs` with only the test module and the two item stubs it needs to compile (`Sid`, and a `session_of` that returns `Sid::Unknown`), so the test fails on the assertion rather than on a compile error.

```rust
//! Which session a process belongs to, and who else is in it.
//!
//! `registry::kill_and_unlink` kills whatever holds a session's dtach socket.
//! That reaches the dtach master and nothing else: dtach `setsid`s the slave
//! side, so the user's shell leads a *different* session, and the master's
//! death arrives there only as a `SIGHUP`. Anything that handles the hangup —
//! Claude Code does — survives, reparents to init, and becomes unreachable.
//! This module is how the sweep finds it: the session id survives reparenting,
//! which is the one property a process tree does not have.
//!
//! Three outcomes everywhere, never two. "I could not read this" is not "this
//! is gone", and folding them together on a path that gates a kill or a
//! confirmation is the mistake `CLAUDE.md`'s table catalogues eleven times.
//! `idesess.rs` is the same shape for the neighbouring question.
use std::path::Path;

/// The session a pid is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sid {
    /// Read and parsed cleanly.
    In(u32),
    /// `ENOENT` — the process exited. Evidence, not a gap.
    Gone,
    /// Could not read, or could not parse. Never folded into `Gone`.
    Unknown,
}

pub fn session_of(_proc_root: &Path, _pid: u32) -> Sid {
    Sid::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one fake `/proc/<pid>/stat`. `comm` is inserted verbatim between
    /// the parentheses, so a test can hand it a name containing spaces and
    /// parentheses — which is the whole hazard this parse exists for.
    fn stat(dir: &Path, pid: u32, comm: &str, ppid: u32, pgrp: u32, sid: u32) {
        let p = dir.join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        // pid (comm) state ppid pgrp session tty_nr … — the fields after
        // `session` are padding; nothing here reads them.
        std::fs::write(
            p.join("stat"),
            format!("{pid} ({comm}) S {ppid} {pgrp} {sid} 34816 1 4194304 100 0 0\n"),
        )
        .unwrap();
    }

    #[test]
    fn reads_the_session_id() {
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
        assert_eq!(session_of(d.path(), 1601267), Sid::In(1601267));
    }

    #[test]
    fn a_comm_containing_spaces_and_parens_does_not_shift_the_fields() {
        // The reason this parse splits on the LAST ')' rather than counting
        // whitespace: both of these are legal process names.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 42, "my prog", 1, 42, 99);
        stat(d.path(), 43, "foo) bar (baz", 1, 43, 77);
        assert_eq!(session_of(d.path(), 42), Sid::In(99));
        assert_eq!(session_of(d.path(), 43), Sid::In(77));
    }

    #[test]
    fn a_missing_pid_is_gone_not_unknown() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_of(d.path(), 12345), Sid::Gone);
    }

    #[test]
    fn an_unparseable_stat_is_unknown_not_gone() {
        // The distinction that matters: `Gone` would let a sweep conclude the
        // session is empty and unlink a socket whose shell is still running.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("7");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("stat"), "not a stat line at all\n").unwrap();
        assert_eq!(session_of(d.path(), 7), Sid::Unknown);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: `reads_the_session_id`, `a_comm_containing_spaces_and_parens_does_not_shift_the_fields` and `a_missing_pid_is_gone_not_unknown` FAIL with `assertion \`left == right\` failed: left: Unknown, right: In(...)` / `right: Gone`. `an_unparseable_stat_is_unknown_not_gone` passes vacuously against the stub — that is expected and is why it is not the only test.

- [ ] **Step 3: Write the implementation**

Replace the `session_of` stub:

```rust
/// `/proc/<pid>/stat`'s session id (field 6).
///
/// Split on the **last** `)`, never on whitespace and never on the first `)`:
/// field 2 is `comm`, which the kernel prints in parentheses without escaping,
/// so a process named `foo) bar (baz` puts both characters inside it. Every
/// field this function wants comes after the whole of `comm`, so the last `)`
/// is the only reliable anchor. After it: state, ppid, pgrp, session.
pub fn session_of(proc_root: &Path, pid: u32) -> Sid {
    let raw = match std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(s) => s,
        // The two outcomes that must stay apart: the process exited, versus
        // roost could not look. Only the first is evidence.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Sid::Gone,
        Err(_) => return Sid::Unknown,
    };
    let Some((_, tail)) = raw.rsplit_once(')') else { return Sid::Unknown };
    match tail.split_whitespace().nth(3).and_then(|f| f.parse().ok()) {
        Some(sid) => Sid::In(sid),
        None => Sid::Unknown,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: 4 passed.

- [ ] **Step 5: Revert-check the parse anchor and record the real message**

Change `rsplit_once(')')` to `split_once(')')`, run
`cargo test --lib procsess -- --test-threads=1`, and copy the **actual** panic
text into a comment above `a_comm_containing_spaces_and_parens_does_not_shift_the_fields`
in the form the rest of this repo uses:

```rust
// Revert-checked: splitting on the first ')' instead of the last fails here — test panicked with <paste the real message>.
```

Then restore `rsplit_once` and re-run to confirm green. Do not write the message from memory; this repo has shipped tests that passed for the wrong reason and the recorded message is the evidence they do not.

- [ ] **Step 6: Commit**

```bash
git add src/procsess.rs src/lib.rs
git commit -m "procsess: read a pid's session id, with comm's parens accounted for"
```

---

### Task 2: `procsess::members_of` — everyone in a session

**Files:**
- Modify: `src/procsess.rs`
- Test: `src/procsess.rs` (same test module)

**Interfaces:**
- Consumes: `Sid`, `session_of` from Task 1.
- Produces: `pub fn members_of(proc_root: &std::path::Path, sid: u32) -> Option<Vec<u32>>` — `None` means *the membership could not be determined*, which callers must treat as "not empty". Task 4 depends on it.

**Why the session and not the process group:** measured 2026-09-04 and recorded in the spec. With job control on — every interactive login shell — a backgrounded job gets its own process group while staying in the shell's session, so `kill -9 -<pgid>` misses it. The session is the unit that does not.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/procsess.rs`:

```rust
#[test]
fn members_of_finds_every_pid_in_the_session_and_no_others() {
    let d = tempfile::tempdir().unwrap();
    // The measured shape: a dtach master leading its own session, the shell
    // leading a second one, and a backgrounded job in the shell's session but
    // in a process group of its own — which is what makes the process group
    // the wrong unit and the session the right one.
    stat(d.path(), 1601266, "dtach", 1, 1601266, 1601266);
    stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
    stat(d.path(), 1601290, "claude", 1601267, 1601290, 1601267);
    stat(d.path(), 999, "unrelated", 1, 999, 999);
    assert_eq!(members_of(d.path(), 1601267), Some(vec![1601267, 1601290]));
    // Asserted explicitly: an empty session and an undeterminable one are
    // different answers, and only this one means "nothing left".
    assert_eq!(members_of(d.path(), 555), Some(vec![]));
}

#[test]
fn one_unreadable_entry_makes_the_whole_membership_unknown() {
    // A sweep uses this to decide it is finished. If an entry roost could not
    // classify were skipped, an unreadable survivor would read as an empty
    // session — the socket would be unlinked and the session reported ended
    // while the shell was still running.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 100, "bash", 1, 100, 100);
    let p = d.path().join("101");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("stat"), "garbage\n").unwrap();
    assert_eq!(members_of(d.path(), 100), None);
}

#[test]
fn a_non_pid_directory_entry_is_skipped_not_fatal() {
    // /proc really contains these: `self`, `thread-self`, `sys`, `net`.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 100, "bash", 1, 100, 100);
    std::fs::create_dir_all(d.path().join("self")).unwrap();
    std::fs::write(d.path().join("uptime"), "1 2\n").unwrap();
    assert_eq!(members_of(d.path(), 100), Some(vec![100]));
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: FAIL to compile — `cannot find function \`members_of\` in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `src/procsess.rs`, above the test module:

```rust
/// Every pid in `sid`'s session, or `None` when some entry could not be
/// classified.
///
/// `None` is not "empty". This is the function a sweep asks "is the session
/// gone yet", and the answer gates unlinking a socket and reporting a session
/// ended — so a `/proc` entry roost could not read has to stop the sweep
/// concluding, not be skipped past. A pid that vanished between the `read_dir`
/// and the `stat` is a different matter: `Sid::Gone` is the outcome the sweep
/// wants, so it is dropped rather than treated as doubt.
pub fn members_of(proc_root: &Path, sid: u32) -> Option<Vec<u32>> {
    let rd = std::fs::read_dir(proc_root).ok()?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        // `/proc` holds non-pid entries (`self`, `sys`, `uptime`); they are
        // not processes and are not doubt either.
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        match session_of(proc_root, pid) {
            Sid::In(s) if s == sid => out.push(pid),
            Sid::In(_) | Sid::Gone => {}
            Sid::Unknown => return None,
        }
    }
    out.sort_unstable();
    Some(out)
}
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: 7 passed.

- [ ] **Step 5: Revert-check the `Unknown` arm and record the real message**

Change `Sid::Unknown => return None` to `Sid::Unknown => {}`, run the tests,
and paste the actual failure into a comment above
`one_unreadable_entry_makes_the_whole_membership_unknown`:

```rust
// Revert-checked: treating an unclassifiable entry as absent fails here — test panicked with <paste the real message>.
```

Restore and re-run.

- [ ] **Step 6: Commit**

```bash
git add src/procsess.rs
git commit -m "procsess: session membership, where unreadable is not empty"
```

---

### Task 3: `procsess::target_sessions` — which sessions a set of socket holders leads

**Files:**
- Modify: `src/procsess.rs`
- Test: `src/procsess.rs` (same test module)

**Interfaces:**
- Consumes: `Sid`, `session_of` from Task 1.
- Produces:
  - `pub fn children_of(proc_root: &std::path::Path, pid: u32) -> Option<Vec<u32>>`
  - `pub fn target_sessions(proc_root: &std::path::Path, holders: &[u32], own_sid: Option<u32>) -> Option<Vec<u32>>`

  Task 4 calls `target_sessions` only; `children_of` is `pub` so its own test can address it.

**The two facts this encodes, both measured on a live host 2026-09-04:**
- `/proc/<master>/task/<master>/children` names the shell — verified on dtach master 1601266, whose children file held `1601267`, a `bash` with `session == pid == 1601267`, distinct from the master's own session 1601266.
- roost's *attach client* for the same socket (pid 134273, ppid = roost) had an **empty** children file. Clients therefore contribute no targets and need no special case, even though `pids_holding` returns them alongside the master.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/procsess.rs`:

```rust
/// Writes a fake `/proc/<pid>/task/<pid>/children`.
fn children(dir: &Path, pid: u32, kids: &[u32]) {
    let p = dir.join(pid.to_string()).join("task").join(pid.to_string());
    std::fs::create_dir_all(&p).unwrap();
    let list: Vec<String> = kids.iter().map(|k| k.to_string()).collect();
    std::fs::write(p.join("children"), format!("{} ", list.join(" "))).unwrap();
}

#[test]
fn a_master_contributes_its_shells_session_and_a_client_contributes_nothing() {
    let d = tempfile::tempdir().unwrap();
    // Measured shape: master 1601266 -> shell 1601267 (its own session);
    // roost's attach client 134273 for the same socket, with no children.
    stat(d.path(), 1601266, "dtach", 1, 1601266, 1601266);
    children(d.path(), 1601266, &[1601267]);
    stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
    children(d.path(), 1601267, &[]);
    stat(d.path(), 134273, "dtach", 134227, 134273, 134227);
    children(d.path(), 134273, &[]);
    assert_eq!(
        target_sessions(d.path(), &[1601266, 134273], Some(134227)),
        Some(vec![1601267])
    );
}

#[test]
fn roosts_own_session_is_never_a_target() {
    // The guard that matters most: roost is itself a process with children,
    // and a mis-derivation here would have it kill the server answering the
    // click.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    children(d.path(), 500, &[501]);
    stat(d.path(), 501, "bash", 500, 501, 501);
    assert_eq!(target_sessions(d.path(), &[500], Some(501)), Some(vec![]));
}

#[test]
fn init_and_pid_zero_are_never_targets() {
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    children(d.path(), 500, &[1]);
    stat(d.path(), 1, "systemd", 0, 1, 1);
    assert_eq!(target_sessions(d.path(), &[500], Some(999)), Some(vec![]));
}

#[test]
fn a_child_that_is_not_a_session_leader_is_not_a_target() {
    // Only the slave side leads its own session. An ordinary child shares its
    // parent's session and killing that session would be killing the holder's,
    // which is a different and much wider thing.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    children(d.path(), 500, &[502]);
    stat(d.path(), 502, "helper", 500, 500, 500);
    assert_eq!(target_sessions(d.path(), &[500], Some(999)), Some(vec![]));
}

#[test]
fn an_unknown_own_session_refuses_every_target() {
    // Without knowing which session is ours we cannot promise not to kill it,
    // and the safe direction is to do nothing and report it.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    children(d.path(), 500, &[501]);
    stat(d.path(), 501, "bash", 500, 501, 501);
    assert_eq!(target_sessions(d.path(), &[500], None), None);
}

#[test]
fn an_unreadable_child_makes_the_whole_derivation_unknown() {
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    children(d.path(), 500, &[501]);
    let p = d.path().join("501");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("stat"), "garbage\n").unwrap();
    assert_eq!(target_sessions(d.path(), &[500], Some(999)), None);
}

#[test]
fn no_holders_is_no_targets_even_with_an_unknown_own_session() {
    // The vacuous case must stay a success: `kill_and_unlink` reaches here for
    // a socket nothing holds, and refusing it would turn "already gone" into
    // "could not determine" and leave the socket behind forever.
    let d = tempfile::tempdir().unwrap();
    assert_eq!(target_sessions(d.path(), &[], None), Some(vec![]));
}

#[test]
fn a_holder_that_already_exited_is_skipped_not_doubt() {
    // The snapshot is a moment old; a holder that died on its own in between
    // has achieved what was wanted and must not stall the sweep.
    let d = tempfile::tempdir().unwrap();
    assert_eq!(target_sessions(d.path(), &[404], Some(999)), Some(vec![]));
}

#[test]
fn a_missing_children_file_is_unknown_not_childless() {
    // Some kernels build without CONFIG_PROC_CHILDREN. "No file" there means
    // roost cannot see the shell at all, not that there is no shell.
    let d = tempfile::tempdir().unwrap();
    stat(d.path(), 500, "dtach", 1, 500, 500);
    assert_eq!(children_of(d.path(), 500), None);
    assert_eq!(target_sessions(d.path(), &[500], Some(999)), None);
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: FAIL to compile — `cannot find function \`children_of\``, `cannot find function \`target_sessions\``.

- [ ] **Step 3: Write the implementation**

Add to `src/procsess.rs`, above the test module:

```rust
/// A pid's direct children, or `None` when the list could not be read.
///
/// `None` covers a kernel built without `CONFIG_PROC_CHILDREN` as well as a
/// process that exited. Both mean roost cannot see what this holder was
/// parenting, which is not the same as it having parented nothing — and the
/// caller must not proceed to kill on that.
pub fn children_of(proc_root: &Path, pid: u32) -> Option<Vec<u32>> {
    let p = proc_root.join(pid.to_string()).join("task").join(pid.to_string()).join("children");
    let raw = std::fs::read_to_string(p).ok()?;
    Some(raw.split_whitespace().filter_map(|w| w.parse().ok()).collect())
}

/// The sessions to sweep for a set of socket holders, or `None` when they
/// could not be determined.
///
/// **Must be called before the holders are killed.** Once a dtach master dies
/// its children reparent to init and the `children` file this reads is gone;
/// deriving the target afterwards is deriving it from nothing.
///
/// A target is a holder's direct child that *leads its own session* — dtach
/// `setsid`s the slave side, so the shell is a session leader in a session the
/// master is not in. An ordinary child (same session as its parent) is not the
/// slave side and is left alone. roost's own attach clients have no children
/// at all and so contribute nothing, which is why they need no special case
/// even though `pids_holding` returns them.
///
/// `own_sid` is roost's own session. `None` means roost could not establish
/// it, and then there is no target this function is willing to name: it cannot
/// promise the sweep would not kill the server answering the click.
pub fn target_sessions(
    proc_root: &Path,
    holders: &[u32],
    own_sid: Option<u32>,
) -> Option<Vec<u32>> {
    // Nothing held the socket, so there is nothing to derive and nothing this
    // function could name to kill. Checked before `own_sid`, because refusing
    // the vacuous case would make every kill fail whenever roost could not
    // read its own `/proc` entry — including the ordinary "the session was
    // already gone" path, which must stay a success.
    if holders.is_empty() {
        return Some(Vec::new());
    }
    let own = own_sid?;
    let mut out = Vec::new();
    for holder in holders {
        let holder_sid = match session_of(proc_root, *holder) {
            Sid::In(s) => s,
            // Died between the snapshot and now: nothing to derive, and not a
            // reason to abandon the other holders.
            Sid::Gone => continue,
            Sid::Unknown => return None,
        };
        let Some(kids) = children_of(proc_root, *holder) else { return None };
        for kid in kids {
            match session_of(proc_root, kid) {
                Sid::In(s)
                    // A session leader (`s == kid`), in a session that is
                    // neither the holder's nor ours, and not init's.
                    if s == kid && s != holder_sid && s != own && s > 1 =>
                {
                    out.push(s)
                }
                Sid::In(_) | Sid::Gone => {}
                Sid::Unknown => return None,
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test --lib procsess -- --test-threads=1
```

Expected: 16 passed.

- [ ] **Step 5: Revert-check the two guards that matter and record the real messages**

Two separate reverts, each run on its own:

1. Drop `&& s != own` from the match guard. Run
   `cargo test --lib procsess -- --test-threads=1` and paste the actual
   failure above `roosts_own_session_is_never_a_target`.
2. Change `let own = own_sid?;` to `let own = own_sid.unwrap_or(0);`. Run
   again and paste the actual failure above
   `an_unknown_own_session_refuses_every_target`.

Restore after each and confirm green.

- [ ] **Step 6: Commit**

```bash
git add src/procsess.rs
git commit -m "procsess: derive the slave-side sessions a socket's holders lead"
```

---

### Task 4: `kill_and_unlink` ends the session, not just the master

**Files:**
- Modify: `src/registry.rs` — `kill_and_unlink_with` (:320), its public wrapper `kill_and_unlink` (:377), and its call site inside `reconcile_with` (:846). Update the existing test at :2138 for the new signature.
- Test: `src/registry.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `procsess::{session_of, members_of, target_sessions, Sid}` from Tasks 1–3.
- Produces: `fn kill_and_unlink_with(sock_path: &Path, snapshot_fn: SnapshotFn, proc_root: &Path) -> bool` (private, new third parameter) and the unchanged public `pub fn kill_and_unlink(sock_path: &Path) -> bool`. `session::end_session` and `session::kill_project` call the public wrapper and need no change.

**Contract change:** `true` currently means *the socket is free*. It now means *the session is gone*. `session::kill_project`'s `ended` counter is what the UI reports, so this is the half of the defect that makes the number honest.

- [ ] **Step 1: Write the failing test**

Add to `registry.rs`'s test module:

```rust
/// A `/proc` fixture: a dtach master holding `sock`, its shell leading its
/// own session, and a HUP-ignoring child in that session but in a process
/// group of its own — the measured shape of the survivor this exists for.
fn proc_with_a_session(dir: &std::path::Path) {
    let stat = |pid: u32, comm: &str, ppid: u32, pgrp: u32, sid: u32| {
        let p = dir.join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("stat"), format!("{pid} ({comm}) S {ppid} {pgrp} {sid} 0 1 0 0 0 0\n")).unwrap();
    };
    let children = |pid: u32, kids: &str| {
        let p = dir.join(pid.to_string()).join("task").join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("children"), format!("{kids} ")).unwrap();
    };
    // Pids above `/proc/sys/kernel/pid_max` (4194304 here), so the `kill -9`
    // this test drives can never reach a real process on the host. A fixture
    // using plausible pids would SIGKILL whatever happened to own them.
    stat(9000700, "dtach", 1, 9000700, 9000700);
    children(9000700, "9000701");
    stat(9000701, "bash", 9000700, 9000701, 9000701);
    children(9000701, "9000702");
    stat(9000702, "claude", 9000701, 9000702, 9000701); // own pgrp, shell's session
    children(9000702, "");
    // roost itself, so `own_sid` resolves and the guard is exercised rather
    // than short-circuited.
    stat(std::process::id(), "roost", 1, std::process::id(), std::process::id());
    children(std::process::id(), "");
}

#[test]
fn a_session_member_left_alive_is_not_confirmed_and_the_socket_stays() {
    // The defect in one assertion: the socket's holder is gone, so the old
    // confirmation ("does anything hold the socket") says yes, finished —
    // while pid 702 is still in the shell's session. Reporting that as ended
    // is what tells the user a number that is too large.
    let d = tempfile::tempdir().unwrap();
    let procd = tempfile::tempdir().unwrap();
    proc_with_a_session(procd.path());
    let sock = d.path().join("term");
    std::fs::write(&sock, b"").unwrap();
    // No process holds the socket path, so the holder kill is a no-op and the
    // fixture's session is left standing — exactly the post-kill state.
    let ok = kill_and_unlink_with(&sock, &|| Some(vec![(9000700, format!("dtach -A {} -E", sock.display()))]), procd.path());
    assert!(!ok, "a session with a live member must not be reported as ended");
    assert!(sock.exists(), "and its socket must stay, so the session stays discoverable");
}

#[test]
fn an_undeterminable_session_leaves_the_socket_in_place() {
    // `target_sessions` returning None must reach the same "leave it" path as
    // an unverifiable `ps`, not be treated as "no sessions to sweep".
    let d = tempfile::tempdir().unwrap();
    // Empty: neither the holder's children nor roost's own session can be
    // read from it, which is the "cannot tell" state this asserts on.
    let procd = tempfile::tempdir().unwrap();
    let sock = d.path().join("term");
    std::fs::write(&sock, b"").unwrap();
    let ok = kill_and_unlink_with(&sock, &|| Some(vec![(9000700, format!("dtach -A {} -E", sock.display()))]), procd.path());
    assert!(!ok, "an underivable session is doubt, not an empty sweep");
    assert!(sock.exists());
}

#[test]
fn nothing_holding_and_no_session_still_unlinks() {
    // The vacuous case must stay vacuous: a socket nobody holds, with no
    // session behind it, is cleaned up and reported as ended.
    let d = tempfile::tempdir().unwrap();
    let procd = tempfile::tempdir().unwrap();
    let sock = d.path().join("term");
    std::fs::write(&sock, b"").unwrap();
    let ok = kill_and_unlink_with(&sock, &|| Some(vec![]), procd.path());
    assert!(ok, "no holder and no session is ended, not doubt");
    assert!(!sock.exists());
}
```

Update the existing test at `registry.rs:2138` for the new signature:

```rust
let ok = kill_and_unlink_with(&sock_path, &|| None, std::path::Path::new("/proc"));
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --lib registry -- --test-threads=1
```

Expected: FAIL to compile — `this function takes 2 arguments but 3 arguments were supplied`.

- [ ] **Step 3: Write the implementation**

Replace `kill_and_unlink_with`'s body and update its two callers. Extend the existing doc comment rather than replacing it — the `None`-snapshot paragraph is still true and still load-bearing:

```rust
/// … (keep the existing doc comment, and add:)
///
/// **Ends the session, not merely the socket's holders.** Killing the dtach
/// master closes the pty, and the kernel turns that into a `SIGHUP` for the
/// slave side — which anything that handles the hangup simply declines.
/// Measured 2026-09-04: a `trap "" HUP` child survived exactly this and
/// reparented to init, while the socket went unheld, so the old confirmation
/// read "finished". Claude Code is that shape, so Close Project routinely left
/// one running and reported it ended.
///
/// The sessions to sweep are derived *before* the holders are killed: once the
/// master dies its children reparent and the link is gone. Anything that left
/// the session on purpose — Claude's `daemon run` and `bg-pty-host` both do,
/// measured — is out of scope by construction, and must stay that way: the
/// daemon is shared across projects, so following it would end other projects'
/// Claudes.
fn kill_and_unlink_with(
    sock_path: &std::path::Path,
    snapshot_fn: SnapshotFn,
    proc_root: &std::path::Path,
) -> bool {
    let Some(snapshot) = snapshot_fn() else {
        eprintln!(
            "roost: could not verify what holds {} (process listing unavailable) — leaving it in place",
            sock_path.display()
        );
        return false;
    };
    let pids = pids_holding(&snapshot, sock_path);

    // Derived before any kill, for the reason in the doc comment above.
    let own = match crate::procsess::session_of(proc_root, std::process::id()) {
        crate::procsess::Sid::In(s) => Some(s),
        _ => None,
    };
    let Some(targets) = crate::procsess::target_sessions(proc_root, &pids, own) else {
        eprintln!(
            "roost: could not determine which session holds {} — leaving it in place",
            sock_path.display()
        );
        return false;
    };

    kill_pids(&pids);
    kill_sessions(proc_root, &targets);

    // Re-killed on every pass, not only re-checked: a process forked by the
    // shell between the derivation and the kill would otherwise be seen
    // forever and never signalled, and the loop would time out on it.
    let mut still = session_or_socket_alive(sock_path, snapshot_fn, proc_root, &targets);
    for _ in 0..20 {
        if !still {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        kill_sessions(proc_root, &targets);
        still = session_or_socket_alive(sock_path, snapshot_fn, proc_root, &targets);
    }
    if still {
        return false;
    }
    let _ = std::fs::remove_file(sock_path);
    true
}

/// `kill -9` for a set of pids, in one invocation.
///
/// pid 0 is refused outright: `kill -9 0` signals this process's whole group,
/// i.e. roost itself. It should be unreachable — the kernel's swapper cannot
/// hold a socket and cannot lead a session — but an impossibility that costs
/// one comparison to make impossible is worth the comparison. stderr is
/// silenced because a pid that exited on its own in between has already
/// achieved what was wanted, and "No such process" noise masks real output.
fn kill_pids(pids: &[u32]) {
    let args: Vec<String> = pids.iter().filter(|p| **p != 0).map(|p| p.to_string()).collect();
    if args.is_empty() {
        return;
    }
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .args(&args)
        .stderr(std::process::Stdio::null())
        .status();
}

/// `kill -9` every member of each target session, never roost itself.
fn kill_sessions(proc_root: &std::path::Path, targets: &[u32]) {
    for sid in targets {
        // A membership roost could not determine is not an empty one, and
        // there is nothing safe to kill from it. The confirmation below sees
        // the same `None` and refuses to finish, which is the outcome wanted.
        let Some(members) = crate::procsess::members_of(proc_root, *sid) else { continue };
        let me = std::process::id();
        let victims: Vec<u32> = members.into_iter().filter(|p| *p != me).collect();
        kill_pids(&victims);
    }
}

/// True while anything holds the socket **or** any target session still has a
/// member, or either question could not be answered. Both halves must be
/// clear before a socket is unlinked and a session reported ended.
fn session_or_socket_alive(
    sock_path: &std::path::Path,
    snapshot_fn: SnapshotFn,
    proc_root: &std::path::Path,
    targets: &[u32],
) -> bool {
    if socket_has_process_with(sock_path, snapshot_fn) {
        return true;
    }
    targets.iter().any(|sid| match crate::procsess::members_of(proc_root, *sid) {
        // Unknown counts as alive: this gates destruction and a report.
        None => true,
        Some(m) => !m.is_empty(),
    })
}
```

Update the public wrapper:

```rust
pub fn kill_and_unlink(sock_path: &std::path::Path) -> bool {
    kill_and_unlink_with(sock_path, &process_snapshot, std::path::Path::new("/proc"))
}
```

And the call site inside `reconcile_with` (:846):

```rust
if kill_and_unlink_with(&sock.path(), snapshot_fn, std::path::Path::new("/proc")) {
```

`reconcile`'s own tests use fake sockets with no holders, so `pids_holding`
returns empty, `target_sessions` returns `Some(vec![])`, and the sweep is a
no-op — they need no change. Confirm that rather than assume it in Step 4.

- [ ] **Step 4: Run the whole suite**

```bash
cargo test -- --test-threads=1
```

Expected: all pass, including every pre-existing `registry` and `session` test. If a `reconcile` test now fails, that is real information about the no-op assumption above — investigate it rather than adjusting the test.

- [ ] **Step 5: Revert-check the sweep and record the real message**

Change `session_or_socket_alive` to `socket_has_process_with(sock_path, snapshot_fn)` alone (the pre-fix confirmation), run `cargo test --lib registry -- --test-threads=1`, and paste the actual failure above `a_session_member_left_alive_is_not_confirmed_and_the_socket_stays`. Restore and re-run.

- [ ] **Step 6: Commit**

```bash
git add src/registry.rs
git commit -m "registry: kill_and_unlink ends the session, not just the socket's holders"
```

---

### Task 5: The real-dtach test — the one the unit tests cannot substitute for

**Files:**
- Modify: `tests/integration.rs`
- Test: the same file

**Interfaces:**
- Consumes: `roost::registry::kill_and_unlink` (public), `roost::session` for the socket path shape.
- Produces: nothing other tasks use.

**Why this task exists:** CLAUDE.md's dev/prod substitution table records `ROOST_CMD=cat` hiding a defect that would have killed every terminal in production. It applies exactly here: with `cat` there is no dtach master, no pty, no hangup, and therefore no bug to observe. Every assertion in Tasks 1–4 is against a fake `/proc`. This is the only test that runs the real mechanism.

- [ ] **Step 1: Write the failing test**

Add to `tests/integration.rs`:

```rust
/// A child that declines the hangup must not survive `kill_and_unlink`.
///
/// This is the measured defect (2026-09-04): `kill -9` on the dtach master
/// closes the pty, the kernel `SIGHUP`s the slave side, the foreground process
/// and a plain background child die — and a `trap "" HUP` child does not. It
/// reparents to init and keeps the project directory as its cwd, while the
/// socket goes unheld and roost reports the session ended.
///
/// Deliberately a real `dtach`, not `ROOST_CMD=cat`: with `cat` there is no
/// master, no pty and no hangup, so the whole mechanism is absent and the test
/// would pass against the unfixed code.
#[test]
fn a_hup_ignoring_child_does_not_survive_kill_and_unlink() {
    if std::process::Command::new("dtach").arg("-h").output().is_err() {
        eprintln!("skipping: dtach not installed");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("term");
    // A unique name so the assertion cannot match another test's process, and
    // so a survivor is identifiable in `ps` if this fails.
    let marker = format!("roost_hup_survivor_{}", std::process::id());
    let script = d.path().join("inner.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nbash -c 'trap \"\" HUP; exec -a {marker} sleep 600' &\nexec sleep 600\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ok = std::process::Command::new("dtach")
        .args(["-n", sock.to_str().unwrap(), "-E", "-r", "winch", "-z"])
        .arg(&script)
        .status()
        .unwrap()
        .success();
    assert!(ok, "dtach -n must create the session this test is about");
    std::thread::sleep(std::time::Duration::from_millis(800));

    let alive = |m: &str| {
        let out = std::process::Command::new("ps").args(["-Ao", "args="]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(m))
    };
    // Asserts the setup state it later negates: without this the test would
    // pass just as well if the child had never started.
    assert!(alive(&marker), "the HUP-ignoring child must be running before the kill");

    let confirmed = roost::registry::kill_and_unlink(&sock);
    std::thread::sleep(std::time::Duration::from_millis(500));

    assert!(!alive(&marker), "the HUP-ignoring child survived kill_and_unlink");
    assert!(confirmed, "and the session must be reported as confirmed ended");
    assert!(!sock.exists(), "and its socket unlinked");
}
```

- [ ] **Step 2: Run to verify it fails against the pre-fix behaviour**

Temporarily restore the old confirmation and no sweep — the simplest faithful
revert is to make `kill_and_unlink_with` skip the sweep by replacing the
`kill_sessions(proc_root, &targets);` calls and
`session_or_socket_alive(...)` with `socket_has_process_with(sock_path, snapshot_fn)`:

```bash
cargo test --test integration a_hup_ignoring_child -- --test-threads=1
```

Expected: FAIL on `the HUP-ignoring child survived kill_and_unlink`. Record the
actual message. Then restore the fix.

**If it passes against the reverted code, stop.** The fixture is not reaching
the state the bug needs, which is the exact failure mode CLAUDE.md records for
the worktree self-parenting test.

- [ ] **Step 3: Run against the fixed code**

```bash
cargo test --test integration a_hup_ignoring_child -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 4: Add the revert-checked comment with the real message**

```rust
// Revert-checked: with the session sweep removed this fails — test panicked with <paste the message from Step 2>.
```

- [ ] **Step 5: Run the whole suite and time it**

```bash
time cargo test -- --test-threads=1
```

A deadlock hangs rather than fails, so the elapsed time is part of the result. Compare against a run on `master` before this branch; a large increase is a finding, not noise.

- [ ] **Step 6: Commit**

```bash
git add tests/integration.rs
git commit -m "test: a HUP-ignoring child must not survive kill_and_unlink, with real dtach"
```

---

### Task 6: Section G in the browser test — the whole gesture, and the number it reports

**Files:**
- Modify: `tests/browser/closeproject.mjs` (add Section G before the `} finally {` block; extend the `//!` header's section list at the top)
- Test: the same file

**Interfaces:**
- Consumes: the existing `fx`, `roost`, `ws`, `ok()`, `sockets()` helpers already defined in that file.
- Produces: nothing other tasks use.

**Why:** Tasks 4 and 5 prove the helper. This proves the *feature*: a user clicking Close Project on a project with a Claude-shaped process in it. It is also the only place the reported count is visible, and no Rust test reaches `static/app.js`.

- [ ] **Step 1: Write the failing test**

Add before the `} finally {` block in `tests/browser/closeproject.mjs`:

```js
  console.log("G. a child that ignores SIGHUP does not survive the close");
  // A fresh project, because Sections A-F have already closed the fixture's.
  const g = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => g.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await until(async () => (await sockets()).includes("term"), 30, "the default terminal");

  // Typed over the terminal socket rather than the xterm: input on that socket
  // is the raw bytes to type (`term.rs` writes a Binary frame straight to the
  // pty), and the page supplies the Origin the handshake requires.
  const marker = `roost_hup_survivor_${Date.now()}`;
  await g.evalIn(`
    new Promise((res) => {
      const w = new WebSocket("ws://127.0.0.1:${roost.port}/ws/${fx.project}/term/term");
      w.onopen = () => {
        w.send(new TextEncoder().encode(
          "bash -c 'trap \\"\\" HUP; exec -a ${marker} sleep 600' &\\n"));
        setTimeout(() => { w.close(); res("sent"); }, 1500);
      };
      w.onerror = () => res("error");
      setTimeout(() => res("timeout"), 8000);
    })
  `);
  await sleep(1500);

  const running = async () => {
    const out = await new Deno.Command("ps", { args: ["-Ao", "args="], stdout: "piped" }).output();
    return new TextDecoder().decode(out.stdout).split("\n").filter((l) => l.includes(marker)).length;
  };
  // Asserts the setup state it later negates: without this, a child that never
  // started would make the assertion below pass for the wrong reason.
  ok((await running()) > 0, "the HUP-ignoring child is running before the close");

  await g.evalIn(`send({ t: "CloseProject" })`);
  await sleep(4000);
  ok((await running()) === 0, `no HUP-ignoring child survived the close: ${await running()} left`);
  ok((await sockets()).length === 0, `and no socket was left behind: ${JSON.stringify(await sockets())}`);
  try { g.close(); } catch { /* already gone */ }
```

Extend the `//!` header's section list at the top of the file with one line describing Section G, matching the style of the existing entries.

- [ ] **Step 2: Run it against the fixed code**

```bash
deno run -A tests/browser/closeproject.mjs
```

Expected: all sections ok, including the two new G lines. If no browser is present the harness skips; that is not a pass — find a host with Chromium (the deploy host has one).

- [ ] **Step 3: Revert-check it**

Remove the session sweep from `registry.rs` again (as in Task 5 Step 2), rebuild, and re-run:

```bash
cargo build && deno run -A tests/browser/closeproject.mjs
```

Expected: `FAIL  no HUP-ignoring child survived the close: 1 left`. Record the real line. Restore the fix, rebuild, re-run green.

Note the browser-test caveat in `tests/browser/README.md`: contention makes a back-to-back sweep unreliable, so a single failure here after a full-suite run is not automatically a regression — re-run it alone.

- [ ] **Step 4: Add the revert-checked comment**

Above Section G, in the file's comment style:

```js
  // Revert-checked: with the session sweep removed from `kill_and_unlink_with`
  // this section fails — <paste the real FAIL line from Step 3>.
```

- [ ] **Step 5: Confirm the running binary, then the full suite**

```bash
grep -o '/home/[^"]*static' $(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/build/roost-*/out/assets_table.rs | head -1
```

Expected: a path under `/home/claude/projects/roost`. If it names another checkout, `cargo clean -p roost` and rebuild — the shared target dir means the browser test would otherwise be exercising a different tree.

```bash
time cargo test -- --test-threads=1
```

- [ ] **Step 6: Commit**

```bash
git add tests/browser/closeproject.mjs
git commit -m "test: Close Project must not leave a HUP-ignoring child, end to end"
```

---

## What this plan deliberately does not do

- **It does not chase processes that left the session.** Claude's `daemon run`
  and `bg-pty-host` set up their own sessions (measured: sids 22465 and
  1602042, both parented to `systemd --user`), so the sweep does not reach
  them, and it must not: the daemon is shared across projects and following it
  would end other projects' Claudes. If the spec's reviewer wants them in
  scope, that is a separate design question, not a parameter.
- **It does not change what closing a *tab* does.** Detaching still leaves the
  session running; only Close Project and an explicit End Session destroy.
- **It does not touch the IDE port.** That is the companion spec,
  `2026-09-04-stable-ide-port-design.md`.
- **It does not add a SIGTERM grace period** — see the decisions section.

## Self-review notes

- **Spec coverage.** Design steps 1–5 → Task 4. "Guards" → Task 3 (own session,
  init, pid 0) and Task 4 (`kill_pids` refusing pid 0). "Parse `/proc/<pid>/stat`
  after the last `)`" → Task 1. "Re-derive at kill time" → Task 4's re-kill loop
  and `members_of` being re-read each pass. "Unknown is not empty" → Tasks 1, 2,
  3 and 4, each with its own test. Testing section → Tasks 1–6. The spec's risk
  "this kills more than it used to" is a release-note item, not a task; flag it
  when the branch is finished.
- **Open at the end of this plan:** the spec's question 4 — what the UI should
  say when a session is *not* confirmed gone. The count becomes truthful here,
  which makes the shortfall meaningful; surfacing it is a follow-up, and
  `session::end_socket`'s existing stderr line is the only report today.
