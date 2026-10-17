# Worktrees roost did not make, and a terminal that only moves when its Claude does

This is the first slice of #158, the follow-up to #156. It answers #158's subagent question the way the user decided on 2026-10-17: **the `WorktreeCreate` hook never re-homes a terminal.** It also adds the three items recorded on #158 after a live miss, where a Claude in `ultima_cluster` created `~/ultima/uc-rel` with a plain `git worktree add` and roost showed nothing:

1. the terminal moves only when its Claude does;
2. a worktree that appears from anywhere gets a banner;
3. a terminal link into another project offers to open that project;
4. Claude is steered toward `EnterWorktree`, through documentation only.
5. a renamed project or worktree takes its terminals with it, which builds on #178's fix that stopped the sweep killing them.

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

## 5. A renamed project or worktree takes its terminals with it

Since #178 (PRs #179, #180), the sweep no longer kills a project's sessions when its directory was **moved** rather than deleted. The test it uses: a live session's shell `cwd` still reaches the recorded `(device, inode)` (`registry::moved_not_deleted`). Those sessions survive, but they are stranded:
- their project URL no longer resolves, so `term.rs` refuses the attach (`resolve_project` → `None`);
- no page lists them;
- terminals re-homed *into* the moved worktree are forgotten by the R18 sweep, because git no longer lists the old path, and they fall back to the main checkout.

This section makes them follow the directory. Session identity cannot move (the hard constraint), so this is the re-home alias again, with one new piece of evidence-backed state.

**The move record.**
- When `moved_not_deleted` finds the inode, it also has the shell's current path to it: the directory carrying the inode on that cwd's ancestor chain.
- If that path resolves as a project under the roots (`projects::resolve_project(roots, new_url)` returns that same directory), the sweep writes `sock/<old key>/.moved-to` containing `new_url`. The write is atomic, 0600, and in the key directory like `.origin`.
- A path under no root writes nothing: the sessions stay alive and stranded, exactly as #178 leaves them, and the sweep logs it.
- `.moved-to` is rewritten if a later pass finds a different path, for example after a second rename.

**Following.** On writing `.moved-to`, for each live session `n` of the old key:
- **(a)** `home::record(old_url, n, new_url)`, so every session of the moved project becomes a resident of the new one. That reuses all of #156's resident machinery: tabs with `home`, `session_homes`, Close Project, the R16 gate, notices, IDE delivery and the overview.
- **(b)** For every re-home record whose owner is `old_url` (main-checkout terminals re-homed into the moved worktree), the owner is rewritten to `new_url`.
- **(c)** The old key's `.base` (`state/worktrees/<old key>.base`) is copied to the new key if the new key has none, so the removal gate's "ahead" still knows the base.
- **(d)** The old layout (`state/<old key>.json`) is copied to the new key if the new key has none. Its file tabs keep working, because a rename does not change paths relative to the project root.
- **(e)** The new project's hub, if open, gets `Event::ProjectMoved { from, to }`, shown as a transient banner: "`worktree-a` was renamed to `worktree-b`; its terminals followed". The old URL's open pages get the same event with a link to the new page.

Every write is atomic, and all of it runs on the sweep's thread with no hub lock held.

**Two narrow widenings, both gated on `.moved-to`:**
- **`home::resolves(home, owner)`.** It currently accepts only `<home>/.claude/worktrees/<dir>`. It also accepts an owner equal to the content of `sock/<home key>/.moved-to`, when that owner resolves. Nothing else widens: a record naming an arbitrary owner without a matching `.moved-to` still does not resolve.
- **`term.rs`.** When `resolve_project(project)` fails and `sock/<key>/.moved-to` names a URL that resolves, the websocket may attach to an **existing** session only. `session::attach` takes its `Probe` path, and a `Spawn` decision is refused with "project was renamed to `<new>`". The resolved new directory is used where `attach` needs a directory. No new shell is ever started under a key whose directory is gone.

**Ordering with the R18 sweep.** `sweep_gone_owners` currently forgets a record when git no longer lists its owner. Before forgetting, it now checks `sock/<owner key>/.moved-to`:
- if present and resolving, it rewrites the record's owner (step b) instead of forgetting it;
- the compare-before-forget re-read stays.

**Cleanup.** Nothing new. When the old key's last session ends, its socket is reaped as before, and the key directory, `.origin`, `.origin-ino` and `.moved-to` go with it. The copied `.base` and layout belong to the new key from then on.

**Out of scope.**
- A directory moved outside every root.
- A rename while roost is stopped and no session survives it: there is no shell, so there is no evidence and nothing to follow.
- Renaming the *branch* only (`git branch -m`), which already works: the switcher label changes, and nothing is keyed by branch.

## CLAUDE.md changes

- **Re-homing bullet:** a terminal is re-homed only on positive evidence that its own Claude process moved. That means the watcher seeing its cwd enter the worktree it asked for, or the user's "Move tab there". Re-homing on the hook is named as the vulnerability-not-cleanup: a subagent's worktree request is indistinguishable from the parent's.
- **Settings-file bullet:** unchanged. The hook still exits 0 with a path exactly when a worktree was created.
- **Re-homing bullet, second part:** a renamed project's sessions follow it only through a `.moved-to` record, which is written solely on #178's inode evidence. `home::resolves` and `term.rs` widen only for an owner that a `.moved-to` names. A widened `term.rs` attaches to existing sessions and never spawns under a key whose directory is gone.

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

- `a_renamed_worktree_takes_its_sessions_with_it`: real git, `git worktree move`, real dtach, a session started inside the worktree, then one sweep. Expect:
  - `.moved-to` names the new URL;
  - the session is a resident of the new project (listed by `owned_live_names(new)`);
  - an attach through the old URL reaches the existing session;
  - an attach that would spawn under the old key is refused with the "renamed" message.
- `a_rehomed_terminal_follows_its_owners_rename`: a main-checkout terminal re-homed into worktree A; A is moved to B. Expect the record's owner to be B, not forgotten, even though git no longer lists A.
- `a_move_outside_every_root_writes_nothing`: the session survives (#178), and no `.moved-to` is written.
- `a_record_naming_an_unrelated_owner_still_does_not_resolve` (the widening is gated): without `.moved-to`, `home::resolves` refuses an owner outside `.claude/worktrees`.
- `the_old_layout_and_base_are_copied_only_when_the_new_key_has_none`.

**Browser (`tests/browser/`):**
- Extend `worktree-rehome.mjs`. Approve, then the fake Claude `chdir`s: the terminal follows within one tick. Approve, then the fake does not `chdir` (a subagent): the tab stays on the main page.
- A new case: `git worktree add` at a sibling path while the page is open shows the banner with a working link.
- A new case: clicking a terminal link into another project shows "That's in …", and its link opens that project at the file.
- A new case: with a terminal open on a worktree page, `git worktree move` it. Within one sweep the banner names the new worktree, and the terminal is usable from the new page with the same PID.
