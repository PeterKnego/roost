# deadlight — running and deploying

Design lives in `docs/superpowers/specs/`. This file is only the operational
knowledge that is not derivable from the code, and that has already caused a
silent failure at least once each.

## Running

`cargo run` binds `127.0.0.1:8444`; the sole CLI argument is the port. Tests:
`cargo test` (never `--release`). Everything else is environment:

| Variable | Purpose | Default |
|---|---|---|
| `DEADLIGHT_ROOTS` | Colon-separated project roots | `/home/claude/ultima:/home/claude/projects` |
| `DEADLIGHT_STATE_DIR` | Workspace state + dtach sockets | `~/.local/state/deadlight/` |
| `DEADLIGHT_ORIGINS` | Comma-separated origin allowlist | global config, else loopback only |
| `DEADLIGHT_CMD` | Terminal command override — **test hook, never set in production** | `dtach -A … -E -r winch -z $SHELL -l` |
| `DEADLIGHT_DEBOUNCE_MS` | Filesystem-watch debounce | 300 |

Running anywhere other than the deploy host needs at least `DEADLIGHT_ROOTS`,
since the defaults are that host's paths.

`dtach` is a **runtime prerequisite** (`brew install dtach` / `apt install
dtach`). Without it, terminals fail at spawn.

## Deploying to ubuntu-16gb-hel1-2

**The unit runs `~/.local/bin/deadlight`, not `target/release/deadlight`** —
and `~/.cargo/config.toml` redirects `target-dir` to `~/.cache/cargo-target`,
so a plain `cargo build --release` updates neither path the service uses.
Building without the install step leaves the old binary running and looks
exactly like a successful deploy that changed nothing.

```bash
tailscale ssh claude@ubuntu-16gb-hel1-2      # Tailscale SSH is enabled
cd /home/claude/projects/deadlight
git checkout master && git pull --ff-only
cargo build --release
install -m 755 ~/.cache/cargo-target/release/deadlight ~/.local/bin/deadlight
systemctl --user restart deadlight
```

**Check the branch before pulling.** The box was once left on a feature
branch, where `git pull --ff-only` cheerfully reported "Already up to date"
while sitting seven commits behind. Verify the resulting commit, not the pull
output.

## `KillMode=process` is load-bearing

deadlight spawns its `dtach` sessions as child processes. systemd's default
`KillMode=control-group` kills the entire cgroup on stop, taking every dtach
session with it and defeating the whole reason dtach is used.

Verified in production: with the default, restarting deadlight lost the
running shell (`pgrep -c dtach` went to 0); with `KillMode=process`, only the
client dies and the session survives — a shell variable set before a restart
is still there afterwards.

## `allowed_origins` is global-config only

It must list the tailnet origin or the browser gets 403 over tailscale:

```toml
allowed_origins = ["https://ubuntu-16gb-hel1-2.tail66d083.ts.net:8444"]
```

Loopback always passes unlisted. It is deliberately **not** readable from a
project's `.deadlight/config.toml`, so a repo you clone cannot allowlist its
own domain. Rejections are logged with the offending values — check
`journalctl --user -u deadlight` when access mysteriously 403s.

Config is re-read every request (`~/.config/deadlight/config.toml`, then
`{project}/.deadlight/config.toml` for theme/hide), so a wrong value is fixed
by editing the file, not redeploying.

## Host notes

`tailscale serve` and `tailscale set` work without sudo (the account is the
tailscale operator); the account's sudo password is *not* its ssh password.

code-server stays running as a fallback — don't restart it casually.
