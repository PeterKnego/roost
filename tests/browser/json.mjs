//! A JSON file still opens in Edit, and its 👁 toggle shows it indented and
//! foldable without changing the file.
//!
//! `cargo test` covers the parser (scalars kept exactly as written, errors
//! with line and column, the depth bound) and the fragment (escaping,
//! folding a big document). What only a browser can show:
//! - the toggle is offered from Edit and comes back;
//! - nodes fold and the bar folds them all;
//! - a minified file is shown indented and is byte-for-byte unchanged on
//!   disk afterwards;
//! - at phone width a long string wraps instead of pushing the page
//!   sideways.
//!
//! Revert-checks performed, all restored:
//!   (a) `json` dropped from VIEW_TOGGLE_EXT -> A fails: Edit offers no 👁.
//!   (b) the expand/collapse click handler removed -> B fails on
//!       "collapse all".
//!   (c) `overflow-wrap: anywhere` dropped from `.jtree, .jl, …` -> C fails:
//!       the page scrolls sideways.
//!
//! Run: deno run -A tests/browser/json.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// Minified, with a number JS would round and a string longer than a phone.
const SRC = `{"name":"demo","big":12345678901234567890123,"tags":["a","b"],"nested":{"deep":{"x":true,"y":null}},` +
  `"long":"${"x".repeat(300)}","empty":{}}`;
await Deno.writeTextFile(`${fx.dir}/data.json`, SRC);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");

  console.log("A. JSON opens in Edit, and 👁 shows the view");
  ok(await evalIn(`defaultMode("data.json") === "Edit"`), "a click still edits a .json");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "data.json", mode: "Edit" } }); true`);
  ok(await until(() => evalIn(`!!document.querySelector(".editwrap .path .modebtn")`), 20, "the toggle"), "Edit offers the 👁 toggle");
  await evalIn(`document.querySelector(".editwrap .path .modebtn").click(); true`);
  ok(await until(() => evalIn(`!!document.querySelector(".jsonview .jtree")`), 20, "the tree"), "the view shows the tree");
  const keys = JSON.parse(await evalIn(`JSON.stringify([...document.querySelectorAll(".jtree .jk")].map((k) => k.textContent))`));
  ok(JSON.stringify(keys) === JSON.stringify(['"name"', '"big"', '"tags"', '"nested"', '"deep"', '"x"', '"y"', '"long"', '"empty"']),
     `every key, in file order (${keys.join(" ")})`);
  ok(await evalIn(`[...document.querySelectorAll(".jtree .jnum")].some((n) => n.textContent === "12345678901234567890123")`),
     "a number past double precision is shown as written");
  ok(await evalIn(`document.querySelector(".jtree").innerText.split("\\n").length > 10`), "the minified file is shown on many lines");

  console.log("\nB. folding");
  const kidsShown = () => evalIn(`(() => { const d = [...document.querySelectorAll(".jtree details.jn")]
    .find((x) => x.querySelector(":scope > summary .jk")?.textContent === '"nested"');
    return d.open && d.querySelector(".jkids").offsetHeight > 0; })()`);
  ok(await kidsShown(), "a small document opens whole");
  await evalIn(`[...document.querySelectorAll(".jtree details.jn > summary")]
    .find((s) => s.querySelector(".jk")?.textContent === '"nested"').click(); true`);
  ok(await until(async () => !(await kidsShown()), 5, "folded"), "a tap on a node folds it");
  ok(await evalIn(`[...document.querySelectorAll(".jtree details.jn:not([open]) > summary .jsum")].some((s) => s.offsetHeight > 0 && /1 key/.test(s.textContent))`),
     "and its summary says what is inside");
  await evalIn(`document.querySelector('.jsonbar button[data-j="close"]').click(); true`);
  ok(await until(() => evalIn(`[...document.querySelectorAll(".jtree details.jn")].every((d) => !d.open)`), 5, "all closed"),
     "collapse all folds every node");
  await evalIn(`document.querySelector('.jsonbar button[data-j="open"]').click(); true`);
  ok(await until(() => evalIn(`[...document.querySelectorAll(".jtree details.jn")].every((d) => d.open)`), 5, "all open"),
     "expand all opens them again");

  console.log("\nC. on a phone");
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 800, deviceScaleFactor: 2, mobile: true });
  await evalIn(`revealPane(Number(document.querySelector(".jsonview").closest(".pane").dataset.pane)); true`);
  await sleep(400);
  const phone = JSON.parse(await evalIn(`JSON.stringify({
    page: document.scrollingElement.scrollWidth,
    pane: (() => { const c = document.querySelector(".jsonview").closest(".content"); return c.scrollWidth - c.clientWidth; })(),
    btn: document.querySelector('.jsonbar button[data-j="open"]').getBoundingClientRect().height,
  })`));
  ok(phone.page <= 390 && phone.pane <= 1, `a 300-character string wraps; nothing scrolls sideways (page ${phone.page}, pane +${phone.pane})`);
  ok(phone.btn >= 44, `the bar buttons are 44 px (${phone.btn})`);

  console.log("\nD. back to Edit, and the file is untouched");
  await evalIn(`document.querySelector(".jsonview").closest(".content").querySelector(".path .modebtn").click(); true`);
  ok(await until(() => evalIn(`!!document.querySelector(".editwrap")`), 20, "the editor"), "✎ goes back to the editor");
  ok(await Deno.readTextFile(`${fx.dir}/data.json`) === SRC, "the file on disk is byte-for-byte what it was");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
