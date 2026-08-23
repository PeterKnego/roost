# Peer sessions

Two Claude sessions started in one resh project edit the same files on the
same branch with no idea the other exists. `resh peers` tells a starting
session who else is already working there.

A worktree is its own project (see `src/worktree.rs`), so "same project" means
one directory and therefore one branch. Sessions in two worktrees of the same
repo are not peers and are not reported: git will not check one branch out
twice, so they cannot collide over a file.

## Wiring it

`resh peers` is meant to be run by a Claude Code `SessionStart` hook, in
`~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "resh peers" } ] }
    ]
  }
}
```

It reads the hook payload on stdin (for the session's own id and cwd, both
optional) and writes the warning to stdout as the JSON a `SessionStart` hook
returns. A session that starts alone prints nothing at all.

**Installing that hook is the whole on/off switch.** There is deliberately no
`peers` key in the config file. `ide` and `share_selection` are settings
because resh does those things on its own initiative and you need a way to
decline; `resh peers` does nothing unless something explicitly runs it. A
switch would also be indistinguishable from working: turned off it prints
nothing, which is byte-identical to "you are alone here".

Stdout is the delivery channel, the exact opposite of `resh notify` beside it.
`notify` writes to `/dev/tty` *because* Claude Code captures hook stdout, which
would swallow its escape sequence. Here that capture is the mechanism: a
`SessionStart` hook's stdout is parsed as JSON and its `additionalContext` is
what reaches the session. Same fact about the harness, opposite conclusion —
do not make the two consistent.

## It informs the arriving session only

The hook fires when a session starts, so a session learns who was already
there. The sessions already there learn nothing about it — this is a snapshot
taken at startup, not a subscription.

The warning closes that asymmetry by hand rather than by machinery: the name
it prints for each peer is a `SendMessage` address (`ListAgents` describes a
session's name as "the name other sessions use to message it", and it is the
same string the registry stores). An arriving session can announce itself to
the sessions it found.

That is offered as a capability, not an instruction. A session told to notify
its peers would message all of them unprompted, and a message wakes the
receiver mid-task — turning a warning meant to prevent disruption into a
source of it. Whether an interruption is worth it depends on what the peer is
doing, which only the reader knows.

One caveat: names are not guaranteed unique. Two sessions in one project have
been observed sharing a derived name, distinguished only by a short reference
that `ListAgents` appends and that the registry file does not carry. The pid
resh prints is unique, but `SendMessage` does not take one.

## Where the roster comes from

Claude Code already keeps one: a file per live session under
`~/.claude/sessions/<pid>.json`, carrying that session's `cwd`, and removed by
the session itself on exit. `resh peers` reads that rather than building a
second one, which is what makes it survive a resh restart, a resh crash, and
resh never having run. resh's own `ide.rs` connection map is in-memory and
connection-scoped: a restart empties it and every already-running Claude
vanishes from it with no way to rebuild.

What resh adds is the project — mapping a `cwd` to a project name — which is
the part of the question only resh can answer.

## Other worktrees of the same repository

A worktree is its own project, so two sessions in two worktrees are not peers
and are not reported in the loud section. They cannot collide over files: git
will not check one branch out twice.

They can still collide over everything around the working tree — `.git` itself,
and whatever build output the repository's tooling shares. So they get their
own quieter block:

```
Also in this repository, in other worktrees:
    - resh-f2 (idle) in /home/claude/projects/resh/.claude/worktrees/projstrip-live
    They cannot collide over your files, but they share .git and whatever build
    output this repository's tooling shares.
```

The advice deliberately names no build tool. Which directory a repository's
tooling shares is a property of the machine, not of resh — this host points
every cargo workspace at one target dir, which is recorded in CLAUDE.md where
it belongs rather than asserted by a binary that runs anywhere.

Membership is decided by `git rev-parse --git-common-dir`: every worktree of a
repository shares the main checkout's `.git`, so equal common dirs means one
repository. resh asks git rather than looking for a `.git` entry, for the
reason `worktree.rs` already gives — a cwd may be a subdirectory, and a
worktree can live anywhere, so a path convention answers confidently and
wrongly.

That costs a subprocess, so it is spent as late as possible: never until some
session is somewhere other than here, then once per distinct directory rather
than once per session. A session starting alone — the ordinary case on every
project — runs no git at all.

`git` failing to run, exiting non-zero, or printing nothing is *cannot tell*,
never "a different repository". The cost of that is a missing advisory line
rather than a false one, which is the right direction for it to fall.

## The roster tracks where a session is, not where it started

Verified 2026-08-23 by watching a session relocate. A session was started in
the main checkout and told to enter a worktree; its registry entry moved with
it, in step with the process's own cwd:

```
t= 5s  registry=/home/claude/projects/resh
       proc    =/home/claude/projects/resh
t=10s  registry=…/.claude/worktrees/cwd-probe
       proc    =…/.claude/worktrees/cwd-probe
```

So a session that relocates mid-life is attributed to where it now is, and
both its old and new neighbours get the right answer. This mattered because the
alternative — a `cwd` recorded once at startup — would have silently
misattributed exactly the sessions most likely to move.

A shell `cd` inside a session is a different thing and moves nothing: the
harness pins the working directory back, and the registry entry is not even
rewritten (`updatedAt` unchanged). Only a harness-level relocation counts.

## Roots

`resh peers` needs to know the project roots, and a hook inherits none of the
service unit's environment. So roots come from `RESH_ROOTS` when set, and
otherwise from `roots` in `~/.config/resh/config.toml`:

```toml
roots = [
  "/home/you/projects",
]
```

A leading `~/` expands. Keep this in step with `Environment=RESH_ROOTS` in the
unit file; the env var wins when both are set, so the unit stays authoritative
for the service itself.

`roots` is **global config only**, and the strictest case of that rule in resh.
`allowed_origins` and `max_upload_bytes` are global-only because a cloned repo
could otherwise widen a boundary drawn around it. `roots` does not widen a
boundary — it defines the space every path confinement is measured in. A
project file that could add a root would make itself the parent of directories
it has no business seeing.

## Three outcomes, not two

A registry entry names a pid, and a pid alone is not an identity once pids
wrap. Liveness compares the `procStart` the session recorded against
`/proc/<pid>/stat`, and there are three answers: running, positively gone
(no such process, or the pid now belongs to something else), and *cannot tell*
(`/proc` unreadable, or a record predating `procStart`).

The third is counted and reported — "N session record(s) could not be checked"
— never folded into "you are alone". Under-warning is the failure this command
exists to prevent, so an uncertainty that went silent would defeat it. See
CLAUDE.md's *Absence of evidence is not evidence of absence*; `src/idecwd.rs`
draws the same distinction for the same kernel question.

`resh peers` always exits 0. A session must never fail to start because resh
could not work out who else was there.
