//! ✻ in a project that already has a Claude: the prompt, the worktree, the tab.
//!
//! Only a real browser can show three of the things the spec promises: that
//! a second ✻ opens *nothing* (asserted on the State snapshot's tabs, never on
//! event order — client-visible ordering is pipelined per connection and was
//! proved non-discriminating once, see README trap 2); that "new worktree"
//! ends in a second browser tab on the worktree project whose first terminal
//! typed `claude --session-id …` by itself; and that the switcher's remove
//! control appears only once that terminal is gone, then removes the
//! directory and the branch.
//!
//! The click on "Start in a new worktree" is a real CDP mouse click, not
//! `element.click()` from Runtime.evaluate: `window.open` needs a user
//! gesture, and this is the same path a person's click takes.
//!
//! Fake `claude`, as in claudeterm.mjs, printing its argv so `--session-id`
//! is observable.
//!
//! Every pane 3 (`proto::RIGHT`) starts with one Terminal tab already —
//! `Workspace::default_layout` seeds it with a session literally named
//! `"term"` (see `workspace.rs`'s `default_layout_matches_the_spec`, and the
//! "default_layout seeds RIGHT with a Terminal tab already" comments in
//! `hub.rs`) — on both the original project and the freshly-created
//! worktree. Every session-count assertion below accounts for that fixed
//! extra tab rather than assuming a pane starts empty, and every "which
//! session is the new one" lookup excludes the literal name `"term"` rather
//! than taking `[0]`. An earlier draft of this file assumed an empty pane and
//! passed its first assertion for the wrong reason — `__sessions(3).length
//! === 1` was already true before the first click ever reached the server,
//! from the pre-existing "term" tab — and only cascaded into real failures
//! two steps later, which is exactly the "passes vacuously" trap CLAUDE.md
//! warns about.
//!
//! Section C's tab lookup also re-reads `/json/list` after `until` resolves,
//! rather than trusting a value out of the poll: `harness.mjs`'s `until`
//! reports only true/false, never what `fn()` computed, so treating its
//! return as the matching target crashed two lines later dereferencing
//! `.webSocketDebuggerUrl` off a bare `true`.
//!
//! Section E's "remove control" check reopens the panel in a loop rather
//! than once: ending the worktree's last live shell drops its live-shell
//! count to zero, which broadcasts `ProjectsChanged` to every open tab
//! (including this one), whose *pre-existing* `hx-trigger="… projects
//! from:body"` on `#wtstrip` answers with its own, non-stateful re-fetch —
//! racing the on-demand `state=1` fetch this task's reopen asks for.
//! Whichever response lands second wins the swap, so immediately after
//! `EndSession` a single reopen intermittently landed on the state-free
//! fragment instead (observed directly: `#wtstrip`'s HTML had no `.wtf`
//! spans at all, not merely ones reading "clean"/"—"). That race lives
//! entirely in wiring this task did not touch, so the fix belongs in the
//! test: reopen (the same real click a person re-glancing at the panel would
//! make) until the state-bearing response wins.
//!
//! Revert-checks (CLAUDE.md: "would this fail if I deleted the code it
//! covers?"), applied and watched failing, then restored:
//!   (a) removing `force: true` from `.wt-here`'s intent: section D's "force
//!       opens a second terminal in the original project" FAILED — the
//!       prompt reappeared instead (server saw a Claude already there and
//!       re-asked) and the session count stayed at 2 instead of reaching 3.
//!       Restored: passes again.
//!   (b) making `showClaudeHere` also call `newTerminal(pane, "claude")`:
//!       did NOT fail the assertion the brief predicted ("no terminal was
//!       opened"), and not by luck: the extra call carries no `force`, so
//!       the server intercepts it exactly like the click that triggered the
//!       prompt in the first place and answers with another `ClaudeHere` —
//!       which re-invokes `showClaudeHere`, which calls `newTerminal` again,
//!       forever. No terminal ever opens (every one of those requests is
//!       intercepted before it reaches `next_free_name`), so the session
//!       count never moves and that assertion is not the one that can see
//!       this bug. It also isn't reliably *any* assertion downstream: the
//!       tight request/response loop's only externally-visible effect is
//!       contention, so whether it starves some later poll enough to fail
//!       depends on host load at the time — watched once starving section
//!       E's "0 ahead" and "remove control" polls into a crash, and watched
//!       again passing everything with the bug still in place. A test that
//!       only sometimes catches a real bug is not a passing revert-check, so
//!       section B now wraps `window.send` (a plain top-level function in
//!       this non-module script, so it hangs off `window`) right after the
//!       prompt appears and counts `NewTerminal{launch:"claude"}` calls over
//!       the next 1.5s: 0 with the guard in place, and with it removed, this
//!       reliably shows dozens to low hundreds across repeated runs — FAILED
//!       every time it was tried. Restored: 0 again, passes.
//!   (c) dropping `history.replaceState` from the State-case launch consumer:
//!       section C's "the ?launch= parameter was consumed" FAILED —
//!       location.search stayed `?launch=claude`. Restored: passes again.
//!   (d) (fix round 1) re-wrapping the switcher's `document.body.dataset.key`
//!       in `encodeURIComponent` before sending it as `current=`: that key is
//!       already `percent_encode(key)` from the server (render.rs's `qkey`),
//!       so re-encoding it turns `outer%2Finner` into `outer%252Finner`,
//!       which the server's single `percent_decode` cannot recover — the
//!       fragment renders "no worktrees" for any project whose key needs
//!       encoding at all. Invisible to this file before this fix round: the
//!       fixture project was flat ("proj"), whose key needs no encoding.
//!       Switched the fixture to a nested project ("outer/inner", key
//!       `outer%2Finner`) so section E's fetch actually exercises this path.
//!       With the bug reintroduced: section E's "the worktree row shows 0
//!       ahead" FAILED deterministically (not a timing flake — the retry
//!       loop's 20 attempts over 20s all hit the broken URL), cascading into
//!       "the remove control appears" and a crash on the next line. Restored:
//!       passes again.
//!
//! Run: deno run -A tests/browser/worktree-launch.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();

const fx = await fixture();
const fakebin = `${fx.base}/fakebin`;
await Deno.mkdir(fakebin, { recursive: true });
// Stays running until stdin closes, so the terminal keeps "a launched Claude" alive.
await Deno.writeFile(`${fakebin}/claude`, enc.encode(`#!/bin/sh\necho "FAKE-CLAUDE-STARTED argv=$*"\ncat >/dev/null\n`), { mode: 0o755 });
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(`#!/bin/sh\nPATH=${JSON.stringify(`${fakebin}:/usr/bin:/bin`)}; export PATH\nexec /bin/bash --noprofile --norc "$@"\n`), { mode: 0o755 });
const git = async (dir, ...args) => {
  const o = await new Deno.Command("git", { args: ["-C", dir, ...args], stdout: "piped", stderr: "piped" }).output();
  return { ok: o.success, out: new TextDecoder().decode(o.stdout), err: new TextDecoder().decode(o.stderr) };
};
// A *nested* project, not fixture()'s flat "proj": its storage key
// (`projects::storage_key`) percent-encodes the '/' to `%2F`
// (`outer%2Finner`), which is exactly what section E's switcher fetch needs
// to catch a double-encoding regression — `document.body.dataset.key` is
// already that percent-encoded key, and wrapping it in encodeURIComponent a
// second time turns `%2F` into `%252F`, which the server's single
// percent_decode cannot recover. The flat "proj" fixture needs no encoding
// at all, so that bug was invisible to this file until now.
const project = "outer/inner";
const projDir = `${fx.roots}/${project}`;
await Deno.mkdir(projDir, { recursive: true });
await Deno.writeFile(`${projDir}/hello.md`, enc.encode("# hello\n"));
await new Deno.Command("git", { args: ["init", "-q"], cwd: projDir, stdout: "null", stderr: "null" }).output();
await git(projDir, "config", "user.email", "t@t"); await git(projDir, "config", "user.name", "t");
await git(projDir, "add", "."); await git(projDir, "commit", "-qm", "init");

const browser = await startBrowser(profileDir(repoRoot));
let page, page2, roost;
// __txt joins on a *hard* newline only (xterm's own isWrapped flag), not on
// every row: the fake claude's argv line is long enough to soft-wrap inside
// the right pane's narrow default width, and a plain per-row join broke the
// wrapped line in half between "argv=--session-id" and its uuid — a regex
// looking for both on one line then timed out for a reason that had nothing
// to do with what the assertion was checking.
const helpers = `window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
    for (let i = 0; i < b.length; i++) { o += b.getLine(i).translateToString(true); const n = b.getLine(i + 1); if (!n || !n.isWrapped) o += "\\n"; } return o; };
  window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);`;
const clickReal = async (pg, selector) => {
  const r = JSON.parse(await pg.evalIn(`JSON.stringify(document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect())`));
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  await pg.cmd("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await pg.cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
};

try {
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shell } });
  await until(async () => (await (await fetch(`http://127.0.0.1:${roost.port}/${project}`)).text()).includes('data-launches="claude"'), 15, "claude offered");
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(helpers);

  console.log("A. first ✻ types claude --session-id");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  // Pane 3 already carries the default "term" tab, so a successful click
  // brings the count to 2, not 1 — see the header comment above.
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 2, 20, "two terminals"), "a terminal opened");
  const first = JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).find((s) => s !== "term");
  await until(() => evalIn(`terms.has(${JSON.stringify(first)})`), 30, "attached");
  const started = await until(async () => /FAKE-CLAUDE-STARTED argv=--session-id [0-9a-f-]{36}/.test(await evalIn(`__txt(${JSON.stringify(first)})`)), 60, "claude with an id");
  ok(started, "claude was started with --session-id <uuid>");

  console.log("\nB. second ✻ asks instead of opening");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  ok(await until(() => evalIn(`!!document.querySelector('.pane[data-pane="3"] .claudehere')`), 10, "the prompt"), "the prompt appeared in the pane");
  // Wrap `send` (a plain top-level function in this non-module script, so it
  // hangs off `window` and is interceptable) to count anything the prompt
  // fires on its own, with no click. `showClaudeHere` calling `newTerminal`
  // itself, on the launch=claude/!force path, is answered with *another*
  // `ClaudeHere` rather than an open terminal — so that particular bug does
  // not show up as an extra session (the assertion right below stays green
  // either way) and can go unnoticed as an async request/response loop that
  // only shows up under load. Counting sends is what actually distinguishes
  // "asked once" from "kept asking".
  await evalIn(`window.__sendCount = 0; const _s = window.send; window.send = (i) => { if (i.t === "NewTerminal" && i.launch === "claude") window.__sendCount++; return _s(i); };`);
  await sleep(1500);
  ok(Number(await evalIn(`window.__sendCount`)) === 0, "the prompt sent nothing on its own");
  ok(JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 2, "and no terminal was opened (State snapshot unchanged)");
  ok((await evalIn(`document.querySelector('.claudehere').textContent`)).includes(first), `it names the terminal (${first})`);

  console.log("\nC. start in a new worktree → a second tab on claude-1 with claude started");
  await clickReal(page, ".claudehere .wt-new");
  // `until` reports only true/false, never the value fn() computed — so the
  // matching target has to be re-read from the list after it resolves rather
  // than trusted to come out of the poll itself (an earlier draft did that
  // and crashed two lines later dereferencing `.webSocketDebuggerUrl` off a
  // bare `true`).
  let targets = null;
  const foundTab = await until(async () => {
    const l = await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();
    targets = l.find((x) => x.url.includes(".claude/worktrees/claude-1")) || null;
    return !!targets;
  }, 30, "a tab on the worktree");
  ok(foundTab, "a second browser tab opened on the worktree project");
  const wt = await git(projDir, "worktree", "list", "--porcelain");
  ok(wt.out.includes(`${projDir}/.claude/worktrees/claude-1`) && wt.out.includes("refs/heads/claude-1"), "git lists the worktree and its branch");
  if (targets) {
    page2 = await attachTarget(targets.webSocketDebuggerUrl);
    await until(() => page2.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "worktree app.js");
    await page2.evalIn(helpers);
    ok(!(await page2.evalIn("location.search")), "the ?launch= parameter was consumed");
    // The worktree project is also freshly opened, so it carries its own
    // default "term" tab; the launched Claude terminal is the other one.
    ok(await until(async () => JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).length === 2, 20, "worktree terminal"), "the worktree opened its own terminal");
    const s2 = JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).find((s) => s !== "term");
    await until(() => page2.evalIn(`terms.has(${JSON.stringify(s2)})`), 30, "attached");
    ok(await until(async () => /FAKE-CLAUDE-STARTED argv=--session-id/.test(await page2.evalIn(`__txt(${JSON.stringify(s2)})`)), 60, "claude in the worktree"), "…with claude already typed into it");
  }

  console.log("\nD. start here anyway");
  await evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newclaude').click()`);
  await until(() => evalIn(`!!document.querySelector('.pane[data-pane="3"] .claudehere')`), 10, "the prompt again");
  await evalIn(`document.querySelector('.claudehere .wt-here').click()`);
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(__sessions(3))`)).length === 3, 20, "a third terminal"), "force opens a second terminal in the original project");

  console.log("\nE. the switcher shows state, and removal waits for the terminal to end");
  // Reopen (not a single click) for the same reason as the remove-control
  // wait below: #wtstrip's own pre-existing `hx-trigger` also answers plain
  // "refresh"/"projects" events — and section D's own new terminal just
  // dispatched one (TerminalStarted's handler does, a few lines up) — with a
  // non-stateful re-fetch that can land after this task's on-demand `state=1`
  // one and clobber it.
  let sawState = false;
  for (let i = 0; i < 20 && !sawState; i++) {
    await evalIn(`document.getElementById("wtbtn").click(); document.getElementById("wtbtn").click()`);
    sawState = await until(() => evalIn(`(document.getElementById("wtstrip").textContent || "").includes("0 ahead")`), 1, null);
  }
  ok(sawState, "the worktree row shows 0 ahead");
  ok(!(await evalIn(`!!document.querySelector("#wtstrip .wtremove")`)), "no remove control while its Claude terminal is attached");
  if (page2) {
    const s2 = JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).find((s) => s !== "term");
    await page2.evalIn(`window.confirm = () => true; send({ t: "EndSession", session: ${JSON.stringify(s2)} })`);
    // Ending it drops the tab, leaving only the untouched default "term" one.
    await until(async () => JSON.parse(await page2.evalIn(`JSON.stringify(__sessions(3))`)).length === 1, 20, "worktree terminal ended");
  }
  // Ending the worktree's last live shell also fires `ProjectsChanged` (see
  // its handler above: "gained its first shell or lost its last"), which
  // every open tab — including this one — answers by dispatching "projects"
  // on document.body. `#wtstrip` itself still listens for that (its own
  // `hx-trigger`, unchanged by this task) and re-fetches *without* `state=1`,
  // racing the on-demand stateful fetch the reopen below asks for. Whichever
  // response lands second wins the swap, so a single reopen is not reliable
  // right after a session ends — reopen (a real click, the same path a
  // person re-glancing at the panel takes) until the state-bearing fragment
  // wins.
  let sawRemove = false;
  for (let i = 0; i < 20 && !sawRemove; i++) {
    await evalIn(`document.getElementById("wtbtn").click(); document.getElementById("wtbtn").click()`);
    sawRemove = await until(() => evalIn(`!!document.querySelector("#wtstrip .wtremove")`), 1, null);
  }
  ok(sawRemove, "the remove control appears once the worktree is idle and clean");
  await evalIn(`window.confirm = () => true; document.querySelector("#wtstrip .wtremove").click()`);
  ok(await until(async () => !(await git(projDir, "worktree", "list", "--porcelain")).out.includes("claude-1"), 20, "worktree gone"), "clicking it removes the worktree");
  // `until`, like its neighbours: the branch goes in git step 2 of the same
  // closure that removed the worktree in step 1, so an immediate check can
  // land in the gap. Observed once on a slow host: worktree gone, branch
  // still listed, then gone a moment later.
  ok(await until(async () => (await git(projDir, "branch", "--list", "claude-1")).out.trim() === "", 10, "branch gone"), "…and its branch");
  ok(await until(async () => { try { await Deno.stat(`${projDir}/.claude/worktrees/claude-1`); return false; } catch { return true; } }, 5, "directory gone"), "the directory is gone");
} finally {
  page?.close(); page2?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
