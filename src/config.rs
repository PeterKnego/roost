//! Settings cascade: global ~/.config/deadlight/config.toml, then
//! {project}/.deadlight/config.toml. Re-read on every request — never cached.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    theme: Option<String>,
    default_tab: Option<String>,
    hide: Option<Vec<String>>,
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
            theme: "dark".into(),
            default_tab: "terminal".into(),
            hide: vec![],
            warning: None,
        }
    }
}

pub fn global_config_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config/deadlight/config.toml")
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

pub fn for_project(project_dir: &Path) -> Settings {
    load(&[
        &global_config_path(),
        &project_dir.join(".deadlight/config.toml"),
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
        assert_eq!(s.theme, "dark");
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
}
