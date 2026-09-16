# The iOS companion — handover

For a Claude running on the macOS machine, or for whoever picks up
[#114](https://github.com/PeterKnego/roost/issues/114).

Everything below that can be established without a Mac has been. What remains
needs one, and this document exists because **the development host cannot do
any of it**: `which xcodebuild swift swiftc` finds nothing, `uname` says Linux.
An iOS app cannot be built, signed, run or tested there, so nothing in this
file has been compiled by the person who wrote it. Treat every code shape as a
proposal, not as something that has run.

Same precedent as `docs/macos-artifact-verification-handover.md`, and for the
same reason.

## 1. What the app is

**A webview around the page roost already serves, plus push registration and a
share sheet.** Home Assistant's own app is largely this; its native parts are
push, location, sensors and a barcode scanner, and roost wants one of them.

Deliberately not: a native terminal, a native file tree, or a second UI to keep
in step with the web one. roost's web UI already works on a phone as of v0.6.0
— one pane at a time, terminal keys, a paste button (#97), and the touch
affordances in #110.

## 2. What is already true, so you do not re-derive it

- **roost is reached over HTTPS through a Cloudflare Tunnel**, gated by
  Cloudflare Access (email SSO, 24-hour session). The dev instance is
  `https://roost.black.si`. A webview must survive that login flow; it is a
  normal browser redirect, so `WKWebView` with a persistent
  `WKWebsiteDataStore` should carry the cookie, but **this is unverified.**
- **roost binds `127.0.0.1` only.** The tunnel is the boundary. The app never
  talks to a LAN address.
- **Every browser-facing websocket checks `Origin`** and refuses a handshake
  carrying none. A `WKWebView` sends one, so the app lands on the *browser*
  side of that rule and its origin must be allowlisted — `allowed_origins`,
  global config only. See CLAUDE.md on why `src/ide.rs` inverts the same rule
  and why the two must not be reconciled.
- **The notification content already exists server-side.** `notify.rs` holds a
  bounded, persisted, machine-wide ring of `{id, project, session, title, body,
  at, read}`. The app does not need to invent a payload.

## 3. The transport fork, which is the real decision

**Web Push does not work in a webview.** iOS grants Web Push to a web app added
to the Home Screen, not to a `WKWebView` inside a third-party app. So:

| | Home Screen web app (#112 + #113) | Native app (#114) |
|---|---|---|
| transport | Web Push, VAPID | APNs |
| who sends | roost itself | a relay holding APNs credentials |
| needs | nothing external | Apple Developer account, a hosted relay |
| roost can do it alone | yes | no — it binds loopback and has never met the device |

That relay is what Home Assistant solved with Nabu Casa, and it is a product
decision: every notification title passes through it.

**So the order matters.** #113 lands Web Push on Linux and Dean checks whether
an installed web app raises the notification on his own iPhone. If it does, the
app's remaining value is onboarding, durable auth and the share sheet — real,
but far smaller than "roost on a phone", and buildable with no relay at all.

**Start with the parts that need no push.** A stored URL and a surviving login
are most of what makes a phone app feel like an app, and neither depends on
#113.

## 4. Suggested first milestone

Nothing here is on the critical path of #113, which is the point.

1. A single-screen app: a text field for the roost URL, stored in the keychain,
   and a `WKWebView` on it.
2. Verify the Access login completes **and survives a cold launch** — that is
   the first thing that would make the app worse than Safari if it failed.
3. Pull-to-refresh, and a way back to the URL field when the stored one is
   wrong. A companion app that cannot be re-pointed is a reinstall.
4. The share extension: text or a URL shared to roost, landing in a Claude
   prompt. **This is the one genuinely native-only interaction** and may be the
   best reason for the whole app; it is worth building early so it can be
   judged.

## 5. Distribution, uncosted

App Store review on every release, permanently, for a single-maintainer
project. TestFlight for one user is far lighter and may be the honest answer
for a long time. Worth settling before the first build rather than after.

## 6. What to send back

- Whether the Access login survives a cold launch in `WKWebView`, and what it
  took.
- Whether an installed **Home Screen** web app on the same device raises a
  notification once #113 lands — the question that sizes this whole issue.
- Anything roost's web UI does badly inside a webview that it does not do badly
  in Safari. That is a roost bug, not an app bug, and it comes back here.
