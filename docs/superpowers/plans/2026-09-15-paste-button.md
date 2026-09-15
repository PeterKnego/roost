# A paste button for the phone

*2026-09-15. #97. Spec: `docs/superpowers/specs/2026-09-15-paste-button-design.md`.*

## The state of play, verified in the code

- `#termkeys` is rendered in `render::workspace_page` (`src/render.rs`) as six
  `<button data-k=…>`; `style.css` keeps it `display: none` until
  `body[data-mpane="3"]`, and gives each button `flex: 1; min-height: 44px`.
- `initTermKeys` (`static/app.js`) binds **`pointerdown` + `preventDefault`**
  on every button and calls `entry.term.input(TERM_KEYS[k](term))`. Its comment
  explains the gesture choice: a click focuses the button, and focusing a
  button on iOS dismisses the soft keyboard.
- `targetTerm()` already resolves which terminal a bar press goes to.
- `TERM_KEYS` values are `() => string`. A paste is not that shape — it is
  async and it does not go through `input` — so it is **not** a seventh entry
  in that table.
- The vendored xterm exposes `paste(` (`static/vendor/xterm.js`).
- `dialog.js` has `askText`, whose shell `#dlg-text` holds an `<input
  type="text">`. Single-line, so it is not the fallback; a new shell is needed.

## Task 1 — the button

`render.rs`: a seventh button, last, `data-k="paste"`, labelled `paste`.
Its own render test asserts it is in the bar and after `^C`.

## Task 2 — `pasteInto(entry)` in app.js

Not a `TERM_KEYS` entry — that table is `() => string` and this is neither.
`initTermKeys` gets one branch before the table lookup:

```js
if (b.dataset.k === "paste") { pasteInto(entry); return; }
```

```js
async function pasteInto(entry) {
  let text = null;
  try {
    if (navigator.clipboard && navigator.clipboard.readText) {
      text = await navigator.clipboard.readText();
    }
  } catch { text = null; }          // refused, or no activation
  if (text === null) return askPasteText(entry);
  if (text) entry.term.paste(text);
}
```

`term.paste`, never `term.input` — the spec's own section. An empty string is
*not* the fallback's trigger: an empty clipboard read successfully is an
answer, and reopening a dialog over it would be the codebase's own "absence of
evidence" mistake pointed at a UI.

## Task 3 — the fallback dialog

A new shell `#dlg-paste` beside the others in `render::workspace_page`, holding
a `<textarea>`, and `askPasteText` in `dialog.js` built on the existing
`runDialog`. Focused on open, so a long-press inside it offers the callout iOS
would not offer over the terminal.

Its text says what to do — long-press, Paste, then Send — because a person who
reached it did so because something was refused, and an empty box with no
explanation is the worst moment to be terse.

## Task 4 — `tests/browser/paste.mjs`

Mobile viewport. The pty-byte technique from `shiftenter.mjs`: `stty raw -echo`
and capture into a file, then read it from Deno.

1. bar has seven buttons, each ≥44 px tall and ≥44 px wide at 390 px
2. with `DECSET 2004` on, a pasted `"a\nb"` arrives wrapped in `ESC[200~`/`ESC[201~`
3. with it off, the same paste arrives bare — **both, or neither means anything**
4. with `navigator.clipboard` stubbed to reject, the dialog opens and its
   textarea has focus; sending from it reaches the pty the same way
5. with `readText` stubbed to resolve, the dialog does **not** open

## Task 5 — revert-checks

- `term.input` instead of `term.paste` → (2) must fail on the missing wrapper.
- hard-coded wrapper → (3) must fail; this is the pair that makes (2) mean
  something.
- fallback removed → (4) must fail.
- `text === null` weakened to `!text` → a successful empty read must not open
  the dialog; assert it via (5) with `readText` resolving `""`.

Each applied, run, failure read, restored, and recorded in the test's comment.

## Out of scope

Desktop. ⌘V already works and the capture-phase `paste` listener already leaves
text alone. Nothing in this touches that path.
