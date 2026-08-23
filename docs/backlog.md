<!-- Not a roadmap: everywhere "later" was said, gathered so it stays findable. -->
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

## First things to do:
- select file in tree or select text in preview, pres Cmd-<Key> and it gets pasted with @reference to Claude active terminal
- moving file/term tabs
- handle worktree selection/switching: top-bar left we already have project+branch, we should add worktree+selector. Rethink how to select/switch worktrees. In-place or new browser tab?
- fix popup UX+design (notifications, dialogs)
- settings system, 
- theme selector
- all project search, ala Idea shift-shift. Opens new search dialog with results.
- multi-wiew per project: now all windows synchronize to project. Could we have same project open with different tab layout / files open. So basically project could have different "views".

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

1. ~~Syntax highlighting while editing.~~ **Shipped 2026-08-21** — see the
   decision below.
2. Tab inserts an indent instead of moving focus; auto-indent on Enter.
3. Line numbers, and go-to-line.
4. In-buffer find. Browser find over a `textarea` barely works.
5. Bracket and quote auto-close.

Items 2 and 5 are now cheaper than they look: `code-input` ships optional
single-file plugins for indentation and bracket closing, neither vendored.

### Candidate libraries

**Decided 2026-08-21: none of the four below.** The winner was
[`code-input`](https://github.com/WebCoder49/code-input) (MIT, no
dependencies, 27 KB), which is not a text editor at all — it wraps a *real*
`<textarea>` and paints a highlighted `<pre>` underneath it, driven by the
highlight.js already vendored here. That is what settled it: see "The risk
that decides it" below, which every candidate in this table has to answer and
which does not arise when the textarea is still the textarea. `editors`, the
200 ms debounce, autosave, ⌘S and the conflict path all kept talking to the
same element they always had. Provenance and the two non-obvious integration
details are in [vendor.md](vendor.md).

The analysis below is kept as the record of what was weighed, not as an open
question.

Constraints that decided it: plain JS, no framework, no build step, and
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

**How it actually resolved:** by not choosing a text model at all. The
normalisation risk above is a property of replacing the textarea, and
`code-input` does not. The path was still tested end to end
(`tests/browser/hledit.mjs`), and the one thing that nearly went wrong was
unrelated to bytes: the element builds its *own* textarea on connect, so
wiring the app's handlers to the one handed *to* it failed silently and
totally — text typed and highlighted while nothing was ever sent or saved.

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

- Persisting a session's mode table across a resh restart — the one case
  `2026-08-20-sticky-modes-design.md` deliberately does not fix. Modes are
  tracked in memory (`screen::Screens`), so a restart leaves an already-running
  full-screen app holding a contract the new process never saw, and the app
  never repeats itself: a `SIGWINCH` repaint re-declares nothing (measured —
  `?1049h` after a winch: 0). The spec rejects persistence for the *screen*
  bit, and that rejection stands: a stale "on the alternate screen" marker
  leaves a blank buffer with the shell's output going somewhere invisible.
  Modes are the cheap half — a wrongly-asserted mouse mode shows up as visible
  junk on the command line and clears on the next Ctrl-C — which is the whole
  argument for treating them differently.
  **Evidence (2026-08-20): this fires on every deploy, and today there were
  nine.** `journalctl --user -u resh` records 9 restarts today, and **4 of the
  7** live sessions on this host are running Claude Code right now — so each
  restart desynchronised four terminals. Measured after one: a reattached
  browser reads `bracketedPaste: false, mouse: "none", focus: false`, and a
  paste goes on the wire as `"one\rtwo"` rather than
  `"\e[200~one\rtwo\e[201~"`, so a pasted three-line prompt submits its first
  line on its own. **A page reload does not fix it** — a reload is the same
  attach, replaying a table that is empty. Restarting the app does, and Claude
  re-asserts its *mouse* modes on interaction so that half tends to heal by
  itself; `?2004h` it emits once at startup only, so paste does not.

### Peer sessions (`resh peers`, 2026-08-23)

- **Nothing guarantees the group is told; the arriving session is asked to do
  it.** The hook informs the session that is starting and no one else. Since
  2026-08-23 the warning instructs that session to announce itself to each peer
  by `SendMessage`, which reaches the earliest session — the one the hook can
  never reach, and the one most likely to be mid-task when someone joins. That
  is a convention carried out by a model, not a guarantee: a session that
  ignores the instruction, or whose peer refuses inbound messages, leaves the
  group as uninformed as before. A guarantee means resh pushing into running
  sessions or re-checking on a timer — a lifecycle, where today there is one
  file read.
  **Evidence (2026-08-23): the one-way gap was measured before the change.**
  `resh-2e` (started 09:52) was told about `resh-f8` at its own start and
  quoted it back verbatim over `SendMessage`; `resh-f8` (started 05:44) was
  never told about `resh-2e` and found it hours later by running `resh peers`
  by hand. Whether the announcement instruction is actually followed in
  practice has **not** been measured — it was deployed the same day.

- **Names can still collide; it is now detected rather than prevented.** Since
  2026-08-23 a shared name marks the offending rows, qualifies the announce
  instruction with where to get `ListAgents`' disambiguating ref, and appends a
  stamped line to `{RESH_STATE_DIR}/error.log`. What is *not* fixed is the
  cause: resh cannot mint or read the ref, so it cannot print an unambiguous
  address, and `SendMessage` accepts no pid. A reader who ignores the warning
  still messages the wrong session.
  **Evidence (2026-08-23): the collision was observed once, that morning, and
  had ended by the afternoon.** Whether the detection ever fires again has not
  been measured — it was deployed the same day, on a host where all nine live
  sessions then had distinct names.

- **resh's own UI says nothing about peers.** The count is known per project at
  any moment, so the picker or project strip could badge a project more than
  one Claude is working in — reaching the person rather than only the arriving
  session, and sidestepping the asymmetry above entirely.
  **Demand unmeasured.** Raised while designing the hook and never asked for.

- **The `git` call has no timeout of its own.** `git_common_dir` uses
  `Command::output()`, which blocks until git exits. A git that hangs — a
  repository on a network filesystem, or one waiting on a lock another process
  holds — would stall a session start rather than let it proceed.
  **Evidence (2026-08-23): measured, and small.** Three resolutions took 10ms
  on this host, ~3ms each, and the hook entry carries `timeout: 10`, so even a
  fully wedged git costs ten seconds once and the session then starts anyway.
  The resolution is also lazy, so a session alone in a project spends none at
  all. That backstop is Claude Code's, not resh's: a caller wiring `resh peers`
  without a timeout inherits the stall.

- **A sibling whose liveness cannot be judged is dropped silently.** The
  `uncheckable` count deliberately covers same-directory records only, so an
  unreadable `/proc` entry for a session in another worktree produces no line
  at all rather than an "N could not be checked" note. That asymmetry is a
  decision, not an oversight: a missed peer means overwritten work while a
  missed sibling means a missed advisory, and reporting uncertainty about the
  quiet case costs more noise than it buys. Recorded so the next reader finds a
  decision rather than rediscovers a gap.
  **Not measured.** No unreadable `/proc` entry has been observed on this host;
  the path exists because it must, not because it has fired.

- **`roots` still lives in two places; drift is now loud instead of silent.**
  `Environment=RESH_ROOTS` in the unit file and `roots` in
  `~/.config/resh/config.toml` must still be kept in step by hand — the env var
  winning is deliberate, so the fix is not deleting one but teaching the unit
  to read the config. Since 2026-08-23 the server compares them at startup and
  complains on stderr and in `error.log` when both speak and disagree.
  **Not measured.** Both entries were written on 2026-08-23 and have not
  drifted; the detector has never fired.


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

### Sharp edges left by the buffers-without-text branch (2026-08-22)

Findings its own reviews raised and deliberately deferred, kept because the
ledger they lived in was scratch. Each was re-checked against the tree on
2026-08-22; the ones already closed are not listed.

- **`wsstate::load` reads an unreadable state file as "never saved".**
  `let Ok(text) = std::fs::read_to_string(path_for(project)) else { return (w,
  None) }` — a permissions blip, or any transient error, is indistinguishable
  from a first run, silently. The workspace comes up as the default layout,
  and the *next* save writes that default over the real state file: every tab,
  and any unsaved buffer text it held, gone. This is the eleven-times defect in
  CLAUDE.md's own table, in the one place whose whole job is not losing state.
  `symlink_metadata` distinguishes the three cases; `Err(NotFound)` is the only
  one that means "never saved".
- **A restore with more than `MAX_BUFFERS` dirty buffers is unbounded.** The
  load cap deliberately truncates only the clean tail, so unsaved work is never
  dropped to make room — right, but it means a hand-edited or corrupt file can
  make `reconcile_buffers_with_disk` re-read arbitrarily many files under the
  registry lock. A sanity ceiling with a warning would close it without
  reintroducing the data loss.
- **`stale` can be true on a `Clean` buffer** (edit → external change → undo
  back to base), which the design doc says cannot happen. Self-heals on
  reactivation; cosmetic, but the doc is wrong until one of them moves.
- **A rename during `wsconn`'s unlocked replay window** rekeys the buffer with
  no `BufferText`, so the old rel replays nothing and the new one is absent
  from the snapshot. Pre-dates the branch.
- **Preview-only buffers get a full `BufferText`**, not the hash-only update
  the spec describes, so every client holds the text of every previewed file in
  `texts`.
- **Two no-op assignments** (`hub.rs`, `b.content = Content::Clean` inside an
  `if b.dirty()` else-branch, where it is already `Clean`).

## Testing

- `tests/browser/mdlinks.mjs`'s `javascript:` step evaluates `!!a` to prove the
  danger anchor was found, then discards the result — so if that anchor ever
  vanished from the preview, the two assertions after it would pass vacuously.
  The `no javascript: href reached the page` assertion beside them is
  independently discriminating, so the security property stays covered, but
  this is the same defect class that produced three vacuous tests during that
  branch's own execution and it is a one-line fix
  (`2026-08-19-preview-links-and-images-design.md`).
- Three tests carry assertions weaker than their comments claim, all from the
  buffers-without-text branch and all verified still present on 2026-08-22:
  `routes.rs`'s `clicking_an_image_shows_a_picture_not_a_binary_error` matches
  the image URL by prefix rather than anchored, so a stray parameter appended
  after `v=<n>` would pass (the mtime is wall-clock, which is why it was
  loosened); `preview-follows.mjs` uses one pane and one file, so the
  "does not re-fetch other panes" property is only verified by reading the
  `rel` match; and `wsstate`'s pre-`base_hash` fixture hand-builds
  `state_dir().join(...)` instead of using the in-scope `path_for`, so a change
  to the naming scheme would make it miss the file.
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
- Syntax highlighting while editing (Editing → low-hanging fruit, item 1) →
  shipped 2026-08-21 for code files; markdown and plaintext stay plain, and
  files past 100 KB stay a plain textarea rather than a laggy one.
- A previewed file following the disk → shipped 2026-08-21. It had never been
  listed as future work because it read as a bug: the watcher only ever
  reported files that had an edit buffer, so a preview had no invalidation
  path at any of the three layers.
- Preview/Edit as a per-file choice → **retired** 2026-08-22 rather than
  shipped. A text file now opens in its editor; Preview survives only where a
  file has a rendered form to look at (markdown, images, and svg, which has
  both and keeps the ✎).
- A one-click Claude terminal → shipped 2026-08-23 as the ✻ next to each tab
  strip's +: a new terminal with `claude` typed into it by the server the
  moment its shell spawns (`proto::Launch`, `launch.rs`). It was never listed
  here — it arrived as a request, not a deferral. No flags, because
  `CLAUDE_CODE_SSE_PORT` in the spawned environment already links the claude
  to this resh's IDE socket; the button hides itself only when a startup probe
  of the login shell positively says `claude` is not installed (an Unknown
  keeps it). Found and fixed on the way: + had been landing on the "press
  Enter" placeholder, because `TerminalStarted` went out before the snapshot
  that carried the tab. Deferred from it: a global-only config key for flags
  (`claude --continue` and the like) — nobody has asked, and a per-project
  value would let a cloned checkout decide what a click executes.
