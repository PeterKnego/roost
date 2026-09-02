//! A file that disappears from under an open tab: deleted, or moved out of
//! the project where nothing can follow it.
//!
//! A rename *within* the project is a different outcome now — the tab follows
//! the file — and has its own test in renamed.mjs. This one is the case where
//! there is nowhere to follow to.
//!
//! Reported from a live instance: a file was renamed from a terminal, and the
//! tab kept the old name over an *empty* editor. Nothing in that is visible to
//! `cargo test`. The server pushes a file's text as `BufferText` and app.js's
//! `mountEditor` seeds the textarea from it; a file that cannot be read
//! produces no push, so the editor mounted with `""` — an empty textarea,
//! labelled "saved", under a filename, which is the exact shape CLAUDE.md
//! names as how work gets overwritten.
//!
//! Every move here is done on disk with no browser involved, because that is
//! how it happens: a Claude in a terminal pane, or a `git mv`.
//!
//! Run: deno run -A tests/browser/vanished.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

// autosave off, for section D: it needs a buffer that is still dirty when its
// file goes away, which with autosave on is a race against a 1s timer.
const fx = await fixture({ autosave: false });
await Deno.writeTextFile(`${fx.roots}/proj/notes.rs`, "fn one() {}\nfn two() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/draft.rs`, "fn draft() {}\n");

const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const wire = (p) => {
  const { evalIn } = p;
  const open = async (rel) => {
    const row = `[...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .some((x) => x.dataset.rel === ${JSON.stringify(rel)})`;
    if (!await until(() => evalIn(row), 10, `the tree row for ${rel}`)) throw new Error(`no row for ${rel}`);
    await evalIn(`(() => { const a = [...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .find((x) => x.dataset.rel === ${JSON.stringify(rel)});
      a.dispatchEvent(new MouseEvent("click", { bubbles: true })); })()`);
    return until(() => tab(rel).then((t) => t !== null), 10, `a tab for ${rel}`);
  };
  const tab = (rel) => evalIn(
    `JSON.stringify(state.panes.flatMap((p) => p.tabs).find((t) => t.k === "File" && t.rel === ${JSON.stringify(rel)}) || null)`,
  ).then(JSON.parse);
  const buffer = (rel) => evalIn(
    `JSON.stringify(state.buffers.find((b) => b.rel === ${JSON.stringify(rel)}) || null)`,
  ).then(JSON.parse);
  // The middle pane's own content, and whether a textarea is mounted in it.
  const pane = () => evalIn(`JSON.stringify((() => {
    const c = document.querySelector('.pane[data-pane="2"] .content');
    if (!c) return null;
    const ta = c.querySelector("textarea");
    return { text: c.textContent.slice(0, 160), hasEditor: !!ta, editorText: ta ? ta.value : null,
             modebtn: (() => { const b = c.querySelector(".path .modebtn"); return b ? b.title : null; })() };
  })())`).then(JSON.parse);
  const paused = (rel) => evalIn(`autosavePaused.has(${JSON.stringify(rel)})`);
  const ready = () => until(
    () => evalIn(`typeof state !== "undefined" && !!(state && state.panes) && !!document.querySelector('ul.tree')`),
    20, "app",
  );
  return { evalIn, open, tab, buffer, pane, paused, ready, close: p.close };
};

try {
  page = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`));
  await page.evalIn(`1`);
  ok(await page.ready(), "the workspace is up");

  console.log("A. a code file, open in the editor it opens in by default");
  ok(await page.open("notes.rs"), "notes.rs opens");
  ok((await page.tab("notes.rs"))?.mode === "Edit", "in Edit, since that is how a text file opens");
  ok(await until(async () => (await page.pane()).editorText === "fn one() {}\nfn two() {}\n", 10, "the editor's text"),
     "with the file's text in it — the control for everything below");

  console.log("\nB. moved out of the project, where nothing can follow it");
  // Out of the watched tree entirely: inotify delivers the `From` half of the
  // rename and no `To`, which is exactly the information a project that cannot
  // see the destination has. `renamed.mjs` covers the paired case.
  await Deno.rename(`${fx.roots}/proj/notes.rs`, `${fx.base}/carried-away.rs`);
  ok(await until(async () => (await page.tab("notes.rs"))?.mode === "Preview", 10, "the demotion"),
     "the tab leaves Edit, so nothing paints a textarea over a file that is gone");
  {
    // Polled, not read once: the demotion arrives as a State event and the
    // pane re-mounts by fetching its fragment, so reading straight after the
    // mode flip can catch it mid-swap with an empty `.content`. Seen for real
    // while checking that this test fails with the fix reverted — an empty
    // pane would have passed `!hasEditor` on its own, which is why that half
    // is asserted only once the text has landed.
    const settled = await until(async () => /not found/.test((await page.pane()).text), 10, "the not-found pane");
    const p = await page.pane();
    ok(settled, `the pane says what happened (got ${JSON.stringify(p.text.slice(0, 60))})`);
    // The assertion the report is actually about. `hasEditor` alone would not
    // do it: the old bug's editor existed and was empty, which is worse than
    // absent, so the pane has to say why instead.
    ok(!p.hasEditor, "and no textarea is mounted over the missing file");
  }
  ok((await page.tab("notes.rs")) !== null, "the tab itself survives — a checkout that puts the file back must find it");
  ok((await page.buffer("notes.rs")) === null, "and its clean buffer is dropped, so no reconnect re-reads a dead path");

  console.log("\nC. and after a reload, which is where the empty editor was seen");
  await page.close();
  page = wire(await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`));
  ok(await page.ready(), "the reloaded page is up");
  {
    const settled = await until(async () => /not found/.test((await page.pane()).text), 10, "the not-found pane");
    const p = await page.pane();
    ok(settled, "still says what happened");
    ok(!p.hasEditor, "still no textarea");
    // Without this the demoted tab is stranded: re-clicking the file in the
    // tree only focuses the tab it already has, and never revisits its mode.
    ok(p.modebtn === "switch to edit", `and offers a way back to Edit (got ${JSON.stringify(p.modebtn)})`);
  }

  console.log("\nD. a deleted file with unsaved work in it is not treated the same way");
  ok(await page.open("draft.rs"), "draft.rs opens");
  await until(async () => (await page.pane()).editorText === "fn draft() {}\n", 10, "draft's text");
  await page.evalIn(`(() => { const ta = document.querySelector('.pane[data-pane="2"] .content textarea');
    ta.value = "fn draft() {}\\nunsaved work\\n"; ta.dispatchEvent(new Event("input", { bubbles: true })); })()`);
  ok(await until(async () => (await page.buffer("draft.rs"))?.dirty === true, 10, "the dirty buffer"),
     "typing makes it dirty");
  // Same proof as renamed.mjs: without it this section passes on a race won
  // rather than on a buffer that is genuinely still unsaved when the file goes.
  await new Promise((r) => setTimeout(r, 1500));
  ok(await page.evalIn(`AUTOSAVE === false`), "the project config turned autosave off");
  ok((await Deno.readTextFile(`${fx.roots}/proj/draft.rs`)) === "fn draft() {}\n",
     "so a second later the edit is still only in the browser");
  // A plain delete this time, not a move: the two arrive as different events
  // (`Remove` versus an unpaired rename `From`) and both have to land here.
  await Deno.remove(`${fx.roots}/proj/draft.rs`);
  ok(await until(async () => (await page.buffer("draft.rs"))?.stale === true, 10, "the stale flag"),
     "losing the file marks the buffer stale");
  ok((await page.tab("draft.rs"))?.mode === "Edit",
     "the tab stays in Edit — Preview would hide the only copy of that text");
  ok((await page.pane()).editorText === "fn draft() {}\nunsaved work\n", "and the unsaved work is still on screen");
  // With autosave off nothing was going to write anyway; what this asserts is
  // that the client *flags* the buffer on BufferStale, which is what would hold
  // autosave back on a project that has it on. Deleting that line in app.js
  // fails here.
  ok(await page.paused("draft.rs"), "and the client flags it, so autosave could not write it back to a path that is gone");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
