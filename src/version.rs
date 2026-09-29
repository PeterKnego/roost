//! Is there a newer roost? See
//! `docs/superpowers/specs/2026-09-12-version-check-design.md`.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

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
    // `Builder::spawn`, not `thread::spawn`: the latter panics when the OS
    // refuses a thread (EAGAIN), which would escape into the websocket connect
    // thread that called us *and* leave the guard taken for the life of the
    // process. No test can make the OS refuse a thread, so this arm is
    // untested by design rather than covered by a test that could not fail.
    let spawned = std::thread::Builder::new()
        .name("roost-version-check".into())
        .spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                check_once_with(fetch);
            }));
            IN_FLIGHT.store(false, Ordering::SeqCst);
        });
    if let Err(e) = spawned {
        eprintln!("roost: version check: could not start a thread: {e}");
        IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    IN_FLIGHT.store(false, Ordering::SeqCst);
    LOADED.store(false, Ordering::SeqCst);
    *cell().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

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

    /// Every branch of the comparison, as a table.
    ///
    /// Two of the three outcomes are cheerful and the third is the one that
    /// matters, so the unreadable rows are here in force: "could not tell" is
    /// never "you are up to date".
    ///
    /// Revert-checked: swapping the operands in `verdict`'s `compare` call
    /// (`compare(l, running)` instead of `compare(running, l)`) fails at
    /// "an index behind the running binary is not an upgrade: 0.5.2 vs
    /// Some(\"0.5.1\")" with `left: Newer("0.5.1")`, `right: UpToDate` — the
    /// `for` loop's `assert_eq!` stops at the *first* row that disagrees
    /// under the swap, which is this one (row 2), not the "a patch release"
    /// row further down. The row right after the equal case is what makes
    /// the swap visible.
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
        //
        // `state_path()` reads the process-global `ROOST_STATE_DIR` (via
        // `wsstate::state_dir()`) even though this test never sets it itself
        // — a concurrently-running test's `set_var` is still a data race on
        // that read without the lock, per `STATE_ENV_LOCK`'s doc comment.
        let _envg = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    ///
    /// Revert-checked: a `verdict` that returns `Newer` whenever the file
    /// holds a version — the shape a stored `"outcome"` would produce — fails
    /// here with `left: Newer("0.5.3"), right: UpToDate`.
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
    ///
    /// Revert-checked. Removing the guard fails with `left: 10, right: 1`.
    /// Moving it *inside* the spawned closure (the staleness test left on the
    /// calling thread) does **not** fail this test — five runs, all green: the
    /// swap is atomic, so exactly one of the ten threads wins it while the
    /// fetch blocks. What that placement gets wrong is the window after the
    /// winner releases the guard: a thread that passed the staleness test
    /// before the check finished, but was scheduled only after, swaps a free
    /// guard and fetches again against an answer that is now fresh. That race
    /// has no seam to force it, so this test cannot see it; the guard is taken
    /// on the calling thread, before the staleness test, so the window does
    /// not exist rather than being rare.
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
        // On the guard, not on `current()`: the thread stores its answer
        // before it releases the guard and finishes, so waiting on the answer
        // alone could return with the thread still running into a later
        // test's ROOST_STATE_DIR.
        for _ in 0..200 {
            if !IN_FLIGHT.load(SeqCst) { break }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!IN_FLIGHT.load(SeqCst), "the check thread finished");
        assert_eq!(CALLS.load(SeqCst), 1, "and still once after it finished");
        assert_eq!(current().unwrap().latest.as_deref(), Some("9.9.9"));
        RELEASE.store(false, SeqCst);
    }

    /// CLAUDE.md: no panic may escape a socket or watcher thread. A fetch that
    /// panics is caught inside `check_once_with` and recorded as a failure,
    /// so it earns the one-hour retry like any other. Deleting the inner
    /// `catch_unwind` fails this on "the panic is caught inside".
    #[test]
    fn a_panicking_fetch_is_caught_and_recorded_as_a_failure() {
        let (_g1, _g2, _d) = env_fixture(true);
        let s = std::panic::catch_unwind(|| check_once_with(panicking_fetch));
        assert!(s.is_ok(), "the panic is caught inside, not by the caller");
        let s = s.unwrap();
        assert!(s.failed_at.is_some(), "a panicking fetch is a failure, not a success");
        assert_eq!(s.latest, None);
    }

    /// The real spawn path: a check thread whose fetch panics must still
    /// release the in-flight guard, or every later check in the process is
    /// silently skipped. Bounded polls throughout, so a stuck guard fails
    /// rather than hangs.
    ///
    /// Revert-checked. Removing the guard release after the thread's
    /// `catch_unwind` fails on "the guard is released after a panicking
    /// check". Two catches stand between the panic and the guard, and each
    /// alone suffices: removing only the inner one (in `check_once_with`) or
    /// only the outer one (around it in the thread) passes; removing both
    /// fails the same way — the thread dies with the guard taken.
    #[test]
    fn a_panicking_check_thread_releases_the_guard() {
        let (_g1, _g2, _d) = env_fixture(true);
        fn counting_fetch(_url: &str) -> Result<String, String> {
            CALLS.fetch_add(1, SeqCst);
            Ok("{\"name\":\"roost\",\"vers\":\"9.9.9\",\"yanked\":false}\n".to_string())
        }
        maybe_check_with(panicking_fetch);
        let wait = |done: &dyn Fn() -> bool| {
            for _ in 0..200 {
                if done() { return }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        wait(&|| CALLS.load(SeqCst) >= 1);
        assert_eq!(CALLS.load(SeqCst), 1, "the panicking fetch ran");
        wait(&|| !IN_FLIGHT.load(SeqCst));
        assert!(!IN_FLIGHT.load(SeqCst), "the guard is released after a panicking check");

        // The failure just recorded is cooling for an hour; make the state
        // stale again so only the guard can stop the next check.
        store(State::default());
        maybe_check_with(counting_fetch);
        wait(&|| CALLS.load(SeqCst) >= 2);
        wait(&|| !IN_FLIGHT.load(SeqCst));
        assert_eq!(CALLS.load(SeqCst), 2, "the next check ran, exactly once");
        assert_eq!(current().unwrap().latest.as_deref(), Some("9.9.9"));
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
}
