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

  console.log("\nB. the gear opens the dialog with the rows the snapshot describes");
  // The gear kept `cursor: help` (the question-mark pointer) from its
  // "not implemented yet" days after it became a real button. Revert-check
  // 2026-09-05: with `#settings { cursor: help; }` back in style.css this
  // reads "help" and fails.
  ok((await one.evalIn(`getComputedStyle(document.getElementById("settings")).cursor`)) === "pointer", "the gear shows the pointer cursor, not the help question mark");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  ok(await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog"), "the dialog opened in-page");
  const labels = await one.evalIn(`[...document.querySelectorAll("#dlg-settings .dlg-row label")].map((l) => l.textContent).join(",")`);
  // No theme row here: the theme is chosen on the Theme pane, which also
  // carries its source line and Clear.
  ok(labels === "hide,show_hidden,autosave,share_selection,worktree_prompt,allowed_origins,max_upload_bytes,ide,roots", `rows in the spec's order, without theme (${labels})`);
  ok(/keystroke/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="autosave"] .doc').textContent`)), "each row explains what the setting does");
  {
    const h = await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="autosave"]').getBoundingClientRect().height`);
    ok(h < 60, `a boolean row is one compact line (${h}px)`);
  }
  ok((await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="share_selection"]').classList.contains("disabled")`)), "a global-only row is disabled in Project scope");
  ok((await one.evalIn(`document.querySelectorAll('#dlg-settings .dlg-row[data-key="allowed_origins"] input, #dlg-settings .dlg-row[data-key="allowed_origins"] textarea').length`)) === 0, "a read-only row has no control");
  ok(/global config file/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="allowed_origins"] .hint').textContent`)), "and says to edit the file by hand");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  ok(/from global/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-themes .theme-source').textContent`)), "the Theme pane's source line says the theme comes from global");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click(); 0`);

  console.log("\nC. preview then Cancel leaves the page as it was");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  // The Theme tab shows only the tiles. style.css's `display: flex` on
  // .dlg-rows outranked the UA's `[hidden] { display: none }`, so the
  // Settings rows stayed visible above the tiles. Revert-check 2026-09-05:
  // dropping the `[hidden] { display: none }` rule brings that back.
  ok((await one.evalIn(`getComputedStyle(document.querySelector("#dlg-settings .dlg-rows")).display`)) === "none", "the Settings rows are hidden under the Theme tab");
  ok((await one.evalIn(`getComputedStyle(document.querySelector("#dlg-settings .dlg-themes")).display`)) !== "none", "and the tiles are shown");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tile[data-name="nord"]').click(); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", 5, "preview"), "clicking a tile previews it");
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click(); 0`);
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(13, 17, 23)", 10, "reverted"), "Cancel restores the theme the dialog opened with");
  ok((await one.evalIn(`document.documentElement.dataset.theme`)) === undefined, "and removes data-theme");

  console.log("\nD. preview then Save writes the project file and the other browser follows");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog again");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tile[data-name="nord"]').click(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => { try { return /theme = "nord"/.test(await Deno.readTextFile(projToml)); } catch { return false; } }, 10, "file"), "the project file holds theme = \"nord\"");
  ok(/# global\ntheme = "dark"/.test(await Deno.readTextFile(globalToml)), "the global file is untouched");
  ok(await until(async () => (await two.evalIn(`document.documentElement.dataset.theme`)) === "nord", 10, "mirror"), "the other browser switched to nord without a reload");
  ok(await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "closed"), "Save closed the dialog");

  console.log("\nE. Clear removes the project key; the hint says the value now comes from global");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  ok(/from project/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-themes .theme-source').textContent`)), "the Theme pane's source line now says from project");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-themes .clear').click(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => !/theme/.test(await Deno.readTextFile(projToml)), 10, "cleared"), "the key is gone from the project file");
  ok(await until(async () => (await two.evalIn(`document.documentElement.dataset.theme`)) === undefined, 10, "back"), "and both browsers are back on the global dark");

  console.log("\nF. Global scope writes the global file, keeping its comment");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-scope button[data-scope="global"]').click(); 0`);
  ok(!(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="worktree_prompt"]').classList.contains("disabled")`)), "worktree_prompt is enabled in Global scope");
  await one.evalIn(`(() => { const c = document.querySelector('#dlg-settings .dlg-row[data-key="worktree_prompt"] input[type="checkbox"]'); c.checked = false; c.dispatchEvent(new Event("change")); })(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => /worktree_prompt = false/.test(await Deno.readTextFile(globalToml)), 10, "global"), "the global file gained worktree_prompt = false");
  ok(/^# global\n/.test(await Deno.readTextFile(globalToml)), "and kept its comment");

  console.log("\nG. a forged write to a read-only key is refused and changes nothing");
  await one.evalIn(`window.__errs = []; const _oe = onEvent; window.onEvent = (ev) => { if (ev.t === "Error") window.__errs.push(ev.msg); return _oe(ev); };
     ctrl.onmessage = (m) => window.onEvent(JSON.parse(m.data));
     send({ t: "SetSetting", scope: "global", key: "allowed_origins", value: ["https://evil.example"] }); 0`);
  ok(await until(async () => Number(await one.evalIn(`window.__errs.length`)) === 1, 5, "error"), "the server answered with an error");
  ok(/allowed_origins/.test(await one.evalIn(`window.__errs[0]`)), "naming the key");
  ok(!/evil/.test(await Deno.readTextFile(globalToml)), "and the file is unchanged");

  console.log("\nH. saving show_hidden takes effect in the browser that saved, and Enter is what saves");
  // The starting state, asserted rather than assumed: without this the
  // showHidden() check below would pass over a page that already had it on.
  ok((await one.evalIn(`showHidden()`)) === false, "the tree is not showing hidden files yet");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-scope button[data-scope="project"]').click(); 0`);
  await one.evalIn(`(() => { const c = document.querySelector('#dlg-settings .dlg-row[data-key="show_hidden"] input[type="checkbox"]');
     c.checked = true; c.dispatchEvent(new Event("change")); c.focus(); })(); 0`);
  // Enter has to arrive from somewhere that is NOT the Save button, or the
  // browser's own button activation would satisfy this assertion with no
  // keydown handler at all.
  ok(await one.evalIn(`document.activeElement === document.querySelector('#dlg-settings .dlg-row[data-key="show_hidden"] input[type="checkbox"]')`),
     "focus is in the checkbox row, not on Save");
  for (const type of ["keyDown", "keyUp"]) {
    await one.cmd("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13, ...(type === "keyDown" ? { text: "\r" } : {}) });
  }
  ok(await until(async () => /show_hidden = true/.test(await Deno.readTextFile(projToml)), 10, "file"), "Enter saved: the project file holds show_hidden = true");
  // The hub clears the workspace override on a show_hidden write, so
  // `state.show_hidden` is null here and showHidden() falls through to the
  // page-load default — which is exactly the value that has to follow the
  // snapshot rather than stay frozen at load time.
  ok(await until(async () => (await one.evalIn(`showHidden()`)) === true, 10, "showHidden"), "and showHidden() is true in the browser that saved");

  console.log("\nI. switching scope after a preview discards the pick, and says so on the page");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  const bgBefore = await probe(one.evalIn, "var(--bg)");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tile[data-name="nord"]').click(); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", 5, "preview"), "the tile previews nord");
  // The preview has to have actually repainted, or "the page went back" below
  // would be true of a page that never left.
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) !== bgBefore, 10, "repaint"), "and the page really repainted");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-scope button[data-scope="global"]').click(); 0`);
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === bgBefore, 10, "reverted"), "switching scope puts the page back on the theme the dialog opened with");
  ok((await one.evalIn(`document.documentElement.dataset.theme`)) === undefined, "and data-theme is gone");
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click(); 0`);
  await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "closed");
  ok((await probe(one.evalIn, "var(--bg)")) === bgBefore, "Cancel leaves it there");
  ok(!/theme/.test(await Deno.readTextFile(projToml)), "and the discarded pick was never written");

  console.log("\nJ. a refused write keeps the dialog open with the values in it, and names the file");
  const goodToml = await Deno.readTextFile(projToml);
  // Hand-broken, the way a person editing this file by hand breaks it.
  // Nothing the dialog can write will fix it, so every Save against it is
  // refused — which is the one path where losing the typed values matters.
  await Deno.writeTextFile(projToml, "{ broken\n");
  const brokenBytes = await Deno.readTextFile(projToml);
  await one.evalIn(`document.querySelectorAll(".error-banner").forEach((b) => b.remove()); document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  ok(await until(async () => /config\.toml/.test(await one.evalIn(`(document.querySelector("#dlg-settings .dlg-warning") || {}).textContent || ""`)), 10, "warning"),
     "the dialog names the config file it could not parse");
  ok((await one.evalIn(`(document.querySelector("#dlg-settings .dlg-warning") || {}).hidden`)) === false, "and that line is not hidden");
  await one.evalIn(`(() => { const t = document.querySelector('#dlg-settings .dlg-row[data-key="hide"] textarea');
     t.value = "dist"; t.dispatchEvent(new Event("input")); })(); 0`);
  const errsBefore = Number(await one.evalIn(`window.__errs.length`));
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => Number(await one.evalIn(`window.__errs.length`)) > errsBefore, 10, "error"), "the refused write answered with an error");
  ok((await one.evalIn(`document.querySelectorAll(".error-banner").length`)) >= 1, "and a banner is on screen");
  ok((await one.evalIn(`document.getElementById("dlg-settings").open`)) === true, "the dialog is still open");
  // Read through `.open`: the shell's textarea keeps its value after the
  // dialog closes, so on its own this would hold just as well over a dialog
  // that had thrown the edit away by closing.
  ok((await one.evalIn(`document.getElementById("dlg-settings").open && document.querySelector('#dlg-settings .dlg-row[data-key="hide"] textarea').value`)) === "dist",
     "with the typed value still in it");
  ok((await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").disabled`)) === false, "and Save usable again");
  ok((await Deno.readTextFile(projToml)) === brokenBytes, "the file is byte-identical");
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click(); 0`);
  await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "closed");
  await Deno.writeTextFile(projToml, goodToml);

  console.log("\nJ2. a throwing onSnapshot cannot wedge the rest of the State handler");
  // `treeShownHidden` is assigned near the END of the State handler, well
  // past followSettings — so a throw that escapes followSettings leaves the
  // sentinel in place, and would for the life of the page.
  await one.evalIn(`settingsOpen = { onSnapshot() { throw new Error("boom from settings.mjs"); } };
     treeShownHidden = "sentinel"; send({ t: "RequestState" }); 0`);
  ok(await until(async () => (await one.evalIn(`treeShownHidden`)) !== "sentinel", 10, "handler"), "the State handler ran on past followSettings");
  ok((await one.evalIn(`settingsOpen`)) === null, "and the hook that threw was cleared, so it cannot throw again");

  console.log("\nK. a dialog dismissed with Escape leaves no live Enter handler behind");
  // Escape and the backdrop are runDialog's exits, not openSettings's, and
  // this browser fires no `close` event for either (see `settingsSession` in
  // dialog.js) — so the Enter handler such an exit leaves on the shared shell
  // has to be inert, or the *next* dialog's Enter re-sends this one's edits.
  ok(!/hide/.test(await Deno.readTextFile(projToml)), "the project file has no hide key to start with");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await one.evalIn(`(() => { const t = document.querySelector('#dlg-settings .dlg-row[data-key="hide"] textarea');
     t.value = "escaped-edit"; t.dispatchEvent(new Event("input")); })(); 0`);
  for (const type of ["keyDown", "keyUp"]) {
    await one.cmd("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 });
  }
  ok(await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "escaped"), "Escape closed the dialog");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog again");
  // Nothing edited in this one, so its own Save has no intent to send and
  // closes at once: any write that appears came from the escaped dialog.
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="show_hidden"] input[type="checkbox"]').focus(); 0`);
  for (const type of ["keyDown", "keyUp"]) {
    await one.cmd("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13, ...(type === "keyDown" ? { text: "\r" } : {}) });
  }
  ok(await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "closed"), "Enter closed the second dialog with nothing to write");
  ok(!/hide/.test(await Deno.readTextFile(projToml)), "and the escaped dialog's edit was not written behind it");

  // Revert-check 7 (section J, 2026-09-05): restoring Save's old body —
  // `settingsOpen = null; finish(true)` on the click, no `awaitingSave` —
  // failed 2:
  //   FAIL  the dialog is still open
  //   FAIL  with the typed value still in it
  // ("and Save usable again" and "the file is byte-identical" stayed green:
  // the button is never disabled under the revert, and the server's refusal
  // is what leaves the file alone, not the client. Those two prove nothing
  // on their own, which is why the pair above exists.) Restored.
  //
  // Revert-check 8 (section J): deleting the `settingsOpen.onError(...)` call
  // from app.js's `case "Error"` failed exactly 1:
  //   FAIL  and Save usable again
  // — the dialog stays open with its values, but Save is disabled forever and
  // the only way out is Cancel. Restored.
  //
  // Revert-check 9 (section J): deleting the `.dlg-warning` block from
  // renderRows failed exactly 2:
  //   FAIL  the dialog names the config file it could not parse
  //   FAIL  and that line is not hidden
  // and nothing else — the refusal, the banner, and the file compare are all
  // still green over a dialog that never says why. Restored.
  //
  // Revert-check 10 (section J2): removing the try/catch around
  // `settingsOpen.onSnapshot(s)` failed both of J2's assertions:
  //   FAIL  the State handler ran on past followSettings
  //   FAIL  and the hook that threw was cleared, so it cannot throw again
  // Restored.
  //
  // Revert-check 11 (section K): removing `if (settingsSession !== session)
  // return;` from openSettings's keydown handler failed exactly 1:
  //   FAIL  and the escaped dialog's edit was not written behind it
  // This is the guard that exists because `close` never fires here (see the
  // comment on `settingsSession`). Note the other three assertions in K stay
  // green under the revert — the dialogs still open and close correctly; the
  // only symptom is a write nobody asked for. Restored.
  //
  // Revert-check 12 (section I): moving followSettings's `settingsOpen`
  // hook back to the END of the function, after the theme check, failed 2:
  //   FAIL  and the page really repainted
  //   FAIL  and data-theme is gone
  // Not obvious, and it only appears once Save waits for its snapshot: the
  // snapshot that closes the dialog is also the one that reports the new
  // effective theme, and the theme check skips while `settingsOpen` is set.
  // With the hook last, section E's Clear leaves *this* page painted with
  // nord that the file no longer sets, and nothing later corrects it — which
  // section I then reads as its opening theme. Restored.
  //
  // Revert-check 6 (section I, 2026-09-05): deleting
  // `if (previewTheme) { applyTheme(themeBefore); previewTheme = null; }`
  // from the scope button's handler failed 3 —
  //   FAIL  switching scope puts the page back on the theme the dialog opened with
  //   FAIL  and data-theme is gone
  //   FAIL  Cancel leaves it there
  // — and nothing outside this section. The first two are the discriminating
  // pair; the third fails only incidentally, because under the revert Cancel
  // has a real repaint to do and the probe immediately after it can read
  // before the restored stylesheet has applied. With the fix in place Cancel
  // is a no-op here (the page is already back), so that probe is stable, and
  // it is what says the fix did not leave the dialog mid-revert. Restored.
  //
  // Revert-check 4 (section H, 2026-09-05): putting `SHOW_HIDDEN_DEFAULT`
  // back to a page-load `const` and dropping followSettings's assignment
  // failed exactly one assertion —
  //   FAIL  and showHidden() is true in the browser that saved
  // — while "Enter saved: the project file holds show_hidden = true" stayed
  // green, which is what says the two halves discriminate separately: the
  // write landed, the saving browser just never learned. Restored.
  //
  // Revert-check 5 (section H): replacing `openSettings`'s keydown handler
  // with a no-op failed both of H's write assertions —
  //   FAIL  Enter saved: the project file holds show_hidden = true
  //   FAIL  and showHidden() is true in the browser that saved
  // — and nothing else in the file (every other section clicks Save). The
  // focus assertion above is what keeps this honest: with focus on the Save
  // button, Chromium's own button activation would have saved with no
  // handler at all. Restored.
  //
  // Revert-check 1: removing `if (previewTheme) applyTheme(themeBefore);`
  // from `openSettings`'s cancelBtn.onclick failed exactly section C:
  //   FAIL  Cancel restores the theme the dialog opened with
  //   FAIL  and removes data-theme
  // (a plain `FAIL (2)`, everything else — including D's Save-writes-nord and
  // E's Clear, both of which run *after* this handler exists but never take
  // its Cancel path — stayed green). Restored, and the file is byte-identical
  // to before the revert.
  //
  // Revert-check 2: changing `okBtn.onclick`'s `send(...)` to fire only for
  // `scope === "project"` failed exactly section F:
  //   FAIL  the global file gained worktree_prompt = false
  // ("and kept its comment" stayed green — it reads a file that was never
  // touched by this revert, so it cannot discriminate on its own; the
  // preceding assertion is the one that matters). Every other section
  // (which only ever writes project scope) stayed green. Restored.
  //
  // Revert-check 3: making `hintFor` return `"default"` unconditionally
  // failed exactly the two hint assertions in B and the one in E:
  //   FAIL  and says to edit the file by hand
  //   FAIL  theme's hint says it comes from global
  //   FAIL  theme's hint now says from project
  // (`FAIL (3)`; every non-hint assertion in those sections, and every other
  // section, stayed green — including F's own hint-free checks). Restored.
} finally {
  try { await one?.close(); } catch {}
  try { await two?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
