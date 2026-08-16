# deadlight v3 — IDE-style workspace design

Supersedes the layout, state and terminal sections of
`2026-08-16-deadlight-v2-design.md`. Path confinement, markdown/diff/code
rendering, the settings cascade and the deployment model carry over unchanged
unless contradicted here.

## What changes and why

v2 is a tab-switcher: one pane at a time, Terminal *or* Files *or* Changes,
with all viewer state living in the browser and the terminal's state living in
zellij. v3 makes it a workspace — four panes visible at once, any kind of tab
in any pane — and moves *all* state to the server.

The argument for server state is that the terminal already works that way and
works well: a session survives the browser dying and reattaches from anywhere.
Having the viewer persist differently (browser storage, URL hash) would be the
inconsistency. One model, one place, one lifetime.

The second driver is that deadlight is primarily for AI engineering. Claude
runs in a pane and edits files in the background. A viewer that does not
reflect those edits live is not merely less convenient — it is showing you
something false. Filesystem watching is therefore a core requirement, not a
nice-to-have.

## Panes and tabs

A **fixed skeleton of four panes**: left-top, left-bottom, middle, right,
separated by three draggable dividers. Panes are never created, split or
destroyed — there is no layout tree, no merge rules, no empty-pane cleanup.

**Tabs are universal.** One flat `Tab` enum covers every content type, and any
tab may live in any pane. A tab strip does not know what it holds, so moving a
tab between panes is an ordinary state mutation rather than a feature per
content type.

```rust
enum Tab {
    Tree,
    Changes,
    File     { rel: String, mode: Mode },  // Mode = Preview | Edit
    Diff     { rel: Option<String> },      // None = the "full diff" entry
    Terminal { session: String },
}
```

Defaults on first open: `Tree` in left-top, `Changes` in left-bottom, nothing
in middle, a `shell` terminal in right. That default reproduces the layout the
panes are named for while leaving everything movable.

**Tab identity focuses rather than duplicates.** Opening a `File` that is
already open anywhere activates the existing tab. Same for `Tree`, `Changes`
and `Diff`. Terminals are distinguished by session name, so `shell` and
`claude` coexist naturally.

**Closing.** Closing a terminal tab **detaches only** — the dtach session
survives and reopening the same name reattaches to it. Killing a session is a
separate, explicit action. Closing a `File` tab whose buffer is dirty prompts
(discard / keep editing); on discard the buffer is dropped, otherwise it
persists in state even with no tab open, which is what makes buffers
crash-safe. A tab whose file has been deleted underneath it stays open and
renders a hint rather than vanishing, so unsaved text is never silently lost.

## State

One `Workspace` per project, owned by the server, mutated only through socket
intents.

```rust
struct Workspace {
    version: u64,                        // bumped on every mutation
    sizes:   Sizes,                      // left_w, right_w, left_split
    panes:   [Pane; 4],                  // LeftTop, LeftBottom, Middle, Right
    buffers: HashMap<String, Buffer>,    // keyed by repo-relative path
}

struct Pane { tabs: Vec<Tab>, active: usize }

struct Buffer {
    text:       String,      // unsaved content
    base_mtime: SystemTime,  // captured at open / last save
    base_hash:  u64,         // the conflict guard
    dirty:      bool,
}
```

**Buffers are keyed by path, not owned by a tab.** The same file open in two
panes shares one buffer and one dirty flag.

**Persistence** to `$DEADLIGHT_STATE_DIR` (default
`~/.local/state/deadlight/`), one `{project}.json` per project, written
debounced. Deliberately outside the repo — following zellij, which keeps
session state in its own directory — so pane drags never appear in
`git status`. Buffers persist too, which makes them crash-safe: kill the
server mid-edit and unsaved text returns. Mode `0600`, directory `0700`,
because buffer text is file content and may be secret.

A corrupt or missing state file yields defaults and a visible warning, never a
crash — the rule the settings cascade already follows.

**Mirroring.** Every mutation broadcasts to all attached clients: open a file
in one browser and it opens in all of them, as two zellij clients mirror one
screen. The server is authoritative; there is no client-side merging and no
version negotiation. Last write wins. `version` exists so a reconnecting
client can tell whether it missed anything and re-request full state.

**The echo rule.** Each connection has an id and broadcasts carry the
originating id. Layout changes are idempotent and applied unconditionally.
Buffer *text* updates are ignored by the client that sent them, or your own
keystrokes round-trip and stomp your cursor. Client-side typing is debounced
before transmission.

## URLs and the wire

```
/                          index page                     (unchanged)
/{project}                 workspace page                 (unchanged)
/static/*                  assets                         (unchanged)
/frag/{project}/*          server-rendered HTML fragments (unchanged)
/ws/{project}/_workspace   workspace state socket         (new)
/ws/{project}/term/{name}  one per terminal tab           (replaces /ws/{project})
```

**Two socket kinds, deliberately different.** Terminal sockets stay dumb byte
pipes — raw binary both ways, the code that already works. The workspace
socket carries JSON text frames.

```
→ {"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"src/main.rs","mode":"Preview"}}
→ {"t":"MoveTab","from":2,"idx":0,"to":3,"at":1}
→ {"t":"CloseTab" | "ActivateTab" | "Resize" | "SetMode"}
→ {"t":"EditBuffer","rel":"src/main.rs","text":"..."}
→ {"t":"SaveBuffer","rel":"src/main.rs","force":false}
→ {"t":"CreateFile" | "CreateDir" | "DeleteFile" | "RenamePath"}

← {"t":"State","version":41,"origin":"c7","ws":{...}}
← {"t":"BufferText","rel":"...","text":"...","origin":"c7"}
← {"t":"BufferStale","rel":"..."}
← {"t":"SaveResult","rel":"...","ok":true}
← {"t":"SaveResult","rel":"...","conflict":{"diff_html":"..."}}
← {"t":"FileChanged","rel":"..."}
← {"t":"TreeChanged"} | {"t":"StatusChanged"} | {"t":"Error","msg":"..."}
```

`State` deliberately excludes buffer text, carrying only sizes, panes and
per-buffer metadata. Text moves in its own `BufferText` event addressed to
everyone but the originator; otherwise every keystroke rebroadcasts every open
buffer to every client.

**All writes travel over the websocket, so HTTP stays GET-only.** This is the
main reason for choosing a second socket over SSE plus POST endpoints: no body
parsing enters the hand-rolled HTTP layer, and there is no state-changing verb
for a hostile page to forge.

## Terminal

**deadlight owns the PTYs.** zellij is dropped. Once deadlight has universal
tabs, zellij's tabs are a second, worse tab system nested inside one of them,
and its status bar, theme, keybindings and first-run wizard all fight the
surrounding UI.

```rust
struct TermSession {
    pty:         PtyPair,
    child:       Box<dyn Child>,
    scrollback:  VecDeque<u8>,   // ring buffer, ~1 MB, replayed on attach
    subscribers: Vec<ConnId>,
}
```

Sessions are keyed `{project}-{name}`. Attach subscribes and replays
scrollback; detach unsubscribes and leaves the PTY running. Mirroring reuses
the same broadcast primitive as workspace state, so multiple clients on one
session cost nothing extra.

**The spawned command is `dtach`, and that is a configuration seam.** deadlight
provides scrollback and mirroring; `dtach` provides survival across a deadlight
restart — which matters because deadlight is under active development and a
rebuild must not kill a running Claude session. dtach draws no UI at all.

```
DEADLIGHT_CMD default:
  dtach -A $DEADLIGHT_STATE_DIR/sock/{project}-{session} -E -r winch -z $SHELL -l
```

`-E` removes the escape character entirely, `-z` the suspend key, `-r winch`
repaints full-screen applications on attach. dtach does not replay scrollback
itself; deadlight's ring covers browser reloads and network drops, and after a
deadlight restart a full-screen app repaints via winch while a bare prompt
comes back blank until the next Enter. Accepted.

**Terminals are pooled DOM nodes on the client** — created once, re-parented
with `appendChild` when a tab moves or activates, never re-rendered from
state. Recreating the node drops the socket and detaches the session. This is
the single sharpest implementation constraint in this design, and it is the
one place where "derive the DOM from state" must not be applied literally.

Every move, activate or divider drag triggers a re-fit and a `resize:` to the
PTY. With clients of differing sizes the PTY takes the **smallest** attached
client's geometry, so nobody sees clipped output.

## Editing

The middle pane's `File` tabs toggle between Preview and Edit. Edit is a plain
textarea — no LSP, no autocomplete, no find/replace. Rich editing belongs in
the editor running in the terminal pane.

Switching to Edit makes the server read the file, record `base_mtime` and
`base_hash`, and push `BufferText`. No new HTTP endpoint.

**Saving:**

1. Resolve through the existing confinement check.
2. `stat` the file. If mtime or hash differ from the buffer's base and `force`
   is false → `SaveResult{conflict}`, nothing written.
3. Otherwise write atomically: temp file in the same directory, copy the
   original's mode, `fsync`, `rename`. No truncate-in-place, so a crash cannot
   leave a half-written source file.
4. Update base mtime/hash, clear `dirty`, broadcast `State`.
5. Broadcast `FileChanged` so open Diff and Changes tabs re-fetch.

Conflicts render by writing the buffer to a temp file and running
`git diff --no-index` against the version on disk, through the existing
`diff_html` — reusing tested machinery rather than adding a differ.

Save is a globally visible act: it updates the file for every attached client,
and a Claude session in the next pane sees the new bytes immediately.

## File operations

`CreateFile`, `CreateDir`, `DeleteFile`, `RenamePath`, all from the tree's
context menu.

This requires a **second path resolver**. The existing one canonicalizes the
target, which requires the target to exist — useless for creation. The new one
canonicalizes the *parent*, confines that against the project root, then
validates the final component separately: non-empty, no `/`, not `.` or `..`.
Creation and rename destinations use it; reads and deletes keep the existing
one.

**Delete is non-recursive** — single files and empty directories only, with
client-side confirmation. Not because recursive delete is an escalation (the
terminal is right there) but because a misclick in a tree should not be able
to remove `target/` or `.git`.

## Filesystem watching

`notify` with per-directory, non-recursive watches registered while walking the
tree, skipping `SKIP_DIRS` and the per-project `hide` list. That skip is
load-bearing: a recursive watch on a Rust project turns every `cargo build`
into thousands of events. New directories are watched as they appear.

`.git` is skipped for the tree, but **`.git/index` and `.git/HEAD` are watched
deliberately** — that is how the Changes pane and branch label learn that
Claude committed something.

Classification is a **pure function** — `(path, open_buffers) -> Tree |
Status | Buffer(rel) | Ignore` — so routing is unit-testable without a
filesystem. Each class is debounced separately, with intervals configurable so
tests can set them to zero.

| change | broadcast |
|---|---|
| anything in the tree | `TreeChanged` → clients re-fetch the tree fragment |
| `.git/index`, `.git/HEAD` | `StatusChanged` → Changes pane and branch badge |
| a file with an open buffer | see below |

**Open-buffer changes** are the AI-engineering case:

- **Buffer clean** → server re-reads, updates base mtime/hash, pushes
  `BufferText` to everyone. Claude edits the file, you watch it change.
- **Buffer dirty** → `BufferStale`; the tab shows a marker and save still runs
  the conflict check. Unsaved work is never overwritten by a background writer.

**Self-write suppression.** deadlight's own saves trigger watch events, which
would push `BufferText` straight back at the tab that just saved. After each
write the server records the path and resulting mtime/hash and drops the
matching event. Without this, every save echoes.

**Graceful degradation.** Linux caps inotify watches per user. On partial
registration failure the server logs it, sets `watch_degraded` in state, and
clients fall back to re-fetching on focus and after saves. Watching is an
optimization; correctness never depends on it.

## Rendering

Unchanged in principle: all content HTML is built in Rust and fetched from
`/frag/...`. What changes is that pane and tab *chrome* is client-rendered from
mirrored state — a few hundred lines of plain JS, no framework.

Only two content types are client-owned: the edit textarea, fed by
`BufferText`, and the terminal. htmx's role shrinks accordingly but does not
disappear.

## Security

Carried from v2 unchanged: bind `127.0.0.1` only, exposure exclusively via
`tailscale serve`, every path through the confinement check, `.deadlight/
theme.css` the only raw per-project file.

**Already shipped ahead of this work** (commit `b7f8a39`): WebSocket
handshakes require an `Origin` on an allowlist, and HTTP requests validate
`X-Forwarded-Host` then `Host` against the same list, closing a drive-by RCE
and DNS rebinding respectively. The allowlist comes from `DEADLIGHT_ORIGINS`
or global config only — never a project's `.deadlight/config.toml`, or a
cloned repo could allowlist itself.

New surface in v3:

- **Session names are the sharpest new input**, landing in a dtach socket path
  and a command line. `[A-Za-z0-9_-]{1,32}`, rejected otherwise. Without this
  the "+ terminal" button is a path-traversal and argument-injection vector.
- **Write intents barely move the threat model.** The socket already spawns a
  shell; anyone who can reach it can already destroy the tree. Reachability is
  the whole boundary, which is why the Origin check matters far more than
  anything about file writes.
- **State files hold buffer text**, hence file content. `0600` / `0700`.
- **dtach socket permissions** are what stop another local user attaching to
  your shell.
- **Resource caps**, since tabs are now user-multipliable: ≤16 terminal
  sessions per project, ≤50 open buffers, 1 MB scrollback per session. The
  existing 2 MB file cap now bounds writes as well as reads.

## Errors

- Fragment endpoints keep returning a `hint` div — the pane shows it, the page
  survives.
- Malformed or unknown socket intents produce an `Error` event, never a panic
  on a socket thread.
- Terminal socket drop shows the disconnected overlay until reconnect.
- Workspace socket drop disables mutation UI and retries with backoff; on
  reconnect the client re-requests full state rather than replaying intents.

## Testing

Three testability requirements shape the design rather than following it:
`DEADLIGHT_STATE_DIR` (or tests contaminate real state), configurable debounce
intervals set to zero in tests, and the pure watcher classifier.

**Unit:** state-machine transitions (move, identity-focus, active-index
adjustment on close, index bounds); persistence round-trip plus corrupt and
missing files; the parent-canonicalizing resolver (traversal, absolute,
symlink escape, `..` final component); session-name validation; conflict
detection and mode-preserving atomic write; protocol codec on malformed input.

**Integration**, on an ephemeral port: two workspace sockets and a broadcast
observed by the second; the echo rule (originator gets no `BufferText` back);
save writes the file and broadcasts; external modification live-updates a clean
buffer and marks a dirty one stale; two terminal clients both receive output
and the smaller geometry wins.

**Browser**, driven by pinchtab against a loopback instance: moving a terminal
tab between panes must keep `readyState === 1` and the same xterm instance. A
naive state-driven re-render passes every other test here and fails this one.

## Stack

Carried: Rust 2021, tungstenite 0.24, portable-pty 0.8, pulldown-cmark 0.13,
toml 0.8, serde 1, vendored htmx 2.0.4 / xterm 5.5.0 / highlight.js /
github-markdown-css. Dev: ureq 2, tempfile 3.

Added: `serde_json` 1, `notify` 8.2.0, `notify-debouncer-full` 0.7.0.
Runtime prerequisite: `dtach` (0.9; `brew install dtach`, `apt install dtach`)
on both the Mac and the deploy host.

Removed: zellij as a runtime dependency.

## Deployment

Unchanged from v2 except that `dtach` must be installed and the terminal
command default changes. The install trap documented in `HANDOFF.md` still
applies: the systemd unit runs `~/.local/bin/deadlight`, `target-dir` is
redirected to `~/.cache/cargo-target`, and `cargo build --release` alone
updates neither.

Migration is a restart. Existing zellij sessions are not adopted; they keep
running under zellij and can be attached from a shell until they are retired.

## Nice-to-haves (post-v3, only if asked)

Drag-and-drop tab reordering (v3 ships a "move to pane" command), images in
markdown preview, git log view, mobile layout, per-theme favicon, `retach` as
the session backend if its scrollback replay proves worth the immaturity.
