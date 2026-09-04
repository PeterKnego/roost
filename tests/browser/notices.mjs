//! The bell panel is about *this* project, and the Clear button under it
//! means only what it shows.
//!
//! Worth a browser test because none of it is reachable from `cargo test`.
//! The server scoping has its own tests (notify.rs, hub.rs, integration.rs),
//! and every one of them can be right while the panel still renders another
//! project's rows, the badge still counts them, or Clear still sends an
//! intent that empties a project the user is not looking at. The last one is
//! the reason this file exists: a button that destroys what it does not
//! display is the shape CLAUDE.md's "destruction requires positive evidence"
//! is about, and only a real browser can press it.
//!
//! Two projects and two pages throughout, deliberately. With one project,
//! "shows only this project's notices" and "shows every notice" are the same
//! sentence, and every assertion here passes against the unscoped server —
//! the README's first trap wearing a different coat.
//!
//! Both notices are raised the production way: an OSC 777 sequence printed
//! by a real shell in a real dtach session, through osc.rs and hub::publish.
//! Nothing here calls `send({t:"..."})` to fake one into the store.
//!
//! Revert-the-fix, watched fail and restored. Two of the three found this
//! file asserting nothing, and both were fixed here rather than shrugged at:
//!
//!   1. `hub::publish` back to `broadcast_all` (notices go to every
//!      project's clients again): 8 failed — both "must not hold the
//!      other's", both badges, all three panel assertions, and "still holds
//!      its own notice before clearing".
//!   2. `wsconn`'s `notify::list_for` back to `list` (the connect replay
//!      ships the whole store): failed **nothing**. Both pages here connect
//!      before either notice exists, so their replay is empty whichever way
//!      the server scopes it — the connect path had no coverage at all.
//!      Section B2 exists because of this; with it, the same revert fails 1
//!      ("and not the other project's"), naming the leaked row.
//!   3. `ClearNotices` back to `notify::clear` (one project's Clear empties
//!      every project): also failed **nothing** at first. The server really
//!      did destroy other's history, but the rebroadcast that follows only
//!      reaches the *clearing* project's clients, so the page on `other`
//!      kept its stale array and went on rendering a row for a notice that
//!      no longer existed — the assertion was reading a client cache and
//!      calling it survival. Section E now asks a freshly opened page, and
//!      the same revert fails 2, both reporting an empty store.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, sleep, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// A second project beside the fixture's own. `roots` is scanned at request
// time, so creating it before the server starts is enough.
await Deno.mkdir(`${fx.roots}/other`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/other/hello.md`, "# other\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));

const wire = (page, project) => {
  const { evalIn } = page;
  const ready = async () => {
    const up = await until(
      () => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"),
      30,
      `app:${project}`,
    );
    // Defined here rather than inside raise(): the socket wait there reads
    // `__t()`, and an undefined helper throws inside the polled expression
    // instead of returning false — which reads as a timeout and buries the
    // real cause under two unrelated ones.
    await evalIn(`window.__t = () => [...terms.values()][0];
      window.__last = () => { const b = __t().term.buffer.active; let s = "";
        for (let y = 0; y <= b.baseY + b.cursorY; y++) s += b.getLine(y).translateToString(true) + "\\n"; return s; };`);
    return up;
  };
  // The badge, the panel and the array are read separately on purpose: the
  // badge is what the user sees from a background tab, the panel is what
  // they see when they look, and `notices` is what both derive from. A bug
  // that filtered only at render time would leave the badge counting rows
  // the panel refuses to show.
  const badge = () => evalIn(`document.getElementById("bellcount").textContent`);
  const stored = () => evalIn(`notices.map((n) => n.project + " " + n.body)`);
  const openPanel = async () => {
    await evalIn(`document.getElementById("noticepanel").hidden = true`);
    await evalIn(`document.getElementById("bell").click()`);
    await until(() => evalIn(`!document.getElementById("noticepanel").hidden`), 5, "panel");
  };
  const rows = () => evalIn(`[...document.querySelectorAll("#noticepanel .notice")].map((r) => r.textContent)`);
  const emptyNote = () => evalIn(`(document.querySelector("#noticepanel .notice-empty") || {}).textContent || ""`);
  // Through the real button. A test that sent ClearNotices itself would pass
  // with the footer wired to nothing at all.
  const clickClear = () => evalIn(`(() => {
    const b = [...document.querySelectorAll("#noticepanel .notice-foot button")].find((x) => x.textContent === "Clear");
    if (!b) return false; b.click(); return true; })()`);
  const clickFirstRow = () => evalIn(`(() => {
    const r = document.querySelector("#noticepanel .notice"); if (!r) return false; r.click(); return true; })()`);
  const path = () => evalIn(`location.pathname`);
  const activeSession = () => evalIn(`(() => {
    for (const p of state.panes) { const t = p.tabs[p.active]; if (t && t.k === "Terminal") return t.session; }
    return null; })()`);

  // Opens a terminal, waits for a live shell prompt, and prints one OSC 777
  // notification through it.
  const raise = async (title, body) => {
    const find = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
      const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
      if (ti >= 0) return { pi, ti, session: state.panes[pi].tabs[ti].session }; } return null; })()`;
    let loc = await evalIn(find);
    if (!loc) {
      await evalIn(`send({ t: "NewTerminal", pane: 0 })`);
      await until(async () => !!(loc = await evalIn(find)), 15, "a terminal tab");
    }
    await evalIn(`send({ t: "ActivateTab", pane: ${loc.pi}, idx: ${loc.ti} })`);
    await sleep(400);
    await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
    await until(() => evalIn("terms.size > 0 && !!__t().sock && __t().sock.readyState === 1"), 30, "socket");
    // readline discards typeahead while it initialises, so a sequence typed
    // before the prompt silently vanishes (README trap 3) and this whole
    // file would then assert on an empty store in both pages — passing for
    // the worst possible reason.
    await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "shell prompt");
    const cmd = `printf '\\033]777;notify;${title};${body}\\007'`;
    await evalIn(`__t().term.input(${JSON.stringify(cmd + "\r")})`);
    return loc.session;
  };

  return { evalIn, ready, badge, stored, openPanel, rows, emptyNote, clickClear, clickFirstRow, path, activeSession, raise, close: page.close };
};

let p, o;
try {
  p = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`), "proj");
  o = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/other`), "other");
  ok(await p.ready() && await o.ready(), "two pages are up, one per project");

  console.log("A. each project raises a notice from a real shell");
  const sessP = await p.raise("Build done", "proj-notice");
  const sessO = await o.raise("Build done", "other-notice");

  ok(
    await until(() => p.stored().then((s) => s.some((x) => x.includes("proj-notice"))), 20, "proj notice"),
    "proj's page received its own notice",
  );
  ok(
    await until(() => o.stored().then((s) => s.some((x) => x.includes("other-notice"))), 20, "other notice"),
    "other's page received its own notice",
  );

  console.log("B. and neither page holds the other's");
  // Only meaningful *after* both arrivals above: checked earlier, "no
  // foreign notice yet" is indistinguishable from "nothing has arrived at
  // all", which is the trap that makes a negative assertion vacuous. The
  // extra grace window covers a broadcast still in flight.
  await sleep(700);
  const storedP = await p.stored();
  const storedO = await o.stored();
  ok(
    !storedP.some((x) => x.includes("other-notice")),
    `proj's page must not hold other's notice (got ${JSON.stringify(storedP)})`,
  );
  ok(
    !storedO.some((x) => x.includes("proj-notice")),
    `other's page must not hold proj's notice (got ${JSON.stringify(storedO)})`,
  );
  ok(await p.badge() === "1", `proj's badge counts one, its own (got ${JSON.stringify(await p.badge())})`);
  ok(await o.badge() === "1", `other's badge counts one, its own (got ${JSON.stringify(await o.badge())})`);

  console.log("B2. a page opened after the fact replays only its own project");
  // Both pages above connected before either notice existed, so their
  // `Notices` replay was empty whichever way the server scoped it — this
  // section is the only thing here that touches the connect path at all.
  // Without it the replay could ship the whole store and every assertion in
  // this file would still pass. (It did: reverting `wsconn`'s `list_for` to
  // `list` failed nothing until this section existed.)
  const late = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`), "late");
  ok(await late.ready(), "a third page is up on proj");
  // `until` returns a boolean, not the value it waited for — reading its
  // result as the array made both assertions below fail on `Array.isArray`
  // no matter what the server sent, which is a test failing for a reason
  // that has nothing to do with its subject. Wait, then read.
  const arrived = await until(() => late.stored().then((s2) => s2.length > 0), 10, "replay");
  const storedLate = await late.stored();
  ok(arrived, `the connect replay delivered something (got ${JSON.stringify(storedLate)})`);
  ok(
    storedLate.some((x) => x.includes("proj-notice")),
    `it carries proj's own history (got ${JSON.stringify(storedLate)})`,
  );
  ok(
    !storedLate.some((x) => x.includes("other-notice")),
    `and not the other project's (got ${JSON.stringify(storedLate)})`,
  );
  await late.close();

  console.log("C. the panel renders that same one project");
  await p.openPanel();
  const rowsP = await p.rows();
  ok(rowsP.length === 1, `proj's panel shows exactly one row (got ${rowsP.length}: ${JSON.stringify(rowsP)})`);
  ok(/proj\s*·\s*/.test(rowsP[0]) && rowsP[0].includes("proj-notice"), `and it is proj's (${rowsP[0]})`);
  ok(!rowsP.join(" ").includes("other"), "with no row attributed to the other project");

  console.log("D. clicking a notice focuses its terminal without leaving the project");
  await o.openPanel();
  const before = await o.path();
  ok(await o.clickFirstRow(), "other's row is clickable");
  ok(
    await until(() => o.activeSession().then((s) => s === sessO), 5, "focus"),
    `the click activated the notice's own terminal tab (${sessO})`,
  );
  ok(await o.path() === before, `and did not navigate the tab away from ${before} (now ${await o.path()})`);
  ok(
    await until(() => o.badge().then((b) => b === ""), 5, "badge cleared"),
    "focusing the session cleared its badge",
  );

  console.log("E. Clear empties this project and leaves the other alone");
  // proj still holds its own unread notice, which is what makes this
  // discriminating: without it, a Clear button wired to nothing would leave
  // other's notice standing too and pass the scoping assertion vacuously.
  await p.openPanel();
  ok((await p.rows()).length === 1, "proj's panel still holds its own notice before clearing");
  ok(await p.clickClear(), "Clear is clickable");
  ok(
    await until(() => p.rows().then((r) => r.length === 0), 5, "cleared"),
    "proj's panel is empty afterwards — the button is really wired",
  );
  ok(/no notifications/.test(await p.emptyNote()), "and says so");
  ok(await p.badge() === "", "proj's badge is gone");

  // Asked of a FRESH page, not of `o`. This is the whole point of the
  // section and it took a revert to find: with `clear_in` reverted to
  // `clear`, the server really does destroy every project's history — but
  // the rebroadcast that follows only reaches the clearing project's own
  // clients, so `o` goes on holding a notice that no longer exists and
  // rendering a row for it. Both its array and its DOM said "survived"
  // while the store was empty. A page that connects now is the only thing
  // here that reads the server's actual state.
  const after = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/other`), "after");
  ok(await after.ready(), "a fresh page is up on other");
  await sleep(500); // the connect replay, which may legitimately be empty
  const survived = await after.stored();
  ok(
    survived.some((x) => x.includes("other-notice")),
    `the other project's notice survived a Clear pressed elsewhere (got ${JSON.stringify(survived)})`,
  );
  await after.openPanel();
  const survivedRows = await after.rows();
  ok(
    survivedRows.length === 1 && survivedRows[0].includes("other-notice"),
    `and renders for a page that just asked the server (got ${JSON.stringify(survivedRows)})`,
  );
  await after.close();
  ok(sessP !== null && sessO !== null, "both sessions were named (guards the raise() helper itself)");
} finally {
  try { await p?.close?.(); } catch {}
  try { await o?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
