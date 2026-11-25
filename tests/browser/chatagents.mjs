//! The chat opens a subagent's own conversation from its Agent card.
//!
//! `cargo test` covers finding the subagent's transcript (`locate_agent`),
//! reading it (`Transcript::open_subagent`) and refusing odd ids. What only a
//! browser can show is the view: that the card opens it, that it follows the
//! file as it grows and its record as it finishes, that a nested subagent
//! opens from inside one, and that back — the button and the phone's own
//! back gesture, which are the same history entry — climbs out one level at
//! a time instead of closing the chat.
//!
//! `claude` is a fake: a shell script, so the kernel names the process
//! `claude` and `chat::locate` finds it by `/proc` exactly as it finds a real
//! one. It writes the `~/.claude/sessions/<pid>.json` record a real Claude
//! writes (session id, cwd, process start time) under a `HOME` this test owns,
//! then waits. The transcripts are written by the test, in the layout Claude
//! Code 2.1.330 uses: `<session>.jsonl`, and beside it
//! `<session>/subagents/agent-<id>.jsonl` with an `agent-<id>.meta.json`
//! naming the `toolUseId` that started it.
//!
//! Revert-checks performed, all restored:
//!   (a) the Agent card rendered like any other card instead of opening
//!       the subagent -> A fails: no card is marked as one that opens.
//!   (b) the popstate handler closing the chat whatever the entry (as before
//!       this) -> D fails: back from a subagent closed the chat.
//!   (c) `showAgentView` leaving the composer shown -> B fails on "the
//!       composer is hidden".
//!
//! Run: deno run -A tests/browser/chatagents.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const SESSION_ID = "0b1f2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
const fx = await fixture();
const home = `${fx.base}/home`;
const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(`${home}/.claude/projects`, { recursive: true });
await Deno.mkdir(fakebin, { recursive: true });
// Not `exec sleep`: the process must stay the script, whose name is `claude`.
await Deno.writeFile(`${fakebin}/claude`, enc.encode(`#!/bin/sh
mkdir -p "$HOME/.claude/sessions"
st=$(awk '{print $22}' /proc/$$/stat)
printf '{"sessionId":"${SESSION_ID}","cwd":"%s","procStart":"%s","startedAt":1}' "$PWD" "$st" \\
  > "$HOME/.claude/sessions/$$.json"
printf '%s' "$PWD" > "$HOME/.claude/fake-cwd"
echo FAKE-CLAUDE-UP
while :; do sleep 1; done
`), { mode: 0o755 });
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(
  `#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`),
  { mode: 0o755 });

const line = (v) => JSON.stringify(v) + "\n";
const user = (content, side) => line({ type: "user", ...(side ? { isSidechain: true } : {}), message: { role: "user", content } });
const said = (text, side) => line({ type: "assistant", ...(side ? { isSidechain: true } : {}),
  message: { role: "assistant", content: [{ type: "text", text }] } });
const call = (id, name, input, side) => line({ type: "assistant", ...(side ? { isSidechain: true } : {}),
  message: { role: "assistant", content: [{ type: "tool_use", id, name, input }] } });
const result = (id, text, side) => user([{ type: "tool_result", tool_use_id: id, content: text }], side);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
  extraEnv: { SHELL: shell, HOME: home } });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");

  console.log("A. a Claude terminal whose conversation started a subagent");
  const before = JSON.parse(await evalIn(`JSON.stringify(state.panes.flatMap((p) => p.tabs.filter((t) => t.k === "Terminal").map((t) => t.session)))`));
  await until(() => evalIn(`!!document.querySelector('.pane[data-pane="3"] .paneicons .newclaude')`), 15, "the ✻ button");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click(); true`);
  let sess = "";
  await until(async () => {
    const now = JSON.parse(await evalIn(`JSON.stringify(state.panes.flatMap((p) => p.tabs.filter((t) => t.k === "Terminal").map((t) => t.session)))`));
    sess = now.find((s) => !before.includes(s)) || "";
    return !!sess;
  }, 20, "a new terminal");
  let cwd = "";
  await until(async () => { try { cwd = await Deno.readTextFile(`${home}/.claude/fake-cwd`); } catch { /* not yet */ } return !!cwd; },
    60, "the fake claude to start");
  ok(!!sess && !!cwd, `claude runs in ${sess}`);
  const tdir = `${home}/.claude/projects/${cwd.replace(/[^A-Za-z0-9]/g, "-")}`;
  const subs = `${tdir}/${SESSION_ID}/subagents`;
  await Deno.mkdir(subs, { recursive: true });
  await Deno.writeTextFile(`${tdir}/${SESSION_ID}.jsonl`,
    user("survey the tests") +
    call("toolu_01Bash", "Bash", { command: "ls" }) + result("toolu_01Bash", "a b") +
    call("toolu_01Abc", "Agent", { description: "Survey the tests", prompt: "Look at tests/" }) +
    call("toolu_03Gone", "Agent", { description: "Lost one", prompt: "?" }) +
    said("Started a survey."));
  await Deno.writeTextFile(`${subs}/agent-a1.meta.json`,
    JSON.stringify({ agentType: "Explore", description: "Survey the tests", toolUseId: "toolu_01Abc" }));
  await Deno.writeTextFile(`${subs}/agent-a1.jsonl`,
    user("Look at tests/ and count them", true) +
    call("toolu_02Nest", "Agent", { description: "Deeper look", prompt: "Read one" }, true) +
    said("SUBAGENT-SAYS-HELLO", true));
  await Deno.writeTextFile(`${subs}/agent-a2.meta.json`,
    JSON.stringify({ agentType: "general-purpose", description: "Deeper look", toolUseId: "toolu_02Nest", finished: true }));
  await Deno.writeTextFile(`${subs}/agent-a2.jsonl`, user("Read one", true) + said("NESTED-SAYS-HI", true));

  await evalIn(`openChat(${JSON.stringify(sess)}, PROJECT); true`);
  let cards = 0;
  await until(async () => {
    cards = Number(await evalIn(`document.querySelectorAll("#chat .chatlog:not(.chatsublog) .ctool").length`));
    return cards >= 3;
  }, 20, "the conversation");
  ok(cards >= 3, `the chat shows the conversation (${cards} tool cards)`);
  const marked = JSON.parse(await evalIn(`JSON.stringify([...document.querySelectorAll("#chat .chatlog:not(.chatsublog) .ctool")].map((c) => c.classList.contains("cagent")))`));
  ok(JSON.stringify(marked) === "[false,true,true]", `only the Agent cards open a subagent (${JSON.stringify(marked)})`);

  const view = () => evalIn(`JSON.stringify({
    sub: !document.querySelector("#chat .chatsublog").hidden,
    main: !document.querySelector("#chat .chatlog:not(.chatsublog)").hidden,
    form: !document.querySelector("#chat .chatform").hidden,
    open: !document.getElementById("chat").hidden,
    title: document.querySelector("#chat .chatagenttitle").textContent,
    state: document.querySelector("#chat .chatagentstate").textContent,
    text: document.querySelector("#chat .chatsublog").textContent,
  })`).then(JSON.parse);

  console.log("\nB. the card opens the subagent's conversation");
  await evalIn(`document.querySelectorAll("#chat .cagent .ctoolhead")[0].click(); true`);
  ok(await until(async () => (await view()).text.includes("SUBAGENT-SAYS-HELLO"), 15, "the subagent's words"),
     "its conversation is shown");
  let v = await view();
  ok(v.sub && !v.main, "in place of the main one");
  ok(v.title === "Survey the tests" && v.state === "running", `titled, and running (${v.title} · ${v.state})`);
  ok(!v.form, "the composer is hidden: what is typed would go to the main Claude");

  console.log("\nC. it follows the subagent as it works and finishes");
  await Deno.writeTextFile(`${subs}/agent-a1.jsonl`, said("LATE-LINE", true), { append: true });
  ok(await until(async () => (await view()).text.includes("LATE-LINE"), 10, "the appended line"), "a line it writes appears");
  await Deno.writeTextFile(`${subs}/agent-a1.meta.json`,
    JSON.stringify({ agentType: "Explore", description: "Survey the tests", toolUseId: "toolu_01Abc", finished: true }));
  ok(await until(async () => (await view()).state === "finished", 10, "finished"), "and its record finishing says so");

  console.log("\nD. a nested subagent, and back out one level at a time");
  await evalIn(`document.querySelector("#chat .chatsublog .cagent .ctoolhead").click(); true`);
  ok(await until(async () => (await view()).text.includes("NESTED-SAYS-HI"), 15, "the nested one"), "the nested subagent opens");
  ok((await view()).title === "Deeper look", "under its own title");
  await evalIn(`document.querySelector("#chat .chatagentback").click(); true`);
  ok(await until(async () => (await view()).text.includes("SUBAGENT-SAYS-HELLO"), 10, "the outer one"), "back returns to the outer subagent");
  // The phone's back gesture is the same history step as the button.
  await evalIn(`history.back(); true`);
  ok(await until(async () => { const w = await view(); return w.main && !w.sub; }, 10, "the main conversation"),
     "back again returns to the conversation");
  v = await view();
  ok(v.open && v.form, "with the chat still open and the composer back");

  console.log("\nE. a subagent with no transcript says so");
  await evalIn(`document.querySelectorAll("#chat .cagent .ctoolhead")[1].click(); true`);
  ok(await until(async () => (await view()).state === "not found", 10, "not found"), "the bar says not found");
  ok((await view()).text.includes("was not found"), "and the view says where to look instead");
  await evalIn(`history.back(); true`);
  await sleep(300);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
