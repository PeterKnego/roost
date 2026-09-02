//! The Changes pane and the header chip follow the working tree.
//!
//! Both render `git status`, and both used to refresh only when a write
//! landed on `.git/index` or `.git/HEAD` — git's own internals, which an
//! ordinary edit never touches. So a file modified after the last git command
//! was missing from the pane whose job is to list it, while the full diff
//! beside it — recomputed per request — showed the change. That is exactly how
//! it was reported from a live instance.
//!
//! Only a browser can see this. The server's `git status` parsing, the
//! fragment it renders, and the watcher's classification are all covered by
//! Rust tests and were all correct; what was missing was an event reaching
//! the client, and app.js is where that lands.
//!
//! Every edit here is made *outside* the browser — the file is written on
//! disk, as a Claude in a terminal pane writes it, which is roost's core use
//! case and the case with no user gesture to hang a refresh on.
//!
//! Run: deno run -A tests/browser/changes.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const git = async (...args) => {
  const out = await new Deno.Command("git", {
    args: ["-c", "user.email=t@t", "-c", "user.name=t", ...args],
    cwd: `${fx.roots}/proj`, stdout: "null", stderr: "piped",
  }).output();
  if (!out.success) throw new Error(`git ${args.join(" ")}: ${new TextDecoder().decode(out.stderr)}`);
};
// A committed baseline, so the pane starts genuinely clean: without this every
// assertion below would be true of a tree that was already dirty at load, and
// "the row appeared" would prove nothing about refreshing.
await Deno.writeTextFile(`${fx.roots}/proj/tracked.md`, "one\n");
await Deno.writeTextFile(`${fx.roots}/proj/opened.md`, "one\n");
await git("add", "-A");
await git("commit", "-qm", "baseline");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  // Scoped to whichever pane actually holds the Changes tab rather than a
  // hard-coded index: the default layout puts it bottom-left, but a test that
  // asserted on pane 1 would silently start asserting on an empty pane if that
  // ever moved.
  const paneSel = `'.pane[data-pane="' + state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Changes")) + '"] .content'`;
  const rows = async () => JSON.parse(await evalIn(
    `JSON.stringify([...document.querySelectorAll(${paneSel} + " ul.changes li a.file")]
       .filter((a) => a.dataset.rel !== "")
       .map((a) => ({ rel: a.dataset.rel, xy: (a.querySelector(".xy") || {}).textContent || "" })))`,
  ));
  const paneText = () => evalIn(`(document.querySelector(${paneSel}) || {}).textContent || ""`);
  const row = async (rel) => (await rows()).find((r) => r.rel === rel) || null;
  const chip = async () => JSON.parse(await evalIn(
    `(() => { const g = document.getElementById("gitinfo");
      const b = g && g.querySelector(".gbullet");
      const m = g && g.querySelector(".gmod");
      return JSON.stringify({ bullet: b ? b.className : null, mod: m ? m.textContent : null }); })()`,
  ));

  console.log("A. a clean tree, before anything is touched");
  ok(await until(async () => (await paneText()).includes("working tree clean"), 10, "the clean pane"),
     "the pane starts out saying the tree is clean");
  ok((await chip()).bullet === "gbullet clean", "and the header bullet agrees");

  // Settle first, and not for tidiness: serving the pane runs `git status`,
  // which refreshes the index's stat cache and so *writes* `.git/index` —
  // the one event that already refreshed this pane. Editing a file while
  // that round trip is still in flight lets the load's own refresh carry the
  // edit into the pane, and section B passes with the fix reverted. Verified:
  // without this wait B is the only section that survives the revert.
  await new Promise((r) => setTimeout(r, 2000));

  console.log("\nB. a committed file, edited on disk with no browser involved");
  await Deno.writeTextFile(`${fx.roots}/proj/tracked.md`, "one\ntwo\n");
  ok(await until(async () => (await row("tracked.md")) !== null, 10, "the tracked.md row"),
     "the modified file appears in the pane without a reload");
  ok((await row("tracked.md"))?.xy?.includes("M"), "and carries git's own status code");
  ok(await until(async () => (await chip()).mod === "~1", 10, "the chip's ~1"),
     "the header chip counts it too");
  ok((await chip()).bullet === "gbullet dirty", "and its bullet turns dirty");

  console.log("\nC. a file created on disk");
  await Deno.writeTextFile(`${fx.roots}/proj/fresh.txt`, "new\n");
  ok(await until(async () => (await row("fresh.txt")) !== null, 10, "the fresh.txt row"),
     "an untracked file appears too");
  ok((await row("fresh.txt"))?.xy === "??", "marked untracked");

  console.log("\nD. a file that is open in a tab — the reported case");
  // A file with an open tab classifies as Class::Buffer in the watcher, which
  // broadcasts the file's own text and nothing else: before the fix this
  // branch refreshed no listing at all, which is why the reported file
  // (open in a tab, edited underneath it) went missing while its neighbours
  // were listed.
  const clicked = await evalIn(`(() => {
    const a = [...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .find((x) => x.dataset.rel === "opened.md");
    if (!a) return false;
    a.dispatchEvent(new MouseEvent("click", { bubbles: true })); return true;
  })()`);
  ok(clicked, "opened.md opens from the tree");
  ok(await until(() => evalIn(`state.panes.some((p) => p.tabs.some((t) => t.k === "File" && t.rel === "opened.md"))`),
                 10, "the opened.md tab"),
     "it has a tab, so the watcher classifies it as a buffer");
  await Deno.writeTextFile(`${fx.roots}/proj/opened.md`, "one\nedited underneath\n");
  ok(await until(async () => (await row("opened.md")) !== null, 10, "the opened.md row"),
     "editing it underneath the tab lists it");

  console.log("\nE. and the build directory does not drag git status behind it");
  // The boundary the fix deliberately keeps: `target/` is where a `cargo
  // build` writes thousands of files, and refreshing on those would put a
  // `git status` subprocess behind every batch of them. The control write
  // afterwards is what makes this silence mean something — without it this
  // section passes just as well with the watcher dead.
  await Deno.mkdir(`${fx.roots}/proj/target`, { recursive: true });
  await Deno.writeTextFile(`${fx.roots}/proj/target/junk.o`, "x\n");
  await new Promise((r) => setTimeout(r, 1500));
  ok(!(await rows()).some((r) => r.rel.startsWith("target")), "nothing under target/ reaches the pane");
  await Deno.writeTextFile(`${fx.roots}/proj/control.txt`, "still watching\n");
  ok(await until(async () => (await row("control.txt")) !== null, 10, "the control row"),
     "while an ordinary file still arrives — the watcher was alive throughout");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
