//! The Files pane and the terminal, driven by something that is not a mouse. #110.
//!
//! Three reports from an iPhone that are one thing: v0.6.0 made the *layout*
//! work on a phone, but the Files pane and the terminal were still driven by
//! right-click, drag and mouse-drag.
//!
//! **One of the three is a real bug on desktop too** and is tested as such: a
//! right-click on a *folder* offered to create in the project root, because
//! directories are `<details>` and the menu was wired only to `<a>` rows.
//!
//! The nesting matters. Section A uses `a/b`, not a top-level folder, because
//! with a top-level `sub` "the folder itself" and "the parent of its parent"
//! are both `""` — a half-fix passes against the shallow case and fails the
//! nested one.
//!
//! Revert-checks performed, all restored:
//!   (a) dropping the `isDir` branch in `fileMenu` -> A's folder assertions
//!       fail, offering `""`/`"a"`; the file and blank-pane controls stay
//!       green, which is what says they are controls.
//!   (b) dropping `stopPropagation` from the summary handler -> A's
//!       "exactly one menu" fails with 2: the container handler written for
//!       blank space fires as well, targeting the root.
//!   (c) binding to `details` rather than `details > summary` -> the nested
//!       folder reports 2 menus, because the outer `<details>` contains the
//!       inner row and its handler fires for descendants too.
//!   (d) select mode that sets its flag but does not gate `wireTouchScroll`
//!       -> C's "no mouse events with the mode off" still passes, but "the
//!       viewport did not move" fails — which is why both halves are asserted.
//!
//! **What this cannot test**, the same limit #97 hit and worth repeating: this
//! host is headless Linux with Chromium. Whether iOS Safari raises
//! `contextmenu` on a long-press, and whether a real finger drags a selection
//! inside xterm, are not reachable here. The gestures need a device; the
//! targeting, the menu contents, the upload call and the mode's state machine
//! are what a browser can honestly cover.
//!
//! Run: deno run -A tests/browser/touchfiles.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.dir}/a/b`, { recursive: true });
await Deno.writeTextFile(`${fx.dir}/a/b/f.txt`, "x\n");
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector("ul.tree details[data-rel]")`), 20, "tree");
  // Both levels expanded, so the nested folder and the file inside it are in
  // the DOM. Lazily-loaded fragments are wired by `wireFileLinks` on insert;
  // if that path missed the new binding, section A would catch it here.
  await evalIn(`document.querySelector('ul.tree details[data-rel="a"]').open = true; 0`);
  await until(() => evalIn(`!!document.querySelector('details[data-rel="a/b"]')`), 15, "a/b");
  await evalIn(`document.querySelector('details[data-rel="a/b"]').open = true; 0`);
  await until(() => evalIn(`!!document.querySelector('a[data-rel="a/b/f.txt"]')`), 15, "a/b/f.txt");

  // The dialogs are stubbed so the assertion is on *what was asked for*, which
  // is the whole contract. Asserting that a dialog opened would pass against
  // every version of this bug.
  await evalIn(`window.__asked = []; window.__menus = 0; window.__items = [];
    askText = (o) => { window.__asked.push(o.value); return Promise.resolve(null); };
    askMenu = (o) => { window.__menus++; window.__items = o.items.map((i) => i.id); return Promise.resolve("new"); };
    true`);

  const rightClick = async (sel) => {
    await evalIn(`window.__asked = []; window.__menus = 0; true`);
    const found = await evalIn(`(() => {
      const el = document.querySelector(${JSON.stringify(sel)});
      if (!el) return false;
      el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
      return true;
    })()`);
    await sleep(350);
    return {
      found,
      asked: JSON.parse(await evalIn(`JSON.stringify(window.__asked)`)),
      menus: await evalIn(`window.__menus`),
    };
  };

  console.log("A. the file menu targets the directory you opened it on");
  const nested = await rightClick('details[data-rel="a/b"] > summary');
  ok(nested.found, "the nested folder a/b is in the tree");
  // The bug, in one line: this used to be "untitled.txt" — the project root.
  ok(nested.asked[0] === "a/b/untitled.txt",
     `a nested folder offers a path inside itself (got ${JSON.stringify(nested.asked)})`);
  // Exactly one. The container handler written for blank space also fires
  // without stopPropagation, and its menu targets the root.
  ok(nested.menus === 1, `exactly one menu opened (got ${nested.menus})`);

  const top = await rightClick('details[data-rel="a"] > summary');
  ok(top.asked[0] === "a/untitled.txt",
     `a top-level folder too (got ${JSON.stringify(top.asked)})`);
  ok(top.menus === 1, `and one menu (got ${top.menus})`);

  // The two controls. A fix that made every right-click use its own rel would
  // break the file case; one that made every click use the root would break
  // both folders. Neither passes with both of these asserted.
  const file = await rightClick('a[data-rel="a/b/f.txt"]');
  ok(file.asked[0] === "a/b/untitled.txt",
     `a file still offers a path beside it (got ${JSON.stringify(file.asked)})`);
  // Counted here too, and it is not symmetry for its own sake — revert-check
  // (c) is the reason. A file row lives *inside* its folder's element, and the
  // `<a>` handler does not stop propagation, so binding the folder menu to the
  // `<details>` rather than to its `<summary>` opens a second menu on every
  // file in an expanded folder. Without this line that swap failed nothing.
  ok(file.menus === 1, `and one menu for a file inside a folder (got ${file.menus})`);
  const blank = await rightClick('.pane .content');
  ok(blank.asked[0] === "untitled.txt",
     `blank space still offers the project root (got ${JSON.stringify(blank.asked)})`);
  ok(blank.menus === 1, `and one for blank space (got ${blank.menus})`);

  console.log("\nB. upload no longer needs a drag");
  ok((await evalIn(`JSON.stringify(window.__items)`)).includes("upload"),
     "the menu offers Upload files…");
  await evalIn(`window.__up = null;
    uploadFiles = (files, dir) => { window.__up = { n: files.length, dir }; };
    askMenu = () => Promise.resolve("upload"); true`);
  await evalIn(`document.querySelector('details[data-rel="a/b"] > summary')
    .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }))`);
  await sleep(350);
  ok(await evalIn(`!!document.querySelector('input[type=file]')`), "it opens a file picker");
  // The picker itself cannot be driven headless, so its `change` is fired with
  // a synthetic FileList — the same shape the browser would hand it.
  const up = await evalIn(`(() => {
    const i = document.querySelector('input[type=file]');
    if (!i) return "no input";
    const dt = new DataTransfer();
    dt.items.add(new File(["u"], "up.txt", { type: "text/plain" }));
    Object.defineProperty(i, "files", { value: dt.files, configurable: true });
    i.onchange();
    return JSON.stringify(window.__up);
  })()`);
  ok(up === '{"n":1,"dir":"a/b"}',
     `the chosen file is sent to the folder the menu was opened on (got ${up})`);
  // A picker left in the document keeps its last selection, and re-picking the
  // same file then fires no `change` at all — the second upload would silently
  // never happen.
  ok(await evalIn(`!document.querySelector('input[type=file]')`), "and the picker is discarded after use");

  console.log("\nC. a terminal can be selected with a finger");
  const loc = JSON.parse(await evalIn(`(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return JSON.stringify({ session: state.panes[pi].tabs[ti].session }); } return null; })()`));
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  await until(() => evalIn(`terms.has(${JSON.stringify(loc.session)})`), 30, "terminal");
  await evalIn(`document.body.dataset.mpane = "3"; 0`);
  await evalIn(`window.__e = () => [...terms.values()][0];
    window.__mice = [];
    for (const t of ["mousedown", "mousemove", "mouseup"]) {
      document.addEventListener(t, () => window.__mice.push(t), true);
    }
    window.__drag = () => {
      window.__mice = [];
      const n = __e().node, r = n.getBoundingClientRect();
      const mk = (type, y, live) => new TouchEvent(type, { bubbles: true, cancelable: true,
        touches: live ? [new Touch({ identifier: 1, target: n, clientX: r.left + 30, clientY: y })] : [],
        changedTouches: [new Touch({ identifier: 1, target: n, clientX: r.left + 30, clientY: y })] });
      const before = __e().term.buffer.active.viewportY;
      n.dispatchEvent(mk("touchstart", r.top + 20, true));
      n.dispatchEvent(mk("touchmove", r.top + 80, true));
      n.dispatchEvent(mk("touchend", r.top + 80, false));
      return { mice: window.__mice, moved: __e().term.buffer.active.viewportY !== before };
    }; true`);
  const press = () => evalIn(`document.querySelector('#termkeys button[data-k="select"]')
    .dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }))`);

  // The bar is at its tap-target floor with this button on it; `paste.mjs`
  // owns that assertion, but a select button too small to press is this
  // file's problem too.
  const sel = JSON.parse(await evalIn(`(() => {
    const r = document.querySelector('#termkeys button[data-k="select"]').getBoundingClientRect();
    return JSON.stringify({ w: Math.round(r.width), h: Math.round(r.height) });
  })()`));
  ok(sel.w >= 44 && sel.h >= 44, `the select button is tappable (${sel.w}x${sel.h})`);
  ok(await evalIn(`!__e().selectMode`), "select mode starts off");
  await press(); await sleep(250);
  ok(await evalIn(`!!__e().selectMode`), "pressing select turns it on");
  ok((await evalIn(`document.querySelector('#termkeys button[data-k="select"]').getAttribute("aria-pressed")`)) === "true",
     "and says so, for anything that is not looking at a colour");

  const on = JSON.parse(await evalIn(`JSON.stringify(__drag())`));
  ok(on.mice.join(",") === "mousedown,mousemove,mouseup",
     `a drag becomes the mouse events xterm's selection is built on (got ${JSON.stringify(on.mice)})`);
  // Both halves, and (d) in the header is why: a mode that lights up and still
  // scrolls passes the first assertion on its own.
  ok(!on.moved, "and the terminal did not scroll under it");

  await press(); await sleep(250);
  ok(await evalIn(`!__e().selectMode`), "pressing again turns it off");
  const off = JSON.parse(await evalIn(`JSON.stringify(__drag())`));
  ok(off.mice.length === 0,
     `and the same drag goes back to scrolling, forwarding nothing (got ${JSON.stringify(off.mice)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
