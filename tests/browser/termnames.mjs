//! What a terminal is called, and what the tab strip calls it.
//!
//! A ✻ click used to be handed whichever `termN` happened to be free, so a
//! strip reading `term, term1, term2` said nothing about which tab had an
//! agent in it. It is given `claude`, `claude2`, … instead, and the strip
//! renders those as "Claude", "Claude 2".
//!
//! The name itself still has to satisfy `^[A-Za-z0-9_-]{1,32}$` — it lands in
//! a dtach socket path and on a command line — so the readable form is the
//! client's business and nothing that addresses a session uses it.
//!
//! Its own file rather than a section of mobile.mjs, where it lived first:
//! the assertions depend on which sessions exist, and by the time that file
//! reached them it had opened terminals in five earlier sections. It failed
//! intermittently there for that reason and nothing else. Here the state is
//! whatever this test made.
//!
//! Run: deno run -A tests/browser/termnames.mjs
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

  const sessions = async () => await evalIn(`state.panes[3].tabs.map((t) => t.session || t.k)`);
  const labels = async () => await evalIn(
    `[...document.querySelectorAll('.pane[data-pane="3"] .tabstrip .tab')]
       .map((t) => t.textContent.replace("×", "").trim())`);

  console.log("A. a plain terminal keeps the historic names");
  // `default_layout` seeds `term`, and existing layouts on disk are full of
  // `termN`, so that sequence is not renumbered — an orphaned socket is a
  // worse outcome than an inconsistent-looking pair of sequences.
  ok((await sessions()).includes("term"), "the seeded tab is `term`");
  await evalIn(`send({ t: "NewTerminal", pane: 3 })`);
  ok(await until(async () => (await sessions()).includes("term1"), 15, "term1"),
     "and the + button carries on with `term1`");

  console.log("B. a Claude click is named for what it is");
  ok(!(await sessions()).some((s) => /^claude/.test(s)), "setup: no Claude terminal yet");
  await evalIn(`send({ t: "NewTerminal", pane: 3, launch: "claude" })`);
  ok(await until(async () => (await sessions()).includes("claude"), 15, "claude"),
     "the first is `claude`, not the next free termN");
  // `force`, because a second ✻ with a Claude already running answers with the
  // worktree prompt instead of opening — a different feature, tested in hub.
  await evalIn(`send({ t: "NewTerminal", pane: 3, launch: "claude", force: true })`);
  ok(await until(async () => (await sessions()).includes("claude2"), 15, "claude2"),
     "the second is `claude2` — numbered from 2, because the first has no number");

  console.log("C. the strip says it in words");
  const shown = await labels();
  ok(shown.includes("Claude") && shown.includes("Claude 2"),
     `they read as Claude and Claude 2 (${JSON.stringify(shown)})`);
  ok(shown.includes("term") && shown.includes("term1"),
     "while a plain terminal is still shown by its name");
  // The names themselves are unchanged, which is what everything else in
  // roost addresses a session by — a socket path, a command line, an intent.
  const raw = await sessions();
  ok(raw.every((s) => /^[A-Za-z0-9_-]{1,32}$/.test(s)),
     `and every name is still a legal session name (${JSON.stringify(raw)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
