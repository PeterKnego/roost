# deadlight — handoff

Per-project remote workspace: persistent zellij terminal + stateless
read-only viewer (tree, git changes, markdown, code) in one Rust binary.

- **Design spec:** `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md`
- **Implementation plan:** `docs/superpowers/plans/2026-08-16-deadlight-v2.md`
- Run: `cargo run` (127.0.0.1:8444), tests: `cargo test`.
- Deployed via systemd user unit `deadlight.service`, exposed via
  `tailscale serve --bg --https=8444 8444`.
- URLs: `/` (index), `/{project}` (workspace). Everything else is plumbing
  (`/static`, `/ws/{project}`, `/frag/{project}/...`).
- Settings: `~/.config/deadlight/config.toml` then
  `{project}/.deadlight/config.toml` (theme, default_tab, hide) — re-read
  every request; edit the file, hit refresh.
- The v1 Python implementation was replaced wholesale on 2026-08-16
  (see git history and the spec's History note).
- code-server stays running as fallback — don't restart it casually
  (Peter's live Claude sessions run under its extension host).
