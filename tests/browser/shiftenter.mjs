//! Shift+Enter sends a line feed, so Claude Code inserts a newline instead of
//! submitting.
//!
//! xterm sends a bare `\r` for Enter *and* for Shift+Enter, so an application
//! cannot tell the two apart — which is why Claude Code submits on Shift+Enter
//! in roost and leaves `\` + Enter as the only way to write a second line.
//! Claude binds its `chat:newline` action to Ctrl+J, and Ctrl+J *is* `\n`, so
//! sending LF for Shift+Enter is all it takes. Nothing here detects Claude:
//! everything that does not deliberately distinguish LF from CR treats them
//! alike (readline runs the line either way, which is why Shift+Enter still
//! submits at a shell prompt).
//!
//! This asserts the byte that reaches the pty, not what Claude does with it —
//! the shell reads one raw byte and prints its decimal value. The other half
//! of the chain was verified by hand against a real `claude` in a pty: fed
//! LF between two words it left both in the prompt box, fed CR it submitted
//! the first. Keeping Claude itself out of the test is deliberate — every
//! scenario here is hermetic, and one that called the Anthropic API would not
//! be. `cargo test` cannot reach any of this: it is all static/app.js.
//!
//! Run: deno run -A tests/browser/shiftenter.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";`);

  console.log("A. a live terminal at a shell prompt");
  const find = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return JSON.stringify({ pi, ti, session: state.panes[pi].tabs[ti].session }); } return null; })()`;
  const loc = JSON.parse(await evalIn(find));
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => evalIn(`terms.has(${JSON.stringify(loc.session)})`), 30, "the terminal"), "a terminal is attached");
  // readline discards typeahead while it initialises, so a command typed
  // before the first prompt vanishes silently (see the README's traps).
  ok(await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 60, "shell prompt"),
     "the shell is at a prompt");

  // One raw byte, printed as a number. `stty raw` is what makes this
  // discriminating: it turns OFF icrnl, which would otherwise translate a CR
  // into an LF on the way in and report 10 for both keys — the test would
  // pass with the handler deleted.
  //
  // RE''ADY, not READY: the command line is echoed to the same screen, so a
  // marker spelled the same way in both places is already on screen before od
  // runs, and every wait below would fall through instantly.
  const PROBE = `stty raw -echo; printf 'RE''ADY'; od -An -tu1 -N1; stty sane; printf 'FIN''ISHED\\n'`;
  const screen = () => evalIn("__txt()");
  const key = async (shift) => {
    await evalIn(`__t().term.focus()`);
    for (const type of ["keyDown", "keyUp"]) {
      await cmd("Input.dispatchKeyEvent", {
        type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13,
        modifiers: shift ? 8 : 0,
      });
    }
  };
  // Returns the byte the pty saw for one press, or null if the probe never ran.
  //
  // Counted, and the *last* match taken, because __txt() is the whole
  // scrollback: an earlier probe's "READY 13" is still sitting in it, and a
  // first-match read reported that instead of this run's. It passed anyway
  // while both answers were 13 — the second call was asserting on the first
  // call's output and could not have failed.
  const byteFor = async (shift, label) => {
    const markers = async () => ((await screen()).match(/READY/g) || []).length;
    const reports = async () => [...(await screen()).matchAll(/READY\s+(\d+)/g)];
    const [m0, r0] = [await markers(), (await reports()).length];
    await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "a prompt");
    await evalIn(`__t().term.input(${JSON.stringify(PROBE + "\r")})`);
    // A *new* marker, not any marker. The earlier probe's is still in the
    // scrollback, so "is READY on screen" was true before this od had even
    // started: the key went into the tty queue while the line discipline was
    // still canonical, icrnl turned the CR into an LF, and the probe reported
    // 10 for plain Enter. The whole buffer is safe to count precisely because
    // the echoed command spells the marker RE''ADY.
    if (!await until(async () => (await markers()) > m0, 20, `${label}: od to start`)) return null;
    await key(shift);
    if (!await until(async () => (await reports()).length > r0, 20, `${label}: od to report`)) return null;
    // The last report, not the first: an earlier probe's is still up there.
    return Number((await reports()).pop()[1]);
  };

  console.log("\nB. shift+enter sends a line feed");
  const shifted = await byteFor(true, "shift+enter");
  ok(shifted === 10, `shift+enter put byte ${shifted} on the pty (10 = LF, what Claude binds chat:newline to)`);

  console.log("\nC. plain enter is untouched");
  // The half that stops the fix from becoming "send LF for every Enter",
  // which would look identical in Claude and break every shell prompt.
  const plain = await byteFor(false, "enter");
  ok(plain === 13, `enter still puts byte ${plain} on the pty (13 = CR)`);
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
