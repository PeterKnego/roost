//! Alt+K mentions the files picked in the tree, not only the file in the
//! active tab.
//!
//! All of this is `static/app.js` — the picking gesture, the state, the
//! ordering, the cap, and which of two possible targets wins — and none of it
//! is reachable from `cargo test`. The server is unchanged: each pick becomes
//! an ordinary `Intent::MentionPath`, which already existed.
//!
//! Traps this file is written against (see README):
//!
//!   - **The intents are captured, not inferred from the terminal.** What
//!     matters is which paths were sent and in what order; reading them back
//!     out of a shell's rendered output would be asserting about xterm.
//!   - **Section B proves plain click still opens.** Adding a gesture to the
//!     tree is only acceptable if the tree's primary job is untouched, and a
//!     test that only exercises ctrl-click would not notice it breaking.
//!   - **Section D picks in a deliberately scrambled order** and asserts the
//!     result comes back in *tree* order. Picking top-to-bottom would pass
//!     against no ordering logic at all.
//!   - **Section E asserts the tab route still works after the picks are
//!     spent.** "The pick wins" and "the pick is the only thing that works"
//!     are different features, and only clearing-then-retrying tells them
//!     apart.
//!
//! Run: deno run -A tests/browser/treemention.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.roots}/proj/src`, { recursive: true });
for (const f of ["alpha.rs", "beta.rs", "gamma.rs", "delta.rs"]) {
  await Deno.writeTextFile(`${fx.roots}/proj/src/${f}`, `// ${f}\n`);
}
// Twenty more, so the cap can be tripped by one shift-range.
for (let i = 0; i < 20; i++) {
  await Deno.writeTextFile(`${fx.roots}/proj/src/bulk${String(i).padStart(2, "0")}.rs`, "//\n");
}

const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof send === 'function' && !!state"), 20, "app.js");

  const TREE = `document.querySelector('.pane[data-pane="0"] .content')`;
  // Captures the intents this page sends, which is the whole observable
  // contract: the server side of MentionPath already has its own tests.
  await evalIn(`window.__sent = []; const __s = send;
    window.send = (i) => { window.__sent.push(i); return __s(i); }; 0`);
  const sent = async () => JSON.parse(await evalIn(`JSON.stringify(window.__sent)`));
  const resetSent = async () => await evalIn(`window.__sent = []; 0`);
  const mentions = async () => (await sent()).filter((i) => i.t === "MentionPath").map((i) => i.rel);
  const picked = async () => JSON.parse(await evalIn(
    `JSON.stringify([...${TREE}.querySelectorAll("a.file.picked")].map((a) => a.dataset.rel))`));

  const expand = async (rel) => {
    await evalIn(`(() => { const d = ${TREE}.querySelector('details[data-rel="${rel}"]');
      if (d) d.open = true; return 0; })()`);
    return await until(async () => await evalIn(
      `!!${TREE}.querySelector('a.file[data-rel="${rel}/alpha.rs"]')`), 10, `${rel} children`);
  };
  const clickRow = async (rel, mods) => await evalIn(`(() => {
    const a = ${TREE}.querySelector('a.file[data-rel="${rel}"]');
    if (!a) throw new Error("no row " + ${JSON.stringify(rel)});
    a.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, ...${JSON.stringify(mods)} }));
    return 0; })()`);
  const altK = async () => await evalIn(`(() => {
    document.dispatchEvent(new KeyboardEvent("keydown",
      { key: "k", code: "KeyK", keyCode: 75, altKey: true, bubbles: true, cancelable: true }));
    return 0; })()`);

  await until(async () => await evalIn(`!!${TREE}.querySelector("ul.tree")`), 10, "tree");
  ok(await expand("src"), "setup: src is expanded so its files have rows");

  console.log("\nA. picking rows in the tree");
  ok((await picked()).length === 0, "setup: nothing is picked to begin with");
  await clickRow("src/alpha.rs", { ctrlKey: true });
  await clickRow("src/gamma.rs", { ctrlKey: true });
  ok(JSON.stringify(await picked()) === '["src/alpha.rs","src/gamma.rs"]',
    `ctrl-click picks rows — got ${JSON.stringify(await picked())}`);
  // A second ctrl-click takes it back, or a mis-pick is unrecoverable without
  // reloading.
  await clickRow("src/gamma.rs", { ctrlKey: true });
  ok(JSON.stringify(await picked()) === '["src/alpha.rs"]',
    `ctrl-clicking a picked row unpicks it — got ${JSON.stringify(await picked())}`);

  console.log("\nB. plain click still opens — the tree's primary job");
  await resetSent();
  await clickRow("src/beta.rs", {});
  ok(
    await until(async () => (await sent()).some((i) => i.t === "OpenTab" && i.tab && i.tab.rel === "src/beta.rs"), 5, "opened"),
    "an unmodified click still opens the file",
  );
  ok(!(await picked()).includes("src/beta.rs"), "and does not pick it");
  ok((await picked()).includes("src/alpha.rs"), "leaving the existing picks alone");

  console.log("\nC. Alt+K sends one MentionPath per picked file");
  await clickRow("src/gamma.rs", { ctrlKey: true });
  await resetSent();
  await altK();
  const got = await until(async () => (await mentions()).length === 2, 5, "two mentions")
    ? await mentions() : await mentions();
  ok(JSON.stringify(got) === '["src/alpha.rs","src/gamma.rs"]',
    `both picked files are mentioned — got ${JSON.stringify(got)}`);
  ok((await sent()).filter((i) => i.t === "MentionPath").every((i) => i.line_start === null && i.line_end === null),
    "with no line range: a tree row names a file, not a region of one");
  // Spent, so a forgotten selection cannot hijack the next Alt+K.
  ok((await picked()).length === 0, `the picks are cleared once sent — got ${JSON.stringify(await picked())}`);

  console.log("\nD. shift extends a range, and order follows the tree");
  await resetSent();
  // Deliberately backwards — anchor at the *later* row and extend upwards.
  // Picking top-to-bottom would pass against no ordering logic at all.
  //
  // A short range on purpose: an earlier draft anchored at delta.rs and
  // extended to alpha.rs, which spans the twenty bulk files as well, so the
  // cap in section F truncated it and this assertion failed about ordering
  // when ordering was correct. Ordering and the cap are separate claims and
  // each gets a fixture that cannot trip the other.
  await clickRow("src/delta.rs", { ctrlKey: true });
  await clickRow("src/bulk17.rs", { shiftKey: true });
  const range = await picked();
  ok(range.length > 2 && range.length < 16,
    `shift-click extends a range, below the cap — got ${range.length} rows`);
  await altK();
  const ordered = await mentions();
  const treeOrder = JSON.parse(await evalIn(
    `JSON.stringify([...${TREE}.querySelectorAll("a.file[data-rel]")].map((a) => a.dataset.rel))`));
  const expected = treeOrder.filter((r) => range.includes(r));
  ok(JSON.stringify(ordered) === JSON.stringify(expected),
    `mentions arrive in tree order, not click order — got ${JSON.stringify(ordered)}`);

  console.log("\nE. the active-tab route is untouched");
  await resetSent();
  ok((await picked()).length === 0, "setup: no picks are left over");
  // src/beta.rs was opened in section B, so it is the active File tab.
  await altK();
  const tabMentions = await until(async () => (await mentions()).length === 1, 5, "tab mention")
    ? await mentions() : await mentions();
  ok(JSON.stringify(tabMentions) === '["src/beta.rs"]',
    `with nothing picked, Alt+K still mentions the active tab — got ${JSON.stringify(tabMentions)}`);

  console.log("\nF. the cap names itself");
  await resetSent();
  await evalIn(`window.__errors = []; const __se = showError;
    window.showError = (m) => { window.__errors.push(m); return __se(m); }; 0`);
  const rows = JSON.parse(await evalIn(
    `JSON.stringify([...${TREE}.querySelectorAll('a.file[data-rel]')].map((a) => a.dataset.rel))`));
  ok(rows.length > 16, `setup: the directory holds ${rows.length} rows, more than the cap`);
  await clickRow(rows[0], { ctrlKey: true });
  await clickRow(rows[rows.length - 1], { shiftKey: true });
  ok((await picked()).length === rows.length, "setup: every row is picked");
  await altK();
  const capped = await mentions();
  ok(capped.length === 16, `at most sixteen are sent — got ${capped.length}`);
  const errs = JSON.parse(await evalIn(`JSON.stringify(window.__errors)`));
  ok(
    errs.some((m) => m.includes("16") && m.includes(String(rows.length))),
    `and it says how many it dropped — got ${JSON.stringify(errs)}`,
  );
} finally {
  if (page) page.close();
  browser.close();
  await roost.close();
  // Not optional, and not tidiness. `fixture()`'s cleanup is what runs
  // `killByCmdline(stateDir)` and removes the temp tree, and the default
  // layout gives the right pane a Terminal tab — so this test starts a real
  // dtach master and its login shell, and without this they outlive it.
  // harness.mjs records what that looks like: "Two /tmp/roost-browser-* trees
  // were once found abandoned on a live host, one of them still holding a
  // running dtach master and its login shell." Measured on this host after a
  // day of runs: 74 abandoned trees and 9 live shells.
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
