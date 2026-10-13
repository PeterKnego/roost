# PDF preview

*2026-10-07. Issue [#121](https://github.com/PeterKnego/roost/issues/121),
which carries the decisions. This records the probe the issue asked for
before any code, and the one place implementation departs from it.*

## The probe: does the built-in viewer render under `sandbox`?

#121 expected it not to, and planned a narrower policy for this route. Run
before writing the route: a scratch server returning a page with an
`<iframe>` on a 13 KB PDF, served `application/pdf` + `nosniff` and each CSP
below, screenshotted 3 s after load.

| `Content-Security-Policy` | Chromium 140 (full, `--headless=new`) | Firefox 141 (Playwright build) |
|---|---|---|
| `sandbox` | renders | renders |
| `sandbox allow-scripts` | renders | renders |
| `sandbox allow-scripts allow-same-origin` | renders | renders |
| none | renders | renders |

So the route ships **plain `sandbox`** — what every other project byte gets —
and there is no exception to scope. Point 3 of the issue's security section
(an exception for `pdf` only) has nothing left to do.

Two traps in running it, recorded so the next probe does not report a false
"blocked": Playwright's Firefox ships with `pdfjs.disabled = true`, which
renders *every* row blank, including no CSP at all — the control row is what
gave it away; and Chromium's `headless_shell` has no PDF viewer at all, so the
full binary is needed.

## Shape, as built

- **Route:** `serve_raw` hands a `pdf` (`PDF_EXT`, beside and separate from
  `IMAGE_EXT`) to `serve_pdf`, which streams it chunked with no size cap.
  `MAX_FILE_BYTES` is unchanged; images on the same route keep it.
- **Shared helper:** `open_regular` (normalise → `safe_resolve` → refuse a
  directory → refuse anything not a regular file → open) and `stream_chunked`
  were lifted out of #120's `serve_download`, which now uses them. One
  implementation of the FIFO refusal, as the issue asked.
- **Fragment:** the `file` fragment's image arm also takes a PDF and returns
  `render::pdf_fragment`: the path stripe, an *open in a new tab* link and an
  `<iframe>` on the raw route with the image tabs' `v=<mtime>` cache key.
- **Client:** `pdf` joins `RENDERED_EXT`, so a click asks for Preview.
  It was already on `NO_TEXT_EDIT_EXT`, so there is no ✎.

## Where it departs from the issue: phones

#121 put phones out of scope, because no phone browser draws a PDF *inside a
frame* (Chrome on Android shows nothing, iOS Safari the first page). But
every phone browser opens a PDF it navigates to, and `~/projects/CLAUDE.md`
requires every page to work on a phone. So the tab carries a link to the same
raw URL, and below 640 px the frame is hidden and the link becomes a full-width
44 px target. That is not the vendored pdf.js the issue reserves for a mobile
follow-up — it is the browser's own viewer, reached the way the phone can.

## Tests

`src/routes.rs`: a PDF past the cap arrives whole across several chunks with
`application/pdf`, `nosniff` and `sandbox` and no `Content-Disposition`; HTML
named `.pdf` stays `application/pdf`; a FIFO named `.pdf` is refused inside a
5 s bound; `../`, an encoded `../`, an absolute path and a resolving symlink out
are refused; the fragment for a name containing `&` and `"` is the frame, with
both escaped. `tests/browser/pdf.mjs`: a 2.3 MB PDF clicked in the tree makes
Chromium's PDF viewer extension appear as a target (an `<iframe>` on a 404 does
not); the link renders on its own; on a phone viewport the frame is gone and
the link is ≥44 px. Every one revert-checked; the failures are in the comments.
