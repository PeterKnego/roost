# resh — an open file has a buffer; only an edited one has text

Three separate complaints turn out to be one design decision. A buffer today
is created only by Edit mode, and the moment it exists it holds the file's
whole text. That single fact is why a preview never notices a file changing
underneath it, why opening a `.env` writes its contents into the state file,
and why the 50-buffer cap is about to become a limit on *browsing* rather than
on editing.

The fix is to separate what a buffer is *for* from what it *holds*. Every open
file tab gets a buffer. Only an edited one gets text.

## The problem, measured

**A previewed file does not follow the disk.** `watch.rs:377` hands `classify`
the list of open *buffers*:

    let open: Vec<String> = h.ws.buffers.keys().cloned().collect();

A file open in Preview has no buffer, so `classify` returns `Class::Tree` and
the browser is told only that "something in the tree changed". The tree pane
refreshes; the preview content is never re-fetched. `file_changed_externally`
could not help even if it were called, because it leaves before it broadcasts:

    let Some(b) = self.ws.buffers.get_mut(rel) else { return true };
    …
    self.broadcast(&Event::FileChanged { rel: rel.to_string() });

And the one client that does listen for `FileChanged` acts only on diffs —
`app.js:172` is `case "FileChanged": refreshKind("Diff")`. So a previewed file
has no invalidation path at any of the three layers.

**Opening a file writes its contents to disk.** `hub.rs` already says so, in
the comment explaining why closing a tab frees its buffer: without it "every
buffer a user opens accumulates (up to MAX_BUFFERS) and is persisted with its
full text to disk forever, including secrets like a .env opened once in Edit".
That is the current behaviour for anything opened in Edit, whether or not a
key was ever pressed in it.

**The cap counts the wrong thing.** `MAX_BUFFERS` is 50 and `MAX_TEXT_BYTES`
is 2 MB, so the ceiling is 100 MB of text held in memory and re-serialised into
the state file. It is enforced at buffer *creation* (`hub.rs:475`), which today
means "50 files open in Edit". Under the edit-by-default change we want next,
it becomes "50 open tabs" — reachable by clicking around a repo, with the 51st
file presenting an empty editor over a non-empty file.

## What a buffer is actually for

    pub struct Buffer { text: String, base_mtime: Option<SystemTime>,
                        base_hash: u64, dirty: bool, stale: bool }

Two different things live in there, and they have opposite lifetimes.

`text` is *reconstructible*. While a buffer is clean it is by definition equal
to what is on disk, and re-reading the file produces it exactly.

`base_hash` and `base_mtime` are *not*. They record what the editor's content
was based on at the moment it was opened, which is the whole basis for
detecting that someone else changed the file since. They can only be captured
at open time; deriving them later — at the first keystroke, say — sets the base
to whatever is on disk *then* and silently swallows the very change they exist
to catch. resh has already shipped that defect once, from the client-side
`/frag/raw` flow: CLAUDE.md's dev/prod table records it as "no browser: saving
was completely broken (`base_hash` never initialised, so every save
conflicted)".

So the lazy design that works is not "no buffer until edited". It is **keep the
base, drop the text**.

## What changes

**A buffer is created when a file tab opens, in either mode**, by the same disk
read `open_for_edit` does now: hash the contents, record the mtime, keep both,
discard the text. Call this a *stub*. It costs a `u64`, an `Option<SystemTime>`
and two bools — call it ~100 bytes against the 2 MB it replaces.

**Text appears when the content actually differs, and not before.** Not "on the
first keystroke": the client sends `EditBuffer` from its `input` listener, and
`input` fires only when the textarea's *value* changes. Arrow keys, Home/End,
PageUp, Ctrl+A, clicking, scrolling and focus do not fire it, so navigating a
file never materialises anything.

That covers one of the two senders. The other is `pushEdit` (`app.js:1137`),
which sends unconditionally and is called by `saveNow` before every save — so
⌘S on a file you had only looked at would push its whole text, mark it dirty
and write it back to disk identically. Typing a character and deleting it again
has the same shape: two `input` events, and the second leaves text equal to the
file.

Both are fixed in one place, and deliberately not in the client: **the server
decides dirtiness by hashing the incoming text against `base_hash`.** Equal
means the buffer stays `Clean` and keeps no text, whatever the client sent;
different means it becomes `Edited`. `hash_text` is already computed on every
save, so this costs one hash per edit and makes every path idempotent — ⌘S on
an untouched file becomes a no-op, an undone edit collapses back to clean, and
no client-side discipline is load-bearing.

That in turn decides the type. Not `text: Option<String>` beside a `dirty`
bool, which leaves `dirty: true, text: None` expressible — a state whose only
possible reading at `do_save` is "write an empty file over the user's work".
An enum makes it unrepresentable:

    enum Content { Clean, Edited(String) }

`dirty` becomes the discriminant rather than a flag anyone can set. `stale`
stays a separate bool and only ever applies to `Edited`, which is already true
today — `file_changed_externally` sets it in the dirty branch only.

**The watcher sees every open file, because every open file now has a buffer.**
`classify`'s existing `open_buffers` list grows to include previewed files with
no change to `classify` itself — the stub is what puts them there. This is the
part that makes the preview bug disappear as a consequence rather than as a
second fix.

**`file_changed_externally` stops requiring text to do its job.** Its three
outcomes become:

| Buffer state | On an external change |
|---|---|
| dirty | mark `stale`, broadcast `BufferStale` — unchanged, unsaved work is never overwritten |
| clean, editor open | re-read, update `base_hash`/`base_mtime`, broadcast `BufferText` so the editor follows the file — as today, minus the retained copy |
| clean, preview open | re-read the *hash* only, update the base, broadcast `FileChanged { rel }` |

**The client acts on `FileChanged` for File tabs, matched by `rel`.** Today's
`refreshKind("Diff")` refreshes by kind alone; a File tab must be re-mounted
only when the file that changed is the one it shows.

**The cap bounds text, not tabs.** `MAX_BUFFERS` becomes a limit on buffers
holding text — i.e. on files with unsaved changes — which is a number you reach
by editing 50 files without saving any of them, not by browsing. Stubs are
bounded by the open tab count and cost ~100 bytes each; that is a deliberate
decision to not add a second cap for something four orders of magnitude
cheaper.

**Persistence follows the same line.** A stub persists as its base and flags,
with no text. A dirty buffer persists with its text, because unsaved work has
to survive a restart — that is what `wsstate`'s "unsaved text is crash-safe"
test pins. A `.env` opened and not edited therefore leaves nothing behind.

## Every read of `b.text`, defined

The change is only safe if each existing reader has an answer for a stub. There
are seven.

| Site | Today | With stubs |
|---|---|---|
| `hub.rs:114` `reconcile_buffers_with_disk` | re-reads every buffer at startup and compares | only dirty buffers need comparing; a stub's base is re-derived from the file it names |
| `hub.rs:487` `open_for_edit` fill | reads the file into `text` | reads it to establish the base; text is kept only if the buffer is already dirty |
| `hub.rs:499` push to the opening client | sends `b.text` | sends the text of *this* read, without retaining it |
| `hub.rs:529` clean-buffer follow | overwrites `text` from disk | updates the base; sends the text it just read |
| `hub.rs:636` `do_save` | writes `buf.text` to disk | only ever runs on a dirty buffer, which has text — but must refuse rather than write an empty string if it does not |
| `hub.rs:654` conflict diff | diffs disk against `buf.text` | same: dirty only |
| `wsconn.rs:91` a connecting client | is sent every buffer's text | is sent text for dirty buffers, and reads from disk for the stubs whose tabs are open |

The `do_save` row is the one with teeth. A save that finds no text must be an
error, never a write — the truncating-save failure this codebase already
guards against for images (`workspace.rs:265`, "SaveBuffer writes from
w.buffers, so if no buffer can ever exist for an image, the truncating save
this task exists to prevent is structurally impossible").

## When the file cannot be read

Every path above now re-reads on demand, which multiplies the number of places
a read can fail — and this is the codebase's most-repeated defect class.
CLAUDE.md: "'I could not determine X' is a third outcome, never folded into 'X
is false'."

- A read that fails must never produce an empty buffer, an empty editor, or an
  empty save. It leaves the last known base in place and reports.
- A file *deleted* while open is distinguishable from one that is merely
  unreadable, via `symlink_metadata` and matching `Err(NotFound)` against
  `Err(_)`, and only the first should close or blank anything.
- `file_changed_externally` already returns `false` for an unreadable file and
  documents that callers must treat it as a tree change; that contract holds.

## Testing

Rust, in `hub.rs`/`workspace.rs`/`wsstate.rs`:

- A file opened in Preview produces a buffer whose text is `None`, and one
  opened in Edit does too until a key is pressed.
- A change on disk to a previewed file broadcasts `FileChanged` — the assertion
  that fails today at the server layer.
- The state file for a clean open buffer contains no file content. Written as a
  literal search for the file's text in the serialised bytes, so it fails if
  the text is stored anywhere in it under any key.
- A dirty buffer still round-trips its text through a restart (the existing
  test must keep passing unchanged).
- A save against a text-less buffer errors and leaves the file byte-identical.
- ⌘S on a file that was opened and never edited leaves it `Clean`, holds no
  text, and does not write — asserted on the file's mtime, not just its bytes,
  since an identical rewrite is still a write.
- An `EditBuffer` whose text hashes equal to `base_hash` leaves the buffer
  `Clean`, so typing a character and deleting it returns to text-less.
- The cap: 50 *dirty* buffers is refused; 50 open clean tabs is not.

Browser, since none of the propagation is visible to `cargo test`:

- A file open in Preview, changed on disk by the test, updates in the page
  without a reload. This is the reported bug and it must fail before the fix.
- A file open in Edit and untouched still follows the disk live.
- A file open in Edit *with* unsaved changes still shows the conflict banner
  rather than adopting the change — the property `autosave.mjs` already covers,
  which must not regress.
- Navigating an open editor — arrows, Home/End, PageUp, Ctrl+A, a click — leaves
  the server holding no text for it. This is the one assertion that has to be
  driven by real key events rather than by `send()`, because what it is really
  testing is which browser event the client listens on.

## Risks

**More reads.** Every attach, every external change and every reconnect now
re-reads from disk what was previously in memory. These are small files behind
a 2 MB cap on a local disk, and the alternative is holding 100 MB; but a
pathological writer (Claude rewriting a watched file in a loop) turns into one
read per debounce tick per open tab. The existing `RESH_DEBOUNCE_MS` and the
`.git` special case in `classify` are the levers if that bites.

**A wider `Class::Buffer` set.** Every open tab now routes its file to the
per-file path instead of the generic tree path. That is the point, but it moves
work from a coalesced `TreeChanged` to per-file handling, and `classify`'s
existing tests should be extended rather than trusted.

**`Option<String>` touches every buffer site.** The type change is what makes
the invalid state unrepresentable, and it is also the reason this is a spec and
not a patch: the compiler will find the seven read sites above, but not the
semantics of what each should do.

## Out of scope

Edit-by-default for text files, which is what prompted this. It stays a
separate change and should land *after* this one, because most of its
objections — the state file filling with every file you click, the cap turning
into a browsing limit — are consequences of buffers holding text, and stop
existing here. PDF rendering is also out of scope; `pdf` belongs on
`NO_TEXT_EDIT_EXT` so a binary never reaches a textarea, which is a one-line
change independent of all of this.

## As built

**`base_mtime` was dropped.** The prose above describes a base of `base_hash`
*and* `base_mtime`; the field survived the rewrite as a write-only remnant —
set in three places, read in none. Deleting it also removed the only
unconfined `dir.join(rel)` on the buffer path (`open_buffer_for` already held
the `safe_resolve`d path) and the asymmetry between the startup and live clean
paths, which set it from different sources. The base is `base_hash` alone: a
hash answers "did the file move under this buffer" without trusting a
timestamp that a cloned repo, a restore, or a coarse filesystem clock can make
agree by accident.
