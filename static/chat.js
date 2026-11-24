// The chat view of a Claude terminal (src/chat.rs). A second view of the same
// session, not a second Claude: the conversation comes from the transcript
// Claude Code writes, menus are read off the rendered screen, and everything
// typed here goes into the same PTY a keystroke would.
//
// Loaded after app.js and uses its globals (PROJECT, state, send, openUrl,
// revealPane, defaultMode, PATH_RE, URL_RE, SAFE_URL, wsUrl). Builds its own
// markup, so the workspace page carries only the <script>.
//
// Layout: the whole screen on a phone (header, switcher and key bar are
// covered, because the conversation is the job), a panel on the right on a
// desktop. Scrolling is the browser's own — momentum on iOS, nothing
// re-implemented — and stays pinned to the newest message unless you have
// scrolled up to read, in which case a ↓ pill says there is more.

const chat = {
  session: null,
  home: null,
  sock: null,
  root: null,
  log: null,
  items: new Map(), // entry id -> element
  live: null,
  retry: 0,
  retryTimer: 0,
  hidden: false,
  atts: [], // attachments since the last send: { name, image, state, el }
};

const CHAT_STICK_PX = 80;

function chatEl(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function buildChat() {
  const root = chatEl("div");
  root.id = "chat";
  root.innerHTML = `
    <div class="chathead">
      <button type="button" class="chatback" aria-label="close chat">&#8592;</button>
      <span class="chattitle"></span>
      <span class="chatmode" hidden></span>
      <button type="button" class="chatterm" title="show the terminal">terminal</button>
    </div>
    <div class="chatbanner" hidden></div>
    <div class="chatlog" aria-live="polite"></div>
    <button type="button" class="chatdown" hidden aria-label="scroll to newest">&#8595;</button>
    <div class="chatlive"></div>
    <div class="chatkeys" hidden>
      <button type="button" data-k="esc">esc</button>
      <button type="button" data-k="up" aria-label="up">&#8593;</button>
      <button type="button" data-k="down" aria-label="down">&#8595;</button>
      <button type="button" data-k="enter" aria-label="enter">&#9166;</button>
      <button type="button" data-k="shifttab" title="cycle mode">&#8679;tab</button>
      <button type="button" data-k="ctrlc">^C</button>
    </div>
    <div class="chatatts" hidden></div>
    <form class="chatform">
      <button type="button" class="chatmore" aria-label="more keys">&#8943;</button>
      <button type="button" class="chatattach" aria-label="attach a photo or file" title="attach a photo or file">&#128206;</button>
      <textarea class="chatinput" rows="1" placeholder="Message Claude" enterkeyhint="enter"
        autocomplete="off" autocapitalize="sentences"></textarea>
      <button type="submit" class="chatsend" aria-label="send">&#10148;</button>
    </form>`;
  document.body.appendChild(root);
  chat.root = root;
  chat.log = root.querySelector(".chatlog");
  const input = root.querySelector(".chatinput");

  root.querySelector(".chatback").onclick = () => closeChat();
  root.querySelector(".chatterm").onclick = () => showTerminalFromChat();
  root.querySelector(".chatmode").onclick = () => chatKey("shifttab");
  root.querySelector(".chatmore").onclick = () => {
    const k = root.querySelector(".chatkeys");
    k.hidden = !k.hidden;
  };
  for (const b of root.querySelectorAll(".chatkeys button")) {
    // pointerdown + preventDefault, as the terminal key bar does: a click
    // would move focus and, on iOS, close the keyboard mid-thought.
    b.addEventListener("pointerdown", (e) => { e.preventDefault(); chatKey(b.dataset.k); });
  }
  root.querySelector(".chatdown").onclick = () => scrollChatToEnd(true);
  chat.log.addEventListener("scroll", () => {
    if (atChatEnd()) root.querySelector(".chatdown").hidden = true;
  }, { passive: true });
  chat.log.addEventListener("click", onChatClick);
  root.querySelector(".chatlive").addEventListener("click", onChatClick);

  const grow = () => {
    input.style.height = "auto";
    input.style.height = Math.min(input.scrollHeight, window.innerHeight * 0.4) + "px";
  };
  input.addEventListener("input", grow);
  input.addEventListener("keydown", (e) => {
    // Enter sends on a desktop keyboard; a phone's Enter is a newline, and
    // sending is the button — a half-typed message sent by a stray return is
    // a turn Claude starts on.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && !phone.matches) {
      e.preventDefault();
      submitChat();
    }
  });
  root.querySelector(".chatform").addEventListener("submit", (e) => { e.preventDefault(); submitChat(); });

  root.querySelector(".chatattach").onclick = () => pickChatFiles();
  // A screenshot pasted into the message box, or files dropped anywhere on
  // the chat, are attachments too. Stopped here so the workspace's own drop
  // handler does not also answer "drop files on the Files pane".
  input.addEventListener("paste", (e) => {
    const files = e.clipboardData && e.clipboardData.files;
    if (!files || !files.length) return;
    e.preventDefault();
    attachChatFiles(files);
  });
  root.addEventListener("dragover", (e) => {
    if (dragHasFiles(e.dataTransfer)) { e.preventDefault(); e.stopPropagation(); }
  });
  root.addEventListener("drop", (e) => {
    if (!dragHasFiles(e.dataTransfer)) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.dataTransfer.files.length) attachChatFiles(e.dataTransfer.files);
  });
}

// ---- attachments ----------------------------------------------------------
//
// Two routes, chosen by what Claude can do with the file:
//
//   - A picture Claude can see (PNG, JPEG, GIF, WebP) goes through /paste,
//     exactly as a screenshot pasted onto the terminal does: roost stores it
//     outside the project and pastes its path into Claude's input, where it
//     becomes "[Image #n]" — attached to the next message, not a file Claude
//     has to be told about.
//   - Anything else (a PDF, a log, an iPhone HEIC the paste route cannot name)
//     is uploaded to the project root, as the Files pane's upload does, and
//     its name is written into the message as @name so Claude reads it.

const CHAT_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A fresh input per pick, as pickAndUpload does: a reused one fires no
/// `change` for the same file twice. No `accept`, so a phone offers the photo
/// library, the camera and files alike.
function pickChatFiles() {
  const pick = document.createElement("input");
  pick.type = "file";
  pick.multiple = true;
  pick.style.position = "fixed";
  pick.style.left = "-9999px";
  pick.onchange = () => {
    if (pick.files && pick.files.length) attachChatFiles(pick.files);
    pick.remove();
  };
  pick.oncancel = () => pick.remove();
  document.body.appendChild(pick);
  pick.click();
}

function attachChatFiles(files) {
  const list = [...files];
  if (list.length > MAX_UPLOAD_PARTS) {
    chatFlash(`${list.length} files at once (limit ${MAX_UPLOAD_PARTS})`);
    return;
  }
  for (const f of list) {
    const image = CHAT_IMAGE_TYPES.includes(f.type);
    const att = { name: f.name || (image ? "image" : "file"), image, state: "up", el: null };
    chat.atts.push(att);
    renderChatAtt(att, f);
    const url = image
      ? `/paste/${chat.home}/${chat.session}`
      : `/upload/${PROJECT}`;
    chatPost(url, f, att);
  }
}

function renderChatAtt(att, file) {
  const row = chat.root.querySelector(".chatatts");
  row.hidden = false;
  const el = chatEl("div", "chatatt up");
  if (att.image) {
    const img = chatEl("img");
    img.alt = "";
    // A data: URL, not a blob: one — the page's CSP allows `img-src 'self'
    // data:` and nothing else, and widening it for a thumbnail is not worth it.
    const rd = new FileReader();
    rd.onload = () => { img.src = rd.result; };
    rd.readAsDataURL(file);
    el.appendChild(img);
  } else {
    el.appendChild(chatEl("span", "chatatticon", "\u{1F4C4}"));
  }
  el.appendChild(chatEl("span", "chattname", att.name));
  el.appendChild(chatEl("span", "chattstate", "0%"));
  row.appendChild(el);
  att.el = el;
}

function setChatAtt(att, state, text) {
  att.state = state;
  att.el.className = `chatatt ${state}`;
  att.el.querySelector(".chattstate").textContent = text;
  if (state === "err") att.el.title = text;
}

/// One file per request, so each chip has its own progress and its own
/// error. XHR rather than fetch for the upload progress, as postFiles.
function chatPost(url, file, att) {
  const form = new FormData();
  form.append("file", file, att.name);
  const xhr = new XMLHttpRequest();
  xhr.open("POST", url);
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) setChatAtt(att, "up", `${Math.round((e.loaded / e.total) * 100)}%`);
  };
  xhr.onload = () => {
    if (xhr.status !== 200) {
      setChatAtt(att, "err", xhr.responseText || `error ${xhr.status}`);
      chatFlash(`${att.name}: ${xhr.responseText || xhr.status}`);
      return;
    }
    if (att.image) {
      setChatAtt(att, "ok", "\u2713");
      att.el.title = "attached to your next message";
      return;
    }
    let body = {};
    try { body = JSON.parse(xhr.responseText); } catch { /* checked below */ }
    const r = (body.results || [])[0];
    if (!r || !r.ok) {
      const why = (r && r.error) || "upload failed";
      setChatAtt(att, "err", why);
      chatFlash(`${att.name}: ${why}`);
      return;
    }
    setChatAtt(att, "ok", "\u2713");
    att.el.title = "uploaded to the project";
    mentionInChat(r.name || att.name);
  };
  xhr.onerror = () => {
    setChatAtt(att, "err", "upload failed");
    chatFlash(`${att.name}: upload failed`);
  };
  xhr.send(form);
}

/// Writes @name into the message where the cursor is, spaced from its
/// neighbours, so the file is part of what the user is about to send.
function mentionInChat(name) {
  const input = chat.root.querySelector(".chatinput");
  const ref = /\s/.test(name) ? `@"${name}"` : `@${name}`;
  const at = input.selectionStart ?? input.value.length;
  const before = input.value.slice(0, at);
  const after = input.value.slice(at);
  const pre = before && !/\s$/.test(before) ? " " : "";
  const post = after && !/^\s/.test(after) ? " " : " ";
  input.value = before + pre + ref + post + after;
  input.dispatchEvent(new Event("input"));
}

function clearChatAtts() {
  chat.atts = [];
  const row = chat.root.querySelector(".chatatts");
  row.replaceChildren();
  row.hidden = true;
}

/// Opens the chat on a terminal. `home` is the project the terminal's socket
/// belongs to (a re-homed session's), as for the terminal itself.
function openChat(session, home) {
  if (!SESSION_RE.test(session)) return;
  if (!chat.root) buildChat();
  const same = chat.session === session && chat.sock;
  chat.session = session;
  chat.home = home || PROJECT;
  chat.hidden = false;
  chat.root.hidden = false;
  document.body.classList.add("chatting");
  hideChatPill();
  chat.root.querySelector(".chattitle").textContent = sessionLabel(session);
  if (!same) {
    resetChatLog();
    connectChat();
  }
  // The back gesture closes the chat rather than leaving the page.
  if (!history.state || history.state.roostChat !== session) {
    history.pushState({ roostChat: session }, "");
  }
  requestAnimationFrame(() => scrollChatToEnd(true));
}

function closeChat(fromPop) {
  if (!chat.root) return;
  chat.root.hidden = true;
  document.body.classList.remove("chatting");
  hideChatPill();
  clearTimeout(chat.retryTimer);
  if (chat.sock) { chat.sock.onclose = null; chat.sock.close(); }
  chat.sock = null;
  chat.session = null;
  if (!fromPop && history.state && history.state.roostChat) history.back();
}

window.addEventListener("popstate", () => {
  if (chat.root && !chat.root.hidden) closeChat(true);
});

/// Steps away to look at something (a file, the terminal) without ending
/// the chat: the socket stays, and a pill brings it back.
function stepOutOfChat() {
  if (!chat.root) return;
  chat.hidden = true;
  chat.root.hidden = true;
  document.body.classList.remove("chatting");
  let pill = document.getElementById("chatpill");
  if (!pill) {
    pill = chatEl("button", "", "Chat");
    pill.id = "chatpill";
    pill.type = "button";
    pill.onclick = () => openChat(chat.session, chat.home);
    document.body.appendChild(pill);
  }
  pill.hidden = false;
}

function hideChatPill() {
  const pill = document.getElementById("chatpill");
  if (pill) pill.hidden = true;
}

function showTerminalFromChat() {
  const s = chat.session;
  stepOutOfChat();
  if (s) focusSession(s);
}

function resetChatLog() {
  chat.items.clear();
  chat.log.textContent = "";
  chat.live = null;
  chat.root.querySelector(".chatlive").textContent = "";
  setChatBanner("");
}

function connectChat() {
  clearTimeout(chat.retryTimer);
  if (chat.sock) { chat.sock.onclose = null; chat.sock.close(); }
  const session = chat.session;
  const sock = new WebSocket(wsUrl(`/ws/${chat.home}/chat/${session}`));
  chat.sock = sock;
  sock.onmessage = (ev) => {
    if (chat.sock !== sock) return;
    let m;
    try { m = JSON.parse(ev.data); } catch { return; }
    onChatMessage(m);
  };
  sock.onopen = () => { chat.retry = 0; };
  sock.onclose = () => {
    if (chat.sock !== sock || !chat.session) return;
    chat.sock = null;
    // Back off, like the terminal does; a deploy restarts roost under us.
    const wait = Math.min(1000 * 2 ** chat.retry, 10000);
    chat.retry++;
    setChatBanner("Reconnecting…");
    chat.retryTimer = setTimeout(() => { if (chat.session === session) connectChat(); }, wait);
  };
}

function onChatMessage(m) {
  switch (m.t) {
    case "hello":
      setChatBanner("");
      break;
    case "source":
      if (m.state === "found") setChatBanner("");
      else if (m.state === "empty") setChatBanner(m.note || "No messages yet.");
      else setChatBanner(m.note || "No conversation found for this terminal.", true);
      break;
    case "entries":
      renderChatEntries(m.reset, m.items || []);
      break;
    case "live":
      renderChatLive(m);
      break;
    case "error":
      chatFlash(m.msg);
      break;
    case "ended":
      setChatBanner(`This terminal is not running (${m.why}).`, true);
      if (chat.sock) { chat.sock.onclose = null; }
      break;
  }
}

function setChatBanner(text, withTerminal) {
  const b = chat.root.querySelector(".chatbanner");
  b.textContent = text;
  if (text && withTerminal) {
    const go = chatEl("button", "", "Open the terminal");
    go.type = "button";
    go.onclick = () => showTerminalFromChat();
    b.append(" ", go);
  }
  b.hidden = !text;
}

function chatFlash(msg) {
  const b = chatEl("div", "chatflash", msg);
  chat.root.appendChild(b);
  setTimeout(() => b.remove(), 3500);
}

function atChatEnd() {
  const l = chat.log;
  return l.scrollHeight - l.scrollTop - l.clientHeight < CHAT_STICK_PX;
}

function scrollChatToEnd(force) {
  const l = chat.log;
  if (!l) return;
  if (force || atChatEnd()) {
    l.scrollTop = l.scrollHeight;
    chat.root.querySelector(".chatdown").hidden = true;
  }
}

function renderChatEntries(reset, items) {
  const stick = atChatEnd();
  if (reset) { chat.items.clear(); chat.log.textContent = ""; }
  for (const e of items) {
    const el = renderChatEntry(e);
    const old = chat.items.get(e.id);
    if (old) {
      // A tool whose result arrived: keep it open if it was.
      if (old.classList.contains("open")) el.classList.add("open");
      old.replaceWith(el);
    } else {
      chat.log.appendChild(el);
    }
    chat.items.set(e.id, el);
  }
  if (stick) scrollChatToEnd(true);
  else if (items.length) chat.root.querySelector(".chatdown").hidden = false;
}

function renderChatEntry(e) {
  switch (e.k) {
    case "User": {
      const d = chatEl("div", "cmsg cuser");
      const b = chatEl("div", "cbubble", e.text);
      linkifyChat(b);
      d.appendChild(b);
      return d;
    }
    case "Assistant": {
      const d = chatEl("div", "cmsg casst");
      // Built by render::markdown_html, the preview's allowlist sanitizer:
      // every attribute escaped, every href decided server-side.
      d.innerHTML = e.html;
      linkifyChat(d);
      return d;
    }
    case "Tool": {
      const d = chatEl("div", "ctool");
      const head = chatEl("button", "ctoolhead");
      head.type = "button";
      const st = e.result ? (e.result.ok ? "ok" : "err") : "run";
      head.append(chatEl("span", `ctoolst ${st}`), chatEl("span", "ctoolname", e.name), " ");
      const sum = chatEl("span", "ctoolsum", e.summary);
      if (e.path) {
        sum.textContent = "";
        const a = chatEl("a", "clink", e.summary);
        a.dataset.rel = e.path;
        sum.appendChild(a);
      }
      head.appendChild(sum);
      d.appendChild(head);
      if (e.result && e.result.text) {
        const pre = chatEl("pre", "ctoolout", e.result.text);
        linkifyChat(pre);
        d.appendChild(pre);
        head.onclick = (ev) => {
          if (ev.target.closest("a")) return;
          d.classList.toggle("open");
        };
      }
      return d;
    }
    case "Note":
    default: {
      const d = chatEl("div", "cnote", e.text || "");
      linkifyChat(d);
      return d;
    }
  }
}

function renderChatLive(m) {
  chat.live = m;
  const host = chat.root.querySelector(".chatlive");
  host.textContent = "";
  const mode = chat.root.querySelector(".chatmode");
  // A menu replaces the footer the mode is read from; keep the last one shown
  // rather than blinking it away for the length of a question.
  if (m.mode) chat.lastMode = m.mode;
  const shown = m.mode || ((m.prompt || m.busy) ? chat.lastMode : null);
  mode.hidden = !shown;
  mode.textContent = shown || "";
  if (m.prompt) {
    const p = m.prompt;
    const card = chatEl("div", "cprompt");
    for (const c of p.context || []) card.appendChild(chatEl("div", "cpctx", c));
    if (p.question) card.appendChild(chatEl("div", "cpq", p.question));
    p.options.forEach((o, i) => {
      const b = chatEl("button", "cpopt" + (i === p.selected ? " sel" : ""));
      b.type = "button";
      b.appendChild(chatEl("span", "cplabel", o.label));
      if (o.detail) b.appendChild(chatEl("span", "cpdetail", o.detail));
      b.onclick = () => {
        if (!chat.sock || chat.sock.readyState !== 1) return;
        // The label rides along so the server can refuse a menu that changed
        // since this was drawn, rather than answering a different question.
        chat.sock.send(JSON.stringify({ t: "choose", index: i, label: o.label }));
        for (const x of card.querySelectorAll("button")) x.disabled = true;
      };
      card.appendChild(b);
    });
    const esc = chatEl("button", "cpesc", "Esc — cancel");
    esc.type = "button";
    esc.onclick = () => chatKey("esc");
    card.appendChild(esc);
    host.appendChild(card);
  } else if (m.busy) {
    const w = chatEl("div", "cbusy");
    w.append(chatEl("span", "cspin"), chatEl("span", "", "Claude is working…"));
    const stop = chatEl("button", "", "Stop");
    stop.type = "button";
    stop.onclick = () => chatKey("esc");
    w.appendChild(stop);
    host.appendChild(w);
  }
  scrollChatToEnd(false);
}

function chatKey(k) {
  if (chat.sock && chat.sock.readyState === 1) chat.sock.send(JSON.stringify({ t: "key", k }));
}

function submitChat() {
  const input = chat.root.querySelector(".chatinput");
  const text = input.value;
  // A picture already sits in Claude's input, so a message of nothing but
  // pictures is sent with Enter alone.
  const pictures = chat.atts.some((a) => a.image && a.state === "ok");
  if (!text.trim() && !pictures) return;
  if (chat.atts.some((a) => a.state === "up")) { chatFlash("still uploading — send when the ✓ appears"); return; }
  if (!chat.sock || chat.sock.readyState !== 1) { chatFlash("not connected — try again in a moment"); return; }
  // After a picture Claude's input reads "[Image #1]", and the text would
  // weld onto it.
  if (text.trim()) chat.sock.send(JSON.stringify({ t: "send", text: pictures ? ` ${text}` : text }));
  else chatKey("enter");
  clearChatAtts();
  input.value = "";
  input.style.height = "auto";
  scrollChatToEnd(true);
}

/// URLs and slash-bearing paths in plain text become links. Inside an
/// existing <a> nothing is touched; inside code it is, because that is where
/// Claude puts file names.
function linkifyChat(root) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement && n.parentElement.closest("a") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  const re = new RegExp(`${URL_RE.source}|${PATH_RE.source}`, "gi");
  for (const n of nodes) {
    const text = n.nodeValue;
    re.lastIndex = 0;
    if (!re.test(text)) continue;
    re.lastIndex = 0;
    const frag = document.createDocumentFragment();
    let at = 0;
    let m;
    while ((m = re.exec(text))) {
      let s = m[0];
      // Sentence punctuation is not part of the link.
      const trail = s.match(/[.,;:!?)\]]+$/);
      if (trail && !/:\d+$/.test(s)) s = s.slice(0, s.length - trail[0].length);
      if (!s) continue;
      frag.append(text.slice(at, m.index));
      const a = chatEl("a", "", s);
      if (SAFE_URL.test(s)) {
        a.href = s;
        a.target = "_blank";
        a.rel = "noopener noreferrer";
      } else {
        a.className = "clink";
        a.dataset.path = s;
      }
      frag.append(a);
      at = m.index + s.length;
    }
    frag.append(text.slice(at));
    n.replaceWith(frag);
  }
}

function onChatClick(e) {
  const a = e.target.closest("a");
  if (!a) return;
  if (a.classList.contains("mdbroken")) { e.preventDefault(); return; }
  const href = a.getAttribute("href");
  if (href && SAFE_URL.test(href)) {
    e.preventDefault();
    openUrl(href);
    return;
  }
  // A project file: the preview's `data-rel` links and tool paths open it
  // directly; a path written in prose goes through `OpenPath`, which the
  // server resolves and confines (and opens at a `:line` suffix).
  if (a.dataset.rel !== undefined) {
    e.preventDefault();
    const rel = a.dataset.rel;
    send({ t: "OpenTab", pane: 2, tab: { k: "File", rel, mode: defaultMode(rel) } });
    stepOutOfChat();
    revealPane(2);
    return;
  }
  if (a.dataset.path) {
    e.preventDefault();
    send({ t: "OpenPath", text: a.dataset.path });
    stepOutOfChat();
    revealPane(2);
  }
}
