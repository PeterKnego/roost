//! The Latest row: five renderings, and the one the server actually sent.
//!
//! #65 step 2. A Rust test proves the comparator, the state file and the
//! `UpdateView` the server builds; only a browser can see the row.
//!
//! Run: deno run -A tests/browser/version.mjs
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

  console.log("A. the harness really turned the check off");
  // The fixture that silently does nothing is the trap README records for
  // `autosave: false`: without this assertion every row below could pass
  // while the suite quietly hit crates.io on every connect.
  const sent = await evalIn(`state.settings.update`);
  ok(!!sent, "the settings snapshot carries an `update` block");
  ok(sent && sent.status === "off",
     `and the harness's ROOST_CONFIG took: status=${sent && sent.status}`);
  // Beside `build`, not inside it — `BuildInfo` promises it is constant for
  // the process, and this is the one server fact on the pane that is not.
  ok(await evalIn(`state.settings.build.status === undefined`),
     "the answer is a sibling of build, not a field of it");

  console.log("B. the row renders what the server sent");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "the dialog"),
     "the settings dialog opens");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  const readRows = () => evalIn(`(() => {
    const out = {};
    for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
      out[r.querySelector("label").textContent] = r.querySelector(".aboutval").textContent.trim();
    }
    return out; })()`);
  let rows = await readRows();
  ok(rows.Latest === "version checks are off",
     `the row is shown when the check is off, and says so (${JSON.stringify(rows.Latest)})`);
  // Discoverable where discovery happens: the row is never hidden, unlike
  // Upgrade. A missing row is the state this one exists to stop hiding.
  ok(Object.keys(rows).indexOf("Latest") === Object.keys(rows).indexOf("Version") + 1,
     `and sits directly under Version (${JSON.stringify(Object.keys(rows))})`);

  console.log("C. all five states, through the real renderer");
  // Switching tabs re-renders About from `state.settings` by reference;
  // closing and reopening does not, because the settings button sends
  // `RequestState` and the server's values overwrite anything set here. See
  // about.mjs, where that cost four failing assertions to discover.
  const asAbout = async (update) => {
    await evalIn(`Object.assign(state.settings.update, ${JSON.stringify(update)}); 0`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
    return (await readRows()).Latest;
  };
  const original = await evalIn(`JSON.parse(JSON.stringify(state.settings.update))`);

  for (const [update, want, why] of [
    [{ status: "newer", latest: "0.5.3" }, "0.5.3 available", "a newer release names itself"],
    [{ status: "up-to-date", latest: "0.5.2" }, "up to date", "nothing newer"],
    [{ status: "unknown", latest: "0.5.2" }, "could not check",
      "a failed check is never reported as up to date, even holding a version"],
    [{ status: "unknown", latest: "" }, "could not check", "and neither is one holding nothing"],
    [{ status: "never", latest: "" }, "not checked yet", "the few seconds after a fresh install"],
    [{ status: "off", latest: "" }, "version checks are off", "and the setting, back where we started"],
  ]) {
    const got = await asAbout(update);
    ok(got === want, `${why} (${JSON.stringify(got)})`);
  }

  // A status this build does not know must degrade to "could not know", not
  // to silence and not to a cheerful answer.
  ok((await asAbout({ status: "something-later", latest: "9.9.9" })) === "could not check",
     "an unrecognised status is one more way of not knowing");

  await asAbout(original);

  // Driven on the function the renderer calls, the way about.mjs drives
  // `installLabel` — the same table, with no dialog in the way.
  const label = async (u) => await evalIn(`latestLabel(${JSON.stringify(u)})`);
  ok((await label({ status: "newer", latest: "1.0.0" })) === "1.0.0 available",
     "latestLabel is top-level and names the version it was given");
  ok((await label(null)) === "could not check",
     "and a missing block is not knowing, not a crash");
  ok((await label({})) === "could not check", "nor is an empty one");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
