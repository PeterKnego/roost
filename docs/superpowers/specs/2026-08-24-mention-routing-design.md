# resh — an @-mention that reaches one Claude

Extends the existing Alt+K mention two ways: it fires from a Preview tab as
well as an Edit tab, and it arrives at *the Claude in the terminal you are
looking at* rather than at every Claude connected to the project.

## The problem this solves

`docs/backlog.md:21` asks for: *select file in tree or select text in preview,
press Cmd-<Key> and it gets pasted with @reference to Claude active terminal.*

The IDE-protocol work
(`2026-08-22-claude-ide-protocol-design.md`) already answered the first half of
that sentence and deliberately rejected its verb. `Intent::MentionPath`
(`src/proto.rs:115-119`) resolves a path server-side and sends `at_mentioned`,
because — in that intent's own doc comment — *a paste lands in whatever state
the terminal is in and competes with whatever Claude is doing at that instant*.
What shipped works, and two gaps remain.

**Only an editor can mention.** `mentionTarget()` (`static/app.js:1747-1758`)
requires `tab.mode === "Edit"`. Markdown and images open in Preview by default
(`defaultMode`, `app.js:117-119`), so the two tab kinds a user is most likely to
be *reading* when they want to point Claude at something are exactly the two
that cannot.

**Every Claude hears it.** `ide::notify_all` (`src/ide.rs:370-383`) fans out to
every connection registered for the project. Two Claudes in two terminals of one
project is the normal case this workspace is built for — four panes, ≤16
sessions — and a mention meant for one of them interrupts both. The backlog
entry says *active terminal*; nothing in the codebase can currently tell one
terminal's Claude from another's.

## What changes, in one sentence

A new `src/idesess.rs` learns which resh session a connected Claude is running
in by reading `RESH_SESSION` out of `/proc/<pid>/environ`, `ide::mention_to`
uses that to pick one connection instead of broadcasting, and `mentionTarget()`
stops refusing Preview tabs.

## The reference format, as verified

Read out of the shipped CLI binary
(`~/.local/share/claude/versions/2.1.241`) on 2026-08-24, not from a write-up.
This is that binary's own minified source, reformatted:

```js
function gIs(e, t) {
  let r = path.relative(cwd(), e.filePath), n;
  if (e.lineStart && e.lineEnd)
    n = e.lineStart === e.lineEnd
      ? `@${r}#L${e.lineStart} `
      : `@${r}#L${e.lineStart}-${e.lineEnd} `;
  else n = `@${r} `;
  if (t !== undefined && !/\s/.test(t)) n = ` ${n}`;
  return n;
}
```

Three things follow, and all three are load-bearing:

- The separator is `#L` and a hyphen — `@src/hub.rs#L12-40`. The backlog
  entry guessed `@reference:from-to`; that format does not exist.
- A single-line selection **collapses**: `lineStart == lineEnd` renders
  `@src/hub.rs#L12`, not `#L12-12`.
- Claude computes the relative path itself, from the absolute `filePath` resh
  sends. resh must keep sending an absolute path and must not pre-relativise.

The receiving schema in the same binary is
`{filePath: string, lineStart?: number, lineEnd?: number}` — exactly what
`ide::mention` (`src/ide.rs:389`) already emits. **No wire change is needed
on the Claude-facing side.** Everything below is internal to resh.

## The correlation already exists as latent state

Two facts meet in the middle, and neither was put there for this feature.

`session_env` (`src/session.rs:169-170`) exports `RESH_PROJECT` and
`RESH_SESSION` into every shell resh spawns. They were added so a program in
that terminal could attribute a `RESH_NOTIFY` notification to its session. A
`claude` started in that terminal inherits both, through dtach and through the
shell.

`ide.rs:811-812` already reads Claude's pid out of the `ide_connected`
notification, and `Conn` (`src/ide.rs:437-445`) already stores a fact derived
from that pid — its cwd, via `idecwd.rs`.

So the missing link is one `/proc/<pid>/environ` read at connect time.

> `RESH_SESSION` now has two consumers. A future cleanup that drops it as
> "only used by notifications" silently breaks mention routing, with no test
> failure at the notification end to catch it.

### `src/idesess.rs`

Shaped like `idecwd.rs` on purpose: same pid, same kernel interface, same
three-outcome discipline.

```rust
pub enum Sess {
    /// RESH_SESSION read, name valid, RESH_PROJECT matches this project.
    In(String),
    /// environ read cleanly and carried no RESH_SESSION — positive evidence
    /// that resh did not spawn this Claude.
    Outside,
    /// Could not read environ. Never a reason to exclude a connection.
    Unknown,
}

pub fn session_of_in(proc_root: &Path, pid: u32, project: &str) -> Sess;
pub fn session_of(pid: u32, project: &str) -> Sess;
```

`/proc/<pid>/environ` is NUL-separated and mode `-r--------`, readable for a
same-uid process — confirmed on this host on 2026-08-24.

Three constraints on the implementation:

- **`RESH_PROJECT` must match as well.** Session names are unique within a
  project, not across them: `main` exists in every project that has one. A
  match on `RESH_SESSION` alone would route a mention to a Claude in a
  different project's terminal of the same name.
- **The name is validated before it is trusted.** It arrives from a process
  environment, which anything in that process tree can set. `session::valid_name`
  (`^[A-Za-z0-9_-]{1,32}$`) gates it. It is only ever compared, never used to
  build a path, so this is defence in depth rather than the only barrier — but
  an unvalidated 4 KB "session name" in an error message is its own problem.
- **`Err` is not `Outside`.** An unreadable `environ` and an `environ` with no
  `RESH_SESSION` are different answers, and CLAUDE.md's table records eleven
  defects that came from folding a failed check into a negative result. Only
  the second is evidence.

## Routing — `ide::mention_to`

`CONNS` (`src/ide.rs:289`) is
`HashMap<String, Vec<(u64, Sender<String>)>>`, and `Sess` — like `cwd` — would
live on `Conn`, which the fan-out cannot see. The tuple becomes a named struct
holding `id`, `reply` and `session`, populated from the `ide_connected` arm by
id.

> Widening the tuple rather than adding a second map keyed by conn id is
> deliberate. `ConnGuard` already exists to clean one registry up on
> disconnect; a parallel map is a second lifetime to get right, and the failure
> mode is a dead connection whose session claims a terminal forever.

`mention_to(project, session: Option<&str>, abs, lines)` selects:

| Client sent | Eligible connections |
|---|---|
| a session, some conn is `In(s)` | those conns, **plus** every `Unknown` |
| a session, nothing claims it, exactly one conn exists | that conn |
| a session, nothing claims it, several conns exist | none — `Error` |
| no session, exactly one conn exists | that conn |
| no session, several conns exist | none — `Error` |

`Unknown` connections stay eligible whenever a session was named, for the
reason `idesess.rs` distinguishes them at all: resh could not tell, and a
mention that reaches one extra Claude is recoverable while one that reaches
none looks like a broken keystroke. `Outside` connections are excluded once a
session is named — that is positive evidence they belong to no resh terminal —
but a lone `Outside` conn still wins the "exactly one" case, which is what
keeps the feature working for a `claude` started over plain ssh.

Failures reach only the client that asked, via `send_to`, matching
`do_mention_path`'s existing behaviour and the revert-checked test that pins it
(`hub.rs:3341`, `mention_path_refusal_reaches_only_the_client_that_asked`).
Messages name the session and the count, because "nothing happened" is the
outcome this feature must never produce silently.

## The wire

`Intent::MentionPath` gains one field:

```rust
MentionPath {
    rel: String,
    line_start: Option<u32>,
    line_end: Option<u32>,
    #[serde(default)]
    session: Option<String>,
}
```

`#[serde(default)]` covers the two decode tests that build this intent from a
JSON string (`proto.rs:365` and `proto.rs:370`) — they keep passing untouched,
and that is the point: it pins that an older client's payload still parses.
It does **not** cover the three struct literals in `hub.rs` (3315, 3352, 3384);
Rust requires every field on a struct-variant literal, so those three get
`session: None` explicitly. Adding it there by hand is preferable to a
`Default` impl, because each of those tests should say out loud which routing
case it is exercising.

The server runs `session` through `valid_name` before use and refuses the
intent otherwise, rather than silently degrading to a broadcast.

## The client — `static/app.js`

**`mentionTarget()` relaxes its tab test** and returns `{rel, mode}`. The line
is `app.js:1755`:

```js
if (tab && tab.k === "File" && tab.mode === "Edit" && editors.has(tab.rel)) return tab.rel;
```

**Two** clauses have to go, not one. `tab.mode === "Edit"` is the obvious one.
`editors.has(tab.rel)` is the trap: `editors` holds textareas, and a Preview
tab has no entry in it, so relaxing only the mode check leaves the function
still returning `null` for every Preview tab — the feature would look
implemented and do nothing. The `editors.has` test stays only on the `Edit`
branch, where it is what guarantees `mentionSelection` has a textarea to read.

The function's existing "focused editor first, else the active File tab in
MIDDLE/RIGHT" rule is otherwise already right, and its comment explains why
(focus sits on the body after a reconnect).

**`saveTarget()` (`app.js:1716-1727`) must not be touched.** It is a
line-for-line twin of `mentionTarget()`, including the same two clauses, and
the temptation to factor them together is exactly wrong: saving is meaningful
only for a tab backed by a textarea, so its `Edit` and `editors.has` tests are
load-bearing where `mentionTarget`'s are not. The duplication is the safer
shape; a shared helper would have to grow a flag that means "am I saving or
mentioning", which is the same decision spelled less clearly.

**`mentionSelection()`** keeps its textarea offset math for `Edit` and returns
no range for `Preview`. See "Why a preview carries no line range" below.

**`activeTerminalSession()`** is new: the active `Terminal` tab, preferring the
most recently focused when more than one pane shows one. `focusSession`
(`app.js:2188`) is the single funnel every terminal activation already goes
through, so it is where "most recently focused" gets recorded.

**No active `File` tab is a no-op with feedback.** The client calls
`showError(...)` directly — the same workspace banner every `Event::Error`
already paints (`app.js:395-402`) — with no round trip, because the client
already knows there is nothing to mention. A keystroke that does nothing and
says nothing is indistinguishable from a broken binding.

## Why a preview carries no line range

The intended design was "exact line numbers wherever the preview holds the
file's text verbatim". **That case has no reachable instance**, and the spec
records why so the next reader does not build against it:

- A tab opens in Preview only when `hasRenderedForm(rel)`, and `RENDERED_EXT`
  (`app.js:107`) is `["md","markdown","png","jpg","jpeg","gif","webp","svg","ico"]`.
- `routes.rs:415` branches on `is_image(rel)` **before** reaching
  `file_fragment`, and `IMAGE_EXT` (`routes.rs:232`) contains `svg` and `ico`.

So every Preview tab is either `article.markdown-body` (md/markdown) or an
`<img>` (the other seven). `file_fragment`'s `<pre class="codeview">` branch —
the one shape that does hold verbatim file text — is reachable only for
extensions Preview mode never admits. It is not dead code; it still serves the
`/frag/<proj>/file` endpoint directly. It is dead *for this feature*.

Rendered markdown has no source-line mapping, so a selection in it yields the
file and no range. That is honest; a guessed range is worse than none.

## What this does not do

- **No tree trigger.** The tree has no selection concept — `render.rs:443`
  sets `.sel` on the row whose file is *open* — and the active File tab is a
  better answer to "what am I pointing at" than a new selection state would be.
- **No new keybinding.** Alt+K already means this and matches what the VS Code
  and JetBrains extensions bind; it gains reach rather than a sibling chord.
- **No markdown line ranges.** Deferred to `docs/backlog.md`. The approach is
  recorded there: pulldown-cmark 0.13 (`Cargo.toml:26`) offers
  `into_offset_iter()`, which yields a byte range per event, so block-level
  source lines are available from the parser rather than hand-tracked. The cost
  is that it reshapes every arm of `markdown_html` (`render.rs:234-295`) —
  this file's raw-HTML neutralizing and link/image escaping surface.
- **No selection *text*.** `ShareSelection` already ships contents, is opt-in
  behind `config::share_selection`, and stays as it is.

## Testing

The discipline CLAUDE.md records applies squarely here, because two of this
change's assertions are the kind that pass vacuously.

**Revert-check both routing assertions.** A test with one connected Claude
cannot tell `mention_to` from `notify_all` — the same trap that produced
`mention_path_refusal_reaches_only_the_client_that_asked`'s two-subscriber
design. Every routing test registers **two** fake Claudes with different
sessions and asserts the non-target's inbox is *empty*, then is revert-checked
by pointing `mention_to` back at `notify_all` and watching it fail.

**`idesess.rs` gets a fixture `/proc`,** exactly as `idecwd.rs`'s tests do, so
the three outcomes are reachable without spawning processes: a directory with an
`environ` containing `RESH_SESSION`/`RESH_PROJECT`, one containing neither, and
an absent or unreadable one. The `Unknown` case must assert it is `Unknown` —
asserting "not `In`" would pass for `Outside` too, and the whole point is that
those two differ.

**A cross-project test.** Two projects each with a session named `main`, and a
mention in one must not reach the other's Claude. This is the assertion that
fails if `RESH_PROJECT` is dropped from the match, and nothing else in the suite
would catch that.

**The client half needs a browser.** No Rust test reaches `static/app.js`, so
the Preview trigger and the empty-target banner are verified under
`tests/browser/`, per CLAUDE.md, with attention to the four traps in
`tests/browser/README.md`. There is already a home for this:
`tests/browser/ide.mjs` covers *"the Alt+K handler that turns an editor
selection into a MentionPath intent"* (its own module doc, line 9) and asserts
on `line_start`/`line_end` at line 453. The new cases extend that file rather
than starting another.

Two shapes to avoid there specifically: a test that asserts a mention "was
sent" without asserting *which* connection received it repeats the
single-subscriber trap above; and a Preview-trigger test must assert the
mention actually carried the previewed `rel`, because "no error banner
appeared" is equally true of a handler that returned `null` and did nothing —
which is precisely the `editors.has` failure mode described above.

## Risks

- **Linux-only.** `/proc/<pid>/environ` has no portable equivalent. `idecwd.rs`
  already makes resh Linux-only in the same way, and the deploy target is a
  Linux host, so this adds no new platform constraint — but it does add a
  second place that would need rewriting if that ever changed.
- **A wrapper that scrubs the environment** breaks the correlation. The result
  is `Outside`, and a mention with a named session skips that connection.
  Acceptable: the fallback is a clear error, not a wrong delivery.
- **The pid is Claude's, not the shell's.** If a user runs `claude` inside
  `tmux` inside a resh terminal, the environment still inherits, so this holds.
  If they `ssh` from a resh terminal to elsewhere, `/proc` on this host has no
  such pid and the result is `Unknown` — eligible, not excluded, which is the
  conservative direction.
