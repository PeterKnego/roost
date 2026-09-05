//! Closing a dirty file tab: the confirmation must not let the tab strip move
//! underneath it.
//!
//! confirm() blocked the event loop, so nothing could arrive between the
//! question and the send. An in-page dialog does not, and CloseTab is
//! addressed by INDEX (proto.rs). So a State event that arrives while the
//! dialog is open — another client closing a tab to the left, a session
//! ending — renumbers the strip, and the index gathered before the wait now
//! names a different tab.
//!
//! Traps this file is written against (see README):
//!   - Section B proves the setup it later negates: three tabs, in a known
//!     order, asserted before the dialog opens. Without that, "the right tab
//!     survived" could pass against a strip that never had the others.
//!   - The assertion is on WHICH tab remains, not on the count. A count is
//!     equally satisfied by closing the wrong one.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

// autosave off: the buffer has to STAY dirty for the dialog to appear at all.
// With autosave on this test would silently take the no-dialog path and prove
// nothing — the README's "buffers that have to stay dirty" trap.
const fx = await fixture({ autosave: false });
const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  await Deno.writeTextFile(`${fx.roots}/proj/a.txt`, "a\n");
  await Deno.writeTextFile(`${fx.roots}/proj/b.txt`, "b\n");
  await Deno.writeTextFile(`${fx.roots}/proj/c.txt`, "c\n");
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const evalIn = page.evalIn;
  await until(async () => await evalIn("typeof askConfirm === 'function'"), 10, "dialog.js loaded");

  // Pane 2 is MIDDLE (proto.rs:8), where file tabs open (app.js:3482).
  const tabs = async () => await evalIn(
    `JSON.stringify(state.panes[2].tabs.filter((t) => t.k === "File").map((t) => t.rel))`);

  for (const f of ["a.txt", "b.txt", "c.txt"]) {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(f)}, mode: "Edit" } }); 0`);
    await until(async () => (await tabs()).includes(f), 10, `${f} opened`);
  }
  // A: the setup this test later negates.
  ok((await tabs()) === '["a.txt","b.txt","c.txt"]', "three file tabs, in order");

  // Make c.txt dirty so its close asks.
  await evalIn(`send({ t: "EditBuffer", rel: "c.txt", text: "c changed\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((b) => b.rel === "c.txt" && b.dirty)`),
    10, "c.txt dirty");

  // B: open c.txt's close dialog, then move the strip underneath it.
  const ci = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "c.txt")`);
  await evalIn(`closeTab(2, ${ci}, state.panes[2].tabs[${ci}], false); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "the dirty-close dialog opened");
  // a.txt closes while the dialog is up: every later tab shifts down one.
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "a.txt") }); 0`);
  await until(async () => !(await tabs()).includes("a.txt"), 10, "a.txt gone");
  ok((await tabs()) === '["b.txt","c.txt"]', "the strip moved while the dialog was open");

  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  await until(async () => !(await tabs()).includes("c.txt"), 10, "c.txt closed");
  // C: WHICH tab remains. A count assertion passes just as well when the
  // wrong tab was closed.
  ok((await tabs()) === '["b.txt"]', "the tab the user clicked was closed, not the one at its old index");

  // D: cancelling closes nothing.
  await evalIn(`send({ t: "EditBuffer", rel: "b.txt", text: "b changed\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((x) => x.rel === "b.txt" && x.dirty)`),
    10, "b.txt dirty");
  const bi = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "b.txt")`);
  await evalIn(`closeTab(2, ${bi}, state.panes[2].tabs[${bi}], false); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await new Promise((r) => setTimeout(r, 500));
  ok((await tabs()) === '["b.txt"]', "cancelling the dialog closes nothing");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
