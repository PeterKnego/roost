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

/// Register per-directory, non-recursive watches while walking the tree,
/// skipping SKIP_DIRS. Non-recursive and per-directory (not one recursive
/// watch on the root) because a recursive watch on a Rust project turns
/// every `cargo build` into thousands of events from `target/` — except we
/// skip `target/` entirely anyway, but the same trap applies to any other
/// large generated directory a project happens to have.
///
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
    let mut ok = true;
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        if let Err(e) = debouncer.watch(&d, RecursiveMode::NonRecursive) {
            eprintln!("deadlight: {project}: failed to watch {}: {e}", d.display());
            ok = false;
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() && !crate::projects::SKIP_DIRS.contains(&name.as_str()) {
                stack.push(p);
            }
        }
    }
    // .git itself is skipped for the tree walk above, but index/HEAD drive
    // the status pane, so watch that one directory deliberately.
    let _ = debouncer.watch(&dir.join(".git"), RecursiveMode::NonRecursive);

    let base = dir.clone();
    std::thread::spawn(move || {
        let _keep = debouncer; // dropping the debouncer stops the watch
        for res in rx {
            let Ok(events) = res else { continue };
            // Lock once per debounced batch, not once per event: the batch
            // is already coalesced by the debouncer, and re-locking per
            // event would mean more lock/unlock churn for no benefit —
            // the lock is never held across I/O either way, since
            // `file_changed_externally`'s read is a filesystem read, not a
            // network or PTY write.
            let mut h = Hub::lock(&hub);
            let open: Vec<String> = h.ws.buffers.keys().cloned().collect();
            let mut tree = false;
            let mut status = false;
            // A single save (temp file + rename) fires several raw events
            // (Create, Modify(Name), Modify(Data)) for the same path within
            // one debounced batch. Collect the distinct buffer paths first
            // and visit each once: `is_self_write` consumes its match, so
            // calling `file_changed_externally` per raw event would let the
            // first occurrence eat the suppression token and the second
            // occurrence in the same batch would read as a "real" external
            // edit — echoing the save straight back at the author, exactly
            // what self-write suppression exists to prevent.
            let mut buffer_rels = std::collections::HashSet::new();
            for ev in &events {
                for path in &ev.paths {
                    let Ok(rel) = path.strip_prefix(&base) else { continue };
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    match classify(&rel, &open, &[]) {
                        Class::Tree => tree = true,
                        Class::Status => status = true,
                        Class::Buffer(r) => {
                            buffer_rels.insert(r);
                        }
                        Class::Ignore => {}
                    }
                }
            }
            for r in buffer_rels {
                h.file_changed_externally(&base, &r);
            }
            if tree {
                h.broadcast(&crate::proto::Event::TreeChanged);
            }
            if status {
                h.broadcast(&crate::proto::Event::StatusChanged);
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
}
