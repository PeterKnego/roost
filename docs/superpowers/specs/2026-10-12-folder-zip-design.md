# Download a folder as a zip

*2026-10-12. Issue [#162](https://github.com/PeterKnego/roost/issues/162),
which carries the problem and the decisions. This records what
implementation settled.*

## What it does

The tree's menu on a folder, and on blank tree space (the project root),
offers **Download as zip**. The browser saves `<folder>.zip`, streamed as it
is built.

## Shape

- `src/zipdl.rs` plans the archive (a walk that only collects paths and
  sizes) and then streams it.
- `GET /frag/{project}/download?path=<dir>&zip=1` serves it.
  `&check=1` answers the plan as JSON instead: `{ok, files, bytes}` or
  `{ok: false, error}`. The client asks that first, because the download
  itself is a navigation, and a refusal served to a navigation replaces the
  workspace with an error page. The check costs one more walk, which is the
  cheap half.
- **The walk is search's walk.** `TreeFilter` (dotfiles, `hide`, `target`,
  `node_modules`, …), plus `.git`, a nested checkout, and a gitignored
  directory (unless `show_hidden`). Gitignore rules are inherited from the
  project root down to the chosen folder, so zipping `app/` applies the
  root's `.gitignore` as well as `app/.gitignore`. The chosen folder itself
  is always included: choosing it is the override.
- **`ROOST-SKIPPED.txt`**, at the archive's root and written last, whenever
  anything was left out. It is grouped as could-not-read, nested checkout,
  gitignored, hidden by the tree filter, symlink, and changed while zipping.
  Each group says what would include it. Each list is capped at 200 lines
  plus a count.
- **Symlinks.** A file symlink is included when `projects::safe_resolve`
  confines it to the project. Any other symlink, including every directory
  symlink, is listed and skipped.
- **Caps, checked before the first byte:** 20 000 files and 1 GiB
  uncompressed (`zipdl::MAX_FILES`, `zipdl::MAX_BYTES`). Over either, the
  plan is refused with the number found.
- **Format.** Deflate through `flate2` and CRC-32 through `crc32fast`. Both
  are already in `Cargo.lock` (ureq), so no new crate is added. Every entry
  uses a data descriptor (general-purpose bit 3) and the UTF-8 name flag
  (bit 11), so sizes need not be known up front and a file that grows
  mid-stream is still a valid entry. The caps keep every offset under 4 GiB
  and the entry count under 65 535, so no Zip64 is needed. Directories get
  entries of their own, so empty ones survive. Timestamps are the files'
  mtimes, as UTC (a zip's DOS time has no zone).
- **A file that fails mid-stream** keeps the bytes already written. Its
  entry is closed correctly and the file is listed under "changed while
  zipping". The response is still dropped without the terminating chunk
  only if writing to the *socket* fails. A file that shrank or vanished is a
  fact for the skipped list, not a broken archive.
- No lock is held: plan and stream run on the connection thread, like
  `serve_download`.
