//! Does a GitHub-style README render its HTML in a real browser?
//!
//! The Rust tests prove the fragment's text; only a browser can prove that
//! the <img> elements the sanitizer emits actually fetch their bytes through
//! the raw route, that a <details> is collapsible, and that nothing the
//! README wrote reached the page as a live element it should not be.
//!
//! The four traps in README.md apply. In particular: naturalWidth, not
//! presence — an <img> exists whether or not its request succeeded.
//!
//! Revert-the-fix, watched fail and restored:
//!   1. Restored the two `Event::Text` arms (`Event::Html(h) =>
//!      Some(Event::Text(h))`, `Event::InlineHtml(h) => Some(Event::Text(h))`)
//!      in markdown_html's filter_map and commented out the
//!      `sanitize_raw_html` call. Every tag — <div>, <img>, <details>, the
//!      <script> — was escaped to text instead of sanitized, so five
//!      assertions failed:
//!        FAIL  six images fetched their bytes through the raw route
//!        FAIL  and no seventh image exists (the remote one was dropped)
//!        FAIL  width survived on an image
//!        FAIL  the centred div is a real div
//!        FAIL  and its URL is nowhere in the page
//!      (naturalWidth===1 count was 0, not 6 — no <img> element existed at
//!      all; and with the remote <img> escaped to text instead of dropped,
//!      "example.invalid" survived in innerHTML as visible text). The
//!      header's original text named "five images" and listed only four
//!      failures; the test itself asserts six (there are six real <img>
//!      tags in the fixture, the remote one is the seventh source that
//!      never becomes an element) and the captured run failed five — both
//!      corrected here to match what the test actually says and what
//!      actually failed.
//!   2. In HtmlSanitizer::emit, replaced the allowlist loop with pushing
//!      every attribute through verbatim (`for (k, v) in &tag.attrs { … }`).
//!      Only one assertion failed:
//!        FAIL  no element carries an attribute starting with on
//!      (the onerror-bearing <img> kept its onerror). Nothing else
//!      regressed — src/href are still decided by emit_img/emit_a, not this
//!      loop, so the six-images and no-seventh-image assertions still held.
//!   Both reverts restored afterwards; `git diff --stat src/render.rs` empty
//!   and `deno run -A tests/browser/mdhtml.mjs` passes clean again.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const PNG = Uint8Array.from(atob(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
), (c) => c.charCodeAt(0));
const proj = `${fx.roots}/${fx.project}`;
await Deno.mkdir(`${proj}/docs/img`, { recursive: true });
for (const n of ["hero", "proposal", "search", "overview", "a5"]) {
  await Deno.writeFile(`${proj}/docs/img/${n}.png`, PNG);
}
// This repository's README shape, reduced: a centred header, five images
// with widths, one with attributes on following lines, a collapsible block
// left unclosed, and the three things that must not survive.
await Deno.writeTextFile(`${proj}/README.md`, [
  '<div align="center">',
  '',
  '<img src="docs/img/hero.png" alt="hero" width="900">',
  '<img',
  '  src="docs/img/proposal.png"',
  '  width="900">',
  '',
  '# title',
  '',
  '<img src="docs/img/search.png" width="600"> <img src="docs/img/overview.png"> <img src="docs/img/a5.png">',
  '<img src="https://example.invalid/x.png" alt="remote alt">',
  '<img src="docs/img/hero.png" onerror="window.__xss = 1">',
  '<script>window.__xss = 2</script>',
  '',
  '</div>',
  '',
  '<details>',
  '<summary>More</summary>',
  '',
  'hidden paragraph',
].join("\n") + "\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "README.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body details")`), 15, "preview");

  // The onerror image points at a real file, so it is one of the six
  // <img> tags in the source; the sanitizer keeps it (minus onerror) and
  // drops only the remote one. Six elements, six with bytes.
  const loaded = await until(() => evalIn(
    `[...document.querySelectorAll(".markdown-body img")].filter((i) => i.naturalWidth === 1).length === 6`,
  ), 15, "six images");
  ok(loaded, "six images fetched their bytes through the raw route");
  ok(await evalIn(`document.querySelectorAll(".markdown-body img").length`) === 6,
    "and no seventh image exists (the remote one was dropped)");
  ok(await evalIn(`document.querySelector('.markdown-body img[width="900"]') !== null`),
    "width survived on an image");
  ok(await evalIn(`document.querySelector('.markdown-body div[align="center"]') !== null`),
    "the centred div is a real div");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("remote alt")`),
    "the remote image left its alt text");
  ok(await evalIn(`!document.body.innerHTML.includes("example.invalid")`),
    "and its URL is nowhere in the page");
  ok(await evalIn(`[...document.querySelectorAll(".markdown-body *")]
      .every((e) => [...e.attributes].every((a) => !a.name.startsWith("on")))`),
    "no element carries an attribute starting with on");
  ok(await evalIn(`document.querySelector(".markdown-body script") === null`),
    "no script element exists");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("<script>")`),
    "the script tag printed as text instead");
  const ran = await until(() => evalIn(`typeof window.__xss !== "undefined"`), 2);
  ok(!ran, "nothing the README wrote executed");

  const d = `document.querySelector(".markdown-body details")`;
  ok(await evalIn(`!${d}.open`), "the details block starts collapsed");
  await evalIn(`${d}.querySelector("summary").click()`);
  ok(await until(() => evalIn(`${d}.open`), 5, "details open"), "and opens on click");
  ok(await evalIn(`${d}.textContent.includes("hidden paragraph")`),
    "the unclosed details still contains its paragraph, closed by the sanitizer");
} finally {
  try { await page?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
