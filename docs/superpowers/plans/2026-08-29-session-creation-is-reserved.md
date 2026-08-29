# Reserved Session Creation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A terminal websocket can *join* a session but can no longer *create* one as a side effect. Creation is authorised under the same lock that serialises it, so a close cannot be raced.

**Spec:** `docs/superpowers/specs/2026-08-29-session-creation-is-reserved-design.md`

**Tech Stack:** Rust (std), plain JS, Deno + headless Chromium.

## The finding that shapes this plan

The spec recorded the last survivor's mechanism as "not established". One half
of it now is, and it decides the whole design:

| | Lock taken |
|---|---|
| `Hub::is_closing` — the guard (`term.rs:102`) | the **hub** mutex |
| `session::attach` — the spawn it guards (`session.rs:251`, held through `map.insert` at `:316`) | the **sessions** mutex |
| `session::kill_project` — the close's sweep | the **sessions** mutex |

The guard and the spawn are on **different mutexes**, so there is no ordering
between them whatsoever. A connect can pass `is_closing` and then spawn at any
later point — the gap is not a "microsecond-scale window" as `term.rs:90`
claims, it is however long the code between the check and `attach` takes
(which includes starting the project's IDE listener). No settle, sweep count,
or guard placement fixes a check that is on the wrong lock.

The close and the spawn already serialise on the sessions mutex. **So the
authorisation must live there too.** Then the two possible orders are both
correct, by construction rather than by timing:

- close first → reservations cleared, project marked closing, known sessions
  collected; a later `attach` finds no authorisation and refuses.
- attach first → it spawns and inserts before releasing; the close's scan,
  which needs the same lock, therefore sees it and kills it.

**Still not established:** why the *second* sweep (`c981141`) also missed the
survivor — by the above it should have found it in the map. Task 7 reproduces
the original under instrumentation before this is declared fixed. Do not skip
it on the grounds that the new invariant "should" cover it.

## Global constraints

- `cargo test`, never `--release`. Run with `--test-threads=1` (a bare `cargo test` hangs here).
- Build from this one checkout — shared target dir, see CLAUDE.md.
- Every new test revert-checked: apply the broken version, run it, read the failure, restore, record it in the test's comment. Two tests in `closeproject.mjs` were wrong before they were right (a vacuous `[].every()` setup; an `[]` read after navigation scored as "not disarmed") — assume the same of anything written here until watched failing.
- Stage explicit paths on commit, never `git add -A`.
- Never hold a lock across blocking I/O. `attach` already holds the sessions lock across the PTY spawn; this plan must not widen that, and must not add a `ps` fork under it.
- Three-valued honesty, inverted for creation: for the live-socket check, "cannot tell" must resolve to **refuse**, because falling through means spawning an unreachable orphan. Say why in the refusal.
- No behaviour change for: a second browser mirroring a running terminal, a session reattaching across a resh restart, `?focus=` from the overview onto a live session.

## File map

| File | Change |
|---|---|
| `src/session.rs` | `Reservation`; move `PENDING_LAUNCH` under the `SESSIONS` mutex; `reserve`, `clear_project`, `begin_close`/`end_close`; `attach` gains the authorisation decision and a typed refusal |
| `src/term.rs` | consume the typed refusal; delete the `Hub::is_closing` call |
| `src/hub.rs` | `do_new_terminal` reserves; `do_close_project` calls `begin_close`/`end_close` and drops the second sweep + `CLOSE_SETTLE`; `Hub::is_closing` deleted |
| `static/app.js` | a refusal must be unambiguously "session ended", never a reconnect |
| `tests/browser/closeproject.mjs` | Section D rewritten against the invariant; new restart-survival section |
| `tests/browser/README.md` | revert-check log |

---

### Task 1: One lock over sessions, reservations and close state

**Files:** `src/session.rs`

- [ ] Introduce `struct Reservation { launch: Option<LaunchRequest> }` so "reserved, no launch" and "not reserved" stop being the same value (`Option<LaunchRequest>` conflates them today, and `do_new_terminal` sets `None` unconditionally).
- [ ] Replace the standalone `PENDING_LAUNCH` static with a field on the same guarded structure as the session map, so one `sessions().lock()` covers map + reservations + closing set. Keep `set_launch`'s key format (`{storage_key}/{name}`) byte-identical — it is load-bearing for nested projects.
- [ ] Add `reserve(project, name, launch)`, `clear_project_reservations(project)`, `begin_close(project) -> Vec<String>` (marks closing, clears reservations, drains and returns the project's session names, all under one lock), `end_close(project)`.
- [ ] `begin_close` returns the names so `kill_project` no longer needs its own scan — one atomic read replaces a scan that could interleave.

**Tests (same file):** a reservation is single-use; `begin_close` clears reservations *and* marks closing in one lock acquisition; `end_close` lifts it; reservations are per project (a reservation in `a` is untouched by closing `b`).

**Revert-check:** make `begin_close` clear reservations *without* marking closing → the per-project test still passes, the ordering test in Task 5 fails. Record which.

---

### Task 2: `attach` decides, under that lock

**Files:** `src/session.rs`

- [ ] Add `pub enum AttachRefusal { Closing, NotReserved, Unverifiable }` and return it from `attach` as a typed error rather than a `String`, so callers can distinguish and tests can assert on *why*.
- [ ] Under the existing lock, before any spawn:
  1. project marked closing → `Err(Closing)`, unconditionally (this outranks every other rule; a session that exists is about to stop existing).
  2. key already in the map → join, exactly as today.
  3. a reservation exists → consume it and create.
  4. otherwise, the restart-survival case: the socket exists **and** a process holds it → reattach. `Ok(_)`/`Err(NotFound)`/`Err(_)` on `symlink_metadata`, and `Option` from the holder snapshot — unknown → `Err(Unverifiable)`, never a fall-through to create.
  5. otherwise → `Err(NotReserved)`.
- [ ] Rule 4's holder check must not fork under the lock. Take the snapshot before acquiring, or reuse `registry`'s existing bounded view; state which in the comment and why it is sound.

**Tests:** one per branch, each asserting the *refusal reason*, not just `is_err()`. Plus: a reservation is consumed by the first attach and a second attach with no session refuses `NotReserved` (this is the whole invariant in one test).

**Revert-check:** delete rule 5 so the fall-through creates again → the `NotReserved` test fails. Delete rule 1 → Task 5's ordering test fails.

---

### Task 3: `term.rs` refuses legibly, and stops asking the wrong lock

**Files:** `src/term.rs`

- [ ] Delete the `Hub::is_closing` call (`:102`) and its comment; the check now lives where the spawn is serialised. Note in its place *why* it moved — a guard on a different mutex from the operation it guards orders nothing.
- [ ] Map each `AttachRefusal` to a close with a distinct reason logged server-side, and ensure the client sees a **clean** close (see Task 4).
- [ ] Verify the Close frame is actually flushed before the stream drops. `ws_read.close(None)` only enqueues; if the socket closes first the browser sees 1006 and *reconnects*, which re-enters the same door and is the shape of `ef30c36`. Prove it with a test asserting the client-visible close code, not by reading the code.

**Revert-check:** drop the flush → the close-code test must fail (this is the one most likely to pass vacuously; if it does not fail, the test is wrong).

---

### Task 4: The client must not retry a refusal

**Files:** `static/app.js`

- [ ] `onclose` already treats clean as "session ended" and unclean as reconnect. Confirm a refusal lands on the clean path, and that a *refused* entry is not left armed (the `ef30c36` fix covers ProjectClosed; this is the refusal path).
- [ ] Show the reason in the pane rather than a bare "session ended" where one is available — a user whose terminal refuses to come back should be told the project was closed.

---

### Task 5: The close uses it

**Files:** `src/hub.rs`

- [ ] `do_new_terminal`: `reserve(...)` in place of `set_launch(...)`, still unconditional.
- [ ] `do_close_project`: `begin_close` under the sessions lock, kill the returned names, `end_close` at the end (in the same block that clears `closing` and broadcasts).
- [ ] Delete `CLOSE_SETTLE` and the second sweep — the invariant makes them dead weight, and leaving a timing hack next to a proof invites someone to trust the wrong one. Also removes the ~400ms during which a close refuses a new terminal, which `closeproject.mjs` Sections B/C currently wait out; drop that wait with it.
- [ ] Delete `Hub::is_closing`. Keep the hub's own `closing` field: it still refuses *intents* (`StartTerminal`, `NewTerminal`, a second `CloseProject`), which is a different job and correctly on the hub lock.

**Test (the ordering test, and the point of the whole plan):** with a project closing, an `attach` refuses `Closing`; and with an attach that wins the lock first, the close's returned name list contains it so it is killed. Both orders, asserted directly, no sleeps.

---

### Task 6: Rust suite

- [ ] `cargo test -- --test-threads=1` green, and *timed* — a deadlock hangs rather than fails, and Task 1 merges two locks' worth of state under one mutex, which is exactly the change that can introduce one. Compare wall time against the pre-change baseline (~50s) and investigate any large increase rather than re-running until it passes.
- [ ] Audit every existing `set_launch` / `attach` caller and test for the new contract; tests that spawn via `attach` directly (`registry.rs`, `hub.rs` tests) will need a reservation or rule 4.

---

### Task 7: Browser tests, including the one that started this

- [ ] `closeproject.mjs` Section D: rewrite from "a racing close leaves nothing behind" (asserted as a *rate*, 3-of-3 / 4-of-4) to the deterministic invariant — a connect with no reservation creates nothing. A test that has to be stated as a rate is a test of a race; this design is supposed to end that.
- [ ] New section: **restart survival**. Start a real dtach session, `resh.restart()` (harness supports it), reconnect, and require the *same* shell — same socket, same pid, scrollback intact. This is rule 4 and the highest-risk regression in the plan; no Rust test reaches it and `RESH_CMD=cat` cannot express it.
- [ ] New section: a second browser mirroring a running terminal still attaches (rule 2), and `?focus=` onto a live session still works.
- [ ] **Reproduce the original**, instrumented: three terminals, close, and record the sessions-lock ordering. If the survivor still appears, the plan has not fixed it and the second-sweep mystery is still live — stop and report rather than declaring the invariant sufficient.
- [ ] Run every other browser test that touches terminals: `reconnect.mjs`, `altscreen.mjs`, `claudeterm.mjs`, `claudetab.mjs`, `worktree-launch.mjs`, `overview.mjs`. Task 2 changes the conditions under which a terminal comes into existence; these are where that shows.

---

### Task 8: Ship

- [ ] `cargo fmt` is not a gate here (the tree carries ~1150 pre-existing diffs); match surrounding style instead.
- [ ] Commit with explicit paths, topic branch, `--no-ff` merge, push.
- [ ] Deploy per `docs/deploy.md` and confirm the **running** binary changed (`sha256sum ~/.local/bin/resh` vs `/proc/<MainPID>/exe`) — and that the served `static/app.js` contains the Task 4 change, since assets are compiled in and a stale one looks exactly like a successful deploy.
- [ ] Confirm the deploy did not take the live sessions with it (`KillMode=process`), by listing them before and after.
- [ ] Update the spec's "not established" paragraph with whatever Task 7 found — including "still not explained", if that is the truth.
