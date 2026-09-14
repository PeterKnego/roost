# Self-Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An `[Update]` button in About for the two shapes roost may replace itself, a header mark and an `Update` / `Later` / `Skip <version>` dialog, tarballs signed on the release and verified offline before anything is unpacked, and a re-exec in place so the sessions survive.

**Architecture:** A new `src/update.rs` owns the pipeline (fetch → verify → unpack → `--version` probe under a timeout → rename → exec), the choices file, the process-wide in-flight guard, and the `UpdateView` fields the dialog renders from. Every network and file step takes its fetch as an injected function, like `registry::reconcile_with` and the version check, so all of it runs in `cargo test` with no network. Three intents (`Update`, `DeferUpdate`, `SkipUpdate`) are diverted in `wsconn.rs` before the hub lock, exactly as `Search` and `RestoreWorkspace` are; the pipeline reports each phase to the requesting connection alone with `send_to`, and the choices reach every page through a fresh settings snapshot to every hub. The release signs each tarball with a minisign key in a new reusable workflow in the global phase, and `build.rs` bakes `keys/roost.pub` into the binary when the maintainer has committed it.

**Tech Stack:** Rust 2021; `minisign-verify` 0.2 (zero dependencies), `lzma-rs` 0.3 and `tar` 0.4 (both pure Rust, no C toolchain for the musl targets); `minisign` 0.9 as a dev-dependency to generate keys and signatures inside tests; the `ureq` agent step 2 promoted; plain JS in `static/app.js` and `static/dialog.js`; Deno + Chromium for the browser test; the C `minisign` CLI on the release runner (`apt-get install minisign`).

**Spec:** docs/superpowers/specs/2026-09-13-self-update-design.md (and the comparison it argues from, `2026-09-13-upgrade-mechanisms-comparison.md`)

**Depends on:** #65 step 2, the version check (`docs/superpowers/plans/2026-09-14-version-check.md`, tracked as #85). This plan consumes, by name: `version::current() -> Option<State>`, `version::verdict(running, latest) -> Latest`, `version::Latest::{UpToDate, Newer(String), Unknown}`, `version::agent() -> ureq::Agent`, `version::view() -> proto::UpdateView`, `version::state_dir_for_update() -> PathBuf`, `version::write_state_to`, `version::reset_for_test()`, and `proto::UpdateView { status, latest }` as a sibling of `SettingsView::build`. **Do not start Task 1 until #85 is merged to `develop`**, and re-read every anchor below against that tree first: the line numbers here were taken on 2026-09-14 from `develop` at `a9e3ed3`, before step 2 landed.

## The state of play, verified in the code (2026-09-14)

Things the spec left open or assumed, checked against the tree so no task has to.

- **Unsaved buffer text survives an exec.** The spec makes this the plan's first task; it is answered. `wsstate::save` writes `BufferDisk { text: b.edited_text(), dirty, base_hash }` (`src/wsstate.rs:148-166`), `text` is `Some` exactly for a dirty buffer, and three tests pin it: `round_trips_layout_and_buffers` (asserts `edited_text() == Some("unsaved")`), `a_clean_buffer_puts_no_file_content_in_the_state_file`, `an_edited_buffer_still_round_trips_its_text` (`src/wsstate.rs:402-470`). And it is written on every edit: the `EditBuffer` arm ends in `self.persist()` (`src/hub.rs:686-704`). Nothing to fix; nothing to task.
- **The tarball's layout is not "one member named `roost`".** `roost-x86_64-unknown-linux-musl.tar.xz` from v0.5.2, listed on 2026-09-14: a directory `roost-x86_64-unknown-linux-musl/` holding `README.md`, `roost` (mode 0755, 4 679 080 bytes), `LICENSE-MIT`, `LICENSE-APACHE`. So the rule is **exactly one regular member whose file name is `roost`**, at any depth; a symlink, a directory, none, or two are refused. Task 3 tests the real layout.
- **`releases/download/` redirects.** `https://github.com/PeterKnego/roost/releases/download/v0.5.2/<file>` answers `302` to `release-assets.githubusercontent.com`; the final `200` carries `content-type: application/octet-stream` and `content-length: 1435944`. `ureq` 2.12 follows up to 5 redirects by default (`ureq-2.12.1/src/agent.rs:262`) and never downgrades HTTPS to HTTP, so the size check reads `Content-Length` off the final response.
- **No self-exec exists anywhere.** `current_exe()` appears once, in `src/install.rs:104`, to locate the binary for the write probe. `std::os::unix::process::CommandExt::exec` appears nowhere. Task 8 introduces it.
- **The crate is what About calls replaceable.** `install::dir_writable` (`src/install.rs:142-157`) is the probe; `config::build_info()` (`src/config.rs:616-632`) turns it into the strings `channel`, `owner`, `replaceable`, `target`, `repository`; and `upgradesLabel` in `static/dialog.js:310-322` reads "roost can replace this copy" for `channel == "release"`, owner neither `homebrew` nor `system-package`, `replaceable == "yes"`. Task 7 uses those same strings server-side, so the button and the label cannot disagree.
- **One dialog at a time.** `runDialog` (`static/dialog.js:28-101`) returns `dismissed` immediately if `openDlg` is set. The About row's `[Update]` button therefore closes the settings dialog first, through a `close()` hook on `settingsOpen`, then opens the update dialog (Task 10).
- **The header has no "chip" class.** The branch pill is `#wtbtn`/`#gitinfo` (`static/style.css:53-81`), server-rendered in `src/render.rs:1880`; `#connstate` (`render.rs:1882`) is the reconnect badge. The mark is a new `#updmark` beside it.
- **Timeout-wrapped child processes exist.** `gitio::run_git` (`src/gitio.rs:26-77`) drains pipes on their own threads and polls `try_wait()` against a deadline, then `kill()` + `wait()`. Task 4 copies that shape for the probe.
- **The two-subscriber privacy test exists.** `a_clean_buffers_failed_replay_read_is_reported_to_that_connection` (`src/wsconn.rs` tests, ~1011-1041) subscribes twice and asserts the second receiver got nothing. Task 8 copies it.
- **The release runner already has a musl C toolchain.** `dist plan --output-format=json` on 2026-09-14 lists `packages_install: sudo apt-get install musl-tools` for both musl targets, so `ring` (which step 2 brings in through `ureq`) compiles there. None of the crates *this* plan adds compile C.
- **`minisign` is an apt package** (`0.12-1build1` on the dev host, in Ubuntu 22.04's universe). The dev host does not have it installed and the C CLI has no non-interactive password option, which is why Task 12 generates the release key **unencrypted** (`minisign -G -W`) and Task 13 installs the CLI on the deploy host for the manual run.
- **`minisign::PublicKey` has `to_base64()`** (`rust-minisign/src/public_key.rs:195`), `KeyPair::generate_unencrypted_keypair()` exists, `sign(pk, sk, reader, trusted, untrusted) -> SignatureBox` and `SignatureBox::into_string()` exist (read from the crate's source on 2026-09-14). `minisign_verify::PublicKey::from_base64`, `Signature::decode(&str)` (the whole `.minisig` text) and `verify(&self, bin, &sig, allow_legacy)` exist (docs.rs, 0.2.5). `lzma_rs::xz_decompress<R: BufRead, W: Write>(&mut R, &mut W)` and `xz_compress` exist (0.3.0).

## Global Constraints

- **The button applies to one condition**: `build.channel == "release"`, `build.owner` neither `"homebrew"` nor `"system-package"`, `build.replaceable == "yes"`, **and a public key is compiled in**. Those four strings come from `config::build_info()`; the server decides and sends `update.offer: bool`, the client never re-derives it. Every other shape keeps exactly what #78 shipped.
- **A checkout gets no mark and no dialog.** Decided client-side from `build.channel == "checkout"`, because About already says "yours to rebuild".
- **URLs are derived, never configured**: `<repository>/releases/download/v<latest>/roost-<target>.tar.xz` and the same with `.minisig`, from `BuildInfo.repository`, the version check's stored fact, and `BuildInfo.target`. `ROOST_UPDATE_BASE` is an **environment-only** test hook that replaces the base for one process; it is never a config key, and it is safe by construction because the signature, not the URL, is the boundary: a redirected download without the private key fails at `verify` and the executable is untouched. Recorded as a decision below.
- **Caps and timeouts**: 64 MB per download (`MAX_ARCHIVE_BYTES`), checked against `Content-Length` before the body is buffered *and* enforced while reading; 10 s per request (the version check's agent timeout, reused); 5 s for the `--version` probe (`PROBE_SECS`).
- **Order is fixed**: capture `current_exe()` first; fetch both files; verify the signature over the tarball bytes **before** decompression touches them; unpack exactly one regular member named `roost`; stage as `.roost-update.<pid>` mode 0755 in the executable's own directory; probe; `rename`; then exec. Every failure before the rename leaves the executable byte-identical and removes the staged file.
- **The probe's positive evidence** is stdout exactly `roost <latest>` (that is what `src/main.rs:8-11` prints) and exit 0, inside the timeout. A hang, a non-zero exit, or another version is a failure that deletes the staged file.
- **The choices file** is `state_dir()/update/choices.json`, schema exactly `{ "skipped", "deferred_until" }`, written atomically (pid-unique temp, then `rename`), beside but never the same file as the version check's `check.json`. Missing, truncated or unreadable means *no choice was made*. `DEFER_SECS` is 24 h; a `deferred_until` more than 24 h ahead reads as expired.
- **Three intents, global not per project**: `Update`, `DeferUpdate`, `SkipUpdate { version }`. `SkipUpdate` with a version that is not the last check's `latest` is ignored and logged. A choice pushes a fresh settings snapshot to every hub.
- **One event**: `UpdateProgress { phase, detail }`, sent to the requester only. `phase` is one of `download`, `verify`, `unpack`, `probe`, `swap`, `restarting`, `failed`, `refused`.
- **No lock across I/O**: the intents are diverted in `wsconn.rs` before the hub lock; the pipeline thread takes the hub lock only to `send_to`, once per phase. The in-flight guard is an `AtomicBool`, cleared on every path except a successful exec. `catch_unwind` wraps the thread; a panic is a failure like any other and clears the guard.
- **`errlog::record` gets every phase transition and every failure.**
- **`UpdateView` grows five fields** beside step 2's `status` and `latest`: `offer: bool`, `skipped: String`, `deferred_until: u64` (0 when not deferred), `failure: String` (`"<phase>: <message>"` or empty; not persisted; dropped when `latest` changes), `installed: String` (the version swapped in when exec failed, else empty). `status` keeps step 2's five values, so `tests/browser/version.mjs` is untouched.
- **The About row** reads, in order of precedence: `restart roost to run <installed>`; `<latest> skipped` (status `newer` and `skipped == latest`); step 2's five renderings; with ` (update failed: <failure>)` appended when `failure` is set.
- **The dialog** opens by itself once per page load when the mark first appears and the choice is not deferred; on every later click of the mark; and from About's button. Its title is `roost <latest> is available — you are running <version>`. After `restarting`, the page reloads when the control socket next reconnects.
- **Signing**: `.github/workflows/sign-release-artifacts.yml`, `workflow_call`, `environment: release`, called from a hand-added `custom-sign-release-artifacts` job in `release.yml` gated on `publishing == 'true'` and listed in `host`'s `needs` and `if`; the secret is `ROOST_MINISIGN_KEY` only, an **unencrypted** minisign secret key. The public key is `keys/roost.pub`, baked by `build.rs` into `ROOST_UPDATE_PUBKEY`; absent means no key and no button, never "trust anything".
- Test harnesses already turn the version check off (step 2). The browser test here turns it **on** with a planted, fresh `check.json`, so the status is `newer` and no request fires; it asserts the file is unchanged afterwards.
- Run tests as `cargo test -- --test-threads=1`; never `--release`. Work in a worktree with an uncommitted `[build] target-dir = "target"` in `.cargo/config.toml`; `git add` explicit paths only.
- Every new test states what deleting or reverting the covered code does to it, and the signature test, the swap test and the single-flight test each get a revert-and-watch-it-fail step.

## Decisions this plan makes where the spec left a choice

1. **The test keypair is generated inside the tests**, with the `minisign` crate as a dev-dependency, rather than committed under `tests/fixtures/`. No secret key lands in the repository, nothing needs rotating, and the C CLI is not on the dev host to produce a fixture with. The CLI-compatibility question the spec's *Handoff* names ("whether `minisign-verify` accepts a signature the `minisign` CLI produced") is answered by Task 13's manual run, which signs with the CLI.
2. **The release key is unencrypted** (`minisign -G -W`) and there is one secret, `ROOST_MINISIGN_KEY`. The C CLI cannot take a password non-interactively, and a password stored beside the key in the same environment protects nothing. Task 12 updates handover item 6.5 and #87 accordingly.
3. **A build without `keys/roost.pub` offers no button.** The maintainer's key (#87) may land after this code does; `build.rs` bakes an empty string when the file is absent, `update::public_key()` returns `None`, `offer` is `false`, and the `Update` intent is refused with "this build carries no release key". Nothing is trusted by default.
4. **`ROOST_UPDATE_BASE`** exists for Task 13's manual run and for nothing else. See Global Constraints for why it is not a hole.
5. **`Escape` during a running update closes the dialog and nothing else.** The pipeline continues; the mark stays; the row shows the outcome. Cancelling a download that has already been verified would be a fourth state with no user in it.

---

## File Structure

**Created**

| File | Single responsibility |
|---|---|
| `src/update.rs` | The pipeline and everything that decides whether to run it: URLs, the compiled-in key, verify, unpack, probe, stage, swap, exec, the guard, the choices file, and `view()`. |
| `src/pubkey.rs` | One function, `pubkey_line`, shared by `build.rs` and `update.rs` through `include!`, like `channel.rs`. |
| `keys/README.md` | Where `roost.pub` goes and what happens while it is absent. |
| `tests/browser/update.mjs` | The mark, the dialog, Later, Skip, the About row, in a real browser. |
| `tests/update.rs` | The choices intents over a real websocket. |
| `.github/workflows/sign-release-artifacts.yml` | Signs every release tarball in the global phase. |

**Modified**

| File | Change |
|---|---|
| `Cargo.toml` | `minisign-verify`, `lzma-rs`, `tar` under `[dependencies]`; `minisign` under `[dev-dependencies]`. |
| `build.rs` | Bakes `keys/roost.pub` into `ROOST_UPDATE_PUBKEY`. |
| `src/lib.rs` | `pub mod update;`. |
| `src/proto.rs` | Three intents, one event, five `UpdateView` fields. |
| `src/version.rs` | `view()` fills the struct with `..Default::default()`. |
| `src/config.rs` | `settings_view` calls `update::view()`. |
| `src/hub.rs` | `broadcast_settings_all()`; three `unreachable!` arms. |
| `src/wsconn.rs` | The divert for the three intents. |
| `src/render.rs` | `#updmark` in the header, the `dlg-update` shell, the shell test's id list. |
| `static/app.js` | The mark, auto-open, `UpdateProgress`, reload on reconnect. |
| `static/dialog.js` | `openUpdate`, `latestLabel`'s three extra renderings, the About button, `settingsOpen.close`. |
| `static/style.css` | `#updmark`, `.dlg-cmd`, `.dlg-progress`. |
| `.github/workflows/release.yml` | The `custom-sign-release-artifacts` job; `host`'s `needs` and `if`. |
| `dist-workspace.toml` | `global-artifacts-jobs` names the signing workflow too. |
| `docs/roost-packaging-handover.md` | Item 6.5: unencrypted key, one secret. |
| `tests/browser/README.md` | The run list and the revert-check log. |
| `docs/superpowers/specs/2026-09-13-self-update-design.md` | The manual run's record (Task 13). |

---

### Task 1: Dependencies, the asset URLs, and the compiled-in key

**Files:**
- Modify: `Cargo.toml:33-49`, `build.rs` (beside the `ROOST_TARGET` line, `build.rs:~78`; and `include!("src/channel.rs")` at `build.rs:10`), `src/lib.rs` (after `pub mod upload;`)
- Create: `src/pubkey.rs`, `src/update.rs`, `keys/README.md`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces:
  - `pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;`
  - `pub const PROBE_SECS: u64 = 5;`
  - `pub const DEFER_SECS: u64 = 24 * 60 * 60;`
  - `fn pubkey_line(text: &str) -> Option<String>` (in `src/pubkey.rs`, included)
  - `pub fn public_key() -> Option<&'static str>`
  - `pub fn release_base(repository: &str, version: &str) -> String`
  - `pub fn download_base(repository: &str, version: &str) -> String`
  - `pub fn asset_urls(base: &str, target: &str) -> (String, String)`

- [ ] **Step 1: Write the failing test**

Create `src/update.rs` with the module doc and only the test module. Deleting the `v` prefix in `release_base` fails the first table; making `pubkey_line` return the first line fails `the_pub_file_s_second_line_is_the_key`.

```rust
//! Updating roost from the button. See
//! `docs/superpowers/specs/2026-09-13-self-update-design.md`.

include!("pubkey.rs");

#[cfg(test)]
mod tests {
    use super::*;

    /// The two URLs, from the three facts the binary already carries. The
    /// tag is `v<version>`, as `gh release list` shows for every release.
    #[test]
    fn the_asset_urls_are_derived_from_repository_version_and_target() {
        let base = release_base("https://github.com/PeterKnego/roost", "0.5.3");
        assert_eq!(base, "https://github.com/PeterKnego/roost/releases/download/v0.5.3");
        let (tarball, sig) = asset_urls(&base, "x86_64-unknown-linux-musl");
        assert_eq!(tarball, "https://github.com/PeterKnego/roost/releases/download/v0.5.3/roost-x86_64-unknown-linux-musl.tar.xz");
        assert_eq!(sig, format!("{tarball}.minisig"));
        // A trailing slash on the repository must not double up.
        assert_eq!(release_base("https://github.com/PeterKnego/roost/", "0.5.3"), base);
    }

    /// The test hook, and its limit: it replaces the base for this process
    /// and nothing else. The signature is the boundary, not the URL.
    #[test]
    fn the_base_can_be_overridden_by_env_for_a_manual_run() {
        let _g = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_UPDATE_BASE", "http://127.0.0.1:8999/");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"), "http://127.0.0.1:8999");
        std::env::remove_var("ROOST_UPDATE_BASE");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"),
            "https://github.com/PeterKnego/roost/releases/download/v9.9.9");
    }

    /// A `.pub` file is an untrusted-comment line and a base64 line; only the
    /// second is the key. `build.rs` bakes that line, so a comment change on
    /// the file must not change the binary's trust.
    #[test]
    fn the_pub_file_s_second_line_is_the_key() {
        let text = "untrusted comment: minisign public key 1234ABCD\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
        assert_eq!(pubkey_line(text).as_deref(), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"));
        assert_eq!(pubkey_line("untrusted comment: nothing else\n"), None, "a comment alone is no key");
        assert_eq!(pubkey_line(""), None);
        assert_eq!(pubkey_line("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3".into()),
            "a bare base64 line is accepted too");
        assert_eq!(pubkey_line("not base64 at all !!\n"), None);
    }

    /// Whatever `build.rs` baked either decodes as a minisign key or is
    /// absent. Absent is what this checkout has until #87 commits the key,
    /// and absent must never read as "trust anything".
    #[test]
    fn the_compiled_in_key_is_absent_or_a_real_key() {
        match public_key() {
            None => {}
            Some(k) => {
                minisign_verify::PublicKey::from_base64(k).expect("keys/roost.pub holds a minisign key");
            }
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Add `pub mod update;` to `src/lib.rs` after `pub mod upload;`. Create `src/pubkey.rs` empty. Run:
`cargo test --lib update:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `release_base` in this scope``.

- [ ] **Step 3: Dependencies**

In `Cargo.toml`, `[dependencies]` gains (after the `ureq` line step 2 added):

```toml
# Step 4 of #65: verify a release tarball's minisign signature offline, then
# open it. All three are pure Rust — the musl targets need no C toolchain for
# them (ring, which ureq brings, is the one that does, and dist installs
# musl-tools on the release runners for it).
minisign-verify = "0.2"
lzma-rs = "0.3"
tar = "0.4"
```

and `[dev-dependencies]` gains:

```toml
# Generates a keypair and signs inside the tests, so no secret key is ever
# committed. Same author and format as the C minisign the release uses.
minisign = "0.9"
```

- [ ] **Step 4: `src/pubkey.rs`, shared with `build.rs`**

```rust
/// The base64 line of a minisign `.pub` file, or `None`. The file is one
/// `untrusted comment:` line and one base64 line; only the second is the
/// key, so a comment edit must not change what a binary trusts.
///
/// Shared with `build.rs` through `include!`, like `channel.rs`: the build
/// script bakes the line, and `update.rs` tests the rule.
fn pubkey_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .filter(|l| l.len() >= 40 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='))
        .map(str::to_string)
}
```

- [ ] **Step 5: `build.rs` bakes the key**

Beside `include!("src/channel.rs");` at `build.rs:10`:

```rust
include!("src/pubkey.rs");
```

In `emit_build_info`, after the `ROOST_TARGET` line:

```rust
    // The release signing key, if the maintainer has committed it (#87).
    // Absent bakes an empty string: that build carries no key, offers no
    // [Update] button, and refuses the intent. It never means "trust anything".
    let pub_path = root.join("keys").join("roost.pub");
    println!("cargo:rerun-if-changed={}", pub_path.display());
    let key = fs::read_to_string(&pub_path).ok().and_then(|t| pubkey_line(&t)).unwrap_or_default();
    println!("cargo:rustc-env=ROOST_UPDATE_PUBKEY={key}");
```

Create `keys/README.md`:

```markdown
# keys

`roost.pub` is the minisign public key every release tarball is signed with.
`build.rs` bakes its base64 line into the binary (`ROOST_UPDATE_PUBKEY`), and
`src/update.rs` verifies a downloaded tarball against it before the archive is
opened. It is generated once, offline, by the maintainer — see item 6.5 in
`docs/roost-packaging-handover.md` — and **the secret key is never committed**.

While the file is absent, a build carries no key: About offers no `Update`
button and the `Update` intent is refused with "this build carries no release
key". Rotation is a new file here and a release: a binary trusts only the key
it was built with.
```

- [ ] **Step 6: The constants, the key accessor, and the URLs**

Above the test module in `src/update.rs`:

```rust
use std::path::{Path, PathBuf};

/// The download cap. The musl tarball is 1.4 MB; a server answering with
/// gigabytes is one to walk away from, and the check happens twice — against
/// `Content-Length` before the body is buffered, and while reading it.
pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
/// The `--version` probe's bound. The macOS lesson from #65: a hang is a
/// fourth outcome that exit status cannot see.
pub const PROBE_SECS: u64 = 5;
/// How long `Later` keeps the dialog from opening by itself. The mark stays.
pub const DEFER_SECS: u64 = 24 * 60 * 60;

/// The public key `build.rs` baked from `keys/roost.pub`, or `None` for a
/// build made without one — which offers nothing, rather than trusting
/// anything.
pub fn public_key() -> Option<&'static str> {
    match option_env!("ROOST_UPDATE_PUBKEY") {
        Some(k) if !k.is_empty() => Some(k),
        _ => None,
    }
}

/// `<repository>/releases/download/v<version>`, the directory dist hosts a
/// release's artifacts under. Derived from `BuildInfo.repository`, never
/// spelled and never configured: a setting that let a repository name the
/// host roost fetches a binary from would be the hole the version-check spec
/// exists to avoid.
pub fn release_base(repository: &str, version: &str) -> String {
    format!("{}/releases/download/v{version}", repository.trim_end_matches('/'))
}

/// `release_base`, unless `ROOST_UPDATE_BASE` names another for this process.
/// Environment only, never config, and a test hook rather than a feature:
/// the manual run in this plan's last task serves a locally signed tarball
/// from it. It is not a hole because the signature is the boundary, not the
/// URL — a redirected download without the private key fails at `verify`
/// and the executable is untouched.
pub fn download_base(repository: &str, version: &str) -> String {
    match std::env::var("ROOST_UPDATE_BASE") {
        Ok(b) if !b.is_empty() => b.trim_end_matches('/').to_string(),
        _ => release_base(repository, version),
    }
}

/// The tarball and its detached signature, named the way dist names them
/// (`roost-<target>.tar.xz`) and the way `minisign -S` names its output.
pub fn asset_urls(base: &str, target: &str) -> (String, String) {
    let tarball = format!("{base}/roost-{target}.tar.xz");
    let sig = format!("{tarball}.minisig");
    (tarball, sig)
}
```

- [ ] **Step 7: Run it to verify it passes**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 4 tests. Also `cargo build` links (the three new crates compile; `minisign` only under test).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock build.rs src/lib.rs src/pubkey.rs src/update.rs keys/README.md
git commit -m "The update's URLs and key are derived; a build without a key offers nothing"
```

---

### Task 2: Verify the signature before anything is opened

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: nothing new
- Produces:
  - `pub fn verify(tarball: &[u8], minisig: &str, pubkey_b64: &str) -> Result<(), String>`
  - `#[cfg(test)] fn test_key() -> (String, minisign::SecretKey)` — a fresh keypair per call
  - `#[cfg(test)] fn sign_bytes(sk: &minisign::SecretKey, data: &[u8]) -> String` — the `.minisig` text

- [ ] **Step 1: Write the failing test**

Deleting the `pk.verify` call (returning `Ok(())` after decoding) fails the flipped-byte and other-key rows; that is the revert Step 6 performs. Each failure carries its own message so the dialog can say which.

```rust
    fn test_key() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_base64(), kp.sk)
    }

    fn sign_bytes(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, data, Some("roost test signature"), None).unwrap().into_string()
    }

    /// A valid signature passes; a flipped byte, another key, and garbage
    /// each fail with their own message — "signature did not verify" is the
    /// one someone should hear about, and it must not be confused with a
    /// signature file that could not be read.
    #[test]
    fn the_signature_is_checked_over_the_tarball_bytes() {
        let (pk, sk) = test_key();
        let data = b"not really a tarball, but the bytes are what is signed".to_vec();
        let sig = sign_bytes(&sk, &data);
        assert_eq!(verify(&data, &sig, &pk), Ok(()));

        let mut flipped = data.clone();
        flipped[7] ^= 0x01;
        assert_eq!(verify(&flipped, &sig, &pk).unwrap_err(), "signature did not verify");

        let (other_pk, _) = test_key();
        assert_eq!(verify(&data, &sig, &other_pk).unwrap_err(), "signature did not verify");

        let e = verify(&data, "this is not a minisig file", &pk).unwrap_err();
        assert!(e.starts_with("the signature file could not be read"), "{e}");

        let e = verify(&data, &sig, "not a key").unwrap_err();
        assert!(e.starts_with("the compiled-in key could not be read"), "{e}");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update::tests::the_signature -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `verify` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
/// Verified over the tarball bytes, **before** decompression touches them:
/// nothing unsigned is parsed. `allow_legacy` is false — the release signs
/// with a current minisign, and the legacy format is one more thing to
/// accept for no reason. The three messages are distinct on purpose: a
/// signature that does not verify is the one someone should hear about.
pub fn verify(tarball: &[u8], minisig: &str, pubkey_b64: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("the compiled-in key could not be read: {e}"))?;
    let sig = minisign_verify::Signature::decode(minisig)
        .map_err(|e| format!("the signature file could not be read: {e}"))?;
    pk.verify(tarball, &sig, false)
        .map_err(|_| "signature did not verify".to_string())
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "A tarball is verified against the compiled-in key before it is opened"
```

- [ ] **Step 6: Revert the check and watch it fail**

Replace `verify`'s last statement with `let _ = (tarball, sig, pk); Ok(())` — the shape a refactor that "already checked the key decodes" would leave. Run:
`cargo test --lib update::tests::the_signature -- --test-threads=1`
Expected: FAIL at the flipped-byte row: `called `Result::unwrap_err()` on an `Ok` value`.

Restore, re-run, expected PASS, and record it above the test:

```rust
    /// Revert-checked: a `verify` that decodes both inputs and returns `Ok`
    /// without calling `pk.verify` fails here at the flipped-byte row with
    /// `called Result::unwrap_err() on an Ok value`.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/update.rs
git commit -m "Record what skipping the signature check does to the flipped-byte row"
```

---

### Task 3: Unpack exactly one `roost`, from the layout dist actually ships

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: `MAX_ARCHIVE_BYTES` (Task 1)
- Produces:
  - `pub fn extract_binary(tar_xz: &[u8]) -> Result<Vec<u8>, String>`
  - `#[cfg(test)] fn tar_xz(members: &[Member]) -> Vec<u8>` and `#[cfg(test)] enum Member<'a> { File(&'a str, &'a [u8]), Dir(&'a str), Link(&'a str, &'a str) }`

- [ ] **Step 1: Write the failing test**

Deleting the `entry_type` match fails the symlink and directory rows; deleting the count fails the two-members row; matching the full path `roost` instead of the file name fails the first row, which is the real layout.

```rust
    enum Member<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    /// An archive the way dist writes one, built in memory: a top directory
    /// named for the target, files under it. `xz_compress` produces a
    /// stream `xz_decompress` reads; the release's is `xz -9`, same format.
    fn tar_xz(members: &[Member]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for m in members {
            let mut h = tar::Header::new_gnu();
            match m {
                Member::File(path, data) => {
                    h.set_mode(0o755);
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, path, *data).unwrap();
                }
                Member::Dir(path) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    b.append_data(&mut h, path, &b""[..]).unwrap();
                }
                Member::Link(path, target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    b.append_link(&mut h, path, target).unwrap();
                }
            }
        }
        let tar = b.into_inner().unwrap();
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut &tar[..], &mut out).unwrap();
        out
    }

    const T: &str = "roost-x86_64-unknown-linux-musl";

    /// The layout `tar -tJf` printed for v0.5.2 on 2026-09-14: a directory
    /// named for the target, and under it README.md, roost, and two
    /// licences. The member is `<dir>/roost`, not `roost`.
    #[test]
    fn the_release_layout_yields_the_one_binary() {
        let a = tar_xz(&[
            Member::Dir(&format!("{T}/")),
            Member::File(&format!("{T}/README.md"), b"# roost"),
            Member::File(&format!("{T}/roost"), b"\x7fELF fake"),
            Member::File(&format!("{T}/LICENSE-MIT"), b"MIT"),
            Member::File(&format!("{T}/LICENSE-APACHE"), b"Apache"),
        ]);
        assert_eq!(extract_binary(&a).unwrap(), b"\x7fELF fake");
    }

    #[test]
    fn every_other_shape_is_refused_by_name() {
        let none = tar_xz(&[Member::File(&format!("{T}/README.md"), b"x")]);
        assert_eq!(extract_binary(&none).unwrap_err(), "the archive has no roost member");

        let two = tar_xz(&[Member::File(&format!("{T}/roost"), b"a"), Member::File("other/roost", b"b")]);
        assert_eq!(extract_binary(&two).unwrap_err(), "the archive has 2 roost members");

        let link = tar_xz(&[Member::Link(&format!("{T}/roost"), "/bin/sh")]);
        assert_eq!(extract_binary(&link).unwrap_err(), "the roost member is a symlink");

        let dir = tar_xz(&[Member::Dir(&format!("{T}/roost/"))]);
        assert_eq!(extract_binary(&dir).unwrap_err(), "the roost member is a directory");

        let e = extract_binary(b"definitely not xz").unwrap_err();
        assert!(e.starts_with("the archive could not be decompressed"), "{e}");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `extract_binary` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
/// The one binary out of a release tarball. dist writes `<target>/roost`
/// beside a README and two licences, so the rule is *exactly one regular
/// member whose file name is `roost`*, at any depth. A symlink there would
/// be followed by nothing here but is refused anyway: the file that gets
/// probed and swapped has to be the bytes that were signed.
pub fn extract_binary(tar_xz: &[u8]) -> Result<Vec<u8>, String> {
    let mut tar = Vec::new();
    lzma_rs::xz_decompress(&mut &tar_xz[..], &mut tar)
        .map_err(|e| format!("the archive could not be decompressed: {e:?}"))?;
    let mut archive = tar::Archive::new(&tar[..]);
    let entries = archive.entries().map_err(|e| format!("the archive could not be read: {e}"))?;
    let mut found: Option<Vec<u8>> = None;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("the archive could not be read: {e}"))?;
        let is_roost = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "roost"))
            .unwrap_or(false);
        if !is_roost {
            continue;
        }
        count += 1;
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Directory => return Err("the roost member is a directory".into()),
            tar::EntryType::Symlink | tar::EntryType::Link => return Err("the roost member is a symlink".into()),
            other => return Err(format!("the roost member is not a regular file ({other:?})")),
        }
        if entry.size() > MAX_ARCHIVE_BYTES {
            return Err("the roost member is larger than the download cap".into());
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|e| format!("the roost member could not be read: {e}"))?;
        found = Some(bytes);
    }
    match (count, found) {
        (0, _) => Err("the archive has no roost member".into()),
        (1, Some(b)) => Ok(b),
        (n, _) => Err(format!("the archive has {n} roost members")),
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "Unpack the one roost, from the layout dist actually ships"
```

---

### Task 4: Prove the new binary runs, inside a bound

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: `PROBE_SECS` (Task 1)
- Produces:
  - `pub fn probe(exe: &Path, want: &str, timeout: std::time::Duration) -> Result<(), String>`
  - `#[cfg(test)] fn fake_exe(dir: &Path, name: &str, body: &str) -> PathBuf`

- [ ] **Step 1: Write the failing test**

The pattern is `gitio::run_git`'s (`src/gitio.rs:26-77`), and the fake binaries are real executables, as `tests/terminals.rs`'s `fake_shell` builds them — a fake claude had to be a real binary for the same reason. The hang row is *timed*: a hang passes by never finishing, so the assertion is that the probe returned within a bound. Deleting the deadline loop hangs this test rather than failing it, which is why the bound is asserted and the run is watched.

```rust
    fn fake_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    /// Positive evidence that the file is a working roost of the version
    /// claimed: stdout exactly `roost <latest>`, exit 0, inside the bound.
    #[test]
    fn the_probe_wants_the_exact_version_line_within_the_timeout() {
        let d = tempfile::tempdir().unwrap();
        let t = std::time::Duration::from_secs(1);
        let good = fake_exe(d.path(), "good", "echo 'roost 9.9.9'");
        assert_eq!(probe(&good, "9.9.9", t), Ok(()));

        let wrong = fake_exe(d.path(), "wrong", "echo 'roost 9.9.8'");
        assert_eq!(probe(&wrong, "9.9.9", t).unwrap_err(),
            "the new binary reports \"roost 9.9.8\", not \"roost 9.9.9\"");

        let noisy = fake_exe(d.path(), "noisy", "echo 'roost 9.9.9'; echo extra");
        assert!(probe(&noisy, "9.9.9", t).is_err(), "a second line is not the exact answer");

        let failing = fake_exe(d.path(), "failing", "exit 3");
        assert!(probe(&failing, "9.9.9", t).unwrap_err().contains("exited"), "a non-zero exit is reported as such");

        // The macOS case: a binary that hangs. Timed, because a hang would
        // otherwise pass by never returning.
        let hang = fake_exe(d.path(), "hang", "sleep 30");
        let started = std::time::Instant::now();
        let e = probe(&hang, "9.9.9", t).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(4), "the probe returned in {:?}", started.elapsed());
        assert_eq!(e, "the new binary did not answer --version within 1s");

        let missing = d.path().join("missing");
        assert!(probe(&missing, "9.9.9", t).unwrap_err().starts_with("the new binary could not be started"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update::tests::the_probe -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `probe` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
/// Run the staged file with `--version` and require stdout to be exactly
/// `roost <want>` — what `main.rs` prints — with exit 0, inside `timeout`.
/// The shape is `gitio::run_git`'s: stdout drained on its own thread so a
/// full pipe cannot wedge the poll, `try_wait` against a deadline, then
/// `kill` + `wait` so a hung child is reaped rather than leaked.
pub fn probe(exe: &Path, want: &str, timeout: std::time::Duration) -> Result<(), String> {
    use std::io::Read;
    let mut child = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("the new binary could not be started: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|e| format!("could not wait for the new binary: {e}"))? {
            Some(st) => break st,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the new binary did not answer --version within {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("the new binary exited {status} on --version"));
    }
    let got = out.trim();
    if got != format!("roost {want}") {
        return Err(format!("the new binary reports {got:?}, not \"roost {want}\""));
    }
    Ok(())
}
```

- [ ] **Step 4: Run it to verify it passes, and time it**

Run: `time cargo test --lib update::tests::the_probe -- --test-threads=1`
Expected: PASS, and the test itself under 3 s (the hang row costs its 1 s timeout, nothing else waits).

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "The new binary has to say which version it is, and say it in time"
```

---

### Task 5: Stage, swap, and the pipeline that fails safe

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: `verify` (Task 2), `extract_binary` (Task 3), `probe` (Task 4), `MAX_ARCHIVE_BYTES`, `crate::version::agent()`
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Phase { Download, Verify, Unpack, Probe, Swap, Exec, Internal }` with `pub fn as_str(self) -> &'static str`
  - `pub type FetchBytes = fn(&str) -> Result<Vec<u8>, String>;`
  - `pub struct Plan { pub exe: PathBuf, pub tarball_url: String, pub sig_url: String, pub pubkey: String, pub want: String, pub probe_timeout: std::time::Duration }`
  - `pub fn stage(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String>`
  - `pub fn swap(staged: &Path, exe: &Path) -> Result<(), String>`
  - `pub fn run_pipeline(plan: &Plan, fetch: FetchBytes, report: &mut dyn FnMut(Phase)) -> Result<(), (Phase, String)>`
  - `pub fn read_capped(r: impl std::io::Read, cap: u64) -> Result<Vec<u8>, String>`
  - `pub fn http_get_bytes(url: &str) -> Result<Vec<u8>, String>`

- [ ] **Step 1: Write the failing test**

The fetch is injected by URL, so the same test drives the success path and every failure. After every failure the executable is byte-identical and no `.roost-update.*` remains; Step 6 reverts the cleanup and watches that assertion fail. Fetch functions are `fn` pointers, so the archive and signature they serve live in statics the test fills first.

```rust
    use std::sync::Mutex as StdMutex;
    static SERVED: StdMutex<Option<(Vec<u8>, String)>> = StdMutex::new(None);

    fn serve(tarball: Vec<u8>, sig: String) {
        *SERVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((tarball, sig));
    }

    /// Answers the two URLs the plan names from `SERVED`, anything else 404.
    fn fetch_served(url: &str) -> Result<Vec<u8>, String> {
        let g = SERVED.lock().unwrap_or_else(|e| e.into_inner());
        let (t, s) = g.as_ref().ok_or("nothing served")?;
        if url.ends_with(".tar.xz") { Ok(t.clone()) }
        else if url.ends_with(".minisig") { Ok(s.clone().into_bytes()) }
        else { Err(format!("{url}: status code 404")) }
    }

    fn fetch_404(url: &str) -> Result<Vec<u8>, String> {
        Err(format!("{url}: status code 404"))
    }

    /// A "release": a fake roost that answers `--version` with `want`,
    /// packed the way dist packs one, signed with `sk`.
    fn release(sk: &minisign::SecretKey, want: &str) -> (Vec<u8>, String) {
        let script = format!("#!/bin/sh\necho 'roost {want}'\n");
        let a = tar_xz(&[Member::Dir(&format!("{T}/")), Member::File(&format!("{T}/roost"), script.as_bytes())]);
        let sig = sign_bytes(sk, &a);
        (a, sig)
    }

    fn plan_in(d: &Path, pk: &str, want: &str) -> Plan {
        let exe = fake_exe(d, "roost", "echo 'roost 0.0.1'");
        Plan {
            exe,
            tarball_url: format!("http://test/roost-{T}.tar.xz"),
            sig_url: format!("http://test/roost-{T}.tar.xz.minisig"),
            pubkey: pk.to_string(),
            want: want.to_string(),
            probe_timeout: std::time::Duration::from_secs(2),
        }
    }

    fn leftovers(d: &Path) -> Vec<String> {
        std::fs::read_dir(d).unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".roost-update."))
            .collect()
    }

    /// The whole pipeline, start to swapped file, with every phase reported
    /// in order and the old file gone.
    #[test]
    fn a_signed_release_replaces_the_executable_in_place() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let mut phases = Vec::new();
        run_pipeline(&plan, fetch_served, &mut |p| phases.push(p)).unwrap();
        assert_eq!(phases, [Phase::Download, Phase::Verify, Phase::Unpack, Phase::Probe, Phase::Swap]);
        let after = std::fs::read(&plan.exe).unwrap();
        assert_ne!(after, before);
        assert!(String::from_utf8_lossy(&after).contains("roost 9.9.9"), "the swapped file is the new one");
        assert_eq!(leftovers(d.path()), Vec::<String>::new(), "no staged file remains");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&plan.exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    /// Every failure before the rename leaves the executable byte-identical
    /// and the staged file gone. Each row names the phase it fails in, which
    /// is what the dialog shows.
    #[test]
    fn every_failure_leaves_the_executable_untouched_and_nothing_staged() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let check = |why: &str| {
            assert_eq!(std::fs::read(&plan.exe).unwrap(), before, "{why}: the executable changed");
            assert_eq!(leftovers(d.path()), Vec::<String>::new(), "{why}: a staged file remains");
        };

        let (phase, msg) = run_pipeline(&plan, fetch_404, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Download);
        assert!(msg.contains("404"), "{msg}");
        check("download failed");

        let (a, _) = release(&sk, "9.9.9");
        let (_, other_sk) = test_key();
        serve(a.clone(), sign_bytes(&other_sk, &a));
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!((phase, msg.as_str()), (Phase::Verify, "signature did not verify"));
        check("signature");

        let empty = tar_xz(&[Member::File(&format!("{T}/README.md"), b"x")]);
        serve(empty.clone(), sign_bytes(&sk, &empty));
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!((phase, msg.as_str()), (Phase::Unpack, "the archive has no roost member"));
        check("no member");

        let (a, sig) = release(&sk, "9.9.8");
        serve(a, sig);
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Probe);
        assert!(msg.contains("9.9.8"), "{msg}");
        check("wrong version");

        let hang_script = format!("#!/bin/sh\nsleep 30\n");
        let a = tar_xz(&[Member::File(&format!("{T}/roost"), hang_script.as_bytes())]);
        serve(a.clone(), sign_bytes(&sk, &a));
        let (phase, _) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Probe);
        check("hang");
    }

    /// The cap, enforced on the body as well as the header — a server that
    /// lies about `Content-Length` is a server that sends more than it said.
    #[test]
    fn a_download_past_the_cap_is_refused_while_reading() {
        let big = std::io::repeat(b'x').take(100);
        assert_eq!(read_capped(big, 99).unwrap_err(), "the download exceeded the 99 byte cap");
        let ok = std::io::repeat(b'x').take(99);
        assert_eq!(read_capped(ok, 99).unwrap().len(), 99);
    }
```

Add `use std::io::Read;` at the top of the test module for `.take`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `run_pipeline` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
/// Where the pipeline was when it stopped. The dialog names it, About names
/// it, and `errlog` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Download,
    Verify,
    Unpack,
    Probe,
    Swap,
    Exec,
    /// The thread panicked. Reported like any other failure, and it clears
    /// the guard like any other failure.
    Internal,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Download => "download",
            Phase::Verify => "verify",
            Phase::Unpack => "unpack",
            Phase::Probe => "probe",
            Phase::Swap => "swap",
            Phase::Exec => "exec",
            Phase::Internal => "internal",
        }
    }
}

/// The injection seam, like `version::FetchFn`: a plain `fn` so it crosses
/// into the detached thread without a lifetime.
pub type FetchBytes = fn(&str) -> Result<Vec<u8>, String>;

/// Everything the pipeline needs, decided before the thread starts. `exe`
/// is captured first of all: on Linux `current_exe` reads `/proc/self/exe`,
/// which after the rename names the deleted inode with ` (deleted)`.
pub struct Plan {
    pub exe: PathBuf,
    pub tarball_url: String,
    pub sig_url: String,
    pub pubkey: String,
    pub want: String,
    pub probe_timeout: std::time::Duration,
}

/// The new bytes, as `.roost-update.<pid>` in the executable's own directory
/// — the same directory because the swap is a rename, and a rename is atomic
/// only within one filesystem.
pub fn stage(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let p = dir.join(format!(".roost-update.{}", std::process::id()));
    std::fs::write(&p, bytes).map_err(|e| format!("could not write {}: {e}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("could not mark {} executable: {e}", p.display()))?;
    }
    Ok(p)
}

/// The one irreversible step, and a single syscall. The running process
/// keeps its old inode until it execs.
pub fn swap(staged: &Path, exe: &Path) -> Result<(), String> {
    std::fs::rename(staged, exe).map_err(|e| format!("rename refused: {e}"))
}

/// Fetch, verify, unpack, probe, swap. Every step before the rename fails
/// safe by construction: the executable is untouched and the staged file is
/// removed. `report` is called as each phase *starts*, so the dialog reads
/// what is happening rather than what just did.
pub fn run_pipeline(plan: &Plan, fetch: FetchBytes, report: &mut dyn FnMut(Phase)) -> Result<(), (Phase, String)> {
    report(Phase::Download);
    let tarball = fetch(&plan.tarball_url).map_err(|e| (Phase::Download, e))?;
    let sig = fetch(&plan.sig_url).map_err(|e| (Phase::Download, e))?;
    let sig = String::from_utf8(sig).map_err(|_| (Phase::Download, "the signature file is not text".to_string()))?;

    report(Phase::Verify);
    verify(&tarball, &sig, &plan.pubkey).map_err(|e| (Phase::Verify, e))?;

    report(Phase::Unpack);
    let bytes = extract_binary(&tarball).map_err(|e| (Phase::Unpack, e))?;
    let dir = plan.exe.parent().ok_or((Phase::Unpack, "the executable has no parent directory".to_string()))?;
    let staged = stage(dir, &bytes).map_err(|e| (Phase::Unpack, e))?;

    report(Phase::Probe);
    if let Err(e) = probe(&staged, &plan.want, plan.probe_timeout) {
        let _ = std::fs::remove_file(&staged);
        return Err((Phase::Probe, e));
    }

    report(Phase::Swap);
    if let Err(e) = swap(&staged, &plan.exe) {
        let _ = std::fs::remove_file(&staged);
        return Err((Phase::Swap, e));
    }
    Ok(())
}

/// Read at most `cap` bytes, and say so if there were more: the header check
/// in `http_get_bytes` is what a well-behaved server passes, and this is what
/// a lying one hits.
pub fn read_capped(r: impl std::io::Read, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    r.take(cap + 1).read_to_end(&mut buf).map_err(|e| format!("the download stopped: {e}"))?;
    if buf.len() as u64 > cap {
        return Err(format!("the download exceeded the {cap} byte cap"));
    }
    Ok(buf)
}

/// The real fetch: the version check's agent (ten-second bound, the
/// User-Agent crates.io asks for, `HTTPS_PROXY` honoured), redirects
/// followed — `releases/download/` answers 302 to a CDN — and the cap checked
/// against `Content-Length` before a byte of body is buffered. A non-2xx is
/// an error in `ureq` 2 (`Error::Status`), whose message carries the code.
pub fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = crate::version::agent().get(url).call().map_err(|e| e.to_string())?;
    if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) {
        if len > MAX_ARCHIVE_BYTES {
            return Err(format!("the server announced {len} bytes, above the {MAX_ARCHIVE_BYTES} byte cap"));
        }
    }
    read_capped(resp.into_reader(), MAX_ARCHIVE_BYTES)
}
```

Add `use std::io::Read;` at the top of the module for `read_capped`.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 10 tests.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "Fetch, verify, unpack, probe, swap — and fail safe before the rename"
```

- [ ] **Step 6: Revert the cleanup and watch the failure test fail**

Delete the two `let _ = std::fs::remove_file(&staged);` lines in `run_pipeline`. Run:
`cargo test --lib update::tests::every_failure -- --test-threads=1`
Expected: FAIL at `wrong version: a staged file remains` — `left: [".roost-update.<pid>"], right: []`.

Restore, re-run, expected PASS, and record it above the test:

```rust
    /// Revert-checked: dropping the `remove_file` after a failed probe fails
    /// here at "wrong version: a staged file remains" with
    /// `left: [".roost-update.<pid>"], right: []`.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/update.rs
git commit -m "Record what a failed probe leaves behind without the cleanup"
```

---

### Task 6: Later and Skip, in their own file

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: `DEFER_SECS` (Task 1), `crate::version::state_dir_for_update()`
- Produces:
  - `#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)] pub struct Choices { pub skipped: Option<String>, pub deferred_until: Option<u64> }`
  - `pub fn choices_path() -> PathBuf`
  - `pub fn read_choices_from(path: &Path) -> Choices`
  - `pub fn write_choices_to(path: &Path, c: &Choices) -> Result<(), String>`
  - `pub fn deferred(c: &Choices, now: u64) -> bool`
  - `pub fn skipped(c: &Choices, latest: &str) -> bool`
  - `pub fn defer_in(path: &Path, now: u64) -> Result<(), String>`
  - `pub fn skip_in(path: &Path, latest: Option<&str>, version: &str) -> Result<(), String>`

- [ ] **Step 1: Write the failing test**

Deleting the `t - now <= DEFER_SECS` clause fails `a_deferral_too_far_ahead_reads_as_expired`. Making `read_choices_from` return `Some(default)` for a truncated file is the *same* as what it does — that is deliberate here and the test names it: no choice is the recoverable direction. Deleting the version comparison in `skip_in` fails `a_skip_from_a_stale_page_is_ignored`.

```rust
    fn ch(skipped: Option<&str>, deferred_until: Option<u64>) -> Choices {
        Choices { skipped: skipped.map(str::to_string), deferred_until }
    }

    #[test]
    fn the_choices_file_round_trips_beside_the_check_file_not_in_it() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("update").join("choices.json");
        let c = ch(Some("0.5.3"), Some(1_789_320_967));
        write_choices_to(&p, &c).unwrap();
        assert_eq!(read_choices_from(&p), c);
        let text = std::fs::read_to_string(&p).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["skipped", "deferred_until"], "{text}");
        // Its own file: the check thread writes check.json, the intent
        // handler writes this, and one file with two writers is a rename
        // race in which a completed check silently discards a click.
        assert_eq!(choices_path().file_name().unwrap(), "choices.json");
        assert_eq!(choices_path().parent().unwrap().file_name().unwrap(), "update");
    }

    /// Missing, truncated or the wrong shape is *no choice*, deliberately —
    /// that errs toward showing the dialog, the recoverable direction.
    #[test]
    fn an_unreadable_choices_file_is_no_choice() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        assert_eq!(read_choices_from(&p), Choices::default());
        std::fs::write(&p, "{\"skipped\":\"0.5").unwrap();
        assert_eq!(read_choices_from(&p), Choices::default());
        std::fs::write(&p, "[]").unwrap();
        assert_eq!(read_choices_from(&p), Choices::default());
    }

    #[test]
    fn a_skip_is_per_version_and_a_newer_one_unskips_by_itself() {
        let c = ch(Some("0.5.3"), None);
        assert!(skipped(&c, "0.5.3"));
        assert!(!skipped(&c, "0.5.4"), "the stored value is the version, not a flag");
        assert!(!skipped(&Choices::default(), "0.5.3"));
    }

    #[test]
    fn a_deferral_expires_at_its_timestamp() {
        let now = 1_000_000u64;
        assert!(deferred(&ch(None, Some(now + 3600)), now), "an hour left");
        assert!(!deferred(&ch(None, Some(now)), now), "expired exactly now");
        assert!(!deferred(&ch(None, Some(now - 1)), now));
        assert!(!deferred(&Choices::default(), now));
    }

    /// A stepped clock, or a file from another host: a deferral further
    /// ahead than `Later` can ever write reads as expired, not as a mark that
    /// stays silent for a year.
    #[test]
    fn a_deferral_too_far_ahead_reads_as_expired() {
        let now = 1_000_000u64;
        assert!(deferred(&ch(None, Some(now + DEFER_SECS)), now), "exactly what Later writes");
        assert!(!deferred(&ch(None, Some(now + DEFER_SECS + 1)), now), "more than Later can write");
    }

    #[test]
    fn later_writes_now_plus_a_day_and_keeps_the_skip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        write_choices_to(&p, &ch(Some("0.5.2"), None)).unwrap();
        defer_in(&p, 1_000_000).unwrap();
        assert_eq!(read_choices_from(&p), ch(Some("0.5.2"), Some(1_000_000 + DEFER_SECS)));
    }

    /// `SkipUpdate` carries the version so a click from a stale page cannot
    /// skip a version it never saw.
    #[test]
    fn a_skip_from_a_stale_page_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        write_choices_to(&p, &ch(None, Some(5))).unwrap();
        let e = skip_in(&p, Some("0.5.4"), "0.5.3").unwrap_err();
        assert_eq!(e, "0.5.3 is not the version the last check saw (0.5.4)");
        assert_eq!(read_choices_from(&p), ch(None, Some(5)), "nothing written");
        assert!(skip_in(&p, None, "0.5.3").is_err(), "no check yet, nothing to skip");

        skip_in(&p, Some("0.5.3"), "0.5.3").unwrap();
        assert_eq!(read_choices_from(&p), ch(Some("0.5.3"), None), "a skip clears the deferral");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find type `Choices` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
use serde::{Deserialize, Serialize};

/// What the user said to the dialog. Two choices, two lifetimes: `Later`
/// defers the self-opening for `DEFER_SECS` and keeps the mark; `Skip` is
/// per version, and a later version un-skips by itself because the stored
/// value is the version string, not a flag.
///
/// **A separate file from the version check's**, on purpose: that one is
/// written by the check thread when a fetch completes, this one by the intent
/// handler when a user clicks. One file with two writers is a rename race in
/// which a completed check silently discards a click.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choices {
    pub skipped: Option<String>,
    pub deferred_until: Option<u64>,
}

pub fn choices_path() -> PathBuf {
    crate::version::state_dir_for_update().join("choices.json")
}

/// Missing, truncated or unreadable is **no choice was made** — which errs
/// toward showing the dialog, the recoverable direction. This is the one
/// reader in the feature where folding "could not look" into the default is
/// right, because the default is the safe one.
pub fn read_choices_from(path: &Path) -> Choices {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Pid-unique temp file, then `rename`, like every piece of persistent
/// evidence here.
pub fn write_choices_to(path: &Path, c: &Choices) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "no parent directory".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(c).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".choices.{}.json.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Deferred until a moment still ahead — and not further ahead than `Later`
/// can write, so a stepped clock or a copied file cannot silence the dialog
/// for a year.
pub fn deferred(c: &Choices, now: u64) -> bool {
    c.deferred_until.is_some_and(|t| t > now && t - now <= DEFER_SECS)
}

pub fn skipped(c: &Choices, latest: &str) -> bool {
    c.skipped.as_deref() == Some(latest)
}

/// `Later`: the deferral moves, the skip stays.
pub fn defer_in(path: &Path, now: u64) -> Result<(), String> {
    let mut c = read_choices_from(path);
    c.deferred_until = Some(now + DEFER_SECS);
    write_choices_to(path, &c)
}

/// `Skip <version>`: only the version the last check actually saw, so a
/// click from a stale page cannot skip a version it never showed. A skip
/// clears the deferral — there is nothing left to defer.
pub fn skip_in(path: &Path, latest: Option<&str>, version: &str) -> Result<(), String> {
    match latest {
        Some(l) if l == version => {}
        Some(l) => return Err(format!("{version} is not the version the last check saw ({l})")),
        None => return Err(format!("{version} is not the version the last check saw (no check yet)")),
    }
    let mut c = read_choices_from(path);
    c.skipped = Some(version.to_string());
    c.deferred_until = None;
    write_choices_to(path, &c)
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 17 tests.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "Later and Skip live in their own file, beside the check's and never in it"
```

---

### Task 7: The wire — three intents, one event, five fields, and `view()`

**Files:**
- Modify: `src/proto.rs` (`Intent` after `RestoreWorkspace`, `Event` after `RestoreReport`, `UpdateView` from step 2), `src/version.rs` (`view()`), `src/config.rs` (`settings_view`'s return expression), `src/hub.rs` (after the `RestoreWorkspace` `unreachable!` arm at `~771-781`; a new free fn after `broadcast_all` at `~2521`), `src/update.rs`
- Test: `src/update.rs` `mod tests`, `src/config.rs` `mod tests`

**Interfaces:**
- Consumes: `proto::UpdateView { status, latest }`, `version::view()`, `version::current()`, `config::build_info()`, `public_key()`, `Choices` and its readers (Task 6)
- Produces:
  - `Intent::Update`, `Intent::DeferUpdate`, `Intent::SkipUpdate { version: String }`
  - `Event::UpdateProgress { phase: String, detail: String }`
  - `UpdateView` fields `offer: bool`, `skipped: String`, `deferred_until: u64`, `failure: String`, `installed: String`
  - `pub fn replaceable_here() -> Result<(), String>` in `update.rs`
  - `pub fn view() -> crate::proto::UpdateView` in `update.rs`
  - `pub fn broadcast_settings_all()` in `hub.rs`
  - statics `LAST_FAILURE: Mutex<Option<(String, Phase, String)>>`, `INSTALLED: Mutex<Option<String>>` and `#[cfg(test)] pub fn reset_for_test()` in `update.rs`

- [ ] **Step 1: Write the failing tests**

In `src/update.rs`'s `mod tests`. `env_fixture` is step 2's helper in `version.rs`'s tests; it is private there, so this module has its own, taking the same two locks in the same order. Deleting the `public_key().is_some()` clause from `view()` cannot fail in this checkout (there is no key) — which is why `replaceable_here` is tested on its own with `BuildInfo` strings, and `offer` is asserted false here with the reason recorded.

```rust
    /// Points both process-global env vars at this test's own fixture.
    /// `STATE_ENV_LOCK` first, then `config::ENV_LOCK`, the order documented
    /// on both; an inversion deadlocks rather than fails.
    fn env_fixture() -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
    ) {
        let g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let g2 = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "version_check = true\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &cfg);
        crate::version::reset_for_test();
        reset_for_test();
        (g1, g2, d)
    }

    fn checked(latest: &str) -> crate::version::State {
        crate::version::State {
            latest: Some(latest.to_string()),
            checked_at: Some(crate::errlog::now_secs()),
            failed_at: None,
        }
    }

    /// The button's condition, from the same four strings `upgradesLabel`
    /// reads, so the server and the label cannot disagree.
    #[test]
    fn replaceable_means_release_not_package_managed_and_writable() {
        let b = |channel: &str, owner: &str, replaceable: &str| crate::proto::BuildInfo {
            channel: channel.into(), owner: owner.into(), replaceable: replaceable.into(),
            ..Default::default()
        };
        assert_eq!(replaceable_by(&b("release", "cargo-bin", "yes")), Ok(()), "the shell installer");
        assert_eq!(replaceable_by(&b("release", "other", "yes")), Ok(()), "the tarball");
        assert_eq!(replaceable_by(&b("release", "homebrew", "yes")).unwrap_err(), "owned by homebrew");
        assert_eq!(replaceable_by(&b("release", "system-package", "yes")).unwrap_err(), "owned by system-package");
        assert_eq!(replaceable_by(&b("release", "other", "no")).unwrap_err(), "not writable by roost");
        assert_eq!(replaceable_by(&b("release", "other", "unknown")).unwrap_err(), "not writable by roost");
        assert_eq!(replaceable_by(&b("checkout", "other", "yes")).unwrap_err(), "channel checkout");
        assert_eq!(replaceable_by(&b("cargo", "cargo-bin", "yes")).unwrap_err(), "channel cargo");
    }

    /// The five fields beside step 2's two, each from the place it lives:
    /// `offer` from the build and the key, `skipped`/`deferred_until` from
    /// the choices file, `failure`/`installed` from this process.
    #[test]
    fn the_view_carries_the_choices_and_the_last_outcome() {
        let (_g1, _g2, _d) = env_fixture();
        crate::version::write_state_to(&crate::version::state_path(), &checked("999.0.0")).unwrap();
        crate::version::reset_for_test();
        let v = view();
        assert_eq!((v.status.as_str(), v.latest.as_str()), ("newer", "999.0.0"), "step 2's fields are untouched");
        assert!(!v.offer, "this test binary is a checkout without a key: no button");
        assert_eq!((v.skipped.as_str(), v.deferred_until, v.failure.as_str(), v.installed.as_str()), ("", 0, "", ""));

        write_choices_to(&choices_path(), &Choices { skipped: Some("999.0.0".into()), deferred_until: None }).unwrap();
        assert_eq!(view().skipped, "999.0.0");

        let soon = crate::errlog::now_secs() + 100;
        write_choices_to(&choices_path(), &Choices { skipped: None, deferred_until: Some(soon) }).unwrap();
        assert_eq!(view().deferred_until, soon);
        write_choices_to(&choices_path(), &Choices { skipped: None, deferred_until: Some(1) }).unwrap();
        assert_eq!(view().deferred_until, 0, "an expired deferral is not carried");

        record_failure("999.0.0", Phase::Verify, "signature did not verify");
        assert_eq!(view().failure, "verify: signature did not verify");
        // A failure belongs to the version it was for. A new check that names
        // another version drops it: the row must not say "update failed" about
        // a release nobody has tried to install.
        crate::version::write_state_to(&crate::version::state_path(), &checked("999.1.0")).unwrap();
        crate::version::reset_for_test();
        assert_eq!(view().failure, "");

        record_installed("999.1.0");
        assert_eq!(view().installed, "999.1.0");
    }
```

And in `src/config.rs`'s `mod tests`, extending step 2's `the_settings_view_carries_the_update_answer_beside_build_not_inside_it` with one assertion after `assert_eq!(v.update.status, "off", ...)`:

```rust
        assert!(!v.update.offer, "a checkout is never offered the button; the field rides beside status");
        assert!(json["update"].get("offer").is_some(), "the client reads state.settings.update.offer");
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib update::tests::replaceable -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `replaceable_by` in this scope``.

- [ ] **Step 3: The protocol**

In `src/proto.rs`, `Intent`, after `RestoreWorkspace`:

```rust
    /// Download, verify, probe, swap and re-exec the newer release the
    /// version check found. Diverted in `wsconn` before the hub lock, like
    /// `Search`; refused for every shape but the two roost may replace.
    Update,
    /// `Later`: the dialog stops opening by itself for a day. The mark stays.
    DeferUpdate,
    /// `Skip <version>`: neither mark nor dialog for that version again. It
    /// carries the version so a click from a stale page cannot skip one it
    /// never saw. Both are global, not per project — a version is not about
    /// any project — and both push a fresh settings snapshot to every hub.
    SkipUpdate { version: String },
```

`Event`, after `RestoreReport`:

```rust
    /// The update pipeline's progress, to the connection that asked and
    /// nobody else. `phase` is `download`, `verify`, `unpack`, `probe`,
    /// `swap`, `restarting`, `failed` or `refused`; `detail` carries the
    /// message for the last two and the version for `restarting`.
    UpdateProgress { phase: String, detail: String },
```

`UpdateView`, after `pub latest: String,`:

```rust
    /// Whether this copy gets the `[Update]` button: channel `release`,
    /// owner neither Homebrew nor a system package, write probe yes, **and**
    /// a public key compiled in. Decided here so the client never
    /// re-derives it.
    pub offer: bool,
    /// The version the user skipped, or empty. A newer `latest` un-skips.
    pub skipped: String,
    /// When `Later` expires, or 0 when not deferred. The mark stays either way.
    pub deferred_until: u64,
    /// `"<phase>: <message>"` from the last attempt at *this* `latest`, or
    /// empty. Not persisted: an exec is the success case.
    pub failure: String,
    /// The version whose file is in place but whose exec failed, or empty —
    /// the one state where the file and the display disagree, reported.
    pub installed: String,
```

In `src/version.rs`, `view()`'s constructor closure becomes:

```rust
    let mk = |status: &str, latest: &str| crate::proto::UpdateView {
        status: status.to_string(),
        latest: latest.to_string(),
        ..Default::default()
    };
```

- [ ] **Step 4: `hub.rs`: the arms and the broadcast**

After the `RestoreWorkspace` `unreachable!` arm in `Hub::handle`:

```rust
            // Diverted in wsconn like the two above: a download must not run
            // under this lock, and a choice is a file write that every hub
            // then hears about. Named here so a second dispatch site fails
            // loudly.
            Intent::Update | Intent::DeferUpdate | Intent::SkipUpdate { .. } => {
                unreachable!("update intents are diverted in wsconn before this lock is taken")
            }
```

After `broadcast_all`:

```rust
/// Push a fresh settings snapshot to every project's clients after a
/// process-wide fact changed — an update choice, which is not about any
/// project, so every open page must drop or keep its mark at once. Same
/// lock order as `broadcast_all`: registry, then hub, never both.
pub fn broadcast_settings_all() {
    let Some(reg) = REGISTRY.get() else { return };
    let hubs: Vec<Arc<Mutex<Hub>>> = {
        let map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    };
    for h in hubs {
        let mut g = Hub::lock(&h);
        g.settings = None;
        let ev = g.snapshot_event(&String::new());
        g.broadcast(&ev);
    }
}
```

- [ ] **Step 5: `update.rs`: the process facts and `view()`**

```rust
use std::sync::Mutex;

/// The last attempt's failure, for the version it was for. Not persisted:
/// an exec is the success case, and a crash is a different problem.
static LAST_FAILURE: Mutex<Option<(String, Phase, String)>> = Mutex::new(None);
/// The version swapped in when the exec afterwards failed. The one state
/// where the file and the display disagree, so About says "restart roost".
static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

pub fn record_failure(latest: &str, phase: Phase, msg: &str) {
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some((latest.to_string(), phase, msg.to_string()));
}

pub fn record_installed(version: &str) {
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(version.to_string());
}

/// The button's condition, from the same four strings `upgradesLabel` in
/// `dialog.js` reads for "roost can replace this copy" — so the server and
/// the label cannot disagree. Each refusal names why, for the log.
pub fn replaceable_by(b: &crate::proto::BuildInfo) -> Result<(), String> {
    if b.channel != "release" {
        return Err(format!("channel {}", b.channel));
    }
    if b.owner == "homebrew" || b.owner == "system-package" {
        return Err(format!("owned by {}", b.owner));
    }
    if b.replaceable != "yes" {
        return Err("not writable by roost".into());
    }
    Ok(())
}

pub fn replaceable_here() -> Result<(), String> {
    replaceable_by(&crate::config::build_info())
}

/// What the dialog and the About row render from: step 2's two fields, plus
/// the choices and this process's last outcome. Read by
/// `config::settings_view` on every cache miss, which `RequestState` forces
/// and every choice broadcasts.
pub fn view() -> crate::proto::UpdateView {
    let mut v = crate::version::view();
    v.offer = replaceable_here().is_ok() && public_key().is_some();
    let now = crate::errlog::now_secs();
    let c = read_choices_from(&choices_path());
    v.skipped = c.skipped.clone().unwrap_or_default();
    v.deferred_until = if deferred(&c, now) { c.deferred_until.unwrap_or(0) } else { 0 };
    v.installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    v.failure = match &*LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((for_version, phase, msg)) if *for_version == v.latest => format!("{}: {msg}", phase.as_str()),
        _ => String::new(),
    };
    v
}

#[cfg(test)]
pub fn reset_for_test() {
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
```

In `src/config.rs`, `settings_view`'s return expression: replace `update: crate::version::view(),` with `update: crate::update::view(),`.

- [ ] **Step 6: Run the three modules**

Run: `cargo test --lib update:: version:: config:: -- --test-threads=1`
Expected: PASS. Step 2's `the_view_reports_five_states_and_never_calls_unknown_up_to_date` still passes: `version::view()` is unchanged in its two fields.

- [ ] **Step 7: Commit**

```bash
git add src/proto.rs src/version.rs src/config.rs src/hub.rs src/update.rs
git commit -m "Three intents, one event, and the five facts About needs beside latest"
```

---

### Task 8: One flight, to one requester, and the exec nothing else can do

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs` `mod tests`

**Interfaces:**
- Consumes: `Plan`, `run_pipeline`, `Phase`, `FetchBytes` (Task 5), `replaceable_here`, `record_failure`, `record_installed` (Task 7), `public_key`, `download_base`, `asset_urls` (Task 1), `crate::version::{current, verdict, Latest}`, `crate::hub::{Hub, ConnId}`, `crate::proto::Event`, `crate::errlog`
- Produces:
  - `pub type ExecFn = fn(&Path) -> String;`
  - `pub fn exec(exe: &Path) -> String` — returns only on failure
  - `pub fn eligible() -> Result<String, String>` — the newer version, or why not
  - `pub fn start(hub: Arc<Mutex<Hub>>, from: ConnId, fetch: FetchBytes)`
  - `pub fn drive(hub: Arc<Mutex<Hub>>, from: ConnId, plan: Plan, fetch: FetchBytes, exec_fn: ExecFn)`
  - `static IN_FLIGHT: AtomicBool`; `reset_for_test` also clears it

- [ ] **Step 1: Write the failing test**

`start` is refused in every test — the test binary is a checkout — which is exactly the privacy test: the refusal must reach the requester and nobody else, with a second subscriber that receives nothing. `drive` is the part after eligibility, so the guard and the exec-failed state are driven with an injected `Plan` and an injected exec. Deleting the `IN_FLIGHT.swap` fails the single-flight test (Step 6 does it); deleting the `catch_unwind` leaves the guard taken after `a_panicking_fetch_clears_the_guard`.

```rust
    use std::sync::atomic::{AtomicBool as TestFlag, AtomicUsize, Ordering::SeqCst};
    static FETCHES: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: TestFlag = TestFlag::new(false);

    fn blocking_404(url: &str) -> Result<Vec<u8>, String> {
        FETCHES.fetch_add(1, SeqCst);
        while !RELEASE.load(SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(format!("{url}: status code 404"))
    }

    fn panicking_fetch(_url: &str) -> Result<Vec<u8>, String> {
        panic!("the socket thread must not carry this");
    }

    fn no_exec(_exe: &Path) -> String {
        "test: exec not attempted".to_string()
    }

    fn events(rx: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        for _ in 0..400 {
            if f() { return }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("condition not reached in 4s");
    }

    /// Two subscribers, because with one `send_to` and `broadcast` are
    /// indistinguishable. This test binary is a checkout, so `start` refuses
    /// — and the refusal is the requester's alone.
    #[test]
    fn a_refusal_reaches_the_requester_and_nobody_else() {
        let (_g1, _g2, d) = env_fixture();
        let mut h = crate::hub::Hub::new("upd_refuse", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        start(hub, a, fetch_404);
        let got = events(&rx_a);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains(r#""t":"UpdateProgress""#) && got[0].contains(r#""phase":"refused""#), "{}", got[0]);
        assert!(got[0].contains("channel checkout"), "the reason is named: {}", got[0]);
        assert!(events(&rx_b).is_empty(), "one connection's refusal is not everyone's");
    }

    /// A second click anywhere while one runs is answered *already updating*
    /// rather than starting a second download.
    #[test]
    fn two_intents_during_one_run_fetch_once_and_refuse_the_second() {
        let (_g1, _g2, d) = env_fixture();
        FETCHES.store(0, SeqCst);
        RELEASE.store(false, SeqCst);
        let (pk, _) = test_key();
        let mut h = crate::hub::Hub::new("upd_flight", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        drive(hub.clone(), a, plan_in(d.path(), &pk, "9.9.9"), blocking_404, no_exec);
        wait_until(|| FETCHES.load(SeqCst) >= 1);
        drive(hub.clone(), b, plan_in(d.path(), &pk, "9.9.9"), blocking_404, no_exec);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(FETCHES.load(SeqCst), 1, "the guard is taken before the thread starts");
        let got = events(&rx_b);
        assert!(got.iter().any(|e| e.contains(r#""phase":"refused""#) && e.contains("already updating")), "{got:?}");
        RELEASE.store(true, SeqCst);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        let got = events(&rx_a);
        assert!(got.iter().any(|e| e.contains(r#""phase":"download""#)), "{got:?}");
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("404")), "{got:?}");
        assert_eq!(view().failure.split(':').next(), Some("download"), "the failure is kept for About");
        RELEASE.store(false, SeqCst);
    }

    /// CLAUDE.md: no panic may escape a socket or watcher thread — and this
    /// one would also leave the guard taken forever.
    #[test]
    fn a_panicking_fetch_clears_the_guard_and_is_reported_as_a_failure() {
        let (_g1, _g2, d) = env_fixture();
        let (pk, _) = test_key();
        let mut h = crate::hub::Hub::new("upd_panic", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        drive(hub, a, plan_in(d.path(), &pk, "9.9.9"), panicking_fetch, no_exec);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        let got = events(&rx_a);
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("internal")), "{got:?}");
    }

    /// The swap succeeded and the exec did not: the file is the new version,
    /// the process is the old one, and About must say so rather than hide it.
    #[test]
    fn a_swapped_file_whose_exec_fails_is_reported_as_installed() {
        let (_g1, _g2, d) = env_fixture();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let mut h = crate::hub::Hub::new("upd_exec", d.path().to_path_buf());
        let (id, rx) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let exe = plan.exe.clone();
        drive(hub, id, plan, fetch_served, no_exec);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        assert!(String::from_utf8_lossy(&std::fs::read(&exe).unwrap()).contains("roost 9.9.9"), "the file was swapped");
        let got = events(&rx);
        assert!(got.iter().any(|e| e.contains(r#""phase":"restarting""#)), "{got:?}");
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("exec: test: exec not attempted")), "{got:?}");
        assert_eq!(view().installed, "9.9.9");
        assert_eq!(view().failure, "", "an exec failure is `installed`, not `failure`: the file is right");
    }
```

Extend `reset_for_test` to also do `IN_FLIGHT.store(false, Ordering::SeqCst);`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib update:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `start` in this scope``.

- [ ] **Step 3: Write the implementation**

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use crate::hub::{ConnId, Hub};
use crate::proto::Event;

/// One update for the whole process. Taken on the calling thread before the
/// pipeline thread starts, so two clicks in the same instant fetch once.
/// Cleared on every path except a successful exec, where the process is
/// about to be replaced.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

pub type ExecFn = fn(&Path) -> String;

/// Replace this process with the file at `exe`, same PID, same arguments,
/// same environment: `ROOST_ROOTS`, `ROOST_STATE_DIR`, `ROOST_BIND_ALL` and
/// the port argument ride along unchanged. systemd sees nothing;
/// `KillMode=process` is irrelevant; the dtach masters stay children of the
/// same PID; a hand-run roost keeps its terminal. The listening socket needs
/// no hand-off: Rust opens sockets close-on-exec, and the new process binds
/// the same port a few milliseconds later. Returns only on failure.
pub fn exec(exe: &Path) -> String {
    use std::os::unix::process::CommandExt;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(exe).args(args).exec();
    err.to_string()
}

/// The three checks, in the order a user would want to hear about them:
/// is this copy roost's to replace, does this build carry a key, and is the
/// last check's version actually newer than this one. `Ok` carries the
/// version to install.
pub fn eligible() -> Result<String, String> {
    replaceable_here()?;
    if public_key().is_none() {
        return Err("this build carries no release key".into());
    }
    let s = crate::version::current().ok_or("no version check has run yet")?;
    let latest = s.latest.ok_or("the last check did not name a version")?;
    match crate::version::verdict(env!("CARGO_PKG_VERSION"), Some(&latest)) {
        crate::version::Latest::Newer(v) => Ok(v),
        _ => Err(format!("{latest} is not newer than {}", env!("CARGO_PKG_VERSION"))),
    }
}

fn progress(phase: &str, detail: &str) -> Event {
    Event::UpdateProgress { phase: phase.to_string(), detail: detail.to_string() }
}

fn log(text: &str) {
    crate::errlog::record(&format!("update: {text}"), crate::errlog::now_secs());
}

/// The `Update` intent. Decides, then hands off to `drive` with the real
/// fetch and the real exec. Nothing here holds the hub lock for longer than
/// one `send_to`.
pub fn start(hub: Arc<Mutex<Hub>>, from: ConnId, fetch: FetchBytes) {
    let latest = match eligible() {
        Ok(v) => v,
        Err(why) => {
            log(&format!("refused: {why}"));
            Hub::lock(&hub).send_to(&from, &progress("refused", &why));
            return;
        }
    };
    // Captured before anything else: after the rename, /proc/self/exe names
    // the deleted inode with " (deleted)" appended, and an exec from it fails.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            let why = format!("this executable's path could not be read: {e}");
            log(&why);
            Hub::lock(&hub).send_to(&from, &progress("refused", &why));
            return;
        }
    };
    let b = crate::config::build_info();
    let (tarball_url, sig_url) = asset_urls(&download_base(&b.repository, &latest), &b.target);
    let plan = Plan {
        exe,
        tarball_url,
        sig_url,
        pubkey: public_key().unwrap_or_default().to_string(),
        want: latest,
        probe_timeout: std::time::Duration::from_secs(PROBE_SECS),
    };
    drive(hub, from, plan, fetch, exec);
}

/// The pipeline on a detached thread, reporting each phase to `from` alone.
/// Split from `start` so a test can drive it with a `Plan` pointing at a
/// tempdir and an exec that does not replace the test runner.
pub fn drive(hub: Arc<Mutex<Hub>>, from: ConnId, plan: Plan, fetch: FetchBytes, exec_fn: ExecFn) {
    // Before the thread, deliberately: a guard taken inside it would let
    // two clicks in the same instant both start.
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        Hub::lock(&hub).send_to(&from, &progress("refused", "already updating"));
        return;
    }
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    log(&format!("starting: {} -> {}", plan.tarball_url, plan.exe.display()));
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut report = |p: Phase| {
                log(p.as_str());
                Hub::lock(&hub).send_to(&from, &progress(p.as_str(), ""));
            };
            run_pipeline(&plan, fetch, &mut report)
        }));
        match outcome {
            Ok(Ok(())) => {
                log(&format!("swapped {} for {}; exec", plan.exe.display(), plan.want));
                Hub::lock(&hub).send_to(&from, &progress("restarting", &plan.want));
                // A moment for the frame to leave the socket before the
                // process that holds it is replaced.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let err = exec_fn(&plan.exe);
                // Only reached when exec returned, which it does only on
                // failure: the new file is in place, this process is not it.
                log(&format!("exec failed: {err}"));
                record_installed(&plan.want);
                IN_FLIGHT.store(false, Ordering::SeqCst);
                Hub::lock(&hub).send_to(&from, &progress("failed", &format!("exec: {err}")));
            }
            Ok(Err((phase, msg))) => {
                log(&format!("failed at {}: {msg}", phase.as_str()));
                record_failure(&plan.want, phase, &msg);
                IN_FLIGHT.store(false, Ordering::SeqCst);
                Hub::lock(&hub).send_to(&from, &progress("failed", &format!("{}: {msg}", phase.as_str())));
            }
            Err(_) => {
                log("the pipeline panicked");
                record_failure(&plan.want, Phase::Internal, "the updater panicked");
                IN_FLIGHT.store(false, Ordering::SeqCst);
                Hub::lock(&hub).send_to(&from, &progress("failed", "internal: the updater panicked"));
            }
        }
    });
}
```

- [ ] **Step 4: Run it to verify it passes, and time it**

Run: `time cargo test --lib update:: -- --test-threads=1`
Expected: PASS, 23 tests. A deadlock hangs rather than fails; the module should finish in well under a minute.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "One update at a time, reported to the one who asked, and a thread nothing escapes"
```

- [ ] **Step 6: Revert the guard and watch single-flight fail**

Delete the `if IN_FLIGHT.swap(true, Ordering::SeqCst) { ... return; }` block from `drive`. Run:
`cargo test --lib update::tests::two_intents -- --test-threads=1`
Expected: FAIL at `the guard is taken before the thread starts` — `left: 2, right: 1`.

Then move the swap *inside* the spawned closure and re-run: expected FAIL the same way. Restore, re-run, expected PASS, and record it above the test:

```rust
    /// Revert-checked twice. Removing the guard fails with `left: 2, right: 1`;
    /// moving it inside the spawned closure fails identically, because both
    /// threads reach the swap before either has set it. Both restored —
    /// which is why the guard is taken on the calling thread.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/update.rs
git commit -m "Record where the update's in-flight guard has to be taken"
```

---

### Task 9: The socket divert, and the choices over a real websocket

**Files:**
- Modify: `src/wsconn.rs` (the read loop, after the `RestoreWorkspace` divert at `~354-380`), `src/update.rs`
- Create: `tests/update.rs`
- Test: `tests/update.rs`

**Interfaces:**
- Consumes: `start`, `http_get_bytes` (Tasks 5, 8), `defer_in`, `skip_in`, `choices_path` (Task 6), `crate::hub::broadcast_settings_all()` (Task 7), `crate::version::current()`
- Produces:
  - `pub fn apply_defer(now: u64) -> Result<(), String>`
  - `pub fn apply_skip(version: &str) -> Result<(), String>`

- [ ] **Step 1: Write the failing test**

Over a real websocket, two pages open. `Later` on one must reach both as a fresh snapshot with `deferred_until` set, and a stale `Skip` must come back as an `Error` to its sender. Deleting the divert makes the first intent hit the `unreachable!` arm and take the socket thread down, which fails the test at `read_until`'s 15 s deadline — a hang-shaped failure, so watch the clock.

```rust
//! The update choices over a real websocket: `Later` reaches every page,
//! and a stale `Skip` is refused to its sender. #65 step 4.
mod common;
use common::*;
use tungstenite::Message;

fn update_of(state_json: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(state_json).unwrap();
    v["ws"]["settings"]["update"].clone()
}

#[test]
fn later_reaches_every_page_and_a_stale_skip_is_refused_to_its_sender() {
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let mut a = ws_connect(port, Some(&origin)).unwrap();
    let mut b = ws_connect(port, Some(&origin)).unwrap();
    let first = read_until(&mut a, r#""t":"State""#);
    let _ = read_until(&mut b, r#""t":"State""#);
    assert_eq!(update_of(&first)["deferred_until"], 0, "nothing deferred yet");

    a.send(Message::Text(r#"{"t":"DeferUpdate"}"#.into())).unwrap();
    let sa = update_of(&read_until(&mut a, r#""t":"State""#));
    let sb = update_of(&read_until(&mut b, r#""t":"State""#));
    assert!(sa["deferred_until"].as_u64().unwrap() > 0, "the sender's page sees the deferral: {sa}");
    assert_eq!(sa["deferred_until"], sb["deferred_until"], "and so does every other page, at once");

    // A version no check has seen: refused, named, and only to the sender.
    a.send(Message::Text(r#"{"t":"SkipUpdate","version":"1.2.3"}"#.into())).unwrap();
    let err = read_until(&mut a, r#""t":"Error""#);
    assert!(err.contains("1.2.3 is not the version the last check saw"), "{err}");
    // No snapshot was broadcast for a refused skip: b's next message is
    // the answer to its own RequestState, and its skip is still empty.
    b.send(Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    let sb = update_of(&read_until(&mut b, r#""t":"State""#));
    assert_eq!(sb["skipped"], "", "{sb}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test update -- --test-threads=1`
Expected: FAIL — `DeferUpdate` reaches `Hub::handle`'s `unreachable!` arm, the socket thread panics, and `read_until` times out: `no message containing "t":"State" within 15s` (or similar, from `tests/common/mod.rs:351`).

- [ ] **Step 3: The wrappers, and the divert**

In `src/update.rs`:

```rust
/// `Later`, then every hub hears it: the mark is on every project page.
pub fn apply_defer(now: u64) -> Result<(), String> {
    defer_in(&choices_path(), now)?;
    crate::hub::broadcast_settings_all();
    Ok(())
}

/// `Skip <version>`, checked against the last check's fact. A mismatch is
/// an error to the sender and a line in the log, and nothing is broadcast:
/// no page's mark changed.
pub fn apply_skip(version: &str) -> Result<(), String> {
    let latest = crate::version::current().and_then(|s| s.latest);
    if let Err(e) = skip_in(&choices_path(), latest.as_deref(), version) {
        log(&format!("skip refused: {e}"));
        return Err(e);
    }
    crate::hub::broadcast_settings_all();
    Ok(())
}
```

In `src/wsconn.rs`, immediately after the `RestoreWorkspace` divert block (before `let dirty = { let mut h = Hub::lock(&hub); ...`):

```rust
                // Diverted like Search and RestoreWorkspace: an update is a
                // download and two probes, a choice is a file write that
                // every hub then hears about, and the reply must reach only
                // this connection. `update::start` takes the hub lock itself,
                // briefly, once per phase; nothing here holds it.
                match &decoded {
                    Ok(proto::Intent::Update) => {
                        crate::update::start(hub.clone(), id.clone(), crate::update::http_get_bytes);
                        continue;
                    }
                    Ok(proto::Intent::DeferUpdate) => {
                        if let Err(msg) = crate::update::apply_defer(crate::errlog::now_secs()) {
                            Hub::lock(&hub).send_to(&id, &proto::Event::Error { msg });
                        }
                        continue;
                    }
                    Ok(proto::Intent::SkipUpdate { version }) => {
                        if let Err(msg) = crate::update::apply_skip(version) {
                            Hub::lock(&hub).send_to(&id, &proto::Event::Error { msg });
                        }
                        continue;
                    }
                    _ => {}
                }
```

- [ ] **Step 4: Run the suite, and time it**

Run: `time cargo test -- --test-threads=1`
Expected: PASS. Compare the wall time with a run from before this branch: a deadlock hangs rather than fails.

- [ ] **Step 5: Commit**

```bash
git add src/wsconn.rs src/update.rs tests/update.rs
git commit -m "The update intents are diverted before the lock, and a choice reaches every page"
```

---

### Task 10: The mark, the dialog, and the button in About

**Files:**
- Modify: `src/render.rs:1882` (after `#connstate`), `src/render.rs:1951` (before `<dialog id="dlg-settings"`), `src/render.rs:3437` (the shell test's id list); `static/style.css` (after `#connstate`, `style.css:1140-1141`); `static/app.js` (`followSettings` at `~4204`, `onEvent`'s `switch` after `case "RestoreReport"`, `connectControl`'s `onopen` at `~541`, the settings button handler at `~3987`); `static/dialog.js` (`latestLabel` from step 2, `renderAbout`'s `latest` arm, `settingsOpen = {` at `~855`, and a new `openUpdate` after `openSettings`)
- Test: `tests/browser/update.mjs` (Task 11) — no Rust test reaches these files, except the shell test in `render.rs`

**Interfaces:**
- Consumes: `state.settings.update` (`UpdateView`, Task 7), `state.settings.build`, `send`, `runDialog`, `upgradeCommand`, `settingsOpen`
- Produces: `function updateWanted(s)`, `function renderUpdateMark()`, `let updateOpen`, `let reloadOnReconnect` in `app.js`; `function openUpdate(settings)` in `dialog.js`; `settingsOpen.close()`

- [ ] **Step 1: The shell**

In `src/render.rs`, after `<span id="connstate" hidden></span>`:

```html
  <button id="updmark" title="" hidden></button>
```

Before `<dialog id="dlg-settings"`:

```html
<dialog id="dlg-update" class="roost">
  <h2 class="dlg-title"></h2>
  <div class="dlg-body"></div>
  <pre class="dlg-cmd" hidden></pre>
  <p class="dlg-progress" hidden></p>
  <div class="dlg-buttons">
    <button type="button" class="dlg-skip"></button>
    <button type="button" class="dlg-later">Later</button>
    <button type="button" class="dlg-ok"></button>
  </div>
</dialog>
```

In `the_workspace_page_ships_empty_dialog_shells` (`render.rs:3437`), add `"dlg-update"` to the id list, and one assertion: `assert!(html.contains(r#"<button id="updmark" title="" hidden></button>"#), "the mark ships hidden and empty");`.

In `static/style.css`, after `#connstate[data-state="offline"]`:

```css
/* The update mark: quiet, beside the chips, on every project page and not
   on the overview. Clicking it opens the update dialog. */
#updmark { height: 24px; padding: 0 8px; border-radius: 6px; border: 1px solid var(--accent);
           background: none; color: var(--accent); cursor: pointer; font: inherit; white-space: nowrap; }
#updmark:hover { background: var(--tool); }
.dlg-cmd { font-family: var(--mono); font-size: 12px; background: var(--tool); padding: 8px; border-radius: 6px;
           white-space: pre-wrap; user-select: all; }
.dlg-progress { color: var(--muted); }
```

- [ ] **Step 2: `app.js` — the mark, the auto-open, the progress, the reload**

Near `let settingsOpen = null;` (`app.js:~4135`):

```js
// ---- the update mark and dialog (#65 step 4) ----
// dialog.js assigns `updateOpen` while its dialog is open, the way it
// assigns `settingsOpen`; app.js calls its hook when UpdateProgress lands.
let updateOpen = null;
// The dialog opens by itself once per page load, when the mark first
// appears and the choice is not deferred. Later clicks of the mark reopen it.
let updateOffered = false;
// Set when the server said "restarting": the next successful control
// reconnect is the new process, and the page reloads to learn its version.
let reloadOnReconnect = false;

// The mark's rule. A checkout never gets one: About already says "yours to
// rebuild", and a developer on a branch is behind a release by design.
function updateWanted(s) {
  const u = (s && s.update) || {};
  const b = (s && s.build) || {};
  return u.status === "newer" && !!u.latest && u.skipped !== u.latest && b.channel !== "checkout";
}

function renderUpdateMark() {
  const el = document.getElementById("updmark");
  if (!el || !state || !state.settings) return;
  const s = state.settings;
  const u = s.update || {};
  const show = updateWanted(s);
  el.hidden = !show;
  if (show) {
    el.textContent = `↑ ${u.latest}`;
    el.title = `roost ${u.latest} is available — you are running ${(s.build || {}).version}`;
  }
  const now = Math.floor(Date.now() / 1000);
  if (show && !updateOffered && !(u.deferred_until > now) && typeof openUpdate === "function") {
    updateOffered = true;
    openUpdate(s);
  }
}
```

In `followSettings()`, after the `settingsOpen.onSnapshot` block: `renderUpdateMark();`.

Beside the settings button handler (`app.js:~3987`):

```js
const updmark = document.getElementById("updmark");
if (updmark) {
  updmark.onclick = () => {
    if (state && state.settings && typeof openUpdate === "function") openUpdate(state.settings);
  };
}
```

In `onEvent`'s `switch`, after `case "RestoreReport"`:

```js
case "UpdateProgress":
  if (ev.phase === "restarting") reloadOnReconnect = true;
  if (ev.phase === "failed") reloadOnReconnect = false;
  if (updateOpen) {
    try { updateOpen.onProgress(ev); } catch (e) { console.error("roost: the update dialog's onProgress threw", e); updateOpen = null; }
  }
  break;
```

In `connectControl`, at the top of `sock.onopen`, after the `if (ctrl !== sock) return;` guard:

```js
    // The server said it was restarting and this is the first socket the
    // new process accepted. Reload, so the page learns the new version the
    // way every client does — the dialog reads "restarting roost…" until
    // then, not "reconnecting".
    if (reloadOnReconnect) { location.reload(); return; }
```

- [ ] **Step 3: `dialog.js` — `latestLabel`, the About button, `openUpdate`**

Replace step 2's `latestLabel` with:

```js
/// What About says about newer versions, from `state.settings.update`.
///
/// Step 2's five renderings, and three more from step 4 in order of
/// precedence: a swapped file whose exec failed ("restart roost to run"),
/// a skipped version, and a failed attempt appended to whatever the row
/// would otherwise say. An unknown status still renders as "could not
/// check": a status this build does not recognise is one more way of not
/// knowing.
function latestLabel(u) {
  const v = u || {};
  if (v.installed) return `restart roost to run ${v.installed}`;
  let text;
  switch (v.status) {
    case "newer": text = v.skipped === v.latest ? `${v.latest} skipped` : `${v.latest} available`; break;
    case "up-to-date": text = "up to date"; break;
    case "never": text = "not checked yet"; break;
    case "off": text = "version checks are off"; break;
    default: text = "could not check";
  }
  if (v.failure) text += ` (update failed: ${v.failure})`;
  return text;
}
```

In `renderAbout`, where the cell is filled (`} else { cell.textContent = v; }`), add a branch before the final `else`:

```js
    } else if (kind === "latest") {
      cell.textContent = v;
      // The action lives where the wondering happens. One dialog at a time,
      // so the settings dialog closes first, through the hook the open
      // dialog exposes; `finish` is not in scope here.
      if (u.offer && u.status === "newer" && u.skipped !== u.latest && !u.installed) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "dlg-upd";
        btn.textContent = u.failure ? "Retry" : "Update";
        btn.onclick = () => {
          const so = settingsOpen;
          if (so && so.close) so.close();
          openUpdate(view);
        };
        cell.appendChild(btn);
      }
    } else {
```

In the `settingsOpen = {` object (`dialog.js:~855`), add:

```js
      close() { finish(false); },
```

After `openSettings`, the new dialog:

```js
/// The update dialog: `roost 0.5.3 is available — you are running 0.5.2`.
/// Update / Later / Skip for a copy roost may replace; the command About
/// already shows, as copyable text, plus Later / Skip for every other
/// shape. A checkout never gets here (`updateWanted` in app.js).
///
/// Escape closes it and nothing else: a running pipeline continues, the
/// mark stays, and the row shows the outcome. After "restarting" the page
/// reloads on the next control reconnect (app.js), so this dialog is the
/// last thing the old process paints.
function openUpdate(settings) {
  const el = document.getElementById("dlg-update");
  const u = (settings && settings.update) || {};
  const b = (settings && settings.build) || {};
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = `roost ${u.latest} is available — you are running ${b.version}`;
    const body = el.querySelector(".dlg-body");
    body.replaceChildren();
    const cmd = el.querySelector(".dlg-cmd");
    const progress = el.querySelector(".dlg-progress");
    progress.hidden = true;
    progress.textContent = "";
    const ok = el.querySelector(".dlg-ok");
    const later = el.querySelector(".dlg-later");
    const skip = el.querySelector(".dlg-skip");
    skip.textContent = `Skip ${u.latest}`;
    ok.disabled = false; later.disabled = false; skip.disabled = false;
    const p = document.createElement("p");
    if (u.offer) {
      cmd.hidden = true;
      ok.hidden = false;
      ok.textContent = u.failure ? "Retry" : "Update";
      p.textContent = "roost downloads the release, verifies its signature, checks that it runs, swaps the file and restarts itself. Terminals survive; this page reloads.";
      if (u.failure) { progress.hidden = false; progress.textContent = `update failed: ${u.failure}`; }
    } else {
      ok.hidden = true;
      const lines = upgradeCommand(b) || [];
      cmd.hidden = lines.length === 0;
      cmd.textContent = lines.join("\n");
      p.textContent = lines.length
        ? "This copy is upgraded by whatever installed it. Run:"
        : "This copy is upgraded by whatever installed it.";
    }
    body.appendChild(p);
    ok.onclick = () => {
      ok.disabled = true; later.disabled = true; skip.disabled = true;
      progress.hidden = false;
      progress.textContent = "starting…";
      send({ t: "Update" });
    };
    later.onclick = () => { send({ t: "DeferUpdate" }); finish("later"); };
    skip.onclick = () => { send({ t: "SkipUpdate", version: u.latest }); finish("skip"); };
    updateOpen = {
      onProgress(ev) {
        progress.hidden = false;
        if (ev.phase === "failed") {
          progress.textContent = `update failed: ${ev.detail}`;
          ok.disabled = false; ok.textContent = "Retry";
          later.disabled = false; skip.disabled = false;
        } else if (ev.phase === "refused") {
          progress.textContent = ev.detail === "already updating" ? "already updating" : `not updated: ${ev.detail}`;
          later.disabled = false; skip.disabled = false;
        } else if (ev.phase === "restarting") {
          progress.textContent = "restarting roost…";
        } else {
          progress.textContent = `${ev.phase}…`;
        }
      },
    };
    return () => (u.offer ? ok : later).focus();
  }, "dismissed").then((v) => { updateOpen = null; return v; });
}
```

- [ ] **Step 4: Run the shell test, then check it by hand**

Run: `cargo test --lib render::tests::the_workspace_page_ships -- --test-threads=1`
Expected: PASS.

CLAUDE.md: verify UI behavior in a real browser before believing it works, and never attach automation to the live instance. Start `./scripts/testroost.sh`, and before opening it, plant a fresh check file so the status is `newer`:

```bash
mkdir -p ~/.local/state/roost-test/update
printf '{"latest":"999.0.0","checked_at":%d,"failed_at":null}\n' "$(date +%s)" > ~/.local/state/roost-test/update/check.json
printf 'version_check = true\n' > ~/.local/state/roost-test/config.toml
```

Open the scratch instance. A checkout build shows **no** mark (correct). In the console run `Object.assign(state.settings.build, {channel:"release", owner:"cargo-bin", replaceable:"yes"}); updateOffered = false; renderUpdateMark()` and confirm: the mark `↑ 999.0.0` appears beside the chips, the dialog opens by itself with both versions in its title and the command variant (no `Update` button, since `offer` is false). Click `Later`: the dialog closes, the mark stays, and `~/.local/state/roost-test/update/choices.json` holds a `deferred_until`. Click the mark, then `Skip 999.0.0`: the mark goes, and About's `Latest` row reads `999.0.0 skipped`. Paste what you saw into the commit body.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs static/style.css static/app.js static/dialog.js
git commit -m "A mark in the header, a dialog with three answers, and a button where the wondering happens"
```

---

### Task 11: The mark, the dialog and the row, in a real browser

**Files:**
- Create: `tests/browser/update.mjs`
- Modify: `tests/browser/README.md` (the run list, and the revert-check log)

**Interfaces:**
- Consumes: `fixture`, `freePort`, `openPage`, `profileDir`, `sleep`, `startBrowser`, `startRoost`, `until` from `./harness.mjs`; `state`, `renderUpdateMark`, `updateOffered`, `latestLabel`, `openUpdate` from the page

The harness turns the version check off (step 2). This test turns it **on** with its own `ROOST_CONFIG`, and plants a *fresh* `check.json` naming `999.0.0` so the status is `newer` and no request fires; section A asserts the file is byte-identical afterwards, which is what says no check ran. The test binary is a checkout, so the *real* mark stays hidden and the *real* `offer` is false; the mark and the button are driven through the real renderers with `state.settings.build` mutated in place, about.mjs's technique. `Later`, `Skip` and the un-skip on a newer version go through the real server.

- [ ] **Step 1: Write the test**

```js
//! The update mark, the dialog, Later, Skip, and the About row. #65 step 4.
//!
//! Run: deno run -A tests/browser/update.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// The check is ON here, with a planted, fresh answer: status "newer" without
// a request. The harness's own config would say "off" and hide everything.
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "version_check = true\n");
const checkFile = `${fx.stateDir}/update/check.json`;
const plant = async (latest) => {
  await Deno.mkdir(`${fx.stateDir}/update`, { recursive: true });
  await Deno.writeTextFile(checkFile,
    JSON.stringify({ latest, checked_at: Math.floor(Date.now() / 1000), failed_at: null }));
};
await plant("999.0.0");
const planted = await Deno.readTextFile(checkFile);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
                                 extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
let page;

const readRows = (evalIn) => evalIn(`(() => {
  const out = {};
  for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
    out[r.querySelector("label").textContent] = r.querySelector(".aboutval").textContent.trim();
  }
  return out; })()`);

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");

  console.log("A. the server sent newer, from the planted file, without a request");
  const u = await evalIn(`state.settings.update`);
  ok(u && u.status === "newer" && u.latest === "999.0.0", `status=${u && u.status} latest=${u && u.latest}`);
  ok(u && u.offer === false, "a checkout is not offered the button");
  await sleep(1500);
  ok((await Deno.readTextFile(checkFile)) === planted, "check.json is byte-identical: no check ran");

  console.log("B. a checkout gets no mark; a replaceable copy does, and the dialog opens itself once");
  ok(await evalIn(`document.getElementById("updmark").hidden`), "no mark for a checkout");
  ok(!(await evalIn(`document.getElementById("dlg-update").open`)), "and no dialog");
  // The real renderer, on a build mutated in place — the about.mjs technique.
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); updateOffered = false; renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "the mark shows for a replaceable copy");
  ok((await evalIn(`document.getElementById("updmark").textContent`)) === "↑ 999.0.0", "and names the version");
  ok(await until(() => evalIn(`document.getElementById("dlg-update").open`), 5, "auto-open"),
     "the dialog opened by itself");
  const title = await evalIn(`document.querySelector("#dlg-update .dlg-title").textContent`);
  const running = await evalIn(`state.settings.build.version`);
  ok(title === `roost 999.0.0 is available — you are running ${running}`, `the title names both versions (${title})`);
  ok(await evalIn(`document.querySelector("#dlg-update .dlg-ok").hidden`), "offer is false: no Update button");
  ok(!(await evalIn(`document.querySelector("#dlg-update .dlg-cmd").hidden`)), "the command is shown instead");
  ok((await evalIn(`document.querySelector("#dlg-update .dlg-skip").textContent`)) === "Skip 999.0.0", "Skip names the version");
  await evalIn(`renderUpdateMark(); 0`);
  ok(await evalIn(`document.getElementById("dlg-update").open`), "a second render does not open a second dialog");

  console.log("C. Later: the dialog closes, the mark stays, the choice is on disk and in every snapshot");
  await evalIn(`document.querySelector("#dlg-update .dlg-later").click()`);
  ok(await until(() => evalIn(`!document.getElementById("dlg-update").open`), 5, "close"), "the dialog closed");
  ok(await until(() => evalIn(`state.settings.update.deferred_until > 0`), 5, "deferred"),
     "the snapshot carries the deferral");
  const choices = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/update/choices.json`));
  ok(choices.deferred_until > Math.floor(Date.now() / 1000), `choices.json holds it (${choices.deferred_until})`);
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "the mark stays after Later");
  ok(!(await evalIn(`document.getElementById("dlg-update").open`)), "and the dialog does not reopen by itself");

  console.log("D. Skip: the mark goes, the row says skipped, and a newer version brings it back");
  await evalIn(`document.getElementById("updmark").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-update").open`), 5, "reopen"), "clicking the mark reopens the dialog");
  await evalIn(`document.querySelector("#dlg-update .dlg-skip").click()`);
  ok(await until(() => evalIn(`state.settings.update.skipped === "999.0.0"`), 5, "skipped"), "the snapshot carries the skip");
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(await evalIn(`document.getElementById("updmark").hidden`), "the mark is gone for the skipped version");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "settings"), "settings opens");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  let rows = await readRows(evalIn);
  ok(rows.Latest === "999.0.0 skipped", `the row reads skipped (${JSON.stringify(rows.Latest)})`);
  await evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click()`);

  // A newer release un-skips by itself. The check's answer is read from
  // disk once per process, so the new fact needs a restart to be seen.
  await plant("999.1.0");
  ok(await roost.restart(), "roost restarted on the same state dir");
  ok(await until(() => evalIn("ctrl && ctrl.readyState === 1 && state.settings.update.latest === '999.1.0'"), 40, "reconnect"),
     "the page reconnected and sees 999.1.0");
  ok((await evalIn(`state.settings.update.skipped`)) === "999.0.0", "the old skip is still recorded");
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "and the mark is back for the newer version");
  ok((await evalIn(`document.getElementById("updmark").textContent`)) === "↑ 999.1.0", "naming it");
  await evalIn(`document.getElementById("dlg-update").dispatchEvent(new Event("cancel", { cancelable: true })); 0`);

  console.log("E. the About button, and the row's three extra renderings");
  await evalIn(`state.settings.update.offer = true; 0`);
  await evalIn(`document.getElementById("settings").click()`);
  await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "settings");
  await sleep(300); // let RequestState's answer land first: it replaces state.settings wholesale
  await evalIn(`state.settings.update.offer = true; 0`);
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  const btn = await evalIn(`(() => { const r = [...document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")]
    .find((x) => x.querySelector("label").textContent === "Latest");
    const b = r && r.querySelector("button.dlg-upd"); return b ? b.textContent : null; })()`);
  ok(btn === "Update", `the Latest row carries the button when offered (${JSON.stringify(btn)})`);
  await evalIn(`[...document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")]
    .find((x) => x.querySelector("label").textContent === "Latest").querySelector("button.dlg-upd").click()`);
  ok(await until(() => evalIn(`!document.getElementById("dlg-settings").open && document.getElementById("dlg-update").open`), 5, "swap"),
     "the button closes settings and opens the update dialog");
  ok(!(await evalIn(`document.querySelector("#dlg-update .dlg-ok").hidden`)), "which now has an Update button");

  // The real intent, from a checkout: refused, and the refusal reaches this
  // dialog as a progress line rather than a silent nothing.
  await evalIn(`document.querySelector("#dlg-update .dlg-ok").click()`);
  ok(await until(() => evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent.startsWith("not updated: channel checkout")`), 5, "refused"),
     `a checkout's Update is refused by name (${await evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent`)})`);
  await evalIn(`document.getElementById("dlg-update").dispatchEvent(new Event("cancel", { cancelable: true })); 0`);

  const label = async (x) => await evalIn(`latestLabel(${JSON.stringify(x)})`);
  ok((await label({ status: "newer", latest: "1.0.0", skipped: "1.0.0" })) === "1.0.0 skipped", "skipped");
  ok((await label({ status: "newer", latest: "1.0.0", skipped: "0.9.0" })) === "1.0.0 available", "an older skip does not apply");
  ok((await label({ status: "newer", latest: "1.0.0", failure: "verify: signature did not verify" })) === "1.0.0 available (update failed: verify: signature did not verify)", "a failure is appended");
  ok((await label({ status: "up-to-date", latest: "1.0.0", installed: "1.0.0" })) === "restart roost to run 1.0.0", "installed wins over everything");
  ok((await label({ status: "off" })) === "version checks are off", "step 2's renderings are untouched");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
```

- [ ] **Step 2: Run it**

Run: `deno run -A tests/browser/update.mjs`
Expected: PASS, 36 assertions. A host with no Chromium prints `SKIP: no chromium found.` and exits 0.

- [ ] **Step 3: Revert three things and watch it fail**

Apply each, run, read the failure, restore:

1. In `static/app.js`, drop the `b.channel !== "checkout"` clause from `updateWanted`.
   Expected: FAIL 1 — section B's `no mark for a checkout`.
2. In `static/dialog.js`, make `openUpdate`'s `skip.onclick` send `DeferUpdate` instead of `SkipUpdate`.
   Expected: FAIL 4 or more, starting at section D's `the snapshot carries the skip` (a 5 s wait), then the mark, the row, and the un-skip.
3. In `tests/browser/update.mjs`, plant `checked_at` 25 hours in the past instead of now.
   Expected: FAIL 1 — section A's `check.json is byte-identical: no check ran` — because the stale file makes the server fire a real request on connect, which rewrites the file. This is the assertion that proves the fixture keeps the suite off the network; do not leave it reverted.

- [ ] **Step 4: Record it in the README**

Add to `tests/browser/README.md`'s run list:

```
deno run -A tests/browser/update.mjs     # the update mark, dialog, Later/Skip, and the About row's button (#65 step 4); plants a fresh check.json so no request fires
```

and to the revert-check log:

```
- In `update.mjs`: dropping the checkout clause from `updateWanted` fails 1
  (a checkout must never see the mark); sending `DeferUpdate` from the Skip
  button fails 4 from section D on. Planting a *stale* check file instead of
  a fresh one fails section A's byte-identical assertion: the server fires a
  real request on connect and rewrites the file. That assertion is the one
  that keeps this file off the network, and it would pass silently with the
  check turned off — which is why the file turns it on and plants an answer
  instead.
```

- [ ] **Step 5: Commit**

```bash
git add tests/browser/update.mjs tests/browser/README.md
git commit -m "A browser test for the mark, the dialog, and what Later and Skip do"
```

---

### Task 12: Signing, on the release

**Files:**
- Create: `.github/workflows/sign-release-artifacts.yml`
- Modify: `.github/workflows/release.yml` (a job after `custom-build-linux-packages` at `~223-230`; `host`'s `needs` at `~233` and `if` at `~239`), `dist-workspace.toml:~101` (`global-artifacts-jobs`), `docs/roost-packaging-handover.md` (item 6.5, `~494-518`)
- Test: none can run — see *What a green suite cannot see*. `dist plan` must still succeed.

**Interfaces:**
- Consumes: the `release` environment; `artifacts-*` bundles
- Produces: `artifacts-signatures`, one `roost-<target>.tar.xz.minisig` per tarball, attached to the release by `host`

- [ ] **Step 1: The reusable workflow**

```yaml
# Signs every release tarball with the maintainer's minisign key, so a running
# roost can verify a download offline before it replaces itself (#65 step 4,
# src/update.rs). In the global phase beside build-linux-packages: the first
# point at which all four tarballs exist.
name: Sign release artifacts

on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string

jobs:
  sign-release-artifacts:
    runs-on: ubuntu-22.04
    # ROOST_MINISIGN_KEY is an *environment* secret, for the reason release.yml
    # gives on publish-homebrew-formula: a repo secret is readable by any
    # workflow file on any ref, and a pull request supplies its own copies.
    # The environment allows only tags v*/*.*.* and master, so refs/pull/N/merge
    # is refused before the job starts — and the caller gates on publishing so a
    # PR run skips this job instead of showing it refused.
    environment: release
    steps:
      - uses: actions/download-artifact@v8
        with:
          pattern: artifacts-*
          path: dist-in/
          merge-multiple: true

      - name: Install minisign
        run: sudo apt-get update -q && sudo apt-get install -y -q minisign

      # The key is unencrypted (generated with `minisign -G -W`): the C CLI
      # has no non-interactive password option, and a password stored beside
      # the key in the same environment would protect nothing.
      - name: Sign every tarball
        env:
          ROOST_MINISIGN_KEY: ${{ secrets.ROOST_MINISIGN_KEY }}
        run: |
          set -euo pipefail
          umask 077
          printf '%s\n' "$ROOST_MINISIGN_KEY" > "$RUNNER_TEMP/minisign.key"
          VERSION=$(echo '${{ inputs.plan }}' | jq -r '.releases[0].app_version')
          mkdir -p out
          for f in dist-in/roost-*.tar.xz; do
            name=$(basename "$f")
            minisign -S -s "$RUNNER_TEMP/minisign.key" -m "$f" -x "out/$name.minisig" \
              -t "roost v$VERSION $name"
          done
          rm -f "$RUNNER_TEMP/minisign.key"
          ls -la out

      # Same version as release.yml's own uploads — see the note in
      # build-linux-packages.yml: v4 and v8 do not interoperate.
      - uses: actions/upload-artifact@v7
        with:
          name: artifacts-signatures
          path: out/*.minisig
```

- [ ] **Step 2: Wire it into `release.yml` by hand**

`release.yml` is hand-edited (`allow-dirty = ["ci"]`, and `dist-workspace.toml:66-96` says why), so this job is added by hand, mirroring `custom-build-linux-packages`, after it:

```yaml
  custom-sign-release-artifacts:
    needs:
      - plan
      - build-local-artifacts
    # Only on a real tag: the `release` environment refuses a pull request's
    # ref before the job starts, which would read as a failed check on every
    # PR. Skipped is fine for `host` below.
    if: ${{ needs.plan.outputs.publishing == 'true' }}
    uses: ./.github/workflows/sign-release-artifacts.yml
    with:
      plan: ${{ needs.plan.outputs.val }}
    secrets: inherit
```

In `host`: add `- custom-sign-release-artifacts` to `needs`, and extend the `if` with
`&& (needs.custom-sign-release-artifacts.result == 'skipped' || needs.custom-sign-release-artifacts.result == 'success')`
beside the `custom-build-linux-packages` clause. `host` downloads `artifacts-*` with `merge-multiple: true` and attaches `artifacts/*`, so the `.minisig` files land on the release with no further change.

In `dist-workspace.toml`, `global-artifacts-jobs = ["./build-linux-packages", "./sign-release-artifacts"]`, with one line of comment: `# Listed for the record; release.yml is hand-edited (allow-dirty), so the job block there is what runs.` Then run `dist plan` and confirm it still succeeds (it validates the file; the hand edit is exempt).

- [ ] **Step 3: Item 6.5 in the handover**

Replace the three bullets in item 6.5 (`docs/roost-packaging-handover.md:~500-510`) with:

```markdown
- Generate the keypair once, offline and **unencrypted**: `minisign -G -W -p
  roost.pub -s roost.key`. The C `minisign` CLI has no non-interactive
  password option, so the release job could not use an encrypted key, and a
  password stored beside the key in the same environment protects nothing.
  Commit the public key as `keys/roost.pub`; **never** commit `roost.key`.
- Add the secret key's file contents to the `release` GitHub environment as
  `ROOST_MINISIGN_KEY` — the one secret; there is no password secret. The
  same environment gates the Homebrew publish (see the comments on
  `publish-homebrew-formula` in `release.yml` for why an environment secret
  and not a repository secret).
- Keep `roost.key` somewhere that survives this machine. A lost key means
  every installed roost refuses future updates and its users run the shell
  installer once — the same failure Tauri documents for its updater.
- Check a release by hand once: `minisign -V -p keys/roost.pub -m
  roost-<target>.tar.xz` against the downloaded tarball and its `.minisig`.
```

Also update #87's body to match (the plan's author does this on the tracker; it is not a repository change).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/sign-release-artifacts.yml .github/workflows/release.yml dist-workspace.toml docs/roost-packaging-handover.md
git commit -m "Every release tarball is signed in the global phase, with one unencrypted key"
```

---

### Task 13: What a green suite cannot see

**Files:**
- Modify: `docs/superpowers/specs/2026-09-13-self-update-design.md` (the *Testing* section's manual-run record)
- Test: none — this task produces evidence

Exec is a process replacement; a test that execs replaces the test runner. The spec asks for one manual run, recorded in the spec before shipping: a scratch roost, a locally built and locally signed tarball served from a local HTTP server, the button clicked in a real browser, and the page coming back on the new version with a dtach shell opened before the click still alive. `ROOST_UPDATE_BASE` (Task 1) and `keys/roost.pub` (Task 1) exist for this run.

- [ ] **Step 1: A key, on the deploy host**

```bash
sudo apt-get install -y minisign
mkdir -p /tmp/upd && cd /tmp/upd
minisign -G -W -p test.pub -s test.key
```

- [ ] **Step 2: The "old" binary, built as a release, trusting the test key**

In the checkout (uncommitted, both reverted at the end):

```bash
cp /tmp/upd/test.pub keys/roost.pub
GITHUB_REF_TYPE=tag cargo build          # channel = release
mkdir -p /tmp/upd/bin && cp target/debug/roost /tmp/upd/bin/roost
/tmp/upd/bin/roost --version             # roost 0.5.x
```

- [ ] **Step 3: The "new" binary, packed and signed the way a release is**

```bash
sed -i 's/^version = ".*"/version = "9.9.9"/' Cargo.toml
cargo build
T=x86_64-unknown-linux-musl              # what BuildInfo.target says on this host: check About
mkdir -p /tmp/upd/serve/roost-$T && cp target/debug/roost /tmp/upd/serve/roost-$T/roost
(cd /tmp/upd/serve && tar -cJf roost-$T.tar.xz roost-$T && minisign -S -s /tmp/upd/test.key -m roost-$T.tar.xz)
git checkout Cargo.toml Cargo.lock && rm keys/roost.pub
(cd /tmp/upd/serve && python3 -m http.server 8999 --bind 127.0.0.1) &
```

Note `T`: the old binary was built with `cargo build` on the host, so `BuildInfo.target` is the *host* triple (`x86_64-unknown-linux-gnu` on the deploy box), and the served file must be named for it. Read it from About before naming the tarball.

- [ ] **Step 4: The scratch instance, with a fresh check naming 9.9.9**

```bash
mkdir -p /tmp/upd/state/update /tmp/upd/roots/scratch && git -C /tmp/upd/roots/scratch init -q
printf 'version_check = true\n' > /tmp/upd/config.toml
printf '{"latest":"9.9.9","checked_at":%d,"failed_at":null}\n' "$(date +%s)" > /tmp/upd/state/update/check.json
ROOST_STATE_DIR=/tmp/upd/state ROOST_ROOTS=/tmp/upd/roots ROOST_CONFIG=/tmp/upd/config.toml \
  ROOST_UPDATE_BASE=http://127.0.0.1:8999 /tmp/upd/bin/roost 8446 &
echo "pid $!"
```

- [ ] **Step 5: The run, in a real browser**

Open `http://127.0.0.1:8446/scratch`. Open a terminal and type `echo alive-$$`. The mark reads `↑ 9.9.9` and the dialog opened itself with an `Update` button (About's *Upgrades* row must read "roost can replace this copy" — `/tmp/upd/bin` is writable and the channel is `release`). Click `Update`. Expected, in order: `download…`, `verify…`, `unpack…`, `probe…`, `swap…`, `restarting roost…`, then the page reloads and About's *Version* reads `9.9.9`; the terminal tab still shows `alive-<pid>` and accepts input; `ls -la /tmp/upd/bin` shows one `roost` and no `.roost-update.*`; `/tmp/upd/bin/roost --version` prints `roost 9.9.9`; `ps -o pid,cmd -p <pid>` shows the same PID; `cat /tmp/upd/state/error.log` shows the phase timeline ending in `swapped … exec`.

Then the failure path, for the dialog's wording: serve a tarball signed with a *different* key (`minisign -G -W -p other.pub -s other.key`, re-sign, restart the http server), restart the scratch roost from Step 4 on the *old* binary again (rebuild it or keep a copy), click `Update`, expect `update failed: verify: signature did not verify`, a `Retry` button, About's row reading `9.9.9 available (update failed: verify: signature did not verify)`, and the binary untouched.

Paste both transcripts into the spec's *Testing* section under a new heading *The manual run, <date>*, and into the PR body.

- [ ] **Step 6: Under systemd, once**

On the deploy host under the unit (`docs/deploy.md`, *Deploying*): the same run against the installed service is worth doing once, and the assertion is `systemctl --user show roost -p MainPID --value` reporting the same PID before and after, with `pgrep -c dtach` unchanged. This needs a real release with a real signature, so it belongs to whoever cuts the first release after this merges — record that in the PR rather than doing it here.

- [ ] **Step 7: The full suite on the Linux deploy host**

```bash
cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])'
time cargo test -- --test-threads=1
deno run -A tests/browser/update.mjs
deno run -A tests/browser/version.mjs
deno run -A tests/browser/about.mjs
deno run -A tests/browser/settings.mjs
```

Expected: the metadata line prints this worktree's own `target`, every browser test PASS, the Rust suite PASS, and the wall time comparable with the run before this branch.

- [ ] **Step 8: Commit the record**

```bash
git status --short      # only .cargo/config.toml, plus the spec
git add docs/superpowers/specs/2026-09-13-self-update-design.md
git commit -m "Record the manual exec run: the button, the swap, the reload, the shell that survived"
```

---

## Self-review

Performed against `docs/superpowers/specs/2026-09-13-self-update-design.md`, section by section, on 2026-09-14.

**1. Spec coverage**

| Spec section | Task |
|---|---|
| *Scope: who sees what* — the one condition; the mark; the dialog; the About row's button; the per-shape buttons; no mark for a checkout | 7 (`replaceable_by`, `offer`), 10 (`updateWanted`, `openUpdate`, the About button), 11 |
| *Out of scope* | Nothing in any task touches the check's endpoint or intervals, the overview page, notarization, rollback, Homebrew or the packages beyond their command, or a CLI. |
| *Later, Skip, and where those choices live* — 24 h, per version, a separate file, atomic, unreadable = no choice, two intents, global, `broadcast_all`, the `SettingsView` sibling, reset by deleting the file | 6 (`Choices`, `defer_in`, `skip_in`), 7 (the intents, the fields, `broadcast_settings_all`), 9 (the divert, the websocket test) |
| *The pipeline* — three checks under the hub lock and a hand-off; capture the path; fetch two files with the derived URL, the same agent, 64 MB against `Content-Length`; verify before opening; unpack one member; probe with a timeout; rename | 8 (`eligible`, `start` — the checks need no hub lock at all, which is stricter than the spec asks), 5 (`http_get_bytes`, `run_pipeline`), 2, 3, 4 |
| *The restart* — persist (verified, not tasked), tell the requester, exec with `args_os`, exec failure recorded and shown | *State of play*; 8 (`restarting`, `exec`, `record_installed`); 10 (`reloadOnReconnect`, `restart roost to run`) |
| *Signing, on both ends* — the reusable workflow, `environment: release`, the key, `keys/roost.pub` compiled in, `minisign-verify`, distinct failure messages, the honest gap | 12; 1 (`build.rs`, `public_key`); 2 |
| *Outcomes, and what the user reads* — `Restarting`/`Refused`/`Failed(Phase, msg)`, the dialog's line and Retry, the About row's `update failed`, not persisted, guard clears on every path but `Restarting`, `errlog` | 5 (`Phase`), 8 (`drive`, `progress`, `log`), 7 (`failure`), 10 (`Retry`, `latestLabel`) |
| *Testing* — every unit bullet; the integration refusal with two subscribers; exec by one manual run; the browser test's list; revert checks | 1–6, 8 (two-subscriber, single-flight, panic), 9, 13, 11; revert steps in Tasks 2, 5, 8, 11 |
| *What a green suite cannot see* | 13 (Steps 1–6), and the first release after merging |
| *Decided in conversation* | All five decisions implemented as stated; none reopened. |
| *Handoff* — the `wsstate::save` check; `minisign-verify` vs the CLI; systemd | *State of play* (answered); 13 Step 5 (the CLI signs); 13 Step 6 |

Three deliberate departures from the spec's wording, each recorded under *Decisions this plan makes*: the test keypair is generated rather than committed; the release key is unencrypted with one secret; the tarball's member is `<dir>/roost`, not `roost`, because that is what dist ships. And one addition the spec did not name: `Outcome::Refused` and `Failed` are not a Rust enum the caller matches on but the `phase` string of one event, because the only consumer is the browser and the enum would have been serialized to exactly that.

**2. Placeholder scan**

No "TBD", "TODO", "similar to Task N", "add error handling", or "write tests for the above". Every code step carries a full code block; every file modification names its anchor; every test step names the command and the expected output. Task 13 produces evidence and names the commands that produce it.

**3. Type consistency**

Checked across tasks: `Plan { exe, tarball_url, sig_url, pubkey, want, probe_timeout }` in Tasks 5 and 8; `Phase` with seven variants and `as_str` in Tasks 5, 7, 8; `FetchBytes = fn(&str) -> Result<Vec<u8>, String>` in Tasks 5, 8, 9; `ExecFn = fn(&Path) -> String` in Task 8; `Choices { skipped: Option<String>, deferred_until: Option<u64> }` in Tasks 6, 7, 9; `UpdateView`'s seven fields with the same names in the Rust producer (Task 7), the JS consumer (Task 10) and the browser assertions (Task 11); the event phase strings `download`, `verify`, `unpack`, `probe`, `swap`, `restarting`, `failed`, `refused` in Tasks 7, 8, 10, 11; `record_failure(latest, phase, msg)` and `record_installed(version)` defined in Task 7 and called in Task 8; `replaceable_by(&BuildInfo)` in Task 7 and `replaceable_here()` in Tasks 7 and 8; `test_key`, `sign_bytes` (Task 2), `tar_xz`, `Member`, `T` (Task 3), `fake_exe` (Task 4), `serve`, `fetch_served`, `fetch_404`, `release`, `plan_in`, `leftovers` (Task 5), `env_fixture`, `checked` (Task 7) reused by name in Task 8's tests.
