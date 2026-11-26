//! The SQLite tab: a project's database, read and edited in place.
//!
//! A `.sqlite`/`.db` clicked in the tree used to show "binary file". The tab
//! (`render::sqlite_fragment`, `static/sqlite.js`) lists tables and views,
//! pages through rows, edits one cell, adds a row, deletes one, and runs a
//! read-only statement. Every request arrives as `Intent::Sql` and is answered
//! with `Event::Sql` to the connection that asked. There is no HTTP route,
//! so the edits stay behind the websocket's `Origin` check like every other
//! state change. `wsconn` runs it before taking the hub lock: SQLite is file
//! I/O with a busy wait, and CLAUDE.md forbids blocking under that lock.
//!
//! Three things here are load-bearing; the spec
//! (`docs/superpowers/specs/2026-11-26-sqlite-view-design.md`) has the rest.
//!
//! - **The database is untrusted.** It may have come with a clone. Every
//!   connection is defensive, does not trust its schema, and may not attach
//!   another file (`harden`). The SQL box is read-only twice: on a read-only
//!   connection, and refused unless SQLite itself calls the statement
//!   read-only.
//! - **Identifiers from the browser are looked up, never spliced.** A table
//!   must be in `sqlite_schema` and a column in its `table_xinfo`. Quoting is
//!   a second line of defence, not the first.
//! - **An edit only lands on the row the user saw.** Every row is sent with a
//!   hash of its full values (`tag`). An update or delete re-reads the row
//!   inside `BEGIN IMMEDIATE` and refuses as stale if the hash moved. The app
//!   that owns the database may be writing to it at the same time, and a
//!   silent overwrite of its change is the save conflict this codebase
//!   refuses everywhere else.

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Extensions that open as a database. `.db` is not SQLite's alone, which is
/// why the header is checked (`resolve`) before anything opens it.
pub const SQLITE_EXT: &[&str] = &["sqlite", "sqlite3", "db", "db3"];

pub fn is_sqlite(rel: &str) -> bool {
    SQLITE_EXT.contains(&crate::assets::ext_of(rel).as_str())
}

/// Rows per page in the grid.
pub const PAGE: i64 = 100;
/// Rows the SQL box returns at most.
pub const QUERY_ROWS: usize = 500;
/// A text cell is cut to this many characters on the wire. The full value is
/// still what the row's tag covers, so a cut cell cannot be edited blind.
pub const TEXT_CAP: usize = 2000;
/// A page stops adding rows once its JSON passes this. Far under the
/// workspace socket's 8 MB frame cap, which would drop the whole connection.
pub const PAGE_BYTES: usize = 1_500_000;
/// Any one statement is interrupted after this.
pub const DEADLINE: Duration = Duration::from_secs(2);
/// How long a write waits for the app holding the database to let go.
const BUSY: Duration = Duration::from_millis(1500);

const MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Req {
    Tables,
    Rows {
        table: String,
        #[serde(default)]
        offset: i64,
        #[serde(default)]
        order: Option<String>,
        #[serde(default)]
        desc: bool,
    },
    Query { sql: String },
    Update { table: String, rowid: i64, tag: String, col: String, value: Option<String> },
    Insert { table: String },
    Delete { table: String, rowid: i64, tag: String },
}

/// What `Event::Sql` carries besides the request's own `rel` and `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub ok: bool,
    pub msg: String,
    /// Refused because the row is no longer what the user saw.
    pub stale: bool,
    pub data: Value,
}

impl Reply {
    fn ok(data: Value) -> Self {
        Reply { ok: true, msg: String::new(), stale: false, data }
    }
    fn err(msg: impl Into<String>) -> Self {
        Reply { ok: false, msg: msg.into(), stale: false, data: Value::Null }
    }
    fn stale() -> Self {
        Reply {
            ok: false,
            msg: "That row changed since you looked; showing it as it is now.".into(),
            stale: true,
            data: Value::Null,
        }
    }
}

/// The database file `rel` names, if it is one: confined, a regular file
/// (a FIFO named `x.db` would block this thread on its first read), and
/// starting with SQLite's header.
pub fn resolve(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = crate::assets::normalize(rel).ok_or("path outside project")?;
    let path = crate::projects::safe_resolve(dir, rel)?;
    // `safe_resolve` canonicalised, so this is the target, not a link.
    let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    let mut head = [0u8; 16];
    use std::io::Read;
    let n = std::fs::File::open(&path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| e.to_string())?;
    // An empty file is a valid empty database to SQLite, but nothing to show,
    // and opening it read-write would make it one. Not ours to decide.
    if n < 16 || &head != MAGIC {
        return Err("not a SQLite database".into());
    }
    Ok(path)
}

/// Answer one request. Never panics on a database's contents; a panic would
/// still be caught by the caller in `wsconn`, but would lose the reason.
pub fn run(dir: &Path, rel: &str, req: Req) -> Reply {
    let path = match resolve(dir, rel) {
        Ok(p) => p,
        Err(e) => return Reply::err(e),
    };
    let r = match req {
        Req::Tables => open(&path, false).and_then(|c| tables(&c)),
        Req::Rows { table, offset, order, desc } => {
            open(&path, false).and_then(|mut c| rows(&mut c, &table, offset, order.as_deref(), desc))
        }
        Req::Query { sql } => open(&path, false).and_then(|mut c| query(&mut c, &sql)),
        Req::Update { table, rowid, tag, col, value } => {
            open(&path, true).and_then(|mut c| update(&mut c, &table, rowid, &tag, &col, value))
        }
        Req::Insert { table } => open(&path, true).and_then(|mut c| insert(&mut c, &table)),
        Req::Delete { table, rowid, tag } => open(&path, true).and_then(|mut c| delete(&mut c, &table, rowid, &tag)),
    };
    r.unwrap_or_else(Reply::err)
}

fn open(path: &Path, write: bool) -> Result<Connection, String> {
    // Never CREATE: a database deleted between the click and the edit must be
    // an error, not a fresh empty file under the old name. No URI either, so
    // the name is a path and nothing else.
    let flags = OpenFlags::SQLITE_OPEN_NO_MUTEX
        | if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let mut c = Connection::open_with_flags(path, flags).map_err(msg)?;
    harden(&mut c)?;
    c.busy_timeout(BUSY).map_err(msg)?;
    arm(&mut c)?;
    Ok(c)
}

/// What SQLite's own guide to untrusted databases asks for. Defensive mode
/// refuses writes to the schema and to virtual tables' shadow tables, which
/// is how a crafted file corrupts itself further. `TRUSTED_SCHEMA` off means
/// a view or trigger cannot call a function with side effects. No attached
/// databases, because `ATTACH` counts as read-only to `stmt_readonly`
/// and would otherwise let the SQL box open, or create, any file this user
/// can reach.
fn harden(c: &mut Connection) -> Result<(), String> {
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(msg)?;
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false).map_err(msg)?;
    c.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0).map_err(msg)?;
    // Off, so a double-quoted name is always a name. With SQLite's legacy
    // default, `"rowid"` on a table without one silently became the string
    // 'rowid', and `rowid_alias` called a WITHOUT ROWID table editable.
    // Only DML: a schema that wrote a default as "x" must still load.
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false).map_err(msg)?;
    Ok(())
}

/// A fresh `DEADLINE` for the next statement.
fn arm(c: &mut Connection) -> Result<(), String> {
    let until = Instant::now() + DEADLINE;
    c.progress_handler(1000, Some(move || Instant::now() > until)).map_err(msg)
}

fn msg(e: rusqlite::Error) -> String {
    if let rusqlite::Error::SqliteFailure(f, _) = &e {
        if f.code == rusqlite::ErrorCode::OperationInterrupted {
            return format!("stopped after {} s", DEADLINE.as_secs());
        }
    }
    e.to_string()
}

/// `"name"`, with any `"` doubled. Only ever applied to a name already found
/// in the schema.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Debug, Clone)]
struct Column {
    name: String,
    ty: String,
    pk: bool,
    notnull: bool,
    /// A generated column: shown, never written.
    generated: bool,
}

/// `("table"|"view")` for a name in the schema, else refused. The lookup is
/// the confinement: nothing the browser sends reaches SQL text unless it is
/// the exact name of something that exists.
fn kind_of(c: &Connection, table: &str) -> Result<String, String> {
    c.query_row(
        "SELECT type FROM sqlite_schema WHERE type IN ('table','view') AND name = ?1",
        [table],
        |r| r.get::<_, String>(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => format!("no table or view named {table}"),
        e => msg(e),
    })
}

fn columns(c: &Connection, table: &str) -> Result<Vec<Column>, String> {
    // A pragma's argument cannot be bound, but its table-valued form can.
    let mut st = c
        .prepare("SELECT name, type, pk, \"notnull\", hidden FROM pragma_table_xinfo(?1) ORDER BY cid")
        .map_err(msg)?;
    let cols = st
        .query_map([table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
        })
        .map_err(msg)?
        .filter_map(Result::ok)
        // hidden 1 is a virtual table's hidden column, which `*` leaves out
        // too; 2 and 3 are generated columns, which are real and shown.
        .filter(|(_, _, _, _, hidden)| *hidden != 1)
        .map(|(name, ty, pk, notnull, hidden)| Column { name, ty, pk: pk > 0, notnull: notnull != 0, generated: hidden >= 2 })
        .collect::<Vec<_>>();
    if cols.is_empty() {
        return Err(format!("{table} has no columns roost can read"));
    }
    Ok(cols)
}

/// The name that reaches this table's rowid, or `None` for a view or a
/// `WITHOUT ROWID` table. A column may itself be called `rowid`, which hides
/// that alias, so the three are tried in turn and SQLite decides: preparing
/// the select fails when the name is not the rowid. That needs DQS off
/// (`harden`); revert-checked: with it on, the test listing tables fails on
/// `kv` being editable.
fn rowid_alias(c: &Connection, table: &str, kind: &str, cols: &[Column]) -> Option<&'static str> {
    if kind != "table" {
        return None;
    }
    ["rowid", "_rowid_", "oid"]
        .into_iter()
        .filter(|a| !cols.iter().any(|col| col.name.eq_ignore_ascii_case(a)))
        .find(|a| c.prepare(&format!("SELECT {} FROM {} LIMIT 0", quote(a), quote(table))).is_ok())
}

fn tables(c: &Connection) -> Result<Reply, String> {
    let mut st = c
        .prepare("SELECT type, name FROM sqlite_schema WHERE type IN ('table','view') ORDER BY name")
        .map_err(msg)?;
    let list: Vec<(String, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(msg)?.filter_map(Result::ok).collect();
    let out: Vec<Value> = list
        .into_iter()
        .map(|(kind, name)| {
            let editable = columns(c, &name).ok().and_then(|cols| rowid_alias(c, &name, &kind, &cols)).is_some();
            json!({ "name": name, "kind": kind, "editable": editable })
        })
        .collect();
    Ok(Reply::ok(json!({ "tables": out })))
}

/// One cell as it goes to the browser. JSON has one number type and the
/// browser holds it as a double, so an integer past 2^53 goes as a string
/// in `{"int": …}` rather than silently becoming its neighbour; likewise a
/// real JSON cannot spell (±inf). Text over `TEXT_CAP` is cut and says so.
fn cell(v: ValueRef<'_>) -> Value {
    const SAFE: i64 = 1 << 53;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) if (-SAFE..=SAFE).contains(&i) => json!(i),
        ValueRef::Integer(i) => json!({ "int": i.to_string() }),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or_else(|| json!({ "real": f.to_string() })),
        ValueRef::Text(t) => {
            let s = String::from_utf8_lossy(t);
            if s.chars().count() > TEXT_CAP {
                json!({ "text": s.chars().take(TEXT_CAP).collect::<String>(), "len": s.chars().count() })
            } else {
                Value::String(s.into_owned())
            }
        }
        ValueRef::Blob(b) => json!({ "blob": b.len() }),
    }
}

/// A hash of a row's full values, types included, so `1` and `'1'` differ
/// and a blob that changed in a byte the grid never showed still differs.
fn tag_of(vals: &[ValueRef<'_>]) -> String {
    let mut h = ring::digest::Context::new(&ring::digest::SHA256);
    for v in vals {
        let (t, bytes): (u8, Vec<u8>) = match *v {
            ValueRef::Null => (0, vec![]),
            ValueRef::Integer(i) => (1, i.to_le_bytes().to_vec()),
            ValueRef::Real(f) => (2, f.to_bits().to_le_bytes().to_vec()),
            ValueRef::Text(t) => (3, t.to_vec()),
            ValueRef::Blob(b) => (4, b.to_vec()),
        };
        h.update(&[t]);
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    h.finish().as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn select_list(cols: &[Column]) -> String {
    cols.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ")
}

fn rows(c: &mut Connection, table: &str, offset: i64, order: Option<&str>, desc: bool) -> Result<Reply, String> {
    let kind = kind_of(c, table)?;
    let cols = columns(c, table)?;
    let alias = rowid_alias(c, table, &kind, &cols);
    let dir = if desc { "DESC" } else { "ASC" };
    let order_sql = match order {
        Some(o) => {
            let col = cols.iter().find(|c| c.name == o).ok_or_else(|| format!("{table} has no column {o}"))?;
            // The rowid as a tie-break, so a page boundary inside a run of
            // equal values does not show a row twice or skip one.
            match alias {
                Some(a) => format!(" ORDER BY {} {dir}, {}", quote(&col.name), quote(a)),
                None => format!(" ORDER BY {} {dir}", quote(&col.name)),
            }
        }
        None => alias.map(|a| format!(" ORDER BY {} {dir}", quote(a))).unwrap_or_default(),
    };
    let offset = offset.max(0);
    let lead = alias.map(|a| format!("{}, ", quote(a))).unwrap_or_default();
    let sql = format!("SELECT {lead}{} FROM {}{order_sql} LIMIT ?1 OFFSET ?2", select_list(&cols), quote(table));
    let skip = usize::from(alias.is_some());
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut cut = false;
    {
        let mut st = c.prepare(&sql).map_err(msg)?;
        let mut rs = st.query([PAGE, offset]).map_err(msg)?;
        while let Some(r) = rs.next().map_err(msg)? {
            let vals: Vec<ValueRef<'_>> = (skip..skip + cols.len()).map(|i| r.get_ref(i)).collect::<Result<_, _>>().map_err(msg)?;
            let row = json!({
                "id": if skip == 1 { r.get::<_, i64>(0).ok() } else { None },
                "tag": tag_of(&vals),
                "cells": vals.iter().map(|v| cell(*v)).collect::<Vec<_>>(),
            });
            bytes += row.to_string().len();
            if bytes > PAGE_BYTES && !out.is_empty() {
                cut = true;
                break;
            }
            out.push(row);
        }
    }
    // Its own deadline: a count over a huge table is the slow part, and
    // running out of time on it says nothing about how many rows there are,
    // so it is reported as unknown rather than as zero.
    arm(c)?;
    let total: Option<i64> = c.query_row(&format!("SELECT count(*) FROM {}", quote(table)), [], |r| r.get(0)).ok();
    Ok(Reply::ok(json!({
        "table": table,
        "kind": kind,
        "editable": alias.is_some(),
        "columns": cols.iter().map(|c| json!({
            "name": c.name, "type": c.ty, "pk": c.pk, "notnull": c.notnull, "generated": c.generated,
        })).collect::<Vec<_>>(),
        "rows": out,
        "offset": offset,
        "page": PAGE,
        "total": total,
        "cut": cut,
    })))
}

fn query(c: &mut Connection, sql: &str) -> Result<Reply, String> {
    if sql.trim().is_empty() {
        return Err("nothing to run".into());
    }
    let mut st = c.prepare(sql).map_err(msg)?;
    // On top of the read-only connection: this also refuses `VACUUM INTO`,
    // which writes a new file and is not a write to this database.
    if !st.readonly() {
        return Err("The SQL box only reads. Edit rows in the grid.".into());
    }
    let names: Vec<String> = st.column_names().into_iter().map(String::from).collect();
    let mut rs = st.query([]).map_err(msg)?;
    let mut out = Vec::new();
    let mut more = false;
    let mut bytes = 0usize;
    while let Some(r) = rs.next().map_err(msg)? {
        if out.len() == QUERY_ROWS || bytes > PAGE_BYTES {
            more = true;
            break;
        }
        let row: Vec<Value> = (0..names.len()).map(|i| r.get_ref(i).map(cell)).collect::<Result<_, _>>().map_err(msg)?;
        bytes += Value::Array(row.clone()).to_string().len();
        out.push(Value::Array(row));
    }
    Ok(Reply::ok(json!({ "columns": names, "rows": out, "more": more })))
}

/// The writable table `table` with its rowid alias, or why not.
fn writable(c: &Connection, table: &str) -> Result<(Vec<Column>, &'static str), String> {
    let kind = kind_of(c, table)?;
    let cols = columns(c, table)?;
    match rowid_alias(c, table, &kind, &cols) {
        Some(a) => Ok((cols, a)),
        None if kind == "view" => Err(format!("{table} is a view; views are read-only here")),
        None => Err(format!("{table} has no rowid; it is read-only here")),
    }
}

/// Inside the caller's transaction: is row `rowid` still the one whose tag
/// the user saw?
fn still(c: &Connection, table: &str, cols: &[Column], alias: &str, rowid: i64, tag: &str) -> Result<bool, String> {
    let sql = format!("SELECT {} FROM {} WHERE {} = ?1", select_list(cols), quote(table), quote(alias));
    let mut st = c.prepare(&sql).map_err(msg)?;
    let mut rs = st.query([rowid]).map_err(msg)?;
    match rs.next().map_err(msg)? {
        None => Ok(false),
        Some(r) => {
            let vals: Vec<ValueRef<'_>> = (0..cols.len()).map(|i| r.get_ref(i)).collect::<Result<_, _>>().map_err(msg)?;
            Ok(tag_of(&vals) == tag)
        }
    }
}

fn update(c: &mut Connection, table: &str, rowid: i64, tag: &str, col: &str, value: Option<String>) -> Result<Reply, String> {
    let (cols, alias) = writable(c, table)?;
    let target = cols.iter().find(|x| x.name == col).ok_or_else(|| format!("{table} has no column {col}"))?;
    if target.generated {
        return Err(format!("{col} is generated; it cannot be set"));
    }
    // IMMEDIATE takes the write lock before the re-read, so nothing can slip
    // in between the check and the write.
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(msg)?;
    if !still(&tx, table, &cols, alias, rowid, tag)? {
        return Ok(Reply::stale());
    }
    // Bound as text (or NULL) and left to the column's affinity, so `42`
    // typed into an INTEGER column is stored as the integer 42.
    let sql = format!("UPDATE {} SET {} = ?1 WHERE {} = ?2", quote(table), quote(&target.name), quote(alias));
    tx.execute(&sql, rusqlite::params![value, rowid]).map_err(msg)?;
    tx.commit().map_err(msg)?;
    Ok(Reply::ok(json!({ "rowid": rowid })))
}

fn insert(c: &mut Connection, table: &str) -> Result<Reply, String> {
    writable(c, table)?;
    c.execute(&format!("INSERT INTO {} DEFAULT VALUES", quote(table)), []).map_err(msg)?;
    Ok(Reply::ok(json!({ "rowid": c.last_insert_rowid() })))
}

fn delete(c: &mut Connection, table: &str, rowid: i64, tag: &str) -> Result<Reply, String> {
    let (cols, alias) = writable(c, table)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(msg)?;
    if !still(&tx, table, &cols, alias, rowid, tag)? {
        return Ok(Reply::stale());
    }
    tx.execute(&format!("DELETE FROM {} WHERE {} = ?1", quote(table), quote(alias)), [rowid]).map_err(msg)?;
    tx.commit().map_err(msg)?;
    Ok(Reply::ok(json!({ "rowid": rowid })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("app.db");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '', age INTEGER, photo BLOB);
             CREATE TABLE kv(k TEXT PRIMARY KEY, v) WITHOUT ROWID;
             CREATE VIEW adults AS SELECT name FROM people WHERE age >= 18;
             CREATE TABLE \"we\"\"ird\"(x);
             INSERT INTO people(name, age, photo) VALUES ('Ana', 34, x'0102'), ('Bor', 9, NULL), ('Cene', 51, NULL);
             INSERT INTO kv VALUES ('a', 1);
             INSERT INTO \"we\"\"ird\" VALUES (1);",
        )
        .unwrap();
        (d, p)
    }

    fn rows_of(dir: &Path, table: &str) -> Value {
        let r = run(dir, "app.db", Req::Rows { table: table.into(), offset: 0, order: None, desc: false });
        assert!(r.ok, "{}", r.msg);
        r.data
    }

    fn row<'a>(data: &'a Value, id: i64) -> &'a Value {
        data["rows"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap()
    }

    #[test]
    fn tables_and_views_are_listed_and_only_rowid_tables_are_editable() {
        let (d, _) = fixture();
        let r = run(d.path(), "app.db", Req::Tables);
        assert!(r.ok, "{}", r.msg);
        let t: Vec<(String, String, bool)> = r.data["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["name"].as_str().unwrap().into(), t["kind"].as_str().unwrap().into(), t["editable"].as_bool().unwrap()))
            .collect();
        assert_eq!(
            t,
            vec![
                ("adults".into(), "view".into(), false),
                ("kv".into(), "table".into(), false),
                ("people".into(), "table".into(), true),
                ("we\"ird".into(), "table".into(), true),
            ]
        );
    }

    #[test]
    fn rows_come_typed_ordered_and_with_a_count() {
        let (d, _) = fixture();
        let data = rows_of(d.path(), "people");
        assert_eq!(data["total"], 3);
        assert_eq!(data["editable"], true);
        let names: Vec<&str> = data["columns"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["id", "name", "age", "photo"]);
        let ana = row(&data, 1);
        assert_eq!(ana["cells"], json!([1, "Ana", 34, { "blob": 2 }]));
        let r = run(d.path(), "app.db", Req::Rows { table: "people".into(), offset: 0, order: Some("age".into()), desc: true });
        let ages: Vec<i64> = r.data["rows"].as_array().unwrap().iter().map(|r| r["cells"][2].as_i64().unwrap()).collect();
        assert_eq!(ages, [51, 34, 9]);
        let r = run(d.path(), "app.db", Req::Rows { table: "people".into(), offset: 2, order: None, desc: false });
        assert_eq!(r.data["rows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_page_holds_at_most_page_rows() {
        let (d, p) = fixture();
        let c = Connection::open(&p).unwrap();
        c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 250) INSERT INTO people(name) SELECT 'p'||i FROM n;").unwrap();
        let data = rows_of(d.path(), "people");
        assert_eq!(data["rows"].as_array().unwrap().len() as i64, PAGE);
        assert_eq!(data["total"], 253);
    }

    #[test]
    fn a_view_and_a_without_rowid_table_read_but_do_not_edit() {
        let (d, _) = fixture();
        let v = rows_of(d.path(), "adults");
        assert_eq!(v["editable"], false);
        assert_eq!(v["rows"].as_array().unwrap().len(), 2);
        assert!(v["rows"][0]["id"].is_null());
        let r = run(d.path(), "app.db", Req::Insert { table: "adults".into() });
        assert!(!r.ok && r.msg.contains("view"), "{}", r.msg);
        let r = run(d.path(), "app.db", Req::Insert { table: "kv".into() });
        assert!(!r.ok && r.msg.contains("no rowid"), "{}", r.msg);
    }

    /// Revert-checked: with `kind_of`'s lookup skipped the first assert
    /// fails, since the name goes on to the column lookup and the error is
    /// not "no table or view named"; with `rows`' column lookup skipped,
    /// the quoted `order` reaches SQL and SQLite's own error comes back
    /// instead of "has no column".
    #[test]
    fn names_from_the_browser_must_exist() {
        let (d, _) = fixture();
        let r = run(d.path(), "app.db", Req::Rows { table: "people; DROP TABLE kv".into(), offset: 0, order: None, desc: false });
        assert!(!r.ok && r.msg.starts_with("no table or view named"), "{}", r.msg);
        let r = run(d.path(), "app.db", Req::Rows { table: "people".into(), offset: 0, order: Some("age; --".into()), desc: false });
        assert!(!r.ok && r.msg.contains("has no column"), "{}", r.msg);
        let r = run(d.path(), "app.db", Req::Update {
            table: "people".into(), rowid: 1, tag: "x".into(), col: "nope".into(), value: None,
        });
        assert!(!r.ok && r.msg.contains("has no column"), "{}", r.msg);
        // A name that needs quoting works when it does exist.
        assert_eq!(rows_of(d.path(), "we\"ird")["rows"][0]["cells"], json!([1]));
    }

    #[test]
    fn an_update_lands_through_affinity_and_moves_the_tag() {
        let (d, p) = fixture();
        let before = rows_of(d.path(), "people");
        let tag = row(&before, 2)["tag"].as_str().unwrap().to_string();
        let r = run(d.path(), "app.db", Req::Update {
            table: "people".into(), rowid: 2, tag: tag.clone(), col: "age".into(), value: Some("10".into()),
        });
        assert!(r.ok, "{}", r.msg);
        let c = Connection::open(&p).unwrap();
        let (age, ty): (i64, String) = c.query_row("SELECT age, typeof(age) FROM people WHERE id=2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((age, ty.as_str()), (10, "integer"));
        let after = rows_of(d.path(), "people");
        assert_ne!(row(&after, 2)["tag"].as_str().unwrap(), tag);
        // NULL is a value too.
        let tag = row(&after, 2)["tag"].as_str().unwrap().to_string();
        let r = run(d.path(), "app.db", Req::Update { table: "people".into(), rowid: 2, tag, col: "age".into(), value: None });
        assert!(r.ok, "{}", r.msg);
        assert!(row(&rows_of(d.path(), "people"), 2)["cells"][2].is_null());
    }

    /// Revert-checked: with `still` short-circuited to `Ok(true)`, the
    /// update goes through and overwrites the other writer's 'Bora' — the
    /// first assert fails.
    #[test]
    fn an_edit_to_a_row_that_changed_since_is_refused_as_stale() {
        let (d, p) = fixture();
        let tag = row(&rows_of(d.path(), "people"), 2)["tag"].as_str().unwrap().to_string();
        // The app owning the database writes in between.
        Connection::open(&p).unwrap().execute("UPDATE people SET name='Bora' WHERE id=2", []).unwrap();
        let r = run(d.path(), "app.db", Req::Update {
            table: "people".into(), rowid: 2, tag: tag.clone(), col: "age".into(), value: Some("11".into()),
        });
        assert!(!r.ok && r.stale, "{r:?}");
        let c = Connection::open(&p).unwrap();
        let age: i64 = c.query_row("SELECT age FROM people WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(age, 9, "the stale edit did not land");
        // A change in a blob byte the grid never shows still counts.
        let tag = row(&rows_of(d.path(), "people"), 1)["tag"].as_str().unwrap().to_string();
        c.execute("UPDATE people SET photo=x'0103' WHERE id=1", []).unwrap();
        let r = run(d.path(), "app.db", Req::Delete { table: "people".into(), rowid: 1, tag });
        assert!(r.stale, "{r:?}");
        let n: i64 = c.query_row("SELECT count(*) FROM people", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn insert_and_delete() {
        let (d, p) = fixture();
        let r = run(d.path(), "app.db", Req::Insert { table: "people".into() });
        assert!(r.ok, "{}", r.msg);
        let id = r.data["rowid"].as_i64().unwrap();
        assert_eq!(id, 4);
        let tag = row(&rows_of(d.path(), "people"), id)["tag"].as_str().unwrap().to_string();
        let r = run(d.path(), "app.db", Req::Delete { table: "people".into(), rowid: id, tag });
        assert!(r.ok, "{}", r.msg);
        let n: i64 = Connection::open(&p).unwrap().query_row("SELECT count(*) FROM people", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_generated_column_shows_but_does_not_take_a_value() {
        let (d, p) = fixture();
        Connection::open(&p).unwrap().execute_batch("CREATE TABLE g(a INTEGER, b INTEGER AS (a*2)); INSERT INTO g(a) VALUES (3);").unwrap();
        let data = rows_of(d.path(), "g");
        assert_eq!(data["rows"][0]["cells"], json!([3, 6]));
        assert_eq!(data["columns"][1]["generated"], true);
        let tag = data["rows"][0]["tag"].as_str().unwrap().to_string();
        let r = run(d.path(), "app.db", Req::Update { table: "g".into(), rowid: 1, tag, col: "b".into(), value: Some("1".into()) });
        assert!(!r.ok && r.msg.contains("generated"), "{}", r.msg);
    }

    #[test]
    fn the_sql_box_reads() {
        let (d, _) = fixture();
        let r = run(d.path(), "app.db", Req::Query { sql: "SELECT name, age FROM people WHERE age > 30 ORDER BY age".into() });
        assert!(r.ok, "{}", r.msg);
        assert_eq!(r.data["columns"], json!(["name", "age"]));
        assert_eq!(r.data["rows"], json!([["Ana", 34], ["Cene", 51]]));
        assert_eq!(r.data["more"], false);
    }

    /// Revert-checked: without the `readonly()` check the UPDATE still fails
    /// (the connection is read-only), but with the message "attempt to write
    /// a readonly database", and `VACUUM INTO` succeeds and writes `copy.db`;
    /// without `SQLITE_LIMIT_ATTACHED` the ATTACH of a database outside the
    /// project succeeds. A read-only connection alone refuses only a file
    /// that does not exist yet.
    #[test]
    fn the_sql_box_refuses_writes_new_files_and_two_statements() {
        let (d, p) = fixture();
        let r = run(d.path(), "app.db", Req::Query { sql: "UPDATE people SET age = 0".into() });
        assert!(!r.ok && r.msg.contains("only reads"), "{}", r.msg);
        let copy = d.path().join("copy.db");
        let r = run(d.path(), "app.db", Req::Query { sql: format!("VACUUM INTO '{}'", copy.display()) });
        assert!(!r.ok && r.msg.contains("only reads"), "{}", r.msg);
        assert!(!copy.exists(), "VACUUM INTO wrote a file");
        // An existing file, outside the project: read-only would open it.
        let (_elsewhere, other) = fixture();
        let r = run(d.path(), "app.db", Req::Query { sql: format!("ATTACH '{}' AS o", other.display()) });
        assert!(!r.ok && r.msg.contains("too many attached"), "{r:?}");
        let r = run(d.path(), "app.db", Req::Query { sql: "SELECT 1; DELETE FROM people".into() });
        assert!(!r.ok && r.msg.contains("Multiple statements"), "{}", r.msg);
        let n: i64 = Connection::open(&p).unwrap().query_row("SELECT count(*) FROM people", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    /// Revert-checked: with `arm` not installing the handler this runs until
    /// the recursion's billion rows are counted, far past the bound below.
    #[test]
    fn a_runaway_query_is_stopped_at_the_deadline() {
        let (d, _) = fixture();
        let t = Instant::now();
        let r = run(d.path(), "app.db", Req::Query {
            sql: "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 1000000000) SELECT count(*) FROM n".into(),
        });
        assert!(!r.ok && r.msg.contains("stopped after"), "{}", r.msg);
        assert!(t.elapsed() < DEADLINE + Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn long_text_is_cut_and_big_integers_keep_their_digits() {
        assert_eq!(cell(ValueRef::Integer(9_007_199_254_740_993)), json!({ "int": "9007199254740993" }));
        assert_eq!(cell(ValueRef::Integer(42)), json!(42));
        let long = "é".repeat(TEXT_CAP + 5);
        let c = cell(ValueRef::Text(long.as_bytes()));
        assert_eq!(c["len"], TEXT_CAP + 5);
        assert_eq!(c["text"].as_str().unwrap().chars().count(), TEXT_CAP);
        assert_eq!(cell(ValueRef::Real(f64::INFINITY)), json!({ "real": "inf" }));
    }

    #[test]
    fn a_tag_tells_types_apart() {
        assert_ne!(tag_of(&[ValueRef::Integer(1)]), tag_of(&[ValueRef::Text(b"1")]));
        assert_ne!(tag_of(&[ValueRef::Text(b"ab"), ValueRef::Text(b"")]), tag_of(&[ValueRef::Text(b"a"), ValueRef::Text(b"b")]));
    }

    /// Revert-checked: without the header check, `notes.db` opens (SQLite
    /// reads it lazily) and the error is SQLite's "file is not a database"
    /// — the message assert fails; without the file-type check the FIFO is
    /// opened for its header and the test hangs.
    #[test]
    fn only_a_real_database_inside_the_project_opens() {
        let (d, _) = fixture();
        std::fs::write(d.path().join("notes.db"), "just text, not SQLite at all").unwrap();
        let r = run(d.path(), "notes.db", Req::Tables);
        assert_eq!(r.msg, "not a SQLite database");
        std::fs::write(d.path().join("empty.db"), "").unwrap();
        assert_eq!(run(d.path(), "empty.db", Req::Tables).msg, "not a SQLite database");
        assert_eq!(std::fs::metadata(d.path().join("empty.db")).unwrap().len(), 0, "an empty file stays empty");
        let fifo = d.path().join("pipe.db");
        let cpath = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert_eq!(run(d.path(), "pipe.db", Req::Tables).msg, "not a regular file");
        let (_d2, real) = fixture();
        std::os::unix::fs::symlink(&real, d.path().join("link.db")).unwrap();
        let r = run(d.path(), "link.db", Req::Tables);
        assert!(!r.ok, "a symlink out of the project opened: {r:?}");
        assert!(!run(d.path(), "../app.db", Req::Tables).ok);
        assert!(!run(d.path(), "missing.db", Req::Tables).ok);
        assert!(!d.path().join("missing.db").exists(), "a missing database is not created");
    }

    #[test]
    fn the_request_shapes_decode() {
        let r: Req = serde_json::from_str(r#"{"op":"rows","table":"t","order":"a","desc":true}"#).unwrap();
        assert_eq!(r, Req::Rows { table: "t".into(), offset: 0, order: Some("a".into()), desc: true });
        let r: Req = serde_json::from_str(r#"{"op":"update","table":"t","rowid":3,"tag":"ab","col":"c","value":null}"#).unwrap();
        assert_eq!(r, Req::Update { table: "t".into(), rowid: 3, tag: "ab".into(), col: "c".into(), value: None });
        assert!(serde_json::from_str::<Req>(r#"{"op":"drop"}"#).is_err());
    }
}
