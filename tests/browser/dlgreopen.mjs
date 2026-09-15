//! Closing the settings dialog and reopening it in the same turn.
//!
//! #49. `settings.mjs` failed at a different assertion almost every run,
//! always in a later section — the shape of a control that is sometimes
//! wedged rather than of one assertion being wrong. This is the mechanism,
//! extracted into something deterministic.
//!
//! `el.close()` sets `.open = false` immediately but *queues* the `close`
//! event rather than dispatching it inline. So a close followed by a reopen
//! inside the same turn runs the OLD session's `close` handler after the new
//! session has already installed itself — and that handler cleared
//! `settingsOpen`, which app.js gates both `onSnapshot` and `onError` on. The
//! confirming snapshot then never reaches the dialog: it sits open, with Save
//! disabled, for good.
//!
//! Reproduced from a real run before being written down: two `openSettings`
//! calls 41ms apart, both from `settingsBtn.onclick`, then a `close` arriving
//! after the second `showModal`, leaving `settingsOpen: false`,
//! `okDisabled: true` and the dialog open.
//!
//! Run: deno run -A tests/browser/dlgreopen.mjs
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
  await until(() => evalIn(`ctrl && ctrl.readyState === 1 && !!state && !!state.settings`), 30, "app.js");

  console.log("A. close and reopen in one turn");
  // One `evalIn`, so cancel and reopen happen in the same task and the close
  // event is still only queued when the second dialog installs itself. That
  // is the timing the flake hit by accident; here it is the point.
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "the dialog"),
     "the settings dialog opens");
  await evalIn(`(() => {
    document.querySelector("#dlg-settings .dlg-cancel").click();
    document.getElementById("settings").click();
  })()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "reopened"),
     "cancelling and reopening in the same turn leaves it open");
  // Let the queued `close` from the first session be delivered. Without the
  // token guard in dialog.js it lands here and clears the live session.
  await sleep(300);

  console.log("B. the reopened dialog is still wired to the server");
  ok(await evalIn(`!!settingsOpen`),
     "the live session survived the previous session's close event");

  // The behavioural half: a save has to still complete. `settingsOpen` being
  // truthy is the mechanism, but what the user loses is the ability to save at
  // all, so assert that rather than only the flag.
  await evalIn(`(() => {
    const cb = document.querySelector('#dlg-settings .dlg-row[data-key="autosave"] input[type="checkbox"]');
    cb.checked = !cb.checked; cb.dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
  await evalIn(`document.querySelector("#dlg-settings .dlg-ok").click()`);
  ok(await until(() => evalIn(`!document.getElementById("dlg-settings").open`), 10, "the save"),
     "Save completes and the dialog closes");
  // Reopened rather than read straight after closing: `okBtn.disabled` stays
  // true while the dialog is shut, and is reset by the next session's fill.
  // Asserting on it in between would be asserting on state nobody can see.
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "reopened"),
     "and it can be opened again");
  ok(!(await evalIn(`document.querySelector("#dlg-settings .dlg-ok").disabled`)),
     "with Save usable — the state the wedge left behind for good");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
