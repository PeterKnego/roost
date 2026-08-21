# resh — running and deploying

Design lives in `docs/superpowers/specs/`. This file is only the operational
knowledge that is not derivable from the code, and that has already caused a
silent failure at least once each.

**Host-specific values live outside this repository**, in
`~/.config/resh/deploy-host.md` on the machine itself: addresses, tailnet
names, ports, roots, and the deploy command with those values filled in. They
describe one machine, they change without the code changing, and a checkout is
the wrong place to keep them. This file stays true of any deployment.

## Running

**The project was called `deadlight` until 2026-08-18.** Anything on disk from
before then — `~/.local/state/deadlight/`, a `deadlight.service` unit, a
`~/.local/bin/deadlight` binary, `~/.config/deadlight/config.toml` — is from
the old name and is not read by this build. The config file is the one that
bites: `allowed_origins` is global-config-only (see *`allowed_origins` is
global-config only*, below), so a host still carrying
`~/.config/deadlight/config.toml` after the rename has resh read
`~/.config/resh/config.toml` instead — which doesn't exist, so the allowlist
is empty and every tailnet browser request 403s while loopback still works.
That is the exact symptom the `allowed_origins` section tells you to check
for; this rename is an undocumented cause of it. The historical design
documents under `docs/superpowers/` keep the old name deliberately: they
record what was true when they were written.

**The per-project config and theme directory also moved**, from
`{project}/.deadlight/` to `{project}/.resh/` (`config.toml` and
`theme.css`). A project still carrying only the old `.deadlight/` directory
silently loses its theme and hide settings — `.resh/` is what gets read now,
and nothing migrates the old one automatically. Verified: no project on the
deploy host has a `.deadlight` directory, so this affects no project today,
but rename it by hand (`git mv .deadlight .resh` inside the project, if it's
tracked) on any project that carries one.

`cargo run` binds `127.0.0.1:8444`; the sole CLI argument to the server itself
is the port. The one other subcommand is `resh notify <title> [body]`,
which never binds a port — see [`docs/notifications.md`](notifications.md).
Tests: `cargo test` (never `--release`). Everything else is environment:

| Variable | Purpose | Default |
|---|---|---|
| `RESH_ROOTS` | Colon-separated project roots | **required** — no built-in default |
| `RESH_STATE_DIR` | Workspace state + dtach sockets | `~/.local/state/resh/` |
| `RESH_ORIGINS` | Comma-separated origin allowlist | global config, else loopback only |
| `RESH_CMD` | Terminal command override — **test hook, never set in production** | `dtach -A … -E -r winch -z $SHELL -l` |
| `RESH_DEBOUNCE_MS` | Filesystem-watch debounce | 300 |
| `RESH_PING_SECS` | Websocket keepalive ping interval — lower it only to test that path | 30 |
| `RESH_STATIC` | Serve web assets from this directory instead of the embedded copies (development) | unset — assets are compiled in |

`RESH_ROOTS` is required: the binary carries no compiled-in default, and
starting without it exits 2 with a message rather than serving an empty
project list — which would come up healthy and look exactly like every project
had vanished. One host's paths used to be the default, which put that machine's
layout into every binary. Give a second instance its own
`RESH_STATE_DIR` too — sharing one is safe as of the `.origin` marker (see
*Projects and sessions*), but two instances sharing a state dir will still show
each other's projects in the strip, which is rarely what you want.

`dtach` is a **runtime prerequisite** (`brew install dtach` / `apt install
dtach`). Without it, terminals fail at spawn.

`notifications.json` (the persisted notice store) lives alongside workspace
state under `RESH_STATE_DIR`. OS notifications additionally need a
secure context — `localhost` or an HTTPS origin such as `tailscale serve`;
plain `http://` to a tailnet IP still shows the in-page notice panel but
cannot ask the OS for permission.

## Projects and sessions

A **project** is any directory under `RESH_ROOTS` that has been opened in
resh. It is normally a git repository; a plain directory still works — the
terminal placeholder offers `git init` and a "start without git" escape.
A git worktree is its own project, and is discovered by asking
`git worktree list`, not by walking the filesystem, so worktrees under
`.claude/worktrees/` are found even though `.claude` is otherwise skipped.

Terminals are **never started implicitly**. Opening a project creates a
Terminal *tab*, not a session; the session begins only when the user presses
Enter or clicks the placeholder. Before this, merely looking at a project
forked a shell that nothing ever reaped — which is how the deploy host
accumulated 13 live shells, 9 of them belonging to directories that no longer
existed.

Sessions outlive both the browser tab and the resh process, so the
**registry is rebuilt at startup** rather than kept only in memory: resh
lists `$RESH_STATE_DIR/*.json` for saved workspaces, walks
`$RESH_STATE_DIR/sock/` for candidate sessions, and checks which sockets a
live `dtach` still holds. Reaping runs at startup and, throttled to once every
few seconds, whenever the project list is enumerated:

- a socket with no process is deleted;
- a session whose project directory is confirmed gone is killed and its socket
  removed;
- a socket is only ever unlinked *after* its holder is confirmed dead, so a
  failed kill leaves an ugly-but-discoverable session rather than an
  unreachable orphan nothing can find again.

**"Confirmed gone" is deliberately narrower than "I can't find it."** Each
project's socket directory holds an `.origin` marker recording the absolute path
the project resolved to. When a key no longer resolves under the current
`RESH_ROOTS`, reaping consults that recorded path rather than concluding the
project vanished — and a key with **no** marker is never reaped at all.

The same rule covers a marker that is present but says nothing usable: an empty
or truncated one, or a recorded path the filesystem cannot answer for (an
`EACCES` after a `chmod`, an unmounted disk, a downed autofs/NFS mount). Only a
recorded path the OS positively reports as *absent* counts. The marker is
written by rename (a transient `.origin.tmp.<pid>` may appear beside it) so no reader
can ever see it half-written, and it is rewritten only when the recorded path
actually changes.

This matters because two resh instances, or one restarted with different
roots, can share a state dir. Without the distinction, starting resh with
different `RESH_ROOTS` against the same `RESH_STATE_DIR` SIGKILLed
every session outside those roots and logged "project directory is gone" about
directories that were never touched — destroying exactly the state dtach exists
to preserve. Reproduced against real dtach before the fix; a regression test
pins it.

Consequence for the first deploy of this version: a state dir written by the
previous version has no markers, so its pre-existing sessions are never reaped
automatically. That is the safe direction — stale rows, not dead shells — and a
project still on disk gains its marker the next time it is opened.

**But a project whose directory is already gone can never gain one**, because
markers are written only when a path resolves successfully. So the orphans that
motivated this whole feature — the 13 live shells found on this host, 9 of them
belonging to deleted directories — survive the deploy and are permanently
unreapable. Deploying does not clean them up; it stops new ones accruing. Clear
them once, by hand:

```bash
# see what is holding sockets, and for which project keys
ps -Aww -o pid=,args= | grep "$HOME/.local/state/resh/sock/"
# for each key whose directory is genuinely gone: kill it, then drop the key dir
kill -9 <pid>
rm -rf "$HOME/.local/state/resh/sock/<key>"
```

Check each directory before killing — a key you do not recognise may be a
project that still exists under different roots, which is exactly the case the
`.origin` marker was added to protect.

One other legacy case, almost certainly hypothetical: a project whose directory
name contains a control character (a newline, say) used to be keyed with that
byte raw and is now percent-encoded, so its old state file and socket directory
become unreachable — and deliberately unreapable, since who holds such a socket
cannot be determined from `ps` output. If `ls "$HOME/.local/state/resh/sock/"`
shows a key spanning two lines, that is one; clear it by hand as above.

Reaping is also suspended entirely when `ps` cannot be trusted (non-zero exit,
or empty output, which on a live host means the listing failed rather than that
nothing is running), for the same reason.

Both are logged, so `journalctl --user -u resh | grep -i reap` after a
restart tells you what the startup sweep decided.

**Closing a terminal tab ends that session.** Its × kills the shell and its
`dtach` **master** — the part that matters: in `-A` mode dtach forks a master
that reparents to init, so killing only resh's own client is a *detach*, not an
end. **Alt-click the × to detach instead**, dropping the tab and leaving the
shell running.

Survival is unaffected by this, because it only ever mattered for the
*involuntary* cases: a dropped browser tab, a closed laptop and a resh restart
send no intent at all, so those sessions live on and are re-attached by name.

**Close Project** ends all of a project's sessions at once. It keeps the saved
layout (reopening restores panes and tabs) and refuses while any buffer has
unsaved changes, listing them by name. It remains the only way to reach a
session with no tab — one orphaned by a browser dying mid-session, or by a
resh version predating per-tab ending. Nothing lists sessions by name in the
UI, and the per-project cap is `MAX_SESSIONS_PER_PROJECT` (16), so such
orphans are invisible and still occupy slots.

New terminals are named by the server — `term`, `term1`, `term2` — from
`session::live_names`, which sees detached sessions too. The client must never
choose: it knows only the sessions it has tabs for, and attaching *creates only
when absent*, so a client-chosen name would eventually reattach the user to an
old shell, scrollback and all, instead of giving them a new one.

## Deploying

**The unit runs `~/.local/bin/resh`, not `target/release/resh`** —
and `~/.cargo/config.toml` redirects `target-dir`, so a plain
`cargo build --release` updates neither path the service uses. Building
without the install step leaves the old binary running and looks exactly like
a successful deploy that changed nothing.

```bash
cd <checkout>
git checkout master && git pull --ff-only
cargo build --release
install -m 755 <target-dir>/release/resh ~/.local/bin/resh
systemctl --user restart resh
```

The concrete host, checkout path and target-dir are in
`~/.config/resh/deploy-host.md` on the machine.

**Check the branch before pulling.** The deploy box was once left on a feature
branch, where `git pull --ff-only` cheerfully reported "Already up to date"
while sitting seven commits behind. Verify the resulting commit, not the pull
output.

**Confirm the *running* binary changed**, not just the built one — the install
trap above makes a no-op deploy indistinguishable from a real one otherwise:

```bash
sha256sum ~/.local/bin/resh
systemctl --user show resh -p MainPID --value | xargs -I{} sha256sum /proc/{}/exe
```

The binary is self-contained: `static/` is compiled in, so the installed
`~/.local/bin/resh` no longer reads the checkout at runtime. Editing
`static/` on the host therefore does *not* change what the running service
serves — that needs a rebuild and reinstall. To iterate on the UI live, run a
second instance with `RESH_STATIC` pointed at a checkout.

Asset lookup is three layers, checked in this order: `$RESH_STATIC` first, then
`~/.config/resh/static/`, then the embedded copy. That order means a stale
`RESH_STATIC` left set on a real deployment silently wins over anything in the
user directory — it is meant for one developer's own instance, not to be left
exported on a shared host. The user directory is not limited to
`themes/{name}.css`: it mirrors `static/`'s own layout, so any path under it —
an overridden `style.css`, a custom font, a logo — is picked up in place of the
embedded file, restricted only by extension (`css`, `svg`, `png`, `jpg`,
`jpeg`, `gif`, `webp`, `ico`, `woff`, `woff2`, `ttf`, `otf`); a theme is
selected with `theme = "{name}"`, which resolves to
`~/.config/resh/static/themes/{name}.css`. A project theme goes in
`{project}/.resh/theme/`, entered through `style.css`; the older single
`.resh/theme.css` still works and the directory wins where both exist. Neither
the user directory nor a project may supply JavaScript or HTML — only
`$RESH_STATIC` can, and only whoever starts the process can set it.

## The development instance

Because the deployed binary ignores the checkout, iterating on the UI needs a
*second* resh whose assets come from a working tree — a transient unit with its
own port, state dir and `RESH_STATIC`. The exact invocation, ports and tailnet
routes for this host are in `~/.config/resh/deploy-host.md`.

**`KillMode=process` is not optional** on that unit. `systemd-run` defaults to
`control-group`, which SIGKILLs everything in the unit's cgroup on stop — the
dtach masters and their shells included. That is the same defect `resh.service`
was fixed for (see the "no systemd" row of CLAUDE.md's dev/prod substitution
table), and without this property the development instance still has it: every
restart silently destroys the shells running under it. The loss is the smaller
half of the cost. The larger half is that it looks like a resh bug — a terminal
that comes back empty, with a shell that has forgotten everything, reads
exactly like a reconnect respawning the session rather than reattaching to it.
That misdiagnosis has already cost time once.

With `KillMode=process`, stopping the unit leaves its dtach sessions running
and systemd says so ("Unit process ... remains running after unit stopped");
the deployed service behaves the same way, and that is the intended direction —
a stale shell is recoverable, a killed one is not.

**It needs its own `RESH_STATE_DIR`.** Two instances sharing one show each
other's projects in the header strip and each other's sessions in the socket
directory, which is rarely what you want from a throwaway dev server.

**Its origin must be allowlisted separately, port included.** The `Origin`
header a browser sends for a non-default port carries that port, which the
unqualified entry does not cover, so `allowed_origins` must list both. Miss the
second one and the failure is confusing rather than obvious: pages load over
plain HTTP while every websocket 403s, so the workspace renders with no tabs
and no terminals.

Adding a dev route widens what can open a websocket — and a websocket spawns a
shell — by exactly one origin. Keep both tailnet-only; do not `tailscale
funnel` them.

## `KillMode=process` is load-bearing

resh spawns its `dtach` sessions as child processes. systemd's default
`KillMode=control-group` kills the entire cgroup on stop, taking every dtach
session with it and defeating the whole reason dtach is used.

Verified in production: with the default, restarting resh lost the
running shell (`pgrep -c dtach` went to 0); with `KillMode=process`, only the
client dies and the session survives — a shell variable set before a restart
is still there afterwards.

## `allowed_origins` is global-config only

It must list the tailnet origin or the browser gets 403 over tailscale. Where
`tailscale serve` maps `:443` the origin carries no port; any other port must
be listed with it (see the dev-instance note above):

```toml
allowed_origins = ["https://<node>.<tailnet>.ts.net"]
```

This host's actual entries are in `~/.config/resh/deploy-host.md`.

This file lives at `~/.config/resh/config.toml`. It is the *only* place an
origin can be allowlisted — a project's own `.resh/config.toml` never can, so
a repo you clone cannot allowlist its own domain. Moving or losing this file
empties the allowlist, which 403s every tailnet browser request while loopback
keeps working: a confusing symptom whose cause is not in the logs.

Loopback always passes unlisted. It is deliberately **not** readable from a
project's `.resh/config.toml`, so a repo you clone cannot allowlist its
own domain. Rejections are logged with the offending values — check
`journalctl --user -u resh` when access mysteriously 403s.

Config is re-read every request (`~/.config/resh/config.toml`, then
`{project}/.resh/config.toml` for theme/hide/show_hidden), so a wrong value is
fixed by editing the file, not redeploying.

### Autosave

The editor writes a buffer out a second after the last keystroke, and
immediately when it loses focus. To turn that off:

```toml
autosave = false
```

Globally or per project, read on the next page load — this one is embedded in
the page (`data-autosave`), not resolved per request, so an open tab keeps the
value it loaded with until it is reloaded.

It is a per-project setting on purpose, unlike `allowed_origins` and
`max_upload_bytes` above: nothing a hostile checkout could put here widens a
boundary. It only decides whether the person editing that project's own files
has to press ⌘S.

Autosave never forces. It goes through the same conflict-guarded save, so a
file that changed underneath the buffer is not overwritten — and once that has
happened, autosave stops for that buffer instead of re-raising the conflict
banner every second. An explicit ⌘S is what resolves it, and a save that
actually lands is what starts autosave again.

### Hidden files in the tree

The file tree hides every entry beginning with a dot — `.git`, `.claude`,
`.gitignore` — along with `target`, `node_modules`, `__pycache__` and anything
in `hide`. To see the dot entries:

```toml
show_hidden = true
```

Globally or per project, and it takes effect on the next tree render, no
restart. It reveals dot entries only: build and vendor directories stay hidden
either way, as does anything you listed in `hide`, which outranks it.

This file sets what a workspace *starts out* doing. The ◌/◍ control in the tree
pane's header overrides it per workspace — mirrored to every browser on that
project and persisted in the workspace state file — and overrides it in both
directions, so a workspace can be toggled off under a global
`show_hidden = true`. Once toggled, the config value no longer moves that
workspace; deleting its state file is what restores "follow the config".

Two things it deliberately does not change. `.git`'s *contents* still do not
refresh the tree as they change (a single git command writes enough inside it
to turn every command into a burst of refreshes), so an expanded `.git` goes
stale until you re-expand it. And uploads into `.git` or `.claude` stay refused
whatever the setting says — those hold a second copy of the repository.
