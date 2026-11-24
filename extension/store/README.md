# roost bridge in the Chrome Web Store

Everything the developer dashboard asks for, in the order it asks, and the
pictures it wants. They are real captures of the extension in this checkout;
`deno run -A extension/store/render.mjs` makes them again after a UI change.
`scripts/package-extension.sh` leaves this directory out of every extension
zip.

## Upload

`roost-extension-<version>-webstore.zip` from the release page. It is the
same files as `roost-extension-<version>.zip`, but with `manifest.json` at the
root, which the store requires. The plain zip keeps them in a
`roost-extension/` folder for "Load unpacked", and the store rejects that
layout.

v0.8.0 was released before the store zip existed. Build it from the tag:

```sh
mkdir -p /tmp/roost-v0.8.0 && git archive v0.8.0 extension scripts/lib.sh | tar -x -C /tmp/roost-v0.8.0
cp scripts/package-extension.sh /tmp/roost-v0.8.0/scripts/
(cd /tmp/roost-v0.8.0 && scripts/package-extension.sh 0.8.0 /tmp)   # → /tmp/roost-extension-0.8.0-webstore.zip
```

It uses an archive of the tag, not a second checkout. CLAUDE.md's "build from
one checkout" rule is about cargo, which this never runs, so a worktree would
add nothing.

## Store listing tab

**Title / summary**: taken from the package ("roost bridge" and the
manifest description). Nothing to type.

**Description**: paste everything between the lines.

---
roost bridge connects your browser to roost, so the Claude coding agent running in a roost terminal on your server can work in the Chrome you are actually logged into, not in a headless browser that sees none of your accounts.

What it is for
roost runs your long-lived coding sessions on a server and shows them in a browser tab. A Claude working there can change code and run tests, but it cannot see the staging site behind your SSO, the admin page you are logged into, or the bug you are looking at right now. roost bridge closes that gap. Claude can ask your browser to:

• list your open tabs
• open a page (in a "Claude" tab group in your window) and close the ones it opened
• read a page: its text, and numbered references to its links, buttons and inputs
• click, type and fill file inputs
• take a screenshot of what is visible
• read the page's console messages and errors

Typical uses: "open staging and check that the checkout still works", "look at the error in the console after I click Save", "screenshot the dashboard and compare it with the design".

You stay in control
• Nothing happens until you allow it. The first request from each Claude session opens a window that shows which roost, project and terminal is asking and what it wants to do. You choose Allow for this session, Allow once, or Deny. No answer means Deny.
• Allowed sessions are listed in the toolbar button and can be revoked at any time. Every grant ends when Chrome restarts.
• Sites on your deny-list are never driven. Neither are Chrome's own pages, the Chrome Web Store, or roost itself.
• Calls are accepted only from roost addresses you add, and only when they are signed with a key that your own roost server generates.
• Unsafe mode, which allows everything without asking, is off by default. The toolbar badge shows !! whenever it is on.

The other direction
• Right-click any page to send it, the selected text or a screenshot to your roost Claude's prompt.
• Each roost project has a simple handoff page where another browser agent, such as Claude in Chrome, can exchange messages with the Claude in your roost.

Privacy
roost bridge talks only to the roost server you configure, which is your own. It sends nothing to the developers or to anyone else, and contains no analytics, tracking or remote code.

Setup
roost bridge needs roost 0.8.0 or later on your server (https://github.com/PeterKnego/roost).
1. On the roost host, register the tools once: claude mcp add -s user roost -- roost mcp
2. Run roost browser-key there and paste the key into this extension's options.
3. Add your roost address in the options, then open roost in a tab.
Full guide: https://github.com/PeterKnego/roost/blob/master/extension/README.md

roost and roost bridge are open source (MIT or Apache-2.0). They are not made by, endorsed by or affiliated with Anthropic. Claude is a trademark of Anthropic.
---

**Category**: Developer Tools

**Language**: English

**Store icon** (128×128): `extension/icons/icon-128.png`

**Screenshots** (1280×800, JPEG, no alpha), in this order:
1. [`screenshot-1-consent.jpg`](screenshot-1-consent.jpg): Claude asks before it touches your browser
2. [`screenshot-2-handoff.jpg`](screenshot-2-handoff.jpg): handing work between Claude in Chrome and Claude in roost
3. [`screenshot-3-options.jpg`](screenshot-3-options.jpg): paired with your own roost, deny-list, unsafe mode off
4. [`screenshot-4-sessions.jpg`](screenshot-4-sessions.jpg): allowed sessions, and revoking them

**Small promo tile** (440×280): [`promo-small-440x280.jpg`](promo-small-440x280.jpg)

**Marquee promo tile** (1400×560): [`promo-marquee-1400x560.jpg`](promo-marquee-1400x560.jpg). Optional; it
is used only if the store features the extension.

**Promo video**: leave empty.

**Official URL**: None. That field is for a domain verified in Search
Console.

**Homepage URL**: `https://github.com/PeterKnego/roost/tree/master/extension`

**Support URL**: `https://github.com/PeterKnego/roost/issues`

**Mature content**: No.

---

## Privacy tab (the next page of the dashboard)

**Single purpose**

> Lets a Claude coding agent running on the user's own roost server view and act in this browser (open, read, click, type, screenshot) after the user allows it, once per Claude session.

**Permission justifications**

- **debugger**: Used to take screenshots of, read the text of, click in, type into and read console messages from the tab a Claude session has been allowed to act on, including a background tab. It is attached only to tabs the user's allowed Claude session acts on. Chrome shows its "is debugging this browser" bar while it is attached, and the user can cancel it there.
- **tabs**: Used to list open tabs for the agent, open and close tabs, and read each tab's URL so the user's deny-list is enforced before every action.
- **tabGroups**: Used to keep the tabs the agent opens in one "Claude" tab group in the user's window, so they are easy to tell apart and close.
- **scripting**: Used to register the extension's relay script on the roost addresses the user adds in the options, and to inject it into roost tabs that are already open. It is not used on any other site.
- **storage**: Used to keep the user's settings (roost addresses, pairing key, deny-list, unsafe mode) and the per-session grants.
- **contextMenus**: Used for the "Send page / selection / screenshot to roost Claude" menu items.
- **activeTab**: Used to capture the visible tab when the user chooses "Send screenshot to roost Claude" from the context menu.
- **Host permissions** (`http://127.0.0.1/*`, `http://localhost/*`; optional `http(s)://*/*`): The relay script runs only on the user's own roost page. Loopback covers a roost on the same machine. Any other roost address is requested as an optional permission for that one origin when the user adds it.

**Remote code**: No, I am not using remote code. All JavaScript is in the package; nothing is fetched or evaluated from a server.

**Data usage**: tick these, because tool results include them:
- ☑ **Website content** (page text, screenshots, element labels)
- ☑ **Web history** (`chrome_tabs` returns open tabs' titles and URLs)

Leave the rest unticked.

Certify all three statements:
- ☑ I do not sell or transfer user data to third parties, outside of the approved use cases
- ☑ I do not use or transfer user data for purposes that are unrelated to my item's single purpose
- ☑ I do not use or transfer user data to determine creditworthiness or for lending purposes

**Privacy policy URL**:
`https://github.com/PeterKnego/roost/blob/develop/extension/PRIVACY.md`
([`../PRIVACY.md`](../PRIVACY.md)).

---

## Expect

- The `debugger` permission puts the item into in-depth review, which usually
  takes days rather than hours.
- Reviewers may ask how to test it. A test instructions box is under
  *Distribution*; the Setup steps above are enough to paste there.

---

## Firefox: addons.mozilla.org

Upload `roost-extension-<version>-firefox.zip` from the release page.

- **Platforms**: Firefox only. **Uncheck Firefox for Android**: the extension
  needs popup windows for consent, which Android does not have, and
  `web-ext lint` warns about it for that reason alone.
- **Source code**: not needed. Nothing is minified, transpiled or bundled;
  the zip is the source.
- **Name / summary / description**: the same as the Chrome listing above.
  AMO's summary is capped at 250 characters; the manifest description fits.
- **Categories**: Web Development (or Developer Tools, if offered).
- **License**: MIT or Apache-2.0 (dual), as the repository.
- **Privacy policy**: the same URL as above. The data collection consent is
  in the manifest (`data_collection_permissions`: website content and
  browsing activity), so Firefox shows it at install and AMO does not ask for
  it separately.
- **Notes to reviewer**: something like this:

  > roost bridge relays signed requests from a roost server the user runs
  > (https://github.com/PeterKnego/roost) to the browser, after a per-session
  > consent window. Firefox has no debugger API, so page actions run through
  > scripting.executeScript and screenshots through tabs.captureTab; both
  > need the optional <all_urls>, requested from the consent window's Allow
  > click. The extension makes no network requests of its own.
  > To test without a roost: the options page and consent window can be
  > inspected; driving needs `roost` 0.8+ (`roost browser-key`, then add its
  > address in the options).
