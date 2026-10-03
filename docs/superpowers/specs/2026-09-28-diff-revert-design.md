# Reverting a change from the Changes pane and the Diff tab

*2026-09-28. Status: implemented on `diff-revert`. Issue
[#125](https://github.com/PeterKnego/roost/issues/125). The issue listed four
open questions; the answers below were settled in review, and the facts that
change the design were found by probing a scratch repository, not assumed.*

## What is wrong today

Right-clicking in the Changes pane or a Diff tab opens the **file tree's**
menu. `mountTab` calls `wireFragment` on every fetched fragment, Diff
included, so blank space offers *New file… / New folder…* at the project
root. Each change row is an `a.file[data-rel]`, so `wireFileLinks` gives it
the full file menu, **Delete** included. In a list of changes, "Delete" reads
as "drop this change" and deletes the file.

And there is no way to throw a change away without a terminal.

## Decisions

1. **The file menu stays on the tree.** `fileMenu` is bound to tree rows,
   markdown `mdlink`s and blank tree space only. The Changes pane and Diff
   tabs get `gitMenu` instead, split by the same `closest("ul.tree")` test the
   click handler already uses.
2. **`gitMenu` has one command: Revert.** *Revert…* on a change row or inside
   a single-file Diff tab; *Revert all…* on the "full diff" row, the full Diff
   tab and blank space in Changes.
3. **A revert never deletes a file.** It touches only paths with a version in
   `HEAD` to return to. Untracked (`??`), added (`A`) and a rename's new path
   are excluded; a deleted file (`D`) is included, since restoring it creates
   and loses nothing.
4. **Only paths inside the project.** In a nested project, git lists files
   outside it too. Those are excluded, confined by stripping the project's
   prefix (see *Deviation* below).
5. **Every revert is saved to `git stash` first**, by the same git command
   that reverts (see *Mechanism*), and the result names the stash entry.
6. **Always confirmed**, with the dialog showing exactly what is lost and
   focus on Cancel.

## Facts found by probing (git 2.53)

- **Porcelain paths depend on `-z`.** Without it they are relative to git's
  cwd: `git -C sub status --porcelain=v2` lists a root file as `../top.txt`
  (the Changes list uses this form). With `-z` they are relative to the
  **repository root**: the same call lists `top.txt` and `sub/in.txt`. The
  plan uses `-z`, so a nested project strips its own prefix
  (`git rev-parse --show-prefix`) and counts a path without it as outside.
  This is why decision 4 is needed at all: without it a nested project's
  revert takes the parent's changes.
- **`stash push` must run from the toplevel.** `git -C sub
  --literal-pathspecs stash push -- in.txt` prints "Saved working
  directory…", then fails `pathspec 'in.txt' did not match any files`,
  exit 1, with nothing reverted and an entry left behind. The push therefore
  runs in the toplevel with prefix + path.
- **A pathspec after `--` is a pattern.** Reverting a file literally named
  `a*` also matches `ab`. `git --literal-pathspecs` fixes it; with it, the
  probe reverted `a*` and left `ab` changed.
- **`git stash push -- <paths>` reverts exactly those paths** (index and
  worktree, back to `HEAD`) and records them as one stash entry, **but not
  atomically**: it saves the entry first and resets the paths afterwards, and
  can fail in between. With a read-only `d/`, pushing `a.txt d/b.txt z.txt`
  printed "Saved working directory…", then `error: unable to write file
  'd/b.txt'`, exit 1, with `a.txt` deleted from the worktree, `z.txt`
  reverted and `d/b.txt` unchanged: the entry held the only copy. A tracked
  file replaced by a directory ("Directory not empty") does the same. It also
  records the rest of the *index* in that entry, so `git stash apply --index`
  can conflict. Plain `git stash apply` restored the reverted content exactly
  in the probe; it does not restore the staged/unstaged split. That is
  the stated recovery, and its limit is written down here rather than
  discovered by a user.

## UI

### The menu

| Right-click on | Item |
|---|---|
| An eligible change row, or inside a single-file Diff tab | **Revert…** |
| "full diff", the full Diff tab, blank space in Changes | **Revert all…** |
| An ineligible row | **Revert…** disabled, with the reason on a second line: *untracked, no committed version* / *added, no committed version* / *renamed* / *outside this project* |

Disabled rather than absent: a menu silently lacking the item does not answer
"why can't I revert this", which is CLAUDE.md's rule that a check which
skipped something says so. `askMenu` items gain `disabled` and `hint`; a
disabled item is not clickable and arrow keys skip it.

`render::changes_fragment` adds `data-xy` to each row so the client can decide
the display. That decision is a hint only; the server decides again.

### The confirmation

`askChoice` with `focus: "cancel"`, so Enter destroys nothing.

- **Title:** *Discard changes to `src/a.rs`?* or *Discard changes to 3 files?*
- **Lines:** *They are saved to git stash first; `git stash apply` brings them
  back.* Then, when they apply: *This includes staged changes.* /
  *2 files are not included: 1 untracked, 1 outside this project.* /
  *`a.rs` has unsaved edits in roost; they are discarded too.*
- **Detail:** one file: its diff; all: the path and XY list. Both are
  server-rendered and escaped, the only thing `detailHtml` accepts.
- **One button:** *Discard changes to 3 files*, beside Cancel.

After a revert a banner says the outcome (see *Results*).

## Protocol

Two intents, both websocket (the HTTP surface stays GET plus the two upload
POSTs).

**`RevertPreview { rel: Option<String> }`**, `None` for all. The server
answers the requester only (`send_to`):

```
Event::RevertPlan {
  rel,
  paths: [{ path, xy }],
  staged: bool,                       // any path has an index-side change
  skipped: { untracked, added, renamed, outside },
  dirty: [path],                      // paths with an unsaved roost buffer
  detail_html,
  token,                              // hash of sorted (path, xy) + their diff text
}
```

An empty `paths` shows the banner *Nothing to revert: …* with the skipped
counts, and no dialog.

**`Revert { rel, token, discard_buffers: Vec<String> }`**, sent only by the
dialog's button. The server rebuilds the plan and proceeds only if the token
matches, every path in `plan.dirty` is in `discard_buffers`, and `paths` is
not empty.

The token is the hash `base_hash` already uses (the plan confirms which).
It is what makes "revert only what the user saw" enforceable: Claude editing
the file between the look and the click changes the diff, and so the token.

## Building a plan

One function, used by both intents. Every failure is its own refusal.

1. `git status --porcelain=v2 -z` in the project directory, parsed by a `-z`
   variant of `parse_status` (the display parser splits lines and would
   misread a quoted or newline-bearing name). A spawn failure, a non-zero exit
   or a timeout: refuse. Exit 0 with no output is a clean tree, a real answer,
   and it yields an empty plan. **An empty plan is refused by its own guard,
   `plan.paths.is_empty()` in `run_revert`, and the token does not cover it.**
   A tree that went clean after a non-empty preview is refused by the token;
   but when the preview was empty too (a nested project whose only change is
   in its parent, say) both tokens hash the same empty basis and match. The
   guard is then all that stops `stash push --` with no paths, which stashes
   the whole repository, the parent's files included. It looks redundant next
   to the token check and is not: `an_empty_plan_is_never_pushed` fails
   without it.
   Any unmerged (`u`) entry: refuse.
2. A merge, rebase, cherry-pick or revert in progress: refuse. Checked from
   the git dir with `symlink_metadata`: `NotFound` is absent, any other error
   is *cannot tell*, which also refuses.
3. Per path: strip the project's prefix (a path without it goes to
   `outside`); `??`, `A`, and a rename's new path to their buckets. A
   single-file preview whose `rel` matches no status entry at all refuses:
   *<rel> is not in git status; refresh the Changes list.* "Matched nothing"
   is not "nothing to revert". The Changes list's `gitio::status` runs with
   `core.quotePath=false` so that a non-ASCII name it shows is the raw name
   the `-z` plan matches.

**Deviation: confinement is by prefix strip, not `safe_resolve`.** Paths
come from git's tree entries, root-relative under `-z`: git refuses a path
beyond a symlink (it tracks the link itself), so there is no link to
resolve, and a deleted file, or a whole deleted directory, has no parent for
`safe_resolve_parent` to canonicalise. The lexical strip is exact for this
input; `safe_resolve` would refuse legitimate deletions.
4. `git diff HEAD -- <paths>` under `--literal-pathspecs`, for the token and
   (one file) the detail.

## Mechanism

Both intents are routed by `wsconn` away from the hub lock, like `Search` and
`CloseProject`: a plan is several git calls with a deadline of up to 15 s
each, the push has none (below), and CLAUDE.md forbids blocking I/O under a
lock every socket on the project needs.

1. Under the hub lock, briefly: read which buffers are dirty (memory only).
2. Unlocked: build the plan, check it, read the top stash entry
   (`git stash list -1 --format=%H%x09%s`; a failure refuses), then, in the
   toplevel, `git --literal-pathspecs stash push -m "roost revert: N files"
   -- <prefix+paths>`, then read the top entry again.
3. Re-lock: reset each buffer that was dirty at the check and named in
   `discard_buffers` to its file on disk, send
   `Event::Reverted { rel, ok, msg, stale }` to the requester, broadcast a
   snapshot. The watcher refreshes Changes.

A buffer that turns dirty between steps 1 and 3 was never named, so it is not
touched; its file changed on disk under it, which is the existing conflict
path for every outside write. Nothing is discarded without having been shown.

The revert is one git command, but not an atomic one: `stash push` can stop
partway after saving (see *Facts*). So the push has **no kill deadline**
(killing it mid-way would make that half-state), and what happened is read
from the stash list, never from git's wording (translated) or exit status
(silent on how far it got). The entry is **ours** when the top hash changed
**and** its subject ends with the exact label: a new hash alone could be a
concurrent stash, the label alone an older roost revert. A push that stopped
partway is reported as such and points at `git stash apply`; nothing ever
suggests `git stash drop`, which there deletes the only copy.

One command was still chosen over `stash create` + `store` + `restore`
(three steps, and a snapshot of the *whole* worktree, so applying it would
bring back changes the user never reverted) and over roost copying file
contents into its own state directory (no git-native recovery).

## Results

| Situation | Message | Git touched |
|---|---|---|
| Status failed to run, exited non-zero, or timed out | *Could not read git status: <reason>. Nothing was reverted.* | no |
| Merge/rebase/cherry-pick/revert in progress, or an unmerged entry | *A merge is in progress; finish or abort it first. Nothing was reverted.* | no |
| Could not tell whether one is in progress | *Could not check for a merge in progress: <reason>. Nothing was reverted.* | no |
| Token mismatch | *These changes changed since you looked; review them again.* (client reopens the preview) | no |
| A dirty buffer not named | *`a.rs` gained unsaved edits; review again.* | no |
| Nothing eligible | *Nothing to revert: 2 untracked, 1 outside this project.* | no |
| `rel` matches no status entry | *<rel> is not in git status; refresh the Changes list. Nothing was reverted.* | no |
| Could not read the stash list before pushing | *Could not read the stash list: <reason>. Nothing was reverted.* | no |
| Push failed, no new entry | *git refused: <first stderr line>. Nothing was reverted.* | no |
| Push failed, a new entry that is ours | *git stopped partway after saving the changes as stash@{0} ("roost revert: 3 files"): some files may already be reverted — check `git status`. `git stash apply` brings them back.* | partly |
| Push failed, a new entry that is not ours | *git refused: <first stderr line>. The stash changed while this ran, so whether this revert saved anything could not be told — see `git stash list` before dropping anything.* | unknown |
| Push failed, stash list unreadable after | *git refused: <first stderr line>. Whether a stash entry was left could not be checked — see `git stash list` before dropping anything.* | unknown |
| Push succeeded, no new entry | *These changes were already gone. Nothing was reverted.* | no |
| Push succeeded, a new entry that is not ours | *git did not report an error. The stash changed while this ran, …* | unknown |
| Success (a new entry that is ours) | *Reverted 3 files; saved as stash@{0} ("roost revert: 3 files"). `git stash apply` brings them back.* | yes |

Refusals reach only the requester; every one but a stale refusal is also
logged to stderr as `roost: revert in <dir>: <msg>`.

## Testing

Each test must fail with the code it covers reverted. The plan names the
revert check for each.

**Rust**, in `gitio` and `hub`, against real scratch repositories:

- Plan: `??`, `A`, a rename's new path each land in their own bucket; a
  parent's change in a real nested project lands in `outside`; `a*` beside `ab` reverts only
  `a*`; a name with a space and a non-ASCII character survives the `-z`
  parser.
- Refusals, each asserting the message **and** the file unchanged on disk:
  status failure (not "nothing to revert"); a tree cleaned between preview
  and revert (refused as changed, not reported as success); a real `MERGE_HEAD`; a stale token
  (file edited between preview and revert); an unnamed dirty buffer.
- Success: the file matches `HEAD` in index and worktree; `git stash list`
  has exactly one `roost revert: 1 file`; `git stash apply` brings the content
  back; an **unselected** changed file is byte-identical. That last one is what
  tells "reverted this" from "reverted everything".
- Privacy: `RevertPlan` and `Reverted` reach only the requester, with **two**
  subscribers (one cannot tell `send_to` from `broadcast`).
- Lock: while a revert is held inside a slow git (a runner blocked on a
  channel), another connection's `RequestState` is answered. Timed, since a
  deadlock hangs rather than fails.

**Browser**, new `tests/browser/revert.mjs`, real dtach and a real repository:

- A change row's menu has *Revert…* and no *Delete*; a tree row's file menu
  is unchanged.
- An untracked row shows *Revert…* disabled with its reason.
- Cancel leaves the file on disk untouched; confirm reverts it on disk, the
  banner names the stash, the row leaves Changes.
- Enter in the dialog does not discard.
- Right-click inside a Diff tab gives the git menu, not *New file…*.

## Out of scope

Hunk- or line-level revert. Stage, unstage, commit. Reverting untracked or
added files (decision 3). Paths outside the project (decision 4).
