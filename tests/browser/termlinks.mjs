//! Are terminal links marked only when the user asks for them?
//!
//! No Rust test reaches static/app.js, so the matchers, the modifier gate and
//! everything a click does live entirely outside `cargo test`.
//!
//! The trap this file is written against: asserting that a mouse event was
//! accepted rather than that a link exists. xterm returns link ranges from a
//! provider; "the provider ran" is true with the gate deleted. So this drives
//! the registered providers themselves and asserts on the ranges they hand
//! back — and it asserts the seeded row was actually found first, because
//! "zero links" is also what an off-screen row produces.
//!
//! The gate is armed with a real CDP key event, never by assigning
//! `linksArmed`: the keydown listener is half of what this task delivers, and
//! setting the flag by hand would leave it untested. (Verified empirically on
//! this host: `e.ctrlKey` IS true on the Control keydown itself when CDP is
//! given `modifiers: 2`, so `linkModifier(e)` arms on the very first event.)
//!
//! Row lookup is bottom-up and matches the row's *whole* trimmed text, not a
//! substring. Seeding by running a command puts the needle on screen twice —
//! once in the echoed command line, once in its output — and a first-match
//! substring search finds the command line, so the assertions would be made
//! against a row that also contains `printf`, quotes and other paths.
//!
//! Revert-the-fix, each one applied, run, watched fail, then restored:
//!   0. Deleted the `registerTermLinks(term, entry)` call in ensureTerm —
//!      the state of the tree before this task. Four failed:
//!        FAIL  two link providers are registered on the terminal (got 0)
//!        FAIL  a path is offered as a link while the modifier is held (got 0: [])
//!        FAIL  both matchers do claim these cells, so ordering is what
//!        resolves it ([])
//!        FAIL  https://example.com/a/b is one whole-URL link, not a path
//!        link over its tail (got [])
//!      Both "no link offered" assertions (1 and 4) went on passing, since
//!      no providers and a closed gate look identical from outside. That is
//!      exactly why the registration guard and the row-on-screen guards are
//!      here: without them this whole file would be green against a tree
//!      with the feature deleted.
//!   1. Deleted `if (!linksArmed) return cb(undefined);` from matchProvider.
//!      Assertion 1 alone failed with:
//!        FAIL  no link is offered with the modifier up (got 1: ["docs/backlog.md"])
//!   2. Swapped the two entries of the `providers` array in
//!      registerTermLinks, so the path provider registers first. Assertion 3
//!      alone failed with:
//!        FAIL  https://example.com/a/b is one whole-URL link, not a path
//!        link over its tail (got ["/example.com/a/b"])
//!      — and the control above it printed
//!      `[["/example.com/a/b"],["https://example.com/a/b"]]`, i.e. both
//!      matchers still claimed the cells and only precedence had changed.
//!   3. Changed PATH_RE's `(?:[\w.@+-]+\/)+` to `(?:[\w.@+-]+\/)*`, allowing
//!      zero directory segments. Assertion 4 alone failed with:
//!        FAIL  a bare filename with no directory offers no link (got 1: ["backlog.md"])
//!   4. Put nudgeLinks' synthetic mousemove back on the `.termhost` element
//!      instead of `.xterm-screen`. Section F alone failed with:
//!        FAIL  arming alone marked the path under the resting pointer (got null)
//!   5. Put nudgeLinks' detour back to a sideways one — a different column on
//!      the same line, off the real `cols` — instead of a different row.
//!      Section F alone failed the same way:
//!        FAIL  arming alone marked the path under the resting pointer (got null)
//!      4 and 5 are both bugs this task shipped and then measured out; see
//!      task-4-report.md. Sections B–E stay green through both, because they
//!      ask the providers directly and never go near the hover path — which
//!      is exactly why F is here.
//! All restored afterwards; the run passes clean again (see task-4-report.md
//! for the exact terminal output).
//!
//! Run: deno run -A tests/browser/termlinks.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const PATH = "docs/backlog.md";
const URL_ = "https://example.com/a/b";
const BARE = "backlog.md";

const fx = await fixture();
// The fixture project holds only hello.md. Task 5 resolves a clicked path
// against the real project, so the path this file prints is a file that
// actually exists — seeded before the server starts, so its tree is right.
await Deno.mkdir(`${fx.base}/roots/proj/docs`, { recursive: true });
await Deno.writeTextFile(`${fx.base}/roots/proj/${PATH}`, "# backlog\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn, cmd } = page;
  // Section F measures real pointer geometry, and the default 800x600 headless
  // window collapses the middle column (README, trap 5).
  await cmd("Emulation.setDeviceMetricsOverride",
            { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";
    // Bottom-up, whole-row equality: see this file's header on why a
    // substring search would find the echoed command line instead.
    window.__rowY = (needle) => { const b = __t().term.buffer.active;
      for (let i = b.length - 1; i >= 0; i--) { const l = b.getLine(i);
        if (l && l.translateToString(true).trim() === needle) return i + 1; }
      return -1; };
    // Asks the registered providers directly, then applies the same
    // first-provider-wins rule xterm's Linkifier applies in
    // _removeIntersectingLinks — which is where registration order does its
    // work, and which no public API exposes.
    window.__resolve = (needle) => new Promise((res) => {
      const y = __rowY(needle);
      const ps = __t().linkProviders || [];
      const replies = new Array(ps.length);
      const done = () => {
        const taken = new Set(); const kept = [];
        for (let i = 0; i < ps.length; i++) for (const l of (replies[i] || [])) {
          let clash = false;
          for (let x = l.range.start.x; x <= l.range.end.x; x++) if (taken.has(x)) clash = true;
          if (clash) continue;
          for (let x = l.range.start.x; x <= l.range.end.x; x++) taken.add(x);
          kept.push(l.text);
        }
        res({ y, links: kept, byProvider: replies.map((r) => (r || []).map((l) => l.text)) });
      };
      if (y < 0 || !ps.length) return done();
      ps.forEach((p, i) => p.provideLinks(y, (ls) => {
        replies[i] = ls || [];
        if (replies.filter((r) => r !== undefined).length === ps.length) done();
      }));
    });
    // Is ctrlKey set on the Control keydown itself? Recorded rather than
    // assumed, because the whole gate hangs off it.
    window.__ctrlOnDown = null;
    addEventListener("keydown", (e) => { if (e.key === "Control") window.__ctrlOnDown = e.ctrlKey; }, true);`);

  const key = (type, modifiers) => cmd("Input.dispatchKeyEvent", {
    type, key: "Control", code: "ControlLeft",
    windowsVirtualKeyCode: 17, nativeVirtualKeyCode: 17, modifiers,
  });
  // CDP's modifier bitmask: Alt 1, Ctrl 2, Meta 4, Shift 8.
  const armed = async (fn) => {
    await key("rawKeyDown", 2);
    await sleep(80);
    try { return await fn(); } finally { await key("keyUp", 0); await sleep(80); }
  };
  const resolve = (needle) => evalIn(`__resolve(${JSON.stringify(needle)})`);

  console.log("A. start a terminal and print something worth linking");
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
  // readline discards typeahead while initialising, so the first command
  // silently vanishes if it is typed before the prompt (README trap 3).
  await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "shell prompt");
  await evalIn(`__t().term.input("printf '%s\\\\n' '${PATH}' '${URL_}' '${BARE}'\\r")`);
  await until(() => evalIn(`__rowY(${JSON.stringify(BARE)}) > 0`), 20, "the seeded rows");

  // Without this, "no links" below is indistinguishable from "no providers".
  ok(await evalIn("(__t().linkProviders || []).length") === 2,
     `two link providers are registered on the terminal (got ${await evalIn("(__t().linkProviders || []).length")})`);

  console.log("\nB. armed state is off by default");
  const r1 = await resolve(PATH);
  ok(r1.y > 0, `the seeded path row is on screen (row ${r1.y}) — guards both path assertions`);
  ok(await evalIn("linksArmed") === false, "the gate starts closed");
  ok(r1.links.length === 0,
     `no link is offered with the modifier up (got ${r1.links.length}: ${JSON.stringify(r1.links)})`);

  console.log("\nC. and on while the modifier is held");
  const r2 = await armed(async () => {
    ok(await evalIn("__ctrlOnDown") === true, "ctrlKey is set on the Control keydown itself");
    ok(await evalIn("linksArmed") === true, "and the keydown listener armed the gate");
    return await resolve(PATH);
  });
  ok(r2.links.length === 1 && r2.links[0] === PATH,
     `a path is offered as a link while the modifier is held (got ${r2.links.length}: ${JSON.stringify(r2.links)})`);
  ok(await evalIn("linksArmed") === false, "and the keyup disarmed it again");

  console.log("\nD. a URL wins over the path inside it");
  const r3 = await armed(() => resolve(URL_));
  ok(r3.y > 0, `the seeded URL row is on screen (row ${r3.y})`);
  // Control: if PATH_RE never matched inside a URL there would be no conflict
  // to resolve, and assertion 3 would pass with the ordering deleted.
  ok(r3.byProvider.some((ts) => ts.some((t) => t !== URL_ && t.includes("example.com"))),
     `both matchers do claim these cells, so ordering is what resolves it (${JSON.stringify(r3.byProvider)})`);
  ok(r3.links.length === 1 && r3.links[0] === URL_,
     `${URL_} is one whole-URL link, not a path link over its tail (got ${JSON.stringify(r3.links)})`);

  console.log("\nE. a bare filename is deliberately not a path");
  const r4 = await armed(() => resolve(BARE));
  ok(r4.y > 0, `the seeded bare-filename row is on screen (row ${r4.y}) — guards the assertion below`);
  ok(r4.links.length === 0,
     `a bare filename with no directory offers no link (got ${r4.links.length}: ${JSON.stringify(r4.links)})`);

  console.log("\nF. and xterm itself marks it, with the pointer already at rest");
  // Everything above talks to the providers directly, which is the right level
  // for the matchers but leaves nudgeLinks — the whole reason arming is
  // visible without moving the mouse — completely uncovered. This section
  // arms the gate with the pointer already parked over the path and asks
  // xterm what it thinks is under the cursor.
  //
  // `_core.linkifier.currentLink` is private, and reached deliberately: it is
  // what the renderer draws the underline from, and xterm publishes no way to
  // ask "what link is hovered right now". The alternative — reading underline
  // styling out of the DOM — would bind this to one of two renderers.
  const seat = await evalIn(`(() => {
    const rows = [...document.querySelectorAll(".xterm-rows div")];
    const n = rows.filter((x) => x.textContent.trim() === ${JSON.stringify(PATH)}).pop();
    if (!n) return null; const b = n.getBoundingClientRect();
    return { x: Math.round(b.left + 30), y: Math.round(b.top + b.height / 2) }; })()`);
  ok(!!seat, `the path row is rendered and hoverable (${JSON.stringify(seat)})`);
  const hovered = `(() => { const l = __t().term._core.linkifier.currentLink; return l ? l.link.text : null; })()`;
  if (seat) {
    await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: seat.x, y: seat.y, buttons: 0 });
    await sleep(300);
    ok(await evalIn(hovered) === null, "resting on the path marks nothing while disarmed");
    // No mouse event between here and the assertion: only nudgeLinks can make
    // xterm re-ask, so this fails if the synthetic move misses its target.
    await key("rawKeyDown", 2);
    await sleep(400);
    const got = await evalIn(hovered);
    ok(got === PATH, `arming alone marked the path under the resting pointer (got ${JSON.stringify(got)})`);
    await key("keyUp", 0);
    await sleep(400);
    ok(await evalIn(hovered) === null, "and releasing unmarked it, again with no mouse movement");
  }

} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
