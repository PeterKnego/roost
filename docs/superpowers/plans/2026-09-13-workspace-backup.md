# Backing up a workspace, conversations included

*2026-09-13. Status: implemented 2026-09-13; every task's revert-check was*
*performed and its observed failure recorded in the test's own comment. #18 step 3. Steps 1 and 2 shipped (`claudesess.rs`, `claudehist.rs`,
#68). Spec: `docs/superpowers/specs/2026-09-13-workspace-backup-design.md`,
reviewed 2026-09-13 — the review replaced the surface, so this plan implements
the download-and-intent design, not the CLI one the spec's first draft had.*

## The state of play, verified in the code

- `wsstate::state_dir()` holds a project's state in five places, all keyed by
  `projects::storage_key`: `<key>.json` (layout), `claude/<key>/<session>.json`
  (#18 step 1), `cwd/<key>/<session>`, `launch/<key>/<session>`, and
  `pasted/<key>/` (project content, out of scope).
- `claudehist::transcript_dir_in(home, dir)` is the derivation, already split
  so it can be tested without touching `HOME`. **That split is what makes
  restore-at-a-different-path testable**, and it already exists.
- `claudehist::recent_in` filters to `*.jsonl` whose stem passes
  `valid_session_id`, newest first, `MAX_SCANNED = 30`. Its "every failure is
  an empty list" model is right for a menu and wrong here — spec, *The error
  model is not `claudehist`'s*.
- `FRAGMENT_KINDS` in `routes.rs` does **not** gate `serve_frag`: an unknown
  kind still reaches it through the `_ =>` arm (that is how `claudehist` is
  served today). The list disambiguates project names containing `theme`.
- `http::respond_with` takes a whole body slice. There is no streaming helper,
  so one is added.
- `wsconn.rs` decodes each intent *before* taking the hub lock, and diverts
  `Search` to a worker thread. That divert is the pattern restore copies.
- The settings dialog already has tabs (`.dlg-tabs`) and three panes
  (`.dlg-rows`, `.dlg-themes`, `.dlg-about`). About is the precedent for a pane
  with nothing to save: its OK button is hidden by CSS.

## Task 1 — `src/backup.rs`: the format, and nothing that touches a project

The container only. Writer and parser, with the entry kinds and the id
validation; no walking, no HTTP.

```rust
pub struct Entry { pub kind: Kind, pub id: String, pub bytes: Vec<u8> }
pub enum Kind { Workspace, Session, Cwd, Launch, Transcript, Memory }
pub fn write_header(w, &Header) -> io::Result<()>
pub fn write_entry(w, &Entry) -> io::Result<()>
pub fn parse(bytes: &[u8]) -> Result<Archive, String>
```

Two properties the tests must pin, because they are the design:

- **`parse` rejects, it never repairs.** A declared length running past the end
  of the buffer fails the archive; it does not truncate the entry. An unknown
  `kind` fails; it is not skipped, because a reader that skips what it does not
  understand restores a subset and calls it a restore.
- **An id that fails validation is refused at parse, not at use.** Session
  names by `session::valid_name`, Claude ids by `claudesess::valid_session_id`,
  memory names by `valid_memory_name` (one segment, no `.`/`..`, no separator).
  Refused, never sanitised — a sanitised id is a different id silently.

Tests: round trip; each refusal asserting *its own message*; a truncated
archive; an archive whose ids are `../escape`, `/etc/passwd`, `-rf`.

## Task 2 — the walk, with a three-way outcome

```rust
pub struct Report { captured: Vec<&'static str>, skipped: Vec<Skip>, unreadable: usize }
pub enum Skip { TooBig{id,bytes,cap}, TooMany{cap}, TotalFull{cap}, BadId{id}, Unreadable{id} }
pub fn collect(project, dir, conversations: bool, w: &mut impl Write) -> Result<Report, String>
```

Caps as spec'd: `MAX_TRANSCRIPTS` 50, `MAX_TRANSCRIPT_BYTES` 32 MB,
`MAX_TOTAL_BYTES` 512 MB, `MAX_MEMORY_BYTES` 1 MB. **Each `Skip` carries the
cap it hit**, so the message can name it; a bare count would pass a test while
naming the wrong one.

A transcript is read into memory whole (bounded by `MAX_TRANSCRIPT_BYTES`)
before its entry header is written, so the declared length is the length
actually written — the file can change size between `metadata` and `read`, and
a header written from the stat would then lie.

The refusal the spec argues for: `conversations` asked for and the transcript
directory itself unreadable → `Err`, before a byte goes out. A per-file failure
inside a readable directory is `Skip::Unreadable`, named, and the archive is
still written.

## Task 3 — `GET /frag/{project}/backup`, chunked

`serve_frag` gains a `["backup"]` arm; `"backup"` joins `FRAGMENT_KINDS`.
`?conversations=1` is the opt-in and nothing else turns it on.

Chunked transfer encoding, not `Content-Length`: the length is not known until
the walk finishes, the alternative is staging 512 MB (in memory or in `/tmp` —
the spec rejects both), and an interrupted chunked response is a browser-side
error rather than a file that looks complete. The per-entry declared lengths are
the second line of defence at restore.

`Content-Disposition: attachment; filename="<key>-<date>.roostbak"` — the key,
percent-encoded already, so nothing from a project name reaches the header raw.

## Task 4 — `RestoreWorkspace`, diverted like `Search`

```rust
RestoreWorkspace { file: String, #[serde(default)] dry_run: bool }
```

`file` is project-relative, resolved by `projects::safe_resolve`. Diverted in
`wsconn` before the hub lock, exactly as `Search` is and for the same reason:
it reads up to `max_upload_bytes` and writes the state directory.

Order, all refusals before the first write:

1. Resolve and read `file`; `parse` the whole archive or fail.
2. Resolve the destination project. **Never the header's `source`.**
3. Refuse if the destination has live sessions, naming them.
4. Build the plan: for each entry, the destination path derived from the
   *destination*. A transcript whose `<id>.jsonl` exists → `Skip::Exists`.
5. `dry_run` → send the plan as `RestoreReport` and stop.
6. Apply: rename the existing layout aside to
   `<key>.json.before-restore-<unixtime>`, write everything else, never
   overwrite a transcript.

New event `RestoreReport { dry_run, lines, refused }`.

## Task 5 — a Backup pane in the settings dialog

A fourth tab beside General/Theme/About. Download is an `<a href>` to the
fragment; the conversations checkbox is **unticked on every open** — not
remembered, per the spec — with its warning sentence beside it. Restore is a
file input that POSTs to the existing `/upload/{project}`, then sends
`RestoreWorkspace { dry_run: true }`; the report renders, and a second button
sends it again with `dry_run: false`.

## Task 6 — `tests/browser/backup.mjs`

Download, upload into a *second* project, dry-run, commit, assert the restored
layout. The trap to avoid is `tests/browser/README.md`'s first: asserting on an
element that already exists before the click.

## The two revert-checks

CLAUDE.md: apply the broken version, run it, read the failure, restore, and
record what the failure looked like in the test's own comment.

- **The re-derivation.** Back up from project A at one path, restore as project
  B at another, assert the transcript landed under B's derived directory. Break
  it by storing and reusing the source's absolute path — the test must go red.
  A same-name round trip would stay green, which is why it is not the test.
- **The no-overwrite.** Assert the existing transcript's *content* is unchanged.
  Break it by dropping the exists-check — the test must go red on content, not
  on a reported skip count, which a skip-then-write-anyway bug leaves correct.
