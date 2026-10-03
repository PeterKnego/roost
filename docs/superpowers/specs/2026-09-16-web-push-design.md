# A notification that reaches your phone with no tab open

*2026-09-16. Status: designed, not implemented. Issues
[#112](https://github.com/PeterKnego/roost/issues/112) (the manifest) and
[#113](https://github.com/PeterKnego/roost/issues/113) (Web Push), both steps of
[#99](https://github.com/PeterKnego/roost/issues/99). The iOS shell is #114 and
is built on the Mac; this spec is the half that can be built and tested here.*

## What is missing, in the codebase's own words

`src/notify.rs`:

> The cost of that, stated plainly: a notice raised in a project with no tab
> open reaches nobody until that project is opened.

Everything that knows there is something to say already exists and ships:
`claudehooks` fires on `SessionStart`/`Notification`/`Stop`, `notify.rs` is a
bounded persisted machine-wide store, `roost notify` and the OSC 777/9
sequences let any process raise one, and `static/sw.js` already turns one into
an OS notification. That file's header says why it is a worker at all:

> a service worker is the only thing that can later receive a Web Push message
> with no tab open. Doing it here now means adding push is a `push` listener,
> not a rewrite.

This is that `push` listener, and the server side that feeds it.

## Step 1: installable (#112)

`static/manifest.webmanifest`, linked from both pages, `display: standalone`,
`start_url: "/"`, `scope: "/"`.

`start_url` is the project list rather than a project: roost serves many, and
an installed app pinned to one is wrong for the tool. `scope` must be `/` or
the first `/frag/` fetch leaves the app's own scope.

**This is not cosmetic and it is not step 2's chrome.** iOS grants Web Push
only to a web app added to the Home Screen, so without a manifest there is no
route to a notification at all.

## Step 2: Web Push (#113)

### VAPID, and where the key lives

An ES256 keypair, generated once and stored at
`$ROOST_STATE_DIR/push/vapid.json` at **0600** — the same care
`claudesess::record` gives a file that merely *names* a transcript, for a file
that is the authority to wake every device roost knows.

The public key, as the uncompressed P-256 point in base64url, is what the
browser needs as `applicationServerKey`; it is served, and it is the only half
that leaves the machine.

Per push: a JWT (`{"alg":"ES256","typ":"JWT"}`, `aud` = the *origin* of the
endpoint, `exp` = now + 12h, `sub` = a `mailto:`), and
`Authorization: vapid t=<jwt>, k=<pubkey>`.

### Payload: encrypted, not payloadless

Both were considered. A payloadless push wakes the worker and lets it fetch the
notice, which needs no HKDF and no AES-GCM — materially fewer crates.

**Rejected, and the reason is not cryptographic.** The worker's fetch would
have to reach roost, and roost is behind Cloudflare Access with a 24-hour
session. A push arriving after that session lapses would wake a worker that
cannot read anything, and the only honest thing it could then show is "roost
needs you" with no idea why — a notification that costs a phone-unlock to
discover it says nothing. The offline case is the same shape.

So: `aes128gcm` (RFC 8188), keyed by ECDH against the subscription's `p256dh`
and `auth`. The payload is genuinely end-to-end — the push service relays
ciphertext it cannot read — so this is not a disclosure decision, only a
dependency one.

Crates: `p256` (ECDH + ECDSA), `hkdf` + `sha2`, `aes-gcm`, `base64`. Settled in
conversation: pure Rust rather than shelling out to `openssl`, because ES256
wants raw `r||s` and openssl emits DER, and a missing or differently-built
openssl would turn a notification into a silent no-op on exactly the machines
least able to debug it.

**An HTTP client is also needed**, and #85's version check wants the same one.
Whichever lands first picks it; this spec assumes `ureq`, already a
dev-dependency, for that reason and no other.

### Subscriptions are a capability, and are stored like one

`endpoint`, `p256dh`, `auth`, per browser, at
`$ROOST_STATE_DIR/push/subs.json`, 0600, written atomically.

A subscription is permission to wake a device. It is bounded (**32**, oldest
evicted) so a browser that resubscribes on every load cannot grow the file
without limit, and it is machine-wide like the notice ring it serves.

### Where the send happens, and where it must not

`notify_and_broadcast` in `hub.rs` is where a notice reaches clients today, and
it runs **under the hub lock**. A push is a network call with a timeout.

CLAUDE.md: *"Never hold a lock across blocking I/O. This project has already
shipped one deadlock that way."* So the send is handed to a detached thread
with the notice already cloned, exactly as `do_close_project` hands off its
session-killing work — not spawned per notice from inside the lock, but queued
to a single sender thread, so a burst of notices cannot fan out into a burst of
threads.

### A dead subscription, and the three answers

A push service answers **410 Gone** for a subscription that no longer exists.
That is positive evidence, and the only response that removes anything.

Everything else — a timeout, a 500, a DNS failure, a refused connection — is
*could not tell*, and the subscription stays. CLAUDE.md's table is eleven rows
of what happens when "I could not reach it" is read as "it is gone"; a phone on
a train would otherwise unsubscribe itself.

## Testing

The parts that can be tested here, and the one that cannot.

- **The VAPID JWT must be verified against a known-good public key**, not
  merely produced. A signature routine that returns 64 bytes of anything passes
  a shape check.
- **The `aes128gcm` output must be decrypted back**, in the test, with the
  subscription keys — a round trip, because an encryption that produces
  plausible ciphertext nobody can read is the failure mode, and no push service
  will ever tell us.
- **Test vectors from RFC 8291** where they exist, so the implementation is
  checked against the standard rather than against itself.
- **410 removes and 500 does not**, asserted on the stored file, not on a log
  line.
- **The send must not hold the hub lock**, and the test for that is a timing
  one: a push endpoint that never answers must not stall a second notice. A
  green run of a deadlock test proves nothing — CLAUDE.md, on timing the runs.
- **A browser test** for the manifest, the subscription handshake and the
  `push` listener, driven with a stubbed push service.

**What cannot be tested here:** whether an installed web app on *Dean's iPhone*
actually receives it. iOS grants Web Push only to a Home Screen app, this host
is headless Linux with Chromium, and #97 and #110 both ended at the same
boundary. That answer decides how much native app #114 has to be, so it is the
first thing to check once this lands.
