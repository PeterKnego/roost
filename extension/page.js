// Everything roost bridge does *inside* a page, as one self-contained
// function. Both backends run it: Chrome's (drive-cdp.js) sends its source
// through the DevTools protocol, Firefox's (drive-page.js) hands it to
// scripting.executeScript, which takes a function and never a string. One
// function rather than many, because each is serialised on its own: nothing
// outside its body exists where it runs, so the helpers must live inside.
"use strict";

// eslint-disable-next-line no-unused-vars
function roostPage(op, a) {
  // The page's documents: the top one and every same-origin iframe's, nested,
  // each with the offset that turns its client coordinates into the top
  // page's. A cross-origin frame's document cannot be reached and is skipped.
  const docs = () => {
    const out = [];
    const walk = (doc, dx, dy) => {
      out.push({ doc, dx, dy });
      for (const f of doc.querySelectorAll("iframe,frame")) {
        let d = null;
        try { d = f.contentDocument; } catch { /* cross-origin */ }
        if (!d || !d.documentElement) continue;
        const r = f.getBoundingClientRect();
        if (!r.width || !r.height) continue;
        walk(d, dx + r.left + f.clientLeft, dy + r.top + f.clientTop);
      }
    };
    walk(document, 0, 0);
    return out;
  };
  const find = () => {
    const ref = a.ref !== undefined && a.ref !== null ? String(a.ref) : null;
    if (ref === null && !a.selector) throw new Error("pass ref (from chrome_read) or selector");
    for (const f of docs()) {
      const el = ref !== null ? f.doc.querySelector(`[data-roost-ref="${CSS.escape(ref)}"]`) : f.doc.querySelector(String(a.selector));
      if (el) return el;
    }
    throw new Error(a.selector ? `nothing matches ${a.selector}` : `no element with ref ${a.ref} — call chrome_read again, refs change when the page does`);
  };
  const describe = (el) => el.tagName.toLowerCase() + ' "' + (el.innerText || el.value || el.getAttribute("aria-label") || "").trim().slice(0, 60) + '"';
  // Scrolls `el` into view, enclosing frames too (outermost last, or it can
  // sit in view inside a frame scrolled out of the page), and returns its
  // centre in the top page's coordinates.
  const centre = (el) => {
    el.scrollIntoView({ block: "center", inline: "center" });
    for (let w = el.ownerDocument.defaultView; w.frameElement; w = w.parent) w.frameElement.scrollIntoView({ block: "center", inline: "center" });
    const r = el.getBoundingClientRect();
    let dx = 0, dy = 0;
    for (let w = el.ownerDocument.defaultView; w.frameElement; w = w.parent) {
      const fr = w.frameElement.getBoundingClientRect();
      dx += fr.left + w.frameElement.clientLeft;
      dy += fr.top + w.frameElement.clientTop;
    }
    return { x: dx + r.left + r.width / 2, y: dy + r.top + r.height / 2, local: { x: r.left + r.width / 2, y: r.top + r.height / 2 } };
  };
  // The element at a top-page point, descending into same-origin frames,
  // with the point in that element's own document.
  const atPoint = (x, y) => {
    let doc = document, lx = x, ly = y, el = null;
    for (;;) {
      el = doc.elementFromPoint(lx, ly);
      if (!el || (el.tagName !== "IFRAME" && el.tagName !== "FRAME")) break;
      let inner = null;
      try { inner = el.contentDocument; } catch { /* cross-origin */ }
      if (!inner) break;
      const r = el.getBoundingClientRect();
      lx -= r.left + el.clientLeft;
      ly -= r.top + el.clientTop;
      doc = inner;
    }
    return { el, x: lx, y: ly };
  };
  // A pointer click as dispatched events, built in the element's own window
  // so a framework in an iframe sees its own event classes. Untrusted
  // (isTrusted false): Firefox gives extensions no way to make trusted input.
  const press = (el, x, y) => {
    const w = el.ownerDocument.defaultView;
    const base = { bubbles: true, cancelable: true, composed: true, view: w, clientX: x, clientY: y, button: 0 };
    el.dispatchEvent(new w.PointerEvent("pointerover", { ...base, pointerType: "mouse" }));
    el.dispatchEvent(new w.MouseEvent("mouseover", base));
    el.dispatchEvent(new w.PointerEvent("pointerdown", { ...base, pointerType: "mouse", buttons: 1 }));
    el.dispatchEvent(new w.MouseEvent("mousedown", { ...base, buttons: 1 }));
    if (typeof el.focus === "function") el.focus({ preventScroll: true });
    el.dispatchEvent(new w.PointerEvent("pointerup", { ...base, pointerType: "mouse" }));
    el.dispatchEvent(new w.MouseEvent("mouseup", base));
    el.dispatchEvent(new w.MouseEvent("click", { ...base, detail: 1 }));
  };
  const focused = () => {
    let el = document.activeElement;
    // Focus inside a same-origin iframe shows here as the iframe itself.
    while (el && (el.tagName === "IFRAME" || el.tagName === "FRAME")) {
      let d = null;
      try { d = el.contentDocument; } catch { /* cross-origin */ }
      if (!d) break;
      el = d.activeElement;
    }
    return el;
  };
  const selectAll = (el) => {
    if (!el) return false;
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") { try { el.select(); } catch { /* not selectable */ } return true; }
    if (el.isContentEditable) {
      const range = el.ownerDocument.createRange();
      range.selectNodeContents(el);
      const sel = el.ownerDocument.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
      return true;
    }
    return false;
  };

  switch (op) {
    case "read": {
      const max = a.max;
      const sel = "a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=option],[role=gridcell],[contenteditable=\"\"],[contenteditable=true]";
      const out = [];
      let n = 0;
      let text = "";
      for (const { doc } of docs()) {
        doc.querySelectorAll("[data-roost-ref]").forEach((e) => e.removeAttribute("data-roost-ref"));
        if (doc.body) text += (text ? "\n" : "") + doc.body.innerText;
        for (const el of doc.querySelectorAll(sel)) {
          const r = el.getBoundingClientRect();
          const cs = el.ownerDocument.defaultView.getComputedStyle(el);
          if (!r.width || !r.height || cs.visibility === "hidden" || cs.display === "none") continue;
          if (++n > 400) break;
          el.setAttribute("data-roost-ref", String(n));
          const label = (el.getAttribute("aria-label") || el.innerText || el.placeholder || el.value || el.name || el.title || el.getAttribute("href") || "").trim().replace(/\s+/g, " ").slice(0, 80);
          const kind = el.tagName.toLowerCase() + (el.type && el.tagName === "INPUT" ? ":" + el.type : "") + (doc === document ? "" : " (in frame)");
          out.push("[" + n + "] " + kind + ' "' + label + '"');
        }
      }
      return { title: document.title, url: location.href, text: text.slice(0, max), truncated: text.length > max, elements: out };
    }
    // Chrome: where to send a trusted click.
    case "locate": {
      const el = find();
      const c = centre(el);
      return { x: c.x, y: c.y, what: describe(el) };
    }
    // Chrome: select the focused field's content, so the text inserted next
    // replaces it.
    case "selectAll":
      return selectAll(focused());
    // Chrome: the target itself when it is a file input, else null. Run with
    // returnByValue off, so the element comes back as a remote object.
    case "fileInput": {
      const el = find();
      return el.tagName === "INPUT" && el.type === "file" ? el : null;
    }
    case "viewport":
      return { w: innerWidth, h: innerHeight, dpr: devicePixelRatio || 1 };
    // Firefox: a click by ref, selector, or a top-page point.
    case "click": {
      if (Number.isFinite(a.x) && Number.isFinite(a.y)) {
        const hit = atPoint(a.x, a.y);
        if (!hit.el) throw new Error(`nothing at ${Math.round(a.x)},${Math.round(a.y)} in the page`);
        press(hit.el, hit.x, hit.y);
        return { what: describe(hit.el) };
      }
      const el = find();
      const c = centre(el);
      press(el, c.local.x, c.local.y);
      return { what: describe(el) };
    }
    // Firefox: replace what the field holds, the way selecting all and typing
    // would, so frameworks that track their own copy of the value see it.
    case "type": {
      const el = find();
      const c = centre(el);
      press(el, c.local.x, c.local.y);
      const w = el.ownerDocument.defaultView;
      const text = String(a.text ?? "");
      if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
        // The prototype's setter, not `el.value =`: React and others shadow
        // the instance's value to track changes, and only a value set
        // underneath them reads as a change when `input` fires.
        const proto = el.tagName === "INPUT" ? w.HTMLInputElement.prototype : w.HTMLTextAreaElement.prototype;
        Object.getOwnPropertyDescriptor(proto, "value").set.call(el, text);
        el.dispatchEvent(new w.InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
        el.dispatchEvent(new w.Event("change", { bubbles: true }));
      } else if (el.isContentEditable) {
        selectAll(el);
        if (!el.ownerDocument.execCommand("insertText", false, text)) {
          el.textContent = text;
          el.dispatchEvent(new w.InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
        }
      } else {
        throw new Error(`${describe(el)} is not a text field`);
      }
      if (a.submit) {
        const k = { key: "Enter", code: "Enter", keyCode: 13, which: 13, bubbles: true, cancelable: true };
        const go = el.dispatchEvent(new w.KeyboardEvent("keydown", k));
        el.dispatchEvent(new w.KeyboardEvent("keypress", k));
        el.dispatchEvent(new w.KeyboardEvent("keyup", k));
        // A real Enter submits the field's form; a dispatched one does not,
        // unless the page handled keydown itself and said so.
        if (go && el.form && el.tagName === "INPUT") el.form.requestSubmit();
      }
      return { what: describe(el) };
    }
    // Firefox: files into an <input type=file>, the target or one it plainly
    // owns. Run in the page's own world, and built from the input's own
    // window either way, so File and DataTransfer are the page's: an
    // uploader in an iframe checks them with its own instanceof.
    case "upload": {
      let el = find();
      if (!(el.tagName === "INPUT" && el.type === "file")) {
        const own = el.querySelector && el.querySelector("input[type=file]");
        const forLabel = el.tagName === "LABEL" ? el.control : el.closest && el.closest("label") && el.closest("label").control;
        el = own || (forLabel && forLabel.type === "file" ? forLabel : null);
        if (!el) throw new Error("Firefox cannot catch the file picker a button opens — point chrome_upload at the <input type=file> itself (chrome_read lists it, often hidden: try a selector such as input[type=file])");
      }
      if (!el.multiple && a.files.length > 1) throw new Error(`that input takes one file, not ${a.files.length}`);
      const w = el.ownerDocument.defaultView;
      const dt = new w.DataTransfer();
      for (const f of a.files) {
        const bin = w.atob(f.data);
        const bytes = new w.Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        dt.items.add(new w.File([bytes], f.name, { type: f.mime }));
      }
      el.files = dt.files;
      el.dispatchEvent(new w.Event("input", { bubbles: true }));
      el.dispatchEvent(new w.Event("change", { bubbles: true }));
      return { what: describe(el) };
    }
    // Firefox: console capture, in the page's own world so it wraps the
    // page's console. Installed once per document; `fresh` says it was just
    // now, so nothing before this moment is in the buffer.
    case "consoleHook": {
      if (window.__roostConsole) return { fresh: false };
      const buf = [];
      Object.defineProperty(window, "__roostConsole", { value: buf });
      const push = (line) => { buf.push(line); if (buf.length > 300) buf.shift(); };
      const show = (v) => { try { return typeof v === "string" ? v : v instanceof Error ? (v.stack || String(v)) : JSON.stringify(v); } catch { return String(v); } };
      for (const level of ["log", "info", "warn", "error", "debug"]) {
        const orig = console[level];
        console[level] = function (...args) {
          push(`[${level}] ${args.map(show).join(" ")}`);
          return orig.apply(this, args);
        };
      }
      addEventListener("error", (e) => push(`[uncaught] ${e.error ? show(e.error) : e.message}${e.filename ? ` (${e.filename}:${e.lineno})` : ""}`));
      addEventListener("unhandledrejection", (e) => push(`[unhandled rejection] ${show(e.reason)}`));
      return { fresh: true };
    }
    case "consoleRead":
      return { messages: window.__roostConsole ? window.__roostConsole.slice() : null };
    default:
      throw new Error(`unknown page operation ${op}`);
  }
}
