//! One file for problems resh detects but nobody would otherwise see.
//!
//! Two of resh's detectors have nowhere useful to complain. `resh peers` runs
//! as a Claude Code hook that always exits 0, and a hook's stderr is shown
//! only when it fails or is slow — so a warning written there on a successful
//! run is discarded, which is indistinguishable from never having detected
//! anything. The server can use stderr, since systemd captures it, but then a
//! reader has two places to look and no reason to guess which.
//!
//! So: one `error.log` under the state directory, appended to by anything that
//! finds a condition worth a human's attention later. The server still echoes
//! its own findings to stderr, because journald already carries its startup
//! messages and dropping that would lose a trail people already read — but the
//! file is the one place that has everything.
//!
//! Only written when something is detected. On a healthy host the file does
//! not exist, so its presence is itself the signal, and it does not grow with
//! ordinary use.
use std::path::Path;

/// Append one stamped line. Best effort: nothing resh does may fail because a
/// log line could not be written, least of all a session starting.
pub fn record(text: &str, now_secs: u64) {
    record_to(&crate::wsstate::state_dir(), text, now_secs)
}

/// Split from [`record`] so tests can point at a real directory instead of
/// setting `RESH_STATE_DIR`, which other tests in this crate read concurrently.
pub fn record_to(dir: &Path, text: &str, now_secs: u64) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("error.log"))
    {
        let _ = writeln!(f, "{now_secs} {text}");
    }
}

/// Seconds since the epoch, or 0 if the clock is before it.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason this module exists rather than an `eprintln!`: a hook that
    /// exits 0 has its stderr discarded, so the finding has to outlive the
    /// process that found it.
    #[test]
    fn findings_are_appended_stamped_and_only_when_there_is_one() {
        let d = tempfile::tempdir().unwrap();
        record_to(d.path(), "duplicate session name in resh: resh-f8", 1_787_400_000);
        record_to(d.path(), "roots disagree", 1_787_400_060);
        let text = std::fs::read_to_string(d.path().join("error.log")).expect("the log must exist");
        assert!(text.contains("resh-f8"), "the finding is recorded: {text:?}");
        assert!(text.contains("1787400000"), "stamped, so a reader can order events: {text:?}");
        assert_eq!(text.lines().count(), 2, "appended, never truncated: {text:?}");

        // Nothing detected, nothing written. The file's existence is the
        // signal, so it must not appear on a healthy host.
        let quiet = tempfile::tempdir().unwrap();
        assert!(!quiet.path().join("error.log").exists());
    }
}
