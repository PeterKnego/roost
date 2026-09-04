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
//!   3. Removed the `renderNotices()` call app.js's "State" handler makes
//!      alongside `renderClaudeHooks()` (final-review item 1: without it,
//!      an OPEN notice panel kept a stale hook row after a confirm or after
//!      another browser flipped the switch — only the bell's mark/tooltip
//!      updated, since nothing else rebuilds the panel's children). Three
//!      assertions failed, all by `until` timeout (5s each) waiting on text
//!      that never changed on the still-open panel:
//!        "the still-open panel's row updates to on with no reopen"
//!        "and its button now reads Disable"
//!        "the other browser's already-open row followed, with no reopen"
//!      Restored (all pass).
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
  // What the CSS actually paints for that attribute: the ::after glyph, its
  // strike, and its colour against the theme's own variables. `mark()` alone
  // passes with the stylesheet deleted (README trap: asserting the attribute
  // and calling it the picture). Revert-check 2026-09-04: with the previous
  // rules, where on drew nothing, the "on draws a Claude-orange ✻" assertion fails
  // with content "none".
  const drawn = () => evalIn(`(() => {
    const b = document.getElementById("bell");
    const cs = getComputedStyle(b, "::after");
    const root = getComputedStyle(document.documentElement);
    const probe = (v) => { const e = document.createElement("i"); e.style.color = v; document.body.appendChild(e); const c = getComputedStyle(e).color; e.remove(); return c; };
    return { content: cs.content, struck: /line-through/.test(cs.textDecorationLine || cs.textDecoration), color: cs.color, claude: probe("var(--claude, #d97757)"), muted: probe("var(--muted)"), warn: probe("var(--warn)") };
  })()`);
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
  return { evalIn, ready, mark, drawn, openPanel, rowText, buttonText, clickButton, confirmYes, confirmText, close: page.close };
};

let one, two;
try {
  one = wire(await openPage(browser.port, url));
  two = wire(await openPage(browser.port, url));
  ok(await one.ready() && await two.ready(), "two pages are up on the same project");

  ok(await one.mark() === "off", "the bell is marked off before enabling");
  {
    const d = await one.drawn();
    ok(d.content === '"✻"' && d.struck && d.color === d.muted, `off draws a struck muted ✻ (${JSON.stringify(d)})`);
  }
  await one.openPanel();
  ok(/Claude notifications for this project: off/.test(await one.rowText()), "the panel's first row says off");
  ok(await one.buttonText() === "Enable", "and offers Enable");

  // Opened before `one` confirms, and deliberately never reopened below:
  // this is what proves the mirrored flip reaches an already-open panel,
  // not just a freshly-opened one.
  await two.openPanel();

  ok(await one.clickButton(), "Enable is clickable");
  ok(/settings\.local\.json/.test(await one.confirmText()), "the confirmation names the file it will write");
  ok(await one.confirmYes(), "and can be confirmed");

  // No openPanel() call between the confirm and these two: `one`'s panel
  // was already open (never closed since the assertions above), and it has
  // to pick up the flip on its own — via the State handler's renderNotices()
  // call, not by being reopened. See revert-check 3 in the file header.
  ok(
    await until(() => one.rowText().then((t) => /Claude notifications for this project: on/.test(t)), 5, "row on"),
    "the still-open panel's row updates to on with no reopen"
  );
  ok(await until(() => one.buttonText().then((t) => t === "Disable"), 5, "button disable"), "and its button now reads Disable");

  const written = await until(async () => {
    try { const v = JSON.parse(await Deno.readTextFile(settings)); return !!(v.hooks && v.hooks.Stop && v.hooks.Notification); } catch { return false; }
  }, 10, "settings file");
  ok(written, "the settings file now holds both events");
  ok(await until(() => one.mark().then((m) => m === "on"), 5, "mark on"), "the clicking browser's bell is unmarked (on)");
  ok(await until(() => two.mark().then((m) => m === "on"), 5, "mirror"), "the other browser's bell followed");
  {
    const d = await one.drawn();
    ok(d.content === '"✻"' && !d.struck && d.color === d.claude, `on draws a Claude-orange ✻, not struck (${JSON.stringify(d)})`);
  }

  // `two`'s panel was opened above, before `one` ever confirmed, and is
  // still open now — no reopen here either.
  ok(
    await until(() => two.rowText().then((t) => /Claude notifications for this project: on/.test(t)), 5, "two row on"),
    "the other browser's already-open row followed, with no reopen"
  );

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
  {
    const d = await one.drawn();
    ok(d.content === '"?"' && !d.struck && d.color === d.warn, `unknown draws a warn ? (${JSON.stringify(d)})`);
  }
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
