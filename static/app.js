// Workspace client. Chrome (tabstrips, tree/changes/file/diff panes) is
// rendered from mirrored server state on every event; terminals are pooled
// DOM nodes that are MOVED between panes with appendChild, never rebuilt —
// rebuilding a xterm instance drops its websocket and detaches the shell.
const PROJECT = document.body.dataset.project;
// The config file's `show_hidden`, embedded by render.rs at page load. The
// workspace's own toggle (state.show_hidden) overrides it when set; null
// means nobody has touched the header button, so the file still decides.
const SHOW_HIDDEN_DEFAULT = document.body.dataset.showHidden === "1";
// Whether the editor writes a buffer out by itself. Read once per page load,
// like SHOW_HIDDEN_DEFAULT: it changes only when someone edits a config file,
// which already needs a reload to take effect.
const AUTOSAVE = document.body.dataset.autosave === "1";
// How long after the last keystroke an autosave fires. VS Code's default,
// and comfortably longer than the 200ms EditBuffer debounce it depends on.
const AUTOSAVE_MS = 1000;
// Highlighting an editor is worth it up to a point and then it is not: the
// buffer cap is 2 MB, and hljs re-runs over the whole text on every pause in
// typing. Past this the editor stays a plain textarea rather than becoming a
// laggy one. Chosen as roughly the largest source file anyone edits by hand;
// resh's own biggest is a quarter of it.
const MAX_HIGHLIGHT_BYTES = 100_000;
// Known to hljs but deliberately left plain — see codeLanguage.
const PLAIN_EXTS = new Set(["md", "markdown", "txt", "text"]);
// code-input wraps a real <textarea> and paints a highlighted <pre> under it,
// which is why it can be dropped in here at all: everything downstream —
// `editors`, the edit debounce, autosave, ⌘S, the conflict patch, blur flush
// — goes on talking to the same textarea it always did. Registered once; the
// element carries template="hl" to select it.
if (window.codeInput && window.hljs) {
  codeInput.registerTemplate("hl", codeInput.templates.hljs(hljs, []));
}
const showHidden = () => (state && state.show_hidden != null ? state.show_hidden : SHOW_HIDDEN_DEFAULT);
// What the tree was last rendered with, so a State that flips the setting
// re-fetches. Fragments are server-rendered against the workspace value, so
// nothing in the DOM would otherwise change until the next filesystem event.
let treeShownHidden = null;
const SESSION_RE = /^[A-Za-z0-9_-]{1,32}$/; // must match session::valid_name server-side
const DIVIDER_PX = 8; // keep in step with --divider in style.css

const wsUrl = (p) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${p}`;

// Mirrors routes.rs NO_TEXT_EDIT_EXT — NOT IMAGE_EXT, which is a wider list
// answering a different question. svg is absent on purpose: it renders as a
// picture but is text, and has always been editable. Nothing checks the two
// lists agree; a mismatch hides or shows the ✎ toggle wrongly, but never
// loses data: workspace.rs refuses or coerces every path to Edit
// server-side too (SetMode refuses it, OpenTab coerces a raw Edit request to
// Preview, and EditBuffer — the actual save chokepoint — refuses to create a
// buffer at all), so no client bug here can make a save truncate a file.
const NO_TEXT_EDIT_EXT = ["png", "jpg", "jpeg", "gif", "webp", "ico", "pdf"];
// Must extract the extension exactly as assets::ext_of does, or syncing the
// lists would not sync the behaviour: take the LAST path segment (so
// `img.d/README` has no extension rather than inheriting `d`) and require a
// dot in it.
const extOf = (rel) => {
  const name = (rel || "").split("/").pop();
  const i = name.lastIndexOf(".");
  return i < 0 ? "" : name.slice(i + 1).toLowerCase();
};
const refusesTextEdit = (rel) => NO_TEXT_EDIT_EXT.includes(extOf(rel));
/// Files this app can *draw* rather than only spell out: markdown renders, and
/// so does every image. Mirrors routes.rs's IMAGE_EXT plus the two markdown
/// extensions render.rs's file_fragment branches on.
///
/// Deliberately wider than NO_TEXT_EDIT_EXT, and the gap is the point — svg is
/// here *and* editable, because it draws as a picture and is text. CLAUDE.md
/// records the release where gating Edit on "is it an image" silently took
/// that away.
const RENDERED_EXT = ["md", "markdown", "png", "jpg", "jpeg", "gif", "webp", "svg", "ico"];
const hasRenderedForm = (rel) => RENDERED_EXT.includes(extOf(rel));
/// How a file opens when the user just clicks it.
///
/// Edit, for anything the editor can hold — which is what an IDE does, and
/// what this app's preview was standing in for. Preview survives where it is
/// not a stand-in: a file with a rendered form, where the drawing is the thing
/// you opened it to see. A file that turns out not to be text at all is not
/// decided here — the server reads it, fails, and moves the tab back to
/// Preview itself, because only it can know.
function defaultMode(rel) {
  return hasRenderedForm(rel) ? "Preview" : "Edit";
}

// --- terminal links ------------------------------------------------------
// Cmd on macOS, Ctrl everywhere else. Not a preference: Ctrl+click on a Mac is
// right-click emulation, so binding there would pop a context menu and open a
// link at once. This is the same platform split xterm itself makes in
// shouldForceSelection (alt on Mac, shift elsewhere).
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const linkModifier = (e) => (IS_MAC ? e.metaKey : e.ctrlKey);

// Tracked rather than read off the event, because provideLinks is never handed
// one. Cleared on blur: a user who switches apps with the key down would
// otherwise come back to a terminal that is silently armed.
let linksArmed = false;

// http and https only. `javascript:`, `data:` and `file:` never become a link
// at all — not a refused one, one that was never offered.
const SAFE_URL = /^https?:\/\//i;
const URL_RE = /\bhttps?:\/\/[^\s"'<>`]+/gi;
// A slash is the evidence. Bare `main.rs` is deliberately not a path: a repo
// has many, so resolution would have to guess, and the same shape matches a
// version string and the `foo.bar` in an error message.
const PATH_RE = /(?:~\/|\.{1,2}\/|\/)?(?:[\w.@+-]+\/)+[\w.@+-]+(?::\d+(?::\d+)?)?/g;

let state = null;
let myOrigin = null;
let ctrl = null;
const terms = new Map();   // session -> {node, term, fit, sock, ...} (see ensureTerm)
const editors = new Map(); // rel -> textarea (the currently mounted one, if any)
const texts = new Map();   // rel -> latest known buffer text (server-authoritative)
const autosaveTimers = new Map(); // rel -> pending autosave timeout
// rel -> pending EditBuffer debounce. Presence means "this client has typed
// something the server has not been told about yet", which is the *only*
// accurate local dirty signal: `state`'s own dirty flag lags by the debounce,
// so a save triggered inside that window (blur, ⌘S) would decide there was
// nothing to save and quietly do nothing.
const pendingEdits = new Map();
// Buffers autosave has taken its hands off: the file diverged from what this
// buffer was based on, so writing it would be a decision, not a save. Left to
// ⌘S (and the banner's overwrite), which is also what clears this — via
// SaveOk, so it reopens only on a write that actually landed.
const autosavePaused = new Set();
/// rels whose EditBuffer this client has sent and not yet seen settled.
///
/// Exists because "is there anything to save?" has three answers, not two.
/// `pendingEdits` is gone the moment the 200ms debounce fires, and the
/// server's `dirty` flag only becomes true when its State arrives; between
/// those two the honest answer is "I cannot tell yet", and autosaveNow used
/// to read that as "nothing to do" and return leaving no timer behind. The
/// edit was then never written — not late, never, until the user happened to
/// type again or blur the window. Idle, the State wins that race and the gap
/// is invisible; on a loaded machine it does not.
const sentEdits = new Set();

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
      // A rel missing from the fresh buffer list is gone server-side (the
      // last tab on it closed clean, or its edits were explicitly
      // discarded) — prune it here rather than let texts/editors grow for
      // the life of the session. This is safe because hub::open_buffer_for
      // always re-broadcasts a fresh BufferText the moment a rel re-enters
      // Edit mode, even for a buffer it kept around dirty; nothing here can
      // be the last copy of unsaved text.
      {
        const openRels = new Set(state.buffers.map((b) => b.rel));
        for (const rel of texts.keys()) if (!openRels.has(rel)) texts.delete(rel);
        for (const rel of editors.keys()) if (!openRels.has(rel)) editors.delete(rel);
        for (const rel of sentEdits) if (!openRels.has(rel)) sentEdits.delete(rel);
        // Autosave resumes as soon as the server says this buffer has nothing
        // outstanding — saved, discarded, or gone. SaveOk is the common route
        // and clears it sooner, but not the only one: the banner's "discard
        // mine" resolves the divergence without any write happening, and
        // without this that buffer stayed paused for the rest of the session
        // with the header still claiming the file had changed underneath it.
        for (const rel of autosavePaused) {
          const b = state.buffers.find((x) => x.rel === rel);
          if (!b || (!b.dirty && !b.stale)) autosavePaused.delete(rel);
        }
      }
      render();
      // Toggling visibility changes what the *server* renders, not how the
      // client draws it, so the State that carries the new value has to be
      // followed by a re-fetch. refreshTree (not a remount) keeps whatever
      // the user had expanded.
      if (treeShownHidden !== null && treeShownHidden !== showHidden()) refreshTree();
      treeShownHidden = showHidden();
      break;
    case "BufferText": {
      // Skip our own text or the cursor jumps; empty origin = external change
      // (a background save, SetMode's initial disk read, or Claude editing
      // the file directly). texts is updated unconditionally (not gated on
      // an editor being mounted right now) so mountEditor can always seed
      // from it, even for text that arrived before its tab was ever opened.
      // NOT gated on the buffer being dirty: EditBuffer's handler (hub.rs)
      // broadcasts BufferText to every *other* client on every keystroke,
      // dirty or not — that's how a second client watching the same file
      // live-syncs with the one typing — and open_buffer_for re-broadcasts
      // the current text on reopen even when already dirty, so a freshly
      // opened tab picks up in-progress edits instead of stale disk
      // content. The one case that must never reach here (a dirty buffer's
      // file changing externally) is handled entirely server-side: the
      // server sends BufferStale instead of BufferText for exactly that
      // case, so a client-side dirty check here would be redundant at best
      // and would break the two legitimate cases above at worst.
      if (ev.origin && ev.origin === myOrigin) break;
      // An empty origin is the server telling us what the file now says — a
      // discard, a reload, an external write. Whatever this client had
      // outstanding is superseded by it, and holding the flag past that would
      // let a timer re-send text the user just discarded.
      if (!ev.origin) sentEdits.delete(ev.rel);
      texts.set(ev.rel, ev.text);
      const ta = editors.get(ev.rel);
      // Through the <code-input> where there is one: assigning the textarea's
      // value directly changes no attribute and fires no input event, so the
      // highlighted layer under it would keep painting the old text. The
      // element's own value setter writes the textarea *and* schedules the
      // repaint.
      if (ta && ta.value !== ev.text) {
        const host = ta.closest("code-input");
        if (host) host.value = ev.text; else ta.value = ev.text;
      }
      break;
    }
    case "BufferStale": {
      // The server only pushes this flag standalone (a dirty buffer whose
      // file changed underneath it) — patch it locally rather than wait
      // for an unrelated event to bring a fresh State snapshot.
      const b = state && state.buffers.find((x) => x.rel === ev.rel);
      // Stale means someone else wrote the file this buffer came from.
      // Autosave stops here rather than racing them.
      autosavePaused.add(ev.rel);
      if (b) { b.stale = true; render(); }
      break;
    }
    case "TreeChanged": refreshTree(); break;
    case "StatusChanged": refreshKind("Changes"); break;
    case "FileChanged": refreshKind("Diff"); refreshFile(ev.rel); break;
    case "SaveConflict":
      // Without this an autosaving client re-raises the banner every second.
      autosavePaused.add(ev.rel);
      showConflict(ev);
      render();
      break;
    // A write that landed is the only thing that resumes autosave: the
    // divergence it paused for is resolved, whether by ⌘S or by overwrite.
    case "SaveOk": autosavePaused.delete(ev.rel); sentEdits.delete(ev.rel); break;
    case "TerminalStarted":
      // The server only validated the name and notified everyone; opening
      // this socket is what actually spawns the PTY (see ensureTerm). But
      // TerminalStarted is broadcast to every client of this project, not
      // just the one that asked — only attach if this client actually has
      // a Terminal tab on that session, or every mirroring tab would open
      // and immediately close a PTY socket for a session it never showed.
      if (state && state.panes.some((p) => p.tabs.some((t) => t.k === "Terminal" && t.session === ev.session))) {
        ensureTerm(ev.session);
      }
      render();
      // live_sessions itself arrives moments later in the State broadcast
      // that follows this event, but the header strip (a separate htmx
      // fragment, not part of `state`) won't refetch on its own — nudge it
      // so ○ becomes ● without waiting for a manual refresh or reload.
      document.body.dispatchEvent(new Event("refresh"));
      break;
    case "GitInit":
      if (!ev.ok) showError("git init failed: " + ev.msg);
      // On success, is_git flips in the State snapshot that follows this
      // event; tabKey folds is_git into a placeholder's key (see tabKey),
      // so that State's render() swaps the "not a git repo" offer for the
      // normal start hint on its own — nothing else needed here.
      break;
    case "CloseRefused":
      // Backstop only: the Close button's own handler already checks
      // dirty buffers before ever sending CloseProject. This covers a
      // buffer that went dirty in the gap between that check and the
      // server processing the intent.
      showError("Cannot close: unsaved changes in " + ev.dirty.join(", "));
      break;
    case "ProjectClosed":
      // A successful close, not a failure — showBanner directly so this
      // doesn't get the "Error:" prefix showError adds.
      showBanner(ev.ended + " terminal session(s) ended");
      terms.forEach((e) => {
        try { e.sock.close(); } catch {}
        try { e.term.dispose(); } catch {}
        // terms.clear() below is the only chance to do this: unlike
        // render()'s own teardown, nothing here will revisit this node
        // later, so a missed remove() leaks a disposed .termhost into
        // #termpool for the life of the page.
        try { e.node.remove(); } catch {}
      });
      terms.clear();
      // Every one of pendingLink's possible entries just had its node
      // removed and its xterm disposed above — drop the stale reference so a
      // later, unrelated PathRefused (a duplicate delivery, say) cannot
      // termFlash a detached node instead of silently finding nothing.
      pendingLink = null;
      // Every node above is gone, but `state.live_sessions` is still the
      // stale pre-close list until the trailing State broadcast lands —
      // tabKey would keep reading ":live" from it, render()'s mountedKey
      // guard would see an unchanged key, and the now-empty pane would sit
      // blank instead of remounting the placeholder. Clear the local view
      // immediately so this render() actually recomputes the key.
      if (state) state.live_sessions = [];
      render();
      document.body.dispatchEvent(new Event("refresh")); // strip marker -> ○
      // Leave the workspace: with its sessions ended there is nothing left here
      // to act on, and staying puts the user in a project they just closed.
      // Delayed so the banner above is readable first — the count is the only
      // confirmation of what actually happened, and the server has already
      // acted, so nothing here depends on the delay completing.
      setTimeout(() => { location.href = "/"; }, 1200);
      break;
    case "Notice": onNotice(ev.notice); break;
    case "Notices":
      notices = ev.list;
      // The tab-strip dot (hasAttention) is derived straight from `notices`
      // on every render — nothing to reconcile here, unlike a separately
      // maintained set that could drift from what the server just said.
      renderNotices();
      render();
      break;
    case "Error":
      // Every server-side failure funnels through here (already-exists,
      // directory-not-empty, path-outside-project, too-many-buffers,
      // no-buffer-for-X, save I/O errors, malformed intents...) — without a
      // visible banner, e.g. deleting a non-empty directory looks like a
      // silent no-op. console.warn stays too, for anyone actually watching devtools.
      console.warn("resh:", ev.msg);
      showError(ev.msg);
      break;
    case "PathRefused":
      // Not showError: that funnels to the workspace banner, which is the
      // wrong shape here and — as the Error case above notes — carries no way
      // back to the terminal that was clicked. This does, via the click still
      // in flight.
      console.warn("resh:", ev.msg);
      if (pendingLink && pendingLink.text === ev.text) {
        termFlash(pendingLink.entry, ev.msg);
        // Cleared only on a match. A mismatch means a DIFFERENT click is
        // still in flight (PATH_RE marks ordinary prose, so a user arming
        // links over a paragraph and clicking twice before the first reply
        // lands is the common case, not an edge case) — clearing here would
        // strand that click's own refusal, which would then find an empty
        // slot and drop silently to console.warn instead of flashing.
        pendingLink = null;
      }
      break;
  }
}

function tabKey(t) {
  switch (t.k) {
    // Mode is part of the key: toggling Preview<->Edit must force a remount
    // (fetched HTML vs. a live textarea), not be treated as "unchanged".
    case "File": return `File:${t.rel}:${t.mode}`;
    case "Diff": return `Diff:${t.rel || ""}`;
    case "Terminal": {
      // The placeholder and the attached terminal are two different DOM
      // shapes for the same tab. Folding them into one key would make
      // render()'s "already mounted, skip" fast path (below) never re-run
      // mountTab once a session goes live, leaving the "press Enter" hint
      // sitting there forever over a terminal that's actually running.
      // Same reasoning for is_git: a successful InitGit must swap the
      // "not a git repo" placeholder for the normal one without the user
      // having to switch tabs and back.
      const live = terms.has(t.session) || state.live_sessions.includes(t.session);
      return live ? `Terminal:${t.session}:live` : `Terminal:${t.session}:placeholder:${state.is_git}`;
    }
    default: return t.k;
  }
}

// Mirrors render::icon_ext — the stylesheet keys every file icon on data-ext,
// and a tab must land on the same glyph as the tree row that opened it.
function iconExt(rel) {
  const name = rel.split("/").pop();
  const ext = name.split(".").pop();
  if (ext === name || !ext || ext.length > 10 || !/^[a-z0-9]+$/i.test(ext)) return "";
  return ext.toLowerCase();
}

function tabLabel(t) {
  switch (t.k) {
    case "Tree": return "Files";
    case "Changes": return "Changes";
    case "File": return t.rel.split("/").pop();
    case "Diff": return t.rel ? t.rel.split("/").pop() : "full diff";
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
      b.className =
        "tab" +
        (ti === pane.active ? " active" : "") +
        (t.k === "Terminal" && hasAttention(t.session) ? " attn" : "");
      // data-kind/data-ext drive the tab's icon; see the [data-kind]/[data-ext]
      // rules in style.css, which paint tree rows from the same attributes.
      b.dataset.kind = t.k.toLowerCase();
      if ((t.k === "File" || t.k === "Diff") && t.rel) b.dataset.ext = iconExt(t.rel);
      const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
      b.innerHTML =
        (meta && meta.dirty ? '<span class="dirty">●</span> ' : "") +
        (meta && meta.stale ? '<span class="stale">⚠</span> ' : "") +
        escapeHtml(tabLabel(t));
      // Terminal tabs route through focusSession, not a bare ActivateTab, so
      // the obvious gesture of clicking a dotted tab is what clears its dot
      // — see hasAttention/focusSession below.
      b.onclick = () =>
        t.k === "Terminal" ? focusSession(t.session) : send({ t: "ActivateTab", pane: pi, idx: ti });
      // Two modes are worth switching between only where the file has both: a
      // rendered form to look at and text to edit. That is markdown and svg —
      // a png renders but cannot be edited, and a code file is editable but
      // renders as nothing, so offering it "switch to preview" would advertise
      // a mode nothing now opens in.
      if (t.k === "File" && hasRenderedForm(t.rel) && !refusesTextEdit(t.rel)) {
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
      x.title =
        t.k === "Terminal" ? "end session (alt-click to detach, leaving it running)" : "close";
      x.textContent = "×";
      x.onclick = (e) => { e.stopPropagation(); closeTab(pi, ti, t, e.altKey); };
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
    // Built here rather than after the mountedKey guard below: the guard
    // returns whenever the active tab is unchanged, but these depend on the
    // pane's tab *count* too, which another pane's move can change without
    // touching this one's active tab.
    buildPaneIcons(el.querySelector(".paneicons"), pi, pane, active, content);
    // The tab strip wraps, so opening or closing a tab can add or drop a row
    // and change the header's height — which resizes .content under a
    // terminal that is still the active tab, and so is never remounted or
    // re-fit by anything below. Reading offsetHeight here forces the layout
    // the innerHTML above invalidated, so this sees the new height. Left
    // stale, the PTY would keep the old geometry, and since it takes the
    // *smallest* attached client's size, that clips output for every other
    // client mirroring the session too.
    const head = el.querySelector(".panehead");
    const headH = String(head.offsetHeight);
    if (head.dataset.h !== headH) {
      head.dataset.h = headH;
      content.querySelectorAll(".termhost").forEach((n) => {
        const e = terms.get(n.dataset.session);
        if (e) { try { e.fit.fit(); sendResize(e); } catch {} }
      });
    }
    // Repainted on every State, not built once at mount: a buffer goes dirty
    // and clean while the editor stays exactly where it is — the guard below
    // deliberately leaves a mounted textarea alone so typing is not torn out
    // from under the cursor — so nothing else would ever update this.
    if (active && active.k === "File" && active.mode === "Edit") paintSaveState(content, active.rel);
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
    // Disarm the reconnect before closing: this teardown is deliberate (no
    // pane references the session any more), and both a pending timer and
    // the close() below firing onclose would otherwise reattach — which,
    // since attach creates when absent, would respawn the shell the user
    // just ended, into a disposed xterm.
    e.gone = true;
    clearTimeout(e.timer);
    try { if (e.sock) e.sock.close(); } catch {}
    try { e.term.dispose(); } catch {}
    e.node.remove();
    terms.delete(session);
    // Same reasoning as ProjectClosed's teardown: this entry's node is gone,
    // so a pendingLink still pointing at it must not outlive it.
    if (pendingLink && pendingLink.entry === e) pendingLink = null;
  });
}

// Per-pane header controls. Everything here drives an existing intent, so the
// result mirrors to other browsers and survives a restart exactly like a drag
// of a divider does — collapse-all is the one exception, being view state that
// no other client has an opinion about.
function buildPaneIcons(host, pi, pane, active, content) {
  if (!host) return;
  host.innerHTML = "";
  const icon = (glyph, title, fn) => {
    const b = document.createElement("span");
    b.className = "paneicon";
    b.title = title;
    b.textContent = glyph;
    b.onclick = fn;
    host.appendChild(b);
    return b;
  };
  if (active && active.k === "Tree") {
    const hidden = showHidden();
    // Filled ring = dot entries are showing. This drives an intent rather
    // than filtering client-side: the rows do not exist in the fragment the
    // server sent, so there is nothing here to un-hide.
    icon(hidden ? "◍" : "◌", hidden ? "hide dotfiles" : "show dotfiles", () => {
      send({ t: "SetShowHidden", on: !showHidden() });
    });
    icon("⌃", "collapse all", () => {
      content.querySelectorAll("details[open]").forEach((d) => { d.open = false; });
    });
  }
  // Only between the two content panes, and only when there is a tab to move.
  // The left column holds narrow tool windows — the tree and the changes list,
  // 260px by default — so moving a terminal or an editor into one is not a
  // move anyone wants, and offering it makes the control read as general when
  // it is not. A control that never does anything useful is worse than one
  // that isn't there.
  const swap = MOVE_BETWEEN[pi];
  if (pane.tabs.length && swap !== undefined) {
    icon("⇄", `move this tab to the ${swap === RIGHT ? "right" : "middle"} pane`, () => {
      // Append, and let the server clamp: `at` is checked against the
      // destination's real length in workspace.rs, which is the authority on
      // a layout this client may already be a broadcast behind on.
      send({ t: "MoveTab", from: pi, idx: pane.active, to: swap, at: state.panes[swap].tabs.length });
    });
  }
  const on = maxState.pane === pi;
  icon(on ? "⤡" : "⤢", on ? "restore pane sizes" : "maximize pane", () => toggleMaximized(pi));
}

// Pane ids, matching proto.rs's LEFT_TOP / LEFT_BOTTOM / MIDDLE / RIGHT.
const MIDDLE = 2;
const RIGHT = 3;
// The only pair a tab is worth moving between; see buildPaneIcons.
const MOVE_BETWEEN = { [MIDDLE]: RIGHT, [RIGHT]: MIDDLE };
// Which pane is maximized, and the sizes to put back. Client-local on purpose:
// the maximized layout itself is just sizes, which the server already stores
// and mirrors, so nothing here needs a new field in the workspace. The cost is
// that a *different* browser resizing while this one is maximized leaves this
// restore stale — recoverable by dragging, and not worth a protocol change.
let maxState = { pane: null, prev: null };

function toggleMaximized(pi) {
  if (maxState.pane === pi) {
    send({ t: "Resize", sizes: maxState.prev });
    maxState = { pane: null, prev: null };
    return;
  }
  // Keep the *original* sizes when maximizing straight from one maximized pane
  // to another, or restore would put back a maximized layout.
  const prev = maxState.prev ?? { ...state.sizes };
  send({ t: "Resize", sizes: maximizedSizes(pi) });
  maxState = { pane: pi, prev };
}

// The grid is `left_w | divider | 1fr | divider | right_w`, with the left
// column split by percentage. Zeroing a track is what collapses a pane, and
// the surviving one takes the width; the middle pane needs no width of its own
// because 1fr absorbs whatever the other two give up.
function maximizedSizes(pi) {
  const grid = document.getElementById("grid");
  const full = Math.max(0, Math.round(grid.clientWidth - 2 * DIVIDER_PX));
  const split = state.sizes.left_split;
  switch (pi) {
    case 0: return { left_w: full, right_w: 0, left_split: 100 };
    case 1: return { left_w: full, right_w: 0, left_split: 0 };
    case 3: return { left_w: 0, right_w: full, left_split: split };
    default: return { left_w: 0, right_w: 0, left_split: split };
  }
}

function pool() { return document.getElementById("termpool"); }

function newTerminal(pane) {
  // No prompt: the server allocates term/term1/term2… from `live_names`, which
  // sees detached sessions the client has no tabs for. A name picked here could
  // collide with one of those, and since attaching creates only when absent,
  // "new terminal" would silently reattach to an old shell instead.
  send({ t: "NewTerminal", pane });
}

function mountTab(content, t) {
  // Invalidate any fetch already in flight for this content element: a
  // response landing after the pane has moved on (e.g. to a Terminal tab)
  // must not clobber whatever is here now — see the dataset.url check below.
  delete content.dataset.url;
  if (t.k === "Terminal") {
    const liveNow = state.live_sessions.includes(t.session);
    // Only attach when a session already exists (state.live_sessions, or a
    // pooled node we already spawned this page-load). Opening a project
    // must never fork a shell on its own — that is how nine unused
    // sessions accumulated on the production host before this gate existed.
    if (!liveNow && !terms.has(t.session)) {
      content.appendChild(terminalPlaceholder(t.session));
      return;
    }
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

// A bare empty pane is not discoverable, and a plain button would train the
// wrong muscle memory — people already press Enter in a fresh terminal to
// check it's alive. So the hint itself *is* the control: Enter or a click
// both start the session. Non-git directories get a different offer (init,
// or a quieter escape to start anyway) so a scratch directory without a
// repo doesn't become unusable.
function terminalPlaceholder(session) {
  const box = document.createElement("div");
  box.className = "termstart";
  const isGit = state.is_git;
  box.innerHTML = isGit
    ? `<p>Press <kbd>Enter</kbd> to start a terminal</p>`
    : `<p>Not a git repository.</p>
       <p><button class="initgit">Initialize git repo</button></p>
       <p><a class="nogit" href="#">start without git</a></p>`;
  // A held or double-tapped Enter must not fire several StartTerminal
  // intents before the box is remounted away — the server already dedupes
  // to one socket, but every extra intent is still a wasted broadcast. The
  // guard must release itself: a refusal (e.g. the 16-session cap in
  // hub.rs) only ever sends this client an Error, which carries no session
  // or tab identity to key a remount on, so no State/TerminalStarted event
  // is guaranteed to come along and remount this box away. Without the
  // timeout a refused start would leave the placeholder permanently inert.
  // 2s is well past any held-Enter repeat rate, so the burst-suppression
  // this guard exists for is unaffected.
  const start = () => {
    if (box.dataset.sent) return;
    box.dataset.sent = "1";
    setTimeout(() => { delete box.dataset.sent; }, 2000);
    send({ t: "StartTerminal", session });
  };
  if (isGit) {
    // Only this branch behaves like a control — tabIndex, the pointer
    // cursor and the focus outline (.termstart-live in style.css) belong to
    // it alone, or the non-git box looks clickable/focusable with no
    // handler wired to the box itself.
    box.classList.add("termstart-live");
    box.tabIndex = 0;
    box.onclick = start;
    box.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); start(); } };
    requestAnimationFrame(() => box.focus());
  } else {
    box.querySelector(".initgit").onclick = () => send({ t: "InitGit" });
    box.querySelector(".nogit").onclick = (e) => { e.preventDefault(); start(); };
  }
  return box;
}

// Stable identity for a tree <li>: a directory's own rel (render::tree_level
// puts `data-rel` on its <details>, open or closed) or a file's rel (on its
// <a class="file">). Used to match old DOM nodes against a freshly-fetched
// listing during reconciliation. Returns null for identity-less rows (e.g.
// the "tree truncated" hint <li>), which just get replaced wholesale since
// they carry no state worth preserving.
function treeItemId(li) {
  const d = li.querySelector(":scope > details[data-rel]");
  if (d) return "dir:" + d.dataset.rel;
  const a = li.querySelector(":scope > a.file[data-rel]");
  if (a) return "file:" + a.dataset.rel;
  return null;
}

// Merge a freshly-fetched listing into an existing <ul> (the tree root, or
// one directory's children) without discarding what's already there. A
// naive innerHTML swap would replace every child <details>, collapsing any
// subdirectory the user had expanded — exactly the problem TreeChanged
// exists to avoid. Instead: walk the fresh listing in the order the server
// sent it (tree_level already sorts dirs-before-files, case-insensitive —
// we never re-sort client-side) and for each entry, reuse the existing DOM
// node when one with the same identity is already present. A reused
// <details> keeps its `open` attribute and whatever children it already
// loaded; a reused file <a> keeps its "sel" class. Anything left over in
// `existing` (present before, absent from the fresh listing) is simply
// never re-appended, i.e. removed. This is also what fixes the nested-open
// race the reviewer flagged: reconciling a parent never tears down a still-
// present child <details>, so a subdirectory's own in-flight refresh always
// lands on a node that's still there (or, if that entry vanished from the
// parent's listing, on a harmlessly detached one).
function reconcileList(ul, html) {
  const fresh = document.createElement("ul");
  fresh.innerHTML = html;
  const existing = new Map();
  Array.from(ul.children).forEach((li) => {
    const id = treeItemId(li);
    if (id) existing.set(id, li);
  });
  const ordered = Array.from(fresh.children).map((li) => {
    const id = treeItemId(li);
    return (id && existing.get(id)) || li;
  });
  ul.innerHTML = "";
  ordered.forEach((li) => ul.appendChild(li));
  wireFileLinks(ul); // see wireFileLinks: no container oncontextmenu here
  window.htmx && htmx.process(ul);
}

// TreeChanged fires on every filesystem write — including every file Claude
// edits from a terminal pane, which is resh's core use case — so this
// must NOT do what refreshKind("Tree") does: a full re-fetch replaces the
// whole tree with a fresh one-level render that only pre-expands the
// currently open file's path, collapsing everything else the user had
// opened. Expansion is deliberately not server state (no protocol change),
// so the only place to learn what's currently expanded is the DOM itself:
// reconcile the root listing (a new root-level file must still show up
// without a reload) and every open <details data-rel>, in place, via
// reconcileList — never a wholesale replace.
function refreshTree() {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (!active || active.k !== "Tree") return;
    const content = document.querySelector(`.pane[data-pane="${pi}"] .content`);
    if (!content) return;
    const root = content.querySelector(":scope > ul.tree");
    if (root) {
      // `dir=` (empty rel) asks tree_level for the project root's children
      // in the same <li>-only shape used for every lazy subdirectory fetch.
      fetch(`/frag/${PROJECT}/tree?dir=`).then((r) => r.text()).then((html) => reconcileList(root, html));
    }
    content.querySelectorAll("details[open][data-rel]").forEach((d) => {
      const rel = d.dataset.rel;
      const ul = d.querySelector(":scope > ul");
      if (!ul) return;
      const url = `/frag/${PROJECT}/tree?dir=${encodeURIComponent(rel)}`;
      fetch(url).then((r) => r.text()).then((html) => reconcileList(ul, html));
    });
  });
}

// Wires the file <a> elements only — no container-level oncontextmenu.
// reconcileList calls this (not wireFragment) on the <ul> it just merged:
// a `ul`/`details` oncontextmenu handler doesn't stop propagation, so
// assigning one at every reconciled nesting level would make a blank-space
// right-click inside a refreshed subdirectory bubble through each level's
// own handler in turn and pop fileMenu's prompt() more than once. The single
// container handler wireFragment sets once, at the pane's outer `.content`
// mount, already catches blank clicks anywhere inside via bubbling.
function wireFileLinks(root) {
  // Any anchor carrying data-rel, not just tree rows: markdown previews emit
  // <a class="mdlink" data-rel> for links to project files, and they want the
  // identical open-as-tab and context-menu behaviour. A no-op for existing
  // markup — every data-rel anchor rendered today already has class="file"
  // (render.rs tree_level, changes_fragment) — and tree <details data-rel>
  // stays excluded because the selector still requires an `a`.
  root.querySelectorAll("a[data-rel]").forEach((a) => {
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
        tab: isDiff ? { k: "Diff", rel: rel || null } : { k: "File", rel, mode: defaultMode(rel) },
      });
    };
    a.oncontextmenu = (e) => { e.preventDefault(); fileMenu(e, a.dataset.rel); };
  });
}

function wireFragment(content) {
  wireFileLinks(content);
  // right-clicking blank space in a tree targets the project root
  content.oncontextmenu = (e) => {
    if (e.target.closest("a[data-rel]")) return;
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

/// Re-mounts a previewed file after it changed on disk.
///
/// By `rel`, not by kind like `refreshKind`: several panes can show several
/// files, and re-fetching all of them because one changed would throw away
/// the scroll position of panes that did not. Edit mode is deliberately not
/// here — an editor follows the file through `BufferText`, which preserves
/// the buffer's own state machine around dirty and stale.
function refreshFile(rel) {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (active && active.k === "File" && active.mode === "Preview" && active.rel === rel) {
      mountTab(document.querySelector(`.pane[data-pane="${pi}"] .content`), active);
    }
  });
}

function openUrl(u) {
  if (!SAFE_URL.test(u)) return;
  // noopener,noreferrer: an opened page gets no handle back to the workspace.
  window.open(u, "_blank", "noopener,noreferrer");
}

// A link at the end of a sentence, or inside prose parentheses, must not
// swallow the punctuation. A parenthesised segment *within* the link survives,
// which is why the bracket trim counts rather than strips.
//
// Applied to BOTH matchers, and at match time rather than at activate time.
// It began as a URL-only helper called from `activate`, which was wrong twice
// over: `PATH_RE`'s last segment is `[\w.@+-]+` and `.` is in that class, so a
// path written at the end of a sentence absorbed the full stop and then failed
// to resolve — reported from real use, as `couldn't open …design.md.` — and
// because the trim ran after matching, the underline covered the punctuation
// even for the URLs it did handle. Trimming here fixes the range and the text
// together, for both.
//
// Only `.` can actually reach this from `PATH_RE` (a comma, semicolon or
// bracket is outside its character class and was never matched), so for paths
// this is precisely a full-stop trim. The cost is that a file genuinely named
// `foo.` cannot be opened by clicking it; sentence-final paths are constant
// and filenames ending in a dot are pathological, so that trade is not close.
function trimTail(s) {
  s = s.replace(/[.,;:!?'"]+$/, "");
  while (
    s.endsWith(")") &&
    (s.match(/\(/g) || []).length < (s.match(/\)/g) || []).length
  ) {
    s = s.slice(0, -1);
  }
  return s;
}

// The click that was sent, so a refusal can be shown in the terminal it came
// from. A single slot, not a map: only one link can be clicked at a time, and
// a map keyed by text would grow for the life of the page.
let pendingLink = null;

function openTermPath(entry, raw) {
  pendingLink = { entry, text: raw };
  // The line number is matched so the whole reference underlines, then dropped
  // — the viewer has no line addressing to spend it on. Saying so is the only
  // thing between "we ignored part of what you clicked" and silence.
  const line = raw.match(/:(\d+)(?::\d+)?$/);
  if (line) termFlash(entry, `line ${line[1]} — opening file`);
  // Verbatim. The client does no parsing; resolution and confinement are one
  // function in projects.rs.
  send({ t: "OpenPath", text: raw });
}

// One provider per pattern, URL registered first. Where the two overlap — the
// path-looking tail of a URL — xterm resolves it by provider index in
// _removeIntersectingLinks, so registration order is the entire mechanism.
// xterm's own OscLinkProvider is registered at construction, ahead of both,
// which is the ordering this wants for free: a link an application declared
// beats anything resh would have guessed over the same cells.
let warnedProvider = false;

function matchProvider(term, re, activate) {
  return {
    provideLinks(y, cb) {
      // Nothing may escape here. _askForLink runs the providers in a loop
      // straight out of the mousemove listener, so a throw from this one
      // skips every provider after it for that event — the same shape as the
      // panic-in-a-socket-thread rule this project holds server-side. Warned
      // once, not per pointer move, or a broken matcher floods the console.
      try {
        // The gate. No link exists to hover, so nothing underlines and
        // nothing can be clicked — rather than a link that exists and refuses.
        if (!linksArmed) return cb(undefined);
        // y is 1-based and absolute: xterm adds buffer.ydisp to the hovered
        // row before asking, so this indexes the scrollback too, not the
        // viewport.
        const line = term.buffer.active.getLine(y - 1);
        if (!line) return cb(undefined);
        const text = line.translateToString(true);
        const out = [];
        re.lastIndex = 0;
        for (let m; (m = re.exec(text)); ) {
          // Trimmed here, not in `activate`, so the range shrinks with the
          // text — otherwise the underline reaches over the sentence's own
          // punctuation. `re.lastIndex` deliberately still points past the
          // untrimmed match, so the loop advances exactly as before and a
          // trim can never re-feed its own tail.
          const raw = trimTail(m[0]);
          if (!raw) continue;
          out.push({
            range: {
              start: { x: m.index + 1, y },
              end: { x: m.index + raw.length, y },
            },
            text: raw,
            // Re-checked at click time: an underline left stale by a missed
            // keyup — alt-tabbing away while holding the key — must not open
            // anything.
            activate: (ev) => {
              if (linkModifier(ev)) activate(raw, ev);
            },
          });
        }
        cb(out.length ? out : undefined);
      } catch (e) {
        if (!warnedProvider) { warnedProvider = true; console.warn("resh: link provider failed:", e); }
        cb(undefined);
      }
    },
  };
}

function registerTermLinks(term, entry) {
  // Built as one ordered list and registered from it, so the order xterm sees
  // and the order a reader (or a test) sees cannot drift apart.
  const providers = [
    matchProvider(term, URL_RE, (raw) => openUrl(raw)),
    matchProvider(term, PATH_RE, (raw) => openTermPath(entry, raw)),
  ];
  for (const p of providers) term.registerLinkProvider(p);
  // Kept on the entry for tests/browser/termlinks.mjs, which has no other way
  // in: xterm exposes no API for enumerating registered link providers, and
  // the alternative — asserting on rendered underline styling — would couple
  // the test to renderer internals that differ between the DOM and canvas
  // renderers. Not dead code; deleting it blinds the only test of the gate.
  entry.linkProviders = providers;
}

// Arming has to nudge xterm to ask again: the Linkifier caches the last cell
// it resolved (_lastBufferCell) and will not re-ask for the same position, so
// a bare re-dispatch at the current spot is ignored. Moving the pointer
// through somewhere else first invalidates that cache using nothing but
// public events.
//
// If this proves unreliable, the graceful degradation is that arming takes
// effect on the next real pointer movement, which is what a user holding a
// modifier is about to do anyway.
let lastPointer = null;
addEventListener("mousemove", (e) => { lastPointer = { x: e.clientX, y: e.clientY }; }, true);

function nudgeLinks() {
  if (!lastPointer) return;
  const el = document.elementFromPoint(lastPointer.x, lastPointer.y);
  const host = el && el.closest && el.closest(".termhost");
  if (!host) return;
  // Dispatched on .xterm-screen rather than on the host, because that is the
  // element xterm hands its Linkifier and therefore the only one carrying the
  // mousemove listener. It is a *descendant* of the host, so a bubbling event
  // dispatched on the host travels the other way and never arrives: measured,
  // and the provider call count did not move at all.
  const screen = host.querySelector(".xterm-screen");
  if (!screen) return;
  const r = screen.getBoundingClientRect();
  // A different *cell* is not enough, and this was measured rather than
  // assumed: within one line the Linkifier answers from its cached provider
  // replies (_askForLink's useLinkCache branch, taken whenever _activeLine
  // still matches), so a sideways nudge asks nobody anything. Only a change of
  // line makes it re-ask. Hence the row height, derived from the terminal's
  // real row count rather than a guessed constant — a wrong height can land
  // the synthetic move back on the same line and quietly do nothing.
  const entry = terms.get(host.dataset.session);
  const rows = (entry && entry.term.rows) || 24;
  const rowH = Math.max(1, r.height / rows);
  const away = {
    x: lastPointer.x,
    y: lastPointer.y - r.top > rowH ? r.top + rowH / 2 : r.top + rowH * 1.5,
  };
  for (const p of [away, lastPointer]) {
    // bubbles: false, and this is load-bearing rather than tidiness. When an
    // application turns on mouse motion reporting (mode 1003), xterm's own
    // bindMouse attaches a mousemove listener to `.xterm` — the PARENT of the
    // element below — which forwards any event with no buttons held to the
    // PTY. A synthetic MouseEvent has buttons === 0, so a bubbling nudge
    // reports four phantom motions per Ctrl chord, two of them at the detour
    // row, and a TUI's hover highlight jumps away and back every time the
    // user reaches for the modifier. It also keeps the nudge away from the
    // selection service's document-level listeners.
    //
    // The Linkifier still sees it: its listener is bound directly on
    // .xterm-screen, and composedPath() still returns the full ancestor chain
    // for a non-bubbling event, so its .xterm-hover guard keeps working. So
    // does the window-level capture listener that maintains lastPointer.
    screen.dispatchEvent(
      new MouseEvent("mousemove", { clientX: p.x, clientY: p.y, bubbles: false }),
    );
  }
}

function setArmed(on) {
  if (linksArmed === on) return;
  linksArmed = on;
  nudgeLinks();
}

addEventListener("keydown", (e) => { if (linkModifier(e)) setArmed(true); });
addEventListener("keyup", (e) => { if (!linkModifier(e)) setArmed(false); });
addEventListener("blur", () => setArmed(false));

function ensureTerm(session) {
  // No "the socket died, rebuild it" branch any more: an entry now heals its
  // own socket (see connectTerm), so a caller cannot find a dead one here.
  // Rebuilding on the way past was also what made recovery depend on the
  // user happening to switch tabs — render() skips remounting a tab that is
  // still active, so a terminal whose socket died under it stayed dead for
  // exactly as long as the user kept looking at it.
  const existing = terms.get(session);
  if (existing) return existing;
  const node = document.createElement("div");
  node.className = "termhost";
  node.dataset.session = session;
  pool().appendChild(node);
  // xterm's own defaults are black-on-white-ish and take no part in the theme
  // cascade, so the active theme's variables are read off :root and handed to
  // it — otherwise the terminal is a black rectangle inside a #1e1f22 pane.
  const css = getComputedStyle(document.documentElement);
  const v = (name, fallback) => css.getPropertyValue(name).trim() || fallback;
  const term = new Terminal({
    convertEol: false,
    fontSize: 13,
    fontFamily: v("--mono", "ui-monospace, Menlo, monospace"),
    theme: {
      background: v("--bg", "#1e1f22"),
      foreground: v("--fg", "#dfe1e5"),
      cursor: v("--accent", "#548af7"),
      cursorAccent: v("--bg", "#1e1f22"),
      selectionBackground: v("--sel-bg", "#2e436e"),
    },
    // xterm already registers an OscLinkProvider, so OSC 8 sequences are
    // parsed and their ranges tracked; this option is the only thing missing,
    // and it defaults to null. Not gated on the modifier, unlike the matchers:
    // the application said in a control sequence that these cells are a link,
    // so there is no guess to protect the user from.
    //
    // The destination is still scheme-checked in openUrl. What is running
    // chooses it, which makes it exactly as trustworthy as plain text.
    // This vendored OscLinkProvider happens to drop non-http(s) URIs before
    // offering them, so today that check is the second of two — but it is
    // the only one resh owns, and it is what covers every other link route
    // in this file as well.
    linkHandler: {
      activate: (ev, uri) => openUrl(uri),
    },
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(node);
  const entry = { node, term, fit, sock: null, tries: 0, timer: null, attached: false, gone: false,
                  selTimer: null, flashTimer: null, linkProviders: null };
  registerTermLinks(term, entry);
  // Copy on select. xterm's rows are `user-select: none`, so a browser
  // selection over terminal text is impossible and xterm's own selection is
  // the only route to the clipboard — reached, until now, only by the
  // browser's own copy command (Cmd+C, right-click → Copy). A full-screen app
  // takes that away the moment it turns on mouse reporting: the right button
  // goes to the app, context menu and all. Selecting in a terminal running
  // Claude Code therefore copied nothing, silently, and the next paste
  // inserted whatever the clipboard still held from before — an image, if that
  // is what was last copied, which resh's own paste route then dutifully typed
  // into the app.
  //
  // Debounced because onSelectionChange fires continuously while a drag grows;
  // only where it settles is worth writing to the clipboard.
  term.onSelectionChange(() => {
    clearTimeout(entry.selTimer);
    entry.selTimer = setTimeout(() => copySelection(entry), 200);
  });
  // OSC 52 is how an application copies on the user's behalf, and it is the
  // half of copying that a selection handler cannot reach: when a full-screen
  // app owns the mouse, the drag never reaches xterm at all, so the app makes
  // the selection itself and sends the text out as `ESC ] 52 ; c ; <base64>`.
  // Claude Code does exactly this — it even says "sent 13 chars via OSC 52" —
  // and xterm.js registers no handler for 52, so until now those bytes went
  // nowhere and the clipboard silently kept whatever it held.
  //
  // Writes only. The query form (`ESC ] 52 ; c ; ?`) asks the terminal to send
  // the clipboard *back* to the application, which would let anything with a
  // shell — or any file someone cats — read what the user last copied. There
  // is no version of that this wants.
  term.parser.registerOscHandler(52, (payload) => {
    const body = payload.slice(payload.indexOf(";") + 1);
    // Refused explicitly rather than left to fall through the base64 decode
    // below, which would also reject it. Something this consequential should
    // be unmistakable at the point a reader looks for it.
    if (body === "?") return true;
    if (body.length > MAX_OSC52_B64) {
      termFlash(entry, "copy too large");
      return true;
    }
    let text;
    try {
      text = new TextDecoder().decode(Uint8Array.from(atob(body), (c) => c.charCodeAt(0)));
    } catch {
      return true; // not base64: not ours to guess at
    }
    if (!text) return true;
    if (!navigator.clipboard) { termFlash(entry, "copy needs https"); return true; }
    navigator.clipboard.writeText(text).then(
      () => termFlash(entry, `copied ${text.length}`),
      () => termFlash(entry, "copy blocked"),
    );
    return true;
  });
  // Shift+Enter writes a newline instead of submitting. xterm sends a bare
  // \r for Enter and for Shift+Enter alike, so an application cannot tell
  // the two apart — which is why Claude Code submits on Shift+Enter here and
  // `\` + Enter was the only way to write a second line.
  //
  // LF, not the ESC CR that a terminal's own Shift+Enter binding usually
  // sends (iTerm2's, via Claude's /terminal-setup). Claude binds its
  // chat:newline action to Ctrl+J, and Ctrl+J *is* LF, so this needs no
  // knowledge of what is running: everything that does not deliberately
  // distinguish LF from CR treats them alike — readline runs the line either
  // way, so Shift+Enter still submits at a shell prompt, vim breaks the line,
  // a pager scrolls. ESC CR would have made Shift+Enter a no-op in bash.
  //
  // Routed through term.input so it takes the same onData path as every
  // other keystroke, including its dead-socket guard.
  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown" || e.key !== "Enter") return true;
    if (!e.shiftKey || e.ctrlKey || e.altKey || e.metaKey) return true;
    // Both: returning false only stops xterm from handling the event, and
    // without preventDefault the browser still delivers it to xterm's hidden
    // textarea, which would insert a line there for the IME path to find.
    e.preventDefault();
    term.input("\n");
    return false;
  });
  term.onData((d) => {
    // Reads entry.sock rather than closing over one socket: a reconnect
    // swaps it, and a closure over the original would spend the rest of the
    // page's life writing into the dead one.
    const s = entry.sock;
    if (s && s.readyState === 1) s.send(new TextEncoder().encode(d));
  });
  terms.set(session, entry);
  connectTerm(entry, session);
  return entry;
}

// The terminal socket is the only one carrying a shell, and it used to be the
// only one that never came back: connectControl retries, this did not. So a
// laptop waking from sleep left every terminal silently swallowing keystrokes
// — onData drops them when the socket isn't OPEN, with no error, no banner
// and no visible difference from a live idle shell — until the user happened
// to switch tabs and back, which is what rebuilt the socket.
function connectTerm(entry, session) {
  const sock = new WebSocket(wsUrl(`/ws/${PROJECT}/term/${session}`));
  sock.binaryType = "arraybuffer";
  entry.sock = sock;
  sock.onmessage = (e) => entry.term.write(new Uint8Array(e.data));
  sock.onopen = () => {
    entry.tries = 0;
    // Every attachment gets the session's whole scrollback replayed
    // (session.rs `attach`) — right for a fresh xterm, wrong for this one,
    // which is already showing that text. Clearing first makes the replay
    // repaint the screen instead of appending up to 1 MB of it twice.
    if (entry.attached) entry.term.reset();
    entry.attached = true;
    // A new attachment carries no geometry server-side and the PTY takes the
    // smallest attached client's, so say ours before anything prints.
    sendResize(entry);
    termStatus(entry, "");
  };
  sock.onclose = (ev) => {
    entry.sock = null;
    if (entry.gone) return; // torn down deliberately; see render()
    // wasClean is the whole discriminator here, and it is load-bearing:
    //   clean   — the server closed this on purpose: the shell exited, or
    //             the handshake was refused. Reconnecting would call
    //             session::attach again, and attach *creates* when absent —
    //             so typing `exit` would silently fork a fresh shell.
    //   unclean — the connection died under us: laptop slept, resh
    //             restarted, network blip. Nothing is wrong with the
    //             session; dtach still holds it. This is the case to heal.
    // The two are otherwise indistinguishable from here — both just stop.
    // Verified against the server rather than assumed: on child exit term.rs
    // delivers a real Close frame, not a bare EOF. integration.rs's
    // child_exit_delivers_a_close_frame_not_a_bare_eof pins that, because if
    // it ever regressed this handler would start respawning killed shells.
    if (ev.wasClean) { termStatus(entry, "session ended"); return; }
    termStatus(entry, "reconnecting…");
    // Capped backoff, and deliberately never gives up: a laptop asleep for
    // eight hours must still find its terminal alive on wake.
    const wait = Math.min(500 * 2 ** entry.tries++, 8000);
    entry.timer = setTimeout(() => connectTerm(entry, session), wait);
  };
}

// A clipboard write is the one thing terminal *output* can do to the machine
// outside the terminal, so it is bounded like everything else that is fed
// attacker-influenced bytes. 100 KB of base64 is far more than any copy a
// person makes and far less than a payload worth worrying about.
const MAX_OSC52_B64 = 100_000;

function copySelection(entry) {
  const text = entry.term.getSelection();
  if (!text) return; // clearing a selection must never clobber the clipboard
  // `navigator.clipboard` exists only in a secure context, so a tailnet IP
  // over plain http has none. That has to become a message rather than an
  // exception thrown inside a selection handler.
  if (!navigator.clipboard) return termFlash(entry, "copy needs https");
  navigator.clipboard.writeText(text).then(
    () => termFlash(entry, `copied ${text.length}`),
    () => termFlash(entry, "copy blocked"),
  );
}

// Feedback is not decoration here. A copy that silently does nothing is the
// exact failure copy-on-select was added to fix, and the clipboard is
// invisible — so every outcome says which one it was, including the failures.
function termFlash(entry, text) {
  const el = entry.node;
  el.removeAttribute("data-flash");
  void el.offsetWidth; // restart the fade rather than let the old one finish
  el.dataset.flash = text;
  clearTimeout(entry.flashTimer);
  entry.flashTimer = setTimeout(() => el.removeAttribute("data-flash"), 1600);
}

// Without this a disconnected terminal looks exactly like a live idle one —
// which is how "it stopped taking input" went unexplained for so long.
function termStatus(entry, text) {
  entry.node.dataset.status = text;
}

function sendResize(e) {
  if (e.sock && e.sock.readyState === 1) e.sock.send(`resize:${e.term.cols}x${e.term.rows}`);
}

/// The extension to highlight this file as, or null to leave it a plain
/// textarea. Code files only: markdown has its own rendered preview and its
/// own reason to be edited as plain text, and plaintext has nothing to colour.
/// The check is hljs's own — an extension it does not know would produce an
/// unhighlighted overlay, which is all cost and no colour.
function codeLanguage(rel, text) {
  if (!window.codeInput || !window.hljs) return null;
  if (text.length > MAX_HIGHLIGHT_BYTES) return null;
  const ext = rel.includes(".") ? rel.split(".").pop().toLowerCase() : "";
  // hljs's own answer, so an extension it cannot highlight never gets an
  // overlay that would paint nothing. Markdown and plaintext are excluded
  // even though hljs knows both: markdown has a rendered preview and is
  // edited as prose, and plaintext has nothing to colour — this is the "code
  // files only" line.
  if (!ext || PLAIN_EXTS.has(ext) || !hljs.getLanguage(ext)) return null;
  return ext;
}

/// Attaches resh's editing behaviour to whichever textarea ends up on screen.
/// Which one that is depends on the file: a plain editor makes its own, while
/// a code editor gets the one <code-input> builds for itself. Registering the
/// wrong one is silent and total — the text still types and still highlights,
/// but no edit is ever sent, nothing autosaves, and ⌘S saves an empty buffer.
///
/// addEventListener, not the `oninput` property, for the same reason:
/// code-input listens on its own textarea, and two `.oninput` assignments
/// would leave whichever ran second holding the only handler.
function wireEditor(ta, rel) {
  editors.set(rel, ta);
  ta.spellcheck = false;
  ta.addEventListener("input", () => {
    // texts must reflect what's on screen the instant it changes, not 200ms
    // later when the debounced EditBuffer actually goes out: render() can
    // re-mount this same rel (a pane switch and back, a BufferStale patch,
    // etc.) at any time in between, and mountEditor always seeds from
    // texts. Updating it here — not in the BufferText handler, which only
    // ever hears about this client's own edit as an echo it discards — is
    // what makes texts an accurate record of the user's current text
    // instead of the last thing the server confirmed.
    texts.set(rel, ta.value);
    clearTimeout(pendingEdits.get(rel));
    pendingEdits.set(rel, setTimeout(() => {
      pendingEdits.delete(rel);
      sentEdits.add(rel);
      send({ t: "EditBuffer", rel, text: ta.value });
    }, 200));
    // Restarted on every keystroke, so it measures the pause in typing
    // rather than the age of the edit.
    clearTimeout(autosaveTimers.get(rel));
    autosaveTimers.set(rel, setTimeout(() => autosaveNow(rel), AUTOSAVE_MS));
  });
  // Clicking a tab, the tree, or a terminal blurs the textarea, so this is
  // what covers "moved on within the page" — the timer covers pauses, and
  // window blur covers leaving the browser.
  ta.addEventListener("blur", () => autosaveNow(rel));
  return ta;
}

function mountEditor(content, rel) {
  // The server reads the file itself the moment this rel enters Edit mode
  // (SetMode/OpenTab, see hub.rs) and pushes the content as a BufferText
  // with an empty origin, landing in `texts` — there is no client-side
  // fetch here. If that push hasn't arrived yet, the editor starts empty
  // and the BufferText handler in onEvent fills it in as soon as it does.
  const text = texts.has(rel) ? texts.get(rel) : "";
  // The same breadcrumb the preview fragments carry, so a file looks like the
  // same file in both modes. Built here rather than in render.rs because edit
  // mode has no server fragment at all: the textarea is client-built and
  // seeded from `texts`.
  const wrap = document.createElement("div");
  wrap.className = "editwrap";
  const bar = document.createElement("div");
  bar.className = "path";
  const name = document.createElement("span");
  name.className = "rel";
  name.textContent = rel; // textContent, not innerHTML: a path is user data
  const st = document.createElement("span");
  st.className = "savestate";
  const btn = document.createElement("button");
  btn.className = "savebtn";
  btn.textContent = "Save";
  btn.title = "write this file out (⌘S / ctrl-S)";
  btn.onclick = () => saveNow(rel);
  bar.append(name, st, btn);
  const lang = codeLanguage(rel, text);
  if (lang) {
    const host = document.createElement("code-input");
    host.setAttribute("template", "hl");
    host.setAttribute("language", lang);
    wrap.append(bar, host);
    content.appendChild(wrap);
    // Set after connecting, never before: with no textarea built yet,
    // code-input's value setter falls back to assigning innerHTML, which
    // would parse a file's own angle brackets as markup and lose them. Once
    // connected it writes the textarea and schedules the first highlight.
    host.value = text;
    const ta = wireEditor(host.querySelector("textarea"), rel);
    // Its own textarea, so it carries none of this app's classes until told.
    ta.classList.add("editor");
  } else {
    const ta = document.createElement("textarea");
    ta.className = "editor";
    ta.value = text;
    wireEditor(ta, rel);
    wrap.append(bar, ta);
    content.appendChild(wrap);
  }
  paintSaveState(content, rel);
}

/// The breadcrumb's right-hand side: what state this buffer is in, and — only
/// where it is the thing that writes the file — a Save button.
function paintSaveState(content, rel) {
  const st = content.querySelector(".savestate");
  const btn = content.querySelector(".savebtn");
  if (!st || !btn) return;
  const meta = state && state.buffers.find((x) => x.rel === rel);
  const paused = !!(meta && (meta.stale || autosavePaused.has(rel)));
  st.className = "savestate" + (paused ? " warn" : "");
  if (paused) {
    st.textContent = "not saved · changed on disk";
    st.title = "this file changed underneath your buffer — save to see what differs";
  } else if (meta && meta.dirty) {
    st.textContent = AUTOSAVE ? "saving…" : "unsaved · ⌘S";
    st.title = AUTOSAVE ? "writing this out" : "press ⌘S (ctrl-S) to save";
  } else {
    st.textContent = "saved";
    st.title = AUTOSAVE ? "saved automatically" : "no unsaved changes";
  }
  // No button where autosave is already writing the file — there the state
  // readout is the whole story. The exception is a buffer autosave has
  // *stopped* writing because the file changed underneath it: saving is then
  // the only route to the conflict diff, and until now only ⌘S reached it.
  btn.hidden = AUTOSAVE && !paused;
}

/// Sends this client's current text for `rel` and cancels the debounce it
/// pre-empts. Without that cancellation the debounced copy lands *after* the
/// save, re-marking the buffer dirty and starting the whole cycle again.
function pushEdit(rel) {
  clearTimeout(pendingEdits.get(rel));
  pendingEdits.delete(rel);
  const ta = editors.get(rel);
  if (ta) {
    sentEdits.add(rel);
    send({ t: "EditBuffer", rel, text: ta.value });
  }
}

/// Writes `rel` out now, if autosave is on and this buffer is still its own
/// business. Deliberately `force: false` — the conflict guard is the whole
/// safety property here, and an autosave that could force is an autosave that
/// can silently overwrite an agent's edit mid-keystroke.
///
/// The dirty check is what keeps a second browser window from re-writing what
/// the first one just saved: the server clears `dirty` in the State it
/// broadcasts after a save, so every other client's timer finds nothing to do.
function autosaveNow(rel) {
  clearTimeout(autosaveTimers.get(rel));
  autosaveTimers.delete(rel);
  if (!AUTOSAVE || autosavePaused.has(rel)) return;
  const meta = state && state.buffers.find((x) => x.rel === rel);
  if (meta && meta.stale) return;
  // sentEdits is the third answer: an edit is on the wire whose State has not
  // come back yet. Without it this guard mistakes that for a clean buffer.
  if (!pendingEdits.has(rel) && !sentEdits.has(rel) && !(meta && meta.dirty)) return;
  pushEdit(rel);
  send({ t: "SaveBuffer", rel, force: false });
}

/// Every mounted editor, written out now. Bound to the events that mean "I am
/// done with this for the moment" — losing focus, switching browser tab,
/// hiding the window — which is the half of autosave a delay alone cannot
/// cover: a timer that has not fired yet loses the last keystrokes of an edit
/// the user has visibly walked away from.
function autosaveAll() {
  for (const rel of editors.keys()) autosaveNow(rel);
}
window.addEventListener("blur", autosaveAll);
document.addEventListener("visibilitychange", () => { if (document.hidden) autosaveAll(); });

/// Which file a save keystroke means. The focused textarea when there is one,
/// and otherwise the file the workspace is actually showing — MIDDLE then
/// RIGHT, the only two panes that hold file tabs (see MOVE_BETWEEN).
///
/// Returns null when nothing editable is open, which is the signal to leave
/// the keystroke to the browser rather than swallow it.
function saveTarget() {
  const el = document.activeElement;
  if (el && el.classList && el.classList.contains("editor")) {
    for (const [rel, ta] of editors) if (ta === el) return rel;
  }
  for (const p of [MIDDLE, RIGHT]) {
    const pane = state && state.panes && state.panes[p];
    const tab = pane && pane.tabs[pane.active];
    if (tab && tab.k === "File" && tab.mode === "Edit" && editors.has(tab.rel)) return tab.rel;
  }
  return null;
}

// Bound on the document, not on the textarea: Cmd/Ctrl-S belongs to the
// browser unless something takes it, so an editor that is merely *open* —
// rather than focused — used to lose the keystroke to Chrome's own save
// dialog. That is not an edge case here: every reconnect and every deploy
// reloads the page and leaves focus on the body, so the first save after one
// silently left the app. Reported twice.
document.addEventListener("keydown", (e) => {
  if (e.altKey || !(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "s") return;
  const rel = saveTarget();
  if (rel === null) return;
  e.preventDefault();
  saveNow(rel);
});

/// The one save path. The shortcut above and the breadcrumb's Save button both
/// come through here, so a save started either way pre-empts the same debounce
/// and surfaces the same conflict.
function saveNow(rel) {
  // The 200ms input debounce may still be pending, so push the text this save
  // is meant to write before asking for the write — otherwise a save typed
  // quickly enough saves the *previous* keystroke's text.
  pushEdit(rel);
  send({ t: "SaveBuffer", rel, force: false });
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
// new visual language just for this. Takes the exact text to show — callers
// that are reporting a failure prepend "Error: " themselves (see showError);
// a success notice like ProjectClosed's session count should not look like one.
function showBanner(text) {
  const box = document.createElement("div");
  box.className = "conflict error-banner";
  const b = document.createElement("b");
  b.textContent = text;
  const dismiss = document.createElement("button");
  dismiss.textContent = "dismiss";
  dismiss.onclick = () => box.remove();
  box.append(b, dismiss);
  document.body.appendChild(box);
  setTimeout(() => box.remove(), 8000);
}

function showError(msg) {
  showBanner(`Error: ${msg}`);
}

// `detach` (alt-click) keeps the old meaning: drop the tab, leave the shell
// running. A plain close ends the session, because a tab that quietly outlives
// its × is how a project accumulates shells nothing can reach — there is no
// session list, and the per-project cap is 16.
function closeTab(pi, ti, t, detach) {
  const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
  if (meta && meta.dirty && !confirm(`${t.rel} has unsaved changes. Close it?`)) return;
  if (t.k === "Terminal" && !detach) {
    if (!confirm(`End session "${t.session}"? This kills the shell and anything running in it.`)) return;
    send({ t: "EndSession", session: t.session });
    return;
  }
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

// "3 sessions" isn't enough to judge whether one of them is a long-running
// job worth checking on first — list them by name. Dirty buffers are
// checked client-side too (not just left to the server's own CloseRefused)
// so the intent is never even sent when it's certain to be refused: the
// user gets one clear message instead of a round trip that undoes nothing.
const closeBtn = document.getElementById("closeproj");
if (closeBtn) closeBtn.onclick = () => {
  const live = (state && state.live_sessions) || [];
  const dirty = ((state && state.buffers) || []).filter((b) => b.dirty).map((b) => b.rel);
  let msg = `Close ${PROJECT}?\n\n`;
  msg += live.length
    ? `${live.length} terminal session(s) will be ended:\n  ${live.join(", ")}\n`
    : "No terminal sessions are running.\n";
  if (dirty.length) {
    // Mirrors the server's own refusal, but told before sending anything
    // rather than after a round trip that would have changed nothing.
    alert(msg + `\nUnsaved changes in:\n  ${dirty.join("\n  ")}\n\nSave or discard them first.`);
    return;
  }
  if (confirm(msg + "\nEnd sessions?")) send({ t: "CloseProject" });
};

// The projects popup, mirroring the bell/noticepanel pair below. It used to be
// an always-visible header strip, which cost width in every workspace for a
// question ("what else is running?") that is asked occasionally and is already
// answered by the front page. The bell covers the one thing you do want
// mid-task — what needs attention — so this became on-demand.
const projBtn = document.getElementById("projbtn");
const projPanel = document.getElementById("projpanel");
if (projBtn && projPanel) {
  projBtn.onclick = () => {
    projPanel.hidden = !projPanel.hidden;
    if (!projPanel.hidden && window.htmx) htmx.trigger(document.body, "refresh");
  };
  // Clicking through to a project should not leave the panel hanging open
  // behind the tab switch.
  projPanel.onclick = (e) => { if (e.target.closest("a")) projPanel.hidden = true; };
  // The badge is the count of RUNNING projects — the panel's whole subject —
  // recomputed whenever htmx swaps a fresh fragment in, since the fragment is
  // server-rendered and the client never builds these rows itself.
  document.body.addEventListener("htmx:afterSwap", (e) => {
    if (e.target && e.target.id === "projstrip") {
      const n = projPanel.querySelectorAll(".proj.live").length;
      const badge = document.getElementById("projcount");
      if (badge) badge.textContent = n ? String(n) : "";
    }
  });
}

const bell = document.getElementById("bell");
if (bell) {
  bell.onclick = () => {
    const p = document.getElementById("noticepanel");
    p.hidden = !p.hidden;
    renderNotices();
  };
}
setFavicon(false);

// A notification click can land on a cold load; consume the fragment once and
// clear it so a later reload does not re-focus.
if (location.hash.startsWith("#session=")) {
  const want = decodeURIComponent(location.hash.slice("#session=".length));
  history.replaceState(null, "", location.pathname);
  // Bounded: if the socket never connects there is nothing to focus, and an
  // uncapped poll would spin for the life of the page.
  let tries = 0;
  const tryFocus = () => {
    if (state) focusSession(want);
    else if (++tries < 50) setTimeout(tryFocus, 100);
  };
  tryFocus();
}

connectControl();

// ---- notifications ----------------------------------------------------
// Notices are machine-wide, so this list spans projects; only the tab dot
// below is scoped to what is on screen.
let notices = [];
let swReg = null;
const baseTitle = document.title;

// A secure context is required for both service workers and the Notification
// API. localhost and `tailscale serve` HTTPS qualify; plain http:// to a
// tailnet IP does not — there the panel still works and OS notifications
// simply are not offered.
const canNotify = () => window.isSecureContext && "Notification" in window;

if (canNotify() && "serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").then(
    (r) => { swReg = r; },
    (e) => console.warn("resh: service worker registration failed", e)
  );
  navigator.serviceWorker.addEventListener("message", (e) => {
    if (e.data && e.data.kind === "focus") focusSession(e.data.session);
  });
}

function unread() { return notices.filter((n) => !n.read).length; }

function renderNotices() {
  const n = unread();
  const count = document.getElementById("bellcount");
  if (count) count.textContent = n ? String(n) : "";
  // The only cue that works from a background tab with no permission granted.
  document.title = n ? `(${n}) ${baseTitle}` : baseTitle;
  setFavicon(n > 0);

  const panel = document.getElementById("noticepanel");
  if (!panel || panel.hidden) return;
  panel.replaceChildren();
  if (!notices.length) {
    const empty = document.createElement("div");
    empty.className = "notice-empty";
    empty.textContent = "no notifications";
    panel.appendChild(empty);
  }
  for (const x of [...notices].reverse()) {
    const row = document.createElement("div");
    row.className = "notice" + (x.read ? " read" : "");
    const who = document.createElement("span");
    who.className = "notice-who";
    // Attribution is server truth; the message text is not. Both go in as
    // textContent regardless.
    who.textContent = `${x.project} · ${x.session}`;
    const title = document.createElement("span");
    title.className = "notice-title";
    title.textContent = x.title;
    const body = document.createElement("span");
    body.className = "notice-body";
    body.textContent = x.body;
    const when = document.createElement("span");
    when.className = "notice-when";
    when.textContent = ago(x.at);
    row.append(who, title, body, when);
    row.onclick = () => openNotice(x);
    panel.appendChild(row);
  }
  const foot = document.createElement("div");
  foot.className = "notice-foot";
  const markAll = document.createElement("button");
  markAll.textContent = "Mark all read";
  markAll.onclick = (e) => { e.stopPropagation(); send({ t: "MarkAllNoticesRead" }); };
  foot.appendChild(markAll);
  const clear = document.createElement("button");
  clear.textContent = "Clear";
  clear.onclick = (e) => { e.stopPropagation(); send({ t: "ClearNotices" }); };
  foot.appendChild(clear);
  if (canNotify() && Notification.permission === "denied") {
    // Browsers never re-prompt after an explicit denial, so offering the
    // same "Enable" button here would be a silent no-op on click — the spec
    // requires saying which of the two situations (denied vs. no secure
    // context) this is, not failing silently either way.
    const s = document.createElement("span");
    s.textContent = "OS notifications are blocked for this site — re-enable them in your browser's site settings";
    foot.appendChild(s);
  } else if (canNotify() && Notification.permission !== "granted") {
    const b = document.createElement("button");
    b.textContent = "Enable OS notifications";
    // Requested from a click, never on load: browsers penalise spontaneous
    // permission prompts, and an unprompted one is worse than none.
    b.onclick = (e) => { e.stopPropagation(); Notification.requestPermission().then(renderNotices); };
    foot.appendChild(b);
  } else if (!canNotify()) {
    const s = document.createElement("span");
    s.textContent = "OS notifications need a secure context (https or localhost)";
    foot.appendChild(s);
  }
  panel.appendChild(foot);
}

function ago(secs) {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - secs);
  if (d < 60) return `${d}s`;
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h`;
  return `${Math.floor(d / 86400)}d`;
}

// A badged favicon, drawn rather than shipped as a second asset so it follows
// whatever the page's icon already is.
function setFavicon(badged) {
  let link = document.querySelector("link#dlfav");
  if (!link) {
    link = document.createElement("link");
    link.id = "dlfav";
    link.rel = "icon";
    document.head.appendChild(link);
  }
  const svg = badged
    ? `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><text y="13" font-size="13">◆</text><circle cx="12.5" cy="3.5" r="3.5" fill="#e5534b"/></svg>`
    : `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><text y="13" font-size="13">◆</text></svg>`;
  link.href = "data:image/svg+xml," + encodeURIComponent(svg);
}

// Mirrors http::percent_encode on the Rust side: encode each segment but keep
// the `/` separators, because a project may be a nested rel path like
// `karpie/src` whose slashes are structural. Encoding the whole string breaks
// nested projects; encoding none of it breaks any project whose directory name
// contains a URL-significant character — a directory called `foo#bar` would
// send the browser to `/foo` with the rest swallowed as a fragment.
const projectPath = (p) => p.split("/").map(encodeURIComponent).join("/");

function openNotice(x) {
  if (!x.read) send({ t: "MarkNoticeRead", id: x.id });
  if (x.project !== PROJECT) {
    location.href = `/${projectPath(x.project)}#session=${encodeURIComponent(x.session)}`;
    return;
  }
  focusSession(x.session);
}

// Marks every unread notice for one session read. Used wherever a route
// lands on a session — a tab-strip click, an in-page notice click, an OS
// notification click (the SW's "focus" message), or a cold #session= load —
// so "cleared when that tab becomes active" (the spec's words for the dot)
// holds no matter which of those gestures got you there. Always scoped to
// PROJECT: the SW only posts "focus" to a window already on that project,
// and the cold-load path is by definition this page's own project.
function markSessionNoticesRead(session) {
  for (const n of notices) {
    if (!n.read && n.project === PROJECT && n.session === session) send({ t: "MarkNoticeRead", id: n.id });
  }
}

// Activate the terminal tab for `session`, opening it if it is not on screen.
// Both paths are ordinary intents, so every connected client follows.
function focusSession(session) {
  if (!session || !SESSION_RE.test(session) || !state) return;
  markSessionNoticesRead(session);
  for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal" && t.session === session);
    if (ti >= 0) {
      send({ t: "ActivateTab", pane: pi, idx: ti });
      return;
    }
  }
  send({ t: "OpenTab", pane: 3, tab: { k: "Terminal", session } });
}

// The tab-strip dot for a session in THIS project: derived from `notices`
// rather than maintained as a separate set, so it can never drift from the
// server's read state in either direction (a dot that never lights because
// nothing ever adds to a separate set, or one that never clears because
// nothing ever removes from it — both were real bugs here). A session in
// another project has no tab here; that is what the bell badge is for.
function hasAttention(session) {
  return notices.some((n) => n.project === PROJECT && n.session === session && !n.read);
}

function onNotice(n) {
  notices.push(n);
  if (canNotify() && Notification.permission === "granted") {
    if (swReg) swReg.active && swReg.active.postMessage({ kind: "notify", notice: n });
    // Fallback when there's no service worker: same attribution rule as
    // sw.js — project/session (server truth) in the title, payload text in
    // the body — so a hostile payload cannot forge another project's banner.
    else new Notification(`${n.project} · ${n.session}`, { body: `${n.title} — ${n.body}`, tag: `${n.project}/${n.session}` });
  }
  renderNotices();
  render();
}

// ---------------------------------------------------------------------------
// Uploads: files dropped or pasted onto the tree, and images pasted onto a
// terminal. Delegated at document level, not bound per row: the tree is an htmx
// fragment replaced wholesale on TreeChanged, and an upload triggers exactly
// that — per-row listeners would not survive their own first success.
const MAX_UPLOAD_PARTS = 16; // must match config::MAX_UPLOAD_PARTS

// The destination for a drop: the nearest row carrying a data-rel. A directory
// row contributes itself, a file row its parent. Null means the drop was not on
// the tree at all, which is what keeps the destination unambiguous and is why
// this needs no confirmation dialog.
function dropDir(target) {
  const el = target && target.closest && target.closest("[data-rel]");
  if (!el) return null;
  const rel = el.dataset.rel;
  if (el.tagName === "DETAILS") return rel;
  return rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
}

// Where a drop should land, widened from the row to the whole Files pane.
// Rows are a small target and the gaps between them are large, so resolving
// only on `[data-rel]` meant most of the pane silently fell through to the
// browser, which navigates to file:/// and throws the workspace away. Pane
// whitespace is the project root — the same thing the tree's own top level is.
function uploadTargetDir(target) {
  const row = dropDir(target);
  if (row !== null) return row;
  const pane = target && target.closest && target.closest(".pane");
  if (pane && pane.querySelector("ul.tree")) return "";
  return null;
}

// True when the drag carries files, as opposed to text being moved inside the
// editor's textarea. Only file drags are intercepted, or dragging a selection
// within a buffer would stop working.
function dragHasFiles(dt) {
  return !!dt && Array.prototype.includes.call(dt.types || [], "Files");
}

function focusedSession() {
  const host = document.activeElement && document.activeElement.closest(".termhost");
  return host ? host.dataset.session : null;
}

// The session a drag is over, so an image can be dropped straight onto the
// terminal it is meant for rather than routed through the file tree.
function sessionUnder(target) {
  const host = target && target.closest && target.closest(".termhost");
  return host ? host.dataset.session : null;
}

function firstImage(files) {
  return [...files].find((f) => f.type.startsWith("image/")) || null;
}

// One reusable banner rather than showBanner's transient ones, because progress
// has to be updated in place and then cleared.
function setUploadProgress(label, fraction) {
  let box = document.getElementById("uploadprogress");
  if (fraction === null) {
    if (box) box.remove();
    return;
  }
  if (!box) {
    box = document.createElement("div");
    box.id = "uploadprogress";
    box.className = "conflict";
    document.body.appendChild(box);
  }
  box.textContent = `${label} — ${Math.round(fraction * 100)}%`;
}

function postFiles(url, files, label) {
  const form = new FormData();
  for (const f of files) form.append("file", f, f.name);
  const xhr = new XMLHttpRequest();
  xhr.open("POST", url);
  // XHR rather than fetch: fetch exposes no upload progress, and a 100 MB send
  // with no feedback is indistinguishable from a hang.
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) setUploadProgress(label, e.loaded / e.total);
  };
  xhr.onload = () => {
    setUploadProgress(label, null);
    if (xhr.status !== 200) return showError(`${label}: ${xhr.responseText || xhr.status}`);
    let body = {};
    try { body = JSON.parse(xhr.responseText); } catch { return; }
    for (const r of body.results || []) if (!r.ok) showError(`${r.name}: ${r.error}`);
  };
  xhr.onerror = () => { setUploadProgress(label, null); showError(`${label}: upload failed`); };
  xhr.send(form);
}

// File.size and FileList.length come from the OS and are readable before a byte
// is sent, so the part cap is checked at drop time. A courtesy, not the
// enforcement — the server applies both caps while streaming regardless.
function tooManyFiles(files) {
  if (files.length > MAX_UPLOAD_PARTS) {
    return `${files.length} files at once (limit ${MAX_UPLOAD_PARTS}) — use git or scp to move a project`;
  }
  return null;
}

function uploadFiles(files, dir) {
  const refusal = tooManyFiles(files);
  if (refusal) return showError(refusal);
  const q = dir ? `?dir=${dir.split("/").map(encodeURIComponent).join("/")}` : "";
  postFiles(`/upload/${PROJECT}${q}`, files, `upload to ${dir || "project root"}`);
}

// A dropped directory arrives as a zero-length entry that fails on read, so a
// size check would send a mystery empty part and surface a confusing server
// error. webkitGetAsEntry is the reliable test; directories are a non-goal, so
// say so at the drop, by name.
function droppedDirectories(dt) {
  const dirs = [];
  for (const item of dt.items || []) {
    const entry = item.webkitGetAsEntry && item.webkitGetAsEntry();
    if (entry && entry.isDirectory) dirs.push(entry.name);
  }
  return dirs;
}

// preventDefault on *every* file drag, not just ones over a valid target.
// Without it the browser handles the drop itself and navigates to file:///,
// which throws away the workspace — and it did so for every pixel that was not
// exactly a tree row, which is most of the window. Refusing a misplaced drop
// out loud is the whole point; navigating away is never the right answer.
document.addEventListener("dragover", (e) => {
  if (dragHasFiles(e.dataTransfer)) e.preventDefault();
});

document.addEventListener("drop", (e) => {
  if (!dragHasFiles(e.dataTransfer)) return;
  e.preventDefault();

  // An image dropped on a terminal goes to that terminal, the same as pasting
  // one there. Dragging a screenshot straight onto the shell that needs it is
  // the obvious gesture, and routing it through the tree instead would leave a
  // file the user then has to mention by hand.
  const session = sessionUnder(e.target);
  if (session) {
    const img = e.dataTransfer.files.length ? firstImage(e.dataTransfer.files) : null;
    if (img) {
      postFiles(`/paste/${PROJECT}/${session}`, [img], "paste");
      return;
    }
    return showError("only images can be dropped on a terminal — drop other files on the Files pane");
  }

  const dir = uploadTargetDir(e.target);
  if (dir === null) {
    return showError("drop files on the Files pane to upload them");
  }
  const dirs = droppedDirectories(e.dataTransfer);
  if (dirs.length) {
    return showError(`folders are not uploaded (${dirs.join(", ")}) — use git or scp for a directory`);
  }
  if (e.dataTransfer.files.length) uploadFiles(e.dataTransfer.files, dir);
});

// Capture phase, and this is not optional. xterm's own paste handler calls
// stopPropagation() on every paste over a terminal and then reads only
// text/plain — so a bubble-phase listener never runs when a terminal has focus,
// which is exactly where pasting a screenshot needs to work. Capture puts this
// ahead of xterm; anything that is not an image is left completely untouched
// and reaches xterm as before.
document.addEventListener("paste", (e) => {
  const files = e.clipboardData && e.clipboardData.files;
  if (!files || !files.length) return;
  const session = focusedSession();
  if (session) {
    const img = firstImage(files);
    if (!img) return; // not an image: xterm's text paste, untouched
    e.preventDefault();
    e.stopPropagation(); // xterm must not also act on this one
    postFiles(`/paste/${PROJECT}/${session}`, [img], "paste");
    return;
  }
  const dir = uploadTargetDir(document.activeElement) ?? uploadTargetDir(e.target);
  if (dir === null) return;
  e.preventDefault();
  uploadFiles(files, dir);
}, true);
