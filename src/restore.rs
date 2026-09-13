//! Putting a workspace back from an archive: every refusal, then the plan.
//!
//! #18 step 3, the destructive half. `backup.rs` owns the container and the
//! two pure operations on it (`plan`, `apply`); this owns the order the checks
//! happen in, which is the whole of what makes a restore safe.
//!
//! **Every refusal happens before the first byte is written.** #18 calls this
//! "the most destructive operation roost would have", and the shape that
//! follows from that is: resolve, read, parse, check, plan — and only then, if
//! the user asked for it a second time, apply. A restore that had already
//! replaced the layout before discovering the archive was truncated would have
//! destroyed something to deliver nothing.
//!
//! The refusals, in the order they fire:
//!
//! 1. **The file resolves inside the project.** `projects::safe_resolve`, like
//!    every other path from a browser. The archive is uploaded through
//!    `POST /upload`, so it is a file in the tree by the time this runs.
//! 2. **It is not bigger than an upload may be.** Same ceiling, deliberately:
//!    inventing a second one here is how a codebase ends up with two limits
//!    that disagree and a bug report about the wrong one.
//! 3. **It parses whole.** `backup::parse` refuses rather than repairs.
//! 4. **No session of this project is live.** A layout swapped under a running
//!    shell is how a terminal tab loses track of the session it is attached
//!    to. Named, so the user knows which to close.
//!
//! And one thing that is deliberately *not* a refusal: the archive's
//! `project` field disagreeing with this one. Restoring `alpha`'s archive into
//! `beta` is a supported operation — it is how a workspace moves to another
//! machine, which is what #18 asked for — so the mismatch is reported in the
//! listing and the user decides. The dry run exists so that they can.
//!
//! ## What is never read from the archive
//!
//! `header.source` names where the project was when the backup was taken. It
//! is shown and never used. Every destination comes from *this* project:
//! `wsstate::state_dir()` and `claudehist::transcript_dir(dir)`. That is what
//! makes an archive restore correctly at a different path on a different
//! machine — #18's third difficulty — and it is the property the no-paths
//! format exists to guarantee.

use crate::proto::Event;

/// The whole restore path, as one call, returning what the browser is told.
///
/// Never panics and never returns an error type: every outcome is a
/// `RestoreReport`, because this runs on a websocket read thread and
/// CLAUDE.md's "no panics may escape a socket thread" applies. A refusal is a
/// report with `refused` set, which the dialog renders as the whole of what
/// happened.
pub fn run(project: &str, dir: &std::path::Path, file: &str, dry_run: bool) -> Event {
    match plan_or_refusal(project, dir, file, dry_run) {
        Err(msg) => Event::RestoreReport { dry_run, lines: Vec::new(), refused: Some(msg) },
        Ok(lines) => Event::RestoreReport { dry_run, lines, refused: None },
    }
}

fn plan_or_refusal(
    project: &str,
    dir: &std::path::Path,
    file: &str,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    let path = crate::projects::safe_resolve(dir, file)
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    let md = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    if !md.is_file() {
        return Err(format!("{file} is not a file"));
    }
    let cap = crate::config::max_upload_bytes();
    if md.len() > cap {
        return Err(format!(
            "{file} is {} and the limit is {} — raise `max_upload_bytes` in the global \
             config if this archive is really yours",
            mb(md.len()),
            mb(cap)
        ));
    }
    let raw = std::fs::read(&path).map_err(|e| format!("cannot read {file}: {e}"))?;
    let archive = crate::backup::parse(&raw)?;

    // Positive evidence, and the reason this is a refusal rather than a
    // warning: `live_names` reports a session with a socket on disk as well as
    // one this process attached, so a restore during a detached-but-running
    // Claude is caught too.
    let live = crate::session::live_names(project);
    if !live.is_empty() {
        return Err(format!(
            "{} still running here ({}). Close them first — restoring a layout \
             underneath a running shell is how a tab loses the session it is attached to.",
            if live.len() == 1 { "1 terminal is" } else { "terminals are" },
            live.join(", ")
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tdir = crate::claudehist::transcript_dir(dir);
    let steps =
        crate::backup::plan(&crate::wsstate::state_dir(), tdir.as_deref(), project, &archive, now)?;

    let mut lines = describe(project, &archive);
    if dry_run {
        lines.extend(steps.iter().map(crate::backup::Step::line));
        // Not a cosmetic footer. Without it a dry run and a real one render as
        // the same list, and the person reading it has no way to tell whether
        // the machine has already changed.
        lines.push(String::from("Nothing has been written yet."));
        return Ok(lines);
    }
    lines.extend(crate::backup::apply(&archive, &steps)?);
    Ok(lines)
}

/// The header, as the two or three lines that precede a listing.
fn describe(project: &str, archive: &crate::backup::Archive) -> Vec<String> {
    let h = &archive.header;
    let mut out = vec![format!(
        "This archive was taken from {} by roost {}.",
        if h.project.is_empty() { "an unnamed project" } else { &h.project },
        if h.roost.is_empty() { "an unknown version" } else { &h.roost }
    )];
    if !h.source.is_empty() {
        // Shown, never used — see the module doc. Saying where it came from is
        // most of how a person recognises an archive they meant to restore.
        out.push(format!("It was at {} then.", h.source));
    }
    if h.project != project {
        // Reported, not refused: restoring one project's archive into another
        // is how a workspace moves to a new machine, which is what #18 asked
        // for. The user decides, and the dry run is what lets them.
        out.push(format!(
            "You are restoring it into {project}, which is a different project. \
             Conversations will be placed where {project} looks for them, not where \
             {} kept them.",
            h.project
        ));
    }
    out.extend(h.notes.iter().cloned());
    out
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(ev: &Event) -> (&[String], Option<&String>) {
        match ev {
            Event::RestoreReport { lines, refused, .. } => (lines, refused.as_ref()),
            other => panic!("expected a RestoreReport, got {other:?}"),
        }
    }

    fn refusal(ev: &Event) -> String {
        let (lines, refused) = report(ev);
        assert!(lines.is_empty(), "a refusal must not also list work: {lines:?}");
        refused.expect("expected a refusal").clone()
    }

    #[test]
    fn a_file_outside_the_project_is_refused_by_the_confinement_not_by_the_parser() {
        // The assertion that matters is *which* check fired. A test that only
        // asserted "refused" would pass against a version that happily read
        // /etc/passwd and then rejected it for not being an archive — which is
        // a read of a file outside the project, reported as a parse error.
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(d.path().join("outside.roostbak"), "ROOSTBAK1\n{}\n").unwrap();
        let e = refusal(&run("p", &proj, "../outside.roostbak", true));
        assert!(
            e.contains("outside project") || e.contains("cannot read"),
            "expected a confinement refusal, got: {e}"
        );
        assert!(!e.contains("not a roost backup"), "the file was read before being refused: {e}");
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_refused_before_anything_is_written() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("notes.txt"), "just some text\n").unwrap();
        let e = refusal(&run("p", &proj, "notes.txt", true));
        assert!(e.contains("not a roost backup"), "{e}");
    }

    #[test]
    fn a_missing_file_says_so_rather_than_reporting_an_empty_restore() {
        // "I could not read it" must never arrive as "there was nothing to
        // do", which an empty `lines` with no `refused` would render as.
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        let ev = run("p", &proj, "nope.roostbak", true);
        let (lines, refused) = report(&ev);
        assert!(refused.is_some(), "a missing archive must refuse: {lines:?}");
    }

    #[test]
    fn restoring_into_a_different_project_is_reported_and_not_refused() {
        // A supported operation — it is how a workspace moves to another
        // machine — so the mismatch has to reach the user as a sentence in the
        // listing, not as a refusal. Both halves are asserted: that it is
        // allowed, and that it is *said*.
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("restoretarget");
        std::fs::create_dir_all(&proj).unwrap();
        let mut raw = Vec::new();
        crate::backup::write_header(
            &mut raw,
            &crate::backup::Header {
                project: "somewhere-else".into(),
                created: 1757750000,
                roost: "0.5.2".into(),
                source: "/old/path/somewhere-else".into(),
                notes: vec!["layout".into()],
            },
        )
        .unwrap();
        std::fs::write(proj.join("a.roostbak"), &raw).unwrap();

        let ev = run("restoretarget", &proj, "a.roostbak", true);
        let (lines, refused) = report(&ev);
        assert!(refused.is_none(), "must not refuse: {refused:?}");
        let joined = lines.join("\n");
        assert!(joined.contains("somewhere-else"), "{joined}");
        assert!(joined.contains("different project"), "{joined}");
        assert!(joined.contains("/old/path/somewhere-else"), "where it came from: {joined}");
        assert!(joined.contains("Nothing has been written yet."), "{joined}");
    }

    #[test]
    fn a_dry_run_says_it_changed_nothing_and_a_real_one_does_not() {
        // The two renders differ only in this line, so without it a person
        // reading the listing cannot tell whether the machine has changed.
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("restoreapply");
        std::fs::create_dir_all(&proj).unwrap();
        let mut raw = Vec::new();
        crate::backup::write_header(
            &mut raw,
            &crate::backup::Header {
                project: "restoreapply".into(),
                created: 1,
                roost: "0.5.2".into(),
                source: String::new(),
                notes: vec![],
            },
        )
        .unwrap();
        crate::backup::write_entry(
            &mut raw,
            &crate::backup::Entry {
                kind: crate::backup::Kind::Workspace,
                id: String::new(),
                at: 0,
                bytes: br#"{"restored":true}"#.to_vec(),
            },
        )
        .unwrap();
        std::fs::write(proj.join("a.roostbak"), &raw).unwrap();

        let dry = run("restoreapply", &proj, "a.roostbak", true);
        let (lines, _) = report(&dry);
        assert!(lines.iter().any(|l| l == "Nothing has been written yet."));
        let landed = crate::wsstate::state_dir().join("restoreapply.json");
        assert!(!landed.exists(), "a dry run wrote the layout");

        let real = run("restoreapply", &proj, "a.roostbak", false);
        let (lines, refused) = report(&real);
        assert!(refused.is_none(), "{refused:?}");
        assert!(
            !lines.iter().any(|l| l == "Nothing has been written yet."),
            "a real restore claimed to have written nothing: {lines:?}"
        );
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), r#"{"restored":true}"#);
        let _ = std::fs::remove_file(&landed);
    }
}
