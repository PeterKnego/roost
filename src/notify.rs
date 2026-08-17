//! Server-side notice store. Bounded, persisted, and global rather than
//! per-project: what needs your attention is a property of the machine, not
//! of whichever project happens to be on screen, and a browser sitting on one
//! project must still learn that another one wants something.
//!
//! Persistence — not just in-memory queueing — is what makes a notice raised
//! at 3am survive a deadlight restart rather than only a closed tab.
//!
//! The mutex here is a leaf lock: taken for bookkeeping, released before any
//! broadcast or hub lock. `record` deliberately does not broadcast for that
//! reason; see `notify_and_broadcast` in hub.rs.
use crate::osc::Parsed;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_NOTICES: usize = 100;
pub const RATE_LIMIT_PER_MIN: usize = 10;
const WINDOW_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub id: u64,
    pub project: String,
    pub session: String,
    pub title: String,
    pub body: String,
    pub at: u64,
    pub read: bool,
}

#[derive(Default)]
struct Window {
    started: u64,
    count: usize,
    suppressed: usize,
}

#[derive(Default)]
struct Store {
    notices: VecDeque<Notice>,
    next_id: u64,
    windows: HashMap<String, Window>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

/// Serialises `persist()` against itself. `record`/`mark_read`/`mark_all_read`/
/// `clear` all release the store mutex before calling `persist`, and all of
/// them write the same `notifications.json.tmp` path — so two racing callers
/// (the PTY pump via `record`, a connection thread via `mark_read`) can
/// interleave their write-then-rename: one's rename can find the other's tmp
/// file already gone (a lost update), or worse, a slower writer's `O_TRUNC`
/// truncate-and-write can land *after* a faster writer already renamed,
/// leaving a zero-length file for `rename` to move into place — which makes
/// `load()` hit its corrupt-file arm and discard the whole history, not just
/// the racing update. This must be a lock of its own, not the store mutex:
/// the store mutex is the leaf lock `record` et al. deliberately release
/// before doing I/O (see the module doc), and holding it across a file write
/// would put a leaf lock across blocking I/O.
static PERSIST_LOCK: Mutex<()> = Mutex::new(());

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path() -> std::path::PathBuf {
    crate::wsstate::state_dir().join("notifications.json")
}

/// Assign an id, apply the rate limit, evict, persist. Returns the notice to
/// broadcast, or `None` when the session is over its limit. Never panics:
/// this runs on the PTY pump thread, which must keep pumping regardless.
pub fn record(project: &str, session: &str, p: Parsed) -> Option<Notice> {
    let ts = now();
    let notice = {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        // Bound the rate-limit map by activity rather than by uptime. An
        // entry whose window has expired and that carries no undelivered
        // suppression count has nothing left to say. Without this, the map
        // gains one permanent entry per session name ever seen — and this
        // server's whole job is spinning worktree sessions up and down, so
        // that is unbounded in practice, not merely in theory. Everything
        // else in deadlight is capped; this must be too.
        s.windows.retain(|_, w| ts.saturating_sub(w.started) < WINDOW_SECS || w.suppressed > 0);
        let key = format!("{project}/{session}");
        let w = s.windows.entry(key).or_default();
        if ts.saturating_sub(w.started) >= WINDOW_SECS {
            // A new window resets the allowance but carries the suppression
            // count across. Counting drops exists solely to tell the user
            // they happened, and the next admitted notice is the first
            // chance to say so — zeroing here would silently discard the
            // very thing the count is for.
            let carried = w.suppressed;
            *w = Window { started: ts, count: 0, suppressed: carried };
        }
        if w.count >= RATE_LIMIT_PER_MIN {
            // Dropped, but counted: a runaway loop must not be able to evict
            // the ring, and the user should learn that it happened.
            w.suppressed += 1;
            return None;
        }
        w.count += 1;
        let suppressed = std::mem::take(&mut w.suppressed);

        s.next_id += 1;
        let mut body = p.body;
        if suppressed > 0 {
            // p.body is already truncated to MAX_BODY by osc::sanitise, so
            // simply appending here and truncating the result from the front
            // would — whenever the body was already at the cap, which is
            // exactly when a suppression count is most likely — discard the
            // whole suffix and keep none of the information it exists to
            // report. Reserve room for the suffix instead, so it always
            // survives and the total still never exceeds MAX_BODY.
            let suffix = format!(" ({suppressed} suppressed)");
            let avail = crate::osc::MAX_BODY.saturating_sub(suffix.chars().count());
            let truncated: String = body.chars().take(avail).collect();
            body = format!("{truncated}{suffix}");
        }
        let n = Notice {
            id: s.next_id,
            project: project.to_string(),
            session: session.to_string(),
            title: p.title.unwrap_or_else(|| session.to_string()),
            body,
            at: ts,
            read: false,
        };
        s.notices.push_back(n.clone());
        while s.notices.len() > MAX_NOTICES {
            s.notices.pop_front();
        }
        n
    };
    persist();
    Some(notice)
}

pub fn list() -> Vec<Notice> {
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    s.notices.iter().cloned().collect()
}

pub fn mark_read(id: u64) {
    {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = s.notices.iter_mut().find(|n| n.id == id) {
            n.read = true;
        }
    }
    persist();
}

/// Marks every notice read in one pass, for the panel's "Mark all read"
/// button — the per-id `mark_read` would need one round trip per notice.
pub fn mark_all_read() {
    {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        for n in s.notices.iter_mut() {
            n.read = true;
        }
    }
    persist();
}

pub fn clear() {
    {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        s.notices.clear();
    }
    persist();
}

/// Read-then-rename, like wsstate::save: a crash mid-write must leave the old
/// file intact rather than a half-written one. Failures are logged, never
/// propagated — notifications are best-effort, and a full disk must not take
/// down a terminal.
fn persist() {
    // Held across the write+rename below, deliberately: see PERSIST_LOCK's
    // doc comment for why a lock across this I/O is correct here even though
    // the store's own leaf lock must never be.
    let _guard = PERSIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snapshot: Vec<Notice> = list();
    let dir = crate::wsstate::state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return eprintln!("deadlight: notifications dir: {e}");
    }
    // The same hardening wsstate::save applies, and for a sharper reason:
    // a notice body is terminal output, so it must not be readable by other
    // local users. Done here rather than relying on some earlier
    // wsstate::save having already tightened the directory — a notice can
    // be recorded before any workspace has ever been saved.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let Ok(json) = serde_json::to_string(&snapshot) else { return };
    let tmp = path().with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
        return eprintln!("deadlight: notifications write: {e}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path()) {
        eprintln!("deadlight: notifications rename: {e}");
        #[cfg(test)]
        RENAME_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Test-only: counts `rename` failures from `persist()`, which is otherwise
/// only observable as an stderr line. Without PERSIST_LOCK, a racing writer
/// can find its own tmp file already moved away by a faster writer's rename
/// — this is that failure, counted rather than merely printed, so a test can
/// assert on it directly instead of scraping stderr.
#[cfg(test)]
static RENAME_FAILURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Called once at startup. A corrupt or absent file leaves an empty store —
/// losing notification history is not worth failing a boot over.
pub fn load() {
    let Ok(text) = std::fs::read_to_string(path()) else { return };
    let Ok(list) = serde_json::from_str::<Vec<Notice>>(&text) else {
        return eprintln!("deadlight: notifications.json unreadable, starting empty");
    };
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    // Resume the counter past the highest persisted id, or a reboot would
    // hand out ids that already name a different notice.
    s.next_id = list.iter().map(|n| n.id).max().unwrap_or(0);
    s.notices = list.into();
}

/// Lets the eviction test observe the map that is supposed to stay bounded.
#[cfg(test)]
pub fn window_count() -> usize {
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    s.windows.len()
}

#[cfg(test)]
pub fn reset_for_test() {
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    *s = Store::default();
    RENAME_FAILURES.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
pub fn rename_failures() -> usize {
    RENAME_FAILURES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Ages out a session's rate-limit window so a test can cross it without
/// sleeping 60 seconds.
#[cfg(test)]
pub fn expire_window_for_test(project: &str, session: &str) {
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = s.windows.get_mut(&format!("{project}/{session}")) {
        w.started = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::Parsed;

    fn parsed(body: &str) -> Parsed {
        Parsed { title: Some("t".into()), body: body.into() }
    }

    /// Every test here mutates process-global state (the store and
    /// DEADLIGHT_STATE_DIR); cargo runs tests in parallel threads.
    fn setup() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        let g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        reset_for_test();
        (g, d)
    }

    #[test]
    fn record_assigns_increasing_ids_and_keeps_server_side_attribution() {
        let (_g, _d) = setup();
        let a = record("karpie", "claude", parsed("one")).unwrap();
        let b = record("deadlight", "shell", parsed("two")).unwrap();
        assert!(b.id > a.id, "ids must increase: {} then {}", a.id, b.id);
        assert_eq!(a.project, "karpie");
        assert_eq!(a.session, "claude");
        assert_eq!(b.project, "deadlight");
        assert!(!a.read);
        assert_eq!(list().len(), 2);
    }

    #[test]
    fn the_ring_evicts_oldest_first() {
        let (_g, _d) = setup();
        for i in 0..MAX_NOTICES + 5 {
            // A fresh session name each time, or the rate limiter would fire.
            record("p", &format!("s{i}"), parsed(&format!("n{i}")));
        }
        let all = list();
        assert_eq!(all.len(), MAX_NOTICES, "ring must stay capped");
        assert_eq!(all.first().unwrap().body, "n5", "oldest five must be gone");
        assert_eq!(all.last().unwrap().body, format!("n{}", MAX_NOTICES + 4));
    }

    #[test]
    fn the_rate_limiter_admits_the_cap_then_rejects_and_counts() {
        let (_g, _d) = setup();
        for i in 0..RATE_LIMIT_PER_MIN {
            assert!(record("p", "loop", parsed(&format!("n{i}"))).is_some(), "notice {i} rejected");
        }
        assert!(record("p", "loop", parsed("over")).is_none(), "the 11th must be dropped");
        assert!(record("p", "loop", parsed("over2")).is_none());
        // Another session is unaffected — the limit is per session, not global.
        assert!(record("p", "other", parsed("fine")).is_some());
        assert_eq!(list().len(), RATE_LIMIT_PER_MIN + 1);
    }

    #[test]
    fn suppressed_notices_are_reported_on_the_next_admitted_one() {
        let (_g, _d) = setup();
        for i in 0..RATE_LIMIT_PER_MIN {
            record("p", "loop", parsed(&format!("n{i}")));
        }
        record("p", "loop", parsed("dropped1"));
        record("p", "loop", parsed("dropped2"));
        expire_window_for_test("p", "loop");
        let n = record("p", "loop", parsed("back")).unwrap();
        assert!(n.body.contains("back"), "original body must survive: {:?}", n.body);
        assert!(n.body.contains('2'), "must report 2 suppressed: {:?}", n.body);
    }

    #[test]
    fn the_suppressed_suffix_cannot_push_the_body_over_its_own_cap() {
        // p.body already arrives at exactly MAX_BODY (osc::sanitise's job);
        // appending "(N suppressed)" after that must still respect the cap,
        // not exceed it — the suffix has to eat into the body, not add to it.
        let (_g, _d) = setup();
        let full = parsed(&"x".repeat(crate::osc::MAX_BODY));
        for _ in 0..RATE_LIMIT_PER_MIN {
            record("p", "loud", full.clone());
        }
        record("p", "loud", full.clone()); // dropped, suppressed count = 1
        expire_window_for_test("p", "loud");
        let n = record("p", "loud", full).unwrap();
        assert!(n.body.contains("suppressed"), "the suffix must survive, not just fit: got {:?}", n.body);
        assert_eq!(
            n.body.chars().count(),
            crate::osc::MAX_BODY,
            "total length must stay at the cap, not exceed it: {:?}",
            n.body
        );
    }

    #[test]
    fn mark_all_read_clears_every_notice_not_just_one() {
        let (_g, _d) = setup();
        record("p", "s1", parsed("one"));
        record("p", "s2", parsed("two"));
        record("p", "s3", parsed("three"));
        mark_all_read();
        let all = list();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|n| n.read), "every notice must be marked, got {all:?}");
    }

    #[test]
    fn mark_read_and_clear_change_what_list_returns() {
        let (_g, _d) = setup();
        let a = record("p", "s1", parsed("one")).unwrap();
        record("p", "s2", parsed("two"));
        mark_read(a.id);
        let all = list();
        assert!(all.iter().find(|n| n.id == a.id).unwrap().read);
        assert!(!all.iter().find(|n| n.id != a.id).unwrap().read, "only the named id");
        clear();
        assert!(list().is_empty());
    }

    #[test]
    fn state_survives_a_reload_including_ids_and_read_flags() {
        let (_g, _d) = setup();
        let a = record("karpie", "claude", parsed("survive me")).unwrap();
        record("karpie", "shell", parsed("second"));
        mark_read(a.id);
        reset_for_test(); // drops the in-memory store, keeps the file
        assert!(list().is_empty(), "reset must really empty the store, or this proves nothing");
        load();
        let all = list();
        assert_eq!(all.len(), 2, "both notices must come back");
        assert_eq!(all[0].id, a.id, "ids must survive");
        assert!(all[0].read, "read flag must survive");
        assert_eq!(all[0].body, "survive me");
        // A new notice must not reuse a persisted id.
        let c = record("karpie", "third", parsed("after reload")).unwrap();
        assert!(c.id > all[1].id, "id counter must resume past the loaded max");
    }

    #[test]
    fn a_corrupt_state_file_is_ignored_rather_than_fatal() {
        let (_g, d) = setup();
        std::fs::write(d.path().join("notifications.json"), b"{ not json").unwrap();
        load(); // must not panic
        assert!(list().is_empty());
        assert!(record("p", "s", parsed("still works")).is_some());
    }

    #[test]
    fn expired_rate_limit_windows_are_evicted() {
        let (_g, _d) = setup();
        record("p", "gone", parsed("one")).unwrap();
        assert_eq!(window_count(), 1);
        expire_window_for_test("p", "gone");
        record("p", "here", parsed("two")).unwrap();
        assert_eq!(window_count(), 1, "the expired window must be evicted, not accumulate");

        // But an expired window still holding an undelivered suppression
        // count must survive, or the count it exists to report is lost.
        for i in 0..RATE_LIMIT_PER_MIN {
            record("p", "loud", parsed(&format!("n{i}")));
        }
        record("p", "loud", parsed("dropped"));
        expire_window_for_test("p", "loud");
        record("p", "other", parsed("three")).unwrap(); // triggers a prune
        let n = record("p", "loud", parsed("back")).unwrap();
        assert!(n.body.contains('1'), "suppression count must survive the prune: {:?}", n.body);
    }

    #[test]
    fn persisted_state_is_not_readable_by_other_users() {
        let (_g, d) = setup();
        record("p", "s", parsed("private terminal output")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = std::fs::metadata(d.path().join("notifications.json")).unwrap();
            assert_eq!(f.permissions().mode() & 0o077, 0, "notifications.json is group/world readable");
            let dir = std::fs::metadata(d.path()).unwrap();
            assert_eq!(dir.permissions().mode() & 0o077, 0, "state dir is group/world readable");
        }
    }

    #[test]
    fn a_missing_title_falls_back_to_the_session_name() {
        let (_g, _d) = setup();
        let n = record("p", "claude", Parsed { title: None, body: "hi".into() }).unwrap();
        assert_eq!(n.title, "claude");
    }

    /// Several threads racing record()->persist() at once, all writing the
    /// same tmp path. This is the shape of the real bug: 8 threads x 200
    /// calls reliably produced ~16 `rename: No such file or directory`
    /// errors without PERSIST_LOCK when this was diagnosed. Asserting
    /// `rename_failures() == 0` is the direct claim PERSIST_LOCK makes —
    /// under the lock, a persist() call always creates its own tmp file
    /// immediately before renaming it, with no other writer able to touch
    /// that path in between, so the rename can never find it already gone.
    /// (A plain "does the final count match" assertion is much weaker here:
    /// once the ring is full, *every* possible snapshot has exactly
    /// MAX_NOTICES entries, so a lost update that swaps one full snapshot
    /// for a slightly different one would not show up in the count at all.)
    #[test]
    fn concurrent_persists_never_lose_a_rename_and_leave_a_file_load_can_recover() {
        let (_g, _d) = setup();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 150;
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..PER_THREAD {
                        record(&format!("p{t}"), &format!("s{i}"), parsed("x")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            rename_failures(),
            0,
            "a concurrent writer found its own tmp file already moved — PERSIST_LOCK should make that impossible"
        );

        reset_for_test();
        load(); // must not hit the corrupt-file arm
        assert_eq!(list().len(), MAX_NOTICES, "the ring should read back full after this many records");
    }
}
