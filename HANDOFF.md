# deadlight — handoff

## What this is

A **stateless, read-only web viewer** for the projects on this box, named after
the nautical deadlight: a fixed pane that cannot be opened. It replaces the
file-browsing/markdown-preview role of code-server, whose stateful
browser↔host machinery (extension hosts, webviews, per-workspace IndexedDB)
kept desyncing after tab reloads and laptop sleep. Peter's workflow: Claude
Code in zellij-web does all the editing/heavy lifting (works great, keeps
reconnecting fine); the IDE was only ever needed for four things —
this tool provides exactly those and nothing else:

1. project file tree browsing
2. git working-tree changes view (status + diffs)
3. rendered markdown preview
4. syntax-highlighted code viewing

Design contract: **no websockets, no sessions, no client state, no writes.**
Every request re-reads disk, so refresh always shows the truth and nothing can
get out of sync. Failure mode is "hit refresh".

## Status: ~80% built, NOT yet run/tested

Done:
- `server.py` — complete stdlib-only HTTP server (Python 3.14, no deps).
  Binds `127.0.0.1:8444`. Endpoints: `/api/projects`, `/api/tree?path=`,
  `/api/file?path=`, `/api/git/status?repo=`, `/api/git/diff?repo=[&path=]`,
  `/` + `/static/*`. Path access restricted to `ROOTS`
  (`/home/claude/ultima`, `/home/claude/projects`); `SKIP_DIRS` hides
  `.git/target/node_modules`; 2 MB file cap; untracked files diff as all-new.
- `static/index.html` — page shell (header: project selector, Files/Changes
  tabs, branch label, change-count badge, refresh button; sidebar + content).
- `static/app.js` — full frontend logic: hash routing
  (`#/<files|changes>/<project>/<relpath>`), lazy-expanding tree, changes list,
  md rendering (marked) + highlighting (highlight.js), homemade unified-diff
  colorizer (`.dl .add/.del/.hunk/.meta` classes), `r` key + focus refresh.
- `static/vendor/` — vendored (no CDN at runtime): marked 12, highlight.js
  11.9 + github-dark css, github-markdown-css 5 (dark).

## Remaining work

1. **`static/style.css` — does not exist yet; the page is unstyled without
   it.** Needs: dark theme; header bar; two-column layout (sidebar ~280px
   scrollable, content pane scrollable); tree/list styling (`.dir`, `.file`,
   `.sel`, `.xy` badges); `.codeview`/`.path` blocks; diff line classes above;
   `.markdown-body` container padding (bg comes from github-markdown-css).
2. **Test end-to-end**: `python3 server.py` then curl the API endpoints and
   click through the UI. Untested code — expect small bugs (tree rendering,
   porcelain-v2 parsing for renames (`2 ` lines) is the most suspect).
3. **systemd user unit** `~/.config/systemd/user/deadlight.service`
   (`ExecStart=/usr/bin/python3 /home/claude/projects/deadlight/server.py`,
   `Restart=always`). Note: `systemctl --user daemon-reload` was blocked by
   the permission classifier in the previous session — have Peter run it via
   `!` if blocked again.
4. **Expose via tailscale**: `tailscale serve --bg --https=8444 8444`
   (serve status already proxies 8082→zellij-web and 8443→code-server; keep
   those). URL: `https://<deploy-host>.<tailnet>.ts.net:8444`.
5. Nice-to-haves (only if Peter asks): images in md preview (relative paths),
   directory git-log view, mobile layout.

## Context worth knowing

- See memory `code-server-webview-lock-contention` for the full failure
  analysis that motivated this tool.
- code-server stays running as fallback (don't restart it casually — Peter's
  live Claude sessions run under its quint-llm-kit window's extension host).
- Name history: was `porthole`, renamed — too crowded on GitHub. `deadlight`
  chosen deliberately; keep it.
- Repo is git-initialized, no commits yet. Peter's email: peter@knego.net.
