//! Adding project roots from the front page: the empty state, Add path, the
//! + beside the roots, and a refused path. Starts roost with ROOST_ROOTS
//! empty (which `roots_from` treats as unset) and the global config on an
//! empty file, so there are no roots at all.
//!
//! Revert-checks performed against `static/overview.js`'s `addRootFlow`
//! (both restored):
//!   (a) removing `renderRoots(reply.roots)` from the success branch ->
//!       section B's "the header shows the root" failed (and C's "two
//!       roots" cascaded from the same removal, since the header never
//!       gains its first entry either).
//!   (b) removing the `addRootFlow(path)` re-open from the error branch ->
//!       section D's "the dialog reopened with the text kept" failed (the
//!       banner still appeared; only the reopen-with-text-kept behavior was
//!       lost).
//!
//! Section B's "kept its comment" assertion also caught a real bug one
//! layer down: `config::write_setting` writing the *first* key into a file
//! that held nothing but a header comment moved that comment to the bottom
//! of the file (toml_edit has no item to hang a lone comment off, so it is
//! stored as the document's own trailing trivia, indistinguishable from a
//! footer to a naive insert). Fixed in `write_setting` itself, with its own
//! revert-checked unit test,
//! `config::tests::a_header_comment_on_an_otherwise_empty_file_stays_a_header`.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "# global\n");
const second = `${fx.base}/more`;
await Deno.mkdir(`${second}/other`, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: `${second}/other`, stdout: "null", stderr: "null" }).output();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: "", port: await freePort(), extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const { evalIn } = page;
  await until(() => evalIn(`!!document.querySelector("#ovprojects .ovnoroots, #ovprojects .ovtree")`), 20, "projects pane");

  console.log("A. no roots: the explanation and Add path");
  ok(await evalIn(`/no defined paths where to look for projects/.test(document.querySelector("#ovprojects .ovnoroots")?.textContent || "")`), "the pane explains the state");
  ok(await evalIn(`!!document.querySelector("#ovprojects .ovnoroots .addroot")`), "and offers Add path");
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 0, "the header lists no roots");

  console.log("\nB. Add path lists the fixture's projects and writes the file");
  await evalIn(`document.querySelector("#ovprojects .ovnoroots .addroot").click(); 0`);
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog"), "the text dialog opened");
  ok(/project root/i.test(await evalIn(`document.querySelector("#dlg-text .dlg-title").textContent`)), "titled for a root");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(fx.roots)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`!!document.querySelector('#ovprojects .ovrow')`), 15, "projects"), "the projects list appeared");
  ok(await until(() => evalIn(`[...document.querySelectorAll("header .roots .root")].some((r) => r.textContent === ${JSON.stringify(fx.roots)})`), 5, "header"), "the header shows the root");
  ok(await until(async () => /roots = \[/.test(await Deno.readTextFile(globalToml)), 5, "file"), "the global file holds the list");
  ok(/^# global\n/.test(await Deno.readTextFile(globalToml)), "and kept its comment");

  console.log("\nC. + adds a second root");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog again");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(second)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`document.querySelectorAll("header .roots .root").length === 2`), 5, "two roots"), "the header shows two roots");
  ok(await until(() => evalIn(`[...document.querySelectorAll("#ovprojects .ovrow")].some((r) => /other/.test(r.textContent))`), 15, "other project"), "and the second root's project is listed");

  console.log("\nD. a path that does not exist is refused and the text is kept");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = "/nowhere/at/all"; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`/no such directory/.test([...document.querySelectorAll(".error-banner")].map((b) => b.textContent).join(" "))`), 5, "banner"), "a banner names the refusal");
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open && document.getElementById("dlg-input").value === "/nowhere/at/all"`), 5, "reopened"), "the dialog reopened with the text kept");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 2, "and the root list is unchanged");
} finally {
  try { await page?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
