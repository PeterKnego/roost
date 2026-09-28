//! Revert from the Changes pane and a Diff tab (#125).
//!
//! Right-clicking a change used to open the tree's file menu, Delete
//! included. This drives the real menus against a real repository and
//! asserts on the file on disk, never on the UI alone: a dialog that
//! confirmed and did nothing would pass any assertion about dialogs.
//!
//! Run: deno run -A tests/browser/revert.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();
const git = async (dir, ...args) => {
  const o = await new Deno.Command("git", { args: ["-C", dir, ...args], stdout: "piped", stderr: "piped" }).output();
  if (!o.success) throw new Error(`git ${args.join(" ")}: ${new TextDecoder().decode(o.stderr)}`);
  return new TextDecoder().decode(o.stdout);
};
const read = (p) => Deno.readTextFile(p);

const fx = await fixture();
await git(fx.dir, "config", "user.email", "t@example.com");
await git(fx.dir, "config", "user.name", "t");
await Deno.writeFile(`${fx.dir}/a.txt`, enc.encode("base\n"));
await Deno.writeFile(`${fx.dir}/b.txt`, enc.encode("base\n"));
// A third committed file, reverted only in section C, so that section's
// "changed nothing" claim cannot be masked by D also using a.txt (fix
// round 1, Important 2): if C ever really discarded a.txt, D's own assertions
// would keep passing off C's leftovers with nothing to tell the two apart.
await Deno.writeFile(`${fx.dir}/c.txt`, enc.encode("base\n"));
await git(fx.dir, "add", "-A");
await git(fx.dir, "commit", "-qm", "base");
await Deno.writeFile(`${fx.dir}/a.txt`, enc.encode("changed\n"));
await Deno.writeFile(`${fx.dir}/b.txt`, enc.encode("changed\n"));
await Deno.writeFile(`${fx.dir}/c.txt`, enc.encode("changed\n"));
await Deno.writeFile(`${fx.dir}/new.txt`, enc.encode("untracked\n"));

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn, cmd } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");

  // Open Changes in pane 0 and wait for its rows. `default_layout` already
  // seeds Changes at pane 1 (LEFT_BOTTOM) and Tree at pane 0 (LEFT_TOP), and
  // `apply_layout`'s OpenTab dedupes by tab identity across every pane
  // (`Workspace::find_tab`), so this activates the existing tab rather than
  // opening a duplicate — verified by reading `src/workspace.rs` before
  // writing this, not assumed.
  await evalIn(`send({ t: "OpenTab", pane: 0, tab: { k: "Changes" } })`);
  const row = (rel) => `document.querySelector('.content[data-kind="Changes"] a[data-rel=${JSON.stringify(rel)}]')`;
  ok(await until(() => evalIn(`!!${row("a.txt")} && !!${row("c.txt")} && !!${row("new.txt")}`), 20, "change rows"),
     "Changes lists the four files");

  // `el` can legitimately be null here: a revert-check that makes an earlier
  // section discard a file for real (e.g. C's cancel silently confirming)
  // removes its row from the Changes pane, so a *later* section's own
  // right-click on that same row must report "nothing to click", not throw
  // and abort every section after it.
  const rightClick = (sel) => evalIn(`(() => { const el = ${sel}; if (!el) return false; const r = el.getBoundingClientRect();
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 5, clientY: r.y + 5 })); return true; })()`);
  const menuItems = () => evalIn(`[...document.querySelectorAll("#dlg-menu[open] .dlg-item")].map((b) => ({ t: b.firstChild.textContent, d: b.disabled, h: b.querySelector(".dlg-hint")?.textContent || "" }))`);
  // NOT `el.close()`: dialog.js's own `runDialog` only clears its one-dialog-
  // at-a-time gate (`openDlg`) from inside `finish`, which a `cancel` event,
  // a backdrop click or an item click reaches — a bare `.close()` bypasses
  // all three, so `openDlg` stays set to the just-closed element and every
  // later `askMenu`/`askChoice` call silently resolves dismissed without
  // ever opening. Seen live: with `.close()` here, section B's contextmenu
  // produced no menu at all (`items` came back `[]`) and section C then threw
  // on a null `.dlg-item`. A real Escape keypress, `dialogs.mjs`'s own way of
  // dismissing a menu, is what the app is actually driven by.
  const closeMenu = async () => {
    await cmd("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await cmd("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  };

  console.log("A. the menu on a change row");
  await rightClick(row("a.txt"));
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  let items = await menuItems();
  ok(items.some((i) => i.t === "Revert…" && !i.d), `a change row offers Revert… (${JSON.stringify(items)})`);
  ok(!items.some((i) => /Delete|New file|Rename/.test(i.t)), "and none of the file menu");
  await closeMenu();

  console.log("\nB. an untracked row says why not");
  await rightClick(row("new.txt"));
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Revert…" && i.d && i.h.includes("untracked")), `disabled with its reason (${JSON.stringify(items)})`);
  await closeMenu();

  // `?.click()`, not an unguarded one: a revert-check that breaks the menu
  // itself (e.g. dropping the Changes/Diff branch in `wireFragment`) leaves
  // no enabled `.dlg-item` here, and an unguarded `.click()` throws and
  // aborts the whole run — which would hide every assertion after the
  // section a given break was never expected to touch. A guarded click that
  // finds nothing simply leaves the confirmation unopened, which the `until`
  // below reports as a normal, named `ok(false, ...)`.
  const openConfirm = async (rel) => {
    await rightClick(row(rel));
    await until(() => evalIn(`!!document.querySelector("#dlg-menu[open] .dlg-item:not(:disabled)")`), 5, "menu");
    await evalIn(`document.querySelector("#dlg-menu[open] .dlg-item:not(:disabled)")?.click()`);
    return await until(() => evalIn(`!!document.querySelector("#dlg-choice[open]")`), 10, "confirmation");
  };

  console.log("\nC. cancel, and Enter, change nothing");
  // Its own file, c.txt, never touched by any other section — so a break
  // that made this section *actually* discard the file cannot be masked by
  // D re-reverting the same path and reporting success anyway (fix round 1,
  // Important 2).
  ok(await openConfirm("c.txt"), "Revert… opens a confirmation");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-choice .dlg-cancel")`), "focus is on Cancel");
  ok(await evalIn(`document.querySelector("#dlg-choice .dlg-detail").textContent.includes("changed")`), "it shows the diff");
  // A trusted CDP key event, not `dispatchEvent(new KeyboardEvent(...))`:
  // askChoice installs no keydown handler of its own, and a browser only
  // applies its native "Enter activates the focused button" behaviour to a
  // TRUSTED event — a synthetic one is silently ignored. The untrusted form
  // proved nothing about Enter; the `.click()` that used to follow it was
  // the only thing actually exercised (fix round 1, Important 1). Escape
  // uses the same trusted mechanism in `closeMenu` above.
  await cmd("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
  await cmd("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  ok(await until(() => evalIn(`!document.querySelector("#dlg-choice[open]")`), 5, "dialog closed"),
     "Enter (focused on Cancel) closed the confirmation");
  // The dialog closing proves nothing about which button Enter actually hit
  // — under `focus: "first"` it lands on Discard, which *also* closes the
  // dialog, and `Revert` is then a websocket round trip the server answers
  // asynchronously. Reading the file immediately after "closed" races that
  // round trip and can read "changed\n" out of sheer luck before the write
  // lands — seen live during this fix's own revert-check, where this
  // assertion passed under the break while the very next section proved the
  // file really had been discarded. Settling past the round trip first is
  // what makes the read mean something.
  await sleep(800);
  ok((await read(`${fx.dir}/c.txt`)) === "changed\n", "Enter on the confirmation left the file untouched");

  // A separate, separately named check that an explicit Cancel click is
  // equally inert — distinct from the Enter path above, which exercises
  // focus rather than the button itself.
  ok(await openConfirm("c.txt"), "Revert… opens a confirmation again");
  await evalIn(`document.querySelector("#dlg-choice .dlg-cancel").click()`);
  ok(await until(() => evalIn(`!document.querySelector("#dlg-choice[open]")`), 5, "dialog closed"),
     "clicking Cancel closed the confirmation");
  await sleep(800);
  ok((await read(`${fx.dir}/c.txt`)) === "changed\n", "clicking Cancel left the file on disk untouched");

  console.log("\nD. confirm reverts on disk and names the stash");
  // Counted rather than string-matched, and independent of C: C now reverts
  // nothing (it uses c.txt, never a.txt), so this count is not D reading a
  // stash C already made (fix round 1, Important 2) — it is proof that THIS
  // confirmation is what created exactly one new entry.
  const stashLines = async () => (await git(fx.dir, "stash", "list")).split("\n").filter((l) => l.length > 0);
  const stashBefore = (await stashLines()).length;
  await openConfirm("a.txt");
  await evalIn(`document.querySelector('#dlg-choice .dlg-choice[data-choice="discard"]')?.click()`);
  ok(await until(async () => (await read(`${fx.dir}/a.txt`)) === "base\n", 15, "reverted"), "a.txt is back at HEAD on disk");
  ok((await read(`${fx.dir}/b.txt`)) === "changed\n", "b.txt, not selected, is untouched");
  await until(async () => (await stashLines()).length === stashBefore + 1, 15, "a new stash entry");
  const stashAfter = await stashLines();
  ok(stashAfter.length === stashBefore + 1,
     `the stash grew by exactly one entry (before ${stashBefore}, after ${stashAfter.length})`);
  ok(!!stashAfter[0] && stashAfter[0].includes("roost revert: 1 file"),
     `the new top stash entry names the revert (${JSON.stringify(stashAfter[0])})`);
  ok(await until(() => evalIn(`[...document.querySelectorAll(".error-banner")].some((b) => b.textContent.includes("stash@{0}"))`), 10, "banner"),
     "the banner names the stash");
  ok(await until(() => evalIn(`!${row("a.txt")}`), 15, "row gone"), "a.txt left the Changes list");

  console.log("\nE. a Diff tab gets the git menu, not the file menu");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "Diff", rel: "b.txt" } })`);
  await until(() => evalIn(`!!document.querySelector('.content[data-kind="Diff"] .diffview')`), 15, "diff tab");
  await rightClick(`document.querySelector('.content[data-kind="Diff"] .diffview')`);
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Revert…") && !items.some((i) => /New file/.test(i.t)), `Diff: ${JSON.stringify(items)}`);
  await closeMenu();

  console.log("\nF. the tree keeps its file menu");
  await evalIn(`send({ t: "OpenTab", pane: 0, tab: { k: "Tree" } })`);
  const treeRow = `document.querySelector('ul.tree a[data-rel="b.txt"]')`;
  await until(() => evalIn(`!!${treeRow}`), 15, "tree row");
  await rightClick(treeRow);
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Delete") && !items.some((i) => i.t.startsWith("Revert")), `tree: ${JSON.stringify(items)}`);
  await closeMenu();
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
