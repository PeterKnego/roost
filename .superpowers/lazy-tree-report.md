# Lazy tree rendering — report

## What changed

### `src/render.rs`
- `tree_fragment` no longer walks the whole project recursively. It renders
  one level via a new `pub fn tree_level(project, dir, rel, open, hide, out)`,
  which:
  - expands directories on the currently-open file's `open` path inline,
    recursively (`<details open data-rel="...">`), so the open file is
    visible on first load with no extra round trip;
  - renders every other directory as a closed stub with an empty `<ul>` and
    lazy-fetch attributes: `<details data-rel="{rel}" hx-get="/frag/{project}/tree?dir={rel}" hx-trigger="toggle once" hx-target="find ul">`;
  - applies the 4,000-entry budget **fresh per call** (i.e. per directory
    level), not once for the whole recursive walk. An ordinary large project
    (many modest directories) no longer trips it; one pathological directory
    with thousands of direct children still does, with the same truncation
    hint, now emitted as a trailing `<li class="hint">` inside that level's
    list rather than a sibling `<div>` after the closing `</ul>`.
  - `tree_level` is `pub` and reused directly by the lazy-fetch route (no
    extra wrapper needed): it already produces exactly the `<li>` items a
    parent's `<ul>` needs, with no outer wrapper.
  - Removed `hx-get`/`hx-target="#content"` from file `<a class="file">`
    items. They were already dead (there's no `#content` element in the
    four-pane layout; `wireFragment` in app.js does the real click wiring).
    They became actively hazardous once tree content started going through
    `htmx.process()`: htmx would bind its own real click→ajax handler
    alongside our manual one on the same node, racing it and firing a
    pointless request at a target that doesn't exist.

### `src/routes.rs`
- `["tree"]` arm: optional `dir` query param. `None` → same root render as
  before. `Some(rel)` → `rel` goes through `projects::safe_resolve(&dir,
  rel)` before any read (same pattern as `["file"]`'s `path`) — outside-project,
  nonexistent, and non-directory targets all fall through to
  `render::hint(...)`, never a listing. `open` still works for both cases
  (in practice the lazy `hx-get` doesn't pass `open`, so it's `""` there,
  which is fine — a directory reachable only via manual lazy-expand can
  never itself be on the eagerly-computed open-file path anyway, since that
  path is already pre-expanded on load).

### `static/app.js`
- `TreeChanged` now calls `refreshTree()` instead of `refreshKind("Tree")`.
  `refreshTree()` finds every `details[open][data-rel]` currently in the
  DOM (for whichever pane has Tree active) and re-fetches *only* that
  directory's children via `?dir=`, swapping into its own `<ul>`. The root
  listing and every closed directory are left untouched. This is the fix
  for "editing one file re-downloads/collapses the whole tree."
- `mountTab`'s Tree branch now calls `htmx.process(content)` after the
  initial manual `fetch()` + `innerHTML` load, so the lazy `<details
  hx-get>` stubs actually get bound (content inserted via `innerHTML` is
  invisible to htmx until told).
- Added one global `htmx.on("htmx:afterSwap", e => wireFragment(e.detail.target))`.
  The lazy directory expand is driven by *real* htmx (not the app's manual
  fetch path), so its swapped-in content never runs through `mountTab`'s own
  `wireFragment()` call; without this, file links inside a manually-expanded
  (not open-path-pre-expanded) directory would be unclickable dead anchors.
  This listener is a no-op for the only other thing htmx drives in this app
  (`#gitinfo`'s status span — no `a.file` there).
- `refreshTree()`'s own per-directory fetch also calls `wireFragment(ul)`
  and `htmx.process(ul)` on what it swaps in, since that path is a plain
  `fetch()`, not an htmx-driven swap, so neither happens automatically.

### htmx attribute semantics settled on
- `hx-trigger="toggle once"` on the `<details>` itself: `<details>` fires a
  native `toggle` DOM event on every open/close transition; htmx accepts
  any native event name. Since these stubs always start closed, the first
  `toggle` is necessarily the "user just expanded it" transition, so `once`
  is exactly "fetch children on first expand, never again" — verified the
  `once` trigger modifier exists in the vendored `htmx.min.js` (2.0.4).
  Pre-expanded (open-path) directories carry no `hx-get` at all — their
  content is already inline, so there's nothing to fetch and no `toggle`
  binding is wanted.
- `hx-target="find ul"`: htmx's `find <selector>` target syntax resolves to
  the first descendant of the triggering element matching the selector
  (confirmed `"find "` is a recognized target-selector prefix in the
  vendored build) — i.e. the `<details>`'s own child `<ul>`. Default swap
  style is `innerHTML` (confirmed in htmx's config defaults), matching
  "swap into that `<ul>`" with no extra `hx-swap` needed.
- Confirmed by reading the vendored source that htmx 2.0.4 has **no
  MutationObserver** — it only binds `hx-*` attributes when it walks the
  DOM itself (boot-time `htmx.process(document.body)`, or as part of its
  own ajax swap pipeline). Content dropped in via plain `element.innerHTML
  = html` is genuinely inert to htmx until `htmx.process()` is called on it
  — this directly confirmed the report's own instruction rather than being
  taken on faith.

## Tests added/updated
All in `src/render.rs` `mod tests` (bottom of file) plus two new
`tests/integration.rs` cases:
- `tree_marks_open_path_and_skips_hidden` (updated): asserts `data-rel`
  attributes and drops the now-removed `hx-get` assertion on file links.
- `tree_renders_one_level_and_closed_dirs_omit_children` (new): a
  directory one level under the open path but not itself on it renders as
  a closed lazy stub and its child (`x.rs`) is absent from the response.
- `tree_pre_expands_the_whole_open_path` (new): a 3-deep open path
  (`a/b/c/main.rs`) renders all three ancestor `<details open>` and the
  selected file.
- `tree_level_answers_a_lazy_dir_fetch` (new): calling `tree_level`
  directly the way the `?dir=` route does returns bare `<li>` items (no
  `<ul>` wrapper) containing the target directory's immediate children,
  with its own subdirectory still a closed stub.
- `tree_dir_traversal_is_rejected_with_hint_and_leaks_no_listing` (new,
  integration): `GET /frag/proj/tree?dir=..` (escapes to the tempdir root,
  which holds a `secret.txt` sibling of the project) returns a
  `class="hint"` body containing neither `secret.txt` nor any `<li`.
- `tree_dir_lazily_returns_a_subdirectorys_children` (new, integration):
  root tree shows a `sub` directory closed (`data-rel="sub"`, no
  `inner.txt`); `?dir=sub` returns `inner.txt`.

## Test command and full output
```
$ cargo test
```
Result: **96 passed** (lib unit tests) + **0** (main) + **22 passed**
(`tests/integration.rs`) + **0** (doc-tests). No failures. One
pre-existing, unrelated warning (`unused import: Mode` in
`src/workspace.rs`, present before this change).

Full test names: see `render::tests::tree_marks_open_path_and_skips_hidden`,
`render::tests::tree_renders_one_level_and_closed_dirs_omit_children`,
`render::tests::tree_pre_expands_the_whole_open_path`,
`render::tests::tree_level_answers_a_lazy_dir_fetch`,
`tree_dir_traversal_is_rejected_with_hint_and_leaks_no_listing`,
`tree_dir_lazily_returns_a_subdirectorys_children` — all `ok`.

## Not verified without a browser
- The actual in-browser behavior of `hx-trigger="toggle once"` firing
  exactly once per `<details>` across real user clicks (verified only by
  reading htmx source for `once`/`toggle` support, not by driving a real
  DOM).
- `htmx:afterSwap`'s `e.detail.target` actually being the swapped `<ul>`
  element in practice (read from htmx source/docs convention, not observed
  live).
- The deep-nesting edge case: if a directory *and* a directory nested
  inside it are simultaneously open when `TreeChanged` fires, both get
  independently re-fetched; whichever resolves first replaces its `<ul>`
  wholesale, which discards the nested one's original DOM node before its
  own (now-orphaned) fetch resolves. Net effect: the nested directory
  visually collapses back to closed on that particular refresh (its parent
  stays open, and expanding it again re-fetches fine). This is a
  minor, accepted degradation under the "expansion is ephemeral, no
  protocol change" constraint — not a crash, not silent data loss, just a
  redundant/wasted fetch and a UI reset one level deep. Flagging for
  browser confirmation since it's the one place true multi-level nested
  expansion + concurrent-refetch timing couldn't be exercised by cargo test.
- Root-level listing (top-level files/dirs directly under the project) is
  deliberately **not** refreshed on `TreeChanged` at all (per the explicit
  "do NOT re-fetch the whole root" instruction) — a new top-level
  file/directory won't appear in the tree until the user manually reloads
  or otherwise triggers a fresh mount. Worth confirming this trade-off is
  acceptable in practice.
