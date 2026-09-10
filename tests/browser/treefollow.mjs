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

  console.log("\nD2. a collapsed directory keeps what was expanded inside it");
  // The invariant this feature claims — "only ever expands and scrolls, never
  // collapses" — rested on "a closed <details> has never been fetched, so its
  // <ul> is empty and swapping it loses nothing". That is only true of a
  // *never-opened* one. `hx-trigger="toggle once"` fetches on first expand;
  // collapsing afterwards leaves the children in place, nested expansions and
  // all. Replacing such a node wholesale silently discards them.
  ok(await expand("x"), "setup: x is expanded");
  ok(await expand("x/y"), "setup: and x/y inside it");
  await evalIn(`(() => { const d = ${TREE}.querySelector('details[data-rel="x"]');
    if (d) d.open = false; return 0; })()`);
  ok(!(await isOpen("x")), "setup: x is collapsed again, with x/y still loaded inside it");
  ok(
    await evalIn(`!!${TREE}.querySelector('details[data-rel="x/y"]')`),
    "setup: x/y's node is still there — collapsing does not unload it",
  );

  // Follow to something under x, so x is on the path and gets re-expanded.
  await Deno.writeTextFile(`${fx.roots}/proj/x/deep.rs`, "fn deep() {}\n");
  await openFile("x/deep.rs");
  ok(await until(async () => (await marked()) === '["x/deep.rs"]', 10, "followed into x"),
    `following into x marks the file — got ${await marked()}`);
  ok(
    await evalIn(`!!${TREE}.querySelector('a.file[data-rel="x/y/unrelated.rs"]')`),
    "and x/y's loaded children survive being on the followed path",
  );

  console.log("\nD3. the mark clears when the last file tab closes");
  // Otherwise the tree goes on claiming the user is looking at a file that is
  // no longer open.
  ok(
    await evalIn(`state.panes[2].tabs.some((t) => t.k === "File")`),
    "setup: at least one file tab is open",
  );
  // One at a time, waiting for each. A batch of `CloseTab` intents addresses
  // tabs by index against a list the server is renumbering as it goes, and
  // reading the pane straight after sending them reports the state before any
  // of them landed — which is what made the first draft of this diagnose the
  // wrong thing.
  for (let n = 0; n < 8; n++) {
    const idx = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File")`);
    if (idx < 0) break;
    await evalIn(`send({ t: "CloseTab", pane: 2, idx: ${idx} }); 0`);
    await until(async () => await evalIn(
      `state.panes[2].tabs.filter((t) => t.k === "File").length`) < 8 - n, 10, "one closed");
  }
  ok(
    await evalIn(`!state.panes[2].tabs.some((t) => t.k === "File")`),
    `setup: every file tab is closed — panes ${await evalIn(`JSON.stringify(state.panes.map((p) => p.tabs.map((t) => t.rel ?? t.k)))`)}`,
  );
  ok(
    await until(async () => (await marked()) === "[]", 10, "mark cleared"),
    `with no file open the tree marks nothing — got ${await marked()}`,
  );

  console.log("\nD4. a stale follow response cannot land on top of a newer one");
  // Two quick tab switches issue two overlapping fetches with no ordering. If
  // the first resolves last it marks and scrolls to the file you already
  // left, and on an idle workspace nothing ever corrects it. The ordering is
  // forced here rather than raced for: a real race would pass most runs.
  await openFile("a/b/c/deep.rs");
  await openFile("p/q/r/second.rs");
  await until(async () => (await marked()) === '["p/q/r/second.rs"]', 10, "settled");

  await evalIn(`(() => {
    window.__realFetch = window.fetch;
    // Hold back the *first* tree fetch from here on, and let the second pass.
    let n = 0;
    window.fetch = (u, o) => {
      const p = window.__realFetch(u, o);
      if (typeof u === "string" && u.includes("/tree?dir=&open=") && n++ === 0) {
        return p.then((r) => new Promise((res) => setTimeout(() => res(r), 1500)));
      }
      return p;
    };
    return 0; })()`);
  await openFile("a/b/c/deep.rs");   // its response is delayed 1.5s
  await openFile("p/q/r/second.rs"); // this one lands first
  await sleep(2500);                 // long enough for the stale one to arrive
  ok(
    (await marked()) === '["p/q/r/second.rs"]',
    `the superseded response is dropped — got ${await marked()}`,
  );
  await evalIn(`window.fetch = window.__realFetch; 0`);

  console.log("\nD5. a failed follow leaves the tree alone");
  // `r.text()` alone accepts a 404's `no such project` body as a listing.
  // It yields no <li>, so the merge empties `ul.tree` — the whole pane and
  // every expansion in it — and calls that a follow. "I could not look"
  // rendered as "there is nothing there".
  ok(await expand("p"), "setup: p is expanded");
  const before404 = await evalIn(`${TREE}.querySelectorAll("li").length`);
  ok(before404 > 0, `setup: the tree has ${before404} rows`);
  await evalIn(`(() => {
    window.__realFetch = window.fetch;
    window.fetch = (u, o) =>
      (typeof u === "string" && u.includes("/tree?dir=&open="))
        ? Promise.resolve(new Response("no such project", { status: 404 }))
        : window.__realFetch(u, o);
    return 0; })()`);
  await openFile("a/b/c/deep.rs");
  await sleep(1200);
  ok(
    await evalIn(`${TREE}.querySelectorAll("li").length`) >= before404,
    `a 404 does not empty the tree — ${before404} rows before, ${await evalIn(`${TREE}.querySelectorAll("li").length`)} after`,
  );
  await evalIn(`window.fetch = window.__realFetch; 0`);

  console.log("\nE. with the setting off, nothing follows");
  // Its own file: D3 deliberately closes every file tab, so without this E
  // would be asserting about a mark it had just cleared.
  await openFile("a/b/c/deep.rs");
  await until(async () => (await marked()) === '["a/b/c/deep.rs"]', 10, "re-marked for E");
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
  // Not optional, and not tidiness. `fixture()`'s cleanup is what runs
  // `killByCmdline(stateDir)` and removes the temp tree, and the default
  // layout gives the right pane a Terminal tab — so this test starts a real
  // dtach master and its login shell, and without this they outlive it.
  // harness.mjs records what that looks like: "Two /tmp/roost-browser-* trees
  // were once found abandoned on a live host, one of them still holding a
  // running dtach master and its login shell." Measured on this host after a
  // day of runs: 74 abandoned trees and 9 live shells.
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
