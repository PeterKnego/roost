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
//!       section D's "the dialog reopened with the text kept" failed (only
//!       the reopen-with-text-kept behavior was lost).
//!   (c) putting the refusal back on a banner — i.e. pinning `addRootFlow`'s
//!       label to the constant "Directory to scan for projects" -> section
//!       D's "the reopened dialog's label names the refusal" times out and
//!       fails. That is the point of the change: the dialog is modal and
//!       covers the banner, so the reason was rendered where it could not be
//!       read.
//!
//! Section E's revert-checks, against `static/style.css` and
//! `render::overview_page` (all restored):
//!   (d) the whole pre-fix header — `#addroot` back *inside* the roots span
//!       and `header .roots` back to `display: inline-flex; flex-wrap: wrap`
//!       -> all three of E's assertions fail: the click at the +'s centre
//!       lands on `div#ovprojects`, the list is 190px tall, and the + is
//!       outside the header's box.
//!   (e) the CSS alone (`flex-wrap: wrap` restored, the + left as a sibling)
//!       -> only "the roots list stays on one line" fails, at 162px. Worth
//!       recording rather than hiding: with the + a sibling the wrapping
//!       list no longer carries it anywhere, so it is the *markup* the
//!       clickability assertion guards and the CSS the height one guards.
//!       Neither assertion covers the other.
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
  // The refusal is the reopened dialog's own label, not a banner: the modal
  // covers a banner (verified by hand — the banner cannot even be clicked),
  // so the reason was unreadable exactly when it was needed.
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open && /no such directory/.test(document.querySelector("#dlg-text .dlg-label").textContent)`), 5, "labelled"),
     "the reopened dialog's label names the refusal");
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open && document.getElementById("dlg-input").value === "/nowhere/at/all"`), 5, "reopened"), "the dialog reopened with the text kept");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 2, "and the root list is unchanged");

  console.log("\nE. four long roots: the + stays in the header, and stays clickable");
  // Four ~60-character roots overflow any header on any window, which is the
  // state the wrapping list broke in: the list grew a second line, the header
  // is a fixed 38px, and the + was pushed out of it — still in the DOM, still
  // `getBoundingClientRect()`-able, and covered by the pane below. So the
  // assertion is what a *click* would hit, not whether the element exists.
  const long = [];
  for (let i = 0; i < 4; i++) {
    const dir = `${fx.base}/root-${i}-${"x".repeat(52)}`;
    await Deno.mkdir(dir, { recursive: true });
    long.push(dir);
  }
  await Deno.writeTextFile(globalToml, `# global\nroots = [${long.map((p) => JSON.stringify(p)).join(", ")}]\n`);
  await evalIn(`location.reload()`);
  ok(await until(() => evalIn(`document.querySelectorAll("header .roots .root").length === 4`), 20, "four roots"),
     "the header lists all four");
  const hit = await evalIn(`(() => {
    const b = document.getElementById("addroot");
    const r = b.getBoundingClientRect();
    const h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    if (!h) return "nothing";
    if (h === b || b.contains(h)) return "addroot";
    return h.tagName.toLowerCase() + (h.id ? "#" + h.id : "." + (h.className || "?"));
  })()`);
  ok(hit === "addroot", `a click at the +'s own centre lands on the + (hit: ${hit})`);
  const rootsH = await evalIn(`Math.round(document.querySelector("header .roots").getBoundingClientRect().height)`);
  ok(rootsH <= 24, `the roots list stays on one line (height ${rootsH})`);
  const inside = await evalIn(`(() => {
    const b = document.getElementById("addroot").getBoundingClientRect();
    const h = document.querySelector("header").getBoundingClientRect();
    return b.top >= h.top - 1 && b.bottom <= h.bottom + 1;
  })()`);
  ok(inside, "and the + is inside the header's box");
} finally {
  try { await page?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
