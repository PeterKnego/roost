//! Close Project, then reopen the project: the terminals must be gone from
//! the tab strip, not just from the process table.
//!
//! Reported from the deployed instance as "I close the project, which should
//! close all terminals, but on frontend (even after reload) I still see
//! terminals under project". The shells really were being killed — the
//! sockets went, the overview emptied — but `do_close_project` never touched
//! the layout, and the layout is *persisted*, so every reload of that project
//! restored a Terminal tab per dead session. A stale tab is not cosmetic
//! here: `workspace.rs`'s `EndSession` arm already explains why one left
//! behind "would offer a click that silently starts a *new* shell under a
//! name the user thought they had just closed".
//!
//! Why a browser test when `hub.rs` already asserts on the reloaded-from-disk
//! workspace: the Rust test proves what the *server* serves. What the user
//! reported is what the page *shows*, which is app.js reading `state.panes`
//! — and, per this directory's whole reason to exist, `cargo test` cannot
//! reach that file. Section C is the other half no Rust test covers: the
//! emptied pane has to stay usable, or this fix trades ghost tabs for a dead
//! pane.
//!
//! Traps this file is written against (see README):
//!   - Section A proves the setup state it later negates (three terminal
//!     tabs, two live sessions), so B's "no terminal tabs" cannot pass
//!     against a project that never had any.
//!   - B asserts the tree/changes tabs *survive*, so wiping the layout
//!     outright would fail rather than pass.
//!   - `confirm()` is stubbed rather than answered over CDP: a native dialog
//!     with no `Page.javascriptDialogOpening` handler wedges the renderer,
//!     which is the harness's documented 30s-timeout hang, not a failure.
//!
//! Section D covers a second, separate defect found the same day: a close
//! that races a terminal still spawning. Revert-checked on its own — see the
//! second paragraph below.
//!
//! Revert-check, performed: with both `drop_terminal_tabs()` calls removed
//! from `hub.rs`'s `do_close_project`, this fails 3 — B's tab assertion and
//! its saved-layout assertion (`term`, `term1`, `term2` all back), plus C,
//! where the "fresh" terminal lands beside the three ghosts as `term3`.
//! Removing only that helper's `self.persist()` fails exactly 1: the
//! saved-layout assertion. Everything the *page* shows still passes there,
//! because this reopen talks to the same still-running hub whose in-memory
//! layout was cleared — which is why the on-disk read is a separate
//! assertion, and is the one that stands for "even after reload".
//!
//! Run: deno run -A tests/browser/closeproject.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const browser = await startBrowser(profileDir(repoRoot));
let ws, after, resh;

/// The dtach sockets on disk for the fixture project — the evidence that the
/// shells themselves ended, kept separate from what the page shows. `.origin`
/// is the registry's marker, not a session.
async function sockets() {
  const out = [];
  try {
    for await (const e of Deno.readDir(`${fx.stateDir}/sock/${fx.project}`)) {
      if (e.name !== ".origin") out.push(e.name);
    }
  } catch { /* the whole directory may be gone; that is zero sessions */ }
  return out.sort();
}

const terminalTabs = (p) =>
  p.evalIn(`JSON.stringify(state.panes.flatMap((q) => q.tabs).filter((t) => t.k === "Terminal").map((t) => t.session))`)
    .then(JSON.parse);
const tabKinds = (p) =>
  p.evalIn(`JSON.stringify(state.panes.flatMap((q) => q.tabs).map((t) => t.k))`).then(JSON.parse);

try {
  resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });

  console.log("A. a project with live terminals");
  ws = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  await until(() => ws.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  const newterm = `document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`;
  // Two more on top of the default layout's own `term` tab, so the close has
  // several sessions in several states to clear: one tab never attached, two
  // with a real dtach master behind them.
  await ws.evalIn(newterm);
  await until(async () => (await terminalTabs(ws)).length >= 2, 20, "a second terminal");
  await ws.evalIn(newterm);
  await until(async () => (await terminalTabs(ws)).length >= 3, 20, "a third terminal");
  await until(async () => (await sockets()).length >= 2, 30, "two dtach sockets");
  const before = await terminalTabs(ws);
  ok(before.length === 3, `the project has terminal tabs to lose: ${JSON.stringify(before)}`);
  ok((await sockets()).length >= 2, `and real shells behind them: ${JSON.stringify(await sockets())}`);

  console.log("B. after Close Project, reopening shows no terminals");
  // Stubbed, not answered over CDP — see the header note on wedged renderers.
  await ws.evalIn(`window.confirm = () => true; window.alert = () => {}; document.getElementById("closeproj").click()`);
  ok(await until(async () => (await sockets()).length === 0, 30, "every socket unlinked"),
     `the shells themselves ended: ${JSON.stringify(await sockets())}`);
  // The sockets go early in the close, but the close is not over: it sweeps a
  // second time after `CLOSE_SETTLE` (see Section D), and `closing` stays true
  // across both — so `term.rs` deliberately refuses a new terminal for that
  // whole window. Reopening inside it made B and C fail here for that reason
  // alone, which is a real property worth waiting out rather than hiding: a
  // user who closes a project and immediately reopens it meets the same
  // refusal, and it clears itself.
  await sleep(1500);
  // A close ends by navigating to `/`; reopen the project the way a user
  // would, which is the reload the report was about.
  after = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  await until(() => after.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "reopened app.js");
  const reopened = await terminalTabs(after);
  ok(reopened.length === 0, `the reopened project has no terminal tabs: ${JSON.stringify(reopened)}`);
  ok((await after.evalIn(`JSON.stringify(state.live_sessions)`)) === "[]", "and no live sessions");
  // The other direction: this clears terminals, not the workspace. Without
  // these two, emptying every pane outright would pass Section B.
  const kinds = await tabKinds(after);
  ok(kinds.includes("Tree"), `the tree pane survived the close: ${JSON.stringify(kinds)}`);
  ok(kinds.includes("Changes"), `the changes pane survived the close: ${JSON.stringify(kinds)}`);
  // What a *reload* reads, as opposed to what this connection was handed.
  const saved = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/${fx.project}.json`));
  const savedTerms = saved.panes.flatMap((p) => p.tabs).filter((t) => t.k === "Terminal");
  ok(savedTerms.length === 0, `the saved layout keeps no terminal tab: ${JSON.stringify(savedTerms)}`);

  console.log("C. the emptied pane is still usable");
  ok(await after.evalIn(`!!document.querySelector('.pane[data-pane="3"] .paneicons .newterm')`),
     "a pane with no tabs keeps its ✛ button");
  await after.evalIn(newterm);
  ok(await until(async () => (await terminalTabs(after)).length === 1, 20, "a fresh terminal"),
     "a new terminal can be started in the closed project again");
  ok(await until(async () => (await sockets()).length === 1, 30, "its socket"),
     `and it is a real shell: ${JSON.stringify(await sockets())}`);
  console.log("D. a terminal still spawning when the close runs must not survive it");
  // The second reported failure, and a different bug from A-C: the close
  // ended the shells and cleared the tabs, and a shell appeared anyway. In
  // production the survivor's dtach had resh itself as its parent and a start
  // time in the same second as the close — a terminal websocket that reached
  // `session::attach` (which creates when absent) while `kill_project` was
  // reading a socket directory and a `ps` snapshot that could not show it yet.
  //
  // No wait between starting the terminal and closing: every other section
  // here waits for the socket to exist first, and that wait is exactly what
  // hides this. Both clicks go in one evaluation so the browser cannot
  // interleave a round trip between them.
  const race = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  await until(() => race.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "race page");
  await race.evalIn(`
    window.confirm = () => true; window.alert = () => {};
    document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click();
    document.getElementById("closeproj").click();
  `);
  await sleep(9000);   // past CLOSE_SETTLE and the second sweep, with margin
  const savedAfter = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/${fx.project}.json`));
  const raceTerms = savedAfter.panes.flatMap((p) => p.tabs).filter((t) => t.k === "Terminal");
  // Proves the close actually committed. Without it, a close that was refused
  // or never sent would leave no sessions to find and pass the next assertion
  // for the opposite reason.
  ok(raceTerms.length === 0, `the racing close committed (layout cleared): ${JSON.stringify(raceTerms)}`);
  ok((await sockets()).length === 0, `no socket outlived the racing close: ${JSON.stringify(await sockets())}`);
  const ps = await new Deno.Command("ps", { args: ["-Ao", "args="], stdout: "piped" }).output();
  const orphans = new TextDecoder().decode(ps.stdout).split("\n")
    .filter((l) => l.includes(`${fx.stateDir}/sock/${fx.project}/`) && l.includes("dtach -A"));
  ok(orphans.length === 0, `no dtach master outlived the racing close: ${orphans.length}`);
  try { race.close(); } catch { /* already gone */ }
} finally {
  try { ws?.close(); } catch { /* already gone */ }
  try { after?.close(); } catch { /* already gone */ }
  await resh?.close();
  browser.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
