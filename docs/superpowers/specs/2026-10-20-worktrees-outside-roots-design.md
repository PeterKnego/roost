# Worktrees outside roost's roots: allow one in place, and keep the banner

This is the design for issue #188. It follows #158's first slice (PR #181).

## The miss
On 2026-10-19 a Claude in `ultima_cluster` ran a plain `git worktree add ~/scratch/wt-wire-level` from the main checkout and committed there. It never entered the worktree. Roost offered the user nothing usable:

- **No hook.** A plain `git worktree add` fires no `WorktreeCreate`.
- **No access.** `~/scratch` is under neither root (`~/ultima:~/projects`). The worktree switcher listed it, greyed and unclickable ("outside roost's roots — cannot be opened", `render.rs` around :1349). It is unopenable by design: a cloned repository's worktree metadata can point a worktree anywhere, so confinement refuses any path under no root.
- **The banner did not last.** §2's `WorktreeAppeared` shows only to a page connected at that moment and auto-dismisses after 8 s. Nothing logged it.
- **Where it came from.** `ultima_cluster/CLAUDE.md` tells Claude to write scratch artifacts under `$HOME/scratch/`. Steering that is the project's job; roost writes nothing to a project's CLAUDE.md.

## Decisions (2026-10-20, with the user)
1. **Open in place, after the user allows it.** The worktree is not moved. Moving it would invalidate the path the Claude that made it still uses.
2. **One worktree per allow.** An allow covers one exact path, pinned by its directory identity `(dev, ino, birth time)`, and only while git still lists it as a worktree of a roost project. There is no directory-wide allow.
3. **The URL sits under its parent:** `<parent>/.claude/outside/<name>`. It is resolved only through roost's own record, never through that path on disk.
4. **The "appeared" banner persists until dismissed.** Dismissal is per project, not per browser.

## 1. The allow record and resolution

**Recording is the only way in.**
- An outside, not-yet-allowed worktree's row gets an **Allow…** action. It opens an in-page confirm dialog: "Open `<path>` (`<branch>`) in roost? It is outside roost's roots."
- Confirming sends the websocket intent `AllowOutsideWorktree { parent, path }`. There is no HTTP. `parent` is the row's parent project, carried explicitly: the switcher sits in every page's header, including sibling worktrees' pages, so the sending page is not necessarily the parent. Nothing else about the sender matters, because every check below is made against `parent` itself.
- Off every lock, the hub checks three things:
  - `parent` resolves under the roots, and `git worktree list` of it reports `path` as a non-main worktree. An empty list is git not answering: refuse, writing nothing.
  - `path` is absolute and equals its own canonicalisation, i.e. no symlink anywhere in it, and it is a directory.
  - `name` passes `wtname::check`. It is the basename sanitised — every byte outside `[A-Za-z0-9_-]` becomes `-`, a leading run that is not alphanumeric is trimmed, `wt` if nothing is left — truncated so that it and any `-2`, `-3`, … suffix (appended if a record for this parent already uses that name) fit in 32 bytes. Sanitising, not refusing: the directory cannot be renamed to suit roost without breaking the path of the Claude working in it.
- If all three hold, the hub writes `state/outside/<parent key>/<name>`: atomic (`wsstate::tmp_suffix` temp, then `hard_link`; `rename` only when rewriting a stale record), mode 0600. The file holds two lines: the canonical path, and `dev ino btime_secs btime_nsec`; an unreadable birth time refuses the allow and fails the resolve.
- It then refreshes the parent hub, so the row turns clickable, and replies to the sender with the new URL.

The record lives only in roost's state directory, never in a repository or in project config. A cloned repo therefore cannot allow itself.

**Resolution.**
- `projects::resolve_project` gains one arm, checked before anything else, for a URL of exactly the shape `<parent>/.claude/outside/<name>`, where `<parent>` is any valid project URL and `<name>` passes `wtname::valid`. The arm:
  - reads `state/outside/<storage_key(parent)>/<name>`;
  - takes `metadata` of the recorded path;
  - returns that path only if it is a directory whose `(dev, ino, birth time)` equals the recorded identity. An unreadable birth time refuses, the same as a mismatch; it never falls back to `(dev, ino)`, which ext4 reuses immediately after a delete.
- Anything else returns `None`, and the URL is **not** tried against disk: a missing record, an unreadable or damaged record, a missing directory, a mismatch, or any stat error.
- So a `<parent>/.claude/outside/<name>` directory planted in a repository is never consulted. The existing dot-segment rule would refuse it in any case.
- The arm is fork-free (two file reads and one stat), so it is safe under the hub and REGISTRY locks, like `resolve_recorded_project`.

**Revocation.** Losing access destroys nothing, so every doubt fails closed.
- An identity mismatch, or a directory that is gone, resolves to `None`.
- A **Revoke** action on an allowed row sends `RevokeOutsideWorktree { parent, name }`, which deletes the record.
- When a listing of the parent's worktrees comes back non-empty without the recorded path, the record is deleted. This happens in the switcher, the overview, or the hub's worktree confirm. An empty listing changes nothing.
- Revocation never touches sessions, layout or `.base` under that URL. The page stops resolving, and a live session stays alive. #178 and #182 keep it from being reaped, since `.origin` records the real path.

**Inside an allowed worktree, confinement is unchanged.** The project directory is the boundary, and `safe_resolve` and `safe_resolve_parent` confine every file path to it, as for any project.

## 2. The switcher, the overview and the page

**Listing.**
- The header switcher's rows come from `registry::known_projects`' grouping; the overview's come from `registry::worktree_rows`. Both already give a path under no root `reachable: false`.
- Before doing that, both now look up an allow record for the parent and that path. If the record matches (same canonical path, same identity), the row becomes:
  - `url` = the virtual URL;
  - `reachable: true`;
  - a new field `outside: Some(path)`.
- Both listings already call `worktree::list` off-lock.

**Rows.**

| State | Row |
|---|---|
| Not allowed | As today (dimmed, unclickable, tooltip), plus an **Allow…** button |
| Allowed | A normal clickable worktree row linking to the virtual URL, with an "outside" marker. The tooltip shows the real path. Claude/dirty/ahead chips are computed on the real path. **Revoke** button. No remove control. |
| Record no longer matches | Shown as not allowed. The record is deleted on the next non-empty listing that lacks the path. |

**The page** at `<parent>/.claude/outside/<name>` is an ordinary worktree page:
- tree, files, search and terminals are confined to the real directory;
- a new terminal's cwd is that directory;
- the IDE listener's workspace is that directory, so a Claude started there gets Alt+K and openDiff.
- The page header carries a persistent note: "outside roost's roots: `<path>`".

**Storage** follows the usual per-URL rules: the storage key is the percent-encoded virtual URL, covering sessions, layout and `.base`. `.origin` records the real path. A rename of the directory is protected from reaping by #178 and #182. It is not followed, because the new path is outside the roots (`Unfollowable`). After a rename the record no longer matches, and the user can allow the new path again.

## 3. The banner that stays until dismissed

**Recording.**
- The hub's worktree confirm step (#181 §2) finds new worktrees, with the existing exclusions: roost-made, present at watcher start, and an empty git answer.
- For each one it appends `{path, branch}` to `state/appeared/<project key>`. The write is atomic and 0600.
- The list holds at most 16 entries per project. Over the cap, the oldest entry is dropped and one log line says so.
- Each appearance also logs `roost: <project>: worktree <path> (<branch>) appeared`, with `, outside roost's roots` appended when that applies.

**Showing.**
- The hub's `State` snapshot carries the list, so a page that connects later, or a page after a restart, sees every entry.
- Each entry renders as a banner that does not auto-dismiss. It is not built with `showBanner` (#181's M2 fix): that helper auto-dismisses after 8 s and dedupes by text, both wrong for a banner meant to persist, and two unrelated worktrees sharing a basename must not merge into one. Instead these render in their own `#appeared` container, keyed by path (`appearedBoxes` in `static/app.js`), reconciled against the hub's `State` on every update rather than rebuilt from scratch, so a click on a button mid-update is never yanked out from under it.
- Banner text depends on where the worktree is:
  - under the roots, or outside and allowed: "worktree `<name>` (`<branch>`) appeared — open", linking to its page;
  - outside and not allowed: "… appeared, outside roost's roots — Allow…", which opens the §1 dialog.
- All text is set with `textContent`, and every href is built with `projectPath`.

**Dismissing.** An entry leaves the list, and the new state is broadcast to every browser on the project, when:
- **Dismiss** is clicked (`DismissAppeared { path }`);
- the worktree's own page is opened;
- the worktree drops out of a non-empty `git worktree list`.

**Not covered.** A worktree made while roost is stopped is never announced, because the watcher's startup seed counts it as known. It still appears in the switcher.

## 4. Out of scope
- **Re-homing into an outside worktree.** `home::owner_shaped` accepts only `<home>/.claude/worktrees/<dir>`. So "Move tab there" and #158's move-on-evidence never target an allowed outside worktree, and a main-checkout Claude that `cd`s into one keeps its tab at home. It can be added later by widening `owner_shaped` to the virtual shape, gated on a matching record.
- **Removing an outside worktree from roost.** It stays a manual `git worktree remove`.
- **A directory-wide allow,** and anything that is not a git worktree of a project under the roots.
- **Following a rename** of an outside worktree. See §2, Storage.
- **#183's wrong-owner chains and #185's minors.** They stay separate.

## Caps
- The appeared list holds at most 16 entries per project (`APPEARED_CAP`).
- At most 32 allow records per parent (`OUTSIDE_CAP`). An allow over the cap is refused with a message naming the cap.

Both are constants, not config. Each names itself when it fires.

## CLAUDE.md changes
The "Every filesystem path is confined" bullet gains three things.

**The exception.**
- A project URL resolves outside the roots only through a roost-written allow record (`state/outside/<parent key>/<name>`).
- The record is written after a user confirmation, on git's own listing of the path as a worktree of a project under the roots.
- It is checked on every resolve against the directory's recorded `(dev, ino, birth time)`, fork-free. An unreadable birth time refuses the allow and fails the resolve.

**Why it is narrow.**
- A cloned repository can point a worktree anywhere.
- The record lives in roost's state, never in a repository or in project config, and it names one directory by identity.
- Anything that does not match resolves to nothing.

**What must never be made "consistent".** Each of these is the vulnerability, not the cleanup:
- resolving the virtual path through disk;
- accepting a record without an identity match;
- letting project config or a repository write records.

## Testing
Every new test is watched failing with its code reverted, and the failure is recorded in the test's comment. Run with `cargo test -- --test-threads=1`.

**Rust:**
- **Allow:** real git, a worktree outside the roots. `AllowOutsideWorktree` writes the record. The virtual URL resolves to the real directory. A second connection sees the refreshed row.
- **Refusals, each one writing nothing:**
  - a path git does not list for this parent;
  - an empty git listing;
  - a path through a symlink;
  - (an invalid basename is no longer a refusal: it is sanitised into a name, §1);
  - over `OUTSIDE_CAP`.
- **Resolution:**
  - a planted `<parent>/.claude/outside/<name>` directory with no record resolves to `None`;
  - a record whose directory was replaced (same path, new inode) resolves to `None`;
  - a damaged record resolves to `None`;
  - an unreadable record resolves to `None`.
- **Revocation:**
  - `RevokeOutsideWorktree` deletes the record;
  - a non-empty listing without the path deletes it;
  - an empty listing keeps it;
  - a live session under the URL survives revocation.
- **Confinement:** a file path escaping the allowed directory is refused by `safe_resolve`. This is the existing behaviour, pinned for this kind of project.
- **Appeared:**
  - an appearance persists across a new hub;
  - `DismissAppeared` removes the entry and both connections receive the new state;
  - opening the worktree's page clears its entry;
  - a non-empty listing without the worktree clears it;
  - the cap drops the oldest entry and logs.

**Browser (`tests/browser/`):**
- a plain `git worktree add` outside the roots shows a banner that survives a page reload;
- **Allow…** confirms, the row becomes clickable, the page opens, and a new terminal's `pwd -P` is the real directory;
- **Revoke** makes the row dim again.
