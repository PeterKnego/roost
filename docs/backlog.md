# Backlog

A collection point for deferred, future-work, and nice-to-have items scattered
across resh's specs and plans. This is not a commitment or a roadmap —
just everywhere "later" was said, gathered so it's findable. Pull items out
when they're actually picked up.

**Measured vs assumed.** Most entries arrived here as a spec's "nice-to-have"
with no stated reason, which means their *demand* was never established — only
that someone once imagined the feature. An entry that overstates its own demand
is worse than no entry, because it spends attention and nothing in the text
tells you which claims were checked. Entries carrying an **Evidence** line were
measured against this host on the date given; entries without one have not
been, and should be read as "someone wanted this once", not as a signal.

Some things are not measurable from here at all, and say so. That is a third
answer, not a quiet vote for "unused".

## UI / UX

- Split terminal/viewer layout — listed as a v2 nice-to-have ("post-v1, only
  if asked"). **Already shipped**: this became the v3 four-pane workspace
  (`2026-08-16-deadlight-v2-design.md`).
- Git log view — nice-to-have in both the v2 and v3 specs; no stated reason,
  just unprioritized (`2026-08-16-deadlight-v2-design.md`,
  `2026-08-16-deadlight-v3-workspace-design.md`).
  **Evidence (2026-08-20): none either way.** `gitio.rs` has no log function, so
  nothing is half-built; Changes and Diff tabs already cover the "what did I
  just do" case a log view is usually reached for. Demand unmeasured.
- Mobile layout — nice-to-have in both the v2 and v3 specs, no stated reason
  (`2026-08-16-deadlight-v2-design.md`, `2026-08-16-deadlight-v3-workspace-design.md`).
  **Evidence (2026-08-20): unmeasurable, not unused.** resh never reads
  `User-Agent` anywhere in `src/`, and nothing logs it, so this host cannot say
  whether anyone has ever opened the workspace on a phone. Establishing demand
  would mean adding that logging first — which is itself a decision.
- Per-theme favicon — nice-to-have in both the v2 and v3 specs, no stated
  reason (`2026-08-16-deadlight-v2-design.md`,
  `2026-08-16-deadlight-v3-workspace-design.md`).
  **Evidence (2026-08-20): the feature it decorates is itself unused.** Five
  themes ship (`darcula`, `dark`, `gruvbox`, `light`, `solarized-dark`), but
  there are **0 user themes** in `~/.config/resh/static/themes/`, **0 project
  themes** in any `.resh/theme/`, and no `theme =` set in the global config —
  so every window is on the default. A favicon that varies by theme has nothing
  to vary with yet.
- Images in markdown preview — **shipped**, see
  `2026-08-19-preview-links-and-images-design.md`. Grew in scope on the way:
  the same missing piece (no route served raw project bytes) was also why a
  `.png` in the tree answered "binary file", and rewriting image `src` was
  half a job without rewriting link `href` too — a link to another file used
  to navigate the browser clean out of the workspace. Heading anchors are the
  recorded non-goal; see below.
- Drag-and-drop tab reordering — speculative idea in the v3 spec, noted as
  partly moot since v3 already ships a "move to pane" command as the
  mechanism for relocating tabs (`2026-08-16-deadlight-v3-workspace-design.md`).
- Drag-n-drop upload, copy-paste of files into the tree, and pasting images into
  the claude terminal — **shipped**, see
  `docs/superpowers/specs/2026-08-19-file-upload-design.md`. Directory upload,
  archive extraction, download/drag-out and a host clipboard bridge are the
  recorded non-goals; scratch retention (nothing prunes `state_dir()/pasted/`)
  and the 16-part limit are the questions left open.
  **Evidence (2026-08-20): both open questions are unexercised.**
  `state_dir()/pasted/` **does not exist** in either the deployed or the dev
  state directory — no image has ever been pasted into a terminal on this host,
  so the unpruned-scratch worry has accumulated exactly nothing. The 16-part
  request limit has likewise never been approached. Real questions, zero
  pressure.
- **Redesign the transient message UI.** Upload errors and the upload progress
  indicator both borrow `showBanner`'s `.conflict` styling, which was designed
  for the save-conflict box and is wrong here: ugly, and positioned where a
  save conflict wants to be rather than where a transient notice does. Reported
  from real use, not from a test. The whole surface wants one design pass —
  errors, per-file upload results, and progress are three different things
  currently rendered as one, and progress in particular should probably sit in
  the pane it belongs to rather than floating over the layout
  (`docs/superpowers/specs/2026-08-19-file-upload-design.md`).
  **Evidence: provenance, which is the strongest kind in this file.** This is
  the only UI/UX entry that came from someone hitting it in real use rather
  than from a spec's nice-to-have list. Nothing further to measure.
- Heading anchors in markdown preview. `#section` links are inert because
  pulldown-cmark emits no heading ids, so a link to a heading in the same
  document lands on nothing. The stated non-goal of the preview-links work
  (`2026-08-19-preview-links-and-images-design.md`).

  **Real but unused here — do not prioritise it on the strength of the
  argument it was first filed with.** That argument was "a README's table of
  contents is the most common thing a markdown link points at, so this is the
  obvious next request", which reasoned from how markdown is used in general
  rather than from this corpus. Surveyed 2026-08-20: **0 of 137 markdown files
  under `/home/claude/projects` contain a single `](#anchor)` link**, and the
  only match anywhere in this repo is a test fixture inside the
  preview-links plan. Every link that does appear in these docs is
  file-to-file, and those work now.

  If it is ever picked up, three things need deciding, none obvious: slug
  generation must match GitHub's algorithm or links copied out of a
  GitHub-rendered doc will not resolve; generated ids must not collide with
  the workspace page's own (a heading called "Content" would collide with the
  pane's `#content`), since the preview is a fragment injected into a live
  page rather than a document of its own; and letting a fragment link change
  `location.hash` interacts with a single-page app whose URL already encodes
  the project.

- Notification centre on the picker page (`/`) as well as the workspace
  page — the notice store is already global, only the markup is missing
  (`2026-08-17-deadlight-notifications-design.md`).
  **Evidence (2026-08-20): the notification feature has never stored a notice.**
  `notifications.json` **does not exist** in `~/.local/state/resh/` or in the
  dev state dir. This applies to the next three entries as well — the picker
  centre, per-project mute and quiet hours, Web Push, and a relay sink are four
  entries resting on a feature with no recorded use on this host. Web Push in
  particular is the most expensive item in this file (VAPID signing, payload
  encryption, subscription storage); it should not be reached for until
  something is actually publishing notices worth chasing to a phone.
- Per-project notification mute and a quiet-hours window — deferred out of
  v1 alongside sound (`2026-08-17-deadlight-notifications-design.md`).
  **Evidence: see the notice-store finding above** — nothing to mute yet.
- Web Push for notifications. The service worker gains a `push` handler; the
  server gains VAPID signing, payload encryption, and subscription storage —
  the step that reaches a phone with no tab open, and the reason the client
  already uses a service worker
  (`2026-08-17-deadlight-notifications-design.md`).
  **Evidence: see the notice-store finding above.** The most expensive item in
  this file, resting on a feature with no recorded use.
- A relay sink (ntfy/Pushover) for notifications: a configured webhook POSTed
  on publish, a cheaper route to a phone than Web Push at the cost of a third
  party and a token to store (`2026-08-17-deadlight-notifications-design.md`).
  **Evidence: see the notice-store finding above.**

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

**Evidence (2026-08-20): this is the one section with measured demand behind
it.** Across the five saved workspaces on this host there are 29 tabs, of which
**9 are File tabs** and 2 carry persisted buffers — so the viewer and editor are
genuinely in use, unlike most of the UI/UX list above. That does not rank the
five items below against each other, but it does mean the section is not
speculative.

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
  **Evidence (2026-08-20): not measurable from this host.** Whether retach has
  matured is a question about retach's upstream, not about anything here. What
  *is* visible locally: dtach is carrying 16 live sessions across restarts
  without complaint, so the incumbent is not under pressure.
- Per-session CPU and memory sampling, shown next to session age — deferred
  with a stated reason in the projects spec's "Future work": needs a sampling
  cadence and per-platform code (`/proc` on Linux, `ps` on macOS), and nothing
  else in that design depends on it (`2026-08-17-deadlight-projects-design.md`).
  **Evidence (2026-08-20): demand unmeasured, but note the adjacent finding.**
  The zellij leftovers below went unnoticed for weeks while holding ~342 MB —
  exactly the kind of thing per-process visibility surfaces. That is an argument
  for the feature, though not one the original entry made.

## Git

- Enforcing project == git repo more strongly than the current soft gate
  ("start without git" escape hatch) — speculative, conditional on the soft
  gate proving too soft in practice; projects spec's "Future work"
  (`2026-08-17-deadlight-projects-design.md`).
  **Evidence (2026-08-20): argues against doing this.** **6 of 23** project
  directories under the configured roots are not git repositories —
  `karpie-validation`, `aeneas-btree`, `bench-parity`, `leanstral-demo`,
  `rings-bench`, `uc-bench-data`. Enforcing project == git repo would lock out
  a quarter of what is actually on this host. The soft gate is not proving too
  soft; it is carrying real load.

## Platform / deployment

- Retiring the legacy zellij-web (8082) and code-server (8443) endpoints —
  deferred deliberately in the v2 spec until deadlight "has earned trust"
  (`2026-08-16-deadlight-v2-design.md`); `docs/deploy.md` still lists
  code-server as a kept fallback, so this remains open.
  **Evidence (2026-08-20): half done, and the other half is live — this is the
  one entry the measurement made MORE urgent, not less.** code-server is
  genuinely gone (binary absent, no unit). Zellij is not: **five processes,
  ~342 MB RSS, ~22 h uptime** — a `zellij web --start --daemonize` listening on
  `127.0.0.1:8082` plus four `--server` instances — and `tailscale serve` still
  maps `:8443` to it. Zellij was replaced by dtach back in v3, so all of that is
  leftovers holding a third of a gigabyte and a tailnet route into a shell
  spawner nothing uses.

  Note the deploy notes were **wrong** about this until corrected: they recorded
  zellij as having been killed on 2026-08-18 and the `:8443` route as dropped,
  and both claims were false. Its sessions have previously reported themselves
  EXITED while the processes lived on, so `kill-all-sessions` will not clear
  them — they go by pid, with the route dropped separately. The host-specific
  commands are in `~/.config/resh/deploy-host.md`.

## Code structure

- Move `IMAGE_EXT` / `is_image` / `NO_TEXT_EDIT_EXT` / `refuses_text_edit` out
  of `routes.rs` and into `assets.rs`, where `ext_of` and `THEME_EXT` already
  live. `workspace.rs` is pure state logic and now reaches into the HTTP layer
  for three predicates, which is backwards
  (`2026-08-19-preview-links-and-images-design.md`).
- `IMAGE_EXT` and `NO_TEXT_EDIT_EXT` are each hand-mirrored in
  `static/app.js` with nothing checking the copies agree. Divergence costs a
  wrongly shown or hidden ✎ toggle, never data — `workspace.rs` is the real
  guard — but it is the same unchecked-sync hazard `FRAGMENT_KINDS` carries,
  and a build-time check could close both.

## Testing

- `tests/browser/mdlinks.mjs`'s `javascript:` step evaluates `!!a` to prove the
  danger anchor was found, then discards the result — so if that anchor ever
  vanished from the preview, the two assertions after it would pass vacuously.
  The `no javascript: href reached the page` assertion beside them is
  independently discriminating, so the security property stays covered, but
  this is the same defect class that produced three vacuous tests during that
  branch's own execution and it is a one-line fix
  (`2026-08-19-preview-links-and-images-design.md`).
- `tests/integration.rs`'s `notices_are_replayed_on_connect_and_read_state_mirrors`
  fails intermittently, timing out waiting for `"read":true`. Diagnosed
  mechanism: `notify::load()` ends by *destructively* replacing the
  process-global in-memory notice store with whatever it just read off disk
  (`s.notices = list.into()`), and `lib.rs` calls `notify::load()` on every
  `serve()`. `tests/integration.rs`'s `start()` spawns `serve()` on a thread
  and returns immediately, and this one binary stands up 31 servers — 7 direct
  `start(...)` calls plus 24 through `fixture()`, which calls it too (counted
  at `76b22c8`). `WS_TEST_LOCK` serialises the websocket tests against each
  other, but not against the 28 of 54 tests that do not take it, which freely
  `start()` servers of their own and set/remove `RESH_STATE_DIR` as they go.
  So one test's `load()` can read a *different* test's state dir — including,
  if the timing lines up wrong, the developer's real `~/.local/state/resh/` — and
  evict the notice another test just published out from under it. When that
  happens, `MarkNoticeRead` finds no such id, and `hub.rs` rebroadcasts
  unconditionally with a notice list in which nothing ended up marked read,
  so the client waiting on `"read":true` times out. Suggested fix: make
  `load()` non-destructive (merge incoming notices by id rather than
  replacing the store wholesale), or gate it behind a `OnceLock` so it only
  ever runs once per process; either way, the integration binary also wants
  one shared env lock covering every `start()` call, not just the websocket
  ones. This predates embedded assets, which has since merged to master and
  brought two more `start()` call sites into the same binary — so the race is
  marginally more likely to trigger now, not less.

## Already shipped (found listed as future/nice-to-have in an earlier doc)

- Split terminal/viewer layout (v2 nice-to-have) → shipped as the v3 four-pane
  workspace.
- File editing (implicit in v2's "viewer is stateless and read-only" framing)
  → shipped in v3 as the Preview/Edit toggle with conflict-guarded save.
- Images in markdown preview (v2 and v3 nice-to-have) → shipped 2026-08-20,
  along with link rewriting and image tabs, which were never listed here
  because nobody had noticed a preview could not follow its own references.
