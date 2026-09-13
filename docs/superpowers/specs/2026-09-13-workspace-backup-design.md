# Backing up a workspace, conversations included

*2026-09-13. Status: designed, not implemented. Issue
[#18](https://github.com/PeterKnego/roost/issues/18), step 3. Steps 1 and 2 are
merged into develop (`src/claudesess.rs`, `src/claudehist.rs`, #68). This design
settles the three questions #18's own security section asks and nothing wider:
step 4, remote destinations, stays out.*

## What and why

#18 recorded a suggested order. Steps 1 and 2 shipped:

1. Record `session_id` and `transcript_path` per terminal — `claudesess.rs`.
2. Offer `claude --resume <id>` — the ✻ menu, `claudehist.rs`, `resume.mjs`.
3. **Local backup/restore of a workspace, transcripts included, to a file the
   user names.** ← this.
4. Remote destinations, only after 3 works.

Today a project's state is spread over six places under `$ROOST_STATE_DIR` and
one directory belonging to another program, and there is no single operation
that captures any of it. Reinstalling the machine loses the layout; losing the
machine loses every conversation. The maintainer's closing comment on #18 puts
the remaining work exactly:

> what remains is the storage question this issue raised and nothing else:
> where a conversation backup lives, what bounds it (those 410 MB are the
> reason it needs some), and whether it travels with the workspace file or
> beside it.

**It travels with it, in one file, and that file contains no paths.** The rest
of this document is why.

## Two facts measured on this host, both of which changed the design

Taken 2026-09-13 from `~/.claude/projects`, the same corpus `claudehist.rs`
was sized against.

**The corpus is 407 MB across 5429 transcripts in 24 project directories.**
The largest single transcript is 25.7 MB; the largest *project* directory is
173 MB over 106 transcripts. So an unbounded per-project backup is a 173 MB
file today and there is nothing stopping it being a gigabyte next year. Bounds
are not defensive tidiness here, they are the feature being possible at all.

**A conversation is not one file.** The directory that `claudehist.rs`
derives holds, alongside `<id>.jsonl`:

- `<id>/subagents/` and `<id>/tool-results/` — a sibling *directory* per
  conversation. For one 14 MB transcript in roost's own project directory the
  sibling is 3.7 MB, and roost's whole project directory is 19 MB.
- `memory/` — project-scoped, not per-conversation: `MEMORY.md` and one file
  per remembered fact. 20 KB for roost.

`claudehist::recent_in` already steps around both, with a comment saying
exactly why ("the same directory holds per-session *directories* named with
the same ids, and a directory is not a transcript"). It is right to, because it
is building a menu. A backup that stepped around them the same way and then
reported success would be quietly lossy, so this design has to say which of the
two it takes and which it leaves, and say so *in its own output* rather than
only here. See **What this deliberately does not capture**.

## Surface: a subcommand, not a route

`roost backup` and `roost restore`, beside `roost notify` and
`roost claude-hook` in `main.rs`.

Four reasons, and the first is a hard constraint rather than a preference.

**CLAUDE.md caps the HTTP surface at two POSTs.** "HTTP is GET-only apart from
`POST /upload` and `POST /paste` … Keep the surface at two." A restore takes a
body. Making it a route means a third POST, and that bullet's whole argument is
that the two existing ones are the entire CSRF surface precisely because there
are two of them to audit.

**#18 says "to a file the user names."** That is an argv path. There is no
file-picker in a browser that names a path on the server.

**A transcript is the highest-value file on the machine** — #18's words, and
the reason `claudesess::record` writes 0600. A route would put every prompt,
every file read and every command output through the tunnel on its way to a
browser's download directory. A subcommand writes it to a local file with no
network hop at all, which is what "local backup" should mean.

**Restore is the most destructive operation roost would have**, again #18's
words. A subcommand cannot be reached by a forged websocket intent or a
cross-origin form post. It requires someone already on the machine.

```
roost backup  <project> <file> [--conversations]
roost restore <file> [--project <name>] [--dry-run]
```

Both print a summary to stdout and exit non-zero on failure — the `roost
notify` contract, deliberately not the `roost claude-hook` one, which must
always exit 0 because it runs inside every Claude session.

## The archive format, and the one property that matters

A single file roost writes and reads. Header line, then a sequence of entries,
each a metadata line followed by that many raw bytes:

```
ROOSTBAK1\n
{"project":"roost","created":1757750000,"roost":"0.5.2","source":"/home/claude/projects/roost",…}\n
{"kind":"workspace","bytes":880}\n
<880 bytes>\n
{"kind":"session","id":"term","bytes":214}\n
<214 bytes>\n
{"kind":"transcript","id":"e1e28e7a-c9c4-4c5b-ad11-6bb188d36d68","at":1757600000,"bytes":14218775}\n
<14218775 bytes>\n
```

Hand-rolled, like this project's HTTP and its websocket framing and its HTML
sanitizer. The alternative was `tar` — a crate, or the binary roost already
shells out to for `git` and `dtach` — and it was rejected for the property
below, not for the dependency.

### The archive contains no paths

Every entry is a *typed record with an id*, never a filesystem location:

| kind | id | restored to |
|---|---|---|
| `workspace` | — | `wsstate::path_for(dest)` |
| `session` | session name | `claudesess` dir for `dest` + name |
| `cwd`, `launch` | session name | the corresponding dir for `dest` + name |
| `transcript` | Claude session id | `claudehist::transcript_dir(dest_dir)` + `<id>.jsonl` |
| `memory` | file name, single segment | `transcript_dir(dest_dir)/memory/<name>` |

Nothing in the file names where anything goes. Restore derives every
destination from the *destination* project, and every id is re-validated before
it becomes a path component: session names by `session::valid_name`
(`^[A-Za-z0-9_-]{1,32}$`), Claude session ids by
`claudesess::valid_session_id`, memory names by a rule that admits one path
segment and no dots. An id that fails is skipped and named; it is never
sanitised into something acceptable.

This buys two things at once, which is why it is the centre of the design.

**It answers #18's third difficulty.** "The transcript path encodes an absolute
cwd … Restore the same project at a different path on another server and Claude
will not find its own transcripts." An archive that stored
`/home/claude/projects/roost/…` would have to rewrite it. This one never
recorded it, so there is nothing to rewrite: the destination directory is
derived from wherever the project is *now*, by the same
`claudehist::transcript_dir_in` that the ✻ menu uses. Restore at a different
path is not a special case in the code; it is the only case.

**It removes tar's extraction hazard by construction.** There is no entry a
malicious archive could use to write outside the destination, because there is
no entry that names a destination. `--no-absolute-names`, `../` stripping and
symlink-entry handling are all absent because the class they defend against
cannot be expressed.

The `source` field in the header is recorded for the human reading a summary
and is never resolved, joined, or restored to. That should be stated in its own
doc comment, because the next reader's instinct will be to use it.

### Not compressed, on purpose

gzip -6 on this host's largest roost transcript: 14218775 → 4434461, a ratio of
3.2. Real, and still not worth a runtime dependency here. `flate2` is in
`Cargo.lock` only through `ureq`, a dev-dependency; promoting it puts it in
every shipped binary for a convenience the user can get with `gzip file.roostbak`
and which every backup destination worth the name already applies itself.

The stronger reason is legibility. This is an archive whose point is to be
readable in five years, possibly by something that is not roost. A header line
of JSON and length-prefixed blobs can be recovered by twenty lines of Python.
That is worth more than a third off the size.

## Bounds, and each one names itself when it fires

Constants in `backup.rs`, not config keys — the same call `search.rs` makes and
for the same reason.

| Constant | Value | Why that number |
|---|---|---|
| `MAX_TRANSCRIPTS` | 50 | Newest first. `claudehist::MAX_SCANNED` is 30 for a menu; a backup should reach further than a menu, and 50 covers 21 of this host's 24 project directories whole. |
| `MAX_TRANSCRIPT_BYTES` | 32 MB | The largest transcript measured is 25.7 MB. |
| `MAX_TOTAL_BYTES` | 512 MB | The entire 24-project corpus is 407 MB, so this cannot be hit by one honest project today, and it bounds the file a user is asked to find room for. |
| `MAX_MEMORY_BYTES` | 1 MB total | roost's is 20 KB. This is a text notebook, not a store. |

**Every one of them names itself in the summary when it fires**, with the
transcript it dropped:

```
backed up roost -> /tmp/roost.roostbak (18.2 MB)
  layout, 3 terminals, 2 conversations
  skipped 1 conversation: 3f1c… is 41.0 MB (MAX_TRANSCRIPT_BYTES is 32 MB)
```

This is the CLAUDE.md rule about `Results`/`Outcome` applied to a CLI: "A
search that skipped something says so … All three used to render as an empty
note, which is the same defect as the table below wearing a quieter coat: no
crash, no lost shell, and no way for the user to tell." A backup that silently
holds 49 of 50 conversations is worse than a search that does, because the user
finds out when they need it.

## The error model is not `claudehist`'s, and that is the point

`claudehist::recent_in` opens with `let Ok(entries) = read_dir(tdir) else {
return Vec::new() }`, and its module doc defends that at length: "Every failure
folds to 'no history' … This is the one place in this codebase where collapsing
'cannot look' into 'nothing there' is right, because the decision it feeds is
*offer a menu or don't* — no shell dies of it."

**That reasoning does not transfer, and reusing `recent()` here would be the
defect.** The decision this feeds is "write a file the user will rely on and
tell them it worked". An unreadable transcript directory would produce a
valid archive containing zero conversations, a cheerful summary, and a user who
finds out on the day they restore. That is the CLAUDE.md table, one row longer.

So the backup walk has its own three-way outcome, the shape `search::Results`
already uses:

- **captured** — read, bounded, written.
- **skipped** — a decision: over a cap, an id that failed validation, a
  directory where a transcript was expected. Named, with which rule fired.
- **unreadable** — *could not look*. Counted separately and, if the transcript
  directory itself is the thing that could not be read while `--conversations`
  was asked for, **the backup fails and writes nothing**, rather than
  succeeding with a hole in it.

That last clause is the one worth arguing about, so: a partial archive is
usable and a refused one is not. The counter-argument is that a user who cannot
read one subdirectory still wants the other 49 conversations. It is refused
anyway *only* when the top-level directory is unreadable — when nothing could
be enumerated at all, so "0 conversations" is indistinguishable from a project
that has none. A per-file `EACCES` inside a readable directory is a skip, named,
and the archive is still written.

## Restore: destruction requires positive evidence

#18 asks: "Does restore ever overwrite an existing transcript?"

**No. There is no flag that makes it.** A transcript at the destination is
append-only history that is not in the archive; replacing it destroys a
conversation to deliver an older copy of a different one. An existing
`<id>.jsonl` is skipped and named, and the summary says how many. The user who
genuinely wants the archived copy can move the live one aside themselves —
which is a deliberate speed bump, not an oversight.

The layout file is the one thing a restore is *for* replacing, so it is
replaced — but the existing one is **renamed aside**, not deleted:
`<key>.json.before-restore-<unixtime>`. A restore onto the wrong project is
then a rename away from being undone, and `wsstate::load` ignores the extra
file (it reads one exact path).

Three further refusals, all before anything is written:

- **A project with live sessions is refused.** `registry` knows which sessions
  have a socket; swapping the layout under a running shell is how a terminal
  tab loses track of the session it is attached to. Named in the refusal, with
  the sessions.
- **An unresolvable destination project is refused.** `resolve_project` against
  this machine's roots, not the archive's `source` field.
- **A malformed archive is refused whole**, before the first write. The header
  parses or nothing happens; an entry whose declared length runs past the end
  of the file fails the archive rather than truncating the entry.

`--dry-run` prints the identical summary and writes nothing. It exists because
this is the operation #18 flags as the most destructive roost would have, and
because the interesting failure — a re-derived transcript directory that is not
the one the user expected — is visible in a listing and invisible in a
success message.

## What this deliberately does not capture

Named here and, more importantly, named in `backup`'s own summary, so a restore
that comes back thinner than expected is explained rather than mysterious.

**The `<id>/` sibling directory** — `subagents/`, `tool-results/`. It is the
bulk (3.7 MB against a 14 MB transcript) and it is one more layer of another
program's private layout. Whether `claude --resume` degrades without it is
**not established**, and this design does not claim it does not; it claims only
that a backup holding the transcripts is worth having before that question is
answered. If the answer turns out to be "it needs them", the format takes it as
another `kind` with the same id and no change to anything else — which is the
argument for the format being typed records rather than a file list.

**Anything a project owns.** Source, `.env`, `.claude/settings.local.json`,
uploads. That is the checkout's business and git's, and roost's state directory
exists precisely so that roost's state never lands in `git status`. A backup
that swept the project directory would be a worse `tar` with a roost logo on it.

**Notifications, the error log, IDE ports, dtach sockets.** Facts about this
moment on this machine. A socket path restored onto another host names nothing;
`registry.rs` would then have to decide whether a socket that is not there is a
dead session or an unreadable one, which is the exact question the CLAUDE.md
table is eleven rows of.

## Security, against #18's three questions

**"Is the transcript included by default?"** No. `roost backup` without
`--conversations` writes layout only. #18: "it is a reason for it to be
explicit, opt-in per project, and never a default". Per *invocation* is
stronger than per project — there is no stored setting that can drift into
being forgotten, and the flag is in the shell history of the person who typed
it.

**"Can a project-scoped config key set a remote destination?"** There is no
config key and no remote destination. Step 4 is out of scope, so the question
this design has to answer is only that it must not *create* the shape — and it
does not: the destination is argv. When step 4 lands, `GLOBAL_ONLY_KEYS` is
where it goes, for the reason #18 gives, which is sharper than the one that put
`share_selection` there: "A cloned repo naming a backup host is strictly worse
than a cloned repo raising a disk ceiling."

**"Does restore ever overwrite an existing transcript?"** Answered above: no.

Two more that #18 does not ask.

**The archive is 0600 and created with it**, not chmodded after — the window
between `create` and `set_permissions` is exactly when a 407 MB file of every
credential the machine has seen is world-readable. `claudesess::record` writes
0600 already and is cited as precedent, but it writes to a temp file in a 0700
directory; this one is written wherever the user pointed it, which may be
`/tmp`.

**A backup is written to a temp name in the destination directory and renamed
into place.** Same discipline as everywhere else in this codebase, and here it
also means an interrupted backup never leaves something that looks like a
complete archive.

## Testing

The failure mode this repo names as dominant is "tests that pass for the wrong
reason", so, concretely:

- **The round trip must be tested through a *different* project name and path**,
  not the same one. A test that backs up and restores `proj` proves nothing
  about the re-derivation that is the entire point — it would pass with the
  absolute path stored and copied back verbatim. Back up from one temp root,
  restore to a second under a different name, and assert the transcript landed
  in the directory derived from the *second*.
- **The no-overwrite test must assert the existing file is unchanged**, by
  content, not merely that restore reported a skip. "Reported a skip" is also
  true of a restore that skipped and then wrote anyway through a second path.
- **The cap tests must assert which cap is named in the output.** A test that
  asserts a count of skips passes when the wrong cap fires, and the summary
  naming the right one is the whole user-facing contract.
- **The unreadable-directory test must distinguish refusal from an empty
  archive.** Both leave the user without conversations; only one of them says
  so. Assert the exit code and the message, not the absence of entries.
- **A malformed archive test per refusal**, asserting on the message. A single
  `is_err()` over a truncated file passes when the parser rejects everything.
- **Revert the fix and watch it fail** for the re-derivation test and the
  no-overwrite test, and record what the failure looked like in the test's own
  comment. Both are the kind that pass vacuously.

`cargo test`, no browser test: there is no browser in this feature.

## Open, and deliberately not settled here

- Whether `claude --resume` degrades without the `<id>/` sibling directory.
  Answering it changes one constant and adds one `kind`.
- Whether a restore should offer to re-point a *worktree* project, whose
  storage key contains `%2F` and whose transcript directory is derived from a
  path under `.claude/worktrees/`. Backing one up works today; restoring it
  onto a host where that worktree does not exist resolves to nothing and is
  refused by the destination check, which is correct but unhelpful.
- Step 4, remote destinations. #18 is right that it should wait for this to
  work, and right about what it needs when it comes.
