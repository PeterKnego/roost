//! A PDF opens in the browser's own viewer. #121.
//!
//! The route's headers, the streaming past the 2 MB cap, the refusals and the
//! fragment's escaping are covered in `src/routes.rs`. What only a browser can
//! say is whether the **viewer actually drew it** — an `<iframe>` existing
//! proves nothing, since one pointed at a 404 or a "not an image" page is
//! still an `<iframe>`.
//!
//! The evidence is Chromium's PDF viewer itself: it runs as the built-in
//! extension `mhjfbmdgcfjbbpaeojofohoefgiehjai`, which appears as a debug
//! target only while it is showing a document. Under `sandbox` too — the probe
//! recorded in the spec found both Chromium's and Firefox's viewers render a
//! sandboxed PDF, which is why the route needed no CSP exception.
//!
//! Revert-checks performed, all restored:
//!   (a) dropping the `is_pdf` branch from `serve_raw` -> section A fails: the
//!       frame gets "not an image", and no viewer target appears.
//!   (b) dropping `"pdf"` from `RENDERED_EXT` in app.js -> section A fails on
//!       `defaultMode`. The server would still coerce the Edit request to
//!       Preview, so the tab itself recovers; the client asking for the wrong
//!       mode is what this pins.
//!   (c) dropping the `max-width: 640px` rule -> section C fails: the frame is
//!       still laid out on a phone.
//!
//! Run: deno run -A tests/browser/pdf.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

/// A valid one-page PDF padded past `MAX_FILE_BYTES` by an unreferenced
/// stream, with a correct xref. Past the cap is the point: a PDF under 2 MB
/// would pass against a route that still applied the image path's limit.
function makePdf(padBytes) {
  const enc = new TextEncoder();
  const parts = [];
  const offsets = [];
  let len = 0;
  const push = (b) => { const u = typeof b === "string" ? enc.encode(b) : b; parts.push(u); len += u.length; };
  const obj = (n, body) => { offsets[n] = len; push(`${n} 0 obj\n`); push(body); push("\nendobj\n"); };
  push("%PDF-1.4\n");
  const text = "BT /F1 48 Tf 72 700 Td (roost pdf) Tj ET";
  obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
  obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
  obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
  obj(4, `<< /Length ${text.length} >>\nstream\n${text}\nendstream`);
  obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
  offsets[6] = len;
  push(`6 0 obj\n<< /Length ${padBytes} >>\nstream\n`);
  push(new Uint8Array(padBytes).fill(0x20));
  push("\nendstream\nendobj\n");
  const xref = len;
  push(`xref\n0 7\n0000000000 65535 f \n`);
  for (let n = 1; n <= 6; n++) push(`${String(offsets[n]).padStart(10, "0")} 00000 n \n`);
  push(`trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`);
  const out = new Uint8Array(len);
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
}

const fx = await fixture();
const pdf = makePdf(2_300_000);
await Deno.writeFile(`${fx.dir}/paper.pdf`, pdf);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const VIEWER = "chrome-extension://mhjfbmdgcfjbbpaeojofohoefgiehjai/";
const viewers = async () =>
  (await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json())
    .filter((t) => (t.url || "").startsWith(VIEWER)).length;
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector('a[data-rel="paper.pdf"]')`), 20, "tree");
  ok(pdf.length > 2_000_000, `the fixture is past the 2 MB cap (${pdf.length} bytes)`);

  console.log("A. clicking the PDF in the tree opens it in the viewer");
  ok(await evalIn(`defaultMode("paper.pdf") === "Preview"`), "a PDF opens in Preview");
  const before = await viewers();
  await evalIn(`document.querySelector('a[data-rel="paper.pdf"]').click(); true`);
  ok(await until(() => evalIn(`!!document.querySelector(".content > iframe.pdfview")`), 20, "the frame"),
     "the tab holds the viewer frame");
  ok(await until(async () => (await viewers()) > before, 20, "the PDF viewer"),
     "and Chromium's PDF viewer is drawing it");
  const box = JSON.parse(await evalIn(`JSON.stringify((() => {
    const r = document.querySelector(".content > iframe.pdfview").getBoundingClientRect();
    return { w: Math.round(r.width), h: Math.round(r.height) };
  })())`));
  ok(box.w > 200 && box.h > 300, `the frame fills the pane rather than a 300x150 default (${box.w}x${box.h})`);
  ok(await evalIn(`!document.querySelector('.path .modebtn')`), "no ✎: a PDF has no text to edit");

  console.log("\nB. the link opens it on its own, in the viewer too");
  const href = await evalIn(`document.querySelector(".content > a.pdfopen").getAttribute("href")`);
  ok(/^\/frag\/[^?]+\/raw\?path=paper\.pdf&v=\d+$/.test(href), `the link is the raw route (${href})`);
  const n = await viewers();
  const t = await (await fetch(`http://127.0.0.1:${browser.port}/json/new?http://127.0.0.1:${roost.port}${href}`,
    { method: "PUT" })).json();
  ok(await until(async () => (await viewers()) > n, 20, "a second viewer"), "a top-level PDF renders in the viewer");
  await fetch(`http://127.0.0.1:${browser.port}/json/close/${t.id}`);

  console.log("\nC. on a phone the frame gives way to the link");
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 800, deviceScaleFactor: 2, mobile: true });
  // One pane at a time on a phone; show the one holding the PDF, as tapping
  // its tab would.
  await evalIn(`revealPane(Number(document.querySelector(".content > iframe.pdfview").closest(".pane").dataset.pane)); true`);
  await sleep(300);
  const phone = JSON.parse(await evalIn(`JSON.stringify({
    frame: getComputedStyle(document.querySelector(".content > iframe.pdfview")).display,
    link: document.querySelector(".content > a.pdfopen").getBoundingClientRect().height,
  })`));
  ok(phone.frame === "none", `the frame is not laid out (${phone.frame})`);
  ok(phone.link >= 44, `the link is a 44px tap target (${phone.link})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
