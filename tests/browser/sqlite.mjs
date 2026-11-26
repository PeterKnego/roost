//! A SQLite database clicked in the tree opens as a tab that shows, pages
//! and edits its rows.
//!
//! `cargo test` covers the server half (`src/sqlite.rs`): names looked up
//! rather than spliced, the stale-row refusal, the read-only SQL box, the
//! deadline, and refusing a file that is not a database. What only a browser
//! can show is the tab itself:
//! - the click opens it;
//! - values reach the page as text, never markup;
//! - an edit made through the dialog lands in the file;
//! - an edit to a row someone else changed meanwhile is refused and the page
//!   shows what is there now;
//! - a row can be added and deleted;
//! - the SQL box answers;
//! - at phone width the page does not scroll sideways and the controls are
//!   44 px.
//!
//! The database is made and checked with Python's `sqlite3`, which stands in
//! for "the app that owns this database", a second writer roost does not
//! control.
//!
//! Revert-checks performed, all restored:
//!   (a) `bodyCell` setting `innerHTML` instead of `textContent` -> A fails:
//!       the cell's `<img onerror>` runs and sets `window.__pwned`.
//!   (b) `write` not reloading after a refusal -> C fails: the grid still
//!       shows the old name after the stale refusal.
//!   (c) dropping the phone rule for `.sqlbar select` -> F fails on the
//!       44 px check.
//!
//! Run: deno run -A tests/browser/sqlite.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

/// Runs `code` with `db` as the open `sqlite3` connection `c`, and returns
/// what it prints.
async function py(db, code) {
  const r = await new Deno.Command("python3", {
    args: ["-I", "-c", `import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\n${code}\nc.commit()`, db],
    stdout: "piped", stderr: "piped",
  }).output();
  if (!r.success) throw new Error(new TextDecoder().decode(r.stderr));
  return new TextDecoder().decode(r.stdout).trim();
}

const fx = await fixture();
const db = `${fx.dir}/app.db`;
try {
  await py(db, `
c.executescript("""
CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '', age INTEGER);
CREATE TABLE kv(k TEXT PRIMARY KEY, v) WITHOUT ROWID;
INSERT INTO kv VALUES ('a', 1);
""")
c.execute("INSERT INTO people(name, age) VALUES (?, ?)", ('<img src=x onerror="window.__pwned=1">', 40))
c.executemany("INSERT INTO people(name, age) VALUES (?, ?)", [("p%d" % i, i) for i in range(2, 151)])`);
} catch (e) {
  console.log(`SKIP: python3 sqlite3 could not make the fixture (${e.message})`);
  await fx.cleanup();
  Deno.exit(0);
}

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector('a[data-rel="app.db"]')`), 20, "tree");

  const view = () => evalIn(`JSON.stringify((() => {
    const g = document.querySelector(".sqlview");
    if (!g) return null;
    return {
      tables: [...g.querySelectorAll(".sqltables option")].map((o) => o.value),
      table: g.querySelector(".sqltables").value,
      range: g.querySelector(".sqlrange").textContent,
      msg: g.querySelector(".sqlmsg").textContent,
      cols: [...g.querySelectorAll(".sqlgrid th span")].map((s) => s.textContent),
      rows: [...g.querySelectorAll(".sqlgrid tbody tr")].map((tr) => [...tr.cells].map((td) => td.textContent)),
      insert: !g.querySelector(".sqlinsert").hidden,
    };
  })())`).then(JSON.parse);
  const cellOf = (rowIdx, colIdx) =>
    `document.querySelectorAll(".sqlview .sqlgrid tbody tr")[${rowIdx}].cells[${colIdx}]`;
  /// Opens the dialog on a cell, types `text`, saves.
  async function editTo(rowIdx, colIdx, text) {
    await evalIn(`${cellOf(rowIdx, colIdx)}.click(); true`);
    await until(() => evalIn(`document.getElementById("dlg-sqlcell").open`), 5, "the cell dialog");
    await evalIn(`(() => { const d = document.getElementById("dlg-sqlcell");
      d.querySelector("#dlg-sqlcell-text").value = ${JSON.stringify(text)};
      d.querySelector(".dlg-ok").click(); })(); true`);
  }

  console.log("A. clicking a database in the tree opens its tables");
  ok(await evalIn(`defaultMode("app.db") === "Preview"`), "a database opens in Preview, not a textarea");
  await evalIn(`document.querySelector('a[data-rel="app.db"]').click(); true`);
  let v = null;
  await until(async () => { v = await view(); return !!v && v.rows.length > 0; }, 20, "the rows");
  ok(v && JSON.stringify(v.tables) === '["kv","people"]', `both tables are listed (${v && v.tables})`);
  ok(v && v.table === "kv", `the first is shown (${v && v.table})`);
  ok(v && !v.insert && /read-only/.test(v.msg), `a WITHOUT ROWID table says it is read-only (${v && v.msg})`);
  await evalIn(`(() => { const s = document.querySelector(".sqlview .sqltables"); s.value = "people";
    s.dispatchEvent(new Event("change")); })(); true`);
  await until(async () => { v = await view(); return v.cols[0] === "id"; }, 10, "people");
  ok(JSON.stringify(v.cols) === '["id","name","age"]', `its columns head the grid (${v.cols})`);
  ok(v.rows.length === 100 && v.range === "1–100 of 150", `a page of 100 of 150 (${v.rows.length}, ${v.range})`);
  ok(v.rows[0][1] === '<img src=x onerror="window.__pwned=1">', "a cell holding markup shows it as text");
  await sleep(300);
  ok(await evalIn(`!document.querySelector(".sqlgrid img") && window.__pwned === undefined`), "and does not run it");
  ok(await evalIn(`!document.querySelector('.path .modebtn')`), "no ✎: a database is not text to edit");

  console.log("\nB. paging, sorting, and an edit that lands in the file");
  await evalIn(`document.querySelector(".sqlview .sqlnext").click(); true`);
  ok(await until(async () => (await view()).range === "101–150 of 150", 10, "page 2"), "› shows the next page");
  await evalIn(`document.querySelector(".sqlview .sqlprev").click(); true`);
  await until(async () => (await view()).range === "1–100 of 150", 10, "page 1");
  await evalIn(`document.querySelectorAll(".sqlview .sqlgrid th")[2].click(); true`);
  await evalIn(`true`);
  await until(async () => (await view()).rows[0][0] === "2", 10, "sorted by age");
  await evalIn(`document.querySelectorAll(".sqlview .sqlgrid th")[2].click(); true`);
  ok(await until(async () => (await view()).rows[0][2] === "150", 10, "descending"), "a header sorts, then reverses");
  await evalIn(`document.querySelectorAll(".sqlview .sqlgrid th")[2].click(); true`);
  await until(async () => (await view()).rows[0][0] === "1", 10, "unsorted");
  await editTo(1, 1, "Zed");
  ok(await until(async () => (await view()).rows[1][1] === "Zed", 10, "the new name"), "the grid shows the edit");
  ok(await py(db, `print(c.execute("SELECT name FROM people WHERE id=2").fetchone()[0])`) === "Zed", "and it is in the file");
  await editTo(1, 2, "77");
  await until(async () => (await view()).rows[1][2] === "77", 10, "the new age");
  ok(await py(db, `print(c.execute("SELECT typeof(age) FROM people WHERE id=2").fetchone()[0])`) === "integer",
     "a number typed into an INTEGER column is stored as one");

  console.log("\nC. an edit to a row that changed meanwhile is refused");
  await py(db, `c.execute("UPDATE people SET name='FromTheApp' WHERE id=3")`);
  await editTo(2, 2, "1");
  ok(await until(async () => /changed since you looked/.test((await view()).msg), 10, "the refusal"), "the edit is refused as stale");
  v = await view();
  ok(v.rows[2][1] === "FromTheApp" && v.rows[2][2] === "3", `and the page shows the row as it is now (${v.rows[2]})`);
  ok(await py(db, `print(c.execute("SELECT age FROM people WHERE id=3").fetchone()[0])`) === "3", "the file kept the app's row");

  console.log("\nD. add a row, then delete it");
  await evalIn(`document.querySelector(".sqlview .sqlinsert").click(); true`);
  ok(await until(async () => { const w = await view(); return w.rows[0][0] === "151" && w.range.endsWith("of 151"); }, 10, "the new row"),
     "+ row adds one, shown first");
  ok(await evalIn(`document.querySelector(".sqlview .sqlgrid tbody tr").classList.contains("fresh")`), "and marked");
  await evalIn(`${cellOf(0, 1)}.click(); true`);
  await until(() => evalIn(`document.getElementById("dlg-sqlcell").open`), 5, "the cell dialog");
  await evalIn(`document.querySelector("#dlg-sqlcell .sqldelrow").click(); true`);
  ok(await until(() => evalIn(`document.getElementById("dlg-confirm").open`), 5, "confirm"), "delete asks first");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); true`);
  ok(await until(async () => (await view()).range.endsWith("of 150"), 10, "the row gone"), "and then deletes");
  ok(await py(db, `print(c.execute("SELECT count(*) FROM people").fetchone()[0])`) === "150", "from the file too");

  console.log("\nE. the SQL box");
  await evalIn(`document.querySelector(".sqlview .sqlqbtn").click(); true`);
  await evalIn(`document.querySelector(".sqlview .sqlsql").value = "SELECT count(*) AS n, max(age) AS oldest FROM people";
    document.querySelector(".sqlview .sqlrun").click(); true`);
  ok(await until(async () => { const w = await view(); return w.cols[0] === "n" && w.rows[0]?.[0] === "150"; }, 10, "the answer"),
     "a query answers in the grid");
  await evalIn(`document.querySelector(".sqlview .sqlsql").value = "DELETE FROM people";
    document.querySelector(".sqlview .sqlrun").click(); true`);
  ok(await until(async () => /only reads/.test((await view()).msg), 10, "refused"), "a write is refused");
  await evalIn(`document.querySelector(".sqlview .sqlqbtn").click(); true`);
  ok(await until(async () => (await view()).cols[0] === "id", 10, "the table again"), "SQL off shows the table again");

  console.log("\nF. on a phone");
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 800, deviceScaleFactor: 2, mobile: true });
  await evalIn(`revealPane(Number(document.querySelector(".sqlview").closest(".pane").dataset.pane)); true`);
  await sleep(400);
  const phone = JSON.parse(await evalIn(`JSON.stringify((() => {
    const g = document.querySelector(".sqlview .sqlgrid").getBoundingClientRect();
    const h = (s) => document.querySelector(".sqlview " + s).getBoundingClientRect().height;
    return { right: g.right, page: document.scrollingElement.scrollWidth, select: h(".sqltables"), next: h(".sqlnext"),
      scrolls: document.querySelector(".sqlview .sqlgrid").scrollWidth > document.querySelector(".sqlview .sqlgrid").clientWidth };
  })())`));
  ok(phone.right <= 391 && phone.page <= 390, `the grid stays inside the screen (right ${phone.right}, page ${phone.page})`);
  ok(phone.select >= 44 && phone.next >= 44, `the picker and pager are 44 px (${phone.select}, ${phone.next})`);
  await editTo(0, 2, "41");
  ok(await until(async () => (await view()).rows[0][2] === "41", 10, "the phone edit"), "an edit works at phone width");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
