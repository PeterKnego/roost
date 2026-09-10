//! The button that puts Claude on the open pull requests.
//!
//! #52. What needed deciding was never how it reviews but **what stops it**,
//! and what a control that starts something which commits and pushes owes the
//! person clicking it. Both of those are what this file checks; the review
//! itself is Claude's job and roost cannot test it.
//!
//! The confirmation is the point of the browser half. `launch.rs` can be
//! tested for the bounds it puts in the prompt, but only a browser can show
//! that nothing starts until the person has been told what will happen —
//! and that dismissing it really starts nothing, which is the assertion that
//! fails if the dialog is decorative.
//!
//! Run: deno run -A tests/browser/prloop.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");

  // The launch is offered only where `claude` was found, and the startup probe
  // takes a login shell to answer. Both buttons share that answer, so waiting
  // for either is waiting for the probe.
  const offered = await until(() => evalIn(`LAUNCHES.includes("prloop")`), 20, "the probe");
  if (!offered) {
    console.log("SKIP: `claude` is not on PATH here, so neither launch is offered.");
    Deno.exit(0);
  }

  console.log("A. the button is its own control");
  const btn = `document.querySelector('.pane[data-pane="3"] .paneicon.newprloop')`;
  ok(await until(() => evalIn(`!!${btn}`), 10, "the button"), "the pane offers it");
  // Deliberately not a mode of the ✻ button: this one starts something that
  // commits and pushes, and that must never be one misread icon away from
  // "a terminal running Claude".
  ok(await evalIn(`!!document.querySelector('.pane[data-pane="3"] .paneicon.newclaude') && ${btn} !== document.querySelector('.pane[data-pane="3"] .paneicon.newclaude')`),
     "and it is a separate control from the plain Claude one");

  console.log("B. it says what it will do before it does it");
  const before = await evalIn(`state.panes[3].tabs.length`);
  await evalIn(`${btn}.click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-confirm").open`), 5, "the confirmation"),
     "clicking it asks first");
  const text = await evalIn(`document.getElementById("dlg-confirm").textContent`);
  // The three things a person needs before agreeing: what it does, what stops
  // it, and what it will not touch. Asserted on the visible text, because a
  // bound that only exists in the prompt is not a bound the user was told
  // about.
  ok(/commit/i.test(text), "naming that it commits");
  ok(/stops/i.test(text) && /2 hours/.test(text) && /5 pushes/.test(text),
     "naming the limits that stop it");
  ok(/default branch/.test(text), "and what it will never touch");

  console.log("C. declining starts nothing");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click()`);
  await until(() => evalIn(`!document.getElementById("dlg-confirm").open`), 5, "dismissed");
  await sleep(600);
  ok((await evalIn(`state.panes[3].tabs.length`)) === before,
     "no terminal was opened — the confirmation is a gate, not a notice");

  console.log("D. agreeing opens a terminal for it");
  await evalIn(`${btn}.click()`);
  await until(() => evalIn(`document.getElementById("dlg-confirm").open`), 5, "again");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click()`);
  ok(await until(async () => (await evalIn(`state.panes[3].tabs.length`)) > before, 15, "the tab"),
     "a terminal opens");
  ok(await until(() => evalIn(`terms.size > 0`), 30, "a shell"), "with a shell in it");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
