# Search Overlay Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a search result readable — one field instead of two, a fixed
left column so the matched text starts on one edge, and a warm chip on every
occurrence of the query.

**Architecture:** Entirely client-side plus markup. `src/search.rs` is not
touched: the server's existing results carry everything needed. Four tasks,
each independently shippable — row layout, then the chip inside it, then the
move of the field into the header, then the note line's treatment.

**Tech Stack:** Rust (`src/render.rs` builds all HTML), plain CSS with
`color-mix(in oklab, …)` tokens, framework-free JS (`static/app.js`), Deno +
Chromium via CDP for browser tests.

**Spec:** `docs/superpowers/specs/2026-08-30-search-overlay-redesign-design.md`

## Global Constraints

- **Never `innerHTML` with dynamic content.** `static/app.js:2950-2954`: a
  matched line and a path are the most attacker-influenced strings this client
  renders; text nodes are the whole defence. Every fragment here is
  `createElement` + `textContent`.
- **`src/search.rs` is not modified.** No walk, cap, or skip-counter behaviour
  may change.
- **All HTML is built in Rust in `render.rs`; escape everything interpolated.**
- **Case folding is ASCII-only**, to match `to_ascii_lowercase` at
  `src/search.rs:183`. `String.prototype.toLowerCase` is forbidden here: it
  folds non-ASCII, and `'İ'.toLowerCase().length === 2`, so an index found in
  the folded string would not map back onto the original.
- **The query used for highlighting is `searchSentQuery`**, not the live input
  — what the server actually answered, the same rule the note line already
  follows.
- **Run `cargo test -- --test-threads=1`**, never `cargo test --release`. A
  bare parallel `cargo test` hangs in this repo.
- **Run browser tests in the foreground:** `deno run -A tests/browser/search.mjs`.
- **Every test is verified by reverting the code it covers and watching it
  fail**, then restoring. Record what the failure said in the test's comment.
- **Line numbers are as of `816e463`.** Each task shifts the ones after it,
  so locate code by the function name, selector, or quoted text given — never
  by scrolling to the number.
- **Build from this checkout only** (`/home/claude/projects/resh`). The shared
  cargo target-dir means a second checkout silently rewrites the asset table.

---

### Task 1: The row becomes a grid with a fixed path column

**Files:**
- Modify: `static/app.js:2955-2969` (`searchRow`), `static/app.js:2982-3028`
  (`renderSearch`'s three category loops)
- Modify: `static/style.css:774-778` (`.searchrow` and its two child rules)
- Test: `tests/browser/search.mjs`

**Interfaces:**
- Produces: `splitPath(rel) -> [dirWithTrailingSlash, basename]`;
  `searchRow(dir, base) -> HTMLDivElement` whose children are
  `.at` (containing `.dir` and `.base`) and an empty `.what`. Task 2 fills
  `.what` through `appendHighlighted`; until then callers set `.textContent`.

- [ ] **Step 1: Write the failing test**

Add to `tests/browser/search.mjs`, before the final `} finally {`:

```js
// --- one left edge, and a path that truncates from the left ----------------
//
// The complaint this answers: a row used to run `path:line` then the text in
// one span, so the matched content began at a different x on every row —
// after `CLAUDE.md:148` on one, after a 60-character spec path on the next.
// There was nothing to run the eye down.
{
  await freshSearch(evalIn, "marker");
  ok(await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow .what").length > 0`), 10, "rows"),
     "setup: rows render with a .what cell");

  const lefts = JSON.parse(await evalIn(`JSON.stringify(
    [...document.querySelectorAll("#searchresults .searchrow .what")]
      .map(n => Math.round(n.getBoundingClientRect().left)))`));
  ok(new Set(lefts).size === 1,
     `every result's text starts at one x (saw ${JSON.stringify([...new Set(lefts)])})`);

  // The long-path case: `.dir` may be clipped, `.base` may not — it carries
  // the filename and the line number, the only parts that identify the hit.
  await freshSearch(evalIn, "deepneedle");
  await until(() => evalIn(`!!document.querySelector("#searchresults .searchrow .base")`), 10, "a deep row");
  const deep = JSON.parse(await evalIn(`(() => {
    const r = document.querySelector("#searchresults .searchrow");
    const dir = r.querySelector(".dir"), base = r.querySelector(".base");
    return JSON.stringify({ dirClipped: dir.scrollWidth > dir.clientWidth,
                            baseWhole: base.scrollWidth <= base.clientWidth + 1,
                            baseText: base.textContent });
  })()`));
  ok(deep.dirClipped, "the directory half of a long path is the part that truncates");
  ok(deep.baseWhole, `the filename and line survive intact (got "${deep.baseText}")`);
}
```

Add this fixture next to the others near the top of the file:

```js
// A deliberately deep path, so the path column must truncate something. The
// filename and `:1` must still be readable afterwards.
await Deno.mkdir(`${fx.roots}/proj/src/very/deeply/nested/directory/tree`, { recursive: true });
await Deno.writeTextFile(
  `${fx.roots}/proj/src/very/deeply/nested/directory/tree/a-long-file-name-here.rs`,
  "let deepneedle = 1;\n",
);
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/search.mjs`
Expected: FAIL — `document.querySelectorAll("… .what")` matches nothing, so
the `until` times out and "setup: rows render with a .what cell" fails.

- [ ] **Step 3: Replace `searchRow` in `static/app.js`**

Replace the whole of `searchRow` (currently `static/app.js:2955-2969`, keeping
its existing doc comment about text nodes) with:

```js
/// Splits `docs/specs/x.md` into ["docs/specs/", "x.md"]. The trailing slash
/// stays on the directory so the two halves concatenate back to the original
/// — the ellipsis lands after it, not instead of it.
function splitPath(rel) {
  const i = rel.lastIndexOf("/");
  return i < 0 ? ["", rel] : [rel.slice(0, i + 1), rel.slice(i + 1)];
}

/// One result row: a path cell on a fixed left column, then the text that
/// matched. Every dynamic part is a text node — a matched line is arbitrary
/// file content and a path is arbitrary filesystem content, which makes these
/// the most attacker-influenced strings this client renders. The innerHTML
/// rule at the top of this file is the whole defence, and it only holds if
/// nothing here interpolates.
///
/// The path is two spans rather than one so a long path ellipsises from the
/// LEFT: `.dir` is allowed to shrink and clip, `.base` is not. CSS
/// `text-overflow` only ever truncates the tail, and the tail of a path is
/// the filename and the line number — the two parts that say which hit this
/// is.
function searchRow(dir, base) {
  const row = document.createElement("div");
  row.className = "searchrow";

  const at = document.createElement("span");
  at.className = "at";
  const d = document.createElement("span");
  d.className = "dir";
  d.textContent = dir;
  const b = document.createElement("span");
  b.className = "base";
  b.textContent = base;
  at.append(d, b);

  const what = document.createElement("span");
  what.className = "what";

  row.append(at, what);
  return row;
}
```

- [ ] **Step 4: Rewrite the three category loops in `renderSearch`**

In `static/app.js`, replace the `files`, `sessions` and `lines` blocks
(currently `:2999-3028`) with:

```js
  // The text cell always holds the thing that matched — the filename for a
  // file hit, the line for a content hit, the name for a session — so the
  // chip Task 2 adds always lands in the same column.
  if (results.files.length) {
    group(`Files (${results.files.length})`);
    for (const f of results.files) {
      const [dir, base] = splitPath(f.rel);
      const row = searchRow(dir, "");
      row.querySelector(".what").textContent = base;
      host.appendChild(row);
      searchRows.push({ kind: "file", rel: f.rel });
    }
  }
  if (results.sessions.length) {
    group(`Sessions (${results.sessions.length})`);
    for (const s of results.sessions) {
      const row = searchRow("terminal", "");
      row.querySelector(".what").textContent = s;
      host.appendChild(row);
      searchRows.push({ kind: "session", session: s });
    }
  }
  if (results.lines.length) {
    group(`Contents (${results.lines.length})`);
    for (const l of results.lines) {
      const [dir, base] = splitPath(l.rel);
      const row = searchRow(dir, `${base}:${l.line}`);
      row.querySelector(".what").textContent = l.text.trim();
      host.appendChild(row);
      searchRows.push({ kind: "line", rel: l.rel, line: l.line });
    }
  }
```

- [ ] **Step 5: Replace the row CSS**

In `static/style.css`, replace lines 774-778 (`.searchrow`, `.searchrow.sel`,
`.searchrow .where`, `.searchrow .line`) with:

```css
/* A fixed left column so every result's text starts at the same x. 22ch fits
   `docs/backlog.md:299` whole; anything longer clips its directory half, which
   is the half that can be lost without losing which hit this is. */
.searchpanel { --search-at: 22ch; }
.searchrow { display: grid; grid-template-columns: var(--search-at) 1fr; gap: 12px;
             align-items: baseline; padding: 3px 12px; cursor: pointer;
             font: 12px/1.6 var(--mono); }
.searchrow.sel { background: var(--row-on); }
.searchrow .at { display: flex; min-width: 0; white-space: nowrap; color: var(--muted); }
.searchrow .at .dir { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.searchrow .at .base { flex: none; }
.searchrow .what { min-width: 0; overflow: hidden; text-overflow: ellipsis;
                   white-space: nowrap; }
```

- [ ] **Step 6: Run the test and watch it pass**

Run: `deno run -A tests/browser/search.mjs`
Expected: ALL PASS, including the two new assertions and every pre-existing
one (the XSS, scroll, and honesty-line sections must be unaffected).

- [ ] **Step 7: Verify the test can fail**

Change `grid-template-columns: var(--search-at) 1fr` to
`grid-template-columns: auto 1fr` — the pre-fix behaviour, a column sized to
its content. Run the test; expect "every result's text starts at one x" to
FAIL with several distinct x values. Restore, re-run, confirm ALL PASS, and
write what the failure said into the test's comment.

- [ ] **Step 8: Commit**

```bash
git add static/app.js static/style.css tests/browser/search.mjs
git commit -m "search: give the results one column to scan down"
```

---

### Task 2: The match chip

**Files:**
- Modify: `static/style.css` (`:root` token block near line 17, and the row rules from Task 1)
- Modify: `static/app.js` (new `appendHighlighted`, called from the three loops)
- Test: `tests/browser/search.mjs`

**Interfaces:**
- Consumes: `searchRow(dir, base)` and its `.what` cell from Task 1.
- Produces: `appendHighlighted(host, text, q)` — appends `text` into `host`,
  each ASCII-case-insensitive occurrence of `q` wrapped in
  `<span class="hit">`. Returns nothing.

- [ ] **Step 1: Write the failing test**

Add to `tests/browser/search.mjs`, after Task 1's section:

```js
// --- the match is visible ---------------------------------------------------
//
// Chips are asserted by rendered colour, not by class name: a `.hit` that
// resolves to the panel's own background is invisible and would still pass a
// class-name test. getComputedStyle returns an unresolved `oklab(...)` for a
// color-mix, so the value is rasterised through a canvas.
{
  await freshSearch(evalIn, "marker");
  await until(() => evalIn(`!!document.querySelector("#searchresults .hit")`), 10, "a chip");

  ok(JSON.parse(await evalIn(`JSON.stringify(
    [...document.querySelectorAll("#searchresults .hit")].every(n => n.textContent.toLowerCase() === "marker"))`)),
     "a chip wraps exactly the matched characters, nothing more");

  // Every occurrence, not just the first. No file in the repo has a line with
  // the same word twice, so the fixture is authored.
  await freshSearch(evalIn, "twice");
  await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "twice rows");
  ok(await evalIn(`document.querySelectorAll("#searchresults .searchrow .what .hit").length >= 2`),
     "both occurrences on one line are chipped, not just the first");

  // The XSS fixture, now going through the splitting path that highlighting
  // introduces — the case the existing section could not reach, because
  // nothing split the string before.
  await freshSearch(evalIn, "onerror");
  await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "xss rows");
  ok(await evalIn(`document.querySelectorAll("#searchresults img").length === 0`),
     "a matched line containing markup is still split into text, not elements");
  ok(await evalIn(`[...document.querySelectorAll("#searchresults .what")].some(n => n.textContent.includes("<img"))`),
     "and the markup is visible as characters");

  // A match past the server's 300-character line cap (src/search.rs:32,
  // applied at :399) arrives as a row whose visible text does not contain the
  // query at all. The row must still render — unadorned, not missing and not
  // an error.
  await freshSearch(evalIn, "farpastthecap");
  await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "a capped row");
  ok(await evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0
                   && document.querySelectorAll("#searchresults .hit").length === 0`),
     "a match beyond the 300-char cap still renders its row, just without a chip");

  // A path that matched only as a subsequence has no contiguous run to wrap.
  // `src/search.rs:139-141` ranks `srch` against `search.rs` this way.
  await freshSearch(evalIn, "srch");
  await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "subsequence rows");
  ok(await evalIn(`document.querySelectorAll("#searchresults .hit").length === 0`),
     "a subsequence path match is left unchipped rather than marked at random");

  // Contrast, on a plain row AND on the selected row, in every theme.
  const chipProbe = `(() => {
    const cx = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
    const srgb = (css) => { cx.clearRect(0,0,1,1); cx.fillStyle = "#000"; cx.fillStyle = css;
      cx.fillRect(0,0,1,1); const d = cx.getImageData(0,0,1,1).data; return [d[0],d[1],d[2]]; };
    const lum = (c) => { const [r,g,b] = c.map(v => { v/=255; return v<=0.04045 ? v/12.92 : Math.pow((v+0.055)/1.055,2.4); });
      return 0.2126*r + 0.7152*g + 0.0722*b; };
    const ratio = (a,b) => (Math.max(a,b)+0.05)/(Math.min(a,b)+0.05);
    const rows = [...document.querySelectorAll("#searchresults .searchrow")];
    const sel = rows.find(r => r.classList.contains("sel"));
    const plain = rows.find(r => !r.classList.contains("sel"));
    const chipOf = (r) => lum(srgb(getComputedStyle(r.querySelector(".hit")).backgroundColor));
    const rowOf = (r) => lum(srgb(getComputedStyle(r).backgroundColor));
    const panel = lum(srgb(getComputedStyle(document.querySelector(".searchpanel")).backgroundColor));
    // A row's own background is transparent until it is selected; fall back
    // to the panel, or the ratio is computed against rgba(0,0,0,0) = black
    // and passes for the wrong reason.
    const bg = (r) => { const l = rowOf(r); return l === 0 && getComputedStyle(r).backgroundColor === "rgba(0, 0, 0, 0)" ? panel : l; };
    return JSON.stringify({
      onPlain: +ratio(chipOf(plain), bg(plain)).toFixed(3),
      onSel:   +ratio(chipOf(sel),   bg(sel)).toFixed(3),
    });
  })()`;

  await freshSearch(evalIn, "marker");
  await until(() => evalIn(`!!document.querySelector("#searchresults .searchrow.sel .hit")`), 10, "a chip on the selected row");
  for (const theme of ["darcula", "dark", "light", "gruvbox", "solarized-dark"]) {
    await evalIn(`(() => { document.querySelector('link[href*="/static/themes/"]').href = "/static/themes/${theme}.css"; return 1; })()`);
    await until(async () => JSON.parse(await evalIn(chipProbe)).onPlain > 1, 10, `${theme} applied`);
    const r = JSON.parse(await evalIn(chipProbe));
    ok(r.onPlain >= 1.15, `${theme}: the chip reads on a plain row (${r.onPlain})`);
    ok(r.onSel >= 1.15, `${theme}: the chip survives on the selected row (${r.onSel})`);
  }
}
```

Add these two fixtures near the top of the file:

```js
// One line, two occurrences of the query. Nothing in the repo has this shape,
// so "chip every match" needs a fixture written for it.
await Deno.writeTextFile(`${fx.roots}/proj/src/twice.txt`, "twice here and twice again\n");
// `srch` matches `search.rs` only as a subsequence — no contiguous run.
await Deno.writeTextFile(`${fx.roots}/proj/src/search.rs`, "nothing to match here\n");
// The match sits past MAX_LINE_CHARS, so the text the server returns is
// truncated before it: the row comes back with nothing to chip.
await Deno.writeTextFile(`${fx.roots}/proj/src/capped.txt`, "x".repeat(320) + "farpastthecap\n");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/search.mjs`
Expected: FAIL — no `.hit` element exists, so the first `until` times out.

- [ ] **Step 3: Add the token and the chip rule to `static/style.css`**

In the `:root` block, after the `--row-on` declaration:

```css
  /* The match chip. Derived from --warn rather than given five hand-picked
     values, because every theme already chose a warm hue there — so a sixth
     theme gets a correct chip for free, and there is no per-theme value to get
     wrong. Translucent on purpose: a selected row is --row-on, and a chip that
     composites is legible on both surfaces by construction, where an opaque
     one would need a second value for the selected case. Same lesson as
     --row-on itself, one step earlier. */
  --hit: color-mix(in oklab, var(--warn) 28%, transparent);
```

And with the row rules:

```css
.searchrow .hit { background: var(--hit); border-radius: 2px; }
```

- [ ] **Step 4: Add `appendHighlighted` to `static/app.js`**

Directly above `searchRow`:

```js
/// ASCII-only lowercase, matching `to_ascii_lowercase` at src/search.rs:183.
/// `String.toLowerCase` is wrong here twice over: it folds beyond ASCII, which
/// the server does not, and it can change a string's length ('İ' folds to two
/// code units) — so an index found in the folded text would not map back onto
/// the original, and the chip would land on the wrong characters.
function lowerAscii(s) { return s.replace(/[A-Z]/g, (c) => c.toLowerCase()); }

/// Appends `text` into `host`, wrapping each occurrence of `q` in a chip.
/// Text nodes and createElement only — never a built-up markup string. The
/// whole reason this function exists is the reason it must not interpolate:
/// `text` is a line out of a file in the project.
///
/// A query that does not occur (a path matched as a subsequence, a match past
/// the server's 300-character line cap) simply appends the text unmarked. That
/// is a row without a chip, not an error and not a missing row.
function appendHighlighted(host, text, q) {
  const needle = lowerAscii(q || "");
  if (!needle) { host.appendChild(document.createTextNode(text)); return; }
  const hay = lowerAscii(text);
  let i = 0;
  for (;;) {
    const at = hay.indexOf(needle, i);
    if (at < 0) break;
    if (at > i) host.appendChild(document.createTextNode(text.slice(i, at)));
    const mark = document.createElement("span");
    mark.className = "hit";
    mark.textContent = text.slice(at, at + needle.length);
    host.appendChild(mark);
    i = at + needle.length;
  }
  host.appendChild(document.createTextNode(text.slice(i)));
}
```

- [ ] **Step 5: Call it from the three loops**

In `renderSearch`, replace each of the three `.textContent =` assignments from
Task 1 with a call. `searchSentQuery` — what the server answered — not the
live input:

```js
      appendHighlighted(row.querySelector(".what"), base, searchSentQuery);
```
```js
      appendHighlighted(row.querySelector(".what"), s, searchSentQuery);
```
```js
      appendHighlighted(row.querySelector(".what"), l.text.trim(), searchSentQuery);
```

- [ ] **Step 6: Run the test and watch it pass**

Run: `deno run -A tests/browser/search.mjs`
Expected: ALL PASS — 4 chip-behaviour assertions plus 10 contrast assertions
(2 per theme × 5 themes).

- [ ] **Step 7: Verify the test can fail**

Two reverts, both run:
1. Change `--hit` to `transparent`. Expect every `onPlain`/`onSel` assertion to
   FAIL at ratio 1.000 — this is the check that the test reads colour rather
   than class names.
2. Change `mark.className = "hit"` to `mark.className = "hit"` with the loop
   `break`ing after the first match. Expect "both occurrences on one line are
   chipped" to FAIL.

Restore both, re-run, confirm ALL PASS, and record both failures in the
section's comment.

- [ ] **Step 8: Commit**

```bash
git add static/app.js static/style.css tests/browser/search.mjs
git commit -m "search: mark every occurrence of the query in its own results"
```

---

### Task 3: One field, in the header, with the panel hanging from it

**Files:**
- Modify: `src/render.rs:1150` (the `#searchbox` button), `src/render.rs:1170-1175` (the overlay)
- Modify: `src/render.rs:1949-1964` (the header test) — add one test
- Modify: `static/style.css:76-82` (`#searchbox`), `:771-773` (`#searchoverlay`, `.searchpanel`), `:769-770` (`#searchinput`)
- Modify: `static/app.js:2851-2892` (`openSearch`, `closeSearch`, the two `#searchbox` handlers), `:2893-2913` (the input handler), `:2925-2932` (the keydown handler)
- Test: `tests/browser/search.mjs`, `cargo test`

**Interfaces:**
- Consumes: nothing from Tasks 1-2 beyond the rows they render.
- Produces: `showSearchPanel()` / `hideSearchPanel()` (panel visibility only,
  no focus effects); `openSearch(returnFocus)` (focus the field, remembering
  where focus was); `closeSearch()` (hide + restore focus). `body.searching`
  is set while the panel is open.

- [ ] **Step 1: Write the failing Rust test**

Add to `src/render.rs`'s test module, after
`the_header_advertises_what_search_actually_does`:

```rust
    /// The header used to carry a <button> dressed as a text field while the
    /// real field lived in the overlay — two things that look like one
    /// control, and only the second accepts typing. There is now exactly one,
    /// and it is the one you can see when the overlay is closed.
    #[test]
    fn the_search_field_exists_once_and_lives_in_the_header() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", "proj", &s, Some("theme.css"), false, &[]);
        assert_eq!(
            h.matches("id=\"searchinput\"").count(),
            1,
            "exactly one search field in the page: {h}"
        );
        let head = h.find("<header>").expect("a header");
        let tail = h.find("</header>").expect("a closed header");
        assert!(
            h[head..tail].contains("id=\"searchinput\""),
            "the field must be in the header: {}",
            &h[head..tail]
        );
        let ov = h.find("id=\"searchoverlay\"").expect("the overlay");
        assert!(
            !h[ov..].contains("<input"),
            "the overlay must not carry a second field: {}",
            &h[ov..]
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- --test-threads=1 the_search_field_exists_once`
Expected: FAIL on the last assertion — the overlay still contains
`<input id="searchinput" …>`.

- [ ] **Step 3: Change the markup in `src/render.rs`**

Replace line 1150 with (note `#searchbox` keeps its id, so the existing
assertion at `:1921` still holds — it is now a wrapper, not a control):

```rust
  <div id="searchbox" title="search this project (ctrl-shift-F or ⌘⇧F)">{SVG_SEARCH}<input id="searchinput" type="text" autocomplete="off" spellcheck="false" placeholder="Search files, contents, sessions"><kbd>⇧⌃F</kbd></div>
```

Replace the overlay block at `:1170-1175` with:

```rust
<div id="searchoverlay" hidden>
  <div class="searchpanel">
    <div id="searchresults"></div>
    <div id="searchnote"></div>
  </div>
</div>
```

- [ ] **Step 4: Run the Rust tests**

Run: `cargo test -- --test-threads=1`
Expected: PASS, including `the_header_advertises_what_search_actually_does`
(the hint string moved from `.hintline` into the input's `placeholder`, and
that test asserts on the string, not the element).

- [ ] **Step 5: Restyle the field and anchor the panel**

In `static/style.css`, replace `#searchbox`'s rules at `:76-82` with:

```css
#searchbox { display: flex; align-items: center; gap: 8px; width: 400px; height: 26px;
             margin-left: auto; margin-right: auto; padding: 0 8px 0 9px; border-radius: 6px;
             border: 1px solid var(--border); background: var(--tool); color: var(--muted); }
#searchbox:focus-within { border-color: var(--accent); }
#searchinput { flex: 1 1 auto; min-width: 0; border: none; background: none;
               color: var(--fg); font: inherit; outline: none; }
#searchinput::placeholder { color: var(--muted); }
#searchbox kbd { border: 1px solid var(--border); border-radius: 3px; padding: 0 4px;
                 font: 11px/16px var(--mono); }
/* The shortcut is a hint for finding the field, not a label for a field you
   are already in. */
#searchbox:focus-within kbd { display: none; }
```

Replace the four rules `#searchoverlay`, `#searchoverlay[hidden]`,
`.searchpanel` and `#searchinput` (the last is the overlay's own field rule and
goes away entirely — the field is in the header now) with:

```css
#searchoverlay { position: fixed; inset: 0; z-index: 40; background: rgba(0,0,0,.35); }
#searchoverlay[hidden] { display: none; }
/* Hangs from the header, centred on the bar, and wider than it: a 400px
   column cannot hold a path and a line of code side by side. */
.searchpanel { position: absolute; top: var(--header-h, 38px); left: 50%;
               transform: translateX(-50%); width: min(880px, 92vw); max-height: 60vh;
               display: flex; flex-direction: column;
               background: var(--bg2); border: 1px solid var(--border);
               border-radius: 8px; overflow: hidden; }
/* The field is in the header, and the backdrop is z-index 40 — without this
   the user types into a dimmed control. Raising the header is what makes the
   panel read as hanging from the bar rather than floating near it. */
body.searching header { position: relative; z-index: 41; }
```

- [ ] **Step 6: Rework the JS in `static/app.js`**

Replace `openSearch` and `closeSearch` (`:2851-2884`) with:

```js
/// Panel visibility only — no focus effects. Separate from openSearch because
/// the field now lives in the header and is focusable on its own: a user can
/// have focus in it with no panel showing, and emptying the box must close the
/// panel without yanking focus away mid-edit.
function showSearchPanel() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || !ov.hidden) return;
  ov.hidden = false;
  document.body.classList.add("searching");
}

function hideSearchPanel() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || ov.hidden) return;
  ov.hidden = true;
  document.body.classList.remove("searching");
  searchRows = [];
  // Dismissing mid-debounce must not let the pending Search still fire: its
  // reply would repopulate searchRows and the (now hidden) result list from a
  // query the user no longer has open.
  clearTimeout(searchDebounce);
  searchSeq++;
}

/// The chord and a click both land here. The query is deliberately NOT
/// cleared — it is selected instead, so typing replaces it but refining after
/// a miss does not mean retyping.
function openSearch(returnFocus) {
  const input = document.getElementById("searchinput");
  if (!input) return;
  // Guarded: pressing the chord while already in the field must not remember
  // the field itself as the place to give focus back to, which would strand
  // focus here forever.
  if (document.activeElement !== input) {
    searchReturnFocus = returnFocus !== undefined ? returnFocus : document.activeElement;
  }
  input.focus();
  input.select();
  if (input.value) showSearchPanel();
}

function closeSearch() {
  hideSearchPanel();
  // .focus() on an element no longer in the document does not throw — it
  // silently no-ops and focus falls to <body>. A State broadcast can detach
  // the remembered terminal node while the overlay is open (a tabstrip
  // re-render), so this has to be checked explicitly rather than trusted to
  // fail loudly.
  if (searchReturnFocus && document.contains(searchReturnFocus)) searchReturnFocus.focus();
  searchReturnFocus = null;
}
```

Delete the two `#searchbox` handlers and the `searchboxMousedownFocus`
variable they use (`:2886-2892`, and its `let` declaration), replacing them
with:

```js
// `focusin` carries relatedTarget: the element that just lost focus, which is
// exactly what closing must give back. The <button> needed a mousedown handler
// to capture this before it stole focus for itself; a real input receives
// focus directly, so that bookkeeping goes away.
document.getElementById("searchinput")?.addEventListener("focusin", (e) => {
  if (e.relatedTarget && e.relatedTarget !== e.target) searchReturnFocus = e.relatedTarget;
  if (e.target.value) showSearchPanel();
});
```

In the input handler (`:2893-2913`), the two places that render an empty state
must also close the panel. Replace:

```js
    if (!q) { searchSeq++; renderSearch(null); return; }
```

with:

```js
    if (!q) { searchSeq++; renderSearch(null); hideSearchPanel(); return; }
```

and add `showSearchPanel();` immediately before the `send({ t: "Search", … })`
line.

Replace the keydown handler's guard (`:2925-2932`) with:

```js
document.addEventListener("keydown", (e) => {
  const ov = document.getElementById("searchoverlay");
  const input = document.getElementById("searchinput");
  const open = ov && !ov.hidden;
  // Escape must also work with focus in the field and no panel yet — that is
  // now a reachable state, where before the field could not exist without the
  // overlay around it.
  const inField = input && document.activeElement === input;
  if (!open && !inField) return;
  if (e.key === "Escape") { e.preventDefault(); closeSearch(); return; }
  if (!open) return;
  if (e.key === "ArrowDown") { e.preventDefault(); moveSearchSel(1); return; }
  if (e.key === "ArrowUp") { e.preventDefault(); moveSearchSel(-1); return; }
  if (e.key === "Enter") { e.preventDefault(); activateSearchRow(searchSel); }
});
```

- [ ] **Step 7: Write the browser test**

Add to `tests/browser/search.mjs`:

```js
// --- one field, and it is not dimmed ----------------------------------------
{
  ok(await evalIn(`document.querySelectorAll("input").length === 1`),
     "the page has exactly one input");
  ok(await evalIn(`!!document.querySelector("header #searchinput")`),
     "and it is in the header");

  await evalIn(`closeSearch()`);
  await evalIn(`(() => { const i = document.getElementById("searchinput");
    i.focus(); i.value = "marker"; i.dispatchEvent(new Event("input",{bubbles:true})); return 1; })()`);
  ok(await until(() => evalIn(`!document.getElementById("searchoverlay").hidden`), 10, "panel opens"),
     "typing in the header field opens the panel");

  // The assertion that catches the stacking bug behaviourally: with the
  // backdrop above the header, the point at the centre of the field belongs to
  // #searchoverlay and the user types into a dimmed control.
  ok(await evalIn(`(() => {
      const r = document.getElementById("searchinput").getBoundingClientRect();
      const el = document.elementFromPoint(r.left + r.width/2, r.top + r.height/2);
      return el && el.id === "searchinput";
    })()`),
     "the field is above the backdrop, not behind it");

  ok(await evalIn(`(() => {
      const p = document.querySelector(".searchpanel").getBoundingClientRect();
      return p.top >= document.querySelector("header").getBoundingClientRect().bottom - 1;
    })()`),
     "the panel hangs below the header rather than over it");

  await evalIn(`(() => { const i = document.getElementById("searchinput");
    i.value = ""; i.dispatchEvent(new Event("input",{bubbles:true})); return 1; })()`);
  ok(await until(() => evalIn(`document.getElementById("searchoverlay").hidden`), 10, "panel closes"),
     "emptying the field closes the panel");
}
```

- [ ] **Step 8: Run both suites**

Run: `cargo test -- --test-threads=1`
Expected: PASS.

Run: `deno run -A tests/browser/search.mjs`
Expected: ALL PASS, including the pre-existing Escape-returns-focus section —
that one is the regression risk in this task, since focus now starts outside
the overlay.

- [ ] **Step 9: Verify the tests can fail**

Delete the `body.searching header { position: relative; z-index: 41; }` rule
and re-run. Expect "the field is above the backdrop, not behind it" to FAIL
with the element at that point being `searchoverlay`. Restore, re-run, confirm
ALL PASS, and record the failure in the test's comment.

- [ ] **Step 10: Commit**

```bash
git add src/render.rs static/app.js static/style.css tests/browser/search.mjs
git commit -m "search: one field, in the header, with the results hanging from it"
```

---

### Task 4: The note line becomes the panel's footer

**Files:**
- Modify: `static/style.css` (`#searchnote` rules)
- Modify: `static/app.js` (end of `renderSearch`, where `parts` is joined)
- Test: `tests/browser/search.mjs`

**Interfaces:**
- Consumes: the `parts` array `renderSearch` already composes
  (`static/app.js:3030-3060`).
- Produces: `#searchnote` carries class `skipped` when the note reports
  anything other than a plain "no matches".

- [ ] **Step 1: Write the failing test**

Add to `tests/browser/search.mjs`, inside the existing unreadable-directory
section (after the `1 place could not be read` assertion, where a chmod-000
directory is already in place):

```js
      // "I could not look there" is the one thing this line exists to say, and
      // it used to say it in the same grey as everything else. A search that
      // found nothing is an answer, not a gap, and must NOT wear the mark.
      ok(await evalIn(`document.getElementById("searchnote").classList.contains("skipped")`),
         "a note reporting an unreadable place is marked");
      ok(await evalIn(`parseFloat(getComputedStyle(document.getElementById("searchnote")).borderLeftWidth) > 0`),
         "and the mark is a rendered edge, not just a class");

      await freshSearch(evalIn, "zzzzznotfoundzzzzz");
      await until(() => evalIn(`document.getElementById("searchnote").textContent.includes("no matches")`), 10, "no-matches note");
      ok(await evalIn(`!document.getElementById("searchnote").classList.contains("skipped")`),
         "a clean 'no matches' is not marked — it is an answer, not a gap");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/search.mjs`
Expected: FAIL — `classList.contains("skipped")` is false; nothing sets it.

- [ ] **Step 3: Set the class in `static/app.js`**

At the end of `renderSearch`, where `parts` is joined into `note.textContent`,
add immediately after that assignment:

```js
  // The mark means "something is missing from this answer", so the one note
  // that is a complete answer — nothing found, nothing skipped, nothing
  // failed — must not carry it.
  const gap = parts.length > 0 && !(parts.length === 1 && parts[0] === "no matches");
  note.classList.toggle("skipped", gap);
```

- [ ] **Step 4: Style the footer in `static/style.css`**

Replace the `#searchnote` rules with:

```css
#searchnote { padding: 6px 12px; font-size: 11px; color: var(--muted);
              border-top: 1px solid var(--border); }
#searchnote:empty { display: none; }
/* Same hue as the match chip, deliberately: inside this panel, warm means the
   search is telling you something — whether that is "your word is here" or "I
   could not look there". */
#searchnote.skipped { border-left: 2px solid var(--warn); padding-left: 10px; }
```

- [ ] **Step 5: Run the test and watch it pass**

Run: `deno run -A tests/browser/search.mjs`
Expected: ALL PASS. Note the section is skipped when running as root (chmod
000 does not block a root read) — if the run prints `SKIP … running as root`,
these assertions did not execute and the task is **not** verified.

- [ ] **Step 6: Verify the test can fail**

Change `const gap = …` to `const gap = false`. Re-run; expect "a note
reporting an unreadable place is marked" to FAIL. Then change it to
`const gap = parts.length > 0` and re-run; expect "a clean 'no matches' is not
marked" to FAIL — this second revert is what proves the test distinguishes a
gap from an answer, rather than just checking that some class exists. Restore,
re-run, confirm ALL PASS, and record both in the comment.

- [ ] **Step 7: Full verification and commit**

```bash
cargo test -- --test-threads=1
deno run -A tests/browser/search.mjs
deno run -A tests/browser/reconnect.mjs   # the header markup changed; this drives it
git add static/app.js static/style.css tests/browser/search.mjs
git commit -m "search: the honesty line reads like the panel's own footer"
```

---

## Deploying

CSS and JS are `include_bytes!`d into the binary (`build.rs:20`), so nothing
here reaches the running service until it is rebuilt and reinstalled. The unit
runs `~/.local/bin/resh`, not `target/release/resh`:

```bash
cargo build --release
install -m 755 /home/claude/.cache/cargo-target/release/resh ~/.local/bin/resh
systemctl --user restart resh
# confirm the running binary is the new one, not just the built one
sha256sum ~/.local/bin/resh
systemctl --user show resh -p MainPID --value | xargs -I{} sha256sum /proc/{}/exe
# and confirm the stylesheet actually turned over
curl -s http://127.0.0.1:8444/static/style.css | grep -- '--hit'
```
