//! A terminal tab running a Claude wears the Claude mark instead of the
//! terminal glyph.
//!
//! Three things only a real browser on a real dtach can show, and no
//! `cargo test` can reach any of them.
//!
//! 1. That `app.js` puts `data-claude` on the tab at all — the Rust side
//!    proves only that the snapshot carries the session names.
//! 2. That `style.css` turns that attribute into a *different picture*. This
//!    is why the assertions read `getComputedStyle(tab, "::before")
//!    .backgroundImage` and look for the brand colour in it, rather than
//!    stopping at the attribute: with the `[data-claude]` rule deleted the
//!    attribute is still set, the tab still looks like every other terminal,
//!    and an attribute-only test would pass green — README trap 2.
//! 3. That the watcher's `/proc` walk sees a Claude nobody launched through
//!    roost. The command is typed by hand into an ordinary shell here,
//!    which is precisely the case the in-process launch record cannot cover.
//!
//! The fake `claude` is a *copy of a real binary*, not a shell script, and
//! that is load-bearing: detection matches `/proc/<pid>/comm`, which for a
//! `#!/bin/sh` script is `sh`. claudeterm.mjs's script-based fake would be
//! invisible to this feature, so a test built on one would assert nothing.
//!
//! It is a copy of `bash` rather than of `sleep` for two reasons found by
//! running it. Coreutils here is a multi-call binary that dispatches on
//! `argv[0]`, so a copy named `claude` answers `unknown program 'claude'` and
//! exits — the first run of this file failed exactly that way, and the fake
//! never ran at all. And the trailing `; :` in the command matters: without
//! it bash exec-optimises its last command, replacing itself, and `comm`
//! becomes `sleep` again.
//!
//! Run: deno run -A tests/browser/claudetab.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const fx = await fixture();
const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(fakebin, { recursive: true });
// A real ELF named `claude`, so `comm` is `claude`. See the header.
await Deno.copyFile("/bin/bash", `${fakebin}/claude`);
await Deno.chmod(`${fakebin}/claude`, 0o755);
// `--noprofile --norc` so the developer's real PATH — and their real claude —
// cannot leak in and be the thing that gets detected.
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(
  `#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`,
), { mode: 0o755 });

const browser = await startBrowser(profileDir(repoRoot));
let page, roost;

try {
  roost = await startRoost({
    repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
    extraEnv: { SHELL: shell },
  });
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  await evalIn(`
    window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);
    // The tab element for a session. Terminal tabs carry no dirty/stale span,
    // so the label is the first child node; the × is a span after it.
    window.__tab = (s) => [...document.querySelectorAll('.tabstrip .tab[data-kind="terminal"]')]
      .find((e) => e.childNodes[0] && e.childNodes[0].textContent.trim() === s) || null;
    window.__marked = (s) => { const e = __tab(s); return !!e && e.hasAttribute("data-claude"); };
    window.__icon = (s) => { const e = __tab(s);
      return e ? getComputedStyle(e, "::before").backgroundImage : "no-tab"; };
  `);

  // Two terminals: one to type `claude` into, one that must stay unmarked.
  // Without the control, a client that ignored the session names and marked
  // every terminal tab would pass every other assertion in this file.
  const openTerm = async () => {
    const before = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`));
    await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
    await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length > before.length, 20, "a new terminal tab");
    const s = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).find((x) => !before.includes(x));
    await until(() => evalIn(`terms.has(${JSON.stringify(s)})`), 30, "the terminal");
    await until(() => evalIn(`__tab(${JSON.stringify(s)}) !== null`), 10, "the tab element");
    return s;
  };

  console.log("A. a plain shell is a plain terminal tab");
  const sess = await openTerm();
  const control = await openTerm();
  const S = JSON.stringify(sess);
  const C = JSON.stringify(control);
  ok(sess !== control, `two terminals to compare (${sess}, ${control})`);

  // Recorded before anything is typed and asserted against afterwards: an
  // "it contains the mark" check alone would also pass on a stylesheet that
  // gave every tab the mark.
  const plainIcon = await evalIn(`__icon(${S})`);
  ok((await evalIn(`__marked(${S})`)) === false, "a shell with no Claude is not marked");
  ok(typeof plainIcon === "string" && plainIcon.includes("svg") && !plainIcon.includes("D97757"),
     "it wears the terminal glyph, not the mark");

  console.log("\nB. a claude typed by hand is detected and marks the tab");
  await evalIn(`terms.get(${S}).term.input("claude --norc --noprofile -c 'sleep 600; :'\\r")`);
  ok(await until(() => evalIn(`__marked(${S})`), 25, "the tab to be marked"),
     "typing `claude` marks its tab — no launch record, no IDE connection");
  const claudeIcon = await evalIn(`__icon(${S})`);
  ok(claudeIcon !== plainIcon, "and the picture actually changed");
  ok(String(claudeIcon).includes("D97757"), "it is the Claude mark, brand-filled");
  ok((await evalIn(`__marked(${C})`)) === false, `the other terminal stayed unmarked (${control})`);

  console.log("\nC. it goes away when claude does");
  await evalIn(`terms.get(${S}).term.input("\\u0003")`);
  ok(await until(async () => (await evalIn(`__marked(${S})`)) === false, 25, "the mark to clear"),
     "killing claude returns the tab to the terminal glyph");
  ok((await evalIn(`__icon(${S})`)) === plainIcon, "and it is the same glyph it started with");
} finally {
  page?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
