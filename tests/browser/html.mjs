//! An HTML report clicked in the tree opens rendered, in a sandboxed frame,
//! with its own CSS and scripts, and cannot reach roost.
//!
//! `cargo test` covers the route: the path-shaped URL, the sandbox CSP on
//! every response, confinement, the double-encoded project key. What only a
//! browser can show:
//! - the page renders, and its relative stylesheet and script load;
//! - its script runs but is in an opaque origin, so it cannot read a roost
//!   response, open the workspace socket, or touch the page around it;
//! - ✎ still reaches the source;
//! - at phone width the frame fits and the link is 44 px.
//!
//! The report tells the test what its script could do by `postMessage`, the
//! one channel an opaque origin keeps.
//!
//! Revert-checks performed, all restored:
//!   (a) `allow-same-origin` added to `VIEW_SANDBOX` and to the frame's
//!       `sandbox` attribute -> B fails on all three: the report reads
//!       /frag/…/tree, opens the workspace socket, and reaches
//!       parent.document.
//!   (b) `html` dropped from the client's RENDERED_EXT -> A fails: a click
//!       opens the source in Edit.
//!   (c) dropping the `.content > .htmlview` rule -> A fails on the fill
//!       check (the frame stays 150 px tall).
//!
//! Run: deno run -A tests/browser/html.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.dir}/reports/cov/assets`, { recursive: true });
await Deno.writeTextFile(`${fx.dir}/reports/cov/index.html`, `<!doctype html>
<html><head><meta charset="utf-8"><link rel="stylesheet" href="assets/report.css"></head>
<body><h1 id="h">Coverage 87%</h1><div id="out">waiting</div>
<script src="assets/report.js"></script></body></html>`);
await Deno.writeTextFile(`${fx.dir}/reports/cov/assets/report.css`, "h1 { color: rgb(1, 2, 3); }");
await Deno.writeTextFile(`${fx.dir}/reports/cov/assets/report.js`, `
(async () => {
  const r = { css: getComputedStyle(document.getElementById("h")).color, origin: String(self.origin) };
  try { const t = await (await fetch("/frag/${fx.project}/tree")).text(); r.read = t.length > 0; }
  catch { r.read = false; }
  r.ws = await new Promise((done) => {
    try {
      const s = new WebSocket(location.origin.replace(/^http/, "ws") === "null"
        ? "ws://" + location.host + "/ws/${fx.project}/_workspace"
        : location.origin.replace(/^http/, "ws") + "/ws/${fx.project}/_workspace");
      s.onopen = () => done(true);
      s.onerror = s.onclose = () => done(false);
    } catch { done(false); }
    setTimeout(() => done(false), 4000);
  });
  try { r.parent = !!parent.document.body; } catch { r.parent = false; }
  document.getElementById("out").textContent = "ran";
  parent.postMessage({ report: r }, "*");
})();`);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await evalIn(`window.__report = null; addEventListener("message", (e) => { if (e.data && e.data.report) window.__report = e.data.report; }); true`);

  console.log("A. a report opens rendered, filling the pane");
  ok(await evalIn(`defaultMode("reports/cov/index.html") === "Preview"`), "an .html opens in Preview");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "reports/cov/index.html",
    mode: defaultMode("reports/cov/index.html") } }); true`);
  ok(await until(() => evalIn(`!!document.querySelector(".content > iframe.htmlview")`), 20, "the frame"), "it is a frame");
  ok(await until(() => evalIn(`!!window.__report`), 20, "the report's script"), "the report's own script ran");
  const rep = JSON.parse(await evalIn(`JSON.stringify(window.__report)`));
  ok(rep.css === "rgb(1, 2, 3)", `its relative stylesheet loaded (${rep.css})`);
  const fill = JSON.parse(await evalIn(`JSON.stringify((() => {
    const f = document.querySelector(".content > iframe.htmlview");
    return { f: f.getBoundingClientRect().height, c: f.closest(".content").clientHeight };
  })())`));
  ok(fill.f > fill.c - 120, `the frame fills the pane under the path (${Math.round(fill.f)} of ${fill.c}px)`);
  ok(await evalIn(`!!document.querySelector(".content .path .modebtn")`), "✎ is there for the source");

  console.log("\nB. its script cannot reach roost");
  ok(rep.origin === "null", `it runs in an opaque origin (${rep.origin})`);
  ok(rep.read === false, "it cannot read a roost response");
  ok(rep.ws === false, "it cannot open the workspace socket");
  ok(rep.parent === false, "it cannot touch the page around it");

  console.log("\nC. ✎ shows the source, and back");
  await evalIn(`document.querySelector(".content .path .modebtn").click(); true`);
  ok(await until(() => evalIn(`[...document.querySelectorAll("textarea, code-input")].some((t) => (t.value || "").includes("Coverage 87%"))`), 20, "the source"),
     "Edit shows the HTML source");

  console.log("\nD. on a phone");
  await evalIn(`window.__report = null; send({ t: "SetMode", rel: "reports/cov/index.html", mode: "Preview" }); true`);
  await until(() => evalIn(`!!document.querySelector(".content > iframe.htmlview")`), 20, "the frame again");
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 800, deviceScaleFactor: 2, mobile: true });
  await evalIn(`revealPane(Number(document.querySelector(".content > iframe.htmlview").closest(".pane").dataset.pane)); true`);
  await sleep(400);
  const phone = JSON.parse(await evalIn(`JSON.stringify((() => {
    const f = document.querySelector(".content > iframe.htmlview").getBoundingClientRect();
    return { w: f.width, right: f.right, h: f.height, page: document.scrollingElement.scrollWidth,
      link: document.querySelector(".content > a.htmlopen").getBoundingClientRect().height };
  })())`));
  ok(phone.w > 300 && phone.right <= 391 && phone.page <= 390, `the frame fits the 390px screen (${phone.w}px, page ${phone.page})`);
  ok(phone.h > 300, `and is tall enough to read (${phone.h}px)`);
  ok(phone.link >= 44, `the link is a 44px tap target (${phone.link})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
