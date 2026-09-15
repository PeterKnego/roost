//! The workspace connection has a visible state, and `send` stops lying.
//!
//! Everything the user does to the workspace is an intent on one socket —
//! opening a file, switching a tab, saving, renaming, moving a tab between
//! panes, forty-six call sites. That socket used to drop every one of them on
//! the floor when it was down, with no indicator anywhere on the page: close
//! a laptop, open it an hour later, and roost looked completely fine while
//! quietly discarding everything you did to it.
//!
//! Traps this file is written against (see README):
//!
//!   - **The connection is cut at a TCP proxy, never with
//!     `Network.emulateNetworkConditions {offline:true}`.** That one blocks
//!     *new* requests and leaves established sockets open, so the test would
//!     assert a reconnect while nothing had ever disconnected. This is the
//!     first entry in the README's trap table and it is the whole reason this
//!     file has a proxy in it.
//!   - **Section C asserts the banner appears *once*, not at least once.**
//!     `EditBuffer` fires on a 200 ms debounce and `ShareSelection` on every
//!     selection change, so "reports the failure" and "buries the page in
//!     identical messages" both satisfy an at-least-once assertion.
//!   - **Section D asserts an intent lands after the reconnect**, by its
//!     effect on server state rather than by the socket's `readyState`. A
//!     socket object in state 1 is not evidence that anything reaches roost —
//!     which is the inverted form of the absence-of-evidence rule this
//!     codebase is built around.
//!
//! Run: deno run -A tests/browser/watchdog.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startProxy, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.writeTextFile(`${fx.roots}/proj/one.txt`, "one\n");
await Deno.writeTextFile(`${fx.roots}/proj/two.txt`, "two\n");

const port = await freePort();
const proxyPort = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const proxy = startProxy({ listenPort: proxyPort, upstreamPort: port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${proxyPort}/proj`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof send === 'function' && !!state"), 20, "app.js");

  const connState = async () => JSON.parse(await evalIn(`(() => {
    const el = document.getElementById("connstate");
    return JSON.stringify({ hidden: el.hidden, state: el.dataset.state || "",
                            text: el.textContent });
  })()`));
  const tabs = async () => JSON.parse(await evalIn(
    `JSON.stringify(state.panes[2].tabs.map((t) => t.rel ?? t.k))`));

  console.log("\nA. connected: the indicator says nothing");
  ok(await until(async () => (await connState()).state === "live", 10, "live"),
    "the socket is up and the header state is live");
  ok((await connState()).hidden,
    "and the indicator is hidden — a permanent 'connected' badge is clutter, not news");

  console.log("\nB. the connection dies: the page says so");
  // Cut at the proxy. `Network.emulateNetworkConditions {offline:true}` would
  // leave this socket open and prove nothing — see the header.
  proxy.cut();
  ok(
    await until(async () => !(await connState()).hidden, 15, "indicator shown"),
    `the indicator appears when the socket dies — got ${JSON.stringify(await connState())}`,
  );
  const shown = await connState();
  ok(/reconnect|offline/i.test(shown.text), `and says what is wrong — "${shown.text}"`);

  console.log("\nC. send() refuses instead of silently dropping");
  await evalIn(`window.__errs = []; const __se = showError;
    window.showError = (m) => { window.__errs.push(m); return __se(m); }; 0`);
  const before = await tabs();
  const first = await evalIn(`send({ t: "OpenTab", pane: 2,
    tab: { k: "File", rel: "one.txt", mode: "Edit" } })`);
  ok(first === false, `send() reports that the intent did not go out — got ${JSON.stringify(first)}`);
  await sleep(400);
  ok(JSON.stringify(await tabs()) === JSON.stringify(before),
    "and nothing happened, which is the honest outcome");
  ok(
    JSON.parse(await evalIn(`JSON.stringify(window.__errs)`)).length === 1,
    `the user is told once — got ${await evalIn(`JSON.stringify(window.__errs)`)}`,
  );

  // Twenty more refusals must not produce twenty more banners. EditBuffer
  // alone would fire several a second while someone is typing.
  await evalIn(`for (let i = 0; i < 20; i++) send({ t: "EditBuffer", rel: "one.txt", text: "x" + i }); 0`);
  const errs = JSON.parse(await evalIn(`JSON.stringify(window.__errs)`));
  ok(errs.length === 1,
    `and only once per outage, not once per intent — got ${errs.length} messages`);

  console.log("\nD. the connection comes back, and intents land again");
  proxy.resume();
  ok(
    await until(async () => (await connState()).state === "live", 30, "reconnected"),
    `the socket reconnects on its own — got ${JSON.stringify(await connState())}`,
  );
  ok((await connState()).hidden, "and the indicator goes away again");

  // Asserted by effect on server state, not by readyState: a socket object in
  // state 1 is not evidence that anything reaches roost.
  const sent = await evalIn(`send({ t: "OpenTab", pane: 2,
    tab: { k: "File", rel: "two.txt", mode: "Edit" } })`);
  ok(sent === true, "send() reports the intent went out");
  ok(
    await until(async () => (await tabs()).includes("two.txt"), 10, "tab opened"),
    `and it really reached roost — got ${JSON.stringify(await tabs())}`,
  );

  // A second outage must warn again: the flag is per-outage, not per-page.
  proxy.cut();
  ok(await until(async () => !(await connState()).hidden, 15, "second outage"),
    "setup: the second outage really happened — `until` returns false rather than throwing, so an unasserted one here would let the CloseTab below actually close a tab and blame the wrong thing");
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: 0 }); 0`);
  const errs2 = JSON.parse(await evalIn(`JSON.stringify(window.__errs)`));
  ok(errs2.length === 2,
    `a later outage is reported again rather than staying quiet — got ${errs2.length}`);
  proxy.resume();
  await until(async () => (await connState()).state === "live", 30, "back for E");

  console.log("\nE. text typed during an outage is not thrown away on reconnect");
  // The scenario the rest of this file makes *visible* but did not make safe.
  // `pushEdit` calls `send` and ignores the answer, so an EditBuffer sent
  // while the socket is down never reaches roost: the server's buffer stays
  // Clean with an unchanged base_hash. On reconnect `resolve_replay`'s
  // `Content::Clean if base_hash == snapshot_base_hash` arm hands back the
  // *disk* text, the client's BufferText handler assigns it into the
  // textarea, and the paragraph you typed while offline is gone — with the
  // header now cleared to live, actively saying it landed.
  await evalIn(`send({ t: "OpenTab", pane: 2,
    tab: { k: "File", rel: "one.txt", mode: "Edit" } }); 0`);
  // Addressed through `editors`, not by "whatever textarea is in pane 2".
  // Section D leaves `two.txt` open there, so a positional selector picks up
  // the wrong file's textarea and the assertion below then reports the disk
  // text of a file nobody typed into — which is exactly how this section
  // failed while the fix it tests was working.
  const TA = `editors.get("one.txt")`;
  ok(
    await until(async () => await evalIn(`(() => { const p = state.panes[2];
      const t = p.tabs[p.active];
      return !!(t && t.k === "File" && t.rel === "one.txt" && editors.has("one.txt")); })()`),
      10, "one.txt editor"),
    "setup: one.txt is the active tab and its editor is mounted",
  );

  proxy.cut();
  ok(await until(async () => !(await connState()).hidden, 15, "outage for E"),
    "setup: the connection is down again");

  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await cmd("Input.insertText", { text: "typed while offline\n" });
  ok(await until(async () => (await evalIn(`${TA}.value`)).includes("typed while offline"), 5, "typed"),
    "setup: the text is in the buffer");
  // Past the 200 ms edit debounce, so pushEdit really has fired and really
  // has been refused. Without this the test would prove nothing: the edit
  // would still be pending when the socket came back.
  await sleep(1200);

  proxy.resume();
  ok(await until(async () => (await connState()).state === "live", 30, "reconnected for E"),
    "setup: the connection comes back");
  // Give the replay every chance to land before believing the text survived.
  await sleep(1500);
  ok(
    (await evalIn(`${TA}.value`)).includes("typed while offline"),
    `text typed during the outage survives the reconnect — got ${JSON.stringify(await evalIn(`${TA}.value`))}`,
  );
  // The other half, and the one the assertion above cannot see. Keeping the
  // text on screen only means the client refused to be overwritten; if the
  // edit never reaches roost it lives in this tab alone and a reload loses
  // it. Verified by reverting the reconnect flush: the assertion above stays
  // green and this one goes red.
  ok(
    await until(async () => await evalIn(
      `!!(state.buffers || []).find((b) => b.rel === "one.txt" && b.dirty)`), 15, "server has it"),
    `and roost received it — buffers: ${await evalIn(`JSON.stringify(state.buffers)`)}`,
  );
} finally {
  if (page) page.close();
  browser.close();
  proxy.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
