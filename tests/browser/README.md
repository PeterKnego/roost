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
deno run -A tests/browser/preview-follows.mjs # a previewed file follows the file on disk
deno run -A tests/browser/buffer-lifecycle.mjs # navigating a file is not an edit; undoing one comes back clean
deno run -A tests/browser/termlinks.mjs # a printed path or URL is a link only while the modifier is held
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
- In `termlinks.mjs`: deleting `ensureTerm`'s `registerTermLinks` call fails 8
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
