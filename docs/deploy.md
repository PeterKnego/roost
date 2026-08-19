# resh — running and deploying

Design lives in `docs/superpowers/specs/`. This file is only the operational
knowledge that is not derivable from the code, and that has already caused a
silent failure at least once each.

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
| `RESH_ROOTS` | Colon-separated project roots | `/home/claude/ultima:/home/claude/projects` |
| `RESH_STATE_DIR` | Workspace state + dtach sockets | `~/.local/state/resh/` |
| `RESH_ORIGINS` | Comma-separated origin allowlist | global config, else loopback only |
| `RESH_CMD` | Terminal command override — **test hook, never set in production** | `dtach -A … -E -r winch -z $SHELL -l` |
| `RESH_DEBOUNCE_MS` | Filesystem-watch debounce | 300 |

Running anywhere other than the deploy host needs at least `RESH_ROOTS`,
since the defaults are that host's paths. Give a second instance its own
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

## Deploying to ubuntu-16gb-hel1-2

**The unit runs `~/.local/bin/resh`, not `target/release/resh`** —
and `~/.cargo/config.toml` redirects `target-dir` to `~/.cache/cargo-target`,
so a plain `cargo build --release` updates neither path the service uses.
Building without the install step leaves the old binary running and looks
exactly like a successful deploy that changed nothing.

```bash
ssh claude@77.42.80.36                       # see the ssh note above
cd /home/claude/projects/resh
git checkout master && git pull --ff-only
cargo build --release
install -m 755 ~/.cache/cargo-target/release/resh ~/.local/bin/resh
systemctl --user restart resh
```

**Check the branch before pulling.** The box was once left on a feature
branch, where `git pull --ff-only` cheerfully reported "Already up to date"
while sitting seven commits behind. Verify the resulting commit, not the pull
output.

## `KillMode=process` is load-bearing

resh spawns its `dtach` sessions as child processes. systemd's default
`KillMode=control-group` kills the entire cgroup on stop, taking every dtach
session with it and defeating the whole reason dtach is used.

Verified in production: with the default, restarting resh lost the
running shell (`pgrep -c dtach` went to 0); with `KillMode=process`, only the
client dies and the session survives — a shell variable set before a restart
is still there afterwards.

## `allowed_origins` is global-config only

It must list the tailnet origin or the browser gets 403 over tailscale. The
node is `resh` and `tailscale serve` maps `:443`, so the origin carries no
port:

```toml
allowed_origins = ["https://resh.tail66d083.ts.net"]
```

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
`{project}/.resh/config.toml` for theme/hide), so a wrong value is fixed
by editing the file, not redeploying.

## Host notes

`tailscale serve` and `tailscale set` work without sudo (the account is the
tailscale operator); the account's sudo password is *not* its ssh password.

**There is no editor fallback any more.** code-server was removed on
2026-08-18 (service disabled, `~/.local/lib/code-server-*`,
`~/.local/share/code-server` and its config deleted, and the `:8443` tailscale
serve route dropped), so resh on `:8444` is the only web workspace on this
host. If resh is down, the way in is ssh.

Zellij went at the same time. It had been replaced by dtach back in v3 but its
`--server` processes and a `zellij web --daemonize` had kept running for weeks —
20 processes holding ~1.1 GB, all with sessions its own `list-sessions` reported
as EXITED, so `kill-all-sessions` would not clean them up and they had to be
killed by pid. Note the bare tailnet hostname (`https://…ts.net/`, no port)
still proxies to `127.0.0.1:8082`, which was zellij web — that route now points
at nothing.
