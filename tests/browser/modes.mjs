//! Do the modes an app declared once survive an attachment?
//!
//! A full-screen app states its whole terminal contract in its first hundred
//! bytes — bracketed paste, mouse reporting, focus reporting, cursor
//! visibility — one sequence each, never repeated. roost's replay is a bounded
//! byte ring, so within a minute or two of the app running, none of that is in
//! it any more, and a browser that reloads gets a terminal with every one of
//! them back at its default while the app believes otherwise.
//!
//! What this asserts is mouse reporting, and the reason is worth writing down,
//! because the obvious symptom to test turns out not to be testable here.
//!
//! Bracketed paste is the symptom a user feels: without `?2004h` xterm.js sends
//! a pasted block unwrapped, so the newline inside it arrives as Enter and a
//! pasted three-line prompt submits its first line on its own. Both states were
//! measured — `"\e[200~one\rtwo\e[201~"` in step, `"one\rtwo"` not — so it is a
//! real difference. But an app declares bracketed paste *before* it enters the
//! alternate screen (Claude Code emits `?2004h`, `?1004h`, then `?1049h`), and
//! since one ring per screen landed, the normal ring holds those and stops
//! growing the moment the app switches. They are already durable. Asserting on
//! paste here would be asserting on something that cannot fail — it passed with
//! this whole commit reverted, which is how the trap was found.
//!
//! The mouse modes are declared *after* the switch, so they live in the
//! alternate ring, which the app's own repaints evict within minutes. They are
//! what this restores, along with cursor visibility and anything else declared
//! mid-app. Reverted, the assertion below reads `true,none,true` — paste and
//! focus carried by the normal ring, mouse gone.
//!
//! The app below is held by a `sleep` rather than left at a shell prompt for a
//! related reason: **bash re-declares bracketed paste before every prompt**, so
//! a prompting shell heals itself. A full-screen app is the case where nothing
//! re-declares anything.
//!
//! The restart path is deliberately not exercised: an app never re-states its
//! modes, so a roost that restarts under one cannot know them, and this fixes
//! attachments (reload, wake, second tab, ring turnover) rather than restarts.
//!
//! Run: deno run -A tests/browser/modes.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startProxy, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const proxyPort = await freePort();
const proxy = startProxy({ listenPort: proxyPort, upstreamPort: roost.port });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${proxyPort}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__modes = () => { const m = __t().term.modes;
      return [m.bracketedPasteMode, m.mouseTrackingMode, m.sendFocusMode].join(","); };
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";`);

  const prompt = () => until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 60, "shell prompt");
  const sh = async (line) => { await prompt(); await evalIn(`__t().term.input(${JSON.stringify(line + "\r")})`); };

  console.log("A. start a terminal");
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
  ok(await until(() => evalIn("terms.size > 0 && !!__t().sock && __t().sock.readyState === 1"), 30, "socket"),
     "terminal socket open");

  console.log("\nB. an app declares its contract exactly as Claude Code does, spends");
  console.log("   a megabyte, and keeps the terminal — no prompt comes back to heal it");
  // One command line, so no prompt is drawn after the declaration: a prompt
  // would re-declare bracketed paste and hide the very defect under test.
  // `sleep` stands in for a TUI sitting there holding the terminal.
  await sh(`printf '\\033[?2004h\\033[?1004h\\033[?1049h\\033[?1000h\\033[?1002h\\033[?1003h\\033[?1006h'; seq 1 200000; sleep 300`);
  ok(await until(async () => (await evalIn("__modes()")) === "true,any,true", 60, "modes"),
     "the browser took on the modes the app declared");
  ok(await until(() => evalIn(`__txt().includes("200000")`), 180, "1 MB of output"),
     "the ring turned over while the app held the terminal");

  console.log("\nC. the browser reattaches — a reload, a sleeping laptop, a second tab");
  proxy.cut();
  await until(async () => (await evalIn("__t().node.dataset.status")) === "reconnecting…", 25, "badge");
  proxy.resume();
  ok(await until(() => evalIn("!!__t().sock && __t().sock.readyState === 1"), 40, "reconnect"), "reattached");
  await sleep(2500);

  // Named one by one: a bare tuple mismatch would not say which mode went.
  const m = (await evalIn("__modes()")).split(",");
  ok(m[1] === "any", `mouse reporting came back with it (got ${m[1]})`);
  ok(m[0] === "true" && m[2] === "true",
     "and the modes the normal ring already carried are still there");
} finally {
  page?.close();
  browser.close();
  proxy.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
