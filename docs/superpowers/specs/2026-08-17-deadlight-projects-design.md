# deadlight — stateful projects design

Adds an explicit, persistent concept of a **project** and makes its live
resources visible and closable. Supersedes nothing; extends
`2026-08-16-deadlight-v3-workspace-design.md`, whose panes, tabs, state
mirroring, and dtach-backed sessions all carry over unchanged.

## The problem this solves

deadlight targets dev sessions that run for days or weeks, across many browser
tabs opened and closed. Today three kinds of state exist and **none of them are
visible**:

| State | Where it lives | Surfaced? |
|---|---|---|
| Saved workspace — layout, tabs, unsaved buffers | `$DEADLIGHT_STATE_DIR/{key}.json` | no |
| Live dtach session — a running shell | `sock/{key}/{name}` + a process | no |
| Orphaned socket — file with no process | `sock/…` | no |

Observed in production on 2026-08-17: **13 live shells**, of which **9 belonged
to directories that no longer existed**. Only one project had a saved workspace,
while two others had shells running over eight hours. Nothing in the UI would
ever have revealed this, and the only way to end a session was `pkill` over ssh.

The root cause of the accumulation is that **opening a project immediately spawns
a shell** — the default layout ships a Terminal tab, so merely looking at a
project forks a bash that nothing ever reaps.

## What a project is

A **project** is a directory under `ROOTS` that the user has opened in deadlight.
It is identified by its path relative to its root (`karpie`, or `karpie/src` for
a nested one). That identity is already the URL; as a storage key it is
percent-encoded (`karpie%2Fsrc`), which is unchanged from today.

A project is **known** to deadlight when it has either a saved workspace or at
least one live session. Both states appear in the UI, distinguished:

- **● live** — one or more running dtach sessions
- **○ idle** — a saved workspace, nothing running

Projects are normally git repositories, but a plain directory is allowed — see
*Git*. Nothing about project identity depends on git.

## Worktrees

A git worktree is **its own project** and needs no special handling to be one:
it is a distinct directory with its own rel path, storage key, dtach sockets
and saved layout, and you work on several in parallel precisely because they
are independent. Detection already works — a linked worktree's `.git` is a
*file* rather than a directory, and every git check in the codebase uses
`.exists()`.

What worktrees add is a **display relationship**: they are siblings of one
repository, so the UI groups them parent-and-child. That grouping is
presentation only; nothing about state, sessions or identity is shared.

**Discovery goes through git, not the filesystem.** `git worktree list
--porcelain` on a repository enumerates its worktrees authoritatively, wherever
they live. This matters because the dominant real-world location is a
*dot-directory*: Claude Code creates worktrees under
`{repo}/.claude/worktrees/{name}`, and deadlight exists for AI-assisted
development, so those are exactly the worktrees a user wants side by side. A
path convention would miss a worktree placed in a sibling directory; asking git
does not.

**The dot-segment rule gets a narrow exception, not a repeal.** `resolve_project`
rejects any path segment beginning with `.`, which today makes
`.claude/worktrees/site-launch` unopenable even if the user knows the URL. A
dot-segment path resolves **only** when git itself reports it as a worktree of a
repository under `ROOTS`. The general rule stands for everything else, so
`.git`, `.venv` and `.config` remain non-projects.

**Confinement is unchanged and still does the real work.** A repository's
worktree metadata lives inside the repository, so a cloned repo could name a
worktree anywhere. The git allowlist only relaxes the *dot* rule; every path
still has to canonicalise under a root, so a worktree outside `ROOTS` cannot be
opened no matter what git says about it. Such a worktree is listed as
unreachable rather than silently omitted, so the user is not left wondering
where it went.

**Grouping key** is the main worktree's absolute path — the first entry `git
worktree list` reports — rather than `--git-common-dir`, which returns a
relative string and would need canonicalising anyway.

**Branch is the label.** Worktrees of one repository differ only by branch, so
the picker and the strip show it:

```
▸ ultima_marketing        ⎇ main          ● 2 shells
    └ site-launch         ⎇ site-launch   ○
```

**Cost control:** `git worktree list` is a subprocess, so it runs only for
directories that are already known to be repositories, and only for the entries
being displayed. Results are cached briefly, since a worktree set changes rarely
compared to how often a listing renders.

## The project registry

A process-wide registry, keyed by project, holding for each: its absolute
directory, its live sessions, and whether a saved workspace exists.

**It must be rebuilt at startup, not merely accumulated in memory.** dtach
sessions deliberately outlive deadlight, so after a restart an in-memory-only
registry would forget every running session — reintroducing exactly the
invisible-orphan bug this design exists to fix. On startup deadlight:

1. Lists `$DEADLIGHT_STATE_DIR/*.json` → projects with a saved workspace.
2. Walks `$DEADLIGHT_STATE_DIR/sock/` → candidate sessions.
3. For each socket, checks whether a live `dtach` process holds it.

**Reaping.** A socket with no live process is deleted. A session whose project
directory no longer exists is killed and its socket removed. Both are logged.
Reaping runs at startup and whenever the registry is enumerated, so the mess
observed above cannot re-accumulate silently.

**Reaping is serialised and throttled.** Because enumeration happens on a
request path, a mutex prevents interleaved sweeps and a minimum interval (a few
seconds) skips redundant ones. The cost is a bounded staleness window: a project
can show ● for up to that interval after its last process actually died. That
replaces an *unbounded* window — previously freshness depended entirely on how
often a page happened to load — and the UI refreshes on load or explicit
refresh rather than polling, so the trade is worth it.

**Session metadata** per session: name, age (from the process start time), and
whether a browser is currently attached. Age is what makes an abandoned session
recognisable at a glance.

## Terminals are started deliberately, never implicitly

**Opening a project must not spawn a shell.** The default layout still places a
Terminal tab in the right pane, but the tab is a *placeholder* until the user
asks for a session. A `Tab::Terminal` therefore gains a distinction between
"tab exists" and "session running"; the session is created only on an explicit
start.

The placeholder pane renders a centred, clickable hint:

```
┌─ shell ──────────────────────────┐
│                                  │
│     Press Enter to start         │
│         a terminal               │
│                                  │
└──────────────────────────────────┘
```

Enter starts it — matching what people already do in a fresh terminal to check
it is alive — and clicking the hint does the same, so the gesture is
discoverable rather than hidden. A plain button was rejected: it is discoverable
but does not match terminal muscle memory, and the hint achieves both.

Closing a terminal tab still only detaches; the session keeps running and the
project stays ● live. Ending sessions is *Close Project*.

## Git

A project is normally a git repository. When a terminal is started in a
directory that is not one, the placeholder offers initialisation instead:

```
┌─ shell ──────────────────────────┐
│  Not a git repository.           │
│                                  │
│   [ Initialize git repo ]        │
│   start without git              │
└──────────────────────────────────┘
```

`git init` runs in the project directory; on success the session starts
normally. **"start without git" is a real escape hatch**, not decoration —
scratch directories stay usable. The gate exists to make the common case
(a repo) the default path, not to forbid the uncommon one.

## UX: the open-projects strip

The workspace header gains a strip of project links in the top right, beside the
refresh control:

```
◆  deadlight        ⎇ master (2)   ● karpie  ● deadlight  ○ glow   ⟳
```

Each entry shows the project name with its ● / ○ marker and a tooltip carrying
the session count and the age of the oldest (`2 sessions · oldest 8h`). The
current project is marked as such. Worktrees appear grouped with their parent
repository and labelled by branch, since that is what distinguishes them.

**Tab reuse.** Each link is `<a href="/{project}" target="dl-{key}">`. A named
target reuses the browsing context of that name, so clicking navigates the
existing tab and the browser focuses it — a click is the user gesture browsers
require. Two honest limits: a tab the user opened by pasting a URL has no name,
so the first click still opens a new tab; and reuse is per browser profile, not
across devices. Server-mediated focus was rejected — browsers block
focus-stealing without a gesture in the *target* tab, so it cannot be made
reliable.

The strip needs data spanning **all** projects, while the workspace socket is
deliberately per-project. It is therefore served by a small cross-project
fragment endpoint, refreshed on load and on the existing refresh trigger.

The picker at `/` shows the same ● / ○ markers on its rows, so "what did I leave
running?" is answerable before entering a workspace.

## UX: Close Project

An explicit action in the workspace header ends **all** dtach sessions for the
project. It is the only way to end sessions from the UI; today the only way at
all is `pkill` over ssh.

Confirmation is mandatory and states exactly what will happen:

```
Close deadlight?

  3 terminal sessions will be ended:
    shell (8h), claude (2h), build (12m)

  ⚠ 2 files have unsaved changes:
    src/hub.rs, README.md

  [ Cancel ]  [ End sessions ]   ← disabled until saved
```

- Sessions are listed individually with ages, because "3 sessions" is not enough
  to judge whether one of them is a long-running job you care about.
- **The saved layout is kept.** Reopening the project restores panes and tabs;
  only the running shells end. Closing is about reclaiming resources, not
  forgetting your work.
- **Dirty buffers block the close**, listed by name, with the confirm disabled
  until they are saved or discarded. Unsaved text is the one piece of state that
  cannot be reconstructed, so it is never destroyed by a resource operation.

After closing, the project becomes ○ idle: still listed, nothing running.

## Errors

- Reaping and startup reconciliation log what they remove; a failure to reap is
  logged and skipped, never fatal.
- `git init` failure surfaces in the placeholder with the git error text; the
  session is not started.
- Ending a session that has already died is not an error.
- A Close Project request for a project with no sessions succeeds trivially.

## Security

Unchanged from v3, with one addition: **Close Project and terminal start are
state-changing, so they travel over the workspace websocket** as intents, never
as HTTP. HTTP stays GET-only. The cross-project strip endpoint is a read and
therefore a normal GET fragment.

Session names remain `[A-Za-z0-9_-]{1,32}`. `git init` runs with the project
directory as cwd and takes no user-supplied arguments.

**A GET can trigger reaping.** The cross-project strip endpoint is a read, but
it calls `known_projects`, which reconciles — deleting stale sockets and
killing processes whose project directory is gone. So a hostile page's
`<img src="http://localhost:8444/frag/_projects">` sends a genuine `Host`,
passes the `Host` check (which defends DNS rebinding, not this), and triggers a
sweep. The impact is bounded — reaping only ends sessions whose project
directory has already been deleted — and this is a single-user tool on
loopback, so it is accepted rather than fixed. Reaping is serialised and
rate-limited so the endpoint cannot be used to amplify `ps` invocations.

The worktree dot-segment exception is a **relaxation of one naming rule, not of
confinement**. A worktree path is accepted only when git reports it *and* it
canonicalises under a root; a repository that names a worktree outside `ROOTS`
gets it listed as unreachable, never opened. `git worktree list` is read-only
and takes no user-supplied arguments.

## Testing

- **Unit:** registry rebuild from a fixture state dir + socket dir, including a
  socket with no process (reaped) and a session whose project directory is gone
  (killed); session-age formatting; the ● / ○ classification.
- **Unit, worktrees:** parsing `git worktree list --porcelain` into a main
  worktree plus children with branches; a dot-segment path resolves when git
  vouches for it and is still refused when git does not; a worktree outside
  `ROOTS` is reported unreachable rather than resolved. These use a real
  `git worktree add` in a temp repo, because the porcelain format is the thing
  under test and a hand-written fixture would not prove we parse git's actual
  output.
- **Integration:** opening a project creates **no** session; an explicit start
  creates exactly one; Close Project ends all of a project's sessions and leaves
  another project's untouched; Close Project is refused while a buffer is dirty.
- **Browser:** the placeholder starts a session on Enter and on click; the strip
  shows ● / ○ correctly and its links carry `target="dl-{key}"`; the close
  dialog lists sessions with ages and blocks on dirty buffers.

## Future work (not in this design)

- **Per-session CPU and memory.** Now that sessions are tracked per project,
  each dtach session's process tree can be sampled (`/proc` on Linux, `ps` on
  macOS) and shown alongside its age — making an expensive runaway job visible.
  Deferred: it needs a sampling cadence and per-platform code, and none of the
  above depends on it.
- A suggestion box for enforcing project=repo more strongly, if the soft gate
  proves too soft in practice.
