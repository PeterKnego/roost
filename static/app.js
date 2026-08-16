// Workspace client. Chrome (tabstrips, tree/changes/file/diff panes) is
// rendered from mirrored server state on every event; terminals are pooled
// DOM nodes that are MOVED between panes with appendChild, never rebuilt —
// rebuilding a xterm instance drops its websocket and detaches the shell.
const PROJECT = document.body.dataset.project;
const SESSION_RE = /^[A-Za-z0-9_-]{1,32}$/; // must match session::valid_name server-side

const wsUrl = (p) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${p}`;

let state = null;
let myOrigin = null;
let ctrl = null;
const terms = new Map();   // session -> {node, term, fit, sock, stale}
const editors = new Map(); // rel -> textarea (the currently mounted one, if any)
const texts = new Map();   // rel -> latest known buffer text (server-authoritative)

function send(intent) {
  if (ctrl && ctrl.readyState === 1) ctrl.send(JSON.stringify(intent));
}

function connectControl() {
  myOrigin = null; // a reconnect must not keep a stale id from the last socket
  ctrl = new WebSocket(wsUrl(`/ws/${PROJECT}/_workspace`));
  ctrl.onmessage = (e) => onEvent(JSON.parse(e.data));
  ctrl.onclose = () => setTimeout(connectControl, 1000);
}

function onEvent(ev) {
  switch (ev.t) {
    case "State":
      myOrigin = myOrigin || ev.origin;
      state = ev.ws;
      render();
      break;
    case "BufferText": {
      // Skip our own text or the cursor jumps; empty origin = external change
      // (a background save, SetMode's initial disk read, or Claude editing
      // the file directly). texts is updated unconditionally (not gated on
      // an editor being mounted right now) so mountEditor can always seed
      // from it, even for text that arrived before its tab was ever opened.
      if (ev.origin && ev.origin === myOrigin) break;
      texts.set(ev.rel, ev.text);
      const ta = editors.get(ev.rel);
      if (ta && ta.value !== ev.text) ta.value = ev.text;
      break;
    }
    case "BufferStale": {
      // The server only pushes this flag standalone (a dirty buffer whose
      // file changed underneath it) — patch it locally rather than wait
      // for an unrelated event to bring a fresh State snapshot.
      const b = state && state.buffers.find((x) => x.rel === ev.rel);
      if (b) { b.stale = true; render(); }
      break;
    }
    case "TreeChanged": refreshTree(); break;
    case "StatusChanged": refreshKind("Changes"); break;
    case "FileChanged": refreshKind("Diff"); break;
    case "SaveConflict": showConflict(ev); break;
    case "Error":
      // Every server-side failure funnels through here (already-exists,
      // directory-not-empty, path-outside-project, too-many-buffers,
      // no-buffer-for-X, save I/O errors, malformed intents...) — without a
      // visible banner, e.g. deleting a non-empty directory looks like a
      // silent no-op. console.warn stays too, for anyone actually watching devtools.
      console.warn("deadlight:", ev.msg);
      showError(ev.msg);
      break;
  }
}

function tabKey(t) {
  switch (t.k) {
    // Mode is part of the key: toggling Preview<->Edit must force a remount
    // (fetched HTML vs. a live textarea), not be treated as "unchanged".
    case "File": return `File:${t.rel}:${t.mode}`;
    case "Diff": return `Diff:${t.rel || ""}`;
    case "Terminal": return `Terminal:${t.session}`;
    default: return t.k;
  }
}

function tabLabel(t) {
  switch (t.k) {
    case "Tree": return "Files";
    case "Changes": return "Changes";
    case "File": return t.rel.split("/").pop();
    case "Diff": return t.rel ? `± ${t.rel.split("/").pop()}` : "± full diff";
    case "Terminal": return t.session;
  }
}

function render() {
  if (!state) return;
  const header = document.querySelector("header");
  if (header) document.documentElement.style.setProperty("--header-h", header.offsetHeight + "px");
  document.documentElement.style.setProperty("--left-w", state.sizes.left_w + "px");
  document.documentElement.style.setProperty("--right-w", state.sizes.right_w + "px");
  document.documentElement.style.setProperty("--left-split", state.sizes.left_split + "%");

  const liveSessions = new Set();
  state.panes.forEach((pane, pi) => {
    const el = document.querySelector(`.pane[data-pane="${pi}"]`);
    const strip = el.querySelector(".tabstrip");
    const content = el.querySelector(".content");
    strip.innerHTML = ""; // cheap and holds no focus — always safe to rebuild
    pane.tabs.forEach((t, ti) => {
      if (t.k === "Terminal") liveSessions.add(t.session);
      const b = document.createElement("span");
      b.className = "tab" + (ti === pane.active ? " active" : "");
      const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
      b.innerHTML =
        (meta && meta.dirty ? '<span class="dirty">●</span> ' : "") +
        (meta && meta.stale ? '<span class="stale">⚠</span> ' : "") +
        escapeHtml(tabLabel(t));
      b.onclick = () => send({ t: "ActivateTab", pane: pi, idx: ti });
      if (t.k === "File") {
        const e = document.createElement("span");
        e.className = "x";
        e.title = t.mode === "Edit" ? "switch to preview" : "switch to edit";
        e.textContent = "✎";
        e.onclick = (ev) => {
          ev.stopPropagation();
          send({ t: "SetMode", rel: t.rel, mode: t.mode === "Edit" ? "Preview" : "Edit" });
        };
        b.appendChild(e);
      }
      const x = document.createElement("span");
      x.className = "x";
      x.title = "close";
      x.textContent = "×";
      x.onclick = (e) => { e.stopPropagation(); closeTab(pi, ti, t); };
      b.appendChild(x);
      strip.appendChild(b);
    });
    const plus = document.createElement("span");
    plus.className = "newterm";
    plus.title = "new terminal";
    plus.textContent = "+";
    plus.onclick = () => newTerminal(pi);
    strip.appendChild(plus);

    const active = pane.tabs[pane.active];
    const activeKey = active ? tabKey(active) : "";
    if (content.dataset.mountedKey === activeKey) {
      // The same tab is still active in this pane. A State snapshot fires
      // on every EditBuffer — including ones caused by the user's own
      // typing in the very editor this pane is showing — so rebuilding
      // content here on every call would tear the textarea (and its focus
      // and caret) out from under the user's cursor on each debounce tick.
      return;
    }
    // Park every terminal before clearing, so nodes are never destroyed.
    content.querySelectorAll(".termhost").forEach((n) => pool().appendChild(n));
    content.innerHTML = "";
    content.dataset.mountedKey = activeKey;
    if (active) mountTab(content, active);
  });

  // A closed/moved-away terminal tab means no pane anywhere still
  // references that session (sessions are deduped globally, same as File
  // tabs by rel) — tear its socket and xterm instance down instead of
  // leaking a live PTY reader for the rest of the page's life.
  terms.forEach((e, session) => {
    if (liveSessions.has(session)) return;
    try { e.sock.close(); } catch {}
    try { e.term.dispose(); } catch {}
    e.node.remove();
    terms.delete(session);
  });
}

function pool() { return document.getElementById("termpool"); }

function newTerminal(pane) {
  const name = prompt("Terminal name (letters, digits, _ and - only, max 32 chars):", "shell");
  if (name === null) return;
  if (!SESSION_RE.test(name)) {
    alert("invalid session name — use only letters, digits, _ and -, up to 32 characters");
    return;
  }
  send({ t: "OpenTab", pane, tab: { k: "Terminal", session: name } });
}

function mountTab(content, t) {
  // Invalidate any fetch already in flight for this content element: a
  // response landing after the pane has moved on (e.g. to a Terminal tab)
  // must not clobber whatever is here now — see the dataset.url check below.
  delete content.dataset.url;
  if (t.k === "Terminal") {
    const e = ensureTerm(t.session);
    content.appendChild(e.node);   // MOVE, not rebuild — the socket survives
    requestAnimationFrame(() => {
      try { e.fit.fit(); e.term.focus(); sendResize(e); } catch {}
    });
    return;
  }
  if (t.k === "File" && t.mode === "Edit") { mountEditor(content, t.rel); return; }
  const url =
    t.k === "Tree" ? `/frag/${PROJECT}/tree`
    : t.k === "Changes" ? `/frag/${PROJECT}/changes`
    : t.k === "File" ? `/frag/${PROJECT}/file?path=${encodeURIComponent(t.rel)}`
    : `/frag/${PROJECT}/diff${t.rel ? "?path=" + encodeURIComponent(t.rel) : ""}`;
  content.dataset.url = url;
  fetch(url).then((r) => r.text()).then((html) => {
    if (content.dataset.url !== url) return; // this pane moved on before we got here
    content.innerHTML = html;
    content.querySelectorAll("pre code").forEach((b) => window.hljs && hljs.highlightElement(b));
    wireFragment(content);
    // Tree fragments carry lazy <details hx-get="...tree?dir=..."
    // hx-trigger="toggle once"> nodes (render::tree_level). htmx only binds
    // hx-* attributes when it walks the DOM itself (page boot, or its own
    // ajax swaps); content dropped in via plain innerHTML — like this fetch
    // — is invisible to it until told, so a freshly loaded tree needs an
    // explicit process() or every closed directory would be inert forever.
    if (t.k === "Tree") window.htmx && htmx.process(content);
  });
}

// TreeChanged fires on every filesystem write — including every file Claude
// edits from a terminal pane, which is deadlight's core use case — so this
// must NOT do what refreshKind("Tree") does: a full re-fetch replaces the
// whole tree with a fresh one-level render that only pre-expands the
// currently open file's path, collapsing everything else the user had
// opened. Expansion is deliberately not server state (no protocol change),
// so the only place to learn what's currently expanded is the DOM itself:
// re-fetch each open <details data-rel> in place and leave the rest alone.
function refreshTree() {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (!active || active.k !== "Tree") return;
    const content = document.querySelector(`.pane[data-pane="${pi}"] .content`);
    if (!content) return;
    content.querySelectorAll("details[open][data-rel]").forEach((d) => {
      const rel = d.dataset.rel;
      const url = `/frag/${PROJECT}/tree?dir=${encodeURIComponent(rel)}`;
      fetch(url).then((r) => r.text()).then((html) => {
        // The node this <details> belongs to may itself have been replaced
        // by an ancestor's refresh completing first; writing into a
        // detached child is harmless (it's just discarded with the node).
        const ul = d.querySelector(":scope > ul");
        if (!ul) return;
        ul.innerHTML = html;
        wireFragment(ul);
        window.htmx && htmx.process(ul);
      });
    });
  });
}

function wireFragment(content) {
  content.querySelectorAll("a.file[data-rel]").forEach((a) => {
    a.onclick = (e) => {
      e.preventDefault();
      // These anchors still carry hx-get/hx-target="#content" from the
      // fragment templates (unchanged per Task 9's scope); #content no
      // longer exists in the four-pane skeleton, so stop the event here
      // or htmx's own delegated listener would also fire and log a swap
      // error trying to target it.
      e.stopPropagation();
      const rel = a.dataset.rel;
      const isDiff = a.getAttribute("hx-get")?.includes("/diff");
      send({
        t: "OpenTab",
        pane: 2,
        tab: isDiff ? { k: "Diff", rel: rel || null } : { k: "File", rel, mode: "Preview" },
      });
    };
    a.oncontextmenu = (e) => { e.preventDefault(); fileMenu(e, a.dataset.rel); };
  });
  // right-clicking blank space in a tree targets the project root
  content.oncontextmenu = (e) => {
    if (e.target.closest("a.file")) return;
    e.preventDefault();
    fileMenu(e, "");
  };
}

// Deliberately prompt-based: no menu widget to build, and every destructive
// action gets a confirmation step for free.
function fileMenu(e, rel) {
  const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
  const choice = prompt(
    `${rel || "(project root)"}\n\n` +
      "1 = new file   2 = new folder   3 = rename   4 = delete\n" +
      "Enter a number:",
    "1"
  );
  if (choice === "1") {
    const name = prompt("New file path:", dir ? `${dir}/untitled.txt` : "untitled.txt");
    if (name) send({ t: "CreateFile", rel: name });
  } else if (choice === "2") {
    const name = prompt("New folder path:", dir ? `${dir}/newdir` : "newdir");
    if (name) send({ t: "CreateDir", rel: name });
  } else if (choice === "3" && rel) {
    const to = prompt("Rename to:", rel);
    if (to && to !== rel) send({ t: "RenamePath", from: rel, to });
  } else if (choice === "4" && rel) {
    // the server refuses non-empty directories regardless of what we ask
    if (confirm(`Delete ${rel}?`)) send({ t: "DeleteFile", rel });
  }
}

function refreshKind(kind) {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (active && active.k === kind) {
      const content = document.querySelector(`.pane[data-pane="${pi}"] .content`);
      mountTab(content, active);
    }
  });
}

function ensureTerm(session) {
  const existing = terms.get(session);
  if (existing && !existing.stale) return existing;
  if (existing) {
    // The socket died (server restart, network blip) — the pooled node is
    // just as dead, so replace it rather than reattach to a closed xterm.
    try { existing.term.dispose(); } catch {}
    existing.node.remove();
    terms.delete(session);
  }
  const node = document.createElement("div");
  node.className = "termhost";
  node.dataset.session = session;
  pool().appendChild(node);
  const term = new Terminal({ convertEol: false, fontSize: 13 });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(node);
  const sock = new WebSocket(wsUrl(`/ws/${PROJECT}/term/${session}`));
  sock.binaryType = "arraybuffer";
  sock.onmessage = (e) => term.write(new Uint8Array(e.data));
  term.onData((d) => { if (sock.readyState === 1) sock.send(new TextEncoder().encode(d)); });
  const entry = { node, term, fit, sock, stale: false };
  sock.onclose = () => { entry.stale = true; };
  sock.onerror = () => { entry.stale = true; };
  terms.set(session, entry);
  return entry;
}

function sendResize(e) {
  if (e.sock.readyState === 1) e.sock.send(`resize:${e.term.cols}x${e.term.rows}`);
}

function mountEditor(content, rel) {
  const ta = document.createElement("textarea");
  ta.className = "editor";
  ta.spellcheck = false;
  // The server reads the file itself the moment this rel enters Edit mode
  // (SetMode/OpenTab, see hub.rs) and pushes the content as a BufferText
  // with an empty origin, landing in `texts` — there is no client-side
  // fetch here. If that push hasn't arrived yet, the textarea starts empty
  // and the BufferText handler in onEvent fills it in as soon as it does.
  ta.value = texts.has(rel) ? texts.get(rel) : "";
  editors.set(rel, ta);
  let timer = null;
  ta.oninput = () => {
    clearTimeout(timer);
    timer = setTimeout(() => send({ t: "EditBuffer", rel, text: ta.value }), 200);
  };
  ta.onkeydown = (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      send({ t: "EditBuffer", rel, text: ta.value });
      send({ t: "SaveBuffer", rel, force: false });
    }
  };
  content.appendChild(ta);
}

function showConflict(ev) {
  const box = document.createElement("div");
  box.className = "conflict";
  box.innerHTML =
    `<b>${escapeHtml(ev.rel)} changed on disk since you opened it.</b>` + ev.diff_html;
  const over = document.createElement("button");
  over.textContent = "overwrite";
  over.onclick = () => { send({ t: "SaveBuffer", rel: ev.rel, force: true }); box.remove(); };
  const reload = document.createElement("button");
  reload.textContent = "discard mine";
  reload.onclick = () => { send({ t: "CloseBuffer", rel: ev.rel }); box.remove(); };
  box.append(over, reload);
  document.querySelector('.pane[data-pane="2"] .content').prepend(box);
}

// Transient, dismissible: reuses .conflict's border/padding/button styling
// (positioned as a fixed overlay via .error-banner) rather than inventing a
// new visual language just for this.
function showError(msg) {
  const box = document.createElement("div");
  box.className = "conflict error-banner";
  const text = document.createElement("b");
  text.textContent = `Error: ${msg}`;
  const dismiss = document.createElement("button");
  dismiss.textContent = "dismiss";
  dismiss.onclick = () => box.remove();
  box.append(text, dismiss);
  document.body.appendChild(box);
  setTimeout(() => box.remove(), 8000);
}

function closeTab(pi, ti, t) {
  const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
  if (meta && meta.dirty && !confirm(`${t.rel} has unsaved changes. Close it?`)) return;
  send({ t: "CloseTab", pane: pi, idx: ti });
}

function escapeHtml(s) {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

// dividers
let drag = null;
document.querySelectorAll(".divider").forEach((d) => {
  d.onmousedown = (e) => { drag = { which: d.dataset.div, x: e.clientX, y: e.clientY }; e.preventDefault(); };
});
window.onmouseup = () => {
  if (drag && state) {
    send({ t: "Resize", sizes: state.sizes });
    // A divider drag resizes a pane's .content without going through
    // render()'s mountedKey guard (which correctly skips remounting a
    // still-active terminal), so nothing else re-fits a terminal that was
    // sitting in that pane. Do it once here instead of on every mousemove
    // frame: the PTY takes the *smallest* attached client's geometry, so a
    // stale cols/rows here would clip output for every other mirroring
    // client until this tab happened to be switched away and back.
    terms.forEach((e) => {
      // Skip anything parked in #termpool: it's display:none, so fit()
      // would measure a 0x0 box and send a bogus resize.
      if (e.node.parentElement && e.node.parentElement.classList.contains("content")) {
        try { e.fit.fit(); sendResize(e); } catch {}
      }
    });
  }
  drag = null;
};
window.onmousemove = (e) => {
  if (!drag || !state) return;
  // Sizes are server-side u32s: round every pixel/percent value before it
  // reaches state, or a fractional value fails JSON deserialization on send.
  if (drag.which === "left-w") state.sizes.left_w = Math.round(Math.max(120, e.clientX));
  if (drag.which === "right-w") state.sizes.right_w = Math.round(Math.max(200, window.innerWidth - e.clientX));
  if (drag.which === "left-split") state.sizes.left_split = Math.round(Math.min(90, Math.max(10, (e.clientY / window.innerHeight) * 100)));
  render();
};

window.addEventListener("resize", () => terms.forEach((e) => { try { e.fit.fit(); sendResize(e); } catch {} }));

// A directory's first expand is driven by real htmx (hx-get + hx-trigger
// "toggle once" on the <details>, see render::tree_level) rather than the
// manual fetch() path everything else in this file uses, so it never runs
// through mountTab's own wireFragment() call. Rewire file-click handling on
// whatever htmx just swapped in — a no-op for the one other thing htmx
// drives (#gitinfo's status span, which has no a.file to find).
window.htmx && htmx.on("htmx:afterSwap", (e) => wireFragment(e.detail.target));

const refreshBtn = document.getElementById("refresh");
if (refreshBtn) refreshBtn.onclick = () => {
  document.body.dispatchEvent(new Event("refresh")); // #gitinfo hx-trigger listens
  send({ t: "RequestState" });
};

connectControl();
