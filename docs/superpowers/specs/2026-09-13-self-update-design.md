# Updating roost from the button

*2026-09-13. Status: designed, not implemented. Issue
[#65](https://github.com/PeterKnego/roost/issues/65), step 4, and the
notification flow #65 describes as `[Update] / [Later] / [Skip]`. Depends on
step 2, the version check
(`2026-09-12-version-check-design.md`, designed, not implemented), and on
steps 1 and 3, merged as #77 and #78. The comparison that informed the shape
is `2026-09-13-upgrade-mechanisms-comparison.md`. Decisions from conversation
on 2026-09-13.*

## What and why

roost can say what it is running, how it was installed, whether it may
replace its own file, and — once step 2 lands — that a newer version exists.
What it cannot do is act on that. This step adds the act, for the two shapes
that are roost's own to act on, and the notification flow that offers it.

Four decisions were made before the design, and everything below follows from
them:

1. **An `[Update]` button in About**, not a CLI subcommand and not a staged
   install at next start.
2. **The full notification flow**: a header mark, an in-page dialog with
   `Update` / `Later` / `Skip`, and persisted choices.
3. **roost restarts itself by re-exec in place.** Same PID, no supervisor
   needed, works identically under a terminal, `nohup`, systemd, or a
   container.
4. **Artifacts are signed with a release key, Tauri-style**, and verified
   offline in roost before anything is unpacked.

Of the four update models compared, this is the in-process one Tauri uses,
with the ownership question answered the way roost already answers it: by
probing, and by offering only where the probe and the channel both say yes.

## Scope: who sees what

**The button applies to one condition**, the one `upgradesLabel` in
`static/dialog.js` already renders as "roost can replace this copy": channel
`release`, owner neither Homebrew nor a system package, write probe `Yes`.
That is the shell installer and the tarball. Every other shape keeps exactly
what #78 shipped — the command, and nothing that acts.

**The trigger is the version check's stored fact.** When the newest unyanked
version is newer than the running one, and is not the version the user
skipped, three surfaces light up:

- **A header mark**, quiet, beside the existing chips, on every project page.
  Clicking it opens the update dialog. Not on the overview page, which has no
  notification surface; the version-check spec excludes it for the same
  reason.
- **The update dialog**, an in-page `<dialog>` on the `dialog.js` primitive.
  Its title names both versions: *roost 0.5.3 is available — you are running
  0.5.2*. It opens by itself once per browser page load when the mark first
  appears, and on any later click of the mark.
- **The About row.** `0.5.3 available` gains an `[Update]` button for
  replaceable copies. About is where a user goes to wonder, so the action
  lives there too, not only in the dialog.

The dialog's buttons depend on the shape:

| Shape | Buttons |
|---|---|
| Replaceable (shell installer, tarball) | `Update` · `Later` · `Skip 0.5.3` |
| Homebrew, system package, `cargo install`, container | the upgrade command About already shows, as copyable text · `Later` · `Skip 0.5.3` |
| Checkout | **no dialog and no mark.** About already says "yours to rebuild", and a developer on a branch is behind a release most of the time by design |

### Out of scope

- Any change to the version check itself: its endpoint, intervals, or state
  file.
- The overview page (`/`).
- macOS notarization. A file roost fetches carries no quarantine attribute,
  which the #65 comment of 2026-09-12 proved is the actual gate.
- Rollback after a successful exec.
- Anything for Homebrew, the packages, `cargo install` or the container beyond
  the command they already get.
- A CLI `roost upgrade`. If one is wanted later it is the same pipeline with a
  different caller.

## Later, Skip, and where those choices live

Two choices, two lifetimes:

- **Later** defers. The dialog stops opening by itself for **24 hours**. The
  header mark stays, because the fact has not changed, and clicking it still
  opens the dialog. Chrome and Firefox both keep their indicator through a
  deferral; a mark that can be dismissed forever by accident is a mark nobody
  trusts.
- **Skip 0.5.3** is per version. Neither the mark nor the dialog appears for
  that version again. A later version un-skips automatically, because the
  stored value is the version string, not a flag.

Both are stored server-side, so every browser on this roost agrees, in a
second small file in the state directory beside the version-check file:

```json
{ "skipped": "0.5.3", "deferred_until": 1789320967 }
```

**A separate file, on purpose.** The version-check file is written by the
check thread when a fetch completes; this one by the intent handler when a
user clicks. One file with two writers is a rename race in which a completed
check silently discards a click. Written atomically — pid-unique temp file,
then `rename` — like every other piece of persistent evidence here. A file
that is missing, truncated or unreadable means *no choice was made*, which
errs toward showing the dialog, the recoverable direction.

**Two new websocket intents** carry the clicks: `DeferUpdate` and
`SkipUpdate { version }`. Both are global rather than per project, so the hub
records the choice and pushes a fresh settings snapshot process-wide with the
existing `broadcast_all`, and every open page drops its mark at once.
`SkipUpdate` carries the version so a click from a stale page cannot skip a
version it never saw: a mismatch is ignored and logged.

**The client learns all of it by the route the version check chose**: one
sibling field on `SettingsView`, beside `build`, holding the latest version,
the skipped version, and the deferral timestamp. `RequestState` already
invalidates that cache, and a new connection's first snapshot carries it, so
the mark renders on page load with no extra round trip.

**Reset is a fact in About, not a control.** The row reads `0.5.3 skipped`
where it would read `0.5.3 available`; deleting the choices file is the reset.
A skip is a considered choice, and un-skipping is rare enough that a button is
not worth its surface.

## The pipeline: from click to swapped file

A new module, `src/update.rs`, owns it.

**Under the hub lock, three checks and a hand-off.** The `Update` intent
requires: the copy is replaceable by the rule above; the stored latest
version is newer than `CARGO_PKG_VERSION` by the version check's comparator;
and a process-wide in-flight guard is free. A second click anywhere while one
runs is answered *already updating* rather than starting a second download.
Then a detached thread runs the steps below with **no lock held**, reporting
each phase to the requesting connection with `send_to` — the primitive whose
privacy the two-subscriber tests already cover. Other clients learn the
result the way everyone does: their socket drops and comes back on the new
version.

1. **Capture the executable path** with `std::env::current_exe()` before
   anything else. On Linux that reads `/proc/self/exe`, which after step 6
   names the deleted old inode with ` (deleted)` appended; an exec from it
   would fail. Read once, at the start.
2. **Fetch two files** from `<repository>/releases/download/v<latest>/`:
   `roost-<target>.tar.xz` and `roost-<target>.tar.xz.minisig`. The target is
   the one `build.rs` baked into `BuildInfo`, so an aarch64 roost never
   installs an x86_64 file. The URL is derived from the repository already in
   `BuildInfo` and the version from the check — **not configurable**, for the
   version-check spec's reason: a setting that let a repo name the host roost
   fetches a binary from would be the hole that spec exists to avoid. The
   same `ureq` agent as the check, ten-second timeout per request, and a size
   cap of 64 MB checked against `Content-Length` before the body is buffered:
   the tarball is a few megabytes, and a server that answers with gigabytes
   is one to walk away from.
3. **Verify the signature** over the tarball bytes with `minisign-verify`
   against the public key compiled in. Before the archive is opened: nothing
   unsigned is parsed.
4. **Unpack the one member** named `roost` from the `.tar.xz` into a temp
   file in the executable's own directory, `.roost-update.<pid>`, mode 0755.
   The same directory because the swap is a rename, and a rename is atomic
   only within one filesystem. A member that is a symlink, a directory, or
   absent, or an archive with two, is refused. `xz` decompression and tar
   reading are two small crates; the release format is dist's and is not
   changing for this.
5. **Prove it runs.** Execute the temp file with `--version` under a
   five-second timeout and require stdout to be exactly `roost <latest>`.
   Positive evidence that the file is a working roost of the version claimed.
   The timeout is the macOS lesson from the #65 comment: a hang is a fourth
   outcome that exit status cannot see. A file that hangs, fails, or reports
   another version is deleted, and nothing is swapped.
6. **Swap** with a single `rename` of the temp file over the captured path.
   The running process keeps its old inode until it execs.

Every step before 6 fails safe by construction: the executable is untouched
and the temp file is removed. Step 6 is the only irreversible one and is a
single syscall.

## The restart

After the swap the thread does three things in order, then execs.

1. **Persist what only lives in memory.** Per-project workspace state is
   saved on every change through the hub's `persist`, and the notice store
   persists on every write, so both are current. What is *not* established is
   whether unsaved buffer text is included in that save — the README promises
   unsaved buffers survive a restart, and this spec makes it the plan's first
   task to read `wsstate::save` and confirm it. If it is not, that is a
   prerequisite fix, not something to paper over here.
2. **Tell the requester.** A final phase message, `restarting`, so the dialog
   reads *restarting roost* rather than *reconnecting*, and a short pause so
   the frame leaves the socket. The dialog then behaves like the reconnect
   banner: waits for the control socket to return, then reloads the page,
   which is how every client learns the new version.
3. **Exec.** `std::os::unix::process::CommandExt::exec` on the captured path
   with this process's `args_os()`. The environment is inherited, which
   carries `ROOST_ROOTS`, `ROOST_STATE_DIR`, `ROOST_BIND_ALL` and the port
   argument unchanged. `exec` keeps the PID: systemd sees nothing,
   `KillMode=process` is irrelevant, and the dtach masters remain children of
   the same process. A hand-run roost keeps its terminal. If `exec` returns —
   which it does only on failure — the thread records it through `errlog`,
   clears the in-flight guard, and the process keeps serving on the old
   binary with the new file already in place. About then reads `restart
   roost to run 0.5.3`: the one state where the file and the display
   disagree, reported rather than hidden.

**The listening socket needs no hand-off.** Rust opens sockets close-on-exec,
so it closes at the exec and the new process binds the same port a few
milliseconds later. Browsers are already built for this: the control socket
retries with capped backoff, and `tests/browser/reconnect.mjs` pins that
terminals heal too.

**What the exec does not preserve, stated plainly:** an in-flight upload or
paste on the old process is dropped, and a websocket intent sent in the gap
is lost. The client's existing *changes are not being sent until this clears*
banner already covers the gap.

## Signing, on both ends

**Release side.** A new reusable workflow,
`.github/workflows/sign-release-artifacts.yml`, runs in the global phase next
to `build-linux-packages` — the first point at which all four tarballs exist.
It downloads the `artifacts-*` bundles, signs every `roost-*.tar.xz` with
`minisign -S`, and uploads the `.minisig` files as one more artifact bundle,
which dist attaches to the release like the packages. The job declares
`environment: release`, for the reason the crates.io job explains in its own
comments: the secret must be unreachable from a pull request branch's copy of
the workflow. It is listed in `global-artifacts-jobs` beside the packages
job.

**The key.** Generated once with `minisign -G`, offline, by the maintainer.
The secret key goes into the `release` environment as `ROOST_MINISIGN_KEY`,
its password as a second secret. The public key is committed as
`keys/roost.pub` and compiled in with `include_str!`, exactly as Tauri bakes
its `pubkey`. It is one line of base64 plus a key ID, so rotation is a code
change and a release: a binary trusts only the key it was built with.

**What losing the key costs, written here so it is not discovered later.**
Existing installs verify only against the committed key. If the secret key is
lost, every roost in the field refuses future updates, and those users run
the shell installer once. Tauri documents the same failure; the mitigation is
the same: keep the key somewhere that survives this machine. No key escrow
inside roost.

**Client side.** `minisign-verify`, zero dependencies, is the crate Tauri's
updater itself uses. The signature covers the tarball bytes, so verification
runs on the buffered download before decompression touches it. A signature
that does not verify is reported as exactly that — *signature did not verify*
— distinct from a download that failed or a `.minisig` that was missing,
because the first is the one someone should hear about.

**Two things this does not replace.** The sha256 files and the SLSA
attestation stay, for humans and `gh attestation verify`. And nothing here
notarizes for macOS.

**The honest gap.** No signing job runs on a pull request, and
`publish_prereleases` is unset, so the first end-to-end run of fetch, verify,
swap and exec against a real signed artifact is **the first tagged release
after this merges**. *Testing* below says what a green suite proves short of
that.

## Outcomes, and what the user reads

The pipeline's result is three-way, in the discipline `install.rs` and the
version check already follow:

```rust
enum Outcome {
    Restarting,             // swap done, exec about to happen
    Refused(Reason),        // never started: not replaceable, nothing newer, already running
    Failed(Phase, String),  // started, stopped, executable untouched
}
```

`Failed` names the phase and carries the specific message: download failed
with the status; signature did not verify; archive had no `roost` member; new
binary did not answer `--version` in time; new binary reported a different
version; rename refused with the OS error. The dialog shows that line and a
`Retry` button. The About row shows the last failure too, `update failed:
<phase>`, until the next attempt or the next check, so a failure survives the
dialog being closed. It is not persisted across a restart: an exec is the
success case, and a crash is a different problem.

`Refused` is what a second click during a run gets, and what a stale page
gets when its stored latest is no longer newer. The dialog reads *already
updating*, or simply closes.

**The in-flight guard clears on every path except `Restarting`**, where the
process is about to be replaced. If exec fails after the swap, the guard
clears and the row says `restart roost to run 0.5.3`.

**Deliberately absent:** retry with backoff, resumed partial downloads,
rollback after exec. A failed exec leaves a newer file and a running older
process, and the answer is a restart, which the row says.

**Logging.** Every phase transition and every failure goes through
`errlog::record`, the same file the roots-conflict warning uses, so the
server log holds the whole timeline after the browser has reloaded.

## Testing

The pipeline takes its fetch as an injected function, like
`registry::reconcile_with` and the version check, so every phase runs in
`cargo test` with no network and no real release.

**Unit, in `update.rs`:**

- The URL builder as a table: repository, version, target in; two exact URLs
  out.
- Signature verification with a **test keypair committed under
  `tests/fixtures/`**: a valid signature passes; a byte flipped in the tarball
  fails; a signature from another key fails; each failure carries its own
  message. This is the test to watch failing with verification deleted.
- Unpacking: an archive with the member, one without, one with two, one whose
  member is a symlink, one whose member is a directory — the last four
  refused, each with its message.
- The `--version` probe against three fake binaries **built as real
  executables** in a tempdir (a fake claude had to be a real binary for the
  same reason): one answers correctly, one answers the wrong version, one
  sleeps past the timeout. The timeout case is *timed*: a hang passes by never
  finishing, so the assertion is that the probe returned within a bound.
- The swap: a tempdir with a fake executable; after the pipeline the file at
  the captured path has the new bytes and no `.roost-update.*` remains. Then
  every earlier failure, asserting the original bytes are byte-identical
  afterwards and the temp file is gone.
- Single-flight: a fetch that blocks, two intents, exactly one fetch call, the
  second answered `Refused`.
- The choices file: round trip; skip suppresses that version and not a newer
  one; deferral expires at the timestamp; a `deferred_until` more than 24
  hours in the future reads as expired (a stepped clock); a truncated file
  reads as no choice.

**Integration, over a websocket:** the intents are refused for a
non-replaceable copy and when nothing is newer, and the refusal reaches only
the sender — with a second subscriber that must receive nothing, the
two-subscriber shape CLAUDE.md asks for.

**Exec is not unit-tested.** It is a process replacement; a test that execs
replaces the test runner. It is covered by **one manual run, recorded in this
spec before shipping**: a roost from `scripts/testroost.sh`, a locally built
tarball signed with the test key and served from a local HTTP server that an
injected base URL points at for that run only, the button clicked in a real
browser, and the page coming back reporting the new version with a dtach
shell opened before the click still alive.

**Browser, `tests/browser/update.mjs`:** the mark appears when the harness
plants a version-check state file naming a newer version; the dialog opens
once per load; Later hides it and keeps the mark; Skip removes both, and a
newer planted version brings them back; the About row renders `available`,
`skipped`, and `update failed`. Each with a re-render check, since the
settings dialog reads by reference on tab switch and from the server on
reopen (#78's technique).

Each new test is watched failing with its branch reverted before it is
trusted — CLAUDE.md's *Testing* section says why.

### What a green suite cannot see

- **The real signing job and the real key.** They run first on a real tag.
  The first release after merging *is* the test, and whoever cuts it clicks
  `Update` on a shell-installer copy before announcing.
- **A real GitHub download**, including redirects from
  `releases/download/` to the CDN, which `ureq` follows by default.
- **The exec under systemd** specifically. The manual run above is a
  terminal roost; a second run on the deploy host under the unit is worth
  doing once, checking `systemctl status` still reports the same main PID
  afterwards.

## Decided in conversation (2026-09-13)

1. Button in About, not a `roost upgrade` subcommand — the subcommand remains
   available later as the same pipeline with a different caller.
2. Full notification flow in this step, not About-only.
3. Re-exec in place, not supervisor restart and not staged-at-next-start.
4. A release signing key verified offline, not sha256-only and not
   `gh attestation verify` at update time.
5. Approach A: roost fetches, verifies, unpacks, probes, swaps and execs
   itself. Delegating to the shell installer was rejected for the `curl` PATH
   dependency, sha256-only verification, always writing to `~/.cargo/bin`,
   and piping a remote script into a shell from the process that holds the
   terminal sockets.
