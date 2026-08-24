//! The tab strip wraps, and the terminal under it re-fits when it does.
//!
//! The strip used to be one row that scrolled horizontally with its scrollbar
//! hidden (`scrollbar-width: none`), so once a pane held more tabs than fit
//! across it — four or five in the 520px right column — the rest were
//! reachable only by a trackpad's horizontal gesture, and with a mouse not at
//! all. Wrapping fixes that, but it also makes the pane header's height depend
//! on the tab count, which resizes .content under a terminal that render()
//! otherwise leaves mounted and untouched.
//!
//! Both halves live in static/style.css and static/app.js, where `cargo test`
//! cannot reach.
//!
//! Run: deno run -A tests/browser/tabwrap.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const NAMES = [];
// 28, not 16: enough to exceed the 4-row cap even now that tabs are
// content-sized (the hidden × stopped reserving ~25px per tab on 2026-08-24,
// and the ✎ left the tab for the filename stripe — 17 tabs then fit in
// exactly 4 rows and section C's premise silently vanished).
for (let i = 1; i <= 28; i++) {
  const n = `a${i}.md`;
  NAMES.push(n);
  await Deno.writeTextFile(`${fx.roots}/proj/${n}`, `# ${n}\n`);
}
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const PANE = 3; // the right pane: 520px wide, and it holds the default terminal

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  // Headless Chromium opens 800x600, which is *narrower* than the default
  // left (260px) and right (520px) panes put together: the middle column
  // collapses to nothing and the right pane hangs off the edge of the
  // viewport. Every measurement below would then be describing that rather
  // than the strip — including elementFromPoint, which returns null off-screen
  // and would fail the reachability assertions for the wrong reason.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  // Measured off getBoundingClientRect, not off the CSS: the question is where
  // the tabs actually are on screen, which is what a user's mouse has to
  // reach. `outside` counts tabs whose box leaves the strip's own box — with a
  // single non-wrapping row that is every tab past the right edge.
  await evalIn(`window.__m = (i) => {
    const el = document.querySelector('.pane[data-pane="${PANE}"]');
    const strip = el.querySelector('.tabstrip'), head = el.querySelector('.panehead');
    const sr = strip.getBoundingClientRect();
    const els = [...strip.querySelectorAll('.tab')];
    const r = els.map((t) => t.getBoundingClientRect());
    const pick = i === undefined ? els.length - 1 : i;
    const b = r[pick], el2 = els[pick];
    const cx = Math.round((b.left + b.right) / 2), cy = Math.round((b.top + b.bottom) / 2);
    const hit = document.elementFromPoint(cx, cy);
    return { n: els.length, head: Math.round(head.getBoundingClientRect().height),
             rows: new Set(r.map((x) => Math.round(x.top))).size,
             overflowX: strip.scrollWidth - strip.clientWidth,
             hidden: strip.scrollHeight - strip.clientHeight,
             outside: r.filter((x) => x.right > sr.right + 1 || x.left < sr.left - 1
                                   || x.bottom > sr.bottom + 1 || x.top < sr.top - 1).length,
             x: cx, y: cy, label: el2.textContent,
             hit: !!(hit && (el2 === hit || el2.contains(hit))) };
  };`);
  const m = (i) => evalIn(`JSON.stringify(__m(${i === undefined ? "" : i}))`).then(JSON.parse);
  const pane = () => evalIn(`JSON.stringify(state.panes[${PANE}])`).then(JSON.parse);
  const mountedKey = () => evalIn(`document.querySelector('.pane[data-pane="${PANE}"] .content').dataset.mountedKey`);
  const open = async (n) => {
    await evalIn(`send({ t: "OpenTab", pane: ${PANE}, tab: { k: "File", rel: ${JSON.stringify(n)}, mode: "Preview" } })`);
    return await until(async () => (await pane()).tabs.some((t) => t.rel === n), 10, n);
  };
  const clickAt = async (x, y) => {
    for (const type of ["mousePressed", "mouseReleased"]) {
      await cmd("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 });
    }
  };

  console.log("A. more tabs than fit across the pane");
  await evalIn(`send({ t: "StartTerminal", session: "term" })`);
  ok(await until(() => evalIn(`terms.has("term") && !!document.querySelector('.pane[data-pane="${PANE}"] .content .termhost')`), 30, "the terminal"),
     "the default terminal is live and mounted in the right pane");
  const one = await m();
  ok(one.rows === 1, `one tab, one row (head ${one.head}px)`);

  for (const n of NAMES.slice(0, 6)) await open(n);
  const many = await m();
  console.log(`    ${many.n} tabs, ${many.rows} row(s), head ${many.head}px, overflowX ${many.overflowX}px`);
  // Three faces of the same defect. All three fail on the pre-wrap CSS: rows
  // stays 1, overflowX is hundreds of pixels, and every tab past the right
  // edge counts as outside.
  ok(many.rows > 1, `the tabs wrapped onto ${many.rows} rows`);
  ok(many.overflowX <= 1, `nothing is parked off to the right (overflowX ${many.overflowX}px)`);
  ok(many.outside === 0, `all ${many.n} tabs are inside the strip's box (${many.outside} outside)`);
  ok(many.head > one.head, `the pane header grew to fit them (${one.head}px → ${many.head}px)`);

  console.log("\nB. the last tab is reachable with a mouse");
  await evalIn(`send({ t: "ActivateTab", pane: ${PANE}, idx: 0 })`);
  ok(await until(async () => (await pane()).active === 0, 10, "terminal active"), "the terminal is active again");
  const last = await m();
  ok(last.hit, `the last tab (${last.label.replace(/[✎×]/g, "")}) is what is painted at its own centre`);
  // A real click at those coordinates, not element.click(): the whole point is
  // that the pixel is hittable. Off-strip, this lands on nothing and the
  // active tab never changes.
  await clickAt(last.x, last.y);
  ok(await until(async () => (await pane()).active === last.n - 1, 10, "the last tab active"),
     "clicking it activates it");

  console.log("\nC. past the row cap it scrolls, and says so");
  for (const n of NAMES.slice(6)) await open(n);
  await evalIn(`send({ t: "ActivateTab", pane: ${PANE}, idx: 0 })`);
  await until(async () => (await pane()).active === 0, 10, "terminal active");
  const full = await m();
  console.log(`    ${full.n} tabs, ${full.rows} row(s), head ${full.head}px, ${full.hidden}px below the fold`);
  ok(full.hidden > 0, `more rows than the cap allows (${full.hidden}px below the fold)`);
  // Bounded, or a pane full of buffers would be all header and no terminal.
  ok(full.head <= 120, `the header stayed bounded at ${full.head}px rather than growing to ${full.rows} rows`);
  ok(!full.hit, "the last tab is out of view to begin with");
  // A wheel over the strip, which is the gesture the visible scrollbar
  // advertises. Nothing else in this file scrolls it. The pointer has to be
  // moved onto the strip first — a bare wheel event goes wherever Chromium
  // last thought the mouse was, which is over a tab that scrolls nothing.
  const first = await m(0);
  await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: first.x, y: first.y });
  await cmd("Input.dispatchMouseEvent", { type: "mouseWheel", x: first.x, y: first.y, deltaX: 0, deltaY: 300 });
  ok(await until(async () => (await m()).hit, 10, "the last tab to scroll into view"),
     "a wheel over the strip brings it into view");
  const scrolled = await m();
  await clickAt(scrolled.x, scrolled.y);
  ok(await until(async () => (await pane()).active === scrolled.n - 1, 10, "the last tab active"),
     "and it is clickable once there");

  console.log("\nD. closing tabs re-fits the terminal underneath");
  await evalIn(`send({ t: "ActivateTab", pane: ${PANE}, idx: 0 })`);
  ok(await until(() => evalIn(`!!document.querySelector('.pane[data-pane="${PANE}"] .content .termhost')`), 10, "terminal remount"),
     "the terminal is showing again");
  const key0 = await mountedKey();
  const rows0 = await evalIn(`terms.get("term").term.rows`);
  const head0 = (await m()).head;
  // Always the *last* tab: workspace::apply_layout clamps `active` on close
  // rather than shifting it, so closing a tab *before* the terminal would
  // slide a different tab under the active index — the pane would remount,
  // mountTab would fit it on the way in, and this would pass with the re-fit
  // deleted.
  let head1 = head0;
  // Close until a full ROW's worth of height is freed, not until the first
  // pixel moves: right at the row cap, losing the below-the-fold overflow
  // shaves ~4px (the capped max-height vs the natural 4-row height) without
  // freeing a terminal row — the re-fit runs and correctly changes nothing,
  // and asserting growth on that 4px was asserting the wrong thing. A strip
  // row is 26px; 20 is that with slack.
  for (let i = 0; i < NAMES.length && head1 > head0 - 20; i++) {
    const p = await pane();
    await evalIn(`send({ t: "CloseTab", pane: ${PANE}, idx: ${p.tabs.length - 1} })`);
    await until(async () => (await pane()).tabs.length === p.tabs.length - 1, 10, "the tab to close");
    head1 = (await m()).head;
  }
  ok(head1 <= head0 - 20, `closing tabs dropped a row from the header (${head0}px → ${head1}px)`);
  ok((await mountedKey()) === key0 && key0.startsWith("Terminal"),
     `the terminal was never remounted across those closes (${key0})`);
  const rows1 = await evalIn(`terms.get("term").term.rows`);
  ok(rows1 > rows0, `the terminal grew into the freed row (${rows0} → ${rows1} rows)`);

  console.log("\nE. hidden controls take no width on inactive tabs");
  // The × kept its box at opacity:0 until 2026-08-24 (and the ✎ beside it,
  // before that control moved to the filename stripe), which put an
  // invisible tail on every inactive tab — a strip of md files read as tabs
  // scattered across the pane (reported from real use). The × is
  // display:none until the tab is hovered or active; the tab growing under
  // the pointer on hover is the accepted cost of content-sized tabs.
  await open(NAMES[0]);
  await evalIn(`send({ t: "ActivateTab", pane: ${PANE}, idx: 0 })`);
  ok(await until(async () => (await pane()).active === 0, 10, "terminal active"), "the terminal is the active tab");
  const probe = () => evalIn(`JSON.stringify((() => {
    const tabs = [...document.querySelectorAll('.pane[data-pane="${PANE}"] .tabstrip .tab')];
    const t = tabs.find((x) => x.textContent.includes(${JSON.stringify(NAMES[0])}));
    if (!t) return null;
    return { w: Math.round(t.getBoundingClientRect().width),
             displays: [...t.querySelectorAll('.x')].map((x) => getComputedStyle(x).display),
             active: t.classList.contains('active') };
  })())`).then(JSON.parse);
  const idle = await probe();
  ok(!!idle && !idle.active && idle.displays.length === 1 && idle.displays.every((d) => d === "none"),
     `an inactive tab's × is display:none, and it is the only hidden control (got ${idle && idle.displays.join(", ")})`);
  const fi = (await pane()).tabs.findIndex((t) => t.rel === NAMES[0]);
  await evalIn(`send({ t: "ActivateTab", pane: ${PANE}, idx: ${fi} })`);
  ok(await until(async () => !!(await probe())?.active, 10, "file tab active"), "the file tab activates");
  const act = await probe();
  ok(act.displays.length === 1 && act.displays[0] !== "none", `the active tab renders its × (${act.displays.join(", ")})`);
  ok(act.w > idle.w + 10, `activation is what pays for its width (${idle.w}px → ${act.w}px)`);
} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
