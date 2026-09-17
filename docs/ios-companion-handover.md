# The native iOS app — handover

For a Claude running on the macOS machine, or for whoever picks up
[#114](https://github.com/PeterKnego/roost/issues/114).

**Nothing in this document has been compiled by the person who wrote it.** The
development host is a headless Linux VM — `which xcodebuild swift swiftc` finds
nothing, `uname` says Linux — so an iOS app cannot be built, signed, run or
tested there. Treat every code shape here as a proposal, not as something that
has run. Same precedent, and the same reason, as
`docs/macos-artifact-verification-handover.md`.

## 1. What this is

**A native app. Not a webview.** An earlier draft of this document proposed a
`WKWebView` shell; that was rejected — the point is an iOS app experience.

The shape follows #15's finding that the phone job is *talking to Claude*,
not driving a three-pane IDE:

- a **real terminal** (`SwiftTerm`)
- native lists of projects and sessions
- a notification that opens the session it came from
- the **share sheet** — text or a URL into a Claude prompt

## 2. The biggest win, and why it justifies native on its own

roost's web UI has needed a bespoke workaround for every basic text gesture,
because xterm.js draws its own selection layer and is driven by mouse events:

- **#97** — a paste button, because iOS raises no Paste callout over a terminal
- **#110** — a select-mode toggle, because a touch drag scrolls instead of
  selecting

`SwiftTerm` is a real `UIView`. Selection, the loupe, and the system copy/paste
menu come from iOS, not from us. **Both of those issues stop existing** in a
native client.

## 3. What roost already gives you, and the one gap

**The terminal is a clean fit.** `/ws/{project}/term/{name}` streams **raw PTY
bytes** — exactly what `SwiftTerm` consumes. No translation layer, no
re-encoding. Send keystrokes back as binary frames; send `resize:{cols}x{rows}`
as a text frame (see `src/term.rs`).

**The workspace is already JSON.** `/ws/{project}/_workspace` carries
`Intent`/`Event` (`src/proto.rs`): the layout snapshot, notices, terminal
lifecycle, everything the web client uses.

**The gap: the lists are HTML.** `/frag/_overview_projects` and its siblings
render htmx fragments. A native client must not parse those. The data exists in
`registry::known_projects`; only its rendering is HTML-only, so a small JSON
surface is a rendering addition, not a redesign. These are reads — GETs — so
they cost nothing against CLAUDE.md's two-POST cap. **That work belongs on the
Linux side; ask for it rather than screen-scraping.**

## 4. Push: no relay is needed

An earlier draft of this document said APNs requires a relay holding the push
credentials. **That was wrong and it mattered**, because it was the main
argument against going native.

APNs is an *outbound* HTTP/2 call to `api.push.apple.com`, authenticated by a
JWT signed with an APNs auth key (`.p8`). A self-hosted server with internet
access can push directly, and roost has internet access.

Home Assistant's relay exists because HA is **multi-tenant** — thousands of
users without developer accounts, instances often unreachable. Neither applies
to one owner with one always-on instance.

The real cost is narrower: **APNs requires HTTP/2**, so roost needs an HTTP/2
client, which is heavier than the HTTP/1.1 client #85 and #113 want. A
dependency argument, not an architectural one.

## 5. Security — do not improvise this

roost has **two opposite rules**, both deliberate, and CLAUDE.md says plainly
that *"reconciling the two is the vulnerability, not the cleanup"*, naming
CVE-2025-52882 (Claude Code's own extension shipped a socket Origin-blind and
unauthenticated through 1.0.23):

- every **browser-facing** socket refuses a handshake carrying **no** `Origin`
- `src/ide.rs` refuses any handshake that **carries** one, and authenticates by
  constant-time comparison against a lock-file token

**A native app is a third case.** It is not local, so the lock-file token does
not reach it. It is not a browser, so an `Origin` allowlist is theatre — the
app can send any string it likes.

Proposed, to be argued in the spec before code:

- a **Cloudflare Access service token** (`CF-Access-Client-Id` /
  `CF-Access-Client-Secret`) carries the app through the tunnel, so the
  transport is authenticated before roost sees the request
- roost gains a **third rule**, named as one, for a remote non-browser client
  with its own token — never by loosening either existing rule

## 6. What gates everything: an Apple Developer account

Nothing in this repo carries a team identifier, and `cargo-dist` does not
notarize, so there is no account in the picture yet.

- without one: a free Apple ID sideload **expires every 7 days**
- with one: the `.p8` for APNs, and TestFlight for a single user — far lighter
  than App Store review on every release, and probably the honest answer for a
  long time

Settle this first. It gates push *and* any install that lasts more than a week.

## 7. Suggested first milestone

Deliberately nothing that needs push, so it is not blocked on #113:

1. Connect: a roost URL and an Access service token, stored in the keychain.
2. List the sessions of one project over `/ws/{project}/_workspace`.
3. Open one in `SwiftTerm` over `/ws/{project}/term/{name}`, keystrokes back,
   `resize:` on rotation.
4. Confirm selection and copy work with a finger, with nothing bespoke — that
   is the thesis of the whole app, and if it is awkward, say so early.

## 8. What to send back

- Whether #97 and #110's workarounds really do become unnecessary. That is the
  claim this app rests on.
- Anything the protocol makes awkward for a native client. That is a roost bug
  and it comes back here.
- Whether the JSON surface in §3 is the right shape, before it is built.
