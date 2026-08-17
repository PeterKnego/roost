# Backlog

A collection point for deferred, future-work, and nice-to-have items scattered
across deadlight's specs and plans. This is not a commitment or a roadmap —
just everywhere "later" was said, gathered so it's findable. Pull items out
when they're actually picked up.

## UI / UX

- Split terminal/viewer layout — listed as a v2 nice-to-have ("post-v1, only
  if asked"). **Already shipped**: this became the v3 four-pane workspace
  (`2026-08-16-deadlight-v2-design.md`).
- Git log view — nice-to-have in both the v2 and v3 specs; no stated reason,
  just unprioritized (`2026-08-16-deadlight-v2-design.md`,
  `2026-08-16-deadlight-v3-workspace-design.md`).
- Mobile layout — nice-to-have in both the v2 and v3 specs, no stated reason
  (`2026-08-16-deadlight-v2-design.md`, `2026-08-16-deadlight-v3-workspace-design.md`).
- Per-theme favicon — nice-to-have in both the v2 and v3 specs, no stated
  reason (`2026-08-16-deadlight-v2-design.md`,
  `2026-08-16-deadlight-v3-workspace-design.md`).
- Images in markdown preview — nice-to-have in both the v2 and v3 specs, no
  stated reason (`2026-08-16-deadlight-v2-design.md`,
  `2026-08-16-deadlight-v3-workspace-design.md`).
- Drag-and-drop tab reordering — speculative idea in the v3 spec, noted as
  partly moot since v3 already ships a "move to pane" command as the
  mechanism for relocating tabs (`2026-08-16-deadlight-v3-workspace-design.md`).
- Drag-n-drop upload of local files into the remote fs pane — speculative idea,
  v3 spec (`2026-08-16-deadlight-v3-workspace-design.md`).
- Copy-paste file content — speculative idea, v3 spec
  (`2026-08-16-deadlight-v3-workspace-design.md`).
- Paste images into the claude terminal (ctrl+v) — speculative idea, v3 spec
  (`2026-08-16-deadlight-v3-workspace-design.md`).

## Editing

Rich editing (LSP, autocomplete, find/replace) was explicitly ruled out of the
middle-pane editor by design, not deferred as a future item: "Rich editing
belongs in the editor running in the terminal pane"
(`2026-08-16-deadlight-v3-workspace-design.md`). Not a backlog item — a
standing scope boundary — but noted here since it reads like one.

## Terminals and sessions

- `retach` as the session backend instead of `dtach`, if its scrollback replay
  proves worth the immaturity — deferred with a stated reason (dtach is stable,
  retach is not) in the v3 spec's nice-to-haves
  (`2026-08-16-deadlight-v3-workspace-design.md`).
- Per-session CPU and memory sampling, shown next to session age — deferred
  with a stated reason in the projects spec's "Future work": needs a sampling
  cadence and per-platform code (`/proc` on Linux, `ps` on macOS), and nothing
  else in that design depends on it (`2026-08-17-deadlight-projects-design.md`).

## Git

- Enforcing project == git repo more strongly than the current soft gate
  ("start without git" escape hatch) — speculative, conditional on the soft
  gate proving too soft in practice; projects spec's "Future work"
  (`2026-08-17-deadlight-projects-design.md`).

## Platform / deployment

- Retiring the legacy zellij-web (8082) and code-server (8443) endpoints —
  deferred deliberately in the v2 spec until deadlight "has earned trust"
  (`2026-08-16-deadlight-v2-design.md`); `docs/deploy.md` still lists
  code-server as a kept fallback, so this remains open.

## Already shipped (found listed as future/nice-to-have in an earlier doc)

- Split terminal/viewer layout (v2 nice-to-have) → shipped as the v3 four-pane
  workspace.
- File editing (implicit in v2's "viewer is stateless and read-only" framing)
  → shipped in v3 as the Preview/Edit toggle with conflict-guarded save.
