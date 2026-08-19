# resh — links and images in markdown preview, and image tabs

Makes a markdown preview's own references work: a link to another file in the
project opens it as a tab instead of navigating the workspace away, and an image
in the project renders instead of showing a broken icon. Adds one GET route
serving image bytes, a resolver shared by both rewrites, and a fragment branch
that makes clicking a `.png` in the tree show the picture rather than an error.
Blocks remote images, which load today.

## The problem this solves

Four symptoms. One missing piece under three of them, and one that is simply a
hole.

**A local link navigates out of the workspace.** This is the worst of the four
and the least obvious. `[the plan](plan.md)` renders as `<a href="plan.md">`,
resolved by the browser against the *page* URL. Clicking it leaves the
single-page workspace: the pane layout, the open buffers, and every attached
terminal view go with it. A README that cross-references its own docs — which is
what READMEs do — is a minefield.

**Markdown images do not render.** Easy to misdiagnose as the renderer stripping
them. It is not. `markdown_html` neutralizes raw HTML to text but leaves
`Event::Start(Tag::Image)` untouched, so `![a cat](cat.png)` really does emit
`<img src="cat.png" alt="a cat" />`. The tag is there. What is missing is
anything to answer it: **no route in this server serves raw project bytes at
all.** `/frag/<proj>/file?path=` returns an HTML fragment, and the project asset
route refuses anything outside a narrow theme allowlist — `routes.rs`'s own test
puts it as "a project may never serve code". So the browser requests something
that yields no image and draws a broken-image icon.

**A `.png` in the tree cannot be opened.** The tree lists every file, but the
`file` fragment reads through `projects::read_text_file`, which sniffs for NUL
bytes and returns `"binary file"` (`projects.rs:323`). The tree offers files it
then refuses to open.

**A remote image loads for real.** Nobody reported this one; it surfaced while
diagnosing the first two. `![x](https://example.com/x.png)` fetches. Opening a
markdown file from a repo you cloned makes your browser issue a request to a
third party that repo's author chose, leaking your IP and the fact and time of
your reading that file.

That last one is out of step with everything around it. The server binds loopback
only, every websocket checks `Origin`, and this very function already refuses to
let repo-authored HTML reach the page. An automatic outbound request on behalf of
repo content is the same category of trust, and it is currently the one hole in
it. It belongs in this change because this is the change that touches image
handling.

## What changes, in one sentence

`markdown_html` learns which file it is rendering, so one resolver can turn every
link and image destination in that file into a project-relative path — which
links emit as a tab-opening anchor and images emit as a new
`/frag/<proj>/raw?path=` route that serves bytes under confinement and a cap.

## One resolver, two emitters

The interesting part of this design is that links and images differ only in what
they *emit*. Where their destination points — remote, absolute, relative to this
file's directory — is one question with one answer, and it must be answered
identically for both or the two will drift.

`markdown_html(md)` becomes `markdown_html(md, project, rel)`: the project to
build URLs with, and the source file's path to resolve against. Its one caller,
`file_fragment`, already has `rel`; `serve_frag` has `project`.

The rewrite happens in the same event-stream position where raw HTML is already
neutralized, so it extends an existing pattern rather than adding a stage. Both
arms consume a single `resolve_dest(dest, from_rel) -> Dest`.

```
enum Dest {
    Remote,                 // has a scheme, or protocol-relative //host
    Data,                   // data: URI
    Local(String),          // project-relative path, normalized
    Other,                  // mailto:, #anchor, anything unrecognised
}
```

`Local` is produced by joining a relative destination to `from_rel`'s directory
(or taking an absolute `/x` as project-root-relative) and normalizing `..`
lexically. That normalization is for *correctness*, not security — every
resulting path is confined again by the server on use, and this spec must not be
read as making the resolver a boundary.

## Link rewriting

| `Dest` | Emitted | Why |
|---|---|---|
| `Local` | `<a class="mdlink" data-rel="…">` | Opens as a tab; see below |
| `Remote` | `<a href="…" target="_blank" rel="noopener noreferrer">` | Kept, but cannot destroy the workspace |
| `Data`, `Other` | Unchanged | `mailto:` and in-page anchors are not ours to rewrite |

A remote link is deliberately **not** blocked the way a remote image is. The two
are different kinds of event: an image issues a request with no user involvement,
while a link requires a deliberate click and shows its destination first. What a
remote link must not do is what it does today — replace the workspace page. So it
gains `target="_blank"`, plus `rel="noopener noreferrer"` so the opened page can
neither reach back through `window.opener` nor learn the workspace URL from a
referer.

### Local links get their behavior from machinery that already exists

The tree emits `<a class="file" data-rel="…">`, and `wireFileLinks` (`app.js:582`)
turns any such anchor into an `OpenTab` intent — plus a right-click file menu.
`wireFragment` already calls it on **every** injected fragment, markdown previews
included (`app.js:434`). So a rewritten link inherits click-to-open and the
context menu with no new client logic.

It cannot simply reuse `class="file"`, though. `.content a.file`
(`style.css:169-195`) styles a *tree row*: block padding scaled by a `--d` depth
variable, a file-type icon in `::before`, indent guides in `::after`, and a
full-width hover band. Applied to a link inside a paragraph it would render an
inline reference as a row of tree furniture.

Hence `class="mdlink"`, styled as an ordinary inline link, and one change to the
selector in `wireFileLinks`:

```
a.file[data-rel]   →   a[data-rel]
```

That widening is a no-op for existing markup, which is worth stating so a
reviewer need not verify it: every `data-rel` anchor rendered today already
carries `class="file"` (`render.rs:169`, `:185`, `:189`). The `data-rel` on tree
`<details>` elements is untouched because the selector still requires an `a`.

## The image route: `/frag/<proj>/raw?path=<rel>`

A new fragment kind rather than a new top-level path segment.

`/img/<proj>/<rel>` reads better and is wrong. Top-level segments in this router
compete with project names, and a project named `img` would shadow the route.
This codebase already pays that price once: `route()` carries an
`rposition("theme")` search (`routes.rs:110`) whose entire job is telling
`.resh/theme/<rel>` apart from a project whose own path contains a `theme`
segment. That complexity is the cost of putting a route where project names live,
and there is no reason to buy it twice.

Hanging the route off `frag` inherits `serve_frag`'s already-correct project
resolution, and `?path=` — the mechanism the `file` and `tree` fragments already
use for untrusted relative paths — makes segment ambiguity structurally
impossible.

`"raw"` must be added to `FRAGMENT_KINDS` (`routes.rs:277`). That list is
hand-synced with `serve_frag`'s match arms and its doc comment already warns that
nothing checks the sync; forgetting it makes `route()` treat `raw` as a
theme-asset path.

The handler is `serve_project_theme` (`routes.rs:385`) with two changes — the
confinement base becomes the project root instead of `.resh/theme`, and the class
check becomes an image-extension allowlist:

1. `assets::normalize(rel)` — rejects absolute paths, `..`, empty segments
2. extension in the allowlist, else 404 — **deny by default**
3. `projects::safe_resolve(dir, rel)` — canonicalize and confine
4. `metadata` first, refuse over `MAX_FILE_BYTES`, and only then read
5. `respond_with(200, content_type(rel), &[NOSNIFF, SANDBOX], &body)`

Step 4 is not an ordering preference. `serve_project_theme`'s own comment spells
out why: a bare `fs::read` allocates the whole file — attacker-controlled size,
from a cloned repo — on the connection thread before any cap could reject it.

`content_type` (`routes.rs:186`) already maps every extension this needs.

### The allowlist, and SVG

`png`, `jpg`, `jpeg`, `gif`, `webp`, `svg`, `ico`.

**One constant, four consumers.** The same list decides whether the raw route
serves a path, whether the `file` fragment renders an image tab, whether
`SetMode { Edit }` is refused, and whether `app.js` draws the ✎ toggle. Three of
those live in Rust and must share one `IMAGE_EXT`; the fourth is a copy in
JavaScript that nothing checks.

That copy is a known hazard of the same shape as `FRAGMENT_KINDS` — and it fails
safe in only one direction. If JS knows about an extension Rust does not, the
toggle is hidden on a file that could have been edited: harmless. If Rust knows
one JS does not, the toggle appears on an image and the server refuses the
intent: recoverable, but confusing. Neither loses data, because the
`workspace.rs` refusal is the actual guard. It is called out here so the plan
puts the JS list next to a comment saying where its twin lives.

SVG earns a sentence because it can carry `<script>`. It is included, for two
reasons already load-bearing in this file: script in an SVG never executes when
the SVG is the source of an `<img>`, and on direct navigation to the URL the
`Content-Security-Policy: sandbox` header — no `allow-scripts` — stops it.
`serve_project_theme` already serves SVG under exactly `NOSNIFF` + `SANDBOX`, so
this follows the established posture rather than inventing one.

Deny-by-default matters here the way `assets::class_of` describes it: an
unrecognised extension must land outside the allowlist, so widening the set is an
edit to the list and never a side effect of someone dropping an unfamiliar file
into a repo.

## Image rewriting

| `Dest` | Emitted |
|---|---|
| `Local` | `<img src="/frag/<proj>/raw?path=…">`, percent-encoded |
| `Data` | Unchanged — self-contained, issues no request |
| `Remote` | **Dropped**, leaving its alt text |
| `Other` | Dropped, leaving its alt text |

### Encoding the path is not a free choice

Both arms must encode through `http::percent_encode`, the helper every other URL
in `render.rs` already uses — not a hand-rolled escape and not a partial one.

This is worth a heading because getting it wrong reproduces the first entry in
CLAUDE.md's defect table verbatim: a decoder turning `+` into a space, so a
project named `gtk+` "did not exist", and its live session was destroyed.
`percent_encode` is a strict allowlist that emits `%2B` for `+`, and
`percent_decode` maps both `%2B` and a bare `+` back — so the pair round-trips a
filename containing `+`, a space, `&`, or `?` only as long as the encoding side
is that function. `/` is deliberately left unescaped, which is correct for a
value in a query string.

A file named `my notes+drafts.png` is not exotic, and it is the case any
hand-rolled encoder gets wrong.

### Dropping an image yields its alt text for free

Filtering out `Event::Start(Tag::Image { .. })` and `Event::End(TagEnd::Image)`
makes the renderer never enter its alt-attribute mode, so the events between them
render as ordinary inline markdown. Verified against pulldown-cmark 0.13 before
this spec was written:

```
input:  text ![a *fancy* cat](https://e.com/b.png) after
output: <p>text a <em>fancy</em> cat after</p>
```

The fallback is therefore not merely an alt string — it is the alt text with its
emphasis intact, flowing inline. No placeholder markup is needed, which is why
none is specified.

Note this is a `filter`, not the `map` the existing HTML neutralization uses:
events are removed, not transformed. The link arm, by contrast, *is* a map.

## Image tabs

Server-side only. In `serve_frag`'s `["file"]` arm, branch on the extension
**before** `read_text_file` is called, and return a fragment holding an `<img>`
pointed at the raw route instead of the usual code or markdown body.

No new fragment kind, no protocol change, no tab-model change: a `File` tab in
`Preview` mode already fetches `/frag/<proj>/file?path=` and injects whatever HTML
comes back (`app.js:427`).

### The one place this could lose data

The ✎ toggle is rendered for every `File` tab (`app.js:241`), and `Edit` mounts a
`<textarea>` seeded from `texts` — which the server cannot populate for a binary
file. An image tab switched to Edit would show an empty editor over a real file,
and saving would truncate the image to nothing.

Closed in **both** places:

- `app.js` omits the toggle when the rel is an image extension.
- `workspace.rs` refuses `Intent::SetMode { mode: Edit }` for an image rel.

The server-side half is not redundant. The client is not a security boundary, and
mirroring means another browser can hold a tab strip rendered before this change
shipped.

## CSP as a backstop, not as the mechanism

The workspace page gains `Content-Security-Policy: img-src 'self' data:`. It
currently sends no CSP at all — `http::html` passes an empty header slice
(`http.rs:131`).

The render-side rewrite stays the *primary* enforcement, because it is precise,
unit-testable in Rust with no browser, and produces real alt text; a CSP-blocked
image leaves a broken-image icon instead. CSP exists to catch what the rewrite
misses — most plausibly a future markdown extension, or an image reaching the page
by a path this design did not anticipate.

Two constraints on the header:

- `data:` is required. The favicon is a data URI (`app.js:1090`); omitting it
  breaks the tab icon.
- Only `img-src` is set. A fuller policy would have to reason about the inline
  script and style this page serves — a much larger change with nothing to do
  with images.

## What this deliberately does not close

**The raw route cannot check `Origin`.** An image that must be embeddable in a
page can never require one. The `Host` check every request already passes
(`routes.rs:42`, the DNS-rebinding guard) still applies.

This adds no new class of exposure, and the reasoning should be recorded so a
reviewer does not read it as an oversight: a remote page can already point an
element at `/frag/<proj>/file?path=` and learn the same existence-and-size facts
from timing and load/error events. It cannot *read* either response — no route
here sends a CORS header, so the bytes stay unreadable cross-origin. Serving the
same project bytes under an image content type does not change that.

Nor is this a new confidentiality boundary. The editor already hands the browser
the contents of any file in the project through the `file` fragment. This route
exposes the same bytes with a different `Content-Type`.

## Caps, and what users will notice

`MAX_FILE_BYTES` (2 MB, `projects.rs:20`) applies unchanged. A screenshot over
2 MB renders as its alt text rather than as a picture.

Worth stating plainly because it *will* be hit — 2 MB is not much for a
full-window PNG — and because the failure is silent from the reader's side. The
cap is kept anyway: it is the cap every other project-controlled read here obeys,
and raising it for one route would let an untrusted repo hand the connection
thread a larger allocation than any other route allows. Revisiting it is a
separate change that should move it everywhere at once.

## Testing

### The trap this feature walks straight into

Path-confinement tests here have failed for the wrong reason before, and CLAUDE.md
names it as the reason a symlink escape once survived review: the test errored
with `ENOENT` before ever reaching the confinement check, and passed green.

So every escape test must **create a real file at the target** outside the project
and assert on the *message*, not on `is_err()`. A test asserting
`../../etc/passwd.png` is refused proves nothing unless something readable exists
at the other end of that path.

### Rust — resolver (`render.rs`)

Tested directly, once, rather than only through the two emitters:

- relative resolves against the source file's directory, not the root —
  `docs/a.md` + `cat.png` → `docs/cat.png`
- `../img/x.png` from `docs/a.md` → `img/x.png`
- absolute `/x.png` → `x.png`
- `https://…`, `//host/x`, `mailto:`, `#anchor`, `data:…` each classify correctly

### Rust — link arm

- a local link emits `data-rel` with the resolved path and **no** `href`
- a local link does **not** carry `class="file"` (it would render as tree
  furniture — assert the absence, since this is invisible in any test that only
  greps for `data-rel`)
- a remote link keeps its `href` and gains both `target="_blank"` and
  `rel="noopener noreferrer"`
- an in-page `#anchor` is untouched

### Rust — image arm

- a local image emits the raw route with the resolved, percent-encoded path
- a remote image emits **no `<img`** and **does** emit its alt text — both
  assertions, since either alone passes for the wrong reason
- a `data:` image survives untouched
- the existing raw-HTML neutralization still holds, guarding against the added
  `filter` dropping the wrong events
- **an image whose filename contains `+` and a space round-trips** — encode the
  rewritten URL, feed it back through `http::percent_decode`, and assert the
  original path comes out. This is CLAUDE.md's defect #1 in miniature, and the
  only test here that would catch an encoder swapped for a plausible-looking one

### Rust — route (`routes.rs`)

- a `.png` inside the project is served, with `image/png`, `nosniff` and `sandbox`
  all asserted
- a `.rs` file is 404 **even though it exists and is readable** — deny-by-default
- a file over the cap is refused with `"asset too large"`
- a symlink inside the project pointing at a real file outside it is refused,
  mirroring `routes.rs:607`
- `..` in `path` is refused

### Rust — workspace

- `SetMode { rel: "a.png", mode: Edit }` leaves the tab in `Preview`

### Browser

Required, not optional: `app.js` and `style.css` both change, and CLAUDE.md
records that no Rust test can reach that file — the dev/prod substitution table
lists "no browser" as having hidden a completely broken save path. A new
`tests/browser/mdlinks.mjs` asserts:

- a project-local image renders with non-zero `naturalWidth`
- a remote image does not render, and its alt text is present
- clicking a link to another `.md` opens it as a tab **and the workspace page did
  not navigate** — assert the URL is unchanged, since that is the actual bug
- clicking a `.png` in the tree shows a picture rather than `"binary file"`
- an image tab has no ✎ control

`naturalWidth` matters: an `<img>` exists in the DOM whether or not its bytes ever
arrived, so asserting the element is present is one of the four traps
`tests/browser/README.md` warns about.

### Verification

Each new test gets the revert-the-fix check CLAUDE.md prescribes — apply the
broken version, run it, read the failure, restore — and the failure mode goes in
the test's own comment. The rewrite tests are the likeliest to pass vacuously: a
rendered `<img` that simply 404s looks identical to a correct one under any
assertion that greps the HTML and stops there.

## Non-goals

- **In-page `#anchor` links.** pulldown-cmark emits no heading ids, so a table of
  contents in a README stays inert. Fixing it means generating slugs, reconciling
  them with ids already on the workspace page, and deciding what a hash does to
  the URL of a single-page app. Its own change. See open questions.
- **Links to directories.** `[src](src/)` resolves to a `File` tab for a
  directory, which the server refuses with the standard hint. Harmless, not
  addressed.
- **Blocking remote links.** Only remote *images* are blocked; see the reasoning
  above.
- **A visible placeholder for a blocked remote image.** The alt text is the
  fallback.
- **Per-project opt-in for remote images.** Considered for badges (shields.io, CI
  status). Dropped: a config key and a second code path, for a case that degrades
  to readable alt text.
- **Raising the 2 MB cap.** It moves everywhere or nowhere.
- **Video, PDF, or other embeds.** The allowlist is images.
- **Editing an image.** Explicitly refused, both ends.

## Open questions for review

1. Is `raw` the right kind name? It matches the GitHub convention, but every
   other fragment kind returns HTML and this one returns bytes. `blob` and
   `bytes` were the alternatives.
2. Heading anchors are the obvious next request once links work at all — a README
   table of contents is the single most common thing a markdown link points at.
   Worth folding in now rather than leaving a half-working link surface?
3. The 2 MB silent-alt-text failure is the weakest point in the experience here.
   Worth a distinguishable message ("image too large to preview") rather than the
   same fallback a blocked remote image gets?
4. Should a local link to a file that does not exist be visually distinct in the
   preview? It is knowable at render time — a dead cross-reference is a common
   documentation defect, and this is the one place resh could surface it — but it
   costs a `symlink_metadata` per link on every preview render.
