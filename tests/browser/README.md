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
- Disabling the `raw` fragment route (`src/routes.rs`) fails the naturalWidth
  assertions in `mdlinks.mjs` (`naturalWidth === 0`, not DOM presence);
  restoring `if (t.k === "File")`'s dropped `!isImage(t.rel)` fails the
  no-edit-toggle assertion; narrowing the double-contextmenu guard back to
  `closest("a.file")` fails the markdown-link right-click assertion with
  `got 2`.

Four things will make a browser test lie to you here. Each is commented at its
site; do not "simplify" them away:

| Trap | What it does to a naive test |
|---|---|
| `Network.emulateNetworkConditions {offline:true}` | Blocks *new* requests, leaves established sockets open. The test asserts a reconnect while nothing ever disconnected. Cut TCP at the proxy instead. |
| `term.paste()` | bash enables bracketed paste, so a pasted newline is inserted literally instead of submitting. The command sits on the prompt and every later wait times out. Use `term.input()` with `\r`. |
| Typing before the prompt | readline discards typeahead while initialising, so the first command silently vanishes. Wait for a prompt. |
| Content that fits one screen | `dtach`'s redraw opens with `\e[H\e[J`, which hides duplicated output all by itself — the no-duplication assertion passes with the reset deleted. Scroll past one screen first. |

## What these cannot prove

A real browser on this host is still one browser on one platform. Safari and
Firefox are untested, as is a real laptop suspend: the harness reproduces its
*effect* on the connection (an abrupt TCP close with no close frame, which the
browser reports as 1006) rather than the suspend itself.
