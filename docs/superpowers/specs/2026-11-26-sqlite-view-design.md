# SQLite tab

*2026-11-26. A SQLite database clicked in the tree (`.sqlite`, `.sqlite3`,
`.db`, `.db3`) used to land in the text path and show "binary file". It now
opens a tab that lists the database's tables and views, shows a table's rows a
page at a time, and lets a row be edited, added or deleted, plus a read-only
SQL box for anything the grid does not cover.*

## Shape

- **Lists:** `SQLITE_EXT` in `routes.rs`, separate from the image, PDF and
  video lists for the same reason those are separate from each other. The four
  extensions join `NO_TEXT_EDIT_EXT` (both copies) and the client's
  `RENDERED_EXT`, so a click asks for Preview and there is no ✎. A text editor
  over a database is how one gets truncated.
- **Fragment:** the `file` fragment's binary arm also takes a database. It
  confines the path and reads the first 16 bytes. Only a file starting with
  `SQLite format 3\0` gets `render::sqlite_fragment`, which is a shell: the
  path stripe and an empty `.sqlview` that `static/sqlite.js` fills. Anything
  else named `.db` gets "not a SQLite database", because `.db` is not SQLite's
  alone.
- **Everything else is a websocket intent.** `Intent::Sql { rel, seq, req }`
  is answered with `Event::Sql { rel, seq, ok, msg, stale, data }` to the
  connection that asked and nobody else. There is no new HTTP route. Reads
  could have been GETs, but the edits cannot be (CLAUDE.md caps POST at two),
  and one transport for both keeps the whole feature behind the socket's
  `Origin` check, including the SQL box.

## Requests (`sqlite::Req`)

| `op` | does | connection |
|---|---|---|
| `tables` | names, kind (table/view), whether editable | read-only |
| `rows` | one page (100) of a table or view, optional `order`/`desc`, total count | read-only |
| `query` | one read-only statement, ≤500 rows | read-only |
| `update` | one cell of one row | read-write |
| `insert` | `INSERT … DEFAULT VALUES` | read-write |
| `delete` | one row | read-write |

## Safety

- **Path:** `projects::safe_resolve`, then `symlink_metadata` must say regular
  file, then the magic header. A FIFO named `x.db` would otherwise block the
  read thread. The database is opened by that resolved path with
  `SQLITE_OPEN_NO_MUTEX` and never `CREATE`: a file deleted between the click
  and an edit is an error, not a new empty database.
- **Identifiers from the browser are never trusted.** A table must be in
  `sqlite_schema`, a column in `PRAGMA table_xinfo` of that table. Then each is
  quoted with `"` doubled anyway. Values are always bound parameters.
- **A database is untrusted input.** It may come from a clone. Every
  connection sets `SQLITE_DBCONFIG_DEFENSIVE` (no writes to the schema or
  shadow tables) and turns `TRUSTED_SCHEMA` off, so a view or trigger cannot
  call a function with side effects. It also sets `SQLITE_LIMIT_ATTACHED` to 0,
  so `ATTACH` cannot reach another file. Extension loading stays off, which is
  SQLite's default.
- **The SQL box is read-only twice.** It runs on a read-only connection, and
  the statement must also report `sqlite3_stmt_readonly`, which refuses
  `VACUUM INTO` and pragmas that write. `prepare` refuses a second statement.
- **Deadline:** a progress handler interrupts any statement after 2 s. A
  count that times out is reported as unknown (`null`), not as 0.
- **Not under the hub lock.** `wsconn` diverts `Sql` before the lock, like a
  revert, and runs it inline on the connection's read thread. A slow query
  stalls only the browser that asked for it.
- **Size:** a text cell is cut to 2000 characters, and a blob is sent as its
  length only. A page stops adding rows past ~1.5 MB of JSON and says so.
  These stay far under the socket's 8 MB frame cap.

## Edits do not clobber

Each row the grid shows carries its `rowid` and a `tag`, a hash of every
column's full value, blobs included. `update` and `delete` run in
`BEGIN IMMEDIATE`. They re-read the row, compare the tag, and only then write.
A row that changed or vanished since the page was read is refused as `stale`,
and the client reloads the page. The app owning the database may be writing
to it at the same moment. This is the same rule as a save conflict: what the
user saw must still be true.

Only rowid tables are editable (the alias is found by preparing
`SELECT "rowid" …`, trying the three names in turn). Views, `WITHOUT ROWID`
tables, generated columns, blob cells and cut cells are shown read-only. A
new value is sent as text or `null` and stored through the column's affinity,
so `42` typed into an `INTEGER` column is stored as an integer.

## Phones

The table picker is a `<select>`, and the grid scrolls inside its own box, so
the page never scrolls sideways. A tap on a cell opens a dialog with the
value, *NULL*, *Save* and *Delete row*. Delete asks again. Inputs use 16 px
text and the buttons are 44 px.

## Tests

`src/sqlite.rs` covers:
- tables and views are listed, with only rowid tables marked editable;
- rows page and order, and an unknown table or column is refused;
- an update is stored through affinity, and a stale tag is refused;
- delete checks the tag;
- the SQL box refuses a write, `VACUUM INTO`, `ATTACH` and two statements;
- the deadline interrupts a runaway recursive query;
- a non-SQLite `.db`, a symlink out and a FIFO are refused.

`tests/browser/sqlite.mjs` drives the tab at 390 px: it opens, pages, edits a
cell, refuses a stale edit, inserts and deletes, and runs a query.
