# The search overlay: one field, a scannable column, a visible match

`2026-08-29-project-search-design.md` built the search. This spec is about
reading its results.

Three complaints, from using it:

1. A query's matches are invisible. `renderSearch` puts a whole matched line
   into one text node (`static/app.js:3020`), so a search for `first` returns
   fifty lines in uniform grey and the eye has to re-find the word it just
   typed, once per row.
2. There are two search fields. `#searchbox` in the header
   (`src/render.rs:1150`) is a `<button>` dressed as a 400px text field; it
   accepts no typing and exists only to open `#searchoverlay`, which has a
   *real* field of its own (`src/render.rs:1172`). The first one is a picture
   of the second.
3. The result list has no column to scan. A row is `path:line` then the text,
   both in one run, so the matched content starts at a different x on every
   row — anywhere from after `CLAUDE.md:148` to after
   `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md:110`.

## What changes, in one sentence

The header's fake field becomes the only field, the results panel drops beneath
it as an anchored overlay wider than the bar, each row lays its path into a
fixed left column so the text starts on one edge, and every occurrence of the
query inside a result wears a translucent warm chip derived from the theme's
own `--warn`.

## Scope

**In:** the header input, the anchored results panel, row layout, match
highlighting, and the note line's visual treatment.

**Out:**

- **Category tabs** (IDEA's All / Classes / Files / Symbols / Actions / Text).
  resh has three categories and they all fit in one list. Tabs would add a
  filter mode, its own keyboard handling, and state to persist, for no gain
  until there are more contributors than fit on screen.
- **Any change to `src/search.rs`.** The server's results are sufficient for
  everything here. This is deliberate: it keeps the whole change inside
  `render.rs`, `style.css` and `app.js`, and it means no walk, cap, or
  skip-counter behaviour is at risk.
- **Fuzzy matching, ranking changes, symbols, replace.** Unchanged from the
  original spec, and out for the same reasons.

## The one thing this must not break

`static/app.js:2950-2954` carries the rule that makes this feature safe:

> Every dynamic part is a text node: a matched line is arbitrary file content
> and a path is arbitrary filesystem content, which makes these the most
> attacker-influenced strings this client renders. The innerHTML rule at the
> top of this file (constant markup only) is the whole defence, and it only
> holds if nothing here interpolates.

Highlighting is precisely the change that tempts an author to build
`before + "<mark>" + hit + "</mark>" + after` and assign it to `innerHTML`.
That would hand every file in the project a script injection into the client of
whoever searches it. The implementation splits into fragments and appends
`createElement("span")` with `textContent` per fragment; nothing here builds a
string of markup. `tests/browser/search.mjs` already has a fixture whose file
name and matched line are both `<img src=x onerror=1>` — the test extends to
assert those still render as characters *after* being split for highlighting,
which is the case the current test cannot reach because nothing splits them
today.

## Structure: one field

`#searchbox` becomes `<input id="searchinput">`, in the header, 400px, keeping
its magnifier and its `<kbd>⇧⌃F</kbd>`. `#searchoverlay` keeps the backdrop and
the panel but loses `<input id="searchinput">`; the panel holds `#searchresults`
and `#searchnote` only.

The panel is `min(880px, 92vw)` — wider than the 400px bar, centred on it,
anchored at `top: var(--header-h, 38px)`. `--header-h` is set from
`header.offsetHeight` at runtime (`static/app.js:605`); the CSS fallbacks
elsewhere in the file disagree with each other (40px at `style.css:90`, 36px at
`:559` and `:630`, 38px at `:707`) and the header is 38px (`style.css:41`), so
new code uses 38px as its fallback and the disagreement is left alone rather
than swept into this change.

### The stacking problem

`#searchoverlay` is `position: fixed; inset: 0; z-index: 40` with a
`rgba(0,0,0,.35)` backdrop (`static/style.css:771`). `header` has no
positioning and no z-index, so with the input in the header the backdrop would
dim the field the user is typing into.

While a search is open, `body` carries `.searching` and `header` takes
`position: relative; z-index: 41`. The workspace dims, the header stays lit,
and the panel appears to hang from the bar. That the header is *above* the
backdrop is what makes the anchoring legible rather than accidental.

### Focus and dismissal

Today `openSearch` clears the field, focuses it, and remembers what to give
focus back to; `closeSearch` restores it, guarding against a node the DOM has
since detached (`static/app.js:2851-2884`). All of that survives — the field
simply lives somewhere else.

Two changes:

- **The query persists.** `openSearch` no longer clears the input; refocusing
  selects all, so typing replaces and ⌘A-then-edit refines. Reopening after a
  miss should not mean retyping.
- **The field is always in the tab order**, because it is now a real input in
  the header. Focusing it (by Tab, click, or chord) with a non-empty value
  opens the panel; emptying it closes the panel but keeps focus. Escape closes
  the panel and returns focus to where it came from, as now.

The document-level keydown handler stays document-level and stays gated on the
overlay being open. `static/app.js:2915-2924` records why: scoped to the
overlay, focus landing on `<body>` took Escape, arrows and Enter with it and
stranded the modal open. That failure is *more* likely now, not less, since
focus starts outside the overlay by construction.

## Row anatomy: a column to scan

Each row is a grid of two cells on one baseline, both 12px `--mono`:

```
CONTENTS  50
  CLAUDE.md:148         Implementation ▮first▮, `#[cfg(test)] mod tests`…
  CLAUDE.md:245         ▮first▮ and get it reviewed, then the plan, then…
  docs/notificat…md:19  be `BEL` (`\007`) or `ESC \`; in the `777` form…
  docs/backlog.md:20    ## ▮First▮ things to do:
                        ↑ text starts here on every row
```

The path cell is fixed-width and `--muted`; the text cell takes the rest and
ellipsises at its end.

**A long path ellipsises from the left, without JavaScript.** `text-overflow:
ellipsis` only ever truncates the tail, and the tail of a path is the filename
and the line number — the two parts that identify the hit. So the path is
emitted as two spans: a `.dir` that shrinks (`overflow: hidden; text-overflow:
ellipsis; min-width: 0`) and a `.base` that does not (`flex: none`), giving
`docs/superpower…/2026-08-16-deadlight-v2-design.md:110` rather than
`docs/superpowers/specs/2026-08-16-dea…`.

File rows and session rows use the same grid, so all three categories share one
left edge: filename in the text cell, directory in the path cell for files;
session name in the text cell, the word `terminal` in the path cell for
sessions.

## The match chip

```css
--hit: color-mix(in oklab, var(--warn) 28%, transparent);
```

**No new per-theme token.** All five themes in `static/themes/` already define
`--warn` — `#d29922`, `#fabd2f`, `#9a6700`, `#b58900`, `#d6a441` — chosen when
warning colour became a theme's own decision. Deriving the chip from it means a
sixth theme gets a correct chip for free, and the chip is warm in every theme
without five hand-picked values.

**Translucent, not opaque, and that is the load-bearing part.** A selected row
is `--row-on` as of `a9b9e5f`. An opaque chip would need a second value to stay
visible there; a chip that composites is visible on both by construction. This
is the same lesson as `--row-on` itself, applied one step earlier: a colour
drawn on top of another surface has to be specified in terms of that
relationship, not as an absolute.

Verification is the canvas-rasterising technique from `a9b9e5f`, because
`getComputedStyle` hands back an unresolved `oklab(...)` for a `color-mix` and
cannot be luminance-parsed. The chip is measured against **both** the plain row
and the selected row, in **all five themes**.

### Where a chip cannot go, and why that is fine

- **Most path hits.** `src/search.rs:150-168` ranks a path by lowercased
  `contains`, falling back to `is_subsequence` (`:139-141`) — `srch` matches
  `search.rs`. A subsequence has no contiguous run to wrap. Rule: chip a path
  only when a case-insensitive `indexOf` finds a real run; otherwise leave it
  plain. Per-character chips on a subsequence would be noise, and inventing a
  span that is not there would be a lie about why the row matched.
- **A match past 300 characters.** `MAX_LINE_CHARS = 300` (`src/search.rs:32`,
  applied at `:399`) truncates the text the server returns. A hit later in a
  very long line arrives as a row whose visible text does not contain the
  query. The row still renders; it simply has no chip. Rendering nothing, or
  rendering an error, would both be worse than a row that is merely unadorned.
- **Case.** The server lowercases the query once (`src/search.rs:183`) and
  matches with `contains` (`:189` for sessions). The client must fold case the
  same way to find the run, and must chip **every** occurrence in the line, not
  the first. No fixture for this exists yet: `grep -rn -i 'first.*first'
  docs/*.md CLAUDE.md` returns nothing, and `docs/backlog.md:299` — which reads
  like a two-match line in the screenshot that prompted this work — contains
  one. The multi-match fixture has to be authored in `search.mjs`, not found.

## The note line

`CLAUDE.md` makes it a hard constraint that a search which skipped something
says so, and that *could not look*, *chose not to look* and *looked and found
nothing* are three answers rather than two. Today that lands as 11px grey text
under the results, which reads as a disclaimer — the visual weight of a
footnote on the one element that exists to be believed.

It becomes the panel's footer: quiet when there is nothing to report, and
carrying a `--warn`-tinted left edge when there is. Same hue as the match chip,
deliberately: within this panel, warm means *the search is telling you
something*, whether that is "your word is here" or "I could not look there".

No wording changes. The three-way distinction and every counter stay exactly as
`renderSearch` composes them today.

## Testing

Everything here lives in `static/app.js`, `static/style.css` and
`src/render.rs`'s markup, and only the last of those is reachable from
`cargo test`. So:

**`cargo test`** — `render.rs`'s existing assertions at `:1921` and
`:1949-1962` are updated, not deleted: the page must contain exactly one
`<input id="searchinput">`, it must be in the header, `#searchoverlay` must no
longer contain an input, and the hint must still say "files, contents,
sessions".

**`tests/browser/search.mjs`** — extended with:

- The chip's contrast against a plain row and against a selected row, in all
  five themes, rasterised through a canvas.
- Every occurrence chipped, not just the first, on a fixture line containing
  the query twice.
- A matched line containing `<img src=x onerror=1>` still renders as
  characters after being split for highlighting — the existing XSS fixture,
  now exercising the splitting path.
- A path hit that matched only as a subsequence renders with no chip.
- Exactly one `<input>` in the page.
- Escape returns focus to the terminal it came from, with focus starting in
  the header field.

Each of these is written by reverting the behaviour it covers and watching it
fail first, per `CLAUDE.md`. The chip assertions in particular are vulnerable
to the failure this project keeps hitting: a test that reads a token name
rather than a rendered colour would pass with the chip fully invisible.

## What could go wrong

- **The input is in the tab order now.** A user tabbing through the workspace
  can land in the search field and start typing into it by accident. Mitigated
  by the panel only opening on non-empty input, but it is a real behaviour
  change from a `<button>`.
- **`renderSearch` is called on every keystroke's reply.** Splitting each line
  into up to *n* fragments makes each row more DOM than it was. At 50 results
  per category and a 120ms debounce this is not expected to matter; if it does,
  the cap is the lever, not the highlighting.
- **`.searching` on `body` is global state.** Anything else that later wants to
  raise the header above a backdrop will collide with it. It is one class and
  one z-index today; a second consumer is the point at which it needs a name
  that is not about search.
