# roost bridge — privacy policy

*Effective 2026-10-14.*

roost bridge is a Chrome extension that lets a Claude coding agent running on
**your own** roost server act in your browser, after you allow it. It is part
of [roost](https://github.com/PeterKnego/roost), an open-source project. It is
not made by, endorsed by or affiliated with Anthropic.

## The short version

- The extension sends data **only to the roost server you configure**. That
  server is your own; nobody else operates it.
- It sends nothing to the roost developers or to any third party. It has no
  analytics, no tracking, no ads and no remote code.
- It acts on a page only when a Claude session you allowed asks it to, or when
  you choose *Send … to roost Claude* from the context menu.

## What it handles, and where it goes

When a Claude session you allowed calls one of its tools, the extension reads
from or acts on browser tabs. The result goes back to the Claude that asked,
on your roost server, through the roost page open in your browser.

| Tool | What leaves the browser |
| --- | --- |
| `chrome_tabs` | Title and URL of each open tab. Tabs on a denied site are listed without their title or URL. |
| `chrome_open`, `chrome_close` | The URL and title of the tab opened or closed. |
| `chrome_read` | The page's title, URL, visible text and the labels of its links, buttons and inputs. |
| `chrome_screenshot` | An image of the visible part of the tab. |
| `chrome_console` | The page's console messages and uncaught errors. |
| `chrome_click`, `chrome_type`, `chrome_upload` | A short description of the element acted on. Typed text and uploaded files come *from* your roost server into the page. |

From the context menu, *Send page / selection / screenshot to roost Claude*
sends that page's title and URL, the selected text, or a screenshot of the
visible tab to the roost page you used last. That page pastes it into its
Claude terminal.

None of this is collected by the extension itself. It is passed to your roost
server, which keeps it only as part of the Claude session that asked. roost
does not send it anywhere.

## What it stores in your browser

- **Settings** (`chrome.storage.local`), kept until you change them or remove
  the extension:
  - the roost addresses you add;
  - the pairing key you paste;
  - your list of sites never to drive;
  - whether unsafe mode and "bring Claude's tabs to the front" are on.
- **Session state** (`chrome.storage.session`), erased when Chrome closes:
  - which Claude sessions you allowed;
  - recently handled call ids, so a call cannot be replayed;
  - the tabs and tab group each session opened;
  - the roost tab you used last.

## Permissions

The extension asks for:

- **debugger**: to screenshot, read, click and type in the tab a Claude
  session was allowed to act on.
- **tabs** and **tabGroups**: to list, open, close and group tabs.
- **scripting**: to add its relay to the roost pages you list.
- **storage**: for the settings above.
- **contextMenus** and **activeTab**: for the *Send to roost Claude* menu.
- **host access**: to the roost addresses you add (loopback by default), and
  to nothing else. Each address is asked for when you add it.

In **Firefox**, which has no debugger API, the extension does the same work
with **scripting** (running its page code in the tab a session acts on) and
**tabs.captureTab**, and so asks for the optional **access to all websites**
when you first allow a Claude session. It changes what the extension *can*
reach, not what it does: it still acts only on a call you allowed, and sends
results only to your roost. The Firefox package declares the same two kinds
of data in its manifest (`websiteContent`, `browsingActivity`).

## Your control

- Every Claude session asks before its first action, and no answer means no.
- Grants can be revoked from the toolbar button at any time.
- Sites on your deny-list are never driven, unsafe mode or not. Neither are
  Chrome's own pages, the Chrome Web Store, or roost itself.
- Removing the extension deletes everything it stored.

## Contact

Questions and reports: <https://github.com/PeterKnego/roost/issues>.
