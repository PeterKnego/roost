# resh — speaking the Claude Code IDE protocol

Makes resh an IDE as far as Claude Code is concerned: a per-project WebSocket
MCP server, discovered through a lock file in `~/.claude/ide/`, that turns
Claude's edit prompts into a real diff tab in the browser, sends the editor's
selection as ambient context, and inserts `@src/hub.rs#L1-99` into Claude's own
prompt box. resh implements the *server* half of a protocol Anthropic ships and
partially documents, and deliberately implements less of it than VS Code does.

## The problem this solves

resh exists for AI-assisted development — the README says so in its third
sentence — and Claude runs in a terminal pane while resh owns the editor beside
it. But the two panes do not know about each other. Everything Claude and the
workspace have to say to one another goes through 80 columns of text.

**A proposed edit arrives as ASCII.** When Claude asks permission to change a
file, the diff renders in the terminal, in the terminal's colours, at the
terminal's width, with no relation to the editor tab that may have the same file
open two panes away. resh already knows how to show a divergence properly:
`textdiff.rs` exists precisely because dumping both files whole "is not a diff",
and the save-conflict banner shows changed hunks with line numbers. Claude's
edits get none of that.

**Pointing at a file is a paste.** `docs/backlog.md:20` — the first item under
"First things to do" — asks for: *select file in tree or select text in preview,
press Cmd-<Key> and it gets pasted with @reference to Claude active terminal*.
Pasting into a terminal is the wrong mechanism and the backlog entry names it
honestly: the paste lands in whatever state the terminal is in, competes with
whatever Claude is doing at that instant, and depends on bracketed paste
surviving a full-screen app. This is the same class of problem that forced the
OSC 52 handler (`2026-08-21-terminal-links-design.md`).

**The selection is invisible.** A user highlights the function they are asking
about and then has to describe it in words.

All three have the same shape: the workspace has state that Claude cannot see,
and Claude has intent the workspace cannot render.

## What changes, in one sentence

resh runs one loopback WebSocket MCP server per open project, advertises it in
`~/.claude/ide/<port>.lock` with a fresh token, injects `CLAUDE_CODE_SSE_PORT`
into every shell it spawns, and handles five messages: `ide_connected`,
`openDiff` and `close_tab` inbound, `at_mentioned` and `selection_changed`
outbound.

## The protocol, as verified

Everything below was read out of the shipped CLI binary
(`~/.local/share/claude/versions/2.1.239`) on 2026-08-22, not from a write-up.
Quoted fragments are that binary's own minified source.

**The IDE is the server.** The extension hosts the socket; `claude` connects out
to it. That inversion is what lets the integration work for a `claude` the IDE
did not spawn — including one attached to a dtach session that outlived the
browser tab, which is resh's normal case.

**Discovery is a lock file named by its port.**

```js
n=u.pid, o=u.ideName, i=u.transport==="ws",
s=u.runningInWindows===!0, a=u.authToken
...
let c=l.replace(".lock","");
return {workspaceFolders:r, port:parseInt(c), pid:n, ideName:o, useWebSocket:i, ...}
```

`~/.claude/ide/<port>.lock` holds `{pid, workspaceFolders[], ideName, transport,
authToken}`. `$CLAUDE_CONFIG_DIR`, when set, relocates the directory.

**Matching has two routes, and the second is a shortcut.**

```js
let r = q.CLAUDE_CODE_SSE_PORT ?? null, n = Nn().normalize("NFC");
if (q.CLAUDE_CODE_IDE_SKIP_VALID_CHECK) c=!0;
else if (l.port === r) c=!0;
else for (let f of l.workspaceFolders) {
  let h = Xhe.resolve(m).normalize("NFC");
  if (n === h || n.startsWith(h + Xhe.sep)) { c=!0; break }
}
```

Either cwd is at or under a declared workspace folder, **or** the port equals
`$CLAUDE_CODE_SSE_PORT`, which skips the path comparison entirely. There is also
a liveness gate — `if(!l.pid||!WAf(l.pid)) continue` — so the lock file's pid
must be resh's own live pid.

**Transport and authentication.**

```js
else if (t.type === "ws-ide") {
  let fe = {"User-Agent": une(), ...t.authToken && {"X-Claude-Code-Ide-Authorization": t.authToken}},
      we = new globalThis.WebSocket(t.url, {protocols:["mcp"], headers: fe, ...})
```

`ws://127.0.0.1:<port>`, subprotocol `mcp`, JSON-RPC 2.0, bearer token in
`X-Claude-Code-Ide-Authorization`. Plaintext `ws://` is deliberate and
Anthropic's JetBrains page argues it: on loopback, anything that can sniff the
socket can also read the token out of the lock file.

**`openDiff` is a blocking call with exactly three outcomes.**

```js
Abw: e[0].text === "FILE_SAVED" && typeof e[1].text === "string"
wbw: e[0].text === "TAB_CLOSED"
Ebw: e[0].text === "DIFF_REJECTED"
```

Anything else raises `Not accepted`. `FILE_SAVED` carries the content in
`e[1].text`, which is how "the user edited the proposal before accepting"
reaches Claude.

**Almost nothing is visible to the model.**

```js
_BS = ["mcp__ide__executeCode","mcp__ide__getDiagnostics"];
function P$f(e){ return !e.startsWith("mcp__ide__") || _BS.includes(e) }
```

The server registers as the MCP client named `ide`, its tools are namespaced
`mcp__ide__*`, and every one except those two is filtered out before the tool
list reaches Claude. `openDiff`, `close_tab` and the selection notifications are
CLI-UI plumbing, not agent capability.

That last fact is the one that makes this feasible. Everything a user perceives
as "Claude is aware of my IDE" is *host-side rendering* of a private RPC
channel. resh is not extending what the model can do; it is taking over how one
existing interaction is drawn.

## Security: the Origin rule inverts here, and that is not a bug

`src/origin.rs` opens with the rule this codebase treats as load-bearing:

> A missing Origin is rejected: every browser sends one, so its absence
> means a non-browser client, which has no business here.

**On the IDE socket that reasoning runs backwards.** The client is `claude`, a
Bun process. It sends no `Origin`. It sends a token. So:

- The workspace socket (`/ws/{project}/_workspace`) authenticates by `Origin`
  and **rejects a handshake with none**.
- The IDE socket authenticates by constant-time token equality and **rejects any
  handshake that carries an `Origin` header at all**, because a browser is the
  only thing that sends one and a browser has no business on this socket.

Two websockets, opposite rules. This must be written down in `CLAUDE.md`
alongside the existing constraint, or someone will later "restore consistency"
and reintroduce the exact defect the constraint was written against.

That defect is not hypothetical. **CVE-2025-52882** is this protocol's own
history: Claude Code extensions ≤1.0.23 ran an unauthenticated localhost
WebSocket and ignored `Origin`. Because WS handshakes bypass same-origin policy,
any web page could scan the local port range, connect, and read files or execute
code with no user interaction beyond visiting the page. The fix shipped in
1.0.24 is the lock-file token described above. resh's hard constraint and
Anthropic's CVE are the same finding, reached independently.

Three consequences for this design:

1. **The token is compared in constant time** and is 128 bits from the OS
   CSPRNG, regenerated per resh start. Never logged, never rendered, never sent
   to a browser.
2. **`executeCode` is not implemented.** It is one of the two model-visible
   tools, and it is arbitrary code execution reachable from this socket. resh
   has no notebook kernel and no reason to grow one. Return a JSON-RPC method
   error. JetBrains does not expose it either, so this is the protocol's normal
   state, not a resh quirk.
3. **The listener binds `127.0.0.1`.** No equivalent of JetBrains' "accept
   connections from all network interfaces" setting. resh's whole security
   boundary is the loopback bind; the WSL scenario that setting exists for is
   not resh's deployment.

## Architecture

### One listener per project

The lock file is keyed by port and declares `workspaceFolders`. A single
host-wide listener would have to list every open project, and the CLI's matcher
takes the *first* folder that contains cwd — so a `claude` in project A could be
handed the socket that renders project B's tabs. Per-project listeners on
OS-assigned ephemeral ports are the shape the protocol wants.

The lifecycle follows the Hub, which is already per-project
(`hub.rs`: *"One Hub per project"*): the listener and lock file are created when
a project's hub is first built and torn down by `CloseProject`.

### Discovery is belt and braces

resh writes the lock file *and* injects the port. `src/session.rs:191-198`
already sets `TERM`, `RESH_NOTIFY`, `RESH_PROJECT`, `RESH_SESSION` on every
spawned shell; `CLAUDE_CODE_SSE_PORT` joins them, scoped to the project's own
listener. A `claude` started inside a resh terminal then matches by port with no
path comparison at all — which sidesteps every symlink, canonicalisation and
worktree question in one move.

The lock file is still required, because it is the only place the token lives,
and it is what makes `/ide` work from a shell resh did not spawn.

### Which directory Claude is actually in

The protocol does not tell you. On connect the CLI sends exactly one thing about
itself:

```js
async function Yki(e){ await e.notification({method:"ide_connected", params:{pid: process.pid}}) }
```

`ide_connected` carries **a pid and nothing else** — no cwd, no workspace
folder, no session id. The standard MCP `initialize` handshake adds `clientInfo`
(name and version), which is also not a path. So the directory question has to
be answered by resh, from the pid.

On Linux it is answered exactly, by the kernel:

```
$ readlink /proc/47421/cwd
/home/claude/projects/resh
```

Verified on this host, same-user process, 2026-08-22. That is *positive
evidence* of Claude's working directory at the moment of asking — better than
anything resh could infer, because it survives a `cd`, and because a worktree is
just a different directory (`worktree.rs`: *"A worktree is its own project —
separate directory, rel path, sessions and layout"*).

This matters more than it first appears. resh knows the directory it *spawned* a
shell in (`session.rs`, `cb.cwd(dir)`), but that is the session's starting point,
not Claude's current one. The two diverge in precisely the case that motivates
asking: a user runs `claude` in a worktree under `{repo}/.claude/worktrees/{name}`
— the location `worktree.rs` documents as dominant, and which Claude Code itself
creates — and every path in every `openDiff` is then relative to a directory
resh's project root does not contain.

Three rules follow, and the third is the one this codebase keeps having to
relearn:

1. **Resolve the pid's cwd on `ide_connected`**, and treat that as the
   connection's working directory for the life of the socket. Re-resolve rather
   than cache if a path arrives that does not confine under it.
2. **Confine against the project root, and never widen it to the cwd.**
   *(Amended after Task 7, which found the original wording unimplementable —
   see `task-7-report.md`.)* `openDiff` sends absolute paths, and they are
   confined against the project workspace. Confining against the resolved cwd
   instead cannot work in either direction: the rel a confinement produces is
   what the hub opens a tab for, and the hub's paths are relative to the
   project root, so `safe_resolve(cwd, rel)` double-joins the prefix and
   refuses *every* `openDiff` from a Claude started in a subdirectory
   (`base=<ws>/src` with `rel="src/a.rs"` canonicalises `<ws>/src/src/a.rs`
   → `ENOENT`); and a cwd *outside* the project would confine paths resh
   cannot render into a tab at all. The original bullet's stated intent still
   holds exactly as written — a worktree under the project root confines
   naturally, a worktree in a sibling directory does not, and that is a
   refusal rather than a silent widening of the root. Confining against the
   root is what delivers it.

   One thing the cwd cannot be traded for: `abs_to_rel` canonicalises its
   target, so it cannot confine a file that does not exist yet — and an
   `openDiff` for a missing file is Claude *creating* one. The parent is
   confined instead (`safe_resolve_parent`, which exists for this split), and
   the final component validated separately.

   The cwd resolution in rule 1 stays: it is how a connection is matched to
   its project, and it is still right.
3. **`readlink` failing is a third outcome.** `ESRCH` means the process is gone;
   `EACCES` or a missing `/proc` means resh cannot tell. Only the first justifies
   dropping the connection. "I could not read `/proc/<pid>/cwd`" must not be read
   as "Claude is not in this project" — that is the same shape as the eleven
   defects in the `CLAUDE.md` table, one socket-close away from killing a live
   integration because a check failed.

Non-Linux hosts have no `/proc/<pid>/cwd`. resh's deploy target is Linux
(`docs/deploy.md`), and the honest fallback elsewhere is to assume the project
root and let confinement refuse what does not fit — a degraded integration, not
a wrong one.

### Writing the lock file

`CLAUDE.md` already mandates atomic temp-file-plus-rename for persistent
evidence. Here it is not a precaution but a correctness requirement: the CLI
**deletes any lock file it cannot parse** (`Failed to delete unreadable IDE
lockfile`). A half-written file does not degrade the integration, it silently
unregisters it.

The reverse direction is where this codebase's standing failure mode lives.
`~/.claude/ide/` is shared with real IDEs and with other resh instances on the
same host. So:

- resh removes only ports it recorded writing. It never enumerates that
  directory and never deletes a file it does not own.
- On startup, resh does not sweep stale entries. A stale lock file is a row the
  CLI itself reaps after a pid check; guessing on resh's behalf risks unlinking
  a live IntelliJ's registration because a check failed. This is the
  "absence of evidence" rule applied to someone else's directory: *stale rows
  are recoverable, deleting another program's live registration is not.*
- If the directory cannot be created or written, IDE integration is reported as
  unavailable in the UI. It is not retried in a loop and it never fails a
  project open.

## The messages

### `ide_connected` → who is on the other end

`{pid}`. Its whole value is the pid: see *Which directory Claude is actually in*
above. resh must handle it (the CLI logs `Failed to send ide_connected
notification` if it errors) even in the phase where it does nothing else.

### `openDiff` → a proposal tab

Claude sends `{old_file_path, new_file_path, new_file_contents, tab_name}` and
**blocks** until resh answers. resh opens a tab showing `textdiff` between the
file on disk and the proposal, with Accept / Reject, and the proposal side
editable.

Three responses, matching the three the CLI accepts:

| User action | Response |
|---|---|
| Accept unchanged | `TAB_CLOSED` |
| Accept after editing the proposal | `FILE_SAVED` + the edited text |
| Reject, or close the tab | `DIFF_REJECTED` |

*(Amended twice: after the final whole-branch review, which found the middle
row unreachable from the UI, and again once the editable box landed.)* **All
three outcomes are now reachable.** Accept and Reject on the proposal tab
produce `TAB_CLOSED` and `DIFF_REJECTED`; an **Edit** button beside them opens
a textarea seeded with Claude's proposed content, and accepting after changing
it answers `FILE_SAVED` plus the text the human typed — which is how Claude
learns the file will not match its own proposal.

The box is built with `createElement` and seeded through `.value`, *not*
rendered into the server-side fragment, and that is a deliberate reading of
CLAUDE.md's "all HTML is built in `render.rs`". That rule exists because
hand-built markup is where escaping goes wrong, and file content inside a
server-rendered `<textarea>` is precisely that trap: a `</textarea>` occurring
in the proposed text would close the element and let the remainder parse as
markup. `.value` is never parsed as HTML at all, so this is the stronger
guarantee rather than a shortcut around the rule.

Two properties the box must not weaken, both covered by
`tests/browser/ide.mjs` §F:

- **It appears only on request.** The diff is the thing to read; a textarea
  covering it by default would defeat reviewing the change.
- **It changes nothing about the content-less case.** A proposal whose
  `Event::Proposal` has not arrived still renders a placeholder with no
  buttons at all — no Accept, no Reject, and no Edit — because answering what
  you cannot see is the failure this whole tab exists to prevent.

**The CLI keeps its own permission prompt open at the same time, and that is
expected.** Its prompt component takes a `showingDiffInIDE` flag and only swaps
the title — `Opened changes in ${ideName} \u29C9` instead of "Do you want to
make this edit" — so both surfaces are live at once and either resolves the
same request: resh's Accept becomes `{behavior:"allow", updatedInput:…}` and
its Reject becomes `{behavior:"deny", message:"User denied via IDE"}`.
**Observed 2026-08-23 against a real `claude` v2.1.241: clicking Accept in resh
cleared the terminal prompt.** resh cannot suppress that prompt and should not
try — its only outputs are the three reply strings. A user who does not want it
is choosing a permission mode (`acceptEdits`), not an integration setting.

Note one wording mismatch inherited from the same component: its
`showingDiffInIDE` branch can print *"Save file to continue"*, which describes
the VS Code flow — edit the diff buffer, save, and the save produces
`FILE_SAVED`. resh gets there by an explicit **Edit** button and then Accept,
because a `/frag` view has no editor buffer to save and "save" in a browser
would read as "write to disk", which is precisely what resh must not do.

**resh must not write the file.** On acceptance the CLI continues its own tool
call with the (possibly edited) content as updated input — `l({behavior:"allow",
updatedInput:C, ...})`. If resh also wrote, the file would be written twice and
resh's `self_writes` suppression would be reasoning about the wrong hash. resh
answers and lets the watcher observe Claude's write, exactly as it does today.

Four consequences worth spelling out, because each is a way to get this wrong:

- **A pending diff is a request held open for minutes.** No lock may be held
  across it. `hub.rs` already has the pattern for this — `do_close_project`
  keeps a `Weak` back-reference so it can re-lock *later* rather than block with
  the lock held.
- **A pending diff cannot survive a restart.** The socket dies with resh, so the
  CLI's call fails on its own. Proposal tabs must therefore be dropped when the
  persisted layout is loaded, not restored into a state whose counterparty is
  gone. This is a new case for layout persistence, not a free one.
- **A dirty buffer for the same path is a real conflict.** resh may be holding
  unsaved edits to the file Claude proposes to rewrite. The proposal tab shows
  that state and accepting is refused while it holds — the same stance as
  conflict-guarded save: never force, make the human resolve it.
- **Pending diffs are capped**, in the spirit of the existing ≤16-session and
  ≤50-buffer caps. Over the cap, answer `DIFF_REJECTED` rather than queueing.

`close_tab {tab_name}` closes the proposal tab it names, and is how Claude
withdraws a proposal it no longer needs.

### `at_mentioned` → the backlog's first item, done properly

resh sends `{filePath, lineStart, lineEnd}`; the CLI inserts
`@src/hub.rs#L12-40` into its own prompt box. No paste, no bracketed-paste
question, no interaction with what the terminal is currently drawing.

Trigger from two places, both of which resh already tracks: the selected node in
the file tree, and the selection in an editor tab.

### `selection_changed` → ambient context

`{filePath, text, selection:{start:{line,character},end:{…}}}`, sent on editor
selection change, debounced.

**This ships file contents to Claude without an explicit user action**, which is
a change in resh's posture and must be treated as one. Claude Code's own answer
is `Read` deny rules; resh has no permission system to hang that off. So: the
feature is off unless a project opts in, and the pane header shows when a
selection is being shared — the same "visible and deliberate" stance the README
takes about sessions. Shipping a highlighted line of `.env` silently is not an
acceptable default.

### `getDiagnostics` → honestly empty

The only model-visible tool resh implements, and resh has no language server, so
it returns an empty diagnostics array. That is protocol-legal and it is what
Claude sees when nothing is wrong — which is a lie by omission if resh ever
grows a `cargo check` bridge and forgets to wire it. Note it in the code, do not
pretend it is a feature.

## What this is not

- **Not a chat panel.** VS Code's current extension bundles its own CLI copy and
  renders a full GUI — session list, history, checkpoints, plan review. That is
  a product, not an integration. resh's premise is that Claude runs in a real
  terminal and the viewer reflects disk; the JetBrains shape (terminal plus a
  sidecar that owns editor state) is the one resh already has.
- **Not `executeCode`.** See Security.
- **Not the tools resh cannot confirm.** `openFile`, `getOpenEditors`,
  `getWorkspaceFolders`, `checkDocumentDirty`, `saveDocument`,
  `closeAllDiffTabs` and `getCurrentSelection` appear in third-party
  reverse-engineering (`coder/claudecode.nvim`), but **were not found in the
  binary** — `getOpenEditors` grepped to zero occurrences. Implementing from an
  unverified list is how you ship dead code. Add each one only after observing a
  real `claude` call it.
- **Not a third transport.** `sse-ide` exists in the CLI's config schema. resh
  speaks `ws-ide` only.

## Testing

The traps here are this codebase's two known ones, both live.

**The substitution trap is unusually sharp.** A mock client that sends what this
spec says the CLI sends will pass against an implementation that is wrong about
what the CLI *actually* sends — and the spec was derived from minified code.
Every mock in this feature is testable in isolation and still capable of being
uniformly wrong, exactly the way `RESH_CMD=cat` hid the missing dtach socket
directory. So a **real `claude` binary** has to be driven against a real resh
listener.

*(Amended after the final whole-branch review.)* This originally read "the suite
must include at least one test that drives a real `claude` binary". **It does
not, and what happened instead was manual.** A real `claude` was driven against
a real resh four times over the branch:

- Task 4 — a real `claude` connects, authenticates with the lock-file token,
  and reports the workspace.
- Task 5 — a fresh project's first terminal carries `CLAUDE_CODE_SSE_PORT`.
  This is how the missing-port defect was found at all; no Rust test had it.
- Task 8, twice — including a full `openDiff` → Accept → the CLI writes the
  file loop.

Nothing automated does any of that. `claude` is not installed on the test host,
its version is not pinned, and a test that skips when the binary is absent
would go green for the wrong reason on precisely the machine that runs the
suite — this codebase's own dominant failure mode. The honest statement of the
state: the substitution trap is held off by a manual step recorded in the task
reports, not by `cargo test`. Automating it (pinned binary, a fixture that
fails rather than skips when it is missing) is worth doing and is not done.

**Would the test fail if the code were deleted?**

- A token test must assert that a handshake with a *wrong* token is refused and
  *why* — not merely that the right one succeeds. A server that accepts
  everything passes the happy-path half.
- The Origin-inversion test needs both directions: no-Origin-plus-good-token
  accepted, and browser-Origin refused. One without the other is the CVE.
- The `openDiff` tests need all three responses distinguished. A test that only
  checks "the call returned" cannot tell `TAB_CLOSED` from `DIFF_REJECTED`, and
  those mean opposite things to Claude.
- The lock-file test must assert the file is never observable half-written, and
  separately that resh does not unlink a lock file it did not create. The second
  is the one that protects a stranger's IntelliJ.

Per `CLAUDE.md`: revert the fix and watch each of these fail before believing
them.

**The browser check is not optional.** Proposal tabs, the accept/reject
affordance, and the mention keybinding all live in `static/app.js`, which no
Rust test reaches. A `tests/browser/*.mjs` script driving a real Chromium is
required, alongside the existing `reconnect.mjs` and `upload.mjs`.

## Task order

1. Listener, handshake, token, Origin inversion. Nothing useful yet; all of the
   security surface. Reviewed on its own.
2. Lock file: atomic write, pid, workspace folder, lifecycle on hub create and
   `CloseProject`, `CLAUDE_CODE_SSE_PORT` in `session.rs`. End state: `/ide`
   connects and says `Connected to resh.` and no message is handled.
3. `ide_connected` and cwd resolution from its pid, including the three-outcome
   `readlink` handling. Nothing user-visible, but every later path decision
   depends on it, so it lands before the first message that carries a path.
4. `getDiagnostics` (empty) and the `mcp__ide__` filtering, so the tool list the
   model sees is correct before anything renders.
5. `at_mentioned`. The backlog's first item, and the smallest thing that is
   visibly better than today.
6. `openDiff` and `close_tab`. The largest task by a distance; the pending-request
   lifecycle, the new tab kind, layout persistence, and the dirty-buffer
   interaction all land here.
7. `selection_changed`, opt-in, with the sharing indicator.

## Open questions

- **Does the proposal tab reuse `Tab::Diff` or add a variant?** `Tab::Diff {rel:
  Option<String>}` is a git diff of a tracked path; a proposal is disk-versus-
  never-written-content keyed by a pending request id. They render similarly and
  mean different things. Leaning to a new variant, which forces the layout-
  persistence question to be answered explicitly rather than by accident.
- **What is `ideName`?** It reaches the user as `Connected to <name>.` "resh" is
  honest. Whether the CLI treats unknown names specially anywhere is unverified.
- **Multiple browsers, one proposal.** resh mirrors all state to every connected
  client. Two people can click Accept and Reject on the same proposal. First
  answer wins is the obvious rule; whether the loser sees anything is not
  decided.
- **Does anything break when a project is opened twice** — two resh instances,
  two lock files, two workspace folders naming the same directory? The CLI takes
  the first match. Untested.
- **Should a worktree get its own lock file?** `worktree.rs` already treats a
  worktree as its own project with its own sessions and layout, which argues
  yes — one listener per worktree, its own `workspaceFolders`. The alternative
  is one listener whose folder list covers the repo and every worktree, which
  reintroduces the first-match ambiguity the per-project rule exists to avoid.
  Leaning to per-worktree; not decided.
