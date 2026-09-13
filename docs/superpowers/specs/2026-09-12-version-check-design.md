# Telling you a newer roost exists

*2026-09-12. Status: designed, reviewed 2026-09-13, not implemented. Issue
[#65](https://github.com/PeterKnego/roost/issues/65), step 2. Steps 1a and 1b are
merged into develop (#77); step 3 is open as #78. Decisions from conversation on 2026-09-12, one of
which reverses a constraint the issue states — see* The reversal *below. The
review on 2026-09-13 changed the state file's schema, named the path by which
the answer reaches About, and settled the three questions that were open.*

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
a detached thread, an explicit timeout, and no panic escaping it).

**A second divergence, smaller and in the other direction.** #65's step 2 asks
for the answer "next to the version in About *and as a quiet mark in the
header*". This design ships About only. That is a scope decision, not a
disagreement — the header mark is the first piece of the `[Update]` flow's
notification surface, and it should arrive with that flow rather than ahead of
it. It is listed here because it changes what on-by-default buys: until the
mark exists, a check nobody sees until they open About mostly keeps the stored
answer warm for the moment they do.

## How roost reaches the network

**`ureq`, promoted from a dev-dependency to a runtime one.**

It is already in `Cargo.lock` along with `rustls`, `ring`, `webpki-roots`,
`flate2` and `url`, because `tests/http.rs`, `tests/lifecycle.rs`,
`tests/roots.rs` and `tests/settings.rs` use it — so no new crate enters the
tree, though those crates do join the shipped binary.

The alternative considered and rejected was shelling out to `curl`, which would
have added nothing to the binary and matched an idiom this codebase uses
heavily (`git` ×9 call sites, `ps` ×4). It was rejected for the PATH
dependency: a check that silently degrades on a host without `curl` answers
"could not tell" for a reason that has nothing to do with the network.

Three settings on that request, each decided rather than defaulted, because
`ureq` 2.12's defaults (read from its manifest and `agent.rs` on 2026-09-13)
are wrong for this use in three places:

- **`proxy-from-env` is enabled.** It is off in ureq's default feature set, so
  a default build ignores `HTTPS_PROXY` entirely. The argument below for
  preferring crates.io is the office behind one NAT — and that office is
  exactly where a proxy is mandatory, so without this feature the check would
  read `Unknown` forever precisely where it was designed to work.
- **The trust store stays `webpki-roots`**, the bundled Mozilla set, not the
  OS store. Behind a TLS-intercepting proxy with a private CA the check
  therefore reads `Unknown`. Accepted and recorded: the alternative is
  `native-tls`, which drags OpenSSL into a static musl build. About says
  "could not check"; it does not say why, and that is the honest limit of what
  this step does.
- **An explicit `timeout()` on the request**, ten seconds. ureq's default is a
  30 s connect timeout and *no* read timeout, so a half-open connection would
  park the detached thread indefinitely. Nothing waits on that thread, so the
  cost would be invisible — which is the reason to bound it, not a reason not
  to.

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
  public,max-age=600`, 7288 bytes for the whole crate history. (Re-fetched
  2026-09-13: 200, same length, four versions, none yanked.)

**The path is derived, not spelled.** The sparse index puts a crate under a
prefix computed from its name's length — one character: `1/`, two: `2/`,
three: `3/{first}/`, four or more: `{first two}/{next two}/`. `ro/os/roost`
is the output of that rule applied to `CARGO_PKG_NAME`, and the rule is a
function with a unit test, so a crate rename changes the URL rather than
silently 404ing into `Unknown` forever. The manual run against the live index
(see *What a green suite cannot see*) then covers only the network, not the
spelling.

**The request identifies itself.** crates.io's crawler policy asks clients to
say who they are, so the `User-Agent` is `roost/<version>
(+https://github.com/PeterKnego/roost)`. A version check that looks like an
anonymous scraper is one a registry is entitled to block.

**Yanked versions are skipped.** The index marks them, and a yanked release is
not something to tell a user to upgrade to. The running version being yanked
itself is not a case this step reports on: if nothing newer and unyanked
exists, the answer is `UpToDate`, and telling someone their copy was pulled is
a different feature with a different tone.

**Prereleases: the index has none, but the running binary can be one.** The
index lists `0.4.0, 0.5.0, 0.5.1, 0.5.2` and no `-rc`; git has
`v0.5.1-rc.1`, `v0.5.2-rc.1` and `v0.5.2-rc.2`, so a checkout built at an rc
tag carries `0.5.2-rc.2` in `CARGO_PKG_VERSION`. The comparator therefore
reads a prerelease suffix on *either* side and orders it below the same
version without one, as semver does — `0.5.2-rc.2` is told that `0.5.2` is
available, which is true. Build metadata (`+…`) is ignored for ordering. A
version string it cannot read at all is `Unknown`, on either side.

## When it fires

**Lazily, when a workspace websocket connects, if the stored answer is stale.**
The check runs on a detached thread; the connection never waits for it.

The trigger is per hub, but the check is **per process and single-flight**: one
in-flight guard for the whole process, taken before the staleness test, so ten
tabs connecting at once — or one browser test opening ten projects — fire one
request, not ten. Two roost processes sharing a state directory are already
handled by the atomic write below; each does its own check and the later
writer wins, which is harmless because both wrote a fact.

Two intervals, not one:

- **24 hours after a successful check.** #65 asks for days rather than minutes.
- **1 hour after a failed one.** A single interval cannot serve both cases:
  punishing a transient failure with a day of silence is wrong, and retrying
  from an offline box on every connect is worse.

A `checked_at` or `failed_at` in the future — a clock stepped backwards, a
state file copied from another host — reads as **stale**, not as fresh until
that future arrives.

**The test harnesses turn it off.** Nine integration test files open
websockets, and every browser test starts a roost. With the check on by
default and fired on connect, a green suite would be making real requests to
crates.io, and a rate-limit or an offline CI runner would surface as a flake
in whichever test lost. So `tests/browser/harness.mjs` and the integration
tests' `ROOST_CONFIG` fixture write `version_check = false`, and one test
asserts that with it off the injected fetch is *never called* — the assertion
that turns "the suite happens not to reach the network" into "the suite
cannot".

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

**`Latest` is computed at display time, never stored.** The state file (below)
holds the fact the network returned — the newest unyanked version — and the
comparison against `CARGO_PKG_VERSION` happens when About is rendered. The
alternative, storing the verdict, fails the first time it matters: upgrade to
0.5.3, restart inside the 24-hour window, and a stored `"outcome": "newer"`
computed by the old binary makes the new one announce "0.5.3 available" while
running 0.5.3. That is the lying version display #65 and #56 both exist to
prevent, and it is why the schema has no `outcome` field.

## Configuration

**`version_check: bool`, default `true`, global-only.**

`config::validate` already refuses a project-scoped write for a global-only key
— *"{key} is a global setting; switch the scope to global"* — so a cloned
repository cannot enable it, disable it, or point it anywhere. The endpoint is
**not** configurable at all. A setting that let a repo name the host roost
fetches from would be the hole this whole section exists to avoid.

**A global file that cannot be parsed means off.** The reader is *not* a copy
of `relaunch_from`, whose `.ok()…unwrap_or(false)` folds absent, unreadable and
unparseable into the default — right for `relaunch`, where the default is the
safe direction, and wrong here, where the default is the request. An operator
who wrote `version_check = false` and later broke the same file with a typo
elsewhere would otherwise get the request they turned off, and nothing in
About would say why. So: absent means `true`; unreadable or unparseable means
`false`. `Settings::warning` already names the broken file in the dialog, so
the silence has an explanation next to it.

## State

A small file in the state directory, beside the rest of the workspace state —
not in the global config, which is the operator's file to hand-edit:

```json
{ "latest": "0.5.3", "checked_at": 1789234567, "failed_at": null }
```

- `latest` is the newest unyanked version the index reported, from the **last
  successful** check. It survives a later failure: a thirty-minute outage must
  not turn "0.5.3 available" into "could not check".
- `checked_at` is when that success happened. It drives the 24-hour interval.
- `failed_at` is when the most recent failure happened, or absent. It drives
  the 1-hour interval, and is cleared by the next success.

Two timestamps because one cannot express "last success 20 hours ago, last
failure 30 minutes ago", which is the state the two intervals were designed
for; and no `outcome`, for the reason given under *The three outcomes*.

The state directory is the right home for the same reason the session registry
lives there: it is roost's own bookkeeping, it is per-installation, and losing
it costs one extra request rather than a wrong answer.

Written atomically — temp file with a pid-unique name, then `rename` — per
CLAUDE.md's rule about persistent evidence, so a reader never sees it
half-written. A file that is missing, truncated, or otherwise unreadable is
"never checked", never an answer.

## Display

### How the answer reaches About

About renders from `state.settings.build`, and that comes from `SettingsView`,
which `Hub::settings` caches and invalidates at exactly four sites:
`RequestState`, `SetSetting`, `SetClaudeHooks`, and a new connection's first
snapshot. The check finishes on a detached thread *after* that first snapshot,
so nothing in the current design would carry its result to a browser.

The route is the one the settings dialog already uses. The result lives in a
process-global (a `OnceLock<Mutex<…>>` beside the in-flight guard), read by
`config::settings_view` into a **new field of `SettingsView`, a sibling of
`build`, not inside `BuildInfo`** — `BuildInfo`'s doc comment promises it is
constant for the life of the process, and this is the one server fact on that
panel that is not. Opening the dialog sends `RequestState`, which invalidates
the cache, so the pane shows whatever the process knows at the moment it is
opened. No push, no broadcast when a check completes: the pane is the only
consumer, and it asks.

### The row

One more row in About, `Latest`, with five renderings:

| State | Row |
|---|---|
| `Newer("0.5.3")` | `0.5.3 available` |
| `UpToDate` | `up to date` |
| `Unknown`, with a recorded `failed_at` | `could not check` |
| no state file at all | `not checked yet` |
| `version_check = false` | `version checks are off` |

The enum stays three-valued. The two extra rows are facts the *state file*
knows rather than the comparator — its absence, and the setting — and About is
exactly where someone goes to wonder about either. "Not checked yet" is the
few seconds after a fresh install, and "version checks are off" is the answer
to "why does this never say anything". Both are assertable in the browser
test, which the silent alternatives were not.

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

- the index path derivation, as a table across name lengths one to five,
  with `roost` → `ro/os/roost` as the row that matters
- the version comparator, as a table, including equal, older, newer,
  unreadable, a prerelease on the running side, a prerelease on the index
  side, and build metadata ignored
- yanked entries skipped, including an index where the *newest* entry is
  yanked, and one where every entry is
- each of the three outcomes, including every distinct way `Unknown` arises
- **the verdict is not stored**: a state file written by a check run "as
  0.5.2" and read "as 0.5.3" must render `up to date`, not `0.5.3 available`
- both staleness intervals: fresh, stale-after-success, stale-after-failure,
  and a timestamp in the future reading as stale
- **a failure keeps the last good answer**: success, then failure, must still
  render `0.5.3 available` and must retry after one hour, not 24
- the state file round-tripping, and a truncated one reading as "never
  checked" rather than as an answer
- single-flight: ten connects with a fetch that blocks fire the fetch once
- the config reader: absent means on, `false` means off, and an unparseable
  file means off
- with `version_check = false`, the injected fetch is never called

Plus a browser assertion that the row renders each of the five states, using
the same technique #78 established: switching tabs re-renders About from
`state.settings` by reference, while closing and reopening does not, because
the settings button sends `RequestState` and the server's values overwrite
anything a test sets.

Each new test should be watched failing with its branch reverted before it is
trusted — CLAUDE.md's *Testing* section says why, and the comparator table in
particular is the kind of test that goes green with the operands swapped.

### What a green suite cannot see

- **The real request.** The injected version never talks to crates.io. One
  manual run against the live index, reported with its output, before this
  ships.
- **Rate limiting or a block.** Nothing here exercises what crates.io does to a
  client it dislikes. The `User-Agent` is the mitigation; there is no test for
  it.
- **A proxy, or an intercepting one.** `proxy-from-env` is a feature flag, not
  a code path this suite drives. A manual run with `HTTPS_PROXY` set to a
  local proxy is the check, and it is worth doing once on the deploy host.

## Deliberately out of scope

- Any download, verification, or replacement of a binary.
- The `[Update]` / `[Later]` / `[Skip]` notification, and the persisted
  "skipped version" state it needs.
- The quiet header mark #65 asks for (see *The reversal*).
- Per-channel checking. If crates.io ever lags a release badly enough to
  matter, this is where that decision gets revisited.
- Saying *why* a check failed. `Unknown` is one row; the reason is in the
  server log if anyone needs it.
- Telling anyone anything on the overview page (`/`), which has no notification
  surface at all.

## Decided in review (2026-09-13)

These were open for the review to settle, and are recorded here so the plan
does not reopen them.

1. **The key is `version_check`.** It sits beside `relaunch`,
   `share_selection` and `worktree_prompt`, which are the same shape, and the
   dialog shows a doc string beside every key, so the file's reading wins over
   the dialog's.
2. **"Never checked" and "the last check failed" are distinguished in the
   display, not in the enum.** They are facts about the state file, and the
   row says so — see *The row*. `Latest` keeps its three variants.
3. **The row is shown when the check is off**, as `version checks are off`.
   Discoverable where discovery happens, and it gives the browser test an
   off state to assert.
