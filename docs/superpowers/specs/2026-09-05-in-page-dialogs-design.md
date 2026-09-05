# in-page dialogs — design

**Status:** proposed
**Date:** 2026-09-05

## Why

roost currently asks every question through a native browser dialog: ten call
sites in `static/app.js` — four `prompt()`, five `confirm()`, one `alert()`.
They are the only part of the workspace that does not look like roost, they
cannot be themed, and one of them is a menu wearing a text box (`"1 = new file
2 = new folder 3 = rename 4 = delete"`), a shape that exists only because
`prompt()` had no other way to offer a choice.

Replacing them is not cosmetic. `confirm()` blocks the JavaScript event loop;
an in-page dialog does not. Every destructive action in the workspace is
gated by one of these, so the replacement changes when evidence is gathered
relative to when it is acted on. That is the subject of "The async hazard"
below and the reason this has a spec rather than being ten substitutions.

## Scope

In: a reusable dialog primitive built on the native `<dialog>` element; a
pointer-anchored context menu replacing the numbered `prompt()`; conversion of
all ten call sites; the merge of Close Project's `alert`/`confirm` pair into
one dialog; re-resolution of the one positional intent that becomes unsafe;
updates to the three browser test files whose assertions the change
invalidates.

Out: any change to the wire protocol; inline editing in the file tree;
animation; a long-press path to the file menu on touch devices (today's only
route is `oncontextmenu`, which is unchanged by this work).

## No library

Six zero-dependency modal libraries were surveyed (ldCover, PicoModal,
MODALit, Z-MODAL, rmodal, Micromodal), 1–6 KB each. All exist to provide focus
trapping, Escape handling, a backdrop, and an inert background. `<dialog>`
with `.showModal()` provides all four natively: Baseline 2022 (Chrome 98,
Firefox 98, Safari 15.4), >95% support, no polyfill, Escape arriving as a
`cancel` event, and rendering in the top layer above every `z-index` on the
page — so no coordination with `#searchoverlay` (z-40) or
`body.searching header` (z-41) is required.

A library would also ship its own CSS, which would then have to be overridden
to reach roost's theme tokens — strictly more work than styling the native
element from scratch. `static/vendor/` gains nothing.

`popover="auto"` was considered for the context menu, since it gives light
dismiss for free. Rejected: Baseline 2024 at ~88–91% against `<dialog>`'s
95%+, and mixing two primitives means a popover that must close itself before
opening a dialog. Backdrop-click-to-close on a `<dialog>` reaches the same
place in four lines, and leaves one primitive to understand.

## The primitive — `static/dialog.js` (new)

A classic script, loaded before `app.js` in `render.rs`. `build.rs` walks
`static/`, so it enters the asset table with no registration step. Globals,
matching the file idiom (`showBanner`, `showError`, `focusSession`).

```js
askConfirm({ title, lines, confirm, danger, blocked }) -> Promise<boolean>
askText({ title, label, value, confirm })              -> Promise<string|null>
askMenu({ items, x, y })                               -> Promise<string|null>
```

`lines` is an array of strings, each rendered as its own paragraph via
`textContent`. `blocked` is a reason string: when present the confirm button
is disabled and the reason is shown beside it, which is what lets Close
Project be one dialog instead of two (see below); when absent the button is
live. `items` is `[{ id, label }]`, and `askMenu` resolves to the chosen
`id`, or `null` if dismissed.

**None of them reject.** Every caller is an event handler; a rejection there
is an unhandled promise with nothing to catch it. A dismissed dialog is a
value — `false`, `null` — never an error.

**One at a time.** A module-level guard; a second call while one is open
resolves immediately with the dismissal value rather than stacking. Stacked
modals over a live terminal are worse than a dropped stray event.

**Escape and backdrop-click both dismiss.** Escape is free, via `cancel`.
Backdrop click is a `click` listener testing `e.target === dialogEl`, because
a click on the backdrop has the dialog element itself as its target.

**`danger: true` focuses Cancel, not Confirm.** A deliberate departure from
native `confirm()`, where Enter accepts. For Delete, End session, Close
project and Remove worktree, Enter cancels and destroying requires a
deliberate click or Tab-then-Enter. This is the codebase's own rule — the
burden of proof is on destroying — applied to the keyboard.

**`askText` preselects the basename.** Renaming `src/main.rs`, the input
holds the full path with `main.rs` selected, so typing replaces the name
rather than the directory. `prompt()` could not do this.

**Focus restoration is explicit.** The spec has `close()` restore focus to the
previously-focused element, but roost's terminals are pooled DOM nodes moved
between panes with `appendChild`, and the interaction between `showModal()`
and a focused xterm is untested here. `dialog.js` records
`document.activeElement` on open and restores it on close, and the browser
test asserts it. Relying on the browser and finding out later is how the
`base_hash` saving defect shipped.

## Markup and the escaping rule

Three shells ship in the server-rendered page, empty, beside `#searchoverlay`:
`#dlg-confirm`, `#dlg-text`, `#dlg-menu`. This follows
`<div id="noticepanel" hidden></div>` (`render.rs:1647`), which has a test
asserting it ships empty (`render.rs:3024`).

**Without a `hidden` attribute.** A `<dialog>` with no `open` attribute is
already `display: none` from the UA stylesheet. Copying the
`#searchoverlay[hidden] { display: none; }` idiom here produces a dialog that
can never be shown — the one place where following the existing pattern is
wrong.

`dialog.js` fills them with `textContent` and builds menu rows with
`createElement`. **No HTML string with an interpolated value exists anywhere
in this feature.**

This is a security property, not tidiness. roost opens cloned repositories, so
a path is attacker-influenceable: a repo can contain a file named
`<img src=x onerror=…>`. `confirm("Delete " + rel)` renders that as inert
text. An `innerHTML`-built dialog renders it as markup, in the workspace
document — the page holding the websocket that spawns shells. `escapeHtml`
exists at `app.js:2334` and would work, but the stronger position is the one
the markdown sanitizer already takes: construct the output so the dangerous
operation is never reached, rather than remember to escape at each site.

## Styling

A `<dialog>` is an ordinary block box; every property already used on
`.searchpanel` applies unchanged. The UA stylesheet supplies a border, `1em`
of padding and `background: canvas`, all of which are reset explicitly.

```css
dialog.roost { background: var(--bg2); color: var(--fg);
               border: 1px solid var(--border); border-radius: 8px;
               padding: 0; width: min(420px, 92vw); }
dialog.roost::backdrop { background: rgba(0,0,0,.35); }
```

`::backdrop` uses a literal, not `var()`. MDN states `::backdrop` "neither
inherits from nor is inherited by any other elements", which would leave a
`var(--…)` reference unresolved; the same page then contradicts itself with a
working example. The question is not settled here and does not need to be:
`#searchoverlay` already uses a literal `rgba(0,0,0,.35)`
(`style.css:819`), so matching it avoids the question. A themed backdrop, if
ever wanted, is a browser check rather than a documentation question.

No animation. Fading a dialog in requires `@starting-style` plus
`transition-behavior: allow-discrete`, because `display` and `overlay` are
discrete properties; both are Baseline 2024, and Firefox had a `::backdrop`
transition bug fixed in 131. `confirm()` had no animation and nobody will miss
it. This removes the only part of the styling story with sharp edges.

## The async hazard

`confirm()` blocks the event loop: nothing can be processed while it is open.
An in-page dialog does not — the websocket keeps delivering `State` events and
the tab strip keeps re-rendering underneath it (`app.js` rebuilds `.tabstrip`
wholesale on every render, per the comment at `render.rs:1665`).

`CloseTab { pane: PaneId, idx: usize }` (`proto.rs:61`) addresses its target
by **position**. So: click a dirty file tab's ×, leave the dialog open, let
another client close a tab to its left, then confirm — and roost closes a
different tab than the one clicked. The index is evidence gathered before the
wait and acted on after it.

Tracing `closeTab` (`app.js:2323-2332`), a dialog is reachable on exactly two
paths, and only one of them then sends a positional intent:

| Path | Dialog | Intent | Stale-safe |
|---|---|---|---|
| Dirty `File` tab, confirmed | yes | `CloseTab { pane, idx }` | **no** |
| `Terminal`, not detaching, confirmed | yes | `EndSession { session }` | yes, keyed by name |
| `Terminal` + alt-click (detach) | no | `CloseTab` | yes, stays synchronous |
| Every other tab kind | no | `CloseTab` | yes, stays synchronous |

**No dialog means no `await` means no staleness.** Those paths keep today's
behaviour exactly. The single async path closes a `File` tab, which carries a
`rel`, so it re-resolves the index after the dialog resolves — the idiom
`focusSession` already uses at `app.js:2761`:

```js
const ti2 = state.panes[pi].tabs.findIndex((x) => x.k === "File" && x.rel === t.rel);
if (ti2 < 0) { showError(`${t.rel} is no longer open`); return; }
send({ t: "CloseTab", pane: pi, idx: ti2 });
```

The `< 0` branch is the point. The tab not being found is not a reason to
close index `ti` anyway: it is "I cannot tell", and the response is to destroy
nothing. It raises a banner rather than returning silently, because a × that
visibly does nothing is indistinguishable from a broken control — which is how
the Alt+K binding was reported (`app.js:2180`).

The remaining intents are content-addressed already and need no work:
`EndSession`(session), `DeleteFile`(rel), `RenamePath`(from/to),
`CreateFile`/`CreateDir`(rel), `RemoveWorktree`(key), `CloseProject`(nothing).
Each was checked in `proto.rs`.

## The call sites

| Today | Becomes |
|---|---|
| `prompt` numbered menu (`app.js:1280`) | `askMenu({x, y, items})` at the pointer |
| `prompt` new file (1287) | `askText({value: "<dir>/untitled.txt"})` → `CreateFile` |
| `prompt` new folder (1290) | `askText({value: "<dir>/newdir"})` → `CreateDir` |
| `prompt` rename (1293) | `askText({value: rel})` → `RenamePath` |
| `confirm` delete (1297) | `askConfirm({danger: true})` → `DeleteFile` |
| `confirm` dirty close (2325) | `askConfirm(…)` → re-resolve index → `CloseTab` |
| `confirm` end session (2327) | `askConfirm({danger: true})` → `EndSession` |
| `alert` + `confirm` close project (2405, 2408) | one `askConfirm`, confirm disabled while dirty |
| `confirm` remove worktree (2466) | `askConfirm({danger: true})` → `RemoveWorktree` |

`fileMenu` and `closeTab` become `async`. All three of their callers
(`app.js:722`, `1262`, `1272`) perform their `preventDefault` /
`stopPropagation` synchronously before the call, so nothing is lost to the
microtask boundary.

The menu is a `<dialog>` of `<button>`s, so Tab, Enter and Escape work
natively; ↑/↓ are added to move focus, because a context menu without arrow
keys reads as broken.

One behaviour improves for free: today, choosing `3` or `4` at the project
root silently does nothing, because the guards are `choice === "3" && rel`.
The menu simply does not offer Rename or Delete when `rel` is empty.

### Close Project

Today the dirty case is an `alert` (dismiss only) and the clean case a
`confirm`. The split exists because a native dialog cannot disable its OK
button. One dialog replaces both: it lists the sessions that will end and any
unsaved files, and disables the confirm button while anything is dirty, with
the reason stated beside it. The client-side pre-check is unchanged — it still
mirrors the server's `CloseRefused` rather than making its own ruling — the
user simply sees the whole picture at once instead of two different dialogs
depending on state.

## Testing

**One existing test becomes vacuous and must be rewritten.** Four browser
files touch window dialogs and they do not mean the same thing:

- `termlinks.mjs:531` stubs `confirm` for **xterm's own fallback**, not
  roost's dialogs, and line 1017 asserts `__confirms.length === 0` as positive
  evidence that roost's `linkHandler` took the activation. xterm still calls
  `confirm`. **This stub stays.** Removing it as part of a sweep would
  silently destroy a real assertion.
- `closeproject.mjs` (121, 172, 221) and `worktree-launch.mjs` (243, 263) stub
  `confirm`/`alert` to auto-accept. After the change those stubs are no-ops,
  but the following assertions ("the project closed", "the worktree went
  away") then fail loudly — the safe way to break.
- `mdlinks.mjs:220-245` is the trap. It asserts `window.__prompts.length === 0`
  — that clicking a markdown link does **not** pop the file menu. Once nothing
  calls `window.prompt`, that count is zero forever: the test passes vacuously
  and stays green while testing nothing. It must be rewritten to assert that
  `#dlg-menu` is not `open`.

The plan:

1. Rust tests in `render.rs` that the three shells ship, mirroring the
   `noticepanel` test at 3024.
2. New `tests/browser/dialogs.mjs` for the primitive: Escape dismisses,
   backdrop click dismisses, Cancel sends no intent, Confirm sends exactly
   one, focus returns to where it was. The "Cancel sends no intent"
   assertions need an intent-recording hook to exist first, or they pass
   whether or not the dialog works.
3. Rewrite `mdlinks.mjs` against `#dlg-menu`; update `closeproject.mjs` and
   `worktree-launch.mjs` to click real buttons; leave `termlinks.mjs` alone.
4. **The stale-index test, revert-checked.** Open a dirty file tab, open its
   close dialog, have a second client close a tab to its left, confirm, and
   assert the intended tab closed. Then revert the re-resolution, re-run, and
   watch it close the wrong one — applied and read, not reasoned about, with
   the failure recorded in the test's comment.
5. `cargo test -- --test-threads=1` (a bare `cargo test` hangs on this host),
   run on the Linux host, plus a real-browser check: `static/app.js` is
   unreachable from any Rust test.

Browser tests flake under contention, so a back-to-back sweep is not by itself
a regression signal; a suspected failure gets re-run alone before it is
believed.

## Future work

- A long-press path to the file menu on touch devices. Today `oncontextmenu`
  is the only route, and this change does not alter that.
- Arrow-key type-ahead in the menu, if it ever grows past four items.
- A themed `::backdrop`, if the inheritance question is ever settled by test.
