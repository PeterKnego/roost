//! Where each terminal's shell was working, recorded while it is alive so a
//! reboot can put you back there.
//!
//! roost outlives its own restart — dtach masters reparent to init — so a
//! restart keeps every shell with its cwd, its history and whatever was
//! running in it. A reboot outlives no process. The tab comes back from the
//! saved layout and `dtach -A` creates a *new* session for it: same name, same
//! tab, a fresh shell in the project root. Whatever tree of directories you
//! had arranged across four terminals is gone, and re-typing it is the first
//! thing anyone does after a reboot.
//!
//! Step 2 of #17. The processes cannot be brought back; the cheap part of
//! their state can. So while a shell is alive roost samples its cwd and writes
//! it down, and a session that is being *created* — never one that is being
//! rejoined, which already has its own cwd — starts there instead.
//!
//! ## Why a walk of `/proc` and not the pid roost already has
//!
//! roost's own child is the dtach *client*, whose cwd is the spawn directory
//! and never changes. The shell is a child of the dtach *master*, which
//! daemonised away from roost at first attach. So the process roost wants is
//! one it has no handle on, and the way to find it is the environment
//! `session_env` exports into it: `ROOST_PROJECT` and `ROOST_SESSION` are
//! inherited by everything under that shell.
//!
//! Everything under it, which is the catch — a `claude`, a `vim`, a `cargo`
//! all match too, and their cwds are not the shell's. The shell is picked out
//! by its parent: it is the process whose parent is the `dtach` master. That
//! is a structural fact about how the session is built, not a guess from
//! process names.
//!
//! The walk is on the same timer as `claudes.rs`'s, and for the same reason:
//! it touches every pid, so it is far too heavy to run per question.
//!
//! ## What this refuses to do
//!
//! A recorded cwd is *restored only inside the project*. Users `cd` out of a
//! checkout all the time, and re-entering `/etc` or another project's tree on
//! their behalf, at boot, from a file, is a different act from following them
//! there while they drive. `restore_dir` confines the recorded path exactly as
//! every other path in roost is confined and falls back to the project root,
//! which is what happens today.
//!
//! And it never claims a directory is gone from a failed check. A cwd that
//! cannot be resolved is not restored; a cwd that cannot be *read* is not
//! deleted. See CLAUDE.md's table — this module writes evidence that a later
//! decision leans on, so a half-written or misread marker here becomes a wrong
//! decision there.

use std::path::{Path, PathBuf};

/// One shell's working directory, as observed in the process table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sample {
    pub project: String,
    pub session: String,
    pub cwd: PathBuf,
}

/// Every roost shell's cwd, from ONE walk of `proc_root`.
///
/// `None` means the walk itself failed — `/proc` unreadable, or not mounted.
/// That is not "no shells are running", and the difference matters: this
/// module's whole output is evidence, and a caller that folded the two would
/// record an empty world over a populated one.
pub fn sample(proc_root: &Path) -> Option<Vec<Sample>> {
    let rd = std::fs::read_dir(proc_root).ok()?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(_pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Some((project, session)) = roost_env(&e.path()) else { continue };
        // The structural test that separates the shell from everything it
        // went on to run. Without it a `claude` deep in a subdirectory would
        // overwrite the shell's own cwd, and the restore would put the next
        // shell somewhere the user never was.
        if !parent_is_dtach(proc_root, &e.path()) {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) else { continue };
        out.push(Sample { project, session, cwd });
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// `ROOST_PROJECT` and `ROOST_SESSION` from a process's environment, when both
/// are there and the session name is one roost could have issued.
///
/// An unreadable `environ` is skipped rather than counted: on a shared machine
/// most pids belong to other users, and `EACCES` is the normal answer for
/// them, not a signal.
fn roost_env(proc_dir: &Path) -> Option<(String, String)> {
    let raw = std::fs::read(proc_dir.join("environ")).ok()?;
    let (mut proj, mut sess) = (None, None);
    for entry in raw.split(|b| *b == 0) {
        let Ok(kv) = std::str::from_utf8(entry) else { continue };
        if let Some(v) = kv.strip_prefix("ROOST_PROJECT=") {
            proj = Some(v.to_string());
        } else if let Some(v) = kv.strip_prefix("ROOST_SESSION=") {
            sess = Some(v.to_string());
        }
    }
    let (p, s) = (proj?, sess?);
    // The same gate every other path applies, and it is not decorative here:
    // the name becomes a filename below.
    if !crate::session::valid_name(&s) || !crate::session::valid_project(&p) {
        return None;
    }
    Some((p, s))
}

/// Is this process's parent a `dtach`?
///
/// The ppid is field 4 of `stat`, and it is read from the *end* of the line:
/// field 2 is `comm` in parentheses and a process may legally be named
/// `foo) S 1 (bar`. Splitting on whitespace from the left puts every later
/// field at an attacker-chosen offset. Everything after the final `)` is
/// fixed-width, so that is where the parse starts.
fn parent_is_dtach(proc_root: &Path, proc_dir: &Path) -> bool {
    let Ok(stat) = std::fs::read_to_string(proc_dir.join("stat")) else { return false };
    let Some(tail) = stat.rfind(')').map(|i| &stat[i + 1..]) else { return false };
    let mut fields = tail.split_whitespace();
    let _state = fields.next();
    let Some(ppid) = fields.next() else { return false };
    let Ok(comm) = std::fs::read_to_string(proc_root.join(ppid).join("comm")) else { return false };
    comm.trim() == "dtach"
}

/// Where the markers live: one file per session, beside nothing else.
fn cwd_dir(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("cwd").join(crate::projects::storage_key(project))
}

/// Records one shell's cwd, atomically.
///
/// Write-then-rename with a pid-unique temp name, the same discipline as
/// `registry::write_origin` and for the same reason: two roost instances may
/// share a state directory, and a reader that saw a half-written marker would
/// start a shell in a truncated path — or, worse, in `/`, since a truncated
/// absolute path is still absolute.
pub fn record(project: &str, session: &str, cwd: &Path) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let dir = cwd_dir(project);
    let bytes = cwd.as_os_str().as_encoded_bytes().to_vec();
    let marker = dir.join(session);
    // A shell that is not moving rewrites nothing. The sampler runs on a
    // timer over every session, so without this the state directory would
    // take a write per session per tick forever.
    if std::fs::read(&marker).map(|cur| cur == bytes).unwrap_or(false) {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{session}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, &marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Drops a session's marker, for when the session itself is ended.
///
/// Best-effort by design: a marker that outlives its session costs a stale
/// path that `restore_dir` will re-validate anyway, while refusing to end a
/// session because its marker would not unlink costs the user their close.
pub fn forget(project: &str, session: &str) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let _ = std::fs::remove_file(cwd_dir(project).join(session));
}

/// The raw recorded path, unvalidated. `restore_dir` is what callers want.
fn recorded(project: &str, session: &str) -> Option<PathBuf> {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return None;
    }
    let raw = std::fs::read(cwd_dir(project).join(session)).ok()?;
    if raw.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(raw)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8(raw).ok()?))
    }
}

/// Where a shell for `project`/`session` should start: the directory it was
/// last seen in, or `dir` — which is what roost does today, and what every
/// answer other than a confirmed, confined, still-present directory falls back
/// to.
///
/// Three ways to get `dir` back, and they are deliberately indistinguishable
/// to the caller because the safe action is identical for all three: nothing
/// was recorded, the record does not resolve inside the project, or it
/// resolves but is not a directory any more.
pub fn restore_dir(project: &str, session: &str, dir: &Path) -> PathBuf {
    let Some(rec) = recorded(project, session) else { return dir.to_path_buf() };
    // Confined exactly as every other path in roost is. A shell that was
    // parked in `/etc` when the machine went down is not evidence that roost
    // should open `/etc` at boot; following the user there while they drive is
    // a different act from doing it for them, from a file, unattended.
    // `safe_resolve` takes a `&str` because its usual input is a
    // project-relative path from a client. An absolute one is fine here and
    // needs no special case: `Path::join` lets it replace the base outright,
    // and the `starts_with` check downstream is exactly what then rejects it.
    // A cwd that is not UTF-8 is refused rather than lossily converted —
    // restoring an approximation of a directory is worse than the fallback.
    let Some(rec) = rec.to_str() else { return dir.to_path_buf() };
    let Ok(inside) = crate::projects::safe_resolve(dir, rec) else { return dir.to_path_buf() };
    // Positive evidence, not `is_dir()`: that swallows EACCES and a downed
    // mount into `false`, which here is merely a wasted restore — but this is
    // the module CLAUDE.md's table is about, and the habit is the point.
    match std::fs::symlink_metadata(&inside) {
        Ok(m) if m.is_dir() => inside,
        _ => dir.to_path_buf(),
    }
}

/// One pass: sample every roost shell and write down where it is.
///
/// A failed walk records nothing at all rather than recording an empty world,
/// so a `/proc` that cannot be read leaves yesterday's markers standing.
pub fn tick(proc_root: &Path) {
    let Some(seen) = sample(proc_root) else { return };
    for s in seen {
        record(&s.project, &s.session, &s.cwd);
    }
}

/// How often to re-walk `/proc`. A shell changing directory is not urgent —
/// the marker is only read when a session is created — and the walk touches
/// every pid on the machine.
///
/// `ROOST_CWD_POLL_SECS` overrides it, on the same footing as `ROOST_PING_SECS`
/// and for the same reason: the browser test needs a `cd` to reach the marker
/// inside a test's patience, and a fifteen-second sleep in the middle of an
/// assertion is how a real check gets quietly replaced by a shorter one. Not
/// documented as an operator knob — there is no reason to want it.
fn poll() -> std::time::Duration {
    let secs = std::env::var("ROOST_CWD_POLL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(15);
    std::time::Duration::from_secs(secs)
}

pub fn watch() {
    std::thread::spawn(|| {
        let every = poll();
        loop {
            tick(Path::new("/proc"));
            std::thread::sleep(every);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A fake `/proc`. Every field this module reads is a real file, so the
    /// parse is exercised rather than mocked — including the `stat` line's
    /// shape, which is the part with a trap in it.
    struct Proc {
        root: tempfile::TempDir,
    }

    impl Proc {
        fn new() -> Self {
            Proc { root: tempfile::tempdir().unwrap() }
        }
        fn path(&self) -> &Path {
            self.root.path()
        }
        /// A process with an environment, a parent, and a cwd symlink.
        fn add(&self, pid: u32, comm: &str, ppid: u32, env: &[(&str, &str)], cwd: Option<&Path>) {
            let d = self.root.path().join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
            // The real format: pid, (comm), state, ppid, then 49 more.
            std::fs::write(d.join("stat"), format!("{pid} ({comm}) S {ppid} 0 0 0 -1 0")).unwrap();
            let mut environ = Vec::new();
            for (k, v) in env {
                environ.extend_from_slice(format!("{k}={v}").as_bytes());
                environ.push(0);
            }
            std::fs::write(d.join("environ"), environ).unwrap();
            if let Some(c) = cwd {
                #[cfg(unix)]
                std::os::unix::fs::symlink(c, d.join("cwd")).unwrap();
            }
        }
    }

    fn shell_env(project: &str, session: &str) -> Vec<(&'static str, String)> {
        vec![("ROOST_PROJECT", project.to_string()), ("ROOST_SESSION", session.to_string())]
    }

    fn as_pairs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
        v.iter().map(|(k, x)| (*k, x.as_str())).collect()
    }

    #[test]
    fn the_shell_is_sampled_and_the_claude_inside_it_is_not() {
        // The distinction the whole module turns on. `session_env` exports
        // ROOST_SESSION into the shell, and *everything the shell runs*
        // inherits it — so a `claude` two directories deep matches the same
        // environment filter. Recording its cwd instead of the shell's would
        // put the next shell somewhere the user never stood.
        let p = Proc::new();
        let sh = shell_env("proj", "term");
        p.add(100, "dtach", 1, &[], None);
        p.add(101, "bash", 100, &as_pairs(&sh), Some(Path::new("/tmp/work")));
        p.add(102, "claude", 101, &as_pairs(&sh), Some(Path::new("/tmp/work/deep")));

        let got = sample(p.path()).expect("the walk itself succeeded");
        assert_eq!(
            got,
            vec![Sample {
                project: "proj".into(),
                session: "term".into(),
                cwd: "/tmp/work".into(),
            }],
            "only the child of the dtach master is the shell"
        );
    }

    #[test]
    fn a_process_name_that_looks_like_a_stat_line_cannot_move_the_ppid() {
        // `stat`'s second field is `(comm)` and a process may legally be
        // called `x) S 1 (y`. Splitting the line from the left puts ppid at an
        // offset the process itself chose — so a shell could name itself into
        // being sampled, or out of it. Everything after the LAST `)` is
        // fixed-width, which is why the parse starts there.
        let p = Proc::new();
        let sh = shell_env("proj", "term");
        p.add(200, "dtach", 1, &[], None);
        let d = p.path().join("201");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("comm"), "evil\n").unwrap();
        // Field 4 read from the left is `999`, a pid that is not a dtach.
        // Read from the right it is `200`, which is.
        std::fs::write(d.join("stat"), "201 (x) S 999 (y) S 200 0 0").unwrap();
        let mut environ = Vec::new();
        for (k, v) in as_pairs(&sh) {
            environ.extend_from_slice(format!("{k}={v}").as_bytes());
            environ.push(0);
        }
        std::fs::write(d.join("environ"), environ).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp/real", d.join("cwd")).unwrap();

        let got = sample(p.path()).expect("the walk succeeded");
        assert_eq!(got.len(), 1, "the real ppid is the one after the final ')': {got:?}");
        assert_eq!(got[0].cwd, Path::new("/tmp/real"));
    }

    #[test]
    fn an_unreadable_proc_is_not_an_empty_one() {
        // The rule this codebase is built around. `sample` feeds `tick`, which
        // writes the markers a later `restore_dir` acts on; folding "cannot
        // look" into "nothing is running" would be an empty world recorded
        // over a populated one.
        assert_eq!(sample(Path::new("/nonexistent-proc-root-for-test")), None);
    }

    #[test]
    fn a_recorded_directory_outside_the_project_is_not_restored() {
        // Users `cd` out of a checkout constantly. Following them there while
        // they drive is one thing; re-entering it for them, at boot, from a
        // file, is another — and it is the act CLAUDE.md's confinement rule
        // exists to refuse.
        crate::wsstate::set_state_dir_for_test();
        let proj = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let key = format!("outsidecwd{}", std::process::id());
        std::fs::create_dir_all(proj.path().join("sub")).unwrap();

        // Asserts the state it then negates: an *inside* path does restore,
        // so a fallback below cannot be mistaken for the feature not working.
        record(&key, "term", &proj.path().join("sub"));
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            proj.path().join("sub").canonicalize().unwrap(),
            "setup: a directory inside the project is restored"
        );

        record(&key, "term", outside.path());
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            proj.path(),
            "a directory outside it falls back to the project root"
        );
    }

    #[test]
    fn a_recorded_directory_that_is_gone_falls_back() {
        crate::wsstate::set_state_dir_for_test();
        let proj = tempfile::tempdir().unwrap();
        let key = format!("gonecwd{}", std::process::id());
        let sub = proj.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        record(&key, "term", &sub);
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            sub.canonicalize().unwrap(),
            "setup: while it is there, it is restored"
        );
        std::fs::remove_dir(&sub).unwrap();
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            proj.path(),
            "once it is gone, the project root — never a spawn into nothing"
        );

        // And the case the *metadata* check is actually load-bearing for.
        // `safe_resolve` canonicalises, so a path that has vanished is already
        // refused by the line above it — removing the metadata check does not
        // fail the assertion above, which is how this one came to be written.
        // A path that exists and is no longer a directory sails through
        // `safe_resolve` and reaches `cb.cwd`, where a non-directory is a
        // spawn failure and a terminal that never opens.
        std::fs::write(&sub, b"a file now stands where the directory was").unwrap();
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            proj.path(),
            "a recorded path that is no longer a directory falls back too"
        );
    }

    #[test]
    fn ending_a_session_must_not_leave_its_directory_for_the_next_one() {
        // `next_free_name` hands `term` straight back out after a close, so a
        // marker that outlived its session would drop a brand-new terminal
        // into the closed one's directory — with nothing on screen to explain
        // why it did not start where every other new terminal starts.
        crate::wsstate::set_state_dir_for_test();
        let proj = tempfile::tempdir().unwrap();
        let key = format!("forgetcwd{}", std::process::id());
        let sub = proj.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        record(&key, "term", &sub);
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            sub.canonicalize().unwrap(),
            "setup: recorded"
        );
        forget(&key, "term");
        assert_eq!(restore_dir(&key, "term", proj.path()), proj.path(), "and forgotten");
    }

    #[test]
    fn a_session_name_from_the_process_table_cannot_escape_the_marker_directory() {
        // The name becomes a filename. It arrives from a process environment,
        // which is chosen by whatever set it — a `claude` in another project,
        // a user exporting ROOST_SESSION by hand — so it is not roost's own
        // string by the time it gets here.
        crate::wsstate::set_state_dir_for_test();
        let proj = tempfile::tempdir().unwrap();
        let key = format!("escapecwd{}", std::process::id());
        // A *subdirectory*, so that "restored" and "fell back" are different
        // answers. Recording the project root itself made both the same path,
        // and the assertion below held with every guard removed — revert-check
        // caught it, review had not.
        let sub = proj.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            {
                record(&key, "term", &sub);
                restore_dir(&key, "term", proj.path())
            },
            sub.canonicalize().unwrap(),
            "setup: a well-formed name does restore, so a fallback below means something"
        );

        for bad in ["../escape", "a/b", "", &"x".repeat(33)] {
            record(&key, bad, &sub);
            assert_eq!(
                restore_dir(&key, bad, proj.path()),
                proj.path(),
                "a name that is not a session name records and restores nothing: {bad:?}"
            );
        }
        assert!(
            !crate::wsstate::state_dir().join("cwd").join("escape").exists(),
            "and nothing was written outside the marker directory"
        );
    }

    #[test]
    fn tick_records_what_it_sampled() {
        // The seam between the two halves. Everything above tests one side or
        // the other; without this, a `tick` that sampled correctly and wrote
        // nothing would pass the whole file.
        crate::wsstate::set_state_dir_for_test();
        let key = format!("tickcwd{}", std::process::id());
        let proj = tempfile::tempdir().unwrap();
        let sub = proj.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let p = Proc::new();
        let sh = shell_env(&key, "term");
        p.add(300, "dtach", 1, &[], None);
        p.add(301, "bash", 300, &as_pairs(&sh), Some(&sub));

        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            proj.path(),
            "setup: nothing recorded yet"
        );
        tick(p.path());
        assert_eq!(
            restore_dir(&key, "term", proj.path()),
            sub.canonicalize().unwrap(),
            "one pass records every shell it saw"
        );
    }
}
