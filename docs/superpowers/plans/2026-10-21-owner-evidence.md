# Ownership changes only on evidence about that terminal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A re-home record's owner changes only when that terminal's own directory identity is found at the new place, or when the terminal's own shell is there. A record is never forgotten into a home URL that now holds a different directory. This closes #183's chains (a)–(d).

**Architecture:**
- **Shared identity.** A new `src/dirid.rs` holds the identity `(dev, ino, btime)` in one format, with a three-valued reader and an explicit legacy form.
- **Identity everywhere.** `home` records (`.<name>.ino`) and registry key markers (`.origin-ino`, plus a list in `.occupant-ino`) move to that format.
- **One finder.** `registry::find_by_identity` looks for one identity at the candidate places.
- **Per-record decisions.** R18 (`claudes`) and rename-following (`home::follow_move_in`, `registry::follow_moves`) make every ownership change through that finder, per record or per session, never from key-level evidence.
- **Forget only into the same directory.** R18 forgets a record only into a home URL whose directory is still the one the session started in.

**Tech Stack:** Rust (std).

**Spec:** `docs/superpowers/specs/2026-10-21-owner-evidence-design.md` (issue #183; decisions recorded on the issue on 2026-10-21). Read it before any task.

## Global Constraints
**Identity format**
- The identity format is one line, `dev ino btime_secs btime_nsec`, taken from the directory's own `metadata` (`dev()`, `ino()`, `created()`).
- A two-field line is *legacy*. Anything else is damaged.
- A legacy identity may **protect from the reap** (the keep direction). It must never **license an ownership change** (the follow direction).
- An unreadable birth time is "cannot read". It never falls back to `(dev, ino)`.

**Records and forgetting**
- A record's owner changes only when the record's own identity is found at the new directory, or, for a key's own native session, when its leader shell's cwd ancestry carries one of the key's identities. Key-level evidence never re-points a record.
- R18 forgets a record only when its home either does not resolve, or resolves to a directory whose identity equals the home key's `.origin-ino`. An occupant, a mismatch, or "cannot tell" keeps the record.
- "Cannot tell" is never folded into either direction:
  - a stat error other than `NotFound`, or an unreadable marker, suspends the record for this pass;
  - the suspension is logged once per record, like R18's existing `suspend`.

**Writes**
- Every write is atomic: a `wsstate::tmp_suffix()` temp file, `create_new` + `mode(0o600)`, then `rename`.
- `OCCUPANT_CAP = 8`. The oldest entry is dropped first, and a log line names the cap.
- No git or `ps` under a hub, `REGISTRY` or `SESSIONS` lock.
- Reaping is unchanged. `registry::reconcile`'s kill decision is not loosened anywhere.

**Tests and process**
- Run tests with `cargo test -- --test-threads=1`. Every new test is watched failing with its code reverted, and the observed failure is recorded in its comment.
- Use real git and real dtach wherever liveness or a rename is the subject. Lock order is STATE then SESSION. Never `pkill dtach`.
- Build only in `/home/claude/projects/roost`. Run `git branch --show-current` (expect `owner-evidence`) right before every commit. Stage explicit paths.

## Review Focus
1. **ext4 inode reuse under a record.** Example: A→B is followed, then B is removed and a new B is created at the same path (same inode, new btime). The record must not keep resolving to the new B as "its" directory, and must not follow into it. Pinned in Task 4 by `a_recreated_target_is_not_the_records_directory`.
2. **A main-checkout resident whose Claude exited.** It has no cwd evidence and its occupant was renamed. The sibling scan alone must find the new place, by the record's identity. Pinned in Task 4 by `a_resident_with_no_claude_follows_by_its_identity`.
3. **Legacy files on the deploy host today.** Every existing `.ino` and `.origin-ino` is two-field. After deploy, nothing may be forgotten or re-pointed that wasn't before, beyond what the new rules require. Legacy identities must upgrade on the next pass. Pinned in Task 2 (`a_legacy_ino_is_upgraded_when_its_owner_matches`) and Task 3 (`a_legacy_origin_protects_but_never_licenses_a_follow`).
4. **A record whose home key directory is gone entirely,** with no sockets and no markers. That is "home does not resolve" or "cannot tell", never a guess. Pinned in Task 5 by `a_home_with_no_key_dir_is_forgotten_only_when_its_url_is_gone`.
5. **Two sessions of one key whose shells are in different renamed directories.** The occupant's session and an earlier-followed session must each follow their own directory, and must not be swapped. Pinned in Task 6 by `each_session_follows_its_own_shell`.

---

### Task 1: `src/dirid.rs` — one identity, three outcomes

**Files:**
- Create `src/dirid.rs`. It needs a `//!` doc covering two points. First, why birth time is part of the identity: ext4 reused the inode in 50 of 50 `rm` + `mkdir` cycles, which was measured for #191. Second, why a legacy identity may protect a directory but never license a follow.
- Modify `src/lib.rs` (`pub mod dirid;`).

**Interfaces (Produces):**
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DirId { pub dev: u64, pub ino: u64, pub bsec: i64, pub bnsec: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored { Full(DirId), Legacy { dev: u64, ino: u64 } }

/// The directory's identity from its own `metadata` (follows a final symlink,
/// like `outside::resolve_in`). Ok(None) = NotFound; Err = cannot tell
/// (any other error, or an unreadable birth time).
pub fn of(path: &Path) -> Result<Option<DirId>, String>;
/// Same, but `symlink_metadata` (for scanning directory entries).
pub fn of_entry(path: &Path) -> Result<Option<DirId>, String>;
pub fn format(id: &DirId) -> String;                 // "dev ino bsec bnsec"
pub fn parse_line(line: &str) -> Option<Stored>;     // None = damaged
/// Does `stored` name the directory `id`? Legacy compares (dev, ino) only —
/// callers decide whether a legacy match may license anything.
pub fn matches(stored: &Stored, id: &DirId) -> bool;
/// Atomic 0600 write of one or more lines (tmp_suffix, create_new, rename).
pub fn write_lines(path: &Path, lines: &[String]) -> Result<(), String>;
```

- [ ] **Step 1: Write the failing tests** at the bottom of `dirid.rs`, using `tempfile`:
  - `of_reads_a_directory_and_says_notfound_for_none`
  - `a_recreated_directory_differs` (`rm` + `mkdir` at the same path in a `$HOME`-based tempdir; the ids are `!=`)
  - `parse_line_full_legacy_and_damaged`: 4 fields → Full, 2 fields → Legacy, and 3 fields, non-numeric, empty or 5 fields → None
  - `matches_legacy_by_dev_ino_only`
  - `write_lines_is_0600_and_atomic`
- [ ] **Step 2:** Run them (`cargo test --lib dirid -- --test-threads=1`) and watch them fail to compile. **Step 3:** Implement, with birth time from `created()`: `duration_since(UNIX_EPOCH)` for times after the epoch, a negative `bsec` for times before it, and `Err` when the time is unreadable. **Step 4:** Run them and watch them pass.
- [ ] **Step 5: Revert checks.**
  - Make `a_recreated_directory_differs` compare `(dev, ino)` only. It fails on `$HOME` ext4, because the inode is reused. Record that.
  - Make `parse_line` accept 3 fields. The damaged-case test fails. Record that.
- [ ] **Step 6: Commit:** `dirid: one directory identity, with birth time and a legacy form (#183)`

---

### Task 2: `home` records carry the four-field identity

**Files:**
- Modify `src/home.rs`: `record_identity_in` (about :75), `identity_of_in` (about :103) and their wrappers.
- Modify `src/hub.rs` `rehome_to` (about :4396), adjusting it to the new return type if needed.

**Interfaces:**
- **Consumes** `dirid` (Task 1).
- **Produces:**
  ```rust
  pub fn record_identity_in(state, home, name, owner_dir: &Path) -> Result<(), String>; // writes dirid::format
  pub fn identity_of_in(state, home, name) -> Result<Option<dirid::Stored>, String>;   // Ok(None)=no file; Err=unreadable/damaged
  /// Upgrade a Legacy .ino to Full when `owner_dir`'s (dev, ino) match it; no-op otherwise.
  pub fn upgrade_identity_in(state, home, name, owner_dir: &Path) -> Result<bool, String>;
  ```
  It also produces non-`_in` wrappers over `wsstate::state_dir()`. `identity_of` (the claudes caller) is updated in Task 4; until then, `claudes.rs` matches `Stored::Full` / `Legacy` and keeps today's behaviour by comparing `(dev, ino)`.

- [ ] **Step 1: Write the failing tests:**
  - `a_record_identity_is_four_fields_and_0600`
  - `a_legacy_ino_reads_as_legacy`
  - `a_legacy_ino_is_upgraded_when_its_owner_matches` (Review Focus 3)
  - `a_legacy_ino_is_not_upgraded_onto_a_different_directory`
  - `a_damaged_ino_is_err_not_none`
- [ ] **Steps 2–4:** Watch them fail, implement, watch them pass. Run `cargo test --lib home -- --test-threads=1`, `cargo test --lib claudes -- --test-threads=1` and `cargo test --lib hub -- --test-threads=1`, all green.
- [ ] **Step 5: Revert checks.**
  - Let upgrade ignore the `(dev, ino)` match. `…not_upgraded_onto_a_different_directory` fails.
  - Make a damaged `.ino` read as `Ok(None)`. The damaged test fails.
  - Record both.
- [ ] **Step 6: Commit:** `home: a re-home record carries its owner's (dev, ino, btime) (#183)`

---

### Task 3: Registry markers move to `dirid`, and occupants become a list

**Files:**
- Modify `src/registry.rs`:
  - `write_identity_at` / `write_origin_identity`, which write the four-field form;
  - `identity_at` / `recorded_identity`, which return `Option<dirid::Stored>`;
  - `identities()`, which returns `Vec<dirid::Stored>`: origin plus every occupant line;
  - the occupied arm of `confirmed_gone`, which **appends** to `.occupant-ino` up to `OCCUPANT_CAP` (8), dropping the oldest and logging;
  - `move_evidence`, `moved_to_evidence`, `sibling_evidence` and `shell_inside`, which compare through `dirid::matches`.
- A match found only through a `Legacy` identity is Moved for the **reap decision** only. In `record_move_with`, a legacy-only match becomes `MoveRecord::Unfollowable`, so no `.moved-to` is written and nothing follows.

**Interfaces (Produces):**
- `pub(crate) const OCCUPANT_CAP: usize = 8;`
- `pub(crate) fn identities(key_dir) -> Vec<dirid::Stored>`
- `MoveEvidence::Moved` gains a `legacy: bool` field, or an equivalent. Pick the smallest change and state it in the report.

- [ ] **Step 1: Write the failing tests** (real git and dtach where a sweep is involved; copy the fixtures of `a_rename_of_the_new_directory_at_the_old_name_is_followed_not_reaped`):
  - `origin_ino_is_written_four_field_and_upgrades_a_legacy_one`
  - `the_occupant_list_appends_and_caps`: 9 occupants leave 8, the oldest is gone, and the log line names the cap
  - `a_third_directory_does_not_erase_an_earlier_occupants_identity` (chain c, key level)
  - `a_legacy_origin_protects_but_never_licenses_a_follow` (Review Focus 3): with a two-field `.origin-ino` and a rename, the session is **not** reaped, and `.moved-to` is **not** written
- [ ] **Steps 2–4:** Watch them fail, implement, watch them pass. Run `cargo test --lib registry -- --test-threads=1` in full; every existing #178/#182/#191 rename and outside test stays green.
- [ ] **Step 5: Revert checks.**
  - Overwrite the occupant file instead of appending. The third-directory test fails.
  - Let a legacy match license `Followable`. The legacy test fails.
  - Record both.
- [ ] **Step 6: Commit:** `registry: markers carry birth time; occupants are a list; legacy never licenses a follow (#183)`

---

### Task 4: One finder, and per-record re-pointing

**Files:**
- Modify `src/registry.rs`:
  ```rust
  pub(crate) enum Found { At(PathBuf, String /*url*/), Nowhere, CannotTell(String) }
  /// Where is the directory carrying `id`, for a record that pointed at `old_url`?
  /// Candidates in order: (1) `.moved-to` target of old_url's key if that key
  /// dir exists; (2) entries beside old_url's last known path (parent of the
  /// key's `.origin`, else parent of `<root>/<old_url>` for each root);
  /// (3) ancestors of `cwd` when given. A candidate counts only if
  /// `dirid::of_entry`/`of` gives Full == id, its URL resolves under the
  /// roots to that same directory, and the URL != old_url. NotFound → next;
  /// other errors → CannotTell. Fork-free except `resolve_project` (may run
  /// git) — never call under a lock.
  pub(crate) fn find_by_identity(roots, old_url, id: &dirid::DirId, cwd: Option<&Path>) -> Found;
  ```
- Modify `src/claudes.rs` `follow_gone_owner_inner` (about :555):
  - Remove the `record_move(owner)` arm at the top.
  - Read the record's identity (`home::identity_of`):
    - `Full(id)`: run `find_by_identity(roots, owner, &id, claude_cwd)`, then:
      - `At(dir, url)` → `rewrite_owner`, which also writes the `.ino` of the new dir;
      - `Nowhere` → NotMoved;
      - `CannotTell` → `suspend`.
    - `Legacy`: try `home::upgrade_identity`. Upgraded means retry as Full; otherwise NotMoved, since legacy never licenses a follow.
    - None → NotMoved.
    - Err → `suspend`.
  - The cwd ancestry walk moves inside `find_by_identity`'s candidate (3).
- Modify `src/home.rs` `follow_move_in` part (b): for each record owned by `old_url` (or `prev`), require `find_by_identity` (or the already-known new dir's identity == the record's own `Full` id) before rewriting. Otherwise skip it. Write the target's `.ino` on rewrite.
- `rewrite_owner` writes `home::record_identity(home, name, new_dir)`. On failure it removes the `.ino` (spec §1).

- [ ] **Step 1: Write the failing tests** (real git + dtach; chain fixtures as in #191's I2 tests):
  - `an_occupants_resident_is_not_repointed_to_b` (chain a): A→B followed, occupant at A, a main-checkout resident re-homed into the occupant, then the occupant is renamed to D. The resident follows **D**, and is never re-pointed to B. Check both through R18 and through `follow_moves`.
  - `a_resident_with_no_claude_follows_by_its_identity` (Review Focus 2): the same setup with no Claude process. The sibling scan finds D.
  - `a_recreated_target_is_not_the_records_directory` (Review Focus 1): `rm` + `mkdir` at the record's target path. The record does not follow into it.
  - `a_rewritten_record_carries_its_new_identity`
- [ ] **Steps 2–4:** Watch them fail, implement, watch them pass. Run `cargo test --lib claudes -- --test-threads=1`, `cargo test --lib home -- --test-threads=1` and `cargo test --lib registry -- --test-threads=1`, timed.
- [ ] **Step 5: Revert checks.**
  - Restore the key-level `record_move` arm. Chain (a)'s test fails, re-pointed to B.
  - Make `find_by_identity` compare `(dev, ino)` only. Review Focus 1's test fails.
  - Record both.
- [ ] **Step 6: Commit:** `claudes, home: a record is re-pointed only to a directory carrying its own identity (#183)`

---

### Task 5: Forget only into the same directory

**Files:**
- Modify `src/claudes.rs`, `sweep_gone_owners_with` (about :656), where it calls `home::forget`. Before forgetting `(home, name)`, classify the home:
  - `resolve_project(roots, home)` is None, **and** each root's `<root>/<home>` is `NotFound` → forget.
  - It resolves to dir D:
    - read the home key's `.origin-ino` (`registry::recorded_identity`, via a `pub(crate)` accessor);
    - `Full` and equal to D's identity, or `Legacy` with `(dev, ino)` equal → forget;
    - different → **keep**, logged once ("its home <home> now holds a different directory");
    - missing, unreadable or a stat error → **keep** (suspend).
  - Any other error → **keep**.
  - Put this in a helper: `fn home_is_its_own(roots, home) -> HomeState { Same, Gone, Other, CannotTell }`.

  The legacy-equal case may *forget*, because forgetting into the same directory is today's behaviour, and the legacy rule forbids only *following*.

- [ ] **Step 1: Write the failing tests:**
  - `a_followed_record_is_kept_not_forgotten_into_an_occupant` (chain b): occupant present, B renamed to a different parent so it can't be found. The record stays `Elsewhere(B)`, the session survives `reconcile`, and `kill_project(new A)` does not end it.
  - `chain_b_follows_to_c_when_c_is_findable`: B renamed to C in the same parent; the record follows to C.
  - `a_removed_worktrees_resident_still_comes_home` (regression): an ordinary removal is forgotten as today.
  - `a_home_with_no_key_dir_is_forgotten_only_when_its_url_is_gone` (Review Focus 4)
- [ ] **Steps 2–5:** Watch them fail, implement, watch them pass. Revert check: forget unconditionally; the occupant test fails, and the session is handed to the occupant. Record it.
- [ ] **Step 6: Commit:** `claudes: never forget a record into a home that now holds a different directory (#183)`

---

### Task 6: A key's own sessions follow their own shell

**Files:**
- Modify `src/registry.rs`, `follow_moves` (about :188) and `record_move_with`'s `inside`/`live` computation.
- Modify `src/home.rs`, `follow_move_in` part (a).
- Rule (spec §3): for each live native session of the key, take its session leader's cwd (the existing `procsess::target_sessions` / `shell_inside` machinery). If an ancestor carries one of `identities(key_dir)` as a **Full** identity, at a directory P whose URL resolves under the roots to P and is not the key's current directory, that session follows P:
  - a record to P's URL, with P's `.ino`;
  - the R16 gate.
- A session whose shell is outside every identity is left alone and logged once.
- This replaces the fresh-or-inside narrowing. Keep `fresh` only for announcing (`ProjectMoved`).

- [ ] **Step 1: Write the failing tests:**
  - `an_occupants_own_session_follows_d_while_b_exists` (chain d)
  - `each_session_follows_its_own_shell` (Review Focus 5): one key with two live sessions, one shell in B (the earlier follow) and one in D (the occupant renamed). Each ends up owned by its own directory.
  - `a_session_whose_shell_left_stays`
  - Regressions: `a_single_rename_still_follows_every_session_inside`, and #182's A→B, cd out, B→C still followed. That second one must stay green through the sibling scan, because the record carries B's Full identity after Task 4.
- [ ] **Steps 2–5:** Watch them fail, implement, watch them pass. Revert check: restore "fresh takes every live session". `each_session_follows_its_own_shell` fails, because the occupant's session is sent to B. Record it.
- [ ] **Step 6: Commit:** `registry, home: a key's own sessions follow their own shell (#183)`

---

### Task 7: CLAUDE.md and the full suite

**Files:**
- Modify `CLAUDE.md`: one paragraph under the rename row of "Absence of evidence", per the spec's CLAUDE.md section. Cite function names, not line numbers.

- [ ] Verify every sentence against the code, and cite file and function in the report.
- [ ] Run `time cargo test -- --test-threads=1` on the committed tree, all targets, with no warnings, and run the asset-table grep.
- [ ] Run `deno run -A tests/browser/worktree-rehome.mjs` and `deno run -A tests/browser/outside.mjs` once each, since both exercise rename-following.
- [ ] **Commit:** `CLAUDE.md: ownership changes only on evidence about that terminal (#183)`
