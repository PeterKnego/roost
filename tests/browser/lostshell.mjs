//! A terminal tab whose shell did not survive a reboot.
//!
//! roost outlives its own restart: dtach masters daemonise away from it and
//! reparent to init, so a restored Terminal tab reattaches and the scrollback
//! is still there. A reboot outlives no process. The tab comes back — it is in
//! the saved layout — pointing at a socket that no longer exists.
//!
//! Both cases rendered the *same* thing when the shell was not running:
//! "Press Enter to start a terminal", which is also what a tab nobody has ever
//! used shows. So losing a long-running Claude to a reboot was
//! indistinguishable, on screen, from never having started one. Same shape as
//! the table in CLAUDE.md wearing a quieter coat: no crash, no error, and no
//! way for the user to tell that something they had is gone.
//!
//! The post-reboot state is seeded on disk rather than acted out, because that
//! is the only honest way to reach it. The harness cannot reboot the machine;
//! killing the dtach master by hand leaves the socket file behind, which is a
//! *different* state (`vanished`, not lost); and planting a socket file
//! without a holder does not work either — roost's startup sweep removes it,
//! correctly, as an orphan. Writing the layout with a session that has no
//! socket at all is exactly what a reboot leaves.
//!
//! Section C is the negative control, and it caught a real bug: the first
//! draft named every terminal tab with no socket, and `default_layout` ships
//! an empty `term` tab — so a brand-new project greeted its user with the news
//! that a shell they had never started had failed to survive. Warning on every
//! placeholder and warning only on a lost one are indistinguishable until some
//! test renders a placeholder that is *not* a loss, which is exactly what a
//! second, untouched project does.
//!
//! Run: deno run -A tests/browser/lostshell.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();

// The layout a reboot leaves behind: a terminal tab recorded in the right
// pane, and — because the machine went down — no socket directory at all.
await Deno.writeTextFile(`${fx.stateDir}/proj.json`, JSON.stringify({
  sizes: { left_w: 260, right_w: 520, left_split: 60 },
  panes: [
    { tabs: [{ k: "Tree" }], active: 0 },
    { tabs: [], active: 0 },
    { tabs: [], active: 0 },
    { tabs: [{ k: "Terminal", session: "ghost" }], active: 0 },
  ],
  buffers: {},
  // What makes this a loss rather than a tab nobody opened: roost watched a
  // shell run in `ghost` and wrote that down at its last save. A state file
  // without this records nothing about shells, and supports no claim that one
  // is gone — which is what section C leans on.
  sessions_seen: ["ghost"],
}));

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const load = async (project) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  // The middle pane collapses to a few pixels at the default 800x600, which
  // has already made a test in this suite assert against an element nothing
  // could have rendered into. 1400x900 is what a person actually has.
  await p.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  return p;
};

const pane = `document.querySelector('.pane[data-pane="3"]')`;
const lostText = `${pane}?.querySelector('.termstart .termlost')?.textContent ?? ""`;
const startText = `${pane}?.querySelector('.termstart')?.textContent ?? ""`;

try {
  page = await load(fx.project);
  const { evalIn } = page;

  console.log("A. the tab comes back, the shell did not");
  ok(await until(() => evalIn(`!!(${pane}?.querySelector('.termstart'))`), 15, "the placeholder"),
     "the restored tab renders a placeholder, not a terminal");
  // The server half, asserted apart from the rendering: the two can fail
  // separately, and "the warning is missing" is a very different bug depending
  // on which side dropped it.
  ok(await evalIn(`(state.lost_sessions || []).includes("ghost")`),
     "the server names the session whose shell is gone");
  // The assertion the whole change exists for. On the *distinguishing*
  // element, not the placeholder's whole text: asserting the whole string
  // would pass just as well if every placeholder said this, which is what
  // section C rules out.
  ok((await evalIn(lostText)).includes("did not survive"),
     "and the tab says the shell is gone rather than inviting a fresh start");
  ok((await evalIn(startText)).includes("to start a terminal"),
     "while still offering the way back — it is a tab you can reuse, not a corpse");

  console.log("B. starting a shell in it clears the warning");
  // `lost_sessions` is computed once, when the hub loads the layout; nothing
  // recomputes it from scratch. Without the prune in `refresh_live_sessions`
  // the name would stay in the list for the life of the process, and a tab you
  // had just started a shell in would go on saying its shell was gone.
  await evalIn(`send({ t: "StartTerminal", session: "ghost" })`);
  ok(await until(() => evalIn(`!(state.lost_sessions || []).includes("ghost")`), 30, "the prune"),
     "once its shell is running the session is no longer named as lost");
  ok(await until(() => evalIn(`!(${pane}?.querySelector('.termlost'))`), 15, "the warning to clear"),
     "and the warning is off the screen");

  console.log("C. a project that has never had a shell");
  // The negative control, and the reason A means anything. `default_layout`
  // puts an empty `term` tab in the right pane of every project nobody has
  // configured, and it has no socket — the same input as `ghost`. What
  // separates them is only that roost never saw a shell in this one. If the
  // client printed the warning on every placeholder, or the server named every
  // socketless tab, this is where it shows, and it did.
  const fresh = `${fx.roots}/fresh`;
  await Deno.mkdir(fresh, { recursive: true });
  await new Deno.Command("git", { args: ["init", "-q"], cwd: fresh, stdout: "null", stderr: "null" }).output();
  try { await page.close(); } catch { /* already gone */ }
  page = await load("fresh");
  ok(await until(() => page.evalIn(`!!(${pane}?.querySelector('.termstart'))`), 15, "the placeholder"),
     "its default terminal tab renders a placeholder too");
  ok((await page.evalIn(startText)).includes("to start a terminal"),
     "with the same invitation — the two are the same placeholder");
  ok((await page.evalIn(`JSON.stringify(state.lost_sessions || [])`)) === "[]",
     "but the server names nothing as lost");
  ok((await page.evalIn(lostText)) === "",
     "and the tab does not claim a shell it never had was destroyed");

  console.log("D. roost restarts; the shell survives, so nothing is lost");
  // The other half of the module comment's claim, and the one that says the
  // warning is about a *reboot* rather than about roost stopping. The dtach
  // master daemonises away from roost, so killing the server leaves the shell
  // running and the socket in place.
  try { await page.close(); } catch { /* already gone */ }
  ok(await roost.restart(), "roost comes back up");
  // Long enough that a restart which did reap the shell would have done so by
  // now, so this is not asserting against a window in which nothing could have
  // happened either way.
  await sleep(1000);
  page = await load(fx.project);
  ok(await page.evalIn(`state.live_sessions.includes("ghost")`),
     "the shell is still there — roost outlives its own restart");
  ok((await page.evalIn(`JSON.stringify(state.lost_sessions || [])`)) === "[]",
     "so nothing is named as lost");
  ok((await page.evalIn(`document.body.textContent`)).includes("did not survive") === false,
     "and nowhere on the page does anything claim otherwise");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
