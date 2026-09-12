//! How this binary was installed, and whether it may replace itself.
//!
//! Step 1b of #65. An `[Update]` button has to know two different things, and
//! the mistake to avoid is answering both from one source.
//!
//! ## Why "who installed me" cannot be baked in
//!
//! Homebrew, the `.deb`, the `.rpm`, the release tarball and the shell
//! installer all ship the **same bytes**. The Homebrew formula downloads the
//! release tarball rather than compiling, and
//! `.github/workflows/build-linux-packages.yml` repackages that same tarball
//! ("Builds the .deb and .rpm from the binaries the release already
//! produced"). Measured on 2026-09-12: one sha256,
//! `a17bc4c9243f1c14a5b3f152dde9447c0bf13e4f41a53c289aa35f11e381f581`, for the
//! brew copy, a `curl`ed tarball and a browser download alike. So no value
//! compiled into the binary can tell those channels apart, and `build.rs`
//! deliberately bakes only the coarse thing it can know: `release`, `cargo` or
//! `checkout`.
//!
//! ## What the button actually needs to ask
//!
//! Not "who installed me" but **"can I replace my own file"**, which is what
//! Firefox asks: its updater is inert when the install directory is not
//! writable, and the UI then says updates are managed by the system rather
//! than offering a button that cannot work. That question has a direct answer,
//! so it gets one — a probe, not a guess.
//!
//! The owner sniff below is therefore **cosmetic**: it picks the wording of a
//! suggested command. If it guesses wrong the user reads a slightly wrong
//! suggestion; nothing acts on it. The load-bearing decision comes from the
//! probe. That split is the same rule the rest of this codebase follows — let
//! the dangerous operation depend on positive evidence and leave the guess to
//! the label.
use std::path::{Path, PathBuf};

// The same file `build.rs` includes, so the arm that decides a `cargo install`
// user's upgrade command is tested rather than assumed.
include!("channel.rs");

/// Baked by `build.rs`: how this binary was produced. `unknown` is a real
/// answer, as everywhere else in `BuildInfo`.
pub fn channel() -> &'static str {
    option_env!("ROOST_CHANNEL").unwrap_or("unknown")
}

/// Can this process replace its own executable?
///
/// Three outcomes, never two. "I could not find out" is not "no": a caller
/// that folded them would either hide a working `[Update]` or offer one that
/// cannot work, and which of those it did would depend on which way the fold
/// went. Same rule as CLAUDE.md's `Path::exists()` note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replaceable {
    Yes,
    No,
    /// The probe itself failed for a reason that is not permission — the
    /// directory is gone, the path could not be read, the filesystem answered
    /// something else.
    Unknown,
}

impl Replaceable {
    pub fn as_str(self) -> &'static str {
        match self {
            Replaceable::Yes => "yes",
            Replaceable::No => "no",
            Replaceable::Unknown => "unknown",
        }
    }
}

/// A best-effort guess at what manages this binary, used only to word a
/// suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Homebrew,
    /// `/usr/bin` or `/usr/local/bin` — a `.deb` or `.rpm`. Which of the two
    /// is deliberately not guessed: both install to the same place, and the
    /// distinction costs a probe of the host's identity to answer a question
    /// the user already knows the answer to.
    SystemPackage,
    /// `~/.cargo/bin`, which `cargo install` *and* the shell installer both
    /// write to (`install-path = "CARGO_HOME"` in dist-workspace.toml), so
    /// this one is disambiguated by the baked channel rather than by the path.
    CargoBin,
    Other,
    Unknown,
}

impl Owner {
    pub fn as_str(self) -> &'static str {
        match self {
            Owner::Homebrew => "homebrew",
            Owner::SystemPackage => "system-package",
            Owner::CargoBin => "cargo-bin",
            Owner::Other => "other",
            Owner::Unknown => "unknown",
        }
    }
}

/// Where this binary lives, or `None` when the platform will not say.
fn exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// Classifies an executable path. Pure, so it is testable without installing
/// anything anywhere.
pub fn owner_of(exe: &Path) -> Owner {
    let s = exe.to_string_lossy();
    // Homebrew keeps the real file under Cellar and symlinks it into its bin;
    // `current_exe` resolves the symlink, so Cellar is what is seen. Matched
    // as a path component rather than a substring: a project directory called
    // "Cellar" further up would otherwise claim every binary under it.
    if exe.components().any(|c| c.as_os_str() == "Cellar") {
        return Owner::Homebrew;
    }
    if let Some(dir) = exe.parent() {
        if dir == Path::new("/usr/bin") || dir == Path::new("/usr/local/bin") {
            return Owner::SystemPackage;
        }
        if dir.ends_with(".cargo/bin") {
            return Owner::CargoBin;
        }
    }
    if s.is_empty() {
        return Owner::Unknown;
    }
    Owner::Other
}

/// Whether `dir` can be written by this process, established by writing.
///
/// Not by reading permission bits: the mode says nothing about uid, gid,
/// supplementary groups, ACLs or a read-only mount, and a wrong answer here
/// either hides a working update or offers a broken one. Replacing a binary is
/// a rename within its directory, so the directory is what has to be writable
/// — a file can be mode 0444 and still be replaced by its owner's rename.
///
/// The probe file carries this process's pid so two roosts probing at once
/// cannot delete each other's, and it is removed immediately.
pub fn dir_writable(dir: &Path) -> Replaceable {
    let probe = dir.join(format!(".roost-write-probe.{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Replaceable::Yes
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Replaceable::No,
        // Read-only filesystems report their own kind on some platforms, and
        // it means the same thing as a permission refusal: the write cannot
        // happen. Anything else — the directory is gone, an interrupted call,
        // a filesystem answering something novel — is not evidence either way.
        Err(e) if e.kind() == std::io::ErrorKind::ReadOnlyFilesystem => Replaceable::No,
        Err(_) => Replaceable::Unknown,
    }
}

/// What this binary can say about its own installation.
#[derive(Debug, Clone, Copy)]
pub struct Install {
    pub channel: &'static str,
    pub replaceable: Replaceable,
    pub owner: Owner,
}

pub fn describe() -> Install {
    let (replaceable, owner) = match exe() {
        Some(p) => (
            p.parent().map(dir_writable).unwrap_or(Replaceable::Unknown),
            owner_of(&p),
        ),
        // No path means no probe and no sniff. Both unknown rather than one of
        // them guessed from the other.
        None => (Replaceable::Unknown, Owner::Unknown),
    };
    Install { channel: channel(), replaceable, owner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_path_names_its_manager() {
        for (path, want) in [
            ("/opt/homebrew/Cellar/roost/0.5.2/bin/roost", Owner::Homebrew),
            ("/usr/local/Cellar/roost/0.5.2/bin/roost", Owner::Homebrew),
            ("/usr/bin/roost", Owner::SystemPackage),
            ("/usr/local/bin/roost", Owner::SystemPackage),
            ("/home/claude/.cargo/bin/roost", Owner::CargoBin),
            ("/home/claude/projects/roost/target/debug/roost", Owner::Other),
        ] {
            assert_eq!(owner_of(Path::new(path)), want, "{path}");
        }
    }

    /// `Cellar` as a path component, not a substring: a project directory
    /// called `Cellar` — or any path merely containing the word — must not
    /// claim a binary Homebrew never installed.
    #[test]
    fn cellar_matches_a_component_and_not_a_substring() {
        assert_eq!(owner_of(Path::new("/home/me/Cellarium/bin/roost")), Owner::Other);
        assert_eq!(owner_of(Path::new("/home/me/my-Cellar-notes/roost")), Owner::Other);
        assert_eq!(owner_of(Path::new("/home/me/Cellar/roost")), Owner::Homebrew);
    }

    #[test]
    fn a_writable_directory_probes_yes_and_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(dir_writable(d.path()), Replaceable::Yes);
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().collect();
        assert!(left.is_empty(), "the probe file was not removed: {left:?}");
    }

    /// The case a permission-bits check gets wrong, and the reason this probes
    /// by writing: mode 0500 is readable and executable, so anything reasoning
    /// from `metadata()` would have to work out uid and group membership to
    /// reach the answer a single `create_new` gives directly.
    #[test]
    fn an_unwritable_directory_probes_no() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("locked");
        std::fs::create_dir(&sub).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
        let got = dir_writable(&sub);
        // Restore before asserting, or a failure leaves a directory TempDir
        // cannot remove and the next run inherits it.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(got, Replaceable::No, "a 0500 directory must not read as writable");
    }

    /// The case that makes this probe rather than read `metadata()`, and the
    /// only one where the two disagree. `/usr/bin` is root-owned mode 0755, so
    /// `Permissions::readonly()` is false — there *is* a write bit — while an
    /// ordinary user cannot write a byte into it. Measured on this host: bits
    /// say writable, `access(W_OK)` says no. A binary installed from a `.deb`
    /// lives exactly here, so reading the bits would offer that user an
    /// `[Update]` that cannot work.
    ///
    /// Skipped as root, where the answer is legitimately `Yes` and the
    /// assertion has nothing to say — a container commonly runs as uid 0.
    #[test]
    fn a_root_owned_bin_directory_probes_no_where_permission_bits_say_yes() {
        if unsafe { libc_getuid() } == 0 {
            eprintln!("skipped: running as root, /usr/bin is genuinely writable");
            return;
        }
        let bin = Path::new("/usr/bin");
        if !bin.is_dir() {
            eprintln!("skipped: no /usr/bin on this host");
            return;
        }
        let bits_say_writable = std::fs::metadata(bin)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false);
        assert!(bits_say_writable, "premise: /usr/bin carries a write bit for its owner");
        assert_eq!(
            dir_writable(bin),
            Replaceable::No,
            "a root-owned directory must not read as writable just because the mode has a write bit"
        );
    }

    /// `getuid` without pulling in a dependency for one call.
    unsafe fn libc_getuid() -> u32 {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }

    /// The third outcome, and the whole reason `Replaceable` is not a bool: a
    /// directory that is not there cannot be written to, but it is also not a
    /// permission refusal, and reporting `No` would say "your install is
    /// managed" about a path that has vanished.
    #[test]
    fn a_missing_directory_probes_unknown_rather_than_no() {
        let d = tempfile::tempdir().unwrap();
        let gone = d.path().join("not-here");
        assert_eq!(dir_writable(&gone), Replaceable::Unknown);
    }

    /// `describe()` must answer for the process actually running the tests,
    /// which is a real binary under `target/` — so the probe and the sniff are
    /// exercised against a live path rather than a fixture.
    #[test]
    fn describe_answers_for_the_running_test_binary() {
        let got = describe();
        assert!(
            ["release", "cargo", "checkout", "unknown"].contains(&got.channel),
            "unexpected channel {:?}",
            got.channel
        );
        // A test binary lives in a directory cargo just wrote, so it is
        // writable; anything else means the probe is not probing.
        assert_eq!(got.replaceable, Replaceable::Yes);
        assert_eq!(got.owner, Owner::Other, "a target/ build is not package-managed");
    }

    /// The arm no other test can reach: a build script is not part of the
    /// crate, so without sharing this function the `cargo` case could only be
    /// exercised by publishing a release. Order is asserted too, because a
    /// tagged CI build sits inside a checkout and testing `.git` first would
    /// label every published artifact `checkout`.
    #[test]
    fn the_channel_is_decided_in_the_right_order() {
        let d = tempfile::tempdir().unwrap();
        let tree = d.path().join("work");
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        let registry = d.path().join("registry/src/index.crates.io-abc/roost-0.5.2");
        std::fs::create_dir_all(&registry).unwrap();
        let bare = d.path().join("tarball");
        std::fs::create_dir_all(&bare).unwrap();

        assert_eq!(channel_from(Some("tag"), &tree), "release", "a tag wins over .git");
        assert_eq!(channel_from(Some("branch"), &tree), "checkout");
        assert_eq!(channel_from(None, &tree), "checkout");
        assert_eq!(channel_from(None, &registry), "cargo");
        assert_eq!(channel_from(Some("tag"), &registry), "release");
        assert_eq!(channel_from(None, &bare), "unknown", "neither git nor registry");
    }

    #[test]
    fn every_variant_has_a_wire_name() {
        assert_eq!(Replaceable::Yes.as_str(), "yes");
        assert_eq!(Replaceable::Unknown.as_str(), "unknown");
        assert_eq!(Owner::Homebrew.as_str(), "homebrew");
        assert_eq!(Owner::SystemPackage.as_str(), "system-package");
    }
}
