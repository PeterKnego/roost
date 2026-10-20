# Worktrees outside roost's roots — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A worktree that a Claude creates outside roost's roots can be opened in place after the user allows that one worktree. Its "appeared" banner stays until it is dismissed.

**Architecture:**
- A new module, `src/outside.rs`, owns the allow records in `state/outside/<parent key>/<name>`. Each record holds the canonical path and its `(dev, ino)`, written atomically with mode 0600.
- `projects::resolve_project` gains one fork-free arm for the virtual URL `<parent>/.claude/outside/<name>`. That URL resolves only through a record whose identity still matches.
- Two hub intents, `AllowOutsideWorktree` and `RevokeOutsideWorktree`, run their git check off-lock.
- The switcher and overview listings turn a matching outside worktree into a normal row.
- A per-project `state/appeared/<key>` list rides on the hub snapshot and is cleared by `DismissAppeared`.

**Tech Stack:** Rust (std, `serde_json`), plain JS (`static/app.js`), Deno + Chromium browser tests.

**Spec:** `docs/superpowers/specs/2026-10-20-worktrees-outside-roots-design.md` (issue #188; the decisions are in the issue's 2026-10-20 comment). Read it before any task. Section numbers below refer to it.

## Global Constraints

**Records and resolution**
- The virtual URL shape is exactly `<parent>/.claude/outside/<name>`. `<parent>` is any valid project URL without that suffix, and `<name>` passes `wtname::valid`.
- The virtual URL is resolved **only** through `state/outside/<storage_key(parent)>/<name>`: read the record, `metadata` the recorded path, and require a directory whose `(dev, ino)` equals the recorded one. Any other outcome is `None`. The URL is never tried against disk.
- The resolve arm is fork-free: no git and no subprocess. It runs under the hub and REGISTRY locks.
- Records are written only by `AllowOutsideWorktree`, after a user confirmation, and only when all of these hold:
  - `parent` resolves under the roots;
  - a **non-empty** `git worktree list` of `parent` reports the path as non-main;
  - the path is absolute, equals its own `canonicalize()`, and is a directory;
  - the name passes `wtname::check`.
- No other code writes records. That includes project config and anything read from a repository.
- Record files are written with a `wsstate::tmp_suffix()` temp file, `OpenOptions::create_new` + `mode(0o600)`, then `rename`.
- Record format: line 1 is the canonical path, line 2 is `dev ino`. Anything else is damaged, and a damaged record is `None`.

**Revocation**
- Losing access fails closed. Revocation deletes only the record. It never touches sessions, layout, `.base` or `.origin`.
- A record is deleted on `RevokeOutsideWorktree`, or when a **non-empty** listing of its parent lacks the path. An empty listing changes nothing.

**Caps** (constants, not config; each must name itself in its message or log when it fires)
- `outside::OUTSIDE_CAP = 32` records per parent.
- `appeared::APPEARED_CAP = 16` entries per project. Over the cap the oldest entry is dropped, and one log line says so.

**General**
- No git under the hub lock, the hub REGISTRY lock, `SESSIONS`, or any claudes static lock. No lock is held across blocking I/O.
- The HTTP surface is unchanged (two POSTs). Every new action is a websocket intent behind `askConfirm` where it changes state.
- Client: text is set with `textContent` only, and every href is built with `projectPath`.
- Tests:
  - run `cargo test -- --test-threads=1`, never bare and never with `--release`;
  - every new test is watched failing with its code reverted, and the observed failure is recorded in the test's comment;
  - use real git wherever git listing is the subject.
- Never attach automation to the live roost on port 8444. Never `pkill dtach`. Build only in `/home/claude/projects/roost`. Run `git branch --show-current` (expect `outside-worktrees`) immediately before every commit. Stage explicit paths only.

## Review Focus

1. **A symlink swap after allow.** The user allows `~/scratch/wt`, and later `~/scratch/wt` is replaced by a symlink to `/etc` or by a different directory. Resolution must refuse: the identity check is on `metadata` of the recorded path, and any path difference refuses. Pinned in Task 1, `a_swapped_directory_does_not_resolve`.
2. **A cloned repository that ships `.claude/outside/<name>`.** A real directory sits at that path in the parent repo, with no record. It must not resolve. Pinned in Task 2, `a_planted_outside_dir_resolves_nowhere`.
3. **Two parents with the same basename,** or one parent with two outside worktrees named `wt`. Names are allocated per parent (`wt`, `wt-2`), and the URLs never collide across parents because the parent is part of the URL. Pinned in Task 1, `names_are_allocated_per_parent`.
4. **The user allows from a sibling worktree's page.** The intent carries `parent` explicitly, the hub validates against that parent, and the allow succeeds. Pinned in Task 3, `allow_from_a_sibling_page_uses_the_named_parent`.
5. **A live terminal on an allowed worktree is revoked.** The session stays alive, and its page stops resolving. Pinned in Task 3, `revoke_leaves_a_live_session_alive`, with real dtach.

---

### Task 1: `src/outside.rs`, the allow records

**Files:**
- Create: `src/outside.rs` (with a `//!` module doc saying why it exists: the one confinement exception, and what must never be made "consistent", from the spec's CLAUDE.md section)
- Modify: `src/lib.rs` (`pub mod outside;`)

**Interfaces (Produces):**
```rust
pub const OUTSIDE_CAP: usize = 32;
pub const SEGMENT: &str = ".claude/outside";

/// `<parent>/.claude/outside/<name>` → (parent, name), else None. Pure.
pub fn split_url(url: &str) -> Option<(&str, &str)>;
pub fn url_for(parent: &str, name: &str) -> String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record { pub name: String, pub path: PathBuf, pub id: (u64, u64) }

/// Reads and validates; None for missing/damaged/unreadable. Fork-free.
pub fn read_in(state: &Path, parent: &str, name: &str) -> Option<Record>;
/// The record's directory when it exists with the recorded identity, else None.
pub fn resolve_in(state: &Path, parent: &str, name: &str) -> Option<PathBuf>;
/// All records for a parent (damaged ones skipped), sorted by name.
pub fn list_in(state: &Path, parent: &str) -> Vec<Record>;
/// The record naming `path` for this parent, if any (exact canonical path match).
pub fn find_in(state: &Path, parent: &str, path: &Path) -> Option<Record>;
/// Validates path (absolute, == canonicalize, is_dir), allocates a name
/// (basename, then -2, -3 … ; wtname::check), enforces OUTSIDE_CAP, writes
/// atomically 0600. Returns the record. Does NOT check git — the caller does.
pub fn allow_in(state: &Path, parent: &str, path: &Path) -> Result<Record, String>;
pub fn revoke_in(state: &Path, parent: &str, name: &str) -> Result<(), String>;
/// Deletes records of `parent` whose path is not in `listed`. Caller passes
/// only a NON-EMPTY listing. Returns names removed.
pub fn prune_in(state: &Path, parent: &str, listed: &[PathBuf]) -> Vec<String>;
// Plus non-`_in` wrappers over `wsstate::state_dir()`.
```

- [ ] **Step 1: Write the failing tests.** Put them at the bottom of `outside.rs`, using `tempfile`. Each must be concrete.
  - `split_url_accepts_only_the_exact_shape`:
    - `"p/.claude/outside/wt"` → `Some(("p","wt"))`;
    - `"karpie/src/.claude/outside/wt"` → `Some(("karpie/src","wt"))`;
    - `None` for `"p/.claude/outside"`, `"p/.claude/outside/wt/x"`, `".claude/outside/wt"`, `"p/.claude/outside/../x"`, and a name that fails `wtname::valid`.
  - `allow_then_resolve` covers allow, read, resolve, and the record file's mode (0600).
  - `a_swapped_directory_does_not_resolve`: allow `d/wt`, then `rm -rf d/wt; mkdir d/wt` → `None`. Also allow `d/wt2`, then replace it with a symlink to another directory → `None`. (Review Focus 1.)
  - `a_damaged_record_does_not_resolve`: one-line file, non-numeric identity, extra lines, empty file → `None`. Also an unreadable record (chmod 000; skip as root) → `None`.
  - `allow_refuses_a_symlinked_or_relative_path`: a path through a symlinked parent, a relative path, a file → `Err`, and no file is written.
  - `names_are_allocated_per_parent`:
    - two different paths both ending `wt` under one parent → names `wt` and `wt-2`;
    - the same basename under another parent → `wt`. (Review Focus 3.)
  - `allowing_the_same_path_twice_returns_the_same_record`.
  - `the_cap_names_itself`: 32 allows succeed; the 33rd is `Err` containing `"32"`.
  - `revoke_and_prune`:
    - `revoke_in` deletes the file;
    - `prune_in` with a listing lacking a path deletes that record and keeps the listed one.
- [ ] **Step 2: Run them and confirm they fail.** `cargo test --lib outside -- --test-threads=1` fails to compile because the module does not exist yet. Save the output.
- [ ] **Step 3: Implement.**
  - Storage: `state/outside/<crate::projects::storage_key(parent)>/<name>`.
  - Identity: `std::os::unix::fs::MetadataExt` `dev()` / `ino()`. On non-unix, `allow_in` returns `Err`.
  - Write with `OpenOptions::new().write(true).create_new(true).mode(0o600)` on a `.<name>.<tmp_suffix>.tmp` temp file, then `rename`. Remove the temp file on any error.
  - `resolve_in` uses `metadata` and requires `is_dir()`. Following a symlink in the final component would land on a different inode, so it fails the identity check. Every stat error → `None`.
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Revert checks. Run each, record the failure in the test's comment, and restore from a `cp` backup.**
  - (a) Drop the identity comparison in `resolve_in` → `a_swapped_directory_does_not_resolve` fails.
  - (b) Use `create(true)` instead of `create_new` and drop the mode → the 0600 assertion fails.
  - (c) Skip the `== canonicalize()` check → `allow_refuses_a_symlinked_or_relative_path` fails.
- [ ] **Step 6: Commit:** `outside: allow records, one worktree by identity (#188)`

---

### Task 2: `resolve_project` resolves the virtual URL, and only through the record

**Files:**
- Modify: `src/projects.rs`. In `resolve_project_with`, add the first arm, before the RESERVED, empty-segment and dot-segment checks: `if let Some((parent, name)) = crate::outside::split_url(name) { return crate::outside::resolve(parent, name); }`. Apply the same arm in `resolve_recorded_project`, which shares `resolve_project_with`.

**Interfaces:**
- Consumes `outside::split_url` and `outside::resolve` from Task 1.

- [ ] **Step 1: Write the failing tests.** These need roots and a state dir. Copy an existing `projects` test's env-lock pattern (grep `STATE_ENV_LOCK` in src/projects.rs tests); do not invent a new one.
  - `an_allowed_outside_worktree_resolves`: allow a directory outside the roots, then `resolve_project(roots, url_for(parent, name))` returns its canonical path.
  - `a_planted_outside_dir_resolves_nowhere`: create `<root>/p/.claude/outside/wt/` on disk with no record → `None`. Then also create a record for a *different* path under that name → it resolves to the recorded path, never the planted one. (Review Focus 2.)
  - `a_path_escaping_an_allowed_worktree_is_refused`: `safe_resolve(<resolved outside dir>, "../x")` and a symlink inside it pointing out → `Err`. This pins existing confinement for this kind of project.
  - `the_arm_never_forks`: resolve the virtual URL with `PATH` set to an empty directory (git unreachable) → it still resolves. Assert on the result. Restore `PATH` under the env lock.
- [ ] **Step 2: Run them and confirm they fail.** The first returns `None`, and the planted case may resolve through the dot-segment vouch path. Record what you see.
- [ ] **Step 3: Implement** the one arm.
- [ ] **Step 4: Run them and confirm they pass.** Then run `cargo test --lib projects -- --test-threads=1` in full.
- [ ] **Step 5: Revert check.** Move the arm after the dot-segment check → `a_planted_outside_dir_resolves_nowhere`, or the first test, fails. Record it.
- [ ] **Step 6: Commit:** `projects: a virtual outside URL resolves only through its record (#188)`

---

### Task 3: `AllowOutsideWorktree` / `RevokeOutsideWorktree`

**Files:**
- Modify: `src/proto.rs`. Add `Intent::AllowOutsideWorktree { parent: String, path: String }` and `Intent::RevokeOutsideWorktree { parent: String, name: String }`, plus `Event::OutsideAllowed { url: String }`. Add the serde round-trip tests the neighbouring intents have.
- Modify: `src/hub.rs`. Add `fn do_allow_outside(&mut self, from, parent, path)` and `fn do_revoke_outside(&mut self, from, parent, name)`. They follow `do_remove_worktree`'s shape (:2640):
  - validate cheaply under the lock (`valid_project(parent)`);
  - then a `work` closure, run on a spawned thread (the existing thread-or-sync dispatch), with no lock held. Allow:
    1. `projects::resolve_project(&roots(), &parent)` → `Err("… does not resolve")`;
    2. `worktree::list(&parent_dir)`; empty → `Err("git did not answer; nothing was allowed")`;
    3. the canonical `path` must equal a listed non-main worktree's canonical path, else `Err("… is not a worktree of <parent>")`;
    4. `outside::allow(&parent, &path)`;
    5. `refresh_and_broadcast(&parent)`, then `broadcast_all(ProjectsChanged)` with no lock held, so every switcher refreshes;
    6. reply `OutsideAllowed { url }` to the sender.
  - Revoke: `outside::revoke(&parent, &name)`, then the same refresh. Errors go to the sender as `Event::Error`.

**Interfaces:**
- Consumes `outside::allow`, `revoke` and `url_for` (Task 1), and `projects::resolve_project` (Task 2).
- Produces `Event::OutsideAllowed`.

- [ ] **Step 1: Write the failing tests** in hub.rs. Use real git: a repo under a root, plus `git worktree add <tmp outside the roots>/wt`. Copy the fixture style of the existing `RemoveWorktree` tests (grep `Intent::RemoveWorktree` in the hub tests).
  - `allow_writes_the_record_and_the_url_resolves`: two connections. The sender gets `OutsideAllowed{url}`, the other connection gets **no** `OutsideAllowed`, and `resolve_project(url)` is `Some`.
  - `allow_refuses_a_path_git_does_not_list`: a plain directory outside the roots → `Error` naming it, and no record.
  - `allow_refuses_when_git_does_not_answer`: inject an empty listing. If there is no seam, make the parent's `.git` unreadable (skip as root) → `Error`, no record.
  - `allow_from_a_sibling_page_uses_the_named_parent`: the sender's hub is a `.claude/worktrees/x` sibling of the parent → it succeeds. (Review Focus 4.)
  - `revoke_deletes_the_record`.
  - `revoke_leaves_a_live_session_alive`: real dtach, a session started under the virtual URL key with cwd in the outside directory. Revoke → the socket is still held, and `resolve_project(url)` is `None`. (Review Focus 5.)
- [ ] **Step 2: Run them and confirm they fail.** **Step 3: Implement.** **Step 4: Run them and confirm they pass.** Time `cargo test --lib hub -- --test-threads=1`; a hang is a bug.
- [ ] **Step 5: Revert checks.**
  - (a) Skip the listing match → `allow_refuses_a_path_git_does_not_list` fails.
  - (b) Treat an empty listing as "no worktrees, so refuse with the wrong message" — or, worse, let it through → `allow_refuses_when_git_does_not_answer` fails on its message assertion.
  - Record both.
- [ ] **Step 6: Commit:** `hub: allow and revoke one outside worktree (#188)`

---

### Task 4: Listings and rows

**Files:**
- Modify: `src/registry.rs`:
  - `ProjectStatus` gains `pub outside: Option<String>` (the real path, for the tooltip and note). Default it to `None` at every constructor; grep `reachable:` to find them.
  - In the `known_projects` grouping's `None =>` arm (about :2140) and in `worktree_rows`' `None` branch (about :2038), call `outside::find(&parent_url, &w.path)` first. If there is a record and `outside::resolve(parent, name)` is `Some`, the row gets `url = url_for(parent, name)`, `key = storage_key(url)`, `reachable: true`, `outside: Some(path)`; in `worktree_rows`, compute `wt` state as for any reachable row.
  - After each listing that is **non-empty**, call `outside::prune(&parent_url, &listed_paths)`.
- Modify: `src/render.rs`, in `worktrees_strip` (:1443) and `overview_worktree_rows` (:1576). Shape the markup after the existing `.wtremove` control.
  - Unreachable rows gain `<button class="wtallow" data-parent="…" data-path="…">Allow…</button>`.
  - Rows with `outside: Some(p)` gain an `outside` class, a title containing the escaped path, and `<button class="wtrevoke" data-parent="…" data-name="…">Revoke</button>`.
  - They get no remove control.
  - Everything interpolated goes through `esc`.

- [ ] **Step 1: Write the failing tests.**
  - registry, real git:
    - `an_allowed_outside_worktree_is_a_reachable_row`;
    - `an_unallowed_one_stays_unreachable`;
    - `a_nonempty_listing_without_the_path_prunes_its_record`;
    - `an_empty_listing_prunes_nothing` (inject an empty `worktree::list` via the existing test seam if there is one; otherwise make the parent's `.git` unreadable).
  - render:
    - `the_allow_button_escapes_its_path`, using a fixture path containing `"`, `<` and `&`;
    - `an_outside_row_has_revoke_and_no_remove`.
- [ ] **Steps 2–4:** watch them fail, implement, watch them pass. Run the `registry` and `render` filters.
- [ ] **Step 5: Revert checks.**
  - (a) Prune on any listing, including an empty one → `an_empty_listing_prunes_nothing` fails.
  - (b) Drop `esc` on `data-path` → the escaping test fails.
  - Record both.
- [ ] **Step 6: Commit:** `registry, render: an allowed outside worktree is a row; Allow… and Revoke (#188)`

---

### Task 5: The appeared list persists

**Files:**
- Create: `src/appeared.rs`, with a `//!` doc covering the spec §3 rationale.
  - `pub const APPEARED_CAP: usize = 16;`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)] pub struct Appeared { pub path: String, pub branch: String }`
  - `pub fn load_in(state, project) -> Vec<Appeared>` (missing or damaged → empty, and a damaged file is logged once)
  - `pub fn add_in(state, project, a) -> Vec<Appeared>`: dedupe by path, append, trim the oldest above the cap with a log line naming `APPEARED_CAP`, write atomically 0600 as JSON
  - `pub fn remove_in(state, project, path) -> Vec<Appeared>`
  - `pub fn retain_listed_in(state, project, listed: &[PathBuf]) -> Vec<Appeared>`, called only with a non-empty listing
  - non-`_in` wrappers
  - Add it to lib.rs.
- Modify: `src/hub.rs`:
  - In `confirm_worktrees`, for each new worktree, call `appeared::add(&self.project, …)` and log ``roost: {project}: worktree {path} ({branch}) appeared``, appending `, outside roost's roots` when `w.url` is `None`. Keep the `WorktreeAppeared` broadcast.
  - Also in `confirm_worktrees`, when `seen` is a non-empty answer, call `appeared::retain_listed` and `outside::prune(&self.project, &listed)` (spec §1 Revocation: the hub's worktree confirm also prunes). Both are file I/O on roost's own state, which is acceptable under the hub lock, as `home::record` is.
  - `snapshot_event` adds `appeared: Vec<AppearedView>` to `WorkspaceView`. `AppearedView` is `{ path, branch, url: Option<String>, outside: bool, allowed: bool }`, with `url` from `projects::project_for_abs`, or from `outside::find` + `url_for` for an allowed outside one. This is fork-free.
  - New `Intent::DismissAppeared { path }` → `appeared::remove`, then broadcast the snapshot.
  - Opening a project's page clears its entry in the *parent's* list. On the first `State` sent to a connection of project U, if U is a worktree (`.claude/worktrees/` or the virtual outside URL) of parent P, run `appeared::remove(P, canonical dir of U)` and refresh P's hub. Do it off-lock after the snapshot is sent: spawn, or use the existing post-snapshot hook if there is one.
- Modify: `src/proto.rs` (the `WorkspaceView.appeared` field with `#[serde(default)]`, `AppearedView`, `Intent::DismissAppeared`).

- [ ] **Step 1: Write the failing tests.**
  - appeared.rs:
    - `add_dedupes_and_caps` (17 adds → 16 left, the oldest gone);
    - `remove_and_retain`;
    - `a_damaged_file_is_empty_not_a_panic`;
    - mode 0600.
  - hub.rs:
    - `an_appearance_survives_a_new_hub`: confirm on hub 1, then a fresh `Hub::new` snapshot includes it;
    - `dismiss_clears_it_for_every_connection`: two connections both receive a snapshot without it;
    - `opening_the_worktree_page_clears_its_entry`;
    - `a_listing_without_it_clears_it`.
- [ ] **Steps 2–5:** watch them fail, implement, watch them pass. Revert checks:
  - (a) No persist in `confirm_worktrees` → `an_appearance_survives_a_new_hub` fails.
  - (b) Dismiss sent only to the sender → the two-connection test fails.
  - Record both.
- [ ] **Step 6: Commit:** `appeared: a worktree banner persists until dismissed (#188)`

---

### Task 6: Client

**Files:**
- Modify: `static/app.js`:
  - **Switcher and overview handlers**, next to `.wtremove` (about :4899). Read every dataset value before the first `await`, as that code's comment explains.
    - `.wtallow` → `askConfirm({ title: "Open outside worktree", lines: [`Open ${path} in roost?`, "It is outside roost's roots; roost will open only this worktree."], confirm: "Allow" })` → `send({ t: "AllowOutsideWorktree", parent, path })`.
    - `.wtrevoke` → `askConfirm({ title: "Revoke outside worktree", lines: [`Stop opening ${name} in roost?`, "Running terminals keep running."], confirm: "Revoke" })` → `send({ t: "RevokeOutsideWorktree", parent, name })`.
  - **`OutsideAllowed {url}`** → `showBanner("Allowed — open")` with the link appended to *its returned box*, as M2 does, href `"/" + projectPath(url)`.
  - **The appeared list from `State`.** Render one persistent banner per entry. Track the boxes by path, so a re-render updates the list rather than duplicating it.
    - Allowed or inside the roots: "worktree <name> (<branch>) appeared — open", with the link.
    - Outside and not allowed: "worktree <name> (<branch>) appeared, outside roost's roots — Allow…", with a button running the same allow flow.
    - Each has a **Dismiss** button → `send({ t: "DismissAppeared", path })`.
    - These banners never auto-dismiss.
    - Replace the one-shot `WorktreeAppeared` handler with a no-op comment explaining that the snapshot now carries the list (the event still arrives).
  - **Page header note.** When `document.body.dataset.outside` is set, show "outside roost's roots: <path>" with `textContent`.
- Modify: `src/render.rs`, the page shell. Find where `data-key` is set on `<body>` (grep `data-key`). Add `data-outside="<escaped real path>"` when the page's URL is a virtual outside URL, via `outside::split_url` + `outside::read`.

- [ ] **Step 1:** `node --check static/app.js`. Add the render test `the_page_carries_its_outside_path_escaped`, with a metacharacter fixture. Watch it fail, implement, watch it pass, run the revert check (drop `esc`), and record it.
- [ ] **Step 2: Commit:** `app.js: Allow…/Revoke, persistent appeared banners, outside note (#188)`

---

### Task 7: Browser test

**Files:**
- Create: `tests/browser/outside.mjs`. Base it on `worktree-rehome.mjs`'s harness use; read `tests/browser/README.md`'s four traps first.

Scenarios. Every absence is paired with a positive check.
- **P.** With the main page open, `git worktree add <tmp outside roots>/wt -b side`. The persistent banner shows "appeared, outside roost's roots". After `page.reload()` it is still there. It is still there after 10 s (beyond the old 8 s).
- **Q.** Click Allow… in the banner and confirm. The switcher row for `wt` becomes a link to `/<parent>/.claude/outside/wt`. Open it, start a terminal, and type `pwd -P; echo MARK` and wait for the MARK echo. The printed path equals the real directory. The header note shows the path.
- **R.** Dismiss on another appeared banner removes it. A second page on the same project also loses it: two pages.
- **S.** Revoke from the switcher and confirm. The row is dimmed again. Navigating to the virtual URL no longer shows a workspace; assert what the server answers for an unresolvable project.

Run it 3 times on its own harness. Revert check: with Task 2's arm removed, Q fails. Record that in the header.

- [ ] **Commit:** `browser: outside worktrees — persistent banner, allow, revoke (#188)`

---

### Task 8: CLAUDE.md and the full suite

**Files:**
- Modify: `CLAUDE.md`.
  - Under "Every filesystem path is confined", add the spec's exception paragraph, the why-narrow paragraph, and the "never consistent" paragraph, citing `outside::resolve` and `projects::resolve_project_with`'s first arm.
  - In Caps, add `outside::OUTSIDE_CAP` (32) and `appeared::APPEARED_CAP` (16).
- [ ] Verify every sentence against the code, citing file:line in the report.
- [ ] `time cargo test -- --test-threads=1` on the committed tree, all targets. `cargo build` with no warnings. Run the asset-table grep from CLAUDE.md, which must name this checkout.
- [ ] **Commit:** `CLAUDE.md: the one confinement exception — an allowed outside worktree (#188)`
