//! A previewed file follows the file on disk.
//!
//! FileChanged used to be handled by refreshKind("Diff") alone — diffs only,
//! and by kind rather than by file. A Preview tab was a one-shot fetch that
//! nothing ever invalidated: it kept showing whatever the file said at the
//! moment it was opened, even after the file changed underneath it.
//!
//! Run: deno run -A tests/browser/preview-follows.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`);
  const { cmd, evalIn } = page;
  // The default headless window is 800x600, which is narrower than the left
  // (260px) and right (520px) panes together: the middle pane — the one every
  // assertion below looks at — collapses to nothing. See the README's traps.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  console.log("A. a previewed file follows the disk");
  await Deno.writeTextFile(`${fx.roots}/proj/watched.md`, "# before\n");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "watched.md", mode: "Preview" } })`);
  const shown = () => evalIn(`(document.querySelector('.pane[data-pane="2"] .content') || {}).textContent || ""`);
  ok(await until(async () => (await shown()).includes("before"), 10, "the preview"),
     "the file is on screen");

  // Written by something other than roost, which is the whole case: an edit
  // roost itself made is suppressed as a self-write.
  await Deno.writeTextFile(`${fx.roots}/proj/watched.md`, "# after\n");
  ok(await until(async () => (await shown()).includes("after"), 15, "the update"),
     "it updates without a reload");
  ok(!(await shown()).includes("before"), "and the old content is gone");
} finally {
  try { page?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
