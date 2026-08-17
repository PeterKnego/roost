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
- Notification centre on the picker page (`/`) as well as the workspace
  page — the notice store is already global, only the markup is missing
  (`2026-08-17-deadlight-notifications-design.md`).
- Per-project notification mute and a quiet-hours window — deferred out of
  v1 alongside sound (`2026-08-17-deadlight-notifications-design.md`).
- Web Push for notifications. The service worker gains a `push` handler; the
  server gains VAPID signing, payload encryption, and subscription storage —
  the step that reaches a phone with no tab open, and the reason the client
  already uses a service worker
  (`2026-08-17-deadlight-notifications-design.md`).
- A relay sink (ntfy/Pushover) for notifications: a configured webhook POSTed
  on publish, a cheaper route to a phone than Web Push at the cost of a third
  party and a token to store (`2026-08-17-deadlight-notifications-design.md`).

## Editing

The v3 spec ruled rich editing out of the middle pane entirely: "Rich editing
belongs in the editor running in the terminal pane"
(`2026-08-16-deadlight-v3-workspace-design.md`). **Reviewed 2026-08-17 and
softened** — the boundary moves rather than disappears.

### The decision

The terminal is where an AI agent does the editing. The browser editor is
therefore not an authoring surface competing with it; it is where a human
*reads* what the agent wrote and makes a small correction — a typo, a config
value, a line to delete. That distinction, not a feature list, is the limit on
"how rich":

- **In scope:** anything that helps you read a file accurately and make one
  small change correctly.
- **Out of scope:** anything that helps you author at volume or navigate a
  codebase. That is the agent's job, and the terminal editor already does it
  better. LSP, diagnostics, autocomplete, multi-cursor, refactoring,
  project-wide search/replace, git gutters, and vim/emacs keymaps all stay
  out — now for a stated reason rather than by blanket exclusion.

### Low-hanging fruit, in order

Edit mode is currently a bare `<textarea>` (`static/app.js`, `mountEditor`).
Preview mode highlights and Edit mode does not, so switching to Edit makes a
file *harder* to read — the first item exists mainly to fix that inversion.

1. Syntax highlighting while editing. highlight.js is already vendored, so
   this costs almost no new bytes.
2. Tab inserts an indent instead of moving focus; auto-indent on Enter.
3. Line numbers, and go-to-line.
4. In-buffer find. Browser find over a `textarea` barely works.
5. Bracket and quote auto-close.

### Candidate libraries

Constraints that decide this: plain JS, no framework, no build step, and
everything vendored into `static/vendor/` so it can be audited and served
offline.

| Candidate | Fit | Cost |
|---|---|---|
| [CodeJar](https://github.com/antonmedv/codejar) | Best fit on paper — a few KB, and its highlighting hook takes highlight.js, which is already vendored | Uses `contenteditable`, which is the risk below |
| [Ace](https://ace.c9.io/) | Single-file core, genuine drop-in, mature, owns its own text model | Heavier; a second highlighting engine alongside highlight.js |
| [CodeMirror 6](https://codemirror.net/) | Best mobile support (relevant to the mobile-layout item above), modular | Official path needs a bundler; the [prebuilt community bundles](https://github.com/paul-norman/codemirror6-prebuilt) mean vendoring someone else's rebuild, which weakens the audit story |
| Monaco | Most capable | Multi-MB with worker files; far past what this pane is for. Not investigated in this pass |

### The risk that decides it

Not size — **byte fidelity**. Save is conflict-guarded against a hash of what
was read from disk, and `texts` must match the buffer exactly for
`EditBuffer`, the stale flag, and the live-follow of external edits to behave.
A `contenteditable` editor normalises whitespace and newlines; if the bytes
saved differ subtly from the bytes displayed, the result is a corrupted file
or a save that conflicts with itself — and a green test suite would not
notice, because this is exactly the class of defect that only shows up in a
real browser.

So whichever candidate wins must be tested against the existing save path
before it is believed: seeding from `texts`, the 200 ms debounced
`EditBuffer`, Ctrl/Cmd+S, clean-buffer live follow, and stale flagging. Ace
and CodeMirror own their own text model and sidestep the normalisation
problem; CodeJar is cheapest but carries it directly. That trade — not the
feature list — is what a spec for this needs to resolve.

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
