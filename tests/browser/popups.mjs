//! The header popups (notifications, running-projects, worktree switcher):
//! opening one must not steal keyboard focus from the terminal or editor, and
//! clicking anywhere outside an open popup must close it.
//!
//! Both are real mouse-event behaviours — focus is grabbed on *mousedown*, so
//! `element.click()` (which dispatches only a click) would exercise neither the
//! bug nor the fix. This drives genuine CDP mouse events, which is why it lives
//! here and not in `cargo test`.
//!
//! Revert-checks (2026-08-24):
//!   - dropping the mousedown-preventDefault loop fails A (focus moves to the
//!     #bell button).
//!   - dropping the capture-phase document handler fails B (the panel stays
//!     open after an outside click).
//!
//! Run: deno run -A tests/browser/popups.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  // A real click at an element's on-screen centre — mousePressed then
  // mouseReleased, so the mousedown that grabs (or, fixed, does not grab)
  // focus actually fires.
  const box = (sel) => evalIn(
    `(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null;
       const r = e.getBoundingClientRect();
       return { x: Math.round((r.left+r.right)/2), y: Math.round((r.top+r.bottom)/2) }; })()`
  ).then((s) => (typeof s === "string" ? JSON.parse(s) : s));
  const clickAt = async (x, y) => {
    for (const type of ["mousePressed", "mouseReleased"]) {
      await cmd("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 });
    }
  };
  const clickSel = async (sel) => { const b = await box(sel); if (!b) throw new Error(`no ${sel}`); await clickAt(b.x, b.y); };
  const hidden = (id) => evalIn(`document.getElementById(${JSON.stringify(id)}).hidden`);
  // What the keyboard is pointed at: the tag, plus whether it is the xterm
  // textarea (xterm marks its own with .xterm-helper-textarea).
  const focusInfo = () => evalIn(`(() => { const a = document.activeElement;
    return JSON.stringify({ tag: a ? a.tagName : null, id: a ? a.id : "",
      xterm: !!(a && a.classList && a.classList.contains("xterm-helper-textarea")) }); })()`).then(JSON.parse);

  console.log("A. opening a popup does not steal focus from the terminal");
  await evalIn(`send({ t: "StartTerminal", session: "term" })`);
  ok(await until(() => evalIn(`terms.has("term") && !!document.querySelector('.termhost')`), 30, "terminal"),
     "the terminal is live");
  // Put focus in the terminal the way a user would land there, then confirm it.
  await evalIn(`terms.get("term").term.focus()`);
  ok(await until(async () => (await focusInfo()).xterm, 10, "terminal focused"),
     "focus starts in the terminal");

  await clickSel("#bell");
  ok(await until(async () => (await hidden("noticepanel")) === false, 10, "notices open"),
     "clicking the bell opens the notifications panel");
  const f = await focusInfo();
  ok(f.xterm, `and focus stayed in the terminal, not the button (activeElement: ${f.tag}#${f.id})`);

  console.log("\nB. clicking outside an open popup closes it");
  // The project name is header chrome, safely outside the panel and its bell.
  await clickSel("header .proj");
  ok(await until(async () => (await hidden("noticepanel")) === true, 10, "notices closed"),
     "an outside click dismisses the panel");

  console.log("\nC. the trigger still toggles it shut");
  await clickSel("#bell");
  await until(async () => (await hidden("noticepanel")) === false, 10, "reopen");
  await clickSel("#bell");
  ok(await until(async () => (await hidden("noticepanel")) === true, 10, "toggled shut"),
     "clicking the bell again closes it");

  console.log("\nD. opening one popup closes another");
  await clickSel("#bell");
  await until(async () => (await hidden("noticepanel")) === false, 10, "notices open again");
  await clickSel("#wtbtn");
  ok(await until(async () => (await hidden("noticepanel")) === true && (await hidden("wtpanel")) === false, 10, "swap"),
     "opening the worktree switcher closes the notifications panel");
} finally {
  page?.close();
  browser.close();
  await roost.close();
}
Deno.exit(fail ? 1 : 0);
