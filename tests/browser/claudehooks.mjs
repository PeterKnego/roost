//! The bell's Claude-hooks switch: the mark on the bell, the row in the
//! panel, the confirmation, and the write it ends in.
//!
//! Worth a browser test for the reason dotfiles.mjs is: the intent and the
//! file rewrite are covered server-side (claudehooks.rs, integration.rs),
//! and all of that can be right while the row sends nothing, the mark draws
//! the wrong state, or a second browser on the project never learns of the
//! flip. Three client paths get their own assertion: the browser that
//! clicked, one that did not, and the unknown state, which must show a
//! reason and no button.
//!
//! The four traps in README.md apply: every assertion names an element or
//! a file, and the file is read from disk, not inferred from the DOM.
//!
//! Revert-the-fix, watched fail and restored:
//!   1. Made the Enable button send nothing (commented out the send in
//!      app.js). "the settings file now holds both events" failed.
//!   2. Removed the `data-claude-hooks` assignment. "the bell is marked
//!      off before enabling" failed.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const settings = `${fx.roots}/proj/.claude/settings.local.json`;
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${roost.port}/proj`;

const wire = (page) => {
  const { evalIn } = page;
  const ready = () => until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app");
  const mark = () => evalIn(`document.getElementById("bell").dataset.claudeHooks`);
  const openPanel = async () => {
    await evalIn(`document.getElementById("noticepanel").hidden = true`);
    await evalIn(`document.getElementById("bell").click()`);
    await until(() => evalIn(`!document.getElementById("noticepanel").hidden`), 5, "panel");
  };
  const rowText = () => evalIn(`(document.querySelector("#noticepanel .hookrow") || {}).textContent || ""`);
  const buttonText = () => evalIn(`(document.querySelector("#noticepanel .hookrow button") || {}).textContent || ""`);
  // Through the real elements, never send(): a button wired to nothing is
  // exactly the defect this file exists to catch.
  const clickButton = () => evalIn(`(() => { const b = document.querySelector("#noticepanel .hookrow button"); if (!b) return false; b.click(); return true; })()`);
  const confirmYes = () => evalIn(`(() => { const b = [...document.querySelectorAll("#noticepanel .hookrow .confirm button")].find((x) => /^(Enable|Disable)$/.test(x.textContent)); if (!b) return false; b.click(); return true; })()`);
  const confirmText = () => evalIn(`(document.querySelector("#noticepanel .hookrow .confirm") || {}).textContent || ""`);
  return { evalIn, ready, mark, openPanel, rowText, buttonText, clickButton, confirmYes, confirmText, close: page.close };
};

let one, two;
try {
  one = wire(await openPage(browser.port, url));
  two = wire(await openPage(browser.port, url));
  ok(await one.ready() && await two.ready(), "two pages are up on the same project");

  ok(await one.mark() === "off", "the bell is marked off before enabling");
  await one.openPanel();
  ok(/Claude notifications for this project: off/.test(await one.rowText()), "the panel's first row says off");
  ok(await one.buttonText() === "Enable", "and offers Enable");

  ok(await one.clickButton(), "Enable is clickable");
  ok(/settings\.local\.json/.test(await one.confirmText()), "the confirmation names the file it will write");
  ok(await one.confirmYes(), "and can be confirmed");

  const written = await until(async () => {
    try { const v = JSON.parse(await Deno.readTextFile(settings)); return !!(v.hooks && v.hooks.Stop && v.hooks.Notification); } catch { return false; }
  }, 10, "settings file");
  ok(written, "the settings file now holds both events");
  ok(await until(() => one.mark().then((m) => m === "on"), 5, "mark on"), "the clicking browser's bell is unmarked (on)");
  ok(await until(() => two.mark().then((m) => m === "on"), 5, "mirror"), "the other browser's bell followed");

  await one.openPanel();
  ok(await one.buttonText() === "Disable", "the row now offers Disable");
  ok(await one.clickButton() && await one.confirmYes(), "Disable, confirmed");
  const emptied = await until(async () => {
    try { const v = JSON.parse(await Deno.readTextFile(settings)); return !v.hooks; } catch { return false; }
  }, 10, "hooks removed");
  ok(emptied, "the file no longer holds hooks");
  ok(await until(() => one.mark().then((m) => m === "off"), 5, "mark off"), "the bell is marked off again");

  // Unknown: a file roost cannot parse shows a reason and no button.
  await Deno.writeTextFile(settings, "{ broken");
  await one.evalIn(`send({ t: "RequestState" })`);
  ok(await until(() => one.mark().then((m) => m === "unknown"), 5, "unknown"), "an unparseable file marks the bell unknown");
  await one.openPanel();
  ok(/cannot tell/.test(await one.rowText()), "the row says it cannot tell");
  ok(await one.buttonText() === "", "and offers no button");
} finally {
  try { await one?.close?.(); } catch {}
  try { await two?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
