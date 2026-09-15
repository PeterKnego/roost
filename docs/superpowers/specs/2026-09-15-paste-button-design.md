# A paste button, because iOS will not offer one

*2026-09-15. Status: designed, not implemented. Issue
[#97](https://github.com/PeterKnego/roost/issues/97). Reported by Dean against
roostedge: "long press to show ios paste is not working", and "maybe we need
paste button?" — which is the answer this implements.*

## What is wrong

On an iPhone, long-pressing a terminal raises no Paste callout, so clipboard
text cannot reach a terminal at all — and on a phone a terminal is mostly a
terminal running Claude.

The cause is a property `app.js` already documents for a different reason, in
the comment above `onSelectionChange`: **xterm's rows are `user-select: none`**,
and the editable element — xterm's helper `<textarea>` — is hidden and parked
under the cursor, not under the finger. iOS raises the edit callout for a
selectable or editable element at the touch point; over terminal rows there is
neither, so there is nothing for it to offer a menu about.

Desktop is unaffected and stays untouched: ⌘V reaches xterm's own handler, and
the capture-phase `paste` listener in `app.js` already returns early for
anything that is not an image.

## The control

**One more button on `#termkeys`**, the mobile-only row in `render::workspace_page`
that already carries esc / tab / ↑ / ↓ / ⏎ / ^C. Seventh in the row, last,
after ^C.

Mobile only, because that is what the bar is (`#termkeys` is `display: none`
until `body[data-mpane="3"]`) and because desktop paste is not broken. A row of
seven at 390 px leaves each button about 51 px wide, over the 44 px tap-target
floor `~/projects/CLAUDE.md` sets — asserted in the browser test rather than
asserted here.

## `term.paste`, never `term.input`

Every existing key in `TERM_KEYS` goes through `entry.term.input(...)`. **The
paste button must not.**

`Terminal.paste(text)` is the call that wraps the text in `ESC [ 200 ~` …
`ESC [ 201 ~` when the program has turned bracketed paste on, and Claude Code
turns it on. Sent through `input`, a multi-line paste arrives as a run of
carriage returns: every line submitted as its own turn. That is worse than the
bug being fixed — pasting nothing wastes a gesture, pasting eleven half-formed
turns into Claude wastes a conversation.

Bracketing is xterm's decision, not roost's: `paste()` consults the mode the
program set, so a plain shell still receives the text unwrapped and behaves as
it always did.

## Two paths, because the first one can be refused

**First: `navigator.clipboard.readText()`.** On iOS Safari this raises the
system's own "Paste" confirmation, which is the right consent UI — the same one
a native app gets — and one extra tap.

**Then: a dialog with a real `<textarea>`, focused.** Reached whenever the
first path is unavailable or rejected. A textarea is exactly the editable
element the iOS callout wants, so long-press works inside it; the user pastes
there and confirms, and the text takes the same `term.paste` route.

The fallback is not politeness. Three ways the first path does not happen, and
none of them is exotic:

- **`navigator.clipboard` is `undefined` outside a secure context.** roost
  binds loopback and is normally reached over the tunnel (HTTPS) or
  `localhost` (which counts as secure), but a LAN address over plain http has
  no Clipboard API at all.
- **Permission refused**, once or permanently.
- **No user activation**, if the gesture the button uses does not satisfy the
  browser's rule.

So the button's contract is: *something always happens.* A refusal that left
the user tapping a dead button would be the same defect in a new coat.

### The gesture, and why the fallback settles it

The other buttons on this bar bind `pointerdown` and `preventDefault()`,
deliberately: a click moves focus first, and on iOS focusing a button dismisses
the soft keyboard, so an arrow press would close the keyboard it exists to be
independent of. That comment is already in `initTermKeys` and stays true.

Whether `pointerdown` counts as the user activation `readText()` demands is
**not something this design asserts** — the rules differ by engine and the
development host cannot test the engine that matters. The design does not need
to know: if activation is missing the promise rejects, and a rejection is the
fallback's trigger like any other. That is why the fallback is the load-bearing
half and the Clipboard API is the shortcut.

## What this cannot verify, stated plainly

The development host is a headless Linux VM with Chromium. **The iOS behaviour
— the callout that does not appear, and whether Safari's paste confirmation is
satisfied — cannot be reproduced or tested on it.**

CLAUDE.md's *dev/prod substitution trap* is a table of four defects that a green
suite could not see because the test substituted something simpler for the real
thing. A Chromium-on-Linux test for an iOS Safari bug is a fifth row of that
table waiting to be written, and pretending otherwise is the failure mode, not
the substitution itself.

What the browser test can honestly cover, and will:

- the button exists in the bar, and the bar still fits a phone at the tap-target floor
- a paste reaches the pty **bracketed**, when the program asked for bracketing
- the same paste reaches it **unbracketed** when it did not
- the fallback dialog opens when the Clipboard API is absent or rejects
- text typed into the fallback reaches the pty the same way
- the button changes nothing on desktop

What it cannot: that any of this fixes the reported gesture. **Confirmation on
Dean's iPhone is part of closing #97**, and the issue says so.

## Testing

- **The bracketing assertion must read the bytes at the pty**, the way
  `shiftenter.mjs` does, not what xterm was called with. `term.paste` being
  called proves nothing about what the program receives.
- **Both bracketing cases, or neither means anything.** A test that only
  asserts the wrapper appears passes against a button that hard-codes the
  wrapper — which would corrupt every paste into a plain shell.
- **The fallback test must assert the dialog opened *and* that the clipboard
  path was not silently used instead**, or it passes on a machine where
  `readText()` happens to work.
- **Revert-check each**, per CLAUDE.md: apply the broken version, run it, read
  the failure, restore, and record what the failure looked like.
