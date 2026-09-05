// Workspace client. Chrome (tabstrips, tree/changes/file/diff panes) is
// rendered from mirrored server state on every event; terminals are pooled
// DOM nodes that are MOVED between panes with appendChild, never rebuilt —
// rebuilding a xterm instance drops its websocket and detaches the shell.
const PROJECT = document.body.dataset.project;
// The programs a new terminal can be asked to start, by wire name. Decided
// server-side at startup (`launch::probe`): a name is here only if the login
// shell could find it, or the check could not tell — a failed check must not
// hide a working button.
const LAUNCHES = (document.body.dataset.launches || "").split(" ").filter(Boolean);
// A worktree tab opened by "start in a new worktree" arrives with
// ?launch=…, read once at load and validated against LAUNCHES the same way a
// server-sent value would be — a stray or hand-edited query string must not
// send an arbitrary wire name to newTerminal.
const pendingLaunch = (() => {
  const l = new URLSearchParams(location.search).get("launch");
  return l && LAUNCHES.includes(l) ? l : null;
})();
let pendingLaunchSent = false;
// A row clicked on the overview arrives with ?focus=<session>, read once at
// load and validated against the session-name shape the same way a
// server-sent value would be — a stray or hand-edited query string must not
// send an arbitrary string into focusSession.
const pendingFocus = (() => {
  const f = new URLSearchParams(location.search).get("focus");
  return f && /^[A-Za-z0-9_-]{1,32}$/.test(f) ? f : null;   // session-name shape
})();
let pendingFocusDone = false;
// The blank tab opened synchronously on the "new worktree" click, navigated
// when WorktreeReady arrives. Opened on the click because a window.open after
// a websocket round trip is not reliably inside the user gesture.
let pendingTab = null;
// The config file's `show_hidden`, embedded by render.rs at page load. The
// workspace's own toggle (state.show_hidden) overrides it when set; null
// means nobody has touched the header button, so the file still decides.
const SHOW_HIDDEN_DEFAULT = document.body.dataset.showHidden === "1";
// Whether the editor writes a buffer out by itself. Read once per page load,
// like SHOW_HIDDEN_DEFAULT: it changes only when someone edits a config file,
// which already needs a reload to take effect.
const AUTOSAVE = document.body.dataset.autosave === "1";
// Whether the editor's current selection is sent to Claude as ambient
// context, embedded once per page load like AUTOSAVE and SHOW_HIDDEN_DEFAULT.
// Off unless the project's config opted in (Settings::share_selection); the
// server checks this again on every ShareSelection it receives (see
// ide::selection_changed), so this client-side gate is a courtesy that saves
// a wasted round trip, not the actual boundary — the boundary is the server
// re-checking a config file it, not this client, controls.
const SHARE_SELECTION = document.body.dataset.shareSelection === "1";
// How long after the last keystroke an autosave fires. VS Code's default,
// and comfortably longer than the 200ms EditBuffer debounce it depends on.
const AUTOSAVE_MS = 1000;
// Highlighting an editor is worth it up to a point and then it is not: the
// buffer cap is 2 MB, and hljs re-runs over the whole text on every pause in
// typing. Past this the editor stays a plain textarea rather than becoming a
// laggy one. Chosen as roughly the largest source file anyone edits by hand;
// roost's own biggest is a quarter of it.
const MAX_HIGHLIGHT_BYTES = 100_000;
// Known to hljs but deliberately left plain — see codeLanguage.
const PLAIN_EXTS = new Set(["md", "markdown", "txt", "text"]);
// What the non-ASCII indicator lets through: TAB, LF and the printable range.
// Everything else it counts and marks — CR included (a stray one in an LF
// file is exactly the kind of thing worth seeing; a wholly CRLF file lights
// up at every line end and that is the strict reading, chosen on purpose),
// DEL and the other C0 controls, and every code point from 0x80 up: smart
// quotes, en/em dashes, NBSP, zero-width space, BOM, emoji, Cyrillic
// look-alikes. `u` so an astral character counts as one, not two.
const NON_ASCII_RE = /[^\t\n\x20-\x7E]+/gu;
// The highlight toggle's home. One flag for the whole browser, not per file:
// it is a way of looking, like a theme, and someone hunting smart quotes
// wants it on in every prose file they open next.
const NONASCII_KEY = "roost.nonascii";
// code-input wraps a real <textarea> and paints a highlighted <pre> under it,
// which is why it can be dropped in here at all: everything downstream —
// `editors`, the edit debounce, autosave, ⌘S, the conflict patch, blur flush
// — goes on talking to the same textarea it always did. Registered once; the
// element carries template="hl" to select it.
//
// The second template is the same overlay put to a different use: for a
// prose file, where hljs has nothing to colour, it marks non-ASCII runs
// instead. Only the highlight function differs, so everything hledit.mjs
// established about the two layers agreeing holds for it unchanged.
if (window.codeInput && window.hljs) {
  codeInput.registerTemplate("hl", codeInput.templates.hljs(hljs, []));
  // preElementStyled=false, as templates.hljs defaults it: that puts the
  // padding on `pre code`, where style.css and hledit.mjs expect it. True
  // moves it to the <pre>, and the marks land 10px off the glyphs.
  codeInput.registerTemplate("nonascii", new codeInput.Template(markNonAscii, false, false, false, []));
}

/// code-input's highlight hook for the "nonascii" template. It arrives with
/// the value already escaped into `el.innerHTML` and nothing else, so
/// textContent is the raw text; rebuilt from that, escaping every piece
/// ourselves, with each non-ASCII run wrapped. The characters themselves are
/// never substituted for visible stand-ins: this layer must lay out glyph for
/// glyph like the textarea over it, and a `␍` where a CR was would walk the
/// rest of the line off the caret. Visibility is the stylesheet's job.
function markNonAscii(el) {
  const text = el.textContent;
  let html = "", last = 0;
  for (const m of text.matchAll(NON_ASCII_RE)) {
    html += escapeHtml(text.slice(last, m.index)) + `<span class="nonascii">${escapeHtml(m[0])}</span>`;
    last = m.index + m[0].length;
  }
  el.innerHTML = html + escapeHtml(text.slice(last));
}

/// How many characters of `text` fall outside NON_ASCII_RE's allowed set —
/// characters, not runs, so "—" and "——" differ.
function nonAsciiCount(text) {
  let n = 0;
  for (const m of text.matchAll(NON_ASCII_RE)) n += [...m[0]].length;
  return n;
}

function nonAsciiOn() {
  try { return localStorage.getItem(NONASCII_KEY) === "1"; } catch { return false; }
}
function setNonAsciiOn(on) {
  try { if (on) localStorage.setItem(NONASCII_KEY, "1"); else localStorage.removeItem(NONASCII_KEY); } catch {}
}
const showHidden = () => (state && state.show_hidden != null ? state.show_hidden : SHOW_HIDDEN_DEFAULT);
// What the tree was last rendered with, so a State that flips the setting
// re-fetches. Fragments are server-rendered against the workspace value, so
// nothing in the DOM would otherwise change until the next filesystem event.
let treeShownHidden = null;
const SESSION_RE = /^[A-Za-z0-9_-]{1,32}$/; // must match session::valid_name server-side
const DIVIDER_PX = 8; // keep in step with --divider in style.css

// Pane-header and launcher icon set. Constant markup ONLY — nothing here is
// ever interpolated, which is what makes innerHTML safe; anything dynamic
// stays in text nodes and dataset attributes as everywhere else.
const PANE_ICONS = {
  dotsOff: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="5" stroke-dasharray="2.2 2.2"/></svg>',
  dotsOn: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="5" stroke-dasharray="2.2 2.2"/><circle cx="8" cy="8" r="2" fill="currentColor" stroke="none"/></svg>',
  collapse: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M4 10l4-4 4 4"/></svg>',
  move: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 5.5h10l-2.5-2.5M13.5 10.5h-10l2.5 2.5"/></svg>',
  // The two arms are a 180° rotation of each other about the 16x16 centre:
  // a corner bracket at (13.5,2.5) with 4.5-long arms, and its mirror at
  // (2.5,13.5). The bracket and the diagonal that feeds it must share that
  // corner — the bottom-left bracket was drawn at (2.5,11.5) while its
  // diagonal still ended at (2.5,13.5), so the shaft ran straight through a
  // detached arrowhead.
  maximize: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M9 2.5h4.5V7M7 13.5H2.5V9M13.5 2.5L9 7M2.5 13.5L7 9"/></svg>',
  restore: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M13.5 7H9V2.5M2.5 9H7v4.5M9 7l4.5-4.5M7 9l-4.5 4.5"/></svg>',
  newterm: '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"><rect x="1.5" y="3.5" width="10.5" height="9.5" rx="1.2"/><path d="M4 6.8l1.7 1.5L4 9.8M8 10.6h2.6"/><path d="M13.3 2.2v3.6M11.5 4h3.6"/></svg>',
};
// The official Claude mark (lobehub packaging of Anthropic's starburst,
// fetched 2026-08-23 from lobehub/lobe-icons static-svg/icons/claude-color.svg),
// brand-filled in every theme on purpose: the point of the real mark is that
// it is recognisable, so it does not take currentColor.
// The filename stripe's Edit/Preview switch: an eye to look at the rendered
// form, a pencil to edit the text. Constant markup only, like PANE_ICONS.
const MODE_ICONS = {
  preview: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M1.6 8s2.4-4.4 6.4-4.4S14.4 8 14.4 8 12 12.4 8 12.4 1.6 8 1.6 8z"/><circle cx="8" cy="8" r="1.9"/></svg>',
  edit: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M11.2 2.6l2.2 2.2L5.6 12.6l-3 .8.8-3z"/></svg>',
};
const CLAUDE_MARK = '<svg width="13" height="13" viewBox="0 0 24 24" fill="#D97757"><path d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z"/></svg>';

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
// id -> {rel, old_text, new_text} for every proposal this client has seen
// content for. Kept outside `state` itself (which is replaced wholesale by
// every "State" event, and `State` never carries proposal text — see
// hub.rs's `ProposalSides`) and reattached as `state.proposals` right after
// each replacement below, so a tab's content survives every later State
// broadcast instead of vanishing on the next keystroke anywhere in the
// project.
const proposals = {};

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
      // Both: renderClaudeHooks() updates the bell's mark/tooltip even when
      // the notice panel is closed (it reads `state` directly, not the
      // panel's DOM), and renderNotices() is what keeps an *open* panel's
      // hook row in step — it early-returns when the panel is hidden, so
      // the per-keystroke cost (this fires on every debounced EditBuffer
      // broadcast) is nil. Without the renderNotices() call here, an open
      // panel kept a stale row after a confirm elsewhere in this tab or a
      // flip from another browser: the mark and tooltip updated, but the
      // row's "on"/"off" text and its Enable/Disable button did not, since
      // nothing but renderNotices() rebuilds the panel's own children.
      renderClaudeHooks();
      renderNotices();
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
        // A proposal id missing from every pane's tabs was answered,
        // withdrawn, or rejected by closing its tab — same pruning shape as
        // openRels above, so a long session does not accumulate whole file
        // bodies for proposals nobody can act on any more.
        const openProposalIds = new Set();
        for (const p of state.panes) for (const t of p.tabs) if (t.k === "Proposal") openProposalIds.add(t.id);
        for (const id of Object.keys(proposals)) if (!openProposalIds.has(id)) delete proposals[id];
      }
      // Reattached, not copied, on every State: renderProposal reads
      // state.proposals[tab.id], and this is what keeps that name valid
      // after `state` itself was just replaced above.
      state.proposals = proposals;
      render();
      // Toggling visibility changes what the *server* renders, not how the
      // client draws it, so the State that carries the new value has to be
      // followed by a re-fetch. refreshTree (not a remount) keeps whatever
      // the user had expanded.
      if (treeShownHidden !== null && treeShownHidden !== showHidden()) refreshTree();
      treeShownHidden = showHidden();
      // A tab opened by "start in a new worktree" arrives with ?launch=…;
      // consume it exactly once, after the first State, then strip it so a
      // reload does not start a second program.
      if (pendingLaunch && !pendingLaunchSent) {
        pendingLaunchSent = true;
        newTerminal(3, pendingLaunch);
        history.replaceState(null, "", location.pathname);
      }
      // A row clicked on the overview arrives with ?focus=<session>; focus
      // that terminal once, after the first State (so its tab exists), then
      // strip it so a reload doesn't re-focus. Uses the same focusSession the
      // tab bar uses; a name the layout lacks is simply ignored.
      if (pendingFocus && !pendingFocusDone) {
        pendingFocusDone = true;
        if (state.panes.some((p) => p.tabs.some((t) => t.k === "Terminal" && t.session === pendingFocus))) {
          focusSession(pendingFocus);
        }
        history.replaceState(null, "", location.pathname);
      }
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
      paintNonAscii(ev.rel);
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
    // The header chip renders the same `git status` the Changes pane does —
    // its dirty dot and counts went stale in exactly the same way, and until
    // this event started firing on ordinary edits (watch.rs) there was
    // nothing to hang it on. A dedicated "git" trigger rather than "refresh":
    // that one also refetches the projects and worktrees strips, which have
    // no stake in this project's working tree.
    case "StatusChanged":
      refreshKind("Changes");
      document.body.dispatchEvent(new Event("git")); // #gitinfo hx-trigger listens
      break;
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
    case "Proposal":
      // Content before the tab that renders it — both on the live path
      // (hub.rs's open_proposal_tab broadcasts this before the State that
      // adds the Tab::Proposal) and on connect (wsconn.rs now replays these
      // ahead of the snapshot too) — so by the time a Tab::Proposal ever
      // reaches tabKey below, this is already populated for it in the
      // overwhelming majority of cases. tabKey still folds "has content"
      // into its own key (see below) to cover the remaining sub-millisecond
      // window rather than assume that ordering.
      proposals[ev.id] = { rel: ev.rel, old_text: ev.old_text, new_text: ev.new_text };
      if (state) { state.proposals = proposals; render(); }
      break;
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
    case "ClaudeHere":
      showClaudeHere(ev.pane, ev.terminals);
      break;
    case "WorktreeReady": {
      const url = "/" + projectPath(ev.url) + (ev.launch ? `?launch=${encodeURIComponent(ev.launch)}` : "");
      if (pendingTab && !pendingTab.closed) {
        pendingTab.location = url;
      } else {
        // The popup was blocked (or something else closed it): fall back to
        // a link rather than lose the worktree the server already created.
        showBanner(`opened ${ev.url.split("/").pop()} — `);
        const a = document.createElement("a");
        a.href = url; a.target = "_blank"; a.textContent = "click to go there";
        document.querySelector(".error-banner:last-of-type b")?.append(a);
      }
      pendingTab = null;
      document.body.dispatchEvent(new Event("projects"));
      break;
    }
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
    case "ProjectsChanged":
      // Some project — usually not this one — gained its first shell or
      // lost its last. The ◆ panel and its badge are a server-rendered
      // fragment that otherwise refetches only on this tab's own triggers,
      // so without this a project closed from another tab stayed "●" here
      // until a reload. A dedicated event, not "refresh": that one also
      // refetches #gitinfo, and another project's shell ending is no reason
      // to re-run git status here.
      document.body.dispatchEvent(new Event("projects"));
      break;
    case "ProjectClosed":
      // A successful close, not a failure — showBanner directly so this
      // doesn't get the "Error:" prefix showError adds.
      showBanner(ev.ended + " terminal session(s) ended");
      terms.forEach((e) => {
        // Disarm before closing, exactly as render()'s teardown does and for
        // the same reason it spells out there: `attach` creates when absent,
        // so a reconnect that survives this teardown does not reattach — it
        // *respawns* a shell in the project the user just closed, into a
        // disposed xterm, with no tab and no client. Omitting it here was the
        // live bug: the close kills each PTY, a socket that dies unclean
        // schedules connectTerm on backoff, `entry.gone` is unset so onclose
        // does not bail, and a retry landing after the server clears
        // `closing` is accepted and spawns. Observed on the deploy host as a
        // fresh dtach whose parent was roost itself, started in the same
        // second as the close, surviving with nothing attached to it.
        e.gone = true;
        clearTimeout(e.timer);
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
      console.warn("roost:", ev.msg);
      showError(ev.msg);
      // do_open_at_line's own confinement check (src/hub.rs) refuses here,
      // never with a RevealLine — so a flag armed for it would otherwise
      // stay armed forever, ready to steal focus (and via wireEditor's blur
      // listener, trigger an autosave) on some *other* browser's later,
      // unrelated RevealLine. Clearing unconditionally on every Error is
      // safe even when this Error has nothing to do with a reveal: the flag
      // only ever changes whether the next RevealLine focuses.
      focusNextReveal = false;
      break;
    case "PathRefused":
      // Not showError: that funnels to the workspace banner, which is the
      // wrong shape here and — as the Error case above notes — carries no way
      // back to the terminal that was clicked. This does, via the click still
      // in flight.
      console.warn("roost:", ev.msg);
      // Same reasoning as the Error case above: a link that doesn't resolve
      // (the comment below calls this "the common case, not an edge case")
      // armed focusNextReveal in openTermPath and will never get the
      // RevealLine that would otherwise have cleared it.
      focusNextReveal = false;
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
    case "SearchResults":
      // A late answer to a query the user has typed past must not paint over
      // what they are looking at now. The server drops most of these; this is
      // the client half of the same rule, for the ones already in flight.
      if (ev.seq !== searchSeq) break;
      renderSearch(ev.results);
      break;
    case "RevealLine": {
      // Consumed once, immediately: focusNextReveal marks "the intent I just
      // sent (OpenAtLine or a line-suffixed OpenPath) is mine", and this
      // RevealLine may be that broadcast coming back, or it may be a
      // completely different browser's action mirrored to this one — the
      // only way to tell them apart client-side. No requestAnimationFrame:
      // an Edit tab mounts synchronously inside the State handler that runs
      // before this event is even queued, and a Preview tab's fetch can take
      // far longer than one frame anyway — revealLine()/tryReveal() below
      // handle that race by retrying from the fetch's own completion, not by
      // guessing at a delay.
      const focus = focusNextReveal;
      focusNextReveal = false;
      revealLine(ev.rel, ev.line, focus);
      break;
    }
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
    // Whether content has arrived is part of the key, not just the id: a
    // Tab::Proposal on its own carries only an id (see proto.rs), so the
    // placeholder and the real hunk view are two different DOM shapes for
    // the same tab, exactly like Terminal's live/placeholder split above.
    // Without this, render()'s mountedKey guard would see an unchanged key
    // when the Proposal event lands right after the tab and never remount —
    // the placeholder (which offers no Accept/Reject) would stick around
    // even once there is something to show and answer.
    case "Proposal": return `Proposal:${t.id}:${state.proposals && state.proposals[t.id] ? "content" : "pending"}`;
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
    case "Proposal": {
      const p = state && state.proposals && state.proposals[t.id];
      return p ? p.rel.split("/").pop() : "proposal";
    }
  }
}

function render() {
  if (!state) return;
  const header = document.querySelector("header");
  if (header) document.documentElement.style.setProperty("--header-h", header.offsetHeight + "px");
  // htmx swaps into #gitinfo/#wtlabel and the #projcount/#bellcount writes
  // below all change the header's width while the panel is open, and
  // #searchbox uses margin:auto, so the field (and the panel anchored to it)
  // drifts unless every render re-measures it. Guarded on "searching" so a
  // closed panel costs nothing on every State broadcast.
  if (document.body.classList.contains("searching")) anchorSearchPanel();
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
      // A terminal running a Claude swaps the terminal glyph for the Claude
      // mark (the [data-claude] rule in style.css). The server derives the
      // list; the client never guesses from the session name, because
      // "claude" is a legal name for a plain shell.
      if (t.k === "Terminal" && (state.claude_sessions || []).includes(t.session)) {
        b.dataset.claude = "";
        b.title = "running Claude";
      }
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
      const x = document.createElement("span");
      x.className = "x";
      x.title =
        t.k === "Terminal" ? "end session (alt-click to detach, leaving it running)" : "close";
      x.textContent = "×";
      x.onclick = (e) => { e.stopPropagation(); closeTab(pi, ti, t, e.altKey); };
      b.appendChild(x);
      strip.appendChild(b);
    });

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
  const icon = (svg, title, fn, cls) => {
    const b = document.createElement("span");
    b.className = cls ? `paneicon ${cls}` : "paneicon";
    b.title = title;
    b.innerHTML = svg; // constant markup from PANE_ICONS/CLAUDE_MARK only
    b.onclick = fn;
    host.appendChild(b);
    return b;
  };
  // The create pair leads the group — they used to trail the last tab inside
  // the wrapping strip, where they drifted with the tab count instead of
  // sitting where pane controls live (moved 2026-08-24, from real use). The
  // extra classes are their stable identity for tests and future styling.
  icon(PANE_ICONS.newterm, "new terminal", () => newTerminal(pi), "newterm");
  // The official Claude mark (see CLAUDE_MARK above). Same button, plus a
  // program to type in: the server allocates the name and types `claude`
  // into the shell it spawns.
  if (LAUNCHES.includes("claude")) {
    icon(CLAUDE_MARK, "new terminal running Claude", () => newTerminal(pi, "claude"), "newclaude");
  }
  if (active && active.k === "Tree") {
    const hidden = showHidden();
    // Filled ring = dot entries are showing. This drives an intent rather
    // than filtering client-side: the rows do not exist in the fragment the
    // server sent, so there is nothing here to un-hide.
    icon(hidden ? PANE_ICONS.dotsOn : PANE_ICONS.dotsOff, hidden ? "hide dotfiles" : "show dotfiles", () => {
      send({ t: "SetShowHidden", on: !showHidden() });
    });
    icon(PANE_ICONS.collapse, "collapse all", () => {
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
    icon(PANE_ICONS.move, `move this tab to the ${swap === RIGHT ? "right" : "middle"} pane`, () => {
      // Append, and let the server clamp: `at` is checked against the
      // destination's real length in workspace.rs, which is the authority on
      // a layout this client may already be a broadcast behind on.
      send({ t: "MoveTab", from: pi, idx: pane.active, to: swap, at: state.panes[swap].tabs.length });
    });
  }
  const on = maxState.pane === pi;
  icon(on ? PANE_ICONS.restore : PANE_ICONS.maximize, on ? "restore pane sizes" : "maximize pane", () => toggleMaximized(pi));
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

function newTerminal(pane, launch) {
  // No prompt: the server allocates term/term1/term2… from `live_names`, which
  // sees detached sessions the client has no tabs for. A name picked here could
  // collide with one of those, and since attaching creates only when absent,
  // "new terminal" would silently reattach to an old shell instead.
  //
  // `launch` names a program (one of LAUNCHES), never a command line: the
  // server owns what is typed, this only says which. Omitted, not null, for
  // the plain + so its message stays the one it has always sent.
  send(launch ? { t: "NewTerminal", pane, launch } : { t: "NewTerminal", pane });
}

// The Edit/Preview switch lives in the filename stripe (`.path`), not on the
// tab: only the active tab's mode is visible, so a per-tab toggle spent ~20px
// on every markdown tab for a control that mostly toggled something you could
// not see. A png renders but cannot be edited, and a code file edits but
// renders as nothing — so which half of the switch is worth offering depends
// on the mode the tab is in, which is what the guard below works out.
function modeButton(rel, mode) {
  // Asymmetric on purpose. The eye is offered only where there is a rendered
  // form worth looking at, as before — a code file in Edit gains nothing from
  // a toggle to a mode nothing opens it in. The pencil is offered from
  // Preview for anything editable, which is wider than it used to be: a text
  // file can now *arrive* in Preview without anyone asking for it, because
  // the server demotes a tab whose file it cannot read — one past the size
  // cap, and (hub::file_vanished) one that was renamed away underneath the
  // tab. With the old rule a demoted .rs was stranded there, with no way back
  // even once the file returned, since re-clicking it in the tree only
  // focuses the tab it already has and never revisits its mode.
  const editable = !refusesTextEdit(rel);
  if (!editable || (mode === "Edit" && !hasRenderedForm(rel))) return null;
  const b = document.createElement("button");
  b.className = "modebtn"; // NOT .savebtn: paintSaveState selects ".savebtn" and must find Save, not this
  b.innerHTML = mode === "Edit" ? MODE_ICONS.preview : MODE_ICONS.edit; // constant markup only
  b.title = mode === "Edit" ? "switch to preview" : "switch to edit";
  b.onclick = () => send({ t: "SetMode", rel, mode: mode === "Edit" ? "Preview" : "Edit" });
  return b;
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
  if (t.k === "Proposal") { renderProposal(content, t); return; }
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
    if (t.k === "File") {
      const mb = modeButton(t.rel, t.mode);
      const path = content.querySelector(".path");
      if (mb && path) path.appendChild(mb);
      // A RevealLine can arrive before this fetch resolves — the State event
      // that creates this tab only starts the fetch, it does not wait for
      // it. tryReveal() is a no-op unless something is still waiting on
      // exactly this rel, so calling it unconditionally on every File mount
      // is cheap and correct either way.
      tryReveal();
      // Same argument for a pending #heading from a markdown link.
      tryAnchor();
    }
    // Tree fragments carry lazy <details hx-get="...tree?dir=..."
    // hx-trigger="toggle once"> nodes (render::tree_level). htmx only binds
    // hx-* attributes when it walks the DOM itself (page boot, or its own
    // ajax swaps); content dropped in via plain innerHTML — like this fetch
    // — is invisible to it until told, so a freshly loaded tree needs an
    // explicit process() or every closed directory would be inert forever.
    if (t.k === "Tree") window.htmx && htmx.process(content);
  });
}

// --- proposal tabs (openDiff) ---------------------------------------------
//
// The hunk view itself is server-rendered (`/frag/{project}/proposal?id=`,
// render::proposal_fragment — reusing textdiff.rs::unified + diff_html, the
// same pair the save-conflict banner's diff_html comes from) rather than
// built here: CLAUDE.md is explicit that HTML is built in Rust, and a
// hand-ported second copy of textdiff.rs's trim/LCS/cap algorithm already
// drifted from it once (an empty old_text produced a phantom "-" line here
// that Rust's `.lines()` never would) before this was caught in review.
//
// What stays client-side is only `state.proposals` — the map keyed by
// proposal id that says whether `Event::Proposal`'s content has arrived yet
// — because that presence check, and the `tabKey` fold built on it below,
// are the safety property: a `Tab::Proposal` carries only an id (proto.rs),
// and the placeholder branch below is what guarantees nothing can be
// accepted or rejected before this client has independently confirmed the
// content exists, not merely trusted that the fragment fetch will succeed.
function renderProposal(el, tab) {
  const p = state.proposals && state.proposals[tab.id];
  if (!p) {
    // Reachable only in the sub-millisecond window between the State that
    // names this tab and the Event::Proposal that carries its content —
    // tabKey folds "has content" into itself, so this is remounted the
    // instant that event lands and is never the tab's steady state.
    //
    // No Accept/Reject button anywhere below this branch, and no fetch
    // either: answering a proposal nobody can read is answering a
    // permission prompt blind, which is exactly what this codebase's
    // conflict-guard exists to prevent (see CLAUDE.md).
    el.textContent = "Waiting for the proposed change to arrive…";
    return;
  }
  el.innerHTML = "";
  const url = `/frag/${PROJECT}/proposal?id=${encodeURIComponent(tab.id)}`;
  // Same in-flight-fetch guard mountTab's generic branch below uses: a
  // proposal answered (or this pane moved on to a different tab entirely)
  // while this fetch was in the air must not have its response land and
  // clobber whatever is showing now.
  el.dataset.url = url;
  fetch(url).then((r) => r.text()).then((html) => {
    if (el.dataset.url !== url) return;
    el.innerHTML = html;
    // The edit box and the action bar go *inside* render.rs's .proposalview,
    // not beside it: that element carries the flex column, so a sibling would
    // sit outside the layout and the bar would stop being pinned.
    //
    // The `|| el` is a deliberate degrade, not a guess papering over a
    // failure. If the wrapper is ever missing (a stale fragment from a server
    // mid-deploy is the realistic way), the honest outcome is the old
    // in-flow layout — ugly but answerable. Refusing to mount the bar would
    // instead leave a proposal on screen that cannot be accepted or
    // rejected, and Claude blocked waiting on an answer nobody can give.
    const view = el.querySelector(".proposalview") || el;
    const bar = document.createElement("div");
    // Reuses .conflict's own button styling (see style.css's
    // ".conflict button, .proposal-actions button" rule) rather than
    // duplicating it under a parallel class.
    bar.className = "proposal-actions";
    // Null unless the box exists *and* its text differs from what Claude
    // proposed. That distinction is the whole protocol difference: an
    // unedited accept answers TAB_CLOSED ("write what you proposed"), an
    // edited one answers FILE_SAVED plus this text ("write mine instead"),
    // which is how Claude learns the file will not match its own proposal.
    const edited = () => {
      const box = el.querySelector(".proposal-edit");
      return box && box.value !== p.new_text ? box.value : null;
    };
    const answer = (accept) => send({
      t: "AnswerProposal", id: tab.id, accept, text: accept ? edited() : null,
    });
    const mkButton = (label, fn) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.onclick = fn;
      return b;
    };
    // Built with createElement and seeded through `.value`, not rendered
    // into the fragment server-side. CLAUDE.md puts HTML in render.rs
    // because hand-built markup is where escaping goes wrong — and file
    // content inside a server-rendered `<textarea>` is exactly that trap: a
    // `</textarea>` in the proposed text would close the element and let
    // the rest parse as markup. `.value` is never parsed as HTML at all, so
    // this is the stronger guarantee rather than a shortcut around the rule.
    const editButton = mkButton("Edit", () => {
      if (el.querySelector(".proposal-edit")) return;
      const box = document.createElement("textarea");
      box.className = "proposal-edit";
      box.value = p.new_text;
      box.spellcheck = false;
      // Above the buttons, below the diff: the diff stays readable while
      // you edit, which is the point of reviewing it at all.
      view.insertBefore(box, bar);
      editButton.remove();
      box.focus();
    });
    bar.append(
      mkButton("Accept", () => answer(true)),
      mkButton("Reject", () => answer(false)),
      editButton,
    );
    view.appendChild(bar);
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
// edits from a terminal pane, which is roost's core use case — so this
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
      // `[run](deploy.md#running)` names a heading as well as a file
      // (render.rs's link_open emits it as data-hash). Armed after the intent,
      // never before: the tab's fragment is only fetched once the State
      // broadcast lands, so at this instant there is nothing to scroll to. A
      // Diff has no headings, so it never arms.
      if (a.dataset.hash && !isDiff) revealAnchor(rel, a.dataset.hash);
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

// A real menu now, rather than a numbered prompt(). The prompt was never a
// menu by choice — it was the only way prompt() could offer four options —
// and it cost a second dialog for every action.
async function fileMenu(e, rel) {
  const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
  const items = [
    { id: "new", label: "New file…" },
    { id: "newdir", label: "New folder…" },
  ];
  // Rename and Delete need a target. The prompt version offered them at the
  // project root and then silently did nothing, because its guards were
  // `choice === "3" && rel`. A menu can simply not offer them.
  if (rel) items.push({ id: "rename", label: "Rename…" }, { id: "delete", label: "Delete" });
  const choice = await askMenu({ items, x: e.clientX, y: e.clientY });
  if (choice === "new") {
    const name = await askText({ title: "New file", label: "Path",
      value: dir ? `${dir}/untitled.txt` : "untitled.txt", confirm: "Create" });
    if (name) send({ t: "CreateFile", rel: name });
  } else if (choice === "newdir") {
    const name = await askText({ title: "New folder", label: "Path",
      value: dir ? `${dir}/newdir` : "newdir", confirm: "Create" });
    if (name) send({ t: "CreateDir", rel: name });
  } else if (choice === "rename") {
    const to = await askText({ title: "Rename", label: "New path", value: rel, confirm: "Rename" });
    if (to && to !== rel) send({ t: "RenamePath", from: rel, to });
  } else if (choice === "delete") {
    // the server refuses non-empty directories regardless of what we ask
    const yes = await askConfirm({ title: "Delete", lines: [`Delete ${rel}?`],
      confirm: "Delete", danger: true });
    if (yes) send({ t: "DeleteFile", rel });
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
  // The line is no longer dropped: the server sends a RevealLine alongside
  // the tab it opens (hub::do_open_path), and revealLine() scrolls there —
  // except when the tab lands on a rendered form with no line surface (a
  // markdown preview, an image), in which case revealLine() finds nothing
  // and does nothing. The label still names the line the link pointed at
  // either way; it is not a promise that the view will jump.
  const line = raw.match(/:(\d+)(?::\d+)?$/);
  if (line) {
    termFlash(entry, `line ${line[1]}`);
    // do_open_path (src/hub.rs) opens the target in Preview, not Edit, so
    // revealInPreview — which never focuses anything — is what usually runs
    // for this click, not revealInEditor. This flag still only ever matters
    // for the Edit case: the incidental one where this rel also happens to
    // have an Edit-mode pane open elsewhere. Armed here regardless, since
    // openTermPath cannot know which case it will be before the reply
    // arrives — see revealLine()'s focus parameter.
    focusNextReveal = true;
  }
  // Verbatim. The client does no parsing; resolution and confinement are one
  // function in projects.rs.
  send({ t: "OpenPath", text: raw });
}

// One provider per pattern, URL registered first. Where the two overlap — the
// path-looking tail of a URL — xterm resolves it by provider index in
// _removeIntersectingLinks, so registration order is the entire mechanism.
// xterm's own OscLinkProvider is registered at construction, ahead of both,
// which is the ordering this wants for free: a link an application declared
// beats anything roost would have guessed over the same cells.
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
        if (!warnedProvider) { warnedProvider = true; console.warn("roost: link provider failed:", e); }
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
  // `focusSession` (below) is the funnel for *activation* — a tab strip
  // click, a notice, the service-worker focus message, #session= routing —
  // but activation and "the user clicked into this terminal's body" are
  // different events. With two panes each showing an active Terminal tab,
  // clicking straight into the one that is already active fires neither
  // ActivateTab nor focusSession, so lastFocusedSession would keep
  // pointing at whichever terminal was last activated (or, right after a
  // reload, stay null and fall back to live[0]) even though the user is now
  // typing into a different one. `focusin` bubbles, and xterm's hidden
  // textarea is a descendant of `node`, so this catches the real DOM focus
  // event a click produces without depending on an xterm API for it — this
  // vendored build's public Terminal facade (the "d" class the UMD bundle
  // exports) wraps an internal core that has its own onFocus/onBlur, but
  // does not forward either one; calling `term.onFocus` throws.
  node.addEventListener("focusin", () => { lastFocusedSession = session; });
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
    // the only one roost owns, and it is what covers every other link route
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
  // is what was last copied, which roost's own paste route then dutifully typed
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
    //   unclean — the connection died under us: laptop slept, roost
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
  if (text.length > MAX_HIGHLIGHT_BYTES) return null;
  return codeExt(rel);
}

/// The extension hljs would highlight `rel` as, or null where the file is
/// prose to this editor — and so where the non-ASCII toggle applies instead.
/// Split from codeLanguage so the size cap does not turn a code file into a
/// prose one: a 200 KB .rs past the cap is still not a file to offer the
/// prose toggle on.
function codeExt(rel) {
  if (!window.codeInput || !window.hljs) return null;
  const ext = rel.includes(".") ? rel.split(".").pop().toLowerCase() : "";
  // hljs's own answer, so an extension it cannot highlight never gets an
  // overlay that would paint nothing. Markdown and plaintext are excluded
  // even though hljs knows both: markdown has a rendered preview and is
  // edited as prose, and plaintext has nothing to colour — this is the "code
  // files only" line.
  if (!ext || PLAIN_EXTS.has(ext) || !hljs.getLanguage(ext)) return null;
  return ext;
}

/// Attaches roost's editing behaviour to whichever textarea ends up on screen.
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
      // On the debounce, not the keystroke: a scan of the whole buffer per
      // keypress is cheap on a page of notes and not on a 2 MB one.
      paintNonAscii(rel);
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
  const mb = modeButton(rel, "Edit");
  const na = nonAsciiButton(content, rel);
  bar.append(name, st, ...[na, mb, btn].filter(Boolean));
  const lang = codeLanguage(rel, text);
  // A prose file takes the overlay too while the non-ASCII toggle is on, under
  // the same size cap and for the same reason: the whole layer repaints on
  // every pause in typing.
  const overlay = lang ? "hl" : (na && nonAsciiOn() && text.length <= MAX_HIGHLIGHT_BYTES ? "nonascii" : null);
  if (overlay) {
    const host = document.createElement("code-input");
    host.setAttribute("template", overlay);
    if (lang) host.setAttribute("language", lang);
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
  paintNonAscii(rel);
}

/// The edit bar's non-ASCII control, for prose files only — a code file's
/// overlay belongs to hljs, and its stray characters are the compiler's
/// business. It is an indicator first: accented, with a count, whenever the
/// buffer holds anything outside the allowed set, on or off. Clicking it
/// turns the marks in the text on or off, for every prose file at once.
/// Nothing here reaches the server: the flag is this browser's, like a
/// theme, and the count is computed from `texts`.
function nonAsciiButton(content, rel) {
  if (codeExt(rel) || !window.codeInput) return null;
  const b = document.createElement("button");
  b.className = "nonasciibtn"; // NOT .savebtn or .modebtn: each of those is selected by name elsewhere
  b.onclick = () => {
    setNonAsciiOn(!nonAsciiOn());
    remountEditor(content, rel);
  };
  return b;
}

/// Repaints the indicator for `rel`'s mounted editor from the current text.
/// Called on mount, on the edit debounce, and when a BufferText lands — the
/// three ways the text under the button changes.
function paintNonAscii(rel) {
  const ta = editors.get(rel);
  const b = ta && ta.closest(".editwrap") && ta.closest(".editwrap").querySelector(".nonasciibtn");
  if (!b) return;
  const text = texts.get(rel) || "";
  const n = nonAsciiCount(text);
  const on = nonAsciiOn();
  const big = text.length > MAX_HIGHLIGHT_BYTES;
  b.classList.toggle("has", n > 0);
  b.classList.toggle("on", on);
  b.disabled = big;
  // The count is in the label, not only the title: "the toggle did nothing"
  // and "there was nothing to mark" have to look different.
  b.textContent = n ? `ä ${n}` : "ä";
  const what = n === 0 ? "no non-ASCII characters" : n === 1 ? "1 non-ASCII character" : `${n} non-ASCII characters`;
  if (big) b.title = `${what} — too large to highlight (over ${Math.round(MAX_HIGHLIGHT_BYTES / 1000)} KB)`;
  else if (on) b.title = `${what} — highlighted; click to stop`;
  else b.title = `${what} (allowed: TAB, LF, 0x20–0x7E) — click to highlight`;
}

/// Rebuilds `rel`'s editor in place, keeping the caret, the scroll position
/// and focus. The textarea a plain editor owns and the one <code-input> builds
/// are different elements, so switching the overlay on or off means a fresh
/// mount — the same thing a pane switch and back does, seeded from `texts`.
/// The pane's mountedKey is untouched, so render() leaves the result alone.
function remountEditor(content, rel) {
  const old = editors.get(rel);
  const focused = !!old && document.activeElement === old;
  const sel = old ? [old.selectionStart, old.selectionEnd] : null;
  // The scrolling element differs by kind: a plain textarea scrolls itself,
  // a code-input host scrolls for both its layers (see scrollEditorTo).
  const scroller = old && (old.closest("code-input") || old);
  const top = scroller ? scroller.scrollTop : 0;
  content.innerHTML = "";
  mountEditor(content, rel);
  const ta = editors.get(rel);
  if (!ta) return;
  if (sel) ta.setSelectionRange(sel[0], sel[1]);
  (ta.closest("code-input") || ta).scrollTop = top;
  if (focused) ta.focus();
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
  // Visible only when pressing it would do something: a dirty buffer that
  // autosave is not already writing, or a paused buffer, where Save is the
  // only route to the conflict diff. A clean buffer used to keep a Save that
  // did nothing when clicked — reported from real use, 2026-08-24.
  btn.hidden = !((meta && meta.dirty && !AUTOSAVE) || paused);
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

// The terminal a mention is aimed at. Recorded on focus rather than derived
// from the layout alone: two panes can each hold an active Terminal tab, and
// the layout says nothing about which one the user last looked at.
let lastFocusedSession = null;

function activeTerminalSession() {
  if (!state) return null;
  const live = [];
  for (const pane of state.panes) {
    const tab = pane.tabs[pane.active];
    if (tab && tab.k === "Terminal") live.push(tab.session);
  }
  if (!live.length) return null;
  // The remembered one only counts while it is still an active tab somewhere;
  // otherwise a closed terminal would keep claiming every mention.
  if (live.includes(lastFocusedSession)) return lastFocusedSession;
  return live[0];
}

// Which tab a mention keystroke means, and in which mode — same "focused
// editor first, else the visible MIDDLE/RIGHT File tab" rule saveTarget uses
// above, for the same reason: focus is often on the body, not a textarea,
// right after a reconnect or a click elsewhere on the page.
//
// Two clauses that look alike are doing different jobs here. `mode === "Edit"`
// is what this function deliberately stops requiring — a Preview tab is a
// perfectly good thing to point Claude at. `editors.has(rel)` stays, but only
// on the Edit branch: `editors` holds textareas, so a Preview tab never has an
// entry, and keeping that test unconditional would leave this returning null
// for every Preview tab — the feature would look implemented and do nothing.
function mentionTarget() {
  const el = document.activeElement;
  if (el && el.classList && el.classList.contains("editor")) {
    for (const [rel, ta] of editors) if (ta === el) return { rel, mode: "Edit" };
  }
  for (const p of [MIDDLE, RIGHT]) {
    const pane = state && state.panes && state.panes[p];
    const tab = pane && pane.tabs[pane.active];
    if (!tab || tab.k !== "File") continue;
    if (tab.mode === "Edit") {
      // An Edit tab whose textarea has not mounted yet is not a target; fall
      // through to the other pane, exactly as this did before.
      if (editors.has(tab.rel)) return { rel: tab.rel, mode: "Edit" };
      continue;
    }
    return { rel: tab.rel, mode: tab.mode };
  }
  return null;
}

// 1-based inclusive line numbers, matching how MentionPath's line_start/
// line_end read (Option<u32> — a caret with no selection sends neither).
function mentionSelection(rel) {
  const ta = editors.get(rel);
  if (!ta || ta.selectionStart === ta.selectionEnd) return { startLine: null, endLine: null };
  const before = ta.value.slice(0, ta.selectionStart);
  const selected = ta.value.slice(ta.selectionStart, ta.selectionEnd);
  const startLine = before.split("\n").length;
  let endLine = startLine + selected.split("\n").length - 1;
  // A selection ending exactly at a line boundary (the caret sits right
  // after the trailing \n) must not count the following, unselected line.
  if (selected.endsWith("\n")) endLine -= 1;
  return { startLine, endLine };
}

// Alt+K, matching the extensions' own binding. The selection's line range
// travels; the text does not (that is ShareSelection, and it is opt-in).
//
// Three ways to recognise one keystroke, because no single property covers
// every browser. Option on macOS is a character-composing modifier, so
// `e.key` is the composed glyph rather than "k" — and what the browser
// reports *instead* differs:
//
//   Linux / Windows      key "k"   code "KeyK"   keyCode 75
//   Chromium on macOS    key "˚"   code "KeyK"   keyCode 75
//   Firefox on macOS     key "˚"   code ""       keyCode 0     <- measured
//
// The last row is the one that keeps biting. macOS exposes no scancode, so
// Firefox derives `code` from the virtual keycode, which is 0 while Option
// is held — mozilla bug 300678 / 44259, open for two decades. There is no
// physical-key information in that event at all: the composed character is
// the only thing identifying it, which is why it is matched literally here.
//
// "˚" is U+02DA and is what Option+K yields on the US layout. This is
// therefore layout-specific by necessity, not by choice — on a layout where
// Option+K composes something else, Firefox on macOS has nothing left to
// match on. The durable answer for that case is a Cmd chord (Cmd does not
// compose, and Firefox reports key and code correctly for it), not another
// character literal.
document.addEventListener("keydown", (e) => {
  if (!e.altKey || (e.code !== "KeyK" && e.key.toLowerCase() !== "k" && e.key !== "˚")) return;
  const target = mentionTarget();
  if (target === null) {
    // Alt+K is Meta-k in readline, so a keystroke aimed at a shell must not
    // raise a banner about tabs. Only a keystroke with nowhere to go and no
    // terminal under it is worth reporting.
    if (e.target && e.target.closest && e.target.closest(".xterm")) return;
    // Silence here is indistinguishable from a broken binding, which is how
    // this was reported in the first place.
    showError("Alt+K mentions the file in the active tab — open a file first.");
    return;
  }
  e.preventDefault();
  // A Preview tab has no textarea and no source-line mapping, so it mentions
  // the whole file. See the spec's "Why a preview carries no line range".
  const sel = target.mode === "Edit"
    ? mentionSelection(target.rel)
    : { startLine: null, endLine: null };
  send({
    t: "MentionPath",
    rel: target.rel,
    line_start: sel.startLine,
    line_end: sel.endLine,
    session: activeTerminalSession(),
  });
});

// --- selection sharing (opt-in, off by default — see SHARE_SELECTION) ------
//
// 0-based line and character offsets, unlike mentionSelection's 1-based line
// numbers above: this feeds `ShareSelection`, which the server turns straight
// into the LSP-style `{line, character}` pairs selection_changed puts on the
// wire (src/ide.rs), not MentionPath's 1-based lineStart/lineEnd.
function shareSelectionSnapshot(rel) {
  const ta = editors.get(rel);
  if (!ta || ta.selectionStart === ta.selectionEnd) return null;
  const before = ta.value.slice(0, ta.selectionStart);
  const text = ta.value.slice(ta.selectionStart, ta.selectionEnd);
  const beforeLines = before.split("\n");
  const startLine = beforeLines.length - 1;
  const startCol = beforeLines[beforeLines.length - 1].length;
  const selLines = text.split("\n");
  const endLine = startLine + selLines.length - 1;
  const endCol = selLines.length === 1 ? startCol + selLines[0].length : selLines[selLines.length - 1].length;
  return { rel, text, startLine, startCol, endLine, endCol };
}

// Debounced in the browser, not the socket thread: a debounce there would
// hold per-connection state (a timer, a pending send) for no reason, since
// this client is the only one that ever needs to coalesce its own rapid
// selection changes.
let shareSelectionTimer = null;
document.addEventListener("selectionchange", () => {
  if (!SHARE_SELECTION) return; // cheapest possible no-op for the common case
  clearTimeout(shareSelectionTimer);
  shareSelectionTimer = setTimeout(() => {
    const target = mentionTarget(); // same "which tab" rule Alt+K uses
    if (target === null || target.mode !== "Edit") return;
    const sel = shareSelectionSnapshot(target.rel);
    if (!sel) return;
    send({
      t: "ShareSelection", rel: sel.rel, text: sel.text,
      start_line: sel.startLine, start_col: sel.startCol,
      end_line: sel.endLine, end_col: sel.endCol,
    });
  }, 200);
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

// The "a Claude is already here" prompt. Per-browser and transient: it is a
// question to the person who clicked, not a state of the project.
function showClaudeHere(pane, terminals) {
  document.querySelectorAll(".claudehere").forEach((n) => n.remove());
  const box = document.createElement("div");
  box.className = "conflict claudehere";
  const b = document.createElement("b");
  b.textContent = terminals.length
    ? `A Claude is already working in this project (${terminals.join(", ")}).`
    : "A Claude is already working in this project.";
  const wt = document.createElement("button");
  wt.className = "wt-new";
  wt.textContent = "Start in a new worktree";
  wt.onclick = () => {
    // Opened synchronously, inside this click's user-gesture, so the popup
    // blocker allows it; WorktreeReady navigates it once the server responds.
    pendingTab = window.open("about:blank");
    send({ t: "NewWorktree", launch: "claude" });
    box.remove();
  };
  const here = document.createElement("button");
  here.className = "wt-here";
  here.textContent = "Start here anyway";
  here.onclick = () => { send({ t: "NewTerminal", pane, launch: "claude", force: true }); box.remove(); };
  const dismiss = document.createElement("button");
  dismiss.textContent = "dismiss";
  dismiss.onclick = () => box.remove();
  box.append(b, wt, here, dismiss);
  const host = document.querySelector(`.pane[data-pane="${pane}"]`) || document.body;
  host.prepend(box);
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
async function closeTab(pi, ti, t, detach) {
  const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
  if (meta && meta.dirty) {
    const yes = await askConfirm({ title: "Unsaved changes",
      lines: [`${t.rel} has unsaved changes. Close it?`], confirm: "Close", danger: true });
    if (!yes) return;
    // The dialog did not block the event loop, so the tab strip may have been
    // rebuilt from a State event while it was open and `ti` may now address a
    // different tab. Re-resolve by rel — the idiom focusSession already uses.
    // The `< 0` branch is the point: not finding the tab is "I cannot tell",
    // never "close index ti anyway".
    //
    // This narrows the race, it does not close it: another client can still
    // renumber the strip between this findIndex and the moment the server
    // applies the CloseTab frame, because CloseTab is positional on the
    // wire and nothing on the client can make "resolve, then send" atomic
    // with the server's view. What the re-resolution buys is shrinking the
    // exposure from "the whole time the dialog is open" (human-scale, the
    // original bug) down to one network round trip. Closing it for real
    // would need CloseTab to address a tab by identity, not position.
    //
    // Revert-check (CLAUDE.md), scenarios 1 and 2 (replacing this whole
    // re-resolution with the stale `send({ t: "CloseTab", pane: pi, idx: ti
    // })`) produced, verbatim:
    //   ok    scenario 1: three file tabs, in order
    //   ok    the dirty-close dialog opened
    //   ok    scenario 1: the strip moved while the dialog was open
    //     (timed out waiting for c.txt closed)
    //   FAIL  scenario 1: the tab the user clicked was closed, not the one at its old index
    //   FAIL  scenario 1: cancelling the dialog closes nothing
    //   ok    pane cleared before scenario 2
    //   ok    scenario 2: four file tabs, in order
    //   ok    scenario 2: the dirty-close dialog opened
    //   ok    scenario 2: the strip moved while the dialog was open
    //   FAIL  scenario 2: the tab the user clicked (c.txt) was closed, not whichever tab shifted into its old index (d.txt)
    //   FAIL (3)
    // With only three tabs (scenario 1), closing a.txt always pushes the
    // stale index past the end of the shrunk strip, so the server refuses
    // the out-of-range CloseTab and c.txt is simply never closed -- a no-op,
    // not the hazard this fix exists for. Scenario 2 adds a fourth tab so the
    // stale index (2, c.txt's original slot) stays in range after a.txt
    // closes (b=0, c=1, d=2): reverted code closed d.txt instead of c.txt,
    // leaving ["b.txt","c.txt"] where the fix leaves ["b.txt","d.txt"] --
    // under revert the tab the user actually clicked (c.txt) survived, and a
    // different, valid tab (d.txt) was silently closed instead, with no
    // error shown.
    const ti2 = state.panes[pi].tabs.findIndex((x) => x.k === "File" && x.rel === t.rel);
    // Scenarios 1 and 2 never make ti2 negative -- their re-resolution
    // always finds something, so this branch had no coverage until scenario
    // 3, where the OTHER client closes c.txt itself (the tab under the
    // dialog, not a neighbor). Revert-check for this branch specifically
    // (replacing the line below with the stale fallback
    // `send({ t: "CloseTab", pane: pi, idx: ti2 < 0 ? ti : ti2 })`) left
    // scenarios 1 and 2 passing (ti2 was never negative for them) and
    // produced, verbatim, for scenario 3:
    //   ok    scenario 3: four file tabs, in order
    //   ok    scenario 3: the dirty-close dialog opened
    //   ok    scenario 3: the strip moved while the dialog was open
    //   FAIL  scenario 3: re-resolution found nothing (c.txt was already gone), so nothing else was closed in its place
    //   FAIL (1)
    // The actual tab list at that assertion was ["a.txt","b.txt"] against an
    // expected ["a.txt","b.txt","d.txt"]: c.txt's original slot (2) was
    // reoccupied by d.txt once c.txt closed (a=0, b=1, d=2), and the stale
    // fallback closed d.txt -- a tab nobody asked to close, silently, with
    // no error -- instead of doing nothing. `rel` also does not disambiguate
    // MoveTab from an actual close: `ti2 < 0` means "not in this pane
    // anymore", which is also true of a tab moved to another pane, so the
    // message below says only that, not that the tab is closed.
    if (ti2 < 0) { showError(`${t.rel} is no longer in this pane`); return; }
    send({ t: "CloseTab", pane: pi, idx: ti2 });
    return;
  }
  if (t.k === "Terminal" && !detach) {
    const yes = await askConfirm({ title: "End session",
      lines: [`End session "${t.session}"?`,
              "This kills the shell and anything running in it."],
      confirm: "End session", danger: true });
    if (!yes) return;
    // This has its own, smaller staleness window: EndSession is addressed by
    // NAME, not position, so a tab-strip reshuffle while the dialog was open
    // does not misdirect it the way a stale index would. But names are freed
    // and reused once a session ends (a scratch run for this task hit
    // `session2 = term1` for exactly that reason), so if THIS session ends
    // and a new session picks up the same name while the dialog is still
    // open, confirming ends a shell the user never saw the name of. There is
    // no cheap client-side fix -- the server would need to hand out an
    // identity that outlives name reuse -- so this is a known, accepted
    // residual, not something patched here.
    send({ t: "EndSession", session: t.session });
    return;
  }
  // No dialog was shown on this path, so nothing awaited and `ti` is still
  // the index the click was made against.
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
if (closeBtn) closeBtn.onclick = async () => {
  const live = (state && state.live_sessions) || [];
  const dirty = ((state && state.buffers) || []).filter((b) => b.dirty).map((b) => b.rel);
  const lines = [live.length
    ? `${live.length} terminal session(s) will be ended: ${live.join(", ")}`
    : "No terminal sessions are running."];
  if (dirty.length) lines.push(`Unsaved changes in: ${dirty.join(", ")}`);
  // One dialog in both states. The alert/confirm split existed only because a
  // native dialog cannot disable its OK button; `blocked` can. The check still
  // mirrors the server's own CloseRefused rather than making its own ruling —
  // it is told before sending anything, rather than after a round trip that
  // would have changed nothing.
  const yes = await askConfirm({
    title: `Close ${PROJECT}?`,
    lines,
    confirm: "End sessions",
    danger: true,
    blocked: dirty.length ? "Save or discard them first." : "",
  });
  if (yes) send({ t: "CloseProject" });
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

// The worktree switcher popup: third of the header popups, same pattern as
// projpanel above and the bell below. Rows are plain anchors — plain click
// navigates this tab, ⌘/ctrl-click is the browser's own new-tab, no JS here.
const wtBtn = document.getElementById("wtbtn");
const wtPanel = document.getElementById("wtpanel");
if (wtBtn && wtPanel) {
  wtBtn.onclick = () => {
    wtPanel.hidden = !wtPanel.hidden;
    if (!wtPanel.hidden && window.htmx) {
      // State costs two git calls per worktree; ask only while looking.
      // `document.body.dataset.key` is already the server's `percent_encode(key)`
      // (render.rs's `qkey`, embedded raw into the sibling `hx-get` on
      // #wtstrip's own span) — wrapping it in encodeURIComponent here escaped
      // the '%' a second time (`%2F` -> `%252F`), which the server's single
      // percent_decode could not undo, so the switcher silently rendered "no
      // worktrees" for any project whose key needed encoding at all.
      htmx.ajax("GET", `/frag/_worktrees?current=${document.body.dataset.key || ""}&state=1`, "#wtstrip");
    }
  };
  // A plain click through to a worktree navigates away anyway; this is for
  // the ⌘-click case, which stays on this page with the panel open. The
  // remove control is the other thing this popup's clicks can mean.
  wtPanel.onclick = async (e) => {
    const rm = e.target.closest(".wtremove");
    if (rm) {
      // Everything the event carries is read before the first await: e is
      // still valid afterward, but the panel's own re-fetches (hx-trigger on
      // "projects"/"refresh") can replace this DOM out from under us while
      // the dialog is open.
      e.preventDefault();
      const key = rm.dataset.key;
      const name = rm.closest(".wtrow")?.textContent.trim().split(/\s+/)[1] || key;
      const yes = await askConfirm({ title: "Remove worktree",
        lines: [`Remove worktree ${name} and its branch?`,
                "roost re-checks that it is clean, idle and merged first."],
        confirm: "Remove", danger: true });
      if (yes) send({ t: "RemoveWorktree", key });
      return;
    }
    if (e.target.closest("a")) wtPanel.hidden = true;
  };
}

const bell = document.getElementById("bell");
if (bell) {
  bell.onclick = () => {
    const p = document.getElementById("noticepanel");
    p.hidden = !p.hidden;
    renderNotices();
  };
}

// Header control buttons must not steal keyboard focus from the terminal or
// the editor. Glancing at notifications, or opening the worktree switcher,
// should leave you still typing where you were — but a <button> grabs focus
// on mousedown by default, so the next keystroke went to the button instead
// of the shell. Preventing that one default keeps focus put; the click still
// fires, and Tab-to-focus is untouched, so keyboard users are not locked out.
// (The pane-header icons are <span>s, which are not focusable, so they never
// had this problem — only these real buttons do.)
for (const id of ["projbtn", "wtbtn", "bell", "settings", "refresh", "closeproj"]) {
  const b = document.getElementById(id);
  if (b) b.addEventListener("mousedown", (e) => e.preventDefault());
}

// Any open header popup closes when you click outside it and its trigger.
// Capture phase for two reasons: it runs before xterm (or any inner handler)
// can stopPropagation the event, and it runs before the trigger's own click
// toggles the panel — so a click on the trigger is seen as "inside the
// trigger" and left alone, and opening a popup never immediately re-closes it.
const HEADER_POPUPS = [
  ["projbtn", "projpanel"],
  ["wtbtn", "wtpanel"],
  ["bell", "noticepanel"],
];
document.addEventListener(
  "mousedown",
  (e) => {
    for (const [btnId, panelId] of HEADER_POPUPS) {
      const panel = document.getElementById(panelId);
      const btn = document.getElementById(btnId);
      if (panel && !panel.hidden && !panel.contains(e.target) && btn && !btn.contains(e.target)) {
        panel.hidden = true;
      }
    }
  },
  true,
);
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
// Scoped to this project by the server: `Notices` on connect and `Notice`
// live both carry only this project's rows (hub::broadcast_to_project,
// notify::list_for). So everything derived from this array — the panel, the
// bell badge, the title prefix, the favicon dot — is about this project and
// nothing else, and a click can always be answered in this tab.
//
// A worktree is its own project key, so a tab on `roost` gets none of
// `roost/.claude/worktrees/claude-1`'s notices either. That is intended: the
// two are separate workspaces with separate terminals.
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
    (e) => console.warn("roost: service worker registration failed", e)
  );
  navigator.serviceWorker.addEventListener("message", (e) => {
    if (e.data && e.data.kind === "focus") focusSession(e.data.session);
  });
}

function unread() { return notices.filter((n) => !n.read).length; }

// The bell's Claude-hooks state. Three values, never two: `null` means the
// server could not read or parse the settings file, and that gets a reason
// and no button, not a guess.
let hookConfirm = null; // "on" | "off" while a confirmation is showing
function hookState() {
  if (!state || state.claude_hooks === undefined) return "unknown";
  return state.claude_hooks === null ? "unknown" : (state.claude_hooks ? "on" : "off");
}
function renderClaudeHooks() {
  const bell = document.getElementById("bell");
  if (!bell) return;
  const s = hookState();
  bell.dataset.claudeHooks = s;
  const word = { on: "on", off: "off", unknown: "cannot tell" }[s];
  bell.title = `notifications (n) · Claude notifications for this project: ${word}`;
}
// The panel's first row: state, and the switch behind a one-line
// confirmation, because it writes into a file roost does not own.
function hookRow() {
  const row = document.createElement("div");
  row.className = "hookrow";
  const s = hookState();
  const label = document.createElement("span");
  if (s === "unknown") {
    label.textContent = "Claude notifications: cannot tell — .claude/settings.local.json could not be read or parsed";
    row.appendChild(label);
    return row;
  }
  label.textContent = `Claude notifications for this project: ${s}`;
  row.appendChild(label);
  if (hookConfirm) {
    const c = document.createElement("span");
    c.className = "confirm";
    const q = document.createElement("span");
    q.textContent = hookConfirm === "on"
      ? "Write two hooks to .claude/settings.local.json? "
      : "Remove roost's hooks from .claude/settings.local.json? ";
    const yes = document.createElement("button");
    yes.textContent = hookConfirm === "on" ? "Enable" : "Disable";
    yes.onclick = (e) => { e.stopPropagation(); send({ t: "SetClaudeHooks", on: hookConfirm === "on" }); hookConfirm = null; renderNotices(); };
    const no = document.createElement("button");
    no.textContent = "Cancel";
    no.onclick = (e) => { e.stopPropagation(); hookConfirm = null; renderNotices(); };
    c.append(q, yes, no);
    row.appendChild(c);
    return row;
  }
  const b = document.createElement("button");
  b.textContent = s === "on" ? "Disable" : "Enable";
  b.onclick = (e) => { e.stopPropagation(); hookConfirm = s === "on" ? "off" : "on"; renderNotices(); };
  row.appendChild(b);
  return row;
}

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
  panel.appendChild(hookRow());
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
  renderClaudeHooks();
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

// Every notice in the panel belongs to this project, so a click always
// resolves here. The old cross-project branch navigated this tab to the other
// project — which is what made a notice from a project you were not looking
// at destroy the workspace you were: one click and ultima_cluster was
// replaced by roost. The guard stays as a guard, not a route: if a foreign
// notice ever reaches this array again (a server regression), it is ignored
// rather than silently hijacking the tab.
function openNotice(x) {
  if (x.project !== PROJECT) return;
  if (!x.read) send({ t: "MarkNoticeRead", id: x.id });
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
  lastFocusedSession = session;
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
// nothing ever removes from it — both were real bugs here). The PROJECT
// check is belt-and-braces now that the server sends this project's notices
// only, and kept because `notices` also feeds the badge and the title: a
// foreign row leaking back in must light nothing rather than light
// everything.
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

// --- project search (⇧⇧) ---------------------------------------------------
//
// Double-tap Shift, IntelliJ-style. Two properties make it safe to arm on the
// document even while a terminal has focus, which is where focus usually is:
//
//   - Shift alone emits nothing to a shell, so intercepting it steals no
//     keystroke. Any Ctrl-/Cmd- chord would have to be taken away from the
//     program running in the terminal instead.
//   - The two presses must be consecutive. Typing "HI" presses Shift twice in
//     quick succession, but the H lands between them and resets the pending
//     state, so ordinary typing cannot open this.
//
// 400 ms was the first guess and it was too tight to use: measured against a
// real browser through CDP's input pipeline, a 350 ms gap opened the overlay
// and a 450 ms gap did not. 700 ms is roughly a Windows double-click default
// with room to spare, and it costs no safety: the intervening-key reset above
// is what stops ordinary typing from reaching here, not the length of the
// window.
//
// **This shortcut cannot be relied on, and ⌘⇧F / Ctrl+Shift+F is the one the
// header advertises.** On the deploy host's own browser a lone Shift keydown
// never reaches the page at all: a capture-phase listener on `document` —
// which sees every event the page receives, before anything could stop it —
// printed nothing for Shift, while Ctrl+Shift+F worked in the same tab, so
// Shift *as a modifier* arrives normally. Something between the keyboard and
// the browser (a remapper, an extension, an input setting) swallows standalone
// modifier presses there.
//
// Widening this window was a wasted fix aimed at that report; the window was
// genuinely too small, but it was never the cause. Left bound because it works
// wherever it can be tested and cannot false-fire, but do not spend time on it
// again without first checking that a bare Shift produces a keydown at all.
const SHIFT_GAP_MS = 700;
let shiftPending = 0;
let searchSeq = 0;
// The query the in-flight `searchSeq` was sent for. The note below reports
// whether *these results* had their contents searched, which is a fact about
// the query the server actually ran — not about whatever is in the box now.
// Reading the live input instead let the caveat flicker for a debounce
// window: results for a 2-character query render while the box already holds
// 3, and the note disagrees with the rows under it.
let searchSentQuery = "";
let searchRows = [];      // [{kind, rel, line, session}] parallel to the DOM rows
let searchSel = 0;
let searchDebounce = null;
let searchReturnFocus = null;

document.addEventListener("keydown", (e) => {
  if (e.key !== "Shift") { shiftPending = 0; return; }
  if (e.repeat) return;   // holding Shift down is one press, not many
  const now = Date.now();
  if (shiftPending && now - shiftPending < SHIFT_GAP_MS) {
    shiftPending = 0;
    openSearch();
    return;
  }
  shiftPending = now;
});

// ⌘⇧F / Ctrl+Shift+F. Added because ⇧⇧ was reported as not working in a real
// browser and stayed broken after the double-tap window was widened — the
// cause is still unknown, so this is a second way in rather than a
// replacement, and ⇧⇧ above is left exactly as it is.
//
// Shift is what makes the chord affordable. A terminal encodes Ctrl-F and
// Ctrl-Shift-F identically — both are `^F` — so binding the shifted one takes
// *nothing* from the shell: plain Ctrl-F still reaches readline as
// forward-char, and xterm needs no special case. That is the whole reason to
// prefer it over the plain chord, which would have had to be refused in
// `attachCustomKeyEventHandler` and would have cost `^F` at every prompt.
//
// `metaKey` on a Mac and `ctrlKey` elsewhere, the same platform split
// `linkModifier` uses — and deliberately not Alt, which readline binds heavily
// and which *composes* on macOS (see the mention handler above, reduced to
// matching the literal `˚` that Option+K produces).
//
// preventDefault takes the browser's own find bar, which is the point: it
// would only ever search the pane that happens to be rendered.
// Ctrl OR Meta, not `IS_MAC ? meta : ctrl`. The platform split is right for
// `linkModifier`, where ⌘-click and Ctrl-click are genuinely different
// gestures — but for a chord there is no conflict to resolve, and the split
// adds a way to fail silently: `IS_MAC` reads `navigator.platform`, which is
// deprecated and returns "" or a frozen value in some browsers, and a
// misdetection makes the handler demand a modifier the user is not pressing.
// Nothing is gained by being strict here.
//
// `e.code` first for the same reason the mention handler ends up matching a
// literal `˚`: `e.key` under a modifier is not dependable across layouts.
document.addEventListener("keydown", (e) => {
  if (e.code !== "KeyF" && e.key !== "f" && e.key !== "F") return;
  if (!e.shiftKey || e.altKey) return;
  if (!e.ctrlKey && !e.metaKey) return;
  e.preventDefault();
  openSearch();
});

/// The panel hangs from the search field, so it has to be centred on the
/// FIELD — not the viewport. `#searchbox` uses `margin: auto` inside a flex
/// header, which centres it in whatever space the left and right button
/// groups leave over; those groups are different widths, so the field sits
/// ~55px left of centre and a viewport-centred panel is visibly lopsided
/// against it. Clamped so the panel cannot run off either edge on a narrow
/// window.
function anchorSearchPanel() {
  const box = document.getElementById("searchbox");
  const panel = document.querySelector(".searchpanel");
  if (!box || !panel) return;
  const half = panel.offsetWidth / 2;
  const cx = box.getBoundingClientRect().left + box.offsetWidth / 2;
  const clamped = Math.min(Math.max(cx, half + 8), window.innerWidth - half - 8);
  document.documentElement.style.setProperty("--search-cx", `${Math.round(clamped)}px`);
}

/// Panel visibility only — no focus effects. Separate from openSearch because
/// the field now lives in the header and is focusable on its own: a user can
/// have focus in it with no panel showing, and emptying the box must close the
/// panel without yanking focus away mid-edit.
function showSearchPanel() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || !ov.hidden) return;
  ov.hidden = false;
  document.body.classList.add("searching");
  // anchorSearchPanel() needs the panel laid out (offsetWidth) to measure
  // it, which is only true after un-hiding — measuring before this line
  // would read the old, possibly-zero width from while it was `hidden`.
  anchorSearchPanel();
}

// Re-anchor live while the panel is open: the field's on-screen position is
// a function of viewport width (the header's flex layout reflows it), so a
// resize without this would leave the panel pointing at where the field
// used to be.
window.addEventListener("resize", () => {
  if (!document.body.classList.contains("searching")) return;
  anchorSearchPanel();
});

function hideSearchPanel() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || ov.hidden) return;
  ov.hidden = true;
  document.body.classList.remove("searching");
  // Repaints empty rather than just clearing the searchRows array: the query
  // is NOT cleared from the field on close (see openSearch below), so a
  // later reopen can find #searchresults still holding THIS query's rendered
  // rows while searchRows says there are none. That disagreement is not
  // cosmetic — activateSearchRow looks a row up by index in searchRows, so
  // ↑/↓/Enter against the stale DOM silently do nothing (`if (!r) return;`)
  // until the user types again. renderSearch(null) is what the erase path a
  // few lines below already uses to keep the two in sync; hiding must too.
  renderSearch(null);
  // Dismissing mid-debounce must not let the pending Search still fire: its
  // reply would repopulate searchRows and the (now hidden) result list from a
  // query the user no longer has open.
  clearTimeout(searchDebounce);
  searchSeq++;
}

/// The chord and a click both land here. The query is deliberately NOT
/// cleared — it is selected instead, so typing replaces it but refining after
/// a miss does not mean retyping.
function openSearch(returnFocus) {
  const input = document.getElementById("searchinput");
  if (!input) return;
  // Guarded: pressing the chord while already in the field must not remember
  // the field itself as the place to give focus back to, which would strand
  // focus here forever.
  if (document.activeElement !== input) {
    searchReturnFocus = returnFocus !== undefined ? returnFocus : document.activeElement;
  }
  input.focus();
  input.select();
  // Re-issued, not just revealed: hideSearchPanel() above always leaves
  // searchRows (and the list) empty, so a bare showSearchPanel() here would
  // show a field full of text over an empty list until the user's next
  // keystroke — worse than the old design, where reopening at least cleared
  // the box to match. Dispatching `input` routes through the field's own
  // handler so this inherits its debounce, its searchSeq bump, and its
  // disconnected-socket branch instead of duplicating any of that here.
  if (input.value) input.dispatchEvent(new Event("input", { bubbles: true }));
}

function closeSearch() {
  hideSearchPanel();
  // .focus() on an element no longer in the document does not throw — it
  // silently no-ops and focus falls to <body>. A State broadcast can detach
  // the remembered terminal node while the overlay is open (a tabstrip
  // re-render), so this has to be checked explicitly rather than trusted to
  // fail loudly.
  if (searchReturnFocus && document.contains(searchReturnFocus)) searchReturnFocus.focus();
  searchReturnFocus = null;
}

// `focusin` carries relatedTarget: the element that just lost focus, which is
// exactly what closing must give back. The <button> needed a mousedown handler
// to capture this before it stole focus for itself; a real input receives
// focus directly, so that bookkeeping goes away.
document.getElementById("searchinput")?.addEventListener("focusin", (e) => {
  if (e.relatedTarget && e.relatedTarget !== e.target) searchReturnFocus = e.relatedTarget;
  if (e.target.value) showSearchPanel();
});

document.getElementById("searchinput")?.addEventListener("input", (e) => {
  const q = e.target.value;
  clearTimeout(searchDebounce);
  // Debounced, because every keystroke is a walk. The server drops answers to
  // queries the user has already typed past, but not sending them at all is
  // cheaper than cancelling them.
  searchDebounce = setTimeout(() => {
    // Erasing the query bumps searchSeq too: a reply to the just-abandoned
    // query must not paint over the now-empty box, the same reason the send
    // branch below bumps it.
    if (!q) { searchSeq++; renderSearch(null); hideSearchPanel(); return; }
    if (!ctrl || ctrl.readyState !== 1) {
      // send() would silently no-op here; without this the box would just
      // sit there showing stale rows (or nothing), which reads as "no
      // matches" when the truth is "never asked". The panel can still be
      // hidden at this point (a chord into an empty field never opens it),
      // so it must be shown here too or the note is painted where nobody
      // can see it.
      searchSeq++;
      showSearchPanel();
      renderSearchDisconnected();
      return;
    }
    searchSentQuery = q;
    showSearchPanel();
    send({ t: "Search", q, seq: ++searchSeq });
  }, 120);
});

// Bound to the document, not to #searchoverlay, and gated on the panel being
// open OR on focus being in the field. The overlay contains exactly one
// focusable element (the input), so focus leaves it trivially — one Tab, or a
// click on any non-row part of the panel, which the backdrop handler below
// deliberately does not treat as a dismissal. With the listener scoped to the
// overlay, focus landing on <body> took Escape, ↑/↓ and Enter with it, and
// `openSearch` early-returns on an already-open overlay, so ⇧⇧ could not
// recover either: the modal was stranded open with only a backdrop click left
// to close it. Trapping Tab instead would have fixed only the first of those
// two routes. The field now lives in the header, outside the overlay by
// construction, so focus starting there — with no panel open yet — is a
// reachable state that Escape must still cover.
document.addEventListener("keydown", (e) => {
  const ov = document.getElementById("searchoverlay");
  const input = document.getElementById("searchinput");
  const open = ov && !ov.hidden;
  const inField = input && document.activeElement === input;
  if (!open && !inField) return;
  if (e.key === "Escape") { e.preventDefault(); closeSearch(); return; }
  if (!open) return;
  if (e.key === "ArrowDown") { e.preventDefault(); moveSearchSel(1); return; }
  if (e.key === "ArrowUp") { e.preventDefault(); moveSearchSel(-1); return; }
  if (e.key === "Enter") { e.preventDefault(); activateSearchRow(searchSel); }
});

// Clicking the backdrop closes; clicking the panel does not.
document.getElementById("searchoverlay")?.addEventListener("mousedown", (e) => {
  if (e.target.id === "searchoverlay") closeSearch();
});

function moveSearchSel(d) {
  if (!searchRows.length) return;
  searchSel = (searchSel + d + searchRows.length) % searchRows.length;
  paintSearchSel();
}

function paintSearchSel() {
  const rows = document.querySelectorAll("#searchresults .searchrow");
  rows.forEach((n, i) => n.classList.toggle("sel", i === searchSel));
  rows[searchSel]?.scrollIntoView({ block: "nearest" });
}

/// Splits `docs/specs/x.md` into ["docs/specs/", "x.md"]. The trailing slash
/// stays on the directory so the two halves concatenate back to the original
/// — the ellipsis lands after it, not instead of it.
function splitPath(rel) {
  const i = rel.lastIndexOf("/");
  return i < 0 ? ["", rel] : [rel.slice(0, i + 1), rel.slice(i + 1)];
}

/// ASCII-only lowercase, matching `to_ascii_lowercase` at src/search.rs:183.
/// `String.toLowerCase` is wrong here twice over: it folds beyond ASCII, which
/// the server does not, and it can change a string's length ('İ' folds to two
/// code units) — so an index found in the folded text would not map back onto
/// the original, and the chip would land on the wrong characters.
function lowerAscii(s) { return s.replace(/[A-Z]/g, (c) => c.toLowerCase()); }

/// Appends `text` into `host`, wrapping each occurrence of `q` in a chip.
/// Text nodes and createElement only — never a built-up markup string. The
/// whole reason this function exists is the reason it must not interpolate:
/// `text` is a line out of a file in the project.
///
/// A query that does not occur (a path matched as a subsequence, a match past
/// the server's 300-character line cap) simply appends the text unmarked. That
/// is a row without a chip, not an error and not a missing row.
function appendHighlighted(host, text, q) {
  const needle = lowerAscii(q || "");
  if (!needle) { host.appendChild(document.createTextNode(text)); return; }
  const hay = lowerAscii(text);
  let i = 0;
  for (;;) {
    const at = hay.indexOf(needle, i);
    if (at < 0) break;
    if (at > i) host.appendChild(document.createTextNode(text.slice(i, at)));
    const mark = document.createElement("span");
    mark.className = "hit";
    mark.textContent = text.slice(at, at + needle.length);
    host.appendChild(mark);
    i = at + needle.length;
  }
  host.appendChild(document.createTextNode(text.slice(i)));
}

/// One result row: a path cell on a fixed left column, then the text that
/// matched. Every dynamic part is a text node — a matched line is arbitrary
/// file content and a path is arbitrary filesystem content, which makes these
/// the most attacker-influenced strings this client renders. The innerHTML
/// rule at the top of this file is the whole defence, and it only holds if
/// nothing here interpolates.
///
/// The path is three spans, not two: a `dir:base` two-span version still
/// ellipsises from the tail once the FILENAME alone is wider than the
/// column — `.base` was `flex: none` so it never shrank, but `text-overflow`
/// on a non-shrinking element does nothing, and the element simply overflows
/// its cell and gets clipped by `.at`'s own `overflow: hidden`, eating the
/// line number off the right end. A search for `first` in this repo showed
/// exactly that: seven rows all reading the same clipped basename, no line
/// number differentiating any of them — the one property this cell exists
/// to preserve, gone.
///
/// So the line number is its own span, `flex: none`, and never shrinks. The
/// filename is `.name`, and the directory `.dir` — both `flex: 0 _ auto`
/// with `text-overflow: ellipsis` — but `.dir` carries a higher
/// flex-shrink than `.name` (see style.css), so the directory gives up
/// space first and the filename only starts clipping once the directory has
/// nothing left to give. Priority, highest first: the line number, then the
/// filename, then the directory.
function searchRow(dir, name, line) {
  const row = document.createElement("div");
  row.className = "searchrow";

  const at = document.createElement("span");
  at.className = "at";
  const d = document.createElement("span");
  d.className = "dir";
  d.textContent = dir;
  const n = document.createElement("span");
  n.className = "name";
  n.textContent = name;
  const ln = document.createElement("span");
  ln.className = "line";
  ln.textContent = line;
  at.append(d, n, ln);

  const what = document.createElement("span");
  what.className = "what";

  row.append(at, what);
  return row;
}

// The "socket is down" case send() handles by silently dropping the intent —
// right for most callers, wrong here, where silence reads as "no matches"
// instead of "never asked".
function renderSearchDisconnected() {
  const host = document.getElementById("searchresults");
  const note = document.getElementById("searchnote");
  if (host) host.textContent = "";
  searchRows = [];
  searchSel = 0;
  if (note) note.textContent = "not connected";
  // A socket that was never asked IS a gap — the strongest one this line can
  // report — so it must wear the same mark a partial answer does, not
  // whatever class the previous query happened to leave behind.
  if (note) note.classList.add("skipped");
}

function renderSearch(results) {
  const host = document.getElementById("searchresults");
  const note = document.getElementById("searchnote");
  if (!host) return;
  host.textContent = "";
  note.textContent = "";
  // Cleared alongside the text, not just at the bottom of this function: the
  // early return below (an emptied query) must not leave a PREVIOUS query's
  // gap mark on an otherwise-blank note.
  note.classList.remove("skipped");
  searchRows = [];
  searchSel = 0;
  if (!results) return;

  const group = (label) => {
    const g = document.createElement("div");
    g.className = "searchgroup";
    g.textContent = label;
    host.appendChild(g);
  };

  // The text cell always holds the thing that matched — the filename for a
  // file hit, the line for a content hit, the name for a session — so the
  // chip Task 2 adds always lands in the same column.
  if (results.files.length) {
    group(`Files (${results.files.length})`);
    for (const f of results.files) {
      const [dir, base] = splitPath(f.rel);
      const row = searchRow(dir, "", "");
      appendHighlighted(row.querySelector(".what"), base, searchSentQuery);
      host.appendChild(row);
      searchRows.push({ kind: "file", rel: f.rel });
    }
  }
  if (results.sessions.length) {
    group(`Sessions (${results.sessions.length})`);
    for (const s of results.sessions) {
      const row = searchRow("terminal", "", "");
      appendHighlighted(row.querySelector(".what"), s, searchSentQuery);
      host.appendChild(row);
      searchRows.push({ kind: "session", session: s });
    }
  }
  if (results.lines.length) {
    group(`Contents (${results.lines.length})`);
    for (const l of results.lines) {
      const [dir, base] = splitPath(l.rel);
      const row = searchRow(dir, base, `:${l.line}`);
      appendHighlighted(row.querySelector(".what"), l.text.trim(), searchSentQuery);
      host.appendChild(row);
      searchRows.push({ kind: "line", rel: l.rel, line: l.line });
    }
  }

  // The honesty line. "No matches" and "I could not look everywhere" are
  // different answers, and only this element can tell them apart — which is
  // the whole reason `Results` carries an outcome instead of being a list.
  const parts = [];
  if (results.outcome.state === "Failed") parts.push(`search failed: ${results.outcome.msg}`);
  if (results.outcome.state === "Truncated") parts.push(`partial results — ${results.outcome.reason}`);
  if (results.unreadable) {
    parts.push(`${results.unreadable} ${results.unreadable === 1 ? "place" : "places"} could not be read`);
  }
  // A decision, not a gap — but the user cannot tell those apart from an answer
  // that simply does not mention them, which is why it is on this line at all.
  // A nested checkout (a worktree, a submodule) holds real source someone may
  // have expected to see; the walk declines it because its files are another
  // project's, and for a worktree they are near-copies of the ones already
  // listed here.
  if (results.skipped_nested) {
    const n = results.skipped_nested;
    parts.push(`${n} nested ${n === 1 ? "checkout" : "checkouts"} not searched`);
  }
  if (!parts.length && !searchRows.length) parts.push("no matches");
  // The other half of the honesty line, and the one the server cannot supply:
  // below three characters wsconn.rs sets `Query::contents = false`, so the
  // walk never opens a single file — yet it truthfully reports `Complete` for
  // the categories it *did* search, and the note above would therefore be
  // empty. That is "I chose not to look" rendered as completeness, which is
  // the one thing this line exists to prevent. Appended rather than
  // substituted, so it composes with a Truncated reason or an unreadable
  // count instead of hiding one.
  //
  // Counted in code points, matching the server's `q.chars().count() >= 3`
  // exactly — a threshold that disagreed with the server's would announce the
  // wrong thing for an emoji or an accented query. Not shown for an empty
  // box: nothing was searched there at all, and the erase path renders
  // through `renderSearch(null)` above anyway.
  //
  // `searchSentQuery`, not the live input: this reports what the server did
  // for the results being rendered, and the box can have moved on during the
  // debounce.
  const q = searchSentQuery;
  if (q && [...q].length < 3) parts.push("contents searched from 3 characters");
  note.textContent = parts.join(" · ");

  // The mark means "something is missing from this answer", so the one note
  // that is a complete answer — nothing found, nothing skipped, nothing
  // failed — must not carry it.
  const gap = parts.length > 0 && !(parts.length === 1 && parts[0] === "no matches");
  note.classList.toggle("skipped", gap);

  if (searchRows.length) paintSearchSel();
}

function activateSearchRow(i) {
  const r = searchRows[i];
  if (!r) return;
  closeSearch();
  if (r.kind === "session") {
    send({ t: "OpenTab", pane: 3, tab: { k: "Terminal", session: r.session } });
  } else if (r.kind === "line") {
    // Only the client that pressed Enter on this row gets focus stolen into
    // the editor once RevealLine comes back — see revealLine()'s focus
    // parameter.
    focusNextReveal = true;
    send({ t: "OpenAtLine", pane: 2, rel: r.rel, line: r.line });
  } else {
    // Same rule the file tree uses (defaultMode, line 145): a rendered form
    // opens in Preview, everything else opens in Edit. Hardcoding Preview
    // here would open the same file differently depending on which path the
    // user clicked it from, and the server does not correct it — coerce_tab
    // only ever demotes Edit to Preview, never promotes back.
    send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: r.rel, mode: defaultMode(r.rel) } });
  }
}

document.getElementById("searchresults")?.addEventListener("click", (e) => {
  const row = e.target.closest(".searchrow");
  if (!row) return;
  const rows = [...document.querySelectorAll("#searchresults .searchrow")];
  activateSearchRow(rows.indexOf(row));
});

// Set true by whichever intent (OpenAtLine from a search row, or a
// line-suffixed OpenPath from a terminal link) is about to provoke a
// RevealLine broadcast from this client, and consumed exactly once by the
// "RevealLine" case in onEvent. RevealLine is broadcast to every browser on
// the tab on purpose (a second browser mirroring the view follows the
// scroll), but focus is not scroll: a mirroring browser's user may be typing
// in an unrelated terminal or editor right now, and wireEditor's blur
// listener would autosave whatever they were in the moment focus is yanked
// away. Everyone gets scrolled and selected; only the client that actually
// asked gets focus.
let focusNextReveal = false;

// {rel, line, focus} for a reveal that has not finished landing anywhere yet,
// or null. Armed by revealLine(), consumed by tryReveal() — which is called
// again from mountTab's fetch completion, because a Preview pane's fetch can
// still be in flight when RevealLine arrives (the State event that creates
// the tab only starts that fetch, it does not wait for it).
//
// A single slot, last-wins: a second RevealLine that arrives before the
// first one's target pane finishes mounting replaces it outright, and the
// first is never applied anywhere. Deliberate, not an oversight — RevealLine
// only ever follows a user-driven OpenAtLine or line-suffixed OpenPath, so
// two in flight on one client at once means two of those landed back to
// back, and jumping to wherever the most recent one points is the same
// "whatever the user is looking at now wins" rule SearchResults' own `seq`
// guard already applies elsewhere in this file — not a queue holding onto
// something the user has moved past.
let pendingReveal = null;
let pendingRevealTimer = null;
// Sweep-scoped, not per-pane: scrollEditorTo() runs once per still-unrevealed
// pane in a single tryReveal() sweep, and without this a frame with k panes
// stuck on an unsynced <pre> schedules k retries, each of which schedules k
// more on its own next sweep — a runaway that a hung tab's unsaved buffers
// would ride along with. Cleared at the top of tryReveal() so a retry that
// is actually still needed after this sweep can still be scheduled once.
let revealRetryScheduled = false;
function scheduleRevealRetry() {
  if (revealRetryScheduled) return;
  revealRetryScheduled = true;
  requestAnimationFrame(() => tryReveal());
}

/// Scroll whichever pane holds `rel` to `line`, and select it where the
/// surface has a selection to give it (the editor). There is no flash here:
/// the editor's selection and the preview's centered scroll are the only
/// feedback either surface has — search's own result list and the
/// terminal's line flash (termFlash, in openTermPath) cover the rest.
///
/// Three surfaces, and only two of them have lines. A code preview is a
/// single <pre class="codeview"> with no per-line elements, but `.codeview`
/// sets no white-space override, so <pre>'s default `white-space: pre`
/// applies and one source line is exactly one visual line — which is what
/// makes the arithmetic in revealInPreview() exact. A *rendered* form —
/// markdown, an image, a read-error placeholder — has no line mapping at
/// all: tryReveal() finds neither a textarea nor a `pre.codeview` for that
/// pane and leaves it alone rather than guessing at a scroll position that
/// would be meaningless there.
///
/// `focus` is per-caller, not per-event: see the comment on
/// `focusNextReveal` above.
function revealLine(rel, line, focus) {
  pendingReveal = { rel, line, focus };
  clearTimeout(pendingRevealTimer);
  // Reached only when tryReveal() below (and every retry from mountTab's
  // fetch completion) never got every matching pane to a `pre.codeview` or
  // `textarea.editor` within this window — the ordinary cases (a fetch that
  // lands, or a rendered form with genuinely no line mapping) clear
  // pendingReveal themselves, well before this fires. Reaching here means
  // some pane matching `rel` is still stuck empty: closed before its fetch
  // landed, or some other path this client cannot observe. Silence here
  // would be exactly the failure mode CLAUDE.md calls out — treating "I
  // could not tell" as nothing having gone wrong — so it says so instead.
  // 4000ms is not measured against any real fetch latency; it is an
  // arbitrary, generously long backstop, chosen only so a stale target
  // can't sit armed indefinitely (see the comment above pendingReveal).
  pendingRevealTimer = setTimeout(() => {
    // Not in a hidden tab. RevealLine is broadcast, so a browser that is
    // merely mirroring someone else's navigation arms this timer too — and
    // in a background tab code-input's rAF loop is throttled to a stop, so
    // scrollEditorTo's sync guard (`pre.textContent.length < ta.value.length`)
    // never clears, the retry never lands, and this fires every time. The
    // result was a user who did nothing being shown an error about someone
    // else's navigation. `pendingReveal` is still cleared either way: the
    // target is stale regardless of who can see the banner.
    if (!document.hidden) {
      showBanner(`couldn't scroll to line ${line} of ${rel} — its tab may have closed, or never finished opening`);
    }
    pendingReveal = null;
  }, 4000);
  tryReveal();
}

/// Applies `pendingReveal`, if any, to every currently-mounted pane that
/// matches — every pane, not just the first: a file open in two panes at
/// once must scroll both, and which one a DOM-order `return` would have hit
/// first is an implementation detail no user should have to think about.
///
/// Marks each pane it actually reveals into (`content._revealedFor`) so a
/// later call for the *same* pendingReveal — mountTab calls this again once
/// some other, still-loading pane's fetch finally lands — does not re-apply
/// to a pane that already got it, which would otherwise re-snap the scroll
/// (and, for the focused client, re-select) out from under a user who
/// scrolled away in the meantime. A new revealLine() call always creates a
/// fresh pendingReveal object, so this marker never blocks a genuinely new
/// reveal of the same pane later.
function tryReveal() {
  revealRetryScheduled = false;
  if (!pendingReveal) return;
  const { rel, line, focus } = pendingReveal;
  let stillWaiting = false;
  for (const content of document.querySelectorAll(".pane .content")) {
    if (content._revealedFor === pendingReveal) continue;
    const ta = content.querySelector("textarea.editor");
    if (ta && editorRel(content) === rel) {
      // A highlighted file's <pre> can still be one animation frame behind a
      // fresh mount (see scrollEditorTo's doc comment) — revealInEditor
      // reports that back rather than silently landing wherever a stale
      // layout happened to be, and schedules its own retry. Leaving
      // _revealedFor unset here (and stillWaiting true) means the 4s banner
      // in revealLine() still fires if that retry never lands, instead of
      // this pane sitting unrevealed forever with no visible sign of it.
      if (revealInEditor(ta, line, focus)) {
        content._revealedFor = pendingReveal;
      } else {
        stillWaiting = true;
      }
      continue;
    }
    if (!matchesRel(content, rel)) continue;
    const pre = content.querySelector("pre.codeview");
    if (pre) {
      revealInPreview(content, pre, line);
      content._revealedFor = pendingReveal;
      continue;
    }
    // This pane's fragment is rel's, but neither surface is mounted. An
    // empty innerHTML means mountTab's fetch hasn't landed yet — worth
    // another pass once it does (mountTab calls tryReveal() itself when it
    // finishes). A non-empty one means the fetch already landed on a
    // rendered form with no line mapping (markdown, an image, a read-error
    // placeholder) — nothing will ever match here, and there is nothing
    // further to wait for.
    if (!content.innerHTML.trim()) stillWaiting = true;
  }
  if (!stillWaiting) {
    pendingReveal = null;
    clearTimeout(pendingRevealTimer);
  }
}

/// Sets the selection and, if `focus` is set, steals focus — then scrolls
/// via scrollEditorTo(), reporting back whether that scroll actually landed
/// (see there) so tryReveal() knows whether to retry rather than mark this
/// pane done.
///
/// Selection and focus are unconditional and immediate: neither depends on
/// any layout being settled, so there is nothing to gain by deferring them,
/// and setting them right away means the caret is in the right place even in
/// the one frame before a highlighted file's scroll catches up.
///
/// Native focus-driven scrolling used to be this function's only mechanism
/// for the focused case, on the theory that focusing a textarea with an
/// active selection makes the browser scroll it into view for free. Measured
/// two ways this turned out not to hold: it only fires on an actual focus
/// *transition* (searching within a file that is already open and focused —
/// a real, common case, not an edge case — calls focus() on an element that
/// is already document.activeElement, so no such event exists to trigger
/// it), and even when a transition does happen, a freshly mounted highlighted
/// file's <pre> may not have finished laying out yet (see scrollEditorTo).
/// scrollEditorTo replaces it with something that does not depend on focus
/// at all.
function revealInEditor(ta, line, focus) {
  const lines = ta.value.split("\n");
  const upto = lines.slice(0, Math.max(0, line - 1)).join("\n").length + (line > 1 ? 1 : 0);
  const end = upto + (lines[line - 1] || "").length;
  ta.setSelectionRange(upto, end);
  if (focus) ta.focus();
  return scrollEditorTo(ta, upto);
}

/// Scrolls `ta`'s scrollable ancestor so the character at `offset` is
/// visible, roughly a third of the way down. Returns whether it actually
/// could — false means "try again next frame", not "gave up".
///
/// Two branches, not one, because only a highlighted file has something to
/// measure. `.editor` sets no white-space override (static/style.css) so a
/// plain textarea soft-wraps by default, and `.editwrap code-input`
/// explicitly sets `white-space: pre-wrap`, which the vendor stylesheet's
/// `code-input textarea, code-input pre { white-space: inherit }` pushes
/// onto both of its layers — so in *both* editor shapes one logical line is
/// not reliably one visual row, unlike the preview's <pre> (see
/// revealInPreview for why that one is exact).
///
/// For a highlighted file, code-input's own host (the `<code-input>` element
/// itself) has a `<pre>` beneath the textarea as a light-DOM child — not
/// shadow DOM; code-input.min.js never calls attachShadow — containing the
/// same text, wrapped identically (both layers share the same white-space
/// rule and width). Walking that `<pre>`'s rendered text to `offset` and
/// measuring a collapsed Range there answers the wrapping question exactly,
/// instead of guessing at visual rows from a character count the way the
/// plain-textarea branch below has to; `getBoundingClientRect()` forces a
/// synchronous layout, so the measurement is never stale relative to
/// whatever is currently in the DOM.
///
/// "Currently in the DOM" is the catch: code-input highlights on the *next*
/// animation frame after a value is set (its scheduleHighlight() only flips
/// a flag; the actual `<pre>` update happens in its own perpetual
/// requestAnimationFrame loop, confirmed by reading code-input.min.js), so a
/// reveal landing in the same tick as a fresh mount can walk a `<pre>` that
/// has no text in it yet — caretRect() returns null. Scheduling a retry
/// through tryReveal() itself, not a closure over this specific pane, means
/// a pane that closed or moved on to a different file before the retry fires
/// re-resolves cleanly (tryReveal re-checks `rel` and mounted elements from
/// scratch) instead of scrolling stale state. revealLine()'s 4-second banner
/// is still the backstop if code-input's own frame loop never runs — the
/// tab closed, say.
function scrollEditorTo(ta, offset) {
  const host = ta.closest("code-input");
  const pre = host && host.querySelector("pre");
  if (pre) {
    // code-input's own update() appends one "\n" to the value before
    // highlighting, so a fully-synced <pre> has exactly one more character
    // than the textarea. Anything short of that means a fresh mount's
    // highlight genuinely has not landed yet, and caretRect() cannot tell
    // that apart from "the target is the very last character of the file" —
    // both look like "the walk ran out of text before reaching offset".
    // Checked here, independently of the walk, with a signal caretRect has
    // no way to get right on its own: without it, a reveal that lands in
    // the same tick as a fresh mount walks a <pre> holding only its initial
    // near-empty state, silently measures *that*, and reports success at
    // the wrong position instead of asking to be retried — which is exactly
    // how this looked from the outside before this check existed: `tryReveal`
    // marked the pane done and cleared `pendingReveal` on the first, bogus
    // measurement, so the correct one a frame later never got a chance to run.
    if (pre.textContent.length < ta.value.length) {
      scheduleRevealRetry();
      return false;
    }
    const rect = caretRect(pre, offset);
    if (rect && rect.height > 0) {
      const hostRect = host.getBoundingClientRect();
      host.scrollTop = Math.max(0, host.scrollTop + (rect.top - hostRect.top) - host.clientHeight / 3);
      return true;
    }
    scheduleRevealRetry();
    return false;
  }
  // No <pre> to measure: a plain (unhighlighted) textarea, which is its own
  // scroll container. This is the same wrap-blind approximation the
  // mirroring branch used to be alone in accepting — it assumes one logical
  // line is one visual row, which is false the moment a line wraps, and
  // that is not fixable without something to measure. Judged better than
  // leaving the viewport unscrolled, but it is not exact, and no comment
  // here should claim otherwise.
  const lh = parseFloat(getComputedStyle(ta).lineHeight) || 20;
  const pad = parseFloat(getComputedStyle(ta).paddingTop) || 0;
  const rows = ta.value.slice(0, offset).split("\n").length - 1;
  ta.scrollTop = Math.max(0, pad + rows * lh - ta.clientHeight / 3);
  return true;
}

/// The bounding rect of the character at `offset` within `pre`'s rendered
/// text, walking its text nodes rather than its markup: hljs wraps tokens in
/// nested `<span>`s, so a Range built against a raw offset into innerHTML
/// would land inside a tag rather than on a character. Returns null if
/// `offset` is past everything `pre` currently holds (see scrollEditorTo for
/// when that happens).
///
/// Spans exactly one character (`[at, at+1)`), never a collapsed point
/// (`[at, at)`). Measured directly: Chromium reports a degenerate
/// `{top:0,left:0,height:0,width:0}` rect for a range collapsed exactly at a
/// text node's own boundary — which `offset` lands on whenever the target
/// line starts a fresh hljs span, i.e. on most lines — while a range
/// spanning one real character never comes back degenerate. This is what
/// broke the first version of this function: it always fell exactly on that
/// boundary, so it read as "not laid out yet" on every measurement, real or
/// not, and scrollEditorTo retried forever without ever fixing anything.
function caretRect(pre, offset) {
  const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
  let node;
  let consumed = 0;
  let last = null;
  while ((node = walker.nextNode())) {
    const len = node.nodeValue.length;
    if (len === 0) continue;
    last = node;
    if (offset < consumed + len) return oneCharRect(node, offset - consumed);
    consumed += len;
  }
  // `offset` lands exactly at (or past) the end of everything walked so far
  // — the last line of the file, say. Anchor to the last real character
  // instead of the boundary past it, for the same reason as above.
  return last ? oneCharRect(last, last.nodeValue.length - 1) : null;
}

function oneCharRect(node, at) {
  const start = Math.max(0, Math.min(at, node.nodeValue.length - 1));
  const range = document.createRange();
  range.setStart(node, start);
  range.setEnd(node, start + 1);
  return range.getBoundingClientRect();
}

function revealInPreview(content, pre, line) {
  // .codeview (static/style.css) sets padding and font but no height or
  // max-height, so it grows to fit its content: pre.scrollHeight ===
  // pre.clientHeight always, and pre.scrollTop is permanently clamped to 0.
  // The element that actually scrolls is `.content` itself — `flex: 1 1
  // auto; overflow: auto`, made a definite size by `.pane`'s
  // `display:flex; flex-direction:column; overflow:hidden` — offset by the
  // <pre>'s own position within it (content.querySelector("pre.codeview")
  // was never the whole story) plus the <pre>'s own top padding, for the
  // same reason the editor branch above adds one.
  const lh = parseFloat(getComputedStyle(pre).lineHeight) || 20;
  const pad = parseFloat(getComputedStyle(pre).paddingTop) || 0;
  content.scrollTop = Math.max(0, pre.offsetTop + pad + (line - 1) * lh - content.clientHeight / 3);
}

/// A `deploy.md#running` link's heading, waiting for the tab it names to mount.
///
/// Same shape as `pendingReveal` and for the same reason: the tab's fragment is
/// fetched asynchronously, so the heading cannot be resolved at click time.
/// Deliberately NOT folded into `pendingReveal`, which matches a pane by
/// finding a `textarea.editor` or `pre.codeview` — a markdown preview is an
/// `article.markdown-body` and has neither, so one merged function would test
/// four shapes to serve two unrelated behaviours.
let pendingAnchor = null;
let pendingAnchorTimer = null;

/// Scrolls `rel`'s preview to heading `hash` once that tab has mounted.
function revealAnchor(rel, hash) {
  pendingAnchor = { rel, hash };
  clearTimeout(pendingAnchorTimer);
  // The same generous 4s backstop revealLine uses, and the same reason for
  // saying something when it fires: a link that silently does nothing is
  // exactly the bug this feature exists to fix, so failing back into silence
  // would reintroduce it in a new place. Which of the three things went wrong
  // is worked out at that point, not guessed at here.
  pendingAnchorTimer = setTimeout(() => {
    if (!pendingAnchor) return;
    pendingAnchor = null;
    if (document.hidden) return;
    const panes = [...document.querySelectorAll(".pane .content")].filter((c) => matchesRel(c, rel));
    if (!panes.length) {
      // Could not look: the tab never arrived.
      showBanner(`couldn't open ${rel} to #${hash} — its tab may have closed, or never finished opening`);
    } else if (!panes.some((c) => c.querySelector("article.markdown-body"))) {
      // Looked, and there is nothing of this kind to look at: an Edit tab is a
      // textarea, which has no headings. A distinct message because the fix is
      // different — switch the tab to Preview, not go hunting for a typo.
      showBanner(`opened ${rel}, but #${hash} is a heading — switch the tab to Preview to jump to it`);
    } else {
      // Looked and found nothing: the heading really is not there.
      showBanner(`opened ${rel}, but it has no heading #${hash} — it may have been renamed or removed`);
    }
  }, 4000);
  tryAnchor();
}

/// Applies `pendingAnchor` to the first mounted preview of its rel.
///
/// Scoped to the preview's own `article`, never `document.getElementById`.
/// Heading ids are bare GitHub slugs (render.rs's `slug`), so a document with a
/// `## Settings` emits an id the workspace chrome already owns — and a global
/// lookup would scroll to the header button instead of the heading.
function tryAnchor() {
  if (!pendingAnchor) return;
  const { rel, hash } = pendingAnchor;
  for (const content of document.querySelectorAll(".pane .content")) {
    if (!matchesRel(content, rel)) continue;
    const body = content.querySelector("article.markdown-body");
    if (!body) continue;
    // A slug is author-controlled and can legally hold characters that are not
    // valid in a selector — a heading called "1.2" slugs to `12`, but one with
    // a stray character would throw and take the whole handler with it.
    let target = null;
    try {
      target = body.querySelector(`#${CSS.escape(hash)}`);
    } catch {
      target = null;
    }
    if (!target) continue;
    target.scrollIntoView({ block: "start", behavior: "auto" });
    clearTimeout(pendingAnchorTimer);
    pendingAnchor = null;
    return;
  }
}

/// The rel an editor pane is showing. The path is already in the breadcrumb
/// as a text node, which is the only place it exists client-side once the
/// textarea is mounted.
function editorRel(content) {
  const n = content.querySelector(".editwrap .path .rel");
  return n ? n.textContent : null;
}

/// Whether `content`'s fetched fragment is a File preview of exactly `rel` —
/// an exact match on the `path` query parameter, never a substring.
/// `encodeURIComponent("main.rs")` is a substring of `?path=src%2Fmain.rs`,
/// and `src%2Fapp.js` is a substring of `src%2Fapp.js.bak`; either direction
/// of a substring test can scroll a pane that is not showing `rel` at all.
/// The `/file` suffix check matters too: a Diff tab's fragment URL
/// (`/frag/{project}/diff?path=...`) carries the same `path` param naming
/// for a different rel-adjacent thing entirely — a Diff never has a
/// `pre.codeview` to match, but without this check a Diff on the same rel
/// could still count as "still waiting" in tryReveal() and stall the 4s
/// timeout for no reason.
function matchesRel(content, rel) {
  if (!content.dataset.url) return false;
  const [path, query] = content.dataset.url.split("?");
  if (!path.endsWith("/file")) return false;
  return new URLSearchParams(query || "").get("path") === rel;
}
