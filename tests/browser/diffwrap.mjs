//! A diff scrolls sideways instead of wrapping.
//!
//! Wrapped, a long changed line broke at every hyphen and went on at column 0,
//! where it read as a line of its own — one with no +/- to say what it was.
//! The view now scrolls instead, and the property that is easy to lose doing
//! that is the row colour: rows are blocks, so by default each is only as wide
//! as the view, and after scrolling right a short added line's green stops
//! dead in the middle of the screen. Nothing in Rust can see either.
//!
//! Run: deno run -A tests/browser/diffwrap.mjs

import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// Hyphenated words, so a wrap would have somewhere to break, plus a short
// line, whose row colour is the one that would fall short after scrolling.
await Deno.writeTextFile(`${fx.roots}/proj/wide.css`,
  `.a { ${"border-radius:6px; flex-direction:column; ".repeat(12)}}\n.b { gap:1px; }\n`);
await Deno.writeTextFile(`${fx.roots}/proj/short.css`, `.c { gap:1px; }\n`);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  console.log("A. a diff with lines wider than the pane");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "Diff", rel: "wide.css" } })`);
  ok(await until(() => evalIn(`document.querySelectorAll('.content[data-kind="Diff"] .diffview .dl.add').length >= 2`), 15, "diff rows"),
     "the diff tab shows the added lines");
  const g = JSON.parse(await evalIn(`JSON.stringify((() => {
    const v = document.querySelector('.content[data-kind="Diff"] .diffview');
    const rows = [...v.querySelectorAll(".dl")];
    const adds = [...v.querySelectorAll(".dl.add")];
    const widths = rows.map((r) => Math.round(r.getBoundingClientRect().width));
    const pane = v.closest(".pane");
    return { ws: getComputedStyle(adds[0]).whiteSpace,
             addH: adds.map((r) => Math.round(r.getBoundingClientRect().height)),
             lineH: parseFloat(getComputedStyle(adds[0]).lineHeight),
             scrollW: v.scrollWidth, clientW: v.clientWidth,
             minW: Math.min(...widths), maxW: Math.max(...widths),
             viewRight: v.getBoundingClientRect().right, paneRight: pane.getBoundingClientRect().right };
  })())`));
  ok(g.ws === "pre", `a diff line does not wrap (white-space: ${g.ws})`);
  // The observable consequence, not just the property: every added line is
  // one row tall. The grid alone already prevents wrapping (its column is as
  // wide as the longest line), so this fails only with both it and `pre`
  // gone — watched: the long line took 150px. The line above is what fails
  // on `pre` alone.
  ok(g.addH.every((h) => Math.abs(h - g.lineH) < 1), `each added line is one row tall (${g.addH} at line-height ${g.lineH})`);
  ok(g.scrollW > g.clientW, `the view scrolls sideways (${g.scrollW}px in ${g.clientW}px)`);
  ok(g.viewRight <= g.paneRight + 1,
     `and stays inside its pane (view right ${Math.round(g.viewRight)} vs pane right ${Math.round(g.paneRight)})`);
  // Fails without the grid: rows are then as wide as the view, not the line.
  ok(g.minW === g.maxW && g.minW >= g.scrollW - 1,
     `every row spans the full scroll width, so colour reaches the end (rows ${g.minW}–${g.maxW}px, content ${g.scrollW}px)`);

  console.log("\nB. a diff whose lines are all short");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "Diff", rel: "short.css" } })`);
  ok(await until(() => evalIn(`(document.querySelector('.content[data-kind="Diff"] .path') || {}).textContent === "short.css"`), 15, "short diff"),
     "the short diff opens");
  const s = JSON.parse(await evalIn(`JSON.stringify((() => {
    const v = document.querySelector('.content[data-kind="Diff"] .diffview');
    const widths = [...v.querySelectorAll(".dl")].map((r) => Math.round(r.getBoundingClientRect().width));
    return { minW: Math.min(...widths), maxW: Math.max(...widths), clientW: v.clientWidth, scrollW: v.scrollWidth };
  })())`));
  // The other half of the column rule: sized to the longest line alone, a
  // short diff's rows would end where their text does, mid-pane.
  ok(s.minW === s.clientW && s.maxW === s.clientW, `rows still fill the view (rows ${s.minW}–${s.maxW}px, view ${s.clientW}px)`);
  ok(s.scrollW === s.clientW, `and nothing scrolls (${s.scrollW}px in ${s.clientW}px)`);
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
