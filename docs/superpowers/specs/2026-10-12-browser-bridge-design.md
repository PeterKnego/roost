# A roost Claude in your real browser

*2026-10-12. Issue [#161](https://github.com/PeterKnego/roost/issues/161),
which carries the problem and the brainstorm's decisions. This records the
shape implementation settled on, and the one measurement that moved it.*

## What it does

Three things, sharing one path:

1. **roost-Claude → your Chrome.** A Claude in a roost terminal can list your
   tabs, open a page, screenshot it, read it, click, type, and read its
   console — in the Chrome you are logged into, not a headless one.
2. **Your Chrome → roost-Claude.** A context-menu item on any page sends its
   URL, the selection, or a screenshot into the Claude terminal of the roost
   tab you used last.
3. **Claude in Chrome ↔ roost-Claude.** A per-project *handoff* page, which
   any browser agent (or a person) can read and write, and which roost-Claude
   reads and writes through tools.

## The measurement that moved it: IDE tools are not model tools

The issue proposed adding the browser tools to `ide.rs`, which is already an
MCP server for every Claude in roost. That does not work. Claude Code 2.1.296
filters the IDE server's tools before the model sees them:

```js
var Cn=["mcp__ide__executeCode","mcp__ide__getDiagnostics"];
function Zo(e){return!e.startsWith("mcp__ide__")||Cn.includes(e)}
```

Anything else roost advertises on that socket is reachable by the CLI's own
code (`openDiff`, `close_tab`) and invisible to the model. So the tools live
in a **separate MCP server**, `roost mcp`, which Claude spawns over stdio like
any other.

## Shape

```
Claude ──stdio──▶ roost mcp ──unix socket──▶ roost server ──workspace ws──▶ roost tab
                                                                              │ postMessage
                                                                              ▼
                                       your Chrome ◀── extension worker ◀── content script
```

### `roost mcp`

A subcommand that speaks MCP over stdin/stdout (newline-delimited JSON-RPC):
`initialize`, `tools/list`, `tools/call`, `ping`. Each `tools/call` runs on its
own thread, because Claude may issue several at once and one can wait two
minutes for a human; writes to stdout are serialised by a mutex.

It knows who it is from its environment, the same way `roost claude-hook`
does: `ROOST_PROJECT` and `ROOST_SESSION`, which every roost terminal
exports, plus its parent pid — the Claude that spawned it. Outside a roost
terminal both are absent and every tool answers *"this Claude is not running
in a roost terminal"*, rather than the server list failing to load.

Registered by the user, once: `claude mcp add -s user roost -- roost mcp`.
roost does not write it anywhere. CLAUDE.md allows roost exactly one Claude
settings file, and only `roost claude-hook` entries in it; an MCP server is
neither, and a user-scope registration is the user's to make. Settings shows
the command and whether it is registered (read-only, from `claude mcp get`
— *not* added in this change; a follow-up).

### The broker socket

`state/hook/browser.sock`, beside `hook.sock` and inside the same verified
0700 directory, with the same `SO_PEERCRED` uid check. A separate socket and
not new ops on `hook.sock`, because the two differ in every parameter that
matters: `hook.sock` answers in ten seconds with one small line, and a browser
call waits up to three minutes and can answer with a multi-megabyte
screenshot. The bind/verify/stale-socket logic is shared, not copied
(`hooksock::bind_private`).

One request per connection: a JSON line (≤ 256 KB) in, a JSON line
(≤ 12 MB) out. At most 16 connections at once, process-wide, like
`hook.sock`.

Not HTTP: CLAUDE.md fixes the HTTP state-change surface at two POSTs, and
no browser can open a Unix socket, which is the whole reason this is safe to
expose to a Claude-spawned process at all.

### The relay: through the roost tab, not a socket of its own

The extension never connects to roost. A content script, injected only into
the roost origins the user lists in the extension's options, talks to the
roost page with `window.postMessage`; the page relays over the workspace
websocket it already has.

What that buys is the reason for the shape:

- **No third authentication rule.** An extension socket would send
  `Origin: chrome-extension://…`, which fits neither of roost's two rules —
  the #117 problem, and CLAUDE.md's "reconciling the two is the
  vulnerability" applies in full. The relay rides an already Origin-checked
  socket.
- **Cloudflare Access just works.** The roost tab is already through it; an
  extension socket would need a service token.

The cost: a roost tab must be open. A call with none fails at once with
*"no roost tab with the extension is open"*, never by waiting.

The danger, and what holds it: roost page JavaScript can now ask your
browser to do things. Every XSS vector in roost (the markdown sanitizer;
#96's HTML preview) is promoted from "can reach a shell" to "can act as you
on every site". So **the extension, not the page, is the policy point**:

- the consent prompt is an extension window, which page JS cannot click;
- grants live in extension storage, which page JS cannot write;
- unsafe mode lives in extension storage, and can never be set from roost;
- the content script accepts messages only from its own window (`event.source
  === window`), never from an iframe, and the content script itself is
  top-frame only — #96 must keep its iframe sandboxed *without*
  `allow-same-origin`;
- the worker identifies the roost origin from `sender.origin`, which Chrome
  fills in, never from the message.

- **every call is signed by the server.** Without this, script in a roost
  page could invent a call claiming any project, session and pid, ride a
  grant the user already gave, and the consent window would show identities
  the attacker chose. So `BridgeCall` carries `payload` (a JSON string:
  call id, project, session, pid, tool, args, issued-at) and `sig`,
  HMAC-SHA256 over the payload's bytes with a key from
  `state/hook/browser.key` (0600, created on first use, written atomically).
  The key reaches the extension once, by hand: `roost browser-key` prints it
  and the user pastes it into the options page. It is never served over
  HTTP. The extension verifies before anything else, refuses a payload
  issued more than five minutes ago (laptop and server clocks differ) and a
  call id it has already seen. Signing the payload as an opaque string
  avoids canonicalising JSON on two sides.

What it still cannot hold, said rather than pretended: script in a roost
page sees every result it relays — a screenshot of whatever Claude asked
for — and could alter a result on its way back to Claude. Another extension
with access to the roost origin can read the same `postMessage` traffic.
Neither can make the browser *do* anything it was not asked to by a real
Claude.

### Which roost tab relays

The server keeps a registry of bridge-ready workspace connections:
`(project, conn id, last active)`. A page announces itself with
`BridgeHello { active }` when its content script answers, and again on
`focus`. A call goes to the **most recently active** one, in any project —
the extension controls the whole browser, so the project the relay tab
shows is irrelevant. `send_to`, never `broadcast`: a screenshot of the
user's bank is not for every connected browser.

A connection that closes fails every call it was relaying, at once, with
*"the roost tab relaying to your browser closed"*. That is the wsconn
teardown path, so it is deterministic, not a timeout.

### Calls, and what bounds them

The broker parks each call: `call id → (asking project, relay project, relay
conn, reply sender)`. The reply intent is accepted only from the connection
the call was sent to. Bounded like `openDiff`:

| bound | value | why |
|---|---|---|
| parked calls per asking project | 16 | a looping Claude must not starve other projects |
| call wait | 180 s | consent waits for a human (120 s), then the action |
| screenshot | viewport only, long side ≤ 2000 px, JPEG ≤ 3.5 MB | the workspace socket drops a connection over its 8 MB frame cap (and base64 adds a third), and the Claude API refuses images over ~5 MB or 8000 px; re-encoded smaller before it is refused |
| request line | 256 KB | `chrome_type` text is the only large argument |

None of the waiting holds a lock: the parked call is a channel, and the
socket thread blocks on `recv_timeout`. Whoever *removes* a parked call
owns its reply — the relay's answer, the asker's timeout, or the relay's
teardown — the rule `hub::ask_inner` already uses, so exactly one answer is
sent. The broker's two maps (relays, calls) are never held together and
never while a hub lock is taken. A send that cannot reach its connection
says so (`hub::send_to_conn` returns whether it delivered) and the next
relay is tried; `Hub::send_to`'s silent no-op would read as delivered and
wait three minutes.

**A relay must prove it is alive.** The content script posts an `ack`
the moment it has a call, before asking the worker, and the page forwards
it as `BridgeAck`. No ack within 5 s and the broker unparks the call,
drops that relay and tries the next. Found by the end-to-end test, not in
review: a roost tab navigated away goes into the back/forward cache with
its socket still open, so the server counted it as a relay and the call
waited the full three minutes. The page also unregisters itself on
`pagehide` and asks again on a cached `pageshow`, so the ordinary case
needs no timeout at all; the ack is the backstop for a frozen tab.

`roost mcp` waits 200 s for a `chrome` op and `wait + 30` s for
`handoff_read`, always longer than the server does.

### Tools

Named `chrome_*`, distinct from Playwright MCP's `browser_*`, because the
two are configured side by side and mean different browsers. Every tool
description says which: *the user's own Chrome, with their logins*.

| tool | arguments | does |
|---|---|---|
| `chrome_tabs` | — | id, title, url, active for every tab |
| `chrome_open` | `url`, `new_tab` (default true) | opens it in the background, waits for load (≤ 30 s) |
| `chrome_screenshot` | `tab_id?` | JPEG of the tab's viewport, returned as MCP image content |
| `chrome_read` | `tab_id?`, `max_chars?` | title, url, visible text, and the interactive elements with numbered refs |
| `chrome_click` | `tab_id?`, `ref` or `selector` | a real (trusted) mouse click at the element's centre |
| `chrome_type` | `tab_id?`, `ref` or `selector`, `text`, `submit?` | focuses, inserts text, optionally presses Enter |
| `chrome_console` | `tab_id?` | console messages and uncaught errors seen since the tab was first attached |
| `chrome_close` | `tab_id?` | closes the tab, **only** if this grant created it with `chrome_open` (new tab); anything else is refused |
| `handoff_read` | `since?`, `wait_seconds?` (≤ 300) | handoff messages after `since`, waiting for one if there are none |
| `handoff_send` | `text` | posts to the handoff page |

`tab_id` omitted means *the tab this session last opened or acted on*, else
the active tab of the focused window — **unless that is a roost tab**, which
is refused with a hint to pass `tab_id`. Claude driving the page it is
being watched through is never what was meant.

The extension uses `chrome.debugger` (the Chrome DevTools Protocol) for
screenshot, read, click, type and console, because it is the one API that
does all five on a background tab and produces trusted input events. Chrome
shows its "is debugging this browser" bar while attached, which is the
honest indicator; the extension detaches a tab after 5 minutes unused, and
`chrome.debugger.onDetach` (the user pressing Cancel on that bar) forgets
the tab's state.

A background tab does not paint, and `Page.captureScreenshot` on one can
hang or come back blank. So a screenshot makes its tab the active one in its
own window first. `chrome_open` opens pages in one unfocused window of
their own, reused, so a screenshot of a page Claude opened never changes
what the user is looking at.

The deny-list is checked against the target before every command and again
after `chrome_open`'s load, because a redirect or a later navigation can
land somewhere the first check never saw.

**The worker's lifetime.** An MV3 service worker is stopped after ~30 s
without events, and neither a pending consent window nor an unanswered
message keeps it up. While a call is outstanding the content script sends
the worker a heartbeat every 20 s. A worker that restarts anyway has lost
its in-flight calls; the content script's pending `sendMessage` then
rejects, and that rejection becomes the call's error — a failure, never a
hang.

**A stale content script.** Reloading or updating the extension orphans the
content scripts already injected, and their pages would stay registered as
relays. The first `runtime` call that throws "context invalidated" makes the
script post `bye`; the page sends `BridgeHello { ready: false }` and the
broker drops it.

### Consent

- **A grant per Claude session.** The first call from `(roost origin,
  project, session, Claude pid)` opens an extension window naming all four,
  the tool and its arguments: *Allow for this session*, *Allow once*,
  *Deny*. A new Claude in the same terminal has a new pid and asks again.
  Grants live in `chrome.storage.session`, so a browser restart forgets
  them, and the popup lists them with a Revoke button.
- No answer in 120 s is Deny.
- Calls that arrive while a prompt for the same grant key is open wait for
  that prompt, rather than each opening a window.
- **Unsafe mode** — everything allowed, no prompts — is a switch in the
  extension's options page and nowhere else. The badge reads `!!` on red the
  whole time it is on.
- **A deny-list beats unsafe mode**: `chrome:`, `chrome-extension:`,
  `devtools:`, `view-source:`, `file:`, the Chrome Web Store, plus origins
  the user adds. Checked against the target tab's URL before every action
  and against `chrome_open`'s URL.

### Your Chrome → roost-Claude

Context menu: *Send page to roost Claude*, *Send selection…*, *Send
screenshot…*. The worker posts it to the most recently focused roost tab's
content script, and the page does what the tree's **Send to Claude** already
does (#146): pastes into the last-focused *Claude* terminal — never a plain
shell, where a multi-line selection would run as commands — as a bracketed
paste, and submits nothing.
A screenshot goes up through the page's existing `POST /paste` — same
origin, Origin-checked — and its path is pasted. No server change: the CSRF
surface stays at two.

### Handoff

Per project, in memory: at most 200 messages of at most 32 KB, each
`{id, at, from, text}` where `from` is `browser` or `claude:<session>`.
Ids start at the server's start time in milliseconds, so a client holding a
`since` from before a restart is not stuck behind ids that now restart at 1. Not
persisted: it is a conversation channel, not a record, and nothing about it
survives a restart today either.

- **The page** is `/frag/{project}/handoff`: a complete, deliberately plain
  HTML page — log, textarea, Send — so an agent driving a browser finds it
  trivial. It opens the project's workspace socket, posts with
  `HandoffPost { text }`, and renders the `Handoff` events every client of
  the project receives.
- **roost-Claude** uses `handoff_read` (blocking up to `wait_seconds` on a
  condvar, never with a lock held across anything else) and
  `handoff_send`.

**Handoff text is untrusted input.** A page Claude in Chrome reads can steer
it into writing here, and roost-Claude has a shell. `handoff_read`'s tool
description says so, in the words the model reads.

Typing a message into the Claude terminal as a prompt is *not* done: a
paste into a Claude mid-turn lands in its input box and is submitted with
whatever the user types next. Claude polls with `handoff_read`, which is
what a two-agent loop wants anyway.

## Hard constraints, restated for this feature

- HTTP stays GET-only plus `/upload` and `/paste`. The handoff page is a GET;
  everything it does is a websocket intent.
- `ide.rs` is untouched.
- `roost mcp` answers notifications with nothing, negotiates the protocol
  version, and on `notifications/cancelled` drops the reply it would have
  written (the broker call itself runs to its own end).
- No new TCP listener. The broker is a Unix socket.
- No lock is held across a parked call or a socket write.
- No panic escapes the broker's connection threads or `roost mcp`'s call
  threads; each answers with an error instead.

## Not done here

- Settings showing whether `roost mcp` is registered.
- Full-page screenshots, network inspection, scrolling, file upload into a
  page.
- A Chrome Web Store listing; the extension loads unpacked from `extension/`.
