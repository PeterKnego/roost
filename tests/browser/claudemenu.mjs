//! The ✻ button's menu: start fresh, or continue a past conversation. #18.
//!
//! The history is Claude Code's, not roost's — `~/.claude/projects/<encoded
//! cwd>/<session-id>.jsonl` — so this test gives roost its own `HOME` and
//! writes transcripts into it. That is the only honest way to reach the state:
//! the harness otherwise passes the developer's real home, where the fixture's
//! temporary project has never been opened and the menu is correctly empty.
//!
//! Section C is the one Dean asked for by name and the reason the others mean
//! anything: **with no history the ✻ button must not open a menu at all**, it
//! must launch a fresh Claude exactly as it did before this existed. A menu
//! that always appears and a menu that appears when there is something in it
//! are indistinguishable until a project with nothing is clicked.
//!
//! Run: deno run -A tests/browser/claudemenu.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const home = `${fx.base}/home`;
// Claude Code's own encoding, verified against this host's real directory:
// every character outside [A-Za-z0-9] becomes '-'.
const encode = (p) => p.replace(/[^A-Za-z0-9]/g, "-");

const OLD = "aaaa1111-2222-3333-4444-555555555555";
const NEW = "bbbb2222-3333-4444-5555-666666666666";
const tdir = `${home}/.claude/projects/${encode(fx.dir)}`;
await Deno.mkdir(tdir, { recursive: true });
const transcript = (text) => [
  JSON.stringify({ type: "mode", sessionId: "x" }),
  // An injected turn, which must be skipped in favour of the real one below —
  // otherwise every row would read "caveat: ...".
  JSON.stringify({ type: "user", isMeta: true, message: { content: "caveat: system note" } }),
  JSON.stringify({ type: "user", message: { content: text } }),
].join("\n");
await Deno.writeTextFile(`${tdir}/${OLD}.jsonl`, transcript("the older conversation"));
await Deno.writeTextFile(`${tdir}/${NEW}.jsonl`, transcript("the newer conversation"));
// Explicit mtimes: two files written in the same millisecond would make this
// test's subject the tie-break rather than the ordering.
const t0 = new Date(1_700_000_000_000);
await Deno.utime(`${tdir}/${OLD}.jsonl`, t0, t0);
await Deno.utime(`${tdir}/${NEW}.jsonl`, t0, new Date(t0.getTime() + 60_000));

// A second project with no transcripts at all — section C.
const bare = `${fx.roots}/bare`;
await Deno.mkdir(bare, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: bare, stdout: "null", stderr: "null" }).output();

const roost = await startRoost({
  repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
  extraEnv: { HOME: home },
});
const browser = await startBrowser(profileDir(repoRoot));
let page;

const claudeBtn = `document.querySelector('.pane[data-pane="3"] .paneicon.newclaude')`;
const menu = `document.querySelector('.chist')`;
const rows = `Array.from(document.querySelectorAll('.chist .chistrow')).map(r => ({label: r.querySelector('.chistlabel').textContent, id: r.dataset.resume}))`;

const load = async (project) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  await p.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  await p.evalIn(`window.__sent = []; window.__origSend = send;
                  send = (m) => { window.__sent.push(JSON.stringify(m)); return window.__origSend(m); }; true`);
  return p;
};
const sent = async (p) => JSON.parse(await p.evalIn(`JSON.stringify(window.__sent)`)).map(JSON.parse);

try {
  page = await load(fx.project);
  const { evalIn } = page;

  console.log("A. the menu lists this project's conversations");
  ok(await evalIn(`!!(${claudeBtn})`), "the ✻ button is there");
  await evalIn(`${claudeBtn}.click(); true`);
  ok(await until(() => evalIn(`!!(${menu})`), 15, "the menu"), "clicking it opens a menu");
  const got = JSON.parse(await evalIn(`JSON.stringify(${rows})`));
  ok(got.length === 3, `New plus both conversations (got ${got.length}: ${JSON.stringify(got)})`);
  ok(got[0] && got[0].id === "", "the first row is New, and carries no id");
  ok(got[1] && got[1].id === NEW && got[2] && got[2].id === OLD,
     `newest first (got ${JSON.stringify(got.map((r) => r.id))})`);
  // The label proves the transcript was actually read, not just listed — and
  // that the injected turn above was skipped.
  ok(got[1] && got[1].label === "the newer conversation",
     `the label is the first thing the user really said (got ${JSON.stringify(got[1] && got[1].label)})`);

  console.log("B. picking a row asks to resume that conversation");
  await evalIn(`window.__sent = []; true`);
  await evalIn(`document.querySelectorAll('.chist .chistrow')[1].click(); true`);
  const pick = (await sent(page)).filter((m) => m.t === "NewTerminal");
  ok(pick.length === 1 && pick[0].launch === "claude" && pick[0].resume === NEW,
     `one NewTerminal naming the chosen conversation (got ${JSON.stringify(pick)})`);
  ok(await until(() => evalIn(`!(${menu})`), 15, "the menu to close"), "and the menu closes");

  console.log("B2. picking New asks for a fresh one");
  await evalIn(`window.__sent = []; ${claudeBtn}.click(); true`);
  await until(() => evalIn(`!!(${menu})`), 15, "the menu");
  await evalIn(`document.querySelector('.chist .chistnew').click(); true`);
  const fresh = (await sent(page)).filter((m) => m.t === "NewTerminal");
  ok(fresh.length === 1 && fresh[0].launch === "claude" && fresh[0].resume === undefined,
     `a plain claude launch, with no resume on it (got ${JSON.stringify(fresh)})`);

  console.log("C. a project with no history opens no menu at all");
  // The requirement in its own words: "če ni zgodovine odpre itak claude
  // fresh". Not a menu with one row — no menu.
  try { await page.close(); } catch { /* already gone */ }
  page = await load("bare");
  await page.evalIn(`${claudeBtn}.click(); true`);
  const direct = await until(async () => (await sent(page)).some((m) => m.t === "NewTerminal"), 15, "the launch");
  ok(direct, "clicking ✻ launches straight away");
  ok(!(await page.evalIn(`!!(${menu})`)), "and no menu was shown");
  const bareSent = (await sent(page)).filter((m) => m.t === "NewTerminal");
  ok(bareSent.length === 1 && bareSent[0].launch === "claude" && bareSent[0].resume === undefined,
     `exactly the message ✻ always sent (got ${JSON.stringify(bareSent)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
