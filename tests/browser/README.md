# Browser tests

`cargo test` cannot reach `static/app.js`. Everything the browser does — the
websockets, the xterm instance, the reconnect logic, the tab lifecycle — is
invisible to all 300 Rust tests, and this project has already shipped defects
that lived exactly there. CLAUDE.md's *dev/prod substitution trap* lists four,
one of which ("no browser: saving was completely broken") was found by hand
against a real browser after the suite was green.

These tests drive a real Chromium over the DevTools Protocol against a real
resh with real `dtach`.

## Running

```bash
deno run -A tests/browser/reconnect.mjs   # terminal survives a dead connection
deno run -A tests/browser/upload.mjs      # file upload and image paste
deno run -A tests/browser/paneicons.mjs   # the per-pane header controls
deno run -A tests/browser/mdlinks.mjs     # markdown preview links/images, and the image-tab edit refusal
deno run -A tests/browser/dotfiles.mjs    # the tree pane's dotfile toggle
deno run -A tests/browser/altscreen.mjs  # a full-screen app's screen survives an attachment
deno run -A tests/browser/modes.mjs      # and so do the modes it declared once
deno run -A tests/browser/copyselect.mjs # selecting copies, and OSC 52 copies too
deno run -A tests/browser/save.mjs       # cmd/ctrl-s saves whether or not the editor has focus
deno run -A tests/browser/autosave.mjs   # the editor writes itself out, and stops when the file diverges
deno run -A tests/browser/tabwrap.mjs    # the tab strip wraps, and re-fits the terminal under it
deno run -A tests/browser/shiftenter.mjs # shift+enter sends LF, so Claude inserts a newline
deno run -A tests/browser/hledit.mjs     # a code file stays highlighted while you edit it
deno run -A tests/browser/edit-by-default.mjs # clicking a text file opens an editor, not a preview
deno run -A tests/browser/preview-follows.mjs # a previewed file follows the file on disk
deno run -A tests/browser/buffer-lifecycle.mjs # navigating a file is not an edit; undoing one comes back clean
deno run -A tests/browser/termlinks.mjs # a printed path or URL is a link only while the modifier is held
deno run -A tests/browser/ide.mjs       # openDiff's proposal tab (Accept/Reject) and the Alt+K mention keybinding
deno run -A tests/browser/claudeterm.mjs # the ✻ button: a terminal with claude typed in, hidden when claude is not installed
deno run -A tests/browser/worktrees.mjs # the header's worktree switcher chip + panel
deno run -A tests/browser/worktree-launch.mjs # the ✻ prompt, worktree creation into a second tab, switcher state and removal; needs a real CDP click for window.open
deno run -A tests/browser/overview.mjs   # the front page (/): live session list, clicking one focuses it, ?at= reaches the picker, and selecting a project narrows/widens the session list
deno run -A tests/browser/claudetab.mjs  # a terminal tab running a Claude wears the Claude mark: the /proc watcher, the data-claude attribute, and the CSS that turns it into a different picture
deno run -A tests/browser/changes.mjs    # the Changes pane and the header chip follow the working tree, not just `.git/index`
deno run -A tests/browser/vanished.mjs   # a file deleted or moved out of the project from under an open tab: no empty editor, and unsaved work survives
deno run -A tests/browser/renamed.mjs    # a file renamed *inside* the project: the tab follows it, with its unsaved work
```

Each scenario is its own file and its own resh, so they can be run in any
order or on their own.

Needs `deno`, `dtach`, a Rust toolchain, and a Chromium. The browser is found,
never installed: `$CHROME`, else `chromium` / `chromium-browser` /
`google-chrome` on `PATH`. With none of those the run **skips** with a message
rather than failing — a machine without a browser is a normal state.

`cargo test` does not run these and must not: they need a browser, they take
tens of seconds, and the Rust suite has to stay runnable everywhere.

## What a run does

Each run is hermetic. `harness.mjs` builds resh, creates a throwaway project
and its own `RESH_STATE_DIR`, starts a private server on a free port, and tears
all of it down afterwards — including any `dtach` session it started, which it
finds by that unique state-dir path. **It never touches the deployed or
development instance**, so a test run cannot kill a session someone is using.

`RESH_CMD` is never set. Substituting a plain command for `dtach` is the trap
that once let a missing socket directory reach production green; a browser test
that skipped real `dtach` would be testing the same fiction from a new angle.

The browser profile persists in `tests/browser/tmp/` (gitignored) so repeat
runs start faster. Deleting it is always safe. It lives there, rather than in a
temp dir, because snap-packaged Chromium is confined to non-hidden paths under
`$HOME` and cannot read `/tmp`.

## Buffers that have to stay dirty: `autosave: false`

`AUTOSAVE_MS` is 1000ms and a blur flushes too, so a test that types into a
buffer and then does something within a second sees a dirty buffer *whether or
not* it asked for autosave to be off. It passes, on a race it happened to win,
and stops winning on a loaded box — or worse, passes for the opposite reason:
`do_save` resets any successfully-saved buffer to `Content::Clean`, so an
autosave can clean up a buffer that a broken hash rule wrongly dirtied.

`fixture({ autosave: false })` writes `.resh/config.toml` into the fixture's own
project before the server starts; `disableAutosave(dir)` does the same for a
second project (`autosave.mjs` and `buffer-lifecycle.mjs` keep one of each, so
both halves of the config cascade are exercised).

**A test that depends on it must prove it took**, because a fixture that
silently does nothing leaves the test green and meaningless. The cheap proof is
`ok(await page.evalIn('AUTOSAVE === false'), ...)`; where the point is a buffer
staying unsaved, wait past the window and require the file on disk to be
untouched as well. Measured by commenting the key out of `disableAutosave` so
every project silently had autosave on again: `renamed.mjs` fails 4,
`vanished.mjs` 6, `autosave.mjs` 4, `edit-by-default.mjs` 1 — and
`buffer-lifecycle.mjs` failed **nothing**, despite a header comment explaining
that its section C depends on autosave being off. Its protection was real and
invisible; it now asserts `AUTOSAVE === false` first, and fails 1 under the same
neutering.

## Writing another one

Ask the question CLAUDE.md asks of every test here: **would this fail if I
deleted the code it covers?** Then answer it for real — apply the broken
version, run it, read the failure, restore. Every scenario here was verified
that way, and it is not ceremony: it caught two assertions in `reconnect.mjs`
and one in `mdlinks.mjs` that passed while asserting nothing — and, in
`dotfiles.mjs`, an assertion that passed because the watcher was re-fetching
the tree ~3 times a second on its own. That last one was a real defect
(`watch::is_access`), found only because the deleted-code check was actually
performed.

- Reverting the reconnect to its pre-fix behaviour (mark the entry stale, never
  retry) fails 7 assertions in `reconnect.mjs`.
- Deleting the `term.reset()` before the replay fails 1, on copy count.
- Deleting the `refreshTree()` call app.js makes when a State flips
  `show_hidden` fails 4 assertions in `dotfiles.mjs`; pinning its glyph to a
  constant fails 3.
- In `autosave.mjs`: deleting the input timer fails 2 assertions, the blur
  flush 1, and reading `AUTOSAVE` as a constant instead of from `data-autosave`
  fails 2. Deleting the conflict *pause* fails 3 — but only after those
  assertions were rewritten to count conflict banners (0 from autosave, 1 from
  ⌘S). The obvious assertion, that the diverged file is not overwritten, passes
  with the pause deleted: that property comes from the server's `force: false`,
  not from the client, and asserting it proves nothing about the pause.
- Dropping the State-driven unpause in `autosave.mjs`'s D3 fails 2: the header
  goes on claiming the file changed underneath a buffer that was just
  discarded, and autosave never resumes for it again.
- Rebinding the save shortcut to the textarea (`ta.onkeydown`, its shape before
  the document-level handler) fails 2 assertions in `save.mjs` — both unfocused
  cases time out with the file unchanged on disk — while the focused case goes
  on passing, which is what says the test is testing focus and not saving.
- In `edit-by-default.mjs`: putting the tree click back to `mode: "Preview"`
  fails 2; handing the ✎ to every editable file again fails 2 — one because a
  code tab regains a toggle to a mode nothing opens in, one because the count
  is per tab rather than per strip. The svg section is the guard on the rule
  that broke first: an svg draws *and* is text, so a "markdown only" toggle
  rule silently made it read-only, which `mdlinks.mjs` caught before it shipped.
- In `hledit.mjs`: making every file plain fails 3; setting the wrong
  `language` attribute fails 3 different ones (spans still exist, but not
  Rust's); wiring a textarea other than the one `<code-input>` builds fails
  exactly 1 — the disk assertion — which is the whole point of that assertion,
  since the text still types and still highlights. Dropping the vendored
  `code-input.min.css` link puts the two layers 153px apart and fails the
  geometry pair; dropping this app's font override fails the font assertion
  alone. Note that overriding the textarea's `font-size` does *not* fail the
  metrics assertion — code-input forces `font-size: inherit !important` on both
  layers, so that pair covers the stylesheet being loaded, not the override.
  On the wrapping half: removing `white-space: pre-wrap` fails 1, letting the
  highlighted layer keep code-input's `width: max-content` fails 1 (the editor
  grows to 4739px inside a 590px pane rather than wrapping), and letting the
  highlight.js theme keep its own background fails 1. The fixture carries an
  unbreakable 420-character token on purpose — the library pins
  `word-wrap: normal` on both layers, so that line scrolls rather than wraps,
  and asserting "nothing overflows" would have been asserting the wrong thing.
- Mounting the editor without its breadcrumb fails 2 assertions in `save.mjs`;
  keeping it but dropping the `.editwrap` flex rules fails 1 more — the
  textarea ends 26px below the pane, hiding its own last lines. Forcing the
  Save button always-hidden fails 3 in `autosave.mjs`, always-shown fails 1 in
  `save.mjs`; the two configs are asserted from opposite directions so neither
  can pass by the button simply never being built.
- Deleting the Shift+Enter handler fails 1 assertion in `shiftenter.mjs` (the
  pty sees 13, not 10) while the plain-Enter assertion goes on passing;
  dropping its `!e.shiftKey` guard so every Enter sends LF flips exactly which
  of the two fails. Both halves were watched failing — the plain-Enter half
  first passed for the wrong reason, reading the *previous* probe's number out
  of the scrollback, which only agreed while both answers were 13.
- In `renamed.mjs`: dropping the `follow_rename` call from `watch.rs`'s batch
  fails 10 of the 20 — the tab never moves. Keeping it and deleting only the
  `BufferText` that follows the rekey fails exactly 1, and it is the one that
  matters: the editor re-mounts at the new name reporting `""`, which is the
  empty editor `vanished.mjs` exists to prevent, arriving through the rename
  door. Deleting the `disk_hash != base_hash` guard in `file_changed_externally`
  fails 2 (the moved buffer is flagged stale and autosave pauses, over a file
  whose bytes never changed).
  Also checked and *not* asserted: sending that `BufferText` before the `State`
  instead of after leaves the file fully green. The prune app.js runs on every
  State keeps the new rel, because the State that moves the tab lists the
  buffer under it — so an ordering assertion would have been a trap rather
  than a guard.
- In `vanished.mjs`: making `file_changed_externally` return a bare `false` for
  a deleted file again — the state before `hub::file_vanished` — fails 9 of the
  20, including the reported one, where the pane reads
  `notes.rssavedSavecode-input…fn one() {` — an editor, mounted, empty, over a
  file that is gone. Restoring that and reverting only app.js's `modeButton`
  guard fails exactly 1 (the demoted tab has no way back to Edit); reverting
  only `render::file_error_fragment` fails the same 1, because the switch is
  appended to the `.path` breadcrumb that fragment carries. Section B polls for
  the not-found text rather than reading it once: the demotion arrives as a
  State event and the pane re-mounts by fetching, so a single read can catch an
  empty `.content` — which would have passed the `!hasEditor` half on its own.
  That was seen during the revert run, not reasoned about.
- In `changes.mjs`: putting `StatusChanged` back behind `.git/index`/`.git/HEAD`
  alone (the `Class::Ignore` guard in `watch.rs`'s batch loop) fails 8 of the 13
  — every "it appeared" assertion, in all four sections. Restoring that and
  dropping only app.js's `new Event("git")` dispatch fails exactly 2, both in
  the header chip, so the two halves of the fix are separately attributable.
  Section B's 2-second settle is load-bearing, not politeness: serving the pane
  runs `git status`, which writes `.git/index` to refresh its stat cache, and
  that write was already a refresh trigger — without the wait, the load's own
  round trip carries B's edit into the pane and B is the *only* section that
  survives the revert. It passed for that wrong reason once, which is how the
  wait got there.
- In `claudetab.mjs`: deleting `app.js`'s `b.dataset.claude` fails 3, starting
  with the marking itself; deleting the `.tabstrip .tab[data-claude]` rule
  from `style.css` fails only 2 — "the picture actually changed" and "brand-
  filled" — while "marks its tab" goes on passing. That second run is the
  reason the assertions read `getComputedStyle(tab, "::before")
  .backgroundImage` rather than stopping at the attribute: a tab can carry
  `data-claude` and still look exactly like every other terminal, so an
  attribute-only test would have shipped green over a feature that draws
  nothing. Both were watched failing, then restored.

  A third failure was the fixture's own, and is worth recording because it
  looked like a product bug: the fake `claude` began as a copy of `/bin/sleep`
  and never ran at all — coreutils is a multi-call binary that dispatches on
  `argv[0]` and answers `unknown program 'claude'`. The screen dump is what
  found it. It is a copy of `bash` now, with a trailing `; :` so bash does not
  exec-optimise itself away and hand `comm` back to `sleep`.
- In `claudeterm.mjs`: deleting `term.rs`'s write of the launch keystrokes
  fails 2 (claude never starts; no port line to read); pinning `LAUNCHES` to
  `["claude"]` instead of reading `data-launches` fails 1 (a ✻ on a server
  whose shell has no claude); and restoring `do_new_terminal`'s old order —
  `TerminalStarted` *before* the snapshot that carries the tab — fails 6,
  starting with "the tab is attached": the client drops a start event for a
  tab it does not hold yet, and both + and ✻ land on the "press Enter"
  placeholder. That last one was the + button's actual behaviour until this
  test clicked it; no Rust test had, because the guard that drops the event
  lives in app.js.
- Restoring the tab strip's single scrolling row (`overflow-x: auto` with a
  hidden scrollbar, `height: 32px` on `.panehead`) fails 11 assertions in
  `tabwrap.mjs`; keeping the wrap but deleting render()'s re-fit of a terminal
  under a header that changed height fails exactly 1 — the terminal's row
  count, which is the only thing that says the PTY was told.
- Disabling the `raw` fragment route (`src/routes.rs`) fails the naturalWidth
  assertions in `mdlinks.mjs` (`naturalWidth === 0`, not DOM presence);
  restoring `if (t.k === "File")`'s dropped `!isImage(t.rel)` fails the
  no-edit-toggle assertion; narrowing the double-contextmenu guard back to
  `closest("a.file")` fails the markdown-link right-click assertion with
  `got 2`.
- In `termlinks.mjs`: deleting `ensureTerm`'s `registerTermLinks` call fails 14
  assertions, deleting the `linksArmed` gate inside the provider fails 3,
  registering the path provider ahead of the URL one fails 2 (the marked text
  is `/example.com/a/b`, the URL's tail, instead of the whole URL), and
  letting `PATH_RE` match zero directory segments fails 1 (a bare
  `backlog.md` becomes a link). The two "no link is offered" assertions pass
  with the whole feature deleted — zero links is also what no providers, and
  an off-screen row, produce — so that file carries a provider-count guard
  and a row-was-found guard beside each of them.
  Sections F and G cover what the provider-level assertions cannot: that
  arming re-marks the link under a pointer that never moved, and that it does
  so without the application noticing. Dispatching the synthetic mousemove on
  `.termhost` rather than on `.xterm-screen` fails 3, and so does making the
  detour a sideways one within the same line; letting those events bubble
  fails 1, because xterm's own `bindMouse` listener sits on `.xterm` and
  forwards any buttons-less motion to the PTY once an app is in mode 1003 —
  four phantom reports per Ctrl chord. Sections B-E stay green through all
  three, since they ask the providers directly and never touch xterm's hover
  path. Section F's precedence assertion hovers a cell **both** matchers
  claim and asserts that it does: at its first seat, inside `https:`, only
  the URL matcher reached and it passed with the registration order reverted.
- Removing the `refreshFile(ev.rel)` call from `FileChanged`'s handler fails 2
  assertions in `preview-follows.mjs` — the update times out and the stale
  text is still on screen — while the initial fetch (the file opening with
  its unchanged content) goes on passing, which is what says the test is
  testing the refresh and not the open.
- Reverting `Buffer::set_text` (`workspace.rs`) to an unconditional
  `Content::Edited(text)` — dropping the hash comparison against the base —
  fails 2 assertions in `buffer-lifecycle.mjs`: ⌘S on an untouched file now
  writes it (mtime changes), and undoing a typed character no longer comes
  back clean. Section A, on arrow/End/PageDown keys, keeps passing unchanged —
  those never reach `EditBuffer` at all, so they cannot discriminate the hash
  rule; that is why B and C exist. Section C runs against a second project
  with autosave off rather than racing a timing window against `AUTOSAVE_MS`
  on the autosave-on one: `do_save` resets any successfully-saved buffer to
  `Content::Clean` regardless of why it was dirty, so with autosave on, a
  window wide enough to be reliable is also wide enough for autosave to fire
  and clean up a wrongly-dirtied buffer itself — masking a broken hash rule
  with the write that comes after it, the same shape of trap as the
  conflict-pause one above, where the wrong property was being measured. With
  autosave off nothing but the hash rule can clean that buffer, so the check
  can use as generous a window as any other assertion here and still fail
  correctly when the rule is gone.
- In `ide.mjs`: dropping `mountTab`'s `Tab::Proposal` branch fails 6
  assertions across sections B and C (no hunk, no Accept/Reject button, no
  reply, tab never closes); deleting only the Accept/Reject button-append in
  `renderProposal` fails 6 different ones — **the two hunk-visibility
  assertions keep passing**, which is what says those two are testing the
  diff view and not the buttons (if button-removal had failed them too, they
  would not be discriminating between "the diff is broken" and "the buttons
  are broken"). Deleting the Alt+K `keydown` listener fails the 3 mention
  assertions in section E and nothing else. Folding `tabKey`'s `"Proposal"`
  case down to `` `Proposal:${t.id}` `` (dropping the content/pending
  distinction) passes sections B/C/D unchanged — this task's own
  `wsconn.rs` fix makes content arrive before the tab in every ordinary run,
  so nothing timing-based here can tell the fold apart from the real code —
  which is why section D also calls `tabKey` directly and asserts its
  output changes once `state.proposals` gains an entry; that direct
  assertion is the one that fails (`"before":"Proposal:...","after":"Proposal:..."`,
  identical) with the fold in place. Neutering `renderProposal`'s `if (!p)`
  guard (`if (false) {`) is the one that needed a second look: with the
  hunk view still built client-side, this failed on an uncaught exception
  before a button could ever be built. After review moved the hunk view to
  a server fragment (`renderProposal`'s content branch now does its own
  `fetch`), the same break instead let a wrongly-issued fetch for the
  bogus id return "this proposal is no longer open" and **append real
  Accept/Reject buttons a moment later** — which a synchronous check right
  after calling `renderProposal` missed entirely (the fetch hadn't resolved
  yet), passing for the wrong reason. Section D now waits ~1.5s after
  calling `renderProposal` before reading the result, specifically so a
  regression that removes the guard is still caught even though it manifests
  asynchronously rather than as an immediate throw. The equivalent check for
  the diff view itself — emptying `render::proposal_fragment` down to just
  the file name — now lives in `src/render.rs`'s own tests
  (`proposal_fragment_reports_distant_changes_as_separate_hunks` and its
  siblings), since the hunk view is Rust now, not a second implementation in
  `app.js`.
  Section I covers `activeTerminalSession()` itself, which none of the above
  reach: making it always return `null` fails the session-name assertion
  (`got {"...,"session":null}`) while the weaker "not session2" assertion
  **keeps passing** — a direct instance of the "no link is offered" trap
  further up this file, which is why the section asserts equality to the
  focused terminal's name, not just inequality to the other live one.
- In `worktrees.mjs`: commenting out the `["frag", "_worktrees"]` arm in
  `routes.rs` fails 5 — the chip label, the two row checks in section B
  against an empty fragment, "the current row is the main worktree", and,
  cascading, section C's navigation (there is no row left to click). The
  href/target pair check passes vacuously against zero rows, which is why it
  alone does not appear in that count. Commenting out `wtBtn.onclick` in
  `app.js` fails exactly 1 — "clicking the chip opens the panel" — while
  every other row/branch/href/current assertion in B still passes, because
  the fragment loads on its own `hx-trigger="load"` regardless of the
  button; only panel *visibility* depends on the handler. Adding
  `target="dl-x"` to the reachable row's anchor in `worktrees_strip` fails
  2: the href/target pair assertion directly, and, cascading, section C's
  navigation check — a targeted anchor opens a new browsing context instead
  of navigating the tab, which is exactly the bug the missing `target=`
  exists to prevent.
- In `worktree-launch.mjs`: removing `force: true` from the prompt's "start
  here anyway" button fails section D (the prompt reappears instead of a
  second terminal opening). Making the prompt call `newTerminal` on its own,
  with no click, does *not* fail the obvious assertion ("no terminal was
  opened") — the server intercepts that call exactly like the one that
  raised the prompt and answers with another `ClaudeHere`, so the two chase
  each other forever without ever reaching a real terminal, and whether that
  contention starves some unrelated poll enough to fail depends on host load
  at the time (observed both ways). The reliable catch counts
  `NewTerminal{launch:"claude"}` sends over the 1.5s after the prompt
  appears, by wrapping `window.send` (a plain top-level function, reachable
  off `window` since this is not a module script) — 0 with the bug fixed,
  dozens to low hundreds with it back. Dropping `history.replaceState` from
  the launch consumer fails "the ?launch= parameter was consumed" only, with
  no cascade — the terminal still opens from the still-present query param,
  it just never gets cleared. See the file's own header for the full
  revert-check log, including a second trap this test needed a workaround
  for: `Workspace::default_layout` always seeds pane 3 with one Terminal tab
  named `"term"` before anything is clicked, so every session-count
  assertion here is written against that baseline rather than an empty pane.

Five things will make a browser test lie to you here. Each is commented at its
site; do not "simplify" them away:

| Trap | What it does to a naive test |
|---|---|
| `Network.emulateNetworkConditions {offline:true}` | Blocks *new* requests, leaves established sockets open. The test asserts a reconnect while nothing ever disconnected. Cut TCP at the proxy instead. |
| `term.paste()` | bash enables bracketed paste, so a pasted newline is inserted literally instead of submitting. The command sits on the prompt and every later wait times out. Use `term.input()` with `\r`. |
| Typing before the prompt | readline discards typeahead while initialising, so the first command silently vanishes. Wait for a prompt. |
| Content that fits one screen | `dtach`'s redraw opens with `\e[H\e[J`, which hides duplicated output all by itself — the no-duplication assertion passes with the reset deleted. Scroll past one screen first. |
| The default 800x600 headless window | Narrower than the default left (260px) and right (520px) panes together: the middle column collapses and the right pane hangs off the viewport. A layout assertion then measures *that*, and `elementFromPoint` returns null off-screen, so a reachability test fails (or passes) for the wrong reason. Override the metrics — see `tabwrap.mjs` and `save.mjs`, where a layout assertion measured 1px of overshoot instead of the real 26px until the viewport was widened. |

## What these cannot prove

A real browser on this host is still one browser on one platform. Safari and
Firefox are untested, as is a real laptop suspend: the harness reproduces its
*effect* on the connection (an abrupt TCP close with no close frame, which the
browser reports as 1006) rather than the suspend itself.
