//! The browser bridge end to end (#161): a real Chromium with the roost
//! extension loaded, a real roost, and `roost mcp` as the subprocess Claude
//! would spawn. Nothing between the MCP request and the DevTools command in
//! the target tab is substituted.
//!
//! What only this can show: that a call crosses all five hops (stdio, the
//! broker socket, the workspace socket, postMessage, the extension worker)
//! and its answer crosses back; that the consent window is what decides; that
//! a call the page invents is refused by the extension, not the page; and
//! that the handoff page and `handoff_*` reach each other.
//!
//! Revert-checks performed, all restored:
//!   (a) the worker's signature check disabled -> J fails: the forged call is
//!       treated as real and waits on a consent prompt.
//!   (b) `allowed` returning true -> D fails: no consent window appears, and
//!       Deny cannot refuse anything.
//!   (c) app.js's `pagehide` unregistering removed -> M fails: the server
//!       still counts the departed page as a relay, and only the 5 s ack
//!       timeout (`browser::ACK_TIMEOUT`, unit-tested) gets the call out.
//!   (d) chrome_close's ownership check disabled -> N fails: the user's own
//!       tab, navigated in place by Claude, is closed.
//!   (e) the screenshot's hand-back of the front tab removed -> O fails: with
//!       follow off, Claude's tab is left in front of the user's.
//!   (f) chrome_type's select-all removed -> F fails: the input submits
//!       "stale hello roost" and the textarea holds "old wordsnew words".
//!   (g) SET_FILES building File/DataTransfer from the top frame's window ->
//!       F fails: the framed input reports framefile:false.
//!
//! Run: deno run -A tests/browser/bridge.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const roostOrigin = `http://127.0.0.1:${roost.port}`;
const meta = JSON.parse(new TextDecoder().decode(
  (await new Deno.Command("cargo", { args: ["metadata", "--format-version", "1", "--no-deps"], cwd: repoRoot, stdout: "piped" }).output()).stdout,
));
const bin = `${meta.target_directory}/debug/roost`;
const env = { ROOST_STATE_DIR: fx.stateDir, PATH: Deno.env.get("PATH") ?? "/usr/bin:/bin", HOME: Deno.env.get("HOME") ?? "/tmp" };

// The page Claude is pointed at: a button that changes the page and logs, an
// input that echoes what is submitted.
const sitePort = await freePort();
const site = Deno.serve({ port: sitePort, hostname: "127.0.0.1", onListen() {} }, () => new Response(`<!doctype html>
<title>Bridge Target</title>
<h1 id="h">untouched</h1>
<button id="b" onclick="document.getElementById('h').textContent='clicked'; console.log('button clicked')">Press me</button>
<form onsubmit="event.preventDefault(); document.getElementById('h').textContent='typed:' + document.getElementById('q').value">
<input id="q" aria-label="query" value="stale "></form>
<textarea id="t" aria-label="notes" oninput="document.getElementById('tv').textContent='area:' + this.value">old words</textarea>
<p id="tv"></p>
<input type="file" id="f" multiple aria-label="files" onchange="document.getElementById('fv').textContent='files:' + [...this.files].map((f) => f.name + '/' + f.size + '/' + f.type).join(',')">
<input type="file" id="hf" style="display:none" onchange="this.files[0].text().then((t) => document.getElementById('hv').textContent='picked:' + this.files[0].name + '=' + t)">
<button id="pick" onclick="document.getElementById('hf').click()">Pick a file</button>
<p id="fv"></p><p id="hv"></p>
<iframe srcdoc="<button onclick=&quot;parent.document.getElementById('h').textContent='framed'&quot;>Inside frame</button><input id=fi value=frame-stale><input type=file id=ff onchange=&quot;parent.document.getElementById('h').textContent='framefile:' + (this.files[0] instanceof File) + '/' + this.files[0].name&quot;>"></iframe>
<button id="corner" style="position:fixed;right:0;bottom:0;width:160px;height:80px" onclick="document.getElementById('h').textContent='cornered'">corner</button>`, { headers: { "content-type": "text/html" } }));
const target = `http://127.0.0.1:${sitePort}/`;

// A profile of its own: the extension's storage persists in it, and a grant
// or key left over from another run would make every check below meaningless.
const prof = `${profileDir(repoRoot)}-bridge`;
await Deno.remove(prof, { recursive: true }).catch(() => {});
const ext = `${repoRoot}/extension`;
const browser = await startBrowser(prof, [`--disable-extensions-except=${ext}`, `--load-extension=${ext}`]);

// ---- roost mcp, as Claude spawns it ------------------------------------------
function startMcp() {
  const p = new Deno.Command(bin, {
    args: ["mcp"], clearEnv: true, env: { ...env, ROOST_PROJECT: fx.project, ROOST_SESSION: "term" },
    stdin: "piped", stdout: "piped", stderr: "null",
  }).spawn();
  const w = p.stdin.getWriter();
  const waiting = new Map();
  let id = 0;
  (async () => {
    let buf = "";
    for await (const chunk of p.stdout.pipeThrough(new TextDecoderStream())) {
      buf += chunk;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i); buf = buf.slice(i + 1);
        if (!line.trim()) continue;
        const m = JSON.parse(line);
        waiting.get(m.id)?.(m);
        waiting.delete(m.id);
      }
    }
  })();
  const rpc = (method, params) => new Promise((res) => {
    const i = ++id;
    waiting.set(i, res);
    w.write(new TextEncoder().encode(JSON.stringify({ jsonrpc: "2.0", id: i, method, params }) + "\n"));
  });
  return {
    rpc,
    tool: async (name, args = {}) => (await rpc("tools/call", { name, arguments: args })).result,
    close: async () => { try { await w.close(); } catch {} try { p.kill("SIGKILL"); } catch {} await p.status; },
  };
}
const text = (r) => (r?.content || []).filter((c) => c.type === "text").map((c) => c.text).join("\n");

const cdpList = async () => (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();

let page, mcp, hand, worker, mine;
try {
  // ---- the extension, configured the way its options page would ----------------
  const findWorker = async () => (await cdpList()).find((t) => t.type === "service_worker" && t.url.endsWith("/worker.js"));
  await until(findWorker, 20, "the extension's worker");
  const swInfo = await findWorker();
  ok(!!swInfo, "A: the extension loaded and its worker is running");
  if (!swInfo) throw new Error("no extension worker — is this Chromium able to load extensions headless?");
  // /json/list omits a worker's webSocketDebuggerUrl, but the endpoint is
  // the same shape as a page's.
  worker = await attachTarget(swInfo.webSocketDebuggerUrl || `ws://127.0.0.1:${browser.port}/devtools/page/${swInfo.id}`);
  const key = new TextDecoder().decode((await new Deno.Command(bin, { args: ["browser-key"], clearEnv: true, env, stdout: "piped", stderr: "null" }).output()).stdout).trim();
  ok(/^[0-9a-f]{64}$/.test(key), "A: `roost browser-key` prints a key");
  await worker.evalIn(`chrome.storage.local.set(${JSON.stringify({ origins: [roostOrigin], key, unsafe: false, deny: [] })}).then(() => roostBridge.syncContentScripts()).then(() => true)`);

  mcp = startMcp();
  const init = await mcp.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "0" } });
  ok(init.result?.serverInfo?.name === "roost", "B: roost mcp initializes");

  // No roost tab yet: a call says so at once rather than waiting.
  const t0 = Date.now();
  const none = await mcp.tool("chrome_tabs");
  ok(none.isError && /extension/.test(text(none)) && Date.now() - t0 < 5000, `B: with no relay tab, a call fails at once and names the fix — ${text(none)}`);

  // ---- the relay comes up --------------------------------------------------------
  page = await openPage(browser.port, `${roostOrigin}/${fx.project}`);
  const ready = await until(() => page.evalIn("typeof bridgeReady !== 'undefined' && bridgeReady"), 20, "the content script's hello");
  ok(ready, "C: the roost page heard the content script and registered as a relay");

  // ---- consent decides -----------------------------------------------------------
  const answerConsent = async (answer) => {
    const findConsent = async () => (await cdpList()).find((t) => t.url.includes("/consent.html"));
    if (!await until(findConsent, 15, "the consent window")) return false;
    const c = await findConsent();
    const cp = await attachTarget(c.webSocketDebuggerUrl || `ws://127.0.0.1:${browser.port}/devtools/page/${c.id}`);
    await until(() => cp.evalIn("document.querySelector('#who dd') !== null"), 10, "the consent details");
    const shown = await cp.evalIn("document.getElementById('who').textContent");
    await cp.evalIn(`document.getElementById(${JSON.stringify(answer)}).click()`);
    return shown;
  };
  const denied = mcp.tool("chrome_open", { url: target });
  const shown = await answerConsent("deny");
  ok(typeof shown === "string" && shown.includes(fx.project) && shown.includes("term"), `D: the consent window names the project and terminal — ${shown}`);
  const d = await denied;
  ok(d.isError && /did not allow/.test(text(d)), `D: Deny refuses the call — ${text(d)}`);

  const allowedCall = mcp.tool("chrome_open", { url: target });
  await answerConsent("session");
  const opened = await allowedCall;
  ok(!opened.isError && /Bridge Target/.test(text(opened)), `D: Allow for this session opens the page — ${text(opened).slice(0, 120)}`);

  // From here the grant holds: no window may appear.
  const read = await mcp.tool("chrome_read");
  const r = JSON.parse(text(read));
  const btn = (r.elements || []).find((e) => e.includes("Press me"));
  ok(!!btn && r.text.includes("untouched"), `E: chrome_read sees the page and numbers its button — ${btn}`);
  ok(!(await cdpList()).some((t) => t.url.includes("/consent.html")), "E: a granted session is not asked again");

  const ref = Number(/^\[(\d+)\]/.exec(btn || "")?.[1]);
  const clicked = await mcp.tool("chrome_click", { ref });
  ok(!clicked.isError, `F: chrome_click by ref — ${text(clicked)}`);
  const after = JSON.parse(text(await mcp.tool("chrome_read")));
  ok(after.text.includes("clicked"), "F: the click was a real click: the page changed");

  const typed = await mcp.tool("chrome_type", { selector: "#q", text: "hello roost", submit: true });
  ok(!typed.isError, `F: chrome_type — ${text(typed)}`);
  const after2 = JSON.parse(text(await mcp.tool("chrome_read")));
  ok(after2.text.includes("typed:hello roost"), "F: the typed text was submitted, replacing the input's old value");
  const area = await mcp.tool("chrome_type", { selector: "#t", text: "new words" });
  ok(!area.isError, `F: chrome_type into a textarea — ${text(area)}`);
  const after3 = JSON.parse(text(await mcp.tool("chrome_read")));
  ok(after3.text.includes("area:new words"), `F: typing into a textarea replaces what it held — ${/area:.*/.exec(after3.text)?.[0]}`);

  const up = await Deno.makeTempDir();
  await Deno.writeTextFile(`${up}/a.txt`, "hello");
  await Deno.writeFile(`${up}/b.png`, new Uint8Array([137, 80, 78, 71, 0, 255]));
  const direct = await mcp.tool("chrome_upload", { selector: "#f", paths: [`${up}/a.txt`, `${up}/b.png`] });
  ok(!direct.isError, `F: chrome_upload into a file input — ${text(direct)}`);
  const after4 = JSON.parse(text(await mcp.tool("chrome_read")));
  ok(after4.text.includes("files:a.txt/5/text/plain,b.png/6/image/png"), `F: the page got both files, bytes and type intact — ${/files:.*/.exec(after4.text)?.[0]}`);
  const picked = await mcp.tool("chrome_upload", { selector: "#pick", paths: [`${up}/a.txt`] });
  ok(!picked.isError, `F: chrome_upload through a button that opens the picker — ${text(picked)}`);
  const after5 = JSON.parse(text(await mcp.tool("chrome_read")));
  ok(after5.text.includes("picked:a.txt=hello"), `F: the picker was caught and filled, no dialog — ${/picked:.*/.exec(after5.text)?.[0]}`);
  const tooMany = await mcp.tool("chrome_upload", { selector: "#pick", paths: [`${up}/a.txt`, `${up}/b.png`] });
  ok(tooMany.isError && /one file/.test(text(tooMany)), `F: a single-file picker refuses two — ${text(tooMany)}`);
  const noPicker = await mcp.tool("chrome_upload", { selector: "#h", paths: [`${up}/a.txt`] });
  ok(noPicker.isError && /no file picker/.test(text(noPicker)), `F: a target that opens no picker says so — ${text(noPicker)}`);
  const missing = await mcp.tool("chrome_upload", { selector: "#f", paths: [`${up}/nope.png`] });
  ok(missing.isError && /cannot read/.test(text(missing)), `F: a missing file fails before the browser is asked — ${text(missing)}`);
  await Deno.remove(up, { recursive: true });

  const framed = JSON.parse(text(await mcp.tool("chrome_read")));
  const inFrame = (framed.elements || []).find((e) => e.includes("Inside frame"));
  ok(/\(in frame\)/.test(inFrame || ""), `F: chrome_read numbers a same-origin iframe's button — ${inFrame}`);
  await mcp.tool("chrome_click", { ref: Number(/^\[(\d+)\]/.exec(inFrame || "")?.[1]) });
  ok(JSON.parse(text(await mcp.tool("chrome_read"))).text.includes("framed"), "F: clicking it by ref clicks inside the frame");
  const fin = (JSON.parse(text(await mcp.tool("chrome_read"))).elements || []).find((e) => e.includes("frame-stale"));
  await mcp.tool("chrome_type", { ref: Number(/^\[(\d+)\]/.exec(fin || "")?.[1]), text: "frame-new" });
  const finAfter = (JSON.parse(text(await mcp.tool("chrome_read"))).elements || []).find((e) => e.includes("(in frame)") && e.startsWith("[") && e.includes("input"));
  ok(/"frame-new"/.test(finAfter || ""), `F: typing into a framed input replaces its value — ${fin} -> ${finAfter}`);
  // The file comes from the frame's own realm: a File built in the top frame
  // fails the frame's instanceof, which is what broke Wix's uploader.
  const fup = await Deno.makeTempDir();
  await Deno.writeTextFile(`${fup}/c.txt`, "framed");
  const ffi = (JSON.parse(text(await mcp.tool("chrome_read"))).elements || []).find((e) => e.includes("(in frame)") && e.includes("file"));
  const fres = await mcp.tool("chrome_upload", { ref: Number(/^\[(\d+)\]/.exec(ffi || "")?.[1]), paths: [`${fup}/c.txt`] });
  ok(!fres.isError, `F: chrome_upload into a framed file input — ${ffi} ${text(fres)}`);
  ok(JSON.parse(text(await mcp.tool("chrome_read"))).text.includes("framefile:true/c.txt"), "F: the framed input got a File of its own window");
  await Deno.remove(fup, { recursive: true });

  // A point on the screenshot: the corner button, 20 px in from the image's bottom right.
  const shotForPoint = (await mcp.tool("chrome_screenshot")).content.find((c) => c.type === "image");
  const jpeg = Uint8Array.from(atob(shotForPoint.data), (c) => c.charCodeAt(0));
  let w = 0, hgt = 0;
  for (let i = 2; i < jpeg.length;) {
    const marker = jpeg[i + 1], len = (jpeg[i + 2] << 8) | jpeg[i + 3];
    if (marker >= 0xc0 && marker <= 0xc3) { hgt = (jpeg[i + 5] << 8) | jpeg[i + 6]; w = (jpeg[i + 7] << 8) | jpeg[i + 8]; break; }
    i += 2 + len;
  }
  const pt = await mcp.tool("chrome_click", { x: w - 20, y: hgt - 20 });
  ok(!pt.isError && JSON.parse(text(await mcp.tool("chrome_read"))).text.includes("cornered"), `F: chrome_click at a screenshot point (${w}x${hgt}) hits what is drawn there — ${text(pt)}`);

  const con = JSON.parse(text(await mcp.tool("chrome_console")));
  ok((con.messages || []).some((m) => m.includes("button clicked")), `G: chrome_console has the page's log — ${JSON.stringify(con.messages)}`);

  const shot = await mcp.tool("chrome_screenshot");
  const img = (shot.content || []).find((c) => c.type === "image");
  ok(img && img.mimeType === "image/jpeg" && img.data.startsWith("/9j/"), `H: chrome_screenshot returns a JPEG as image content (${img ? img.data.length : 0} b64 chars)`);

  // ---- where Claude's tabs go ----------------------------------------------------
  const tabInfo = (id) => worker.evalIn(`chrome.tabs.get(${id}).then(async (t) => ({
    active: t.active, windowId: t.windowId,
    group: t.groupId === -1 ? null : (await chrome.tabGroups.get(t.groupId)).title }))`);
  const firstTab = await tabInfo(JSON.parse(text(opened)).tab_id);
  ok(firstTab.group === `roost - ${fx.project}` && firstTab.active, `O: an opened page is in the session's group, in front — ${JSON.stringify(firstTab)}`);
  const wtTitle = await worker.evalIn(`roostBridge.groupTitle("repo/.claude/worktrees/worktree-bright-fox")`);
  ok(wtTitle === "roost - repo - bright-fox", `O: a worktree's group names its repo and worktree — ${wtTitle}`);
  const second = JSON.parse(text(await mcp.tool("chrome_open", { url: `${target}?second` })));
  const secondTab = await tabInfo(second.tab_id);
  ok(secondTab.group === `roost - ${fx.project}` && secondTab.windowId === firstTab.windowId,
    `O: the session's next page joins the same group — ${JSON.stringify(secondTab)}`);

  // With follow off a page opens behind, and a screenshot borrows the front
  // only for itself. The tab in front beforehand is the one that must be
  // back afterwards, not merely "some tab other than Claude's".
  await worker.evalIn(`chrome.storage.local.set({ follow: false }).then(() => true)`);
  const front = await worker.evalIn(`chrome.tabs.query({ active: true, windowId: ${secondTab.windowId} }).then(([t]) => t.id)`);
  const behind = JSON.parse(text(await mcp.tool("chrome_open", { url: `${target}?behind` })));
  const behindTab = await tabInfo(behind.tab_id);
  ok(!behindTab.active && behindTab.group === `roost - ${fx.project}`, `O: with follow off, a page opens behind — ${JSON.stringify(behindTab)}`);
  const behindShot = await mcp.tool("chrome_screenshot", { tab_id: behind.tab_id });
  const frontAfter = await worker.evalIn(`chrome.tabs.query({ active: true, windowId: ${secondTab.windowId} }).then(([t]) => t.id)`);
  ok((behindShot.content || []).some((c) => c.type === "image") && frontAfter === front,
    `O: its screenshot works and hands the front back — before ${front}, after ${frontAfter}`);
  await worker.evalIn(`chrome.storage.local.set({ follow: true }).then(() => true)`);

  const roostTab = await mcp.tool("chrome_open", { url: `${roostOrigin}/${fx.project}` });
  ok(roostTab.isError && /roost tab/.test(text(roostTab)), `I: Claude may not drive roost itself — ${text(roostTab)}`);
  const file = await mcp.tool("chrome_open", { url: "file:///etc/passwd" });
  ok(file.isError && /http/.test(text(file)), `I: chrome_open refuses a file: URL — ${text(file)}`);

  // ---- chrome_close: only what this session opened ------------------------------
  // A tab the user opened, which Claude then navigates in place: driving it
  // is allowed, closing it is not. The navigation is what makes this
  // discriminating — a check on "was this session's last tab" would pass it.
  mine = await openPage(browser.port, `${target}?mine`);
  const listed = JSON.parse(text(await mcp.tool("chrome_tabs"))).tabs;
  const mineId = listed.find((t) => String(t.url).includes("?mine"))?.id;
  ok(typeof mineId === "number", `N: the user's own tab is listed — ${mineId}`);
  const navigated = await mcp.tool("chrome_open", { url: `${target}?navigated`, new_tab: false, tab_id: mineId });
  ok(!navigated.isError, `N: Claude may navigate the user's tab in place — ${text(navigated).slice(0, 80)}`);
  const refused = await mcp.tool("chrome_close", { tab_id: mineId });
  const stillThere = JSON.parse(text(await mcp.tool("chrome_tabs"))).tabs.some((t) => t.id === mineId);
  ok(refused.isError && /not opened by this session/.test(text(refused)) && stillThere,
    `N: chrome_close refuses the user's tab, and it stays open — ${text(refused)}`);

  const own = JSON.parse(text(await mcp.tool("chrome_open", { url: `${target}?own` })));
  const closed = await mcp.tool("chrome_close", { tab_id: own.tab_id });
  const gone2 = !JSON.parse(text(await mcp.tool("chrome_tabs"))).tabs.some((t) => t.id === own.tab_id);
  ok(!closed.isError && gone2, `N: chrome_close closes a tab this session opened — ${text(closed)}`);

  // ---- a call the page invents ---------------------------------------------------
  // What injected script in a roost page could do: post a call straight to
  // the content script. The extension must refuse it, because only the
  // server holds the key.
  // Bounded: an extension that did not check the signature would treat this
  // as a real call from an ungranted session and open a consent prompt, and
  // the result would never come.
  const forged = await page.evalIn(`new Promise((res) => {
    setTimeout(() => res({ ok: null, error: "no result in 10 s — the extension did not refuse the forged call outright" }), 10000);
    addEventListener("message", function h(e) {
      if (e.data && e.data.roostBridge === 1 && e.data.from === "ext" && e.data.type === "result" && e.data.call === "forged-1") { removeEventListener("message", h); res(e.data); }
    });
    const payload = JSON.stringify({ call: "forged-1", project: ${JSON.stringify(fx.project)}, session: "term", pid: 1, tool: "chrome_tabs", args: {}, issued: Date.now() });
    window.postMessage({ roostBridge: 1, from: "page", type: "call", call: "forged-1", payload, sig: "0".repeat(64) }, location.origin);
  })`);
  ok(forged && forged.ok === false && /signature/.test(forged.error), `J: a call signed with the wrong key is refused by the extension — ${forged && forged.error}`);

  // ---- your browser -> roost Claude ----------------------------------------------
  await page.evalIn("window.__sent = null; sendTextToClaude = (t) => { window.__sent = t; }; true");
  await page.cmd("Page.bringToFront");
  await page.evalIn("dispatchEvent(new Event('focus')); true");
  await sleep(300);
  const delivered = await worker.evalIn(`roostBridge.sendToRoost({ text: "From my browser: Example\\nhttps://example.com/" })`);
  await until(() => page.evalIn("window.__sent"), 5, "the page receiving the send");
  const sent = await page.evalIn("window.__sent");
  ok(delivered === true && typeof sent === "string" && sent.includes("https://example.com/"), `K: "send to roost Claude" reaches the page's Claude paste — ${JSON.stringify(sent)}`);

  // ---- handoff, both ways --------------------------------------------------------
  hand = await openPage(browser.port, `${roostOrigin}/frag/${fx.project}/handoff`);
  await until(() => hand.evalIn("document.getElementById('status').textContent === 'connected'"), 10, "the handoff page to connect");
  const reading = mcp.tool("handoff_read", { wait_seconds: 20 });
  await sleep(300);
  await hand.evalIn(`document.getElementById('message').value = 'please check the login page'; document.getElementById('send').requestSubmit(); true`);
  const got = await reading;
  ok(/please check the login page/.test(text(got)) && /from browser/.test(text(got)), `L: handoff_read wakes on the page's message — ${text(got).trim()}`);
  await mcp.tool("handoff_send", { text: "login page is fine" });
  const shownReply = await until(() => hand.evalIn("document.getElementById('log').textContent.includes('login page is fine')"), 5, "the reply on the page");
  ok(shownReply, "L: handoff_send appears on the handoff page");

  // ---- the relay goes away -------------------------------------------------------
  // Navigating away closes the page's workspace socket, which is the
  // teardown the broker hangs its "fail at once" on.
  await page.cmd("Page.navigate", { url: "about:blank" });
  await sleep(800);
  const t1 = Date.now();
  const gone = await mcp.tool("chrome_tabs");
  ok(gone.isError && Date.now() - t1 < 5000, `M: with the relay tab closed, a call fails at once — ${text(gone)}`);
} catch (e) {
  ok(false, `threw: ${e.stack || e}`);
} finally {
  try { await page?.close(); } catch {}
  try { await hand?.close(); } catch {}
  try { await mine?.close(); } catch {}
  await mcp?.close();
  browser.close();
  await site.shutdown();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `${fail} FAILED` : "all ok");
Deno.exit(fail ? 1 : 0);
