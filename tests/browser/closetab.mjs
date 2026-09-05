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
//! Two scenarios, because the stale index can land two different ways once a
//! tab closes out from under it:
//!   - Scenario 1 (three tabs): closing a.txt while the dialog is open
//!     leaves the stale index PAST THE END of the shrunk strip. The server
//!     refuses an out-of-range CloseTab, so the bug here is a silent no-op —
//!     nothing closes. Real, but not the hazard the fix exists for.
//!   - Scenario 2 (four tabs): with one more tab in play, the same shift
//!     leaves the stale index landing on a tab that still EXISTS — d.txt,
//!     not c.txt. This is the case that actually loses work: the wrong tab
//!     closes silently, with no error and no sign anything went wrong.
//!
//! Traps this file is written against (see README):
//!   - Each scenario proves the setup it later negates: tabs opened in a
//!     known order, asserted before the dialog opens. Without that, "the
//!     right tab survived" could pass against a strip that never had the
//!     others.
//!   - The assertion is on WHICH tab remains, not on the count. A count is
//!     equally satisfied by closing the wrong one — this is exactly why
//!     scenario 1 alone was insufficient: it can only prove "something
//!     happened or didn't", never "the right thing happened".
//!   - Scenario 2 waits on the tab COUNT dropping to two, not on "c.txt
//!     gone": under the reverted code c.txt never closes (d.txt does
//!     instead), so waiting for c.txt's specific absence would time out
//!     before the discriminating comparison ever ran.
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
  ok((await tabs()) === '["a.txt","b.txt","c.txt"]', "scenario 1: three file tabs, in order");

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
  ok((await tabs()) === '["b.txt","c.txt"]', "scenario 1: the strip moved while the dialog was open");

  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  await until(async () => !(await tabs()).includes("c.txt"), 10, "c.txt closed");
  // C: WHICH tab remains. A count assertion passes just as well when the
  // wrong tab was closed.
  ok((await tabs()) === '["b.txt"]', "scenario 1: the tab the user clicked was closed, not the one at its old index");

  // D: cancelling closes nothing.
  await evalIn(`send({ t: "EditBuffer", rel: "b.txt", text: "b changed\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((x) => x.rel === "b.txt" && x.dirty)`),
    10, "b.txt dirty");
  const bi = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "b.txt")`);
  await evalIn(`closeTab(2, ${bi}, state.panes[2].tabs[${bi}], false); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await new Promise((r) => setTimeout(r, 500));
  ok((await tabs()) === '["b.txt"]', "scenario 1: cancelling the dialog closes nothing");

  // Clear the pane before scenario 2. This closes whatever is at index 0
  // repeatedly, by direct server intent (bypassing closeTab's dirty-confirm
  // entirely, which is not what scenario 2 tests) -- not "close b.txt",
  // because scenario 1 under the REVERTED code leaves relics behind (a
  // no-op stale CloseTab means c.txt is never actually closed there). A
  // targeted close of only the tab scenario 1 was "supposed" to leave
  // contaminates scenario 2's setup with a leftover tab in the wrong slot,
  // which was observed to produce a false PASS in scenario 2 by coincidence
  // (see the comment above the findIndex line below).
  for (let guard = 0; guard < 10 && (await tabs()) !== "[]"; guard++) {
    await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File") }); 0`);
    await new Promise((r) => setTimeout(r, 100));
  }
  ok((await tabs()) === "[]", "pane cleared before scenario 2");

  // ---- Scenario 2: four tabs, so the stale index stays IN RANGE ----
  // With three tabs, closing a.txt always pushes the stale index past the
  // end (scenario 1's out-of-range no-op). A fourth tab means the shift
  // lands the stale index on a tab that still exists, which is the actual
  // hazard: the wrong tab closes with no error, silently.
  await Deno.writeTextFile(`${fx.roots}/proj/d.txt`, "d\n");
  for (const f of ["a.txt", "b.txt", "c.txt", "d.txt"]) {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(f)}, mode: "Edit" } }); 0`);
    await until(async () => (await tabs()).includes(f), 10, `${f} opened (scenario 2)`);
  }
  ok((await tabs()) === '["a.txt","b.txt","c.txt","d.txt"]', "scenario 2: four file tabs, in order");

  // Make c.txt dirty so its close asks.
  await evalIn(`send({ t: "EditBuffer", rel: "c.txt", text: "c changed again\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((b) => b.rel === "c.txt" && b.dirty)`),
    10, "c.txt dirty (scenario 2)");

  const ci2 = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "c.txt")`);
  await evalIn(`closeTab(2, ${ci2}, state.panes[2].tabs[${ci2}], false); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "scenario 2: the dirty-close dialog opened");

  // a.txt closes while the dialog is up. With four tabs this leaves b=0,
  // c=1, d=2 -- the stale index 2 (c.txt's original slot) now names d.txt,
  // which exists, instead of falling off the end.
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "a.txt") }); 0`);
  await until(async () => !(await tabs()).includes("a.txt"), 10, "a.txt gone (scenario 2)");
  ok((await tabs()) === '["b.txt","c.txt","d.txt"]', "scenario 2: the strip moved while the dialog was open");

  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  // Wait on COUNT dropping to two, not on "c.txt gone": under the reverted
  // code c.txt never closes (d.txt does instead), so waiting for c.txt's
  // absence specifically would time out before the assertion below ran.
  await until(async () => JSON.parse(await tabs()).length === 2, 10, "one tab closed (scenario 2)");
  ok((await tabs()) === '["b.txt","d.txt"]',
    "scenario 2: the tab the user clicked (c.txt) was closed, not whichever tab shifted into its old index (d.txt)");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
