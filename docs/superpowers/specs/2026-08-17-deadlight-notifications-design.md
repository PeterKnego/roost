# deadlight notifications — design

**Status:** proposed
**Date:** 2026-08-17

## Why

Claude runs in a terminal pane and works unattended for minutes at a time. Two
moments matter and both are currently invisible unless you are watching the
pane: Claude needs a decision, and Claude is finished. deadlight already knows
which session produced which byte — it owns the PTY — so it is the natural
place to turn "this session wants you" into something that reaches you when
the tab is in the background.

The feature ships as browser-delivered OS notifications. It is shaped so that
delivery to a phone with no tab open is a later addition to one component, not
a rewrite.

## Scope

In: an escape-sequence ingress from the terminal, a persisted server-side
notice store, cross-project delivery over the workspace socket, an in-page
notification centre, OS notifications via a service worker, and a
`deadlight notify` subcommand.

Out (deferred, see Future work): Web Push subscriptions, third-party relays
(ntfy/Pushover), per-project notification settings, sound.

## The notice

```rust
struct Notice {
    id: u64,          // monotonic, assigned by the store
    project: String,  // rel path, e.g. "karpie/src" — server truth, never from payload
    session: String,  // session name — server truth, never from payload
    title: String,    // <= 100 chars after sanitising
    body: String,     // <= 500 chars after sanitising
    at: u64,          // unix seconds
    read: bool,
}
```

`project` and `session` come from the PTY pump's own identity, not from the
escape sequence. This is what makes a notice attributable: text arriving over
a terminal is attacker-influenced (see Threat model), so nothing the payload
says about its own origin is trusted or even parsed.

## Ingress — `src/osc.rs` (new)

A stateful byte-stream parser:

```rust
pub struct Parser { /* accumulating state */ }
pub struct Parsed { pub title: Option<String>, pub body: String }
impl Parser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Parsed>;
}
```

Stateful because a sequence can straddle an 8 KiB read boundary — that is the
whole reason this is not a regex over a chunk.

Two accepted wire forms:

| Form | Sequence | Notes |
|---|---|---|
| urxvt/kitty/dunst | `ESC ] 777 ; notify ; TITLE ; BODY ST` | The one `deadlight notify` emits |
| iTerm2 | `ESC ] 9 ; BODY ST` | Title defaults to the session name |

`ST` is `BEL` (`0x07`) or `ESC \`. In the 777 form a `;` inside BODY is
literal — only the first three delimiters are structural — so message text
does not need escaping.

Bounds, all of which exist to keep a hostile or merely binary stream from
turning the parser into an accumulator:

- An in-flight sequence is abandoned after 4 KiB without a terminator.
- An in-flight sequence is abandoned on `CR` or `LF`. Real OSC never contains
  either, and this bounds the damage from `cat` of a binary file that happens
  to contain `ESC ]`.
- An `ESC` inside a sequence ends it, unless it is the `ESC \` terminator.
  That is what real terminals do, and it is also what keeps the parser
  bounded: there is no state in which it consumes input without either
  buffering it against the 4 KiB cap or ending the sequence. A parser that
  instead tries to skip over an embedded ANSI sequence and carry on
  reintroduces exactly that unbounded state, and can swallow the next
  sequence's prefix.
- A sequence ends at its own first terminator. No look-ahead: a later,
  unrelated `ESC \` in the same chunk must not retroactively demote an
  earlier `BEL`.
- Parsed fields are sanitised: invalid UTF-8 is replaced lossily, C0/C1
  control characters are stripped, then title and body are truncated to their
  caps. No `ESC` can reach this step — one would have ended the sequence.

**The byte stream is not modified.** The parser scans a copy; the original
bytes reach xterm.js untouched. xterm.js silently discards OSC codes it has no
handler registered for, so the sequence is invisible in the terminal anyway,
and not rewriting the stream means this feature cannot corrupt terminal
output. Failing open on cosmetics is the right trade against failing closed on
correctness.

### Where the parser runs

In the existing PTY pump thread in `session.rs`, positioned deliberately:

```
read(chunk)                    // blocking, no lock held (unchanged)
  -> parser.feed(&chunk)       // pure CPU, no lock held        <-- new
  -> lock registry             // push_scrollback + fan-out (unchanged)
  -> drop lock
  -> notify::publish(notices)  // <-- new, lock released first
```

Parser state is a local in the pump closure, not a field on `Session`, so
scanning needs no lock at all. Publishing happens after the registry lock is
dropped. Together these mean the change introduces no new lock ordering and
cannot hold the session registry across the store's I/O — the constraint this
codebase has already violated once.

## Store — `src/notify.rs` (new)

A bounded global ring, newest last, capped at 100 notices, behind a leaf mutex
that is never held across a broadcast. Persisted to
`$DEADLIGHT_STATE_DIR/notifications.json` using the write-then-rename
discipline `wsstate.rs` already uses, in the same `0o700` directory.

Persistence — not just in-memory queueing — is what makes the 3am case survive
a deadlight restart rather than only a closed tab.

```rust
pub fn publish(project: &str, session: &str, p: Parsed);  // assigns id + at, evicts, persists, broadcasts
pub fn list() -> Vec<Notice>;
pub fn mark_read(id: u64);
pub fn clear();
pub fn load();                                            // at startup
```

**Rate limit:** at most 10 notices per session per rolling minute. Beyond
that, further notices from that session are dropped, and the drop is counted
so the next accepted notice from the session can say `(N suppressed)`. A
runaway loop in a terminal must not be able to evict the ring or machine-gun
the OS notification centre.

## Egress — cross-project delivery

> **Superseded 2026-09-04.** This section describes delivery as it originally
> shipped and no longer describes the code. Notices are now delivered to the
> clients of the project they belong to and nobody else
> (`hub::broadcast_to_project`, `notify::list_for`), and `MarkAllNoticesRead`
> and `ClearNotices` are scoped the same way (`mark_all_read_in`,
> `clear_in`). The *store* is still machine-wide; only delivery changed.
>
> Why: a browser tab is opened on exactly one project key — and a worktree is
> a key of its own — so a foreign notice had no tab there to focus and nothing
> to do on a click but navigate the user away from the project they were
> working in. Clicking a `roost` row from an `ultima_cluster` workspace
> replaced that workspace; the service worker's "reuse any roost window"
> fallback did the same from an OS banner. The known cost is that a notice
> raised in a project with no tab open now reaches nobody until that project
> is opened, and the "notification centre on `/`" listed under Future work
> below is still not built. See `notify.rs`'s module doc.

`Hub` is per project, but `hub::REGISTRY` holds every live hub. `publish`
clones the `Arc`s under the registry lock, drops it, then broadcasts to each
hub in turn. A browser sitting on `/karpie` therefore sees a notice fired in
`/deadlight` — the case that actually matters when Claude is running in three
projects at once.

New wire types in `proto.rs`:

```rust
Event::Notice  { notice: Notice }       // one, live
Event::Notices { list: Vec<Notice> }    // full history, sent on connect
Intent::MarkNoticeRead { id: u64 }
Intent::ClearNotices
```

Notices are deliberately **not** folded into `WorkspaceView`, or every
workspace change would rebroadcast the whole history to every client.

`Event::Notices` carries the whole store — every project's notices, not just
the connecting client's — because the badge counts what needs your attention
anywhere, and the panel routes across projects.

Read state is global, not per client: marking a notice read in one browser
marks it read in all of them, the same way every other piece of workspace
state mirrors. `MarkNoticeRead` and `ClearNotices` therefore rebroadcast
`Event::Notices` to every hub, so no two clients can disagree about the badge
count.

## Client

**Service worker, not `new Notification()`.** A new `/sw.js` route, registered
on load, showing notices via `registration.showNotification()`. The code is
equivalent today; the difference is that this is the form that later grows a
`push` event handler without touching anything else. `tag` is
`project/session`, so a chatty session replaces its own notification instead
of stacking twenty.

**Clicking targets a specific browser tab.** `notificationclick` runs with
user-gesture privilege, so the handler may focus and navigate windows. In
order:

1. `clients.matchAll({type: 'window', includeUncontrolled: true})`, find a
   window whose URL is already the notice's project → `client.focus()`, then
   `postMessage` the target session.
2. Otherwise focus any deadlight window and `client.navigate('/<project>')`,
   carrying the session in the fragment.
3. Otherwise `clients.openWindow('/<project>#session=<name>')`.

The service worker calls `clients.claim()` on activate, or a freshly
registered worker would not control the page until the next reload and step 1
would find nothing to focus.

**Once focused**, routing reuses existing machinery: send `ActivateTab` for
the terminal tab, or `OpenTab` first if that session is not open in any pane.
Every connected client follows, because that is already how the workspace
socket behaves. On load, a `#session=<name>` fragment is consumed the same
way and then cleared from the URL.

## Attention cues

OS notifications are the out-of-app channel. These are the in-app ones, for
when deadlight is on screen and no notification is shown — and for when
permission was never granted:

- **Bell badge** in the header: unread count across all projects.
- **Browser tab**: `document.title` gains an `(N)` prefix while unread, and
  the favicon swaps to a badged variant. This is the only cue that works from
  a background tab without notification permission.
- **Terminal tab dot** in the tab strip for the session that fired, cleared
  when that tab becomes active. This one is necessarily per-project — a
  session in another project has no tab on screen — so it complements the
  badge rather than replacing it. Attention needed *elsewhere* is the bell's
  job; attention needed *here* is the dot's.

**Notification centre.** A bell button in the existing `<header>`, beside
`#refresh`, carrying an unread count badge. Clicking opens a panel listing
recent notices as `project · session · title · relative time`; each row is
clickable and routes as above. The panel carries *Mark all read*, *Clear*, and
— when permission has not yet been granted — an *Enable OS notifications*
button. Permission is requested from that button's click, never automatically
on load: browsers penalise spontaneous permission prompts, and a prompt you
did not ask for is worse than no notifications.

All notice text is inserted with `textContent`, never `innerHTML`. Any
server-rendered notice text goes through `esc()` in `render.rs`.

**Degradation.** Service workers and the Notification API both require a
secure context. `localhost` and `tailscale serve` HTTPS qualify; plain
`http://` to a tailnet IP does not. Where the context is insecure or
permission is denied, the in-page notification centre works fully, OS
notifications are unavailable, and the panel says which of the two it is
rather than failing silently.

## How Claude learns notifications exist

Three layers, because the discovery problem is different for a human reading
docs, a model reading its environment, and an automated hook.

**1. Environment, at spawn.** `session.rs` already builds the child's
environment; it gains three variables:

```
DEADLIGHT_NOTIFY=1
DEADLIGHT_PROJECT=<project rel path>
DEADLIGHT_SESSION=<session name>
```

This is the layer that makes the capability *self-describing*: a model that
wonders whether it can notify can answer the question from its own
environment, and the project/session variables let anything that wants to
label itself do so.

**2. `deadlight notify` on PATH.** `main.rs` currently treats `argv[1]` as a
port. It gains one subcommand:

```
deadlight notify <title> [body]
```

It writes the OSC 777 sequence to `/dev/tty`, falling back to stdout, and
exits — it never binds a port or starts a server. Writing to `/dev/tty` rather
than stdout is the load-bearing detail: Claude Code captures hook stdout, so a
hook that printed to stdout would be swallowed. `/dev/tty` is the controlling
terminal, which is the PTY deadlight is reading.

Exits non-zero with a message on stderr if there is no controlling terminal
and stdout is not a tty, so a misconfigured hook fails loudly instead of
silently doing nothing.

**3. Documentation.** A new `docs/notifications.md` covering the sequence, the
subcommand, the environment variables, and a ready-to-paste Claude Code hook
configuration wiring the `Notification` and `Stop` hooks to
`deadlight notify`. deadlight does not write into a user's repo or
`.claude/settings.json`; installing hooks stays the user's action.

## Threat model

Any process writing to the PTY can emit the sequence — including `cat` of a
file someone else wrote. The text is therefore treated as untrusted:

| Risk | Mitigation |
|---|---|
| Impersonating another session or deadlight itself | `project`/`session` come from the pump's identity; the UI renders attribution from those fields only, and the payload has no origin fields to parse |
| Terminal/HTML injection via message text | C0/C1 stripped at parse; `textContent` in JS; `esc()` in Rust |
| Ring eviction or notification flooding | 10/minute/session rate limit; 100-notice cap; suppression counted and surfaced |
| Unbounded parser accumulation | 4 KiB cap, `CR`/`LF` abandon |
| Click routing to an unintended session | Session names already constrained by `session::valid_name`; routing uses stored server-side values |

No new HTTP surface and no new verb: ingress is the PTY, egress is the
existing workspace socket. The GET-only invariant is untouched, so this adds
no CSRF surface. `/sw.js` is a static GET like the rest of `/static`.

## Error handling

- The parser never panics: every index is bounded, and malformed input
  discards the in-flight sequence rather than erroring upward.
- A persistence write failure logs to stderr and leaves the notice in memory.
  Notifications are best-effort; a full disk must not take down a terminal.
- `publish` cannot propagate a failure into the pump thread — the pump must
  keep pumping regardless. No panic escapes the pump or a socket thread.
- Service worker registration failure degrades to the in-page centre.

## Caps

| Thing | Cap |
|---|---|
| Retained notices (global) | 100 |
| Title | 100 chars |
| Body | 500 chars |
| Notices per session per minute | 10 |
| In-flight OSC sequence | 4 KiB |

## Testing

Written against this repo's standing question — *would this fail if I deleted
the code it covers?*

**Parser (`osc.rs`, unit).**

- A sequence split across chunk boundaries at *every* byte offset yields
  exactly one notice with the right fields. A stateless implementation passes
  the single-chunk case and fails this one, which is the point.
- An unterminated sequence longer than 4 KiB is abandoned: assert the parser's
  buffered length returns to zero, not merely that no notice was emitted — the
  bug being prevented is accumulation, not emission.
- A chunk of binary noise containing `ESC ]` and newlines yields no notice and
  leaves no state.
- Sanitising asserts the *result*: a title containing `ESC[31m` comes out
  without it; a 300-char title comes out at exactly 100.
- Both wire forms, and a `;` inside BODY surviving as a literal.

**Store (`notify.rs`, unit).** Ring eviction keeps the newest 100 and drops
the oldest; a persistence round-trip preserves ids and read flags; the rate
limiter admits 10 and rejects the 11th within the window, then admits again
after it, and the suppression count is what the next notice reports.

**Delivery (integration).** Two workspace clients on *different* projects; a
notice published for project A must arrive at the client on project B. A
single-client test cannot distinguish cross-project broadcast from
same-project delivery, which is exactly the failure mode this repo has already
shipped once.

**End to end (integration).** Spawn a session whose `DEADLIGHT_CMD` points at
a generated script that emits the OSC 777 sequence, attach a workspace socket,
assert an `Event::Notice` arrives carrying the spawning project and session.
This runs through the real pump rather than calling `publish` directly.

`DEADLIGHT_CMD` splits on whitespace, so the command must be a single path
token — an inline `sh -c 'printf …'` would be torn apart by that split. The
test writes a small script to a temp dir and points `DEADLIGHT_CMD` at it.

**Browser, required before believing it works.** Per this repo's history: that
the OS notification actually appears, that clicking it focuses the tab and
activates the right terminal tab, that the cross-project click navigates, and
that permission denial degrades to the panel rather than a broken bell. Also
that clicking focuses the *right* window when two projects are open in two
tabs, and that the title/favicon badge appears and clears.

Verify the `/dev/tty` write actually reaches deadlight from a real Claude Code
`Stop` hook — the one assumption here that is inferred rather than observed.
It is checked early during implementation, and if hook output cannot reach the
terminal the hook layer is reassessed then. Nothing else depends on it: the
escape sequence and `deadlight notify` work regardless, and only the
fires-automatically-on-stop convenience is at stake.

**Linux host.** Run the suite there too; the parser is platform-independent
but the PTY pump is where this hooks in.

## Files

| File | Change |
|---|---|
| `src/osc.rs` | new — parser |
| `src/notify.rs` | new — store, rate limit, publish/broadcast |
| `src/session.rs` | feed the parser in the pump; three new child env vars |
| `src/proto.rs` | `Event::Notice`, `Event::Notices`, two intents |
| `src/hub.rs` | handle the two intents; send `Notices` on connect |
| `src/render.rs` | bell + badge in `<header>`, notification panel markup |
| `src/routes.rs` | `/sw.js` |
| `src/main.rs` | `notify` subcommand |
| `static/app.js` | socket handling, panel, permission, click routing, title/favicon badge, tab dot |
| `static/sw.js` | new — `showNotification`, `notificationclick`, `clients.claim` |
| `static/style.css` | bell, badge, panel, tab dot |
| `docs/notifications.md` | new — sequence, subcommand, env, hook config |
| `README.md`, `docs/deploy.md` | the CLI is no longer "only a port" |

## Future work

- **Web Push.** The service worker gains a `push` handler; the server gains
  VAPID signing, payload encryption, and subscription storage. This is the
  step that reaches a phone with no tab open, and the reason the client uses a
  service worker from day one.
- **Relay sink.** A configured webhook (ntfy/Pushover) POSTed on publish — a
  cheaper route to the phone than Web Push, at the cost of a third party and a
  token to store.
- **Per-project mute** and a quiet-hours window.
- **Notification centre on the picker page** (`/`) as well as the workspace
  page. The store is already global; only the markup is missing.
