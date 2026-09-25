//! Downloading a file from the tree. #120.
//!
//! The menu item lives in `static/app.js`, which no Rust test reaches — the
//! route's own behaviour (headers, chunking, refusals, filename escaping) is
//! covered in `src/routes.rs`. What is only testable here is that the gesture
//! produces a real download with the right name and the right bytes.
//!
//! **A real download, not a `fetch`.** The menu item is a top-level navigation,
//! and that is the whole reason it needs no `Origin` check and no XHR. Driving
//! it with `fetch` would exercise a path no user takes and would pass against a
//! menu item that navigates nowhere. `Browser.setDownloadBehavior` puts the
//! file on disk so the assertion is on bytes that actually landed.
//!
//! Revert-checks performed, all restored:
//!   (a) making the Download item unconditional (dropping `rel && !isDir`) ->
//!       section C fails: the item appears on a folder.
//!   (b) pointing the navigation at `/frag/{p}/raw?path=` instead of
//!       `download` -> section B fails on the bytes, because `raw` refuses a
//!       non-image with a 404 page.
//!   (c) dropping the `dirty` branch in the label -> section D fails.
//!
//! Run: deno run -A tests/browser/download.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture({ autosave: false });
const BODY = "line one\nline two\n";
await Deno.writeTextFile(`${fx.dir}/build.log`, BODY);
await Deno.mkdir(`${fx.dir}/sub`, { recursive: true });
const dlDir = `${fx.base}/downloads`;
await Deno.mkdir(dlDir, { recursive: true });

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: dlDir });
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector("ul.tree")`), 20, "tree");

  // The menu is driven for real; only the *choice* is stubbed, because a
  // <dialog> menu cannot be clicked through reliably and the choice is not
  // what this file is testing.
  const openMenuOn = async (sel, choice) => {
    await evalIn(`window.__items = null;
      askMenu = (o) => { window.__items = o.items.map((i) => ({ id: i.id, label: i.label }));
                         return Promise.resolve(${JSON.stringify(choice)}); }; true`);
    const found = await evalIn(`(() => {
      const el = document.querySelector(${JSON.stringify(sel)});
      if (!el) return false;
      el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 8, clientY: 8 }));
      return true;
    })()`);
    await sleep(350);
    return { found, items: JSON.parse(await evalIn(`JSON.stringify(window.__items)`) || "null") };
  };

  console.log("A. the menu offers Download on a file");
  const onFile = await openMenuOn('a[data-rel="build.log"]', null);
  ok(onFile.found, "the file is in the tree");
  ok((onFile.items || []).some((i) => i.id === "download"), `Download is offered (${JSON.stringify(onFile.items)})`);

  console.log("\nB. choosing it lands the file, with the right name and bytes");
  await openMenuOn('a[data-rel="build.log"]', "download");
  const landed = `${dlDir}/build.log`;
  const arrived = await until(async () => {
    try { return (await Deno.readTextFile(landed)) === BODY; } catch { return false; }
  }, 20, "the downloaded file");
  ok(arrived, `build.log arrived with its exact bytes at ${landed}`);

  console.log("\nC. a folder is not offered a download");
  // Nothing to download, and an item that did nothing is the shape the menu
  // was rebuilt to remove — rename/delete are absent at the project root for
  // the same reason.
  const onDir = await openMenuOn('details[data-rel="sub"] > summary', null);
  ok(onDir.found, "the folder is in the tree");
  ok(!(onDir.items || []).some((i) => i.id === "download"),
     `Download is not offered on a folder (${JSON.stringify(onDir.items)})`);

  console.log("\nD. a dirty file says which version it hands back");
  // A tab with unsaved edits holds text the server has never seen. A download
  // that silently returned the on-disk file would be the quiet kind of wrong,
  // so the label says so.
  ok(await evalIn(`AUTOSAVE === false`), "autosave is off, so the buffer can stay dirty");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "build.log", mode: "Edit" } }); true`);
  await until(() => evalIn(`!!document.querySelector('.pane[data-pane="2"] textarea')`), 20, "the editor");
  await evalIn(`send({ t: "EditBuffer", rel: "build.log", text: "edited, not saved\\n" }); true`);
  ok(await until(() => evalIn(
       `!!(state.buffers || []).find((b) => b.rel === "build.log" && b.dirty)`), 20, "a dirty buffer"),
     "the buffer is dirty");
  const dirty = await openMenuOn('a[data-rel="build.log"]', null);
  const item = (dirty.items || []).find((i) => i.id === "download");
  ok(item && /saved version/i.test(item.label),
     `the label says which version (${JSON.stringify(item && item.label)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
