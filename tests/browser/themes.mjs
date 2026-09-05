//! daisyUI themes reach the page: `theme = "nord"` in a project's config puts
//! `data-theme="nord"` on <html>, and the bridge turns daisyUI's variables
//! into roost's own, so the body is painted in nord's base colour and
//! `--border` is a colour again (daisyUI's themes file defines it as a width).
//! A roost theme name keeps working exactly as before.
//!
//! Real Chromium against a real roost: the Rust tests prove the HTML; only a
//! browser can prove the cascade resolves.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// proj → nord (a daisyUI theme); proj2 → dark (roost's own file).
await Deno.mkdir(`${fx.dir}/.roost`, { recursive: true });
await Deno.writeTextFile(`${fx.dir}/.roost/config.toml`, 'theme = "nord"\n');
const dir2 = `${fx.roots}/proj2`;
await Deno.mkdir(`${dir2}/.roost`, { recursive: true });
await Deno.writeTextFile(`${dir2}/hello.md`, "# hi\n");
await Deno.writeTextFile(`${dir2}/.roost/config.toml`, 'theme = "dark"\n');
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));

// A colour as the browser resolves it, so oklch and hex compare on equal terms.
const probe = (evalIn, expr) => evalIn(`(() => { const e = document.createElement("i"); e.style.color = ${JSON.stringify(expr)};
  document.body.appendChild(e); const c = getComputedStyle(e).color; e.remove(); return c; })()`);

let one, two;
try {
  one = await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`);
  await until(() => one.evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app");
  ok((await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", "<html> carries data-theme=nord");
  const base = await probe(one.evalIn, "var(--color-base-100)");
  const bg = await probe(one.evalIn, "var(--bg)");
  ok(base !== "rgb(0, 0, 0)" && base === bg, `--bg resolves to daisyUI's base-100 (${bg} vs ${base})`);
  // The body paints with --window, which style.css derives from the theme's
  // variables; equality with the probe proves the derivation reached paint.
  const bodyBg = await one.evalIn(`getComputedStyle(document.body).backgroundColor`);
  const win = await probe(one.evalIn, "var(--window)");
  ok(bodyBg === win && win !== "rgb(0, 0, 0)", `the body is painted in the theme's --window (${bodyBg})`);
  const border = await one.evalIn(`getComputedStyle(document.documentElement).getPropertyValue("--border").trim()`);
  // Non-empty AND not a length: an empty string passed this before the bridge
  // existed, which is the vacuous form of the same check.
  ok(border !== "" && !/^\d/.test(border), `--border is a colour, not daisyUI's width (${JSON.stringify(border)})`);

  two = await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj2`);
  await until(() => two.evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app 2");
  ok((await two.evalIn(`document.documentElement.dataset.theme`)) === undefined, "a roost theme sets no data-theme");
  ok((await probe(two.evalIn, "var(--bg)")) === "rgb(13, 17, 23)", "and dark.css's --bg is what it always was");
} finally {
  try { await one?.close(); } catch {}
  try { await two?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
