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
  await until(async () => !(await connState()).hidden, 15, "second outage");
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: 0 }); 0`);
  const errs2 = JSON.parse(await evalIn(`JSON.stringify(window.__errs)`));
  ok(errs2.length === 2,
    `a later outage is reported again rather than staying quiet — got ${errs2.length}`);
  proxy.resume();
} finally {
  if (page) page.close();
  browser.close();
  proxy.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
