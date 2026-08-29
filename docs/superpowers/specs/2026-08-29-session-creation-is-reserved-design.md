# A session is created only where it was asked for

Closing a project ends its shells. Three times on 2026-08-29 it also left one
running: a `bash` under a fresh `dtach`, parented to resh itself, started in
the same second as the close, in no saved layout and with no client attached.
The front page listed it because it was real. The workspace showed nothing
because the layout no longer mentioned it. Both views were correct; the shell
should not have existed.

Three fixes shipped that day. Each was a real defect, each is revert-checked,
and none of them was this:

| Commit | What it fixed | Why it wasn't enough |
|---|---|---|
| `fb0385e` | the close never cleared the terminal tabs, so a *reopen* restored a tab per dead session | tabs, not shells |
| `c981141` | one `kill_project` sweep cannot see a `dtach` still forking, so a racing spawn survived | sized against a 50ms fork; the real lag measured 722ms |
| `ef30c36` | `ProjectClosed`'s teardown left each terminal's reconnect armed, so a retry could respawn | closed one route in, not the class |

This spec is about the reason there is always another one.

## What was measured

The last survivor, from the deploy host. Times from `/proc/<pid>/stat` against
`btime`, and `stat -c %y` on the two files:

| Time | Event |
|---|---|
| `09:49:35.604` | `aeron.json` written with no terminal tabs — the close committing |
| `09:49:35.850` | `dtach` spawned, parent = the resh server (+246ms) |
| `09:49:36.326` | its socket file appears (+722ms) |

The running binary was confirmed byte-identical to a fresh build of master
(`8026c9b0…`, installed and `/proc/<MainPID>/exe` alike), so both sweeps and
the `is_closing` guard were live.

**And by the current model, that survivor should have died.** `closing` is set
before the thread starts, so `term.rs:102` should have refused the connect;
failing that, `attach` inserts into the session map before returning, so the
second sweep — which re-reads that map ~400ms after the first finishes —
should have found it. It did not. The interleaving that produced this is *not
established*, and this document does not guess at it.

That is the finding, not a gap in the write-up. A close is currently correct
only if a reader can hold the interleaving of a websocket thread, a session
map lock held across a PTY spawn, a background kill thread, two `ps`
snapshots, and a socket file that appears three quarters of a second late — and
can be sure of it. Three attempts say that reader does not exist.

## Three answers to "what terminals are there"

| | Source of truth | Written by |
|---|---|---|
| **layout** | `ws.panes[].tabs`, persisted per project | hub intents |
| **sessions** | in-memory `SESSIONS` ∪ socket files ∪ `ps` holders | `session::attach`, `kill_project`, `registry::reconcile` |
| **attachment** | live terminal websockets | browsers, at will |

Nothing reconciles them, and the load-bearing flaw is in how the second one
grows:

> **`session::attach` creates when absent, and its only production caller is a
> websocket connect** (`term.rs:149`).

So the set of sessions is extended by clients, asynchronously, through a path
that takes no hub lock and is interlocked with nothing. A close is a
snapshot-and-destroy over a set that can grow behind it — and the snapshot is
taken from two places (a map, a directory) that a spawn reaches at different
times, hundreds of milliseconds apart.

Every fix so far has plugged one route into that set. The design guarantees
more routes: any code path that opens a terminal websocket is a session
factory, whether it meant to be or not. `term.rs:102`'s own comment concedes
this, saying the guard is "not airtight, and cannot be from here".

## The design: creation requires a reservation

Make creation server-authoritative. A websocket connect may **join** a session;
it may **create** one only by consuming a reservation that an intent placed
under the hub lock.

`term.rs` may proceed when any of these holds, and otherwise refuses:

1. **The session is in the map** — an ordinary second browser, or another tab
   mirroring a running terminal.
2. **Its socket exists and a process holds it** — a session that outlived a
   resh restart. This is why the rule cannot be "reservation only": after a
   restart the map is empty, the dtach master is alive, and `dtach -A`
   *reattaches*. Removing this would break the whole reason dtach is used.
3. **A reservation exists for `project/name`**, and consuming it is what
   authorises the spawn.

Otherwise: no map entry, no live socket, no reservation → the connect is
refused and the client is told the session ended. It does not create one.

### The reservation already half exists

`session::set_launch` parks a `LaunchRequest` under exactly this key
(`{storage_key}/{name}`), `do_new_terminal` writes one on every allocation
("unconditionally, `None` included"), and `attach` `remove`s it at spawn time
— once, deliberately, so a failed spawn cannot be retried into a shell nobody
asked to be a claude shell. That is a single-use reservation with a consume
step, built for a neighbouring reason.

The change is to make it **required** rather than incidental, and to make its
presence — not the mere absence of a map entry — the thing that authorises a
spawn. `Option<LaunchRequest>` becomes a `Reservation { launch: Option<…>, … }`
so that "reserved with no launch" and "not reserved" stop being the same value.

### What a close becomes

1. Set the project `Closing`.
2. Clear every reservation for it. Nothing new can be authorised from here.
3. Clear the terminal tabs (already done, `fb0385e`).
4. Kill the known sessions.
5. Set `Closed`.

Step 2 is what makes step 4 a set that cannot grow. A connect arriving at any
point during or after has no reservation, and — once its session is killed —
no map entry and no live socket, so it is refused by construction rather than
by winning a race.

### Absence of evidence, pointed the other way

Rule 2 asks "does a process hold this socket", and `registry`'s snapshot
helpers already return `Option` precisely because the answer can be *unknown*
(`holders_snapshot`, `pids_holding_path`). CLAUDE.md's rule is that "I cannot
tell" must never collapse into "false" **when the consequence is destruction**.
Here the consequence of guessing runs the other way: treating unknown as
"nothing holds it" means falling through to a spawn, which is how an
unreachable orphan is born.

So for *creation* the burden of proof is on creating. Unknown → refuse, and
say so. The cost is a terminal that fails to reattach on a flaky `ps` and comes
back on a retry; the cost of the other choice is the bug this spec exists for.

## What this deletes

- `Hub::is_closing` and its `term.rs` guard — a timing window standing in for
  an invariant.
- `CLOSE_SETTLE` and the second `kill_project` sweep (`c981141`). Once the set
  cannot grow, sweeping twice has nothing to catch. Removing it also removes
  the ~400ms during which a close refuses a new terminal, which Sections B and
  C of `closeproject.mjs` currently wait out.

Both were the right call given what was known; neither survives the invariant.

## Testing

The three fixes so far were verified by watching them fail, and two of the
tests were wrong before they were right — one setup assertion was vacuous on
an empty map, and one scored "I could not look" as "not disarmed". The lesson
carries here: **this design is testable in a way the timing fixes were not**,
and that is a reason to prefer it.

- The invariant is a pure question about a connect (`term.rs`), so it is unit
  testable per branch: in-map, live socket, reservation, and the three
  negatives, without a browser and without a race.
- "A connect with no reservation does not create" is deterministic. The
  current property — "a connect does not win a race" — is not, which is why
  `closeproject.mjs`'s Section D asserts a rate (3 of 3 failing, 4 of 4
  passing) rather than a fact.
- The browser tests keep their role: proving that the legitimate paths still
  work — a new terminal, a mirrored tab, and above all a session reattaching
  across a resh restart, which is rule 2 and which no Rust test reaches.

Every negative test asserts on *why* it was refused, not merely that it was.

## Risks

- **Restart survival is the one that matters.** Rule 2 is the whole of it, and
  it is the path a unit test substitutes away (`RESH_CMD=cat` leaves no master
  to survive). It needs a real-dtach test that restarts the server, which
  `harness.mjs` already supports.
- **A reservation nobody claims** — a tab opened and closed before any browser
  attached — must expire, or `next_free_name` leaks names. `do_new_terminal`'s
  existing unconditional `set_launch` already has this shape; the spec should
  say who clears it (tab close, project close, and a bound).
- **`?focus=` from the overview** navigates into a workspace and mounts a tab.
  If that tab's session is gone, the user gets "session ended" instead of a
  silently fresh shell. That is the intended behaviour and a visible change.

## Open questions for review

1. Should rule 2 consult `reconcile`'s existing bounded-staleness view of
   sockets rather than taking its own snapshot per connect? A `ps` fork per
   terminal connect is affordable; per *reconnect storm* after a restart it may
   not be.
2. Does a reservation belong on the hub (per project, under its lock, cleared
   with the layout) rather than in `session`'s global map? The close needs to
   clear them transactionally with `closing`, which argues for the hub.
3. Is "refused" distinguishable enough at the client? app.js currently treats a
   clean close as "session ended" and an unclean one as a reconnect. A refusal
   must be unambiguously the former, or the retry loop re-enters the same door
   — which is the shape of `ef30c36`.
