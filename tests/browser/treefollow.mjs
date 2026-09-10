//! The file tree follows the file you are looking at: it expands to reveal
//! it, marks its row, and never collapses anything on the way.
//!
//! The server half of this already existed and was simply unused — `?open=`
//! makes `tree_level` expand every ancestor of a path inline and recursively
//! and put `class="file sel"` on the row, and `render.rs` has tests for both.
//! What lives only in static/app.js, where `cargo test` cannot reach, is
//! everything that decides *when* to ask and how to merge the answer.
//!
//! Traps this file is written against (see README):
//!
//!   - **Section D is the whole point.** Any test that only checks "the path
//!     got expanded" passes against a wholesale `innerHTML` replace, which is
//!     the implementation that makes an auto-expanding tree hostile: it lands
//!     the path and discards every directory the user had opened. D expands
//!     two unrelated directories first and asserts they survive.
//!   - **Section C switches between tabs that are already open.** Following
//!     only on *first open* is a different, easier feature, and the issue
//!     asks for both — a test that only ever opens new files cannot tell them
//!     apart.
//!   - **Every assertion names the directory it expects**, never a count of
//!     open `<details>`. A count is equally satisfied by expanding the wrong
//!     one, which is precisely what a broken merge does.
//!   - **Section E turns the setting off and asserts nothing moved**, which is
//!     the only thing that can catch a follow wired to fire regardless.
//!
//! Run: deno run -A tests/browser/treefollow.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
for (const p of ["a/b/c", "p/q/r", "x/y"]) {
  await Deno.mkdir(`${fx.roots}/proj/${p}`, { recursive: true });
}
await Deno.writeTextFile(`${fx.roots}/proj/a/b/c/deep.rs`, "fn deep() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/p/q/r/second.rs`, "fn second() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/x/y/unrelated.rs`, "fn unrelated() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/top.rs`, "fn top() {}\n");

const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof send === 'function' && !!state"), 20, "app.js");

  // Pane 0 holds the Tree tab in the default layout.
  const TREE = `document.querySelector('.pane[data-pane="0"] .content')`;
  const isOpen = async (rel) => await evalIn(
    `!!${TREE}.querySelector('details[data-rel="${rel}"][open]')`);
  const marked = async () => await evalIn(
    `(() => { const s = ${TREE}.querySelectorAll("a.file.sel");
      return JSON.stringify([...s].map((a) => a.dataset.rel)); })()`);
  const openFile = async (rel) => {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } }); 0`);
    return await until(async () => await evalIn(
      `(() => { const p = state.panes[2]; const t = p.tabs[p.active];
        return !!(t && t.k === "File" && t.rel === ${JSON.stringify(rel)}); })()`),
      10, `${rel} active`);
  };
  // Expands a directory the way a user does, and waits for its children to
  // arrive — the lazy `hx-trigger="toggle once"` fetch.
  const expand = async (rel) => {
    await evalIn(`(() => { const d = ${TREE}.querySelector('details[data-rel="${rel}"]');
      if (d) d.open = true; return 0; })()`);
    return await until(async () => await evalIn(
      `(() => { const d = ${TREE}.querySelector('details[data-rel="${rel}"]');
        const u = d && d.querySelector(":scope > ul");
        return !!(u && u.children.length); })()`), 10, `${rel} children`);
  };

  console.log("\nA. the setting reaches the page");
  ok(await evalIn(`document.body.dataset.followTree === "1"`),
    "follow_tree is on by default and embedded at page load");
  ok(await until(async () => await evalIn(`!!${TREE}.querySelector("ul.tree")`), 10, "tree"),
    "the tree pane rendered");
  // The state this section later negates: nothing is expanded or marked yet.
  ok(!(await isOpen("a")), "setup: nothing is expanded to begin with");
  ok((await marked()) === "[]", `setup: no row is marked to begin with — got ${await marked()}`);

  console.log("\nB. opening a deep file expands to it and marks it");
  await openFile("a/b/c/deep.rs");
  ok(await until(async () => (await marked()) === '["a/b/c/deep.rs"]', 10, "marked"),
    `the file's row is marked, and only it — got ${await marked()}`);
  for (const d of ["a", "a/b", "a/b/c"]) {
    ok(await isOpen(d), `and ${d} was expanded to reveal it`);
  }
  ok(await evalIn(`!!${TREE}.querySelector('a.file[data-rel="a/b/c/deep.rs"]')`),
    "so the row actually exists in the DOM, not just in the fetch");

  console.log("\nC. switching between tabs that are already open moves the mark");
  await openFile("p/q/r/second.rs");
  ok(await until(async () => (await marked()) === '["p/q/r/second.rs"]', 10, "second marked"),
    `the second file is marked — got ${await marked()}`);
  // Back to the first, by activating a tab that already exists. Following
  // only on first open is a different feature; this is the half that catches
  // it.
  const firstIdx = await evalIn(
    `state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "a/b/c/deep.rs")`);
  ok(firstIdx >= 0, "setup: the first file is still an open tab, not reopened");
  await evalIn(`send({ t: "ActivateTab", pane: 2, idx: ${firstIdx} }); 0`);
  ok(await until(async () => (await marked()) === '["a/b/c/deep.rs"]', 10, "mark moved back"),
    `activating an already-open tab moves the mark — got ${await marked()}`);

  console.log("\nD. following never collapses what the user expanded");
  // Two directories with nothing to do with either file, expanded by hand.
  ok(await expand("x"), "setup: the user expands x");
  ok(await expand("x/y"), "setup: and x/y inside it");
  ok(await isOpen("x") && await isOpen("x/y"), "setup: both are open before the follow");

  await openFile("p/q/r/second.rs");
  ok(await until(async () => (await marked()) === '["p/q/r/second.rs"]', 10, "followed"),
    `following moved to the other file — got ${await marked()}`);
  // The assertion this file exists for. A wholesale replace lands the new
  // path and throws these away.
  ok(await isOpen("x"), "x is still expanded after following elsewhere");
  ok(await isOpen("x/y"), "and so is x/y, nested inside it");
  ok(await evalIn(`!!${TREE}.querySelector('a.file[data-rel="x/y/unrelated.rs"]')`),
    "with its children still loaded, not an emptied shell");
  // And the previous file's path is not collapsed either.
  ok(await isOpen("a/b/c"), "the previously-followed path is left expanded too");

  console.log("\nE. with the setting off, nothing follows");
  await evalIn(`document.body.dataset.followTree = "0"; 0`);
  const before = await marked();
  await openFile("top.rs");
  await sleep(600);
  ok((await marked()) === before,
    `the mark does not move with following off — got ${await marked()}, was ${before}`);
  ok(await isOpen("a") && await isOpen("x/y"),
    "and turning it off collapses nothing that was already expanded");
} finally {
  if (page) page.close();
  browser.close();
  await roost.close();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
