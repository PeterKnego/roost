//! The header's worktree switcher: the chip grows a label only when the repo
//! has somewhere to switch to, the panel lists idle worktrees, and a row
//! click navigates THIS tab (no target= — ⌘-click new-tab is the browser's).
//!
//! Section A starts a terminal before checking the chip: `known_projects`
//! (registry.rs) only lists a project once it has a saved workspace or a
//! live session, so opening the page alone persists nothing and the second
//! family member never appears — the chip label would never fill and the
//! wait would just time out. A live session is the cheapest way to get the
//! worktree into the registry.
//!
//! Revert-check counts (CLAUDE.md: "would this fail if I deleted the code it
//! covers?", watched for real, not asserted from a thought experiment; each
//! applied alone, run, and restored before the next):
//! - Commenting out the `["frag", "_worktrees"]` arm in routes.rs fails 5:
//!   the chip label (A), "the idle worktree is listed", "with its branch"
//!   and "the current row is the main worktree" (B, against an empty
//!   fragment), and — cascading, since there is then no row to click —
//!   section C's "the same tab now shows the worktree's workspace". "rows
//!   have href and no target" passes vacuously (`.every()` over zero rows).
//! - Commenting out `wtBtn.onclick` in app.js fails exactly 1: "clicking the
//!   chip opens the panel". Everything else in B still passes — the
//!   fragment loads on its own `hx-trigger="load"` regardless of the
//!   button, only the *panel* visibility depended on the handler — and A,
//!   C, D are all untouched.
//! - Adding `target="dl-x"` to the reachable row's anchor in
//!   `worktrees_strip` fails 2: the href/target pair assertion directly,
//!   and — cascading — section C's navigation check, because a click on a
//!   targeted anchor opens a new browsing context instead of navigating
//!   this tab, which is exactly the bug the missing `target=` prevents.
//!
//! Run: deno run -A tests/browser/worktrees.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const git = async (cwd, ...args) => {
  const p = new Deno.Command("git", { args, cwd, stdout: "null", stderr: "piped" }).spawn();
  const st = await p.status;
  if (!st.success) throw new Error(`git ${args.join(" ")} failed in ${cwd}`);
};

const fx = await fixture();
const proj = `${fx.roots}/proj`;
await Deno.writeTextFile(`${proj}/README.md`, "hello\n");
await git(proj, "init", "-b", "master");
await git(proj, "-c", "user.email=t@t", "-c", "user.name=t", "add", "README.md");
await git(proj, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "init");
// The path Claude Code itself uses for worktrees, which projects.rs vouches for.
await git(proj, "worktree", "add", "-b", "feature-x", ".claude/worktrees/feat");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app");

  // See the header comment: the registry only knows a project once it has a
  // live session or a saved layout, so the main worktree needs one before
  // the family has two members for the chip to react to.
  await evalIn(`document.querySelector(".termstart").click()`);
  ok(await until(() => evalIn(`!!document.querySelector(".termhost")`), 15, "terminal"),
     "a terminal is live in the main worktree");

  console.log("A. the chip knows it has company");
  ok(await until(() => evalIn(`document.getElementById("wtlabel").textContent.includes("main worktree")`),
     15, "wtlabel filled"), "the chip label names the main worktree");

  console.log("\nB. the panel lists the idle worktree");
  await evalIn(`document.getElementById("wtbtn").click()`);
  ok(await until(() => evalIn(`!document.getElementById("wtpanel").hidden`), 10, "panel open"),
     "clicking the chip opens the panel");
  ok(await until(() => evalIn(
      `[...document.querySelectorAll("#wtpanel .wt")].some((a) => a.textContent.includes("feat"))`),
     10, "worktree row"), "the idle worktree is listed");
  ok(await evalIn(
      `[...document.querySelectorAll("#wtpanel .wt")].some((a) => a.textContent.includes("feature-x"))`),
     "with its branch");
  // The ⌘-click behaviour IS the missing target= — pin the pair.
  ok(await evalIn(
      `[...document.querySelectorAll("#wtpanel a.wt")].every((a) => a.getAttribute("href") && !a.hasAttribute("target"))`),
     "rows have href and no target");
  // Optional chaining rather than a bare querySelector: a revert that drops
  // every row (Step 3's first revert-check) must fail this assertion, not
  // crash the run on a null dereference before it can be counted.
  ok(await evalIn(
      `document.querySelector("#wtpanel .wt.current")?.textContent.includes("proj") ?? false`),
     "the current row is the main worktree");

  console.log("\nC. a plain click switches this tab");
  await evalIn(`[...document.querySelectorAll("#wtpanel a.wt")].find((a) => a.textContent.includes("feat"))?.click()`);
  // Measured: evalIn stayed live across this same-tab navigation on this
  // Chromium (a new default execution context arrives on the same CDP
  // session), so the direct check is tried first. The fallback below —
  // reopening the page handle at the destination URL — is the harness's
  // documented escape if a future Chromium version makes evalIn go stale
  // instead, per the brief's note.
  let navigated = await until(() => evalIn(`location.pathname.endsWith("/worktrees/feat")`), 15, "navigated");
  if (!navigated) {
    page.close();
    page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
    navigated = await until(() => page.evalIn(`location.pathname.endsWith("/worktrees/feat")`), 15, "navigated (reopened)");
  }
  ok(navigated, "the same tab now shows the worktree's workspace");

  console.log("\nC2. the destination chip names the worktree we switched to");
  // Reuse the app-ready `until` pattern, via the CURRENT page.evalIn — the
  // handle may have been reopened above if evalIn went stale on navigation.
  await until(() => page.evalIn("typeof terms !== 'undefined' && !!state"), 30, "app on destination");
  ok(await until(() => page.evalIn(
      `document.getElementById("wtlabel").textContent.includes("· feat")`),
     15, "wtlabel filled on destination"),
     "the destination chip's label names the child worktree (fragment needs a load; " +
     "also exercises the percent-encoded child key round-trip in a real browser)");
  await page.evalIn(`document.getElementById("wtbtn").click()`);
  ok(await until(() => page.evalIn(
      `document.querySelector("#wtpanel .wt.current")?.textContent.includes("feat") ?? false`),
     10, "current row on destination"),
     "the destination panel's current row is the child worktree");

  console.log("\nD. the launcher carries the Claude mark");
  await until(() => page.evalIn("typeof terms !== 'undefined' && !!state"), 30, "app again");
  ok(await until(() => page.evalIn(
      `!!document.querySelector('.tabstrip .newclaude svg[fill="#D97757"]')`),
     10, "mark"), "the ✻ button is an SVG mark with the Claude brand fill now");
} finally {
  try { page?.close(); } catch {}
  browser.close();
  await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
