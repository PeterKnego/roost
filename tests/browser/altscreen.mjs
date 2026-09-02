//! Does a full-screen app's terminal state survive an attachment?
//!
//! Claude Code — like vim, less, htop — runs in the *alternate screen*: one
//! `\e[?1049h` at startup, one `\e[?1049l` at exit, and nothing in between
//! that says which buffer the frames belong in. roost's replay is a raw byte
//! ring (session.rs, 1 MB): once that single startup sequence falls off the
//! front, every browser that attaches afterwards paints the app's frames into
//! the *normal* buffer while the app still believes it is on the alternate
//! one. Nothing looks wrong until the app exits: its `\e[?1049l` then finds a
//! terminal that was never switched, so instead of restoring the pre-app
//! screen it only restores a cursor that was never saved (0,0) — and the exit
//! banner and the shell prompt print over the top of the leftover frame. That
//! is the "garbled screen when exiting claude" report.
//!
//! Run: deno run -A tests/browser/altscreen.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startProxy, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const proxyPort = await freePort();
const proxy = startProxy({ listenPort: proxyPort, upstreamPort: roost.port });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${proxyPort}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__buf = () => __t().term.buffer.active.type;
    // The *visible* screen, not the whole buffer. "Garbled screen" is a
    // statement about what is on the screen, and a scrollback-wide search
    // would find the pre-app marker sitting far above the viewport and pass
    // in exactly the broken case this exists to catch.
    window.__screen = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < __t().term.rows; i++) {
        const l = b.getLine(b.viewportY + i); if (l) s += l.translateToString(true) + "\\n"; }
      return s; };
    window.__onscreen = (n) => __screen().includes(n);
    window.__has = (n) => __txt().includes(n);
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

  console.log("\nB. something on the normal screen, the way a shell session starts");
  await sh("echo before-app-pk41");
  ok(await until(() => evalIn(`__has("before-app-pk41")`), 25, "marker"), "the pre-app screen carries a marker");

  console.log("\nC. a full-screen app opens the alternate screen and paints a frame");
  // `printf` rather than a real TUI: the whole of what a TUI does to the
  // terminal *state* is these two sequences, and a fake one can be driven
  // deterministically. What is under test is roost's handling of them.
  await sh(`printf '\\033[?1049h\\033[H\\033[2Jframe-body-pk41\\r\\n'`);
  ok(await until(() => evalIn(`__buf() === "alternate"`), 25, "alt buffer"),
     "the browser followed the app onto the alternate screen");
  ok(await evalIn(`__onscreen("frame-body-pk41") && !__onscreen("before-app-pk41")`),
     "the frame is on the alternate screen and the normal screen is untouched behind it");

  console.log("\nD. the app runs long enough to push its own startup off the 1 MB ring");
  // A real one does this in minutes: every repaint is kilobytes. `seq` is the
  // cheapest way to spend more than 1 MB of PTY output.
  await sh("seq 1 200000");
  ok(await until(() => evalIn(`__has("200000")`), 180, "1 MB of output"), "the ring has turned over");

  console.log("\nE. the browser reattaches — a reload, a sleeping laptop, a second tab");
  proxy.cut();
  await until(async () => (await evalIn("__t().node.dataset.status")) === "reconnecting…", 25, "badge");
  proxy.resume();
  ok(await until(() => evalIn("!!__t().sock && __t().sock.readyState === 1"), 40, "reconnect"), "reattached");
  await sleep(2500);
  ok(await evalIn(`__buf() === "alternate"`),
     "the replay put the browser back on the alternate screen the app is still on");

  console.log("\nF. the app exits");
  await sh(`printf '\\033[?1049l'; echo exit-banner-pk41`);
  await sleep(2500);
  ok(await evalIn(`__buf() === "normal"`), "leaving the alternate screen lands on the normal one");
  ok(await evalIn(`__onscreen("before-app-pk41")`),
     "the pre-app screen came back — the exit did not print over the app's leftover frame");
  ok(await evalIn(`!__onscreen("199999")`),
     "the app's own output is off the screen, not sitting under the exit banner");
} finally {
  page?.close();
  browser.close();
  proxy.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
