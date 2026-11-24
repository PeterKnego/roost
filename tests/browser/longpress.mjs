//! On a phone: a long-press is the tree's right-click, a folder can be sent to
//! Claude, and the upload button stays on the Files pane.
//!
//! iOS Safari fires no `contextmenu` for a long-press, so before this the
//! tree's menu — New, Upload into a folder, Send to Claude, Rename, Delete —
//! was unreachable on an iPhone. The press is driven with `pointerType:
//! "touch"` pointer events, which is what app.js listens for; the menu that
//! opens is the real `askMenu` dialog.
//!
//! Revert-checks performed, all restored:
//!   (a) removing the `pointerdown` listener's timer -> A fails: no menu.
//!   (b) removing the capture-phase `click` swallow -> A fails: lifting the
//!       finger after the menu opened also opened the file as a tab.
//!   (c) putting Send to Claude back inside `if (rel && !isDir)` -> B fails.
//!   (d) gating the upload icon on `active.k === "Tree"` again -> C fails
//!       once a terminal is the Files pane's front tab.
//!
//! Run: deno run -A tests/browser/longpress.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.dir}/docs`, { recursive: true });
await Deno.writeTextFile(`${fx.dir}/docs/a.md`, "# a\n");
await Deno.writeTextFile(`${fx.dir}/notes.txt`, "hello\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector('.content[data-kind="Tree"] a[data-rel="notes.txt"]')`), 20, "tree");
  await evalIn(`revealPane(Number(document.querySelector('.content[data-kind="Tree"]').closest(".pane").dataset.pane)); true`);
  await sleep(300);

  // Presses `sel` for `ms` with a touch pointer, then lifts and clicks as a
  // finger would. Returns the menu's items as they were while it was open.
  const press = (sel, ms) => evalIn(`(async () => {
    const el = document.querySelector(${JSON.stringify(sel)});
    const r = el.getBoundingClientRect();
    const at = { bubbles: true, cancelable: true, pointerType: "touch", isPrimary: true,
                 clientX: r.left + 20, clientY: r.top + r.height / 2 };
    el.dispatchEvent(new PointerEvent("pointerdown", at));
    await new Promise((d) => setTimeout(d, ${ms}));
    const dlg = document.getElementById("dlg-menu");
    const items = dlg.open ? [...dlg.querySelectorAll(".dlg-item")].map((b) => b.textContent.trim()) : null;
    el.dispatchEvent(new PointerEvent("pointerup", at));
    el.click();
    await new Promise((d) => setTimeout(d, 600));
    return JSON.stringify(items);
  })()`).then(JSON.parse);
  const dismiss = () => evalIn(`(() => {
    const dlg = document.getElementById("dlg-menu");
    // Escape reaches dialog.js as \`cancel\`; a synthetic keydown would not.
    if (dlg.open) dlg.dispatchEvent(new Event("cancel", { cancelable: true }));
    return !dlg.open;
  })()`);
  const fileTabOpen = (rel) => evalIn(`state.panes.some((p) => p.tabs.some((t) => t.k === "File" && t.rel === ${JSON.stringify(rel)}))`);

  console.log("A. a long-press on a file opens its menu, and only the menu");
  const fileSel = '.content[data-kind="Tree"] a[data-rel="notes.txt"]';
  const items = await press(fileSel, 700);
  ok(Array.isArray(items) && items.includes("Rename…") && items.includes("Download"),
     `the file's menu opened (${JSON.stringify(items)})`);
  ok(!(await fileTabOpen("notes.txt")), "lifting the finger did not also open the file");
  await until(dismiss, 10, "menu closed");
  ok((await press(fileSel, 120)) === null, "a short tap opens no menu");
  ok(await until(() => fileTabOpen("notes.txt"), 10, "tab"), "and still opens the file");
  await evalIn(`revealPane(Number(document.querySelector('.content[data-kind="Tree"]').closest(".pane").dataset.pane)); true`);
  await sleep(300);

  console.log("\nB. a folder's menu offers Send to Claude");
  const launches = await evalIn(`LAUNCHES.includes("claude")`);
  const dirItems = await press('.content[data-kind="Tree"] details[data-rel="docs"] > summary', 700);
  ok(Array.isArray(dirItems) && dirItems.includes("Download as zip"), `the folder's menu opened (${JSON.stringify(dirItems)})`);
  if (launches) ok(dirItems.includes("Send to Claude"), "with Send to Claude in it");
  else console.log("  skip  claude is not on this host's PATH, so the item is rightly absent");
  await until(dismiss, 10, "menu closed");

  console.log("\nC. the upload button stays on the Files pane with a terminal in front");
  const pi = await evalIn(`state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"))`);
  await evalIn(`newTerminal(${pi}); true`);
  await until(() => evalIn(`state.panes[${pi}].tabs[state.panes[${pi}].active]?.k === "Terminal"`), 20, "terminal in front");
  await sleep(300);
  ok(await evalIn(`!!document.querySelector('.pane[data-pane="${pi}"] .paneicon.upload')`),
     "the Files pane still has its upload button");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
