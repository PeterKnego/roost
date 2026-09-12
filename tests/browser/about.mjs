//! The About pane: what binary is this, actually.
//!
//! #56. roost is deployed by building it and copying a binary about, and
//! nothing in the UI could answer "is this the thing I built?" — a question
//! this project has already got wrong twice (CLAUDE.md, "Verify, don't
//! trust": the running binary that did not change, and the build from a
//! second checkout that cargo reported as `Fresh`), and once more on
//! 2026-09-10, when a phone was reported as still broken after a CSS fix it
//! had never fetched.
//!
//! Driven in a browser because the values cross three layers to get here —
//! `build.rs` bakes them in, `config::build_info` reads them, the settings
//! snapshot carries them, and `dialog.js` renders them. A Rust test covers the
//! first two; only this can see the last.
//!
//! Run: deno run -A tests/browser/about.mjs
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

  console.log("A. the tab exists and is reachable");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "the dialog"),
     "the settings dialog opens");
  ok(await evalIn(`!!document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]')`),
     "and carries an About tab beside General and Theme");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  ok(await evalIn(`!document.querySelector("#dlg-settings .dlg-about").hidden
                   && document.querySelector("#dlg-settings .dlg-rows").hidden`),
     "selecting it shows the About pane and hides the settings rows");
  // Nothing here is editable, so the control that chooses *which file* a
  // change is written to has nothing to say.
  ok(await evalIn(`document.querySelector("#dlg-settings .dlg-scope").hidden`),
     "and the scope switch is gone, since nothing on this pane is written anywhere");

  console.log("B. it reports this binary, not a placeholder");
  const readRows = () => evalIn(`(() => {
    const out = {};
    for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
      out[r.querySelector("label").textContent] = r.querySelector(".aboutval").textContent.trim();
    }
    return out; })()`);
  const rows = await readRows();
  ok(!!rows.Version && !!rows.Commit && !!rows.Built && !!rows.Repository
       && !!rows.Installed && !!rows.Upgrades,
     `all six rows are present: ${JSON.stringify(Object.keys(rows))}`);
  // The assertion that makes this pane worth having: the commit shown is the
  // commit the checkout is on. Compared against `state.settings.build`, which
  // the Rust test has already tied to `build.rs` — so a rendering that
  // invented a value, or read the wrong field, fails here.
  const server = await evalIn(`state.settings.build`);
  ok(rows.Commit === server.commit && server.commit !== "unknown",
     `the commit is the server's own (${rows.Commit})`);
  ok(rows.Version === server.version, `and so is the version (${rows.Version})`);
  // Provenance (#65 step 1b). Asserted against the server's own fields, not
  // against a hardcoded phrase, so a renderer that invented a value or read
  // the wrong field fails — the same reason the commit assertion above
  // compares with `state.settings.build`.
  //
  // The harness runs roost from `target/`, in a checkout, in a directory cargo
  // just wrote. So the server must report exactly this, and the row must be
  // the phrase that follows from it. A test asserting only "some phrase is
  // shown" would pass with the two fields swapped.
  ok(server.channel === "checkout" && server.owner === "other" && server.replaceable === "yes",
     `the server describes its own install (channel=${server.channel} owner=${server.owner} replaceable=${server.replaceable})`);
  ok(rows.Installed === "built from a checkout",
     `and the row says how it got here (${JSON.stringify(rows.Installed)})`);
  // A checkout is yours to rebuild — #65 rules out roost touching a working
  // tree, and the probe alone would say yes because `target/` is writable by
  // definition. So the channel has to override the probe here.
  ok(rows.Upgrades === "yours to rebuild",
     `and who may replace it (${JSON.stringify(rows.Upgrades)})`);
  // And the row that only appears when there is something to type. This host
  // is a checkout, so there must be nothing — asserted as an absence, which is
  // the half a table of present cases cannot cover.
  ok(rows.Upgrade === undefined,
     `no command is offered for a checkout (${JSON.stringify(rows.Upgrade)})`);

  // The row's *presence*, driven through the real renderer.
  //
  // Switching tabs re-renders from `state.settings` by reference; closing and
  // reopening does not work, because the settings button sends `RequestState`
  // and the server's own values overwrite anything set here. That cost four
  // failing assertions to discover, so it is written down.
  const asAbout = async (build) => {
    await evalIn(`Object.assign(state.settings.build, ${JSON.stringify(build)}); 0`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
    return {
      rows: await readRows(),
      codes: await evalIn(`(() => {
        const r = [...document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")]
          .find((x) => x.querySelector("label").textContent === "Upgrade");
        return r ? [...r.querySelectorAll(".aboutval code")].map((c) => c.textContent) : null; })()`),
    };
  };
  const original = await evalIn(`JSON.parse(JSON.stringify(state.settings.build))`);

  const brew = await asAbout({ owner: "homebrew", channel: "release", replaceable: "yes" });
  ok(brew.rows.Upgrade === "brew upgrade roost",
     `a Homebrew install is given its command (${JSON.stringify(brew.rows.Upgrade)})`);
  // As <code> elements, not a formatted string: the value is a command, and
  // the row builds it the way every other value on this page is built.
  ok(JSON.stringify(brew.codes) === JSON.stringify(["brew upgrade roost"]),
     `rendered as one code element (${JSON.stringify(brew.codes)})`);

  const deb = await asAbout({ owner: "system-package", channel: "release", replaceable: "no" });
  ok(deb.codes && deb.codes.length === 2,
     `a distro package gets both commands, since nothing here knows which host this is (${JSON.stringify(deb.codes)})`);
  ok(deb.codes && !deb.codes.some((c) => c.includes("apt upgrade")),
     "and never `apt upgrade`, which would find nothing: no package repository is published");

  const tarball = await asAbout({ owner: "other", channel: "release", replaceable: "yes" });
  ok(tarball.rows.Upgrade === undefined && tarball.codes === null,
     `the row is absent where roost will offer the button instead (${JSON.stringify(tarball.rows.Upgrade)})`);

  await asAbout(original); // leave the pane describing this binary again

  // The branches this host cannot be put into, tested on the functions the
  // renderer actually calls. `installLabel` and `upgradesLabel` are top-level
  // in dialog.js precisely so this is possible: driving them through the UI is
  // not, because opening the dialog sends `RequestState` and the server's own
  // values overwrite anything the test sets.
  //
  // The Homebrew row is the one the design turns on. A Cellar directory IS
  // writable — the probe answers `yes` — and overwriting it would still be
  // wrong, because `brew` would then describe a file that is not there. So a
  // package manager's copy must not become "roost may replace it" just
  // because the probe said yes, and this case asserts exactly that pairing.
  const label = async (build) => JSON.parse(await evalIn(
    `JSON.stringify([installLabel(${JSON.stringify(build)}), upgradesLabel(${JSON.stringify(build)}),
                     upgradeCommand(${JSON.stringify(build)})])`));

  for (const [build, want, why] of [
    [{ owner: "homebrew", channel: "release", replaceable: "yes" },
      ["Homebrew", "whatever installed it", ["brew upgrade roost"]],
      "a writable Cellar is still not roost's to replace"],
    [{ owner: "system-package", channel: "release", replaceable: "no" },
      ["a system package", "whatever installed it",
        ["sudo apt install ./roost_*.deb", "sudo dnf install ./roost-*.rpm"]],
      "a .deb in /usr/bin gets both commands and never `apt upgrade`, which finds nothing"],
    [{ owner: "cargo-bin", channel: "cargo", replaceable: "yes" },
      ["cargo install", "roost can replace this copy", ["cargo install roost --force"]],
      "cargo install and the shell installer share a directory, so the channel splits them"],
    [{ owner: "cargo-bin", channel: "release", replaceable: "yes" },
      ["the shell installer", "roost can replace this copy", null],
      "...and the shell installer gets no command, because roost will offer the button"],
    [{ owner: "other", channel: "release", replaceable: "unknown" },
      ["the release tarball", "unknown", null],
      "an unanswered probe says so rather than guessing, and still offers no command"],
    [{ owner: "other", channel: "checkout", replaceable: "yes" },
      ["built from a checkout", "yours to rebuild", null],
      "a writable checkout is still not roost's to update"],
    [{ owner: "unknown", channel: "unknown", replaceable: "unknown" },
      ["unknown", "unknown", null],
      "nothing known reads as nothing known"],
  ]) {
    const got = await label(build);
    ok(JSON.stringify(got) === JSON.stringify(want), `${why} (${JSON.stringify(got)})`);
  }
  ok((await evalIn(`document.querySelector("#dlg-settings .dlg-about a").href`)).startsWith(server.repository),
     "and the repository link points at the server's own repository");
  // A tree with edits in it must say so. "Built from a1b2c3d" is false in the
  // common case of a local build, and this pane exists to be trusted.
  ok(/^[0-9a-f]{7,}(-dirty)?\??$/.test(server.commit),
     `the commit is a hash, optionally marked dirty: ${server.commit}`);
  ok(rows.Built !== "unknown" && !/1970/.test(rows.Built),
     `the build time is a real time, not the epoch: ${rows.Built}`);

  console.log("C. it looks like the rest of the dialog");
  // Reported: "prov nč v stilu ostalega dela appa". The first cut reused
  // `.ro-row`, the single-column shape meant for one long read-only path, and
  // gave the pane no gutter at all — four short values came out full-bleed
  // against the dialog edge, each on two lines, under a scope switch that had
  // nothing to switch.
  //
  // Asserted by *comparison* rather than against numbers: the claim is that
  // this pane lines up with General, and a hard-coded 14px would keep passing
  // if General moved.
  const leftOf = async (sel) => await evalIn(
    `Math.round(document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect().left)`);
  const aboutLeft = await leftOf("#dlg-settings .dlg-about .dlg-row label");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
  await sleep(200);
  const generalLeft = await leftOf("#dlg-settings .dlg-rows .dlg-row label");
  ok(Math.abs(aboutLeft - generalLeft) <= 1,
     `its labels start where General's do (${aboutLeft} vs ${generalLeft})`);

  // The scope switch chooses which file a change is written to, and Save
  // writes it. Neither has anything to say about four read-only values.
  ok(await evalIn(`!document.querySelector("#dlg-settings .dlg-scope").hidden`),
     "General shows the scope switch");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  ok(!(await evalIn(`!!document.querySelector("#dlg-settings .dlg-scope").offsetParent`)),
     "About hides it — and really hides it, not just sets the attribute");
  ok(!(await evalIn(`!!document.querySelector("#dlg-settings .dlg-ok").offsetParent`)),
     "and hides Save, which would invite the question of what it saves");
  ok((await evalIn(`document.querySelector("#dlg-settings .dlg-cancel").textContent`)) === "Close",
     "leaving one button, named for what it does");

  console.log("D. the repository is a link");
  const link = await evalIn(`(() => {
    const a = document.querySelector("#dlg-settings .dlg-about a");
    return a ? { href: a.href, target: a.target, rel: a.rel } : null; })()`);
  ok(!!link && link.href.startsWith("https://"), `the repository is a link (${link && link.href})`);
  ok(link && link.target === "_blank" && link.rel.includes("noopener"),
     "opened in a new tab, with noopener — the workspace must not be navigated away from");
  // Not the UA's blue-and-underlined. Every other link in this app is the
  // accent, and a browser-default link inside a themed dialog is the one thing
  // that cannot be mistaken for part of it.
  const linkColor = await evalIn(`getComputedStyle(document.querySelector("#dlg-settings .dlg-about a")).color`);
  const accent = await evalIn(`getComputedStyle(document.documentElement).getPropertyValue("--accent").trim()`);
  ok(linkColor !== "rgb(0, 0, 238)" && linkColor !== "rgb(0, 0, 255)",
     `it is not the browser's default link blue (${linkColor})`);
  ok(!!accent, `and the theme defines the accent it uses (${accent})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
