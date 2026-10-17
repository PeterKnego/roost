# Worktrees roost did not make, and a terminal that only moves when its Claude does

This is the first slice of #158, the follow-up to #156. It answers #158's subagent question the way the user decided on 2026-10-17: **the `WorktreeCreate` hook never re-homes a terminal.** It also adds the three items recorded on #158 after a live miss, where a Claude in `ultima_cluster` created `~/ultima/uc-rel` with a plain `git worktree add` and roost showed nothing:

1. the terminal moves only when its Claude does;
2. a worktree that appears from anywhere gets a banner;
3. a terminal link into another project offers to open that project;
4. Claude is steered toward `EnterWorktree`, through documentation only.

## Why the hook cannot be the trigger

Claude Code fires `WorktreeCreate` for three things: `EnterWorktree`, `claude --worktree`, and a subagent with `isolation: "worktree"`. On 2026-10-12, against Claude Code 2.1.296, a probe found that the subagent's payload has the same keys as the others, with no agent marker. So roost cannot tell which one asked. Since #156, approving re-homes the asking terminal. For a subagent, that moves the parent's tab while the parent Claude stays in the main checkout.

The hook tells roost what Claude *intends*. What actually happened shows up later, in `/proc/<pid>/cwd`. The probe also confirmed that the Claude process itself changes directory on `EnterWorktree`: same pid, and its cwd moves into the worktree. A subagent's worktree never moves the parent process. So the right trigger is the process moving, not the request.

## 1. The terminal moves when its Claude does

**Approval stops writing the re-home record.**
- `approve_ask` (`hub.rs`) still validates, creates the worktree, installs the worktree hook into it, replies `Path` and sends the clicker `WorktreeReady`.
- It no longer calls `home::record`, and the R16 gate before that call goes with it.

**Approval records an expectation instead.**
- The entry is `(home, session, worktree_url, at: Instant)`, held in a process-level map in `claudes.rs` beside `LAST_OFFERED` / `GONE_ANNOUNCED`.
- It is in memory only. A restart loses it, which only means the move becomes an offer (below).
- At most one entry per `(home, session)`; a newer approval replaces the older.

**The watcher acts on the evidence.** Each tick, `detect_moves_in` already produces `Move::Into { home, session, worktree }` when a Claude's cwd is inside a worktree of its home under `.claude/worktrees/`. `offer_moves` gains one branch:
- **Expected and fresh** (`(home, session)` expects exactly this `worktree`, and the entry is ≤ `EXPECT_MOVE_WINDOW`, 30 s, old):
  - Re-home through the same code path `do_rehome` uses: the R16 gate, then `home::record`, then refreshing the home and the owner hub one lock at a time, then `broadcast_all` with no lock held.
  - Remove the expectation.
  - No banner. The user already approved this move, and the worktree page is already open.
- **Everything else:** unchanged, including an expired expectation and a Claude that entered some other worktree. These get the existing "Move tab there" offer.

**Expiry.** Expired entries are swept on every tick. A subagent's approval therefore leaves an entry that expires and does nothing: the worktree exists, its page opened from the approval, and the parent terminal stays where it is.

**Cost.** The terminal follows up to one tick, about 3 s, after Claude enters the worktree, instead of at approval time. That is the price of the move being evidence-based. The worktree page opens immediately as before, and briefly shows no terminal until the move lands.

**Constraints that carry over.**
- The whole sequence runs on the watcher thread, off every hub lock, with `catch_unwind` around it. That matches how the watcher already handles moves and gone owners.
- Only a move into the exact `.claude/worktrees/<dir>` named by the expectation qualifies. `detect_moves_in` already requires that shape (`home::owner_path`).
- The R16 gate refusing (the owner already has a live session with that name) leaves the expectation to expire, and the existing offer path takes over.

## 2. A worktree that appears from anywhere gets a banner

**Watching.**
- `watch.rs` already watches `<project>/.git` non-recursively. It will also watch `<project>/.git/worktrees` non-recursively.
- If that directory does not exist yet, the existing `.git` watch sees it being created, and the watch is added then.
- `git worktree add` creates `.git/worktrees/<id>/` whichever tool ran it and wherever the worktree lives.
- Only a project whose `.git` is a directory counts, meaning a main checkout. A linked worktree's `.git` is a file and is never watched this way.

**Classification.** `classify(".git/worktrees/<id>…")` returns a new `Class::Worktrees`. It is ahead of the `.git` ignore rule, like `.git/index` and `.git/HEAD`. Bursts are debounced like other classes.

**Confirming.**
- On `Class::Worktrees`, the hub spawns a thread with no lock held. It runs `worktree::list(dir)` and diffs the non-main paths against the hub's last known set.
- The known set is seeded off-lock when the hub's watcher starts, so worktrees that already existed never banner.
- `worktree::list` returning empty means git did not answer, and that changes nothing, in the known set or in banners.

**Announcing.** For each new path, the hub broadcasts `Event::WorktreeAppeared { path, branch, url: Option<String> }` to its own browsers:
- `url` is the worktree's roost project URL when its path resolves under a root (§3's resolver, which is fork-free), otherwise `None`.
- The client shows a dismissible banner: "worktree `uc-rel` (docs/release-2.14.0) appeared — open", or "… appeared, outside roost's roots" with no link.
- The text goes through `textContent`, and the link is built with `projectPath`.

**Not for worktrees roost made.** `approve_ask` and `do_new_worktree` add their new path to the hub's known set *before* the git add, so the watch event finds it already known. Those flows already open the page.

**Removal.** A path that drops out of the list is removed from the known set. There is no banner, because #157 owns finishing.

## 3. Terminal links into another project

`do_open_path` currently answers "path is outside this project" when `projects::resolve_terminal_path(&self.dir, text)` refuses. With this change, before refusing, the hub tries `projects::project_for_abs(roots, abs)`. This is a new function, and it is fork-free:
- **The path to resolve.** The absolute path `resolve_terminal_path` computed (`~` expanded, any `:line` split off).
- **Finding the project.** Walk its ancestors, from the nearest outward, while they stay under a root. Return the first ancestor that has a `.git`, either a dir or a file, checked with `symlink_metadata`. A `NotFound` means not this one. Any other error stops the walk with `None`, because "cannot tell" is never treated as "found".
- **The URL.** The project URL is that ancestor's path relative to its root, plus the file's path relative to the ancestor.
- **Dot segments.** A URL with a dot segment is accepted only in the `.claude/worktrees/<valid name>` shape. That is the one place `resolve_project` would vouch for a dot segment, and checking the shape avoids the git call that vouching makes.
- **Under a lock.** The function runs under the hub lock, which is why it uses only `symlink_metadata` and never git.

If a project is found and it is not this one, the hub replies `Event::PathElsewhere { project, rel, line }`:
- The client shows "That's in `<project>` — open" with a link to `/<project>?open=<rel>[:line]`.
- The new page reads `?open=` once at load, the same way `?launch` / `?fork` / `?focus` are read and stripped. It sends an `OpenPath` that **its own** server confines as usual.
- No file outside this project is ever opened by this project, so confinement does not change. The link only navigates.

Otherwise the reply stays "path is outside this project".

## 4. Steering Claude to `EnterWorktree`: documentation only

roost writes nothing into a project's CLAUDE.md: the hard constraint limits it to its two owned hook commands in `settings.local.json`. `docs/deploy.md`, next to the setting, adds a suggested line for a project's CLAUDE.md: *create worktrees with the EnterWorktree tool, so they land in `.claude/worktrees/` and roost can ask and follow*.

## CLAUDE.md changes

- **Re-homing bullet:** a terminal is re-homed only on positive evidence that its own Claude process moved. That means the watcher seeing its cwd enter the worktree it asked for, or the user's "Move tab there". Re-homing on the hook is named as the vulnerability-not-cleanup: a subagent's worktree request is indistinguishable from the parent's.
- **Settings-file bullet:** unchanged. The hook still exits 0 with a path exactly when a worktree was created.

## Not in this slice

- #158's correctness, UX, cost and test items. Those are separate slices.
- Banners for removed worktrees (#157).
- A browser that connects later than the move or the banner sees the result, the moved tab or the worktree in the switcher, but not the notice.

## Testing

Each test is written to fail with its code reverted, and is watched doing so.

**Rust (`cargo test -- --test-threads=1`):**
- `approve_no_longer_rehomes`: approving an ask creates the worktree and replies `Path`. The record reads `Here`, and an expectation exists.
- `an_expected_move_rehomes_without_an_offer`:
  - setup: a fake `/proc` whose Claude for `(home, term3)` has cwd inside the expected `.claude/worktrees/worktree-x`;
  - one watcher step;
  - expect: the record is `Elsewhere(owner)`, no `ClaudeElsewhere` offer was sent, and the expectation is gone.
  - Use real git and two hubs.
- `an_expectation_without_a_move_expires_and_moves_nothing` (the subagent case): the cwd stays in the main checkout, and the window is advanced with an injectable clock. Expect the record to stay `Here`, the expectation to be removed, and no offer.
- `a_late_move_gets_the_offer_not_an_automatic_move`: the move happens after expiry, so expect the existing offer.
- `an_expected_move_into_a_different_worktree_is_offered_not_taken`.
- `a_plain_git_worktree_add_is_announced_once`:
  - real git: `git worktree add ../sibling` from the main checkout;
  - drive the watcher's classify, then the hub's confirm step;
  - expect: exactly one `WorktreeAppeared` with the right `branch` and `url`, a second event produces no banner, and a worktree that existed when the watcher started is never announced.
- `a_worktree_roost_made_is_not_announced`.
- `an_unanswered_git_list_changes_nothing`.
- `project_for_abs`:
  - a sibling repo under the root gives its URL;
  - a nested path inside a repo gives the repo plus `rel`;
  - a path under no root gives `None`;
  - an unreadable ancestor gives `None`;
  - a `.claude/worktrees/<name>` path is accepted, and another dot segment is not.
- `a_terminal_link_into_another_project_offers_it` and `a_link_under_no_project_is_still_outside`.

**Browser (`tests/browser/`):**
- Extend `worktree-rehome.mjs`. Approve, then the fake Claude `chdir`s: the terminal follows within one tick. Approve, then the fake does not `chdir` (a subagent): the tab stays on the main page.
- A new case: `git worktree add` at a sibling path while the page is open shows the banner with a working link.
- A new case: clicking a terminal link into another project shows "That's in …", and its link opens that project at the file.
