//! Which roost terminal a process is running in, from its pid.
//!
//! `session_env` (`session.rs`) exports `ROOST_PROJECT` and `ROOST_SESSION`
//! into every shell roost spawns, originally so a program in that terminal
//! could attribute a `ROOST_NOTIFY` notification to its session. A `claude`
//! started in that terminal inherits both, through dtach and through the
//! shell — which makes the same two variables the answer to the opposite
//! question: given a connected Claude's pid, which of this project's
//! terminals is it sitting in?
//!
//! That question has no answer in the IDE protocol. `ide_connected` carries
//! a pid and nothing else, so roost asks the kernel — the same move, for the
//! same reason, that `idecwd.rs` makes for the working directory.
//!
//! Three outcomes, not two. "I could not read this process's environment" is
//! not "this process is not in a roost terminal". Only the second is evidence,
//! and only the second may exclude a connection from a mention: a mention
//! that reaches one Claude too many is recoverable, one that reaches none
//! looks like a broken keystroke.
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sess {
    /// `ROOST_SESSION` read, the name is valid, and `ROOST_PROJECT` names the
    /// project being asked about.
    In(String),
    /// The environment read cleanly and positively places this process
    /// outside this project's terminals — either no roost variables at all,
    /// or a different project's. Evidence, so it may exclude.
    Outside,
    /// roost could not tell. Never a reason to exclude a connection.
    Unknown,
}

pub fn session_of_in(proc_root: &Path, pid: u32, project: &str) -> Sess {
    // Read the whole thing: environ is a few KB and there is no way to seek
    // to a variable. A read error is the "cannot tell" case and is the only
    // failure this function can have — the parse below cannot fail.
    let raw = match std::fs::read(proc_root.join(pid.to_string()).join("environ")) {
        Ok(b) => b,
        Err(_) => return Sess::Unknown,
    };
    let mut session: Option<&str> = None;
    let mut proj: Option<&str> = None;
    // NUL-separated, and a *value* may contain '=' — so this splits on the
    // first '=' via strip_prefix on the full key, never on every '='.
    for entry in raw.split(|b| *b == 0) {
        let Ok(s) = std::str::from_utf8(entry) else { continue };
        if let Some(v) = s.strip_prefix("ROOST_SESSION=") {
            session = Some(v);
        } else if let Some(v) = s.strip_prefix("ROOST_PROJECT=") {
            proj = Some(v);
        }
    }
    match (session, proj) {
        // Both present and this is the project: the only case that can name
        // a terminal. An unusable name is "cannot tell", not "not here" —
        // Outside would exclude the connection, which is the wrong direction
        // for a value roost failed to make sense of.
        (Some(s), Some(p)) if p == project => {
            if crate::session::valid_name(s) { Sess::In(s.to_string()) } else { Sess::Unknown }
        }
        // A clean environment with neither variable: roost did not spawn this
        // process. Positive evidence.
        (None, None) => Sess::Outside,
        // In this project, but the session name is gone. "Cannot tell which
        // terminal" — not "in no terminal": the symmetric (Some, None) case
        // below is Unknown for the same reason, and only positive evidence
        // may exclude a connection from a mention.
        (None, Some(p)) if p == project => Sess::Unknown,
        // `ROOST_PROJECT` is set and names a different project — with or
        // without a session name, since the arms above already claimed
        // every same-project case (named session, or the guard just above).
        // Positive evidence: this pid is accounted for by another project's
        // terminal, not "cannot tell".
        (_, Some(_)) => Sess::Outside,
        // One variable without the other. Something scrubbed the
        // environment partially; the name cannot be trusted to mean this
        // project, and roost cannot tell what it does mean.
        (Some(_), None) => Sess::Unknown,
    }
}

pub fn session_of(pid: u32, project: &str) -> Sess {
    session_of_in(Path::new("/proc"), pid, project)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a fake `/proc` whose pid 4242 has the given NUL-separated
    /// environment. Returns the TempDir so the caller keeps it alive — a
    /// dropped TempDir removes the fixture out from under the test.
    fn fake_proc(vars: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let pdir = d.path().join("4242");
        std::fs::create_dir(&pdir).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        for v in vars {
            buf.extend_from_slice(v.as_bytes());
            buf.push(0);
        }
        std::fs::write(pdir.join("environ"), &buf).unwrap();
        d
    }

    #[test]
    fn a_claude_in_a_roost_terminal_reports_its_session() {
        let d = fake_proc(&["PATH=/usr/bin", "ROOST_PROJECT=karpie", "ROOST_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::In("main".into()));
    }

    /// The distinction this whole enum exists for. A clean environment is
    /// evidence; an unreadable one is not.
    #[test]
    fn a_claude_started_outside_roost_is_outside() {
        let d = fake_proc(&["PATH=/usr/bin", "HOME=/home/x"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Outside);
    }

    /// Revert-checked: changing the `Err(_)` arm to `Sess::Outside` failed
    /// this test — `assertion `left == right` failed: left: Outside, right:
    /// Unknown` — then restored.
    #[test]
    fn an_unreadable_environ_is_unknown_not_outside() {
        // The pid directory exists but has no environ. Folding this into
        // Outside is how a live Claude silently stops receiving mentions.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("4242")).unwrap();
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    #[test]
    fn a_missing_proc_entry_is_unknown_not_outside() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    #[test]
    fn a_missing_proc_filesystem_is_unknown() {
        // Not Linux, or a container without /proc.
        let d = tempfile::tempdir().unwrap();
        let absent = d.path().join("no-proc-here");
        assert_eq!(session_of_in(&absent, 4242, "karpie"), Sess::Unknown);
    }

    /// Session names are unique within a project, not across them: `main`
    /// exists in most projects that have one.
    ///
    /// This is defence in depth, not the only barrier — and the spec's
    /// framing of it was too strong. `CONNS` is keyed by project, so a
    /// mention for project A cannot reach a connection registered under B no
    /// matter what this returns. What the ROOST_PROJECT test actually buys is
    /// a correct answer for a Claude whose environment says one project
    /// while it is connected to another's socket (lock-file discovery by
    /// path, rather than the `CLAUDE_CODE_SSE_PORT` shortcut `session_env`
    /// sets). Rare, but "rare" is not "impossible", and `Outside` is the
    /// honest answer for it.
    #[test]
    fn the_same_session_name_in_another_project_is_outside() {
        let d = fake_proc(&["ROOST_PROJECT=other", "ROOST_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Outside);
    }

    /// A name that fails valid_name cannot be matched against anything, so
    /// it is "cannot tell", not "not here". Outside would exclude this
    /// connection from a mention; Unknown leaves it eligible.
    #[test]
    fn an_invalid_session_name_is_unknown_not_in() {
        let d = fake_proc(&["ROOST_PROJECT=karpie", "ROOST_SESSION=../../etc/passwd"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    /// A partially scrubbed environment: the session is named but the
    /// project is not, so the name cannot be trusted to mean this project.
    #[test]
    fn a_session_without_a_project_is_unknown() {
        let d = fake_proc(&["ROOST_SESSION=main"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    /// The symmetric partial scrub: the project is named and matches, but
    /// the session is gone. This is "cannot tell which terminal", not
    /// "positively in a different project" — the `(_, Some(_))` arm must not
    /// swallow this case, since `p == project` is caught by the guarded arm
    /// above it only when a session name is also present.
    #[test]
    fn a_project_without_a_session_is_unknown_not_outside() {
        let d = fake_proc(&["ROOST_PROJECT=karpie"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::Unknown);
    }

    /// environ is NUL-separated, not newline-separated, and a value may
    /// itself contain '='. Splitting on the wrong byte or on every '='
    /// silently truncates the name.
    #[test]
    fn a_value_containing_an_equals_sign_survives() {
        let d = fake_proc(&["ROOST_PROJECT=karpie", "ROOST_SESSION=a-b", "OTHER=x=y=z"]);
        assert_eq!(session_of_in(d.path(), 4242, "karpie"), Sess::In("a-b".into()));
    }
}
