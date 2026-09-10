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

  console.log("E3. the header popups open where the finger is");
  // #projpanel has two triggers (HEADER_POPUPS in app.js: `projbtn` and
  // `projname`) and is anchored `right: 8px` for the first of them. The phone
  // trim hides `projbtn`, so the only thing left to tap is the project name on
  // the LEFT while the panel opened hard against the right edge — measured at
  // 390px, the name at x 35-85 and the panel at x 151-382, with nothing
  // connecting them. Reported from a phone.
  //
  // Asserted against the viewport rather than against the trigger: the fix is
  // that these span the screen, so "aligned with the button" is the wrong
  // question. All three are checked because they are anchored to *different*
  // sides, and a rule that caught one would be equally wrong about the others.
  for (const [panel, trigger] of [["projpanel", "projname"], ["noticepanel", "bell"], ["wtpanel", "wtbtn"]]) {
    await evalIn(`(() => { const t = document.getElementById(${JSON.stringify(trigger)});
      t.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); t.click(); })()`);
    await sleep(300);
    const r = await evalIn(`(() => { const e = document.getElementById(${JSON.stringify(panel)});
      if (!e || !e.offsetParent) return null; const b = e.getBoundingClientRect();
      return { l: Math.round(b.left), r: Math.round(b.right) }; })()`);
    ok(r && r.l <= 8 && r.r >= PHONE.width - 8,
       `#${panel} spans the screen instead of hanging off one edge (${JSON.stringify(r)})`);
    await evalIn(`document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }))`);
    await sleep(150);
  }

  console.log("E4. the terminal scrolls under a finger");
  // Reported from a phone as "sticking". `.xterm-viewport` is a real
  // scrollable div, so it scrolls — and then hands the leftover delta to the
  // page the moment it hits an end, which stalls the gesture and starts the
  // next drag somewhere else.
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').click()`);
  await sleep(300);
  await evalIn(`[...terms.values()][0].term.input(${JSON.stringify("seq 1 300" + String.fromCharCode(13))})`);
  ok(await until(async () => (await evalIn(screenText)).includes("300"), 20, "output"),
     "a terminal with more output than fits");
  const vp = `document.querySelector('.pane[data-pane="3"] .xterm-viewport')`;
  ok(await until(async () => (await evalIn(`${vp} ? ${vp}.scrollHeight - ${vp}.clientHeight : 0`)) > 50, 10, "scrollback"),
     "and it has somewhere to scroll to");
  await evalIn(`${vp}.scrollTop = ${vp}.scrollHeight`);
  const atBottom = await evalIn(`${vp}.scrollTop`);
  const mid = await evalIn(`(() => { const r = ${vp}.getBoundingClientRect();
    return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }; })()`);
  // Raw touch events, not `Input.synthesizeScrollGesture`. Its touch path is
  // inert in chrome-headless-shell — measured: a 400px touch gesture moved the
  // viewport 3900 -> 3900 while the same gesture as `mouse` moved it to 3770,
  // and a hand-dispatched touch sequence to 3700. A test built on it would
  // have reported "the terminal does not scroll" forever, about a terminal
  // that scrolls.
  const touch = (type, y) => page.cmd("Input.dispatchTouchEvent",
    { type, touchPoints: type === "touchEnd" ? [] : [{ x: mid.x, y }] });
  const drag = async (from, steps) => {
    await touch("touchStart", from);
    for (let i = 1; i <= steps; i++) { await touch("touchMove", from + i * 25); await sleep(16); }
    await touch("touchEnd", 0);
    await sleep(500);
  };
  await drag(mid.y - 150, 8);
  const scrolled = await evalIn(`${vp}.scrollTop`);
  ok(scrolled < atBottom, `a finger dragged down scrolls the terminal back (${atBottom} -> ${scrolled})`);

  // The half that was reported as sticking. `.xterm-viewport` is a real
  // scrollable div, so it always scrolled; what it also did was hand the
  // leftover delta to the page on reaching an end, which stalls the gesture
  // and starts the next drag from somewhere else.
  //
  // Asserted on the computed property as well as the behaviour, and the
  // distinction is worth being plain about: the drag above is a real gesture,
  // while `overscroll-behavior` is checked as a *style* because the page in
  // this layout has nothing to scroll anyway — so a behavioural check of the
  // chaining would pass with the property removed. It is a regression guard
  // on the fix, not a demonstration of it.
  ok((await evalIn(`getComputedStyle(${vp}).overscrollBehaviorY`)) === "contain",
     "and the gesture is kept inside the terminal rather than chaining to the page");
  ok((await evalIn(`getComputedStyle(${vp}).touchAction`)) === "pan-y",
     "with the browser told up front that a vertical drag is a scroll");
  await drag(mid.y - 200, 16);
  ok((await evalIn(`window.scrollY`)) === 0, "dragging past the top leaves the page where it was");

  console.log("E5. the terminal key bar");
  // Claude's TUI is driven with arrows and Enter, and a phone soft keyboard has
  // neither — so its menus ("1. yes / 2. no", a file picker, a permission
  // prompt) were simply unreachable. Reported from a phone.
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').click()`);
  await sleep(400);
  ok(await evalIn(`!!document.getElementById("termkeys")?.offsetParent`),
     "the key bar is on screen with the terminal in front");
  ok(await evalIn(`document.querySelector('#mobilebar button[data-mpane="0"]').click(), true`)
     && await until(async () => !(await evalIn(`!!document.getElementById("termkeys")?.offsetParent`)), 5, "hidden"),
     "and not over the file tree, where those keys mean nothing");
  await evalIn(`document.querySelector('#mobilebar button[data-mpane="3"]').click()`);
  await sleep(400);

  // What the buttons actually send. Recorded at `term.input`, because the
  // bytes are the whole point: an arrow is not one byte string, and a TUI that
  // has set DECCKM expects ESC O A where a shell expects ESC [ A. Sending the
  // wrong one moves nothing and looks like a dead button.
  await evalIn(`window.__sent = []; (() => { const e = ${JSON.stringify("x")} && null; })();
    (function () { const t = [...terms.values()][0].term; const orig = t.input.bind(t);
      t.input = (d) => { window.__sent.push(d); return orig(d); }; })()`);
  const press = async (k) => {
    await evalIn(`(() => { const b = document.querySelector('#termkeys button[data-k="${k}"]');
      b.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true })); })()`);
    await sleep(120);
  };
  await press("up"); await press("down"); await press("enter"); await press("esc");
  const sent = await evalIn(`window.__sent`);
  ok(JSON.stringify(sent) === JSON.stringify(["\u001b[A", "\u001b[B", "\r", "\u001b"]),
     `a shell at a prompt gets the normal cursor sequences: ${JSON.stringify(sent)}`);

  // The mode-aware half. Putting the terminal into application-cursor-keys
  // mode is what Claude's menus do, and the same button must then send a
  // different sequence — this is the assertion that fails if the bytes are
  // hard-coded, which is how the buttons would be dead exactly where they are
  // needed.
  await evalIn(`window.__sent = []; [...terms.values()][0].term.write("\u001b[?1h")`);
  await sleep(400);
  await press("up"); await press("down");
  const appSent = await evalIn(`window.__sent`);
  ok(JSON.stringify(appSent) === JSON.stringify(["\u001bOA", "\u001bOB"]),
     `and a TUI in DECCKM gets the application ones: ${JSON.stringify(appSent)}`);
  await evalIn(`[...terms.values()][0].term.write("\u001b[?1l")`);

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

  // Reported from the desktop as well as the phone: the project dropdown
  // opened across the header from the name you clicked. #projpanel has two
  // triggers and was anchored in CSS to the right edge for one of them, so
  // clicking the project name on the left opened a panel on the right.
  for (const trigger of ["projname", "projbtn"]) {
    await page.evalIn(`(() => { const p = document.getElementById("projpanel");
      if (!p.hidden) { p.hidden = true; } })()`);
    await page.evalIn(`document.getElementById(${JSON.stringify(trigger)}).click()`);
    await sleep(250);
    const g = await page.evalIn(`(() => {
      const p = document.getElementById("projpanel"), t = document.getElementById(${JSON.stringify(trigger)});
      if (!p || p.hidden) return null;
      const pr = p.getBoundingClientRect(), tr = t.getBoundingClientRect();
      return { dx: Math.round(pr.left - tr.left), below: Math.round(pr.top - tr.bottom) }; })()`);
    ok(g && Math.abs(g.dx) <= 8 && g.below >= 0 && g.below < 40,
       `opened from #${trigger} it sits under it (${JSON.stringify(g)})`);
  }
  await page.evalIn(`document.getElementById("projpanel").hidden = true`);

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
