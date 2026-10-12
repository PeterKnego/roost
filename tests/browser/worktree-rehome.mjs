//! Claude's `WorktreeCreate` hook, end to end: ask, approve, re-home, close
//! and fork (#156).
//!
//! No Rust test can reach `static/app.js`, and this feature's promise is a
//! chain that crosses it twice: a hook process asks the server over its Unix
//! socket, the server asks a *browser*, a person's click opens a new tab, and
//! the terminal the hook ran in shows up on that tab — the same shell, not a
//! new one — while the page it came from says where it went. Each scenario
//! below drives one link of that chain through a real Chromium against a real
//! roost on real dtach.
//!
//! The fake `claude` is a real ELF named `claude`, compiled here from a few
//! lines of C (`FAKE_C` below), for the reason `claudetab.mjs` gives: roost's
//! Claude detection reads `/proc/<pid>/comm`, which a script would report as
//! `sh`. It reads lines from its terminal and answers each with a marker the
//! typed line itself cannot contain (upper case vs the lower-case command,
//! and a tag the test chooses), so a wait can never be satisfied by the echo
//! of what was typed:
//!
//!   `worktree <name>` runs `$FAKE_ROOST worktree-hook` — the binary the
//!     harness started, never whatever is on PATH — with Claude Code's
//!     payload on stdin; exit 0 prints `HOOK-PATH <name> <path>` and
//!     `chdir`s there, as Claude Code does; non-zero prints
//!     `HOOK-REFUSED <name> <stderr>`. Either way `HOOK-MS <name> <ms>`.
//!   `pid <tag>` prints `PID <tag> <getpid()>`; `pwd <tag>` prints
//!     `CWD <tag> <getcwd()>`.
//!   Every line it prints is also appended to `$FAKE_LOG`, which is how
//!     scenario D reads it with no page open.
//!
//! The roost terminal's environment is roost's own (`ROOST_PROJECT`,
//! `ROOST_SESSION`, `ROOST_STATE_DIR` — inherited, not set by this file), so
//! the hook reaches the hook socket of *this* run's server exactly as a real
//! Claude's hook would.
//!
//! Scenario order is A, B, E, C, D: B needs A's re-homed shell, E needs a
//! main-page terminal B did not end, C reuses E's terminal to run the fake
//! again, and D closes every page, so it comes last.
//!
//! Every absence asserted here is paired with a positive check that the
//! thing would be visible if present: the main page's tab for the moved
//! terminal (the one `openTerm` returns, `term1` — not the seeded `term`,
//! which never attaches) is asserted present before the move; `worktree-slow`'s absence from `git worktree
//! list` beside `worktree-fox`'s presence; D's channel is proved with a
//! `pid` answer before the refused request is sent; and E's fork choice is
//! asserted *absent* from the snapshot the page already held, so the
//! dialog's own `RequestState` round trip is what makes it appear.
//!
//! Scenario D drives the terminal with `dtach -p` (push stdin into the
//! session's socket) — no page and no pty helper needed, and it writes into
//! the same pty a browser would. Output comes back through `$FAKE_LOG`.
//!
//! Revert-check (CLAUDE.md: "would this fail if I deleted the code it
//! covers?"), applied, watched failing, restored:
//!   Task 4's resident skip in `session.rs`'s `kill_project_inner` — the
//!   `Owner::Elsewhere(_) => { spared.push(..); false }` retain arm —
//!   turned into `=> true`, so Close Project on main ends the re-homed
//!   shell too. Observed: every other section still green, and B fails 2 —
//!     ok    the close ended the native term2
//!       (timed out waiting for pid b1)
//!     FAIL  the worktree page's terminal still answers, same process (undefined)
//!       (timed out waiting for pwd b2)
//!     FAIL  …and still in worktree-fox
//!   B carries a second native terminal for that reason: its socket going
//!   is what says the close really ran, so "the resident survived" cannot
//!   pass against a Close Project that did nothing. Restored with `cp`:
//!   46 ok, 0 FAIL.
//!
//!   `openWorktreeDialog`'s `await requestFreshState();` (static/app.js)
//!   deleted, binary rebuilt (assets are embedded). No Rust test reaches
//!   it, so this is its only guard. Observed: E fails exactly 1, every other
//!   section green —
//!     ok    the page's current snapshot does not yet offer it as forkable
//!       (timed out waiting for the fork choice)
//!     FAIL  the + dialog offers "Continue term's Claude there (fork)" — from its own RequestState
//!   The "not yet forkable" read sits right before the + click, after the
//!   panel's own fetches, so the whole window in which only RequestState
//!   could change the answer is covered. A missing fork choice dismisses
//!   the dialog with a real Escape rather than crashing, so C and D still
//!   report. Restored with `cp`: 46 ok, 0 FAIL.
//!
//! Run: deno run -A tests/browser/worktree-rehome.mjs
import { attachTarget, fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();
const dec = new TextDecoder();
const t0 = Date.now();
const stamp = () => `${((Date.now() - t0) / 1000).toFixed(1)}s`;

const FAKE_C = String.raw`
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/time.h>
static FILE *logf;
static void say(const char *fmt, const char *a, const char *b) {
  char buf[8192];
  snprintf(buf, sizeof buf, fmt, a, b);
  fputs(buf, stdout); fputs("\r\n", stdout); fflush(stdout);
  if (logf) { fputs(buf, logf); fputs("\n", logf); fflush(logf); }
}
static long now_ms(void) { struct timeval t; gettimeofday(&t, 0); return t.tv_sec * 1000L + t.tv_usec / 1000; }
static void readall(int fd, char *out, size_t cap) {
  size_t n = 0; ssize_t r;
  while (n + 1 < cap && (r = read(fd, out + n, cap - 1 - n)) > 0) n += r;
  out[n] = 0;
  while (n && (out[n-1] == '\n' || out[n-1] == '\r')) out[--n] = 0;
  for (size_t i = 0; i < n; i++) if (out[i] == '\n') out[i] = ' ';
}
int main(int argc, char **argv) {
  const char *lp = getenv("FAKE_LOG");
  if (lp) logf = fopen(lp, "a");
  char args[2048] = "";
  for (int i = 1; i < argc; i++) { strncat(args, argv[i], 1000); strncat(args, " ", 2); }
  say("FAKE-READY argv=%s%s", args, "");
  char line[1024];
  while (fgets(line, sizeof line, stdin)) {
    line[strcspn(line, "\r\n")] = 0;
    char cmd[64] = "", arg[256] = "";
    if (sscanf(line, "%63s %255s", cmd, arg) < 1) continue;
    char cwd[4096]; if (!getcwd(cwd, sizeof cwd)) strcpy(cwd, "?");
    if (!strcmp(cmd, "pid")) { char p[32]; snprintf(p, sizeof p, "%d", (int)getpid()); say("PID %s %s", arg, p); }
    else if (!strcmp(cmd, "pwd")) say("CWD %s %s", arg, cwd);
    else if (!strcmp(cmd, "worktree")) {
      const char *roost = getenv("FAKE_ROOST");
      char payload[8192];
      snprintf(payload, sizeof payload,
        "{\"hook_event_name\":\"WorktreeCreate\",\"session_id\":\"t1\",\"cwd\":\"%s\",\"name\":\"%s\"}", cwd, arg);
      int in[2], out[2], err[2];
      if (pipe(in) || pipe(out) || pipe(err)) { say("HOOK-REFUSED %s %s", arg, "pipe failed"); continue; }
      long t = now_ms();
      pid_t c = fork();
      if (c == 0) {
        dup2(in[0], 0); dup2(out[1], 1); dup2(err[1], 2);
        close(in[1]); close(out[0]); close(err[0]);
        execl(roost, roost, "worktree-hook", (char *)0);
        perror("exec"); _exit(127);
      }
      close(in[0]); close(out[1]); close(err[1]);
      write(in[1], payload, strlen(payload)); close(in[1]);
      char o[8192], e[8192];
      readall(out[0], o, sizeof o); readall(err[0], e, sizeof e);
      close(out[0]); close(err[0]);
      int st = 0; waitpid(c, &st, 0);
      char ms[32]; snprintf(ms, sizeof ms, "%ld", now_ms() - t);
      if (WIFEXITED(st) && WEXITSTATUS(st) == 0) {
        say("HOOK-PATH %s %s", arg, o);
        if (chdir(o)) say("CHDIR-FAILED %s %s", arg, o);
      } else {
        say("HOOK-REFUSED %s %s", arg, e);
      }
      say("HOOK-MS %s %s", arg, ms);
    }
  }
  return 0;
}
`;

const fx = await fixture();
const project = fx.project;
const projDir = fx.dir;
const git = async (...args) => {
  const o = await new Deno.Command("git", { args: ["-C", projDir, ...args], stdout: "piped", stderr: "piped" }).output();
  return { ok: o.success, out: dec.decode(o.stdout), err: dec.decode(o.stderr) };
};
await git("config", "user.email", "t@t"); await git("config", "user.name", "t");
await git("add", "."); await git("commit", "-qm", "init");

// The roost binary the harness runs (startRoost does not return it, so the
// same path is derived the same way). startRoost builds it before anything
// in a terminal can call it.
const meta = JSON.parse(dec.decode((await new Deno.Command("cargo", {
  args: ["metadata", "--format-version", "1", "--no-deps"], cwd: repoRoot, stdout: "piped",
}).output()).stdout));
const roostBin = `${meta.target_directory}/debug/roost`;

const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(fakebin, { recursive: true });
await Deno.writeTextFile(`${fx.base}/fake.c`, FAKE_C);
const fakeLog = `${fx.base}/fake.log`;
await Deno.writeTextFile(fakeLog, "");
// `--noprofile --norc` so the developer's real claude cannot be the one that
// runs. FAKE_ROOST/FAKE_LOG are this file's own; nothing roost reads.
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(
  `#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\n` +
  `FAKE_ROOST=${JSON.stringify(roostBin)}; export FAKE_ROOST\nFAKE_LOG=${JSON.stringify(fakeLog)}; export FAKE_LOG\n` +
  `exec /bin/bash --noprofile --norc "$@"\n`,
), { mode: 0o755 });

const browser = await startBrowser(profileDir(repoRoot));
let page, page2, page3, roost;
// Joins soft-wrapped rows (xterm's isWrapped), so a long path is one line.
const helpers = `window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
    for (let i = 0; i < b.length; i++) { o += b.getLine(i).translateToString(true); const n = b.getLine(i + 1); if (!n || !n.isWrapped) o += "\\n"; } return o; };
  window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);
  window.__tabOf = (s) => state.panes.flatMap((p) => p.tabs).find((t) => t.k === "Terminal" && t.session === s) || null;`;
const clickReal = async (pg, selector) => {
  const r = JSON.parse(await pg.evalIn(`JSON.stringify(document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect())`));
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  await pg.cmd("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
};
const ready = (pg) => until(() => pg.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
const type = (pg, s, text) => pg.evalIn(`terms.get(${JSON.stringify(s)}).term.input(${JSON.stringify(text)})`);
/// Waits for `re` in session `s`'s screen and returns its match, or null.
const waitFor = async (pg, s, re, secs, label) => {
  let m = null;
  await until(async () => { m = (await pg.evalIn(`__txt(${JSON.stringify(s)})`)).match(re); return !!m; }, secs, label);
  return m;
};
const findTab = async (needle) => {
  let t = null;
  await until(async () => {
    const l = await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();
    t = l.find((x) => x.type === "page" && x.url.includes(needle)) || null;
    return !!t;
  }, 30, `a tab on ${needle}`);
  return t;
};
const timings = {};
/// Opens a terminal with the pane's own + and returns its name once attached.
const openTerm = async (pg) => {
  const before = JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`));
  await pg.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`)).some((s) => !before.includes(s)), 20, "a new terminal");
  const s = JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`)).find((x) => !before.includes(x));
  if (!s) return null;
  await until(() => pg.evalIn(`terms.has(${JSON.stringify(s)})`), 30, "attached");
  return s;
};

try {
  // Inside the try, so `fx.cleanup` runs if it fails.
  let cc;
  try {
    cc = await new Deno.Command("cc", { args: ["-O0", "-o", `${fakebin}/claude`, `${fx.base}/fake.c`], stdout: "inherit", stderr: "inherit" }).output();
  } catch (e) {
    throw new Error(`cc not found: the fake claude is compiled from C and needs a C compiler on PATH (${e.message})`);
  }
  if (!cc.success) throw new Error("cc failed to compile the fake claude");
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shell } });
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  ok(await ready(page), "main page loaded");
  await page.evalIn(helpers);

  // ------------------------------------------------------------------ A
  let tA = Date.now();
  console.log(`A. approve: the hook asks, a click creates, the shell follows (${stamp()})`);
  // A terminal of our own: `default_layout`'s seeded "term" tab is a
  // placeholder that never attaches until someone presses Enter in it.
  const sA = await openTerm(page);
  const SA = JSON.stringify(sA);
  ok(!!sA && await page.evalIn(`__sessions(3).includes(${SA}) && !!__tabOf(${SA})`),
     `the main page shows its terminal tab \`${sA}\` before the move`);
  ok(!!await waitFor(page, sA, /\$\s*$/m, 30, "a prompt"), "the shell is at a prompt");
  await type(page, sA, "echo PRE$((6*7))MOVE\r");
  ok(!!await waitFor(page, sA, /^PRE42MOVE$/m, 10, "the marker"), "a scrollback marker printed before the move");
  await type(page, sA, "claude\r");
  ok(!!await waitFor(page, sA, /FAKE-READY/, 10, "the fake"), "the fake claude is running");
  await type(page, sA, "pid a1\r");
  const pidBefore = (await waitFor(page, sA, /^PID a1 (\d+)/m, 10, "pid a1"))?.[1];
  ok(!!pidBefore, `the fake's pid before the move (${pidBefore})`);
  await type(page, sA, "worktree fox\r");
  ok(await until(() => page.evalIn(`document.getElementById("dlg-choice").open
      && document.querySelector('#dlg-choice .dlg-title').textContent === ${JSON.stringify(`Claude in ${sA} wants a worktree`)}
      && document.querySelector('#dlg-choice .dlg-body').textContent.includes("worktree-fox")`), 15, "the ask dialog"),
     "the ask appears as an in-page dialog naming the terminal and worktree-fox");
  await clickReal(page, '#dlg-choice .dlg-choice[data-choice="create"]');
  // Read on the main page first: the Rehomed banner lives 8 s.
  ok(await until(() => page.evalIn(`[...document.querySelectorAll(".error-banner b")].some((b) => b.textContent.includes(${JSON.stringify(sA + " moved to worktree-fox")}))`), 15, "the moved-to notice"),
     `the main page shows \`${sA} moved to worktree-fox\``);
  ok(await until(() => page.evalIn(`!__sessions(3).includes(${SA}) && !__tabOf(${SA})`), 15, "the tab to go"),
     `and its \`${sA}\` tab is gone`);
  const tgt = await findTab("/.claude/worktrees/worktree-fox");
  ok(!!tgt && new URL(tgt.url).pathname.endsWith(`/${project}/.claude/worktrees/worktree-fox`),
     `a new page opened at …/.claude/worktrees/worktree-fox (${tgt?.url})`);
  const wtList = (await git("worktree", "list", "--porcelain")).out;
  ok(wtList.includes(`${projDir}/.claude/worktrees/worktree-fox`), "git lists worktree-fox");
  if (tgt) {
    page2 = await attachTarget(tgt.webSocketDebuggerUrl);
    ok(await ready(page2), "worktree page loaded");
    await page2.evalIn(helpers);
    ok(await until(() => page2.evalIn(`terms.has(${SA}) && __tabOf(${SA})?.home === ${JSON.stringify(project)}`), 30, "the resident"),
       `the worktree page shows \`${sA}\` as a resident (home = the main project)`);
    ok(!!await waitFor(page2, sA, /^PRE42MOVE$/m, 20, "the replayed marker"),
       "its scrollback carries the marker typed before the move");
    const hp = await waitFor(page2, sA, /^HOOK-PATH fox (\S+)/m, 20, "the hook's path");
    ok(!!hp && hp[1].endsWith("/.claude/worktrees/worktree-fox"), `the hook printed the worktree path (${hp?.[1]})`);
    await type(page2, sA, "pid a2\r");
    const pidAfter = (await waitFor(page2, sA, /^PID a2 (\d+)/m, 10, "pid a2"))?.[1];
    ok(!!pidAfter && pidAfter === pidBefore, `the same process answers after the move (${pidBefore} → ${pidAfter})`);
    await type(page2, sA, "pwd a3\r");
    const cwd = (await waitFor(page2, sA, /^CWD a3 (\S+)/m, 10, "pwd a3"))?.[1];
    ok(!!cwd && cwd.endsWith(`/${project}/.claude/worktrees/worktree-fox`), `and it is in the worktree (${cwd})`);
  }
  timings.A = Date.now() - tA;

  // ------------------------------------------------------------------ B
  let tB = Date.now();
  console.log(`\nB. Close Project on main spares the resident (${stamp()})`);
  await page.bringToFront();
  // A native terminal beside the departed one, so the close is seen to end
  // something: without it, a Close Project that did nothing at all would
  // pass the "the resident survived" half of this section.
  const sN = await openTerm(page);
  const sockOf = (n) => `${fx.stateDir}/sock/${project}/${n}`;
  const present = async (path) => { try { await Deno.lstat(path); return true; } catch { return false; } };
  ok(!!sN && await until(() => present(sockOf(sN)), 20, "its socket"), `main also has a native shell, ${sN}, with a socket`);
  ok(await present(sockOf(sA)), `the resident ${sA}'s socket still lives under main (its home)`);
  await page.evalIn(`document.getElementById("closeproj").click(); 0`);
  ok(await until(() => page.evalIn(`document.getElementById("dlg-confirm").open`), 10, "the confirmation"), "Close Project asks first");
  await page.evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  ok(await until(async () => !(await present(sockOf(sN))), 30, "the native socket to go"), `the close ended the native ${sN}`);
  if (page2) {
    await page2.bringToFront();
    await type(page2, sA, "pid b1\r").catch(() => {});
    const pidB = (await waitFor(page2, sA, /^PID b1 (\d+)/m, 10, "pid b1"))?.[1];
    ok(!!pidB && pidB === pidBefore, `the worktree page's terminal still answers, same process (${pidB})`);
    await type(page2, sA, "pwd b2\r").catch(() => {});
    ok(!!await waitFor(page2, sA, /^CWD b2 \S+\/worktree-fox$/m, 10, "pwd b2"), "…and still in worktree-fox");
  }
  timings.B = Date.now() - tB;

  // ------------------------------------------------------------------ E
  let tE = Date.now();
  console.log(`\nE. + → fork: the new page types claude --resume <id> --fork-session (${stamp()})`);
  // A close ends by navigating to `/`, and refuses new terminals for its
  // settle window (closeproject.mjs, section B); reopen the way a user would.
  await sleep(1500);
  await page.evalIn(`location.href = ${JSON.stringify(`http://127.0.0.1:${roost.port}/${project}`)}; 0`);
  ok(await until(() => page.evalIn(`location.pathname === ${JSON.stringify(`/${project}`)}`), 15, "the project page")
     && await ready(page), "main page reopened");
  await page.evalIn(helpers);
  await page.bringToFront();
  const sE = await openTerm(page);
  const SE = JSON.stringify(sE);
  ok(!!sE && sE !== sA, `a fresh native terminal on main (${sE})`);
  ok(!!await waitFor(page, sE, /\$\s*$/m, 30, "a prompt"), "it is attached and at a prompt");
  // Written only now, after the page loaded and the terminal attached, so
  // no subscribe-time snapshot can have carried it.
  const forkId = "0f0e0d0c-1111-4222-8333-944445555666";
  await Deno.mkdir(`${fx.stateDir}/claude/${project}`, { recursive: true });
  await Deno.writeTextFile(`${fx.stateDir}/claude/${project}/${sE}.tmp`, JSON.stringify({ session_id: forkId, event: "SessionStart" }));
  await Deno.rename(`${fx.stateDir}/claude/${project}/${sE}.tmp`, `${fx.stateDir}/claude/${project}/${sE}.json`);
  await page.evalIn(`document.getElementById("wtbtn").click(); 0`);
  ok(await until(() => page.evalIn(`!!document.querySelector("#wtstrip .wtadd")`), 15, "the + button"), "the worktree panel offers +");
  // Read last thing before the + click, after the panel's own fetches: from
  // here on only the dialog's RequestState can change the answer.
  ok(!(await page.evalIn(`(state.forkable || []).includes(${SE})`)), "the page's current snapshot does not yet offer it as forkable");
  await page.evalIn(`document.querySelector("#wtstrip .wtadd").click(); 0`);
  const forkSel = `#dlg-choice .dlg-choice[data-choice="fork:${sE}"]`;
  const offered = await until(() => page.evalIn(`document.getElementById("dlg-choice").open
      && document.querySelector('#dlg-choice .dlg-title').textContent === "New worktree"
      && !!document.querySelector(${JSON.stringify(forkSel)})`), 15, "the fork choice");
  ok(offered, `the + dialog offers "Continue ${sE}'s Claude there (fork)" — from its own RequestState`);
  if (!offered) {
    // Dismissed with a real Escape (README: a bare .close() leaves dialog.js's
    // one-at-a-time gate shut), so C still gets to show its ask.
    for (const type of ["keyDown", "keyUp"]) {
      await page.cmd("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    }
  } else {
  await page.evalIn(`(() => { const i = document.querySelector("#dlg-choice .dlg-choice-input"); i.value = "wolf"; i.dispatchEvent(new Event("input")); })()`);
  await clickReal(page, forkSel);
  const tgtE = await findTab("/.claude/worktrees/worktree-wolf");
  ok(!!tgtE, `a new page opened on worktree-wolf (${tgtE?.url})`);
  if (tgtE) {
    page3 = await attachTarget(tgtE.webSocketDebuggerUrl);
    ok(await ready(page3), "worktree-wolf page loaded");
    await page3.evalIn(helpers);
    const typed = `claude --resume ${forkId} --fork-session`;
    let where = null;
    const seen = await until(async () => {
      for (const s of JSON.parse(await page3.evalIn(`JSON.stringify([...terms.keys()])`))) {
        if ((await page3.evalIn(`__txt(${JSON.stringify(s)})`)).includes(typed)) { where = s; return true; }
      }
      return false;
    }, 40, "the typed fork line");
    ok(seen, `its terminal shows \`${typed}\` typed (in ${where})`);
    // The fake then actually ran with those arguments.
    if (where) ok(!!await waitFor(page3, where, new RegExp(`FAKE-READY argv=--resume ${forkId} --fork-session`), 20, "fake argv"),
      "and the claude it started received exactly those arguments");
  }
  }
  timings.E = Date.now() - tE;

  // ------------------------------------------------------------------ C
  let tC = Date.now();
  console.log(`\nC. nobody clicks: the hook is refused at 60 s and nothing is created (${stamp()})`);
  await page.bringToFront();
  await type(page, sE, "claude\r");
  ok(!!await waitFor(page, sE, /FAKE-READY argv=\s*$/m, 10, "the fake"), "the fake claude runs in main's terminal");
  const askedAt = Date.now();
  await type(page, sE, "worktree slow\r");
  ok(await until(() => page.evalIn(`document.getElementById("dlg-choice").open
      && document.querySelector('#dlg-choice .dlg-body').textContent.includes("worktree-slow")`), 15, "the ask dialog"),
     "the ask for worktree-slow is shown (and left unanswered)");
  const refused = await waitFor(page, sE, /^HOOK-REFUSED slow (.*)$/m, 80, "the refusal");
  const waited = Date.now() - askedAt;
  ok(!!refused && refused[1].includes("within 60 s"), `the fake prints the refusal: ${refused?.[1]}`);
  ok(waited >= 59_000, `…and only after the 60 s ran out (${(waited / 1000).toFixed(1)} s)`);
  ok(await until(() => page.evalIn(`!document.getElementById("dlg-choice").open`), 10, "the dialog to close"),
     "the expired ask's dialog was withdrawn");
  const wtC = (await git("worktree", "list", "--porcelain")).out;
  ok(wtC.includes("/.claude/worktrees/worktree-fox") && !wtC.includes("worktree-slow"),
     "git worktree list shows worktree-fox but no worktree-slow");
  ok(!(await git("branch", "--list", "worktree-slow")).out.trim(), "and there is no worktree-slow branch");
  timings.C = Date.now() - tC;

  // ------------------------------------------------------------------ D
  let tD = Date.now();
  console.log(`\nD. no browser: refused at once (${stamp()})`);
  page?.close(); page2?.close(); page3?.close();
  page = page2 = page3 = null;
  for (const t of await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json()) {
    if (t.type === "page") await fetch(`http://127.0.0.1:${browser.port}/json/close/${t.id}`);
  }
  ok(await until(async () => !(await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json()).some((t) => t.type === "page"), 10, "no pages"),
     "every browser page is closed");
  await sleep(2000);
  const sock = `${fx.stateDir}/sock/${project}/${sE}`;
  const push = async (text) => {
    const p = new Deno.Command("dtach", { args: ["-p", sock], stdin: "piped", stdout: "null", stderr: "piped" }).spawn();
    const w = p.stdin.getWriter(); await w.write(enc.encode(text)); await w.close();
    const o = await p.output();
    return o.success ? null : dec.decode(o.stderr);
  };
  const logHas = async (re) => { let m = null; await until(async () => { m = (await Deno.readTextFile(fakeLog)).match(re); return !!m; }, 10, String(re)); return m; };
  const perr = await push("pid d1\n");
  ok(perr === null, `dtach -p reaches the session's socket${perr ? `: ${perr}` : ""}`);
  ok(!!await logHas(/^PID d1 \d+$/m), "the fake answers through it with no page open");
  const pushedAt = Date.now();
  await push("worktree lonely\n");
  const r = await logHas(/^HOOK-REFUSED lonely (.*)$/m);
  const ms = Number((await logHas(/^HOOK-MS lonely (\d+)$/m))?.[1]);
  ok(!!r && r[1].includes("no roost tab is open"), `refused: ${r?.[1]}`);
  ok(Number.isFinite(ms) && ms < 2000, `in under 2 s (hook took ${ms} ms; ${Date.now() - pushedAt} ms push to log)`);
  ok(!(await git("worktree", "list", "--porcelain")).out.includes("worktree-lonely"), "and no worktree-lonely was created");
  timings.D = Date.now() - tD;
} finally {
  page?.close(); page2?.close(); page3?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(`\ntimings (ms): ${JSON.stringify(timings)}, total ${stamp()}`);
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
