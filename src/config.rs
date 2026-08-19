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
}
