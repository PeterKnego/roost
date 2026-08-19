# resh — file upload and terminal image paste design

Adds one websocket intent that writes *bytes* into a project, and two clients
for it: drag-and-drop or paste of a local file into the file tree, and paste of
an image into a terminal tab, which hands the shell's Claude a real image
attachment. Changes no existing intent and no HTTP route.

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

One new intent carrying base64 bytes, one byte-typed write path beside
`fileops::save`, and a client that turns a `DataTransfer` into that intent —
plus, for images pasted onto a terminal, a scratch file outside the repo whose
absolute path is injected into the PTY as a bracketed paste.

## Why not a multipart POST

The obvious shape — `POST /upload`, `multipart/form-data`, the answer every web
framework gives — is the expensive one here.

`http.rs` rejects any method but GET at the parse layer (`if method != "GET"`),
and the parser stops at the blank line: there is no `Content-Length` handling,
no chunked reading, no body machinery of any kind. A POST route therefore means
adding request-body reading to the hand-rolled parser *and* a hand-rolled
multipart parser — boundary scanning, per-part headers, CRLF handling, all of it
streaming under a size cap. `Cargo.toml` carries no multipart crate, and the
maintained ones are async, which this server is not.

It also spends the GET-only constraint, which `CLAUDE.md` calls load-bearing
because it is why there is no CSRF surface. That is recoverable rather than
fatal — a multipart POST is a simple request, but browsers still send `Origin`
on cross-origin POSTs, so a check equal in strength to the websocket's is
possible — but it means a second security-critical check to keep correct
forever, in a layer that has never needed one.

One argument that does **not** support the websocket, and is recorded here so a
reviewer does not reach for it: mirroring is not a differentiator. `watch.rs`
broadcasts `TreeChanged` from the filesystem watcher, so a file written by any
route propagates to other browsers without hub coupling.

The actual reason is simpler. **Multipart exists to pack several files into one
body; a websocket has message boundaries natively.** N files is N messages, and
the boundary parser is a problem already solved. The socket is also already
`Origin`-checked in its handshake, already capped (`MAX_FRAME_BYTES`, 8 MB), and
already carries a typed JSON envelope with a decoder.

## The intents

```rust
/// Writes bytes into the project. Distinct from SaveBuffer: no base hash,
/// because an upload has no buffer it was opened from, and bytes rather than
/// text, because the payload may not be UTF-8.
UploadFile { rel: String, data_b64: String },

/// Writes an image outside the repo and pastes its path into a live session.
/// Not UploadFile plus a client-side terminal write: the scratch path is
/// server-chosen, and letting a client name a path outside the project would
/// hand it an arbitrary-write primitive.
PasteImage { session: String, data_b64: String },
```

Base64 rather than a binary frame. A 2 MB payload expands to ~2.7 MB, well
inside the 8 MB frame cap, so the only cost is bytes on a loopback or tailnet
socket. The alternative — an intent announcing a length, followed by a raw
binary frame — saves the expansion but adds per-connection "expecting bytes"
state to `wsconn.rs`, which currently ignores binary frames entirely. The
decoder is ~20 lines and testable in isolation; the state machine is neither.

Decode order matters for the cap: check `data_b64.len()` against the encoded
ceiling *before* allocating the decode buffer, not after.

## Writing bytes safely

A new `fileops::write_bytes(project_dir, rel, bytes)`, beside `save`, sharing
its cap:

1. `projects::safe_resolve_parent` for the destination — the file does not
   exist yet, so `safe_resolve` is the wrong one. This is the existing rule,
   not a new one.
2. Validate the final component client-independently: non-empty, no path
   separators, no `.` or `..`, no control characters or NUL. The client sends
   `file.name` straight from a `DataTransfer`, which is attacker-controlled in
   the drive-by case even though the socket is `Origin`-checked.
3. Decide collision with **positive evidence**, per `CLAUDE.md`:
   `symlink_metadata` on the destination and match — `Err(NotFound)` → absent,
   write; `Ok(_)` → present, refuse and report; `Err(_)` → **cannot tell, do
   nothing**. `Path::exists()` is not acceptable here: it collapses "not there"
   and "cannot look", and it follows symlinks, which is exactly the last row of
   the defect table in `CLAUDE.md`.
4. Write atomically: temp file with a pid-unique name in the destination
   directory, then `rename`. A watcher fires on the write, and a reader that
   sees a half-written file will act on the gap.

An upload never overwrites in v1. That is the conservative direction the
codebase's own history argues for, and it is cheap to relax later; the reverse
is not.

## Where pasted images live

**Not in the repo.** resh already takes care that its own state never appears in
`git status` — layout and unsaved buffers live outside the project for exactly
this reason — and a paste-scratch directory inside the working tree would undo
that on the first screenshot.

Scratch files go to `state_dir()/pasted/<storage_key>/`, reusing the
percent-encoded project key convention from `wsstate::path_for` so a nested
project's `/` cannot land in a filename.

**The extension is load-bearing, and it is the filename that is read, not the
content** (see the evidence appendix). The server sniffs the magic bytes to
choose the extension, and refuses anything that is not PNG, JPEG, GIF, or WebP
— the set Claude Code's paste detection accepts. A correct image saved under
the wrong extension degrades silently into pasted text, which is precisely the
kind of failure that looks like the feature "sometimes not working".

## Injecting the paste

The server writes `\x1b[200~<absolute-path>\x1b[201~` to the session's PTY via
`session::write_input`, which already takes the registry lock only long enough
to clone the writer `Arc` and drops it before the blocking write. Its doc
comment explains the deadlock that discipline avoids, and it is the same
deadlock `CLAUDE.md` records as already shipped once. Nothing new is needed
here — but nothing may bypass it either.

Three properties, each empirically established rather than assumed:

- **The bracketed-paste markers are required.** The same path typed as raw
  characters lands as literal text.
- **The path must be absolute.** The relative branch of Claude Code's handler
  falls back to reading the remote clipboard, which does not exist here.
- **The extension gates the behaviour, not the bytes.**

`PasteImage` refuses when the named session is not live. Writing markers into a
dead PTY is not destructive, but reporting success for a paste nobody will see
is worse than an error.

## Client

Both entry points produce a `FileList` and share one code path.

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
  scoping is what makes the destination unambiguous, and it is the property
  that makes the feature safe to ship without a confirmation dialog.

`dragover` must call `preventDefault()`, or the browser navigates to the file
instead of delivering a drop.

A paste is routed by focus: a terminal tab with an image on the clipboard sends
`PasteImage`; otherwise a file on the clipboard sends `UploadFile` to the
tree's current directory. Multiple files send one intent each — no batching, so
one failure cannot take out its neighbours.

## Testing

The rule from `CLAUDE.md` applies with full force: **would this fail if I
deleted the code it covers?** For the negative cases, assert on *why*.

- **Confinement.** `../escape.png` and a symlink whose parent escapes a root
  must be refused, and the assertion must be on the confinement error message.
  A test that passes because the write hit `ENOENT` first is the exact hole
  that let a symlink escape survive review here before.
- **Collision.** With a file already present, the upload is refused *and the
  original bytes are unchanged*. Asserting `is_err()` alone would pass against
  an implementation that truncated the file and then failed.
- **Cannot-tell.** A destination whose parent directory is unreadable
  (`EACCES`) must be refused, distinguished from the absent case. Skip when
  running as root, where the mode has no effect — a test that silently passes
  because the fixture never entered its own precondition is a documented past
  failure here.
- **Bad base64.** Malformed padding is rejected and leaves no temp file behind.
- **Oversize.** Refused on the encoded length, before the decode allocates.
- **Extension refusal.** A valid PNG offered as `.dat` is refused by
  `PasteImage`; a `.txt` holding PNG bytes is refused. This is the control that
  proves the sniffing runs, since both differ from the accepted case only in
  the name.
- **Injection bytes.** Assert the exact byte sequence written to the PTY,
  including both markers. `CLAUDE.md` records a test whose subject was a call
  to `record_origin` but which only asserted survival — true with nothing
  written at all. "The session is still alive after a paste" is that test again.
- **Concurrency.** Two sessions pasted into simultaneously both complete, and
  the suite is *timed* — a lock-ordering regression hangs rather than fails, so
  a green count proves nothing on its own.

Browser verification is not optional and not substitutable. Synthetic
`DragEvent`s prove the plumbing only; a real drag from a file manager cannot be
synthesized, and the Finder-paste path cannot be exercised from Linux at all.
Both need a human with a mouse before this is believed. The suite must also run
on the Linux host, per the dev/prod substitution table.

## Non-goals

- **Directories.** `webkitGetAsEntry` recursion roughly doubles the client and
  raises questions (symlinks, depth, partial failure) that files do not.
- **Download or drag-out.** Dragging *from* the browser to a desktop needs the
  Chromium-only `DownloadURL` transfer. A download button on a GET route is
  straightforward and unrelated to this design.
- **A clipboard bridge on the host.** Making Claude Code's own `Ctrl+V` work
  would mean `Xvfb` plus a resident `xclip` owning the X selection, `DISPLAY`
  threaded into `session::attach` — which sessions predating the change could
  never pick up, since they deliberately survive restarts. Two host packages
  and a helper process to reach a result the path injection reaches with none.
- **Chunked or resumable uploads.** Anything above the cap is refused outright.
- **Pasting images into the editor**, which has no representation for bytes.

## Open questions for review

1. **The 2 MB cap versus real screenshots.** A 4K PNG screenshot is routinely
   3–5 MB, so the headline use case may not fit. The frame ceiling is 8 MB, so
   ~4 MB is mechanically available — but the cap is a documented hard
   constraint, and raising it is a deliberate change, not an implementation
   detail. Downscaling server-side is the alternative and is worse: it makes
   resh lossy about the user's data.
2. **Collision policy.** Refuse (proposed) versus auto-suffix `name-1.png`.
   Auto-suffix never destroys either, and is friendlier for repeated drops.
3. **Scratch retention.** Nothing prunes `pasted/` as specified. Age-based
   sweep, count cap, or leave it to the user.
4. **Dotfiles.** Whether `.env` may be uploaded at all.

## Appendix: evidence

Recorded so a reviewer need not re-derive it. Claude Code 2.1.235, this host.

**Terminal image paste.** Three runs of one PTY harness, varying only the input:

| Input | Result |
|---|---|
| `.png` path, bracketed paste | `[Image#1]` — path text consumed, attachment made |
| `.png` path, typed as raw characters | plain text, no attachment |
| `.txt` path, **byte-identical PNG content**, bracketed | plain text, no attachment |

The third run is the control: identical bytes, different name, different
outcome — so the gate is the filename and the first run cannot have passed for
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
