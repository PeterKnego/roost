# roost — raw HTML in markdown preview: sanitize, don't neutralize

Makes a README written for GitHub look in roost the way it looks on GitHub.
Raw HTML in a markdown file is currently rendered as escaped text; this change
passes an allowlisted subset through, with every `src` and `href` routed
through the resolver that markdown links and images already use, and
everything else still rendered as text.

## The problem this solves

roost's own README no longer renders in roost. It opens with a centred header,
five `<img>` tags with widths, a `<sub>` note under a table and two
`<details>` blocks, all of which GitHub renders and all of which roost's
preview prints as literal `&lt;img src="docs/img/hero.png" ...&gt;`. Reported
from real use on 2026-09-03; confirmed against the live instance's
`/frag/roost/file?path=README.md`.

This is not the image path failing. `markdown_html` turns every
`Event::Html` and `Event::InlineHtml` into `Event::Text` at `render.rs:341`,
on purpose, with the comment "raw HTML from repo content must never reach the
page". The 2026-08-19 preview-links design kept that rule and built the image
and link rewriting *around* it, in the same event-stream position. Markdown
images (`![a](x.png)`) therefore work; HTML images (`<img src="x.png">`) are
text.

The rule exists for a reason that has not changed. A cloned repository's
markdown is untrusted, and the preview renders inside the workspace origin,
the origin that drives every terminal websocket. A `<script>`, an
`<img onerror=…>`, a `<a href="javascript:…">` or a `<meta http-equiv>` in a
README would run as the user. Neutralizing to text closes all of that with
one arm of one match.

But mixing HTML into markdown is standard. CommonMark allows HTML blocks and
inline HTML; GitHub renders them through a sanitizer with a tag and attribute
allowlist; and the three things markdown cannot express, centring, image
width and collapsible sections, are exactly what polished READMEs use HTML
for. A preview that prints tags as text is wrong for the front page of every
project a user opens, starting with this one.

## What changes, in one sentence

`markdown_html` stops turning raw HTML into text and instead runs each HTML
block and inline tag through a new sanitizer, which tokenizes the raw string,
re-emits only allowlisted tags with allowlisted attributes from values it
escaped itself, sends every `src` and `href` through `resolve_dest` exactly
as the markdown arms do, and escapes everything else as text.

## Design

### Output is only what we construct

The sanitizer never copies a byte of input into the output unescaped. It
tokenizes the raw HTML into three kinds of token, tags, text, and everything
else (comments, `<!DOCTYPE`, CDATA, processing instructions, a `<` that opens
no well-formed tag), and:

- **text** is escaped with the same `esc` every other interpolation in
  `render.rs` uses;
- **everything else** is treated as text and escaped, so `<!-- x -->` prints
  as `<!-- x -->`, which is what today's behaviour does for all HTML and is
  the safe default for anything the tokenizer does not understand;
- **a tag** whose name is on the allowlist is rebuilt from scratch, `<name`,
  then each attribute on that tag's allowlist whose value passed its rule,
  then `>`; a tag whose name is not on the allowlist is escaped as text, so
  `<script>alert(1)</script>` prints, visibly, rather than vanishing.

Printing rather than dropping is deliberate and matches the current
behaviour: a README author who wrote something roost refuses can see what
was refused, and a reviewer reading a cloned repo's README in roost sees the
`<script>` the author put there.

### The allowlist

Chosen from what GitHub-style READMEs actually use, starting with this
repository's own, and kept short. Anything not listed can be added later with
a test; nothing here should be removed without one.

| Tag | Attributes | Value rule |
|---|---|---|
| `div`, `p` | `align` | one of `left`, `center`, `right` |
| `img` | `src`, `alt`, `width`, `height`, `align` | `src` per **Destinations** below; `alt` any text; `width`/`height` decimal digits only, at most 4; `align` as above |
| `a` | `href`, `title` | per **Destinations** |
| `details` | `open` | boolean, emitted bare |
| `summary`, `b`, `strong`, `i`, `em`, `sub`, `sup`, `kbd`, `code`, `br` | none | |

Tag names are matched case-insensitively and emitted lowercase, because
`<IMG>` is an `img` to every browser and must not slip past a
case-sensitive list. Attribute names likewise. An attribute not on the tag's
list is dropped silently: `class`, `style`, `id` and every `on*` handler go
this way. `style` in particular is refused rather than filtered, because a
CSS allowlist is a second sanitizer and nothing in scope needs one.

`br` is emitted as `<br>` and takes no closing tag. `<br clear="right">`,
which the old draft README used, loses its attribute and becomes a plain
break; `clear` is not worth a rule.

### Destinations: one resolver, now three emitters

The preview-links design's central rule was that links and images differ
only in what they emit, and both must ask `resolve_dest` where a destination
points. HTML tags are the third emitter and follow the same table, so a
`<img src>` and a `![](src)` naming the same file render the same bytes, and
an `<a href>` opens a tab exactly as `[t](href)` does.

- **`<img src>`**: `Dest::Local(p)` becomes `/frag/<project>/raw?path=<p>`,
  the same URL the markdown arm builds; `Dest::Data` is kept as written;
  `Remote`, `Passthrough` and `Broken` drop the whole tag and emit the `alt`
  text, escaped, in its place, the same fallback the markdown arm gives a
  remote image. The CSP `img-src 'self' data:` backstop remains behind this
  as it does for markdown images.
- **`<a href>`**: the open tag is produced by the existing `link_open`, which
  already yields the tab-opening `mdlink` anchor for local paths, the
  `_blank`/`noopener noreferrer` anchor for `http`/`https`/`mailto`/`tel`,
  and the inert `mdbroken` anchor for every other scheme. `title` is passed
  through to it. Nothing about anchors is designed here; they reuse what the
  markdown arm reuses.
- The README's badge links, `[![CI](https://…/badge.svg)](https://…)`, are
  markdown, not HTML, and keep today's behaviour: remote image dropped to its
  alt text inside a working link.

### Balance

Raw HTML arrives as separate events. pulldown-cmark emits an HTML block as
one `Html` event per line between a `Start(HtmlBlock)` and an
`End(HtmlBlock)`, and each inline tag as its own `InlineHtml` event
(verified against 0.13 on 2026-09-03). Two consequences.

First, a tag whose attributes continue on the next line, the shape
`<img\n  src="…"\n  width="…">` that centred README headers commonly use,
arrives as two events. So the sanitizer does not run per event: for an HTML
block it joins every `Html` string between the block's start and end and
sanitizes the joined text once, and for inline HTML it sanitizes the single
event, which CommonMark guarantees is one complete tag. A tag still
unterminated at the end of a joined block is escaped as text.

Second, an open tag and its close are usually in different blocks (the
`<div>` of a centred header closes twenty lines of markdown later), so the
sanitizer keeps a small stack across the whole document:

- an allowlisted open tag pushes its name;
- a close tag pops if it matches the top, is escaped as text if the stack is
  empty or the top differs (a stray `</div>` prints rather than closes
  something it did not open);
- at the end of the document, every name still on the stack is closed in
  order.

This keeps a README that forgets its `</details>` from hiding the rest of
the preview, and a stray close tag from closing anything roost's own markup
opened. The browser's fragment parser would contain most of this anyway,
since the preview is inserted as a fragment inside `<article>`; the stack
makes it not depend on that.

### Where it sits

Everything is in `render.rs`. The two arms that currently read
`Event::Html(h) => Some(Event::Text(h))` go away, and the work moves to a
pass over the collected event vector, beside `fill_heading_ids`, which
already exists because heading ids need the same look-ahead. The pass
replaces each `Start(HtmlBlock)`…`End(HtmlBlock)` run with one `Event::Html`
of sanitized output, and each `InlineHtml` with an `Event::Html` of its
sanitized form, then appends the closing tags the stack still holds. No new
module: the sanitizer is one function over a `&str` plus the stack, roughly
the size of `fill_heading_ids`, and it belongs beside the resolver it calls. No new
dependency: an HTML tokenizer for a fixed allowlist is a loop over bytes,
and this project hand-rolls HTTP for the same reason it would not pull one
in for this.

The `Event::Html` that `markdown_html` itself emits for links (from
`link_open`) does not pass through the sanitizer. It is built from escaped
values already, and routing it through would be a second escaping.

## Security argument, stated once

The boundary is the allowlist plus "output is only what we construct". The
checks a reviewer should be able to make from the tests alone:

1. No tag name outside the allowlist reaches the output as a tag.
2. No attribute outside a tag's allowlist reaches the output at all.
3. No attribute value reaches the output unescaped.
4. Every `src` and `href` is decided by `resolve_dest` and `link_open`,
   never copied.
5. Case does not bypass 1 or 2.
6. A malformed or truncated tag is text.

The list of things this does *not* protect against is the same as today's:
an image the project itself contains is served under the raw route's
confinement and 2 MB cap, and an SVG served through it carries the route's
`sandbox` CSP, so a script inside it does not run in the workspace origin.
Nothing here widens that route.

## Tests

Revert-checked, in the sense CLAUDE.md means: each is written to fail with
the sanitizer replaced by today's text arm or with the named rule removed,
and the failure is recorded in the test's comment.

Unit tests in `render.rs`, on `markdown_html` output:

- `<div align="center">` renders as a `div` with only `align`; the same tag
  with `class`, `style`, `id` and `onclick` renders with only `align`.
- `<img src="docs/img/hero.png" width="900">` in `README.md` renders with
  `src="/frag/proj/raw?path=docs/img/hero.png"` and `width="900"`;
  `<img src="../x.png">` from a root file drops the tag and shows the alt.
- `<img src="https://e.com/x.png" alt="cat">` renders `cat` and no `<img`
  and no `e.com`, the twin of the existing remote-markdown-image test.
- `<img src="x" onerror="alert(1)">` contains no `onerror`.
- `<a href="javascript:alert(1)">` renders the inert `mdbroken` anchor;
  `<a href="docs/deploy.md">` renders the `mdlink` anchor with `data-rel`.
- `<script>alert(1)</script>` renders as escaped text containing
  `&lt;script&gt;`, not as a tag.
- `<IMG SRC="x.png">` behaves as `<img src>`; `<ScRiPt>` as `<script>`.
- `<!-- hidden -->` renders as escaped text.
- `<details open><summary>More</summary>` renders `open` bare and `summary`
  as a tag.
- `<img width="10px">` drops `width`; `width="99999"` drops it too.
- A document that opens `<details>` and never closes it ends with
  `</details>`; a lone `</div>` renders as escaped text.
- An `<img>` whose `src` and `width` sit on the two lines after `<img`,
  inside an HTML block, renders as one `img` with both attributes.
- A block ending in `<img src="x.png"` with no `>` renders as escaped text.

A browser test, `tests/browser/mdhtml.mjs`, opens this repository's README
in a preview and asserts that five `img` elements exist with `src` on the
raw route, that the first `details` is collapsed and opens on click, and
that the fragment contains no literal `&lt;img`. The mdlinks test's four
traps apply: the assertions must name specific elements and counts, not
"no error".

## Out of scope

- `style` attributes, `class`, `id`, tables in HTML, `<video>`, `<picture>`,
  `<source>`, `<svg>` inline, `<h1>`–`<h6>` in HTML, `<ul>`/`<li>` in HTML.
  Each is a rule with a test; none is needed by a README in sight.
- Sanitizing SVG *content*. An SVG stays a file the raw route serves under
  `sandbox`; inline `<svg>` is not on the allowlist.
- GitHub's exact rendering of `align` and `width` (it maps them to styles).
  roost emits the attributes and lets the browser apply its legacy meaning,
  which is what centres a `div` and sizes an `img`.
- Changing what the markdown arms do. They are the reference this copies.
