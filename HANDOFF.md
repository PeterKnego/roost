# deadlight — handoff

Per-project remote workspace: a four-pane IDE-style layout (left-top,
left-bottom, middle, right, three draggable dividers) in one Rust binary.
Tabs are universal — one flat `Tab` type (Tree, Changes, File, Diff,
Terminal) and any tab can live in any pane. All workspace state lives on
the server and is live-mirrored to every connected browser, the way two
zellij clients used to mirror one screen: open a file in one browser and
it opens in all of them.

- **Design spec:** `docs/superpowers/specs/2026-08-16-deadlight-v3-workspace-design.md`
- **Implementation plan:** `docs/superpowers/plans/2026-08-16-deadlight-v3.md`
- v2 history: `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md`,
  `docs/superpowers/plans/2026-08-16-deadlight-v2.md`.
- Run: `cargo run` (127.0.0.1:8444), tests: `cargo test`.
- Env overrides: `DEADLIGHT_ROOTS` (colon-separated project roots, else the
  deploy host's `/home/claude/{ultima,projects}`), `DEADLIGHT_CMD` (terminal
  command), `DEADLIGHT_ORIGINS` (comma-separated origin allowlist),
  `DEADLIGHT_STATE_DIR` (where workspace state and dtach sockets live,
  default `~/.local/state/deadlight/`, one `{project}.json` per project —
  deliberately outside the repo so pane drags never show up in
  `git status`), `DEADLIGHT_DEBOUNCE_MS` (filesystem-watch debounce; tests
  set it near zero). Running on a machine that isn't the deploy host needs
  at least `DEADLIGHT_ROOTS`.
- Deployed via systemd user unit `deadlight.service`, exposed via
  `tailscale serve --bg --https=8444 8444`.

## What's new in v3

- **zellij is gone.** deadlight owns the PTYs itself, holding a 1 MB
  scrollback ring per session and fanning output out to every attached
  client. It spawns `dtach` purely so sessions survive a deadlight
  restart. `dtach` is therefore a **runtime prerequisite** on any machine
  running deadlight (`brew install dtach` / `apt install dtach`); already
  installed on both the Mac and the deploy host.
- **Files can be edited and saved**, plus created, renamed and deleted.
  Saving is conflict-guarded: if the file changed on disk since the buffer
  was opened, the write is refused and a diff of yours-vs-disk is shown.
- **The filesystem is watched.** A file changed outside the browser
  updates a clean buffer live (this is the point: Claude edits files in a
  terminal pane while you watch), while a buffer with unsaved changes is
  only flagged stale — your unsaved work is never overwritten by a
  background writer.
- **All writes travel over the websocket, so HTTP stays GET-only.** URLs:
  `/ws/{project}/_workspace` (JSON intents up, events down) and
  `/ws/{project}/term/{name}` (raw bytes, one per terminal tab), replacing
  the old `/ws/{project}`.
- **Migration:** existing zellij sessions are not adopted by v3 — they
  keep running under zellij and can be attached from a shell until
  retired.

## Deploying to <deploy-host>

**The unit runs `~/.local/bin/deadlight`, not `target/release/deadlight`** —
and `~/.cargo/config.toml` redirects `target-dir` to `~/.cache/cargo-target`,
so a plain `cargo build --release` updates neither path the service uses.
Building without the install step leaves the old binary running and looks
like the deploy silently did nothing.

```bash
tailscale ssh claude@<deploy-host>      # Tailscale SSH is enabled
cd /home/claude/projects/deadlight && git pull --ff-only
cargo build --release
install -m 755 ~/.cache/cargo-target/release/deadlight ~/.local/bin/deadlight
systemctl --user restart deadlight
```

`tailscale serve` and `tailscale set` work without sudo (the account is the
tailscale operator); the account's sudo password is *not* its ssh password.
- URLs: `/` (index), `/{project}` (workspace). Everything else is plumbing
  (`/static`, `/ws/{project}/_workspace`, `/ws/{project}/term/{name}`,
  `/frag/{project}/...`).
- Settings: `~/.config/deadlight/config.toml` then
  `{project}/.deadlight/config.toml` (theme, default_tab, hide) — re-read
  every request; edit the file, hit refresh.
- **`allowed_origins` is global-config only** and must list the tailnet
  origin, or the browser gets 403 over tailscale:
  `allowed_origins = ["https://<deploy-host>.<tailnet>.ts.net:8444"]`.
  Loopback always passes unlisted. It is deliberately not readable from a
  project's `.deadlight/config.toml`, so a cloned repo cannot allowlist
  itself. Rejections are logged with the offending values —
  `journalctl --user -u deadlight` when access mysteriously 403s.
- The v1 Python implementation was replaced wholesale on 2026-08-16
  (see git history and the spec's History note).
- code-server stays running as fallback — don't restart it casually
  (Peter's live Claude sessions run under its extension host).
