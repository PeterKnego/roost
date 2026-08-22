//! The `openDiff` proposal tab and the Alt+K mention keybinding.
//!
//! Everything under test lives in static/app.js: the client-side map keyed
//! by proposal id (renderProposal, tabKey's "content"/"pending" split), the
//! hand-built hunk view (renderHunks — a client-side port of
//! textdiff.rs::unified, since a proposal's content arrives once over the
//! socket with no server-rendered fragment to fetch), and the Alt+K handler
//! that turns an editor selection into a MentionPath intent. No Rust test
//! can reach any of it.
//!
//! This drives a real resh, a real dtach-backed terminal (which is what
//! actually provisions the ide listener — see term.rs's `ide::for_project`
//! call), and a real browser — but a **fake** Claude on the wire: a
//! hand-rolled client that speaks exactly the handshake and JSON-RPC shapes
//! src/ide.rs implements (its own Rust tests do the same thing with
//! tungstenite's request builder; this is the same idea from Deno, since
//! Deno's WebSocket constructor cannot set the custom auth header the real
//! handshake requires). That keeps this test hermetic and deterministic —
//! see task-8-report.md for a *separate*, one-off verification against a
//! real `claude` binary, following the pexpect+pyte technique documented in
//! that plan's Task 4/5 reports. "A browser test driven by a fake Claude"
//! is also why acceptance is asserted on the reply resh sends back over the
//! ide socket (TAB_CLOSED) and on the file being untouched, not on the file
//! changing: resh never writes the file itself (the brief's own words) — a
//! real CLI is what would apply the edit, and this test never runs one.
//!
//! Run: deno run -A tests/browser/ide.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

/// Like harness.mjs's `until`, but returns what `fn()` produced instead of a
/// bare boolean — every caller here needs the matched log entry or lock file
/// path, not just "did it eventually show up".
async function poll(fn, secs, label) {
  for (let i = 0; i < secs * 10; i++) {
    const v = await fn();
    if (v) return v;
    await sleep(100);
  }
  console.log(`    (timed out waiting for ${label})`);
  return null;
}

// --- a minimal hand-rolled ide-protocol client -----------------------------
//
// src/ide.rs's handshake requires exactly one custom header
// (X-Claude-Code-Ide-Authorization) and refuses any request that carries an
// Origin at all — the same handshake a real `claude` performs. Deno's
// `WebSocket` constructor cannot set arbitrary headers, so this is a raw TCP
// connection plus a hand-rolled HTTP upgrade and a minimal RFC 6455 frame
// codec (client frames masked, server frames not — text opcode only, since
// every message either side sends here is a small JSON-RPC frame that fits
// in one, unfragmented).

class ByteReader {
  constructor(conn) { this.conn = conn; this.buf = new Uint8Array(0); }
  async fill(min) {
    while (this.buf.length < min) {
      const chunk = new Uint8Array(4096);
      const n = await this.conn.read(chunk);
      if (n === null) throw new Error("ide socket closed while reading");
      const merged = new Uint8Array(this.buf.length + n);
      merged.set(this.buf);
      merged.set(chunk.subarray(0, n), this.buf.length);
      this.buf = merged;
    }
  }
}

function wsKey() {
  return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))));
}

async function readHandshakeResponse(reader) {
  for (;;) {
    const s = reader.buf;
    let idx = -1;
    for (let i = 0; i + 3 < s.length; i++) {
      if (s[i] === 13 && s[i + 1] === 10 && s[i + 2] === 13 && s[i + 3] === 10) { idx = i; break; }
    }
    if (idx >= 0) {
      const header = new TextDecoder().decode(s.slice(0, idx));
      reader.buf = reader.buf.slice(idx + 4);
      return header;
    }
    await reader.fill(reader.buf.length + 1); // force at least one more byte in
  }
}

async function readFrame(reader) {
  await reader.fill(2);
  const b0 = reader.buf[0], b1 = reader.buf[1];
  const opcode = b0 & 0x0f;
  let len = b1 & 0x7f;
  let need = 2;
  if (len === 126) { await reader.fill(4); len = (reader.buf[2] << 8) | reader.buf[3]; need = 4; }
  else if (len === 127) {
    await reader.fill(10);
    let l = 0n;
    for (let i = 0; i < 8; i++) l = (l << 8n) | BigInt(reader.buf[2 + i]);
    len = Number(l);
    need = 10;
  }
  await reader.fill(need + len);
  const payload = reader.buf.slice(need, need + len);
  reader.buf = reader.buf.slice(need + len);
  return { opcode, payload };
}

async function sendText(conn, text) {
  const payload = new TextEncoder().encode(text);
  const maskKey = crypto.getRandomValues(new Uint8Array(4));
  const masked = new Uint8Array(payload.length);
  for (let i = 0; i < payload.length; i++) masked[i] = payload[i] ^ maskKey[i % 4];
  let head;
  if (payload.length < 126) head = Uint8Array.of(0x81, 0x80 | payload.length);
  else if (payload.length < 65536) {
    head = Uint8Array.of(0x81, 0x80 | 126, (payload.length >> 8) & 0xff, payload.length & 0xff);
  } else throw new Error("payload too large for this test's frame writer");
  const frame = new Uint8Array(head.length + 4 + masked.length);
  frame.set(head, 0);
  frame.set(maskKey, head.length);
  frame.set(masked, head.length + 4);
  await conn.write(frame);
}

/// Connects as a fake Claude CLI. Every JSON message received — a request's
/// eventual reply, or an out-of-band notification like `at_mentioned` — lands
/// in `.log`, so a caller can search what already arrived instead of racing
/// a background reader with its own assertion.
async function connectFakeClaude(port, token) {
  const conn = await Deno.connect({ hostname: "127.0.0.1", port });
  const req =
    `GET / HTTP/1.1\r\n` +
    `Host: 127.0.0.1:${port}\r\n` +
    `Connection: Upgrade\r\n` +
    `Upgrade: websocket\r\n` +
    `Sec-WebSocket-Version: 13\r\n` +
    `Sec-WebSocket-Key: ${wsKey()}\r\n` +
    `Sec-WebSocket-Protocol: mcp\r\n` +
    `X-Claude-Code-Ide-Authorization: ${token}\r\n` +
    `\r\n`;
  await conn.write(new TextEncoder().encode(req));
  const reader = new ByteReader(conn);
  const header = await readHandshakeResponse(reader);
  if (!header.startsWith("HTTP/1.1 101")) throw new Error(`ide handshake refused: ${header}`);

  const log = [];
  (async () => {
    try {
      for (;;) {
        const { opcode, payload } = await readFrame(reader);
        if (opcode === 0x8) return; // Close
        if (opcode !== 0x1) continue; // this test sends only single-frame text messages
        try { log.push(JSON.parse(new TextDecoder().decode(payload))); } catch { /* not JSON-RPC, ignore */ }
      }
    } catch { /* socket closed under us — nothing further to read */ }
  })();

  let nextId = 1;
  return {
    log,
    async call(method, params) {
      const id = nextId++;
      await sendText(conn, JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      return id;
    },
    close() { try { conn.close(); } catch {} },
  };
}

// ---------------------------------------------------------------------------

const fx = await fixture();
const projectDir = `${fx.roots}/${fx.project}`;
// Isolates this run's lock file from the developer's real ~/.claude/ide
// (idelock.rs honours CLAUDE_CONFIG_DIR the same way the real CLI does) —
// and it lives under fx.base, so fx.cleanup() removes it with everything
// else instead of needing its own sweep, unlike the manual real-`claude`
// checks in earlier tasks' reports.
const claudeConfigDir = `${fx.base}/claude-config`;

const ACCEPT_FILE = "accept-me.txt";
const REJECT_FILE = "reject-me.txt";
const MENTION_FILE = "mention-me.txt";
const acceptOld = "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\n";
const acceptNew = acceptOld.replace("delta\n", "DELTA CHANGED\n");
const rejectOld = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
const rejectNew = rejectOld.replace("three\n", "THREE CHANGED\n");
await Deno.writeTextFile(`${projectDir}/${ACCEPT_FILE}`, acceptOld);
await Deno.writeTextFile(`${projectDir}/${REJECT_FILE}`, rejectOld);
await Deno.writeTextFile(`${projectDir}/${MENTION_FILE}`, "aaa\nbbb\nccc\nddd\neee\n");

const resh = await startResh({
  repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
  extraEnv: { CLAUDE_CONFIG_DIR: claudeConfigDir },
});
const browser = await startBrowser(profileDir(repoRoot));
let page, claude;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  // See the README's traps: the default 800x600 headless window collapses
  // the middle pane, which every assertion below reads from.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  console.log("A. an ide listener comes up when a terminal attaches, and a fake Claude connects to it");
  // Starting the seeded "term" session is what makes term.rs call
  // ide::for_project (see its own comment there: the only other trigger is a
  // _workspace connect, which already happened above via openPage) — the
  // same path a real `claude` running in a resh terminal takes.
  const findTerm = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return state.panes[pi].tabs[ti].session; } return null; })()`;
  const session = await evalIn(findTerm);
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(session)} })`);
  ok(await until(() => evalIn(`terms.has(${JSON.stringify(session)})`), 30, "terminal"), "a terminal attaches");

  const lockFile = await poll(async () => {
    try {
      for await (const e of Deno.readDir(`${claudeConfigDir}/ide`)) {
        if (e.name.endsWith(".lock")) return `${claudeConfigDir}/ide/${e.name}`;
      }
    } catch { /* directory not created yet */ }
    return null;
  }, 15, "an ide lock file");
  ok(!!lockFile, "an ide lock file appears once a terminal has attached");
  if (!lockFile) {
    // Nothing below this point can proceed without a real port/token to
    // connect to — throw with a clear reason rather than let
    // Deno.readTextFile(null) fail on an unrelated-looking message and skip
    // the pass/fail summary at the bottom of this file.
    throw new Error("no ide lock file appeared — cannot continue");
  }
  const lockJson = JSON.parse(await Deno.readTextFile(lockFile));
  const idePort = Number(lockFile.match(/(\d+)\.lock$/)[1]);
  claude = await connectFakeClaude(idePort, lockJson.authToken);
  console.log(`    fake Claude connected on ide port ${idePort}`);

  console.log("\nB. Claude proposes an edit, resh shows the changed hunk, and Accept resolves it");
  const acceptAbs = `${projectDir}/${ACCEPT_FILE}`;
  const acceptReqId = await claude.call("tools/call", {
    name: "openDiff",
    arguments: {
      old_file_path: acceptAbs, new_file_path: acceptAbs,
      new_file_contents: acceptNew, tab_name: "accept-me",
    },
  });
  ok(await until(() => evalIn(`state.panes[2].tabs.some((t) => t.k === "Proposal")`), 15, "a proposal tab"),
     "a proposal tab opens in the middle pane");
  const acceptHunkVisible = await until(async () => {
    const t = await evalIn(`(document.querySelector('.pane[data-pane="2"] .content') || {}).textContent || ""`);
    return t.includes("-delta") && t.includes("+DELTA CHANGED");
  }, 15, "the changed hunk");
  ok(acceptHunkVisible,
     "the proposal tab shows the specific changed line (-delta / +DELTA CHANGED), not just an empty tab");
  ok((await Deno.readTextFile(acceptAbs)) === acceptOld,
     "the file on disk is still untouched while the proposal is only proposed");

  ok(await evalIn(`(() => { const b = [...document.querySelectorAll(
       '.pane[data-pane="2"] .proposal-actions button')].find((x) => x.textContent === "Accept");
     if (!b) return false; b.click(); return true; })()`),
     "the Accept button is present and clickable");
  const acceptReply = await poll(
    () => claude.log.find((m) => m.id === acceptReqId && m.result), 10, "the accept reply",
  );
  ok(acceptReply?.result?.content?.[0]?.text === "TAB_CLOSED",
     `accepting an unedited proposal reports TAB_CLOSED over the ide socket, got ${JSON.stringify(acceptReply?.result)}`);
  // A useful negative guard, not the discriminator for this section: this
  // passes just as well with the whole Accept feature deleted (no button,
  // no reply — the file was never going to change either way). The
  // TAB_CLOSED assertion just above is what actually proves resh answered
  // the accept; this one proves resh did not additionally do the CLI's own
  // job of writing the file (the CLI applies the edit with the content
  // this answer returns — a browser test driven by a fake Claude can never
  // see the file change, since no CLI is running to make that write).
  ok((await Deno.readTextFile(acceptAbs)) === acceptOld,
     "resh itself never wrote the file — only the reply above says the proposal was accepted");
  ok(await until(() => evalIn(`!state.panes.some((p) => p.tabs.some((t) => t.k === "Proposal"))`), 10, "tab closed"),
     "the proposal tab closes once it has been answered");

  console.log("\nC. Reject leaves the file untouched and reports DIFF_REJECTED");
  const rejectAbs = `${projectDir}/${REJECT_FILE}`;
  const rejectReqId = await claude.call("tools/call", {
    name: "openDiff",
    arguments: {
      old_file_path: rejectAbs, new_file_path: rejectAbs,
      new_file_contents: rejectNew, tab_name: "reject-me",
    },
  });
  const rejectHunkVisible = await until(async () => {
    const t = await evalIn(`(document.querySelector('.pane[data-pane="2"] .content') || {}).textContent || ""`);
    return t.includes("-three") && t.includes("+THREE CHANGED");
  }, 15, "the second proposal's hunk");
  ok(rejectHunkVisible, "the second proposal also shows its own changed line");
  ok(await evalIn(`(() => { const b = [...document.querySelectorAll(
       '.pane[data-pane="2"] .proposal-actions button')].find((x) => x.textContent === "Reject");
     if (!b) return false; b.click(); return true; })()`),
     "the Reject button is present and clickable");
  const rejectReply = await poll(
    () => claude.log.find((m) => m.id === rejectReqId && m.result), 10, "the reject reply",
  );
  ok(rejectReply?.result?.content?.[0]?.text === "DIFF_REJECTED",
     `rejecting reports DIFF_REJECTED over the ide socket, got ${JSON.stringify(rejectReply?.result)}`);
  ok((await Deno.readTextFile(rejectAbs)) === rejectOld, "the file is unchanged after a reject");
  ok(await until(() => evalIn(`!state.panes.some((p) => p.tabs.some((t) => t.k === "Proposal"))`), 10, "tab closed"),
     "the rejected proposal's tab closes too");

  console.log("\nD. a proposal tab with no content yet offers no Accept/Reject");
  // The sub-millisecond real-world window this covers (the State naming a
  // Tab::Proposal arriving fractionally before the Event::Proposal carrying
  // its content) cannot be reliably *won* from here even after wsconn.rs's
  // connect-path reordering — so this calls the renderer directly for an id
  // `state.proposals` has never heard of, which is exactly the shape that
  // window produces, and is the only way to make this deterministic instead
  // of a flake that happens to pass.
  //
  // renderProposal's content branch now fetches `/frag/.../proposal` (it
  // used to build the hunk view synchronously in-line), so the correct
  // guard — the `if (!p)` early return, before any fetch is ever issued —
  // has to be checked *after* giving a wrongly-issued fetch time to land,
  // or a regression that removes the guard would still read as "no button"
  // simply because this check ran before that fetch's response arrived.
  // Stashed on `window` because each `evalIn` call is its own isolated
  // `Runtime.evaluate` — a plain `const` here would not survive to the
  // second call below.
  await evalIn(`(() => {
    window.__placeholderEl = document.createElement("div");
    window.__placeholderThrew = false;
    try { renderProposal(window.__placeholderEl, { k: "Proposal", id: "no-such-proposal-id" }); }
    catch (e) { window.__placeholderThrew = true; }
  })()`);
  await sleep(1500); // headroom for a wrongly-issued fetch to a real local server to land
  const placeholder = JSON.parse(await evalIn(`JSON.stringify({
    hasButton: !!window.__placeholderEl.querySelector("button"),
    text: window.__placeholderEl.textContent,
    threw: window.__placeholderThrew,
  })`));
  ok(!placeholder.hasButton,
     `a content-less proposal renders no Accept/Reject button even after waiting out a possible fetch (rendered text: ${JSON.stringify(placeholder.text)})`);
  ok(!placeholder.threw, "and rendering it does not throw");
  ok(placeholder.text.length > 0, "it shows placeholder text rather than a blank pane");
  // The mechanism that closes the race in real use: render()'s mountedKey
  // guard skips remounting a tab whose key is unchanged (see render(), the
  // comment above the "same tab is still active" early return), so tabKey
  // has to fold "has this id's content arrived" into its own output or a
  // placeholder mounted first would never be replaced once the real content
  // shows up. Asserted directly on tabKey's own output — not by trying to
  // win the sub-millisecond window itself — because section B/C above
  // cannot exercise this: this task's own wsconn.rs fix makes content
  // arrive before the tab in every ordinary run, so a timing-based
  // assertion here would pass identically with the fold deleted (confirmed
  // by hand: folding tabKey's "Proposal" case down to `Proposal:${t.id}`
  // with no content/pending suffix left every assertion above green).
  const tabKeyCheck = JSON.parse(await evalIn(`(() => {
    const before = tabKey({ k: "Proposal", id: "tabkey-remount-check" });
    state.proposals["tabkey-remount-check"] = { rel: "x", old_text: "a", new_text: "b" };
    const after = tabKey({ k: "Proposal", id: "tabkey-remount-check" });
    delete state.proposals["tabkey-remount-check"];
    return JSON.stringify({ before, after });
  })()`));
  ok(tabKeyCheck.before !== tabKeyCheck.after,
     `tabKey changes once a proposal's content arrives, forcing a remount (got ${JSON.stringify(tabKeyCheck)})`);

  console.log("\nE. Alt+K sends the selected range as an @-mention to Claude");
  // MentionPath is deliberately not pasted into the terminal (see proto.rs's
  // own doc comment on it) — it reaches Claude only as an `at_mentioned` MCP
  // notification on the ide socket, so that is what this asserts on, not
  // anything printed in a terminal pane.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(MENTION_FILE)}, mode: "Edit" } })`);
  ok(await until(() => evalIn(`!!document.querySelector("textarea.editor")`), 10, "the mention file's editor"),
     "the mention file opens in an editor");
  // Selects lines 2-3 ("bbb\\nccc\\n"), 1-based — the same convention
  // MentionPath's line_start/line_end carry.
  await evalIn(`(() => {
    const ta = document.querySelector("textarea.editor");
    const start = ta.value.indexOf("bbb");
    const end = ta.value.indexOf("ddd"); // exclusive end selects through "ccc\\n"
    ta.focus();
    ta.setSelectionRange(start, end);
  })()`);
  for (const type of ["rawKeyDown", "keyUp"]) {
    await cmd("Input.dispatchKeyEvent", {
      type, modifiers: 1 /* Alt */, key: "k", code: "KeyK",
      windowsVirtualKeyCode: 75, nativeVirtualKeyCode: 75,
    });
  }
  const mentioned = await poll(
    () => claude.log.find((m) => m.method === "at_mentioned" && (m.params?.filePath || "").endsWith(MENTION_FILE)),
    10, "the at_mentioned notification",
  );
  ok(!!mentioned, "Claude's ide socket receives an at_mentioned notification for the selection");
  ok(mentioned?.params?.lineStart === 2 && mentioned?.params?.lineEnd === 3,
     `it carries the selected line range (2-3), got ${JSON.stringify(mentioned?.params)}`);
  ok(mentioned && !("id" in mentioned), "at_mentioned is a notification (no id), not something resh expects an answer to");
} finally {
  try { claude?.close(); } catch {}
  try { page?.close(); } catch {}
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
