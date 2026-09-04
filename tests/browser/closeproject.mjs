//! Close Project, then reopen the project: the terminals must be gone from
//! the tab strip, not just from the process table.
//!
//! Reported from the deployed instance as "I close the project, which should
//! close all terminals, but on frontend (even after reload) I still see
//! terminals under project". The shells really were being killed — the
//! sockets went, the overview emptied — but `do_close_project` never touched
//! the layout, and the layout is *persisted*, so every reload of that project
//! restored a Terminal tab per dead session. A stale tab is not cosmetic
//! here: `workspace.rs`'s `EndSession` arm already explains why one left
//! behind "would offer a click that silently starts a *new* shell under a
//! name the user thought they had just closed".
//!
//! Why a browser test when `hub.rs` already asserts on the reloaded-from-disk
//! workspace: the Rust test proves what the *server* serves. What the user
//! reported is what the page *shows*, which is app.js reading `state.panes`
//! — and, per this directory's whole reason to exist, `cargo test` cannot
//! reach that file. Section C is the other half no Rust test covers: the
//! emptied pane has to stay usable, or this fix trades ghost tabs for a dead
//! pane.
//!
//! Traps this file is written against (see README):
//!   - Section A proves the setup state it later negates (three terminal
//!     tabs, two live sessions), so B's "no terminal tabs" cannot pass
//!     against a project that never had any.
//!   - B asserts the tree/changes tabs *survive*, so wiping the layout
//!     outright would fail rather than pass.
//!   - `confirm()` is stubbed rather than answered over CDP: a native dialog
//!     with no `Page.javascriptDialogOpening` handler wedges the renderer,
//!     which is the harness's documented 30s-timeout hang, not a failure.
//!
//! Section F is the reported bug reduced to its mechanism, and the one to read
//! first: after a close, a websocket to /ws/{project}/term/{name} must be
//! refused, not answered with a fresh shell. Revert-checked by restoring
//! `session::attach`'s create-when-absent — F then fails all 4, and the
//! failure *is* the report: `sockets: ["term1"]`, `dtach masters: 2`, from
//! nothing but a reconnect to a project that was already closed.
//!
//! Sections D and E cover two further defects found the same day, both of
//! which left a live shell behind rather than a stale tab: a close that races
//! a terminal still spawning (D), and a close that leaves each terminal's
//! reconnect armed (E). Each is revert-checked on its own below.
//!
//! Section G is a different bug from all of the above, and the one Tasks 1-5
//! of this branch actually fixed: a shell that traps SIGHUP (Claude Code
//! does) used to survive Close Project outright, because only the dtach
//! master died and the child it guarded reparented and lived on. It proves
//! the whole gesture end to end — a real browser click, a real dtach, a real
//! child process that ignores the signal `kill_and_unlink`'s old behavior
//! relied on — since this is also the only place the count roost reports to
//! the user is visible, and no Rust test reaches app.js.
//!
//! D is kept for coverage but is no longer the primary guard. It could only
//! ever be stated as a *rate* — 3-of-3 failing, 4-of-4 passing — because it
//! depended on a spawn landing inside a timing window. F depends on a rule
//! instead, which is the whole point of the reservation design: a connect
//! with no reservation, no session and no live socket is refused, always.
//!
//! Revert-check, performed: with both `drop_terminal_tabs()` calls removed
//! from `hub.rs`'s `do_close_project`, this fails 3 — B's tab assertion and
//! its saved-layout assertion (`term`, `term1`, `term2` all back), plus C,
//! where the "fresh" terminal lands beside the three ghosts as `term3`.
//! Removing only that helper's `self.persist()` fails exactly 1: the
//! saved-layout assertion. Everything the *page* shows still passes there,
//! because this reopen talks to the same still-running hub whose in-memory
//! layout was cleared — which is why the on-disk read is a separate
//! assertion, and is the one that stands for "even after reload".
//!
//! Run: deno run -A tests/browser/closeproject.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const browser = await startBrowser(profileDir(repoRoot));
let ws, after, roost;

/// The dtach sockets on disk for the fixture project — the evidence that the
/// shells themselves ended, kept separate from what the page shows. `.origin`
/// is the registry's marker, not a session.
async function sockets() {
  const out = [];
  try {
    for await (const e of Deno.readDir(`${fx.stateDir}/sock/${fx.project}`)) {
      if (e.name !== ".origin") out.push(e.name);
    }
  } catch { /* the whole directory may be gone; that is zero sessions */ }
  return out.sort();
}

const terminalTabs = (p) =>
  p.evalIn(`JSON.stringify(state.panes.flatMap((q) => q.tabs).filter((t) => t.k === "Terminal").map((t) => t.session))`)
    .then(JSON.parse);
const tabKinds = (p) =>
  p.evalIn(`JSON.stringify(state.panes.flatMap((q) => q.tabs).map((t) => t.k))`).then(JSON.parse);

try {
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });

  console.log("A. a project with live terminals");
  ws = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => ws.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  const newterm = `document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`;
  // Two more on top of the default layout's own `term` tab, so the close has
  // several sessions in several states to clear: one tab never attached, two
  // with a real dtach master behind them.
  await ws.evalIn(newterm);
  await until(async () => (await terminalTabs(ws)).length >= 2, 20, "a second terminal");
  await ws.evalIn(newterm);
  await until(async () => (await terminalTabs(ws)).length >= 3, 20, "a third terminal");
  await until(async () => (await sockets()).length >= 2, 30, "two dtach sockets");
  const before = await terminalTabs(ws);
  ok(before.length === 3, `the project has terminal tabs to lose: ${JSON.stringify(before)}`);
  ok((await sockets()).length >= 2, `and real shells behind them: ${JSON.stringify(await sockets())}`);

  console.log("B. after Close Project, reopening shows no terminals");
  // Stubbed, not answered over CDP — see the header note on wedged renderers.
  await ws.evalIn(`window.confirm = () => true; window.alert = () => {}; document.getElementById("closeproj").click()`);
  ok(await until(async () => (await sockets()).length === 0, 30, "every socket unlinked"),
     `the shells themselves ended: ${JSON.stringify(await sockets())}`);
  // The sockets go early in the close, but the close is not over: it sweeps a
  // second time after `CLOSE_SETTLE` (see Section D), and `closing` stays true
  // across both — so `term.rs` deliberately refuses a new terminal for that
  // whole window. Reopening inside it made B and C fail here for that reason
  // alone, which is a real property worth waiting out rather than hiding: a
  // user who closes a project and immediately reopens it meets the same
  // refusal, and it clears itself.
  await sleep(1500);
  // A close ends by navigating to `/`; reopen the project the way a user
  // would, which is the reload the report was about.
  after = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => after.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "reopened app.js");
  const reopened = await terminalTabs(after);
  ok(reopened.length === 0, `the reopened project has no terminal tabs: ${JSON.stringify(reopened)}`);
  ok((await after.evalIn(`JSON.stringify(state.live_sessions)`)) === "[]", "and no live sessions");
  // The other direction: this clears terminals, not the workspace. Without
  // these two, emptying every pane outright would pass Section B.
  const kinds = await tabKinds(after);
  ok(kinds.includes("Tree"), `the tree pane survived the close: ${JSON.stringify(kinds)}`);
  ok(kinds.includes("Changes"), `the changes pane survived the close: ${JSON.stringify(kinds)}`);
  // What a *reload* reads, as opposed to what this connection was handed.
  const saved = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/${fx.project}.json`));
  const savedTerms = saved.panes.flatMap((p) => p.tabs).filter((t) => t.k === "Terminal");
  ok(savedTerms.length === 0, `the saved layout keeps no terminal tab: ${JSON.stringify(savedTerms)}`);

  console.log("C. the emptied pane is still usable");
  ok(await after.evalIn(`!!document.querySelector('.pane[data-pane="3"] .paneicons .newterm')`),
     "a pane with no tabs keeps its ✛ button");
  await after.evalIn(newterm);
  ok(await until(async () => (await terminalTabs(after)).length === 1, 20, "a fresh terminal"),
     "a new terminal can be started in the closed project again");
  ok(await until(async () => (await sockets()).length === 1, 30, "its socket"),
     `and it is a real shell: ${JSON.stringify(await sockets())}`);
  console.log("D. a terminal still spawning when the close runs must not survive it");
  // The second reported failure, and a different bug from A-C: the close
  // ended the shells and cleared the tabs, and a shell appeared anyway. In
  // production the survivor's dtach had roost itself as its parent and a start
  // time in the same second as the close — a terminal websocket that reached
  // `session::attach` (which creates when absent) while `kill_project` was
  // reading a socket directory and a `ps` snapshot that could not show it yet.
  //
  // No wait between starting the terminal and closing: every other section
  // here waits for the socket to exist first, and that wait is exactly what
  // hides this. Both clicks go in one evaluation so the browser cannot
  // interleave a round trip between them.
  const race = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => race.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "race page");
  await race.evalIn(`
    window.confirm = () => true; window.alert = () => {};
    document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click();
    document.getElementById("closeproj").click();
  `);
  await sleep(9000);   // past CLOSE_SETTLE and the second sweep, with margin
  const savedAfter = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/${fx.project}.json`));
  const raceTerms = savedAfter.panes.flatMap((p) => p.tabs).filter((t) => t.k === "Terminal");
  // Proves the close actually committed. Without it, a close that was refused
  // or never sent would leave no sessions to find and pass the next assertion
  // for the opposite reason.
  ok(raceTerms.length === 0, `the racing close committed (layout cleared): ${JSON.stringify(raceTerms)}`);
  ok((await sockets()).length === 0, `no socket outlived the racing close: ${JSON.stringify(await sockets())}`);
  const ps = await new Deno.Command("ps", { args: ["-Ao", "args="], stdout: "piped" }).output();
  const orphans = new TextDecoder().decode(ps.stdout).split("\n")
    .filter((l) => l.includes(`${fx.stateDir}/sock/${fx.project}/`) && l.includes("dtach -A"));
  ok(orphans.length === 0, `no dtach master outlived the racing close: ${orphans.length}`);
  try { race.close(); } catch { /* already gone */ }

  console.log("E. the close disarms each terminal's reconnect");
  // The third reported failure, and the one that actually explains "the last
  // terminal always persists". `render()`'s teardown sets `gone` and clears
  // the retry timer before closing a socket, and its comment says why:
  // `attach` creates when absent, so a reconnect that outlives the teardown
  // respawns the shell rather than reattaching. The ProjectClosed teardown
  // did neither — so a PTY killed by the close whose socket died unclean
  // scheduled `connectTerm` on backoff, and a retry landing after the server
  // cleared `closing` was accepted and spawned. Observed live as a dtach
  // parented to roost, started in the same second as the close, with nothing
  // attached to it.
  //
  // Asserts on the entries themselves rather than on a surviving socket: the
  // spawn needs a retry to land in a narrow window, so a socket check is a
  // coin flip that passes most runs whether or not the bug is there. `gone`
  // is what makes onclose bail, so it is the property, not a proxy for it.
  const fin = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => fin.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "final page");
  await fin.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => (await fin.evalIn(`terms.size`)) >= 1, 20, "a mounted terminal");
  await until(async () => (await sockets()).length >= 1, 30, "its socket");
  // Held outside `terms`, which the handler clears — otherwise there would be
  // nothing left to inspect afterwards. Captured and *counted* before the
  // close: `[].every()` is true, so a check that only ran afterwards would
  // pass on an empty array, which is precisely how this section first
  // reported a pass and then a bare `[]` under revert. The count is the guard
  // against that.
  const held = await fin.evalIn(`window.__entries = [...terms.values()]; window.__entries.length`);
  ok(held > 0, `setup: ${held} live terminal(s) captured to inspect after the close`);
  ok(await fin.evalIn(`window.__entries.every((e) => !e.gone)`),
     "setup: none of them is marked gone before the close");
  await fin.evalIn(`window.confirm = () => true; window.alert = () => {};
    document.getElementById("closeproj").click();`);
  // Read at 25ms before the handler's 1200ms navigation destroys the page,
  // keeping the last reading that actually saw the array. After navigation
  // `window.__entries` is undefined and the map yields `[]` — which is "I
  // could not look", not "nothing was disarmed". Folding those together is
  // the mistake this repo has a whole CLAUDE.md section about, and it is not
  // hypothetical here: the first version of this section asserted on `[]` and
  // so reported a failure it had not actually observed.
  let seen = null;
  for (let i = 0; i < 160; i++) {
    let r;
    try { r = JSON.parse(await fin.evalIn(`JSON.stringify((window.__entries || []).map((e) => !!e.gone))`)); }
    catch { break; }                 // context destroyed by the navigation
    if (r.length === held) { seen = r; if (r.every(Boolean)) break; }
    await sleep(25);
  }
  if (seen === null) {
    ok(false, `could not observe the entries before the page navigated (not the same as "not disarmed")`);
  } else {
    ok(seen.every(Boolean), `all ${held} terminal(s) disarmed by the close: ${JSON.stringify(seen)}`);
  }
  try { fin.close(); } catch { /* already gone */ }

  console.log("F. after a close, reconnecting to an ended session creates nothing");
  // The reported bug, reduced to its mechanism. Every earlier section closes
  // a project and then checks that nothing came back; this one *tries* to
  // bring it back, the same way a browser did — a websocket to
  // /ws/{project}/term/{name} — and requires the server to refuse.
  //
  // Driven from a page rather than from Deno so the connect carries a real
  // Origin; a header-less connect would be refused by `origin.rs` long before
  // `attach`, which would pass this section while proving nothing about it.
  //
  // This is deterministic, unlike the old Section D, which could only be
  // stated as a rate (3-of-3 failing / 4-of-4 passing) because it depended on
  // a spawn landing inside a settle window. The invariant replaced the
  // window: no reservation, no session, no live socket → refused, always.
  const back = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  await until(() => back.evalIn(`document.readyState === "complete"`), 20, "overview loaded");
  const ended = await back.evalIn(`
    new Promise((res) => {
      const ws = new WebSocket("ws://127.0.0.1:${roost.port}/ws/${fx.project}/term/term1");
      ws.onclose = (e) => res("closed:" + e.code + ":" + e.wasClean);
      ws.onopen = () => setTimeout(() => res("still-open"), 4000);
      setTimeout(() => res("no-answer"), 8000);
    })
  `);
  ok(String(ended).startsWith("closed:"), `the server refused the reconnect: ${ended}`);
  ok(String(ended).endsWith(":true"), `and refused it *cleanly*, so app.js will not retry: ${ended}`);
  await sleep(2000);
  ok((await sockets()).length === 0, `no shell was spawned by the attempt: ${JSON.stringify(await sockets())}`);
  const ps2 = await new Deno.Command("ps", { args: ["-Ao", "args="], stdout: "piped" }).output();
  const revived = new TextDecoder().decode(ps2.stdout).split("\n")
    .filter((l) => l.includes(`${fx.stateDir}/sock/${fx.project}/`) && l.includes("dtach -A"));
  ok(revived.length === 0, `and no dtach master appeared: ${revived.length}`);
  try { back.close(); } catch { /* already gone */ }

  console.log("G. a child that ignores SIGHUP does not survive the close");
  // A fresh project, because Sections A-F have already closed the fixture's.
  const g = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => g.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  // F's close wiped the layout's terminal tabs, so nothing auto-spawns here
  // the way Section A's terminal did on a project that had never been
  // closed — get one the same way C does after its own close: click
  // new-terminal, then wait for its socket, rather than waiting for a socket
  // that will never appear on its own.
  await g.evalIn(newterm);
  await until(async () => (await sockets()).length === 1, 30, "its socket");

  // Typed over the terminal socket rather than the xterm: input on that socket
  // is the raw bytes to type (`term.rs` writes a Binary frame straight to the
  // pty), and the page supplies the Origin the handshake requires.
  const marker = `roost_hup_survivor_${Date.now()}`;
  await g.evalIn(`
    new Promise((res) => {
      const w = new WebSocket("ws://127.0.0.1:${roost.port}/ws/${fx.project}/term/term");
      w.onopen = () => {
        w.send(new TextEncoder().encode(
          "bash -c 'trap \\"\\" HUP; exec -a ${marker} sleep 600' &\\n"));
        setTimeout(() => { w.close(); res("sent"); }, 1500);
      };
      w.onerror = () => res("error");
      setTimeout(() => res("timeout"), 8000);
    })
  `);
  await sleep(1500);

  const running = async () => {
    const out = await new Deno.Command("ps", { args: ["-Ao", "args="], stdout: "piped" }).output();
    return new TextDecoder().decode(out.stdout).split("\n").filter((l) => l.includes(marker)).length;
  };
  // Asserts the setup state it later negates: without this, a child that never
  // started would make the assertion below pass for the wrong reason.
  ok((await running()) > 0, "the HUP-ignoring child is running before the close");

  // Revert-checked: with the two `kill_sessions(proc_root, &targets)` calls
  // removed from `kill_and_unlink_with` and both `session_or_socket_alive`
  // calls replaced by `socket_has_process_with(sock_path, snapshot_fn)`, this
  // section fails — "FAIL  no HUP-ignoring child survived the close: 1 left"
  // — while every other section (A-F) still passes. Restored via `cp` from a
  // backup, never `git checkout`.
  await g.evalIn(`send({ t: "CloseProject" })`);
  // The `ended` count is the number roost tells the user, and per the design
  // spec "the `ended` number is half the defect" — the old confirmation used
  // to call this session ended (the master died) while the HUP-ignoring
  // child above was still running, which would have reported it here too.
  // Read the banner `app.js`'s `ProjectClosed` handler renders
  // (`showBanner(ev.ended + " terminal session(s) ended")`), not merely that
  // one appeared: a presence-only check passes for any count, the same
  // vacuous shape this branch's own plan already produced twice.
  ok(
    await until(() => g.evalIn(`!!document.querySelector(".error-banner")`), 30, "the close banner"),
    "Close Project shows a banner reporting how many sessions ended"
  );
  const closedBanner = await g.evalIn(`(document.querySelector(".error-banner") || {}).textContent || ""`);
  ok(
    closedBanner.includes("1 terminal session(s) ended"),
    `banner reports exactly the one session this section created: ${JSON.stringify(closedBanner)}`
  );
  await sleep(4000);
  ok((await running()) === 0, `no HUP-ignoring child survived the close: ${await running()} left`);
  ok((await sockets()).length === 0, `and no socket was left behind: ${JSON.stringify(await sockets())}`);
  try { g.close(); } catch { /* already gone */ }
} finally {
  try { ws?.close(); } catch { /* already gone */ }
  try { after?.close(); } catch { /* already gone */ }
  await roost?.close();
  browser.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
