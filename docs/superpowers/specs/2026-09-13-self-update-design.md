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
  appears, and on any later click of the mark. Opened by itself it focuses
  `Later`, never `Update`: it can land while the user is typing in a
  terminal, and Enter or Space must not start an update. Opened by a click,
  it focuses `Update`.
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
clears and the row says `restart roost to run 0.5.3`. The dialog says the
same and offers no `Update`: the file is already in place, and a second run
would only download it again.

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

### The manual run, 2026-09-29

Run on the Linux deploy host, against a **scratch** roost only — the live
instance, `~/.cargo/bin/roost` and the systemd unit were not touched. `$S`
below abbreviates a per-session scratch directory on that host.

**Setup.**

- minisign: the official static `minisign-0.11-linux.tar.gz` release (the
  version the release workflow pins), `minisign -v` → `minisign 0.11`. Two
  keypairs, `test` (key id `EE6981AB76E996AA`) and `other`
  (`3BDF52AC0BDF7362`), both `-G -W`.
- The old binary: this branch at `f2ca33b` with `test.pub` copied to
  `keys/roost.pub`, `GITHUB_REF_TYPE=tag cargo build` → `roost 0.6.0`,
  channel `release`, copied to `$S/bin/roost`. Its About read
  `target x86_64-unknown-linux-gnu`, owner `other`, *Upgrades* "roost can
  replace this copy".
- The new binary: `Cargo.toml` version set to `9.9.9`, plain `cargo build`,
  packed the way dist packs it (`roost-x86_64-unknown-linux-gnu/roost` in
  `roost-x86_64-unknown-linux-gnu.tar.xz`) and signed with
  `minisign -S -s test.key` (the CLI verified its own signature with
  `-Vm … -p test.pub` before serving). The same tarball bytes were signed a
  second time with `other.key` for the failure path.
- `Cargo.toml`/`Cargo.lock` restored from `cp` backups and `keys/roost.pub`
  deleted afterwards; the tree was clean again before the run started.
- The scratch instance: `ROOST_STATE_DIR=$S/state ROOST_ROOTS=$S/roots
  ROOST_CONFIG=$S/config.toml` (`version_check = true`)
  `ROOST_UPDATE_BASE=http://127.0.0.1:8999 $S/bin/roost 8446`, with a fresh
  `check.json` naming `9.9.9` planted first, and
  `python3 -m http.server 8999 --bind 127.0.0.1` serving the tarball.
- The browser: headless Chromium through `tests/browser/harness.mjs`
  (`startBrowser`/`openPage`/`evalIn`), from a scratch Deno script. The
  terminal was opened with `NewTerminal`/`StartTerminal` as `notices.mjs`
  does and `echo alive-$$` typed into it; the dialog was dismissed once so
  About could be read, reopened by clicking the mark, and **`Update` was
  clicked in the DOM** (`.dlg-ok.click()`). Every `UpdateProgress` frame the
  dialog received was appended to `sessionStorage` by a wrapper around the
  dialog's own `onProgress`, so the list survives the reload. The About
  values below are each row's `textContent`, which includes the row's button
  label — hence `9.9.9 availableUpdate` and `…)Retry`.

**Success path.** Every expectation held:

- The mark read `↑ 9.9.9` and the dialog opened itself with an `Update`
  button.
- The page saw, in order, `download…`, `verify…`, `unpack…`, `probe…`,
  `swap…`, `restarting roost…`, then reloaded by itself ~3 s after the click
  and reported `version 9.9.9`; the mark and the dialog were gone.
- The roost PID was the same before and after (`1900561`, same start time,
  `/proc/<pid>/exe` now the swapped file), `$S/bin` held one `roost` and no
  `.roost-update.*`, its sha256 equals the packed 9.9.9 binary's, and
  `$S/bin/roost --version` printed `roost 9.9.9`.
- The dtach master (`1901356`) and its shell (`1901358`) were the same
  processes before and after, and after the reload the terminal re-attached
  by itself and the shell answered `echo still-$$` with `still-1901358`.
- `error.log` holds the whole timeline, ending in `swapped … for 9.9.9; exec`.

Three observations, none a defect:

- **The pre-exec scrollback is not replayed.** The `alive-1901358` line was
  not on screen after the reload; the shell was, and answered. The replay
  ring lives in the process's memory, so an exec loses it exactly as any
  roost restart does (`screen.rs`: "after a restart the ring is empty"). The
  plan's wording ("the terminal tab still shows `alive-<pid>`") promised
  more than this spec does; the spec's claim — the shell *still alive* —
  holds.
- The new version reported channel `checkout` and commit `f2ca33bda-dirty`,
  because the 9.9.9 binary was built without `GITHUB_REF_TYPE` from a tree
  with an edited `Cargo.toml`, as the plan's recipe does. A real release is
  channel `release`.
- The swapped file is mode `0755`; the file it replaced was `0775`. The
  swap sets its own mode rather than copying the old one.

**Failure path.** The old binary copied back, the scratch roost restarted on
it, the `other`-signed tarball served, `Update` clicked:

- Frames `download…`, `verify…`, then `update failed: verify: signature did
  not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)`
  — the plan's expected text plus the URL suffix that `update::located`
  appends by design — and the button became `Retry`.
- About's *Latest* row read `9.9.9 available (update failed: verify:
  signature did not verify (<url>))` with a `Retry` button; the version
  stayed `0.6.0`.
- The binary was untouched: sha256 `9b55a3a1…1d109` and size before and
  after, `roost 0.6.0`. The shell survived this too (`still-1901358`).

**Under systemd: not done here, and owed.** The exec under the unit
(`systemctl --user show roost -p MainPID --value` the same before and after,
`pgrep -c dtach` unchanged) needs a release carrying the real key and a real
signature. It belongs to the first release that ships the real
`keys/roost.pub`, whoever cuts it, before it is announced.

**The suite afterwards,** on the same host after a normal `cargo build`
(no key, `roost 0.6.0`): `cargo test --lib -- --test-threads=1` 1098 passed
in 47.8 s wall; `cargo test --test '*' -- --test-threads=1` 82 passed across
11 binaries in 30.0 s wall; `update.mjs`, `version.mjs`, `about.mjs` and
`settings.mjs` all PASS.

Transcript, success path:

```text
== before
2026-09-29T13:27:57+00:00
    PID                  STARTED CMD
1900561 Tue Sep 29 13:26:37 2026 $S/bin/roost 8446
total 41572
drwxrwxr-x 2 claude claude     4096 Sep 29 13:25 .
drwxrwxr-x 9 claude claude     4096 Sep 29 13:27 ..
-rwxrwxr-x 1 claude claude 42804440 Sep 29 13:25 roost
9b55a3a1fee3b1a174621ebec936821370857bc35bff3c67ae769746f31d1109  $S/bin/roost
roost 0.6.0
ls: cannot access '$S/state/error.log': No such file or directory
[13:28:02.587] chromium /snap/bin/chromium cdp 35053
[13:28:02.845] page ready: true
[13:28:02.847] BuildInfo: {"version":"0.6.0","target":"x86_64-unknown-linux-gnu","channel":"release","owner":"other","replaceable":"yes"}
[13:28:02.847] update view: {"status":"newer","latest":"9.9.9","offer":true,"skipped":"","deferred_until":0,"failure":"","installed":""}
[13:28:02.847] mark: "↑ 9.9.9"
[13:28:02.847] dialog opened itself: true
[13:28:02.847] dialog title: "roost 9.9.9 is available — you are running 0.6.0"
[13:28:02.848] dialog button: "Update"
[13:28:03.590] About before: {"Version":"0.6.0","Latest":"9.9.9 availableUpdate","Commit":"f2ca33bda","Built":"9/29/2026, 1:25:46 PM","Repository":"GitHub","Installed":"the release tarball","Upgrades":"roost can replace this copy","Upgrade":"curl -LO https://github.com/PeterKnego/roost/releases/latest/download/roost-x86_64-unknown-linux-gnu.tar.xz"}
[13:28:04.096] terminal socket: true
[13:28:04.200] prompt: true
[13:28:04.308] terminal session term printed: alive-1901358
[13:28:04.344] ps before:
    PID    PPID CMD
1901358 1901356 /bin/bash -l
    PID CMD
1901356 /usr/bin/dtach -A $S/state/sock/scratch/term -E -r winch -z /bin/bash -l

[13:28:04.352] reopened via mark: true
[13:28:04.352] dialog button: "Update"
[13:28:04.352] clicking Update
[13:28:07.443] page reloaded: true
[13:28:07.444]   progress frame: {"t":1790688484354,"phase":"download","detail":"","shown":"download…"}
[13:28:07.444]   progress frame: {"t":1790688484365,"phase":"verify","detail":"","shown":"verify…"}
[13:28:07.444]   progress frame: {"t":1790688484490,"phase":"unpack","detail":"","shown":"unpack…"}
[13:28:07.444]   progress frame: {"t":1790688486501,"phase":"probe","detail":"","shown":"probe…"}
[13:28:07.444]   progress frame: {"t":1790688486528,"phase":"swap","detail":"","shown":"swap…"}
[13:28:07.444]   progress frame: {"t":1790688486528,"phase":"restarting","detail":"9.9.9","shown":"restarting roost…"}
[13:28:07.444] BuildInfo after: {"version":"9.9.9","target":"x86_64-unknown-linux-gnu","channel":"checkout"}
[13:28:07.444] update view after: {"status":"up-to-date","latest":"9.9.9","offer":false,"skipped":"","deferred_until":0,"failure":"","installed":""}
[13:28:07.444] mark after: "(hidden)"
[13:28:07.444] update dialog open after: false
[13:28:08.162] About after: {"Version":"9.9.9","Latest":"up to date","Commit":"f2ca33bda-dirty","Built":"9/29/2026, 1:25:54 PM","Repository":"GitHub","Installed":"built from a checkout","Upgrades":"yours to rebuild"}
[13:28:08.163] terminal re-attached without StartTerminal: true
    (timed out waiting for alive in replay)
[13:28:18.492] replay still shows alive line: false
[13:28:18.597] shell answered: still-1901358 (same pid as alive: true)
[13:28:18.627] ps after:
    PID    PPID CMD
1901358 1901356 /bin/bash -l
    PID CMD
1901356 /usr/bin/dtach -A $S/state/sock/scratch/term -E -r winch -z /bin/bash -l

== after
2026-09-29T13:28:48+00:00
    PID                  STARTED CMD
1900561 Tue Sep 29 13:26:37 2026 $S/bin/roost 8446
$S/bin/roost
total 41812
drwxrwxr-x  2 claude claude     4096 Sep 29 13:28 .
drwxrwxr-x 10 claude claude     4096 Sep 29 13:28 ..
-rwxr-xr-x  1 claude claude 42803224 Sep 29 13:28 roost
5594f8968c5d2aeb4cd3ae641d946ab5e1281018da1c0e95b69e8b96ce1cc11b  $S/bin/roost
5594f8968c5d2aeb4cd3ae641d946ab5e1281018da1c0e95b69e8b96ce1cc11b  $S/roost-new
roost 9.9.9
-- error.log
1790688484 update: starting: http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz -> $S/bin/roost
1790688484 update: download
1790688484 update: verify
1790688484 update: unpack
1790688486 update: probe
1790688486 update: swap
1790688486 update: swapped $S/bin/roost for 9.9.9; exec
-- roost stdout/stderr
roost listening on http://127.0.0.1:8446
roost: startup reap — 1 stale ide lock file(s) from a previous run
roost listening on http://127.0.0.1:8446
-- http.log
127.0.0.1 - - [29/Sep/2026 13:28:04] "GET /roost-x86_64-unknown-linux-gnu.tar.xz HTTP/1.1" 200 -
127.0.0.1 - - [29/Sep/2026 13:28:04] "GET /roost-x86_64-unknown-linux-gnu.tar.xz.minisig HTTP/1.1" 200 -
LISTEN 0      128                      127.0.0.1:8446       0.0.0.0:*    users:(("roost",pid=1900561,fd=3))          
```

Transcript, failure path:

```text
== before failure run
total 41572
drwxrwxr-x  2 claude claude     4096 Sep 29 13:28 .
drwxrwxr-x 10 claude claude     4096 Sep 29 13:29 ..
-rwxrwxr-x  1 claude claude 42804440 Sep 29 13:29 roost
9b55a3a1fee3b1a174621ebec936821370857bc35bff3c67ae769746f31d1109  $S/bin/roost
roost 0.6.0
b0729de758335231b43c7e321364b11a5515fe6484a14e19c77d7cbcc2e80e43  $S/serve-bad/roost-x86_64-unknown-linux-gnu.tar.xz
b0729de758335231b43c7e321364b11a5515fe6484a14e19c77d7cbcc2e80e43  $S/serve/roost-x86_64-unknown-linux-gnu.tar.xz
untrusted comment: signature from minisign secret key
RURic98LrFLfO2B1H6B6VTHfaIiEbpuC+5xD7//wACxh98ERxsg9UrH/z0LgFEPvu/YwseB6qwG9kZeIyFkRxiBDOjMPmFqtAwQ=
[13:29:13.340] chromium /snap/bin/chromium cdp 37569
[13:29:13.516] page ready: true
[13:29:13.531] BuildInfo: {"version":"0.6.0","target":"x86_64-unknown-linux-gnu","channel":"release","owner":"other","replaceable":"yes"}
[13:29:13.531] update view: {"status":"newer","latest":"9.9.9","offer":true,"skipped":"","deferred_until":0,"failure":"","installed":""}
[13:29:13.533] mark: "↑ 9.9.9"
[13:29:13.533] dialog opened itself: true
[13:29:13.533] dialog title: "roost 9.9.9 is available — you are running 0.6.0"
[13:29:13.533] dialog button: "Update"
[13:29:14.279] About before: {"Version":"0.6.0","Latest":"9.9.9 availableUpdate","Commit":"f2ca33bda","Built":"9/29/2026, 1:25:46 PM","Repository":"GitHub","Installed":"the release tarball","Upgrades":"roost can replace this copy","Upgrade":"curl -LO https://github.com/PeterKnego/roost/releases/latest/download/roost-x86_64-unknown-linux-gnu.tar.xz"}
[13:29:14.685] terminal socket: true
[13:29:14.692] prompt: true
[13:29:14.797] terminal session term printed: alive-1901358
[13:29:14.830] ps before:
    PID    PPID CMD
1901358 1901356 /bin/bash -l
    PID CMD
1901356 /usr/bin/dtach -A $S/state/sock/scratch/term -E -r winch -z /bin/bash -l

[13:29:14.836] reopened via mark: true
[13:29:14.837] dialog button: "Update"
[13:29:14.838] clicking Update
[13:29:14.942]   progress frame: {"t":1790688554840,"phase":"download","detail":"","shown":"download…"}
[13:29:14.943]   progress frame: {"t":1790688554847,"phase":"verify","detail":"","shown":"verify…"}
[13:29:14.943]   progress frame: {"t":1790688554847,"phase":"failed","detail":"verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)","shown":"update failed: verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)"}
[13:29:14.943] dialog progress line: "update failed: verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)"
[13:29:14.943] dialog button: "Retry"
[13:29:14.944] update view: {"status":"newer","latest":"9.9.9","offer":true,"skipped":"","deferred_until":0,"failure":"verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)","installed":""}
[13:29:15.667] About after failure: {"Version":"0.6.0","Latest":"9.9.9 available (update failed: verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz))Retry","Commit":"f2ca33bda","Built":"9/29/2026, 1:25:46 PM","Repository":"GitHub","Installed":"the release tarball","Upgrades":"roost can replace this copy","Upgrade":"curl -LO https://github.com/PeterKnego/roost/releases/latest/download/roost-x86_64-unknown-linux-gnu.tar.xz"}
[13:29:15.669] version after: 0.6.0
[13:29:15.669] shell still answers:
[13:29:15.774]    true
== after failure run
2026-09-29T13:29:15+00:00
    PID                  STARTED CMD
1901837 Tue Sep 29 13:29:04 2026 $S/bin/roost 8446
total 41572
drwxrwxr-x  2 claude claude     4096 Sep 29 13:29 .
drwxrwxr-x 11 claude claude     4096 Sep 29 13:29 ..
-rwxrwxr-x  1 claude claude 42804440 Sep 29 13:29 roost
9b55a3a1fee3b1a174621ebec936821370857bc35bff3c67ae769746f31d1109  $S/bin/roost
roost 0.6.0
-- error.log (tail)
1790688486 update: swap
1790688486 update: swapped $S/bin/roost for 9.9.9; exec
1790688554 update: starting: http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz -> $S/bin/roost
1790688554 update: download
1790688554 update: verify
1790688554 update: failed at verify: signature did not verify (http://127.0.0.1:8999/roost-x86_64-unknown-linux-gnu.tar.xz)
-- http-bad.log
127.0.0.1 - - [29/Sep/2026 13:29:14] "GET /roost-x86_64-unknown-linux-gnu.tar.xz HTTP/1.1" 200 -
127.0.0.1 - - [29/Sep/2026 13:29:14] "GET /roost-x86_64-unknown-linux-gnu.tar.xz.minisig HTTP/1.1" 200 -
```

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

## Handoff

Written for whoever picks this up, since the person who designed it is not
the person implementing it.

- **Branch.** `self-update`, cut from `develop` at `7270fa6` (the merge of
  #78). It carries the two documents and nothing else.
- **Order of work.** The version check (step 2, its own spec) is designed but
  **not implemented**, and this design consumes its stored fact, its
  comparator, its `ureq` agent and its `SettingsView` route. Implement step 2
  first, or in the same plan as the first tasks here. Then invoke the
  `writing-plans` skill on this spec; the plan's first task is the
  `wsstate::save` check named under *The restart*.
- **Maintainer-only prerequisites**, outside the repository: the minisign
  keypair and the two `release` environment secrets, recorded as open item
  6.5 in `docs/roost-packaging-handover.md`. The code can be written and
  tested against the committed test keypair before those exist; the real
  key is needed only for the first release.
- **Building in a worktree on the dev host.** `~/.cargo/config.toml` there
  points every checkout at one shared target directory, and `build.rs` bakes
  absolute asset paths, so a second checkout silently builds over the first
  (CLAUDE.md, *Build from one checkout*). Redirect the worktree with an
  uncommitted `[build] target-dir = "target"` in its `.cargo/config.toml`, or
  check the branch out in the main directory instead. Run tests with
  `cargo test -- --test-threads=1`; a bare `cargo test` has hung here.
- **What was verified this session**: the baseline suite on this branch, 913
  library tests and every integration binary green, single-threaded. What
  was **not**: whether `wsstate::save` includes dirty buffer text, whether
  `minisign-verify` accepts a signature produced by the `minisign` CLI
  unchanged (Tauri's `.sig` files are minisign format, which is the
  expectation), and anything about the exec under systemd.
