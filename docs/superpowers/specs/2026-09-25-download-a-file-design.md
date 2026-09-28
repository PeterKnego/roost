# Downloading a file from the tree

*2026-09-25. Status: designed, not implemented. Issue
[#120](https://github.com/PeterKnego/roost/issues/120), which already carries
the decisions. This spec records what implementation has to settle on top of
them, and re-derives the two that are load-bearing rather than restating the
list.*

## What is missing

roost can put a file on the server — drag-and-drop, the menu's *Upload files…*
(#110), or a pasted image — and has no way to get one back. The only thing it
sends to a browser today is the whole-project `.roostbak`, which is an archive,
not a file you pick. Fetching a build artifact or a log means leaving roost for
`scp`.

## It is the second streaming download, not the first

`serve_backup` already exists and already settled the hard parts, so the job is
to follow it rather than reinvent:

- **chunked**, because an error after the first byte can no longer change the
  status, and a browser must report a failed download rather than save a
  truncated file that looks complete
- **`nosniff`**, so a `.html` or `.svg` cannot render in roost's origin
- **no `Origin` check**, because a download is a top-level navigation and
  browsers send none — what stands in its place is that the response is not
  readable cross-origin, plus the DNS-rebinding host gate in `route()`

The new arm points at that comment rather than repeating the argument. One
addition: `Content-Security-Policy: sandbox`, which `serve_raw` already sends
and `serve_backup` does not need — this one serves arbitrary repository
content, so it gets the stronger pair.

## Where it differs from every other read path, and why that is safe

**No size cap.** `projects::MAX_FILE_BYTES` (2 MB) governs the other read
paths *because they load the whole file into memory* — `serve_raw` does
`fs::read`, and the buffer then also gets re-encoded into a response. This one
reads 64 KB at a time and writes each chunk as it goes, so peak memory is 64 KB
whatever the file size. The cap would be protecting against a cost this path
does not pay, and the files worth downloading — build output, logs — are
routinely past it.

What it does cost is **one connection thread for the length of the transfer**,
which is the honest trade and is stated so nobody has to rediscover it.

## The two refusals that are not obvious

**It must be a regular file**, matched on the metadata rather than inferred
from "not a directory". A FIFO or a device node in a cloned repository would
otherwise block the connection thread on `read` *forever* — no error, no
timeout, one thread gone per attempt. `symlink_metadata` is not the right call
here (`safe_resolve` has already canonicalised and confined the path, so a
symlink has been resolved to its target and the target is what matters); what
is needed is `file_type().is_file()`, and everything else refused.

A directory gets upload's own wording back: *"folders are not downloaded — use
git or scp for a directory"*.

**The filename is attacker-controlled header content.** It comes from the
repository, so a file called `a"b.txt` or one with a newline in its name would
otherwise end the quoted string or inject a header. Two forms, per RFC 6266:

```
Content-Disposition: attachment; filename="<ascii fallback>"; filename*=UTF-8''<pct-encoded>
```

The fallback replaces `"`, `\`, control characters and every non-ASCII byte;
the RFC 5987 form carries the real name for anything that understands it. Both
are emitted, and both are tested against a fixture that really contains those
characters — an escaping test whose fixture has nothing to escape is one of the
seven vacuous tests CLAUDE.md lists by name.

## Testing

The issue lists the cases. Three of them decide whether the suite is worth
anything:

- **The FIFO test needs a timeout**, or a regression hangs the run instead of
  failing it. CLAUDE.md: a deadlock hangs rather than fails, so counting
  failures says nothing — the test must bound its own wait and report.
- **The symlink-escape test's target must exist.** Otherwise it fails with
  `ENOENT` before reaching the confinement check, which is exactly how a
  symlink escape survived review here once already.
- **A body over 64 KB**, so more than one chunk is exercised. A single-chunk
  test passes against a writer that ignores its loop.

Plus `tests/browser/download.mjs`, because the menu item is in `static/app.js`
and no Rust test reaches that file.

## Out of scope

Folders and zip archives; multi-select; a Download entry in a tab's own menu;
downloading an unsaved buffer. Each needs an archive format, a count cap, and
the `.git`/nested-checkout refusals `search.rs` carries — a separate issue if
ever wanted.
