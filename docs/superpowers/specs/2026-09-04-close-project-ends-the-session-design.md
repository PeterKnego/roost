# Close Project ends the session, not just the master

Close Project reports the sessions it ended. On 2026-09-04 that number was
measured to be an overstatement: the shell dies, and a child that handles
SIGHUP does not. Claude Code is exactly such a child. The survivor reparents
to `systemd --user`, keeps the project directory as its working directory,
and is invisible to roost forever after — there is no socket left to find it
by, and nothing in the UI that ever mentioned it.

This is the same defect class as CLAUDE.md's table, pointed at a new module.
The socket being unheld proves **the dtach master is gone**. It is read as
**the session is gone**.

## What was measured

`registry::kill_and_unlink_with` (`src/registry.rs:320`) kills exactly the
pids `pids_holding` returns (`:254`), which match processes whose *command
line contains the socket path* — the dtach master and its clients, and
nothing else. Everything downstream dies only via the SIGHUP the kernel sends
when the pty master closes.

`grep -rn 'pgid|killpg|setsid|getpgid' src/*.rs` returns one comment and no
code. roost has no process-group handling anywhere.

### Experiment 1 — a HUP handler survives the documented kill

A dtach session with three children, then `kill -9` on the master, which is
verbatim what `kill_and_unlink` does:

| | pid | pgid | sid | after `kill -9 194555` |
|---|---|---|---|---|
| `dtach -n exp.sock … ./inner.sh` | 194555 | 194555 | 194555 | gone |
| foreground child | 194556 | 194556 | 194556 | gone |
| `trap "" HUP; sleep 600` | **194558** | 194556 | 194556 | **alive**, reparented to `systemd --user` |
| plain background child | 194559 | 194556 | 194556 | gone |

Note the sid column. dtach `setsid`s the slave side: the master is its own
session, the shell is a *different* one. That split is why the master's death
reaches the shell only as a hangup, and why anything that declines the hangup
is simply never signalled.

### Experiment 2 — the process group is the wrong unit

The obvious repair — kill the shell's process group — was measured and is not
sufficient. With job control on (as in any interactive login shell), a
background job gets **its own process group** while staying in the shell's
session:

| | pid | pgid | sid |
|---|---|---|---|
| shell / foreground | 235613 | 235613 | 235613 |
| backgrounded HUP-ignorer | 235615 | **235615** | 235613 |

`kill -9 -235613` killed the foreground and left `235615` running. A sweep
over the **session** found it:

```
$ for d in /proc/[0-9]*; do … sid == 235613 … done
sid-match pid=235613            <- zombie, already reaped
sid-match pid=235615 bg_job_hup_ignorer 600
```

`kill -9 -<slave-side pgid>` reaping everything in experiment 1 was an
artefact of that fixture: a non-interactive script has job control off, so
its background children stayed in one group. The real shell does not.

**The unit is the session.**

### Experiment 3 — where Claude Code actually sits

From this host's live processes, sid read from `/proc/<pid>/stat`:

| process | ppid | sid | in its terminal's session? |
|---|---|---|---|
| `claude --session-id …` (in `ultima_cluster/term`) | 3176723 (its shell) | 3176723 | **yes** |
| `claude daemon run --origin transient` | 36527 | 22465 | no — its own |
| `claude bg-pty-host …` | 36527 | 1602042 | no — its own |
| `claude bg-spare …` | 1602042 | 1602059 | no — its own |

The interactive Claude — the one the user means — is in the session and is
reachable. Its daemon and background hosts have deliberately left it, and
`claude daemon run` is **shared across projects** (this one's `--spawned-by`
names an `ultima_cluster` worktree). Chasing them would end other projects'
Claudes.

That is the design boundary, and it is drawn by measurement rather than
policy: **anything that left the session left it on purpose, and roost does
not follow.**

## Why it reports success

`kill_and_unlink_with`'s confirmation loop polls `socket_has_process_with`,
which asks only whether anything still holds the **socket path**. A survivor's
command line does not contain it — `sleep 600` and `claude --resume` never
did. So `still_alive` goes false, the socket is unlinked, `end_socket`
returns `true`, `kill_project` increments `ended`, and `ProjectClosed` tells
the user a number that is too large.

The doc comment on that function is careful in one direction and silent in the
other. It never unlinks on a shrug — the whole `None`-snapshot branch exists
for that — but "nothing holds the socket" was allowed to stand in for
"nothing is left", and those are different claims.

## The design: end the session

`kill_and_unlink` gains a step, and its contract tightens from *the socket is
free* to *the session is gone*.

1. **Snapshot the socket holders**, as today.
2. **Before killing anything**, derive the target sessions. For each holder,
   read its children (`/proc/<pid>/task/<pid>/children`); each child that is a
   session leader (`sid == pid`) and whose sid differs from the holder's is a
   target. A dtach *client* has no such child and contributes nothing, so
   clients need no special case.
3. **Kill the holders**, as today.
4. **Sweep each target session**: every pid whose sid equals a target,
   `kill -9`, then re-read and repeat until empty or bounded out.
5. **Confirm** on the union — no holder, no session member — then unlink.

Step 2 must precede step 3. Once the master dies its children reparent to init
and the `children` link is gone; deriving the target afterwards is deriving it
from nothing.

### Guards, because this is a wider kill than today's

- **Never our own session.** Refuse any target equal to roost's own sid, and
  refuse sid 1 and 0 outright — the same shape as the existing `pid == 0`
  guard, and for the same reason: an impossibility that costs one comparison
  to make impossible.
- **Parse `/proc/<pid>/stat` after the last `)`.** Field 2 is `comm`, which
  may contain spaces and parentheses; `awk '{print $6}'` gets the wrong field
  for a process named `my prog`. The sid is the fourth field after the final
  `)`.
- **Re-derive at kill time, never from a stale snapshot.** A session id is a
  pid and pids recycle. The window between step 2 and step 4 is milliseconds
  and roost holds no lock across it, but the rule is cheap: the sweep re-reads
  `/proc` on every pass rather than filtering one frozen list.
- **Unknown is not empty.** An unreadable `/proc`, or a holder whose children
  cannot be read, means *this session could not be determined* — not *this
  session is empty*. It must fall into the existing not-confirmed path: leave
  the socket in place, report `false`, say why on stderr.

That last guard is the one CLAUDE.md's table is about, and here it points the
same way it usually does: failing to kill is recoverable, and the socket
staying put is what keeps the session discoverable by a later pass.

## What this is not

- **Not a process-tree walk.** Ancestry is broken by exactly the reparenting
  this bug is made of; by the time roost looks, the survivor's parent is
  `systemd --user`. The session id survives reparenting, which is the whole
  reason it is the right key.
- **Not `kill -9 -<pgid>`.** Experiment 2.
- **Not a cwd sweep.** "Kill everything whose cwd is under this project"
  would catch a Claude that `cd`'d elsewhere zero times and catch an unrelated
  editor of the same directory every time.
- **Not a change to detach.** Closing a *tab* still leaves the session
  running; that is deliberate and untouched. Only Close Project and an
  explicit End Session destroy.

## Testing

The discriminating test already exists as a measurement, which is unusual and
worth using: experiment 1 is the failing case, in three lines of fixture.

- **Real dtach, not `ROOST_CMD=cat`.** This is the dev/prod substitution trap
  in CLAUDE.md's table verbatim — with `cat` there is no master, no pty, no
  hangup and therefore no bug to observe. The test must spawn a real dtach
  session containing `trap "" HUP` and assert the child is gone afterwards.
- **Revert-and-watch is pre-verified.** With step 2/4 removed, the child
  survives; that has been observed on this host before the fix exists, so the
  test is known to discriminate rather than assumed to.
- **The session enumeration is unit-testable** against a fake `/proc` the way
  `claudes::try_claude_terminals` already is (`proc_root` is injectable), and
  the snapshot source is already injectable in `registry` (`SnapshotFn`).
  Cover: a holder with no children (a client), a comm containing a space and a
  `)`, an unreadable entry, and roost's own sid offered as a target.
- **Assert on the count Close Project reports**, not only on the processes.
  The `ended` number is half the defect.
- Every negative asserts on *why* — "could not determine the session" and
  "a process survived" are different outcomes and must read differently.

## Risks

- **This kills more than it used to.** A user who backgrounds a long job in a
  roost terminal and expects it to outlive Close Project loses it. That is
  the intent, and it is a visible behaviour change worth a line in the release
  note.
- **A wrongly derived session id kills a wrong tree.** The guards above are
  the mitigation; the sid-leader check (`sid == pid`, child of a confirmed
  holder) is narrow enough that a mis-derivation needs the master's own
  `children` file to be wrong.
- **Claude Code's background hosts still survive**, by design. The number
  Close Project reports must not start claiming otherwise.

## Open questions for review

1. **SIGTERM before SIGKILL?** Claude Code would get a chance to flush; a
   shell would get a chance to run its `EXIT` trap. The cost is latency in a
   path that already polls up to 500ms per session, and a second thing that
   can hang. Worth it, or is SIGKILL honest?
2. **Does `end_session` (one tab) get the same sweep?** Symmetry says yes.
   The counter-argument is blast radius: closing one tab is a far more casual
   gesture than Close Project, and today it destroys strictly less.
3. **Should `reconcile`'s project-gone branch use it too?** It shares
   `kill_and_unlink`, so it gets this for free — which means a deleted project
   directory would start reaping sessions more thoroughly on a background
   sweep, with no user gesture behind it. That may be right; it should be
   decided rather than inherited.
4. **What does the UI say when a session is not confirmed gone?** Today the
   count is silently short. With this change the count becomes truthful, which
   makes the shortfall meaningful and therefore worth showing.
