//! The overview front page (`/`): it lists real sessions, clicking one opens
//! that terminal focused, "Open a directory" still reaches the picker, and
//! (Section D) selecting a project on the left narrows the right pane's
//! session list, with an `All` link to widen it back.
//!
//! Only a real browser + real dtach proves the session list reflects live
//! PTYs (the Claude/shell mark and attached count are exactly the kind of
//! thing a RESH_CMD=cat unit test renders without touching a real
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
//!   (c) Section D: tracing the server and client while writing this file —
//!       `routes.rs`'s `serve_index`, `render.rs`'s `overview_page`, and
//!       `overview.js`'s click handler — found no code path that carries
//!       `?sel=` from the row-click navigation into either fragment's
//!       `hx-get` (both are hardcoded strings with no query string). This
//!       was confirmed directly, outside the browser, by building resh and
//!       curling a throwaway instance: `GET /` and `GET /?sel=proj` return
//!       byte-identical `hx-get="/frag/_overview_projects"` /
//!       `hx-get="/frag/_overview_sessions"` markup. If that is still true
//!       when this file is finally run, Section D's "narrows" and (to a
//!       lesser extent, since an always-unfiltered view also "widens" back
//!       to nothing changed) "widens" assertions are expected to FAIL for
//!       real — that is a genuine product gap (the client never re-supplies
//!       `sel` to the polled fragments, whether via `overview.js` reading
//!       `location.search` on load or `overview_page` threading it through
//!       the initial `hx-get` attributes), not a bug in this test. Fixing it
//!       is outside this task's scope (no Rust/JS changes) and is called
//!       out separately in the task report.
//!
//! Run: deno run -A tests/browser/overview.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startResh, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();               // creates a git project `proj` under fx.roots
const browser = await startBrowser(profileDir(repoRoot));
let page, page2, page3, ws, ws2, resh;

/// Opens `project`'s workspace, starts a terminal via pane 3's + button, and
/// waits for it to attach. Returns the CDP client and the new session's
/// name. Same boilerplate the brief's Section A/B setup uses, factored out
/// so Section D can run it a second time against a second project.
async function startTerminal(browserPort, reshPort, project) {
  const w = await openPage(browserPort, `http://127.0.0.1:${reshPort}/${project}`);
  await until(() => w.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, `${project} workspace app.js`);
  await w.evalIn(`window.__sessions = (pi) => state.panes[pi].tabs.filter((t)=>t.k==="Terminal").map((t)=>t.session);`);
  await w.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await w.evalIn(`JSON.stringify(__sessions(3))`)).length > 0, 20, `${project} a terminal`);
  const sess = JSON.parse(await w.evalIn(`JSON.stringify(__sessions(3))`))[0];
  await until(() => w.evalIn(`terms.has(${JSON.stringify(sess)})`), 30, `${project} attached`);
  return { w, sess };
}

try {
  resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
  // Start a real terminal in `proj` via the workspace, so the overview has a session to show.
  const t1 = await startTerminal(browser.port, resh.port, fx.project);
  ws = t1.w;
  const sess = t1.sess;

  console.log("A. the overview lists the live session");
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/`);
  const sessionsText = () => page.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  ok(await until(async () => (await sessionsText()).includes(fx.project) && (await sessionsText()).includes(sess), 15, "session row"),
     `the overview's right pane lists ${fx.project} · ${sess}`);

  console.log("B. clicking the session opens the workspace focused on it");
  await page.evalIn(`document.querySelector('.ovsession a').click()`);
  ok(await until(async () => (await page.evalIn("location.pathname")).includes(fx.project), 15, "navigated to workspace"),
     "the row navigates to the project workspace");
  ok(await until(async () => (await page.evalIn("location.search")) === "", 10, "?focus consumed"),
     "?focus was stripped after focusing");

  console.log("C. Open a directory reaches the picker");
  page2 = await openPage(browser.port, `http://127.0.0.1:${resh.port}/?at=`);
  ok(await until(() => page2.evalIn(`!!document.getElementById("picker")`), 10, "picker"), "?at= shows the picker");

  console.log("D. selecting a project narrows the right pane; All widens it back");
  // A second project, built directly under fx.roots the same way fixture()
  // builds `proj` — a single-project fixture can't tell "filtered" from
  // "unfiltered" apart, since both render as "show everything there is".
  const proj2 = "proj2";
  await Deno.mkdir(`${fx.roots}/${proj2}`, { recursive: true });
  await new Deno.Command("git", { args: ["init", "-q"], cwd: `${fx.roots}/${proj2}`, stdout: "null", stderr: "null" }).output();
  const t2 = await startTerminal(browser.port, resh.port, proj2);
  ws2 = t2.w;
  const sess2 = t2.sess;

  // A fresh tab: `page` already navigated away in Section B, and this
  // section's own row click navigates too (overview.js sets
  // `location.href`), so reusing `page` or racing `page2` would confuse
  // which navigation an assertion is reading.
  page3 = await openPage(browser.port, `http://127.0.0.1:${resh.port}/`);
  const sessionsPane = () => page3.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  ok(await until(async () => (await sessionsPane()).includes(sess) && (await sessionsPane()).includes(sess2), 15, "both sessions listed"),
     `before selecting, #ovsessions lists both ${fx.project} · ${sess} and ${proj2} · ${sess2}`);

  ok(await until(() => page3.evalIn(`!!document.querySelector('#ovprojects .ovrow')`), 15, "project rows"),
     "the left pane lists at least one project row");
  // Found by visible label, not by an assumed `data-key` shape —
  // `registry::encode_key`'s exact encoding is an implementation detail
  // this test shouldn't have to reproduce.
  const clickedKey = await page3.evalIn(`(() => {
    const rows = [...document.querySelectorAll('#ovprojects .ovrow:not(.unreachable)')];
    const row = rows.find((r) => r.textContent.includes(${JSON.stringify(fx.project)}) && !r.textContent.includes(${JSON.stringify(proj2)}));
    if (!row) return null;
    row.click();
    return row.dataset.key ?? "";
  })()`);
  ok(clickedKey !== null, `found ${fx.project}'s row in the left pane and clicked it`);
  ok(await until(async () => (await page3.evalIn("location.search")).includes("sel="), 15, "?sel= navigation"),
     "clicking the row navigates to a ?sel= URL (the row-click intent, not the row's own <a>)");
  ok(await until(async () => (await sessionsPane()).includes(sess) && !(await sessionsPane()).includes(sess2), 15, "narrowed sessions"),
     `selecting ${fx.project} narrows #ovsessions to its own session and excludes ${proj2}'s`);

  ok(await until(() => page3.evalIn(`!!document.querySelector('.ovall')`), 10, "All link"),
     "the scoped view offers an All link back out");
  await page3.evalIn(`document.querySelector('.ovall').click()`);
  ok(await until(async () => (await page3.evalIn("location.search")) === "", 15, "back to unfiltered"),
     "the All link (href=\"/\") clears ?sel=");
  ok(await until(async () => (await sessionsPane()).includes(sess) && (await sessionsPane()).includes(sess2), 15, "widened sessions"),
     `All widens #ovsessions back to both ${fx.project} and ${proj2}`);
} finally {
  page?.close(); page2?.close(); page3?.close(); ws?.close(); ws2?.close();
  browser.close();
  if (resh) await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
