# Transient messages: four tenants, four lifetimes

`.conflict` is a two-line CSS rule (`static/style.css:356`) — a warn-coloured
border, 8px of padding, a `--bg2` ground. Four unrelated things wear it, and
they do not agree about what they are:

| Call site | Placement today | Lifetime | What it actually is |
|---|---|---|---|
| `showConflict` `app.js:2105` | prepended into pane 2's `.content` | until answered | a **question** |
| `showClaudeHere` `app.js:2123` | prepended into a pane | until answered | a **question** |
| `showBanner` / `showError` `app.js:2155` | `.error-banner` → `position: fixed` | 8s | a **notice** |
| `setUploadProgress` `app.js:2645` | `document.body.appendChild`, no positioning class | the upload | a **status** |

The stylesheet already records this drift happening once and being corrected.
The comment at `style.css:358` explains why `.proposal-actions` was pulled
*out* of `.conflict`: "`.conflict` is a transient in-flow warning… a proposal's
bar is permanent pane furniture." That split was made deliberately, and then
three more tenants moved in without the question being re-asked.

This is the only entry in `docs/backlog.md` that arrived from someone hitting
it in real use rather than from a spec's nice-to-have list.

## What changes, in one sentence

Messages are separated by **lifetime** rather than sharing one costume, and
each kind is placed where its lifetime implies: a notice near what produced it,
a status on the edge of the pane it is filling, a question under the thing it
is about, and a filename edited in the tree row that holds it.

## Scope

Three separately-shippable parts, one spec, because the whole point is that
they share a vocabulary and splitting the spec is how they drift apart again:

1. **The message primitives** — `.conflict`'s four tenants become `notice`,
   `status`, and `question`, with correct placement.
2. **The native dialogs** — the ten `prompt()` / `confirm()` / `alert()` sites
   become in-app surfaces. This is the bulk of the work.
3. **The bell panel** — `#noticepanel` adopts the same severity vocabulary.

Non-goals: the notification *routing* (`docs/notifications.md`) is untouched;
this is about how a message looks and where it sits, never about which client
receives it.

## The three-way distinction this rests on

The taxonomy is not "error / warning / info". Severity is a colour. What
decides *placement* is lifetime:

- **Notice** — something happened, no answer is needed, it expires on its own.
- **Status** — something is happening now, to one pane, and it ends when the
  operation does.
- **Question** — a decision only the user can make; it must never expire,
  because an auto-dismissing question is a decision made by timeout.
- **Edit** — naming a file. Not a message at all, and the reason it appears
  here is that `prompt()` currently makes it look like one.

## Measured defects this fixes

Every number below was measured against the running app on 2026-08-31, by
driving real Chromium through `tests/browser/harness.mjs`.

**Simultaneous notices render as one.** Every banner is `position: fixed; top:
12px; right: 12px`, so three at once occupy an identical band: all at
`top: 20`, `height: 52`, right-anchored. `elementFromPoint` at the first
banner's own corner returns `"Error: third"`. This is not hypothetical —
`postFiles` loops `showError` once per failed part (`app.js:2666`), and an
upload request may carry up to `MAX_UPLOAD_PARTS` = 16 (`src/config.rs:181`).
A drop where four files are too large reports one of them.

**A notice overlaps the header.** The banner box runs `top 20 → bottom 91`
against a header band of `0 → 38`: an 18px collision. Note that a
centre-point hit test says otherwise — the controls' centres sit at y≈19, one
pixel above the banner's top edge — so a probe written that way is
non-discriminating and must not be used as the regression test.

**Progress squeezes the layout for the duration of every upload.** The box is
appended bare to `document.body` with no positioning class, so it inherits
`.conflict`'s in-flow placement. Measured: pane 0 `510 → 478`, pane 1
`332 → 311`, restoring on clear. Document height never changes, so this is
flex compression, not scroll growth.

**Not a defect, checked and ruled out:** no PTY resize results. The terminal
held 53 rows across a 68px host shrink. The squeeze is visual only.

## Placement: near the subject

The rule is that *where* a message appears carries information. A message
belongs to the pane whose work produced it.

**A per-pane region, between `.panehead` and `.content`.** `.pane` is already
`display: flex; flex-direction: column` (`style.css:153`), so a `flex: 0 1 auto`
region slots in with no layout surgery.

This is the correction that matters most. Both existing questions prepend into
`.content`, which puts them *above* the pane's own chrome: the save conflict
lands above the file's breadcrumb, and the claude-here prompt above the
terminal's tab bar. Each pushes its pane's entire contents down and reads as
belonging to nothing. Under the chrome, the pane keeps its identity and the
message visibly belongs to it.

The region is capped at `max-height: 45%` with `overflow-y: auto`. Sixteen
upload failures must not push the tree off-screen.

**Pane-less failures float, as a real column.** A delete or rename failure has
no pane to belong to. `#globalmsgs` is a fixed flex column at
`top: calc(var(--header-h) + 6px)`, which fixes the header collision by
construction rather than by tuning an offset.

## Severity is a 2px left rule

Not a full outline. `.pane` already draws a border, and a second rectangle
inside it is what made `.conflict` read as detached furniture floating in a
pane rather than as part of it.

```
.msg.m-err  { border-left-color: var(--del-fg); }
.msg.m-warn { border-left-color: var(--warn); }
.msg.m-ok   { border-left-color: var(--add-fg); }
```

**The kind classes must be namespaced.** A bare `.warn` already exists as a
global utility at `style.css:120` — `color: #d29922; cursor: help`. A first
draft using `.msg.warn` silently inherited both, rendering entire sentences
amber with a help cursor. It was invisible in the source and obvious in a
screenshot.

**No new theme tokens.** `--del-fg`, `--warn`, `--add-fg`, `--accent`, `--bg2`
and `--border` are defined by every shipped theme, so all five and any user
theme get this without edits. Verified by loading `themes/light.css` over the
prototype.

## Typography: things on disk are monospace

One rule governs the whole surface. Prose is in the UI face; the *subject* of a
message — a path, a session name, a branch — is `var(--mono)`, the same
treatment the tree and the breadcrumb already give it. `.msg .subj` carries it.

## Status: progress on the pane's own edge

A `.paneprog` element absolutely positioned on the pane's bottom edge, filling
left to right, plus a small mono `.panestat` pill at the pane's bottom-right.

Both are `position: absolute`, which is the entire point: the measured 32px
squeeze is fixed structurally, not by making the box smaller. `.pane` already
carries a 1px border, so this replaces an edge rather than introducing a shape.

**The label must not go in `.panehead`.** A first draft put it there and it
competed with the tab strip for width: the Files pane's title truncated to "F"
and the strip grew a horizontal scrollbar. A pane is narrow and its head is
already full.

## Questions

Placed in the per-pane region, with `.savebtn`'s quiet treatment — never native
button chrome, which renders as bright white blocks in a dark theme and becomes
the loudest thing on screen.

**Neither choice is accented**, for the reason `style.css:366` already gives
for `.proposal-actions`: when both options destroy something — overwrite
discards the disk's changes, discard-mine discards yours — the affordance must
not lean on the answer.

### The one modal

Closing a project concerns the whole window and has no pane to live in. It is
the only surface that earns a centred modal. Backdrop matches the search
overlay's `rgba(0,0,0,.35)`.

Red marks the destructive choice; it must not recommend it. A first draft gave
it a red outline and it became the most prominent element in the dialog,
reading as the primary call to action — the opposite of what a confirm wants.
Colour on the label, neutral border, and focus lands on the safe choice.

## Replacing the native dialogs

Ten sites. This is the largest part and the one with a real hazard.

| Site | Becomes |
|---|---|
| `app.js:1214` `fileMenu`'s numbered `prompt` | a real context menu |
| `app.js:1221`, `1224` new file / new folder | an inline row in the tree |
| `app.js:1227` rename | the row itself becomes a field |
| `app.js:1231` delete | a question in the Files pane |
| `app.js:2178` close a dirty buffer | a question in that pane |
| `app.js:2180` end a session | a question in that pane |
| `app.js:2258` `alert` — unsaved changes | a notice |
| `app.js:2261` close the project | the modal |
| `app.js:2319` remove a worktree | the modal |

**Every one of these call sites becomes asynchronous.** `prompt()` and
`confirm()` return a value inline; nothing that replaces them can. This is the
bulk of the work and the reason part 2 ships separately.

### The hazard: `reconcileList` deletes a half-typed filename

`reconcileList` (`app.js:1107`) does `ul.innerHTML = ""` and then re-appends
only the `<li>`s present in the server's fresh listing, matched by
`treeItemId`. Consequently:

- **Rename survives.** The row still exists on disk under its old name, so it
  matches, and the *same `<li>` node* is reused — an `<input>` inside it comes
  along untouched.
- **Create does not.** A half-typed new filename is in no fresh listing, so it
  is silently dropped.

And per the comment at `app.js:1125`, `TreeChanged` fires on *every filesystem
write* — "including every file Claude edits from a terminal pane, which is
resh's core use case." So a Claude working in a pane erases your half-typed
filename, at a random moment, with no error.

**Requirement:** a pending row carries an explicit marker that `reconcileList`
preserves and re-inserts at its sorted position. `reconcileList` was written to
preserve *server-derived* identity (`data-rel`, `open`, `sel`); a pending row
has no server identity yet, so it must be preserved on a different basis.

This is named here rather than left to be discovered during implementation,
because its failure mode is intermittent, silent, and looks like the user's own
mistake.

**Settled 2026-08-31, by probing the running app** (real CDP mouse events, not
`element.click()`, since focus is the thing under test):

| Arrangement | Field focuses | Typing lands | Intents the click sends |
|---|---|---|---|
| `<input>` inside `<a class="file" data-rel>` | yes | yes | `["OpenTab:hello.md"]` |
| `<input>` replacing the anchor | yes | yes | none |

So the editing row **replaces** the anchor for the duration of the edit, and
restores it afterwards.

The reason is not the one that was expected. Clicks are *not* swallowed —
`wireFileLinks` calls `preventDefault` on the *click*, and focus has already
happened on the earlier mousedown, so the field works perfectly well inside the
anchor. What it cannot do is stop the anchor's handler also firing: every click
into the field to reposition the caret would additionally open that file in
pane 2. Reaching for `stopPropagation` on the input would work, but leaves a
live `OpenTab` handler one stray event away from the field it is wrapping;
removing the anchor removes the question.

Recording the wrong hypothesis on purpose: "the handler swallows the click" is
the intuitive read, it is wrong, and a plan written around it would have
produced a different structure for a reason that does not exist.

## Testing

Rust tests cannot reach `static/app.js`, and every surface in this spec lives
there — so the load-bearing tests are browser tests, and each must be checked
by reverting the fix and watching it fail.

**Assertions that would pass vacuously**, called out because they are the
obvious ones to write:

- *"Three notices exist in the DOM."* True today. The defect is that they
  overlap. Assert **disjoint bounding boxes**, or `elementFromPoint` at each
  notice's own corner returning that notice.
- *"The header buttons are not covered."* A centre-point hit test already
  passes today, by one pixel. Assert **box intersection** between the notice
  region and the header band.
- *"An upload shows progress."* True today. Assert the **pane heights are
  unchanged** while it runs — that is the actual fix.
- *"A question renders in the pane."* True today. Assert it sits **below**
  `.panehead` in document order, which is what changes.
- *"A pending tree row exists."* Assert it **survives a `TreeChanged`** — fire
  a real filesystem write and confirm the half-typed name is still there. This
  is the only assertion that covers the hazard above.

Contrast must be measured across all five themes the way the search overlay's
selection colour was, by rasterising through a canvas: `getComputedStyle`
returns an unresolved `oklab(...)` for a `color-mix` and cannot be
luminance-parsed.

## What could go wrong

**The per-pane region changes pane layout for every pane.** `.pane` is a flex
column and the region is a new child. A pane with no messages must be
byte-identical to today — `.msgs:empty { display: none }` — or every pane gains
a phantom gap.

**`position: relative` on `.pane`.** `.paneprog` needs it. Any existing
absolutely-positioned descendant currently resolving against a further ancestor
would re-anchor. `style.css:240` and `:252` both position against something;
they must be checked.

**A question that outlives its pane.** If the pane closes while a question is
open, the question goes with it — correct, but the intent it was going to send
never happens and nothing says so. The three-way rule applies: that is "the
user did not answer", not "the user said no".

**The modal and the terminal.** A modal takes focus; every terminal pane is an
xterm that wants keystrokes. Focus must be restored to whatever had it when the
modal closes, or answering a confirm silently detaches the keyboard from the
shell.

## Prior art in this repo

The prototype for every screenshot in the design review was produced by
injecting the proposed stylesheet into the *running* app rather than building a
mockup, so the CSS reviewed is the CSS that ships. That approach caught both
first-draft bugs recorded above, neither of which was visible in the source.
