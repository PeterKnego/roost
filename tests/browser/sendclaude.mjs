//! Send to Claude from the tree's menu. #146.
//!
//! Only a real browser on a real dtach can show the part that matters: that
//! the path reaches a Claude's *input*, which for a Claude the click itself
//! started means waiting until that Claude is listening. Typed too early, it
//! is gone — measured against a real `claude` on this host: input sent when
//! `?2004h` first appears is lost, 0.5 s later it lands.
//!
//! `claude` itself is never run (a real one calls the Anthropic API). The
//! fake reproduces the three things this feature depends on: it **discards
//! whatever was typed before it was ready** (`tcflush`, which is what makes
//! "pasted too early" fail here as it does for real), it then disables and
//! re-enables bracketed paste the way Claude Code does as it mounts, and it
//! records every byte it is sent so the assertion is on the paste markers as
//! well as the path — a typed path would pass a text check and could answer a
//! permission prompt for real.
//!
//! Revert-checks performed, all restored:
//!   (a) `claimClaudeSend` pasting at once instead of via `whenClaudeReady` ->
//!       section A fails: the fake flushed the path before it was listening.
//!   (b) `term.paste(p.text)` -> `term.input(p.text)` in `claimClaudeSend` ->
//!       section A fails on the `ESC[200~` marker alone: the path arrives, as
//!       keystrokes. (The fake records partial reads for exactly this; with
//!       `read -d '~'` alone a typed path was never written, and (b) failed
//!       for the wrong reason.)
//!   (c) `claudeTarget` returning null always -> section B fails: the click
//!       asks for a second Claude, which the worktree prompt intercepts, so
//!       the running one never receives the path.
//!
//! Run: deno run -A tests/browser/sendclaude.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const fx = await fixture();
await Deno.writeTextFile(`${fx.dir}/build.log`, "x\n");
await Deno.mkdir(`${fx.dir}/sub dir`, { recursive: true });
await Deno.writeTextFile(`${fx.dir}/sub dir/note one.txt`, "y\n");
const got = `${fx.base}/claude-got`;
const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(fakebin, { recursive: true });
// One second of start-up, then a flush: anything sent during start-up is lost,
// exactly the failure the readiness wait exists for.
await Deno.writeFile(`${fakebin}/claude`, enc.encode(`#!/bin/bash
stty -icanon -echo
sleep 1
python3 -c 'import termios; termios.tcflush(0, termios.TCIFLUSH)'
printf '\\033[?2004l\\033[?2004h'
echo FAKE-CLAUDE-READY
while true; do
  IFS= read -r -t 0.3 -d '~' chunk; rc=$?
  printf '%s' "$chunk" >> ${JSON.stringify(got)}
  [ $rc -eq 0 ] && printf '~' >> ${JSON.stringify(got)}
done
`), { mode: 0o755 });
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(
  `#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`),
  { mode: 0o755 });

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
  extraEnv: { SHELL: shell } });
const browser = await startBrowser(profileDir(repoRoot));
let page;
const readGot = async () => { try { return await Deno.readTextFile(got); } catch { return ""; } };

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector("ul.tree")`), 20, "tree");
  ok(await until(() => evalIn(`LAUNCHES.includes("claude")`), 20, "claude offered"), "the page offers claude");

  // The menu is driven for real; only the choice is stubbed, as in download.mjs.
  const openMenuOn = async (sel, choice) => {
    await evalIn(`window.__items = null;
      askMenu = (o) => { window.__items = o.items.map((i) => ({ id: i.id, label: i.label }));
                         return Promise.resolve(${JSON.stringify(choice)}); }; true`);
    const found = await evalIn(`(() => {
      const el = document.querySelector(${JSON.stringify(sel)});
      if (!el) return false;
      el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 8, clientY: 8 }));
      return true;
    })()`);
    await sleep(350);
    return { found, items: JSON.parse(await evalIn(`JSON.stringify(window.__items)`) || "null") };
  };
  const claudeTabs = () => evalIn(`JSON.stringify(state.panes.flatMap((p) => p.tabs)
    .filter((t) => t.k === "Terminal" && (state.claude_sessions || []).includes(t.session)).map((t) => t.session))`)
    .then(JSON.parse);

  console.log("A. with no Claude running, a new one is started and the path pasted once it listens");
  const menu = await openMenuOn('a[data-rel="build.log"]', "claude");
  ok(menu.found, "the file is in the tree");
  ok((menu.items || []).some((i) => i.id === "claude" && i.label === "Send to Claude"),
     `Send to Claude is offered (${JSON.stringify(menu.items)})`);
  ok(await until(async () => (await readGot()).includes("@build.log "), 30, "the fake to receive the path"),
     `the fake Claude received the path (${JSON.stringify(await readGot())})`);
  ok((await readGot()).startsWith("\x1b[200~@build.log \x1b[201"),
     `as one bracketed paste, not keystrokes (${JSON.stringify(await readGot())})`);
  const first = await claudeTabs();
  ok(first.length === 1, `one Claude terminal (${JSON.stringify(first)})`);
  ok(await until(() => evalIn(`lastFocusedSession === ${JSON.stringify(first[0])}`), 10, "focus"),
     "and it is the focused terminal");

  console.log("\nB. with one running, it goes to that one, and a spaced path is quoted");
  if (!(await evalIn(`!!document.querySelector('a[data-rel="sub dir/note one.txt"]')`))) {
    await evalIn(`(() => { const d = document.querySelector('details[data-rel="sub dir"]'); if (d) d.open = true; })()`);
    await until(() => evalIn(`!!document.querySelector('a[data-rel="sub dir/note one.txt"]')`), 10, "nested file");
  }
  await openMenuOn('a[data-rel="sub dir/note one.txt"]', "claude");
  ok(await until(async () => (await readGot()).includes(`@"sub dir/note one.txt" `), 15, "second path"),
     `the running Claude received the quoted path (${JSON.stringify(await readGot())})`);
  const second = await claudeTabs();
  ok(JSON.stringify(second) === JSON.stringify(first), `no second Claude was started (${JSON.stringify(second)})`);

  console.log("\nC. a folder is not offered it");
  const onDir = await openMenuOn('details[data-rel="sub dir"] > summary', null);
  ok(onDir.found, "the folder is in the tree");
  ok(!(onDir.items || []).some((i) => i.id === "claude"), `not offered on a folder (${JSON.stringify(onDir.items)})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
