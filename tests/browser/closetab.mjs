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
//! Three scenarios, because the stale index can land three different ways
//! once the strip changes out from under an open dialog:
//!   - Scenario 1 (three tabs): closing a.txt while the dialog is open
//!     leaves the stale index PAST THE END of the shrunk strip. The server
//!     refuses an out-of-range CloseTab, so the bug here is a silent no-op —
//!     nothing closes. Real, but not the hazard the fix exists for.
//!   - Scenario 2 (four tabs): with one more tab in play, the same shift
//!     leaves the stale index landing on a tab that still EXISTS — d.txt,
//!     not c.txt. This is the case that actually loses work: the wrong tab
//!     closes silently, with no error and no sign anything went wrong.
//!   - Scenario 3 (four tabs, the tab UNDER the dialog closes): the other
//!     client closes c.txt itself — the very tab whose dialog is open — not
//!     a neighbor. Re-resolving by rel then finds nothing at all: this is
//!     the only scenario that exercises the `< 0` branch, which the first
//!     two never reach because their re-resolution always succeeds.
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
//!   - Scenarios 2 and 3 wait on a condition that holds under BOTH the fix
//!     and the bug (a count change either way, or a banner-or-count race),
//!     not on the specific effect the fix itself is supposed to prevent —
//!     otherwise the reverted run would time out before ever reaching the
//!     assertion that is supposed to catch it.
//!   - A targeted inter-scenario cleanup (close exactly the tab a prior
//!     scenario was "supposed" to leave) can be fooled by a REVERTED run's
//!     own relics: scenario 1 under the bug never actually closes c.txt (see
//!     its no-op above), so a cleanup that only knows to close b.txt leaves
//!     c.txt behind to contaminate scenario 2's setup. `clearPane` below
//!     closes whatever is at index 0 in a loop instead, so it empties the
//!     pane regardless of which relics a broken closeTab left behind. This
//!     was not hypothetical: an earlier version of this file had the
//!     targeted cleanup, and it produced a false PASS in scenario 2 against
//!     the reverted code, by coincidence, because the contaminated tab order
//!     happened to put the stale index back on the right tab.
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

  // Closes whatever File tab sits at index 0, repeatedly, until the pane is
  // empty -- regardless of what a previous (possibly-reverted) scenario left
  // behind. Uses `until` per step rather than a fixed sleep: this fails
  // loudly on a genuine timeout instead of guessing a delay that happens to
  // work today. See the header comment for the false-PASS this replaced.
  const clearPane = async (label) => {
    let n = JSON.parse(await tabs()).length;
    while (n > 0) {
      await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File") }); 0`);
      await until(async () => JSON.parse(await tabs()).length < n, 5, `a tab closed while clearing (${label})`);
      n = JSON.parse(await tabs()).length;
    }
    ok((await tabs()) === "[]", `pane cleared before ${label}`);
  };

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

  await clearPane("scenario 2");

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

  await clearPane("scenario 3");

  // ---- Scenario 3: the tab UNDER the dialog is the one that closes ----
  // Scenarios 1 and 2 always find *something* at the re-resolved index --
  // their re-resolution never fails, so the `< 0` branch (the banner, and
  // doing nothing) has no coverage at all. Here the other client closes
  // c.txt itself -- the tab the dialog is FOR -- while the dialog is open,
  // so re-resolving by rel is guaranteed to miss.
  await Deno.writeTextFile(`${fx.roots}/proj/d.txt`, "d\n");
  for (const f of ["a.txt", "b.txt", "c.txt", "d.txt"]) {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(f)}, mode: "Edit" } }); 0`);
    await until(async () => (await tabs()).includes(f), 10, `${f} opened (scenario 3)`);
  }
  ok((await tabs()) === '["a.txt","b.txt","c.txt","d.txt"]', "scenario 3: four file tabs, in order");

  await evalIn(`send({ t: "EditBuffer", rel: "c.txt", text: "c changed a third time\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((b) => b.rel === "c.txt" && b.dirty)`),
    10, "c.txt dirty (scenario 3)");

  const ci3 = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "c.txt")`);
  await evalIn(`closeTab(2, ${ci3}, state.panes[2].tabs[${ci3}], false); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "scenario 3: the dirty-close dialog opened");

  // c.txt itself closes while the dialog is up (another client, or a
  // vanished-file sweep -- anything that removes exactly the tab in
  // question). The strip becomes [a,b,d]: a=0, b=1, d=2 -- the stale index
  // 2 (c.txt's original slot) now names d.txt, a tab that still exists but
  // is not the one anyone asked to close.
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "c.txt") }); 0`);
  await until(async () => !(await tabs()).includes("c.txt"), 10, "c.txt gone (scenario 3)");
  ok((await tabs()) === '["a.txt","b.txt","d.txt"]', "scenario 3: the strip moved while the dialog was open");

  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  // Wait for either outcome, not just the fix's: the fix shows a banner and
  // changes nothing (tab count stays 3); the buggy fallback removes d.txt
  // (count drops to 2). Waiting on only one of these would time out under
  // the other code path before the discriminating assertion below ran.
  await until(async () => {
    const banner = await evalIn(`!!document.querySelector(".error-banner")`);
    const count = JSON.parse(await tabs()).length;
    return banner || count < 3;
  }, 10, "close attempt settled (scenario 3)");
  ok((await tabs()) === '["a.txt","b.txt","d.txt"]',
    "scenario 3: re-resolution found nothing (c.txt was already gone), so nothing else was closed in its place");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
