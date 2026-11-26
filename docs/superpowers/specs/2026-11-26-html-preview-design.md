# HTML report preview

*2026-11-26. An `.html` or `.htm` file clicked in the tree used to open as
source in the editor. A generated report (coverage, test results, a
benchmark, a Playwright report) is meant to be looked at, not read as
markup. It now opens rendered, the way markdown does, with ✎ for the
source.*

## Shape

- **Lists:** `HTML_EXT` in `routes.rs`. The two extensions join the client's
  `RENDERED_EXT`, so a click asks for Preview. They stay **off**
  `NO_TEXT_EDIT_EXT`: HTML is text, so ✎ switches to the source and saving
  works. This is the same split as SVG.
- **Fragment:** the `file` fragment's `is_html` arm returns
  `render::html_fragment`. It holds the path stripe, an *open in a new tab*
  link, and an `<iframe class="htmlview" sandbox=…>` on the view route,
  with `v=<mtime>` as the cache key.
- **Route:** `/frag/.view/<project>/<rel>`, served by `serve_view`.

## Why a path-shaped route

A report is a page plus the files it links relatively: `assets/report.css`,
`report.js`, `data.json`, images. The raw route takes the file as
`?path=`, so a relative link from it resolves to `/frag/<project>/report.css`
and misses. On a path-shaped URL the browser resolves every relative link
itself, and each one comes back through the same route.

- **`.view` cannot shadow a project:** `projects::resolve_project` refuses
  a first segment that starts with a dot, and every project's URL starts
  with its first segment.
- **The project key is one segment, encoded twice** (`view_url`). A key may
  hold `/` (`team/app`), and the request path is percent-decoded before it
  is split, so the key is encoded once to keep its `/` as `%2F` and once
  more for the URL. Every relative link keeps that segment as written.
- **The file** goes through `open_regular`, the shared pipeline: normalise,
  `safe_resolve` (confined, symlinks resolved), then regular files only. A
  directory or a FIFO is a 404. It is streamed with no size cap, as for a
  PDF, because a Playwright report embeds its data and passes 2 MB.
- **The type** comes from the extension (`view_type`: the asset table's
  types plus `htm`, `mjs`, `json`, `map`, `txt`, `csv`, `xml`, `wasm`),
  pinned by `nosniff`.

## The boundary: sandbox without `allow-same-origin`

A report may come with a clone, and its scripts must run (charts, sortable
tables). Every response on the route, the page and each subresource alike,
carries:

    Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox

The frame repeats it as its `sandbox` attribute. The header is what also
covers the page when it is opened in its own tab.

What is missing is the boundary: **no `allow-same-origin`.** The report
runs in an opaque origin:
- Everything it sends carries `Origin: null`, which `origin::origin_allowed`
  refuses on every websocket and both POSTs. A test now pins that.
- It cannot read any roost response, because roost sends no CORS headers.
- It cannot reach `parent.document`.

The browser test checks all three from inside a report, and with
`allow-same-origin` added, the report opens the workspace socket that
spawns shells. That one token is the difference between a report and
script running as roost.

GET stays read-only, so a GET a report triggers changes nothing. A form
it posts is refused by the Origin check, like any cross-origin page's.

## Phones

A phone browser renders HTML in a frame (a PDF is the case it does not), so
the frame stays at every width. The link becomes a 44 px target below
640 px. The frame is white whatever the theme, because reports assume a
white page.

## Tests

- **`src/routes.rs`:**
  - the frame's `src`, taken from the fragment, served with `text/html`,
    `nosniff` and the sandbox, and without `allow-same-origin`;
  - its relative CSS, JS and JSON served with their types and sandboxed too;
  - `../`, `%2E%2E`, a double-encoded `..`, `..` inside the project key, an
    unknown project, a symlink out and a directory all refused;
  - the fragment is the sandboxed frame, with the name escaped.
- **`src/origin.rs`:** `Origin: null` is refused.
- **`tests/browser/html.mjs`:**
  - the report renders with its relative CSS and script;
  - from inside the report: an opaque origin, no read of `/frag/…/tree`,
    no workspace socket, no `parent.document`;
  - ✎ shows the source;
  - at 390 px the frame fits and the link is 44 px.
