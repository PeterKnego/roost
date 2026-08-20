//! Does selecting text in a terminal put it on the clipboard, and say so?
//!
//! Why this has to exist at all: xterm's rows are `user-select: none`, so a
//! native browser selection over terminal text is impossible — xterm's own
//! selection is the only route to the clipboard. The routes that used to reach
//! it were browser-native ones (Cmd+C, right-click → Copy), and a full-screen
//! app that turns on mouse reporting takes the right button — and with it the
//! context menu — for itself. So in a terminal running Claude Code, selecting
//! and copying did nothing at all: the clipboard kept whatever it held, and
//! the next paste inserted that instead. If what it held was an image, resh's
//! own image-paste route then typed an image into the app.
//!
//! Both states are asserted, because the interesting one is the second: mouse
//! reporting off (a plain shell) and on (what Claude Code turns on, where the
//! selection is made with shift held).
//!
//! Run: deno run -A tests/browser/copyselect.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn, cmd } = page;
  await cmd("Browser.grantPermissions", {
    origin: `http://127.0.0.1:${resh.port}`,
    permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"],
  });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";
    window.__mouse = () => __t().term.modes.mouseTrackingMode;
    window.__flash = () => __t().node.dataset.flash || "";
    window.__rowRect = (needle) => { const rows = [...document.querySelectorAll(".xterm-rows div")];
      const r = rows.find((n) => n.textContent.includes(needle));
      if (!r) return null; const b = r.getBoundingClientRect();
      return { x: b.left + 4, y: b.top + b.height / 2, x2: b.left + 4 + 7 * needle.length }; };`);

  const clip = () => evalIn(`navigator.clipboard.readText().then((t) => t, () => "<unreadable>")`);
  const seed = () => evalIn(`navigator.clipboard.writeText("STALE-CLIPBOARD").then(() => "ok")`);

  const MARKER = "SELECTME-abc123-END";
  const drag = async (shift) => {
    await evalIn(`__t().term.clearSelection(); __t().node.dataset.flash = "";`);
    const r = await evalIn(`__rowRect(${JSON.stringify(MARKER)})`);
    if (!r) throw new Error("marker row not on screen");
    const mods = shift ? 8 : 0;
    const ev = (type, x, buttons) => cmd("Input.dispatchMouseEvent",
      { type, x, y: r.y, button: "left", buttons, clickCount: 1, modifiers: mods });
    await ev("mousePressed", r.x, 1);
    for (const x of [r.x + 20, r.x + 60, r.x2]) { await ev("mouseMoved", x, 1); await sleep(60); }
    await ev("mouseReleased", r.x2, 0);
    await sleep(900); // the copy is debounced: a drag settles before it fires
  };

  console.log("A. start a terminal with something worth selecting");
  const find = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return { pi, ti, session: state.panes[pi].tabs[ti].session }; } return null; })()`;
  let loc = await evalIn(find);
  if (!loc) {
    await evalIn(`send({ t: "NewTerminal", pane: 0 })`);
    await until(async () => !!(loc = await evalIn(find)), 15, "a terminal tab");
  }
  await evalIn(`send({ t: "ActivateTab", pane: ${loc.pi}, idx: ${loc.ti} })`);
  await sleep(500);
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => evalIn("terms.size > 0 && !!__t().sock && __t().sock.readyState === 1"), 30, "socket"),
     "terminal socket open");
  await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "shell prompt");
  await evalIn(`__t().term.input("echo ${MARKER}\\r")`);
  await until(() => evalIn(`__txt().includes(${JSON.stringify(MARKER)})`), 20, "marker");

  console.log("\nB. a plain shell: select with the mouse");
  await seed();
  await drag(false);
  ok(await evalIn(`__t().term.getSelection().length > 0`), "xterm registered the selection (guards the rest)");
  const one = await clip();
  ok(one.includes("echo") || one.includes(MARKER.slice(0, 8)),
     `the selection reached the clipboard (${JSON.stringify(one.slice(0, 40))})`);
  ok(/copied/.test(await evalIn("__flash()")), `and the terminal said so (${await evalIn("__flash()")})`);

  console.log("\nC. clearing a selection must not clobber what was copied");
  await evalIn(`__t().term.clearSelection()`);
  await sleep(900);
  const still = await clip();
  ok(still === one, "an empty selection left the clipboard alone");

  console.log("\nD. the case that broke: a full-screen app owns the mouse");
  await evalIn(`__t().term.input("printf '\\\\033[?1000h\\\\033[?1002h\\\\033[?1003h\\\\033[?1006h'; sleep 300\\r")`);
  await sleep(2000);
  ok(await evalIn("__mouse()") === "any", "mouse reporting is on, as Claude Code leaves it");
  await seed();
  await drag(true); // shift, the way a real terminal bypasses an app's mouse
  ok(await evalIn(`__t().term.getSelection().length > 0`), "shift-drag still selects locally");
  const two = await clip();
  ok(two.includes(MARKER.slice(0, 8)) || two.includes("echo"),
     `the selection reached the clipboard with the app holding the mouse (${JSON.stringify(two.slice(0, 40))})`);
  ok(/copied/.test(await evalIn("__flash()")), "and the terminal said so there too");
} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
