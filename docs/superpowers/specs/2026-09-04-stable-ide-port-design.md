# A Claude that outlived roost should say so

Alt+K in a terminal that predates the running roost says `no Claude is
connected to this project`. A Claude is running in that terminal. roost knows
it is — `claudes::try_claude_terminals` finds it in `/proc` and the tab wears
its mark. The two facts are both true and the message is the only thing the
user sees.

Measured on the deploy host, 2026-09-04, with roost (pid 134227) up since
07:19:30:

```
$ ss -tanp | grep -E ':(46793|36639)\b'
LISTEN 0 128 127.0.0.1:36639 … users:(("roost",pid=134227,fd=21))
LISTEN 0 128 127.0.0.1:46793 … users:(("roost",pid=134227,fd=6))
```

Both IDE listeners, no connections in any state, while port 8444 carried nine.
All three running Claudes started **before** 07:19:30 — 05:20, Sep 2, and
Sep 3. Their websockets died with the previous roost process and none of them
redialled.

## What was measured

`CLAUDE_CODE_SSE_PORT` in each dtach master's environment, against the port
its project's listener is actually on:

| dtach master | created | baked port | live port | |
|---|---|---|---|---|
| `roost/term` | Sep 4 07:17 | 44701 | 46793 | ✗ |
| `roost/term1` | Sep 2 12:33 | 41011 | 46793 | ✗ |
| `ultima_cluster/term` | Sep 3 13:46 | 41779 | 36639 | ✗ |
| `ultima_cluster/term1` | Aug 30 19:49 | 45377 | 36639 | ✗ |
| `explore/term` | Aug 27 21:39 | 46225 | — | ✗ |
| `ste_skill/term` | Aug 29 07:09 | 38015 | — | ✗ |

Six for six. Three lines produce it:

- `ide.rs:106` — `TcpListener::bind(("127.0.0.1", 0))`. A fresh OS-assigned
  port on every start, and on every project *reopen* (`term.rs:132` rebuilds
  the listener after a `CloseProject`).
- `session.rs:393` — `CLAUDE_CODE_SSE_PORT` is written into the environment at
  spawn time, once.
- `dtach -A`. When the socket already exists the new process **attaches**; its
  environment is discarded. The environment the shell keeps is the one its
  master was born with.

## What the stale variable does *not* do

The obvious inference from that table — that the terminal is poisoned, and a
new `claude` started in it inherits a dead port — was measured and is **false**.

Four probes against the live host, each a real interactive `claude` under a
pty, counting sockets to port 46793 owned by the probe's own pid:

| | `CLAUDE_CODE_SSE_PORT` | cwd | connected |
|---|---|---|---|
| A | 46793 (correct) | `projects/roost` | **yes** |
| B | 41011 (stale, no lock file) | `projects/roost` | **yes** |
| C | 41011 (stale) | `/tmp` | no |
| D | 41011 (stale) + a second, dead lock file for the same workspace | `projects/roost` | **yes** |

C is the negative control, and it is the reason A, B and D mean anything: the
detector can return zero. An earlier version of this measurement reported B as
"no" and was wrong — the detector was `grep '46793.*ESTAB'`, and `ss` prints
the state column *first*, so the pattern could never match an established
connection. It is recorded here because that failure is invisible: the number
it produces is zero, which is exactly the number the hypothesis predicted.

The client's own code says why B works. From the 2.1.260 bundle, discovery
(`gBt`) scores each lock file:

```js
if (a.CLAUDE_CODE_IDE_SKIP_VALID_CHECK) R = true;
else if (v.port === r) R = true;                 // r = CLAUDE_CODE_SSE_PORT
else for (let B of v.workspaceFolders) { … cwd === B || cwd.startsWith(B + sep) … }
```

The SSE port is a **shortcut past** the path comparison, not a precondition
for it. When no lock file carries that port — which is the case after a clean
roost restart, since `idelock::Lock`'s drop removes the file — the shortcut
simply does not fire, and the cwd match finds the live listener.

So the population this spec is for is narrower than the table suggests:
**Claudes that were already running when roost restarted.** They lost a
websocket, and nothing in the client redials it. A Claude started afterwards is
fine.

## The primary fix: say what is actually true

roost has the evidence and does not use it. `claudes::try_claude_terminals`
(`src/claudes.rs:83`) already walks `/proc` and reads `environ` for
`ROOST_PROJECT` / `ROOST_SESSION`. Reading `CLAUDE_CODE_SSE_PORT` out of the
same bytes costs nothing — no extra walk, no extra syscall — and the cached
scan already refreshes every 3s (`claudes::POLL`).

`ide::notify_selected` (`ide.rs:449`) then has three answers where it has one:

| what roost knows | today | should say |
|---|---|---|
| no connection, no claude in `/proc` | "no Claude is connected to this project" | unchanged — it is true |
| no connection, a claude in this terminal whose `CLAUDE_CODE_SSE_PORT` ≠ the live port | same sentence | *Claude in `term1` predates this roost (port 41011, now 46793) and cannot reconnect on its own. Start a new terminal, or restart claude in that one.* |
| no connection, a claude in this terminal, port unreadable | same sentence | *Claude is running in `term1` but is not connected to roost.* |

Which is CLAUDE.md's own rule applied to a message instead of a kill: "I could
not reach it" is not "it is not there", and the third row is not the second.

The advice in row two is measurement B: restarting `claude` in the same
terminal **does** work, because the stale variable no longer matches anything
and the path fallback takes over. A new terminal also works. Both are worth
naming; neither was discoverable from the message that ships today.

## The secondary fix: a stable port removes a mis-routing hazard

This one is not about function — B proves the integration works without it —
but about which project a Claude lands in.

`else if (v.port === r) R = true` is checked **before** any path comparison,
and when exactly one candidate carries that port the client returns it
exclusively. So if project A's surviving shell holds port 40000, and a later
roost start hands 40000 to project **B**, a `claude` in A's terminal matches
B's lock file by port, takes B's auth token, and connects to B's workspace with
no path comparison ever performed. Its mentions and diffs land in the wrong
project.

Today that lottery is run once per project per restart, forever. Making the
port stable removes the draw:

- **Where:** `<state>/ide/<storage_key>.port`, one small file per project. Not
  the workspace JSON — that is loaded and written under the hub lock, while
  `ide::start_in` deliberately runs outside it, and coupling them reintroduces
  the lock-across-I/O question CLAUDE.md forbids. Not one shared map file
  either: two roosts sharing a `ROOST_STATE_DIR` is a supported configuration
  (`registry::write_origin`'s doc comment is built around it) and a shared file
  is a read-modify-write race between them.
- **How:** written atomically — temp file with a pid-unique name, then
  `rename` — which is `write_origin`'s existing pattern, imported rather than
  reinvented.
- **Rule:** no record → bind 0, record what was bound. Record present → try
  `bind(("127.0.0.1", recorded))`; on any failure fall back to 0 and record the
  new one. Never fail a project over it; `for_project_in` already treats the
  listener as a convenience.

This is not per-project *configuration*. It is state roost writes about itself,
so it gets no config key and no settings-pane scope decision.

## How other IDEs avoid this, and why roost cannot copy them

The VS Code extension (`anthropic.claude-code`) is the same protocol, other
side: it runs the websocket server and writes the same lock file. roost's
`ide.rs` implements that server role. Two things differ, both visible in the
client bundle.

**The client applies extra checks — but only inside an IDE terminal.** Where
`E = notWSL && GI()` and `GI()` is "running in a VS Code or JetBrains
terminal":

```js
if (E) { if (!(r !== null && v.port === r)) {
  if (!v.pid || !yQt(v.pid)) continue;              // process.kill(pid, 0)
  if (process.ppid !== v.pid) { if (!(await y()).has(v.pid)) continue }  // ≤10 ancestors
}}
```

So inside VS Code a candidate must be **alive** and must be **this process's
ancestor**. That is what makes two windows on the same folder unambiguous:
claude attaches to the window it is running in, decided by process ancestry
rather than by path. roost is not a "supported terminal", so `E` is false and
roost gets neither check — path matching alone, with no liveness test on the
lock file's pid.

**The structural difference is lifetime.** A VS Code integrated terminal is
created by the window and dies with it, so `CLAUDE_CODE_SSE_PORT` can never
outlive the server that set it — the failure this spec is about is unreachable
there by construction. roost's whole value proposition is the opposite: dtach
sessions outlive the server deliberately, which is why the variable goes stale
and why roost, alone, has to handle it.

Ancestry is not available to roost either. A Claude in a dtach session has
`systemd --user` in its ancestry, not roost.

## What this is not

- **Not a switch to path-based discovery.** roost already gets path matching as
  the fallback (measurement B). `CLAUDE_CODE_SSE_PORT` stays because it is
  right for the fresh-terminal case and sidesteps worktree and symlink
  canonicalisation, which is what `session.rs:386` says it is for.
- **Not a reconnect mechanism.** roost cannot make a running `claude` redial.
  The primary fix exists because of that, not in spite of it.
- **Not a change to the token or the Origin rules.** `ide.rs`'s inverted
  handshake (CVE-2025-52882) is untouched; a stable port changes what number is
  bound, nothing about who may connect to it.

## Testing

- **The probe matrix above is the test design**, and it already has a proven
  negative control (C). Any automated version must keep one: the first run of
  this measurement produced a confident, wrong "no" from a detector that could
  only ever print zero.
- **The port record round-trips**, and a recorded port that cannot be bound
  falls back to an ephemeral one and *rewrites* the record. Unit-testable
  against a temp state dir; `idelock::set_ide_dir_for_test` already establishes
  the pattern for keeping test writes out of the real `~/.claude/ide`.
- **Restart survival needs the real-dtach harness.** `ROOST_CMD=cat` leaves no
  master to survive a restart — CLAUDE.md's substitution table verbatim.
- **The message is a pure function** of (connections, `/proc` scan, live port)
  and should be tested as one — including the row that must *not* change, so a
  fix to one case cannot silently rewrite the honest case into a guess. Assert
  on the rendered sentence, not an intermediate: CLAUDE.md records a
  message-formatting bug that every test in its module was structurally unable
  to see.

## Risks

- **A recorded port squatted by something else.** Handled by the fallback, but
  stability is best-effort and the spec must not promise more. The failure is
  today's behaviour, which is the right floor.
- **Two roosts sharing a state dir** race for the same recorded port. One wins,
  one falls back and rewrites the record, after which they alternate on every
  restart. Acceptable; an argument for the record being advisory.
- **A stale port becomes *more* likely to be live**, which is the point — and
  is only safe because it keeps working for the *same project*. The key is
  `storage_key`, the same percent-encoded key the rest of the state dir uses,
  and getting it wrong is the failure to test for.

## Open questions for review

1. **Does roost leave lock files behind when it is killed?** `idelock::Lock`'s
   drop removes the file, so a clean exit is fine; a `SIGKILL` or an OOM is
   not, and the module deliberately never sweeps the shared directory. The
   client applies no liveness check on roost's lock files (`E` is false), so a
   stale one is a candidate forever. Measurement D says an extra dead lock did
   not prevent a connection, so this is not urgent — but D tested one stale
   lock, not the general case, and the selection rule that made D pass was not
   established.
2. **Should the banner offer the action** rather than describe it? The ✻ path
   already exists; an error banner that spawns shells is a new kind of thing.
3. **Does the overview want this signal?** It already renders ✻ per terminal
   from the same `/proc` scan. "Running but disconnected" is arguably a
   different glyph, and the overview is where a user would see six at once.
4. **Bound on the port record.** A project opened once and never again leaves a
   file forever. `reconcile` already sweeps the state dir on a throttle; does
   this ride along, or is one small file per project simply fine?
