# Making a project from the front page

*2026-09-13. Status: designed, not implemented. Reported by Dean: "ko klikneš
add button v seznamu projektov, bi mogel sam sprejet ime pa kreirat folder, pa
ga ne." Decisions from that conversation, including one that overrides the
first option offered.*

## What happens today

The front page has exactly one `+`, in the header beside the roots list,
titled *add a project root*. It sends `AddRoot { path }` on `/ws/_roots`, and
`roots::add_root` refuses anything that is not an **absolute path to a
directory that already exists** (`src/roots.rs`, the `is_absolute` check and
the `NotFound` arm below it).

So typing a name — the obvious thing to do when looking at a list of projects
and wanting another one — produces ``\`mqtt-bridge\` is not an absolute path``.
There is no control anywhere in roost that creates a project.

## What it becomes

**One field, and what you type decides which of two things happens.**

| You type | roost does |
|---|---|
| an absolute path that exists | adds it as a root — unchanged |
| an absolute path that does not exist | offers to create it, then adds it as a root |
| anything relative | creates it as a **project** under a root, and `git init`s it |

Dean's words settle the split: *"what i type is relative path to roost root
dir"*. A relative path has no meaning as a root — a root is a place on disk
roost scans, and there is nothing for it to be relative to — so the two
readings cannot collide, which is what makes one field honest rather than
clever.

The first design offered a second `+` on the Projects pane for this and kept
the header one as it was. Rejected in favour of the above, and recorded because
it is the option the question led with.

### Which root, when there is more than one

**One root: it is used, with no question. More than one: the dialog asks.**

Never a silent choice about where a folder lands on disk. The client already
has the list — it renders it in the header — so this costs one `askChoice`
and no round trip.

The chosen root arrives from the browser and is **re-validated against
`projects::roots()`** before anything is created. The row that offered it is a
hint, not an authorisation; the same rule `RemoveWorktree` and
`claudehist::has` already state about their own ids.

### `git init`, always

roost's project list *is* the set of git repositories under the roots
(`registry::known_projects`), and the Changes pane, the branch chip and
worktrees all assume one. A folder created without it would not appear in the
list it was created from — the feature would look broken in the most confusing
possible way.

If `mkdir` succeeds and `git init` fails, that is reported as what it is:
the directory was created, the repository was not. **The directory is not
removed.** Undoing a create by deleting a directory is precisely the move
CLAUDE.md's table is eleven rows of, and the failure that leaves it there is a
failure to run `git`, which says nothing about what the directory now contains.

## The part that needs care

`roots.rs`'s module doc currently ends: *"Validation reads metadata and
canonicalises; **it never creates, lists or follows into anything**."* This
change makes that false, so the doc changes with it — and the reason it was
worth writing is the reason the confinement below is not optional.

**A relative path from a browser becomes a `mkdir`.** It is confined by
`projects::safe_resolve_parent`, which CLAUDE.md names for exactly this case:
"for creation and rename destinations (it canonicalises the parent and
validates the final component, because the target does not exist yet)".
Reused rather than reimplemented, and that choice carries a deliberate
restriction with it: the *parent* must already exist, so `a/b` works when `a`
does and is refused otherwise. One level at a time, by the primitive this
codebase already trusts.

The absolute case creates too, and is confined by nothing — because there is
nothing to confine it to. A root is by definition an arbitrary place on disk,
and a person typing an absolute path into a field labelled *directory to scan
for projects* has named it. What stands there instead is that it is
**confirmed**: the refusal becomes a question, not an action.

Both creations are `create_dir_all`, and neither follows a symlink into
existence: `safe_resolve_parent` canonicalises the parent, so a symlinked
parent is resolved to where it really points and checked against the root from
there.

### What is refused, and says why

- A relative path when there are **no roots at all**. There is nothing for it
  to be relative to, and the answer is to add an absolute one first — which is
  what the empty-state's own *Add path* button already asks for.
- A relative path that escapes its root (`../elsewhere`).
- A target that already exists. Named, and **not touched** — not adopted as a
  project, not `git init`ed. "It is already there" is a different sentence from
  "I made it", and a `git init` over a directory someone already has is a
  change to a repository this feature was not asked to make.
- Everything `add_root` already refuses, unchanged: `ROOST_ROOTS` in effect, a
  duplicate root, a path that is not UTF-8, a `roots` key that is not a list.

## Protocol

`/ws/_roots` gains a second intent rather than a second socket — it is still
one exchange per connection, and it is still the front page's only write.

```rust
enum RootsIntent {
    AddRoot { path: String },
    // `root` is which root to create under; None is only valid when there is
    // exactly one, and is re-validated either way.
    NewProject { root: Option<String>, rel: String },
}
```

Replies: `{"t":"Roots","roots":[…]}` as now, `{"t":"Project","key":…,"path":…}`
for a created project, `{"t":"Error","msg":…}` for every refusal.

## Testing

The failure mode this repo names as dominant is a test that cannot fail, so:

- **The confinement test must reach the confinement.** A `../escape` whose
  parent does not exist fails with `ENOENT` before `safe_resolve_parent` ever
  compares anything — which is the exact shape CLAUDE.md records as the reason
  "a symlink escape survived review". The fixture has to make the escape
  *possible* and then assert it was refused, and assert on the message.
- **Assert the directory is absent afterwards**, not merely that an `Err` came
  back. A refusal that creates first and reports second is still a refusal.
- **The "already exists" test must assert the existing directory is
  unchanged** — that nothing was `git init`ed into it. Asserting the refusal
  alone passes against a version that inits and then complains.
- **The one-root case must be a different test from the many-root case.** With
  one root, a server that ignored the `root` field entirely would pass; the
  many-root test is what makes the field mean something, and it must assert the
  project landed under the *second* root, not the first.
- **A browser test**, because the dialog's branch — one root versus several —
  is in `static/overview.js` and no Rust test reaches it.
