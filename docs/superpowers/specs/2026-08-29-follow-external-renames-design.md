# Following an external rename

A file renamed from a terminal — `mv`, `git mv`, a Claude's own edit tool —
leaves every tab and buffer addressing the old path. Since
`hub::file_vanished` (2026-08-28) that state is at least honest: the tab is
demoted to Preview and the pane says "not found", and a dirty buffer keeps its
unsaved text and is marked stale. This spec is about the better answer: move
the tab to where the file went.

## What the filesystem actually tells us

Measured on the deploy host (Linux 7.0, notify 8.2), watching a temp tree and
performing four moves. Raw `notify` events, one line each:

| Move | Events delivered |
|---|---|
| `before.rs` → `after.rs` (same dir) | `Name(From)` + `Name(To)` + **`Name(Both) paths=[before.rs, after.rs]`**, all `tracker=Some(902195)` |
| `after.rs` → `sub/moved.rs` (watched subdir) | same three, one tracker |
| `sub/moved.rs` → outside the tree | **`Name(From)` only** |
| outside the tree → `arrived.rs` | **`Name(To)` only** |

Three things follow, and they decide the whole design:

1. **The kernel does the pairing, and `notify` already surfaces it.** inotify
   gives both halves of a rename the same cookie; `notify`'s inotify backend
   matches them itself and synthesises a third event, `Name(Both)`, carrying
   *both paths in one event* (`notify-8.2.0/src/inotify.rs:251-261`). There is
   nothing to correlate and nothing to guess.
2. **The unpaired cases are exactly the ones that must not be followed.** A
   move out of the tree arrives as `From` with no partner; a move in arrives as
   `To` with no partner. Those are a deletion and a creation, which is what
   they are to a project that cannot see the other end.
3. So "did this file move, and where to?" is answered by **positive evidence
   from the kernel**, not by inference. That is the standard CLAUDE.md sets for
   anything that rewrites user-visible state.

## The two crates, and why neither is needed

**`notify-debouncer-full`** does exactly this correlation
(`lib.rs:399`: `if trackers_match || file_ids_match`) — but the tracker half is
what raw `notify` already gave us above, and this project removed the debouncer
for a confirmed defect: under FSEvents' coalescing on macOS its
rename-correlation queue silently drops `Remove` events, so a deleted file
never reached the UI (the reason is recorded at `watch.rs:spawn`). Re-adopting
it to obtain a pairing we already have would trade a known bug for a
convenience.

**`file-id`** is the *other* half of that `||`: an inode (Unix) or file index
(Windows) used to match a rename whose two halves cannot be paired by cookie.
That is a real gap and it is macOS-shaped — `notify-8.2.0/src/fsevent.rs:192`
says it outright: *"FSEvents provides no mechanism to associate the old and new
sides of a rename event"*, and emits `RenameMode::Any` with no tracker. But
identity matching needs the old file's id captured **before** it was renamed,
which means a cache, and in this codebase every save is an atomic
temp-file-plus-rename (`fileops::atomic_write`) — so a cached inode goes stale
on every save of every open file. The cache would need refreshing at three call
sites, and a missed refresh silently disables the feature rather than failing
loudly.

**Decision: neither.** Use `Name(Both)` from raw `notify`. Zero new
dependencies, no cache, no heuristic. On macOS a rename keeps today's
behaviour — the tab demotes and says "not found" — which is correct, just not
clever. The deploy host is Linux; macOS is a development platform here.

## Design

`watch.rs`, in the debounced batch, before anything else:

- Collect `(old, new)` from every `Modify(Name(Both))` whose **both** paths
  strip to a rel under the project base. A pair with one end outside the
  project never forms, so it falls through to the existing vanish/create paths.
- Apply the renames **before** computing `open`, so the rest of the batch
  classifies against where the file is now.

`hub::follow_rename(old, new)`:

- Do nothing unless a tab or buffer actually references `old` — otherwise every
  `git` index rewrite and every atomic save (both of which are renames) would
  bump the workspace version and persist.
- Reuse `rekey_after_rename`, which already exists for resh's own rename intent
  and already moves a whole subtree by `/`-boundary prefix — so renaming a
  *directory* moves every tab under it for free.
- Broadcast `State`, then — only for a dirty buffer — `BufferText` at the new
  rel. Without that second event the editor re-mounts at the new name with
  nothing to seed from (app.js keys `texts` by rel) and the user's unsaved work
  vanishes from the screen: the same empty-editor failure this line of work
  started from, arriving through the rename door. Verified by removing it —
  `renamed.mjs` then reports the editor as `""`.

  The order between the two is *not* load-bearing, though it looks like it
  should be: app.js prunes `texts` against every State's buffer list, but the
  State that moves the tab lists the buffer under its new rel, so a
  `BufferText` sent first survives it. Measured, not assumed — sending it first
  leaves the browser test green.

One correctness fix falls out. `file_changed_externally` marks a dirty buffer
stale on *any* external event for its path. After a rename the content has not
changed, so the buffer would be flagged as diverged from a file it matches
exactly. Guard the flag on `disk_hash != base_hash`: a file that matches what
the buffer was based on has not diverged from it, whatever happened to it.

## Not in scope

- **macOS rename following** — needs `file-id` and a cache; see above.
- **Cross-filesystem moves.** `mv` between filesystems is copy-then-delete:
  different inode, no cookie, and correctly not a rename to anything watching.
- **Two tabs on one file.** An external `mv a.rs b.rs` with *both* open leaves
  two tabs addressing `b.rs`. resh's own rename cannot produce this
  (`fileops::rename` refuses an existing destination). Collapsing them means
  closing a tab on filesystem evidence, which is the one thing
  `file_vanished` deliberately does not do; left as-is.
