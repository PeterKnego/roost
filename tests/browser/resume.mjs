//! The offer to continue the Claude a reboot interrupted. #18 step 2.
//!
//! roost is handed a Claude Code session id on every hook event and, until
//! this, wrote it down and read it back nowhere: `claudesess::recorded` had
//! exactly one caller outside its own module and that caller was a test. So a
//! reboot left a tab that said its shell was gone and offered only a fresh
//! start, while the conversation it had been running sat on disk, resumable,
//! with roost holding its id.
//!
//! What only a browser can check is here, and deliberately only that. The
//! bytes that reach the shell (`claude --resume <id>`, and the refusal to type
//! an id that fails its check) are pinned in Rust, at the hub, where the
//! recorded id and the typing meet. This file covers the two things no Rust
//! test can see: whether the button renders where it should and nowhere else,
//! and which message each way of activating the placeholder actually sends.
//!
//! **Every case needs its own tab**, which is why the fixture looks
//! over-provisioned: starting a terminal consumes its placeholder, and the
//! placeholder is the thing under test. Four activations — Enter on the box,
//! a click on the box, a click on the button, Enter on the focused button —
//! are four tabs, across two projects.
//!
//! Section B is the negative control and it is the reason A means anything. A
//! terminal tab is seeded whose shell is equally gone and for which nothing
//! was recorded — the same input in every respect except the one under test.
//! An offer rendered on every lost tab and an offer rendered on the right one
//! are indistinguishable until something renders a lost tab that has nothing
//! to resume.
//!
//! Run: deno run -A tests/browser/resume.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const ID = "864ee734-e3ab-434e-8278-745a850b16ad";

/// The layout a reboot leaves: terminal tabs roost watched a shell run in,
/// with no sockets left. One per pane so every placeholder renders at once and
/// none needs a tab click.
const seed = async (key, sessions) => {
  const panes = [{ tabs: [{ k: "Tree" }], active: 0 }, { tabs: [], active: 0 },
                 { tabs: [], active: 0 }, { tabs: [], active: 0 }];
  sessions.forEach(([pane, session]) => {
    panes[pane].tabs.push({ k: "Terminal", session });
  });
  await Deno.writeTextFile(`${fx.stateDir}/${key}.json`, JSON.stringify({
    sizes: { left_w: 260, right_w: 520, left_split: 60 },
    panes,
    buffers: {},
    // What makes these losses rather than tabs nobody opened: roost watched a
    // shell run in each and wrote that down at its last save.
    sessions_seen: sessions.map(([, s]) => s),
  }));
};

/// What the hook would have written, in the shape `claudesess` writes it.
const record = async (key, session) => {
  await Deno.mkdir(`${fx.stateDir}/claude/${key}`, { recursive: true });
  await Deno.writeTextFile(`${fx.stateDir}/claude/${key}/${session}.json`,
    JSON.stringify({ session_id: ID, event: "Stop" }));
};

// proj: `plain` has no record (section B, and the plain-Enter case);
// `ghost2` and `ghost` do.
await seed("proj", [[1, "ghost2"], [2, "plain"], [3, "ghost"]]);
await record("proj", "ghost");
await record("proj", "ghost2");

// A second project for the one activation proj has no tab left for. A plain
// terminal tab, nothing recorded.
const proj2 = `${fx.roots}/proj2`;
await Deno.mkdir(proj2, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: proj2, stdout: "null", stderr: "null" }).output();
await seed("proj2", [[3, "clicked"]]);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const paneSel = (n) => `document.querySelector('.pane[data-pane="${n}"]')`;
const GHOST = paneSel(3), PLAIN = paneSel(2), GHOST2 = paneSel(1);
const resumeBtn = (p) => `${p}?.querySelector('.termstart .termresume')`;
const boxOf = (p) => `${p}.querySelector('.termstart')`;

const load = async (project) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  // The middle pane collapses to a few pixels at the default 800x600, which
  // has already made a test in this suite assert against an element nothing
  // could have rendered into.
  await p.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  // Wrapping `send` is what makes section C discriminating. Asserting only
  // that a terminal appears would pass just as well if the button sent a plain
  // `StartTerminal` — the visible outcome is identical, and the difference
  // (whether the conversation comes back) is invisible for as long as the
  // machine has no `claude` to run.
  await p.evalIn(`window.__sent = []; window.__origSend = send;
                  send = (m) => { window.__sent.push(JSON.stringify(m)); return window.__origSend(m); }; true`);
  return p;
};
const starts = async (p) => JSON.parse(await p.evalIn(`JSON.stringify(window.__sent)`))
  .map(JSON.parse).filter((m) => m.t === "StartTerminal");

try {
  page = await load(fx.project);
  const { evalIn } = page;

  console.log("A. a lost tab whose Claude roost recorded");
  // The server half, apart from the rendering: the two fail separately, and a
  // missing button means something very different depending on which side
  // dropped it.
  ok(await until(() => evalIn(`(state.resumable_sessions || []).includes("ghost")`), 15, "the server's list"),
     "the server names the session it recorded a Claude for");
  ok(await until(() => evalIn(`!!(${resumeBtn(GHOST)})`), 15, "the button"),
     "and its placeholder offers to resume it");
  ok((await evalIn(`${boxOf(GHOST)}?.textContent ?? ""`)).includes("to start a terminal"),
     "while still offering a plain shell — the resume is an extra control, not a replacement");

  console.log("B. a lost tab with nothing recorded");
  ok(await evalIn(`!(state.resumable_sessions || []).includes("plain")`),
     "the server does not name it");
  ok(await evalIn(`!(${resumeBtn(PLAIN)})`),
     "and no resume is offered on it");
  // The claim `claudesess`'s module doc forbids: absence of a record is
  // *unknown*, never "no Claude ran here". Nothing on the page may say so.
  ok((await evalIn(`${boxOf(PLAIN)}?.textContent ?? ""`)).includes("did not survive"),
     "it still says its shell is gone");
  ok(!(await evalIn(`document.body.textContent`)).match(/no claude|never ran|no conversation/i),
     "and nothing anywhere claims it had no Claude");

  console.log("C. what each way of starting the terminal asks for");
  // Enter on the box, which is where the placeholder puts focus. It must not
  // resume: #17 is explicit that a resume continues a conversation whose last
  // turn may have been mid-edit, so it is offered and never the thing already
  // under the user's fingers.
  await evalIn(`(() => { const b = ${boxOf(PLAIN)};
                b.focus(); b.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); })()`);
  ok(await until(async () => (await starts(page)).length === 1, 15, "the intent"),
     "Enter on the box sends StartTerminal");
  ok((await starts(page)).every((m) => m.resume !== true),
     "and never asks to resume");

  // Enter with the *button* focused, which is a different path from either
  // click and the one that actually needed a guard. A <button> turns Enter
  // into a click itself, but keydown reaches the box first — so without
  // `e.target === box` on the box's own handler, the plain start wins the
  // `dataset.sent` race and the user who tabbed to "Resume" and pressed Enter
  // gets a bare shell, silently, with the button gone.
  await evalIn(`window.__sent = []; true`);
  await evalIn(`(() => { const b = ${resumeBtn(GHOST2)}; b.focus();
                b.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); b.click(); })()`);
  const viaKey = await starts(page);
  ok(viaKey.length === 1 && viaKey[0].resume === true && viaKey[0].session === "ghost2",
     `Enter on the focused button resumes rather than starting a plain shell (got ${JSON.stringify(viaKey)})`);

  // The click, and the assertion that there is exactly one intent: the button
  // sits inside a box that is itself a control, so a click on it runs both
  // handlers on the way up.
  await evalIn(`window.__sent = []; true`);
  await evalIn(`${resumeBtn(GHOST)}.click(); true`);
  const viaClick = await starts(page);
  ok(viaClick.length === 1,
     `the button sends exactly one StartTerminal, not one per handler on the way up (got ${viaClick.length})`);
  ok(viaClick.length === 1 && viaClick[0].resume === true && viaClick[0].session === "ghost",
     "and it asks to resume this session");
  // The id is not on the wire. It lands on a command line, so the browser is
  // told *that* there is one and never its value; the server looks it up.
  ok(!JSON.stringify(await evalIn(`JSON.stringify(window.__sent)`)).includes(ID),
     "the session id never crosses the wire");

  console.log("D. the offer goes when the shell comes back");
  ok(await until(() => evalIn(`state.live_sessions.includes("ghost")`), 30, "the shell"),
     "the terminal starts");
  ok(await until(() => evalIn(`!(state.resumable_sessions || []).includes("ghost")`), 30, "the prune"),
     "and the session stops being offered as resumable");
  ok(await until(() => evalIn(`!(${resumeBtn(GHOST)})`), 15, "the button to go"),
     "so the button is off the screen");

  console.log("E. a plain click on the placeholder");
  // Not a variation on the Enter case in C. The box's handler receives the
  // event as its first argument and an Event is truthy, so a handler wired as
  // `box.onclick = start` rather than `() => start(false)` turns every click
  // on a placeholder into a resume. Nothing else in this file clicks the box.
  try { await page.close(); } catch { /* already gone */ }
  page = await load("proj2");
  ok(await until(() => page.evalIn(`!!(${boxOf(GHOST)})`), 15, "the placeholder"), "its tab renders one");
  await page.evalIn(`${boxOf(GHOST)}.click(); true`);
  const viaBox = await until(async () => (await starts(page)).length === 1, 15, "the intent")
    ? await starts(page) : [];
  ok(viaBox.length === 1, "clicking the placeholder starts a terminal");
  ok(viaBox.length === 1 && viaBox[0].resume !== true,
     `and asks for a plain shell, not a resume (got ${JSON.stringify(viaBox)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
