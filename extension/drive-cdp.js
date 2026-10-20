// Chrome's backend: drives a tab through chrome.debugger (the DevTools
// protocol), which is the one Chrome API that screenshots, reads, clicks and
// types in a background tab and produces *trusted* input. Firefox has no
// debugger API at all (bug 1316741), which is why drive-page.js exists; the
// worker picks whichever this browser can run, and everything above the
// backend — signature, consent, grants, deny-list — is the same for both.
"use strict";

const IDLE_DETACH_MS = 5 * 60 * 1000;
const MAX_CONSOLE = 300;

const attached = new Map(); // tabId -> { console: [], timer, fresh }
// chrome_upload waits here for the page to open its file picker.
const choosers = new Map(); // tabId -> resolve

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

// Registered only where the API exists: this file is loaded in Firefox too,
// as one of the background scripts, and must cost nothing there.
if (chrome.debugger) {
  chrome.debugger.onEvent.addListener((src, method, params) => {
    if (method === "Runtime.consoleAPICalled") {
      const text = (params.args || []).map((v) => v.value !== undefined ? String(v.value) : (v.description || v.type)).join(" ");
      const f = params.stackTrace && params.stackTrace.callFrames[0];
      pushConsole(src.tabId, `[${params.type}] ${text}${f ? ` (${f.url}:${f.lineNumber + 1})` : ""}`);
    } else if (method === "Runtime.exceptionThrown") {
      const d = params.exceptionDetails || {};
      pushConsole(src.tabId, `[uncaught] ${(d.exception && d.exception.description) || d.text}${d.url ? ` (${d.url}:${d.lineNumber + 1})` : ""}`);
    } else if (method === "Log.entryAdded") {
      const e = params.entry;
      pushConsole(src.tabId, `[${e.level}] ${e.text}${e.url ? ` (${e.url})` : ""}`);
    } else if (method === "Page.fileChooserOpened") {
      const resolve = choosers.get(src.tabId);
      if (resolve) { choosers.delete(src.tabId); resolve(params); }
    }
  });
  // The user pressed Cancel on Chrome's "is debugging this browser" bar, or
  // the tab closed: forget it, and the next call attaches afresh.
  chrome.debugger.onDetach.addListener((src) => {
    const st = attached.get(src.tabId);
    if (st) clearTimeout(st.timer);
    attached.delete(src.tabId);
  });
}

/// The first line of a page exception, without the "Error: " a thrown Error
/// carries — what roostPage says is the message, not the stack around it.
function pageError(details) {
  const d = (details.exception && details.exception.description) || details.text || "the page threw";
  return new Error(String(d).split("\n")[0].replace(/^(Uncaught )?Error: /, ""));
}

/// roostPage(op, a) in the tab, by value.
async function pageCall(tabId, op, a = {}, byValue = true) {
  const expression = `(${roostPage.toString()})(${JSON.stringify(op)}, ${JSON.stringify(a)})`;
  const r = await cdp(tabId, "Runtime.evaluate", { expression, returnByValue: byValue, awaitPromise: true });
  if (r.exceptionDetails) throw pageError(r.exceptionDetails);
  return byValue ? r.result.value : r.result;
}

async function trustedClick(tabId, x, y) {
  for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
    await cdp(tabId, "Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: type === "mouseMoved" ? 0 : 1 });
  }
}

// Puts the files into the file input `this` the way a person picking them
// would leave it, then tells the page, which listens for change (React too).
// The input may sit in a same-origin iframe while this runs in the top
// frame, so File, DataTransfer and Event come from the input's own window:
// objects from the top frame's realm fail the iframe's instanceof checks
// (Wix's uploader then reads no URI off the file).
const SET_FILES = `function (files) {
  const w = this.ownerDocument.defaultView || window;
  const dt = new w.DataTransfer();
  for (const f of files) {
    const bin = atob(f.data);
    const bytes = new w.Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    dt.items.add(new w.File([bytes], f.name, { type: f.mime }));
  }
  this.files = dt.files;
  this.dispatchEvent(new w.Event('input', { bubbles: true }));
  this.dispatchEvent(new w.Event('change', { bubbles: true }));
  return this.files.length;
}`;

// The file input to fill: the target itself, or the one whose picker a
// click on the target opens. The picker is intercepted, so no dialog ever
// appears in front of the user.
async function fileInput(tabId, args, at) {
  const direct = await pageCall(tabId, "fileInput", { ref: args.ref, selector: args.selector }, false);
  if (direct && direct.objectId) return { objectId: direct.objectId, multiple: null };
  await cdp(tabId, "Page.enable");
  await cdp(tabId, "Page.setInterceptFileChooserDialog", { enabled: true });
  try {
    const opened = new Promise((resolve) => choosers.set(tabId, resolve));
    await trustedClick(tabId, at.x, at.y);
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

// chrome_screenshot shrinks the viewport by this factor (device pixels,
// capped to SHOT_LONG_SIDE); a point on the image divided by it is a CSS
// pixel in the viewport, which is what Input.dispatchMouseEvent takes.
async function shotScale(tabId) {
  const m = await cdp(tabId, "Page.getLayoutMetrics");
  const vp = m.cssVisualViewport || m.visualViewport;
  const dpr = (await pageCall(tabId, "viewport")).dpr;
  return dpr * Math.min(1, SHOT_LONG_SIDE / (Math.max(vp.clientWidth, vp.clientHeight) * dpr));
}

async function capture(tab) {
  await attach(tab.id);
  await cdp(tab.id, "Emulation.setFocusEmulationEnabled", { enabled: true }).catch(() => {});
  const m = await cdp(tab.id, "Page.getLayoutMetrics");
  const vp = m.cssVisualViewport || m.visualViewport;
  const dpr = (await pageCall(tab.id, "viewport")).dpr;
  const scale = Math.min(1, SHOT_LONG_SIDE / (Math.max(vp.clientWidth, vp.clientHeight) * dpr));
  const clip = { x: vp.pageX, y: vp.pageY, width: vp.clientWidth, height: vp.clientHeight, scale };
  for (const quality of [70, 50, 35]) {
    const r = await withTimeout(cdp(tab.id, "Page.captureScreenshot", { format: "jpeg", quality, clip, captureBeyondViewport: false }), 15000, "the screenshot");
    if (r.data.length <= MAX_SHOT_B64) return r.data;
  }
  throw new Error("the screenshot is too large even at low quality");
}

// eslint-disable-next-line no-unused-vars
const cdpDrive = {
  name: "cdp",

  async read(tab, max) {
    await attach(tab.id);
    return pageCall(tab.id, "read", { max });
  },

  async click(tab, args) {
    await attach(tab.id);
    let at;
    if (Number.isFinite(args.x) && Number.isFinite(args.y)) {
      // A point on the last screenshot: for what refs cannot reach, such as
      // a cross-origin iframe or a canvas.
      const k = await shotScale(tab.id);
      at = { x: args.x / k, y: args.y / k, what: `point ${args.x},${args.y} on the screenshot` };
    } else {
      at = await pageCall(tab.id, "locate", { ref: args.ref, selector: args.selector });
    }
    await trustedClick(tab.id, at.x, at.y);
    return { what: at.what };
  },

  async type(tab, args) {
    await attach(tab.id);
    const at = await pageCall(tab.id, "locate", { ref: args.ref, selector: args.selector });
    await trustedClick(tab.id, at.x, at.y);
    // Typing replaces what the field holds, the way a person selects all
    // first: insertText alone lands at the caret, so a textarea that already
    // has text (or an input that does not select on focus) would end up with
    // the old and the new run together.
    await pageCall(tab.id, "selectAll");
    await cdp(tab.id, "Input.insertText", { text: String(args.text ?? "") });
    if (args.submit) {
      const key = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 };
      await cdp(tab.id, "Input.dispatchKeyEvent", { type: "keyDown", text: "\r", ...key });
      await cdp(tab.id, "Input.dispatchKeyEvent", { type: "keyUp", ...key });
    }
    return { what: at.what };
  },

  async upload(tab, args, files) {
    await attach(tab.id);
    const at = await pageCall(tab.id, "locate", { ref: args.ref, selector: args.selector });
    const input = await fileInput(tab.id, args, at);
    if (input.multiple === false && files.length > 1) throw new Error(`that picker takes one file, not ${files.length}`);
    const r = await cdp(tab.id, "Runtime.callFunctionOn", {
      objectId: input.objectId, functionDeclaration: SET_FILES, arguments: [{ value: files }], returnByValue: true,
    });
    if (r.exceptionDetails) throw pageError(r.exceptionDetails);
    return { what: at.what };
  },

  async screenshot(tab) {
    // A background tab does not paint; make it the visible one in its
    // window, and with follow off, hand the user's own tab back afterwards.
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
  },

  async console(tab) {
    const st = await attach(tab.id);
    return { messages: st.console.slice(), fresh: st.fresh };
  },
};
