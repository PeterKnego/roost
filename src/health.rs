//! A periodic look at the invariants nothing else watches, which **only ever
//! logs**.
//!
//! It never repairs anything, and that restriction is the whole design. Every
//! defect in this codebase's "absence of evidence" table is a check that
//! failed, concluded the thing was gone, and destroyed it — and those ran on
//! paths a person triggered and watched. Putting that judgement on a timer,
//! unattended, is the same mistake with a wider blast radius. A reporter has
//! no such failure mode: at worst it is wrong out loud.
//!
//! For the same reason it calls nothing that reaps. `registry::known_projects`
//! looks like the obvious way to enumerate projects and is exactly what this
//! must not use: it runs `reconcile`, which kills sessions. Everything here
//! touches the filesystem and loopback sockets and nothing else — no hub lock,
//! no session registry, no subprocess.
//!
//! It reports on *change*, not every pass. `registry.rs` already learned this
//! one: a permanently broken check "would otherwise spam the journal forever
//! with the identical line".
use std::collections::BTreeSet;
use std::path::Path;

/// Everything wrong right now, in a stable order so two passes compare.
pub fn check_in(ide_dir: &Path, proc_root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(ide_dir) else {
        // Not a finding: a missing directory is the normal state before any
        // project has opened, and an unreadable one is something we cannot
        // tell about — neither is a fault to announce every five minutes.
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Some(port) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".lock"))
            .and_then(|n| n.parse::<u16>().ok())
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            // Someone else's malformed file is not ours to grade.
            continue;
        };
        if v.get("ideName").and_then(|x| x.as_str()) != Some("resh") {
            continue;
        }
        let Some(pid) = v.get("pid").and_then(|x| x.as_u64()).and_then(|p| u32::try_from(p).ok())
        else {
            continue;
        };
        let alive = match std::fs::symlink_metadata(proc_root.join(pid.to_string())) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(proc_root) {
                    Ok(_) => Some(false),
                    // No /proc at all: we cannot tell, so we say nothing.
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };
        let answers = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(200),
        )
        .is_ok();
        match (alive, answers) {
            // Ours, gone, and nothing on the port: debris a restart should
            // have swept. Worth saying, because it means the sweep did not run
            // or did not match.
            (Some(false), false) => {
                out.insert(format!("stale ide lock {port}.lock — pid {pid} is gone and nothing answers that port"));
            }
            // We are alive and still advertising a port we no longer serve:
            // `claude` will find this lock, connect, and fail. Silent
            // otherwise — nothing else notices a listener that died.
            (Some(true), false) => {
                out.insert(format!("ide lock {port}.lock advertises a port this resh no longer serves"));
            }
            _ => {}
        }
    }
    out
}

/// Reports only what changed, so a standing fault is said once rather than
/// every pass.
#[derive(Default)]
pub struct Latch {
    seen: BTreeSet<String>,
}

impl Latch {
    /// The findings worth printing now: new ones, plus a note when a fault
    /// clears — a problem that goes away silently is a problem you never
    /// learn was transient.
    pub fn step(&mut self, now: BTreeSet<String>) -> Vec<String> {
        let mut lines: Vec<String> =
            now.difference(&self.seen).map(|s| format!("resh: health — {s}")).collect();
        lines.extend(
            self.seen.difference(&now).map(|s| format!("resh: health — cleared: {s}")),
        );
        self.seen = now;
        lines
    }
}

/// Interval between passes. Long by default: this watches for drift, not for
/// events, and a short period buys nothing but journal noise.
pub fn interval() -> std::time::Duration {
    let secs = std::env::var("RESH_HEALTH_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s >= 10)
        .unwrap_or(300);
    std::time::Duration::from_secs(secs)
}

/// Starts the periodic pass. Nothing here can wedge the server: it owns no
/// locks, spawns no processes, and a panic would take only this thread — so
/// it is written not to have one.
pub fn spawn() {
    let every = interval();
    std::thread::spawn(move || {
        let mut latch = Latch::default();
        loop {
            std::thread::sleep(every);
            let findings = check_in(&crate::idelock::ide_dir(), Path::new("/proc"));
            for line in latch.step(findings) {
                eprintln!("{line}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with(alive: &[u32]) -> tempfile::TempDir {
        let p = tempfile::tempdir().unwrap();
        for pid in alive {
            std::fs::create_dir(p.path().join(pid.to_string())).unwrap();
        }
        p
    }

    fn lock(dir: &Path, port: u16, ide: &str, pid: u32) {
        std::fs::write(
            dir.join(format!("{port}.lock")),
            serde_json::json!({"pid": pid, "workspaceFolders": ["/w"], "ideName": ide,
                               "transport": "ws", "authToken": "x"})
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_stale_lock_of_ours_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let p = proc_with(&[]);
        lock(d.path(), 5601, "resh", 4242);
        let f = check_in(d.path(), p.path());
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f.iter().next().unwrap().contains("stale ide lock 5601"));
    }

    #[test]
    fn a_live_lock_whose_port_answers_is_not_reported() {
        // The healthy case must be silent, or every pass logs and the whole
        // thing becomes noise nobody reads.
        let d = tempfile::tempdir().unwrap();
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id();
        let p = proc_with(&[me]);
        lock(d.path(), port, "resh", me);
        assert!(check_in(d.path(), p.path()).is_empty());
    }

    #[test]
    fn a_live_resh_advertising_a_dead_port_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let p = proc_with(&[me]);
        lock(d.path(), 5602, "resh", me);
        let f = check_in(d.path(), p.path());
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f.iter().next().unwrap().contains("no longer serves"));
    }

    #[test]
    fn another_ides_stale_lock_is_not_our_business() {
        // Reporting on a foreign IDE's directory entries would be both wrong
        // and the first step towards acting on them.
        let d = tempfile::tempdir().unwrap();
        let p = proc_with(&[]);
        lock(d.path(), 5603, "IntelliJ IDEA", 4242);
        assert!(check_in(d.path(), p.path()).is_empty());
    }

    #[test]
    fn an_unreadable_proc_produces_no_finding() {
        // "I could not tell" is not a fault to report, and reporting it every
        // pass on a host without /proc would be pure noise.
        let d = tempfile::tempdir().unwrap();
        lock(d.path(), 5604, "resh", 4242);
        assert!(check_in(d.path(), &d.path().join("no-proc")).is_empty());
    }

    #[test]
    fn a_standing_fault_is_said_once_and_its_clearing_is_said_too() {
        // The `registry.rs` lesson: without this the journal gets the same
        // line every pass forever. Reverting `step` to return `now` wholesale
        // fails this on the second call.
        let mut l = Latch::default();
        let one: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(l.step(one.clone()).len(), 1, "first sighting is reported");
        assert!(l.step(one.clone()).is_empty(), "the same fault must not repeat");
        let cleared = l.step(BTreeSet::new());
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0].contains("cleared: a"), "{cleared:?}");
        assert!(l.step(BTreeSet::new()).is_empty(), "a clear is announced once too");
    }

    #[test]
    fn the_interval_refuses_a_useless_value() {
        // A one-second health pass would connect to every advertised port
        // every second. The floor is not a style choice.
        std::env::set_var("RESH_HEALTH_SECS", "1");
        assert_eq!(interval(), std::time::Duration::from_secs(300));
        std::env::set_var("RESH_HEALTH_SECS", "30");
        assert_eq!(interval(), std::time::Duration::from_secs(30));
        std::env::remove_var("RESH_HEALTH_SECS");
    }
}
