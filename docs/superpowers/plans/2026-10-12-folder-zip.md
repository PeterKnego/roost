# Folder zip — plan

Spec: [`2026-10-12-folder-zip-design.md`](../specs/2026-10-12-folder-zip-design.md).

1. **`zipdl::plan`.** Walk with the filter, ignore inheritance, symlink
   confinement and caps. Unit tests: a fixture with `.git`, a nested
   checkout, a gitignored `dist/`, a dotfile, `node_modules`, an inside and
   an outside symlink, and an unreadable directory.
2. **`zipdl::write`.** Streaming deflate entries with data descriptors,
   directory entries, the skipped file, and the central directory. Tested by
   unpacking the result with `unzip -t` / Python's `zipfile` in the test,
   and by a pure-Rust reader of the central directory.
3. **Route.** `download?zip=1` and `&check=1` in `routes.rs`, chunked
   writer, `Content-Disposition` through `disposition_name`.
4. **Menu.** `fileMenu` offers *Download as zip* on folders and on the
   root; check first, then navigate.
5. **Browser test.** `download.mjs` gains a folder section.
