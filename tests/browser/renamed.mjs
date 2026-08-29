//! A file renamed inside the project, from outside resh: the tab follows it.
//!
//! inotify gives both halves of a rename the same cookie and `notify` merges
//! them into one `Modify(Name(Both))` carrying both paths, so the watcher is
//! told where the file went rather than inferring it — see
//! docs/superpowers/specs/2026-08-29-follow-external-renames-design.md. The
//! server half is unit-tested; what only a browser can show is that the tab on
//! screen ends up addressing the new path with the right text in it, which is
//! two client-side maps (`texts`, `editors`) keyed by a rel that just changed
//! underneath them.
//!
//! The dirty half is the one with teeth: app.js prunes `texts` against every
//! State's buffer list, so the `BufferText` carrying unsaved work to the new
//! rel has to arrive *after* the State that moves the tab, or the editor
//! re-mounts empty over the user's edit.
//!
//! Run: deno run -A tests/browser/renamed.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.writeTextFile(`${fx.roots}/proj/notes.rs`, "fn one() {}\nfn two() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/draft.rs`, "fn draft() {}\n");
await Deno.mkdir(`${fx.roots}/proj/sub`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/proj/sub/deep.rs`, "fn deep() {}\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const wire = (p) => {
  const { evalIn } = p;
  const tab = (rel) => evalIn(
    `JSON.stringify(state.panes.flatMap((q) => q.tabs).find((t) => t.k === "File" && t.rel === ${JSON.stringify(rel)}) || null)`,
  ).then(JSON.parse);
  const open = async (rel) => {
    const row = `[...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .some((x) => x.dataset.rel === ${JSON.stringify(rel)})`;
    if (!await until(() => evalIn(row), 10, `the tree row for ${rel}`)) throw new Error(`no row for ${rel}`);
    await evalIn(`(() => { const a = [...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .find((x) => x.dataset.rel === ${JSON.stringify(rel)});
      a.dispatchEvent(new MouseEvent("click", { bubbles: true })); })()`);
    return until(() => tab(rel).then((t) => t !== null), 10, `a tab for ${rel}`);
  };
  const expand = (dir) => evalIn(`(() => { const d = [...document.querySelectorAll('.pane[data-pane="0"] .content details')]
    .find((x) => x.dataset.rel === ${JSON.stringify(dir)}); if (!d) return false; d.open = true;
    d.dispatchEvent(new Event("toggle")); return true; })()`);
  const buffer = (rel) => evalIn(
    `JSON.stringify(state.buffers.find((b) => b.rel === ${JSON.stringify(rel)}) || null)`,
  ).then(JSON.parse);
  const pane = () => evalIn(`JSON.stringify((() => {
    const c = document.querySelector('.pane[data-pane="2"] .content');
    if (!c) return null;
    const ta = c.querySelector("textarea");
    return { text: c.textContent.slice(0, 160), editorText: ta ? ta.value : null };
  })())`).then(JSON.parse);
  const paused = (rel) => evalIn(`autosavePaused.has(${JSON.stringify(rel)})`);
  const ready = () => until(
    () => evalIn(`typeof state !== "undefined" && !!(state && state.panes) && !!document.querySelector('ul.tree')`),
    20, "app",
  );
  return { evalIn, open, expand, tab, buffer, pane, paused, ready, close: p.close };
};

try {
  page = wire(await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`));
  ok(await page.ready(), "the workspace is up");

  console.log("A. a clean file, renamed with `mv` from outside");
  ok(await page.open("notes.rs"), "notes.rs opens in a tab");
  ok(await until(async () => (await page.pane()).editorText === "fn one() {}\nfn two() {}\n", 10, "its text"),
     "with its text in the editor");
  await Deno.rename(`${fx.roots}/proj/notes.rs`, `${fx.roots}/proj/moved.rs`);
  ok(await until(async () => (await page.tab("moved.rs")) !== null, 10, "the moved tab"),
     "the tab follows the file to its new name");
  ok((await page.tab("notes.rs")) === null, "and nothing is left addressing the old one");
  ok((await page.tab("moved.rs"))?.mode === "Edit", "still in Edit — nothing about a move makes a file unreadable");
  {
    const p = await page.pane();
    ok(p.editorText === "fn one() {}\nfn two() {}\n", `the editor still holds the file (got ${JSON.stringify(p.editorText)})`);
    ok(!/not found/.test(p.text), "and nothing claims the file is missing");
  }

  console.log("\nB. a file with unsaved work, renamed underneath the editor");
  ok(await page.open("draft.rs"), "draft.rs opens");
  await until(async () => (await page.pane()).editorText === "fn draft() {}\n", 10, "draft's text");
  await page.evalIn(`(() => { const ta = document.querySelector('.pane[data-pane="2"] .content textarea');
    ta.value = "fn draft() {}\\nunsaved work\\n"; ta.dispatchEvent(new Event("input", { bubbles: true })); })()`);
  ok(await until(async () => (await page.buffer("draft.rs"))?.dirty === true, 10, "the dirty buffer"), "typing makes it dirty");
  await Deno.rename(`${fx.roots}/proj/draft.rs`, `${fx.roots}/proj/final.rs`);
  ok(await until(async () => (await page.tab("final.rs")) !== null, 10, "the moved tab"), "its tab follows too");
  // The assertion this whole file is for. `texts` is keyed by rel and pruned
  // against every State's buffer list, so an editor re-mounting at the new name
  // has nothing to seed from unless the server re-sent the text after the State.
  ok((await page.pane()).editorText === "fn draft() {}\nunsaved work\n",
     `the unsaved work is still on screen at the new name (got ${JSON.stringify((await page.pane()).editorText)})`);
  ok((await page.buffer("final.rs"))?.dirty === true, "and the buffer is still dirty, under the new key");
  ok((await page.buffer("final.rs"))?.stale === false,
     "not stale: moving a file changes its name, not its bytes, so nothing diverged");
  ok(!(await page.paused("final.rs")), "so autosave is not paused either");

  console.log("\nC. a directory rename takes every tab under it");
  ok(await page.expand("sub"), "the sub directory expands");
  ok(await page.open("sub/deep.rs"), "sub/deep.rs opens");
  await Deno.rename(`${fx.roots}/proj/sub`, `${fx.roots}/proj/renamed-dir`);
  ok(await until(async () => (await page.tab("renamed-dir/deep.rs")) !== null, 10, "the re-pathed tab"),
     "the tab moves with the directory, not just with a file");
  ok((await page.tab("sub/deep.rs")) === null, "and the old path is gone from the tab strip");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
