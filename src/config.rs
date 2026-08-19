//! Settings cascade: global ~/.config/resh/config.toml, then
//! {project}/.resh/config.toml. Re-read on every request — never cached.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    theme: Option<String>,
    default_tab: Option<String>,
    hide: Option<Vec<String>>,
    allowed_origins: Option<Vec<String>>,
    max_upload_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub default_tab: String,
    pub hide: Vec<String>,
    pub warning: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "darcula".into(),
            default_tab: "terminal".into(),
            hide: vec![],
            warning: None,
        }
    }
}

pub fn global_config_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config/resh/config.toml")
}

pub fn load(paths: &[&Path]) -> Settings {
    let mut s = Settings::default();
    let mut warnings = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // missing file is normal, not a warning
        };
        match toml::from_str::<RawConfig>(&text) {
            Ok(raw) => {
                if let Some(v) = raw.theme {
                    s.theme = v;
                }
                if let Some(v) = raw.default_tab {
                    s.default_tab = v;
                }
                if let Some(v) = raw.hide {
                    s.hide = v;
                }
            }
            Err(e) => warnings.push(format!("{}: {}", path.display(), e.message())),
        }
    }
    if !warnings.is_empty() {
        s.warning = Some(warnings.join("; "));
    }
    s
}

/// Origins allowed to open a websocket or issue requests, from
/// `RESH_ORIGINS` (comma-separated) or the global config's
/// `allowed_origins`. Deliberately **not** part of [`Settings`]: a per-project
/// `.resh/config.toml` must never be able to allowlist an origin, or a
/// hostile repo could allowlist itself. Loopback is always allowed without
/// configuration — see [`crate::origin`].
pub fn allowed_origins() -> Vec<String> {
    let from_env: Vec<String> = std::env::var("RESH_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if !from_env.is_empty() {
        return from_env;
    }
    std::fs::read_to_string(global_config_path())
        .ok()
        .and_then(|t| toml::from_str::<RawConfig>(&t).ok())
        .and_then(|r| r.allowed_origins)
        .unwrap_or_default()
}

/// How often an otherwise silent websocket sends a Ping.
///
/// Both socket threads block on a channel between events, and a websocket
/// read blocks forever against a peer that stopped existing without TCP
/// noticing — a laptop that slept and woke on another network, say. Nothing
/// else in this process would ever discover that: there is no read deadline
/// on an upgraded socket, and an idle shell produces no output to fail on.
/// Writing something periodically is what turns that silence into an error
/// the existing teardown path already handles.
///
/// Thirty seconds is chosen to sit far below any NAT or tunnel idle timeout
/// while costing nothing measurable. `RESH_PING_SECS` exists so a test need
/// not wait that long; one second is its practical floor.
pub fn ping_interval() -> std::time::Duration {
    let secs = std::env::var("RESH_PING_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// 100 MB. Screenshots run 3–5 MB and a short screen recording 50–100 MB, so
/// this clears the real cases with room; past it is a mis-drag, and a mis-drag
/// that fills the disk breaks dtach socket creation, state writes and git at
/// once.
pub const DEFAULT_MAX_UPLOAD: u64 = 100_000_000;

/// Not configurable, deliberately. This expresses a product decision — resh is
/// not a project transfer tool, `git` and `scp` are — rather than fitting a
/// machine, and a tunable would only invite the decision to be configured away.
pub const MAX_UPLOAD_PARTS: usize = 16;

/// Aggregate bytes one upload request may carry.
///
/// Global-only, exactly like [`allowed_origins`] and for the same reason: a
/// per-project `.resh/config.toml` ships inside the repository, so a cloned
/// hostile repo could otherwise raise its own disk ceiling. Deliberately **not**
/// part of [`Settings`], which is the only thing a project file can reach.
pub fn max_upload_bytes() -> u64 {
    max_upload_from(&global_config_path())
}

/// Split from [`max_upload_bytes`] so tests can point at a real file instead of
/// rewriting `HOME`, which `state_dir` and `global_config_path` both read and
/// which other tests are running against concurrently.
fn max_upload_from(global: &Path) -> u64 {
    // A zero or unparseable value falls back rather than disabling the limit:
    // the failure mode of reading a typo as "unlimited" is a full disk.
    if let Ok(v) = std::env::var("RESH_MAX_UPLOAD") {
        if let Some(n) = v.trim().parse::<u64>().ok().filter(|n| *n > 0) {
            return n;
        }
    }
    std::fs::read_to_string(global)
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.max_upload_bytes)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_UPLOAD)
}

pub fn for_project(project_dir: &Path) -> Settings {
    load(&[
        &global_config_path(),
        &project_dir.join(".resh/config.toml"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_when_no_files() {
        let d = tempfile::tempdir().unwrap();
        let s = load(&[&d.path().join("none.toml")]);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn project_overrides_global_per_key() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "theme = \"light\"\nhide = [\"dist\"]").unwrap();
        fs::write(&p, "theme = \"gruvbox\"").unwrap();
        let s = load(&[&g, &p]);
        assert_eq!(s.theme, "gruvbox"); // project wins
        assert_eq!(s.hide, vec!["dist"]); // untouched key survives from global
        assert_eq!(s.default_tab, "terminal"); // default fills the rest
        assert!(s.warning.is_none());
    }

    #[test]
    fn malformed_file_warns_and_keeps_defaults() {
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad.toml");
        fs::write(&bad, "theme = [unclosed").unwrap();
        let s = load(&[&bad]);
        assert_eq!(s.theme, "darcula");
        assert!(s.warning.is_some());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.toml");
        fs::write(&f, "future_key = 1\ntheme = \"light\"").unwrap();
        let s = load(&[&f]);
        assert_eq!(s.theme, "light");
        assert!(s.warning.is_none());
    }

    /// A zero or garbage `RESH_PING_SECS` must fall back to the default, not
    /// be taken literally: `recv_timeout(0)` would turn both writer threads
    /// into busy loops flooding their sockets with Pings, which is worse than
    /// the leak the ping exists to bound. Verified by deleting the guard and
    /// watching the "0" case fail.
    #[test]
    fn ping_interval_defaults_and_rejects_a_useless_value() {
        // No other test reads this var, so setting it here races nothing.
        std::env::remove_var("RESH_PING_SECS");
        assert_eq!(ping_interval(), std::time::Duration::from_secs(30), "unset");
        std::env::set_var("RESH_PING_SECS", "5");
        assert_eq!(ping_interval(), std::time::Duration::from_secs(5), "explicit override");
        for bad in ["0", "-1", "", "soon"] {
            std::env::set_var("RESH_PING_SECS", bad);
            assert_eq!(
                ping_interval(),
                std::time::Duration::from_secs(30),
                "{bad:?} must fall back to the default rather than disabling or busy-looping"
            );
        }
        std::env::remove_var("RESH_PING_SECS");
    }

    /// `RESH_MAX_UPLOAD` is process-global and these tests write it, so they
    /// serialise. Without this they interleave and each sees another's value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The test that fails the moment someone "helpfully" moves this key into
    /// `Settings`. A project's `.resh/config.toml` ships inside the repository,
    /// so a cloned hostile repo could otherwise raise its own disk ceiling and
    /// turn a mis-drag into a disk-fill — the same argument `allowed_origins`
    /// already makes for itself.
    ///
    /// Asserts through `load`, which is the only path a project file has: if
    /// the key were in `Settings`, the returned value would differ from the
    /// default and this fails.
    #[test]
    fn a_project_config_cannot_carry_an_upload_ceiling() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("project.toml");
        fs::write(&proj, "max_upload_bytes = 999999999\ntheme = \"light\"\n").unwrap();
        let s = load(&[&proj]);
        // The keys a project *may* set still work, so this is not passing
        // because the file failed to parse.
        assert_eq!(s.theme, "light", "the project file must still be read");
        assert_eq!(
            s,
            Settings { theme: "light".into(), ..Settings::default() },
            "a project config must not be able to carry an upload ceiling"
        );
    }

    #[test]
    fn the_ceiling_comes_from_the_global_file_and_defaults_without_one() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_MAX_UPLOAD");
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope.toml");
        assert_eq!(max_upload_from(&missing), DEFAULT_MAX_UPLOAD);

        let global = d.path().join("config.toml");
        fs::write(&global, "max_upload_bytes = 5000\n").unwrap();
        assert_eq!(max_upload_from(&global), 5000);
    }

    #[test]
    fn the_env_var_overrides_the_global_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("config.toml");
        fs::write(&global, "max_upload_bytes = 5000\n").unwrap();
        std::env::set_var("RESH_MAX_UPLOAD", "1234");
        assert_eq!(max_upload_from(&global), 1234);
        std::env::remove_var("RESH_MAX_UPLOAD");
    }

    /// A typo must not read as "no limit". Zero and garbage both fall back to
    /// the default rather than disabling the ceiling, because the failure mode
    /// of getting this wrong is a full disk.
    #[test]
    fn a_bad_value_falls_back_rather_than_disabling_the_ceiling() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("config.toml");
        fs::write(&global, "max_upload_bytes = 0\n").unwrap();
        std::env::remove_var("RESH_MAX_UPLOAD");
        assert_eq!(max_upload_from(&global), DEFAULT_MAX_UPLOAD, "0 must not mean unlimited");

        for bad in ["banana", "0", "-5"] {
            std::env::set_var("RESH_MAX_UPLOAD", bad);
            assert_eq!(
                max_upload_from(&global),
                DEFAULT_MAX_UPLOAD,
                "RESH_MAX_UPLOAD={bad} must fall back, not disable the ceiling"
            );
        }
        std::env::remove_var("RESH_MAX_UPLOAD");
    }
}
