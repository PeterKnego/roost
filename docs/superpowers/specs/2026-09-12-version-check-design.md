# Telling you a newer roost exists

*2026-09-12. Status: designed, not implemented. Issue
[#65](https://github.com/PeterKnego/roost/issues/65), step 2. Steps 1a, 1b and
3 are merged (#77, #78). Decisions from conversation on 2026-09-12, one of
which reverses a constraint the issue states — see* The reversal *below.*

## What and why

roost can already say what it is running and how it got there. `BuildInfo`
carries the version, commit, build time, the baked channel, whether roost may
replace its own binary, and a best-effort owner; About shows all of it, and
offers the upgrade command for the four channels roost must never act on
itself.

What it cannot do is tell you a newer version exists. That is this step, and it
is the only part of #65 that requires roost to reach the network.

Deliberately small: **learn the newest published version, and say so in About.**
No download, no button, no banner. The `[Update]` flow #65 settled on needs
this answer first, and needs it to be trustworthy before anything acts on it.

## The reversal

**#65 says the check must be "off by default or explicitly opt-in". This design
has it on by default,** with a global-only setting to turn it off. Recorded here
rather than quietly diverging, because it is a constraint the issue states in
its own words.

Two things changed since it was written.

The first is that the issue's premise — "this is roost reaching the network,
which it does not do today" — overstates the novelty, and the issue now carries
a note saying so. roost's whole purpose is running agents; `claude`, `cargo`,
`npm` and `git` reach the internet constantly from processes roost spawned,
with the user's credentials. The network is not what is being introduced.

The second is the timing decision below. A check that fires *lazily, when a
browser connects* is not an unattended daemon reaching out; it is a request that
follows a person opening the application, like every other request that machine
makes. A roost left running for a month makes no requests until someone looks
at it.

What remains genuinely new is narrower, and the measures below are sized to it:
roost's own code names the destination (hence global-config-only), and the
request runs in the process that holds the websocket that spawns shells (hence
a detached thread, a short timeout, and no panic escaping it).

## How roost reaches the network

**`ureq`, promoted from a dev-dependency to a runtime one.**

It is already in `Cargo.lock` along with `rustls` and `ring`, because
`tests/http.rs`, `tests/lifecycle.rs`, `tests/roots.rs` and `tests/settings.rs`
use it — so no new crate enters the tree, though those three crates do join the
shipped binary.

The alternative considered and rejected was shelling out to `curl`, which would
have added nothing to the binary and matched an idiom this codebase uses
heavily (`git` ×9 call sites, `ps` ×4). It was rejected for the PATH
dependency: a check that silently degrades on a host without `curl` answers
"could not tell" for a reason that has nothing to do with the network.

## What roost checks against

**The crates.io sparse index, for every channel:**
`https://index.crates.io/ro/os/roost`.

One endpoint, one parser, one failure mode — not the three that #65's table
implies. The reasoning is measured rather than assumed:

- **Every release publishes there.** `dist-workspace.toml` has
  `publish-jobs = ["homebrew", "./publish-crates-io"]`.
- **The channels do not meaningfully diverge.** On 2026-09-12 crates.io, the
  GitHub tag and the brew formula all report `0.5.2`, and the tap is pushed by
  the release run itself — measured at 68 seconds after the release published
  (`09:03:58Z` → `09:05:06Z`).
- **The GitHub releases API is rate limited to 60 requests per hour per IP**
  when unauthenticated. Several roosts behind one NAT — an office, a VPN exit —
  share that budget, so the check would degrade precisely where roost is most
  likely to be deployed. Measured: `x-ratelimit-limit: 60`.
- **The sparse index is CDN-cached and unauthenticated**: `cache-control:
  public,max-age=600`, `x-cache: HIT`, 7288 bytes for the whole crate history.

**The request identifies itself.** crates.io's crawler policy asks clients to
say who they are, so the `User-Agent` is `roost/<version>
(+https://github.com/PeterKnego/roost)`. A version check that looks like an
anonymous scraper is one a registry is entitled to block.

**Yanked versions are skipped.** The index marks them, and a yanked release is
not something to tell a user to upgrade to.

**Prereleases are out of scope** because there are none to handle: the index
currently lists `0.4.0, 0.5.0, 0.5.1, 0.5.2` and no `-rc`, even though rc tags
exist in git. If that changes, the comparator below must learn about them
before this is shipped again.

## When it fires

**Lazily, when a workspace websocket connects, if the stored answer is stale.**
The check runs on a detached thread; the connection never waits for it.

Two intervals, not one:

- **24 hours after a successful check.** #65 asks for days rather than minutes.
- **1 hour after a failed one.** A single interval cannot serve both cases:
  punishing a transient failure with a day of silence is wrong, and retrying
  from an offline box on every connect is worse.

## The three outcomes

```rust
enum Latest {
    UpToDate,
    Newer(String),
    Unknown,
}
```

The same discipline as `install.rs`'s `Replaceable`, and for the same reason:
"could not reach crates.io" is not "you are up to date". The issue states this
requirement outright — *"the badge must be able to say I do not know"* — and it
is the one property most likely to be lost in a refactor, because two of the
three states are cheerful and the third is the one that matters.

Concretely, `Unknown` covers: no network, DNS failure, a non-200 answer, a body
that does not parse, an empty index entry, and a version string the comparator
cannot read. None of them may be reported as `UpToDate`.

## Configuration

**`version_check: bool`, default `true`, global-only.**

`config::validate` already refuses a project-scoped write for a global-only key
— *"{key} is a global setting; switch the scope to global"* — so a cloned
repository cannot enable it, disable it, or point it anywhere. The endpoint is
**not** configurable at all. A setting that let a repo name the host roost
fetches from would be the hole this whole section exists to avoid.

## State

A small file in the state directory, beside the rest of the workspace state —
not in the global config, which is the operator's file to hand-edit:

```json
{ "checked_at": 1789234567, "latest": "0.5.3", "outcome": "newer" }
```

The state directory is the right home for the same reason the session registry
lives there: it is roost's own bookkeeping, it is per-installation, and losing
it costs one extra request rather than a wrong answer.

Written atomically — temp file with a pid-unique name, then `rename` — per
CLAUDE.md's rule about persistent evidence, so a reader never sees it
half-written.

## Display

One more row in About, `Latest`, with three renderings:

| Outcome | Row |
|---|---|
| `Newer("0.5.3")` | `0.5.3 available` |
| `UpToDate` | `up to date` |
| `Unknown` | `could not check` |

The `Upgrade` row that shipped in #78 already says what to type, and is already
absent for the two channels roost will eventually update itself. Nothing else
changes: **no banner, no notification, no button.** Those belong to the
`[Update]` flow, which needs a notification surface that is not `notify.rs`'s
per-project `Notice` — a version is not about any project, and that store's own
documentation says a notice raised in a project with no tab open reaches
nobody.

## Testing

**The HTTP call is injected**, exactly as `registry::reconcile_with(roots,
snapshot_fn)` already does for its process snapshot. Every branch below is then
testable with no network:

- the version comparator, as a table, including equal, older, newer, and
  unreadable
- yanked entries skipped, including an index where the *newest* entry is yanked
- each of the three outcomes, including every distinct way `Unknown` arises
- both staleness intervals: fresh, stale-after-success, stale-after-failure
- the state file round-tripping, and a truncated one reading as `Unknown`
  rather than as an answer

Plus a browser assertion that the row renders each of the three states, using
the same technique #78 established: switching tabs re-renders About from
`state.settings` by reference, while closing and reopening does not, because
the settings button sends `RequestState` and the server's values overwrite
anything a test sets.

### What a green suite cannot see

- **The real request.** The injected version never talks to crates.io. One
  manual run against the live index, reported with its output, before this
  ships.
- **That the index URL is right.** The `ro/os/` prefix is derived from the crate
  name's length; a rename would silently 404, which reads as `Unknown` forever
  — cheerfully, and with no error anyone would notice. The manual run is what
  catches this, and it is worth re-running after any rename.
- **Rate limiting or a block.** Nothing here exercises what crates.io does to a
  client it dislikes. The `User-Agent` is the mitigation; there is no test for
  it.

## Deliberately out of scope

- Any download, verification, or replacement of a binary.
- The `[Update]` / `[Later]` / `[Skip]` notification, and the persisted
  "skipped version" state it needs.
- Per-channel checking. If crates.io ever lags a release badly enough to
  matter, this is where that decision gets revisited.
- Prerelease versions (see above — there are none to handle today).
- Telling anyone anything on the overview page (`/`), which has no notification
  surface at all.

## Open, for the review to settle

1. **Is `version_check` the right key name**, beside `autosave`,
   `show_hidden`, `share_selection` and `follow_tree`? `check_for_updates` reads
   better in a settings dialog; `version_check` reads better in a config file.
2. **Should `Unknown` distinguish "never checked" from "the last check
   failed"?** They are different facts and the row could say so — *"not checked
   yet"* against *"could not check"* — at the cost of a fourth state in a design
   whose whole argument is that three is the right number.
3. **Does the row appear when `version_check` is off?** Showing nothing is
   tidy; showing *"version checks are off"* is discoverable, and About is
   exactly where someone goes to wonder about this.
