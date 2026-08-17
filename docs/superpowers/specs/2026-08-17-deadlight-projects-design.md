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
current project is marked as such.

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

## Testing

- **Unit:** registry rebuild from a fixture state dir + socket dir, including a
  socket with no process (reaped) and a session whose project directory is gone
  (killed); session-age formatting; the ● / ○ classification.
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
