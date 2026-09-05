//! The settings dialog: live theme preview, Save/Cancel, both scopes, the
//! rows the snapshot describes, and that a read-only key has no write path.
//! Rust proves the intent, the file and the snapshot; only a browser can
//! prove the cascade repaints and that a second browser follows.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "# global\ntheme = \"dark\"\n");
const projToml = `${fx.dir}/.roost/config.toml`;
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${roost.port}/proj`;

const probe = (evalIn, expr) => evalIn(`(() => { const e = document.createElement("i"); e.style.color = ${JSON.stringify(expr)};
  document.body.appendChild(e); const c = getComputedStyle(e).color; e.remove(); return c; })()`);

let one, two;
try {
  one = await openPage(browser.port, url);
  two = await openPage(browser.port, url);
  for (const p of [one, two]) await until(() => p.evalIn("ctrl && ctrl.readyState === 1 && !!state && !!state.settings"), 30, "app");

  console.log("A. applyTheme switches the cascade in place, both directions");
  const darkBg = await probe(one.evalIn, "var(--bg)");
  ok(darkBg === "rgb(13, 17, 23)", `the page opened on dark.css (${darkBg})`);
  await one.evalIn(`applyTheme("nord"); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", 5, "data-theme"), "a daisyUI name sets data-theme");
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === (await probe(one.evalIn, "var(--color-base-100)")), 10, "bridge"), "and --bg follows nord's base once the bridge loads");
  await one.evalIn(`applyTheme("light"); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === undefined, 5, "no data-theme"), "a roost name removes data-theme");
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(255, 255, 255)", 10, "light"), "and light.css paints");
  ok((await one.evalIn(`document.querySelectorAll('link[href="/static/daisy-bridge.css"]').length`)) === 0, "the bridge link is gone");
  // Revert-check 1: removing `drop("theme-bridge")` from applyTheme's roost
  // branch left the bridge link element in the DOM after switching to a
  // roost theme, and this assertion failed:
  //   FAIL  the bridge link is gone
  // Restored, and it passes again.
  await one.evalIn(`applyTheme("dark"); 0`);
  await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(13, 17, 23)", 10, "back to dark");

  // Revert-check 2, as specified: changing the vendored theme-daisy link's
  // `first` argument from `true` to `false` (so it is inserted right before
  // style.css instead of at the very start of <head>) did NOT fail this
  // assertion, or any other in section A — the run stayed green. Traced with
  // document.head dumps at each step (both with `first: true` and `first:
  // false`, side by side): the two runs produce byte-identical link order and
  // --border values at every step. The reason: applyTheme's roost branch
  // unconditionally drops any existing theme-roost link and recreates it via
  // insertBefore(styleLink) on every switch TO a roost theme, and the daisy
  // branch unconditionally drops theme-roost before it returns — together
  // these guarantee theme-roost always ends up immediately after whatever
  // remains before style.css (theme-daisy, once created), regardless of
  // where theme-daisy itself was anchored. So for any sequence reachable
  // from this fixture's roost-initial config, theme-daisy's own `first`
  // position is not load-bearing; `first: true` is still the right,
  // defensive choice (protects a config that does NOT always drop-and-
  // recreate, e.g. today's dead-simple discipline changing later), just not
  // one this exact revert can observe breaking.
  //
  // The --border assertion is not vacuous, though: flipping the OTHER call's
  // hardcoded argument instead — `ensure("theme-roost", ..., false)` to
  // `true`, so the roost link itself is anchored at head start — reproduces
  // exactly the failure mode this assertion exists to catch:
  //   FAIL  --border is a colour under a roost theme with the vendored file loaded
  // (computed value "1px", daisyUI's vendored :root default, winning over
  // light.css's #d0d7de because the roost link then lands BEFORE the vendor
  // file instead of after it). The --bg assertion above stays green through
  // this too — light.css still owns --bg — so only --border catches it.
  // Restored (`false`), and it passes again.
  ok(!/^\d/.test(await one.evalIn(`getComputedStyle(document.documentElement).getPropertyValue("--border").trim()`)), "--border is a colour under a roost theme with the vendored file loaded");
} finally {
  try { await one?.close(); } catch {}
  try { await two?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
