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
