# Claude account groups: rotate a rate-limited Claude onto the next account

*2026-09-14. Status: designed, not implemented. Decisions from conversation on
2026-09-14, after reading [claude-swap](https://github.com/realiti4/claude-swap)
at commit 7187ce8. Issue [#90](https://github.com/PeterKnego/roost/issues/90), step 1 of
three, tracked as [#91](https://github.com/PeterKnego/roost/issues/91); quota
is #92 and automatic switching #93.*

## What and why

A Claude Code subscription has a 5-hour and a 7-day window. Someone with more
than one account wants to keep working when one runs out, and today the only
way from inside roost is to leave roost: log out, log in as the other account,
come back, and hope the conversation resumes. claude-swap solves this on the
command line. This spec gives roost the same ability in the shape roost
already has: a terminal per launch, a choice made on the ✻ button, and a
conversation that follows the person rather than the account.

Four decisions were made before the design, and everything below follows from
them:

1. **Session mode, not a global switch.** Each account is its own
   `CLAUDE_CONFIG_DIR` profile. Nothing rewrites the login under `~/.claude`,
   so a click in roost never moves a Claude running outside it. claude-swap's
   *other* mechanism, rewriting the default login in place under Claude Code's
   own advisory locks, is what most of its code exists to make safe, and roost
   does not take it on.
2. **Restart and resume, on a button.** A switch is a *new* terminal that
   resumes the same conversation on the next account. The old terminal is
   closed. A running Claude cannot change accounts in place under this model,
   and the design does not pretend otherwise.
3. **Log in once per account.** Adding an account creates an empty profile and
   opens a terminal on it; Claude Code's own `/login` fills it. roost never
   reads, copies, or refreshes a credential. The token lineage of each
   account lives in exactly one place, which is what removes the capture-back,
   drift, and lock problems claude-swap has to solve because it copies.
4. **Round robin first, quota later.** The button rotates in group order and
   calls no Anthropic endpoint. A later step may read the usage endpoint
   claude-swap uses; it is additive and out of scope here.

Two further decisions were made by the designer and confirmed:

- **Rotation state is per group, not per project.** A limit is a property of
  the account. When project A rotates off account 1, project B on the same
  group must not launch onto it either.
- **The project binding is not project config.** Which paid account runs on a
  checkout is the same class of decision as `relaunch`, which `config.rs`
  keeps global-only because a cloned repository must not get a vote. The
  binding lives in roost's state directory and is set from the UI.

## Profiles and groups

### On disk

```
<state>/claude/accounts/<name>/    a profile: the CLAUDE_CONFIG_DIR target, 0700
<state>/claude/groups.json         groups, their current pointer, project bindings
```

`<state>` is `wsstate::state_dir()`. Nothing about accounts lives in a
project, in `~/.config/roost/config.toml`, or in `~/.claude`.

**An account is a name**, matching the session-name regex
`^[A-Za-z0-9_-]{1,32}$` for the same reason session names do: it lands in a
path and on a command line. The name `default` is reserved and means
`~/.claude` with no `CLAUDE_CONFIG_DIR` set, so the login the user already has
is an account with no work and no directory. Everything else about an account
is read from its profile when asked and stored nowhere else:

- **Email for display** comes from `oauthAccount.emailAddress` in the
  profile's `.claude.json`. Three readings, kept apart and rendered
  differently: the file is absent → *not logged in yet*; the file is present
  but cannot be read or parsed → *unreadable*; the key is present → the
  address. The first two are never shown as each other, and neither is ever
  shown as an empty address.
- **Whether the profile exists** is the directory's existence, read with
  `symlink_metadata` and matched three ways as CLAUDE.md requires: `NotFound`
  is absent, any other error is *cannot tell*, `Ok` is present.

**A group** is a name (same regex), an ordered list of account names, and a
current index. **A binding** maps a project's existing percent-encoded storage
key to a group name. One file holds all of it:

```json
{
  "version": 1,
  "groups": {
    "work": { "accounts": ["default", "second"], "current": 1 }
  },
  "bindings": {
    "roost": "work",
    "karpie%2Fsrc": "work"
  }
}
```

Written whole, temp file with a pid-unique name then `rename`, mode 0600, the
discipline `registry.rs` and `claudesess` already set. Read on every use,
never cached, like `config.rs`. A file that cannot be parsed is *unknown*: the
UI says so, every launch behaves as unbound, and nothing rewrites it, for the
reason `claudehooks` refuses to rewrite a settings file it could not read.

A group may name an account whose profile is absent (removed by hand, or a
state directory restored on another host). Rotation skips absent members and
says it did; a group with no present member refuses the switch and says why.
A member whose presence *cannot be told* is not skipped: the switch refuses,
because guessing either way is the mistake the table in CLAUDE.md is about.

### What a profile contains

Creating a profile:

1. `create_dir` at 0700. The name has already passed the regex; the parent is
   roost's own state directory, so `safe_resolve_parent` is not the tool here,
   but the same rule holds: the browser supplies a name, never a path.
2. Seed `.claude.json` with exactly two keys: `hasCompletedOnboarding: true`,
   and `theme` copied from `~/.claude.json` when readable, `"dark"` otherwise.
   Claude Code shows first-run onboarding when either is missing; claude-swap
   found this and it costs nothing to reuse. Nothing else is seeded, and in
   particular no identity block: `/login` writes that.
3. Symlinks into `~/.claude`, one per entry, whether or not the target exists
   yet:

   | Link | Why shared |
   |---|---|
   | `settings.json`, `keybindings.json`, `CLAUDE.md`, `skills`, `commands`, `agents`, `plugins` | The person's setup, not the account's. claude-swap shares the first six; `plugins` is added because this user's workflow depends on them, and is a spike item below. |
   | `projects`, `history.jsonl` | **Load-bearing for this design.** A conversation started on one account must be resumable on the next, so the transcripts have to be one directory. claude-swap makes this opt-in; here it is the point. |
   | `ide` | **Load-bearing for roost.** A Claude with a profile scans `<profile>/ide/*.lock`, not `~/.claude/ide/`. Without this link roost's lock file is invisible to it and the terminal silently loses image paste, `@file`, proposals, and connected-Claude detection. claude-swap deliberately excludes this directory; roost needs it. |

   Never shared: `.credentials.json` and `.claude.json`. Those *are* the
   account.

Removing a profile deletes the directory, and only with positive evidence it
is unused: the process table, read the way `claudes::claude_terminals` reads
it, shows no `claude` whose environment carries `CLAUDE_CONFIG_DIR` naming
that profile. A process table that cannot be read means refuse, with that
message. The `default` account cannot be removed; it has no directory.

The `default` entry is also why sharing is symlinks *into* `~/.claude` rather
than a third neutral location: the default profile is the one that already
holds the history, and every other profile joins it.

## Launch

### The request

`session::LaunchRequest` gains `account: Option<String>`, the account name,
`None` meaning today's behaviour. `launch::keystrokes` gains the resolved
profile path and types, for a non-default account:

```
CLAUDE_CONFIG_DIR='<state>/claude/accounts/second' claude --resume <id>
```

The path goes through `shell_single_quote`, the helper that already quotes the
PR-loop prompt, and the module comment there explains why a constant is quoted
anyway. `default` and `None` type exactly what is typed today; the existing
keystroke tests must not change.

**The environment is on the command line, not in `session_env`.** The shell
stays account-neutral: a `claude` typed by hand later in that terminal uses the
default login, `session_env` and its tests are untouched, and a dtach session's
environment, fixed for its life, does not have to be. The profile path is
server-chosen from a validated name; the browser sends the name of a group or
an account and never a path, the rule `proto::Intent::NewTerminal` already
applies to the resume id.

### What ✻ does in a bound project

A ✻ click in a project bound to a group launches on the group's *current*
account. The id is minted as today. Two things are recorded that are not
today:

- **The minted id, against the terminal, at spawn.** Today a `Fresh` id lives
  only in the launch request; `claudesess` learns ids from hooks, which are
  per-project and opt-in, so a project with hooks off has no idea which
  conversation its Claude is in. The switch needs that id. So the spawn path
  writes a `claudesess` record with event `launch`, and a later hook record
  replaces it as hooks already do. Ending the session drops it, as it drops
  every other record about a terminal.
- **The account, beside the launch kind.** `relaunch::record` keeps the launch
  kind so a project opened after a reboot comes back with `claude` typed;
  it now keeps the account too, so it comes back on the same account. The
  relaunch still never resumes, for the reason `relaunch.rs` gives.

An unbound project behaves exactly as today. The ✻ menu in a bound project
shows which account the launch will use.

## The switch

A **Switch account** button appears on a Claude terminal tab in a bound
project whose group has more than one present member. Pressing it opens an
in-page confirmation naming both accounts: *Restart this Claude on `second`,
resuming the conversation? This terminal closes.* On confirmation the browser
sends:

```
Intent::SwitchAccount { session: String }
```

The hub then, in this order:

1. **Reads the terminal's recorded conversation id** from `claudesess`, before
   anything ends, because ending the session drops the record.
2. **Advances the group's current index**, wrapping, skipping absent members,
   and writes `groups.json`. Refuses, with the reason, if no other member is
   present or one cannot be told.
3. **Reserves a new terminal in the same pane** through the path
   `do_new_terminal` uses, with a launch request carrying the next account and
   `ClaudeSession::Resume(id)`, but only after `claudehist::has` confirms the
   transcript exists in the shared history. If no id was recorded, or the
   transcript is not there, the request carries a `Fresh` id instead and the
   event says so: the user asked for a Claude on the next account and gets
   one, and is told it is new. Never a silent fall to a bare launch.
4. **Ends the old session** through `do_end_session`, which already confirms
   the dtach master off the hub lock and already tolerates a session that has
   gone.

Reserving before ending is deliberate. The per-project cap of 16 sessions
makes the reservation the step that can fail; if it does, the old terminal is
still there and the switch is refused whole. The destructive step has the same
evidence as closing a tab: a click on that tab, confirmed.

The index advance is per group, so the next ✻ in *any* project bound to that
group also lands on the next account. That is the point of a group.

The new terminal's tab carries the account name. The old tab's scrollback is
gone; the conversation is in the transcript and comes back on resume, minus
the prompt cache, which Claude Code rebuilds on the first message.

## The UI

**Global settings dialog**, a new *Claude accounts* section on the
`openSettings` dialog in `static/dialog.js`:

- The account list: name, and the email, *not logged in*, or *unreadable*.
  `default` is always first and shows the email from `~/.claude.json` by the
  same three-way read. Each row is laid out with a usage column that this
  step leaves empty: #92 fills it with the 5-hour, 7-day and per-model
  windows, and the project header chip and ✻ menu rows below get the current
  account's tightest window the same way. The row, the chip and the menu are
  designed here so #92 adds numbers, not surfaces.
- **Add account**: a name field, validated client-side by the same regex and
  again by the server. On success the server creates the profile and opens a
  terminal in the current project with `CLAUDE_CONFIG_DIR` set and `claude`
  typed, so Claude Code's login prompt is the next thing the user sees. The
  prompt prints a URL and takes a code back, which works over a terminal.
- **Remove**: an in-page confirmation, then the evidence-gated delete above.
  A refusal shows the reason.
- **Groups**: name, ordered members chosen from the account list, add and
  remove. The current pointer is shown, not edited.

**The project header** in a bound project shows *group · current account*,
and a selector binds the project to a group or unbinds it. Unbound projects
show nothing new.

**A Claude terminal tab** in a bound project shows the account it was launched
on, and the *Switch account* button. The button is absent, not disabled, when
the group has one present member, and disabled with a title when the state
file is unknown.

All new intents follow the existing rule: names, never paths or command lines,
and every one is validated in `proto` decode and again at the point of use.

## What this does not do

- **Quota.** No usage numbers, no *best* strategy. The endpoint claude-swap
  polls is internal and unversioned; when it is added it is a read of an
  access token, never a refresh, and an expired token reads as *unknown until
  a terminal is opened on this account*.
- **Automatic switching.** The button first. Detecting the limit, whether from
  the endpoint or from the terminal, is a later step that this design leaves
  room for: everything the button does is one intent the server can also send
  itself.
- **User-scope MCP servers.** `~/.claude.json`'s top-level `mcpServers` is
  per-profile and is not mirrored. Documented as a limitation; claude-swap
  mirrors it with a marker-and-stash scheme that is not worth its weight
  here yet.
- **Importing logins**, from `~/.claude` or a claude-swap store. Decided
  against: one copy today is two lineages tomorrow.
- **Live hop.** Swapping a credentials file under a running Claude. Recorded
  as the third option in conversation; a spike, not a design.

## The spike before task one

Every claim in *What a profile contains* about how Claude Code behaves with a
profile of symlinks is claude-swap's experience or a reading of its code, not
something roost has measured. Before the first task, on this host, against the
installed `claude` 2.1.270:

1. Create a profile by hand as section *What a profile contains* describes.
   Run `claude` with `CLAUDE_CONFIG_DIR` set to it. Confirm `/login` completes
   and writes `.credentials.json` into the profile, and that no onboarding
   screen appears.
2. With a roost running for a project, confirm the Claude connects on the IDE
   socket through the symlinked `ide` directory, and that `ide.mjs`-class
   behaviour (a proposal, an `@file`) works.
3. Send one message and confirm the transcript lands in `~/.claude/projects`
   through the symlinked `projects`, and that `claudehist` lists it.
4. Confirm `plugins` load through the symlink, or record that they do not and
   drop `plugins` from the shared list.
5. Confirm that Claude Code's writers replace the *target* of `settings.json`
   and `history.jsonl`, not the symlink. claude-swap's module comment says the
   settings writer detects symlinks and writes through; measure it.

A failure in 2 or 3 changes section *What a profile contains*, not the rest of
the design. A failure in 1 stops the work.

## Testing

**Rust, `cargo test --test-threads=1` as this repository requires.** Each new
test is checked by reverting the code it covers and watching it fail, and the
failure mode goes in the test's comment.

- `launch::keystrokes`: an account types the quoted prefix and the resume flag;
  a profile path containing a single quote is quoted correctly; `default` and
  `None` type today's bytes, asserted byte-for-byte against the existing
  expectations.
- Rotation: advancing wraps; an absent member is skipped and reported; a group
  of one present member refuses; a member that cannot be told refuses with a
  distinct reason from absent. The fixture for the last uses a directory with
  its permissions removed, so the check reaches the *cannot tell* branch and
  does not pass on `ENOENT`, the trap CLAUDE.md names.
- Profile creation: every symlink from the table exists and points into
  `~/.claude`; the seeded config holds exactly two keys; the directory is 0700.
- Removal: refuses when a fake `/proc` shows a `claude` whose environment
  names the profile; refuses when the process table is unreadable; deletes
  otherwise. The first two assert on the reason, not `is_err()`.
- `groups.json`: an unparsable file reads as unknown and is not rewritten,
  asserted by comparing bytes before and after.
- Hub: `SwitchAccount` reserves the new terminal with `Resume(id)` and the
  next account, then ends the old session; falls to `Fresh` and emits the
  event when the transcript is absent; refuses whole when the session cap is
  reached and leaves the old session's reservation in place; is refused in an
  unbound project. Recording the minted id at spawn is asserted by reading
  the record back, not by survival, the vacuity `claudesess`'s own tests
  document.

**Browser, `tests/browser/accounts.mjs`**, outside `cargo test` like the rest,
against a real roost and real dtach with a fake `claude` that is a real binary
printing its environment and arguments, the shape `claudeterm.mjs` and the
tab-marking work already use:

1. Add an account through the settings dialog; assert the profile directory
   and its symlinks exist.
2. Bind the project to a group of `default` and the new account; press ✻;
   assert the terminal shows `CLAUDE_CONFIG_DIR` unset, because `default` is
   current.
3. Press *Switch account*, confirm; assert a second terminal appears in the
   same pane whose output shows the profile path and `--resume` with the same
   id the first terminal was started with, and that the first tab is gone.
4. Press ✻ again; assert the launch is on the second account without a
   switch, because the pointer is per group.

The four traps in `tests/browser/README.md` apply, in particular typing before
the prompt: the fake `claude` prints on start, so the assertion waits for its
output rather than for the keystrokes.

## Risks

- **Claude Code's private layout.** `CLAUDE_CONFIG_DIR`, the profile file
  names, and the identity block are read from claude-swap's `paths.py`, which
  cites Claude Code's source, and confirmed against this host's real files.
  They can change in any release. The failure direction is chosen: a profile
  Claude Code does not understand shows a login prompt, and a missing identity
  key shows *not logged in*. Nothing is destroyed by a layout change.
- **Symlinked directories.** The spike exists because this is where the design
  rests on another program's behaviour. The `ide` link is the one roost cannot
  do without.
- **An exported API key in the user's `.profile`.** `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, and `CLAUDE_CODE_OAUTH_TOKEN` override the profile's
  login inside Claude Code. claude-swap scrubs them; roost types into a login
  shell and cannot. The account list shows what `/login` recorded, and a
  Claude silently using a key instead is a documented caveat, not something
  roost can detect from outside.
- **Two roosts, one state directory.** `ideport.rs` already documents that two
  roosts sharing `ROOST_STATE_DIR` alternate on the port file; two roosts
  advancing the same group pointer would leapfrog the same way. Same
  standing: unsupported, and not made worse.
- **The container image.** `~/.claude` and the state directory must both be
  on the volume, or the profiles and the history they link to part company on
  the next image pull. `docs/deploy.md` gets a line.

## Decided in conversation (2026-09-14)

1. Session mode over global switch.
2. Restart and resume, on a button; automation later.
3. Log in once per account; no import.
4. Round robin first; quota later.
5. Closing the old terminal and opening a new one, over killing the Claude in
   place or typing `/exit`.
6. The account on the typed command line, not in the shell's environment.
7. Rotation state per group; the binding in state, not project config.

## Handoff

The implementation plan follows the writing-plans process and is task-by-task
with a review between tasks. Suggested order: the spike; the state module
(profiles, groups, the file); `keystrokes` and the launch request; the
minted-id record at spawn; the hub switch; the settings dialog and header;
the browser test; `docs/deploy.md`. Quota and automatic switching are separate
specs when their turn comes.
