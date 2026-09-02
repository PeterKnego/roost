# roost — running and deploying

Design lives in `docs/superpowers/specs/`. This file is only the operational
knowledge that is not derivable from the code, and that has already caused a
silent failure at least once each.

**Host-specific values live outside this repository**, in
`~/.config/roost/deploy-host.md` on the machine itself: addresses, tailnet
names, ports, roots, and the deploy command with those values filled in. They
describe one machine, they change without the code changing, and a checkout is
the wrong place to keep them. This file stays true of any deployment.

## Running

**The project was called `resh` until 2026-09-02, and `deadlight` before
2026-08-18.** Anything on disk from before then — `~/.local/state/resh/`, a
`resh.service` unit, a `~/.local/bin/resh` binary, `~/.config/resh/config.toml`
— is from an old name and is not read by this build unless you point it there
(see the next paragraph). The config file is the one that bites:
`allowed_origins` is global-config-only (see *`allowed_origins` is global-config
only*, below), so a host still carrying `~/.config/resh/config.toml` after the
rename has roost read `~/.config/roost/config.toml` instead — which doesn't
exist, so the allowlist is empty and every tailnet browser request 403s while
loopback still works. That is the exact symptom the `allowed_origins` section
tells you to check for; this rename is an undocumented cause of it. The
historical design documents under `docs/superpowers/` keep the old names
deliberately: they record what was true when they were written.

**Do not move the state directory while sessions are live.** Every dtach
master carries its socket path in its argument list, and the registry
identifies a socket's holder by matching that exact path in `ps` output. A
socket reached through a new path — moved *or* symlinked — reads as held by
nothing, and a socket held by nothing is unlinked, which leaves its shell
alive but unreachable forever. Either end every session first and then move
`~/.local/state/resh/` to `~/.local/state/roost/`, or keep the old path by
setting `ROOST_STATE_DIR=$HOME/.local/state/resh` in the unit. The author's
own host does the latter.

**The per-project config and theme directory also moved**, from
`{project}/.resh/` to `{project}/.roost/` (`config.toml` and `theme/`). A
project still carrying only the old `.resh/` directory silently loses its
theme and hide settings — `.roost/` is what gets read now, and nothing
migrates the old one automatically. Verified 2026-09-02: no project under the
deploy host's roots has a `.resh` directory. Rename it by hand (`git mv .resh
.roost` inside the project, if it's tracked) on any project that carries one.

`cargo run` binds `127.0.0.1:8444`; the sole CLI argument to the server itself
is the port. One subcommand never binds a port: `roost notify <title> [body]`
(see [`docs/notifications.md`](notifications.md)).
Tests: `cargo test` (never `--release`). Everything else is environment:

| Variable | Purpose | Default |
|---|---|---|
| `ROOST_ROOTS` | Colon-separated project roots | global config `roots`, else none — roost exits 2 |
| `ROOST_STATE_DIR` | Workspace state + dtach sockets | `~/.local/state/roost/` |
| `ROOST_ORIGINS` | Comma-separated origin allowlist | global config, else loopback only |
| `ROOST_CMD` | Terminal command override — **test hook, never set in production** | `dtach -A … -E -r winch -z $SHELL -l` |
| `ROOST_DEBOUNCE_MS` | Filesystem-watch debounce | 300 |
| `ROOST_PING_SECS` | Websocket keepalive ping interval — lower it only to test that path | 30 |
| `ROOST_HEALTH_SECS` | Interval for the periodic health pass, which only logs. Values under 10 are ignored | 300 |
| `ROOST_CONFIG` | Path to the global config file, overriding `~/.config/roost/config.toml` | unset |
| `ROOST_STATIC` | Serve web assets from this directory instead of the embedded copies (development) | unset — assets are compiled in |

`ROOST_ROOTS` is required unless the global config supplies `roots`: the binary
carries no compiled-in default, and starting with neither exits 2 with a
message rather than serving an empty project list — which would come up healthy
and look exactly like every project had vanished. One host's paths used to be
the default, which put that machine's layout into every binary. The env var
wins when both are set, so the unit file stays authoritative for the service;
the config entry exists for callers that inherit none of the unit's
environment, which today means nothing shipped with roost, but the key stays
so a second instance's tooling can read it. Give a second instance its own
`ROOST_STATE_DIR` too — sharing one is safe as of the `.origin` marker (see
*Projects and sessions*), but two instances sharing a state dir will still show
each other's projects in the strip, which is rarely what you want.

`dtach` is a **runtime prerequisite** (`brew install dtach` / `apt install
dtach`). Without it, terminals fail at spawn.

`notifications.json` (the persisted notice store) lives alongside workspace
state under `ROOST_STATE_DIR`. OS notifications additionally need a
secure context — `localhost` or an HTTPS origin such as `tailscale serve`;
plain `http://` to a tailnet IP still shows the in-page notice panel but
cannot ask the OS for permission.

## Upgrading from a build that shipped `resh peers`

`resh peers` is gone (spec `docs/superpowers/specs/2026-08-25-worktree-launch-design.md`).
Remove its `SessionStart` entry from `~/.claude/settings.json` on every host that
had it; left in place it prints `command not found` at every session start —
loud, harmless, and the reason this note exists.

## Projects and sessions

A **project** is any directory under `ROOST_ROOTS` that has been opened in
roost. It is normally a git repository; a plain directory still works — the
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

Sessions outlive both the browser tab and the roost process, so the
**registry is rebuilt at startup** rather than kept only in memory: roost
lists `$ROOST_STATE_DIR/*.json` for saved workspaces, walks
`$ROOST_STATE_DIR/sock/` for candidate sessions, and checks which sockets a
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
`ROOST_ROOTS`, reaping consults that recorded path rather than concluding the
project vanished — and a key with **no** marker is never reaped at all.

The same rule covers a marker that is present but says nothing usable: an empty
or truncated one, or a recorded path the filesystem cannot answer for (an
`EACCES` after a `chmod`, an unmounted disk, a downed autofs/NFS mount). Only a
recorded path the OS positively reports as *absent* counts. The marker is
written by rename (a transient `.origin.tmp.<pid>` may appear beside it) so no reader
can ever see it half-written, and it is rewritten only when the recorded path
actually changes.

This matters because two roost instances, or one restarted with different
roots, can share a state dir. Without the distinction, starting roost with
different `ROOST_ROOTS` against the same `ROOST_STATE_DIR` SIGKILLed
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
ps -Aww -o pid=,args= | grep "$HOME/.local/state/roost/sock/"
# for each key whose directory is genuinely gone: kill it, then drop the key dir
kill -9 <pid>
rm -rf "$HOME/.local/state/roost/sock/<key>"
```

Check each directory before killing — a key you do not recognise may be a
project that still exists under different roots, which is exactly the case the
`.origin` marker was added to protect.

One other legacy case, almost certainly hypothetical: a project whose directory
name contains a control character (a newline, say) used to be keyed with that
byte raw and is now percent-encoded, so its old state file and socket directory
become unreachable — and deliberately unreapable, since who holds such a socket
cannot be determined from `ps` output. If `ls "$HOME/.local/state/roost/sock/"`
shows a key spanning two lines, that is one; clear it by hand as above.

Reaping is also suspended entirely when `ps` cannot be trusted (non-zero exit,
or empty output, which on a live host means the listing failed rather than that
nothing is running), for the same reason.

Both are logged, so `journalctl --user -u roost | grep -i reap` after a
restart tells you what the startup sweep decided.

**Closing a terminal tab ends that session.** Its × kills the shell and its
`dtach` **master** — the part that matters: in `-A` mode dtach forks a master
that reparents to init, so killing only roost's own client is a *detach*, not an
end. **Alt-click the × to detach instead**, dropping the tab and leaving the
shell running.

Survival is unaffected by this, because it only ever mattered for the
*involuntary* cases: a dropped browser tab, a closed laptop and a roost restart
send no intent at all, so those sessions live on and are re-attached by name.

**Close Project** ends all of a project's sessions at once. It keeps the saved
layout (reopening restores panes and tabs) and refuses while any buffer has
unsaved changes, listing them by name. It remains the only way to reach a
session with no tab — one orphaned by a browser dying mid-session, or by a
roost version predating per-tab ending. Nothing lists sessions by name in the
UI, and the per-project cap is `MAX_SESSIONS_PER_PROJECT` (16), so such
orphans are invisible and still occupy slots.

New terminals are named by the server — `term`, `term1`, `term2` — from
`session::live_names`, which sees detached sessions too. The client must never
choose: it knows only the sessions it has tabs for, and attaching *creates only
when absent*, so a client-chosen name would eventually reattach the user to an
old shell, scrollback and all, instead of giving them a new one.

## Deploying

**The unit runs `~/.local/bin/roost`, not `target/release/roost`** —
and `~/.cargo/config.toml` redirects `target-dir`, so a plain
`cargo build --release` updates neither path the service uses. Building
without the install step leaves the old binary running and looks exactly like
a successful deploy that changed nothing.

```bash
cd <checkout>
git checkout master && git pull --ff-only
cargo build --release
install -m 755 <target-dir>/release/roost ~/.local/bin/roost
systemctl --user restart roost
```

The concrete host, checkout path and target-dir are in
`~/.config/roost/deploy-host.md` on the machine.

**Check the branch before pulling.** The deploy box was once left on a feature
branch, where `git pull --ff-only` cheerfully reported "Already up to date"
while sitting seven commits behind. Verify the resulting commit, not the pull
output.

**Confirm the *running* binary changed**, not just the built one — the install
trap above makes a no-op deploy indistinguishable from a real one otherwise:

```bash
sha256sum ~/.local/bin/roost
systemctl --user show roost -p MainPID --value | xargs -I{} sha256sum /proc/{}/exe
```

The binary is self-contained: `static/` is compiled in, so the installed
`~/.local/bin/roost` no longer reads the checkout at runtime. Editing
`static/` on the host therefore does *not* change what the running service
serves — that needs a rebuild and reinstall. To iterate on the UI live, run a
second instance with `ROOST_STATIC` pointed at a checkout.

Asset lookup is three layers, checked in this order: `$ROOST_STATIC` first, then
`~/.config/roost/static/`, then the embedded copy. That order means a stale
`ROOST_STATIC` left set on a real deployment silently wins over anything in the
user directory — it is meant for one developer's own instance, not to be left
exported on a shared host. The user directory is not limited to
`themes/{name}.css`: it mirrors `static/`'s own layout, so any path under it —
an overridden `style.css`, a custom font, a logo — is picked up in place of the
embedded file, restricted only by extension (`css`, `svg`, `png`, `jpg`,
`jpeg`, `gif`, `webp`, `ico`, `woff`, `woff2`, `ttf`, `otf`); a theme is
selected with `theme = "{name}"`, which resolves to
`~/.config/roost/static/themes/{name}.css`. A project theme goes in
`{project}/.roost/theme/`, entered through `style.css`; the older single
`.roost/theme.css` still works and the directory wins where both exist. Neither
the user directory nor a project may supply JavaScript or HTML — only
`$ROOST_STATIC` can, and only whoever starts the process can set it.

## The development instance

Because the deployed binary ignores the checkout, iterating on the UI needs a
*second* roost whose assets come from a working tree — a transient unit with its
own port, state dir and `ROOST_STATIC`. The exact invocation, ports and tailnet
routes for this host are in `~/.config/roost/deploy-host.md`.

**`KillMode=process` is not optional** on that unit. `systemd-run` defaults to
`control-group`, which SIGKILLs everything in the unit's cgroup on stop — the
dtach masters and their shells included. That is the same defect `roost.service`
was fixed for (see the "no systemd" row of CLAUDE.md's dev/prod substitution
table), and without this property the development instance still has it: every
restart silently destroys the shells running under it. The loss is the smaller
half of the cost. The larger half is that it looks like a roost bug — a terminal
that comes back empty, with a shell that has forgotten everything, reads
exactly like a reconnect respawning the session rather than reattaching to it.
That misdiagnosis has already cost time once.

With `KillMode=process`, stopping the unit leaves its dtach sessions running
and systemd says so ("Unit process ... remains running after unit stopped");
the deployed service behaves the same way, and that is the intended direction —
a stale shell is recoverable, a killed one is not.

**It needs its own `ROOST_STATE_DIR`.** Two instances sharing one show each
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

roost spawns its `dtach` sessions as child processes. systemd's default
`KillMode=control-group` kills the entire cgroup on stop, taking every dtach
session with it and defeating the whole reason dtach is used.

Verified in production: with the default, restarting roost lost the
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

This host's actual entries are in `~/.config/roost/deploy-host.md`.

This file lives at `~/.config/roost/config.toml`. It is the *only* place an
origin can be allowlisted — a project's own `.roost/config.toml` never can, so
a repo you clone cannot allowlist its own domain. Moving or losing this file
empties the allowlist, which 403s every tailnet browser request while loopback
keeps working: a confusing symptom whose cause is not in the logs.

Loopback always passes unlisted. It is deliberately **not** readable from a
project's `.roost/config.toml`, so a repo you clone cannot allowlist its
own domain. Rejections are logged with the offending values — check
`journalctl --user -u roost` when access mysteriously 403s.

Config is re-read every request (`~/.config/roost/config.toml`, then
`{project}/.roost/config.toml` for theme/hide/show_hidden), so a wrong value is
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

### Turning the Claude Code integration off

**Global config only.** On by default. To switch it off:

```toml
ide = false
```

roost then starts no ide listener, writes no lock file, and puts no
`CLAUDE_CODE_SSE_PORT` in a spawned shell — so `claude` never discovers roost
and falls back to its own terminal diffs. That is the only shape this switch
can take from roost's side: refusing an `openDiff` *after* the CLI has already
connected makes it log `Failed to show diff in IDE` and rethrow, which fails
the edit instead of degrading it.

**If you only want the diff drawn in the terminal, this is the wrong knob** —
use the CLI's own `diffTool` setting (`/config` → *Diff tool* → `terminal`).
That decision is made before the CLI ever calls `openDiff`, which is why it
degrades cleanly and `ide = false` does not.

Global only, for the same reason as `allowed_origins`: a checked-out repo must
not be able to switch an integration back on after you have switched it off.
An unreadable or unparseable global config leaves the integration **on** — a
typo elsewhere in that file must not silently disable it. (`share_selection`
defaults the other way, and deliberately: see below.)

### Sharing the editor selection with Claude

Off by default. When on, roost sends whatever text is currently highlighted in
the editor to every Claude connected over the ide socket, as ambient context,
on a 200ms debounce after the selection stops changing — not only when you
press a key that means "share this." That is a different posture than every
other IDE integration in this file: `allowed_origins` and the Origin checks
guard who can *reach* roost, and `@`-mentioning a file (Alt+K) is a deliberate
act; this ships file contents with no explicit gesture at all, the moment
someone selects a line. A highlighted line of `.env` leaves the host exactly
the same way a highlighted line of anything else does. To turn it on:

```toml
share_selection = true
```

**Global config only**, like `allowed_origins` and `max_upload_bytes`. It was
settable per-project until 2026-08-23, on the argument that a project enabling
it only exposes its own files, so there is no ceiling to widen. True as far as
it goes — but it left a "does file content leave this machine" decision in a
file a cloned repo ships. Every such decision now lives in the one config file
that is yours rather than a checkout's. A `share_selection` line in
`{project}/.roost/config.toml` is ignored.

A malformed project `.roost/config.toml` is skipped whole, not partially
applied (see "Config is re-read every request" above) — so with a global
`share_selection = true`, a typo anywhere else in that file silently leaves
sharing *on* for that project, with only the page's `⚠ config` warning to
notice by.

Read once per page load like `autosave`, not resolved per request: an open tab
keeps the value it loaded with until reloaded. Whenever it is on, the header
shows `⧉ sharing selection` for as long as the tab is open — that indicator is
the whole visibility half of the contract (roost has no permission system to
scope this the way Claude Code's own `Read` deny rules do), so its absence is
the only thing standing between a stale `share_selection = true` and a
never-again-noticed cross-project accident. Checked again on the server for
every selection roost receives, so flipping the key back off during a session
takes effect on the very next selection change, not on the next reload.

### Prompting before a second Claude

On by default. When roost has positive evidence a Claude is already running in
a project — a terminal it typed `claude` into, or a connection on the IDE
socket — a further ✻ click asks instead of opening another terminal there.
The prompt is sent to the clicker alone; nothing changes for anyone else
looking at the project, and no session name is allocated until the click is
confirmed. To turn it off, so ✻ always opens a terminal the way it did before
this existed:

```toml
worktree_prompt = false
```

**Global config only**, like `allowed_origins` and `ide`: it changes what a
button does in every project, and a checkout must not get to decide that. An
unreadable or unparseable global config leaves the prompt **on** — a typo
elsewhere in that file must not silently change what a click does.

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
