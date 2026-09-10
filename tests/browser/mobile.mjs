//! roost on a phone: one pane at a time, and every affordance reachable by
//! touch.
//!
//! #15, piece 2. Before this there was no phone layout at all — not a cramped
//! one. `static/style.css` had no width breakpoint anywhere in a thousand
//! lines; the workspace grid asks for ~790px of fixed columns before the
//! middle pane gets a single pixel, and the viewport meta tells the browser to
//! lay that out at 390px and try. roost was reachable from a phone and
//! unusable on arrival.
//!
//! CLAUDE.md: "`tests/browser/` is the only thing that can see any of this. A
//! mobile layout needs a CDP run at a phone viewport, and a skip is not a
//! pass." So this file emulates a real device — width, DPR, `mobile: true` and
//! touch — rather than merely narrowing the window, because the two differ in
//! ways that matter here: `mobile: true` is what makes the viewport meta take
//! effect, and without touch emulation `matchMedia` and tap targets are being
//! asked the wrong question.
//!
//! The trap this file is written against is the one in tests/browser/README:
//! at 390px almost everything is offscreen or zero-sized, so an assertion that
//! merely finds an element, or reads a size without comparing it to the
//! viewport, passes for the wrong reason. Every assertion here compares
//! against the viewport or against the *other* panes.
//!
//! Run: deno run -A tests/browser/mobile.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

// A representative small phone. Narrower than an iPhone 15 (393) on purpose:
// if it works here it works on the common sizes, and 390 is what #15 names.
const PHONE = { width: 390, height: 844, deviceScaleFactor: 3, mobile: true };

const fx = await fixture();
await Deno.writeTextFile(`${fx.dir}/notes.md`, "# notes\n\nsome text\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const load = async (metrics) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await p.cmd("Emulation.setDeviceMetricsOverride", { ...metrics, screenOrientation: undefined });
  if (metrics.mobile) {
    await p.cmd("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
  }
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  return p;
};

const visiblePanes = `[...document.querySelectorAll('#grid > .pane')]
  .filter((p) => p.offsetParent !== null).map((p) => p.dataset.pane)`;
const barVisible = `!!document.getElementById("mobilebar")?.offsetParent`;

try {
  page = await load(PHONE);
  const { evalIn } = page;

  console.log("A. one pane at a time, and a way to switch");
  ok(await evalIn(barVisible), "the pane switcher is on screen");
  ok((await evalIn(`document.querySelectorAll("#mobilebar button").length`)) === 4,
     "with one button per pane");
  // The assertion the whole change exists for. Not "the grid is narrow" —
  // exactly one pane is in the layout, which is what makes the one on screen
  // usable at all.
  const shown = await evalIn(visiblePanes);
  ok(shown.length === 1, `exactly one pane is laid out, got ${JSON.stringify(shown)}`);
  ok(shown[0] === "3", "and it opens on the terminal — #15's whole point is talking to Claude");
  ok((await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').getAttribute("aria-pressed")`)) === "true",
     "the switcher says which one that is");

  console.log("B. the pane that is showing actually fills the phone");
  // The trap this guards: at 390px a pane can be 'visible' and 8px wide, or
  // pushed off the right edge entirely, and a test that only asked
  // `offsetParent !== null` would call that a pass.
  const box = await evalIn(`(() => { const p = document.querySelector('#grid > .pane[data-pane="3"]');
    const r = p.getBoundingClientRect();
    return { w: Math.round(r.width), h: Math.round(r.height), left: Math.round(r.left), right: Math.round(r.right) }; })()`);
  ok(box.w > PHONE.width * 0.9, `the pane is ${box.w}px of a ${PHONE.width}px viewport`);
  ok(box.left >= 0 && box.right <= PHONE.width + 1, `and it is on screen (${box.left}..${box.right})`);
  ok(box.h > 300, `with usable height, got ${box.h}px`);
  // Nothing may scroll the page sideways — the one thing the project's own
  // rules call out by name.
  ok((await evalIn(`document.documentElement.scrollWidth`)) <= PHONE.width + 1,
     "and the document does not scroll horizontally");

  console.log("B2. the widths that were actually broken");
  // Measured on `develop` before this change: at 768px (every tablet) the
  // middle pane rendered 2px wide and `scrollWidth` was 804 against a 768
  // viewport — the whole page scrolling sideways, which the workspace rules
  // forbid by name. A landscape phone at 844px was the same. Both are well
  // clear of any 640px breakpoint, which is why this file checks them.
  for (const [label, m] of [
    ["tablet portrait", { width: 768, height: 1024, deviceScaleFactor: 2, mobile: true }],
    ["phone landscape", { width: 844, height: 390, deviceScaleFactor: 3, mobile: true }],
    ["a desktop window dragged to 800", { width: 800, height: 900, deviceScaleFactor: 1, mobile: false }],
  ]) {
    await page.cmd("Emulation.setDeviceMetricsOverride", m);
    await sleep(250);
    const panes = await evalIn(visiblePanes);
    const docW = await evalIn(`document.documentElement.scrollWidth`);
    ok(panes.length === 1 && docW <= m.width + 1,
       `${label} (${m.width}px): one pane (${JSON.stringify(panes)}), no sideways scroll (${docW})`);
  }
  await page.cmd("Emulation.setDeviceMetricsOverride", PHONE);
  await sleep(250);

  console.log("B3. how much of the screen the chrome takes");
  // A budget, not a pixel count. Measured at 22% before the header was
  // trimmed — it wrapped to four rows at 390px — and that is before a soft
  // keyboard takes half of what is left, which is the state the pane you are
  // actually working in has to survive. Asserted as a ceiling so that adding a
  // control to the header cannot quietly spend the screen: the number moving
  // is fine, nobody noticing is not.
  const chrome = await evalIn(`(() => {
    const h = document.querySelector("header").getBoundingClientRect().height;
    const b = document.getElementById("mobilebar").getBoundingClientRect().height;
    return { header: Math.round(h), bar: Math.round(b), pct: Math.round(((h + b) / window.innerHeight) * 100) }; })()`);
  ok(chrome.pct <= 20,
     `header (${chrome.header}px) plus switcher (${chrome.bar}px) is ${chrome.pct}% of the screen`);
  // The two controls the phone layout drops, and the reason each is safe to
  // drop: this asserts the trim actually happened, because a rule that stopped
  // matching would show up here and nowhere else.
  ok((await evalIn(`["refresh", "projbtn"].filter((id) => !!document.getElementById(id)?.offsetParent)`)).length === 0,
     "refresh and the projects strip are dropped — the browser reload and the home link are their routes");
  ok(await evalIn(`!!document.getElementById("bell")?.offsetParent && !!document.getElementById("settings")?.offsetParent`),
     "while the ones with no other route stay");

  console.log("C. switching panes");
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="0"]').click()`);
  ok(await until(async () => JSON.stringify(await evalIn(visiblePanes)) === '["0"]', 5, "the tree"),
     "tapping Files shows the tree pane and only the tree pane");
  ok((await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').getAttribute("aria-pressed")`)) === "false",
     "and the switcher moves with it");

  console.log("D. opening a file brings its pane forward");
  // The gesture route. Without it a tap on a file in the tree opens the editor
  // in a pane that is not on screen — the intent goes out, the tab appears,
  // and nothing visible happens at all, which is the phone version of a
  // silent no-op.
  ok(await until(() => evalIn(`!!document.querySelector('.pane[data-pane="0"] .content a.file')`), 15, "the tree"),
     "the tree lists the project's files");
  await evalIn(`[...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
    .find((a) => a.textContent.includes("notes.md")).click()`);
  ok(await until(async () => JSON.stringify(await evalIn(visiblePanes)) === '["2"]', 10, "the editor"),
     "tapping a file switches to the editor pane");
  ok(await until(async () => (await evalIn(`document.querySelector('.pane[data-pane="2"] .content')?.textContent ?? ""`)).includes("notes"), 10, "content"),
     "and the file is what is in it");

  console.log("E. the terminal is usable at this width");
  // #15's argument is that the phone job is talking to Claude, so a phone
  // layout that renders a terminal it cannot type into has done nothing. The
  // assertion is on the *fitted column count*: an xterm in a pane that was
  // never re-fitted keeps the 80 columns it was built with and clips, and
  // "there is a terminal element" would pass either way.
  // Started while a DIFFERENT pane is on screen, which is the ordinary case
  // and the one with the bug in it: the terminal pane is `display:none` at
  // that moment, so the xterm mounts measuring zero. Only the switch re-fits
  // it. Starting it in the pane already showing would fit it by accident and
  // this section would pass with `showMobilePane`'s fit deleted.
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="0"]').click()`);
  const sess = await evalIn(`state.panes[3].tabs.find((t) => t.k === "Terminal")?.session ?? null`);
  ok(!!sess, "the terminal pane has a session to start");
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(sess)} })`);
  ok(await until(() => evalIn("terms.size > 0"), 30, "a terminal"), "its shell starts while its pane is hidden");
  await sleep(800);
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').click()`);
  await sleep(1200);
  const grid = await evalIn(`(() => { const t = [...terms.values()][0].term; return { cols: t.cols, rows: t.rows }; })()`);
  ok(grid.cols > 20 && grid.cols < 70,
     `switching to it fits it to the phone, not 80 columns or 0 (got ${grid.cols}x${grid.rows})`);
  const host = await evalIn(`(() => { const r = document.querySelector('.pane[data-pane="3"] .termhost').getBoundingClientRect();
    return { w: Math.round(r.width), right: Math.round(r.right) }; })()`);
  ok(host.right <= PHONE.width + 1, `and it does not run off the screen (right edge ${host.right})`);
  // term.input, never term.paste: bash turns on bracketed paste, under which
  // a pasted line is not run.
  // JSON.stringify, not an escape written straight into the template
  // literal: that way the carriage return is interpolated into the source
  // sent to the page as a real CR, which leaves the string literal it sits
  // inside unterminated. The page reports only "Invalid or unexpected
  // token", with no hint that the test wrote it.
  await evalIn(`[...terms.values()][0].term.input(${JSON.stringify("echo PHONE_OK" + String.fromCharCode(13))})`);
  const screenText = `(() => { const b = [...terms.values()][0].term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + String.fromCharCode(10);
      return s; })()`;
  ok(await until(async () => (await evalIn(screenText)).includes("PHONE_OK"), 20, "the echo"),
     "and it answers what is typed into it");

  console.log("E2. touch targets");
  // 44px is the floor Apple and Android both publish. The desktop sizes here
  // are 20px, which is a miss on every attempt.
  const small = await evalIn(`(() => {
    const out = [];
    for (const b of document.querySelectorAll("#mobilebar button, header button")) {
      if (!b.offsetParent) continue;
      const r = b.getBoundingClientRect();
      if (r.height < 44 || r.width < 44) out.push((b.id || b.dataset.mpane || b.textContent.trim() || "?") + ":" + Math.round(r.width) + "x" + Math.round(r.height));
    }
    return out; })()`);
  ok(small.length === 0, `every visible header/switcher control is at least 44x44 (small: ${JSON.stringify(small)})`);
  // iOS zooms the page when a focused input is under 16px and does not zoom
  // back out on blur, which strands the user at 2x with no way back.
  ok((await evalIn(`getComputedStyle(document.getElementById("searchinput")).fontSize`)) === "16px",
     "the search field is 16px, so focusing it does not zoom the page");

  console.log("F. a desktop is untouched");
  // The negative control, and the reason any of the above means anything: all
  // of it is scoped to a media query, and a rule that leaked would show here
  // as a desktop with three of its four panes gone.
  try { await page.close(); } catch { /* already gone */ }
  page = await load({ width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const desktop = await page.evalIn(visiblePanes);
  ok(desktop.length === 4, `all four panes are laid out on a desktop, got ${JSON.stringify(desktop)}`);
  ok(!(await page.evalIn(barVisible)), "and the switcher is not on screen");
  ok((await page.evalIn(`getComputedStyle(document.querySelector('#grid > .divider')).display`)) !== "none",
     "the dividers are back, so a desktop can still resize its panes");
  // The scoping check for B3's trim, and it is here because B3 cannot make it:
  // "hidden on a phone" is equally true of a rule that hides them everywhere.
  // Revert-checked — moving `#refresh, #projbtn { display: none }` out of the
  // media query passes the whole file without this line.
  ok((await page.evalIn(`["refresh", "projbtn"].filter((id) => !!document.getElementById(id)?.offsetParent)`)).length === 2,
     "and both controls the phone drops are back — the trim is scoped to the phone, not global");

  console.log("F2. the band just above the breakpoint");
  // Not broken before, but the narrowest useful thing on a 1000px screen was
  // the pane holding the file you are reading: 192px. Asserted as a floor
  // rather than an exact number, because the number is a consequence of the
  // side columns' clamps and not a promise.
  await page.cmd("Emulation.setDeviceMetricsOverride", { width: 1000, height: 900, deviceScaleFactor: 1, mobile: false });
  await sleep(250);
  const midW = await page.evalIn(`Math.round(document.querySelector('#grid > .pane[data-pane="2"]').getBoundingClientRect().width)`);
  ok(midW >= 300, `at 1000px the middle pane is ${midW}px, not the 192px the unclamped grid gives`);
  ok((await page.evalIn(`document.documentElement.scrollWidth`)) <= 1000, "and nothing scrolls sideways");
  await page.cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await sleep(250);
  ok((await page.evalIn(`Math.round(document.querySelector('#grid > .pane[data-pane="2"]').getBoundingClientRect().width)`)) > 550,
     "while a real desktop keeps the width it always had — the clamp is scoped, not global");

  console.log("G. a desktop window dragged narrow becomes a phone, with no reload");
  // The case a media query gets right and a User-Agent check does not. It is
  // also the one that catches an init that only ever runs on first paint.
  await page.cmd("Emulation.setDeviceMetricsOverride", PHONE);
  ok(await until(async () => (await page.evalIn(visiblePanes)).length === 1, 5, "one pane"),
     "narrowing the window collapses to one pane");
  ok(await page.evalIn(barVisible), "and the switcher appears without a reload");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
