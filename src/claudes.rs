//! What resh can say about Claudes running in a project, from what resh
//! itself observed — never from Claude's own session files.
//!
//! Two signals: a terminal resh typed `claude` into (`session::launched_names`)
//! and a connection on the project's IDE socket (`ide::connected_sessions`).
//! Three answers, not two: with the IDE integration switched off, a `claude`
//! typed by hand into a plain terminal is invisible, so "found nothing" is
//! not "nothing there". Only `Present` may change what a button does.

use crate::idesess::Sess;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeEvidence {
    /// Terminal names resh could attribute, sorted, deduplicated. May be
    /// empty when the only evidence is a connection it could not place.
    Present(Vec<String>),
    Absent,
    Unknown,
}

pub fn evidence_from(launched: &[String], connected: &[Sess], ide_on: bool) -> ClaudeEvidence {
    let mut names: Vec<String> = launched.to_vec();
    let mut any = !launched.is_empty();
    for s in connected {
        match s {
            Sess::In(n) => { names.push(n.clone()); any = true; }
            Sess::Unknown => any = true,
            // Positively in another project's terminal: not evidence here.
            Sess::Outside => {}
        }
    }
    if any {
        names.sort();
        names.dedup();
        return ClaudeEvidence::Present(names);
    }
    if ide_on { ClaudeEvidence::Absent } else { ClaudeEvidence::Unknown }
}

/// Terminals of `project` that a running `claude` process sits in, read from
/// the process table. `session_env` exports `RESH_PROJECT`/`RESH_SESSION`
/// into every resh shell and a `claude` started there inherits them, so a
/// `claude` process's environment names its terminal — `idesess.rs` reads
/// exactly this for one pid; this walks every pid whose `comm` is `claude`.
///
/// This is the restart-proof signal. The launch record and the IDE
/// connection map are in-process memory: a resh restart (every deploy)
/// empties both, and the overview then showed every Claude as a plain
/// shell until each one was restarted. A process that exists is evidence
/// regardless of who remembers starting it.
///
/// `proc_root` is injectable so a fake `/proc` can drive the test; an
/// unreadable entry is skipped, never treated as "no Claude here".
pub fn claudes_in_proc(proc_root: &std::path::Path, project: &str) -> Vec<String> {
    names_for(project, &claude_terminals(proc_root))
}

/// Every `(project, terminal)` a running `claude` sits in, from ONE walk of
/// `proc_root`. The overview polls every few seconds and asks about every
/// project, so the walk is done once per request and shared — the same
/// hoisting `ages_snapshot`/`holders_snapshot` do for `ps` — rather than
/// once per project. An entry whose environment cannot be read is skipped,
/// never counted as "no Claude".
pub fn claude_terminals(proc_root: &std::path::Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(proc_root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(_pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
        if comm.trim() != "claude" {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("environ")) else { continue };
        let (mut proj, mut sess) = (None, None);
        for entry in raw.split(|b| *b == 0) {
            let Ok(kv) = std::str::from_utf8(entry) else { continue };
            if let Some(v) = kv.strip_prefix("RESH_PROJECT=") { proj = Some(v.to_string()); }
            else if let Some(v) = kv.strip_prefix("RESH_SESSION=") { sess = Some(v.to_string()); }
        }
        if let (Some(p), Some(s)) = (proj, sess) {
            if crate::session::valid_name(&s) {
                out.push((p, s));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn names_for(project: &str, scan: &[(String, String)]) -> Vec<String> {
    scan.iter().filter(|(p, _)| p == project).map(|(_, s)| s.clone()).collect()
}

/// One project's evidence, walking `/proc` itself. For a single question —
/// a ✻ click — this is the right call; a loop over projects should scan
/// once with `claude_terminals` and use `claude_evidence_with_scan`.
pub fn claude_evidence(project: &str) -> ClaudeEvidence {
    claude_evidence_with_scan(project, &claude_terminals(std::path::Path::new("/proc")))
}

/// `claude_evidence` over an already-taken process scan.
pub fn claude_evidence_with_scan(project: &str, scan: &[(String, String)]) -> ClaudeEvidence {
    let mut launched: Vec<String> =
        crate::session::launched_names(project).into_iter().map(|(n, _)| n).collect();
    launched.extend(names_for(project, scan));
    evidence_from(&launched, &crate::ide::connected_sessions(project), crate::config::ide_enabled())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idesess::Sess;

    #[test]
    fn an_ide_connection_alone_is_present_and_names_its_terminal() {
        // Revert-checked: not including Sess::In names fails here — test panicked with `assertion 'left == right' failed: left: Present([]), right: Present(["term2"])`.
        assert_eq!(evidence_from(&[], &[Sess::In("term2".into())], true), ClaudeEvidence::Present(vec!["term2".into()]));
    }

    #[test]
    fn a_launched_terminal_alone_is_present_even_with_ide_off() {
        // Revert-checked: returning `Unknown` whenever `!ide_on` fails here — test panicked with `assertion 'left == right' failed: left: Unknown, right: Present(["term"])`.
        assert_eq!(evidence_from(&["term".into()], &[], false), ClaudeEvidence::Present(vec!["term".into()]));
    }

    #[test]
    fn a_connection_resh_cannot_place_is_still_present_but_unnamed() {
        // Revert-checked: not treating Sess::Unknown as evidence fails here — test panicked with `assertion 'left == right' failed: left: Absent, right: Present([])`.
        assert_eq!(evidence_from(&[], &[Sess::Unknown], true), ClaudeEvidence::Present(vec![]));
    }

    #[test]
    fn nothing_with_ide_on_is_absent() {
        // Asserted on the variant: `!= Present` would also pass for Unknown.
        // Revert-checked: always returning Unknown fails here — test panicked with `assertion 'left == right' failed: left: Unknown, right: Absent`.
        assert_eq!(evidence_from(&[], &[], true), ClaudeEvidence::Absent);
    }

    #[test]
    fn nothing_with_ide_off_is_unknown() {
        // Revert-checked: dropping the `ide_on` branch yields Absent here — test panicked with `assertion 'left == right' failed: left: Absent, right: Unknown`.
        assert_eq!(evidence_from(&[], &[], false), ClaudeEvidence::Unknown);
    }

    #[test]
    fn a_terminal_seen_both_ways_is_named_once() {
        // Revert-checked: skipping dedup fails here — test panicked with `assertion 'left == right' failed: left: Present(["term", "term"]), right: Present(["term"])`.
        assert_eq!(
            evidence_from(&["term".into()], &[Sess::In("term".into()), Sess::Outside], true),
            ClaudeEvidence::Present(vec!["term".into()])
        );
    }

    /// A `claude` in this project's terminal is evidence even when resh
    /// remembers launching nothing (it just restarted). Built on a fake
    /// `/proc`: pid 100 is a claude in term3 of this project, pid 200 a
    /// claude in another project, pid 300 a bash in this project.
    /// Revert-checked: with the scan's result replaced by `Vec::new()` the
    /// first assertion failed with `left: [] right: ["term3"]`.
    #[test]
    fn a_running_claude_process_names_its_terminal() {
        let d = tempfile::tempdir().unwrap();
        let mk = |pid: u32, comm: &str, env: &str| {
            let p = d.path().join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
        };
        mk(100, "claude", "RESH_PROJECT=karpie\nRESH_SESSION=term3\n");
        mk(200, "claude", "RESH_PROJECT=other\nRESH_SESSION=term\n");
        mk(300, "bash", "RESH_PROJECT=karpie\nRESH_SESSION=term1\n");
        std::fs::write(d.path().join("self"), b"").unwrap(); // a non-pid entry, skipped
        assert_eq!(claudes_in_proc(d.path(), "karpie"), vec!["term3".to_string()]);
        assert!(claudes_in_proc(d.path(), "nowhere").is_empty());
        // The one-walk scan every loop shares: both claudes, neither the bash.
        assert_eq!(
            claude_terminals(d.path()),
            vec![("karpie".to_string(), "term3".to_string()), ("other".to_string(), "term".to_string())]
        );
    }
}
