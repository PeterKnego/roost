# Touch affordances for the Files pane and the terminal

*2026-09-16. #110. Spec: `docs/superpowers/specs/2026-09-16-touch-affordances-design.md`.*

## Verified in the code first

- `render.rs` renders a directory as
  `<li><details data-rel="sub" …><summary>sub</summary><ul></ul></details></li>`
  and a file as `<li><a class="file" data-rel="sub/x.txt" …>`.
- `wireFileLinks` binds `oncontextmenu` on `a[data-rel]` only. `wireFragment`
  binds a container handler that returns early for `a[data-rel]` and otherwise
  calls `fileMenu(e, "")`.
- `fileMenu(e, rel)` derives `dir` by `rel.slice(0, rel.lastIndexOf("/"))`.
- `uploadFiles(files, dir)` builds `?dir=` and calls `postFiles`; both already
  work for a subdirectory, server included.
- `wireTouchScroll(node, term)` gates everything on `translate()` —
  mouse-reporting on, or the alternate screen. Otherwise touch falls through to
  native scrolling.
- `TERM_KEYS` is `() => string`; the paste button is already branched before
  that table, so a second non-key button has a precedent to follow.

## Task 1 — `fileMenu` learns what it was given

```js
async function fileMenu(e, rel, isDir = false) {
  const dir = isDir ? rel : (rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "");
```

Rename/Delete still key on `rel`, so a folder gets them too — which it should,
and `DeleteFile` already refuses a non-empty directory server-side.

## Task 2 — folders get the menu

In `wireFileLinks`, alongside the `<a>` loop, bind `summary` elements:

```js
root.querySelectorAll("details[data-rel] > summary").forEach((s) => {
  s.oncontextmenu = (e) => {
    e.preventDefault();
    e.stopPropagation();              // or the container handler also fires
    fileMenu(e, s.parentElement.dataset.rel, true);
  };
});
```

`stopPropagation` matters: `wireFragment`'s container handler only skips
`a[data-rel]`, so without it a folder would open two menus.

Lazy `<details>` load their children later, and `wireFileLinks` is already
called on inserted tree fragments — confirm the new binding rides that path.

## Task 3 — "Upload files…" in the menu

A new item, after New folder. Opens a hidden `<input type="file" multiple>`,
and on change calls `uploadFiles(input.files, dir)` — the same call the drop
makes, with the menu's own `dir`.

The input is created per invocation and discarded, rather than living in the
page: a persistent one keeps the last selection, and re-picking the same file
then fires no `change` at all.

## Task 4 — select mode on the terminal bar

An eighth button, `data-k="select"`, `aria-pressed`. Branched before
`TERM_KEYS` like the paste button.

`wireTouchScroll` gains one gate at the top of its three handlers: when the
entry is in select mode, return immediately and do not `preventDefault`, so
xterm sees the touches. Forward `touchstart`/`touchmove`/`touchend` to xterm as
mouse events on the `.xterm-screen` element.

Turns itself off on `onSelectionChange` settling with a non-empty selection
(copy-on-select already runs there) and when the pane switches away.

## Task 5 — `tests/browser/touchfiles.mjs`

1. right-click **nested** folder `a/b` → New file offers `a/b/untitled.txt`
2. right-click file `a/b/f.txt` → offers `a/b/untitled.txt` (unchanged)
3. right-click blank pane → offers `untitled.txt` (unchanged)
4. exactly one menu opens for a folder
5. Upload files… on folder `a/b` → `uploadFiles` called with dir `a/b`
6. select mode: pressed state on, `wireTouchScroll` stands down, off again
   after a selection

## Task 6 — revert-checks

- drop the `isDir` branch → (1) fails, (2) and (3) stay green
- drop `stopPropagation` → (4) fails with two menus
- bind the menu to `details` instead of `summary` → check which of (1)/(4) moves
- select mode that lights up but does not gate `wireTouchScroll` → (6) fails

Each applied, run, failure read, restored, recorded in the test's comment.
