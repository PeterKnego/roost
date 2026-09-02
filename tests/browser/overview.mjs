//! The overview front page (`/`): it lists real sessions, clicking one opens
//! that terminal focused, "Open a directory" still reaches the picker, and
//! (Section D) selecting a project on the left narrows the right pane's
//! session list, with an `All` link to widen it back.
//!
//! Only a real browser + real dtach proves the session list reflects live
//! PTYs (the Claude/shell mark and attached count are exactly the kind of
//! thing a ROOST_CMD=cat unit test renders without touching a real
//! terminal). Assertions read DOM/State, never event order (README trap 2:
//! client-visible ordering pipelines per connection and was proved
//! non-discriminating once in reconnect.mjs). Every wait uses `until`, which
//! returns false on timeout rather than throwing — so what's asserted below
//! is always the actual DOM/state read *after* `until` resolves, never
//! `until`'s own boolean (the "asserted nothing, passed off its own
//! timeout" trap CLAUDE.md documents elsewhere in this codebase).
//!
//! Section D's carry-in: Task 4's review noted `build_overview_sessions`'s
//! scope-filter predicate (`sel` -> a project + its worktree children) has
//! no Rust unit test, since the routes-level tests exercise it with empty
//! `roots`. A single-project fixture cannot discriminate "filtered" from
//! "unfiltered" (both look like "show everything"), so Section D creates a
//! second project, `proj2`, directly under `fx.roots` — the same way
//! `fixture()` builds `proj` — and starts a live terminal in each, so the
//! right pane has two distinct sessions to narrow between. It runs after
//! Sections A-C, once they're done with `fx.project`'s single session, so
//! their `.ovsession a` (only) assumption stays true regardless of render
//! order between the two projects.
//!
//! Worktree-expansion is not covered here: `worktree-launch.mjs` already
//! creates one worktree end to end, and the tree's own rendering (caret,
//! chips, nesting) is unit-tested in Task 2's `render.rs` tests
//! (`overview_projects_nests_worktrees_under_their_parent_and_marks_selection`).
//!
//! Revert-checks, recorded here for a human to run with a real Chromium —
//! this session has none (the snap was removed), so none of the below was
//! actually watched failing:
//!   (a) remove `history.replaceState` from `app.js`'s `?focus` consumer ->
//!       Section B's "?focus was stripped after focusing" should fail
//!       (`location.search` keeps `?focus=...`).
//!   (b) revert `build_overview_sessions` (`routes.rs`) to return `vec![]`
//!       unconditionally -> Section A's "lists a live session" assertions
//!       should fail. (Pinning `overview_sessions`'s mark to a fixed glyph
//!       is not independently observable through this test — the mark is
//!       text, not checked against process state here — which is why the
//!       brief redirects that revert-check to the scope-empty one instead.)
//!   (c) Section D: at the time this file was first written, `?sel=` was
//!       not carried from the row-click navigation into either fragment's
//!       `hx-get`, so Section D was expected to fail for real. That gap was
//!       closed in commit 5b411b3: `render.rs`'s `overview_page` now bakes
//!       `?sel=<key>` into both fragments' `hx-get` URLs, and `routes.rs`'s
//!       `serve_index` reads `sel` off the query string to render it. So as
//!       of that commit, all four sections — including Section D's "narrows"
//!       and "widens" assertions — are expected to PASS when this file is
//!       run against a real Chromium. Revert `overview_page`'s `?sel=`
//!       threading (or `serve_index`'s reading of it) to re-observe Section D
//!       fail the way it originally did.
//!
//! Run: deno run -A tests/browser/overview.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();               // creates a git project `proj` under fx.roots
const browser = await startBrowser(profileDir(repoRoot));
let page, page2, page3, ws, ws2, roost;

/// Opens `project`'s workspace, starts a terminal via pane 3's + button, and
/// waits for it to attach. Returns the CDP client and the new session's
/// name. Same boilerplate the brief's Section A/B setup uses, factored out
/// so Section D can run it a second time against a second project.
async function startTerminal(browserPort, roostPort, project) {
  const w = await openPage(browserPort, `http://127.0.0.1:${roostPort}/${project}`);
  await until(() => w.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, `${project} workspace app.js`);
  await w.evalIn(`window.__sessions = (pi) => state.panes[pi].tabs.filter((t)=>t.k==="Terminal").map((t)=>t.session);`);
  await w.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await w.evalIn(`JSON.stringify(__sessions(3))`)).length > 0, 20, `${project} a terminal`);
  const sess = JSON.parse(await w.evalIn(`JSON.stringify(__sessions(3))`))[0];
  await until(() => w.evalIn(`terms.has(${JSON.stringify(sess)})`), 30, `${project} attached`);
  return { w, sess };
}

try {
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
  // Start a real terminal in `proj` via the workspace, so the overview has a session to show.
  const t1 = await startTerminal(browser.port, roost.port, fx.project);
  ws = t1.w;
  const sess = t1.sess;

  console.log("A. the overview lists the live session");
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const sessionsText = () => page.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  ok(await until(async () => (await sessionsText()).includes(fx.project) && (await sessionsText()).includes(sess), 15, "session row"),
     `the overview's right pane lists ${fx.project} · ${sess}`);

  console.log("B. clicking the session opens the workspace focused on it");
  await page.evalIn(`document.querySelector('.ovsession a').click()`);
  ok(await until(async () => (await page.evalIn("location.pathname")).includes(fx.project), 15, "navigated to workspace"),
     "the row navigates to the project workspace");
  ok(await until(async () => (await page.evalIn("location.search")) === "", 10, "?focus consumed"),
     "?focus was stripped after focusing");

  console.log("C. the directory picker is gone");
  // `?at=` was the picker's URL. The overview lists every project directory
  // under the roots, so browsing to find one had nothing left to add.
  page2 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/?at=`);
  ok(await until(() => page2.evalIn(`!!document.getElementById("overview")`), 10, "overview"),
     "?at= serves the overview");
  ok(!(await page2.evalIn(`!!document.getElementById("picker")`)), "there is no picker any more");
  ok(!(await page2.evalIn(`document.body.innerHTML.includes("?at=")`)),
     "and nothing on the page points at one");

  console.log("D. selecting a project narrows the right pane; All widens it back");
  // A second project, built directly under fx.roots the same way fixture()
  // builds `proj` — a single-project fixture can't tell "filtered" from
  // "unfiltered" apart, since both render as "show everything there is".
  const proj2 = "proj2";
  await Deno.mkdir(`${fx.roots}/${proj2}`, { recursive: true });
  await new Deno.Command("git", { args: ["init", "-q"], cwd: `${fx.roots}/${proj2}`, stdout: "null", stderr: "null" }).output();
  const t2 = await startTerminal(browser.port, roost.port, proj2);
  ws2 = t2.w;
  const sess2 = t2.sess;

  // A fresh tab: `page` already navigated away in Section B. This
  // section's row click no longer navigates (the panes swap in place), but
  // it still needs a tab that is actually on the overview.
  page3 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const sessionsPane = () => page3.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  ok(await until(async () => (await sessionsPane()).includes(sess) && (await sessionsPane()).includes(sess2), 15, "both sessions listed"),
     `before selecting, #ovsessions lists both ${fx.project} · ${sess} and ${proj2} · ${sess2}`);

  ok(await until(() => page3.evalIn(`!!document.querySelector('#ovprojects .ovrow')`), 15, "project rows"),
     "the left pane lists at least one project row");
  // Found by visible label, not by an assumed `data-key` shape —
  // `registry::encode_key`'s exact encoding is an implementation detail
  // this test shouldn't have to reproduce.
  // A document reload wipes anything on `window`, so this marker is what
  // tells "the pane was swapped in place" apart from "the page navigated" —
  // the point of the selection being app-like rather than a link.
  // Revert-checked: restoring the old `location.href = "?sel=…"` in
  // overview.js fails this line ("FAIL selecting swapped the panes in
  // place — the document never reloaded").
  await page3.evalIn(`window.__alive = "kept"`);
  const clickedKey = await page3.evalIn(`(() => {
    const rows = [...document.querySelectorAll('#ovprojects .ovrow:not(.unreachable)')];
    const row = rows.find((r) => r.textContent.includes(${JSON.stringify(fx.project)}) && !r.textContent.includes(${JSON.stringify(proj2)}));
    if (!row) return null;
    row.click();
    return row.dataset.key ?? "";
  })()`);
  ok(clickedKey !== null, `found ${fx.project}'s row in the left pane and clicked it`);
  ok(await page3.evalIn(`window.__alive`) === "kept",
     "selecting swapped the panes in place — the document never reloaded");
  ok(await until(async () => (await page3.evalIn("location.search")).includes("sel="), 15, "?sel= navigation"),
     "clicking the row navigates to a ?sel= URL (the row-click intent, not the row's own <a>)");
  // Project-qualified labels, as the "before selecting" check above uses:
  // both fixtures' first terminal is named `term`, so a bare
  // `includes(sess) && !includes(sess2)` is `includes("term") &&
  // !includes("term")` — unsatisfiable. Found by the first real run of
  // this file (the server's ?sel= round-trip was correct; the test wasn't).
  ok(await until(async () => {
    const t = await sessionsPane();
    return t.includes(`${fx.project} · ${sess}`) && !t.includes(`${proj2} · ${sess2}`);
  }, 15, "narrowed sessions"),
     `selecting ${fx.project} narrows #ovsessions to its own session and excludes ${proj2}'s`);

  ok(await until(() => page3.evalIn(`!!document.querySelector('.ovall')`), 10, "All link"),
     "the scoped view offers an All link back out");
  await page3.evalIn(`document.querySelector('.ovall').click()`);
  ok(await until(async () => (await page3.evalIn("location.search")) === "", 15, "back to unfiltered"),
     "the All link (href=\"/\") clears ?sel=");
  ok(await until(async () => (await sessionsPane()).includes(sess) && (await sessionsPane()).includes(sess2), 15, "widened sessions"),
     `All widens #ovsessions back to both ${fx.project} and ${proj2}`);

  console.log("E. expanding one project survives selecting another");
  // Worktrees to expand: the fixture's projects have none, and an expander
  // that yields nothing cannot show whether expansion was preserved.
  for (const p of [`${fx.roots}/${fx.project}`, `${fx.roots}/${proj2}`]) {
    const g = async (...a) =>
      await new Deno.Command("git", { args: ["-C", p, ...a], stdout: "null", stderr: "null" }).output();
    await g("config", "user.email", "t@t");
    await g("config", "user.name", "t");
    await g("add", "-A");
    await g("commit", "-qm", "init");
    await g("worktree", "add", "-q", "-b", `wt-${p.split("/").pop()}`, ".claude/worktrees/wt");
  }
  const page4 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const tree = () => page4.evalIn(`document.getElementById("ovprojects")?.textContent || ""`);
  const kidsOf = (label) =>
    page4.evalIn(`(() => {
      const rows = [...document.querySelectorAll('#ovprojects .ovrow')];
      const parent = rows.find((r) => !r.classList.contains('child') && r.textContent.includes(${JSON.stringify(label)}));
      if (!parent) return -1;
      return rows.filter((r) => r.dataset.parent === parent.dataset.key).length;
    })()`);
  const clickRow = (label, what) =>
    page4.evalIn(`(() => {
      const rows = [...document.querySelectorAll('#ovprojects .ovrow')];
      const row = rows.find((r) => !r.classList.contains('child') && r.textContent.includes(${JSON.stringify(label)}));
      if (!row) return false;
      (${what === "caret" ? "row.querySelector('.ovcaret')" : "row"}).click();
      return true;
    })()`);

  ok(await until(async () => (await tree()).includes(fx.project) && (await tree()).includes(proj2), 15, "both projects"),
     "the left pane lists both projects");
  ok(await clickRow(fx.project, "caret"), `clicked ${fx.project}'s expander`);
  ok(await until(async () => (await kidsOf(fx.project)) > 0, 15, "worktree child"),
     `expanding ${fx.project} loads its worktree`);

  await page4.evalIn(`window.__alive2 = "kept"`);
  ok(await clickRow(proj2, "row"), `selected ${proj2}`);
  ok(await until(async () => (await kidsOf(proj2)) > 0, 15, "selected project's worktree"),
     `selecting ${proj2} loads its worktree too`);
  ok(await page4.evalIn(`window.__alive2`) === "kept",
     "selecting did not reload the document");
  // The reported bug: selecting a project collapsed whatever was already
  // open, because the left pane is re-fetched and the expanded set has to
  // survive that round trip.
  ok((await kidsOf(fx.project)) > 0,
     `${fx.project} is still expanded after selecting ${proj2}`);

  // Selecting must not re-fetch the list the selection was made from: the
  // row is already on screen, and the only thing the server can add is that
  // project's own worktrees.
  //
  // Counted from the browser's own resource timeline, not by wrapping
  // `fetch`/`XMLHttpRequest`: those wrappers looked right and caught
  // nothing, so the first version of this assertion passed with the
  // re-fetch fully restored — a test that could not fail. The poll is
  // stopped first (a detached node stops polling) so the count reflects the
  // click and not a poll that happened to land beside it.
  //
  // Revert-checked: putting `refresh("proj", sel)` back into select() fails
  // this line ("FAIL neither collapsing nor selecting re-fetched the project
  // list (1 requests, unchanged)").
  //
  // Revert-checked: putting `refresh("proj", sel)` back into select() fails
  // this line ("FAIL neither collapsing nor selecting re-fetched the
  // project list (1 requests, unchanged)").
  const listRequests = () =>
    page4.evalIn(
      `performance.getEntriesByType('resource').filter((e) => e.name.includes('_overview_projects')).length`,
    );
  await page4.evalIn(`(() => {
    const el = document.getElementById('ovprojects');
    const clone = el.cloneNode(true);
    clone.setAttribute('hx-trigger', 'none');
    el.replaceWith(clone);
    if (window.htmx) htmx.process(clone);
  })()`);
  const before = await listRequests();
  await clickRow(proj2, "caret");
  ok(await until(async () => (await kidsOf(proj2)) === 0, 10, "collapsed"),
     `collapsing ${proj2} removes its worktree rows`);
  await clickRow(proj2, "row");
  ok(await until(async () => (await kidsOf(proj2)) > 0, 15, "worktrees back"),
     `selecting ${proj2} again brings its worktrees back`);
  await sleep(700); // any re-fetch would have been recorded by now
  ok((await listRequests()) === before,
     `neither collapsing nor selecting re-fetched the project list (${before} requests, unchanged)`);
  page4.close();

  console.log("F. a selection survives the poll");
  // The reported bug, as reported: select a project, wait past one poll,
  // select another, wait again. Verified against the bug: before the fix
  // these four assertions failed ("proj is still the selected row after a
  // poll" and the three like it) while everything else in the file passed. htmx captures a polling element's URL when
  // it processes the node, so a selection that only rewrites `hx-get` never
  // reaches the poll — five seconds later the pane snaps back to whatever
  // the page was first opened with.
  const page5 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const curRow = () =>
    page5.evalIn(`(document.querySelector('#ovprojects .ovrow.current')?.textContent || "").trim()`);
  const sessText = () => page5.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  const pick = (label) =>
    page5.evalIn(`(() => {
      const rows = [...document.querySelectorAll('#ovprojects .ovrow')];
      const row = rows.find((r) => !r.classList.contains('child') && r.textContent.includes(${JSON.stringify(label)}));
      if (!row) return false;
      row.click();
      return true;
    })()`);

  ok(await until(async () => (await page5.evalIn(`document.querySelectorAll('#ovprojects .ovrow').length`)) > 1, 15, "rows"),
     "the left pane lists the projects");
  ok(await pick(fx.project), `selected ${fx.project}`);
  ok(await until(async () => (await curRow()).includes(fx.project), 10, "marked current"),
     `${fx.project} is marked current`);
  await sleep(6000); // one poll interval, and then some
  ok((await curRow()).includes(fx.project),
     `${fx.project} is still the selected row after a poll`);
  ok(!(await sessText()).includes(`${proj2} · `),
     `the sessions pane is still narrowed to ${fx.project} after a poll`);

  ok(await pick(proj2), `selected ${proj2}`);
  ok(await until(async () => (await curRow()).includes(proj2), 10, "marked current"),
     `${proj2} is marked current`);
  await sleep(6000);
  ok((await curRow()).includes(proj2),
     `${proj2} is still the selected row after a poll`);
  ok(!(await sessText()).includes(`${fx.project} · ${sess}`),
     `the sessions pane is still narrowed to ${proj2} after a poll`);

  // …and the loop is still a loop. Every assertion above would also pass if
  // refreshing had simply stopped, so this proves the page still learns
  // things it was not told: a directory created after load must appear on
  // its own. Revert-checked by deleting the `setInterval` — this line is
  // the only one in the section that then fails.
  await Deno.mkdir(`${fx.roots}/latecomer`, { recursive: true });
  ok(await until(async () =>
      (await page5.evalIn(`document.getElementById("ovprojects")?.textContent || ""`)).includes("latecomer"),
     20, "a project created after load"),
     "the pane keeps refreshing: a directory created after load shows up on its own");
  page5.close();

  console.log("G. a project with nothing running can still be opened");
  // The reported dead end: the only link on the page that reached a
  // workspace was a session row, so a project with no sessions — every
  // project nobody has opened yet — could not be opened at all.
  const page6 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const idle = "quiet-project";
  await Deno.mkdir(`${fx.roots}/${idle}`, { recursive: true });
  const pickRow = (label) =>
    page6.evalIn(`(() => {
      const rows = [...document.querySelectorAll('#ovprojects .ovrow')];
      const row = rows.find((r) => !r.classList.contains('child') && r.textContent.includes(${JSON.stringify(label)}));
      if (!row) return false;
      row.click();
      return true;
    })()`);
  ok(await until(async () =>
      (await page6.evalIn(`document.getElementById("ovprojects")?.textContent || ""`)).includes(idle),
     20, "the idle project"), `${idle} is listed though nothing runs in it`);
  ok(await pickRow(idle), `selected ${idle}`);
  ok(await until(async () =>
      (await page6.evalIn(`document.getElementById("ovsessions")?.textContent || ""`)).includes("no sessions running"),
     15, "empty sessions pane"), "its sessions pane says nothing is running");
  // Two ways in, both at project level: the row's own control, and the one
  // the empty pane offers. Revert-checked: dropping the row's control from
  // `ov_row` fails "the selected row offers a way in".
  ok(await page6.evalIn(`!!document.querySelector('#ovprojects .ovrow.current .ovgo')`),
     "the selected row offers a way in");
  ok(await page6.evalIn(`!!document.querySelector('#ovsessions .ovgo')`),
     "so does the pane that has nothing to list");
  await page6.evalIn(`document.querySelector('#ovprojects .ovrow.current .ovgo').click()`);
  ok(await until(async () => (await page6.evalIn("location.pathname")) === `/${idle}`, 15, "workspace"),
     `it opens ${idle}'s workspace`);
  page6.close();
} finally {
  page?.close(); page2?.close(); page3?.close(); ws?.close(); ws2?.close();
  browser.close();
  if (roost) await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
