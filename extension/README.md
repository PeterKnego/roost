# roost bridge — the Chrome extension

Lets a Claude running in roost use **your** Chrome: list tabs, open a page,
screenshot it, read it, click, type, read its console. It works with your
logins, so it always asks first. Design and threat model:
[docs/superpowers/specs/2026-10-12-browser-bridge-design.md](../docs/superpowers/specs/2026-10-12-browser-bridge-design.md).

## Setup, once

1. **Give Claude the tools.** On the roost host:

   ```sh
   claude mcp add -s user roost -- roost mcp
   ```

   roost never registers this itself. Claude sessions started after this
   have the `chrome_*` and `handoff_*` tools.

2. **Load the extension.** In Chrome, open `chrome://extensions`, turn on
   *Developer mode*, choose *Load unpacked* and pick this `extension/`
   directory (copy it to your laptop first if roost runs elsewhere).

3. **Pair it.** On the roost host, run `roost browser-key`. In the
   extension's options (right-click its toolbar icon → *Options*), paste
   the key, then add your roost address, e.g. `https://roost.example.com` or
   `http://127.0.0.1:8444`. Chrome asks you to allow access to that address.

4. **Open roost** in a tab, or reload it. That tab relays calls. Without an
   open roost tab, the tools fail at once and tell you so.

## When Claude asks

The first call from a Claude session opens a small window naming the roost
address, the project, the terminal and the Claude process, and showing what
it wants to do:

- **Allow for this Claude session**: allowed until that Claude exits, Chrome
  restarts, or you revoke it from the toolbar popup.
- **Allow once**
- **Deny** (also what happens after 2 minutes with no answer)

**Unsafe mode** in the options allows everything with no prompt. The
toolbar badge reads **!!** while it is on.

Some pages are never driven, unsafe mode or not: `chrome://` and other
browser pages, the Chrome Web Store, roost itself, and any domain you list
under *Never drive these sites*.

While Claude works in a tab, Chrome shows its "is debugging this browser"
bar. Pressing *Cancel* there detaches it. Pages Claude opens go into an
orange "roost - <project>" tab group in your window (with the worktree
name added for a worktree) and come to the front,
so you can watch. To keep them behind your own tab instead, turn off *Bring
Claude's tabs to the front* in the options.

## Sending to Claude

Right-click any page:

- *Send page to roost Claude*
- *Send selection to roost Claude*
- *Send screenshot to roost Claude*

It goes to the roost tab you used last and is pasted into that project's
Claude terminal, the same as the tree's *Send to Claude*. Nothing is
submitted. The toolbar badge flashes **ok** or **no**.

## Handoff with Claude in Chrome

Each project has a plain message page at `/frag/<project>/handoff`. Point
Claude in Chrome (or yourself) at it. The roost Claude reads it with
`handoff_read`, which can wait for a reply, and answers with `handoff_send`.
Treat what arrives there as untrusted: a web page can steer what a browser
agent writes.

## Rotating the key

Delete `state/hook/browser.key` in roost's state directory, run
`roost browser-key` again, and paste the new key into every browser.
