# resh

A per-project remote workspace in a single Rust binary: a four-pane IDE-style
layout in the browser, backed by real terminals that survive you closing the
tab.

It exists for AI-assisted development. Claude runs in a terminal pane and edits
files; the viewer reflects those edits live, so what you see is what is on
disk — not a stale snapshot.

## What it does

**Four panes, universal tabs.** Left-top, left-bottom, middle and right, with
draggable dividers. One flat tab type — file tree, git changes, file
preview/editor, diff, terminal — and any tab can live in any pane.

**All state lives on the server and mirrors live.** Open a file in one browser
and it opens in every connected browser, the way two clients attached to one
terminal multiplexer mirror each other. Layout and unsaved buffers persist
across restarts, outside the repo, so pane drags never appear in `git status`.

**Files go in through the browser.** Drag files from the desktop onto the file
tree, or copy and paste them there, and they land in that directory. Paste a
screenshot onto a terminal and it reaches the program running there as an
actual image, not as a path — which is how you show Claude the thing you are
looking at. Folders are refused on purpose: `git`, `rsync` and `scp` are what
move a project, and an upload is capped per request rather than per file.

**Terminals survive.** Each terminal is a PTY owned by resh and wrapped in
`dtach`, so sessions outlive both a dropped browser tab and a resh
restart. resh keeps a 1 MB scrollback ring per session and fans output out
to every attached client.

**Sessions are visible and deliberate.** Opening a project starts nothing — a
terminal tab waits for you to press Enter — and the header shows which projects
have shells running, with their count and age. Close Project ends them all,
keeps your layout, and refuses while a buffer is unsaved. Sessions that outlive
a restart are rediscovered at startup; sockets with no process, and shells whose
directory is gone, are reaped.

**Editing is conflict-guarded.** Save refuses if the file changed on disk since
your buffer was opened, showing a diff of what differs — the changed hunks
with their line numbers, not both files whole. A clean buffer follows external
writes live; a buffer with unsaved changes is only flagged stale, never
overwritten. Discarding yours reloads the file, and a restart re-checks every
open buffer against the disk, so a file that moved while resh was down comes
back flagged rather than looking current.

**Autosave, and it knows when to stop.** A buffer is written out a second after
you stop typing, and the moment the editor loses focus. It saves through the
same conflict guard as ⌘S — never forcing — and takes its hands off a file
that has diverged, rather than re-raising a banner every second; ⌘S is how you
resolve that one. The pane header carries the state (`saved`, `saving…`, `not
saved · changed on disk`), which is also where ⌘S is advertised when autosave
is off. Turn it off with `autosave = false` in a global or per-project
`.resh/config.toml` — see [`docs/deploy.md`](docs/deploy.md#autosave).

**A projects/worktrees/sessions overview.** The front page (`/`) is a
two-pane overview: known projects and their worktrees on the left (expand a
project to see its worktrees, each with its Claude/dirty/ahead state), and the
live terminal/Claude sessions on the right — filtered to a project's family
when you select one, all of them otherwise. Clicking a session opens its
workspace with that terminal focused. **＋ Open a directory** (or `/?at=`)
reaches the directory picker to open a directory resh hasn't seen — single
click selects, double click descends, Enter opens, git repos get a one-click
shortcut.

**Dot entries are hidden, until you say otherwise.** The tree leaves out
`.git`, `.claude` and every other dot entry by default, along with build and
vendor directories. The ◌ control in the tree pane's header brings them back —
it mirrors to your other browsers and survives a restart, like any other
workspace change. `show_hidden = true` in a global or per-project
`.resh/config.toml` sets what a workspace starts out doing — see
[`docs/deploy.md`](docs/deploy.md#hidden-files-in-the-tree).

**Claude Code sees the workspace.** A `claude` running in a terminal pane
connects back to resh as its IDE: it can point at a file instead of pasting a
path, and a file it proposes to change opens as a diff tab with Accept /
Reject rather than eighty columns of terminal ASCII. The ✻ next to a tab
strip's + opens a new terminal with `claude` already typed into it — no
flags, because the shell's environment is what links it to this resh. The
button is there unless resh asked your login shell at startup and it could
not find `claude` (a check that could not run keeps the button and says so on
stderr). Off by default, and
separate from all of that, resh can also send whatever you have highlighted in
the editor to every connected Claude as ambient context: set
`share_selection = true` in a global or per-project `.resh/config.toml`. It
ships file contents with no explicit gesture, so read
[`docs/deploy.md`](docs/deploy.md#sharing-the-editor-selection-with-claude)
before turning it on; while it is on, the header says `⧉ sharing selection`.

**Desktop notifications.** A process in a terminal — Claude finishing a task,
a hook needing a decision — can raise a notification with one escape sequence
or `resh notify`; it shows up as a bell across every project and, given a
secure context, as an OS notification that clicks back to the terminal that
raised it. See [`docs/notifications.md`](docs/notifications.md).

## Quick start

```bash
RESH_ROOTS="$HOME/Projects" cargo run --quiet 8444
# then open http://127.0.0.1:8444/
```

Requires `dtach` (`brew install dtach` / `apt install dtach`) and `git`.
Running the binary directly takes one CLI argument, the port. One
subcommand binds nothing: `resh notify <title> [body]` raises a
notification from inside a resh terminal — see
[`docs/notifications.md`](docs/notifications.md). Everything else is
environment — see [`docs/deploy.md`](docs/deploy.md).

## URL surface

| Path | Purpose |
|---|---|
| `/` | Projects/worktrees/sessions overview (`?at=` is the directory picker) |
| `/{project}` | Workspace — may be nested, e.g. `/karpie/src` |
| `/frag/{project}/…` | Server-rendered HTML fragments |
| `/ws/{project}/_workspace` | Workspace state socket — JSON intents up, events down |
| `/ws/{project}/term/{name}` | One raw-bytes socket per terminal |

## Security model

resh binds `127.0.0.1` only and is meant to be exposed by something that
authenticates, such as `tailscale serve`. **The websocket spawns a shell**, so
the loopback bind is the security boundary and is deliberately not
configurable.

- WebSocket handshakes bypass the same-origin policy, so every socket checks
  `Origin` against an allowlist; HTTP checks `Host`/`X-Forwarded-Host` against
  the same list to defeat DNS rebinding.
- The allowlist is readable from the environment or global config only — never
  from a project's own `.resh/config.toml`, so a repo you clone cannot
  allowlist its own domain.
- HTTP is **GET-only**. Every state change travels over the websocket, so there
  is no state-changing verb for a hostile page to forge.
- Every filesystem path is confined to the project directory by canonicalising
  and prefix-checking; creation paths canonicalise the parent and validate the
  final component separately.

## Documentation

- [`docs/deploy.md`](docs/deploy.md) — running, environment, deployment traps
- [`docs/notifications.md`](docs/notifications.md) — triggering, hooking up
  Claude Code, limits
- `docs/superpowers/specs/` — design documents
- `docs/superpowers/plans/` — implementation plans
- [`CLAUDE.md`](CLAUDE.md) — conventions and constraints for working in this repo

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Bundled third-party front-end assets keep their own licenses (all permissive);
their attributions are recorded in [`docs/vendor.md`](docs/vendor.md).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
