# roost creates every worktree, and the terminal follows

Issue #156, the creation half of the worktree lifecycle (#157 is finishing:
merge, PR, prune). roost becomes the only thing that creates a worktree in a
project it manages. When a Claude in a roost terminal asks for one, the user
approves it in the browser, roost creates it, and the terminal that asked is
**re-homed**: from then on it is shown, counted and closed as part of the
worktree's project, while the process itself never moves.

This spec corrects #156 in one place. The issue named `CwdChanged` as the
re-home trigger; Claude Code does not raise it for a worktree (below). The
trigger is the `WorktreeCreate` hook itself.

## What Claude Code actually does

Probed 2026-10-11 against Claude Code 2.1.296 in a scratch repository, `-p`
mode, with a hook that logs every payload plus the Claude process's
`/proc/<pid>/cwd`. Interactive mode was not probed; nothing observed depends
on the mode, but the first implementation task re-runs the probe there.

| Question | Observed |
|---|---|
| Does a `WorktreeCreate` hook that prints a path replace creation? | Yes. Git listed only the hook's branch; Claude Code made none of its own. |
| What does the payload carry? | `session_id`, `transcript_path`, `cwd` (the main checkout), `hook_event_name`, `name`, and mid-session `prompt_id`. No base, no branch. |
| A hook that exits non-zero, mid-session (`EnterWorktree`)? | Creation fails, Claude shows the hook's stderr, the session carries on in the main checkout. |
| A hook that exits non-zero, at start-up (`claude --worktree`)? | Claude aborts: `Error creating worktree: …`. It never starts. |
| A hook that exits 0 and prints nothing? | Claude aborts: `hook succeeded but returned no worktree path`. |
| Does `EnterWorktree` raise `CwdChanged`? | **No.** A plain `cd` does, with `old_cwd`/`new_cwd`. |
| Does the process really change directory? | Yes. Same pid; `/proc/<pid>/cwd` and the `cwd` of every later hook (`PostToolUse`, `Stop`) are the worktree from the `EnterWorktree` result onward. |
| `claude --worktree` at start-up? | `SessionStart.cwd` is already the worktree. |
| The session that entered the worktree runs `/exit`? | **Claude Code removes the worktree itself** — directory and `.git/worktrees` entry gone within seconds — even though a hook created it. The branch survives. Observed for a clean worktree in `auto` permission mode; whether a dirty one prompts first (the docs say so) was not observed. |

Re-observed in interactive mode on 2026-10-11 (plan Task 0): the first six rows
hold unchanged, and `claude --resume <id> --fork-session` run from inside a
worktree finds the session and starts with that worktree as its directory.

Two consequences carry the design. The hook is the only moment roost can ask
*before* the move, so the approval lives there. And `/proc/<pid>/cwd` tells
roost where any Claude it can see is working, with no hook at all, so a move
roost was not asked about is still detectable.

## Why re-homing is an alias, not a move

A session's identity is the dtach socket path, and that path cannot change.
It lives at `sock/<storage_key(project)>/<name>` (`session.rs` `sock_path`),
and every liveness check in the codebase finds the socket's holder by matching
that exact path as a whole argument in `ps` output (`registry::pids_holding`).
The path was baked into the dtach master's argv at spawn.

Each way of physically moving a session breaks something CLAUDE.md treats as
a hard constraint:

| Moving this | Breaks |
|---|---|
| The socket file into `sock/<worktree key>/` | `pids_holding` on the new path is empty: `reconcile` unlinks it as dead, `probe_socket` refuses attach, `kill_and_unlink` sees no holder and orphans the shell. The `Path::exists`-on-a-moved-thing row of the evidence table, again. |
| The `SESSIONS` map entry to the worktree's key | The pump thread captured its key at spawn and `break`s when it is gone (`session.rs:675`): the terminal stops being read. |
| `ROOST_PROJECT` / `CLAUDE_CODE_SSE_PORT` | Cannot be done: a running process's environment is fixed. |

So the session stays exactly where it is — its socket, its `SESSIONS` key, its
env, its IDE listener all keep naming the main project — and one persisted
record says which project **owns** it for display and lifecycle. Every
project-keyed lookup that matters is taught to read that record. Nothing in
`reconcile` changes: the physical layout it sweeps is untouched, which is the
point.

Terms used below: the **home** is the project whose `sock/` directory holds
the socket (the main checkout); the **owner** is the project the session is
shown in. They differ only for a re-homed session.

## The pieces

### 1. One creator, one convention

`worktree::create` gains a `name` parameter and a `Source`:

- Location `.claude/worktrees/worktree-<name>`, branch `worktree-<name>` —
  Claude Code's own convention, so a worktree does not betray who asked for it.
- Base: HEAD of the main checkout, recorded in `.base` as today. Not Claude
  Code's `worktree.baseRef` default (the remote default branch): a mid-session
  fork wants to continue from where it is.
- `<name>` must match `^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$` — the session-name
  alphabet, minus a leading `-` or `_`: it becomes a path component and a
  `git` argument, and the leading-character rule keeps it from ever reading as
  an option. Refused
  with a message that quotes the rule. The name from Claude Code is either the
  user's `--worktree <name>` or its generated `bright-running-fox`; both fit.
- roost's own generator (the `+` path, §6) makes the same
  `adjective-adjective-noun` shape from a word list shipped in the binary, and
  retries on collision up to `MAX_WORKTREES` like `claude-N` does today.
- A name that already exists as a branch or a directory is refused, not
  suffixed. Claude asked for that name; silently giving it another is the kind
  of disagreement this spec exists to remove.

Existing `claude-N` worktrees keep working: nothing reads the prefix.

### 2. The hook, and the channel it talks over

A new subcommand, `roost worktree-hook`, installed on `WorktreeCreate` only,
with `"timeout": 120` in its entry so Claude Code waits out the dialog.

`roost claude-hook` never talks to the server — it writes an escape sequence
to its tty and files under `state/claude/`. Approval needs a round trip, and
nothing provides one, so this adds a channel:

- A Unix socket at `state/hook.sock`, mode 0600, created at start-up (stale
  file: `symlink_metadata`, then `connect`; refused → it is ours and dead,
  unlink; anything else → refuse to start the listener and log, never unlink
  a socket another roost may hold).
- Each connection is checked with `SO_PEERCRED`: a peer uid other than ours
  is closed without a reply.
- One request, one reply, newline-delimited JSON, a 64 KiB read bound:
  request `{ "op": "worktree_create", "project", "session", "name", "cwd",
  "session_id" }`, reply `{ "path": … }` or `{ "refuse": "reason" }`.
- A thread per connection like everything else; no lock held while waiting
  for the browser (the hub parks a pending request, see §3).

Not HTTP, deliberately. CLAUDE.md fixes the HTTP state-change surface at
exactly two POSTs, both Origin-checked, and a localhost endpoint that creates
directories and moves terminals is precisely what a drive-by page would want.
No browser can open a Unix socket, so no Origin rule applies and the surface
stays at two. Filesystem permission plus peer uid is the same trust boundary
as `state/` itself.

What the hook does, by case:

| Case | Behaviour | Why |
|---|---|---|
| `ROOST_PROJECT`/`ROOST_SESSION` unset (a Claude outside roost) | Creates the worktree itself, roost's convention, no dialog, prints the path, exit 0. | Installing the hook must not break `claude --worktree` for a session roost has nothing to do with. There is no terminal to re-home. |
| Env set, `hook.sock` absent or refused | Same as above. | roost is stopped or restarting; refusing would make a restart break every Claude's worktree command. §7 picks the move up when roost is back. |
| Env set, socket answers `path` | Prints it, exit 0. | |
| Env set, socket answers `refuse` | Reason on stderr, exit 1. | Claude Code shows it; mid-session the Claude carries on in place. |
| Any read/parse failure of the reply | Reason on stderr, exit 1. | Could not tell what roost decided → do not create. |

The fallback rows create a worktree, which destroys nothing; that is why
"could not reach roost" may fall through to creating instead of refusing. It is
the opposite direction from every destructive default in this codebase, and
on purpose.

### 3. Asking the user

The hub of `ROOST_PROJECT` receives the request and sends every connected
browser of that project an `Event::WorktreeAsk { id, session, name }`. The
dialog (the in-page `askChoice` every other question uses):

> **Claude in `term3` wants a worktree** `worktree-bright-running-fox`
> [Create and open in new tab] [Refuse]

- The first answer from any browser wins; the others' dialogs close.
- **No browser connected** to the project at request time: refuse at once,
  "no roost tab is open on this project to approve it".
- **No answer in 60 s**: refuse, "nobody approved it in roost within 60 s".
- At most one pending ask per session; a second while one is open is refused.

Both defaults refuse: a refusal costs Claude a retry, an unapproved approval
moves a terminal.

On approve, in this order, each step returning before the next on failure:
create (§1) → write the re-home record (§4) → install the hooks into the new
worktree's own `.claude/settings.local.json` (§8) → reply `path` → broadcast
`ProjectsChanged` for both projects. The approving click opens the new tab
with `window.open` inside its own user activation, as ✻ does today
(`pendingTab`), and `WorktreeReady` navigates it. If creation fails the hook
gets `refuse` with git's message and the record is never written.

### 4. The re-home record

`state/home/<home storage key>/<name>`, one file per re-homed session,
containing the owner's project URL. Written atomically (pid-unique temp, then
`rename`), read with three outcomes: `NotFound` → not re-homed; unreadable or
empty → **unknown**, and every consumer below treats unknown as "keep both
views, destroy nothing"; contents → the owner.

New module `home.rs` owns it, with `owner_of(home, name)`,
`residents_of(owner) -> Vec<(home, name)>`, `record`, `forget`.

Who reads it, and what changes. Everything not listed keeps keying on the
home, unchanged — socket, `SESSIONS`, pump, cap, env, modes, cwd, launch,
`claudesess`:

| Lookup | Change |
|---|---|
| Owner's hub `live_sessions` (`refresh_live_sessions`) | Adds `residents_of(owner)` that are live in their home. |
| Home's hub `live_sessions` | Drops sessions whose owner is elsewhere. The home's tab for it closes with a transient notice "`term3` moved to `worktree-x` →". |
| `Tab::Terminal` | Gains `home: Option<String>`, `None` meaning "this project". The client connects to `/ws/{home}/term/{name}` — the real session — from the owner's page. |
| `term.rs` attach | Unchanged path; `/ws/{home}/term/{name}` already attaches the real session. Origin is checked as for every terminal. |
| `hub::publish` (notifications, bell) | Routes `(home, name)` to the owner's hub. |
| `claudes.rs` `/proc` scan → `claude_evidence` | Maps a Claude's `(ROOST_PROJECT, ROOST_SESSION)` through `owner_of` before grouping by project. |
| IDE: `idesess::session_of`, `ide::publish` of `openDiff` | The Claude stays connected to the home's listener (its env names that port). Diffs and selections for a re-homed session are delivered to the owner's browsers. `openDiff` confinement is unchanged: the worktree is inside the home's directory. |
| `do_remove_worktree` gates, `registry::removable` | Count residents: a worktree with a live re-homed session is not removable. Without this the worktree could be deleted under the running Claude. |
| `session::kill_project(home)` (Close Project on the main checkout) | Skips sessions owned elsewhere. Closing a project ends that project's terminals, and this one is not the main checkout's any more. |
| `session::kill_project(owner)` (Close Project on the worktree) | Also ends its residents, through `end_session(home, name)`. |
| `session::end_session(home, name)` | Also `home::forget`. |
| Overview rows (`session_rows`, `worktree_rows`) | Show a resident under its owner, not its home. |

`reconcile` is deliberately absent from this table.

**The record's own sweep.** A record whose socket is `NotFound` under
`symlink_metadata` is removed, in the same pass as `modes::sweep_orphans`; any
other error keeps it. A record whose owner no longer resolves (the worktree
was removed by hand with `git worktree remove`) is **not** deleted: it is
ignored, so the session shows in its home again, and the record stays until
the session ends. A missing worktree is not evidence the session is gone.

**Leaving.** A re-homed Claude that runs `ExitWorktree` or `cd`s out stays
re-homed: roost does not move it back automatically. §7's detector shows it
("`term3`'s Claude is working in the main checkout") with a "Move back" that
`forget`s the record. Automatic moves in both directions on cwd evidence alone
would make a tab jump between pages because of a `cd`.

### 5. The cap

The 16-sessions-per-project cap keeps counting the physical home, as it must:
it bounds sockets in one `sock/` directory and dtach processes started from
one project. A main checkout that has re-homed ten Claudes into ten worktrees
can start six more terminals. Stated rather than fixed: the alternative is two
caps that can disagree.

### 6. roost-initiated: `+` in the worktree switcher

Shown only on the main checkout (worktrees are started from there, today's
rule). A dialog:

```
New worktree
  Name: [bright-running-fox]   → worktree-bright-running-fox
  ( ) Continue term3's Claude there (fork)   default when exactly one is known
  ( ) Start a fresh Claude
  ( ) Just a shell
                     [Cancel] [Create]
```

- **Fork** opens the worktree's page with a new terminal there (a native
  session of the worktree project, no re-homing involved) and types
  `claude --resume <session id> --fork-session` (that a fork starts in the
  directory it is launched from is from Claude Code's docs, not yet observed;
  the first implementation task probes it with the rest). The id is
  `claudesess::recorded(project, term)` — the id the hooks reported, which
  survives `/clear` and resumes, unlike the `--session-id` roost typed at
  launch. The fork's own id arrives through the existing `SessionStart` hook
  (`source: "fork"`). The original Claude keeps running: this is parallel
  work, not a hand-off. Offered only for a terminal with a recorded id; one
  whose Claude roost merely sees in `/proc` has no id and the option is
  absent, not guessed.
- **Fresh** types `claude`. **Just a shell** types nothing.
- ✻'s existing "Start in a new worktree" answer opens this dialog with Fork
  preselected instead of creating `claude-N` directly. `Intent::NewWorktree`
  gains `name` and `launch: Fork { session } | Claude | Shell`; the server
  re-checks that `session` exists and has a recorded id.

### 7. Moves roost was not asked about

The `claudes.rs` `/proc` tick already reads every Claude's environ every 3 s;
it also reads `/proc/<pid>/cwd`. When a Claude's cwd is inside a worktree of
its home project (resolved through `git worktree list`, never by string
prefix) and its terminal is not re-homed there, the home's browsers get a
dismissible banner:

> `term3`'s Claude is working in `worktree-foo` — [Move tab there]

"Move tab there" writes the record (§4). Never automatic: this path had no
approval, and a stale `/proc` read is still a read. A `cwd` that cannot be
read is "unknown" and shows nothing.

**A worktree that Claude Code removed.** When the re-homed Claude exits, Claude
Code deletes its clean worktree (table above). §4 already makes the terminal
fall back to its home, record kept, nothing killed. What it does not do is
say so: the owner's tab simply loses its terminal. So when a resident's owner
stops resolving *and* `git worktree list` of the home, read successfully, no
longer lists it, the home's browsers get a transient notice: "`worktree-x` was
removed (Claude Code removes a clean worktree when its session exits);
`term3` is back here". A `worktree list` that fails shows nothing. The
branch is untouched; #157 decides what finishing does with it. This covers a Claude with hooks off, a
worktree made while roost was down (§2's fallback), and the Move-back case of
§4.

### 8. Installing the hook, and the constraint that widens

The worktree hook is per project, like the notification hooks, and installed
by the user, not silently: a project setting "Let roost create Claude's
worktrees", and the §7 banner offers it once when a move was detected in a
project without it. It goes through `claudehooks::set`, which keeps
everything that makes that module safe (the only settings file written is
`.claude/settings.local.json`; an unparseable file is `Unknown` and refused).
Because that file is untracked, a new worktree does not inherit it; §3
installs it there at creation when the main checkout has it.

CLAUDE.md's bullet changes from "only entries whose command is exactly `roost
claude-hook` … always exits 0" to: roost owns entries whose command is exactly
`roost claude-hook` or exactly `roost worktree-hook`. `claude-hook` always
exits 0. `worktree-hook` is installed **only** on `WorktreeCreate`, where a
non-zero exit is Claude Code's documented refusal and a silent exit 0 is
itself an error ("returned no worktree path"); it exits non-zero only when
roost answered `refuse` or its answer could not be read. Installing it on any
other event is a defect: a non-zero there would show an error in every Claude
session in that checkout.

A new hard-constraint bullet records §4's rule: *a session's socket path, its
`SESSIONS` key and its environment name its home forever; re-homing changes
ownership through `state/home/` and nothing else.* And the "Absence of
evidence" table does not grow, which is the test of whether §4 is right.

## Not in this spec

- Merge, PR, prune, relaxing the removal gate — #157.
- A Claude outside any roost terminal: its worktree is created in roost's
  convention (§2) and shows in the switcher; there is nothing to re-home.
- Taking over removal. Claude Code removes a clean worktree when its session
  exits (and, per its docs, asks first for a dirty one) — roost only reports
  it (§7). A `WorktreeRemove` hook, which might hand removal to roost, is
  unverified and is #157's.
- Choosing a base in the `+` dialog. HEAD, always.
- Backing up `state/home/`. A restored workspace re-homes nothing; sessions
  come back in their home.

## Testing

Each test watched failing with the code it covers reverted, and the failure
noted in its comment.

Rust (`cargo test -- --test-threads=1`, per the project's known hang):

- **Name validation**: `--foo`, `_x`, `../x`, `a/b`, `a b`, 33 characters,
  empty — each refused, asserting on the message; `Foo-bar_2` accepted.
- **create with a name**: real `git worktree add` in a temp repo; branch and
  directory are `worktree-<name>`, `.base` holds HEAD's branch; an existing
  branch of that name is refused, not suffixed.
- **Hook channel**: the peer-uid decision is a pure function of the
  `ucred`, unit-tested with a foreign uid (a real foreign-uid peer needs
  privileges a test does not have); the same-uid path is tested end to end
  over a real socket. A request over 64 KiB is cut off. A hub with no browser answers `refuse` with
  the "no roost tab" reason.
- **Hook fallbacks**: the subcommand with env unset, and with env set and no
  socket, creates and prints a path; a `refuse` reply exits 1 with the reason
  on stderr; garbage on the socket exits 1.
- **Re-home is not destruction** — the test the whole design rests on: spawn
  a real dtach session (not `ROOST_CMD=cat`; the dev/prod table) in the home,
  re-home it, then run `reconcile`, `kill_project(home)`,
  `refresh_live_sessions` on both hubs, and the record sweep. Assert the dtach
  master is still alive by pid **and** that `probe_socket` still finds it
  held, and that it is listed under the owner and not the home. Then
  `kill_project(owner)` and assert it is gone. Reverting `kill_project(home)`'s
  skip must kill it — that is the failure to watch.
- **Removal gate** refuses a worktree with a live resident.
- **Unknown record** (empty file, unreadable directory): both hubs list the
  session, nothing is killed, nothing is deleted.
- **Owner gone**: delete the worktree directory behind roost's back; the
  session reappears in its home, the record survives.
- **Concurrent sweep**: `reconcile` looping on one thread while another
  records and forgets re-homes; no session dies, runs timed (a deadlock hangs).

Browser (`tests/browser/worktree-rehome.mjs`, real Chromium, real dtach, a
fake `claude` that is a real binary and calls `roost worktree-hook` the way
Claude Code does):

- Ask → approve with a real mouse click → new tab opens on the worktree page
  with the terminal attached and still showing its scrollback; the main page's
  tab is gone with the notice; typing in the new tab reaches the same process
  (echo a marker, read it back).
- Ask → no click → refused at 60 s; ask with no browser open → refused at once.
- `+` → Fork types the resume line with the recorded id.
