//! The ✻ button: a new terminal with `claude` already typed into it.
//!
//! Two things only a real browser on a real dtach can show. That the glyph
//! in the tab strip is wired to `NewTerminal{launch:"claude"}` — `cargo test`
//! proves the server types the program into the shell it spawns, but not
//! that anything on the page asks it to. And that the typed program actually
//! *runs* in a real `bash -l` under dtach: the keystrokes are written the
//! instant the PTY exists, well before the first prompt, and the README's
//! trap 3 records typeahead vanishing on this host. This is the test that
//! says whether the shell keeps it.
//!
//! `claude` itself is never run — a real one would call the Anthropic API.
//! `$SHELL` is a script that puts a fake `claude` first on `PATH` and execs
//! `bash --noprofile --norc`, so the login shell under test sees exactly the
//! programs this test gives it and none the developer has. The fake prints
//! the `CLAUDE_CODE_SSE_PORT` it was handed, which is the whole reason the
//! button needs no flags: that variable is how a claude started here finds
//! this roost's IDE socket.
//!
//! The second server has a `$SHELL` whose `PATH` holds nothing, which is how
//! the startup check (`launch::probe`) is shown to reach the page: the button
//! must not be there. The page attribute starts out offering it (the check
//! is asynchronous and an unfinished check must not hide a working button),
//! so both halves poll the HTML for the state they expect before opening it.
//!
//! Run: deno run -A tests/browser/claudeterm.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const fx = await fixture();
const fakebin = `${fx.base}/fakebin`;
const emptybin = `${fx.base}/emptybin`;
await Deno.mkdir(fakebin, { recursive: true });
await Deno.mkdir(emptybin, { recursive: true });
await Deno.writeFile(`${fakebin}/claude`, enc.encode(
  `#!/bin/sh\necho "FAKE-CLAUDE-STARTED sse=\${CLAUDE_CODE_SSE_PORT:-none}"\n`), { mode: 0o755 });
// `--noprofile --norc` so the developer's real PATH (and real claude) cannot
// leak in through ~/.profile; `-l`/`-i` from roost and from the probe are
// still passed through, so this is still the login interactive shell both
// of them ask for.
const shellWith = async (name, path) => {
  const p = `${fx.base}/${name}`;
  await Deno.writeFile(p, enc.encode(
    `#!/bin/sh\nPATH=${JSON.stringify(path)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`), { mode: 0o755 });
  return p;
};
const shellPresent = await shellWith("shell-present", `${fakebin}:/usr/bin:/bin`);
const shellAbsent = await shellWith("shell-absent", emptybin);

const pageHtml = async (port) => (await fetch(`http://127.0.0.1:${port}/${fx.project}`)).text();

const browser = await startBrowser(profileDir(repoRoot));
let page, roost;

try {
  // ---------------------------------------------------------- claude present
  console.log("A. claude is on the login shell's PATH");
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shellPresent } });
  ok(await until(async () => (await pageHtml(roost.port)).includes('data-launches="claude"'), 15, "page to offer claude"),
     "the page offers the claude launch");
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
      for (let i = 0; i < b.length; i++) o += b.getLine(i).translateToString(true) + "\\n"; return o; };
    window.__last = (s) => __txt(s).split("\\n").filter((l) => l.trim()).pop() || "";
    window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);`);

  const strips = Number(await evalIn(`document.querySelectorAll(".paneicons .newterm").length`));
  const stars = Number(await evalIn(`document.querySelectorAll(".paneicons .newclaude").length`));
  ok(stars > 0 && stars === strips, `every pane with a new-terminal control has the Claude one (${stars} of ${strips})`);
  ok((await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').title`)).includes("Claude"),
     "the glyph says what it does");

  const before = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`));
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length > before.length, 20, "a new terminal tab"),
     "clicking ✻ opens a new terminal tab");
  const sess = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).find((s) => !before.includes(s));
  ok(await until(() => evalIn(`terms.has(${JSON.stringify(sess)})`), 30, "the terminal"), `the tab is attached (${sess})`);
  const started = await until(async () => (await evalIn(`__txt(${JSON.stringify(sess)})`)).includes("FAKE-CLAUDE-STARTED"), 60, "claude to start");
  ok(started, "claude was started without anyone typing");
  const line = (await evalIn(`__txt(${JSON.stringify(sess)})`)).split("\n").find((l) => l.includes("FAKE-CLAUDE-STARTED")) || "";
  ok(/sse=\d+/.test(line), `it was handed this roost's IDE port (${line.trim()})`);
  ok(await until(async () => (await evalIn(`__last(${JSON.stringify(sess)})`)).trimEnd().endsWith("$"), 30, "a prompt after claude"),
     "the shell is still there when claude exits");

  console.log("\nB. the plain + is still plain");
  const before2 = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`));
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length > before2.length, 20, "a new terminal tab");
  const plain = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).find((s) => !before2.includes(s));
  await until(() => evalIn(`terms.has(${JSON.stringify(plain)})`), 30, "the terminal");
  ok(await until(async () => (await evalIn(`__last(${JSON.stringify(plain)})`)).trimEnd().endsWith("$"), 60, "a prompt"),
     "+ opens a shell at a prompt");
  ok(!(await evalIn(`__txt(${JSON.stringify(plain)})`)).includes("FAKE-CLAUDE"), "and nothing was typed into it");

  page.close(); page = null;
  await roost.close(); roost = null;

  // ----------------------------------------------------------- claude absent
  console.log("\nC. claude is not on the login shell's PATH");
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shellAbsent } });
  ok(await until(async () => (await pageHtml(roost.port)).includes('data-launches=""'), 15, "the check to finish"),
     "the startup check reaches the page: no launches offered");
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => page.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  const stars2 = Number(await page.evalIn(`document.querySelectorAll(".paneicons .newclaude").length`));
  const plus2 = Number(await page.evalIn(`document.querySelectorAll(".paneicons .newterm").length`));
  ok(stars2 === 0, `no ✻ anywhere (${stars2})`);
  ok(plus2 > 0, `the new-terminal control is still there (${plus2})`);
} finally {
  page?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
