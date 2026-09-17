# Asking what the other agents are doing

*2026-09-17. Status: designed, not implemented. Issue
[#118](https://github.com/PeterKnego/roost/issues/118). From comparing roost
against [herdr](https://herdr.dev), whose agents can "wait until another agent
is genuinely blocked" — the one capability roost had no answer to.*

## The whole feature is already on disk

`claudehooks::EVENTS` is three events, and `claudesess::Recorded` stores which
one it last saw, per terminal, at
`$ROOST_STATE_DIR/claude/<key>/<session>.json`:

| recorded event | state |
|---|---|
| `SessionStart` | started, no turn finished — **working** |
| `Notification` | wants permission or input — **blocked** |
| `Stop` | a turn completed — **idle** |

So this is not a new mechanism. It is a rendering of one that has been running
since #18 step 1, and the work is in the reporting, not the sensing.

## Surface: a subcommand, and nothing else

```
roost sessions [--project KEY] [--json]
roost wait <session> [--for idle|blocked] [--timeout SECONDS]
```

Beside `roost notify` and `roost claude-hook`. A process in a roost terminal
already has identity — `session_env` exports `ROOST_PROJECT`/`ROOST_SESSION`,
and `claudesess::terminal_from_env` already reads them back, so `--project` is
a fallback rather than the normal path.

**No HTTP endpoint** (CLAUDE.md caps that at two POSTs), **no socket**, **no
authentication surface**. It reads files a local process can already read.
Unlike #117, nothing here is remote.

## The one thing that makes this safe or dangerous

**`unknown` is a real answer, and `wait --for idle` must never accept it.**

Hooks are per-project and opt-in via the bell. A Claude typed by hand into a
plain terminal records nothing, and `claudesess::recorded` returns `Option`
exactly so absence means *unknown* — its own doc: *"No recorded session must
never be presented as there was no session."*

An agent told "idle" because roost could not look would start editing on top of
another agent's half-finished work. So:

- `sessions` prints `unknown` as its own state, never folded into `idle`
- **`wait` never succeeds on `unknown`.** It keeps waiting and times out, and
  the timeout message says which of the two happened: *"never reached idle"*
  versus *"roost has no record of that terminal — hooks are not enabled for
  this project, so it cannot tell"*. The second is a different problem with a
  different fix, and a shared message would send the user looking in the wrong
  place.

This is CLAUDE.md's rule applied to a new consumer: *"I could not determine X"
is a third outcome, never folded into "X is false".*

The same rule applies one level up. `session::socket_names_checked` returns
`Option<Vec<String>>` — `None` is *could not read the socket directory*.
`sessions` returns an error there rather than an empty list, because "no
terminals" and "I could not look" would otherwise print identically, and an
agent scripting against the empty list would conclude it is alone.

## What a state is derived from, and what it is not

`live` comes from the socket directory — the same positive evidence
`registry` uses. A terminal with a socket and no record is `live` with state
`unknown`, which is the honest rendering of "a shell is there, and roost has
no hook telling it what is in it".

Deliberately **not** read: the pane contents. herdr reads every pane to
classify it; roost does not need to, because the agent tells it through a hook
that already fires — and reading scrollback to guess at state would be
inventing evidence where a fact already exists.

## Exit codes

`roost notify`'s convention, which exists so a misconfigured hook is loud:

- `0` — the wait was satisfied, or `sessions` printed
- `1` — timed out, or state could not be read
- `2` — usage error

A wait that times out is a **failure**, not a quiet `0`. A script that reads
`roost wait other --for idle && do_the_thing` has to stop when the wait did not
happen.

## Testing

- **Each of the three events maps to its state**, asserted separately. A table
  test that only checks `Stop → idle` passes against a function returning
  `idle` for everything.
- **`unknown` is asserted twice**: that it is reported, and that `wait` refuses
  to be satisfied by it. The second is the one that matters and the one a
  shape-only test would miss.
- **The two timeout messages are asserted by their text**, because they send
  the reader to two different places.
- **An unreadable socket directory is an error, not an empty list** — asserted
  on the error, and skipped rather than inverted when running as root, which
  can read a 0000 directory.
- **Revert-check each**, per CLAUDE.md, and record the observed failure.

## Not in this step

**Spawning.** "Agents start each other" needs the CLI to reach the running
server, which means a local socket — a real new surface that deserves its own
issue rather than arriving beside a read-only command.
