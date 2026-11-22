# Video preview

*2026-11-22. A screencast (`.mp4`, `.m4v`, `.mov`, `.webm`) clicked in the
tree used to land in the text path and show "binary file". It now opens a tab
holding the browser's own `<video>` player. The shape follows the PDF tab
(#121, `2026-10-07-pdf-preview-design.md`); this records where it differs.*

## Shape

- **Lists:** `VIDEO_EXT` in `routes.rs`, beside and separate from `IMAGE_EXT`
  and `PDF_EXT` (deny-by-default; not an `<img>` a markdown file may embed).
  The four extensions join `NO_TEXT_EDIT_EXT` (both copies) and the client's
  `RENDERED_EXT`, so a click asks for Preview and there is no ✎.
- **Fragment:** the `file` fragment's image/PDF arm also takes a video and
  returns `render::video_fragment`: the path stripe, an *open in a new tab*
  link, and `<video controls playsinline preload="metadata">` on the raw
  route with the usual `v=<mtime>` cache key.
- **Route:** `serve_raw` hands a video to `serve_video`, which uses the shared
  `open_regular` (confinement, the FIFO/device refusal) and has no size cap.

## Where it departs from the PDF route: ranges

A PDF is streamed chunked from byte 0. A video cannot be: **iOS Safari will
not play a video unless a range request is answered with `206`**, and
Chromium cannot seek past what it has downloaded without one. So
`serve_video` reads one `Range: bytes=` spec (`byte_range`) and answers:

| Request | Answer |
|---|---|
| none, another unit, a list, anything malformed | `200`, whole file |
| `bytes=a-b`, `bytes=a-`, `bytes=-n` inside the file | `206`, `Content-Range`, exactly those bytes |
| starting at or past the end, or `bytes=-0` | `416`, `Content-Range: bytes */len` |

Every answer carries `Accept-Ranges: bytes` and a `Content-Length` (not
chunked), taken from the open file's own metadata. A file that shrinks while
it is read ends the connection short, which the browser reports as a failed
load rather than the whole video.

Headers are the raw route's: the type from the extension (`video/mp4`,
`video/webm`, `video/quicktime`) pinned by `nosniff`, and `CSP: sandbox`.
This is still GET-only; nothing here is a new state change.

## Phones

Unlike a PDF, phone browsers play video inline (`playsinline` keeps iOS from
going full screen on play), so the player stays at every width and the link
becomes a 44 px target below 640 px. The link is also the way out for a codec
the browser lacks (HEVC `.mov` in Chromium): opened directly, it either plays
or downloads.

## Tests

`src/routes.rs`: `byte_range` over every shape above; a video past the 2 MB cap
served whole with `Accept-Ranges` and `Content-Length`; a middle range and a
suffix range return exactly their bytes; an out-of-range start is a 416; HTML
named `.mov` stays `video/quicktime`; `../`, an encoded `../`, a symlink out, a
directory and a FIFO are refused; the fragment is the player, escaped.
`tests/browser/video.mjs`: ffmpeg-made VP9 WebM and H.264 MP4 decode, fill the
pane and seek; the raw route answers a range with 206 in the browser; at 390 px
the player fits and the link is ≥44 px. Revert-checks are in the comments.
