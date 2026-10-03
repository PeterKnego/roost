# The Files pane and the terminal, driven by a finger

*2026-09-16. Status: designed, not implemented. Issue
[#110](https://github.com/PeterKnego/roost/issues/110). Reported by Dean from
an iPhone: no new file in the selected folder, no upload by drop, no terminal
selection.*

## The one sentence

v0.6.0 made the *layout* work on a phone. The Files pane and the terminal are
still driven by **right-click, drag, and mouse-drag**, and a phone has none of
the three. #97's paste button was the first instance; this is the rest.

## 1. The folder bug, which is not phone-only

Reproduced in Chromium on `develop` @ 5c0cf8e:

| right-click on | New file offers |
|---|---|
| folder `sub` | `untitled.txt` — **the project root** |
| file `sub/inside.txt` | `sub/untitled.txt` — correct |

Directories render as `<details data-rel>`; the menu is wired only to `<a>`
rows, so a folder falls through to the container handler written for blank
space, which passes `rel: ""`.

**Two defects, and fixing either alone still leaves it wrong:**

- the menu is not wired to folders at all
- `fileMenu` computes `dir` by stripping the last segment — right for a file,
  wrong for a folder, which *is* the directory

So `fileMenu` has to be told which of the two it was given. A boolean
parameter, not a guess from the string: `rel` has no shape that distinguishes
`docs` the folder from `docs` the extensionless file, and guessing from the DOM
at use time is how the two get confused again later.

**A test must use a nested folder.** With a top-level `sub`, "the folder" and
"the parent of the folder's parent" are both `""` and a half-fix passes.

## 2. Upload without a drag

**Nothing about the existing upload path is broken.** Verified: a real
`POST /upload/proj?dir=sub` lands the file and the tree refreshes; and
`uploadTargetDir` is correct for every reachable point, because `<summary>`
spans the full row. There is simply no way to *start* an upload without a
mouse.

So: an **"Upload files…" item in the file menu**, opening a hidden
`<input type="file" multiple>` and calling the same `uploadFiles(files, dir)`
the drop calls, with the same `dir` the menu was opened on. Drag-and-drop stays
exactly as it is.

This is the one item that is plainly better on desktop too, which is worth
saying: it is not a phone workaround bolted to a mouse-first pane.

## 3. Selecting terminal text on touch

Two facts make it impossible today, and both are already documented in
`app.js`:

- xterm's rows are `user-select: none` and it draws its own selection layer, so
  a browser selection cannot happen.
- xterm's selection is driven by **mouse** events. On touch a drag is consumed
  by `wireTouchScroll` or by native scrolling, and a drag never synthesises a
  mouse drag.

**A select-mode toggle on `#termkeys`**, beside the paste button. While it is
on, `wireTouchScroll` stands down and touch points are forwarded to xterm as
mouse events, so a drag selects. Copy-on-select and the OSC 52 handler — both
already shipped — then do the rest unchanged.

Three properties it must have:

- **It turns itself off.** A terminal stuck in select mode is a terminal that
  no longer scrolls, with nothing on screen saying why. Off on selection
  complete, and off when the pane changes.
- **It is visible while on.** `aria-pressed`, and a style that is not only
  colour.
- **It suspends scrolling rather than replacing it.** `wireTouchScroll`'s
  `translate()` gate stays exactly as it is; select mode is a second gate in
  front of it, so nothing about the scrolling behaviour that shipped changes
  when the mode is off.

## What is deliberately not done

- **No long-press timer, yet.** Item 1 wires the folder case and fixes the
  targeting, which is the whole of the *bug*. Whether a phone additionally
  needs a `touchstart` timer depends on whether iOS Safari synthesises
  `contextmenu` on a long-press over these elements, and that cannot be
  answered on this host. Adding a timer blind risks two menus on the devices
  where the synthetic event does arrive, and a bounced menu is worse than no
  menu. This is the first thing to revisit after the device check.
- **No drag-to-upload on touch.** The menu item makes it unnecessary.
- **No selection handles or magnifier.** xterm draws its own selection; a
  native-feeling handle UI is a much larger piece of work and this is about
  making copy possible at all.

## What cannot be verified here

Headless Linux, Chromium. **The gestures themselves — long-press, and a
touch-drag selecting inside xterm — are not reachable.** CLAUDE.md's dev/prod
substitution trap applies exactly as it did for #97, and the honest statement
is the same: a browser test covers the targeting, the menu contents, the
upload call and the mode's state machine; it does not cover whether a finger on
an iPhone produces them. **Dean's device check is part of closing #110.**

## Testing

- **The folder-targeting test must use a nested folder** (`a/b`), for the
  reason above.
- **It must assert on the path the dialog offers**, not that a dialog opened.
- **The upload test must assert the `dir` the picker's files are sent with**,
  and must open the menu on a *folder* — a file's parent and a folder's own rel
  differ only there.
- **The select-mode test must assert `wireTouchScroll` stood down**, not merely
  that a class toggled; a mode that lights up and still scrolls is the failure.
- **Revert-check each**, per CLAUDE.md, and record the observed failure.
