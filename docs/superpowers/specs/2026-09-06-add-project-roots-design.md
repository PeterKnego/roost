# Adding project roots from the front page

*2026-09-06. Status: implemented (see the plan of the same date).*

## What and why

A root is a directory roost scans for projects. Today roots come only from
`ROOST_ROOTS` or the global config's `roots` list, and a roost started with
neither refuses to start: it prints a four-line notice and exits 2. That is
the right answer to a misconfigured service, and the wrong first thing for
someone who just ran `cargo install roost` and typed `roost 8444`. They see
nothing in the browser at all.

This lets roost start with no roots, explains the state on the front page,
and offers **Add path** there; and it puts the same action next to the roots
in the front page's header for adding more later. Decided with Peter on
2026-09-06.

## Behaviour

### Start without roots

With no roots from either source, roost starts, prints the existing notice to
stderr (shortened: the browser now carries the explanation), and serves. The
front page's Projects pane shows, instead of an empty list:

> Roost has no defined paths where to look for projects. Add a path where
> your projects live; roost will search it for git repositories and list
> them here.

with one button, **Add path**. The Sessions pane says what it says for an
empty scope today.

### Roots are re-read per connection

`main.rs` resolves the root list once and hands a clone to every connection
thread. A root added while the server runs must be seen by the next request,
so the accept loop resolves `projects::roots()` per connection instead — an
environment read and one small file parse, the same cost the rest of config
already pays per request. `registry::reconcile` at startup keeps the startup
list; nothing else holds one.

### The front page

The header's roots label becomes the list of roots, each on its own, with a
**+** control after them (`#addroot`, title "add a project root"). Both it and
**Add path** open the existing text dialog (`askText`) with title "Add a
project root", the label "Directory to scan for projects" and confirm "Add".
The front page therefore loads `dialog.js` and carries the `dlg-text` and
`dlg-confirm` shells, under the same structural CSS lock as the workspace,
since the overview also applies a project-independent theme stylesheet.

On success the page refreshes both fragments (the existing refresh function)
and rewrites the header's roots list from the reply. On refusal the reason
shows as a banner and the dialog's text is kept, so a typo is one edit away —
`askText` resolves on confirm, so the page re-opens it prefilled.

### The write: a roots socket

The front page has no websocket, and HTTP stays at `GET` plus the two upload
POSTs. So the write travels on a new browser-facing socket, `/ws/_roots`,
dispatched by `route_ws` before the terminal fallback and handshaken by the
same `Origin` check as the workspace socket (`origin::origin_allowed`,
refusing a handshake with none). It carries one intent and two events:

```
AddRoot { path: String }
→ Roots { roots: Vec<String> }      (the new list, for the header)
→ Error { msg: String }
```

The connection is one exchange: the client opens it when the person
confirms, sends, reads one event, closes.

Validation, in order, each refusal naming what it saw:

1. `path` is trimmed; a leading `~/` expands against `HOME`; the result must
   be absolute ("`foo` is not an absolute path").
2. `symlink_metadata`: `Err(NotFound)` → "no such directory"; any other
   error → "cannot read <path>: <error>" — *could not look* is not *absent*;
   `Ok` but not a directory (a symlink is followed with `metadata` for this
   one question) → "not a directory".
3. `canonicalize`; the canonical path must not already be in the current
   root list, compared canonically ("already a root").
4. If `ROOST_ROOTS` is set and non-empty in this process's environment, the
   write is refused: "this roost's roots come from ROOST_ROOTS; add it
   there (the unit file, for a service)". The environment wins over the file
   for every read, so writing the file would change nothing visible and look
   like a silent failure.
5. Append the canonical path to `roots` in the global config through
   `config::write_setting` (a `List`, the existing list read with
   `raw_setting` first), under the global write lock. The global file is
   created if absent. A file that does not parse is refused with its error,
   unchanged, as every config write is.

The canonical path is what is written: the list is what other callers (hooks
with no environment) resolve projects by, and two spellings of one directory
are two roots.

### The settings dialog

`roots` stays read-only there; its description becomes "Directories scanned
for projects — add one from the front page, any directory including / or your
home, which widens what a browser here reaches to exactly what a shell from the
same origin already reaches; removing one is a hand edit of
~/.config/roost/config.toml."

## Security

Adding a root exposes more of the filesystem to whoever reaches the socket,
so it sits behind the boundary every shell-spawning socket already has: the
`Origin` check with loopback or the configured allowlist. A page on another
origin cannot complete the handshake. There is no path-confinement question
in the usual sense — a root is by definition outside every project — but
nothing here creates, follows into, or lists a directory: validation reads
metadata and canonicalises, and the scan that follows is the existing one.
`allowed_origins`, `max_upload_bytes` and `ide` remain unwritable from any
page.

A root may be **any** directory, including `/` or `$HOME`; nothing here
restricts the choice, and the tree, search and the editor then reach
everything under it. That is a widening worth stating plainly, and it is
bounded by what already holds: whoever can complete the handshake can also
open a terminal on the same origin, and a shell reaches the whole filesystem
already. So adding a root widens what the *browser* reaches to exactly what a
shell from the same origin reaches — it grants no reach the origin did not
already have. Removing one is deliberately not offered here: it is a hand edit
of `roots` in `~/.config/roost/config.toml` (a live session under a removed
root would otherwise be reaped by the next sweep, which is not an undo).

## Testing

Rust, `projects.rs` or a new `roots.rs`:

- each validation case fails with its own message: relative, missing,
  unreadable (a directory with no search permission, skipped when running as
  root), a file, a duplicate spelled two ways;
- `ROOST_ROOTS` set → refused by name, file untouched;
- append keeps an existing list, unrelated keys and comments, and creates the
  file when absent; the written entry is canonical.
- `lib.rs`: the accept loop's per-connection root resolution is covered by
  the browser test below (a root added mid-run is listed by the next
  request).

Browser, `tests/browser/roots.mjs`: start roost with `ROOST_ROOTS` empty and
`ROOST_CONFIG` on an empty file; the front page shows the sentence and the
button; **Add path** with the fixture's roots directory lists its project,
the header shows the root, the global file holds it; **+** adds a second
directory and both appear; a nonexistent path is refused with a banner and
the dialog reopens with the text kept. The Rust origin tests already cover a
handshake without `Origin`; `roots.mjs` sends one from the page's own
origin, which is the positive case.

## Non-goals

- Removing or reordering roots.
- Editing roots in the settings dialog.
- Any change to `ROOST_ROOTS` precedence.
