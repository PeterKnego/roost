# deadlight — handoff

Per-project remote workspace: persistent zellij terminal + stateless
read-only viewer (tree, git changes, markdown, code) in one Rust binary.

- **Design spec:** `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md`
- **Implementation plan:** `docs/superpowers/plans/2026-08-16-deadlight-v2.md`
- Run: `cargo run` (127.0.0.1:8444), tests: `cargo test`.
- Env overrides: `DEADLIGHT_ROOTS` (colon-separated project roots, else the
  deploy host's `/home/claude/{ultima,projects}`), `DEADLIGHT_CMD` (terminal
  command), `DEADLIGHT_ORIGINS` (comma-separated origin allowlist). Running
  on a machine that isn't the deploy host needs at least `DEADLIGHT_ROOTS`.
- Deployed via systemd user unit `deadlight.service`, exposed via
  `tailscale serve --bg --https=8444 8444`.

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
  (`/static`, `/ws/{project}`, `/frag/{project}/...`).
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
