// The roost bridge's service worker: the policy point for everything a roost
// Claude asks this browser to do.
//
// A call arrives from a roost page's content script, but the page is only a
// relay — and possibly a compromised one, since any XSS in roost runs there.
// So nothing the page says is trusted: every call is HMAC-signed by the roost
// server with a key the user pasted into the options page (and the page never
// sees), and consent, grants and unsafe mode live here, in extension storage.
"use strict";

const CONSENT_TIMEOUT_MS = 120000;
const MAX_CALL_AGE_MS = 5 * 60 * 1000; // laptop and server clocks differ
const IDLE_DETACH_MS = 5 * 60 * 1000;
const MAX_SHOT_B64 = Math.floor(3.5 * 1024 * 1024 * 4 / 3); // 3.5 MB of JPEG
const SHOT_LONG_SIDE = 2000;
const LOAD_TIMEOUT_MS = 30000;
const MAX_CONSOLE = 300;
// Never driven, unsafe mode or not. A scheme list rather than an allow-list
// of http(s) for tabs a user already has open; chrome_open allows only
// http(s) on top of this.
const DENIED_SCHEMES = ["chrome:", "chrome-extension:", "devtools:", "view-source:", "file:", "chrome-untrusted:", "edge:"];
const DENIED_HOSTS = ["chromewebstore.google.com", "chrome.google.com"];

// ---- settings --------------------------------------------------------------

async function settings() {
  const s = await chrome.storage.local.get({ origins: [], key: "", unsafe: false, deny: [], follow: true });
  return s;
}

async function session(name, fallback) {
  const v = (await chrome.storage.session.get({ [name]: fallback }))[name];
  return v;
}
async function setSession(name, value) {
  await chrome.storage.session.set({ [name]: value });
}

async function paintBadge() {
  const { unsafe } = await settings();
  await chrome.action.setBadgeText({ text: unsafe ? "!!" : "" });
  if (unsafe) await chrome.action.setBadgeBackgroundColor({ color: "#d1242f" });
  await chrome.action.setTitle({ title: unsafe ? "roost bridge — UNSAFE MODE: every call is allowed without asking" : "roost bridge" });
}

/// Content scripts follow the origins list: registered for exactly those
/// origins, top frame only, and injected into roost tabs already open so a
/// new origin works without a reload.
let syncing = Promise.resolve();
function syncContentScripts() {
  // Serialised: a settings change and an explicit call arriving together
  // would otherwise both unregister and then both register, and the second
  // registration fails as a duplicate.
  syncing = syncing.then(doSyncContentScripts, doSyncContentScripts);
  return syncing;
}

async function doSyncContentScripts() {
  const { origins } = await settings();
  try { await chrome.scripting.unregisterContentScripts({ ids: ["roost"] }); } catch { /* none yet */ }
  if (!origins.length) return;
  const matches = origins.map((o) => `${o}/*`);
  await chrome.scripting.registerContentScripts([{
    id: "roost", matches, js: ["content.js"], runAt: "document_idle", allFrames: false, persistAcrossSessions: true,
  }]);
  for (const tab of await chrome.tabs.query({ url: matches })) {
    chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ["content.js"] }).catch(() => {});
  }
}

chrome.runtime.onInstalled.addListener(async () => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "send-page", title: "Send page to roost Claude", contexts: ["page", "link"] });
    chrome.contextMenus.create({ id: "send-selection", title: "Send selection to roost Claude", contexts: ["selection"] });
    chrome.contextMenus.create({ id: "send-shot", title: "Send screenshot to roost Claude", contexts: ["page"] });
  });
  await syncContentScripts();
  await paintBadge();
});
chrome.runtime.onStartup.addListener(paintBadge);
chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== "local") return;
  if (changes.origins) syncContentScripts();
  if (changes.unsafe) paintBadge();
});

// ---- signature ---------------------------------------------------------------

const hexToBytes = (h) => new Uint8Array((h.match(/../g) || []).map((b) => parseInt(b, 16)));

/// HMAC-SHA256 over the payload's exact bytes, keyed by the key's hex text
/// itself — the same key material `browser::sign` uses.
async function verify(key, payload, sig) {
  if (!/^[0-9a-f]{64}$/.test(sig || "")) return false;
  const k = await crypto.subtle.importKey("raw", new TextEncoder().encode(key), { name: "HMAC", hash: "SHA-256" }, false, ["verify"]);
  return crypto.subtle.verify("HMAC", k, hexToBytes(sig), new TextEncoder().encode(payload));
}

/// A signed payload is good once: the page could otherwise resend a call the
/// user allowed. Keyed by issue time and id, because a roost restart starts
/// call ids at 1 again.
async function firstSighting(p) {
  const now = Date.now();
  const seen = await session("seen", {});
  for (const k of Object.keys(seen)) if (now - seen[k] > 2 * MAX_CALL_AGE_MS) delete seen[k];
  const k = `${p.issued}:${p.call}`;
  if (seen[k]) return false;
  seen[k] = now;
  await setSession("seen", seen);
  return true;
}

// ---- consent -------------------------------------------------------------------

const consents = new Map(); // id -> { info, resolve, windowId, timer }
const promptsByGrant = new Map(); // grant key -> Promise<answer>
let nextConsent = 1;

const grantKey = (origin, p) => `${origin}|${p.project}|${p.session}|${p.pid}`;

async function ask(origin, p) {
  const key = grantKey(origin, p);
  // Calls that arrive while this grant's prompt is open wait for it.
  if (promptsByGrant.has(key)) return promptsByGrant.get(key);
  const answer = new Promise((resolve) => {
    const id = String(nextConsent++);
    const info = { origin, project: p.project, session: p.session, pid: p.pid, tool: p.tool, args: p.args };
    const settle = (a) => {
      const c = consents.get(id);
      if (!c) return;
      consents.delete(id);
      clearTimeout(c.timer);
      if (c.windowId) chrome.windows.remove(c.windowId).catch(() => {});
      resolve(a);
    };
    const timer = setTimeout(() => settle("deny"), CONSENT_TIMEOUT_MS);
    consents.set(id, { info, settle, timer, windowId: null });
    chrome.windows.create({ url: `consent.html?id=${id}`, type: "popup", width: 520, height: 560, focused: true })
      .then((w) => { const c = consents.get(id); if (c) c.windowId = w.id; else chrome.windows.remove(w.id).catch(() => {}); })
      .catch(() => settle("deny"));
  });
  promptsByGrant.set(key, answer);
  try { return await answer; } finally { promptsByGrant.delete(key); }
}

chrome.windows.onRemoved.addListener((wid) => {
  for (const [, c] of consents) if (c.windowId === wid) c.settle("deny");
});

async function allowed(origin, p) {
  const { unsafe } = await settings();
  if (unsafe) return true;
  const key = grantKey(origin, p);
  const grants = await session("grants", {});
  if (grants[key]) return true;
  const a = await ask(origin, p);
  if (a === "session") {
    const g = await session("grants", {});
    g[key] = { origin, project: p.project, session: p.session, pid: p.pid, at: Date.now() };
    await setSession("grants", g);
  }
  return a === "session" || a === "once";
}

// ---- deny-list -------------------------------------------------------------------

async function deniedReason(url) {
  let u;
  try { u = new URL(url); } catch { return null; } // about:blank and friends
  if (DENIED_SCHEMES.includes(u.protocol)) return `${u.protocol} pages are never driven`;
  if (DENIED_HOSTS.includes(u.hostname)) return `${u.hostname} is never driven`;
  const { deny, origins } = await settings();
  for (const d of deny) {
    const h = d.trim().toLowerCase();
    if (h && (u.hostname === h || u.hostname.endsWith(`.${h}`))) return `${u.hostname} is on your deny-list`;
  }
  if (origins.includes(u.origin)) return "that is a roost tab — Claude does not drive the page it is watched through";
  return null;
}

async function guard(tab) {
  const why = await deniedReason(tab.url || tab.pendingUrl || "");
  if (why) throw new Error(why);
}

// ---- tabs --------------------------------------------------------------------------

async function lastTabs() { return session("lastTab", {}); }
async function rememberTab(gk, tabId) {
  const m = await lastTabs();
  m[gk] = tabId;
  await setSession("lastTab", m);
}

/// Tabs a grant created with chrome_open, which are the only ones
/// chrome_close may close. A tab's id is the evidence, recorded at creation:
/// closing is destructive, and a user's own tab — even one Claude navigated
/// with new_tab:false — is not Claude's to close. Session storage is wiped
/// with the browser, the same lifetime as the tab ids it holds; when an
/// extension reload wipes it early, every close is refused, which is the
/// safe direction.
async function openedTabs() { return session("opened", {}); }
async function recordOpened(gk, tabId) {
  const m = await openedTabs();
  m[tabId] = gk;
  await setSession("opened", m);
}
chrome.tabs.onRemoved.addListener(async (tabId) => {
  const m = await openedTabs();
  if (!(tabId in m)) return;
  delete m[tabId];
  await setSession("opened", m);
});

async function resolveTab(args, gk) {
  if (args.tab_id !== undefined && args.tab_id !== null) {
    return chrome.tabs.get(Number(args.tab_id)).catch(() => { throw new Error(`no tab ${args.tab_id}`); });
  }
  const last = (await lastTabs())[gk];
  if (last !== undefined) {
    const t = await chrome.tabs.get(last).catch(() => null);
    if (t) return t;
  }
  const [t] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!t) throw new Error("no active tab — pass tab_id (see chrome_tabs)");
  const { origins } = await settings();
  try {
    if (origins.includes(new URL(t.url).origin)) {
      throw new Error("the active tab is roost itself — pass tab_id (see chrome_tabs) or open a page with chrome_open");
    }
  } catch (e) { if (e.message.startsWith("the active tab")) throw e; }
  return t;
}

async function waitLoaded(tabId) {
  const until = Date.now() + LOAD_TIMEOUT_MS;
  await sleep(150);
  for (;;) {
    const t = await chrome.tabs.get(tabId);
    if (t.status === "complete") return t;
    if (Date.now() > until) return t; // still loading is an answer, not an error
    await sleep(200);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// "roost - <repo> - <worktree>" for a project under `.claude/worktrees/`,
/// where both Claude Code and roost put them, else "roost - <project>". A
/// label only: a worktree living elsewhere is named like any project, and
/// nothing decides anything by it.
function groupTitle(project) {
  let p = project;
  try { p = decodeURIComponent(project); } catch { /* not encoded */ }
  const wt = /^(.*)\/\.claude\/worktrees\/([^/]+)$/.exec(p);
  return wt ? `roost - ${wt[1]} - ${wt[2].replace(/^worktree-/, "")}` : `roost - ${p}`;
}

/// A grant key is `origin|project|session|pid`; only the project may hold a
/// "|" of its own.
const grantProject = (gk) => gk.split("|").slice(1, -2).join("|");

/// Pages Claude opens go into one tab group per roost project, in the window
/// the user last used, so they can watch it work beside their own tabs. Per
/// project, not per Claude session, because the title names the project:
/// two sessions would otherwise make two groups the user cannot tell apart.
/// Which session may close a tab is `opened`'s business, not the group's.
/// With `follow` on (the default) the new tab comes to the front; off, it
/// opens behind and only a screenshot brings it forward, briefly.
async function claudeTab(url, gk) {
  const { follow } = await settings();
  const groups = await session("groups", {});
  const project = grantProject(gk);
  const gkey = `${gk.split("|")[0]}|${project}`;
  const group = groups[gkey] !== undefined ? await chrome.tabGroups.get(groups[gkey]).catch(() => null) : null;
  let windowId = group && group.windowId;
  if (!windowId) {
    const w = await chrome.windows.getLastFocused({ windowTypes: ["normal"] }).catch(() => null);
    windowId = w && !w.incognito ? w.id : null;
  }
  if (!windowId) {
    // No normal window to join (all closed, or only incognito, where this
    // extension does not run): start one.
    const w = await chrome.windows.create({ type: "normal", focused: follow });
    windowId = w.id;
  }
  const tab = await chrome.tabs.create({ windowId, url, active: follow });
  if (group) {
    await chrome.tabs.group({ groupId: group.id, tabIds: [tab.id] });
  } else {
    const id = await chrome.tabs.group({ tabIds: [tab.id], createProperties: { windowId } });
    await chrome.tabGroups.update(id, { title: groupTitle(project), color: "orange" });
    groups[gkey] = id;
    await setSession("groups", groups);
  }
  return tab;
}

// ---- the DevTools protocol -----------------------------------------------------------

const attached = new Map(); // tabId -> { console: [], timer }

async function attach(tabId) {
  let st = attached.get(tabId);
  if (!st) {
    try {
      await chrome.debugger.attach({ tabId }, "1.3");
    } catch (e) {
      // A worker restart forgets the map but not the attachment.
      if (!/already attached/i.test(String(e && e.message))) throw e;
    }
    st = { console: [], timer: null, fresh: true };
    attached.set(tabId, st);
    await cdp(tabId, "Runtime.enable").catch(() => {});
    await cdp(tabId, "Log.enable").catch(() => {});
  } else {
    st.fresh = false;
  }
  clearTimeout(st.timer);
  st.timer = setTimeout(() => detach(tabId), IDLE_DETACH_MS);
  return st;
}

function detach(tabId) {
  const st = attached.get(tabId);
  if (st) clearTimeout(st.timer);
  attached.delete(tabId);
  chrome.debugger.detach({ tabId }).catch(() => {});
}

const cdp = (tabId, method, params = {}) => chrome.debugger.sendCommand({ tabId }, method, params);

function pushConsole(tabId, line) {
  const st = attached.get(tabId);
  if (!st) return;
  st.console.push(line);
  if (st.console.length > MAX_CONSOLE) st.console.shift();
}

chrome.debugger.onEvent.addListener((src, method, params) => {
  if (method === "Runtime.consoleAPICalled") {
    const text = (params.args || []).map((a) => a.value !== undefined ? String(a.value) : (a.description || a.type)).join(" ");
    const f = params.stackTrace && params.stackTrace.callFrames[0];
    pushConsole(src.tabId, `[${params.type}] ${text}${f ? ` (${f.url}:${f.lineNumber + 1})` : ""}`);
  } else if (method === "Runtime.exceptionThrown") {
    const d = params.exceptionDetails || {};
    pushConsole(src.tabId, `[uncaught] ${(d.exception && d.exception.description) || d.text}${d.url ? ` (${d.url}:${d.lineNumber + 1})` : ""}`);
  } else if (method === "Log.entryAdded") {
    const e = params.entry;
    pushConsole(src.tabId, `[${e.level}] ${e.text}${e.url ? ` (${e.url})` : ""}`);
  }
});
// chrome_upload waits here for the page to open its file picker.
const choosers = new Map(); // tabId -> resolve

chrome.debugger.onEvent.addListener((src, method, params) => {
  if (method !== "Page.fileChooserOpened") return;
  const resolve = choosers.get(src.tabId);
  if (resolve) { choosers.delete(src.tabId); resolve(params); }
});

// The user pressed Cancel on Chrome's "is debugging this browser" bar, or the
// tab closed: forget it, and the next call attaches afresh.
chrome.debugger.onDetach.addListener((src) => {
  const st = attached.get(src.tabId);
  if (st) clearTimeout(st.timer);
  attached.delete(src.tabId);
});

async function evaluate(tabId, expression) {
  const r = await cdp(tabId, "Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error((r.exceptionDetails.exception && r.exceptionDetails.exception.description) || r.exceptionDetails.text);
  return r.result.value;
}

function withTimeout(p, ms, what) {
  return Promise.race([p, sleep(ms).then(() => { throw new Error(`${what} did not finish in ${ms / 1000} s`); })]);
}

// ---- tools -----------------------------------------------------------------------------

// The page's documents: the top one and every same-origin iframe's, nested,
// each with the offset that turns its client coordinates into the top
// page's. A cross-origin frame's document cannot be reached and is skipped.
const FRAMES = `const roostDocs = () => {
  const out = [];
  const walk = (doc, dx, dy) => {
    out.push({ doc, dx, dy });
    for (const f of doc.querySelectorAll('iframe,frame')) {
      let d = null;
      try { d = f.contentDocument; } catch {}
      if (!d || !d.documentElement) continue;
      const r = f.getBoundingClientRect();
      if (!r.width || !r.height) continue;
      walk(d, dx + r.left + f.clientLeft, dy + r.top + f.clientTop);
    }
  };
  walk(document, 0, 0);
  return out;
};
const roostFind = (ref, selector) => {
  for (const f of roostDocs()) {
    const el = ref !== null ? f.doc.querySelector('[data-roost-ref="' + ref + '"]') : f.doc.querySelector(selector);
    if (el) return { el, f };
  }
  return null;
};`;

const findArgs = (args) => args.ref !== undefined && args.ref !== null
  ? `${JSON.stringify(String(args.ref))}, null`
  : `null, ${JSON.stringify(String(args.selector || ""))}`;

const READ_SCRIPT = (maxChars) => `(() => {
  ${FRAMES}
  const sel = 'a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=option],[role=gridcell],[contenteditable=""],[contenteditable=true]';
  const docs = roostDocs();
  const out = [];
  let n = 0;
  let text = '';
  for (const { doc } of docs) {
    doc.querySelectorAll('[data-roost-ref]').forEach((e) => e.removeAttribute('data-roost-ref'));
    if (doc.body) text += (text ? '\\n' : '') + doc.body.innerText;
    for (const el of doc.querySelectorAll(sel)) {
      const r = el.getBoundingClientRect();
      const cs = getComputedStyle(el);
      if (!r.width || !r.height || cs.visibility === 'hidden' || cs.display === 'none') continue;
      if (++n > 400) break;
      el.setAttribute('data-roost-ref', String(n));
      const label = (el.getAttribute('aria-label') || el.innerText || el.placeholder || el.value || el.name || el.title || el.getAttribute('href') || '').trim().replace(/\\s+/g, ' ').slice(0, 80);
      const kind = el.tagName.toLowerCase() + (el.type && el.tagName === 'INPUT' ? ':' + el.type : '') + (doc === document ? '' : ' (in frame)');
      out.push('[' + n + '] ' + kind + ' "' + label + '"');
    }
  }
  return { title: document.title, url: location.href, text: text.slice(0, ${maxChars}), truncated: text.length > ${maxChars}, elements: out };
})()`;

const LOCATE_SCRIPT = (args) => `(() => {
  ${FRAMES}
  const hit = roostFind(${findArgs(args)});
  if (!hit) return null;
  const { el } = hit;
  el.scrollIntoView({ block: 'center', inline: 'center' });
  // Each enclosing frame too, outermost last, or the element can sit in
  // view inside a frame that is itself scrolled out of the page.
  for (let w = el.ownerDocument.defaultView; w.frameElement; w = w.parent) w.frameElement.scrollIntoView({ block: 'center', inline: 'center' });
  const r = el.getBoundingClientRect();
  let dx = 0, dy = 0;
  for (let w = el.ownerDocument.defaultView; w.frameElement; w = w.parent) {
    const fr = w.frameElement.getBoundingClientRect();
    dx += fr.left + w.frameElement.clientLeft;
    dy += fr.top + w.frameElement.clientTop;
  }
  return { x: dx + r.left + r.width / 2, y: dy + r.top + r.height / 2, what: el.tagName.toLowerCase() + ' "' + (el.innerText || el.value || el.getAttribute('aria-label') || '').trim().slice(0, 60) + '"' };
})()`;

// Select the focused field's whole content, so the text typed next replaces it.
const SELECT_ALL_SCRIPT = `(() => {
  let el = document.activeElement;
  // Focus inside a same-origin iframe shows here as the iframe itself.
  while (el && (el.tagName === 'IFRAME' || el.tagName === 'FRAME')) {
    let d = null;
    try { d = el.contentDocument; } catch {}
    if (!d) break;
    el = d.activeElement;
  }
  if (!el) return false;
  if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') { try { el.select(); } catch {} return true; }
  if (el.isContentEditable) {
    const range = el.ownerDocument.createRange();
    range.selectNodeContents(el);
    const sel = el.ownerDocument.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    return true;
  }
  return false;
})()`;

// Puts the files into the file input `this` the way a person picking them
// would leave it, then tells the page, which listens for change (React too).
const SET_FILES = `function (files) {
  const dt = new DataTransfer();
  for (const f of files) {
    const bin = atob(f.data);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    dt.items.add(new File([bytes], f.name, { type: f.mime }));
  }
  this.files = dt.files;
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return this.files.length;
}`;

// The located element itself when it is a file input, else null.
const FILE_INPUT_SCRIPT = (args) => `(() => {
  ${FRAMES}
  const hit = roostFind(${findArgs(args)});
  const el = hit && hit.el;
  return el && el.tagName === 'INPUT' && el.type === 'file' ? el : null;
})()`;

// The file input to fill: the target itself, or the one whose picker a
// click on the target opens. The picker is intercepted, so no dialog ever
// appears in front of the user.
async function fileInput(tabId, args, at) {
  const direct = await cdp(tabId, "Runtime.evaluate", { expression: FILE_INPUT_SCRIPT(args) });
  if (direct.result && direct.result.objectId) return { objectId: direct.result.objectId, multiple: null };
  await cdp(tabId, "Page.enable");
  await cdp(tabId, "Page.setInterceptFileChooserDialog", { enabled: true });
  try {
    const opened = new Promise((resolve) => choosers.set(tabId, resolve));
    await click(tabId, at.x, at.y);
    const ev = await withTimeout(opened, 5000, "a file picker opening").finally(() => choosers.delete(tabId));
    const { object } = await cdp(tabId, "DOM.resolveNode", { backendNodeId: ev.backendNodeId });
    return { objectId: object.objectId, multiple: ev.mode === "selectMultiple" };
  } catch (e) {
    throw new Error(/did not finish/.test(String(e && e.message))
      ? "clicking it opened no file picker — target the <input type=file> or the button that opens it"
      : e.message);
  } finally {
    await cdp(tabId, "Page.setInterceptFileChooserDialog", { enabled: false }).catch(() => {});
  }
}

async function locate(tabId, args) {
  if ((args.ref === undefined || args.ref === null) && !args.selector) throw new Error("pass ref (from chrome_read) or selector");
  const at = await evaluate(tabId, LOCATE_SCRIPT(args));
  if (!at) throw new Error(args.selector ? `nothing matches ${args.selector}` : `no element with ref ${args.ref} — call chrome_read again, refs change when the page does`);
  return at;
}

async function click(tabId, x, y) {
  for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
    await cdp(tabId, "Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: type === "mouseMoved" ? 0 : 1 });
  }
}

// chrome_screenshot shrinks the viewport by this factor (device pixels,
// capped to SHOT_LONG_SIDE); a point on the image divided by it is a CSS
// pixel in the viewport, which is what Input.dispatchMouseEvent takes.
async function shotScale(tabId) {
  const m = await cdp(tabId, "Page.getLayoutMetrics");
  const vp = m.cssVisualViewport || m.visualViewport;
  const dpr = (await evaluate(tabId, "devicePixelRatio")) || 1;
  return dpr * Math.min(1, SHOT_LONG_SIDE / (Math.max(vp.clientWidth, vp.clientHeight) * dpr));
}

async function screenshot(tab) {
  // A background tab does not paint; make it the visible one in its window,
  // and with follow off, hand the user's own tab back afterwards.
  let restore = null;
  if (!tab.active) {
    if (!(await settings()).follow) [restore] = await chrome.tabs.query({ active: true, windowId: tab.windowId });
    await chrome.tabs.update(tab.id, { active: true });
    await sleep(250);
  }
  try {
    return await capture(tab);
  } finally {
    if (restore) await chrome.tabs.update(restore.id, { active: true }).catch(() => {});
  }
}

async function capture(tab) {
  await attach(tab.id);
  await cdp(tab.id, "Emulation.setFocusEmulationEnabled", { enabled: true }).catch(() => {});
  const m = await cdp(tab.id, "Page.getLayoutMetrics");
  const vp = m.cssVisualViewport || m.visualViewport;
  const dpr = (await evaluate(tab.id, "devicePixelRatio")) || 1;
  const scale = Math.min(1, SHOT_LONG_SIDE / (Math.max(vp.clientWidth, vp.clientHeight) * dpr));
  const clip = { x: vp.pageX, y: vp.pageY, width: vp.clientWidth, height: vp.clientHeight, scale };
  for (const quality of [70, 50, 35]) {
    const r = await withTimeout(cdp(tab.id, "Page.captureScreenshot", { format: "jpeg", quality, clip, captureBeyondViewport: false }), 15000, "the screenshot");
    if (r.data.length <= MAX_SHOT_B64) return r.data;
  }
  throw new Error("the screenshot is too large even at low quality");
}

async function runTool(gk, tool, args) {
  switch (tool) {
    case "chrome_tabs": {
      const tabs = await chrome.tabs.query({});
      const out = [];
      for (const t of tabs) {
        const why = await deniedReason(t.url || "");
        out.push({ id: t.id, active: t.active, window: t.windowId, title: why ? "(hidden)" : t.title, url: why ? `(${why})` : t.url });
      }
      return { tabs: out };
    }
    case "chrome_open": {
      let u;
      try { u = new URL(String(args.url || "")); } catch { throw new Error(`not a URL: ${args.url}`); }
      if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error("chrome_open opens http and https URLs only");
      const why = await deniedReason(u.href);
      if (why) throw new Error(why);
      let tab;
      if (args.new_tab === false) {
        tab = await resolveTab(args, gk);
        await guard(tab);
        tab = await chrome.tabs.update(tab.id, { url: u.href });
      } else {
        tab = await claudeTab(u.href, gk);
        await recordOpened(gk, tab.id);
      }
      tab = await waitLoaded(tab.id);
      await rememberTab(gk, tab.id);
      // A redirect can land somewhere the first check never saw.
      const after = await deniedReason(tab.url || "");
      if (after) { await chrome.tabs.update(tab.id, { url: "about:blank" }).catch(() => {}); throw new Error(`it redirected: ${after}`); }
      return { tab_id: tab.id, url: tab.url, title: tab.title, loading: tab.status !== "complete" };
    }
    case "chrome_screenshot": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      const data = await screenshot(tab);
      return { image: { data, mimeType: "image/jpeg" }, tab_id: tab.id, url: tab.url, title: tab.title };
    }
    case "chrome_read": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      await attach(tab.id);
      const max = Math.max(1000, Math.min(Number(args.max_chars) || 20000, 100000));
      const r = await evaluate(tab.id, READ_SCRIPT(max));
      return { tab_id: tab.id, ...r };
    }
    case "chrome_click": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      await attach(tab.id);
      let at;
      if (Number.isFinite(args.x) && Number.isFinite(args.y)) {
        // A point on the last screenshot: for what refs cannot reach, such
        // as a cross-origin iframe or a canvas.
        const k = await shotScale(tab.id);
        at = { x: args.x / k, y: args.y / k, what: `point ${args.x},${args.y} on the screenshot` };
      } else {
        at = await locate(tab.id, args);
      }
      await click(tab.id, at.x, at.y);
      await sleep(300);
      const now = await chrome.tabs.get(tab.id);
      return { clicked: at.what, tab_id: tab.id, url: now.url };
    }
    case "chrome_type": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      await attach(tab.id);
      const at = await locate(tab.id, args);
      await click(tab.id, at.x, at.y);
      // Typing replaces what the field holds, the way a person selects all
      // first: insertText alone lands at the caret, so a textarea that
      // already has text (or an input that does not select on focus) would
      // end up with the old and the new run together.
      await evaluate(tab.id, SELECT_ALL_SCRIPT);
      await cdp(tab.id, "Input.insertText", { text: String(args.text ?? "") });
      if (args.submit) {
        const key = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 };
        await cdp(tab.id, "Input.dispatchKeyEvent", { type: "keyDown", text: "\r", ...key });
        await cdp(tab.id, "Input.dispatchKeyEvent", { type: "keyUp", ...key });
        await sleep(300);
      }
      return { typed_into: at.what, tab_id: tab.id, submitted: !!args.submit };
    }
    case "chrome_upload": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      await attach(tab.id);
      const files = Array.isArray(args.files) ? args.files : [];
      if (!files.length) throw new Error("no files to upload");
      const at = await locate(tab.id, args);
      const input = await fileInput(tab.id, args, at);
      if (input.multiple === false && files.length > 1) throw new Error(`that picker takes one file, not ${files.length}`);
      const r = await cdp(tab.id, "Runtime.callFunctionOn", {
        objectId: input.objectId, functionDeclaration: SET_FILES, arguments: [{ value: files }], returnByValue: true,
      });
      if (r.exceptionDetails) throw new Error((r.exceptionDetails.exception && r.exceptionDetails.exception.description) || r.exceptionDetails.text);
      await sleep(300);
      return { uploaded: files.map((f) => f.name), into: at.what, tab_id: tab.id };
    }
    case "chrome_console": {
      const tab = await resolveTab(args, gk);
      await guard(tab);
      await rememberTab(gk, tab.id);
      const st = await attach(tab.id);
      return {
        tab_id: tab.id,
        messages: st.console.slice(),
        note: st.fresh ? "collection started just now; reproduce the problem, then call chrome_console again" : undefined,
      };
    }
    case "chrome_close": {
      const tab = await resolveTab(args, gk);
      // Ownership, not the deny-list, decides here: a tab this session opened
      // that has since wandered onto a denied page is still its own to close.
      const owner = (await openedTabs())[tab.id];
      if (owner !== gk) {
        throw new Error(owner === undefined
          ? `tab ${tab.id} was not opened by this session with chrome_open — only those can be closed`
          : `tab ${tab.id} was opened by another Claude session — only that session can close it`);
      }
      await chrome.tabs.remove(tab.id);
      const last = await lastTabs();
      if (last[gk] === tab.id) { delete last[gk]; await setSession("lastTab", last); }
      return { closed: tab.id };
    }
    default:
      throw new Error(`unknown tool ${tool}`);
  }
}

// ---- a call, end to end ------------------------------------------------------------------

async function handleCall(msg, origin) {
  const { key } = await settings();
  if (!key) throw new Error("the roost extension has no key — run `roost browser-key` on the roost host and paste it into the extension's options");
  if (!(await verify(key, String(msg.payload || ""), String(msg.sig || "")))) {
    throw new Error("the call's signature does not match this extension's key — run `roost browser-key` and paste it into the extension's options again");
  }
  const p = JSON.parse(msg.payload);
  if (String(p.call) !== String(msg.call)) throw new Error("the call id does not match its signed payload");
  if (Math.abs(Date.now() - Number(p.issued)) > MAX_CALL_AGE_MS) throw new Error("the call is too old (or this computer's clock is far from the roost host's)");
  if (!(await firstSighting(p))) throw new Error("that call was already handled");
  if (!(await allowed(origin, p))) throw new Error("the user did not allow this in the browser");
  return runTool(grantKey(origin, p), p.tool, p.args || {});
}

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  if (!msg || sender.id !== chrome.runtime.id) return;
  if (msg.type === "consent-info" || msg.type === "consent-answer") {
    // Only our own consent page — page script in a roost tab cannot send
    // these, because it cannot reach chrome.runtime at all.
    if (!sender.url || !sender.url.startsWith(chrome.runtime.getURL("consent.html"))) return;
    const c = consents.get(String(msg.id));
    if (msg.type === "consent-info") { reply(c ? c.info : null); return; }
    if (c && ["session", "once", "deny"].includes(msg.answer)) c.settle(msg.answer);
    reply({ ok: true });
    return;
  }
  // Everything below comes from a content script, which must be in a top
  // frame of a configured roost origin. `sender.origin` is filled by Chrome,
  // never by the message.
  if (!sender.tab || sender.frameId !== 0) return;
  (async () => {
    const { origins } = await settings();
    if (!origins.includes(sender.origin)) return reply({ ok: false, error: `${sender.origin} is not a roost origin in the extension's options` });
    if (msg.type === "keepalive") return reply({ ok: true });
    if (msg.type === "roost-tab") { await setSession("roostTab", sender.tab.id); return reply({ ok: true }); }
    if (msg.type === "call") {
      try { reply({ ok: true, result: await handleCall(msg, sender.origin) }); } catch (e) { reply({ ok: false, error: String(e && e.message || e) }); }
      return;
    }
    reply({ ok: false, error: "unknown message" });
  })();
  return true; // the reply is async
});

// ---- your browser → roost Claude -------------------------------------------------------------

async function flash(text) {
  await chrome.action.setBadgeText({ text });
  await chrome.action.setBadgeBackgroundColor({ color: "#9a6700" });
  setTimeout(paintBadge, 4000);
}

/// Hands `msg` ({text, image?}) to the roost tab focused last, whose page
/// pastes it into its Claude terminal. True when that page took it.
async function sendToRoost(msg) {
  const roostTab = await session("roostTab", null);
  const target = roostTab !== null ? await chrome.tabs.get(roostTab).catch(() => null) : null;
  if (!target) return false;
  const r = await chrome.tabs.sendMessage(target.id, { type: "send", ...msg }).catch(() => null);
  return !!(r && r.ok);
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const page = `${tab.title || ""}\n${info.linkUrl || tab.url}`;
  let msg;
  if (info.menuItemId === "send-selection") {
    msg = { text: `From my browser (${tab.url}):\n> ${String(info.selectionText || "").replace(/\n/g, "\n> ")}` };
  } else if (info.menuItemId === "send-shot") {
    const image = await chrome.tabs.captureVisibleTab(tab.windowId, { format: "png" }).catch(() => null);
    if (!image) return flash("no");
    msg = { text: `Screenshot from my browser: ${page}`, image };
  } else {
    msg = { text: `From my browser: ${page}` };
  }
  flash((await sendToRoost(msg)) ? "ok" : "no");
});

// Test hook: the browser suite drives the worker through the DevTools
// protocol and needs to reach these without a UI.
self.roostBridge = { settings, syncContentScripts, verify, deniedReason, sendToRoost, groupTitle };
