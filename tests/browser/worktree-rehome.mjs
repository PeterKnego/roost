//! Claude's `WorktreeCreate` hook, end to end: ask, approve, re-home, close
//! and fork (#156) — and the worktrees roost did not make (#158): the tab
//! moves only when its Claude does, a plain `git worktree add` is announced,
//! a terminal link into another project offers it, and a renamed worktree
//! takes its terminals with it.
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
//!   `worktree-nochdir <name>` is the same hook, but on exit 0 it prints
//!     `STAYED <name> <cwd>` and does not `chdir` — what a subagent's
//!     `isolation: "worktree"` looks like from outside: the ask is
//!     identical, and the parent Claude never moves.
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
//! Scenario order is A (with A′), F, B, E, C, G, H, D. A′ needs A's main
//! page and its banner record, which also holds F's announcement beside A's
//! roost-made worktrees; B needs A's re-homed shell, E needs a main-page
//! terminal B did not end, C reuses E's terminal to run the fake again; G
//! links into the `sib` worktree F created, and H renames the `sib` page G
//! opened; D closes every page, so it comes last.
//!
//! A no longer expects the move at approval (#158 §1). Approving leaves an
//! expectation; the claudes watcher, one `/proc` pass every 3 s, re-homes
//! the tab only once it sees that terminal's Claude inside the worktree. So
//! A's clock starts at `HOOK-PATH` (printed just before the `chdir`) and
//! allows 6 s, two ticks. A′ runs `worktree-nochdir` and watches the main
//! page for 35 s — past the expectation's 30 s life.
//!
//! Banners live 8 s, so each page keeps a record of every banner it has
//! shown (`__banners`, a MutationObserver installed with the helpers) and
//! absences are read off that record, not off a snapshot a banner could
//! have come and gone between.
//!
//! Every absence asserted here is paired with a positive check that the
//! thing would be visible if present: the main page's tab for the moved
//! terminal (the one `openTerm` returns, `term1` — not the seeded `term`,
//! which never attaches) is asserted present before the move; `worktree-slow`'s absence from `git worktree
//! list` beside `worktree-fox`'s presence; D's channel is proved with a
//! `pid` answer before the refused request is sent; and E's fork choice is
//! asserted *absent* from the snapshot the page already held, so the
//! dialog's own `RequestState` round trip is what makes it appear. A′'s
//! "no `term2 moved to` notice" is read from the same record that holds
//! A's `term1 moved to worktree-fox`, and its "worktree-owl's page has no
//! `term2`" is the same `__tabOf` check that found `term1` on
//! worktree-fox's page. F's "fox and owl were never announced" sits beside
//! sib's announcement on that same record. G's "main did not open its own
//! hello.md" is read before and after the click, and F proved that a plain
//! `/sib` page has no hello.md tab, so G's `?open=` is what opened it there.
//! H reads the followed terminal only once the page really is `/sib2`: the
//! old page still shows the same session, so reading it there would pass
//! with nothing having followed.
//!
//! H's sweep is one load of `/` in a real tab, after the 3 s throttle. A
//! bare `fetch` of `/` sweeps nothing — the page is a static shell and the
//! sweep is its project list's fragment — which is how H first failed.
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
//!   #158 Task 4's change reverted: `approve_ask` (src/hub.rs) re-homes at
//!   approval again — `let _ = crate::home::record(&project, &session,
//!   &owner);` added right after `expect_move` — binary rebuilt. Observed:
//!   F, B, E, C, G, H and D green; A and A′ fail 6 —
//!     (timed out waiting for the hook's path on main)
//!     FAIL  the hook answered with a path (read on main)
//!     (timed out waiting for the hook's answer)
//!     FAIL  the hook answered and the fake stayed where it was
//!     FAIL  for 35 s the main page kept `term2`'s tab (gone at 0.1 s)
//!     FAIL  and the worktree-owl page has no `term2` (the same check that found `term1` on worktree-fox's)
//!     FAIL  no `term2 moved to` notice was ever shown — where `term1`'s was
//!     FAIL  the main page's terminal still answers, same process (… → undefined)
//!   The no-chdir half is the subject: the tab left at approval, before the
//!   hook had even answered. The two "hook answered" failures are the same
//!   move seen earlier — the tab is gone from main before the reply prints
//!   there. Restored with `cp`: 86 ok, 0 FAIL.
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
    else if (!strcmp(cmd, "worktree") || !strcmp(cmd, "worktree-nochdir")) {
      int stay = !strcmp(cmd, "worktree-nochdir");
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
        if (stay) say("STAYED %s %s", arg, cwd);
        else if (chdir(o)) say("CHDIR-FAILED %s %s", arg, o);
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
let page, page2, page3, pS, roost;
// Joins soft-wrapped rows (xterm's isWrapped), so a long path is one line.
const helpers = `window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
    for (let i = 0; i < b.length; i++) { o += b.getLine(i).translateToString(true); const n = b.getLine(i + 1); if (!n || !n.isWrapped) o += "\\n"; } return o; };
  window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);
  window.__tabOf = (s) => state.panes.flatMap((p) => p.tabs).find((t) => t.k === "Terminal" && t.session === s) || null;
  // Every banner this page has shown, kept after its 8 s life ends, so an
  // absence over a 35 s window is read off a record rather than a snapshot
  // that a banner could have come and gone between.
  if (!window.__banners) {
    window.__banners = [];
    const seen = new WeakMap();
    // The appeared banners (#188) are not \`.error-banner\`s: they live in
    // their own \`#appeared\` container and stay until dismissed. Both are
    // recorded, so one record answers "was it ever shown" for either kind.
    const rec = () => { for (const el of document.querySelectorAll(".error-banner, #appeared .appeared-banner")) {
      let r = seen.get(el); if (!r) { r = {}; seen.set(el, r); window.__banners.push(r); }
      r.text = el.querySelector("b")?.textContent || ""; r.count = el.dataset.count || "1";
      r.hrefs = [...el.querySelectorAll("a")].map((a) => a.getAttribute("href")); } };
    new MutationObserver(rec).observe(document.body, { childList: true, subtree: true, characterData: true });
  }
  window.__bannersWith = (sub) => window.__banners.filter((r) => r.text.includes(sub));`;
const bannersWith = async (pg, sub) => JSON.parse(await pg.evalIn(`JSON.stringify(__bannersWith(${JSON.stringify(sub)}))`));
// The first of the element's own boxes, not its bounding rect: a link that
// wraps across two lines in a banner has a bounding rect whose centre is the
// banner's padding, and a click there goes nowhere (H's link did exactly that).
const clickReal = async (pg, selector) => {
  const r = JSON.parse(await pg.evalIn(`JSON.stringify(document.querySelector(${JSON.stringify(selector)}).getClientRects()[0])`));
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  await pg.cmd("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
};
/// termlinks.mjs's clickLink, modifier always held: hover with Control down
/// so xterm's linkifier captures the link, then a modifier-held press and
/// release at the same point, which is what xterm activates on. The row is
/// matched whole and bottom-up, so the echoed command line is never it.
const clickLink = async (pg, needle, session) => {
  const seat = await pg.evalIn(`(() => {
    const entry = terms.get(${JSON.stringify(session)});
    if (!entry) return null;
    const rows = [...document.querySelectorAll(".xterm-rows div")].filter((x) => entry.node.contains(x));
    const n = rows.filter((x) => x.textContent.trim() === ${JSON.stringify(needle)}).pop();
    if (!n) return null;
    const b = n.getBoundingClientRect();
    const scr = entry.node.querySelector(".xterm-screen").getBoundingClientRect();
    return { x: Math.round(scr.left + 4.5 * scr.width / entry.term.cols), y: Math.round(b.top + b.height / 2) };
  })()`);
  if (!seat) return false;
  const ctrl = (type, m) => pg.cmd("Input.dispatchKeyEvent", { type, key: "Control", code: "ControlLeft", windowsVirtualKeyCode: 17, nativeVirtualKeyCode: 17, modifiers: m });
  await ctrl("rawKeyDown", 2); await sleep(150);
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: seat.x, y: seat.y, buttons: 0 });
  await sleep(200);
  await pg.cmd("Input.dispatchMouseEvent", { type: "mousePressed", x: seat.x, y: seat.y, button: "left", buttons: 1, clickCount: 1, modifiers: 2 });
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x: seat.x, y: seat.y, button: "left", buttons: 0, clickCount: 1, modifiers: 2 });
  await sleep(200);
  await ctrl("keyUp", 0); await sleep(150);
  return true;
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
/// A browser tab whose path is exactly `path` (query ignored, since a page
/// opened with `?open=` strips it as soon as it has sent it).
const findTabAt = async (path) => {
  let t = null;
  await until(async () => {
    const l = await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();
    t = l.find((x) => x.type === "page" && (() => { try { return new URL(x.url).pathname === path; } catch { return false; } })()) || null;
    return !!t;
  }, 30, `a tab at ${path}`);
  return t;
};
const closeTab = async (t) => { if (t) await fetch(`http://127.0.0.1:${browser.port}/json/close/${t.id}`); };
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
  // Approval no longer moves anything (§1): the hook answers, the fake
  // `chdir`s, and only then can the claudes watcher — one `/proc` pass every
  // 3 s — see this Claude inside the worktree and re-home the tab. So the
  // clock starts at HOOK-PATH, which the fake prints immediately before its
  // `chdir`, and the bound is two ticks.
  ok(!!await waitFor(page, sA, /^HOOK-PATH fox \S+/m, 20, "the hook's path on main"), "the hook answered with a path (read on main)");
  const hookAt = Date.now();
  ok(await until(() => page.evalIn(`!__sessions(3).includes(${SA}) && !__tabOf(${SA})`), 6, "the tab to go"),
     `within 6 s of the chdir the main page's \`${sA}\` tab is gone (${((Date.now() - hookAt) / 1000).toFixed(1)} s)`);
  ok(await until(async () => (await bannersWith(page, `${sA} moved to worktree-fox`)).length > 0, 5, "the moved-to notice"),
     `the main page shows \`${sA} moved to worktree-fox\``);
  const tgt = await findTab("/.claude/worktrees/worktree-fox");
  ok(!!tgt && new URL(tgt.url).pathname.endsWith(`/${project}/.claude/worktrees/worktree-fox`),
     `a new page opened at …/.claude/worktrees/worktree-fox (${tgt?.url})`);
  const wtList = (await git("worktree", "list", "--porcelain")).out;
  ok(wtList.includes(`${projDir}/.claude/worktrees/worktree-fox`), "git lists worktree-fox");
  if (tgt) {
    page2 = await attachTarget(tgt.webSocketDebuggerUrl);
    ok(await ready(page2), "worktree page loaded");
    await page2.evalIn(helpers);
    const left = Math.max(0.5, 6 - (Date.now() - hookAt) / 1000);
    ok(await until(() => page2.evalIn(`terms.has(${SA}) && __tabOf(${SA})?.home === ${JSON.stringify(project)}`), left, "the resident"),
       `within 6 s of the chdir the worktree page shows \`${sA}\` as a resident, home = main (${((Date.now() - hookAt) / 1000).toFixed(1)} s)`);
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

  // A second terminal, and this time the Claude does not follow its own
  // worktree — which is what a subagent's `isolation: "worktree"` looks like
  // from here: the same ask, the same approval, and the parent stays put.
  console.log(`A′. approve, but the Claude never chdirs: the tab stays (${stamp()})`);
  await page.bringToFront();
  const sO = await openTerm(page);
  const SO = JSON.stringify(sO);
  ok(!!sO && sO !== sA && await page.evalIn(`!!__tabOf(${SO})`), `a second main terminal, \`${sO}\``);
  ok(!!await waitFor(page, sO, /\$\s*$/m, 30, "a prompt"), "it is at a prompt");
  await type(page, sO, "claude\r");
  ok(!!await waitFor(page, sO, /FAKE-READY/, 10, "the fake"), "the fake claude is running in it");
  await type(page, sO, "pid o1\r");
  const pidO = (await waitFor(page, sO, /^PID o1 (\d+)/m, 10, "pid o1"))?.[1];
  await type(page, sO, "worktree-nochdir owl\r");
  ok(await until(() => page.evalIn(`document.getElementById("dlg-choice").open
      && document.querySelector('#dlg-choice .dlg-title').textContent === ${JSON.stringify(`Claude in ${sO} wants a worktree`)}
      && document.querySelector('#dlg-choice .dlg-body').textContent.includes("worktree-owl")`), 15, "the owl ask"),
     "the ask for worktree-owl appears");
  await clickReal(page, '#dlg-choice .dlg-choice[data-choice="create"]');
  ok(!!await waitFor(page, sO, /^STAYED owl \S+/m, 20, "the hook's answer"), "the hook answered and the fake stayed where it was");
  const owlAt = Date.now();
  const tgtO = await findTab("/.claude/worktrees/worktree-owl");
  let pageO = null;
  if (tgtO) {
    pageO = await attachTarget(tgtO.webSocketDebuggerUrl);
    ok(await ready(pageO), "the worktree-owl page opened and loaded");
    await pageO.evalIn(helpers);
  } else ok(false, "the worktree-owl page opened");
  // Polled the whole window, not read once at the end: the expectation is
  // live for 30 s, so a wrong move could happen at any tick of it.
  await page.bringToFront();
  let leftAt = null;
  while (Date.now() - owlAt < 35_000) {
    if (leftAt === null && !(await page.evalIn(`__sessions(3).includes(${SO}) && !!__tabOf(${SO})`))) leftAt = Date.now() - owlAt;
    await sleep(500);
  }
  ok(leftAt === null, `for 35 s the main page kept \`${sO}\`'s tab${leftAt === null ? "" : ` (gone at ${(leftAt / 1000).toFixed(1)} s)`}`);
  if (pageO) ok(await pageO.evalIn(`!__tabOf(${SO}) && !terms.has(${SO}) && Array.isArray(state.panes)`),
     `and the worktree-owl page has no \`${sO}\` (the same check that found \`${sA}\` on worktree-fox's)`);
  ok((await bannersWith(page, `${sO} moved to`)).length === 0
     && (await bannersWith(page, `${sA} moved to worktree-fox`)).length > 0,
     `no \`${sO} moved to\` notice was ever shown — where \`${sA}\`'s was`);
  // Tolerant, as B's are: a tab that wrongly moved has no terminal here,
  // and that is reported by the waitFor below, not by a crash.
  await type(page, sO, "pid o2\r").catch(() => {});
  const pidO2 = (await waitFor(page, sO, /^PID o2 (\d+)/m, 10, "pid o2"))?.[1];
  ok(!!pidO && pidO2 === pidO, `the main page's terminal still answers, same process (${pidO} → ${pidO2})`);
  ok((await git("worktree", "list", "--porcelain")).out.includes(`${projDir}/.claude/worktrees/worktree-owl`), "and git lists worktree-owl");
  await closeTab(tgtO); pageO?.close();
  timings.A = Date.now() - tA;

  // ------------------------------------------------------------------ F
  let tF = Date.now();
  console.log(`\nF. a plain \`git worktree add\` is announced once, with a link (${stamp()})`);
  // The positive this section pairs with A's absences: a worktree roost did
  // NOT make. Run from Deno, nothing roost is told about.
  const wtAdd = await git("worktree", "add", "-q", "../sib", "-b", "sib");
  ok(wtAdd.ok, `git worktree add ../sib -b sib ${wtAdd.ok ? "" : wtAdd.err}`);
  const sibText = "worktree sib (sib) appeared";
  ok(await until(async () => (await bannersWith(page, sibText)).length > 0, 15, "the appeared banner"),
     `the main page shows \`${sibText} — open\``);
  await sleep(4000);
  const sibB = await bannersWith(page, sibText);
  ok(sibB.length === 1 && sibB[0].count === "1", `exactly once (${JSON.stringify(sibB)})`);
  ok(sibB[0]?.hrefs?.length === 1 && sibB[0].hrefs[0] === "/sib", `its link is /sib (${sibB[0]?.hrefs})`);
  ok((await bannersWith(page, "appeared")).every((r) => r.text.startsWith(sibText)),
     "and worktree-fox and worktree-owl, which roost made, were never announced");
  // A working link: click it for real (target=_blank, a user gesture) and
  // the page it opens is sib's project page. It also proves that a plain
  // /sib page holds no hello.md tab, which G's ?open= is then measured
  // against.
  let sibTab = null;
  const sibBox = `[...document.querySelectorAll("#appeared .appeared-banner")].find((e) => (e.querySelector("b")?.textContent || "").startsWith(${JSON.stringify(sibText)}))`;
  if (await page.evalIn(`!!${sibBox}`)) {
    await page.evalIn(`${sibBox}.querySelector("a").setAttribute("data-t9", "sib"); 0`);
    await clickReal(page, `a[data-t9="sib"]`);
    sibTab = await findTabAt("/sib");
  }
  ok(!!sibTab, `clicking it opens a tab at /sib (${sibTab?.url})`);
  if (sibTab) {
    const ps = await attachTarget(sibTab.webSocketDebuggerUrl);
    ok(await ready(ps) && await ps.evalIn(`PROJECT === "sib"`), "which loads sib's own project page");
    ok(!(await ps.evalIn(`state.panes.some((p) => p.tabs.some((t) => t.k === "File" && t.rel === "hello.md"))`)),
       "with no hello.md tab of its own");
    ps.close(); await closeTab(sibTab);
  }
  timings.F = Date.now() - tF;

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

  // ------------------------------------------------------------------ G
  let tG = Date.now();
  console.log(`\nG. a terminal link into another project offers it (${stamp()})`);
  await page.bringToFront();
  // The fixture's absolute path is long (it lives under $TMPDIR) and the
  // terminal pane is a fixed 520 px column, so the path would wrap, and
  // clickLink matches a row whole. A wide window, and the column widened on
  // #grid itself — state renders set --right-w on <html>, which an inline
  // value on #grid outranks — keep it one row.
  await page.cmd("Emulation.setDeviceMetricsOverride", { width: 2400, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.evalIn(`document.getElementById("grid").style.setProperty("--right-w", "1500px"); 0`);
  const sG = await openTerm(page);
  const SG = JSON.stringify(sG);
  ok(!!sG && !!await waitFor(page, sG, /\$\s*$/m, 30, "a prompt"), `a shell on main (${sG})`);
  const absQ = `${fx.roots}/sib/hello.md`;
  await type(page, sG, `echo ${absQ}\r`);
  // The output row is the path alone; the command row has `$ echo` before it.
  ok(!!await waitFor(page, sG, new RegExp(`^${absQ.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m"), 10, "the printed path"),
     `the terminal shows sib's ${absQ}`);
  const mainHello = `state.panes.some((p) => p.tabs.some((t) => t.k === "File" && t.rel === "hello.md"))`;
  ok(!(await page.evalIn(mainHello)), "main has no hello.md tab of its own before the click");
  const cols = await page.evalIn(`terms.get(${SG}).term.cols`);
  const clicked = await clickLink(page, absQ, sG);
  ok(clicked, `the path's row was found whole and modifier-clicked (${absQ.length} chars, ${cols} columns)`);
  const elseText = "That's in sib — open";
  ok(await until(async () => (await bannersWith(page, elseText)).length > 0, 10, "That's in sib"),
     `the main page shows \`${elseText}\``);
  const elseB = (await bannersWith(page, elseText))[0];
  ok(elseB?.hrefs?.length === 1 && elseB.hrefs[0] === "/sib?open=hello.md", `its link is /sib?open=hello.md (${elseB?.hrefs})`);
  // The same click again: the identical text dedupes into the same banner,
  // whose count update used to rewrite the line and delete its link — and
  // whose new link went to whichever div came last on the page. Another
  // banner shown in between makes that a different banner. Revert-checked,
  // run and restored: with app.js's banner code as it was (count via
  // `b.textContent`, links via `.error-banner:last-of-type b`), fails "two
  // identical clicks leave one banner with one link ([[]])"; without the
  // banner in between, that old code passed.
  await page.evalIn(`showBanner("an unrelated banner"); 0`);
  ok(await clickLink(page, absQ, sG), "the same path clicked a second time");
  const elseLive = `[...document.querySelectorAll(".error-banner")].filter((e) => e.dataset.text === ${JSON.stringify("That's in sib — ")})`;
  ok(await until(() => page.evalIn(`${elseLive}.some((e) => e.dataset.count === "2")`), 10, "the deduped count"),
     "the second click is counted on the same banner");
  const twice = JSON.parse(await page.evalIn(`JSON.stringify(${elseLive}.map((e) => [...e.querySelectorAll("a")].map((a) => a.getAttribute("href"))))`));
  ok(twice.length === 1 && twice[0].length === 1 && twice[0][0] === "/sib?open=hello.md",
     `two identical clicks leave one banner with one link (${JSON.stringify(twice)})`);
  // Banners stack at one fixed spot: the unrelated one, newer, would sit on
  // top of the link the next step clicks.
  await page.evalIn(`document.querySelectorAll(".error-banner").forEach((e) => { if (e.dataset.text === "an unrelated banner") e.remove(); }); 0`);
  await sleep(1000);
  ok(!(await page.evalIn(mainHello)), "and main did not open its own hello.md instead");
  let sibTabG = null;
  const elseBox = `[...document.querySelectorAll(".error-banner")].find((e) => e.dataset.text === ${JSON.stringify("That's in sib — ")})`;
  if (await page.evalIn(`!!${elseBox}`)) {
    await page.evalIn(`${elseBox}.querySelector("a").setAttribute("data-t9", "else"); 0`);
    await clickReal(page, `a[data-t9="else"]`);
    sibTabG = await findTabAt("/sib");
  }
  ok(!!sibTabG, `clicking it opens a tab on sib (${sibTabG?.url})`);
  if (sibTabG) {
    pS = await attachTarget(sibTabG.webSocketDebuggerUrl);
    ok(await ready(pS) && await pS.evalIn(`PROJECT === "sib"`), "sib's page loaded");
    await pS.evalIn(helpers);
    // F proved a plain /sib page has no hello.md tab; this one is ?open='s.
    ok(await until(() => pS.evalIn(`state.panes.some((p) => { const t = p.tabs[p.active]; return t && t.k === "File" && t.rel === "hello.md"; })`), 15, "hello.md active"),
       "and hello.md is open there, the active tab of its pane");
  }
  timings.G = Date.now() - tG;

  // ------------------------------------------------------------------ H
  let tH = Date.now();
  console.log(`\nH. git worktree move: the terminal follows the rename (${stamp()})`);
  if (pS) {
    await pS.bringToFront();
    const sH = await openTerm(pS);
    const SH = JSON.stringify(sH);
    ok(!!sH && !!await waitFor(pS, sH, /\$\s*$/m, 30, "a prompt"), `a shell on sib's page (${sH})`);
    await type(pS, sH, `echo "SH-$((2+3))-$$"\r`);
    const pidH = (await waitFor(pS, sH, /^SH-5-(\d+)$/m, 10, "the shell's pid"))?.[1];
    ok(!!pidH, `its shell's pid (${pidH})`);
    const mv = await git("worktree", "move", `${fx.roots}/sib`, `${fx.roots}/sib2`);
    ok(mv.ok, `git worktree move sib sib2 ${mv.ok ? "" : mv.err}`);
    // One sweep: past the 3 s throttle, then one load of the front page in
    // a real tab. `/` itself is a static shell; the sweep is its project
    // list's fragment (`project_rows` → `reconcile_throttled`), which only a
    // page that runs its htmx fetches reaches — a bare fetch of `/` sweeps
    // nothing, which is how this section first failed.
    await sleep(3500);
    const front = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
    const movedText = "sib was renamed to sib2; its terminals followed";
    ok(await until(async () => (await bannersWith(pS, movedText)).length > 0, 10, "ProjectMoved"),
       `the old page shows \`${movedText}\``);
    const mvB = (await bannersWith(pS, movedText))[0];
    ok(mvB?.hrefs?.length === 1 && mvB.hrefs[0] === "/sib2", `with a link to /sib2 (${mvB?.hrefs})`);
    front.close();
    for (const t of await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json()) {
      if (t.type === "page" && new URL(t.url).pathname === "/") await closeTab(t);
    }
    const onMain = await bannersWith(page, "sib2");
    console.log(`    (main page's banners naming sib2: ${JSON.stringify(onMain.map((r) => r.text))})`);
    ok((await git("worktree", "list", "--porcelain")).out.includes(`${fx.roots}/sib2`), "git lists sib2");
    const movedBox = `[...document.querySelectorAll(".error-banner")].find((e) => e.dataset.text === ${JSON.stringify(movedText)})`;
    // Brought to the front first: the `/` tab opened above is the active one,
    // and a real click goes to the page on top.
    await pS.bringToFront();
    const boxLive = await pS.evalIn(`!!${movedBox}`);
    if (boxLive) {
      await pS.evalIn(`${movedBox}.querySelector("a").setAttribute("data-t9", "moved"); 0`);
      await clickReal(pS, `a[data-t9="moved"]`);
    } else console.log("    (the banner was already gone when its link was to be clicked)");
    const onNew = await until(() => pS.evalIn(`location.pathname === "/sib2"`), 15, "/sib2") && await ready(pS)
       && await pS.evalIn(`PROJECT === "sib2"`);
    ok(onNew, "following it loads sib2's page");
    // Only on sib2's page: the old page still shows the same session, so
    // reading it there would pass with nothing having followed.
    if (onNew) {
      await pS.evalIn(helpers);
      ok(await until(() => pS.evalIn(`terms.has(${SH}) && __tabOf(${SH})?.home === "sib"`), 20, "the followed terminal"),
         `sib2's page shows \`${sH}\`, as a resident whose home is still sib`);
      // `pwd -P`: bash's own $PWD is the name it cd'd by, which the rename
      // does not update; the kernel's cwd is the directory itself.
      await type(pS, sH, `echo "SH-$((3+4))-$$-$(pwd -P)"\r`);
      const after = await waitFor(pS, sH, /^SH-7-(\d+)-(\S+)$/m, 10, "the shell after the move");
      ok(!!after && after[1] === pidH, `the same shell answers there (${pidH} → ${after?.[1]})`);
      ok(!!after && after[2] === `${fx.roots}/sib2`, `and it is in the renamed directory (${after?.[2]})`);
    } else ok(false, "so the followed terminal could not be looked for there");
  } else ok(false, "H needs G's sib page");
  timings.H = Date.now() - tH;

  // ------------------------------------------------------------------ D
  let tD = Date.now();
  console.log(`\nD. no browser: refused at once (${stamp()})`);
  page?.close(); page2?.close(); page3?.close(); pS?.close();
  page = page2 = page3 = pS = null;
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
  page?.close(); page2?.close(); page3?.close(); pS?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(`\ntimings (ms): ${JSON.stringify(timings)}, total ${stamp()}`);
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
