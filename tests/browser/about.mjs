//! The About pane: what binary is this, actually.
//!
//! #56. roost is deployed by building it and copying a binary about, and
//! nothing in the UI could answer "is this the thing I built?" — a question
//! this project has already got wrong twice (CLAUDE.md, "Verify, don't
//! trust": the running binary that did not change, and the build from a
//! second checkout that cargo reported as `Fresh`), and once more on
//! 2026-09-10, when a phone was reported as still broken after a CSS fix it
//! had never fetched.
//!
//! Driven in a browser because the values cross three layers to get here —
//! `build.rs` bakes them in, `config::build_info` reads them, the settings
//! snapshot carries them, and `dialog.js` renders them. A Rust test covers the
//! first two; only this can see the last.
//!
//! Run: deno run -A tests/browser/about.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");

  console.log("A. the tab exists and is reachable");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "the dialog"),
     "the settings dialog opens");
  ok(await evalIn(`!!document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]')`),
     "and carries an About tab beside General and Theme");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  ok(await evalIn(`!document.querySelector("#dlg-settings .dlg-about").hidden
                   && document.querySelector("#dlg-settings .dlg-rows").hidden`),
     "selecting it shows the About pane and hides the settings rows");
  // Nothing here is editable, so the control that chooses *which file* a
  // change is written to has nothing to say.
  ok(await evalIn(`document.querySelector("#dlg-settings .dlg-scope").hidden`),
     "and the scope switch is gone, since nothing on this pane is written anywhere");

  console.log("B. it reports this binary, not a placeholder");
  const rows = await evalIn(`(() => {
    const out = {};
    for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
      out[r.querySelector("label").textContent] = r.querySelector(".ro").textContent.trim();
    }
    return out; })()`);
  ok(!!rows.Version && !!rows.Commit && !!rows.Built && !!rows.Repository,
     `all four rows are present: ${JSON.stringify(Object.keys(rows))}`);
  // The assertion that makes this pane worth having: the commit shown is the
  // commit the checkout is on. Compared against `state.settings.build`, which
  // the Rust test has already tied to `build.rs` — so a rendering that
  // invented a value, or read the wrong field, fails here.
  const server = await evalIn(`state.settings.build`);
  ok(rows.Commit === server.commit && server.commit !== "unknown",
     `the commit is the server's own (${rows.Commit})`);
  ok(rows.Version === server.version, `and so is the version (${rows.Version})`);
  // A tree with edits in it must say so. "Built from a1b2c3d" is false in the
  // common case of a local build, and this pane exists to be trusted.
  ok(/^[0-9a-f]{7,}(-dirty)?\??$/.test(server.commit),
     `the commit is a hash, optionally marked dirty: ${server.commit}`);
  ok(rows.Built !== "unknown" && !/1970/.test(rows.Built),
     `the build time is a real time, not the epoch: ${rows.Built}`);

  console.log("C. the repository is a link");
  const link = await evalIn(`(() => {
    const a = document.querySelector("#dlg-settings .dlg-about a");
    return a ? { href: a.href, target: a.target, rel: a.rel } : null; })()`);
  ok(!!link && link.href.startsWith("https://"), `the repository is a link (${link && link.href})`);
  ok(link && link.target === "_blank" && link.rel.includes("noopener"),
     "opened in a new tab, with noopener — the workspace must not be navigated away from");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
