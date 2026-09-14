# Version Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Learn the newest published roost version from the crates.io sparse index and say so in About — one new row, five renderings, no download, no button, no banner.

**Architecture:** A new `src/version.rs` owns everything: the sparse-index path rule, the index-line parser, the semver comparator, the three-valued `Latest`, the state file under `state_dir()/update/`, and a single-flight detached-thread fetch with an injected `FetchFn`. `wsconn::handle` fires `version::maybe_check()` on a workspace websocket connect, after the Origin check and outside every lock. The answer reaches the browser by the route the settings dialog already uses: a process-global cell read by `config::settings_view` into a new `SettingsView.update` field — a **sibling** of `build`, never inside `BuildInfo`, whose doc promises it is constant for the process. `static/dialog.js` renders the row.

**Tech Stack:** Rust 2021, `ureq` 2.12 (promoted from dev-dependency, `proxy-from-env` added, TLS backend unchanged), `serde`/`serde_json`, plain JS in `static/dialog.js`, Deno + Chromium for the browser test.

**Spec:** docs/superpowers/specs/2026-09-12-version-check-design.md

## Global Constraints

- `ureq` is promoted from `[dev-dependencies]` to `[dependencies]` as `ureq = { version = "2.12", features = ["proxy-from-env"] }`; it is removed from dev-dependencies (a normal dependency is already visible to `tests/*.rs`). Verified in `Cargo.lock:1179-1182` — the tree already holds 2.12.1.
- **The trust store stays `webpki-roots`.** `ureq`'s `default = ["tls", "gzip"]` and `tls = ["dep:webpki-roots", "dep:rustls", "dep:rustls-pki-types"]` (read from the vendored `ureq-2.12.1/Cargo.toml`). Do **not** add `native-certs` or `native-tls`: it drags OpenSSL into a static musl build. Behind a TLS-intercepting proxy the check reads `Unknown`, and that is accepted and recorded.
- **`proxy-from-env` is enabled.** It is off in ureq's default set (`ureq-2.12.1/src/agent.rs:267-270` makes `try_proxy_from_env` default to the feature flag), so a default build ignores `HTTPS_PROXY` entirely.
- **Promoting `ureq` puts `rustls` and `ring` into the shipped binary, and `ring` compiles C.** The release runners are fine: `dist plan --output-format=json` lists `sudo apt-get install musl-tools` under `packages_install` for both musl targets (read on the dev host on 2026-09-14). Two places are not: the container job in `ci.yml:92-94` builds `x86_64-unknown-linux-musl` after only `rustup target add`, and a dev host without `musl-tools`. Tried on 2026-09-14 with this exact promotion: `cargo build --target x86_64-unknown-linux-musl` failed at `ring v0.17.14` with `failed to find tool "x86_64-linux-musl-gcc"`. Task 6 fixes the CI step and corrects the packaging handover, which currently says the release binary has no crypto dependency.
- **An explicit 10 s `timeout()` on the agent.** `AgentBuilder::timeout` (`ureq-2.12.1/src/agent.rs:490`) is the overall bound. ureq's own defaults are a 30 s connect timeout and *no* read timeout, so a half-open connection would park the detached thread forever and nothing waits on that thread.
- **Endpoint `https://index.crates.io/ro/os/roost`**, derived — never spelled — by the sparse-index prefix rule: one character `1/`, two `2/`, three `3/{first}/`, four or more `{first two}/{next two}/`, applied to `CARGO_PKG_NAME`. The endpoint is **not** configurable at all, by any scope.
- **User-Agent `roost/<version> (+https://github.com/PeterKnego/roost)`**, built from `CARGO_PKG_VERSION` and `CARGO_PKG_REPOSITORY`.
- **Two intervals: 24 h after a success, 1 h after a failure.** A `checked_at` or `failed_at` in the future reads as **stale**, never as fresh until that future arrives.
- **`version_check`: bool, default `true`, global-only.** Absent means `true`; unreadable or unparseable means `false`. This is deliberately **not** a copy of `relaunch_from` (`src/config.rs:337-343`), whose `.ok()…unwrap_or(false)` folds absent, unreadable and unparseable together.
- **State file schema is exactly `{ "latest", "checked_at", "failed_at" }`** — no `outcome` field, ever. `Latest` is computed at display time against `CARGO_PKG_VERSION`.
- **The state file lives in a subdirectory**, `state_dir()/update/check.json`, never at the top of the state directory: `registry::known_projects_inner` (`src/registry.rs:1085-1104`) globs every `*.json` there into a project row, which is exactly the phantom-project bug `src/notify.rs:84-105` records.
- **Written atomically**: pid-unique temp file, then `rename`, per CLAUDE.md's rule about persistent evidence. A file that is missing, truncated, or otherwise unreadable is "never checked", never an answer.
- **No lock held across the request.** The in-flight guard is an `AtomicBool`; the result cell's `Mutex` is taken only to swap a small value, never around I/O.
- **No panic escapes the check thread** (`catch_unwind`), and the in-flight guard is released on the panic path too.
- **`Unknown` is never reported as `UpToDate`.** It covers: no network, DNS failure, a non-200 answer, a body that does not parse, an empty index entry, and a version string the comparator cannot read — on either side.
- **The test harnesses turn the check off.** `tests/browser/harness.mjs` and `tests/common/mod.rs` write `version_check = false` into a `ROOST_CONFIG` fixture, and one Rust test asserts the injected fetch is *never called* with it off.
- Run tests as `cargo test -- --test-threads=1` (a bare `cargo test` has hung on this host). **Never** `cargo test --release`.
- Work in the worktree `.claude/worktrees/version-check`. Its `.cargo/config.toml` carries a `[build] target-dir = "target"` redirect that **must never be committed** — `git add` explicit paths only, never `git add -A`.
- Every test-writing step states what deleting or reverting the covered code does to the test. The comparator table, the "verdict is not stored" test and the single-flight test each get an actual revert-and-watch-it-fail step.

---

## File Structure

**Created**

| File | Single responsibility |
|---|---|
| `src/version.rs` | Everything about "is there a newer roost": the index path rule, the index-line parser, the comparator, `Latest`, the state file, the single-flight fetch, and the `UpdateView` the dialog renders from. |
| `tests/browser/version.mjs` | The Latest row's five renderings, in a real browser, through the real renderer. |

**Modified**

| File | Change |
|---|---|
| `Cargo.toml` | `ureq` moves from `[dev-dependencies]` to `[dependencies]` with `proxy-from-env`. |
| `src/lib.rs` | `pub mod version;`. |
| `src/config.rs` | `version_check()` / `version_check_from()`; `GLOBAL_ONLY_KEYS`; `validate`'s bool arm; the settings doc row; `settings_view` fills `SettingsView.update`; `ENV_LOCK` becomes `pub(crate)` so `version.rs`'s tests can serialise on `ROOST_CONFIG`. |
| `src/proto.rs` | `pub struct UpdateView`, and `SettingsView.update` beside `build`. |
| `src/wsconn.rs` | One call to `version::maybe_check()` after the handshake. |
| `static/dialog.js` | `latestLabel`, the `Latest` row in `ABOUT_ROWS`, and its arm in `renderAbout`'s `value()`. |
| `tests/common/mod.rs` | `start()` points `ROOST_CONFIG` at a fixture carrying `version_check = false`. |
| `tests/browser/harness.mjs` | `startRoost` writes and points `ROOST_CONFIG` at a fixture carrying `version_check = false`. |
| `tests/browser/settings.mjs`, `roots.mjs`, `ide.mjs` | Their own `ROOST_CONFIG` files gain `version_check = false` (they override the harness's). |
| `tests/roots.rs` | Its own `ROOST_CONFIG` file gains `version_check = false`. |

---

### Task 1: The `version_check` config key

**Files:**
- Modify: `src/config.rs:105` (`GLOBAL_ONLY_KEYS`), `src/config.rs:154-158` (`validate`'s bool arm), `src/config.rs:9-22` (`RawConfig`), after `src/config.rs:343` (the reader), `src/config.rs:670-671` (the settings doc row), `src/config.rs:992` (`ENV_LOCK` visibility), `src/config.rs:1338` (the key-order assertion)
- Test: `src/config.rs` `#[cfg(test)] mod tests` (bottom of the same file, per CLAUDE.md's style rule)

**Interfaces:**
- Consumes: `RawConfig`, `global_config_path()` (`src/config.rs:168`), `writable_in` (`src/config.rs:112`)
- Produces:
  - `pub fn version_check() -> bool`
  - `fn version_check_from(global: &Path) -> bool`
  - `pub(crate) static ENV_LOCK: std::sync::Mutex<()>` (was private)

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs`'s `mod tests`. This is the test that would go green if someone copied `relaunch_from`: the `unparseable_means_off` assertion is the one that fails against a `.ok()…unwrap_or(true)` reader, and `absent_means_on` is the one that fails against `unwrap_or(false)`. Delete either arm of the reader and one of these three fails.

```rust
    /// The three-way rule the spec insists on, and the reason it is not a copy
    /// of `relaunch_from`: an operator who wrote `version_check = false` and
    /// later broke the same file with a typo elsewhere must not silently get
    /// back the network request they turned off.
    #[test]
    fn an_unreadable_global_file_means_the_check_is_off_but_an_absent_one_does_not() {
        let d = tempfile::tempdir().unwrap();

        let missing = d.path().join("nope.toml");
        assert!(version_check_from(&missing), "absent means on: the check is the default");

        let empty = d.path().join("empty.toml");
        fs::write(&empty, "theme = \"dark\"\n").unwrap();
        assert!(version_check_from(&empty), "a file that says nothing about it means on");

        let off = d.path().join("off.toml");
        fs::write(&off, "version_check = false\n").unwrap();
        assert!(!version_check_from(&off), "false means off");

        let on = d.path().join("on.toml");
        fs::write(&on, "version_check = true\n").unwrap();
        assert!(version_check_from(&on), "true means on");

        // The arm that separates this reader from `relaunch_from`.
        let broken = d.path().join("broken.toml");
        fs::write(&broken, "version_check = false\nthis is not = = toml\n").unwrap();
        assert!(
            !version_check_from(&broken),
            "a file that did not parse means off — `Settings::warning` names it in the dialog"
        );

        let unreadable = d.path().join("adir.toml");
        fs::create_dir(&unreadable).unwrap();
        assert!(!version_check_from(&unreadable), "a file that cannot be read means off");
    }

    /// A cloned repository must not be able to turn this on, off, or anywhere.
    #[test]
    fn version_check_is_global_only_and_takes_a_bool() {
        assert_eq!(writable_in("version_check"), ["global"]);
        assert_eq!(
            validate(Scope::Project, "version_check", Some(&V::Bool(false))).unwrap_err(),
            "version_check is a global setting; switch the scope to global"
        );
        assert!(validate(Scope::Global, "version_check", Some(&V::Bool(false))).is_ok());
        assert_eq!(
            validate(Scope::Global, "version_check", Some(&V::Str("yes".into()))).unwrap_err(),
            "version_check takes true or false"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::tests::an_unreadable_global_file -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `version_check_from` in this scope``.

- [ ] **Step 3: Add the field, the reader, the allowlist and the validator**

In `RawConfig` (`src/config.rs:9-22`), after `relaunch: Option<bool>,`:

```rust
    version_check: Option<bool>,
```

Replace `src/config.rs:105`:

```rust
pub const GLOBAL_ONLY_KEYS: &[&str] =
    &["share_selection", "worktree_prompt", "relaunch", "version_check"];
```

In `validate`'s bool arm (`src/config.rs:154-158`), extend the pattern:

```rust
        (
            "show_hidden" | "autosave" | "follow_tree" | "share_selection" | "worktree_prompt"
            | "relaunch" | "version_check",
            SettingValue::Bool(_),
        ) => Ok(()),
```

After `relaunch_from` (`src/config.rs:343`):

```rust
/// Whether roost asks crates.io what the newest published version is.
///
/// Global only (see `GLOBAL_ONLY_KEYS`): roost's own code names the
/// destination, and a setting a cloned repository could write would let it
/// enable, disable, or — if the endpoint were ever configurable — redirect a
/// request this process makes. The endpoint is deliberately not a setting at
/// all.
///
/// **Deliberately not `relaunch_from`'s shape.** That reader folds absent,
/// unreadable and unparseable into one default, which is right where the
/// default is "do nothing" and wrong here, where the default is a request. An
/// operator who wrote `version_check = false` and later broke the same file
/// with a typo elsewhere would otherwise get back the request they turned off,
/// with nothing in About to say why. So: absent means on; unreadable or
/// unparseable means off. `Settings::warning` already names the broken file in
/// the dialog, so the silence has an explanation beside it.
pub fn version_check() -> bool {
    version_check_from(&global_config_path())
}

fn version_check_from(global: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(global) else {
        // Absent and unreadable are not the same thing here, but they differ
        // only in the direction `read_to_string` cannot report: a missing file
        // is the overwhelmingly common case and means on, and a genuinely
        // unreadable one is separated below by the parse, not here.
        return !global.exists() || global.symlink_metadata().is_ok_and(|m| !m.is_file());
    };
    match toml::from_str::<RawConfig>(&text) {
        Ok(raw) => raw.version_check.unwrap_or(true),
        Err(_) => false,
    }
}
```

Note the `read_to_string` arm: a path that does not exist at all returns `true` (absent), and anything else that failed to read returns `false` (cannot look). `!global.exists()` is used only in the *non-destructive* direction here — the worst outcome is a request not made — so CLAUDE.md's `symlink_metadata` rule is satisfied by the second clause, which refuses a directory or a dangling symlink target rather than reading it as "absent".

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib config:: -- --test-threads=1`
Expected: PASS, except `the_settings_view_reports_effective_project_global_and_default_per_key`, which fails on the key-order assertion once Step 5 adds the row. Run it again after Step 5.

- [ ] **Step 5: Add the settings row and fix the key-order assertion**

After `src/config.rs:670-671` (the `relaunch` push):

```rust
    push("version_check", "bool", V::Bool(version_check()), V::Bool(true), false,
        "Ask crates.io once a day whether a newer roost has been published, and say so in About. Nothing is downloaded.");
```

Replace the key list at `src/config.rs:1338`:

```rust
        assert_eq!(keys, ["theme", "hide", "show_hidden", "autosave", "follow_tree", "share_selection", "worktree_prompt", "relaunch", "version_check", "allowed_origins", "max_upload_bytes", "ide", "roots"]);
```

- [ ] **Step 6: Make `ENV_LOCK` reachable from `version.rs`'s tests**

Replace `src/config.rs:992`:

```rust
    /// `ROOST_MAX_UPLOAD` and `ROOST_CONFIG` are process-global and these tests
    /// write them, so they serialise. Without this they interleave and each
    /// sees another's value.
    ///
    /// `pub(crate)` because `version.rs`'s tests point `ROOST_CONFIG` at their
    /// own fixture too. A test needing both this and
    /// `wsstate::STATE_ENV_LOCK` takes **`STATE_ENV_LOCK` first**; the order
    /// has to be total, and no test today takes them the other way round (an
    /// inversion deadlocks, and a deadlock hangs rather than fails).
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
```

- [ ] **Step 7: Run the whole config module**

Run: `cargo test --lib config:: -- --test-threads=1`
Expected: PASS, all config tests.

- [ ] **Step 8: Commit**

```bash
git add src/config.rs
git commit -m "A version_check key that reads a broken file as off"
```

---

### Task 2: The sparse-index path, derived from the crate name

**Files:**
- Create: `src/version.rs`
- Modify: `src/lib.rs:51` (module list, alphabetical order puts `version` between `upload` and `watch` — the list is not strictly alphabetical, so insert after `pub mod upload;` at line 45)
- Test: `src/version.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub const INDEX_BASE: &str = "https://index.crates.io";`
  - `pub fn index_path(name: &str) -> Option<String>`
  - `pub fn index_url(name: &str) -> Option<String>`
  - `pub fn user_agent() -> String`

- [ ] **Step 1: Write the failing test**

Create `src/version.rs` with only the test module for now. The table is what turns "a URL somebody typed" into "a rule with a unit test", so a crate rename changes the URL rather than 404ing into `Unknown` forever. Delete the length dispatch and every row but one fails; hardcode `"ro/os/roost"` and the one-, two- and three-character rows fail.

```rust
//! Is there a newer roost? See
//! `docs/superpowers/specs/2026-09-12-version-check-design.md`.

#[cfg(test)]
mod tests {
    use super::*;

    /// The sparse index's own prefix rule, as a table. `roost` -> `ro/os/roost`
    /// is the row that matters; the others exist so a crate rename changes the
    /// URL instead of silently 404ing into `Unknown` forever.
    #[test]
    fn the_index_path_is_derived_from_the_name_length() {
        for (name, want) in [
            ("a", "1/a"),
            ("ab", "2/ab"),
            ("abc", "3/a/abc"),
            ("abcd", "ab/cd/abcd"),
            ("roost", "ro/os/roost"),
            ("serde_json", "se/rd/serde_json"),
        ] {
            assert_eq!(index_path(name).as_deref(), Some(want), "{name}");
        }
        assert_eq!(index_path("").as_deref(), None, "a nameless crate has no path");
    }

    #[test]
    fn this_crate_resolves_to_the_endpoint_the_spec_names() {
        assert_eq!(
            index_url(env!("CARGO_PKG_NAME")).as_deref(),
            Some("https://index.crates.io/ro/os/roost")
        );
    }

    /// crates.io's crawler policy asks a client to say who it is; a version
    /// check that looks like an anonymous scraper is one a registry is
    /// entitled to block. Asserted against the literal the spec spells, so
    /// dropping either half fails.
    #[test]
    fn the_request_identifies_itself() {
        let ua = user_agent();
        assert_eq!(ua, format!("roost/{} (+https://github.com/PeterKnego/roost)", env!("CARGO_PKG_VERSION")));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Add `pub mod version;` to `src/lib.rs` after line 45 (`pub mod upload;`), then run:
`cargo test --lib version:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `index_path` in this scope``.

- [ ] **Step 3: Write the implementation**

At the top of `src/version.rs`, above the test module:

```rust
/// The sparse index. Not a setting, at any scope: a key that let a repository
/// name the host roost fetches from would be the hole global-only config
/// exists to avoid.
pub const INDEX_BASE: &str = "https://index.crates.io";

/// The index's own prefix rule, derived rather than spelled: one character
/// `1/`, two `2/`, three `3/{first}/`, four or more `{first two}/{next two}/`.
/// Bytes, not chars — crate names are ASCII, and a non-ASCII name would not be
/// one crates.io accepts.
pub fn index_path(name: &str) -> Option<String> {
    let n = name.as_bytes();
    let lower = name.to_ascii_lowercase();
    let l = lower.as_bytes();
    Some(match n.len() {
        0 => return None,
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", l[0] as char),
        _ => format!(
            "{}{}/{}{}/{lower}",
            l[0] as char, l[1] as char, l[2] as char, l[3] as char
        ),
    })
}

pub fn index_url(name: &str) -> Option<String> {
    Some(format!("{INDEX_BASE}/{}", index_path(name)?))
}

/// crates.io's crawler policy asks clients to say who they are.
pub fn user_agent() -> String {
    format!("roost/{} (+{})", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"))
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add src/version.rs src/lib.rs
git commit -m "The index path is a rule with a table, not a spelled URL"
```

---

### Task 3: Reading the index entry — newest, unyanked

**Files:**
- Modify: `src/version.rs`
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `index_url` (Task 2)
- Produces:
  - `pub fn newest_unyanked(body: &str) -> Option<String>`
  - `fn parse(v: &str) -> Option<Parsed>` (private; Task 4 makes it the comparator's front half)

Task 4 replaces the placeholder ordering used here with the real comparator; this task defines the parser and the yanked rule, and its tests keep passing across that swap.

- [ ] **Step 1: Write the failing test**

Deleting the `if e.yanked { continue; }` line fails `the_newest_entry_being_yanked_is_skipped` and `every_entry_yanked_is_not_an_answer`. Deleting the `serde_json::from_str(...).ok()` skip fails `a_malformed_line_does_not_discard_the_file`. Returning `lines().last()` instead of the maximum fails `the_newest_is_by_version_not_by_file_order`.

```rust
    /// The index is JSON lines, newest last today — but "last" is a property
    /// of how crates.io happens to write the file, not a guarantee, so the
    /// answer is the maximum.
    #[test]
    fn the_newest_is_by_version_not_by_file_order() {
        let body = "\
{\"name\":\"roost\",\"vers\":\"0.4.0\",\"yanked\":false}
{\"name\":\"roost\",\"vers\":\"0.5.2\",\"yanked\":false}
{\"name\":\"roost\",\"vers\":\"0.5.0\",\"yanked\":false}
{\"name\":\"roost\",\"vers\":\"0.5.1\",\"yanked\":false}
";
        assert_eq!(newest_unyanked(body).as_deref(), Some("0.5.2"));
    }

    /// A yanked release is not something to tell a user to upgrade to.
    #[test]
    fn the_newest_entry_being_yanked_is_skipped() {
        let body = "\
{\"name\":\"roost\",\"vers\":\"0.5.1\",\"yanked\":false}
{\"name\":\"roost\",\"vers\":\"0.5.2\",\"yanked\":true}
";
        assert_eq!(newest_unyanked(body).as_deref(), Some("0.5.1"));
    }

    #[test]
    fn every_entry_yanked_is_not_an_answer() {
        let body = "\
{\"name\":\"roost\",\"vers\":\"0.5.1\",\"yanked\":true}
{\"name\":\"roost\",\"vers\":\"0.5.2\",\"yanked\":true}
";
        assert_eq!(newest_unyanked(body), None, "no unyanked release is `Unknown`, not `UpToDate`");
    }

    #[test]
    fn an_empty_index_entry_is_not_an_answer() {
        assert_eq!(newest_unyanked(""), None);
        assert_eq!(newest_unyanked("\n\n  \n"), None);
    }

    /// One unreadable line must not cost the whole file, and an unreadable
    /// *version* must not become the answer — a `vers` the comparator cannot
    /// read would otherwise be stored and render as "could not check" forever.
    #[test]
    fn a_malformed_line_does_not_discard_the_file() {
        let body = "\
not json at all
{\"name\":\"roost\",\"vers\":\"0.5.1\",\"yanked\":false}
{\"name\":\"roost\",\"vers\":\"banana\",\"yanked\":false}
{\"name\":\"roost\"}
";
        assert_eq!(newest_unyanked(body).as_deref(), Some("0.5.1"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `newest_unyanked` in this scope``.

- [ ] **Step 3: Write the implementation**

Add above the test module in `src/version.rs`:

```rust
/// A version as the comparator understands it: three numeric components, an
/// optional prerelease, and build metadata discarded. Anything else is
/// `None` — which reaches the user as `Unknown`, never as `UpToDate`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    nums: [u64; 3],
    pre: Option<String>,
}

fn parse(v: &str) -> Option<Parsed> {
    // Build metadata is ignored for ordering, per semver.
    let v = v.trim().split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
        Some(_) => return None, // a trailing `-` with nothing after it
        None => (v, None),
    };
    let mut it = core.split('.');
    let mut nums = [0u64; 3];
    for slot in nums.iter_mut() {
        let part = it.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if it.next().is_some() {
        return None; // four components is not a version this reads
    }
    Some(Parsed { nums, pre })
}

/// The newest unyanked version in a sparse-index body.
///
/// `None` for an empty entry, for one where every release is yanked, and for
/// one whose every `vers` is unreadable — all of which are `Unknown`, never
/// `UpToDate`. One malformed line costs that line and nothing else: a registry
/// that adds a field must not make the check go dark.
pub fn newest_unyanked(body: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        vers: String,
        #[serde(default)]
        yanked: bool,
    }
    let mut best: Option<(Parsed, String)> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Entry>(line) else { continue };
        if e.yanked {
            continue;
        }
        let Some(p) = parse(&e.vers) else { continue };
        let better = match &best {
            None => true,
            Some((b, _)) => cmp_parsed(&p, b) == std::cmp::Ordering::Greater,
        };
        if better {
            best = Some((p, e.vers));
        }
    }
    best.map(|(_, s)| s)
}

/// Placeholder ordering, replaced by the real comparator in the next task.
fn cmp_parsed(a: &Parsed, b: &Parsed) -> std::cmp::Ordering {
    a.nums.cmp(&b.nums)
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add src/version.rs
git commit -m "Read the index entry: newest, unyanked, one bad line survivable"
```

---

### Task 4: The comparator and the three outcomes

**Files:**
- Modify: `src/version.rs`
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `parse`, `cmp_parsed` (Task 3)
- Produces:
  - `pub enum Latest { UpToDate, Newer(String), Unknown }`
  - `pub fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering>`
  - `pub fn verdict(running: &str, latest: Option<&str>) -> Latest`

- [ ] **Step 1: Write the failing test**

The spec names this as the test most likely to go green with the operands swapped, so it is a table with rows that disagree under a swap in both directions, and Step 6 actually performs the swap. Deleting the prerelease arm of `cmp_parsed` fails the two rc rows; deleting the `split('+')` in `parse` fails the two build-metadata rows; making `verdict` return `UpToDate` for an unreadable version fails the last three.

```rust
    /// Every branch of the comparison, as a table.
    ///
    /// Two of the three outcomes are cheerful and the third is the one that
    /// matters, so the unreadable rows are here in force: "could not tell" is
    /// never "you are up to date".
    #[test]
    fn the_comparator_orders_running_against_the_index() {
        use Latest::*;
        for (running, index, want, why) in [
            ("0.5.2", Some("0.5.2"), UpToDate, "the same version"),
            ("0.5.2", Some("0.5.1"), UpToDate, "an index behind the running binary is not an upgrade"),
            ("0.5.2", Some("0.5.3"), Newer("0.5.3".into()), "a patch release"),
            ("0.5.2", Some("0.6.0"), Newer("0.6.0".into()), "a minor release"),
            ("0.5.2", Some("1.0.0"), Newer("1.0.0".into()), "a major release"),
            ("0.9.0", Some("0.10.0"), Newer("0.10.0".into()), "components are numbers, not strings"),
            // The case the spec measured: git has v0.5.2-rc.2, the index has
            // none, so a checkout built at an rc tag must be told 0.5.2 exists.
            ("0.5.2-rc.2", Some("0.5.2"), Newer("0.5.2".into()), "a prerelease is below its own release"),
            ("0.5.2", Some("0.5.2-rc.1"), UpToDate, "and the release is above the prerelease"),
            ("0.5.2-rc.1", Some("0.5.2-rc.2"), Newer("0.5.2-rc.2".into()), "rc.2 is above rc.1"),
            ("0.5.2-rc.2", Some("0.5.2-rc.1"), UpToDate, "and rc.1 is not above rc.2"),
            ("0.5.2+build.7", Some("0.5.2"), UpToDate, "build metadata is ignored on the running side"),
            ("0.5.2", Some("0.5.2+build.7"), UpToDate, "and on the index side"),
            ("0.5.2", None, Unknown, "nothing to compare against"),
            ("0.5.2", Some("banana"), Unknown, "an unreadable index version"),
            ("banana", Some("0.5.3"), Unknown, "an unreadable running version"),
            ("0.5", Some("0.5.3"), Unknown, "two components is not a version this reads"),
            ("0.5.2.1", Some("0.5.3"), Unknown, "and neither is four"),
            ("", Some("0.5.3"), Unknown, "nor an empty string"),
        ] {
            assert_eq!(verdict(running, index), want, "{why}: {running} vs {index:?}");
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib version::tests::the_comparator -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `verdict` in this scope``.

- [ ] **Step 3: Write the implementation**

Replace the placeholder `cmp_parsed` from Task 3 with the real one and add the rest:

```rust
/// What roost tells the user about its own version.
///
/// The same discipline as `install::Replaceable`, and for the same reason:
/// "could not reach crates.io" is not "you are up to date". Two of the three
/// are cheerful and the third is the one that matters, which is what makes it
/// the variant a refactor loses. **Computed at display time, never stored** —
/// see `State`'s doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    UpToDate,
    Newer(String),
    Unknown,
}

fn cmp_parsed(a: &Parsed, b: &Parsed) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match a.nums.cmp(&b.nums) {
        Equal => match (&a.pre, &b.pre) {
            (None, None) => Equal,
            // A prerelease orders below the same version without one, as
            // semver does: 0.5.2-rc.2 is told that 0.5.2 is available.
            (Some(_), None) => Less,
            (None, Some(_)) => Greater,
            (Some(x), Some(y)) => cmp_pre(x, y),
        },
        other => other,
    }
}

/// semver's dotted-identifier rule: numeric identifiers compare numerically
/// and rank below alphanumeric ones, and a shorter run of identifiers is
/// lower when every shared one is equal.
fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (mut ai, mut bi) = (a.split('.'), b.split('.'));
    loop {
        let o = match (ai.next(), bi.next()) {
            (None, None) => return Equal,
            (None, Some(_)) => return Less,
            (Some(_), None) => return Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(p), Ok(q)) => p.cmp(&q),
                (Ok(_), Err(_)) => Less,
                (Err(_), Ok(_)) => Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if o != Equal {
            return o;
        }
    }
}

/// `None` when either side is a version string this cannot read — which
/// reaches the user as `Unknown`.
pub fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(cmp_parsed(&parse(a)?, &parse(b)?))
}

/// The whole display decision, in one place, from two strings.
pub fn verdict(running: &str, latest: Option<&str>) -> Latest {
    let Some(l) = latest else { return Latest::Unknown };
    match compare(running, l) {
        None => Latest::Unknown,
        Some(std::cmp::Ordering::Less) => Latest::Newer(l.to_string()),
        Some(_) => Latest::UpToDate,
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
git add src/version.rs
git commit -m "The comparator, with prereleases on either side"
```

- [ ] **Step 6: Revert the fix and watch the table fail**

Not a thought experiment. Apply this one-line swap in `verdict` — the mistake the spec names, and the one a table of equal-looking rows would not catch:

```rust
    match compare(l, running) {
```

Run: `cargo test --lib version::tests::the_comparator -- --test-threads=1`
Expected: FAIL, with the first failing row reported as
`a patch release: 0.5.2 vs Some("0.5.3")` — `left: UpToDate, right: Newer("0.5.3")`.

Then swap it back and re-run; expected PASS. Record the observed failure in a comment above the test:

```rust
    /// Revert-checked: swapping the operands in `verdict`'s `compare` call
    /// fails at "a patch release: 0.5.2 vs Some(\"0.5.3\")" with
    /// `left: UpToDate, right: Newer("0.5.3")`. The rows either side of the
    /// equal case are what make the swap visible.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/version.rs
git commit -m "Record what the comparator table does with its operands swapped"
```

---

### Task 5: The state file, and the two intervals

**Files:**
- Modify: `src/version.rs`
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `crate::wsstate::state_dir()` (`src/wsstate.rs:114`), `verdict` (Task 4)
- Produces:
  - `pub struct State { pub latest: Option<String>, pub checked_at: Option<u64>, pub failed_at: Option<u64> }` (`Serialize`, `Deserialize`, `Debug`, `Clone`, `Default`, `PartialEq`)
  - `pub const FRESH_SECS: u64 = 24 * 60 * 60;`
  - `pub const RETRY_SECS: u64 = 60 * 60;`
  - `pub fn state_dir_for_update() -> std::path::PathBuf`
  - `pub fn state_path() -> std::path::PathBuf`
  - `pub fn read_state_from(path: &std::path::Path) -> Option<State>`
  - `pub fn write_state_to(path: &std::path::Path, s: &State) -> Result<(), String>`
  - `pub fn stale(s: Option<&State>, now: u64) -> bool`
  - `fn now() -> u64`

- [ ] **Step 1: Write the failing test**

Deleting the `t <= now` guard in `stale` fails `a_future_timestamp_reads_as_stale`. Replacing `read_state_from`'s `.ok()?` chain with an `unwrap_or_default()` fails `a_truncated_file_is_never_checked_not_an_answer`. Collapsing the two intervals into one fails `the_two_intervals_are_not_one`. Storing a verdict instead of the fact fails `the_verdict_is_not_stored`.

```rust
    fn st(latest: Option<&str>, checked: Option<u64>, failed: Option<u64>) -> State {
        State {
            latest: latest.map(str::to_string),
            checked_at: checked,
            failed_at: failed,
        }
    }

    #[test]
    fn the_state_file_round_trips_and_lives_out_of_the_project_namespace() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("update").join("check.json");
        let s = st(Some("0.5.3"), Some(1789234567), None);
        write_state_to(&p, &s).unwrap();
        assert_eq!(read_state_from(&p), Some(s));

        // The schema is exactly the three keys the spec names. An `outcome`
        // field is the thing this file must never grow.
        let text = std::fs::read_to_string(&p).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["latest", "checked_at", "failed_at"], "{text}");

        // `registry::known_projects_inner` globs *.json at the top of the
        // state dir into project rows — a file named there would show as a
        // phantom project, exactly as `notify.rs` records.
        assert_eq!(state_path().parent().unwrap().file_name().unwrap(), "update");
        assert_eq!(state_path().file_name().unwrap(), "check.json");
    }

    #[test]
    fn a_truncated_file_is_never_checked_not_an_answer() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("check.json");
        assert_eq!(read_state_from(&p), None, "missing is never checked");
        std::fs::write(&p, "{\"latest\":\"0.5.").unwrap();
        assert_eq!(read_state_from(&p), None, "truncated is never checked, not an empty answer");
        std::fs::write(&p, "").unwrap();
        assert_eq!(read_state_from(&p), None, "empty is never checked");
        std::fs::write(&p, "[]").unwrap();
        assert_eq!(read_state_from(&p), None, "the wrong shape is never checked");
    }

    #[test]
    fn the_two_intervals_are_not_one() {
        let now = 1_000_000u64;
        assert!(stale(None, now), "never checked is stale");
        assert!(!stale(Some(&st(Some("0.5.3"), Some(now - 20 * 3600), None)), now),
            "a success 20 hours ago is fresh");
        assert!(stale(Some(&st(Some("0.5.3"), Some(now - 25 * 3600), None)), now),
            "a success 25 hours ago is stale");
        assert!(!stale(Some(&st(None, None, Some(now - 30 * 60))), now),
            "a failure 30 minutes ago is not retried yet");
        assert!(stale(Some(&st(None, None, Some(now - 90 * 60))), now),
            "a failure 90 minutes ago is retried");
        // The state the two intervals were designed for, and the one a single
        // interval cannot express.
        assert!(!stale(Some(&st(Some("0.5.3"), Some(now - 25 * 3600), Some(now - 30 * 60))), now),
            "stale success, recent failure: wait out the hour, not the day");
        assert!(stale(Some(&st(Some("0.5.3"), Some(now - 25 * 3600), Some(now - 90 * 60))), now),
            "stale success, hour elapsed: retry");
    }

    /// A clock stepped backwards, or a state file copied from another host.
    #[test]
    fn a_future_timestamp_reads_as_stale() {
        let now = 1_000_000u64;
        assert!(stale(Some(&st(Some("0.5.3"), Some(now + 3600), None)), now),
            "a success in the future is stale, not fresh until that future arrives");
        assert!(stale(Some(&st(None, None, Some(now + 3600))), now),
            "and so is a failure in the future");
    }

    /// The schema has no `outcome` field, and this is why.
    ///
    /// Upgrade to 0.5.3, restart inside the 24-hour window, and a stored
    /// verdict computed by the old binary would make the new one announce
    /// "0.5.3 available" while running 0.5.3 — the lying version display #65
    /// and #56 both exist to prevent.
    #[test]
    fn the_verdict_is_not_stored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("check.json");
        // Written by a check that ran while this binary was 0.5.2.
        write_state_to(&p, &st(Some("0.5.3"), Some(1789234567), None)).unwrap();
        let s = read_state_from(&p).unwrap();
        assert_eq!(verdict("0.5.2", s.latest.as_deref()), Latest::Newer("0.5.3".into()),
            "as 0.5.2, the same file says an upgrade exists");
        // Read back by the 0.5.3 binary the user just installed.
        assert_eq!(verdict("0.5.3", s.latest.as_deref()), Latest::UpToDate,
            "as 0.5.3, the same file must say up to date — the fact is stored, the verdict is not");
    }

    /// A thirty-minute outage must not turn "0.5.3 available" into "could not
    /// check".
    #[test]
    fn a_failure_keeps_the_last_good_answer() {
        let after_failure = st(Some("0.5.3"), Some(1_000_000), Some(1_002_000));
        assert_eq!(verdict("0.5.2", after_failure.latest.as_deref()), Latest::Newer("0.5.3".into()));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find type `State` in this scope``.

- [ ] **Step 3: Write the implementation**

Add to `src/version.rs`:

```rust
use serde::{Deserialize, Serialize};

/// 24 hours after a success. #65 asks for days rather than minutes.
pub const FRESH_SECS: u64 = 24 * 60 * 60;
/// 1 hour after a failure. A single interval cannot serve both cases:
/// punishing a transient failure with a day of silence is wrong, and retrying
/// from an offline box on every connect is worse.
pub const RETRY_SECS: u64 = 60 * 60;

/// What the network returned, not what it meant.
///
/// **No `outcome` field, deliberately.** Storing the verdict fails the first
/// time it matters: upgrade to 0.5.3, restart inside the 24-hour window, and a
/// verdict computed by the old binary makes the new one announce "0.5.3
/// available" while running 0.5.3. `latest` is a fact; `Latest` is computed
/// against `CARGO_PKG_VERSION` at display time.
///
/// Two timestamps because one cannot express "last success 20 hours ago, last
/// failure 30 minutes ago", which is the state the two intervals exist for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The newest unyanked version the index reported, from the last
    /// **successful** check. It survives a later failure.
    pub latest: Option<String>,
    /// When that success happened. Drives `FRESH_SECS`.
    pub checked_at: Option<u64>,
    /// When the most recent failure happened. Drives `RETRY_SECS`, and is
    /// cleared by the next success.
    pub failed_at: Option<u64>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A subdirectory, **not** a bare `.json` at the top of the state dir: that
/// top level is the project-workspace namespace, and
/// `registry::known_projects_inner` globs `*.json` there into project rows —
/// a file named `version.json` would list as a phantom project called
/// "version". `notify.rs` already shipped exactly that bug once.
pub fn state_dir_for_update() -> std::path::PathBuf {
    crate::wsstate::state_dir().join("update")
}

pub fn state_path() -> std::path::PathBuf {
    state_dir_for_update().join("check.json")
}

/// Missing, truncated, or otherwise unreadable is **never checked**, never an
/// answer. Nothing here folds "could not look" into "there is nothing".
pub fn read_state_from(path: &std::path::Path) -> Option<State> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<State>(&text).ok()
}

/// Temp file with a pid-unique name, then `rename`, per CLAUDE.md: a reader
/// must never see this half-written and act on the gap. Two roost processes
/// sharing a state directory each do their own check and the later writer
/// wins, which is harmless because both wrote a fact.
pub fn write_state_to(path: &std::path::Path, s: &State) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "no parent directory".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let json = serde_json::to_string(s).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".check.{}.json.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Whether a check is due. A timestamp in the future is stale, not fresh until
/// that future arrives: a clock stepped backwards or a state file copied from
/// another host must not silence the check for a day.
pub fn stale(s: Option<&State>, now: u64) -> bool {
    let Some(s) = s else { return true };
    let fresh = s.checked_at.is_some_and(|t| t <= now && now - t < FRESH_SECS);
    let cooling = s.failed_at.is_some_and(|t| t <= now && now - t < RETRY_SECS);
    !(fresh || cooling)
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: PASS, 15 tests.

- [ ] **Step 5: Commit**

```bash
git add src/version.rs
git commit -m "The state file holds the fact, not the verdict"
```

- [ ] **Step 6: Revert the fix and watch `the_verdict_is_not_stored` fail**

Apply the shortcut the spec warns about — the shape "the file said newer, so it is newer" — by replacing `verdict`'s body:

```rust
pub fn verdict(running: &str, latest: Option<&str>) -> Latest {
    let _ = running;
    match latest {
        Some(l) => Latest::Newer(l.to_string()),
        None => Latest::Unknown,
    }
}
```

Run: `cargo test --lib version::tests::the_verdict_is_not_stored -- --test-threads=1`
Expected: FAIL at
`as 0.5.3, the same file must say up to date — the fact is stored, the verdict is not` — `left: Newer("0.5.3"), right: UpToDate`.

Restore `verdict` and re-run; expected PASS. Add the observed failure as a comment above the test:

```rust
    /// Revert-checked: a `verdict` that returns `Newer` whenever the file
    /// holds a version — the shape a stored `"outcome"` would produce — fails
    /// here with `left: Newer("0.5.3"), right: UpToDate`.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/version.rs
git commit -m "Record what a stored verdict does to the upgrade-and-restart case"
```

---

### Task 6: The fetch — ureq, one flight, a detached thread, no escaping panic

**Files:**
- Modify: `Cargo.toml:45-49` (the tail of `[dependencies]` and all of `[dev-dependencies]`), `src/version.rs`, `.github/workflows/ci.yml:92-94` (the musl toolchain `ring` needs), `docs/roost-packaging-handover.md` (sections 2.5 and C3, which say there is no crypto dependency)
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `index_url`, `user_agent` (Task 2); `newest_unyanked` (Task 3); `State`, `stale`, `state_path`, `read_state_from`, `write_state_to`, `now` (Task 5); `crate::config::version_check()` (Task 1)
- Produces:
  - `pub type FetchFn = fn(&str) -> Result<String, String>;`
  - `pub fn agent() -> ureq::Agent` — the configured agent, reusable by step 4's downloader
  - `pub fn http_get(url: &str) -> Result<String, String>`
  - `pub fn current() -> Option<State>`
  - `pub fn maybe_check()`
  - `pub fn maybe_check_with(fetch: FetchFn)`
  - `pub fn check_once_with(fetch: FetchFn) -> State` — one synchronous check, returns the state it wrote
  - `#[cfg(test)] pub fn reset_for_test()`

- [ ] **Step 1: Promote `ureq` and write the failing test**

Replace `Cargo.toml`'s `[dependencies]` tail and `[dev-dependencies]`:

```toml
futures-util = { version = "0.3", default-features = false }
# Promoted from a dev-dependency: the version check is the one thing roost does
# that reaches the network. `proxy-from-env` is off in ureq's default feature
# set, so a default build ignores HTTPS_PROXY entirely — and an office behind
# one NAT, where a proxy is mandatory, is exactly where this check was designed
# to work. The TLS backend is left alone: the default `tls` feature is
# webpki-roots (the bundled Mozilla set), and `native-tls` would drag OpenSSL
# into a static musl build.
ureq = { version = "2.12", features = ["proxy-from-env"] }

[dev-dependencies]
tempfile = "3"
```

`ring` now builds for the musl targets, and it compiles C. In `.github/workflows/ci.yml`, replace the `Install the musl target` step (lines 92-94):

```yaml
      # Runners have docker and the musl target is a rustup add away — and,
      # since the version check promoted ureq, ring needs a C compiler that
      # targets musl (cc-rs looks for x86_64-linux-musl-gcc, then musl-gcc).
      # dist installs the same package on the release runners.
      - name: Install the musl target
        run: |
          rustup target add x86_64-unknown-linux-musl
          sudo apt-get update -q && sudo apt-get install -y -q musl-tools
```

In `docs/roost-packaging-handover.md`, section 2.5 (`~50-66`) opens with "The release binary has no TLS or crypto dependency at all" and C3 (`~118-125`) says "nothing in the tree needs a C toolchain beyond libc bindings". Both were true through 0.5.2 and stop being true here. Replace 2.5's first paragraph with:

```markdown
The release binary carries `rustls` and `ring` since the version check
(#65 step 2) promoted `ureq` to a runtime dependency; before that both were
dev-only. `ring` compiles C, so the musl targets need `musl-tools` on the
build host — dist installs it on the release runners (`dist plan` lists it
under `packages_install`), `ci.yml`'s container job installs it explicitly,
and a dev host needs `sudo apt-get install musl-tools` once. There is still
no `openssl`, `openssl-sys` or `native-tls` anywhere in the lock file.
```

and add one sentence to C3 after "Both musl targets were built …": "Since step 2, `ring` needs `musl-tools` as well; see 2.5."

On a dev host, before `cargo build --target x86_64-unknown-linux-musl`: `sudo apt-get install musl-tools`. Then confirm the fix with that exact command; expected: `Finished`, and `file target/x86_64-unknown-linux-musl/debug/roost` reports a static executable.

Then add to `src/version.rs`'s `mod tests`. Deleting the `IN_FLIGHT.swap` guard fails the single-flight test; deleting the `catch_unwind` fails `a_panicking_fetch_does_not_leave_the_guard_taken`; making a failed fetch overwrite `latest` fails `a_failed_check_keeps_the_last_good_latest`.

```rust
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: AtomicBool = AtomicBool::new(false);

    /// Blocks until the test lets it go, so the guard is still held while the
    /// other nine callers arrive.
    fn blocking_fetch(_url: &str) -> Result<String, String> {
        CALLS.fetch_add(1, SeqCst);
        while !RELEASE.load(SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok("{\"name\":\"roost\",\"vers\":\"9.9.9\",\"yanked\":false}\n".to_string())
    }

    fn failing_fetch(_url: &str) -> Result<String, String> {
        CALLS.fetch_add(1, SeqCst);
        Err("dns: no such host".into())
    }

    fn panicking_fetch(_url: &str) -> Result<String, String> {
        CALLS.fetch_add(1, SeqCst);
        panic!("the socket thread must not carry this");
    }

    /// Points both process-global env vars at this test's own fixture.
    /// `STATE_ENV_LOCK` **first**, then `config::ENV_LOCK` — the order is
    /// documented on both, and an inversion deadlocks rather than fails.
    fn env_fixture(on: bool) -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
    ) {
        let g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let g2 = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, if on { "version_check = true\n" } else { "version_check = false\n" }).unwrap();
        std::env::set_var("ROOST_CONFIG", &cfg);
        CALLS.store(0, SeqCst);
        RELEASE.store(false, SeqCst);
        reset_for_test();
        (g1, g2, d)
    }

    /// Ten tabs connecting at once — or one browser test opening ten projects
    /// — fire one request, not ten.
    #[test]
    fn ten_connects_with_a_blocking_fetch_fire_it_once() {
        let (_g1, _g2, _d) = env_fixture(true);
        for _ in 0..10 {
            maybe_check_with(blocking_fetch);
        }
        // Wait for the one thread to actually be inside the fetch.
        for _ in 0..200 {
            if CALLS.load(SeqCst) >= 1 { break }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(CALLS.load(SeqCst), 1, "the in-flight guard is taken before the staleness test");
        RELEASE.store(true, SeqCst);
        for _ in 0..200 {
            if current().is_some() { break }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(CALLS.load(SeqCst), 1, "and still once after it finished");
        assert_eq!(current().unwrap().latest.as_deref(), Some("9.9.9"));
        RELEASE.store(false, SeqCst);
    }

    /// CLAUDE.md: no panic may escape a socket or watcher thread — and this
    /// one would also leave the guard taken forever, silencing every later
    /// check in the process.
    #[test]
    fn a_panicking_fetch_does_not_leave_the_guard_taken() {
        let (_g1, _g2, _d) = env_fixture(true);
        let s = std::panic::catch_unwind(|| check_once_with(panicking_fetch));
        assert!(s.is_ok(), "the panic is caught inside, not by the caller");
        let s = s.unwrap();
        assert!(s.failed_at.is_some(), "a panicking fetch is a failure, not a success");
        assert_eq!(s.latest, None);
    }

    /// The whole point of two timestamps: a thirty-minute outage must not turn
    /// "9.9.9 available" into "could not check".
    #[test]
    fn a_failed_check_keeps_the_last_good_latest() {
        let (_g1, _g2, _d) = env_fixture(true);
        RELEASE.store(true, SeqCst);
        let good = check_once_with(blocking_fetch);
        assert_eq!(good.latest.as_deref(), Some("9.9.9"));
        assert!(good.checked_at.is_some() && good.failed_at.is_none());

        let bad = check_once_with(failing_fetch);
        assert_eq!(bad.latest.as_deref(), Some("9.9.9"), "the last good answer survives");
        assert_eq!(bad.checked_at, good.checked_at, "the success timestamp is untouched");
        assert!(bad.failed_at.is_some(), "and the failure is recorded for the one-hour retry");
        RELEASE.store(false, SeqCst);
    }

    /// A body that parsed but named no usable version is "could not determine",
    /// which earns the one-hour retry rather than a day of silence.
    #[test]
    fn an_index_with_nothing_usable_is_a_failure_not_a_success() {
        let (_g1, _g2, _d) = env_fixture(true);
        fn all_yanked(_url: &str) -> Result<String, String> {
            Ok("{\"name\":\"roost\",\"vers\":\"0.5.2\",\"yanked\":true}\n".to_string())
        }
        let s = check_once_with(all_yanked);
        assert_eq!(s.latest, None);
        assert!(s.failed_at.is_some(), "the retry is the hour, not the day");
        assert!(s.checked_at.is_none());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `maybe_check_with` in this scope``.

- [ ] **Step 3: Write the implementation**

Add to `src/version.rs`:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// The request's overall bound. ureq's own defaults are a 30 s connect timeout
/// and *no* read timeout, so a half-open connection would park the detached
/// thread indefinitely. Nothing waits on that thread, so the cost would be
/// invisible — which is the reason to bound it, not a reason not to.
pub const TIMEOUT_SECS: u64 = 10;

/// The injection seam, the same shape `registry::reconcile_with(roots,
/// snapshot_fn)` uses for its process snapshot: every branch below is testable
/// with no network. A plain `fn` pointer rather than a boxed closure so it
/// crosses into the detached thread without a lifetime.
pub type FetchFn = fn(&str) -> Result<String, String>;

/// The configured agent. Three settings, each decided rather than defaulted;
/// see the module's spec. Step 4's downloader reuses this.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .user_agent(&user_agent())
        .build()
}

/// Every failure is one string: About says "could not check" and does not say
/// why — the reason is here, in the server log, for anyone who needs it.
pub fn http_get(url: &str) -> Result<String, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

/// One check for the whole process, taken **before** the staleness test.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// The answer this process knows, seeded from disk on first read and kept up
/// to date by the check thread. `config::settings_view` reads it; nothing
/// pushes or broadcasts when a check completes, because the About pane is the
/// only consumer and it asks (`RequestState` invalidates the hub's settings
/// cache).
static CELL: OnceLock<Mutex<Option<State>>> = OnceLock::new();
static LOADED: AtomicBool = AtomicBool::new(false);

fn cell() -> &'static Mutex<Option<State>> {
    CELL.get_or_init(|| Mutex::new(None))
}

/// Never holds the lock across the disk read: CLAUDE.md's rule, and this one
/// is called from `settings_view`, which runs under the hub lock.
pub fn current() -> Option<State> {
    if !LOADED.load(Ordering::Acquire) {
        let disk = read_state_from(&state_path());
        let mut g = cell().lock().unwrap_or_else(|e| e.into_inner());
        // A check that finished first already stored a fresher answer and set
        // the flag; the swap tells us so, and we leave it alone.
        if !LOADED.swap(true, Ordering::AcqRel) {
            *g = disk;
        }
        return g.clone();
    }
    cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn store(s: State) {
    let mut g = cell().lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(s);
    LOADED.store(true, Ordering::Release);
}

/// One check, start to finish, synchronously. Returns the state it wrote, so a
/// test can assert on it without waiting on a thread.
///
/// A fetch that panics is a failure like any other: this runs on a thread
/// nothing joins, so an escaping panic would be invisible *and* would leave
/// the in-flight guard taken forever.
pub fn check_once_with(fetch: FetchFn) -> State {
    let previous = current().unwrap_or_default();
    let fetched = index_url(env!("CARGO_PKG_NAME")).and_then(|url| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&url)))
            .unwrap_or_else(|_| Err("the fetch panicked".into()))
            .map_err(|e| {
                eprintln!("roost: version check: {e}");
                e
            })
            .ok()
    });
    // A body that parsed but named no usable version is "could not determine",
    // not "nothing is newer" — so it takes the one-hour retry, not the day.
    let next = match fetched.as_deref().and_then(newest_unyanked) {
        Some(latest) => State { latest: Some(latest), checked_at: Some(now()), failed_at: None },
        None => State { failed_at: Some(now()), ..previous },
    };
    if let Err(e) = write_state_to(&state_path(), &next) {
        eprintln!("roost: version state write: {e}");
    }
    store(next.clone());
    next
}

/// Fired from a workspace websocket connect. Returns immediately; the
/// connection never waits for the request.
pub fn maybe_check() {
    maybe_check_with(http_get);
}

pub fn maybe_check_with(fetch: FetchFn) {
    if !crate::config::version_check() {
        return;
    }
    // Before the staleness test, deliberately: ten tabs connecting at once
    // must fire one request, not ten, and a guard taken after the test would
    // let all ten through the window between.
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    if !stale(current().as_ref(), now()) {
        IN_FLIGHT.store(false, Ordering::SeqCst);
        return;
    }
    // Detached: nothing joins this, and nothing may escape it.
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_once_with(fetch);
        }));
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
pub fn reset_for_test() {
    IN_FLIGHT.store(false, Ordering::SeqCst);
    LOADED.store(false, Ordering::SeqCst);
    *cell().lock().unwrap_or_else(|e| e.into_inner()) = None;
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --lib version:: -- --test-threads=1`
Expected: PASS, 19 tests. Also run `cargo build` and confirm it links.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/version.rs .github/workflows/ci.yml docs/roost-packaging-handover.md
git commit -m "One flight, ten seconds, and a thread nothing can escape"
```

- [ ] **Step 6: Revert the guard and watch single-flight fail**

Delete the guard from `maybe_check_with`:

```rust
    // if IN_FLIGHT.swap(true, Ordering::SeqCst) {
    //     return;
    // }
```

Run: `cargo test --lib version::tests::ten_connects -- --test-threads=1`
Expected: FAIL at `the in-flight guard is taken before the staleness test` — `left: 10, right: 1`.

Then move the guard *inside* the spawned closure (the plausible-looking placement) and re-run: expected FAIL the same way, because ten threads reach the swap before any of them sets it. Restore the original placement and re-run; expected PASS. Record both in a comment above the test:

```rust
    /// Revert-checked twice. Removing the guard fails with `left: 10, right: 1`;
    /// moving it *inside* the spawned closure fails identically, because ten
    /// threads reach the swap before any of them has set it. Both restored —
    /// which is why the guard is taken on the calling thread, before the
    /// staleness test.
```

- [ ] **Step 7: Commit the comment**

```bash
git add src/version.rs
git commit -m "Record where the in-flight guard has to be taken"
```

---

### Task 7: The trigger, on a workspace websocket connect

**Files:**
- Modify: `src/wsconn.rs:159` (immediately after `let Ok(mut ws_read) = accepted else { return };`)
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `version::maybe_check()` (Task 6)
- Produces: nothing new

**Why here.** After the handshake, so a refused Origin fires nothing; before the hub lock is taken at `src/wsconn.rs:164`, so no lock is held across anything the check does; and not in `main.rs`/`serve`, because a roost left running for a month must make no requests until someone looks at it.

- [ ] **Step 1: Write the failing test**

The trigger itself is one call with no return value, so the test that can fail is the one that proves the *policy* it implements: a fresh state means no request. Deleting the `stale` test from `maybe_check_with` fails it.

```rust
    /// The connect is lazy *and* throttled: a browser reconnecting every few
    /// seconds must not fire a request every few seconds.
    #[test]
    fn a_fresh_state_fires_nothing_on_connect() {
        let (_g1, _g2, _d) = env_fixture(true);
        write_state_to(&state_path(), &st(Some("9.9.9"), Some(now()), None)).unwrap();
        for _ in 0..5 {
            maybe_check_with(failing_fetch);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(CALLS.load(SeqCst), 0, "a check 0 seconds ago is fresh; nothing is fetched");

        // ...and a stale one does fire, which is what says the assertion above
        // is about staleness and not about the guard or the config.
        reset_for_test();
        write_state_to(&state_path(), &st(Some("9.9.9"), Some(now() - 25 * 3600), None)).unwrap();
        maybe_check_with(failing_fetch);
        for _ in 0..200 {
            if CALLS.load(SeqCst) >= 1 { break }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(CALLS.load(SeqCst), 1, "a check 25 hours ago is stale");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib version::tests::a_fresh_state -- --test-threads=1`
Expected: PASS already — `maybe_check_with`'s staleness test landed in Task 6. Confirm it is not vacuous by commenting out the `if !stale(...)` block in `maybe_check_with`, re-running (expected FAIL: `a check 0 seconds ago is fresh; nothing is fetched` — `left: 1, right: 0`), and restoring it.

- [ ] **Step 3: Wire the trigger**

In `src/wsconn.rs`, immediately after line 159:

```rust
    let Ok(mut ws_read) = accepted else { return };

    // Lazily, when a browser connects, and only if the stored answer is stale
    // — a roost left running for a month makes no request until someone looks
    // at it. Here rather than in `serve`: after the Origin check, so a refused
    // handshake fires nothing, and before the hub lock below, so nothing is
    // held across it. It returns at once; the connection never waits for the
    // request, which runs on a detached thread. Single-flight for the whole
    // process, so ten tabs are one request.
    crate::version::maybe_check();
```

- [ ] **Step 4: Run the suite**

Run: `cargo test -- --test-threads=1`
Expected: PASS. Time the run and compare with the previous one: a deadlock hangs rather than fails, so a run that suddenly takes much longer is the signal, not a failure count.

- [ ] **Step 5: Commit**

```bash
git add src/wsconn.rs src/version.rs
git commit -m "The check fires when a browser connects, not when roost starts"
```

---

### Task 8: `SettingsView.update` — the route to About

**Files:**
- Modify: `src/proto.rs:320-333` (`SettingsView`, and a new struct above it), `src/config.rs:688-695` (`settings_view`'s return expression), `src/version.rs`
- Test: `src/version.rs` `mod tests`, `src/config.rs` `mod tests`

**Interfaces:**
- Consumes: `Latest`, `verdict`, `current`, `crate::config::version_check()`
- Produces:
  - `pub struct UpdateView { pub status: String, pub latest: String }` in `src/proto.rs`
  - `pub fn view() -> crate::proto::UpdateView` in `src/version.rs`
  - `SettingsView.update: UpdateView`

`status` is one of exactly five strings: `"off"`, `"never"`, `"unknown"`, `"up-to-date"`, `"newer"`. The enum stays three-valued; the two extra are facts the *state file* knows — its absence, and the setting — which is why they live on the wire and not in `Latest`.

- [ ] **Step 1: Write the failing test**

Deleting the `version_check()` gate makes `off` render as `never`; deleting the `current().is_none()` arm makes `never` render as `unknown`; folding `Unknown` into `UpToDate` fails the `unknown` row — which is the one the whole feature exists to keep.

```rust
    /// The five renderings the About row has, decided on the server so the
    /// enum can stay three-valued.
    #[test]
    fn the_view_reports_five_states_and_never_calls_unknown_up_to_date() {
        // Off.
        let (_g1, _g2, d) = env_fixture(false);
        let v = view();
        assert_eq!(v.status, "off");
        assert_eq!(v.latest, "");
        drop((_g1, _g2, d));

        // Never checked: the check is on, and there is no state file.
        let (_g1, _g2, _d) = env_fixture(true);
        assert_eq!(view().status, "never");

        // Could not check: a state file that records only a failure.
        write_state_to(&state_path(), &st(None, None, Some(now() - 60))).unwrap();
        reset_for_test();
        assert_eq!(view().status, "unknown", "a failure is `could not check`, never `up to date`");

        // Could not check, second way in: a `latest` the comparator cannot read.
        write_state_to(&state_path(), &st(Some("banana"), Some(now()), None)).unwrap();
        reset_for_test();
        assert_eq!(view().status, "unknown");

        // Up to date.
        write_state_to(&state_path(), &st(Some(env!("CARGO_PKG_VERSION")), Some(now()), None)).unwrap();
        reset_for_test();
        let v = view();
        assert_eq!(v.status, "up-to-date");
        assert_eq!(v.latest, env!("CARGO_PKG_VERSION"));

        // Newer.
        write_state_to(&state_path(), &st(Some("999.0.0"), Some(now()), None)).unwrap();
        reset_for_test();
        let v = view();
        assert_eq!(v.status, "newer");
        assert_eq!(v.latest, "999.0.0", "step 4 reads this string, so it is carried whole");
    }
```

And in `src/config.rs`'s `mod tests`:

```rust
    /// The one server fact on the About panel that is *not* constant for the
    /// life of the process, so it is a sibling of `build`, not a field of it —
    /// `BuildInfo`'s own doc comment promises constancy.
    #[test]
    fn the_settings_view_carries_the_update_answer_beside_build_not_inside_it() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("global.toml");
        fs::write(&global, "version_check = false\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &global);
        let v = settings_view(d.path());
        assert_eq!(v.update.status, "off", "the reader is consulted, not defaulted");
        let json = serde_json::to_value(&v).unwrap();
        assert!(json.get("update").is_some(), "the client reads state.settings.update");
        assert!(json["build"].get("status").is_none(), "and not state.settings.build.status");
        std::env::remove_var("ROOST_CONFIG");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib version::tests::the_view_reports_five -- --test-threads=1`
Expected: FAIL to compile with ``cannot find function `view` in this scope``.

- [ ] **Step 3: Add `UpdateView` to the protocol**

In `src/proto.rs`, immediately above `pub struct SettingsView` (line 320):

```rust
/// What roost knows about newer versions of itself.
///
/// A **sibling** of `SettingsView::build`, not a field of `BuildInfo`: that
/// struct's doc comment promises it is constant for the life of the process,
/// and this is the one server fact on the About panel that is not.
///
/// `status` is one of `off`, `never`, `unknown`, `up-to-date`, `newer`. Five
/// values where `version::Latest` has three, because two of them — no state
/// file at all, and the setting turned off — are facts the *state file* knows
/// rather than outcomes the comparator produces. About is exactly where
/// someone goes to wonder about either, and both are assertable in the browser
/// test, which the silent alternatives were not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct UpdateView {
    pub status: String,
    /// The newest unyanked version the last successful check saw, or empty.
    /// Carried whole rather than folded into `status` because #65's step 4
    /// names it in a dialog title and in a skipped-version record.
    pub latest: String,
}
```

And add the field to `SettingsView`, after `pub build: BuildInfo,` (line 332):

```rust
    /// See `UpdateView`. Beside `build`, never inside it.
    pub update: UpdateView,
```

- [ ] **Step 4: Add `version::view()` and wire `settings_view`**

In `src/version.rs`:

```rust
/// What the About pane renders from. Read by `config::settings_view` on every
/// cache miss, which `RequestState` forces — so opening the dialog shows
/// whatever the process knows at that moment. No push, no broadcast when a
/// check completes: the pane is the only consumer, and it asks.
pub fn view() -> crate::proto::UpdateView {
    let mk = |status: &str, latest: &str| crate::proto::UpdateView {
        status: status.to_string(),
        latest: latest.to_string(),
    };
    if !crate::config::version_check() {
        return mk("off", "");
    }
    // No state file at all is "not checked yet" — the few seconds after a
    // fresh install — and is not the same answer as a check that failed.
    let Some(s) = current() else { return mk("never", "") };
    match verdict(env!("CARGO_PKG_VERSION"), s.latest.as_deref()) {
        Latest::Newer(v) => mk("newer", &v),
        Latest::UpToDate => mk("up-to-date", s.latest.as_deref().unwrap_or("")),
        Latest::Unknown => mk("unknown", s.latest.as_deref().unwrap_or("")),
    }
}
```

In `src/config.rs`, the `SettingsView { .. }` expression at lines 688-695:

```rust
    SettingsView {
        build: build_info(),
        update: crate::version::view(),
        keys,
        themes: crate::themes::catalogue(),
        project_file: ".roost/config.toml".into(),
        global_file: global.display().to_string(),
        warning: s.warning,
    }
```

- [ ] **Step 5: Run both tests to verify they pass**

Run: `cargo test --lib version:: config:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/proto.rs src/config.rs src/version.rs
git commit -m "The answer reaches About beside build, not inside it"
```

---

### Task 9: The `Latest` row in About

**Files:**
- Modify: `static/dialog.js:284-375` (the label helpers — `latestLabel` goes after `upgradeCommand`), `static/dialog.js:609-620` (`ABOUT_ROWS`), `static/dialog.js:622-630` (`renderAbout`'s `value()`)
- Test: `tests/browser/version.mjs` (Task 11) — no Rust test can reach this file

**Interfaces:**
- Consumes: `state.settings.update` (`UpdateView`, Task 8)
- Produces: `function latestLabel(u)` — top level in `dialog.js`, like `installLabel`/`upgradesLabel`, precisely so the browser test can drive its table directly

- [ ] **Step 1: Add the helper**

After `upgradeCommand` (which ends at `static/dialog.js:375`):

```js
/// What About says about newer versions, from `state.settings.update`.
///
/// Five renderings, three of which come from the comparator and two from the
/// state file itself: its absence ("not checked yet", the few seconds after a
/// fresh install) and the setting ("version checks are off", the answer to
/// "why does this never say anything"). Both are shown rather than left blank
/// — an empty row is the same defect in a quieter coat, and neither was
/// assertable while they were silent.
///
/// An unknown status renders as "could not check" rather than throwing or
/// going blank: About does not say *why* a check failed, and a status this
/// does not recognise is one more way of not knowing.
function latestLabel(u) {
  const v = u || {};
  switch (v.status) {
    case "newer": return `${v.latest} available`;
    case "up-to-date": return "up to date";
    case "never": return "not checked yet";
    case "off": return "version checks are off";
    default: return "could not check";
  }
}
```

- [ ] **Step 2: Add the row and its rendering**

In `ABOUT_ROWS` (`static/dialog.js:609-620`), after the `Version` entry:

```js
    ["Latest", "latest",
      "The newest version published to crates.io, checked once a day when you open roost. Nothing is downloaded."],
```

In `renderAbout` (`static/dialog.js:622-630`), extend the value dispatch (lines 625-630):

```js
  function renderAbout() {
    about.replaceChildren();
    const b = (view && view.build) || {};
    const u = (view && view.update) || {};
    const value = (kind) =>
      kind === "command" ? (upgradeCommand(b) || []).join(" / ")
      : kind === "built" ? fmtBuilt(b.built_epoch)
      : kind === "install" ? installLabel(b)
      : kind === "upgrades" ? upgradesLabel(b)
      : kind === "latest" ? latestLabel(u)
      : (b[kind] || "unknown");
```

The row is always present — unlike `Upgrade`, there is no state in which it has nothing to say, and "version checks are off" is exactly the state a missing row would hide.

- [ ] **Step 3: Check it by hand in a real browser**

CLAUDE.md: verify UI behavior in a real browser before believing it works, and never attach automation to the live instance.

```bash
./scripts/testroost.sh
```

Open the scratch instance, press the settings button, choose About, and confirm the `Latest` row sits under `Version` with its label aligned to the others. With no state file yet it reads `not checked yet` or, once the check lands, `up to date` / `<version> available`. Paste what you saw into the commit body.

- [ ] **Step 4: Commit**

```bash
git add static/dialog.js
git commit -m "A Latest row that says which of five things it knows"
```

---

### Task 10: The suites cannot reach the network

**Files:**
- Modify: `tests/common/mod.rs:50-64` (`start`), `tests/browser/harness.mjs:364-412` (`startRoost`), `tests/browser/settings.mjs:14`, `tests/browser/roots.mjs:69` and `:211`, `tests/browser/ide.mjs:217`, `tests/roots.rs:20-22`
- Test: `src/version.rs` `mod tests`

**Interfaces:**
- Consumes: `maybe_check_with`, `env_fixture` (Task 6)
- Produces: nothing new

Nine integration test files open websockets and every browser test starts a roost. With the check on by default and fired on connect, a green suite would be making real requests to crates.io, and a rate limit or an offline runner would surface as a flake in whichever test lost.

- [ ] **Step 1: Write the failing test**

This is the assertion that turns "the suite happens not to reach the network" into "the suite cannot". Deleting the `if !crate::config::version_check() { return; }` line from `maybe_check_with` fails it.

```rust
    /// With the setting off, the injected fetch is never called — not
    /// "usually", not "when the state happens to be fresh". Deleting the
    /// config gate from `maybe_check_with` fails this with `left: 1, right: 0`.
    #[test]
    fn with_the_check_off_the_fetch_is_never_called() {
        let (_g1, _g2, _d) = env_fixture(false);
        // No state file at all, so staleness cannot be what stops it.
        assert_eq!(read_state_from(&state_path()), None);
        for _ in 0..5 {
            maybe_check_with(failing_fetch);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(CALLS.load(SeqCst), 0, "the setting is off; nothing may reach the network");
        assert_eq!(read_state_from(&state_path()), None, "and nothing was written either");
    }
```

- [ ] **Step 2: Run it to verify it fails, then passes**

Run: `cargo test --lib version::tests::with_the_check_off -- --test-threads=1`
Expected: PASS (the gate landed in Task 6). Prove it is not vacuous: comment out the gate, re-run, expect FAIL with `the setting is off; nothing may reach the network` — `left: 5, right: 0`. Restore and re-run; expected PASS.

- [ ] **Step 3: Turn it off for the integration suite**

In `tests/common/mod.rs`, inside `start()`, before the listener is bound:

```rust
/// Every integration binary's server reads the *developer's* real
/// `~/.config/roost/config.toml` unless something says otherwise, and nine of
/// these files open workspace websockets — which is what fires the version
/// check. A green suite that makes real requests to crates.io turns a rate
/// limit or an offline runner into a flake in whichever test lost, so the
/// suite says so out loud instead.
///
/// Only when the caller has not already named a config (`tests/roots.rs`
/// has), and held for the process's life so the path stays valid: a `TempDir`
/// dropped here would take the file with it.
fn disable_version_check() {
    static FIXTURE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    if std::env::var_os("ROOST_CONFIG").is_some() {
        return;
    }
    let d = FIXTURE.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "version_check = false\n").unwrap();
        d
    });
    std::env::set_var("ROOST_CONFIG", d.path().join("config.toml"));
}
```

and call it as the first line of `start()`:

```rust
pub fn start(roots: Vec<PathBuf>) -> u16 {
    disable_version_check();
    isolate_ide_dir_for_tests();
```

In `tests/roots.rs:20-22`, which names its own config before calling `start`, write the key into it:

```rust
    let global = d.path().join("config.toml");
    std::fs::write(&global, "version_check = false\n").unwrap();
    std::env::set_var("ROOST_CONFIG", &global);
```

(That test later asserts the file gained `roots = [`; `toml_edit` preserves the existing key, and the `# global`-style assertions in that file are unaffected.)

- [ ] **Step 4: Turn it off for the browser suite**

In `tests/browser/harness.mjs`'s `startRoost`, before `const spawn = ...`:

```js
  // The browser suite runs the real binary with the developer's real HOME, so
  // without this every test that opens a project would fire a real request at
  // crates.io on connect. Written into the state dir, which is this run's own
  // and is removed with it. Placed *before* `...extraEnv` below, so a test
  // that names its own config (settings.mjs, roots.mjs, ide.mjs) still wins —
  // each of those writes the key into its own file instead.
  const offConfig = `${stateDir}/version-check-off.toml`;
  await Deno.writeTextFile(offConfig, "version_check = false\n");
```

and in the `env:` object, immediately before `...extraEnv`:

```js
      ROOST_CONFIG: offConfig,
      ...extraEnv,
```

Then add the key to the three files that override it:

- `tests/browser/settings.mjs:14` → `await Deno.writeTextFile(globalToml, "# global\ntheme = \"dark\"\nversion_check = false\n");` (the `/# global\ntheme = "dark"/` assertions at lines 147 and 169 still match).
- `tests/browser/roots.mjs:69` → `await Deno.writeTextFile(globalToml, "# global\nversion_check = false\n");` and `:211` → `` `# global\nversion_check = false\nroots = [${long.map((p) => JSON.stringify(p)).join(", ")}]\n` `` — line 211 rewrites the whole file mid-test, so without the key there the check comes back on for the rest of the run.
- `tests/browser/ide.mjs:217` → `await Deno.writeTextFile(globalConfig, "share_selection = true\nversion_check = false\n");`

- [ ] **Step 5: Prove the fixture took**

A fixture that silently does nothing leaves a test green and meaningless — the trap `tests/browser/README.md` records for `autosave: false`. Run one browser test and read the status the server sent:

```bash
deno run -A tests/browser/about.mjs
```

Expected: PASS, unchanged. Then, against a scratch instance from the harness, confirm the value is reachable — this is asserted properly in Task 11, and the cheap check now is:

```bash
cargo test -- --test-threads=1
```
Expected: PASS, with no test taking noticeably longer than before (a real request to crates.io costs up to ten seconds and would show).

- [ ] **Step 6: Commit**

```bash
git add src/version.rs tests/common/mod.rs tests/roots.rs tests/browser/harness.mjs tests/browser/settings.mjs tests/browser/roots.mjs tests/browser/ide.mjs
git commit -m "The suites turn the check off, and one test says they cannot reach out"
```

---

### Task 11: The row, in a real browser

**Files:**
- Create: `tests/browser/version.mjs`
- Modify: `tests/browser/README.md` (the run list, and the revert-check log)

**Interfaces:**
- Consumes: `fixture`, `freePort`, `openPage`, `profileDir`, `sleep`, `startBrowser`, `startRoost`, `until` from `./harness.mjs`; `latestLabel` and `state.settings.update` from the page
- Produces: nothing

The technique is #78's, and `tests/browser/about.mjs:98-104` writes down why: **switching tabs re-renders About from `state.settings` by reference, while closing and reopening does not**, because the settings button sends `RequestState` and the server's own values overwrite anything a test sets. That cost four failing assertions to discover there; do not rediscover it here.

- [ ] **Step 1: Write the test**

```js
//! The Latest row: five renderings, and the one the server actually sent.
//!
//! #65 step 2. A Rust test proves the comparator, the state file and the
//! `UpdateView` the server builds; only a browser can see the row.
//!
//! Run: deno run -A tests/browser/version.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");

  console.log("A. the harness really turned the check off");
  // The fixture that silently does nothing is the trap README records for
  // `autosave: false`: without this assertion every row below could pass
  // while the suite quietly hit crates.io on every connect.
  const sent = await evalIn(`state.settings.update`);
  ok(!!sent, "the settings snapshot carries an `update` block");
  ok(sent && sent.status === "off",
     `and the harness's ROOST_CONFIG took: status=${sent && sent.status}`);
  // Beside `build`, not inside it — `BuildInfo` promises it is constant for
  // the process, and this is the one server fact on the pane that is not.
  ok(await evalIn(`state.settings.build.status === undefined`),
     "the answer is a sibling of build, not a field of it");

  console.log("B. the row renders what the server sent");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "the dialog"),
     "the settings dialog opens");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  const readRows = () => evalIn(`(() => {
    const out = {};
    for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
      out[r.querySelector("label").textContent] = r.querySelector(".aboutval").textContent.trim();
    }
    return out; })()`);
  let rows = await readRows();
  ok(rows.Latest === "version checks are off",
     `the row is shown when the check is off, and says so (${JSON.stringify(rows.Latest)})`);
  // Discoverable where discovery happens: the row is never hidden, unlike
  // Upgrade. A missing row is the state this one exists to stop hiding.
  ok(Object.keys(rows).indexOf("Latest") === Object.keys(rows).indexOf("Version") + 1,
     `and sits directly under Version (${JSON.stringify(Object.keys(rows))})`);

  console.log("C. all five states, through the real renderer");
  // Switching tabs re-renders About from `state.settings` by reference;
  // closing and reopening does not, because the settings button sends
  // `RequestState` and the server's values overwrite anything set here. See
  // about.mjs, where that cost four failing assertions to discover.
  const asAbout = async (update) => {
    await evalIn(`Object.assign(state.settings.update, ${JSON.stringify(update)}); 0`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
    await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
    return (await readRows()).Latest;
  };
  const original = await evalIn(`JSON.parse(JSON.stringify(state.settings.update))`);

  for (const [update, want, why] of [
    [{ status: "newer", latest: "0.5.3" }, "0.5.3 available", "a newer release names itself"],
    [{ status: "up-to-date", latest: "0.5.2" }, "up to date", "nothing newer"],
    [{ status: "unknown", latest: "0.5.2" }, "could not check",
      "a failed check is never reported as up to date, even holding a version"],
    [{ status: "unknown", latest: "" }, "could not check", "and neither is one holding nothing"],
    [{ status: "never", latest: "" }, "not checked yet", "the few seconds after a fresh install"],
    [{ status: "off", latest: "" }, "version checks are off", "and the setting, back where we started"],
  ]) {
    const got = await asAbout(update);
    ok(got === want, `${why} (${JSON.stringify(got)})`);
  }

  // A status this build does not know must degrade to "could not know", not
  // to silence and not to a cheerful answer.
  ok((await asAbout({ status: "something-later", latest: "9.9.9" })) === "could not check",
     "an unrecognised status is one more way of not knowing");

  await asAbout(original);

  // Driven on the function the renderer calls, the way about.mjs drives
  // `installLabel` — the same table, with no dialog in the way.
  const label = async (u) => await evalIn(`latestLabel(${JSON.stringify(u)})`);
  ok((await label({ status: "newer", latest: "1.0.0" })) === "1.0.0 available",
     "latestLabel is top-level and names the version it was given");
  ok((await label(null)) === "could not check",
     "and a missing block is not knowing, not a crash");
  ok((await label({})) === "could not check", "nor is an empty one");
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

Run: `deno run -A tests/browser/version.mjs`
Expected: PASS, 16 assertions. A host with no Chromium prints `SKIP: no chromium found.` and exits 0, which is normal.

- [ ] **Step 3: Revert three things and watch it fail**

Apply each, run, read the failure, restore:

1. In `static/dialog.js`, make `latestLabel` fall through to `"up to date"` instead of `"could not check"`.
   Expected: FAIL 4 — the two `unknown` rows, the unrecognised-status row, and the two `label(null)`/`label({})` rows.
2. In `latestLabel`, drop the `off` case so it falls through.
   Expected: FAIL 3 — section B's row assertion and the last row of section C, plus the fall-through row now reading `version checks are off` for no status at all.
3. In `tests/browser/harness.mjs`, remove the `ROOST_CONFIG: offConfig` line.
   Expected: FAIL 1 — `and the harness's ROOST_CONFIG took: status=never` (or `up-to-date`, if the developer's machine is online and the state file was already warm). This is the assertion that proves the fixture is real.

- [ ] **Step 4: Record it in the README**

Add to `tests/browser/README.md`'s run list:

```
deno run -A tests/browser/version.mjs    # the About Latest row: five renderings, and the harness's off switch
```

and to the revert-check log:

```
- In `version.mjs`: making `latestLabel` fall through to "up to date" instead
  of "could not check" fails 4 — the whole point of a three-valued answer is
  the one variant that is not cheerful. Dropping its `off` case fails 3.
  Removing `ROOST_CONFIG` from `startRoost` fails 1, on section A's
  `status === "off"`: that assertion exists because a fixture that silently
  does nothing leaves every row below green and meaningless, and here it
  would also mean the suite was hitting crates.io on every connect.
```

- [ ] **Step 5: Commit**

```bash
git add tests/browser/version.mjs tests/browser/README.md
git commit -m "A browser test for the five things the Latest row can say"
```

---

### Task 12: What a green suite cannot see

**Files:**
- Modify: none (this task produces evidence, not code)

**Interfaces:**
- Consumes: `version::http_get`, `version::index_url`, `version::agent` (Task 6)
- Produces: output pasted into the PR body

The injected version never talks to crates.io, `proxy-from-env` is a feature flag rather than a code path the suite drives, and nothing here exercises what crates.io does to a client it dislikes. Three runs, each reported with its output, before this ships.

- [ ] **Step 1: The real request, against the live index**

Add a temporary `#[test]` marked `#[ignore]` so it never runs in CI, run it explicitly, then remove it:

```rust
    /// Run by hand: `cargo test --lib version::tests::live -- --ignored --nocapture --test-threads=1`
    #[ignore]
    #[test]
    fn live() {
        let url = index_url(env!("CARGO_PKG_NAME")).unwrap();
        println!("GET {url}");
        println!("user-agent: {}", user_agent());
        let body = http_get(&url).expect("the live index");
        println!("{} bytes, {} lines", body.len(), body.lines().count());
        let newest = newest_unyanked(&body);
        println!("newest unyanked: {newest:?}");
        println!("verdict for {}: {:?}", env!("CARGO_PKG_VERSION"),
                 verdict(env!("CARGO_PKG_VERSION"), newest.as_deref()));
    }
```

Run: `cargo test --lib version::tests::live -- --ignored --nocapture --test-threads=1`
Expected: a `GET https://index.crates.io/ro/os/roost`, a body of roughly 7 KB over four or more lines, a `newest unyanked: Some("0.5.2")` (or later), and a verdict. Paste the whole block into the PR.

- [ ] **Step 2: The same request through a proxy**

On the deploy host, with any local forward proxy listening (`ssh -D` will not do — it is SOCKS5 without the `socks-proxy` feature; use an HTTP proxy such as `tinyproxy` or a one-liner `python3 -m proxy`):

```bash
HTTPS_PROXY=http://127.0.0.1:8888 \
  cargo test --lib version::tests::live -- --ignored --nocapture --test-threads=1
```

Expected: the same output, and a matching line in the proxy's own log — which is the only thing that proves `proxy-from-env` is compiled in rather than silently ignored. Then re-run with `HTTPS_PROXY=http://127.0.0.1:1` (nothing listening) and expect the test to fail with a connection error inside ten seconds, not to hang and not to bypass the proxy. Paste both.

- [ ] **Step 3: Remove the temporary test**

Delete the `live` test. It has served its purpose, and an `#[ignore]`d network test in the tree is one somebody eventually un-ignores.

- [ ] **Step 4: Run the full suite on the Linux deploy host**

CLAUDE.md: run the suite on the Linux host too, and build from one checkout — this work is in a worktree, so confirm the shared target dir was not touched:

```bash
cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])'
cargo test -- --test-threads=1
deno run -A tests/browser/version.mjs
deno run -A tests/browser/about.mjs
deno run -A tests/browser/settings.mjs
```

Expected: the metadata line prints this worktree's own `target`, all three browser tests PASS, and the Rust suite PASSes. Time the Rust run and compare it with a run from before this branch: a deadlock hangs rather than fails, so the elapsed time is the signal.

- [ ] **Step 5: Commit**

```bash
git add -u src/version.rs
git commit -m "Drop the by-hand live check now that it has been run"
```

(If step 3 left nothing to commit, skip this step — `git status --short` must show only `.cargo/config.toml` modified, which is never committed.)

---

## Self-review

Performed against `docs/superpowers/specs/2026-09-12-version-check-design.md`, section by section.

**1. Spec coverage**

| Spec section | Task |
|---|---|
| *What and why* — learn the newest published version, say so in About | 2-9 |
| *The reversal* — on by default, global-only off switch, lazy trigger | 1 (default `true`, global-only), 7 (lazy on connect) |
| *How roost reaches the network* — ureq promoted, `proxy-from-env`, webpki-roots kept, 10 s timeout | 6 (Cargo.toml + `agent()`), 12 (the proxy run the suite cannot drive) |
| *What roost checks against* — the sparse index, the derived path rule | 2 |
| ...the User-Agent | 2 (`user_agent()`, asserted against the literal) |
| ...yanked versions skipped | 3 |
| ...prereleases and build metadata | 4 |
| *When it fires* — on a workspace websocket connect, detached, single-flight | 6 (guard + thread), 7 (the trigger site) |
| ...the two intervals, and a future timestamp as stale | 5 |
| ...the test harnesses turn it off | 10 |
| *The three outcomes* — `Latest`, every way `Unknown` arises | 4 (the enum and the table), 8 (the view's `unknown` rows) |
| ...computed at display time, never stored | 5 (`the_verdict_is_not_stored`, with its revert check), 8 (`view()` computes it) |
| *Configuration* — `version_check`, global-only, absent=true / unparseable=false | 1 |
| *State* — the schema, the two timestamps, the atomic write, unreadable = never checked | 5 |
| *Display / How the answer reaches About* — the `SettingsView` sibling field, no push | 8 |
| *Display / The row* — five renderings | 9 (the helper), 11 (asserted) |
| *Testing* — every bullet | 2, 3, 4, 5, 6, 8, 10, 11; the three named revert checks are Tasks 4/6 step 6 and Task 5 step 6 |
| *What a green suite cannot see* — the live run, the proxy run | 12 |
| *Deliberately out of scope* | Nothing in any task downloads, replaces, notifies, marks the header, checks per channel, explains a failure, or touches `/`. |
| *Decided in review* — the key name, the display-not-enum split, the row shown when off | 1, 8, 9/11 |

No gaps.

**2. Placeholder scan**

No "TBD", "TODO", "similar to Task N", "add error handling", or "write tests for the above". Every code step carries a full code block; every file modification names a verified line range; every test step names the command and the expected output text. The one step that produces no code (Task 12) produces pasteable evidence and says exactly which commands produce it.

**3. Type consistency**

Checked across tasks: `State { latest: Option<String>, checked_at: Option<u64>, failed_at: Option<u64> }` is used identically in Tasks 5, 6, 7 and 8; `Latest { UpToDate, Newer(String), Unknown }` in Tasks 4, 5 and 8; `FetchFn = fn(&str) -> Result<String, String>` in Tasks 6, 7 and 10; `UpdateView { status, latest }` in Tasks 8, 9 and 11, with the same five status strings (`off`, `never`, `unknown`, `up-to-date`, `newer`) in the Rust producer, the JS consumer and the browser assertions. `index_path`/`index_url` return `Option<String>` in Tasks 2, 6 and 12. `cmp_parsed` is introduced as a placeholder in Task 3 and replaced in Task 4, with Task 3's tests still passing after the swap (they compare distinct numeric triples only). `config::ENV_LOCK` is made `pub(crate)` in Task 1 and used in Task 6's `env_fixture`.

**Two places the spec left a choice, and what was decided**

- **A fetch that succeeded but named no usable version** (every entry yanked, an empty entry, an unreadable `vers`) is recorded as a **failure** — `failed_at` set, `latest` preserved — not as a success. The spec lists these under `Unknown` for display but does not say which interval they earn. "Could not determine the newest version" is the one-hour case, not the twenty-four-hour one, and folding it into a success would silence the check for a day over something that may be a transient index problem. Task 6, `an_index_with_nothing_usable_is_a_failure_not_a_success`.
- **The state file's location.** The spec says "a small file in the state directory". It cannot be a bare `*.json` at the top of that directory: `registry::known_projects_inner` (`src/registry.rs:1085-1104`) turns every such file into a project row, which is the phantom-project bug `src/notify.rs:84-105` already records. It is therefore `state_dir()/update/check.json`, and Task 5 asserts the subdirectory so a later "simplification" back to the top level fails.
