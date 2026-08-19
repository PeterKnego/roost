//! The per-pane header controls: move a tab between panes, maximize/restore a
//! pane, collapse the tree.
//!
//! Worth a browser test rather than a Rust one because all three are gestures:
//! the intents behind them (MoveTab, Resize) were already covered server-side
//! and still did nothing for a user, since nothing in the UI ever sent a
//! MoveTab at all.
//!
//! Run: deno run -A tests/browser/paneicons.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.roots}/proj/src/inner`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/proj/src/main.rs`, "fn main() {}\n");
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app");
  const icon = (pane, prefix, shift = false) =>
    evalIn(`(() => { const b = [...document.querySelectorAll('.pane[data-pane="${pane}"] .paneicons .paneicon')]
      .find((x) => x.title.startsWith(${JSON.stringify(prefix)}));
      if (!b) return false;
      b.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: ${shift} })); return true; })()`);
  const tabs = (pane) => evalIn(`JSON.stringify(state.panes[${pane}].tabs.map((t) => t.k))`);

  console.log("A. move a tab to the next pane");
  const before0 = JSON.parse(await tabs(0));
  ok(before0.includes("Tree"), `pane 0 starts with the tree (${before0})`);
  ok(await icon(0, "move"), "the move control exists");
  ok(await until(async () => JSON.parse(await tabs(1)).includes("Tree"), 10, "tree in pane 1"),
     "the tab landed in the next pane");
  ok(!JSON.parse(await tabs(0)).includes("Tree"), "and left the one it came from");

  console.log("\nB. shift-click sends it back the other way");
  ok(await icon(1, "move", true), "the control is there in its new home");
  ok(await until(async () => JSON.parse(await tabs(0)).includes("Tree"), 10, "tree back in pane 0"),
     "shift-click moves it to the *previous* pane, not the next");

  console.log("\nC. maximize and restore");
  const before = JSON.parse(await evalIn("JSON.stringify(state.sizes)"));
  ok(await icon(3, "maximize"), "the maximize control exists");
  ok(await until(async () => {
    const s = JSON.parse(await evalIn("JSON.stringify(state.sizes)"));
    return s.left_w === 0 && s.right_w > before.right_w;
  }, 10, "maximized"), `the pane took the width (left_w 0, right_w grew from ${before.right_w})`);
  ok(await icon(3, "restore"), "the control now offers restore");
  ok(await until(async () => (await evalIn("JSON.stringify(state.sizes)")) === JSON.stringify(before), 10, "restored"),
     "restore puts the previous sizes back exactly");

  console.log("\nD. collapse all");
  const treePane = await evalIn(`state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"))`);
  await evalIn(`(() => { const d = document.querySelector('.pane[data-pane="${treePane}"] .content details');
    if (d) d.open = true; })()`);
  const openBefore = await evalIn(`document.querySelectorAll('.pane[data-pane="${treePane}"] .content details[open]').length`);
  ok(openBefore > 0, `something is expanded to collapse (${openBefore} open)`);
  ok(await icon(treePane, "collapse"), "the collapse control exists on the tree pane");
  ok(await until(async () =>
    (await evalIn(`document.querySelectorAll('.pane[data-pane="${treePane}"] .content details[open]').length`)) === 0,
    10, "collapsed"), "every disclosure closed");

  console.log("\nE. the controls are pane-appropriate");
  ok(!(await icon(3, "collapse")), "collapse all is absent from a pane showing no tree");
} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
