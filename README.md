# deadlight

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

**Terminals survive.** Each terminal is a PTY owned by deadlight and wrapped in
`dtach`, so sessions outlive both a dropped browser tab and a deadlight
restart. deadlight keeps a 1 MB scrollback ring per session and fans output out
to every attached client.

**Sessions are visible and deliberate.** Opening a project starts nothing — a
terminal tab waits for you to press Enter — and the header shows which projects
have shells running, with their count and age. Close Project ends them all,
keeps your layout, and refuses while a buffer is unsaved. Sessions that outlive
a restart are rediscovered at startup; sockets with no process, and shells whose
directory is gone, are reaped.

**Editing is conflict-guarded.** Save refuses if the file changed on disk since
your buffer was opened, showing a diff of yours versus disk. A clean buffer
follows external writes live; a buffer with unsaved changes is only flagged
stale, never overwritten.

**A directory picker, not a fixed list.** Browse into any directory under the
configured roots and open it as a workspace — single click selects, double
click descends, Enter opens, and git repos get a one-click shortcut.

## Quick start

```bash
DEADLIGHT_ROOTS="$HOME/Projects" cargo run --quiet 8444
# then open http://127.0.0.1:8444/
```

Requires `dtach` (`brew install dtach` / `apt install dtach`) and `git`.
The only CLI argument is the port; everything else is environment — see
[`docs/deploy.md`](docs/deploy.md).

## URL surface

| Path | Purpose |
|---|---|
| `/` | Directory picker (`?at=` browses) |
| `/{project}` | Workspace — may be nested, e.g. `/karpie/src` |
| `/frag/{project}/…` | Server-rendered HTML fragments |
| `/ws/{project}/_workspace` | Workspace state socket — JSON intents up, events down |
| `/ws/{project}/term/{name}` | One raw-bytes socket per terminal |

## Security model

deadlight binds `127.0.0.1` only and is meant to be exposed by something that
authenticates, such as `tailscale serve`. **The websocket spawns a shell**, so
the loopback bind is the security boundary and is deliberately not
configurable.

- WebSocket handshakes bypass the same-origin policy, so every socket checks
  `Origin` against an allowlist; HTTP checks `Host`/`X-Forwarded-Host` against
  the same list to defeat DNS rebinding.
- The allowlist is readable from the environment or global config only — never
  from a project's own `.deadlight/config.toml`, so a repo you clone cannot
  allowlist its own domain.
- HTTP is **GET-only**. Every state change travels over the websocket, so there
  is no state-changing verb for a hostile page to forge.
- Every filesystem path is confined to the project directory by canonicalising
  and prefix-checking; creation paths canonicalise the parent and validate the
  final component separately.

## Documentation

- [`docs/deploy.md`](docs/deploy.md) — running, environment, deployment traps
- `docs/superpowers/specs/` — design documents
- `docs/superpowers/plans/` — implementation plans
- [`CLAUDE.md`](CLAUDE.md) — conventions and constraints for working in this repo
