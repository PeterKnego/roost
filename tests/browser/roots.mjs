//! The front page's `+`: making a project, adding a root, and creating a root
//! that is not there yet. One field, and what you type decides — absolute is a
//! root, anything else is a project under one. Starts roost with ROOST_ROOTS
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
//! Revert-checks for the `+`'s two new halves (all restored):
//!   (f) making `makeProject` always take `roots[0]` and never ask -> D's
//!       "with two roots it asks which one" and "both roots are offered"
//!       fail. B2 stays green, which is what says it is the *other* branch:
//!       with one root the two implementations are indistinguishable.
//!   (g) deleting the `dlg-choice` shell from `render::overview_page` -> B2's
//!       "the front page ships the choice dialog its own script calls" and
//!       both of D's fail. Written as two assertions rather than one for a
//!       reason: `.open` on a missing shell throws, and an uncaught
//!       TypeError takes the run down without reporting a single section
//!       after it.
//!   (h) `add_root` creating whether or not `create` was set -> E's "Cancel
//!       creates nothing" fails, which is the assertion that makes the
//!       confirmation a confirmation rather than a delay.
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
  // One field, two meanings — so the label has to say both, or someone types
  // a name expecting a root (or a path expecting a project).
  const label0 = await evalIn(`document.querySelector("#dlg-text .dlg-label").textContent`);
  ok(/absolute path/i.test(label0) && /project/i.test(label0), `the label names both halves (got ${JSON.stringify(label0)})`);
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(fx.roots)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`!!document.querySelector('#ovprojects .ovrow')`), 15, "projects"), "the projects list appeared");
  ok(await until(() => evalIn(`[...document.querySelectorAll("header .roots .root")].some((r) => r.textContent === ${JSON.stringify(fx.roots)})`), 5, "header"), "the header shows the root");
  ok(await until(async () => /roots = \[/.test(await Deno.readTextFile(globalToml)), 5, "file"), "the global file holds the list");
  ok(/^# global\n/.test(await Deno.readTextFile(globalToml)), "and kept its comment");

  console.log("\nB2. with one root, a name makes a project and asks nothing");
  // The other half of section D. With one root there is no choice to make, and
  // a dialog that asked anyway would be a question with one answer. Tested
  // here, while exactly one root is configured, because two lines later there
  // are two and this branch becomes unreachable.
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = "solo"; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(async () => {
    try { return (await Deno.stat(`${fx.roots}/solo/.git`)).isDirectory; } catch { return false; }
  }, 15, "the solo project"), "it lands in the only root, git initialised");
  // Two assertions, not one. `.open` on a missing shell throws and takes the
  // whole run down with an uncaught TypeError — legible enough to debug, but
  // not a failing assertion, and a run that dies here reports nothing about
  // the sections after it. Revert-checked by deleting the shell from
  // `render::overview_page`: this now fails as a normal assertion, and the
  // Rust test `the_front_page_ships_every_dialog_shell_its_own_script_asks_for`
  // catches the same removal without a browser at all.
  ok(await evalIn(`!!document.getElementById("dlg-choice")`),
     "the front page ships the choice dialog its own script calls");
  ok(!(await evalIn(`!!document.getElementById("dlg-choice")?.open`)), "and nothing was asked");

  console.log("\nC. + adds a second root");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog again");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(second)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`document.querySelectorAll("header .roots .root").length === 2`), 5, "two roots"), "the header shows two roots");
  ok(await until(() => evalIn(`[...document.querySelectorAll("#ovprojects .ovrow")].some((r) => /other/.test(r.textContent))`), 15, "other project"), "and the second root's project is listed");

  console.log("\nD. a name makes a project, and with two roots it asks which");
  // The report this change came from: "ko klikneš add button v seznamu
  // projektov, bi mogel sam sprejet ime pa kreirat folder, pa ga ne."
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = "mqtt-bridge"; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  // Two roots are configured by now, so a silent choice about where a folder
  // lands on disk is exactly what must not happen.
  ok(await until(() => evalIn(`document.getElementById("dlg-choice").open`), 5, "the root choice"),
     "with two roots it asks which one");
  const choices = JSON.parse(await evalIn(
    `JSON.stringify([...document.querySelectorAll("#dlg-choice .dlg-choice")].map((b) => b.dataset.choice))`));
  ok(choices.length === 2 && choices.includes(fx.roots) && choices.includes(second),
     `both roots are offered (got ${JSON.stringify(choices)})`);
  // Picked deliberately: the *second* root. A client that ignored the choice
  // and a server that always took roots[0] would both pass against the first.
  await evalIn(`document.querySelector('#dlg-choice .dlg-choice[data-choice=${JSON.stringify(second)}]').click(); 0`);
  ok(await until(async () => {
    try { return (await Deno.stat(`${second}/mqtt-bridge/.git`)).isDirectory; } catch { return false; }
  }, 15, "the new project"), "the folder is created in the chosen root, with a git repository in it");
  let strayed = false;
  try { await Deno.stat(`${fx.roots}/mqtt-bridge`); strayed = true; } catch { /* expected */ }
  ok(!strayed, "and not in the root nobody chose");
  ok(await until(() => evalIn(`[...document.querySelectorAll("#ovprojects .ovrow")].some((r) => /mqtt-bridge/.test(r.textContent))`), 15, "the row"),
     "it appears in the projects list without a reload");
  ok(await until(() => evalIn(`new URLSearchParams(location.search).get("sel") === "mqtt-bridge"`), 5, "selection"),
     "and it is selected, so a reload comes back to it");

  console.log("\nE. a path that does not exist offers to create it");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  const fresh = `${fx.base}/made-by-roost`;
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(fresh)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`document.getElementById("dlg-confirm").open`), 5, "the confirm"),
     "an absolute path that is not there asks before creating it");
  // Cancel first, and assert nothing happened: a confirmation that creates
  // whichever button you press is not a confirmation.
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await sleep(300);
  let made = false;
  try { await Deno.stat(fresh); made = true; } catch { /* expected */ }
  ok(!made, "Cancel creates nothing");
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 2, "and adds no root");

  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(fresh)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-confirm").open`), 5, "the confirm again");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  ok(await until(async () => {
    try { return (await Deno.stat(fresh)).isDirectory; } catch { return false; }
  }, 15, "the created root"), "confirming creates the directory");
  ok(await until(() => evalIn(`document.querySelectorAll("header .roots .root").length === 3`), 5, "three roots"),
     "and adds it as a root");

  console.log("\nF. a refusal that is not 'missing' still reopens with the reason");
  // The other branch, and the one that says the `missing` flag is doing work:
  // a path that exists but is a *file* cannot be created into, so it must come
  // back as a refusal on the label — not as an offer to create it.
  const afile = `${fx.base}/a-file`;
  await Deno.writeTextFile(afile, "not a directory\n");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(afile)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open && /not a directory/.test(document.querySelector("#dlg-text .dlg-label").textContent)`), 5, "labelled"),
     "the reopened dialog's label names the refusal");
  ok(!(await evalIn(`document.getElementById("dlg-confirm").open`)), "and no offer to create it");
  ok(await evalIn(`document.getElementById("dlg-input").value === ${JSON.stringify(afile)}`), "with the text kept");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 3, "and the root list is unchanged");

  console.log("\nG. four long roots: the + stays in the header, and stays clickable");
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
