//! A host without `dtach` says so on the terminal (#123).
//!
//! The bug this pins: with `dtach` missing, opening a terminal showed
//! "session ended" and nothing else — the same words a deliberate `exit`
//! leaves — while the cause went only to roost's stderr, which the person at
//! the browser is usually on another machine from.
//!
//! `ROOST_CMD` stays unset, as the README requires of every test here: the
//! failure under test is the real default command meeting a real `PATH`.
//! roost's `PATH` is a directory of symlinks to everything on the developer's
//! `PATH` *except* `dtach`, so git, ps and the rest still resolve and the only
//! thing missing is the one thing under test.
//!
//! B is the control, on a second server with the ordinary `PATH`: the same
//! steps open a working shell, so A's refusal is dtach's absence and not a
//! terminal that never starts here at all.
//!
//! Run: deno run -A tests/browser/nodtach.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const bin = `${fx.base}/nodtach-bin`;
await Deno.mkdir(bin, { recursive: true });
for (const dir of (Deno.env.get("PATH") ?? "/usr/bin:/bin").split(":")) {
  if (!dir.startsWith("/")) continue;
  try {
    for await (const e of Deno.readDir(dir)) {
      if (e.name === "dtach") continue;
      // First on PATH wins, as it would for a lookup.
      try { await Deno.symlink(`${dir}/${e.name}`, `${bin}/${e.name}`); } catch { /* already linked */ }
    }
  } catch { /* an unreadable PATH entry: nothing to link from it */ }
}

const browser = await startBrowser(profileDir(repoRoot));
let roost, page;

/// Opens a terminal the way the UI does and returns once its socket has either
/// opened or been refused.
async function openTerminal(evalIn) {
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`window.__t = () => [...terms.values()][0];`);
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
  await until(() => evalIn("terms.size > 0 && !!__t()"), 30, "terminal entry");
}

try {
  console.log("A. no dtach on roost's PATH");
  ok(!(await Deno.lstat(`${bin}/dtach`).then(() => true, () => false)), "the fixture PATH really lacks dtach");
  ok(await Deno.lstat(`${bin}/git`).then(() => true, () => false), "…and still has everything else (git, for one)");
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { PATH: bin } });
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await openTerminal(page.evalIn);
  const status = () => page.evalIn("__t().node.dataset.status || ''");
  ok(await until(async () => { const s = await status(); return s !== "" && s !== "reconnecting…"; }, 20, "a status"),
     "the refused terminal settles on a status");
  const said = await status();
  ok(said.includes("dtach is not installed"), `it names what is missing: ${JSON.stringify(said)}`);
  ok(said.includes("install dtach"), "and says how to fix it");
  ok(said !== "session ended", "not the words a deliberate exit leaves");
  // A reason-bearing close is still a clean one. Were it read as a dead
  // connection, the client would retry into the same refusal forever.
  await sleep(3000);
  ok(await page.evalIn("__t().tries === 0 && !__t().sock"), "and it does not reconnect into the same refusal");
  ok((await status()) === said, "the explanation stays on screen");
  // The badge is an absolutely positioned ::after, sized to its content, so a
  // sentence in it ran off the pane's left edge. Bounding its width is what
  // makes it wrap; it already inherits `white-space: normal`, which a
  // revert-check showed, so that is not re-declared or asserted.
  const fits = await page.evalIn(`getComputedStyle(__t().node, "::after").maxWidth !== "none"`);
  ok(fits, "the badge wraps within the pane rather than overflowing it");
  page.close(); page = null;
  await roost.close(); roost = null;

  console.log("\nB. control: the ordinary PATH opens a real shell");
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await openTerminal(page.evalIn);
  ok(await until(() => page.evalIn("!!__t().sock && __t().sock.readyState === 1"), 30, "socket"),
     "with dtach present the same steps give a live terminal");
  ok((await page.evalIn("__t().node.dataset.status || ''")) === "", "and no status badge");
} finally {
  page?.close();
  browser.close();
  await roost?.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
