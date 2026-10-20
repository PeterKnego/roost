# Ownership changes only on evidence about that terminal (#183)

Follow-up to #158 slice 1 (PR #181), #182 (PR #187) and #188 (PR #191).

## The problem
Rename-following decides who owns a terminal from evidence about a **key as a whole**. When a new directory appears at a renamed project's old name (an *occupant*), that evidence describes the wrong directory. Issue #183 lists four chains:

- **(a)** The occupant's residents are re-pointed to B, the first rename's target. `follow_gone_owner_inner` (claudes.rs) takes `record_move(owner)`'s key-level `Followable(B)` and rewrites every resident of A. `home::follow_move_in` part (b) does the same for every record owned by A.
- **(b)** Occupant plus a second rename of B, to C. A record written by rename-following has no `.<name>.ino`, so R18 cannot follow it a second time. R18 forgets it to `Here`, and `Here` is now the occupant's A.
- **(c)** `.occupant-ino` holds one identity. A third directory at A overwrites the second occupant's identity, and that occupant's renamed sessions lose their evidence.
- **(d)** Non-destructive: an occupant renamed while B exists leaves its own sessions at A, unlisted.

(a)–(c) end with a record that names the wrong owner, and Close Project on that owner would end a session working elsewhere: ownership taken from stale evidence and fed into a destructive path.

## Decisions (2026-10-21, with the user)
1. Fix all four chains, including (d).
2. **Per-terminal identity.**
   - Every re-home record carries the identity of the directory it points at.
   - A record's owner changes only when that identity is found.
   - A key's own sessions follow their own shells.
   - A record whose home now holds a different directory is kept, never forgotten into it.
   - Occupant identities become a list.

   The alternatives were per-key directory generations, and refusing occupants. Both were declined.

## §1. Every record carries the identity of the directory it points at
**The file.** `state/home/<home key>/.<name>.ino` holds the owner directory's identity as one line: `dev ino btime_secs btime_nsec`. This is the identity #191 established, because ext4 reuses a freed inode in 50 of 50 `rm` + `mkdir` cycles.

**Who writes it.** Every writer of a record writes the target directory's identity, from its own `metadata` (birth time via `Metadata::created()`):
- `hub::rehome_to` (already writes it; extended to birth time);
- `home::follow_move_in` parts (a) and (b);
- `claudes::rewrite_owner`.

If the identity cannot be read, the record is still written but its `.ino` is removed. That record can never be followed again: it can only be kept or forgotten (§2). Losing a follow is recoverable. Following on a guess is not.

**Legacy files.** A two-field `.ino` from #181 is upgraded on the next R18 pass in which the owner resolves and its `(dev, ino)` matches: birth time is added. A legacy identity that cannot be upgraded counts as no identity.

**The rule.** A record may change owner from X to Y only when all of these hold:
- Y's directory carries the record's own identity. Candidates are tried in order:
  1. the `.moved-to` target of X's key, if X's key directory exists;
  2. the entries beside X's last known path (the parent of X's key `.origin`, or the parent of `<root>/<X>`). This is the same scan as #182's `sibling_evidence`, with the record's identity;
  3. the ancestors of the resident Claude's cwd (as today).
- Y resolves under the roots.
- Y is `owner_shaped` for the record's home.

**Removed.**
- The `record_move(owner)` arm at the top of `follow_gone_owner_inner`, which re-pointed every resident of a key on key-level evidence.
- `follow_move_in` part (b)'s re-point of every record owned by the old URL. Part (b) now applies the rule above to each record.

`follow_move_in` part (a), for the key's own sessions, is replaced by §3.

**Errors.** A candidate's `symlink_metadata` that is `NotFound` means "try the next". Any other error suspends the record for this pass (logged once). An unreadable `.ino` also suspends it.

## §2. Forgetting versus keeping
R18 (`claudes::sweep_gone_owners`) forgets a record when git no longer lists its owner and §1 found no move. Forgetting means `Here`, so the terminal belongs to its home URL again. The home's state decides:

| Home state | R18 action |
|---|---|
| Resolves to a directory carrying the home key's recorded `.origin-ino` identity | **Forget** (as today) |
| Does not resolve | **Forget** (as today). `Here` names nothing a page or Close Project can act on. |
| Resolves to a directory whose identity differs (an occupant) | **Keep** the record |
| `.origin-ino` unreadable, or the stat errors | **Keep** the record |

The first row keeps the existing case working: Claude Code removed a worktree, so the terminal comes home. A kept record is logged once per record, like R18's existing suspensions. It keeps the terminal out of the occupant's Close Project, because `kill_project` spares records owned elsewhere.

A record whose identity is found nowhere is never rewritten to `Here` by `follow_move_in`. Reaping is unchanged: a session whose own directory is really deleted is still reaped by `registry::reconcile`'s existing evidence (#178, #182).

## §3. Occupant identities, and a key's own sessions
**Occupants (fixes c).**
- `.occupant-ino` becomes a list, one identity per line in the four-field format.
- The occupied arm of `confirmed_gone` appends the current occupant's identity when it is not already listed.
- At most `OCCUPANT_CAP = 8` entries. The oldest is dropped, and one log line names the cap.
- `registry::identities()` returns the origin identity plus every listed occupant.
- The list is removed with `.moved-to` once the key holds no live socket (unchanged).

**`.origin-ino`** gains birth time the same way: it is rewritten in four-field form on the next pass while the directory resolves. A legacy two-field identity may still protect a session from the reap (the keep direction). It never licenses a follow (the ownership direction), so a move found only through a legacy identity is `Unfollowable`.

**A key's own sessions (fixes d).** This replaces `follow_moves`' current narrowing: today fresh passes take every live session, and non-fresh passes take only sessions whose shell is inside the `.moved-to` target.

For each live native session of a key:
- If the session leader's cwd has an ancestor carrying one of the key's identities at a directory P, where P resolves under the roots and is not the key's current directory, that session follows P:
  - a record to P's URL, with P's identity as its `.ino`;
  - the R16 gate (a name already live in P is refused, as today).
- A session whose shell has left its directory gives no evidence. It stays where it is, and this is logged once.

So an occupant renamed to D while B exists takes its own sessions to D, and A's earlier-followed sessions stay with B.

## CLAUDE.md
One paragraph under the rename row of "Absence of evidence":
- A terminal's owner changes only on evidence about that terminal: the identity its record carries, found at the new place, or its own shell.
- A record is never forgotten into a home URL that now holds a different directory.
- Reconciling this back to key-level evidence is the vulnerability, not the cleanup.

## Out of scope
- Directories moved to a different parent.
- The remaining #190 and #185 items.

## Testing
All tests use real git and real dtach where liveness is asserted. Every test is revert-checked, and the observed failure is recorded in its comment.

**(a)**
- A main-checkout resident re-homed into the occupant is not re-pointed to B when the occupant is renamed to D. It follows D by its own identity.
- The same holds when that resident's Claude has `cd`'d out (the sibling scan finds D).

**(b)**
- With an occupant present, B renamed to C: the followed session's record follows to C.
- In the shape R18 reaches: a main checkout A, with a resident re-homed into its worktree, is renamed to B and followed. An occupant then stands at A's URL, and B is moved to a different parent, so the record's directory cannot be found. The record is **kept**, and `kill_project` on the new A does not end the session. In the worktree-shaped variant the home is unchanged, so §2's table says forget, and the record is correctly forgotten into its own home.
- R18 never reaches a record homed at a worktree key, because `owner_leaf` refuses an owner outside the home's own `.claude/worktrees/`. Such a record is protected by §1's removal of the key-level re-point, not by §2.

**(c)**
- Occupant 1 is followed to D.
- A third directory is created at A and removed.
- The D session still survives `reconcile` and is still owned by D.
- The occupant list holds both identities. The cap drops the oldest and logs.

**(d)**
- The occupant's own session follows D while B still exists.
- A session that has `cd`'d out stays and is logged.

**Regressions**
- A removed worktree's resident is forgotten home.
- A single rename still follows.
- #182's A→B, `cd` out, B→C still follows.
- `a_moved_to_does_not_outlive_its_sessions` still passes.

**Legacy**
- A two-field `.ino` / `.origin-ino` protects from the reap but never licenses a follow.
- It is upgraded on the next pass when the directory matches.

**Identity unreadable**
- The record is written without `.ino`.
- It is later kept or forgotten, never followed.
