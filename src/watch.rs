//! Filesystem watching. deadlight is for AI engineering: Claude edits files
//! in the background, so a viewer that does not reflect that is showing
//! something false. Classification is pure so the routing table is testable
//! without an OS event or a sleep.
use crate::hub::Hub;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    Tree,
    Status,
    Buffer(String),
    Ignore,
}

/// Pure. `rel` is project-relative with `/` separators.
pub fn classify(rel: &str, open_buffers: &[String], hide: &[String]) -> Class {
    let first = rel.split('/').next().unwrap_or("");
    if first == ".git" {
        return match rel {
            ".git/index" | ".git/HEAD" => Class::Status,
            _ => Class::Ignore,
        };
    }
    if crate::projects::SKIP_DIRS.contains(&first) || hide.iter().any(|h| h == first) {
        return Class::Ignore;
    }
    if open_buffers.iter().any(|b| b == rel) {
        return Class::Buffer(rel.to_string());
    }
    Class::Tree
}

/// True when this event was caused by deadlight's own save. Consumes the
/// record, so a later external edit that happens to reproduce the same
/// content is not swallowed too.
pub fn is_self_write(
    seen: &mut std::collections::HashMap<String, u64>,
    rel: &str,
    disk_hash: u64,
) -> bool {
    match seen.get(rel) {
        Some(h) if *h == disk_hash => {
            seen.remove(rel);
            true
        }
        _ => false,
    }
}

type ProjectDebouncer =
    notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>;

/// inotify's default `max_user_watches` is commonly 8192–65536 depending on
/// the distro (tunable via `fs.inotify.max_user_watches`, but deadlight
/// can't assume it was tuned). Once this many directories are watched, stop
/// registering more instead of either erroring the walk out or silently
/// eating the whole machine's inotify budget. VS Code's watcher backend and
/// IntelliJ's `fsnotifier` both degrade the same way on Linux — a visible
/// "incomplete" state, or a fallback to periodic scans — rather than
/// failing or hanging, and that's what `watch_degraded` communicates to the
/// UI here.
// Only the per-directory (Linux) `watch_tree` and the tests below use this
// and `collect_watch_dirs` outside of a `cargo test` build; on macOS/Windows
// a plain `cargo build` would otherwise warn about both as dead code.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
const MAX_WATCHED_DIRS: usize = 8192;

/// Collect directories under `root` (skipping SKIP_DIRS), up to a total of
/// `cap` counted from `already` — so the initial walk and later calls that
/// pick up newly-created directories share one budget for the life of a
/// watcher rather than each independently walking past the cap. Returns the
/// directories to watch and whether the cap was hit.
///
/// Pure filesystem walking, no OS watch calls, so the cap can be tested
/// directly without a real watcher.
///
/// Uses `DirEntry::file_type()`, not `Path::is_dir()`: the latter follows
/// symlinks, so a self-referential symlink inside the project (`ln -s .
/// loop`) would make this recurse forever.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn collect_watch_dirs(root: PathBuf, already: usize, cap: usize) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut count = already;
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        if count >= cap {
            return (out, true);
        }
        count += 1;
        // The directory itself counts toward the cap and is returned
        // whether or not it can be listed — matching the pre-cap behavior
        // of watching every directory reached, even one whose contents
        // turn out to be unreadable.
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir && !crate::projects::SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(p);
                }
            }
        }
        out.push(d);
    }
    (out, false)
}

/// Register a watch on `root` and (Linux/other) every directory beneath it,
/// or (macOS/Windows) `root` alone. Used both for the initial walk in
/// `spawn` and, from inside the running watcher, to pick up directories
/// created after startup — except on the recursive platforms, where the
/// caller skips that second use entirely since one watch already covers
/// the whole subtree.
///
/// inotify has no recursive mode, so Linux registers one non-recursive
/// watch per directory, bounded by `MAX_WATCHED_DIRS` (see its doc comment)
/// via the `watched` counter the caller threads through every call for the
/// life of one watcher. FSEvents (macOS) and ReadDirectoryChangesW
/// (Windows) watch subtrees natively and cheaply, so there registering one
/// `Recursive` watch on `root` replaces the whole walk — no per-directory
/// cap needed, and no filtering of SKIP_DIRS at registration time either:
/// `classify` already filters those out of the events this delivers, so
/// the cost is some discarded events, not incorrect behavior.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn watch_tree(debouncer: &mut ProjectDebouncer, project: &str, root: PathBuf, watched: &mut usize) -> bool {
    use notify::RecursiveMode;
    let already = *watched;
    let (dirs, hit_cap) = collect_watch_dirs(root, already, MAX_WATCHED_DIRS);
    if hit_cap && already < MAX_WATCHED_DIRS {
        // Only log the transition into degraded, not once per subsequent
        // directory discovered afterward — those all hit the same cap.
        eprintln!(
            "deadlight: {project}: hit the {MAX_WATCHED_DIRS}-directory watch cap; \
             file-change tracking is now incomplete for this project"
        );
    }
    let mut ok = !hit_cap;
    for d in dirs {
        if let Err(e) = debouncer.watch(&d, RecursiveMode::NonRecursive) {
            eprintln!("deadlight: {project}: failed to watch {}: {e}", d.display());
            ok = false;
            continue;
        }
        *watched += 1;
    }
    ok
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn watch_tree(debouncer: &mut ProjectDebouncer, project: &str, root: PathBuf) -> bool {
    use notify::RecursiveMode;
    match debouncer.watch(&root, RecursiveMode::Recursive) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("deadlight: {project}: failed to watch {}: {e}", root.display());
            false
        }
    }
}

/// True when a panic anywhere in one debounced batch's handling should not
/// be allowed to end watching for the rest of the process's life. Stringify
/// the payload defensively: `Any` panic payloads are almost always `&str`
/// or `String`, but not guaranteed to be, and this is a log line, not a
/// place to panic-on-panic.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Returns false if watching could not be established (e.g. inotify
/// instance limits) — correctness never depends on it, so callers only use
/// this to flag the workspace as degraded, not to fail project setup.
pub fn spawn(project: &str, dir: PathBuf, hub: Arc<Mutex<Hub>>, debounce: Duration) -> bool {
    use notify::RecursiveMode;
    // The OS reports fully-resolved paths in events (e.g. FSEvents on macOS
    // resolves `/var` -> `/private/var`), but callers may hand us a path
    // that still has a symlink component (a temp dir root, or a project
    // root itself symlinked). Canonicalize once up front so `strip_prefix`
    // below is comparing like with like — otherwise every event is silently
    // dropped by the `let Ok(rel) = ... else { continue }` guard, and
    // watching quietly does nothing.
    let dir = dir.canonicalize().unwrap_or(dir);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = match notify_debouncer_full::new_debouncer(debounce, None, tx) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("deadlight: watcher unavailable for {project}: {e}");
            return false;
        }
    };
    // On the per-directory (Linux/other) platform, `watched` is the running
    // total shared between this initial walk and every later call that
    // picks up a directory created after startup, so the cap in
    // `watch_tree` applies across the whole life of the watcher, not per
    // call.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut watched = 0usize;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let ok = watch_tree(&mut debouncer, project, dir.clone(), &mut watched);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let ok = watch_tree(&mut debouncer, project, dir.clone());
    // .git itself is skipped by the per-directory walk (it's in SKIP_DIRS),
    // and isn't implied by the recursive root watch either on the other
    // platforms — index/HEAD drive the status pane, so watch that one
    // directory deliberately regardless of which path was taken above.
    let _ = debouncer.watch(&dir.join(".git"), RecursiveMode::NonRecursive);

    let base = dir.clone();
    let project_name = project.to_string();
    std::thread::spawn(move || {
        // Dropping the debouncer stops the watch, so it must stay bound for
        // the loop's whole lifetime regardless of platform. Only the
        // per-directory (Linux) platform ever reads it again afterward, to
        // register a watch on a directory created after startup.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut keep = debouncer;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let _keep = debouncer;
        for res in rx {
            let Ok(events) = res else { continue };
            // One panic anywhere below must not silently end watching for
            // this project for the rest of the process's life: the spec
            // treats a live view of external edits as a core requirement,
            // not a nicety, so losing it needs to be loud and recoverable,
            // not a quiet, permanent stop.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // First pass: resolve every event to a project-relative
                // path, and — per-directory platforms only — eagerly
                // register a watch on any newly created directory. This
                // does not touch the hub — no lock is held across the
                // filesystem I/O in `watch_tree` (readdir plus watch
                // registration). Necessary on Linux: unlike macOS FSEvents,
                // inotify never reports anything from inside a directory
                // that was never explicitly watched, so a `git checkout` or
                // a plain `mkdir` that adds a new directory would go
                // permanently unseen without this. Unnecessary — and
                // skipped — on macOS/Windows, where the single recursive
                // watch registered in `spawn` already covers any directory
                // created later, existing or not.
                let mut rels: Vec<String> = Vec::new();
                for ev in &events {
                    for path in &ev.paths {
                        let Ok(rel) = path.strip_prefix(&base) else { continue };
                        rels.push(rel.to_string_lossy().replace('\\', "/"));
                        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                        {
                            let skip = path
                                .file_name()
                                .map(|n| crate::projects::SKIP_DIRS.contains(&n.to_string_lossy().as_ref()))
                                .unwrap_or(true);
                            // `symlink_metadata`, not `metadata`: do not
                            // follow a symlink into recursing on it, same
                            // reasoning as `watch_tree`'s use of
                            // `file_type()`.
                            let is_dir =
                                std::fs::symlink_metadata(path).map(|m| m.is_dir()).unwrap_or(false);
                            if !skip && is_dir {
                                watch_tree(&mut keep, &project_name, path.clone(), &mut watched);
                            }
                        }
                    }
                }

                // Lock once per debounced batch, not once per event: the
                // batch is already coalesced by the debouncer, and
                // re-locking per event would mean more lock/unlock churn
                // for no benefit — the lock is never held across blocking
                // I/O either way, since `file_changed_externally`'s read is
                // a small local filesystem read, not a network or PTY
                // write.
                let mut h = Hub::lock(&hub);
                let open: Vec<String> = h.ws.buffers.keys().cloned().collect();
                let mut tree = false;
                let mut status = false;
                // A single save (temp file + rename) fires several raw
                // events (Create, Modify(Name), Modify(Data)) for the same
                // path within one debounced batch. Collect the distinct
                // buffer paths first and visit each once: `is_self_write`
                // consumes its match, so calling `file_changed_externally`
                // per raw event would let the first occurrence eat the
                // suppression token and the second occurrence in the same
                // batch would read as a "real" external edit — echoing the
                // save straight back at the author, exactly what
                // self-write suppression exists to prevent.
                let mut buffer_rels = std::collections::HashSet::new();
                for rel in &rels {
                    match classify(rel, &open, &[]) {
                        Class::Tree => tree = true,
                        Class::Status => status = true,
                        Class::Buffer(r) => {
                            buffer_rels.insert(r);
                        }
                        Class::Ignore => {}
                    }
                }
                for r in buffer_rels {
                    // A deleted buffer's file can't be read back, and
                    // `classify` already routed it away from `Class::Tree`,
                    // so nothing else here would ever refresh the listing
                    // for it — treat "could not read" as a tree change too.
                    if !h.file_changed_externally(&base, &r) {
                        tree = true;
                    }
                }
                if tree {
                    h.broadcast(&crate::proto::Event::TreeChanged);
                }
                if status {
                    h.broadcast(&crate::proto::Event::StatusChanged);
                }
            }));
            if let Err(payload) = outcome {
                eprintln!(
                    "deadlight: {project_name}: watcher batch panicked, continuing: {}",
                    panic_message(payload.as_ref())
                );
            }
        }
    });
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bufs() -> Vec<String> {
        vec!["src/main.rs".to_string()]
    }

    #[test]
    fn git_index_and_head_drive_the_status_pane() {
        assert!(matches!(classify(".git/index", &bufs(), &[]), Class::Status));
        assert!(matches!(classify(".git/HEAD", &bufs(), &[]), Class::Status));
    }

    #[test]
    fn other_git_internals_are_ignored() {
        assert!(matches!(classify(".git/objects/ab/cdef", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify(".git/logs/HEAD", &bufs(), &[]), Class::Ignore));
    }

    #[test]
    fn open_buffers_beat_the_generic_tree_class() {
        match classify("src/main.rs", &bufs(), &[]) {
            Class::Buffer(rel) => assert_eq!(rel, "src/main.rs"),
            other => panic!("expected Buffer, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_files_refresh_the_tree() {
        assert!(matches!(classify("src/other.rs", &bufs(), &[]), Class::Tree));
        assert!(matches!(classify("README.md", &bufs(), &[]), Class::Tree));
    }

    #[test]
    fn skip_dirs_and_hide_are_ignored_entirely() {
        // a cargo build must not generate a storm of tree refreshes
        assert!(matches!(classify("target/debug/deadlight", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify("node_modules/x/y.js", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify(".venv/lib/p.py", &bufs(), &[]), Class::Ignore));
        let hide = vec!["dist".to_string()];
        assert!(matches!(classify("dist/bundle.js", &bufs(), &hide), Class::Ignore));
    }

    #[test]
    fn self_writes_are_suppressed_once() {
        let mut seen = std::collections::HashMap::new();
        seen.insert("a.rs".to_string(), 42u64);
        // deadlight just wrote this content; the resulting event is ours
        assert!(is_self_write(&mut seen, "a.rs", 42));
        // and only once — a later external edit with other content is real
        assert!(!is_self_write(&mut seen, "a.rs", 43));
    }

    // `spawn`'s initial tree walk runs synchronously on the caller — in
    // production, a connection thread inside `Hub::for_project` — so a
    // symlink that resolves back into the tree it's inside of must not be
    // followed, or the walk (and the HTTP request behind it) never returns.
    #[cfg(unix)]
    #[test]
    fn symlink_loop_does_not_hang_the_walk() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sd = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());

        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "hi").unwrap();
        std::os::unix::fs::symlink(".", d.path().join("loop")).unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let dir = d.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        // Run the call on its own thread and bound the wait: a regression
        // here should fail this test, not hang the whole suite.
        std::thread::spawn(move || {
            let ok = spawn("proj", dir, hub, Duration::from_millis(50));
            let _ = tx.send(ok);
        });
        let ok = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("spawn must return promptly even with a symlink loop in the tree");
        assert!(ok);

        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    // The per-directory (Linux) watch path must stop growing once it hits
    // MAX_WATCHED_DIRS rather than walking (and trying to watch) every one
    // of a huge tree's directories — this is the bounded-walk half of the
    // large-project fix; `collect_watch_dirs` is exercised directly here so
    // the test doesn't depend on inotify or run on a platform that has it.
    #[test]
    fn bounded_walk_stops_at_the_cap() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::create_dir(d.path().join(format!("d{i}"))).unwrap();
        }
        // root + 50 children = 51 directories total, well under a cap of 10.
        let (dirs, hit_cap) = collect_watch_dirs(d.path().to_path_buf(), 0, 10);
        assert!(hit_cap, "a tree bigger than the cap must report it was hit");
        assert_eq!(dirs.len(), 10, "must stop exactly at the cap, not walk past it");
    }

    #[test]
    fn bounded_walk_does_not_hit_the_cap_on_a_small_tree() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        std::fs::create_dir(d.path().join("b")).unwrap();
        let (dirs, hit_cap) = collect_watch_dirs(d.path().to_path_buf(), 0, 8192);
        assert!(!hit_cap);
        assert_eq!(dirs.len(), 3, "root plus its two children");
    }

    #[test]
    fn bounded_walk_honors_an_already_count_from_a_prior_call() {
        // Mirrors how `watch_tree` threads `watched` across the initial
        // walk and later per-created-directory calls: a call that starts
        // already at the cap must add nothing and report degraded.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        let (dirs, hit_cap) = collect_watch_dirs(d.path().to_path_buf(), 5, 5);
        assert!(hit_cap);
        assert!(dirs.is_empty());
    }
}
