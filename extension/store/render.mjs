//! Re-makes the Chrome Web Store pictures in this directory from the extension
//! as it is in this checkout.
//!
//! Every picture is a real page: the extension is loaded into Chromium, a
//! private roost is built and started (the browser suite's harness does both),
//! and the consent window, options page, toolbar popup and a handoff page are
//! captured, then set on store-sized canvases with a caption. Nothing is drawn
//! by hand, so a change to the extension's UI is one command away from a
//! listing that shows it.
//!
//! The one thing staged is the consent window's request: a real one needs a
//! Claude calling a tool, so the page's own `consent-info` question is answered
//! with the details a first `chrome_open` would carry.
//!
//! Output is JPEG, because the store refuses an alpha channel, at the exact
//! sizes it asks for.
//!
//! Run: deno run -A extension/store/render.mjs
import { attachTarget, fixture, freePort, openPage, startBrowser, startRoost, sleep, until }
  from "../../tests/browser/harness.mjs";

const here = new URL(".", import.meta.url).pathname;
const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
const ext = `${repoRoot}/extension`;

const fx = await fixture();
const PROJECT = "acme-shop";
await Deno.mkdir(`${fx.roots}/${PROJECT}`, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: `${fx.roots}/${PROJECT}` }).output();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const meta = JSON.parse(new TextDecoder().decode(
  (await new Deno.Command("cargo", { args: ["metadata", "--format-version", "1", "--no-deps"], cwd: repoRoot, stdout: "piped" }).output()).stdout,
));
const bin = `${meta.target_directory}/debug/roost`;
const raw = {};

const prof = `${Deno.env.get("HOME")}/roost-browser-tmp/store-profile`;
await Deno.remove(prof, { recursive: true }).catch(() => {});
const browser = await startBrowser(prof, [`--disable-extensions-except=${ext}`, `--load-extension=${ext}`]);
const list = async () => (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).json();

try {
  const findWorker = async () => (await list()).find((t) => t.type === "service_worker" && t.url.endsWith("/worker.js"));
  if (!await until(findWorker, 20, "the extension's worker")) throw new Error("the extension did not load");
  const sw = await findWorker();
  const id = new URL(sw.url).host;
  const worker = await attachTarget(sw.webSocketDebuggerUrl || `ws://127.0.0.1:${browser.port}/devtools/page/${sw.id}`);

  // What a paired extension in use holds.
  await worker.evalIn(`Promise.all([
    chrome.storage.local.set({ origins: ["https://roost.example.com"], key: "${"a1".repeat(32)}", unsafe: false,
                               deny: ["mybank.com", "mail.google.com"] }),
    chrome.storage.session.set({ grants: {
      a: { origin: "https://roost.example.com", project: "${PROJECT}", session: "term", pid: 48213, at: Date.now() },
      b: { origin: "https://roost.example.com", project: "${PROJECT}/.claude/worktrees/checkout-fix", session: "claude", pid: 51877, at: Date.now() },
    } }),
  ]).then(() => true)`);

  const capture = async (url, name, w, h, stub) => {
    const p = await openPage(browser.port, "about:blank");
    await p.cmd("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 2, mobile: false });
    await p.cmd("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "light" }] });
    // The stub only takes with the Page domain enabled; without it the
    // consent window renders "This request is no longer waiting".
    await p.cmd("Page.enable");
    if (stub) await p.cmd("Page.addScriptToEvaluateOnNewDocument", { source: stub });
    await p.cmd("Page.navigate", { url });
    await sleep(1500);
    const r = await p.cmd("Page.captureScreenshot", { format: "png" });
    raw[name] = `data:image/png;base64,${r.result.data}`;
    p.close();
  };

  await capture(`chrome-extension://${id}/consent.html?id=1`, "consent", 520, 590, `
    const realSend = chrome.runtime.sendMessage.bind(chrome.runtime);
    chrome.runtime.sendMessage = (m) => m && m.type === "consent-info"
      ? Promise.resolve({ origin: "https://roost.example.com", project: "${PROJECT}", session: "term", pid: 48213,
          tool: "chrome_open", args: { url: "https://staging.acme-shop.example/checkout" } })
      : realSend(m);`);
  if (raw.consent.length < 100_000) throw new Error("the consent window captured empty — the stub did not take");
  await capture(`chrome-extension://${id}/options.html`, "options", 760, 1010);
  await capture(`chrome-extension://${id}/popup.html`, "popup", 440, 320);

  // A short, real exchange on the handoff page: two messages typed into it,
  // one posted by `roost mcp` the way a roost Claude posts.
  const hp = `http://127.0.0.1:${roost.port}/frag/${PROJECT}/handoff`;
  {
    const p = await openPage(browser.port, hp);
    await until(() => p.evalIn("document.getElementById('status').textContent === 'connected'"), 10, "the handoff page");
    const say = (t) => p.evalIn(`document.getElementById('message').value = ${JSON.stringify(t)}; document.getElementById('send').requestSubmit(); true`);
    await say(`I found the bug report: on staging, the "Pay now" button does nothing when the cart has a gift card. Console shows TypeError in checkout.js. Can you fix it?`);
    await sleep(400);
    const mcp = new Deno.Command(bin, { args: ["mcp"], clearEnv: true, stdin: "piped", stdout: "null",
      env: { ROOST_STATE_DIR: fx.stateDir, ROOST_PROJECT: PROJECT, ROOST_SESSION: "term", PATH: "/usr/bin:/bin" } }).spawn();
    const w = mcp.stdin.getWriter();
    await w.write(new TextEncoder().encode(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "handoff_send",
      arguments: { text: "Fixed: applyGiftCard() read cart.total before it was computed. Patched in src/checkout.js and deployed to staging. Could you press Pay now again with the gift card cart and tell me what you see?" } } }) + "\n"));
    const posted = await until(() => p.evalIn("document.getElementById('log').textContent.includes('applyGiftCard')"), 10, "roost mcp's reply");
    await w.close().catch(() => {});
    try { mcp.kill(); } catch {}
    await mcp.status;
    if (!posted) throw new Error("roost mcp's handoff_send never reached the page");
    await say("Works now — the order confirmation page loads and the gift card balance is deducted.");
    await sleep(500);
    p.close();
  }
  await capture(hp, "handoff", 900, 620);

  // ---- canvases ------------------------------------------------------------
  const bytes = await Deno.readFile(`${ext}/icons/icon-128.png`);
  let bin64 = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin64 += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  const icon = `data:image/png;base64,${btoa(bin64)}`;
  const css = `
    * { box-sizing: border-box; margin: 0; }
    body { font-family: "DejaVu Sans", system-ui, sans-serif; color: #0d1b2a; overflow: hidden;
           background: linear-gradient(135deg, #eef4ff 0%, #dbe8ff 55%, #c9dcff 100%); }
    .brand { display: flex; align-items: center; gap: 12px; font-weight: 700; color: #1f4fbf; }
    .brand img { border-radius: 10px; }
    h1 { font-weight: 800; letter-spacing: -0.02em; line-height: 1.12; }
    p { color: #33415c; line-height: 1.45; }
    .frame { background: #fff; border-radius: 14px; overflow: hidden; border: 1px solid #c9d6ee;
             box-shadow: 0 24px 60px rgba(20,40,90,.28), 0 2px 6px rgba(20,40,90,.12); }
    .bar { height: 30px; background: #f1f4f9; border-bottom: 1px solid #dde3ee; display: flex; align-items: center; gap: 7px; padding: 0 12px; }
    .bar i { width: 11px; height: 11px; border-radius: 50%; background: #d4dae6; display: block; }
    .bar span { margin-left: 10px; font-size: 12px; color: #6b7a90; font-family: "DejaVu Sans Mono", monospace; }
    .frame img { display: block; }`;
  const shot = ({ title, body, img, bar, w }) => `<!doctype html><html><head><style>${css}
      .wrap { width: 1280px; height: 800px; display: flex; align-items: center; gap: 56px; padding: 0 72px; }
      .text { flex: 0 0 420px; display: flex; flex-direction: column; gap: 22px; }
      .brand { font-size: 22px; } h1 { font-size: 44px; } p { font-size: 20px; }
      .shot { flex: 1; display: flex; justify-content: center; }
    </style></head><body><div class="wrap">
      <div class="text"><div class="brand"><img src="${icon}" width="44" height="44">roost bridge</div>
        <h1>${title}</h1><p>${body}</p></div>
      <div class="shot"><div class="frame"><div class="bar"><i></i><i></i><i></i><span>${bar}</span></div>
        <img src="${img}" style="width:${w}px"></div></div>
    </div></body></html>`;

  const render = async (html, w, h, file) => {
    const tmp = `${fx.base}/${file}.html`;
    await Deno.writeTextFile(tmp, html);
    const p = await openPage(browser.port, `file://${tmp}`);
    await p.cmd("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 1, mobile: false });
    await sleep(700);
    const r = await p.cmd("Page.captureScreenshot", { format: "jpeg", quality: 92, clip: { x: 0, y: 0, width: w, height: h, scale: 1 } });
    await Deno.writeFile(`${here}${file}`, Uint8Array.from(atob(r.result.data), (c) => c.charCodeAt(0)));
    console.log(`  wrote extension/store/${file}`);
    p.close();
  };

  await render(shot({ title: "Claude asks before it touches your browser",
    body: "The first call from each Claude session opens this window: which roost, which project, which terminal, and what it wants to do. Deny is one click, and no answer is a deny.",
    img: raw.consent, bar: "roost bridge — permission", w: 470 }), 1280, 800, "screenshot-1-consent.jpg");
  await render(shot({ title: "Hand work between Claude in Chrome and Claude in roost",
    body: "Each roost project has a plain handoff page. You, or a browser agent such as Claude in Chrome, write there; the Claude in your roost terminal reads it, does the work on the server, and answers.",
    img: raw.handoff, bar: `roost.example.com/frag/${PROJECT}/handoff`, w: 640 }), 1280, 800, "screenshot-2-handoff.jpg");
  await render(shot({ title: "Paired with your own roost, on your terms",
    body: "Calls are accepted only from roost addresses you list and only when signed with your roost's own key. Sites on your deny-list are never driven, and unsafe mode is off unless you turn it on.",
    img: raw.options, bar: "roost bridge — options", w: 470 }), 1280, 800, "screenshot-3-options.jpg");
  await render(shot({ title: "See every allowed session, revoke any of them",
    body: "The toolbar button lists the Claude sessions you allowed. Grants end when that Claude exits or Chrome restarts, and Revoke ends one now.",
    img: raw.popup, bar: "roost bridge", w: 440 }), 1280, 800, "screenshot-4-sessions.jpg");

  await render(`<!doctype html><html><head><style>${css}
    .t { width: 440px; height: 280px; padding: 30px 32px; display: flex; flex-direction: column; justify-content: center; gap: 14px; }
    .brand { font-size: 30px; color: #0d1b2a; } h1 { font-size: 22px; font-weight: 700; color: #1f4fbf; } p { font-size: 15px; }
  </style></head><body><div class="t"><div class="brand"><img src="${icon}" width="64" height="64">roost bridge</div>
    <h1>Your Claude in roost, in your own Chrome</h1><p>With your consent, once per Claude session.</p></div></body></html>`,
    440, 280, "promo-small-440x280.jpg");

  await render(`<!doctype html><html><head><style>${css}
    .m { width: 1400px; height: 560px; display: flex; align-items: center; gap: 64px; padding: 0 90px; }
    .l { flex: 1; display: flex; flex-direction: column; gap: 22px; }
    .brand { font-size: 40px; color: #0d1b2a; } h1 { font-size: 46px; } p { font-size: 22px; max-width: 640px; }
    .r .frame img { width: 420px; }
  </style></head><body><div class="m">
    <div class="l"><div class="brand"><img src="${icon}" width="84" height="84">roost bridge</div>
      <h1>Let the Claude in your roost work in the browser you are logged into</h1>
      <p>Open pages, read them, click, type and take screenshots, only after you allow it.</p></div>
    <div class="r"><div class="frame"><div class="bar"><i></i><i></i><i></i><span>permission</span></div>
      <img src="${raw.consent}"></div></div>
  </div></body></html>`, 1400, 560, "promo-marquee-1400x560.jpg");
} finally {
  browser.close();
  await roost.close();
  await fx.cleanup();
}
Deno.exit(0);
