//! Does a terminal survive the connection dying under it?
//!
//! The bug this pins: a laptop waking from sleep left every terminal silently
//! swallowing keystrokes. The control socket retried; the terminal socket only
//! marked itself stale, and the sole thing that rebuilt it — ensureTerm, via
//! mountTab — is skipped by render() while the same tab stays active. So the
//! terminal healed only if the user happened to switch tabs, and until then
//! onData dropped every keystroke with no error and nothing on screen to
//! distinguish it from a live idle shell.
//!
//! Run: deno run -A tests/browser/reconnect.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startProxy, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const proxyPort = await freePort();
const proxy = startProxy({ listenPort: proxyPort, upstreamPort: resh.port });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${proxyPort}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__count = (n) => (__txt().match(new RegExp(n, "g")) || []).length;
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";`);

  // Keystrokes sent before bash has finished initialising are discarded by
  // readline, so every command waits for a prompt first — the same thing a
  // human does without thinking. Without this the assertions fail for a
  // reason that has nothing to do with reconnecting.
  const prompt = () => until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "shell prompt");
  // term.input(), not term.paste(): bash enables bracketed paste, under which
  // a pasted newline is inserted literally instead of submitting the line, so
  // a pasted command sits on the prompt forever and every later assertion
  // waits on output that is never coming.
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
  // A Terminal tab is only a tab: the shell starts when the client asks,
  // because opening a project must never fork one by itself.
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => evalIn("terms.size > 0 && !!__t().sock && __t().sock.readyState === 1"), 30, "socket"),
     "terminal socket open");

  console.log("\nB. mark the shell, so a respawn would be detectable");
  await sh("MARKER=zx9q7");
  await sh("echo hit-$MARKER");
  ok(await until(async () => (await evalIn(`__count("hit-zx9q7")`)) >= 1, 25, "marker output"),
     "the shell runs commands and echoes the marker");
  // Push the marker up into xterm's scrollback. Without this the whole
  // session fits on one screen, where dtach's redraw (which opens with
  // \e[H\e[J) hides any duplication by itself: the no-duplication assertion
  // below then passes even with the reset deleted, which it did.
  await sh("seq 1 60");
  await until(() => evalIn(`__txt().includes("\\n60")`), 25, "scrolled output");
  const before = await evalIn(`__count("hit-zx9q7")`);

  console.log("\nC. the laptop sleeps");
  proxy.cut();
  ok(await until(async () => (await evalIn("__t().node.dataset.status")) === "reconnecting…", 25, "badge"),
     "the dead socket says so on screen instead of silently eating keystrokes");
  ok(await evalIn("__t().sock === null || __t().sock.readyState !== 1"),
     "the socket really is down (guards against a vacuous pass below)");

  console.log("\nD. and wakes");
  await sleep(3000);
  const tries = await evalIn("__t().tries");
  ok(tries >= 2, `it kept retrying while the network was gone (${tries} attempts)`);
  proxy.resume();
  ok(await until(() => evalIn("!!__t().sock && __t().sock.readyState === 1"), 40, "reconnect"),
     "the socket came back with no user action at all");
  ok(await until(async () => (await evalIn("__t().node.dataset.status")) === "", 15, "badge clear"), "badge cleared");

  console.log("\nE. the same shell, and the screen is not printed twice");
  await sh("echo back-$MARKER");
  ok(await until(async () => (await evalIn(`__count("back-zx9q7")`)) >= 1, 25, "output"),
     "typing works again AND $MARKER survived — reattached, never respawned");
  const after = await evalIn(`__count("hit-zx9q7")`);
  ok(after === before, `the scrollback replay repainted rather than appended (${before} -> ${after} copies)`);

  console.log("\nE2. and it survives the server restarting under it");
  ok(await resh.restart(), "resh restarted");
  ok(await until(() => evalIn("!!__t().sock && __t().sock.readyState === 1"), 40, "reconnect"),
     "reconnected across a server restart");
  await sh("echo again-$MARKER");
  ok(await until(async () => (await evalIn(`__count("again-zx9q7")`)) >= 1, 25, "output"),
     "the shell itself outlived the server — dtach still holds it");

  console.log("\nF. exiting must not resurrect it");
  await sh("exit");
  ok(await until(async () => await evalIn(`terms.size === 0 || __t().node.dataset.status === "session ended"`), 30, "clean close"),
     "a deliberate exit ends it");
  // A clean close must not be retried: session::attach creates the session
  // when it is absent, so a reconnect here would fork a fresh shell every
  // time someone typed `exit`.
  await sleep(4000);
  ok(await evalIn("terms.size === 0 || !__t().sock"), "no socket reopened — no shell forked behind the user's back");
} finally {
  page?.close();
  browser.close();
  proxy.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
