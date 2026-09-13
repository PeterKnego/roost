//! Backing up a workspace and restoring it. #18 step 3.
//!
//! The pane, the download, and the two-step restore all live in
//! `static/dialog.js` and `static/app.js`, which no Rust test can reach —
//! CLAUDE.md is explicit that anything touching them is checked here.
//!
//! Like `claudemenu.mjs`, this gives roost its own `HOME`: the conversations
//! are Claude Code's directory, not roost's, and against the developer's real
//! home the fixture's temporary project has never been opened, so every
//! transcript assertion would pass by finding nothing.
//!
//! **The restore is driven into a *second* project, at a different path.** A
//! round trip back into the source proves nothing: it stays green against an
//! implementation that records the source's absolute path and copies it back,
//! which is precisely #18's third difficulty. The assertion that matters is
//! that the transcript lands under the *destination's* derived directory.
//!
//! Run: deno run -A tests/browser/backup.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const home = `${fx.base}/home`;
// Claude Code's own encoding, the same one claudehist.rs derives.
const encode = (p) => p.replace(/[^A-Za-z0-9]/g, "-");

const ID = "aaaa1111-2222-3333-4444-555555555555";
const BODY = [
  JSON.stringify({ type: "mode", sessionId: "x" }),
  JSON.stringify({ type: "user", message: { content: "the archived conversation" } }),
].join("\n") + "\n";

const srcT = `${home}/.claude/projects/${encode(fx.dir)}`;
await Deno.mkdir(srcT, { recursive: true });
await Deno.writeTextFile(`${srcT}/${ID}.jsonl`, BODY);
await Deno.mkdir(`${srcT}/memory`, { recursive: true });
await Deno.writeTextFile(`${srcT}/memory/MEMORY.md`, "- a remembered thing\n");

// The destination: a different project, at a different path, so its derived
// transcript directory is a different directory.
const destDir = `${fx.roots}/dest`;
await Deno.mkdir(destDir, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: destDir, stdout: "null", stderr: "null" }).output();
const destT = `${home}/.claude/projects/${encode(destDir)}`;

const roost = await startRoost({
  repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
  extraEnv: { HOME: home },
});
const browser = await startBrowser(profileDir(repoRoot));
let page;

const pane = `document.querySelector('#dlg-settings .dlg-backup')`;
const box = `document.querySelector('#dlg-settings .bk-conversations')`;

const load = async (project) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  await p.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => p.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");
  return p;
};
const openBackupPane = async (p) => {
  await p.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => p.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await p.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="backup"]').click(); 0`);
  await until(() => p.evalIn(`!!(${pane}) && !${pane}.hidden`), 5, "the backup pane");
};

try {
  page = await load(fx.project);
  const { evalIn } = page;

  console.log("A. the pane exists and conversations are off");
  await openBackupPane(page);
  ok(await evalIn(`!!(${box})`), "there is a conversations switch");
  ok((await evalIn(`${box}.checked`)) === false, "and it is off");
  ok(/every command it ran/i.test(await evalIn(`document.querySelector('#dlg-settings .dlg-backup .doc').textContent`)),
     "with the sentence saying what it would include beside it");
  // The OK button would ask what it saves; About already sets this precedent.
  ok(await evalIn(`document.querySelector('#dlg-settings .dlg-ok').hidden`), "Save is hidden on this pane");

  console.log("B. the switch is not remembered");
  // #18: a backup that includes transcripts must be "explicit, opt-in per
  // project, and never a default", and the shape it warns about is one
  // "configured once and forgotten". A box that comes back ticked is that.
  await evalIn(`${box}.click(); 0`);
  ok((await evalIn(`${box}.checked`)) === true, "setup: ticking it takes");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click(); 0`);
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="backup"]').click(); 0`);
  ok((await evalIn(`${box}.checked`)) === false, "leaving the pane and coming back clears it");
  await evalIn(`document.querySelector('#dlg-settings .dlg-cancel').click(); 0`);
  await openBackupPane(page);
  ok((await evalIn(`${box}.checked`)) === false, "and so does reopening the dialog");

  console.log("C. the download is an archive, and only the opt-in puts a conversation in it");
  // Fetched from the page rather than through a real download: the assertion
  // is about what the server sends for the URL this pane builds, and a
  // headless download lands somewhere this test would have to go hunting for.
  const grab = async (q) => await evalIn(
    `fetch("/frag/${fx.project}/backup${q}").then(r => r.text())`);
  const plain = await grab("");
  ok(plain.startsWith("ROOSTBAK1\n"), `a layout-only backup is an archive (got ${JSON.stringify(plain.slice(0, 20))})`);
  ok(!plain.includes("the archived conversation"),
     "and holds no conversation");
  const full = await grab("?conversations=1");
  ok(full.includes("the archived conversation"),
     "the opt-in puts the transcript in — so the assertion above is a decision, not an empty directory");
  ok(full.includes("a remembered thing"), "and the project's memory travels with it");
  ok(/"kind":"transcript","id":"aaaa1111/.test(full), "as a typed entry keyed by session id");
  // The property the whole format exists for: no absolute path anywhere in the
  // body except the header's informational `source`.
  const afterHeader = full.slice(full.indexOf("\n", full.indexOf("\n") + 1));
  ok(!afterHeader.includes(srcT), `no entry names the source directory (${srcT})`);

  console.log("D. restoring into a different project, at a different path");
  // Written to disk so the file input gets a real file, through the real
  // upload endpoint — the restore names a file that must already be there.
  const archive = `${fx.base}/one.roostbak`;
  await Deno.writeFile(archive, new TextEncoder().encode(full));
  try { await page.close(); } catch { /* already gone */ }
  page = await load("dest");
  await openBackupPane(page);

  const objectId = (await page.cmd("Runtime.evaluate", {
    expression: `document.querySelector('#dlg-settings .bk-file')`,
  })).result?.result?.objectId;
  ok(!!objectId, "the restore file input is there");
  await page.cmd("DOM.enable");
  await page.cmd("DOM.setFileInputFiles", { files: [archive], objectId });
  await page.evalIn(`document.querySelector('#dlg-settings .bk-file').dispatchEvent(new Event("change")); 0`);

  const report = `document.querySelector('#dlg-settings .bk-report')`;
  ok(await until(() => page.evalIn(`!!(${report})`), 20, "the dry-run listing"),
     "choosing a file produces a listing");
  const lines = await page.evalIn(`${report}.textContent`);
  ok(/different project/.test(lines), "which says this archive came from another project");
  ok(/Nothing has been written yet/.test(lines), "and that nothing has happened yet");
  // The assertion this whole file is for: the destination named in the plan is
  // the *destination's* directory, not the source's.
  ok(lines.includes(destT), `the conversation is bound for ${destT} (got ${JSON.stringify(lines)})`);
  ok(!lines.includes(srcT), "and not for the directory it came from");

  console.log("E. and the second click actually restores");
  // The positive half. On its own this passes with the guard deleted, which
  // is why section G exists — see the note there.
  ok(await page.evalIn(`!!document.querySelector('#dlg-settings .bk-restore')`),
     "a Restore button appears once a listing has come back");
  await page.evalIn(`document.querySelector('#dlg-settings .bk-restore').click(); 0`);
  const landed = `${destT}/${ID}.jsonl`;
  const arrived = await until(async () => {
    try { return (await Deno.readTextFile(landed)) === BODY; } catch { return false; }
  }, 20, "the restored transcript");
  ok(arrived, `the conversation landed at ${landed}`);
  ok(await until(() => page.evalIn(
       `/Nothing has been written yet/.test(${report}.textContent) === false`), 10, "the real report"),
     "and the listing no longer claims nothing was written");
  ok(!(await page.evalIn(`!!document.querySelector('#dlg-settings .bk-restore')`)),
     "the Restore button is gone, so the same plan cannot be run twice");
  ok(/still in this project/.test(await page.evalIn(`${report}.textContent`)),
     "and the report says the uploaded archive was left behind, not deleted");

  console.log("F. a restore never overwrites a conversation already here");
  const mine = "{\"type\":\"user\",\"message\":{\"content\":\"mine, written after\"}}\n";
  await Deno.writeTextFile(landed, mine);
  await page.evalIn(`document.querySelector('#dlg-settings .dlg-cancel').click(); 0`);
  await openBackupPane(page);
  const objectId2 = (await page.cmd("Runtime.evaluate", {
    expression: `document.querySelector('#dlg-settings .bk-file')`,
  })).result?.result?.objectId;
  await page.cmd("DOM.setFileInputFiles", { files: [archive], objectId: objectId2 });
  await page.evalIn(`document.querySelector('#dlg-settings .bk-file').dispatchEvent(new Event("change")); 0`);
  ok(await until(() => page.evalIn(`!!(${report}) && /is already here/.test(${report}.textContent)`), 20,
       "the second listing"),
     "the plan says it will leave the existing conversation alone");
  await page.evalIn(`document.querySelector('#dlg-settings .bk-restore').click(); 0`);
  await until(() => page.evalIn(
    `/Nothing has been written yet/.test(${report}.textContent) === false`), 20, "the second restore");
  // On content, not on the reported skip: "reported a skip" is also true of a
  // restore that reports one and then writes anyway through a second path.
  ok((await Deno.readTextFile(landed)) === mine,
     "and the conversation on this machine is byte-for-byte untouched");
  console.log("G. a refused archive offers nothing to click");
  // The discriminating case for the two-click gate, and it took a revert-check
  // to find it. Asserting that Restore *exists* after a listing arrives is true
  // whether or not the guard is there; asserting it is absent *before* one
  // arrives is a race. A refusal is neither: the upload succeeded, so the pane
  // has a file — and a Restore button here would let someone restore an archive
  // the server has just told them it will not read.
  const junk = `${fx.base}/not-an-archive.roostbak`;
  await Deno.writeTextFile(junk, "PK\u0003\u0004 this is a zip, not a roost backup\n");
  await page.evalIn(`document.querySelector('#dlg-settings .dlg-cancel').click(); 0`);
  await openBackupPane(page);
  const objectId3 = (await page.cmd("Runtime.evaluate", {
    expression: `document.querySelector('#dlg-settings .bk-file')`,
  })).result?.result?.objectId;
  await page.cmd("DOM.setFileInputFiles", { files: [junk], objectId: objectId3 });
  await page.evalIn(`document.querySelector('#dlg-settings .bk-file').dispatchEvent(new Event("change")); 0`);
  const refused = `document.querySelector('#dlg-settings .bk-refused')`;
  ok(await until(() => page.evalIn(`!!(${refused})`), 20, "the refusal"),
     "a file that is not an archive is refused");
  ok(/not a roost backup/.test(await page.evalIn(`${refused}.textContent`)),
     "and the refusal says what is wrong with it");
  ok(!(await page.evalIn(`!!document.querySelector('#dlg-settings .bk-restore')`)),
     "with nothing to click: a refused archive must not be restorable");
  ok(!(await page.evalIn(`!!(${report})`)), "and no listing is shown for it");
  console.log("H. the pane works on a phone");
  // ~/projects/CLAUDE.md: every web project must work on desktop *and* mobile,
  // no exceptions. A settings pane with a two-column row and a fixed-width
  // button is exactly what that rule exists for.
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await page.evalIn(`document.querySelector('#dlg-settings .dlg-cancel').click(); 0`);
  await openBackupPane(page);
  const fits = await page.evalIn(`(() => {
    const p = ${pane};
    return { overflow: p.scrollWidth - p.clientWidth,
             dl: document.querySelector('#dlg-settings .bk-download').getBoundingClientRect().height,
             // Geometry, not width. A width threshold does not discriminate:
             // measured, the two-column row still gives the sentence 298px at
             // 390px wide, so any threshold loose enough to pass one-column
             // passes two-column too. Whether the switch sits *below* the text
             // or beside it is the property, and it has one answer.
             stacked: ${box}.getBoundingClientRect().top
                      >= document.querySelector('#dlg-settings .dlg-backup .doc').getBoundingClientRect().bottom };
  })()`);
  ok(fits.overflow <= 1, `the pane does not scroll sideways (overflow ${fits.overflow}px)`);
  // 44px is the tap-target floor the same file sets.
  ok(fits.dl >= 44, `the Download button is tappable (${fits.dl}px tall)`);
  // The row collapses to one column below 640px, so the explanation sentence
  // gets the full width instead of sharing it with the switch. Revert-checked:
  // deleting the `grid-template-columns: 1fr` from the mobile block fails this
  // and nothing else.
  ok(fits.stacked, "the switch sits below its explanation, not beside it");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
