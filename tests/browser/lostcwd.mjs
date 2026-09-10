//! A terminal comes back from a reboot in the directory it was working in.
//!
//! Step 2 of #17. A reboot kills every dtach master, so the tab comes back
//! from the saved layout and `dtach -A` gives it a brand-new shell in the
//! project root. Whatever tree of directories you had arranged across four
//! terminals is gone, and re-typing it is the first thing anyone does.
//!
//! This is the file that has to exist, because every substitution in
//! CLAUDE.md's dev/prod table is in play at once here. The Rust tests drive
//! `cwds::sample` against a *fake* `/proc` built out of temp files: it covers
//! the parse, and it cannot cover whether a real dtach session's shell is
//! actually reachable that way — whether it really is the child of a process
//! whose `comm` is `dtach`, whether the environment really survives the
//! daemonising fork, whether `/proc/<pid>/cwd` really tracks a `cd` typed at a
//! prompt. `ROOST_CMD=cat` hid a socket directory that was never created and
//! terminals that would have died at spawn in production; this walks the same
//! ground.
//!
//! So: a real roost, real dtach, a real shell, a real `cd`, and the real
//! sampler thread reading the real `/proc`. The reboot is simulated the only
//! way it can be — kill the dtach master and take the socket with it, which
//! is what a machine going down leaves behind.
//!
//! Run: deno run -A tests/browser/lostcwd.mjs
import { fixture, freePort, killByCmdline, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.dir}/deep/nested`, { recursive: true });

// The sampler's real timer, wound down. Not faked: the thread, the walk and
// the marker write are all the production ones — only the sleep between passes
// is shorter, because a test cannot wait fifteen seconds per `cd` and a test
// that slept through the sampler instead would be asserting nothing.
Deno.env.set("ROOST_CWD_POLL_SECS", "1");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const load = async () => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await p.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  return p;
};

// The terminal's visible text. `term.input`, never `term.paste`: bash turns on
// bracketed paste, under which a pasted line is not run.
const wire = async (p) => {
  await p.evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n";
      return s; };`);
};
const sh = async (p, line) => p.evalIn(`__t().term.input(${JSON.stringify(line + "\r")})`);

try {
  page = await load();

  console.log("A. a real shell, in a real directory");
  const start = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return { pi, ti, session: state.panes[pi].tabs[ti].session }; } return null; })()`;
  const loc = await page.evalIn(start);
  ok(!!loc, "the default layout has a terminal tab");
  await page.evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => page.evalIn("terms.size > 0"), 30, "a terminal"), "its shell starts");
  await wire(page);
  await sleep(1500);
  await sh(page, "cd deep/nested");
  await sh(page, "pwd");
  ok(await until(async () => (await page.evalIn("__txt()")).includes("/deep/nested"), 20, "the cd"),
     "and it can be driven into a subdirectory");

  console.log("B. the sampler finds it in the real /proc");
  // The assertion the fake `/proc` cannot make. If the shell is not where
  // `parent_is_dtach` expects it, or the environment did not survive dtach's
  // daemonising fork, no marker is ever written and every Rust test still
  // passes.
  const marker = `${fx.stateDir}/cwd/proj/${loc.session}`;
  ok(await until(async () => {
    try { return (await Deno.readTextFile(marker)).endsWith("/deep/nested"); } catch { return false; }
  }, 30, "the marker"), "roost records where the shell actually is");

  console.log("C. the machine goes down");
  // A reboot, as closely as this can be staged: the dtach master dies and its
  // socket goes with it. Deliberately NOT the tab's × — that calls
  // `end_session`, which forgets the marker on purpose, and would test the
  // opposite of this.
  try { await page.close(); } catch { /* already gone */ }
  await roost.close();
  ok(await killByCmdline(fx.stateDir) > 0, "the dtach master is killed, as a reboot would");
  await Deno.remove(`${fx.stateDir}/sock/proj/${loc.session}`).catch(() => {});
  ok(await Deno.stat(marker).then(() => true).catch(() => false),
     "the marker outlives it — it is the only thing that does");

  console.log("D. it comes back where it was");
  const back = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: roost.port });
  page = await openPage(browser.port, `http://127.0.0.1:${back.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => page.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  await page.evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => page.evalIn("terms.size > 0"), 30, "a fresh terminal"), "a fresh shell starts");
  await wire(page);
  await sleep(1500);
  await sh(page, "pwd");
  ok(await until(async () => (await page.evalIn("__txt()")).includes("/deep/nested"), 20, "the restored cwd"),
     "and it starts in the directory the old one was working in");
  await back.close();
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
