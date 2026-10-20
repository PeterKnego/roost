# roost bridge in Firefox — plan

Spec: [`2026-10-19-firefox-bridge-design.md`](../specs/2026-10-19-firefox-bridge-design.md).

1. **`page.js`.** The string scripts become one function, `roostPage(op, a)`,
   with Firefox's own operations added: click, type, upload, consoleHook,
   consoleRead and viewport.
2. **`drive-cdp.js`.** The DevTools code leaves `worker.js` unchanged in
   behaviour; `bridge.mjs` must stay green throughout.
3. **`drive-page.js`.** `executeScript` and `captureTab`; `needSites`; the
   console watch.
4. **`worker.js`.** `importScripts`, the backend choice, port-less patterns,
   Firefox's denied pages, optional tab groups.
5. **Consent and options.** The site-access request in the Allow click; the
   options button; port-less grants.
6. **Packer.** The Firefox zip and its manifest; `scripts/test/extension.sh`
   reads it back; `web-ext lint`.
7. **`bridge-firefox.mjs`.** Real Firefox over Marionette, with revert-checks.
8. **Docs.** `extension/README.md`, `PRIVACY.md`, AMO notes in
   `extension/store/README.md`, and browser-neutral tool descriptions in
   `mcp.rs`.
