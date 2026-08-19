# resh — file upload and terminal image paste design

Adds two POST endpoints that stream `multipart/form-data` into a project: one
for files dropped or pasted onto the file tree, one for an image pasted onto a
terminal, which hands the shell's Claude a real image attachment. Adds no
websocket intent. **Spends the GET-only constraint** — see below, because that
is the part of this design that needs the most scrutiny.

## The problem this solves

resh is a remote workspace, but nothing can enter it through the browser. Every
write path in the codebase is UTF-8 text:

- `Intent::EditBuffer { rel, text: String }` and `SaveBuffer` (`proto.rs`) are
  the only routes to a file's contents.
- `fileops::save` takes `&str`, caps at `MAX_WRITE_BYTES`, and guards with a
  hash of the text the buffer was opened from.
- `CreateFile` makes an empty file and nothing else.

So a screenshot, a PDF, a test fixture, or a tarball reaches the workspace only
by leaving it — `scp`, or an ssh session next to the browser tab. That is the
one workflow resh exists to remove.

The second consequence is sharper, because it hits the reason the project
exists at all. resh's README states its purpose as AI-assisted development,
with Claude running in a terminal pane. Claude cannot see a screenshot of the
UI it is editing, and the user cannot give it one: `Cmd+V` in a resh terminal
reads *the browser host's* clipboard, and Claude Code — running on the remote
Linux box — reads *that* machine's X clipboard, which on a headless server does
not exist. The two clipboards never meet.

## What changes, in one sentence

Two `POST` endpoints parse `multipart/form-data` with `multer` and stream each
part to disk under a size cap, plus a client that turns a `DataTransfer` into a
`FormData` — and, for an image pasted onto a terminal, a scratch file outside
the repo whose absolute path is injected into the PTY as a bracketed paste.

## Why POST, when the websocket was right there

The socket carries every other state change, and an earlier draft of this
design used it: a `data_b64` field on a new intent. That draft was wrong, and
the reason is worth recording because it is not the reason one expects.

It is not about the 33% base64 expansion, and not about hand-rolled parsers.
**It is that base64-over-websocket buffers the entire payload in memory before
a single byte reaches disk.** An 8 MB upload costs ~19 MB resident — a ~10.7 MB
JSON string and the decoded bytes, both live at once — and ten concurrent
uploads cost ~190 MB. Streamed multipart is bounded by a small per-connection
buffer no matter how much is sent, and the cap can be enforced *mid-body*, with
the connection dropped before the rest arrives.

That difference only matters under a specific kind of use, and that use is
exactly what to expect: people will reach for this to move more than one file.
A transport that has to hold everything in RAM before deciding whether it is
allowed is the wrong shape for a feature whose foreseeable misuse is bulk.

The websocket keeps everything else. This adds no intent, no event, and no hub
state; mirroring still works because `watch.rs` broadcasts `TreeChanged` for a
file written by any route, so other browsers refresh without the upload path
knowing they exist.

## Spending the GET-only constraint

`CLAUDE.md` says HTTP stays GET-only, and that every state change is a
websocket intent, *because that is why resh has no CSRF surface*. This design
breaks that rule. It is the only rule broken here, and the amendment is:

> HTTP is GET-only apart from `/upload` and `/paste`, which check `Origin`
> exactly as the websocket handshakes do.

What that costs, stated plainly rather than minimised. A `multipart/form-data`
POST is a CORS *simple request*: any page the user visits can submit one
cross-origin with no preflight, and the browser will send it. Nothing in the
response reaches the attacker, but the write still happens — so without a check
this endpoint is a drive-by "write an arbitrary file into any project the
browser can reach", which is materially worse than the read-only exposure GET
routes have.

`Origin` is what closes it, and it closes it completely: browsers set `Origin`
on cross-origin POSTs and it cannot be forged from script. This is the same
defence, and the same code path, that `origin::origin_allowed` already applies
to the two websocket endpoints — the ones that spawn a shell. If that check is
strong enough to stand between a hostile page and a PTY, it is strong enough to
stand between one and a file write.

Two things follow, and both are requirements rather than notes:

- **A POST with no `Origin` header is refused**, matching `wsconn.rs`'s
  handling. Non-browser clients send none, and this endpoint exists for the
  browser.
- **The existing `host_allowed` check still applies**, so DNS rebinding cannot
  reach it either.

The honest summary: the CSRF surface goes from zero to one endpoint pair, each
carrying the project's strongest existing check. That is a real cost, accepted
deliberately, and it is the thing to re-examine first if this design is ever
revisited.

## The endpoints

```
POST /upload/{project}?dir=<rel>     multipart/form-data, N file parts
POST /paste/{project}/{session}      multipart/form-data, one image part
```

`dir` is a project-relative directory, percent-encoded the way every other
route encodes one. Empty or absent means the project root.

Both respond `application/json`. An upload reports **per file**, because a
twelve-file drop where three names collide is not one error:

```json
{"results": [
  {"name": "logo.png",  "ok": true},
  {"name": "notes.md",  "ok": false, "error": "already exists: notes.md"}
]}
```

Status `200` when the request was well-formed, even if individual files failed —
the request succeeded, the items did not. Cap breaches are different: they are
answered `413` and the connection is closed *without* draining the rest of the
body, because continuing to read is precisely what the cap exists to prevent.

Separate endpoints rather than one with a mode flag: they differ in destination
(project versus scratch), in side effect (none versus a PTY write), and in
validation (any bytes versus a sniffed image). Folding them together would mean
one handler branching on all three.

## Three caps, because one does not answer the question

A per-file limit alone does not constrain bulk at all — a thousand files of
100 KB each sails past it. So:

| Cap | Value | What it stops |
|---|---|---|
| Per file | 8 MB | A single oversized file; sized for a 4K screenshot, which runs 3–5 MB |
| Parts per request | 16 | A directory's worth of files in one go |
| Aggregate per request | 32 MB | Sixteen large files adding up |

These are the mechanism by which "this is not a project transfer tool" is
enforced, rather than merely documented. Someone dragging a source tree onto
the tree gets a clear `413` early, not a slow melt — and because parsing is
streamed, both request-level caps are enforced while reading, so the refusal
happens before the bytes are accepted rather than after.

`MAX_TEXT_BYTES` and `MAX_FRAME_BYTES` are untouched. Nothing about this design
goes near the websocket, so the frame ceiling has no reason to move — an
earlier draft that shipped uploads over the socket had to raise it, and that
whole entanglement disappears with the transport.

`CLAUDE.md`'s cap line ("2 MB file cap for reads *and* writes") becomes wrong
when this ships and must be amended in the same change.

## Receiving a part

`multer` is runtime-agnostic: its `tokio-io` feature is optional and off by
default, so it parses a `Stream` of chunks with no async runtime. Driven with
`futures::executor::block_on` over an adapter around the existing `TcpStream`,
it needs no change to resh's thread-per-connection model, no tokio, and no
replacement of the hand-rolled GET parser — which keeps `routes.rs`, its tests,
and both websocket paths exactly as they are.

The reason to take the dependency at all is that boundary scanning, per-part
headers and CRLF handling are fiddly parsing of untrusted input, which is the
last thing worth hand-rolling. The existing parser already reads the request
line and headers and stops at the blank line; only body reading is new.

Each part streams into a temp file in the *destination* directory, named with
the pid so two processes cannot collide, then:

1. `projects::safe_resolve_parent` for the destination — the file does not
   exist yet, so `safe_resolve` is the wrong one.
2. Validate the filename client-independently: non-empty, no path separators in
   either direction, no `.` or `..`, no control characters or NUL. `multer`
   reports the browser's `filename` verbatim and it is attacker-influenced.
   **A part whose filename contains a separator is refused, not flattened** —
   that is how directory upload stays out (see non-goals).
3. **Refuse any destination inside `projects::SKIP_DIRS`** — `.git`, `.claude`,
   `target`, `node_modules`, `__pycache__`, `.venv`. Those directories are not
   rendered in the tree, so a file written there is invisible in the UI that
   put it there: the user cannot see it, open it, or delete it, and the next
   upload of the same name is refused as "already exists" against a file they
   have no way to find. `.git` raises that from confusing to destructive, since
   a write into an object or ref directory can corrupt the repository.

   This is a *visibility* rule, not a path-safety one — the paths are inside
   the project and perfectly legal, which is why nothing else refuses them.
   Ordinary dotfiles are unaffected: the tree hides a fixed list of
   directories, not a leading dot, so `.gitignore` renders like any other file
   (confirmed in the browser run in the appendix, where it was the row a drop
   resolved against) and uploading one is honest.
4. Decide collision with **positive evidence**, per `CLAUDE.md`:
   `symlink_metadata` on the destination — `Err(NotFound)` → absent, proceed;
   `Ok(_)` → present, refuse and report; `Err(_)` → **cannot tell, do nothing**.
   `Path::exists()` is not acceptable: it collapses "not there" and "cannot
   look", and it follows symlinks, which is the last row of the defect table in
   `CLAUDE.md`.
5. `rename` the temp file into place.

Checked before the stream starts *and* again before the rename, since a
streamed part takes long enough for the answer to change. A part that fails at
any step deletes its temp file and records a per-file error; the remaining
parts still run.

## Where pasted images live

**Not in the repo.** resh already takes care that its own state never appears in
`git status` — layout and unsaved buffers live outside the project for exactly
this reason — and a paste-scratch directory inside the working tree would undo
that on the first screenshot.

Scratch files go to `state_dir()/pasted/<storage_key>/`, reusing the
percent-encoded project key from `wsstate::path_for` so a nested project's `/`
cannot land in a filename.

**The extension is load-bearing, and it is the filename that is read, not the
content** (see the appendix). The server sniffs the first chunk's magic bytes to
choose the extension and refuses anything that is not PNG, JPEG, GIF or WebP —
the set Claude Code's paste detection accepts. Sniffing the first chunk rather
than the whole file keeps this streaming like every other part. A correct image
saved under the wrong extension degrades silently into pasted text, which is
exactly the failure that reads as "the feature sometimes doesn't work".

## Injecting the paste

After the scratch file is renamed into place, the server writes
`\x1b[200~<absolute-path>\x1b[201~` to the session's PTY via
`session::write_input`, which takes the registry lock only long enough to clone
the writer `Arc` and drops it before the blocking write.

**This runs on the HTTP connection's own thread**, which is the shape that makes
it safe: unlike the websocket draft, no hub lock is held, because the hub is
not involved at all. That draft had to defer the write to a spawned thread to
avoid holding the hub lock across a blocking PTY write — the deadlock
`CLAUDE.md` records as already shipped once. Moving uploads to HTTP removes the
hazard rather than managing it.

Three properties, each empirically established rather than assumed:

- **The bracketed-paste markers are required.** The same path typed as raw
  characters lands as literal text.
- **The path must be absolute.** The relative branch of Claude Code's handler
  falls back to reading the remote clipboard, which does not exist here.
- **The extension gates the behaviour, not the bytes.**

`/paste` refuses when the named session is not live. Writing markers into a dead
PTY is not destructive, but reporting success for a paste nobody will see is
worse than an error.

## Client

Both entry points produce a `FileList`, become one `FormData`, and go out on one
`XMLHttpRequest` — chosen over `fetch` for `upload.onprogress`, which a
multi-file send needs and `fetch` does not expose.

Listeners are **delegated at document level**, not bound per row. The tree is an
htmx fragment replaced wholesale on `TreeChanged` — which an upload itself
triggers via the watcher — so per-row listeners would die on the first refresh
after the first successful drop.

Destination resolution mirrors what the markup already provides
(`render.rs:138-169` puts `data-rel` on every directory `<details>` and every
file `<a>`):

- `e.target.closest("[data-rel]")`, then `<details>` → that directory,
  file `<a>` → its parent directory.
- No `[data-rel]` ancestor → the drop is outside the tree; ignore it. Tree-only
  scoping is what makes the destination unambiguous, and it is what makes this
  safe to ship without a confirmation dialog.

`dragover` must call `preventDefault()`, or the browser navigates to the file
instead of delivering a drop.

A paste is routed by focus: a terminal tab with an image on the clipboard goes
to `/paste`; otherwise files on the clipboard go to `/upload`. The per-file
results are rendered through the existing `showError` path, one line per failed
file.

## Testing

The rule from `CLAUDE.md` applies with full force: **would this fail if I
deleted the code it covers?** For the negative cases, assert on *why*.

- **Origin.** A POST from a foreign origin, and a POST with no `Origin` at all,
  are both refused — and the assertion must confirm *the file was not written*,
  not merely that the status was 403. This is the check the whole GET-only
  amendment rests on, so it gets the same treatment
  `ws_rejects_foreign_and_missing_origin` already gives the socket.
- **Method.** `GET /upload/...` is still refused, so the new arm cannot be
  reached without a body.
- **Confinement.** A part named `../escape.png` is refused with the confinement
  message. A test that passes because the write hit `ENOENT` first is the exact
  hole that let a symlink escape survive review here before.
- **Separators are refused, not flattened.** A part named `sub/a.png` is an
  error; it must not land as `a.png`. This is the test that keeps directory
  upload from arriving by accident.
- **Hidden destinations.** `dir=.git` is refused; `.gitignore` as a filename
  succeeds. The pair is the control — both differ from each other only in
  whether the *directory* is one the tree hides.
- **Collision.** With a file already present, that part is refused *and the
  original bytes are unchanged*. Asserting an error alone would pass against an
  implementation that truncated and then failed.
- **Cannot-tell.** A destination whose parent is unreadable (`EACCES`) is
  refused, distinguished from the absent case. Skip when running as root, where
  the mode has no effect — a test that silently passes because the fixture never
  entered its own precondition is a documented past failure here.
- **Each cap, separately.** One 9 MB file; seventeen small files; sixteen files
  summing past 32 MB. Each must be refused by *its own* limit, so the messages
  must differ — three tests that all pass because one cap fires would tell us
  nothing about the other two.
- **A cap breach does not leave a partial file.** After a 413, the destination
  directory contains no temp files and no truncated upload.
- **Partial failure.** Three parts where the middle one collides: the first and
  third land, the response names the middle one, and the status is still 200.
- **Extension refusal.** A valid PNG posted to `/paste` as `.dat` is refused;
  a `.txt` holding PNG bytes is refused. Both differ from the accepted case
  only in the name, which is what makes them a control on the sniffing.
- **Injection bytes.** Assert the exact byte sequence written to the PTY,
  including both markers. `CLAUDE.md` records a test whose subject was a call
  to `record_origin` but which only asserted survival — true with nothing
  written at all. "The session is still alive after a paste" is that test again.

Browser verification is not optional and not substitutable. Synthetic
`DragEvent`s prove the plumbing only; a real drag from a file manager cannot be
synthesized, and the Finder-paste path cannot be exercised from Linux at all.
`tests/browser/` now exists (a Deno-driven Chromium harness), and `CLAUDE.md`
says anything touching `static/app.js` should be checked there — so the client
gets an automated test as well as a manual pass. The suite must also run on the
Linux host, per the dev/prod substitution table.

## Non-goals

- **Directories.** Deliberately unsupported, and enforced rather than merely
  declared: a part whose filename contains a separator is refused, and the
  part-count cap refuses a tree-sized drop. resh is a development workspace
  with git sitting right there — `git clone`, `rsync` and `scp` are what move a
  project. Uploads are for assets, fixtures and screenshots.
- **Archive extraction.** The obvious workaround for the above, and a worse
  feature: zip-slip path traversal and decompression bombs are a vulnerability
  class of their own, in a component whose whole job is handling hostile input.
- **Download or drag-out.** Dragging *from* the browser to a desktop needs the
  Chromium-only `DownloadURL` transfer. A download button on a GET route is
  straightforward and unrelated to this design.
- **A clipboard bridge on the host.** Making Claude Code's own `Ctrl+V` work
  would mean `Xvfb` plus a resident `xclip` owning the X selection, `DISPLAY`
  threaded into `session::attach` — which sessions predating the change could
  never pick up, since they deliberately survive restarts. Two host packages
  and a helper process to reach a result the path injection reaches with none.
- **Resumable uploads.** Anything above a cap is refused outright.
- **Pasting images into the editor**, which has no representation for bytes.

## Open questions for review

1. **Collision policy.** Refuse (proposed) versus auto-suffix `name-1.png`.
   Auto-suffix never destroys either, and is friendlier for repeated drops — but
   refusing is the direction this codebase's history argues for, and it is cheap
   to relax later.
2. **Scratch retention.** Nothing prunes `pasted/`. Age-based sweep, count cap,
   or leave it to the user.
3. **The three cap values.** 8 MB / 16 parts / 32 MB are proposed, not derived.
   They are the enforcement of the directories decision, so they are worth an
   opinion rather than a shrug.

## Appendix: evidence

Recorded so a reviewer need not re-derive it. Claude Code 2.1.235, this host.

**Terminal image paste.** Three runs of one PTY harness, varying only the input:

| Input | Result |
|---|---|
| `.png` path, bracketed paste | `[Image#1]` — path text consumed, attachment made |
| `.png` path, typed as raw characters | plain text, no attachment |
| `.txt` path, **byte-identical PNG content**, bracketed | plain text, no attachment |

The third run is the control: identical bytes, different name, different
outcome — so the gate is the filename, and the first run cannot have passed for
an unrelated reason.

**Drop plumbing.** Headless Chromium against the dev instance, `disruptor-rs`,
with a document-level listener:

| Target | Reached document | `DataTransfer.files` | Resolved |
|---|---|---|---|
| File row `<a data-rel=".gitignore">` | yes | intact | project root |
| Directory `<summary>` | yes | intact | `.github` |
| `ul.tree` background | yes | intact | unresolved |
| `body` | yes | intact | unresolved |

No existing handler — htmx, the divider `mousedown`, xterm — swallowed `drop`
or stopped propagation, and `closest()` resolves correctly from a `<summary>`
that carries no `data-rel` of its own. A paste carrying a file reaches a
document-level handler with `clipboardData.files` intact.

**Dependency survey**, taken from crates.io while choosing the transport:

| Crate | Version | Last updated | Note |
|---|---|---|---|
| multer | 3.1.0 | 2024-05-04 | chosen; `tokio-io` optional and off |
| tiny_http | 0.12.0 | 2022-10-06 | rejected; last *commit* 2023-05-16 |
| rouille | 3.6.2 | 2023-04-24 | rejected; dormant |
| multipart | 0.18.0 | 2021-05-29 | rejected; unmaintained, two forks exist |
| axum / actix-web | 0.8.9 / 4.14.1 | 2026 | rejected; async, would mean tokio |

`tiny_http` was the closest architectural fit and was still rejected on a
specific finding: `Request::upgrade()` returns `Box<dyn ReadWrite + Send>`,
while `wsconn.rs:102` and `term.rs:99` both call `get_ref().try_clone()` to
split each socket into reader and writer threads. A boxed trait object cannot be
cloned, so adopting it would have forced a mutex around blocking socket writes —
the bug class this codebase documents most heavily.
