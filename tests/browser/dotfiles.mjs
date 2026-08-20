//! The tree pane's dotfile toggle: ◌ / ◍ in the pane header.
//!
//! Worth a browser test rather than a Rust one for the reason paneicons.mjs
//! is: the intent behind it (SetShowHidden) and the filtering it drives are
//! both covered server-side, and all of that can be correct while the button
//! sends nothing, draws the wrong glyph, or leaves the tree showing the
//! listing it fetched before the toggle — none of which any Rust test can see.
//!
//! Three client paths are distinct here and each gets its own assertion: the
//! live update in the browser that clicked, the mirrored update in one that
//! did not, and the *initial* render of a page opened afterwards, which
//! resolves the same value from a State snapshot instead of from an event.
//!
//! Run: deno run -A tests/browser/dotfiles.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// `fixture` already git-inits the project, so .git is a real dot-directory
// here rather than one this test had to fabricate.
await Deno.mkdir(`${fx.roots}/proj/src`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/proj/src/main.rs`, "fn main() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/.gitignore`, "target\n");
await Deno.mkdir(`${fx.roots}/proj/.config`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/proj/.config/notes.txt`, "hi\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${resh.port}/proj`;
let one, two, three;

// A page's tree pane, its rows, and its toggle. Every helper is scoped to one
// page object, because the whole point of the mirroring assertions is that two
// pages can disagree.
const wire = (page) => {
  const { evalIn } = page;
  const pane = () => evalIn(`state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"))`);
  const rows = async () => JSON.parse(await evalIn(
    `(async () => { const pi = state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"));
      return JSON.stringify([...document.querySelectorAll('.pane[data-pane="' + pi + '"] .content [data-rel]')]
        .map((n) => n.dataset.rel)); })()`,
  ));
  const toggle = async () => JSON.parse(await evalIn(
    `(() => { const pi = state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"));
      const b = [...document.querySelectorAll('.pane[data-pane="' + pi + '"] .paneicons .paneicon')]
        .find((x) => /dotfiles/.test(x.title));
      return JSON.stringify(b ? { glyph: b.textContent, title: b.title } : null); })()`,
  ));
  // The click goes through the real element, not through send(): a control
  // wired to nothing is exactly the defect this file exists to catch.
  const click = () => evalIn(
    `(() => { const pi = state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"));
      const b = [...document.querySelectorAll('.pane[data-pane="' + pi + '"] .paneicons .paneicon')]
        .find((x) => /dotfiles/.test(x.title));
      if (!b) return false; b.click(); return true; })()`,
  );
  // Pins TreeChanged shut for the rest of the run. Not cosmetic: a watcher
  // event storm (measured at ~3/s on an idle project, on this commit and on
  // the one before this feature — a pre-existing defect, not this toggle's)
  // re-fetches the tree several times a second all by itself. Left alone, it
  // refreshes the listing for us and every assertion below passes with the
  // toggle's own refresh path deleted — verified by deleting it. Muting the
  // unrelated stimulus is what makes the click the only thing that can put a
  // dot row on screen.
  const muteTreeChanged = () => evalIn(
    `(() => { const oe = onEvent; window.onEvent = (e) => { if (e.t === "TreeChanged") return; return oe(e); }; })()`,
  );
  const ready = () => until(
    () => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state && !!document.querySelector('ul.tree')"),
    30, "app",
  );
  const has = async (rel) => (await rows()).includes(rel);
  return { evalIn, pane, rows, toggle, click, ready, has, muteTreeChanged, close: page.close };
};

try {
  one = wire(await openPage(browser.port, url));
  two = wire(await openPage(browser.port, url));
  ok(await one.ready(), "page one is up");
  ok(await two.ready(), "page two is up on the same project");
  await one.muteTreeChanged();
  await two.muteTreeChanged();

  console.log("A. hidden by default, and the control says so");
  ok(await one.has("hello.md"), "an ordinary file renders");
  ok(await one.has("src"), "and an ordinary directory");
  ok(!(await one.has(".gitignore")), "a dotfile does not");
  ok(!(await one.has(".git")), "nor does .git");
  ok(!(await one.has(".config")), "nor a dot-directory");
  {
    const t = await one.toggle();
    // Glyph asserted explicitly: strip tests with no glyph assertion are the
    // exact failure CLAUDE.md records — swapping ● and ○ left them all green.
    ok(t !== null, "the tree pane offers a dotfile control");
    ok(t?.glyph === "◌", `it draws the hollow ring while hidden (got ${t?.glyph})`);
    ok(t?.title === "show dotfiles", `and offers to show (got ${JSON.stringify(t?.title)})`);
  }

  console.log("\nB. clicking it reveals them in this page");
  ok(await one.click(), "the control is clickable");
  ok(await until(() => one.has(".gitignore"), 10, "dotfile row"), "the dotfile appears");
  ok(await one.has(".git"), ".git appears too — all dot entries, not a curated subset");
  ok(await one.has(".config"), "and the dot-directory");
  ok(await one.has("hello.md"), "and the ordinary rows are still there");
  {
    const t = await one.toggle();
    ok(t?.glyph === "◍", `the glyph fills in (got ${t?.glyph})`);
    ok(t?.title === "hide dotfiles", `and now offers to hide (got ${JSON.stringify(t?.title)})`);
  }

  console.log("\nC. and in the browser that did not click");
  ok(await until(() => two.has(".gitignore"), 10, "mirrored dotfile row"),
     "page two updated without being touched");
  ok((await two.toggle())?.glyph === "◍", "its control shows the new state too");

  console.log("\nD. a page opened afterwards renders it from the snapshot");
  three = wire(await openPage(browser.port, url));
  ok(await three.ready(), "page three is up");
  await three.muteTreeChanged();
  // Distinct path: this page never saw a SetShowHidden event, only the State
  // it connected with. A client that applied the value on events alone would
  // pass B and C and fail here.
  ok(await three.has(".gitignore"), "its first render already includes dot rows");
  ok((await three.toggle())?.glyph === "◍", "and its control agrees");

  console.log("\nE. expanded directories survive the toggle");
  await one.evalIn(
    `(() => { const pi = state.panes.findIndex((p) => p.tabs.some((t) => t.k === "Tree"));
      const d = [...document.querySelectorAll('.pane[data-pane="' + pi + '"] .content details')]
        .find((x) => x.dataset.rel === "src");
      if (d) d.open = true; })()`,
  );
  const openBefore = await one.evalIn(
    `document.querySelectorAll('.pane .content details[open][data-rel="src"]').length`);
  ok(openBefore === 1, "src is expanded before the toggle");
  ok(await one.click(), "toggle it back off");
  ok(await until(async () => !(await one.has(".gitignore")), 10, "dotfiles gone"), "the dot rows go away");
  ok(await one.has("hello.md"), "without emptying the tree");
  // A remount would have collapsed this; refreshTree reconciles in place.
  ok(await one.evalIn(`document.querySelectorAll('.pane .content details[open][data-rel="src"]').length`) === 1,
     "and src is still expanded — the tree was reconciled, not rebuilt");
  ok((await one.toggle())?.glyph === "◌", "the glyph is hollow again");

  console.log("\nF. the control belongs to the tree pane only");
  const elsewhere = await one.evalIn(
    `[...document.querySelectorAll('.pane[data-pane="3"] .paneicons .paneicon')].some((x) => /dotfiles/.test(x.title))`);
  ok(!elsewhere, "a pane showing no tree offers no dotfile control");
} finally {
  [one, two, three].forEach((p) => p?.close());
  browser.close();
  await resh.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
