# deadlight v2 — design

Date: 2026-08-16. Status: approved in brainstorming, pending implementation plan.
Supersedes the v1 design described in HANDOFF.md.

## What it is

A per-project remote workspace served from this box: a persistent Claude/zellij
terminal plus a read-only project viewer (file tree, git changes, markdown
preview, syntax-highlighted code) in one browser page. It replaces both the
file-viewing role of code-server and the web frontend of zellij-web. zellij
itself stays: it keeps owning terminal sessions, persistence, and crash
survival; deadlight wraps it.

Core contract, revised from v1:

- The **viewer** is stateless and read-only. Every pane render re-reads disk.
  No writes over HTTP, no cookies, no localStorage, no client state machine.
- The **terminal** is the sole exception: one websocket per attached tab,
  bridging to a zellij client. All terminal state lives in zellij server-side;
  the socket is a disposable pipe, not a session.
- Claude changes deadlight's behavior by editing text config files on disk
  (see Settings). Deadlight never edits anything.

## URLs

Two user-facing pages:

| URL | Page |
|-----|------|
| `/` | project index: all directories under ROOTS |
| `/{project}` | workspace: terminal + viewer for that project |

Plumbing, never navigated to by the user:

- `/static/*` — vendored assets (htmx, xterm.js + fit addon, highlight.js,
  theme CSS, app.js glue)
- `/ws/{project}` — terminal websocket
- `/frag/{project}/tree?path=`, `/frag/{project}/file?path=`,
  `/frag/{project}/changes`, `/frag/{project}/diff?path=` — HTML fragments
  fetched by htmx into panes; `/frag/{project}/theme.css` — optional
  per-project CSS (see Themes)

`{project}` is a single path segment matched against directory names under
`ROOTS = [/home/claude/ultima, /home/claude/projects]`. Reserved names:
`static`, `ws`, `frag`. If the same name exists under both roots, the first
root wins; the index shows full paths so the shadowing is visible.

### Navigation and history

- No `hx-push-url`, no `pushState` — ever. Tabs, file opens, and tree clicks
  change the page, never the URL. The Back button cannot traverse app flow and
  therefore cannot disturb the terminal.
- Viewer state (active tab + open file) is mirrored into the URL hash via
  `history.replaceState` (`/{project}#files/src/main.rs`). `replaceState`
  creates no history entries and hash changes trigger no loads, so Back stays
  inert — but a reload or duplicated tab restores the open pane.
- Reload is not prevented (browsers own ⌘R) — it is made cheap: websocket
  drops → reattach → zellij repaints; viewer restores from the hash. Reload is
  also unnecessary for freshness: the refresh control / `r` key re-fetches the
  current fragment without touching the terminal.

### Multi-tab

Allowed, explicitly. Each tab on `/{project}` is an independent zellij client
(zellij natively supports multiple attached clients — tabs mirror each other)
plus an independent viewer. Nothing is shared between tabs, so nothing can
conflict.

## Terminal

- Page opens `/ws/{project}`; server spawns a PTY (portable-pty) running
  `zellij attach --create {project}` and pumps bytes both ways.
- xterm.js + fit addon render it. Resize events send the new geometry to the
  PTY.
- Socket close (tab closed, laptop sleep, network blip) → kill the PTY child →
  zellij client detaches; the session lives on. Client JS auto-reconnects with
  short backoff and on window focus.
- The terminal pane sits **outside** the htmx swap target, so viewer
  navigation never remounts it.

## Page layout

Header: project name (links to `/`), tabs **Terminal | Files | Changes**,
branch label + change-count badge, refresh control. Terminal tab is the
full-pane terminal and stays mounted (and connected) while other tabs are
shown. Files and Changes are sidebar + content pane:

- **Files**: tree rendered server-side as nested `<details>` elements; the
  directories along the currently open file's path come pre-expanded. Dir
  expansion beyond that is ephemeral DOM state. Clicking a file loads
  `/frag/.../file` into the content pane.
- **Changes**: porcelain-v2 status list (XY badges) plus a "full diff" entry;
  clicking loads the server-colorized diff.

Split terminal+viewer layout is a nice-to-have, not v1.

## Rendering

All HTML is built in Rust (small string-building helpers, no template engine):

- Markdown: pulldown-cmark, wrapped in `.markdown-body` (github-markdown-css
  stays vendored).
- Code: served as escaped text; highlight.js runs client-side on htmx
  `afterSwap` (avoids compiling syntect's grammar blob into the binary).
- Diffs: unified diff classified line-by-line server-side into
  `.dl .add/.del/.hunk/.meta/.ctx` divs.
- Untracked files diff as all-new; `git diff HEAD` otherwise (v1 logic ports).

Limits carried from v1: 2 MB file cap, binary sniff (NUL in first 8 kB unless
a known text extension), `SKIP_DIRS = {.git, target, node_modules,
__pycache__, .venv}` plus per-project `hide` config.

## Settings

Text-based, Claude-editable, re-read on every request — no restart, no reload
endpoint:

1. `~/.config/deadlight/config.toml` — global defaults
2. `{project}/.deadlight/config.toml` — per-project overrides (shallow merge,
   per-key)

v1 keys:

```toml
theme = "dark"          # built-in theme name
default_tab = "terminal" # terminal | files | changes
hide = ["dist"]          # extra skip-dirs, appended to SKIP_DIRS
```

Unknown keys are ignored; a malformed file falls back to defaults and renders
a visible warning in the header (never a crash).

### Themes

Built-in themes are CSS-variable sets shipped in `static/themes/` (v1: `dark`,
`light`, `gruvbox`, `solarized-dark`). The workspace page links the selected
theme's stylesheet. If `{project}/.deadlight/theme.css` exists it is served
after the theme (via `/frag/{project}/theme.css`) for arbitrary overrides — so
Claude can pick a theme or author one.

## Stack

Rust binary, no async runtime:

| Crate | Role |
|-------|------|
| tiny_http | HTTP serving + connection upgrade |
| tungstenite | websocket over the upgraded stream |
| portable-pty | PTY for `zellij attach` |
| pulldown-cmark | markdown |
| toml + serde | config |

git operations shell out to the `git` binary (porcelain=v2 parsing ports from
server.py). No serde_json — responses are HTML.

Known risk: the tiny_http → tungstenite upgrade handshake is manual (~30
lines: compute `Sec-WebSocket-Accept`, send 101, wrap the raw stream). If it
fights us, the sanctioned fallback is switching to axum; nothing else in the
design changes.

## Security

- Bind `127.0.0.1:8444` only; exposure is exclusively via
  `tailscale serve --bg --https=8444 8444`. **The websocket is a shell** — the
  localhost bind is the security boundary and must never be widened.
- Every file/repo path resolves through the ROOTS check (`Path::canonicalize`
  + prefix test) before any read; `/static` is prefix-checked against its own
  dir.
- `.deadlight/theme.css` is the only per-project file served raw; it is served
  as `text/css` only.

## Errors

- Fragment endpoints return an error `<div class="hint">` with the message —
  the pane shows it, the page survives.
- Websocket drop shows a "disconnected" overlay on the terminal until
  reconnect succeeds.
- git failures (not a repo, timeout) render as hints in the Changes pane;
  non-repo projects simply show no branch/badge.

## Testing

- Unit: path-safety (traversal, symlink escape), porcelain-v2 parsing
  (including `2 ` rename lines), config cascade/merge, diff classification.
- Integration: spawn the server on an ephemeral port, curl `/`, `/{project}`,
  each fragment endpoint, a ws echo through a dummy PTY command.
- Manual: full browser pass — attach, reload, duplicate tab, laptop-sleep
  reconnect, theme switch via editing config.toml.

## Deployment

- systemd user unit `~/.config/systemd/user/deadlight.service`
  (`ExecStart={repo}/target/release/deadlight`, `Restart=always`). Note:
  `systemctl --user daemon-reload` may be blocked by the permission
  classifier — have Peter run it via `!`.
- tailscale serve 8444 (8082 zellij-web and 8443 code-server stay until
  deadlight has earned trust; retire them later, not now).

## Deleted / superseded

`server.py`, most of `static/app.js` (shrinks to xterm glue + hash mirror +
`r` key), `static/index.html` (server-rendered now), vendored marked.
Kept: highlight.js, github-markdown-css.

## Nice-to-haves (post-v1, only if asked)

Split terminal/viewer layout, images in markdown preview, git log view,
mobile layout, per-theme favicon.
