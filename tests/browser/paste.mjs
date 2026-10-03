//! The paste button on the terminal key bar. #97.
//!
//! **This file cannot test the bug it exists for, and pretending otherwise is
//! the point of this paragraph.** The report is that iOS raises no Paste
//! callout when you long-press a terminal. This host is a headless Linux VM
//! driving Chromium; the callout, and whether Safari's paste confirmation is
//! satisfied by the button's gesture, are not reachable from here. CLAUDE.md's
//! *dev/prod substitution trap* is a table of four defects a green suite could
//! not see because the test substituted something simpler for the real thing,
//! and a Chromium-on-Linux test for an iOS Safari bug is a fifth row of it
//! waiting to be written. Confirmation on a real iPhone is part of closing #97.
//!
//! What is genuinely covered: the button is in the bar and the bar still fits a
//! phone; a paste reaches the pty **bracketed when the program asked for
//! bracketing and bare when it did not**; the fallback dialog opens when the
//! Clipboard API is missing or refuses, and its text reaches the pty the same
//! way; and a clipboard that reads successfully does not open the dialog.
//!
//! Revert-checks performed, all restored:
//!   (a) `term.input` instead of `term.paste` in `pasteInto` -> B and D fail
//!       (nothing arrives; `head` is still waiting for the six wrapper bytes)
//!       and **C fails too**, reporting `"alpha\nbeta"`. The plan predicted C
//!       would stay green here and it does not, which is worth more than the
//!       prediction was: `paste` normalises the newline to CR and `input` does
//!       not, so `input` is wrong in two ways, not one.
//!   (b) hard-coding the wrapper -> **only C fails**, at `"\x1b[200~alph"`.
//!       This is the pair working: B alone passes against a button that always
//!       brackets, which would put literal escape characters into every plain
//!       shell; C alone passes against one that never does.
//!   (c) deleting the `askPasteText` call -> all four of D fail, no dialog.
//!   (d) `if (text === null)` weakened to `if (!text)` -> only E fails: a
//!       clipboard that read successfully as empty opens a dialog over it.
//!
//! One more, on the test rather than the code: the first version sized
//! `head -c` generously instead of exactly. Every capture came back empty or
//! holding the *next* probe's command line, because `head` was still waiting
//! when the file was read and the shell stayed inside it. Sized exactly, the
//! file is complete the moment it reaches length — and the byte count is
//! pinned too, so a paste carrying anything extra fails rather than passing.
//!
//! Run: deno run -A tests/browser/paste.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

// Multi-line on purpose: one line would pass whether or not the paste was
// bracketed, because the difference only shows when a newline is in it.
const TEXT = "alpha\nbeta";

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  // The bar is mobile-only (`body[data-mpane="3"] #termkeys`), so a desktop
  // viewport would find every button zero-sized and every assertion vacuous.
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";`);

  const loc = JSON.parse(await evalIn(`(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return JSON.stringify({ pi, ti, session: state.panes[pi].tabs[ti].session }); } return null; })()`));
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  await until(() => evalIn(`terms.has(${JSON.stringify(loc.session)})`), 30, "terminal");
  await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 60, "prompt");
  // The terminal pane has to be the one in front, or `targetTerm()` resolves
  // by last-focused instead and the bar would be testing a different path.
  await evalIn(`document.body.dataset.mpane = "3"; __t().term.focus(); 0`);

  console.log("A. the bar carries a paste button, and still fits a phone");
  const bar = await evalIn(`(() => {
    const bs = [...document.querySelectorAll("#termkeys button")];
    const p = bs.find((b) => b.dataset.k === "paste");
    if (!p) return JSON.stringify({ found: false });
    const r = p.getBoundingClientRect();
    const overflow = document.getElementById("termkeys").scrollWidth
                   - document.getElementById("termkeys").clientWidth;
    return JSON.stringify({ found: true, n: bs.length, w: r.width, h: r.height, overflow });
  })()`);
  const b = JSON.parse(bar);
  ok(b.found, "there is a paste button");
  ok(b.n === 8, `the bar has eight buttons (got ${b.n})`);
  // ~/projects/CLAUDE.md's tap-target floor, and the reason the button count
  // matters. Measured at 390px: seven buttons gave 51px each, eight give
  // exactly 44 — the floor itself. **The row is full.** A ninth button fails
  // this assertion rather than quietly shipping a control too small to hit,
  // which is what it is here to do.
  ok(b.h >= 44, `it is tall enough to tap (${Math.round(b.h)}px)`);
  ok(b.w >= 44, `and wide enough (${Math.round(b.w)}px)`);
  ok(b.overflow <= 1, `the bar does not scroll sideways (overflow ${b.overflow}px)`);

  // The exact bytes the pty receives, the technique shiftenter.mjs uses: raw
  // mode so nothing is translated on the way in, and a file so the assertion
  // reads bytes rather than a rendered screen.
  //
  // `head -c <exact>` rather than a round number, and this is the whole of why
  // the first version of this test failed everywhere. Sized generously, `head`
  // is still waiting when the assertion reads the file — so the read came back
  // empty — and the shell stays inside `head` swallowing the *next* probe's
  // command line, which is how a later capture came back holding
  // `…\x1b[201~printf '\0`. Sized exactly, `head` exits on the last byte of
  // the paste and the file is complete when it appears.
  //
  // It also makes the test stricter: the byte *count* is pinned, so a paste
  // that arrives with anything extra fails rather than being tolerated.
  const capture = async (expect, run) => {
    const out = `${fx.base}/paste-${(capture.n = (capture.n || 0) + 1)}.bin`;
    await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "a prompt");
    // `printf` sets or clears DECSET 2004 on xterm's side — the program
    // telling the terminal it wants bracketed paste, exactly as Claude Code
    // does. Then raw mode, then capture.
    const mode = expect.startsWith("\x1b[200~") ? "h" : "l";
    const n = new TextEncoder().encode(expect).length;
    await evalIn(`__t().term.input(${JSON.stringify(
      `printf '\\033[?2004${mode}'; stty raw -echo; head -c ${n} > ${out}; stty sane; printf 'CAP''TURED\\n'\r`)})`);
    // The probe has to be *inside* `head` before the paste arrives, or the
    // bytes land on the command line instead. Waiting for the file to exist is
    // the positive evidence that the redirect has been set up.
    await until(async () => { try { await Deno.stat(out); return true; } catch { return false; } },
                20, "the capture to start");
    await run();
    // Waits for the whole expected length. A paste that is bracketed when it
    // should not be (or the reverse) never reaches `n`, so this times out and
    // returns what did arrive — which the assertions then report verbatim.
    const complete = await until(
      async () => { try { return (await Deno.stat(out)).size >= n; } catch { return false; } },
      15, "the pasted bytes");
    // Recovery, and it is what makes each section an independent test rather
    // than a domino. When the bytes never arrive the shell is still sitting
    // inside `head`, so every later capture inherits a terminal that is not at
    // a prompt and reports `null` — three failures for one defect, only the
    // first of them true. Found by running revert-check (a): it failed B, and
    // then C and D as well, which would have said C was not the control it is.
    if (!complete) {
      // JSON.stringify, not an escape inside the template literal: the
      // template resolves `\\r` to a real carriage return before the string
      // reaches the page, and a raw CR inside a JS string literal is a line
      // terminator — `SyntaxError: Invalid or unexpected token`, thrown inside
      // `capture` where it reads as the test harness breaking rather than as
      // the bug it is.
      await evalIn(`__t().term.input(${JSON.stringify("\u0003")})`);
      await sleep(300);
      await evalIn(`__t().term.input(${JSON.stringify("stty sane\r")})`);
      await sleep(600);
    }
    try { return new TextDecoder().decode(await Deno.readFile(out)); } catch { return null; }
  };

  const tapPaste = async () => {
    await evalIn(`document.querySelector('#termkeys button[data-k="paste"]')
      .dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }))`);
  };
  // Stubs the Clipboard API, because a headless Chromium has no clipboard and
  // a permission prompt is not something a test can answer.
  const stubClipboard = async (impl) =>
    await evalIn(`(() => { Object.defineProperty(navigator, "clipboard", {
      configurable: true, value: ${impl} }); return true; })()`);

  console.log("\nB. a paste is bracketed when the program asked for it");
  await stubClipboard(`{ readText: () => Promise.resolve(${JSON.stringify(TEXT)}) }`);
  // xterm normalises the newline to CR on the way out, so this is the byte
  // sequence a program actually sees — pinned whole rather than probed with
  // `includes`, which would pass on a paste carrying extra bytes either side.
  const WRAPPED = `\x1b[200~alpha\rbeta\x1b[201~`;
  const on = await capture(WRAPPED, tapPaste);
  // Revert-check (a): `term.input` instead of `term.paste` fails exactly here,
  // with the text arriving bare — which in Claude means every line submitted
  // as its own turn.
  ok(on === WRAPPED,
     `it arrives wrapped in the bracketed-paste markers (got ${JSON.stringify(on)})`);

  console.log("\nC. and bare when it did not");
  // The control, and B means nothing without it: a button that always wrapped
  // would pass B and corrupt every paste into a plain shell, which would see
  // the escape sequence as literal characters.
  await stubClipboard(`{ readText: () => Promise.resolve(${JSON.stringify(TEXT)}) }`);
  const BARE = "alpha\rbeta";
  const off = await capture(BARE, tapPaste);
  ok(off === BARE, `no wrapper when the program never asked (got ${JSON.stringify(off)})`);

  console.log("\nD. a refused clipboard falls back to a box you can paste into");
  await stubClipboard(`{ readText: () => Promise.reject(new Error("NotAllowedError")) }`);
  await tapPaste();
  ok(await until(() => evalIn(`document.getElementById("dlg-paste").open`), 10, "the dialog"),
     "the paste dialog opens");
  ok(await evalIn(`document.activeElement === document.getElementById("dlg-paste-text")`),
     "with the box focused, which is what makes a long-press offer a callout");
  ok(/Long-press/.test(await evalIn(`document.querySelector("#dlg-paste .dlg-label").textContent`)),
     "and says what to do");
  // The box must reach the pty by the same route, or the fallback would be a
  // second paste implementation with its own bugs.
  const fromBox = await capture(WRAPPED, async () => {
    await evalIn(`(() => { const t = document.getElementById("dlg-paste-text");
      t.value = ${JSON.stringify(TEXT)};
      document.querySelector("#dlg-paste .dlg-ok").click(); })()`);
  });
  ok(fromBox === WRAPPED, `text sent from the box arrives bracketed too (got ${JSON.stringify(fromBox)})`);

  console.log("\nE. an empty clipboard is an answer, not a failure");
  // "I read it and it was empty" is not "I could not read it". Folding them
  // together reopens a prompt over a clipboard the user genuinely emptied —
  // this codebase's own "absence of evidence is not evidence of absence",
  // pointed at a UI. Revert-check (d) is this assertion.
  await stubClipboard(`{ readText: () => Promise.resolve("") }`);
  await tapPaste();
  await sleep(800);
  ok(!(await evalIn(`document.getElementById("dlg-paste").open`)),
     "a successful empty read opens no dialog");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
