//! The browser bridge in a real Firefox (#177): the Firefox package the
//! release ships, installed into Firefox, against a real roost and `roost
//! mcp`. bridge.mjs is the same story in Chromium, through the DevTools
//! protocol; Firefox has none, and this drives the backend that stands in for
//! it (extension/drive-page.js): scripting.executeScript and tabs.captureTab.
//!
//! What it cannot drive, and so does not: the consent window. Firefox lets no
//! automation open or script a moz-extension page — not BiDi, not
//! Marionette's content context — so the extension runs in unsafe mode here,
//! with access to all sites granted the way Firefox's own extension tests
//! grant it. The consent logic is shared with Chrome and covered there; what
//! is Firefox's own about it (the Allow click asking for site access) is not
//! reached by any automated test.
//!
//! Firefox's limitations are asserted, not just tolerated: a picker-opening
//! button is refused with the reason, and click/type results carry the
//! untrusted-input note.
//!
//! Revert-checks performed, all restored:
//!   (a) drive-page.js's upload run in the ISOLATED world -> F still passes:
//!       roostPage builds File and DataTransfer from the input's own window,
//!       which in Firefox's isolated world is an Xray onto the page's, and
//!       Firefox accepts that. MAIN is kept so the objects are plainly the
//!       page's; this test does not distinguish the two.
//!   (b) roostPage's "type" assigning `el.value =` instead of the prototype
//!       setter -> F still passes on this plain page (no framework tracks the
//!       value); kept anyway for React, and noted here so nobody reads this
//!       test as covering it.
//!   (c) needSites() always passing -> D fails: without access, a read
//!       reports Firefox's own error instead of saying how to grant it.
//!   (d) content.js's once-per-page guard removed -> C fails: after the
//!       address list changes, calls come back "that call was already
//!       handled". Found by this test, intermittently, before the guard.
//!
//! Run: deno run -A tests/browser/bridge-firefox.mjs   (needs $FIREFOX or firefox on PATH)
import { fixture, freePort, startRoost, until, sleep } from "./harness.mjs";
import { startFirefox } from "./firefox.mjs";

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
<button id="pick" onclick="document.getElementById('f').click()">Pick a file</button>
<p id="fv"></p>
<iframe srcdoc="<button onclick=&quot;parent.document.getElementById('h').textContent='framed'&quot;>Inside frame</button><input id=fi value=frame-stale><input type=file id=ff onchange=&quot;parent.document.getElementById('h').textContent='framefile:' + (this.files[0] instanceof File) + '/' + this.files[0].name&quot;>"></iframe>
<button id="corner" style="position:fixed;right:0;bottom:0;width:160px;height:80px" onclick="document.getElementById('h').textContent='cornered'">corner</button>`, { headers: { "content-type": "text/html" } }));
const target = `http://127.0.0.1:${sitePort}/`;

// The Firefox package exactly as the release builds it.
const pkg = `${fx.base}/pkg`;
const pack = await new Deno.Command(`${repoRoot}/scripts/package-extension.sh`, { args: ["0.0.1", pkg], cwd: repoRoot, stdout: "null", stderr: "piped" }).output();
if (!pack.success) throw new Error(`packing failed: ${new TextDecoder().decode(pack.stderr)}`);
const extDir = `${fx.base}/ext`;
await new Deno.Command("python3", { args: ["-I", "-c", "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])", `${pkg}/roost-extension-0.0.1-firefox.zip`, extDir] }).output();

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
const json = (r) => { try { return JSON.parse(text(r)); } catch { return {}; } };

const ID = "roost-bridge@roost";
const sites = (verb) => `
  const { ExtensionPermissions } = ChromeUtils.importESModule("resource://gre/modules/ExtensionPermissions.sys.mjs");
  const policy = WebExtensionPolicy.getByID(${JSON.stringify(ID)});
  await ExtensionPermissions.${verb}(${JSON.stringify(ID)}, { permissions: [], origins: ["<all_urls>"] }, policy.extension);
  return policy.allowedOrigins.patterns.map((p) => p.pattern);`;

let ff, mcp;
try {
  ff = await startFirefox();
  const installed = await ff.installTemporary(extDir);
  ok(installed === ID, `A: Firefox installs the release's Firefox package — ${installed}`);
  const key = new TextDecoder().decode((await new Deno.Command(bin, { args: ["browser-key"], clearEnv: true, env, stdout: "piped", stderr: "null" }).output()).stdout).trim();

  // What the options page would leave behind, written where it would be.
  const configured = await ff.chrome(`
    const policy = WebExtensionPolicy.getByID(${JSON.stringify(ID)});
    const { ExtensionStorageIDB } = ChromeUtils.importESModule("resource://gre/modules/ExtensionStorageIDB.sys.mjs");
    const db = await ExtensionStorageIDB.open(ExtensionStorageIDB.getStoragePrincipal(policy.extension));
    const changes = await db.set(${JSON.stringify({ origins: [roostOrigin], key, unsafe: true, deny: [], follow: true })});
    if (changes) ExtensionStorageIDB.notifyListeners(${JSON.stringify(ID)}, changes);
    return policy.allowedOrigins.patterns.map((p) => p.pattern);`);
  ok(Array.isArray(configured) && configured.includes("http://127.0.0.1/*"), `A: loopback access is granted at install — ${JSON.stringify(configured)}`);

  mcp = startMcp();
  await mcp.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "0" } });

  // ---- the relay comes up --------------------------------------------------------
  await ff.navigate(`${roostOrigin}/${fx.project}`);
  // Asked through a call rather than app.js's `bridgeReady`: Marionette runs
  // page scripts in a sandbox that cannot see a page's lexical globals.
  const ready = await until(async () => !(await mcp.tool("chrome_tabs")).isError, 20, "the roost tab to relay");
  ok(ready, "C: the roost page relays through Firefox's content script (registered on a port-less match pattern)");

  // ---- a second copy of the content script ---------------------------------------
  // Changing the address list re-injects the content script into roost tabs
  // already open — which this one is. Two live copies would each forward
  // every call; the second is refused as a replay and that refusal can win
  // the race back to roost.
  await ff.chrome(`
    const policy = WebExtensionPolicy.getByID(${JSON.stringify(ID)});
    const { ExtensionStorageIDB } = ChromeUtils.importESModule("resource://gre/modules/ExtensionStorageIDB.sys.mjs");
    const db = await ExtensionStorageIDB.open(ExtensionStorageIDB.getStoragePrincipal(policy.extension));
    const changes = await db.set({ origins: [${JSON.stringify(roostOrigin)}, "http://127.0.0.1:1"] });
    if (changes) ExtensionStorageIDB.notifyListeners(${JSON.stringify(ID)}, changes);
    return true;`);
  await sleep(1500);
  const replays = [];
  for (let i = 0; i < 6; i++) {
    const t = await mcp.tool("chrome_tabs");
    if (t.isError) replays.push(text(t));
  }
  ok(replays.length === 0, `C: after a re-injection, calls are answered once — ${replays[0] || "6 of 6 answered"}`);

  // ---- no site access yet --------------------------------------------------------
  const opened = await mcp.tool("chrome_open", { url: target });
  ok(!opened.isError && /Bridge Target/.test(text(opened)), `D: chrome_open needs no site access — ${text(opened).slice(0, 100)}`);
  const refused = await mcp.tool("chrome_read");
  ok(refused.isError && /access to websites/.test(text(refused)), `D: without site access, a read says how to grant it — ${text(refused)}`);
  const granted = await ff.chrome(sites("add"));
  ok(granted.includes("<all_urls>"), "D: access to all sites granted");

  // ---- the page backend ----------------------------------------------------------
  const r = json(await mcp.tool("chrome_read"));
  const btn = (r.elements || []).find((e) => e.includes("Press me"));
  ok(!!btn && r.text.includes("untouched"), `E: chrome_read sees the page and numbers its button — ${btn}`);
  const framed = (r.elements || []).find((e) => e.includes("Inside frame"));
  ok(!!framed && framed.includes("(in frame)"), `E: and a same-origin iframe's — ${framed}`);

  const ref = Number(/^\[(\d+)\]/.exec(btn || "")?.[1]);
  const clicked = json(await mcp.tool("chrome_click", { ref }));
  ok(/untrusted/.test(clicked.note || ""), `F: a Firefox click says it was untrusted — ${clicked.note}`);
  ok(json(await mcp.tool("chrome_read")).text.includes("clicked"), "F: and the page took it");

  const typed = json(await mcp.tool("chrome_type", { selector: "#q", text: "hello roost", submit: true }));
  ok(/untrusted/.test(typed.note || ""), "F: typing carries the same note");
  ok(json(await mcp.tool("chrome_read")).text.includes("typed:hello roost"), "F: the input's old value was replaced and the form submitted");
  await mcp.tool("chrome_type", { selector: "#t", text: "new words" });
  ok(json(await mcp.tool("chrome_read")).text.includes("area:new words"), "F: a textarea's text was replaced, and its input event fired");

  const updir = `${fx.base}/up`;
  await Deno.mkdir(updir, { recursive: true });
  await Deno.writeTextFile(`${updir}/a.txt`, "hello");
  await Deno.writeFile(`${updir}/b.png`, new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]));
  const up = await mcp.tool("chrome_upload", { selector: "#f", paths: [`${updir}/a.txt`, `${updir}/b.png`] });
  ok(!up.isError, `F: chrome_upload into a file input — ${text(up).slice(0, 80)}`);
  ok(json(await mcp.tool("chrome_read")).text.includes("files:a.txt/5/text/plain,b.png/6/image/png"), "F: the page got both files, bytes and type intact");
  const picker = await mcp.tool("chrome_upload", { selector: "#pick", paths: [`${updir}/a.txt`] });
  ok(picker.isError && /cannot catch the file picker/.test(text(picker)), `F: a button that opens a picker is refused with the reason — ${text(picker).slice(0, 90)}`);

  const fref = Number(/^\[(\d+)\]/.exec(framed || "")?.[1]);
  await mcp.tool("chrome_click", { ref: fref });
  ok(json(await mcp.tool("chrome_read")).text.includes("framed"), "F: clicking by ref clicks inside the frame");
  const ffile = await mcp.tool("chrome_upload", { selector: "#ff", paths: [`${updir}/a.txt`] });
  ok(!ffile.isError, `F: the frame's file input is found by selector too — ${text(ffile).slice(0, 80)}`);
  const afterFrame = json(await mcp.tool("chrome_read")).text;
  ok(afterFrame.includes("framefile:true/a.txt"), `F: the framed input got a File of its own window — ${(/framefile:[^\s]*/.exec(afterFrame) || [""])[0]}`);

  // ---- console -------------------------------------------------------------------
  const first = json(await mcp.tool("chrome_console"));
  ok(/started just now/.test(first.note || ""), `G: the first chrome_console starts collection — ${first.note}`);
  await mcp.tool("chrome_click", { selector: "#b" });
  const logs = json(await mcp.tool("chrome_console"));
  ok((logs.messages || []).some((m) => m.includes("button clicked")), `G: and the page's log is collected after that — ${JSON.stringify(logs.messages)}`);

  // ---- screenshot ----------------------------------------------------------------
  const shot = await mcp.tool("chrome_screenshot");
  const img = (shot.content || []).find((c) => c.type === "image");
  ok(img && img.mimeType === "image/jpeg" && img.data.startsWith("/9j/"), `H: chrome_screenshot is a JPEG, through tabs.captureTab (${img ? img.data.length : 0} b64 chars)`);
  if (img) {
    // The corner button's middle, in the screenshot's own pixels.
    const bytes = Uint8Array.from(atob(img.data), (c) => c.charCodeAt(0));
    let w = 0, h = 0;
    for (let i = 2; i < bytes.length;) {
      const m = bytes[i + 1], len = (bytes[i + 2] << 8) | bytes[i + 3];
      if (m === 0xc0 || m === 0xc2) { h = (bytes[i + 5] << 8) | bytes[i + 6]; w = (bytes[i + 7] << 8) | bytes[i + 8]; break; }
      i += 2 + len;
    }
    const vp = await ff.page("({ w: innerWidth, h: innerHeight })"); // the roost tab's viewport; same window, same size
    const k = w / vp.w;
    await mcp.tool("chrome_click", { x: Math.round(w - 80 * k), y: Math.round(h - 40 * k) });
    ok(json(await mcp.tool("chrome_read")).text.includes("cornered"), `H: chrome_click at a screenshot point hits what is drawn there (${w}x${h})`);
  }

  // ---- what is never driven ------------------------------------------------------
  const roostTab = await mcp.tool("chrome_open", { url: `${roostOrigin}/${fx.project}` });
  ok(roostTab.isError && /roost tab/.test(text(roostTab)), `I: Claude may not drive roost itself — ${text(roostTab)}`);

  // ---- a call the page invents ---------------------------------------------------
  const forged = await ff.page(`new Promise((res) => {
    setTimeout(() => res({ ok: null, error: "no result in 10 s" }), 10000);
    addEventListener("message", function h(e) {
      if (e.data && e.data.roostBridge === 1 && e.data.from === "ext" && e.data.type === "result" && e.data.call === "forged-1") { removeEventListener("message", h); res(e.data); }
    });
    const payload = JSON.stringify({ call: "forged-1", project: ${JSON.stringify(fx.project)}, session: "term", pid: 1, tool: "chrome_tabs", args: {}, issued: Date.now() });
    window.postMessage({ roostBridge: 1, from: "page", type: "call", call: "forged-1", payload, sig: "0".repeat(64) }, location.origin);
  })`);
  ok(forged && forged.ok === false && /signature/.test(forged.error), `J: a call signed with the wrong key is refused, unsafe mode or not — ${forged && forged.error}`);

  // ---- closing what it opened ----------------------------------------------------
  const closed = await mcp.tool("chrome_close");
  ok(!closed.isError, `N: chrome_close closes the tab this session opened — ${text(closed).slice(0, 60)}`);
} catch (e) {
  ok(false, `threw: ${e.stack || e}`);
} finally {
  await mcp?.close();
  await ff?.close();
  await site.shutdown();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `${fail} FAILED` : "all ok");
Deno.exit(fail ? 1 : 0);
