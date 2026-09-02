//! Filesystem watching. roost is for AI engineering: Claude edits files
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
///
/// `filter` is the tree's own visibility rule, so what refreshes the listing
/// and what appears in it stay the same set: with `show_hidden` off a touched
/// dotfile is not a tree change, and with it on it is.
///
/// `.git` is the exception, deliberately ahead of the filter: even when
/// `show_hidden` renders it, a single `git status` writes enough inside it to
/// turn every command into a burst of tree refreshes. Its rows go stale until
/// the directory is re-expanded, which is the recoverable side of that trade.
pub fn classify(rel: &str, open_buffers: &[String], filter: &crate::projects::TreeFilter) -> Class {
    let first = rel.split('/').next().unwrap_or("");
    if first == ".git" {
        return match rel {
            ".git/index" | ".git/HEAD" => Class::Status,
            _ => Class::Ignore,
        };
    }
    if filter.skips(first) {
        return Class::Ignore;
    }
    if open_buffers.iter().any(|b| b == rel) {
        return Class::Buffer(rel.to_string());
    }
    Class::Tree
}

/// True when this event was caused by roost's own save. Consumes the
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

/// Upper bound on how many raw events accumulate into one debounced batch
/// before it's processed regardless of whether the quiet period was
/// reached. Raw `notify` (unlike the old debouncer) delivers several events
/// per single file save — a temp-file-then-rename can be Create,
/// Modify(Name), and Modify(Data) for one path in one save — so a large
/// batch is ordinary, not a sign of trouble. This cap exists only so an
/// unusually bursty change (a big `git checkout` or `rm -rf`) can't grow
/// the batch Vec without bound while new events keep arriving before the
/// debounce timer gets a chance to fire.
const MAX_BATCH_EVENTS: usize = 10_000;

/// inotify's default `max_user_watches` is commonly 8192–65536 depending on
/// the distro (tunable via `fs.inotify.max_user_watches`, but roost
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
fn watch_tree(watcher: &mut notify::RecommendedWatcher, project: &str, root: PathBuf, watched: &mut usize) -> bool {
    use notify::{RecursiveMode, Watcher};
    let already = *watched;
    let (dirs, hit_cap) = collect_watch_dirs(root, already, MAX_WATCHED_DIRS);
    if hit_cap && already < MAX_WATCHED_DIRS {
        // Only log the transition into degraded, not once per subsequent
        // directory discovered afterward — those all hit the same cap.
        eprintln!(
            "roost: {project}: hit the {MAX_WATCHED_DIRS}-directory watch cap; \
             file-change tracking is now incomplete for this project"
        );
    }
    let mut ok = !hit_cap;
    for d in dirs {
        if let Err(e) = watcher.watch(&d, RecursiveMode::NonRecursive) {
            eprintln!("roost: {project}: failed to watch {}: {e}", d.display());
            ok = false;
            continue;
        }
        *watched += 1;
    }
    ok
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn watch_tree(watcher: &mut notify::RecommendedWatcher, project: &str, root: PathBuf) -> bool {
    use notify::{RecursiveMode, Watcher};
    match watcher.watch(&root, RecursiveMode::Recursive) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("roost: {project}: failed to watch {}: {e}", root.display());
            false
        }
    }
}

/// Reads are not changes. inotify reports them — `IN_OPEN` and `IN_ACCESS`
/// arrive as `EventKind::Access` — and roost reads the directories it watches
/// constantly, because rendering the tree is a `read_dir` of exactly the
/// directory the watcher is watching.
///
/// Letting those through was a self-sustaining loop, not merely noise. The
/// batch handler re-registers a watch on any directory an event names, and
/// registering walks that directory, and walking it opens it, which produced
/// the next event. One tree render lit it and it never went out: ~3 batches a
/// second forever, each one broadcasting `TreeChanged` (the project root
/// strips to an empty rel, which classifies as `Class::Tree`), so every
/// browser on the project re-fetched the tree three times a second for as
/// long as the project stayed open. It survived with the client's re-fetch
/// disabled, because by then the watcher was feeding itself.
///
/// Nothing is lost by dropping these: a write that matters arrives as
/// `Modify`/`Create`/`Remove` too, and an `Access(Close(Write))` with no
/// accompanying modification means the file was opened and closed unchanged.
fn is_access(ev: &notify::Event) -> bool {
    matches!(ev.kind, notify::EventKind::Access(_))
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
    use notify::{RecursiveMode, Watcher};
    // The OS reports fully-resolved paths in events (e.g. FSEvents on macOS
    // resolves `/var` -> `/private/var`), but callers may hand us a path
    // that still has a symlink component (a temp dir root, or a project
    // root itself symlinked). Canonicalize once up front so `strip_prefix`
    // below is comparing like with like — otherwise every event is silently
    // dropped by the `let Ok(rel) = ... else { continue }` guard, and
    // watching quietly does nothing.
    let dir = dir.canonicalize().unwrap_or(dir);
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    // Plain `notify`, not `notify-debouncer-full`: the debouncer's
    // rename-correlation queue silently drops `Remove` events under
    // FSEvents' coalescing on macOS (confirmed empirically — raw `notify`
    // on the same delete delivers `Remove(File)`, the debounced stream
    // never does), so a deleted file never reached the UI. Debouncing is
    // done by hand in the loop below instead, on the channel's own
    // `recv_timeout`.
    let mut watcher = match notify::recommended_watcher(move |res| {
        // Runs on notify's internal event thread. The receiver end can
        // already be gone (process shutdown tearing threads down in some
        // order) — a dropped receiver must not panic notify's thread, so
        // the send error is discarded rather than unwrapped.
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("roost: watcher unavailable for {project}: {e}");
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
    let ok = watch_tree(&mut watcher, project, dir.clone(), &mut watched);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let ok = watch_tree(&mut watcher, project, dir.clone());
    // .git itself is skipped by the per-directory walk (it's in SKIP_DIRS),
    // and isn't implied by the recursive root watch either on the other
    // platforms — index/HEAD drive the status pane, so watch that one
    // directory deliberately regardless of which path was taken above.
    let _ = watcher.watch(&dir.join(".git"), RecursiveMode::NonRecursive);

    let base = dir.clone();
    let project_name = project.to_string();
    std::thread::spawn(move || {
        // Dropping the watcher stops the watch, so it must stay bound for
        // the loop's whole lifetime regardless of platform. Only the
        // per-directory (Linux) platform ever reads it again afterward, to
        // register a watch on a directory created after startup.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut keep = watcher;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let _keep = watcher;
        // Hand-rolled debounce: block for the first event of a batch, then
        // keep folding in whatever else arrives within `debounce` of the
        // last one, so a burst of raw events for one save (or one delete)
        // still produces a single downstream batch — matching what the
        // debouncer used to hand this loop, minus the dropped `Remove`s.
        loop {
            let first = match rx.recv() {
                Ok(r) => r,
                // Sender side is gone, which only happens if the watcher
                // itself was dropped — nothing left to watch for.
                Err(_) => break,
            };
            let mut events: Vec<notify::Event> = Vec::new();
            match first {
                // Dropped here rather than later so an access never starts a
                // batch, never counts toward MAX_BATCH_EVENTS, and above all
                // never reaches the re-registration below — see `is_access`.
                Ok(ev) if is_access(&ev) => continue,
                Ok(ev) => events.push(ev),
                Err(e) => {
                    eprintln!("roost: {project_name}: watch error: {e}");
                    continue;
                }
            }
            while events.len() < MAX_BATCH_EVENTS {
                match rx.recv_timeout(debounce) {
                    Ok(Ok(ev)) if is_access(&ev) => continue,
                    Ok(Ok(ev)) => events.push(ev),
                    Ok(Err(e)) => eprintln!("roost: {project_name}: watch error: {e}"),
                    // Quiet period reached (or sender gone, which the next
                    // outer `recv()` will notice and exit on) — either way,
                    // the batch collected so far is ready to process.
                    Err(_) => break,
                }
            }
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
                // batch is already coalesced by the recv loop above, and
                // re-locking per event would mean more lock/unlock churn
                // for no benefit — the lock is never held across blocking
                // I/O either way, since `file_changed_externally`'s read is
                // a small local filesystem read, not a network or PTY
                // write.
                // Read before locking, never after: this is a filesystem
                // read, and the Hub lock is not held across blocking I/O.
                // Re-read per batch rather than captured at spawn, so
                // editing `.roost/config.toml` takes effect on the next
                // change instead of at the next restart, matching the
                // request path.
                // Rename pairs, straight from the kernel. inotify gives both
                // halves of a rename the same cookie and `notify` merges them
                // into a single `Modify(Name(Both))` carrying both paths, so
                // this is *told* where a file went — it does not infer it from
                // a delete and a create landing in the same window, which is
                // what makes it safe to rewrite tabs on. Measured on this
                // host: an in-tree move delivers From, To and Both under one
                // tracker; a move out of the tree delivers From alone and a
                // move in delivers To alone, so neither pairs, and both stay
                // the deletion and the creation they are to a project that
                // cannot see the other end.
                //
                // macOS delivers `RenameMode::Any` with no tracker and no
                // pairing at all ("FSEvents provides no mechanism to associate
                // the old and new sides", notify's own fsevent.rs), so there a
                // rename keeps the older behaviour: the tab demotes and says
                // the file is not found. See
                // docs/superpowers/specs/2026-08-29-follow-external-renames-design.md
                // for why neither notify-debouncer-full nor file-id is used to
                // close that gap.
                let mut renames: Vec<(String, String)> = Vec::new();
                for ev in &events {
                    if !matches!(
                        ev.kind,
                        notify::EventKind::Modify(notify::event::ModifyKind::Name(
                            notify::event::RenameMode::Both
                        ))
                    ) {
                        continue;
                    }
                    if let [from, to] = &ev.paths[..] {
                        if let (Ok(f), Ok(t)) = (from.strip_prefix(&base), to.strip_prefix(&base)) {
                            renames.push((
                                f.to_string_lossy().replace('\\', "/"),
                                t.to_string_lossy().replace('\\', "/"),
                            ));
                        }
                    }
                }

                let settings = crate::config::for_project(&base);
                let mut h = Hub::lock(&hub);
                // Applied before `open` is collected, so everything below
                // classifies against where the file is *now*: the destination's
                // own events are in `rels` too, and they have to find the tab
                // that just moved onto them. The old path falls through to
                // `Class::Tree` — nothing references it any more — which is the
                // tree refresh that drops its row.
                for (old, new) in &renames {
                    h.follow_rename(old, new);
                }
                // The workspace's own override comes from the hub already
                // locked here — no registry lookup and no I/O under the lock.
                let filter = settings.tree_filter_with(h.ws.show_hidden);
                // Tabs, not just buffers: a previewed file has no buffer, and
                // classifying it as a generic tree change is why its pane
                // never refreshed. Buffers are still unioned in because one
                // can outlive its tab for as long as it takes a close to be
                // processed.
                let mut open: Vec<String> = h.ws.open_file_rels();
                open.extend(h.ws.buffers.keys().cloned());
                open.sort();
                open.dedup();
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
                    let class = classify(rel, &open, &filter);
                    // Every class but `Ignore` is a working-tree path, and a
                    // working-tree path is what `git status` reports on — so
                    // all of them refresh the Changes pane, not just git's own
                    // internals. Gating that pane on `.git/index`/`.git/HEAD`
                    // alone meant the ordinary case never reached it: editing
                    // a file writes neither, so the list stayed frozen at
                    // whatever the last commit (or the page load) rendered,
                    // while the full diff beside it — recomputed per request —
                    // showed the change.
                    //
                    // `Ignore` is the right boundary rather than "any path at
                    // all": it is where `target/` and `node_modules/` land, and
                    // a `cargo build` writing thousands of files there would
                    // otherwise put a `git status` subprocess behind every
                    // debounced batch, for paths git itself ignores. The cost
                    // is that a hidden file with `show_hidden` off (a
                    // `.gitignore` edit, say) does not refresh the pane it
                    // would appear in, which the ⟳ button recovers; the
                    // storm does not have a recovery.
                    if !matches!(class, Class::Ignore) {
                        status = true;
                    }
                    match class {
                        Class::Tree => tree = true,
                        // Already handled above, along with everything else
                        // that is not `Ignore` — a commit or a checkout is
                        // still a status change, it is just no longer the
                        // only one.
                        Class::Status => {}
                        Class::Buffer(r) => {
                            buffer_rels.insert(r);
                        }
                        Class::Ignore => {}
                    }
                }
                for r in buffer_rels {
                    // `false` means the file is genuinely gone (a read that
                    // merely refused the *contents* — binary, oversize — still
                    // returns true), and `classify` already routed it away
                    // from `Class::Tree`, so nothing else here would ever
                    // refresh the listing for it — a deletion is a tree change
                    // too.
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
                    "roost: {project_name}: watcher batch panicked, continuing: {}",
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
    use crate::projects::TreeFilter;

    fn bufs() -> Vec<String> {
        vec!["src/main.rs".to_string()]
    }

    #[test]
    fn git_index_and_head_drive_the_status_pane() {
        assert!(matches!(classify(".git/index", &bufs(), &TreeFilter::default()), Class::Status));
        assert!(matches!(classify(".git/HEAD", &bufs(), &TreeFilter::default()), Class::Status));
    }

    #[test]
    fn other_git_internals_are_ignored() {
        assert!(matches!(classify(".git/objects/ab/cdef", &bufs(), &TreeFilter::default()), Class::Ignore));
        assert!(matches!(classify(".git/logs/HEAD", &bufs(), &TreeFilter::default()), Class::Ignore));
    }

    #[test]
    fn open_buffers_beat_the_generic_tree_class() {
        match classify("src/main.rs", &bufs(), &TreeFilter::default()) {
            Class::Buffer(rel) => assert_eq!(rel, "src/main.rs"),
            other => panic!("expected Buffer, got {other:?}"),
        }
    }

    // The classifier and the tree must agree about which rows exist: a change
    // the listing would never show is not a reason to re-render it, and one it
    // does show is. Each case asserts both settings, so a classifier that
    // ignored the filter fails one half.
    #[test]
    fn a_dotfile_is_a_tree_change_only_when_the_tree_shows_it() {
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        for rel in [".gitignore", ".claude/worktrees/feat/src/main.rs", ".venv/lib/p.py"] {
            assert!(
                matches!(classify(rel, &bufs(), &TreeFilter::default()), Class::Ignore),
                "{rel} is not in the default listing, so it cannot change it"
            );
            assert!(
                matches!(classify(rel, &bufs(), &on), Class::Tree),
                "{rel} is a visible row under show_hidden and must refresh"
            );
        }
    }

    // `.git` is the deliberate exception, ahead of the filter: `show_hidden`
    // renders it, but a single git command writes enough inside it to turn
    // every command into a burst of refreshes. Its rows go stale until the
    // directory is re-expanded; the two files the status pane reads still get
    // through, or the pane would stop tracking the branch.
    #[test]
    fn git_internals_stay_quiet_even_with_show_hidden_on() {
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        assert!(matches!(classify(".git/objects/ab/cdef", &bufs(), &on), Class::Ignore));
        assert!(matches!(classify(".git/logs/HEAD", &bufs(), &on), Class::Ignore));
        assert!(matches!(classify(".git/index", &bufs(), &on), Class::Status));
        assert!(matches!(classify(".git/HEAD", &bufs(), &on), Class::Status));
    }

    // Build output is not a hidden file: revealing dot entries must not let a
    // cargo build resume storming the tree.
    #[test]
    fn show_hidden_does_not_reopen_the_build_output_storm() {
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        assert!(matches!(classify("target/debug/roost", &bufs(), &on), Class::Ignore));
        assert!(matches!(classify("node_modules/x/y.js", &bufs(), &on), Class::Ignore));
    }

    #[test]
    fn ordinary_files_refresh_the_tree() {
        assert!(matches!(classify("src/other.rs", &bufs(), &TreeFilter::default()), Class::Tree));
        assert!(matches!(classify("README.md", &bufs(), &TreeFilter::default()), Class::Tree));
    }

    #[test]
    fn skip_dirs_and_hide_are_ignored_entirely() {
        // a cargo build must not generate a storm of tree refreshes
        assert!(matches!(classify("target/debug/roost", &bufs(), &TreeFilter::default()), Class::Ignore));
        assert!(matches!(classify("node_modules/x/y.js", &bufs(), &TreeFilter::default()), Class::Ignore));
        assert!(matches!(classify(".venv/lib/p.py", &bufs(), &TreeFilter::default()), Class::Ignore));
        let hide = vec!["dist".to_string()];
        assert!(matches!(classify("dist/bundle.js", &bufs(), &TreeFilter { hide: &hide, ..Default::default() }), Class::Ignore));
    }

    // A Claude worktree at `.claude/worktrees/{name}` is a whole second
    // checkout inside the project. Every file written in it would otherwise
    // refresh the tree of the *parent* project, which is displaying an
    // entirely different working copy — unless `show_hidden` is on, in which
    // case the parent's tree really is displaying those rows and has to keep
    // them current.
    #[test]
    fn a_worktree_under_dot_claude_never_refreshes_its_parents_tree() {
        assert!(matches!(
            classify(".claude/worktrees/feat/src/main.rs", &bufs(), &TreeFilter::default()),
            Class::Ignore
        ));
        // The whole directory goes, not just `worktrees/` — so `.claude`'s own
        // config files stop live-refreshing too. Accepted: they are Claude's
        // state, not the project's source, and one skip list drives the tree,
        // the watcher, and the picker alike.
        assert!(matches!(classify(".claude/settings.json", &bufs(), &TreeFilter::default()), Class::Ignore));
    }

    #[test]
    fn self_writes_are_suppressed_once() {
        let mut seen = std::collections::HashMap::new();
        seen.insert("a.rs".to_string(), 42u64);
        // roost just wrote this content; the resulting event is ours
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
        std::env::set_var("ROOST_STATE_DIR", sd.path());

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

        std::env::remove_var("ROOST_STATE_DIR");
    }

    // The watcher reads the project's settings itself, per batch of events —
    // nothing hands it a filter at spawn time. Without that, `show_hidden`
    // would render dot rows that then never refreshed, and the tree would show
    // a `.gitignore` frozen at whatever it said when the pane loaded.
    //
    // Both halves run against a live watcher, and each one waits for a
    // *positive* event before drawing its conclusion: the "must not refresh"
    // case ends by touching an ordinary file and requiring the refresh that
    // proves the watcher was alive and listening the whole time. A test that
    // only waited out a timeout would pass just as well with the watcher dead.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_watcher_reads_show_hidden_from_the_project_it_is_watching() {
        assert!(
            !dotfile_refreshes_the_tree(None, None),
            "with the default settings a dotfile is not a visible row, so it must not refresh"
        );
        assert!(
            dotfile_refreshes_the_tree(Some("show_hidden = true"), None),
            "with show_hidden on the dotfile IS a visible row and must refresh"
        );
    }

    // And the header toggle, which is the value the *tree* renders against
    // once someone has used it. Without the watcher reading the same source,
    // turning dotfiles on from the UI would show rows that then froze — the
    // failure would look like a broken watcher, not a missed override.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_watcher_follows_the_header_toggle_over_the_config_file() {
        assert!(
            dotfile_refreshes_the_tree(None, Some(true)),
            "toggled on against a silent config: the row is visible, so it must refresh"
        );
        assert!(
            !dotfile_refreshes_the_tree(Some("show_hidden = true"), Some(false)),
            "toggled off against show_hidden = true: the row is gone, so it must not"
        );
    }

    /// Spawns a real watcher over a fresh project containing `config`, with
    /// `toggle` as the workspace's header setting, writes `.gitignore`, and
    /// reports whether a TreeChanged followed. Panics rather than returning
    /// false if the control write (an ordinary file, always a visible row)
    /// fails to produce one — that means the harness itself was not working,
    /// which is not the same answer as "filtered out".
    #[cfg(target_os = "linux")]
    fn dotfile_refreshes_the_tree(config: Option<&str>, toggle: Option<bool>) -> bool {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("seed.txt"), "").unwrap();
        if let Some(text) = config {
            std::fs::create_dir(d.path().join(".roost")).unwrap();
            std::fs::write(d.path().join(".roost/config.toml"), text).unwrap();
        }

        let hub = Arc::new(Mutex::new(Hub::new("watch_show_hidden", d.path().to_path_buf())));
        Hub::lock(&hub).ws.show_hidden = toggle;
        assert!(spawn("watch_show_hidden", d.path().to_path_buf(), hub.clone(), Duration::from_millis(20)));
        let rx = Hub::lock(&hub).subscribe().1;

        std::fs::write(d.path().join(".gitignore"), "target\n").unwrap();
        let saw_dotfile = waits_for_tree_changed(&rx);
        // Control: an ordinary write must always be seen, whatever the
        // settings say. If this fails the watcher was not delivering at all
        // and `saw_dotfile` means nothing.
        std::fs::write(d.path().join("ordinary.txt"), "hi\n").unwrap();
        assert!(
            waits_for_tree_changed(&rx),
            "the watcher must deliver an ordinary file's change; without that the \
             dotfile result is meaningless"
        );

        std::env::remove_var("ROOST_STATE_DIR");
        saw_dotfile
    }

    /// The whole chain, through a real watcher and a real `mv`: kernel event →
    /// `Modify(Name(Both))` → `Hub::follow_rename` → the tab addresses the new
    /// path. Every piece of it is covered by a unit test somewhere; none of
    /// those would notice if the batch loop stopped looking at rename events
    /// at all, which is the failure this one exists for.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_external_rename_moves_the_tab_through_a_live_watcher() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("old.rs"), "fn one() {}\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("watch_rename", d.path().to_path_buf())));
        let rx = {
            let mut h = Hub::lock(&hub);
            let (c, rx) = h.subscribe();
            h.handle(
                &c,
                crate::proto::Intent::OpenTab {
                    pane: crate::proto::MIDDLE,
                    tab: crate::proto::Tab::File {
                        rel: "old.rs".into(),
                        mode: crate::proto::Mode::Edit,
                    },
                },
            );
            rx
        };
        assert!(spawn("watch_rename", d.path().to_path_buf(), hub.clone(), Duration::from_millis(20)));

        std::fs::rename(d.path().join("old.rs"), d.path().join("new.rs")).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let moved = loop {
            if std::time::Instant::now() > deadline {
                break false;
            }
            let rel = Hub::lock(&hub).ws.panes[crate::proto::MIDDLE as usize]
                .tabs
                .iter()
                .find_map(|t| match t {
                    crate::proto::Tab::File { rel, .. } => Some(rel.clone()),
                    _ => None,
                });
            if rel.as_deref() == Some("new.rs") {
                break true;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(moved, "the tab must follow the file to where it was moved");

        // The browser is told, not just the server's own copy: everything the
        // user sees comes off this channel, and a rekey nobody hears about is
        // a tab that stays wrong on screen until a reload.
        let mut saw_state = false;
        while let Ok(m) = rx.try_recv() {
            if m.contains(r#""t":"State""#) && m.contains("new.rs") {
                saw_state = true;
            }
        }
        assert!(saw_state, "a State naming the new rel has to reach the browser");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// True if a TreeChanged arrives within the window. The window only has to
    /// outrun the 20ms debounce; it is generous because a slow CI box delaying
    /// an event must not read as "the event was filtered".
    #[cfg(target_os = "linux")]
    fn waits_for_tree_changed(rx: &std::sync::mpsc::Receiver<String>) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while let Ok(m) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            if m.contains(r#""t":"TreeChanged""#) {
                return true;
            }
        }
        false
    }

    // The Changes pane renders `git status`, and the only thing that used to
    // refresh it was a write to `.git/index` or `.git/HEAD` — git's own
    // internals, which an ordinary edit never touches. A file modified after
    // the last git command was therefore absent from the list while showing
    // up in the full diff beside it, which is recomputed per request. That is
    // how it was reported: README.md edited, listed by `git diff HEAD`, and
    // missing from the pane whose whole job is to list it.
    //
    // Two cases, because they take different branches of the classifier and
    // one of them refreshed nothing at all: an ordinary file (`Class::Tree`,
    // which at least re-rendered the tree) and a file open in a buffer
    // (`Class::Buffer`, which broadcasts only the file's own text) — the
    // reported case, since README.md was open in a tab.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_working_tree_write_refreshes_the_status_pane() {
        assert!(
            writing_refreshes_the_status_pane("plain.txt", &[]),
            "an edit to an ordinary file changes what `git status` reports"
        );
        assert!(
            writing_refreshes_the_status_pane("open.txt", &["open.txt"]),
            "and so does one to a file that happens to be open in a buffer"
        );
    }

    /// Writes `rel` under a live watcher, with `buffers` registered as open
    /// buffers, and reports whether a `StatusChanged` followed. The `saw_any`
    /// control is what makes a `false` mean something: the buffer case
    /// broadcasts `BufferText`/`FileChanged` and the tree case `TreeChanged`,
    /// so a run that saw *nothing* is a dead watcher, not a classifier that
    /// declined to refresh the pane.
    #[cfg(target_os = "linux")]
    fn writing_refreshes_the_status_pane(rel: &str, buffers: &[&str]) -> bool {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join(rel), "before\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("watch_status", d.path().to_path_buf())));
        for b in buffers {
            Hub::lock(&hub).ws.buffers.insert((*b).to_string(), crate::workspace::Buffer::default());
        }
        assert!(spawn("watch_status", d.path().to_path_buf(), hub.clone(), Duration::from_millis(20)));
        let rx = Hub::lock(&hub).subscribe().1;

        std::fs::write(d.path().join(rel), "after\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let (mut saw_status, mut saw_any) = (false, false);
        while let Ok(m) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            saw_any = true;
            if m.contains(r#""t":"StatusChanged""#) {
                saw_status = true;
                break;
            }
        }
        assert!(saw_any, "the watcher delivered nothing at all for {rel}; the result below would mean nothing");

        std::env::remove_var("ROOST_STATE_DIR");
        saw_status
    }

    // The storm, as a test. Rendering the tree is a `read_dir` of the watched
    // directory, and inotify reports reads — so before `is_access`, one tree
    // render started a loop that re-registered the watch, which re-walked the
    // directory, which read it again, ~3 times a second for as long as the
    // project stayed open.
    //
    // Both halves matter and neither works alone. The first read must produce
    // nothing, and the *quiet must hold*: the defect was self-sustaining, so a
    // test that only checked the first second would pass against a loop that
    // had merely not started yet. The control write at the end is what makes
    // the silence mean something — without it this passes just as well with
    // the watcher dead.
    #[cfg(target_os = "linux")]
    #[test]
    fn reading_a_watched_directory_is_not_a_change() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), "").unwrap();
        std::fs::write(d.path().join("seed.txt"), "").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("watch_no_access", d.path().to_path_buf())));
        assert!(spawn("watch_no_access", d.path().to_path_buf(), hub.clone(), Duration::from_millis(20)));
        let rx = Hub::lock(&hub).subscribe().1;

        // Exactly what serving a tree fragment does, subdirectory included.
        for dir in [d.path().to_path_buf(), d.path().join("src")] {
            for e in std::fs::read_dir(&dir).unwrap() {
                let _ = e.unwrap().file_name();
            }
        }

        let quiet_for = |secs: u64| {
            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            let mut seen = Vec::new();
            while let Ok(m) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                seen.push(m);
            }
            seen
        };
        let first = quiet_for(1);
        assert!(first.is_empty(), "a directory read must broadcast nothing; got {first:?}");
        // The loop ran at ~3/s, so a second of silence here would have held
        // dozens of events if it were still alive.
        let later = quiet_for(2);
        assert!(later.is_empty(), "and the quiet must hold, not merely start; got {later:?}");

        std::fs::write(d.path().join("real.txt"), "hi\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw_write = false;
        while let Ok(m) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            if m.contains(r#""t":"TreeChanged""#) {
                saw_write = true;
                break;
            }
        }
        assert!(saw_write, "the watcher must still report a real write; silence above meant nothing otherwise");

        std::env::remove_var("ROOST_STATE_DIR");
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

    /// Polls a hub subscriber for a broadcast message containing `needle`
    /// until it arrives or `deadline` passes, checking the clock instead of
    /// sleeping a guessed-at duration: OS watch latency plus the watcher's
    /// own debounce window make any fixed sleep either flaky (too short) or
    /// slow (padded well past what's needed) on a shared CI box.
    fn wait_for(rx: &std::sync::mpsc::Receiver<String>, needle: &str, deadline: std::time::Instant) -> bool {
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match rx.recv_timeout(remaining) {
                Ok(msg) if msg.contains(needle) => return true,
                Ok(_) => continue, // some other broadcast; keep waiting for `needle`
                Err(_) => return false, // deadline elapsed inside recv_timeout
            }
        }
    }

    // Regression test for the bug this whole change fixes: on macOS,
    // `notify-debouncer-full` 0.7's rename-correlation queue silently drops
    // `Remove` events under FSEvents' coalescing, so a file deleted from a
    // watched project never produced a `TreeChanged` broadcast — the tree
    // pane just kept showing a file that was gone. Creates and modifies
    // went through the debouncer fine, which is exactly why this class of
    // bug survived: nothing exercised a delete through a real watcher.
    // Drives `spawn` end to end against a real temp directory and a real
    // OS watch, not just `classify`, since the bug lived in the debouncing
    // layer that `classify` never sees.
    #[test]
    fn deleted_files_reach_the_ui_same_as_created_ones() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sd = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", sd.path());

        let d = tempfile::tempdir().unwrap();
        let hub = Arc::new(Mutex::new(Hub::new("watch-delete-regression", d.path().to_path_buf())));
        let rx = Hub::lock(&hub).subscribe().1;

        let ok = spawn(
            "watch-delete-regression",
            d.path().to_path_buf(),
            hub.clone(),
            Duration::from_millis(50),
        );
        assert!(ok, "watcher must start on a plain temp dir");

        let file = d.path().join("a.txt");
        std::fs::write(&file, "hi").unwrap();
        assert!(
            wait_for(&rx, "TreeChanged", std::time::Instant::now() + std::time::Duration::from_secs(10)),
            "a created file must be observed"
        );

        std::fs::remove_file(&file).unwrap();
        assert!(
            wait_for(&rx, "TreeChanged", std::time::Instant::now() + std::time::Duration::from_secs(10)),
            "a deleted file must be observed too — this is the debouncer's dropped Remove event"
        );

        std::env::remove_var("ROOST_STATE_DIR");
    }
}
