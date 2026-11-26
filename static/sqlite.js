// The SQLite tab (src/sqlite.rs, render::sqlite_fragment).
//
// The fragment is a shell. Everything in it comes from `Intent::Sql`
// requests over the workspace socket, answered by `Event::Sql` with the same
// `seq`. Every value from the database reaches the DOM through textContent:
// a database from a cloned repository can hold `<img onerror=…>` in any
// cell, and this page holds the socket that spawns shells.
//
// Edits carry the row's `tag`, a hash the server took of the row's values
// when the page was read. If the row changed since, the server writes
// nothing and answers `stale`, and the page reloads to show what is there
// now. So an app writing to its own database while you look is never
// silently overwritten.

const sqlPending = new Map();
let sqlSeq = 0;

/// One request, as a promise of `{ok, msg, stale, data}`. Rejects only when
/// the socket is down or no answer comes; a refusal resolves with ok:false.
function sqlReq(rel, req) {
  const seq = ++sqlSeq;
  return new Promise((resolve, reject) => {
    if (!send({ t: "Sql", rel, seq, req })) { reject(new Error("not connected")); return; }
    // The server's own deadline is 2 s a statement; this only catches an
    // answer lost to a reconnect.
    const timer = setTimeout(() => { sqlPending.delete(seq); reject(new Error("no answer from roost")); }, 20000);
    sqlPending.set(seq, (ev) => { clearTimeout(timer); resolve(ev); });
  });
}

/// Called by app.js for every `Sql` event.
function sqlEvent(ev) {
  const done = sqlPending.get(ev.seq);
  if (!done) return;
  sqlPending.delete(ev.seq);
  done(ev);
}

/// How a cell reads in the grid, and whether it may be edited in place.
/// See sqlite::cell for the shapes.
function sqlCellText(v) {
  if (v === null) return { text: "NULL", nul: true, full: true };
  if (typeof v === "number") return { text: String(v), full: true };
  if (typeof v === "string") return { text: v, full: true };
  if ("int" in v) return { text: v.int, full: true };
  if ("real" in v) return { text: v.real, full: true };
  if ("blob" in v) return { text: `‹blob ${v.blob} B›`, full: false, blob: true };
  if ("text" in v) return { text: v.text + "…", full: false, cut: v.len };
  return { text: "?", full: false };
}

function sqlTableKey(rel) { return `roost.sql.${PROJECT}.${rel}`; }

/// Wire one `.sqlview` freshly put on the page.
function mountSqlite(root) {
  const rel = root.dataset.rel;
  const $ = (s) => root.querySelector(s);
  const pick = $(".sqltables"), grid = $(".sqlgrid"), msg = $(".sqlmsg");
  const insertBtn = $(".sqlinsert"), qbtn = $(".sqlqbtn"), qform = $(".sqlquery");
  const pager = $(".sqlpager"), range = $(".sqlrange"), prev = $(".sqlprev"), next = $(".sqlnext");
  const v = { table: null, offset: 0, order: null, desc: false, data: null, mark: null };

  const say = (text, bad) => { msg.textContent = text || ""; msg.classList.toggle("bad", !!bad); };

  async function loadTables() {
    let r;
    try { r = await sqlReq(rel, { op: "tables" }); } catch (e) { say(e.message, true); return; }
    if (!r.ok) { say(r.msg, true); return; }
    pick.replaceChildren();
    for (const t of r.data.tables) {
      const o = document.createElement("option");
      o.value = t.name;
      o.textContent = t.kind === "view" ? `${t.name} (view)` : t.name;
      pick.appendChild(o);
    }
    if (!r.data.tables.length) {
      say("This database has no tables yet.");
      grid.replaceChildren();
      pager.hidden = true;
      return;
    }
    let want = null;
    try { want = localStorage.getItem(sqlTableKey(rel)); } catch { /* private mode */ }
    v.table = r.data.tables.some((t) => t.name === want) ? want : r.data.tables[0].name;
    pick.value = v.table;
    loadRows();
  }

  async function loadRows() {
    if (v.table === null) return;
    const asked = { op: "rows", table: v.table, offset: v.offset, order: v.order, desc: v.desc };
    let r;
    try { r = await sqlReq(rel, asked); } catch (e) { say(e.message, true); return; }
    if (asked.table !== v.table) return; // the picker moved on meanwhile
    if (!r.ok) { say(r.msg, true); return; }
    v.data = r.data;
    renderRows();
  }

  function headCell(c, sortable) {
    const th = document.createElement("th");
    const name = document.createElement("span");
    name.textContent = c.name;
    th.appendChild(name);
    if (c.type) {
      const ty = document.createElement("small");
      ty.textContent = c.type.toLowerCase() + (c.pk ? " · pk" : "");
      th.appendChild(ty);
    }
    if (sortable) {
      th.classList.add("sort");
      if (v.order === c.name) th.dataset.dir = v.desc ? "desc" : "asc";
      // None, then ascending, then descending, then none again.
      th.onclick = () => {
        if (v.order !== c.name) { v.order = c.name; v.desc = false; }
        else if (!v.desc) v.desc = true;
        else { v.order = null; v.desc = false; }
        v.offset = 0;
        loadRows();
      };
    }
    return th;
  }

  function bodyCell(val) {
    const td = document.createElement("td");
    const c = sqlCellText(val);
    td.textContent = c.text;
    if (c.nul) td.classList.add("nul");
    if (typeof val === "number") td.classList.add("num");
    return [td, c];
  }

  function renderRows() {
    const d = v.data;
    insertBtn.hidden = !d.editable;
    const table = document.createElement("table");
    const head = document.createElement("tr");
    for (const c of d.columns) head.appendChild(headCell(c, true));
    table.createTHead().appendChild(head);
    const body = table.createTBody();
    for (const row of d.rows) {
      const tr = document.createElement("tr");
      if (v.mark !== null && row.id === v.mark) tr.classList.add("fresh");
      row.cells.forEach((val, i) => {
        const [td, c] = bodyCell(val);
        const col = d.columns[i];
        const canEdit = d.editable && row.id !== null && c.full && !col.generated;
        if (canEdit) td.classList.add("edit");
        td.onclick = () => editCell(row, col, val, c, canEdit);
        tr.appendChild(td);
      });
      body.appendChild(tr);
    }
    grid.replaceChildren(table);
    grid.scrollTop = 0;
    const shown = d.rows.length;
    pager.hidden = false;
    const total = d.total === null ? "?" : String(d.total);
    range.textContent = shown ? `${d.offset + 1}–${d.offset + shown} of ${total}` : `0 of ${total}`;
    prev.disabled = d.offset === 0;
    next.disabled = d.cut ? false : d.total === null ? shown < d.page : d.offset + shown >= d.total;
    const notes = [];
    if (!d.editable) notes.push(d.kind === "view" ? "A view: read-only." : "No rowid: read-only.");
    if (d.cut) notes.push("Rows are large; this page holds fewer.");
    say(notes.join(" "));
  }

  async function editCell(row, col, val, c, canEdit) {
    const dlg = document.getElementById("dlg-sqlcell");
    const area = dlg.querySelector("#dlg-sqlcell-text");
    const nul = dlg.querySelector(".sqlnullbox");
    const answer = await runDialog(dlg, (finish) => {
      dlg.querySelector(".dlg-title").textContent = col.name;
      const lab = dlg.querySelector(".dlg-label");
      lab.textContent = [col.type && col.type.toLowerCase(), col.pk && "primary key",
        col.generated && "generated", c.blob && "blob, not shown",
        c.cut && `${c.cut} characters, first ${c.text.length - 1} shown`,
        !canEdit && !c.blob && !c.cut && !col.generated && "read-only"].filter(Boolean).join(" · ");
      lab.hidden = !lab.textContent;
      area.value = c.nul ? "" : c.text;
      area.readOnly = !canEdit;
      nul.checked = !!c.nul;
      nul.disabled = !canEdit;
      dlg.querySelector(".sqlnull").hidden = !canEdit && !c.nul;
      nul.onchange = () => { area.disabled = nul.checked; };
      area.disabled = nul.checked;
      const ok = dlg.querySelector(".dlg-ok");
      ok.hidden = !canEdit;
      ok.onclick = () => finish({ value: nul.checked ? null : area.value });
      const del = dlg.querySelector(".sqldelrow");
      del.hidden = !(v.data && v.data.editable && row.id !== null);
      del.onclick = () => finish({ del: true });
      dlg.querySelector(".dlg-cancel").onclick = () => finish(null);
      return () => { if (canEdit) area.focus(); };
    }, null);
    if (!answer) return;
    const table = v.table;
    if (answer.del) {
      const sure = await askConfirm({
        title: `Delete this row from ${table}?`,
        lines: [`rowid ${row.id}. This writes to the database file and cannot be undone here.`],
        confirm: "Delete", danger: true,
      });
      if (!sure) return;
      return write({ op: "delete", table, rowid: row.id, tag: row.tag }, "Row deleted.");
    }
    if (!c.nul && answer.value === c.text) return;
    write({ op: "update", table, rowid: row.id, tag: row.tag, col: col.name, value: answer.value }, "Saved.");
  }

  async function write(req, done) {
    let r;
    try { r = await sqlReq(rel, req); } catch (e) { say(e.message, true); return; }
    if (r.ok) {
      if (req.op === "insert") {
        // Newest first, so the row just added is in view to fill in.
        v.order = null; v.desc = true; v.offset = 0;
      }
      v.mark = r.data && typeof r.data.rowid === "number" ? r.data.rowid : null;
      await loadRows();
      say(done);
      return;
    }
    // Stale or refused: either way, show what the database holds now.
    await loadRows();
    say(r.msg, true);
  }

  pick.onchange = () => {
    v.table = pick.value; v.offset = 0; v.order = null; v.desc = false; v.mark = null;
    try { localStorage.setItem(sqlTableKey(rel), v.table); } catch { /* private mode */ }
    showQuery(false);
    loadRows();
  };
  $(".sqlreload").onclick = () => { v.mark = null; if (qbtn.getAttribute("aria-pressed") === "true") runQuery(); else loadTables(); };
  insertBtn.onclick = () => write({ op: "insert", table: v.table }, "Row added; tap a cell to fill it in.");
  prev.onclick = () => { v.offset = Math.max(0, v.offset - v.data.page); loadRows(); };
  next.onclick = () => { v.offset += v.data.rows.length; loadRows(); };

  function showQuery(on) {
    qbtn.setAttribute("aria-pressed", on ? "true" : "false");
    qform.hidden = !on;
    if (!on && v.data) renderRows();
  }
  qbtn.onclick = () => {
    const on = qbtn.getAttribute("aria-pressed") !== "true";
    showQuery(on);
    if (on) {
      const box = $(".sqlsql");
      if (!box.value && v.table !== null) box.value = `SELECT * FROM "${v.table.replace(/"/g, '""')}" LIMIT 50`;
      box.focus();
    }
  };
  async function runQuery() {
    const sql = $(".sqlsql").value;
    let r;
    try { r = await sqlReq(rel, { op: "query", sql }); } catch (e) { say(e.message, true); return; }
    if (!r.ok) { say(r.msg, true); return; }
    const table = document.createElement("table");
    const head = document.createElement("tr");
    for (const name of r.data.columns) head.appendChild(headCell({ name }, false));
    table.createTHead().appendChild(head);
    const body = table.createTBody();
    for (const row of r.data.rows) {
      const tr = document.createElement("tr");
      for (const val of row) {
        const [td, c] = bodyCell(val);
        td.onclick = () => editCell({ id: null }, { name: "result" }, val, c, false);
        tr.appendChild(td);
      }
      body.appendChild(tr);
    }
    grid.replaceChildren(table);
    pager.hidden = true;
    insertBtn.hidden = true;
    say(`${r.data.rows.length} row${r.data.rows.length === 1 ? "" : "s"}${r.data.more ? ", more not shown" : ""}.`);
  }
  qform.onsubmit = (e) => { e.preventDefault(); runQuery(); };
  // Ctrl/⌘-Enter runs; a bare Enter is a newline, since SQL spans lines.
  $(".sqlsql").onkeydown = (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); runQuery(); }
  };

  pager.hidden = true;
  loadTables();
}
