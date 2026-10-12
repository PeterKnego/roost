# Browser bridge — plan

Spec: [`2026-10-12-browser-bridge-design.md`](../specs/2026-10-12-browser-bridge-design.md).

1. **Shared private socket.** Pull `hooksock::start`'s directory check,
   stale-socket removal, bind and chmod into `hooksock::bind_private(name)`;
   `hook.sock` uses it unchanged. `peer_uid` becomes `pub(crate)`.
2. **Handoff log.** `handoff.rs`: per-project bounded log, `post`, `since`,
   `wait_since` on a condvar. Unit-tested for the caps and for waking.
3. **Broker.** `browser.rs`: the bridge registry (`hello`, `gone`), parked
   calls (`call_blocking`, `deliver`), the per-project cap, and the socket
   server on `browser.sock` serving `chrome`, `handoff_read` and
   `handoff_send` ops. Unit tests with two registered relays, so `send_to`
   and `broadcast` are distinguishable; revert-check routing and teardown.
4. **Wire.** `Intent::{BridgeHello, BridgeReply, HandoffPost}`,
   `Event::{BridgeCall, Handoff}`. `wsconn` diverts `BridgeReply` before the
   hub lock and calls `browser::gone` on teardown; `BridgeHello` and
   `HandoffPost` are hub intents. `hub::send_to_conn` delivers one event to
   one connection and says whether it could.
5. **`roost mcp`.** `mcp.rs`: stdio JSON-RPC loop, tool list, a thread per
   call, the broker client. Unit tests drive it with in-memory streams and a
   fake broker.
6. **Handoff page.** `/frag/{project}/handoff` in `routes.rs`, page in
   `render.rs`, `static/handoff.js`.
7. **Page relay.** `app.js`: content-script handshake, `BridgeHello` on
   hello and focus, `BridgeCall` → content script → `BridgeReply`, and
   "send to Claude" from the extension reusing `sendToClaude`/paste.
8. **Extension.** `extension/`: manifest, worker (consent, grants, unsafe
   mode, deny-list, CDP tools), content script, consent window, options,
   popup.
9. **End to end.** `tests/browser/bridge.mjs`: Chromium with the extension
   loaded, a real roost, `roost mcp` as a subprocess: open, screenshot, read,
   click, type, console, consent deny, handoff both ways.
10. **Docs.** README section and `extension/README.md`.
