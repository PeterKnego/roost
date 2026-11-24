# Worktrees roost did not make — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A terminal follows its Claude into a worktree only when the Claude process actually moves; any new worktree gets a banner; terminal links into other projects offer to open them; a renamed project or worktree takes its live terminals with it.

**Architecture:** Approval of a `WorktreeCreate` ask stops writing the re-home record and instead leaves a 30 s in-memory expectation; the claudes watcher (3 s, off-lock) re-homes when `/proc` shows the asking Claude's cwd inside that worktree. The file watcher additionally watches `.git/worktrees` and the hub confirms new entries off-lock with `git worktree list`. A fork-free `projects::project_for_abs` maps an absolute path to the roost project containing it (used by the banner, the link offer, and rename-following). Rename-following turns #178's inode evidence into a `sock/<old key>/.moved-to` record, re-homes the old key's sessions to the new project, and widens `home::resolves` and `term.rs` only for owners a `.moved-to` names (attach-only, never spawn).

**Tech Stack:** Rust (std, existing `notify` watcher, `serde_json`), plain JS (`static/app.js`), Deno + Chromium browser tests.

**Spec:** `docs/superpowers/specs/2026-10-17-worktrees-roost-did-not-make-design.md` (issue #158; builds on #178 / PRs #179, #180). Read it before any task — its section numbers are referenced below.

## Global Constraints

- The `WorktreeCreate` hook never re-homes a terminal (spec §1, user decision 2026-10-17). `approve_ask` must not call `home::record`.
- Expectation window `EXPECT_MOVE_WINDOW = 30 s`; at most one expectation per `(home, session)`; in memory only (lost on restart).
- Re-homing anywhere goes through one shared function with the R16 gate (`session::live_names(owner)` must not contain the name) and refreshes hubs one lock at a time, then `broadcast_all` with no lock held.
- No `git` (`worktree::list`, `create_named`, …) under a hub lock, the hub REGISTRY lock, `SESSIONS`, or any claudes static lock. `projects::project_for_abs` is fork-free (only `symlink_metadata`), because `do_open_path` runs under the hub lock.
- A worktree banner never fires for: a worktree that existed when the project's watcher started, a worktree roost itself created, or a `worktree::list` that came back empty (git did not answer).
- A session's socket path, `SESSIONS` key and env name its home forever (CLAUDE.md). Rename-following re-homes; it never moves a socket.
- `.moved-to` is written only on #178's inode evidence (`registry::moved_not_deleted` finding the recorded `(dev, ino)` on a live shell's cwd ancestry) and only when the new path resolves as a project under the roots. Atomic, 0600.
- `home::resolves` and `term.rs` widen only for an owner equal to the content of the home key's `.moved-to`; `term.rs` then attaches to existing sessions only and refuses a spawn with "project was renamed to `<new>`".
- HTTP surface unchanged (two POSTs). All client text via `textContent`; links built with `projectPath`.
- Tests: `cargo test -- --test-threads=1` (never bare, never `--release`); every new test watched failing with its code reverted, the failure recorded in its comment; real dtach (lock order STATE then SESSION; `panic!`, not skip) wherever survival or attach is asserted.
- Build only in `/home/claude/projects/roost` (shared target dir). This checkout is shared with other actors: run `git branch --show-current` immediately before every commit.

## Review Focus

1. **A Claude that moves into the expected worktree while roost restarts** — the expectation is gone, so the user gets the "Move tab there" offer rather than nothing. Pinned in Task 4 (`an_expectation_lost_to_a_restart_falls_back_to_the_offer` — simulated by clearing the map).
2. **Two asks from one terminal in a row** (first refused or timed out, second approved) — only the latest expectation counts, never the stale one. Pinned in Task 4 (`a_newer_expectation_replaces_the_older`).
3. **`git worktree add` immediately followed by `git worktree remove`** within one debounce window — no banner for a worktree that no longer exists, and no crash. Pinned in Task 5 (`a_worktree_added_and_removed_before_confirming_is_not_announced`).
4. **A terminal link to a path under a different root, or through a symlink** (`~/projects/x` from an `~/ultima` project; `~/ultima/cur -> uc-rel`) — offered as the project where the file actually is, because the path is canonicalised before the walk. Pinned in Task 2 (`a_path_under_another_root_resolves_there`, `a_symlinked_path_resolves_to_its_target_project`).
5. **A rename while a browser has the old worktree page open** — the old page gets the `ProjectMoved` banner with a working link; its terminal tab keeps working through the attach-only widening. Pinned in Task 9 (browser case).

---

### Task 1: `moved_not_deleted` reports where the directory went

**Files:**
- Modify: `src/registry.rs` (`moved_not_deleted` and its call in `reconcile_with`; tests at the bottom)

**Interfaces:**
- Produces:
  ```rust
  /// What a live shell says about a project whose recorded path is NotFound.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub(crate) enum MoveEvidence {
      /// No evidence of a move (no identity recorded, or no shell inside it).
      None,
      /// The recorded directory is alive at this absolute path.
      Moved(std::path::PathBuf),
      /// Could not tell (unnamed shell, unreadable cwd, unreadable key dir).
      CannotTell,
  }
  pub(crate) fn move_evidence(key_dir: &Path, snapshot: &[(u32, String)], proc_root: &Path) -> MoveEvidence;
  ```
  `moved_not_deleted` becomes `!matches!(move_evidence(..), MoveEvidence::None)` (behaviour unchanged).

- [ ] **Step 1: Failing test** — extend `a_live_session_whose_project_was_moved_is_not_killed`'s fixture into a new test that calls `move_evidence` directly after the rename:

```rust
    #[test]
    fn move_evidence_names_the_new_directory() {
        with_state(|state| {
            let sock_dir = state.join("sock/ev-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("ev-proj");
            fs::create_dir_all(project_dir.join("sub")).unwrap();
            // The shell sits in a subdirectory: the evidence is the ancestor
            // that carries the recorded inode, not the cwd itself.
            spawn_detached_dtach_in(&sock_path, &project_dir.join("sub")).expect("dtach must be available");
            let _ = reconcile(&[root.path().to_path_buf()]);
            let moved_to = root.path().join("ev-renamed");
            fs::rename(&project_dir, &moved_to).unwrap();
            let snap = process_snapshot().expect("ps");
            assert_eq!(
                move_evidence(&sock_dir, &snap, std::path::Path::new("/proc")),
                MoveEvidence::Moved(moved_to.canonicalize().unwrap()),
            );
            kill_test_pids(&sock_path);
        });
    }
```

- [ ] **Step 2:** Run `cargo test --lib registry::tests::move_evidence -- --test-threads=1` → compile error (no `move_evidence`). Capture it.
- [ ] **Step 3: Implement** by refactoring the body of `moved_not_deleted` into `move_evidence`: every `return true` for "cannot tell" becomes `return MoveEvidence::CannotTell`; the inode match returns `MoveEvidence::Moved(p.to_path_buf())` where `p` is the ancestor whose `(dev, ino)` matched; no identity → `MoveEvidence::None`; end of loop → `MoveEvidence::None`. Keep `#[cfg(not(unix))]` returning `None`. `moved_not_deleted` delegates.
- [ ] **Step 4:** `cargo test --lib registry -- --test-threads=1` → all pass (the #178 tests must stay green).
- [ ] **Step 5: Revert check:** return `MoveEvidence::Moved(cwd.clone())` (the cwd, not the matching ancestor) → the new test fails with the `sub` path. Restore with `cp`; record in the test comment.
- [ ] **Step 6: Commit** — `registry: move evidence carries the directory's new path (#158)`.

---

### Task 2: `projects::project_for_abs` — which roost project contains a path (fork-free)

**Files:**
- Modify: `src/projects.rs` (new function + tests)

**Interfaces:**
- Produces:
  ```rust
  /// The roost project containing `abs`, as (project URL, path of `abs`
  /// relative to that project). Fork-free: only `symlink_metadata`.
  pub fn project_for_abs(roots: &[PathBuf], abs: &Path) -> Option<(String, String)>;
  ```

Rules (spec §3): `abs` must be under some root (compare canonicalised roots; `abs` is canonicalised by the caller or here with `canonicalize`, `None` on error). Walk ancestors of `abs` from nearest outward while still under that root; the first ancestor that has a `.git` (dir or file; `symlink_metadata` `Ok`) is the project; `NotFound` → keep walking; any other error → return `None`. If no ancestor under the root has `.git` → `None` (the caller keeps "path is outside this project"). URL = ancestor relative to its root with `/` separators. Reject a URL with a dot segment unless it is exactly `<x>/.claude/worktrees/<name>` with `crate::wtname::valid(name)` or `crate::session::valid_name(name)` (existing `claude-N`). The `rel` is `abs` relative to the ancestor (empty string for the project root itself).

- [ ] **Step 1: Failing tests**

```rust
    #[test]
    fn project_for_abs_finds_the_repo_and_the_relative_path() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("uc-rel");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join("docs/a.md"), "x").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let abs = repo.join("docs/a.md").canonicalize().unwrap();
        assert_eq!(project_for_abs(&roots, &abs), Some(("uc-rel".into(), "docs/a.md".into())));
    }

    #[test]
    fn a_worktree_under_dot_claude_resolves_and_another_dot_segment_does_not() {
        let root = tempfile::tempdir().unwrap();
        let wt = root.path().join("p/.claude/worktrees/worktree-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ...").unwrap(); // linked worktree: .git is a file
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(project_for_abs(&roots, &wt.canonicalize().unwrap()).map(|p| p.0),
                   Some("p/.claude/worktrees/worktree-x".into()));
        let odd = root.path().join("p/.hidden/q");
        std::fs::create_dir_all(odd.join(".git")).unwrap();
        assert_eq!(project_for_abs(&roots, &odd.canonicalize().unwrap()), None);
    }

    #[test]
    fn a_path_under_no_root_is_none() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert_eq!(project_for_abs(&[root.path().to_path_buf()], &other.path().canonicalize().unwrap()), None);
    }

    #[test]
    fn a_path_under_another_root_resolves_there() {
        let (r1, r2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(r2.path().join("x/.git")).unwrap();
        let roots = vec![r1.path().to_path_buf(), r2.path().to_path_buf()];
        assert_eq!(project_for_abs(&roots, &r2.path().join("x").canonicalize().unwrap()).map(|p| p.0), Some("x".into()));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_resolves_to_its_target_project() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("uc-rel/.git")).unwrap();
        std::fs::write(root.path().join("uc-rel/a.md"), "x").unwrap();
        std::os::unix::fs::symlink(root.path().join("uc-rel"), root.path().join("cur")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        // Callers pass what the terminal printed; project_for_abs canonicalises.
        assert_eq!(project_for_abs(&roots, &root.path().join("cur/a.md")),
                   Some(("uc-rel".into(), "a.md".into())));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_ancestor_is_none_not_a_guess() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        let abs = locked.join("inner").canonicalize().unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = project_for_abs(&[root.path().to_path_buf()], &abs);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if nix_is_root() { return; } // root reads through 0o000; nothing to assert
        assert_eq!(got, None);
    }
```
(Add a tiny `fn nix_is_root() -> bool { unsafe { libc::getuid() == 0 } }` in the test module.)

- [ ] **Step 2:** run → compile error. Capture.
- [ ] **Step 3: Implement** per the rules above, with a doc comment explaining why fork-free (callers under the hub lock).
- [ ] **Step 4:** run → pass.
- [ ] **Step 5: Revert check:** treat any `symlink_metadata` error as `NotFound` (keep walking) → `an_unreadable_ancestor_is_none_not_a_guess` fails. Restore; record.
- [ ] **Step 6: Commit** — `projects: project_for_abs, fork-free (#158)`.

---

### Task 3: one shared re-home function

**Files:**
- Modify: `src/hub.rs` (extract from `do_rehome`'s work closure)

**Interfaces:**
- Produces:
  ```rust
  /// Re-homes `(home, session)` to `owner`: R16 gate, `home::record`, then
  /// refresh home and owner hubs one lock at a time and `broadcast_all`.
  /// Must be called with no hub lock held. Errors are user-facing text.
  pub fn rehome_to(home: &str, session: &str, owner: &str) -> Result<(), String>;
  ```
  `do_rehome`'s work closure calls it for the `Some(to)` path after its own `worktree::list` / `owner_path` validation (unchanged), keeping its existing tests green.

- [ ] **Step 1: Failing test** (two hubs, map-session fixture as the existing Rehome tests use):

```rust
    #[test]
    fn rehome_to_refuses_a_name_already_live_in_the_owner_and_records_otherwise() {
        // Reuse the fixture of the existing Rehome R16 test (search hub.rs tests for
        // "R16" + "Rehome"); set up home "h" with live "term3", owner worktree "w".
        // 1) With "term3" also live in "w": rehome_to("h","term3","w") is Err containing
        //    "already has a terminal named term3"; record stays Here.
        // 2) Without it: Ok(()); home::owner_of("h","term3") == Elsewhere("w"); the owner
        //    hub's session_homes contains "term3" right after the call (no sleep).
    }
```
Write it concretely against that existing fixture (copy its setup verbatim into this test; do not invent a new fixture).

- [ ] **Step 2:** run → compile error. **Step 3:** extract; `do_rehome` calls it. **Step 4:** run `cargo test --lib hub -- --test-threads=1` → all pass, timed.
- [ ] **Step 5: Revert check:** drop the R16 gate inside `rehome_to` → case 1 fails. Restore; record.
- [ ] **Step 6: Commit** — `hub: rehome_to, the one re-home path (#158)`.

---

### Task 4: §1 — the terminal moves only when its Claude does

**Files:**
- Modify: `src/hub.rs` (`approve_ask`: remove the R16 gate + `home::record` block; call `crate::claudes::expect_move(&project, &session, &owner_url)` after the worktree is created, before replying)
- Modify: `src/claudes.rs` (expectation map; `offer_moves` branch; clock injection)
- Modify: existing tests that asserted approval re-homes (`approve_creates_records_and_replies_path`, `approve_refreshes_the_owner_hub_before_replying`, the R16 record-gate test, and any browser assertion relying on it) — update them to the new contract; do not delete their other assertions.

**Interfaces:**
- Produces (claudes.rs):
  ```rust
  pub const EXPECT_MOVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
  pub fn expect_move(home: &str, session: &str, worktree_url: &str);
  /// Test seam: the same, with an explicit "now".
  pub(crate) fn offer_moves_at(moves: &[Move], last: &Mutex<..>, expect: &Mutex<HashMap<(String,String),(String, std::time::Instant)>>, now: std::time::Instant, rehome: &dyn Fn(&str,&str,&str) -> Result<(),String>);
  ```
  Production `offer_moves` calls `offer_moves_at(.., Instant::now(), &crate::hub::rehome_to)`. Expired entries are removed at the start of each call.

Branch in `offer_moves_at` for `Move::Into { home, session, worktree }`: if the expectation for `(home, session)` exists, `now - at <= EXPECT_MOVE_WINDOW`, and its URL == `worktree` → call `rehome(home, session, worktree)`; on `Ok` remove the expectation and **send no offer** (and record `worktree` in `last` so the next tick does not offer either); on `Err` (R16 refused) log, **keep** the expectation (it expires on its own, spec §1), and fall through to the normal offer. Any other case → existing behaviour.

- [ ] **Step 1: Failing tests** (claudes.rs, pure — inject `rehome` as a recording closure and `now`):

```rust
    #[test]
    fn an_expected_move_rehomes_without_an_offer() {
        let last = Mutex::new(HashMap::new());
        let expect = Mutex::new(HashMap::new());
        let t0 = std::time::Instant::now();
        expect.lock().unwrap().insert(("h".into(), "term3".into()), ("h/.claude/worktrees/worktree-x".into(), t0));
        let calls = std::cell::RefCell::new(vec![]);
        let mv = [Move::Into { home: "h".into(), session: "term3".into(), worktree: "h/.claude/worktrees/worktree-x".into() }];
        offer_moves_at(&mv, &last, &expect, t0 + std::time::Duration::from_secs(3),
            &|h, s, o| { calls.borrow_mut().push((h.to_string(), s.to_string(), o.to_string())); Ok(()) });
        assert_eq!(calls.borrow().len(), 1, "re-homed once");
        assert!(expect.lock().unwrap().is_empty(), "expectation consumed");
        // and no ClaudeElsewhere offer was broadcast: assert via the `last` map
        // holding the worktree (offer suppressed) and, if offer_moves_at sends
        // through a broadcaster, inject it too and assert it was not called.
    }

    #[test]
    fn an_expectation_without_a_move_expires_and_moves_nothing() { /* no Move for (h, term3); now = t0 + 31 s → map empty, rehome never called */ }

    #[test]
    fn a_late_move_gets_the_offer_not_an_automatic_move() { /* Move::Into at t0 + 31 s → rehome not called; offer path taken */ }

    #[test]
    fn an_expected_move_into_a_different_worktree_is_offered_not_taken() { /* expectation for worktree-x, Move::Into worktree-y → rehome not called */ }

    #[test]
    fn a_newer_expectation_replaces_the_older() { /* expect_move twice for (h, term3) with x then y; Move::Into y → rehomed to y */ }

    #[test]
    fn an_expectation_lost_to_a_restart_falls_back_to_the_offer() { /* empty expect map + Move::Into → offer path */ }
```
Make the offer path observable: give `offer_moves_at` an injected `broadcast: &dyn Fn(&str, &Event)` (production: `crate::hub::broadcast_to_project`) and assert on it. Write all bodies concretely.

Plus hub.rs: `approve_no_longer_rehomes` — using the existing approve fixture, after `Path` arrives assert `home::owner_of(project, "term3") == Owner::Here` and that `claudes` holds an expectation for it (add a `#[cfg(test)] pub(crate) fn expectation_for(home, session) -> Option<String>`).

- [ ] **Step 2:** run → compile errors / failures. Capture. **Step 3:** implement. **Step 4:** run `--lib claudes` and `--lib hub`, timed → pass.
- [ ] **Step 5: Revert checks:** (a) restore `home::record` in `approve_ask` → `approve_no_longer_rehomes` fails; (b) drop the window check → `a_late_move_gets_the_offer…` fails; (c) drop the URL equality → `…different_worktree…` fails. Restore each; record.
- [ ] **Step 6: Commit** — `worktrees: a terminal moves only when its Claude does; the hook never re-homes (#158)`.

---

### Task 5: §2 — a worktree that appears from anywhere gets a banner

**Files:**
- Modify: `src/watch.rs` (`Class::Worktrees`; classify `.git/worktrees` and `.git/worktrees/<id>…` ahead of the `.git` ignore; only when `<dir>/.git` is a directory (`symlink_metadata`; a linked worktree's `.git` is a file and is never watched this way), add a NonRecursive watch on `<dir>/.git/worktrees` when it exists at start, and when an event shows `.git/worktrees` was created; in the dispatch loop set `worktrees = true` and, after the loop, call `h.worktrees_changed()` — which only spawns)
- Modify: `src/hub.rs` (`Hub.known_worktrees: Option<BTreeSet<PathBuf>>`, seeded off-lock when the watcher starts; `pub fn note_created_worktree(project, path)` used by `approve_ask` and `do_new_worktree` **before** their git add; `fn worktrees_changed(&mut self)` spawns a thread: `worktree::list(dir)` → if empty, return; diff non-main canonical paths against `known_worktrees` → for each new path broadcast `Event::WorktreeAppeared { path, branch, url }` with `url = projects::project_for_abs(&projects::roots(), &path).map(|p| p.0)`; update the set (removed paths dropped, no banner))
- Modify: `src/proto.rs` (`Event::WorktreeAppeared { path: String, branch: String, url: Option<String> }`)
- Modify: `static/app.js` (banner: "worktree `<basename>` (`<branch>`) appeared — open" linking `/${projectPath(url)}`; without `url`: "… appeared, outside roost's roots"; `textContent` only)

**Interfaces:**
- Consumes: `projects::project_for_abs` (Task 2).
- Produces: `hub::note_created_worktree(project: &str, path: &Path)`, `Event::WorktreeAppeared`.

- [ ] **Step 1: Failing tests:**
  - watch.rs: `classify(".git/worktrees/x/HEAD", ..) == Class::Worktrees` and `classify(".git/worktrees", ..) == Class::Worktrees`; `.git/objects/..` still `Ignore`.
  - hub.rs `a_plain_git_worktree_add_is_announced_once` — real git repo hub (existing `repo_with_worktree`-style fixture, two connections): seed known set (call the seeding fn synchronously in test), then `git worktree add ../sibling -b sib`, call the confirm step synchronously (`#[cfg(test)]` sync variant of the spawned work), assert exactly one `WorktreeAppeared` with `branch == "sib"` reached **both** connections; call again → no second event.
  - `a_worktree_that_existed_at_start_is_never_announced`.
  - `a_worktree_roost_made_is_not_announced` — `note_created_worktree` before the add → no event.
  - `an_unanswered_git_list_changes_nothing` — inject a `list` returning empty → no event, known set unchanged.
  - `a_worktree_added_and_removed_before_confirming_is_not_announced`.
- [ ] **Steps 2–4:** RED, implement, GREEN (`--lib watch`, `--lib hub`), timed.
- [ ] **Step 5: Revert checks:** (a) skip the seeding → "existed at start" fails; (b) treat empty list as "all gone" → "unanswered" fails. Restore; record.
- [ ] **Step 6:** `node --check static/app.js`.
- [ ] **Step 7: Commit** — `watch: a worktree that appears from anywhere is announced once (#158)`.

---

### Task 6: §3 — terminal links into another project

**Files:**
- Modify: `src/hub.rs` (`do_open_path`: on `resolve_terminal_path` Err, compute the absolute path the same way `resolve_terminal_path` does — factor its `~`/line-suffix handling into a `projects::terminal_abs(project_dir, text) -> Option<(PathBuf, Option<u32>)>` if not already separate — then `project_for_abs`; if `Some((url, rel))` and `url != self.project` → `send_to(from, Event::PathElsewhere { project: url, rel, line })`; else the existing error)
- Modify: `src/proto.rs` (`Event::PathElsewhere { project: String, rel: String, line: Option<u32> }`)
- Modify: `static/app.js` (banner "That's in `<project>` — open" → `/${projectPath(project)}?open=${encodeURIComponent(rel + (line ? ":" + line : ""))}`; read `?open=` once at load beside `?focus` — validate it is a relative path without `..` segments and length ≤ 4096 — strip it with the existing `history.replaceState`, and after the first `State` send `{ t: "OpenPath", text: open }`)

- [ ] **Step 1: Failing tests** (hub.rs): `a_terminal_link_into_another_project_offers_it` (roots with two repos; `OpenPath { text: "<abs of other repo>/a.md:7" }` → `PathElsewhere { project: "other", rel: "a.md", line: Some(7) }` to the sender only — two connections, assert the other got nothing); `a_link_under_no_project_is_still_outside` (an absolute path outside every root → the existing "path is outside this project" error).
- [ ] **Steps 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Revert check:** skip the `url != self.project` guard → a link to a missing file inside *this* project produces `PathElsewhere` to itself; add `a_missing_file_in_this_project_is_not_offered_as_elsewhere` first and watch it fail. Restore; record.
- [ ] **Step 6:** `node --check static/app.js`. **Step 7: Commit** — `hub: a terminal link into another project offers to open it (#158)`.

---

### Task 7: §5a — the move record and following

**Files:**
- Modify: `src/registry.rs` (in `reconcile_with`, where `move_evidence` returns `Moved(new_dir)`: compute `new_url` as `new_dir` relative to the root containing it (`/`-separated; `None` if under no root); if `projects::resolve_project(roots, &new_url) == Some(new_dir)` (spec §5 — the project need not be a git repo), write `sock/<old key>/.moved-to` (atomic, 0600; rewrite only if different) and, **outside** the per-key loop's critical parts, call `crate::home::follow_move(&old_url, &new_url, &live_names)`; if none, log "moved outside the roots; not following")
- Modify: `src/home.rs` (new):
  ```rust
  /// Spec §5 steps a–d. Never under a lock. Idempotent.
  pub fn follow_move(old_url: &str, new_url: &str, live: &[String]);
  pub fn moved_to(home: &str) -> Option<String>; // reads sock/<key>/.moved-to
  ```
  (a) `record(old_url, n, new_url)` for each live `n` not already `Elsewhere(new_url)`; (b) rewrite every record whose owner == `old_url` to `new_url` (scan `all_in`); (c) copy `state/worktrees/<old key>.base` → `<new key>.base` if absent (`worktree::read_base` / `write_base`); (d) copy `state/<old key>.json` → `state/<new key>.json` if absent (`wsstate` paths; atomic copy via temp + rename).
- Modify: `src/claudes.rs` `sweep_gone_owners_with`: before forgetting a record whose owner `o` is gone, if `home::moved_to(o)` is `Some(n)` and `n` resolves → rewrite the record's owner to `n` instead of forgetting (compare-before-forget re-read retained).
- Modify: `src/proto.rs` + `src/hub.rs` (`Event::ProjectMoved { from: String, to: String }` broadcast to both projects after following, via `broadcast_to_project` with no lock held); `static/app.js` banner ("`<from basename>` was renamed to `<to basename>`; its terminals followed" + link to `to`).

**Interfaces:**
- Consumes: `MoveEvidence` (Task 1), `rehome`-free `home::record` (records only; hub refresh happens via the existing `ProjectsChanged`/refresh path — call `crate::hub::refresh_and_broadcast(new_url)` and `(old_url)` after following).
- Produces: `home::follow_move`, `home::moved_to`, `Event::ProjectMoved`.

- [ ] **Step 1: Failing tests:**
  - registry.rs `a_renamed_worktree_takes_its_sessions_with_it` — real git repo under a root, `git worktree add .claude/worktrees/worktree-a`, real dtach session started inside it under key `<repo>/.claude/worktrees/worktree-a`, confirming reconcile, `git worktree move` to `worktree-b`, reconcile → `.moved-to` == `<repo>/.claude/worktrees/worktree-b`; `home::owner_of(old, name) == Elsewhere(new)`; `session::owned_live_names(new)` contains `(name, Some(old))`; the master is still alive.
  - `a_move_outside_every_root_writes_nothing` — rename the project dir to a path outside the roots → no `.moved-to`, session alive.
  - home.rs `follow_move_rewrites_owner_records_and_copies_state_only_when_absent`.
  - claudes.rs `a_rehomed_terminal_follows_its_owners_rename` — record `(h, term3) → old`; `moved_to(old) = Some(new)` (write the file in the test state dir); run the gone-owner sweep with a `home_dir_of`/list that no longer lists `old` → record now `Elsewhere(new)`, not forgotten.
- [ ] **Steps 2–4:** RED, implement, GREEN (`--lib registry home claudes hub`), timed.
- [ ] **Step 5: Revert checks:** (a) skip `follow_move` → the first test fails on `owner_of`; (b) remove the `moved_to` check in the gone-owner sweep → `a_rehomed_terminal_follows…` fails (record forgotten). Restore; record.
- [ ] **Step 6: Commit** — `registry: a renamed project or worktree takes its sessions with it (#158)`.

---

### Task 8: §5b — the two gated widenings

**Files:**
- Modify: `src/home.rs` (`resolves_in`: additionally return `true` when `moved_to(home)` == `Some(owner)` and `owner` resolves via `projects::resolve_project`; the existing `.claude/worktrees` rule unchanged)
- Modify: `src/term.rs` (when `resolve_project(roots, &project)` is `None`: if `crate::home::moved_to(&project)` is `Some(new)` and `resolve_project(roots, &new)` is `Some(new_dir)`, call `session::attach_existing(&project, name, &new_dir)`; else the existing refusal)
- Modify: `src/session.rs` (`pub fn attach_existing(project, name, dir) -> Result<Attachment, String>`: same as `attach` but a `Decision::Spawn` returns `Err(format!("project was renamed to {new}"))` — pass the new URL in for the message — and nothing is reserved)

- [ ] **Step 1: Failing tests:**
  - home.rs `a_record_naming_an_unrelated_owner_still_does_not_resolve` — owner outside `.claude/worktrees`, no `.moved-to` → `resolves_in` false; with a matching `.moved-to` and the owner dir existing under the root → true.
  - session.rs `attach_existing_joins_a_live_session_and_never_spawns` — real dtach: a live session under key K → `attach_existing(K, name, dir)` Ok and `any_process_holds(sock)` unchanged; a name with no session → Err containing "renamed", and no socket created.
- [ ] **Steps 2–4:** RED, implement, GREEN.
- [ ] **Step 5: Revert checks:** (a) accept any owner in `resolves_in` when `.moved-to` exists (not equality) → the unrelated-owner case fails; (b) let `attach_existing` spawn → the "never spawns" case fails. Restore; record.
- [ ] **Step 6: Commit** — `home, term: a moved key attaches to its existing sessions only (#158)`.

---

### Task 9: browser tests

**Files:**
- Modify: `tests/browser/worktree-rehome.mjs`

Scenarios (real Chromium, real dtach, the existing fake `claude` binary which `chdir`s on `HOOK-PATH`; every absence paired with a positive):
- [ ] **A′ (§1):** approve → the fake `chdir`s → within 6 s the worktree page shows the terminal with the same PID and the main page's tab is gone. Then a second terminal: approve → the fake does **not** `chdir` (add a `worktree-nochdir <name>` command to the fake) → after 35 s the main page still has the tab, the worktree page has none, and no "moved to" notice appeared; the worktree exists in `git worktree list`.
- [ ] **F (§2):** with the main page open, `git worktree add ../sib -b sib` from Deno → the banner shows "sib" and its link's href ends with `/sib` (or the sibling's URL).
- [ ] **G (§3):** in a terminal on project P, `echo <abs path of a file in project Q>`; click the link (the harness's existing link-click helper, see `termlinks.mjs`) → "That's in Q" banner; its link opens Q's page and the file tab for that path is active.
- [ ] **H (§5, Review Focus 5):** terminal on a worktree page; `git worktree move` it → within one sweep (load `/` to trigger) the old page shows the `ProjectMoved` banner with a link to the new page; on the new page the terminal answers with the same PID.
- [ ] **Run** 3 times against its own harness (never the live instance). **Revert check:** with Task 4's revert (a) applied, A′'s no-chdir half fails (tab moved). Record in the header.
- [ ] **Commit** — `browser: worktree moves follow the process; banners for new and renamed worktrees; links into other projects (#158)`.

---

### Task 10: CLAUDE.md and docs

**Files:**
- Modify: `CLAUDE.md` (re-homing bullet: re-homed only on positive evidence the Claude process moved, or the user's "Move tab there", or a `.moved-to` rename; re-homing on the hook named as the vulnerability-not-cleanup with the subagent reason; the two `.moved-to` widenings and "attach-only, never spawn under a gone key")
- Modify: `docs/deploy.md` (the suggested CLAUDE.md line for projects: "create worktrees with the EnterWorktree tool, so they land in `.claude/worktrees/` and roost can ask and follow"; correct the paragraph that says approval re-homes the terminal and that subagent approvals move the parent's tab — they no longer do)
- [ ] **Step 1:** edit; verify each sentence against the code (cite file:line in the report).
- [ ] **Step 2: Full suite** `time cargo test -- --test-threads=1` (all targets) on the committed tree; `cargo build` no warnings; asset-table grep names this checkout.
- [ ] **Step 3: Commit** — `CLAUDE.md, deploy.md: a terminal follows its Claude, never the hook; renames follow (#158)`.
