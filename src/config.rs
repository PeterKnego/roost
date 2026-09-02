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
    show_hidden: Option<bool>,
    autosave: Option<bool>,
    allowed_origins: Option<Vec<String>>,
    max_upload_bytes: Option<u64>,
    share_selection: Option<bool>,
    ide: Option<bool>,
    roots: Option<Vec<String>>,
    worktree_prompt: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub default_tab: String,
    pub hide: Vec<String>,
    pub show_hidden: bool,
    /// Whether the editor writes a buffer out on its own — a display-level
    /// preference like `show_hidden`, so a project may set it either way for
    /// itself. Unlike `allowed_origins` and `max_upload_bytes`, nothing a
    /// hostile checkout could set here widens a boundary: it only decides
    /// whether the person editing that project's own files has to press ⌘S.
    pub autosave: bool,
    /// Off unless a project asks for it. This ships file contents to Claude
    /// with no explicit user action, and resh has no permission system to
    /// scope it the way Claude Code's own `Read` deny rules do. Unlike
    /// `allowed_origins` and `max_upload_bytes`, this *is* allowed per
    /// project: sharing your own selection with your own Claude cannot raise
    /// a ceiling on anything, so a project opting itself in is a decision
    /// only that project's own files are exposed by.
    pub warning: Option<String>,
}

impl Settings {
    /// The tree's visibility rule, borrowed rather than cloned: the caller
    /// holds the `Settings` for the length of the render.
    pub fn tree_filter(&self) -> crate::projects::TreeFilter<'_> {
        crate::projects::TreeFilter { hide: &self.hide, show_hidden: self.show_hidden }
    }

    /// The same rule with the workspace's header toggle applied. `None` means
    /// the workspace has never been toggled and this file is still the
    /// answer; `Some` is a decision a person made in the UI and outranks the
    /// file in both directions, including a `Some(false)` against a global
    /// `show_hidden = true`.
    pub fn tree_filter_with(&self, over: Option<bool>) -> crate::projects::TreeFilter<'_> {
        crate::projects::TreeFilter {
            hide: &self.hide,
            show_hidden: over.unwrap_or(self.show_hidden),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "darcula".into(),
            default_tab: "terminal".into(),
            hide: vec![],
            show_hidden: false,
            autosave: true,
            warning: None,
        }
    }
}

/// `ROOST_CONFIG` overrides the location, which is what lets a test drive a
/// *global-only* setting without touching the developer's real
/// `~/.config/resh/config.toml` — the same reason `ROOST_STATE_DIR` exists.
/// Operators get the same knob for free: a second instance can carry its own
/// origins and caps without a second home directory.
pub fn global_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("ROOST_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
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
                if let Some(v) = raw.show_hidden {
                    s.show_hidden = v;
                }
                if let Some(v) = raw.autosave {
                    s.autosave = v;
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
/// `ROOST_ORIGINS` (comma-separated) or the global config's
/// `allowed_origins`. Deliberately **not** part of [`Settings`]: a per-project
/// `.resh/config.toml` must never be able to allowlist an origin, or a
/// hostile repo could allowlist itself. Loopback is always allowed without
/// configuration — see [`crate::origin`].
pub fn allowed_origins() -> Vec<String> {
    let from_env: Vec<String> = std::env::var("ROOST_ORIGINS")
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
/// while costing nothing measurable. `ROOST_PING_SECS` exists so a test need
/// not wait that long; one second is its practical floor.
pub fn ping_interval() -> std::time::Duration {
    let secs = std::env::var("ROOST_PING_SECS")
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
/// Is the Claude Code IDE integration enabled? **Global config only.**
///
/// Off means resh starts no ide listener, writes no lock file, and puts no
/// `CLAUDE_CODE_SSE_PORT` in a spawned shell — so `claude` simply never
/// discovers resh and falls back to its own terminal diffs. That is the only
/// shape a kill switch can take from this side: refusing an `openDiff` once
/// the CLI has already found us makes it log "Failed to show diff in IDE" and
/// rethrow, which fails the edit rather than degrading it. The graceful
/// per-user choice about *where* a diff is drawn is the CLI's own `diffTool`
/// setting, not ours.
///
/// Global only, like `allowed_origins` and `max_upload_bytes`: a checked-out
/// repo must not be able to switch an integration back on for itself after
/// the user has switched it off.
pub fn ide_enabled() -> bool {
    ide_enabled_from(&global_config_path())
}

fn ide_enabled_from(global: &Path) -> bool {
    std::fs::read_to_string(global)
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.ide)
        // Absent, unreadable or unparseable all mean "on": the integration is
        // the default, and a typo elsewhere in the file must not silently
        // disable a feature the user never asked to turn off.
        .unwrap_or(true)
}

/// Whether ✻ offers a worktree when a Claude is already in the project.
/// Global only: it changes what a button does everywhere, and a checkout
/// must not get to decide that. Absent, unreadable or unparseable mean on.
pub fn worktree_prompt() -> bool {
    worktree_prompt_from(&global_config_path())
}

fn worktree_prompt_from(global: &Path) -> bool {
    std::fs::read_to_string(global)
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.worktree_prompt)
        .unwrap_or(true)
}

/// The directories scanned for projects, from the global config's `roots`.
/// **Global config only**, and the strictest case of that rule in this file.
///
/// `allowed_origins` and `max_upload_bytes` are global-only because a cloned
/// repo could otherwise widen a boundary set around it. `roots` is worse: it
/// does not widen a boundary, it *defines* the space every path confinement
/// is relative to. A project file that could add a root would make itself the
/// parent of directories it has no business seeing — so this is deliberately
/// not part of [`Settings`], which is the only thing a project file reaches.
///
/// A leading `~/` expands against `HOME`. This file is hand-edited, unlike
/// the unit file's `ROOST_ROOTS`, and a literal `~` directory that matches
/// nothing would fail as "no projects at all" — indistinguishable from every
/// project having vanished.
pub fn configured_roots() -> Vec<PathBuf> {
    roots_from_global(&global_config_path())
}

/// Split from [`configured_roots`] for the reason [`max_upload_from`] is:
/// tests point at a real file rather than rewriting `HOME`, which
/// `state_dir` and `global_config_path` both read and which other tests are
/// running against concurrently.
pub fn roots_from_global(global: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(global)
        .ok()
        .and_then(|t| toml::from_str::<RawConfig>(&t).ok())
        .and_then(|r| r.roots)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| expand_home(s.trim()))
        .collect()
}

fn expand_home(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest),
            // No HOME to expand against. Keep the literal rather than
            // silently dropping the entry: a root that resolves to nothing
            // shows up as one missing project, not as every project gone.
            None => PathBuf::from(s),
        },
        None => PathBuf::from(s),
    }
}

/// Does the editor selection travel to Claude? **Global config only.**
///
/// Moved here from the per-project cascade on 2026-08-23. The old reasoning
/// was that a project enabling it only exposes its own files, so there is no
/// ceiling to widen — true, but it put a "does file content leave this
/// machine" decision in a file a cloned repo ships. Every such decision now
/// lives in the one config file that is the user's own.
pub fn share_selection() -> bool {
    share_selection_from(&global_config_path())
}

fn share_selection_from(global: &Path) -> bool {
    std::fs::read_to_string(global)
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.share_selection)
        // Absent, unreadable or unparseable all mean "off". This one guards
        // content leaving the host, so the failure of a check is never
        // allowed to read as consent.
        .unwrap_or(false)
}

fn max_upload_from(global: &Path) -> u64 {
    // A zero or unparseable value falls back rather than disabling the limit:
    // the failure mode of reading a typo as "unlimited" is a full disk.
    if let Ok(v) = std::env::var("ROOST_MAX_UPLOAD") {
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

    // `show_hidden` is off unless a file turns it on, and a project may turn
    // it on for itself without the global file mentioning it — the tree's
    // visibility is a display preference, not a boundary any config could
    // widen.
    #[test]
    fn show_hidden_defaults_off_and_a_project_can_turn_it_on() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "hide = [\"dist\"]").unwrap();
        assert!(!load(&[&g]).show_hidden);
        fs::write(&p, "show_hidden = true").unwrap();
        let s = load(&[&g, &p]);
        assert!(s.show_hidden);
        assert_eq!(s.hide, vec!["dist"]); // and the global key still survives
        assert!(s.warning.is_none());
    }

    // Autosave is on unless a file turns it off, and both layers can move it
    // in both directions. Asserting only the default would pass with the
    // cascade never reading the key at all.
    #[test]
    fn autosave_defaults_on_and_either_layer_can_turn_it_off() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "hide = [\"dist\"]").unwrap();
        assert!(load(&[&g]).autosave, "on unless something says otherwise");

        fs::write(&p, "autosave = false").unwrap();
        let s = load(&[&g, &p]);
        assert!(!s.autosave, "a project can turn it off for itself");
        assert_eq!(s.hide, vec!["dist"], "and the global key still survives");

        // The other direction: a global `false` that a project overrides back
        // on. Without this the cascade could be a one-way latch.
        fs::write(&g, "autosave = false").unwrap();
        assert!(!load(&[&g]).autosave);
        fs::write(&p, "autosave = true").unwrap();
        assert!(load(&[&g, &p]).autosave, "a project can turn it back on");
        assert!(load(&[&g, &p]).warning.is_none());
    }

    // Off unless a file turns it on — the opposite default from autosave,
    // because this key ships file contents to Claude with no explicit user
    // action. Both layers can still move it in both directions, same as
    // autosave and show_hidden: asserting only the default would pass with
    // the cascade never reading the key at all.
    //
    // Revert-checked: flipping `Settings::default()`'s `share_selection` to
    // `true` failed the first assertion here (`!load(&[&g]).share_selection`)
    // — `assertion failed: !load(...)` — leaving the rest of this test (which
    // never re-checks the off state) green. The same break also failed
    // `ide::tests::selection_sharing_is_off_unless_a_project_opts_in` and
    // `render::tests::share_selection_is_off_by_default_and_the_indicator_
    // appears_only_when_it_is_on` — three legitimate hits on the one default,
    // not evidence any of the three is redundant. Then restored.
    #[test]
    fn share_selection_is_global_only_and_a_project_cannot_turn_it_on() {
        // Scope changed on 2026-08-23: it used to cascade like `autosave`,
        // on the argument that a project enabling it only exposes its own
        // files. True, but it left a "does file content leave this machine"
        // decision in a file a cloned repo ships. Now every such decision
        // lives in the one config file that is the user's own.
        //
        // Revert-checked: routing `share_selection_from` through `load(&[global,
        // project])` instead of reading the global file alone failed the third
        // assertion here — the project's `true` reached it. Then restored.
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");

        fs::write(&g, "hide = [\"dist\"]").unwrap();
        assert!(!share_selection_from(&g), "off unless the global file says otherwise");

        fs::write(&g, "share_selection = true").unwrap();
        assert!(share_selection_from(&g), "the user's own global file can turn it on");

        // The whole point of the move: this file is a checkout's, not the
        // user's, and it must not be able to reach this setting at all.
        fs::write(&p, "share_selection = true").unwrap();
        fs::write(&g, "hide = [\"dist\"]").unwrap();
        assert!(!share_selection_from(&g), "a project file cannot turn it on");

        // And the cascade no longer carries it, so nothing reads it by accident.
        let s = load(&[&g, &p]);
        assert_eq!(s.hide, vec!["dist"], "the global keys that do cascade still do");
    }

    #[test]
    fn an_unreadable_global_file_leaves_sharing_off_and_the_ide_on() {
        // The two defaults point opposite ways on purpose. A check that
        // failed must never read as consent to send file contents, and must
        // never silently disable an integration the user did not turn off.
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope.toml");
        assert!(!share_selection_from(&missing), "cannot read => not sharing");
        assert!(ide_enabled_from(&missing), "cannot read => integration stays on");

        let junk = d.path().join("junk.toml");
        fs::write(&junk, "this is not toml [[[").unwrap();
        assert!(!share_selection_from(&junk), "unparseable => not sharing");
        assert!(ide_enabled_from(&junk), "unparseable => integration stays on");
    }

    #[test]
    fn the_ide_kill_switch_is_global_only() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        assert!(ide_enabled_from(&g), "on by default");
        fs::write(&g, "ide = false").unwrap();
        assert!(!ide_enabled_from(&g), "the user's global file can switch it off");
        // A repo must not be able to switch it back on for itself.
        fs::write(&p, "ide = true").unwrap();
        assert!(!ide_enabled_from(&g), "a project file cannot re-enable it");
        let _ = p;
    }

    #[test]
    fn worktree_prompt_is_on_unless_the_global_config_says_off() {
        // Revert-checked: `unwrap_or(false)` fails the first assertion.
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("config.toml");
        assert!(worktree_prompt_from(&g), "absent file: on");
        std::fs::write(&g, "worktree_prompt = false\n").unwrap();
        assert!(!worktree_prompt_from(&g));
        std::fs::write(&g, "this is not toml\n").unwrap();
        assert!(worktree_prompt_from(&g), "unparseable: on, a typo must not change a button");
    }

    // The reverse direction: a global `true` is what a per-project `false`
    // has to be able to override, or the setting is one-way.
    #[test]
    fn a_project_can_turn_show_hidden_back_off() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "show_hidden = true").unwrap();
        fs::write(&p, "show_hidden = false").unwrap();
        assert!(load(&[&g]).show_hidden);
        assert!(!load(&[&g, &p]).show_hidden);
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

    /// A zero or garbage `ROOST_PING_SECS` must fall back to the default, not
    /// be taken literally: `recv_timeout(0)` would turn both writer threads
    /// into busy loops flooding their sockets with Pings, which is worse than
    /// the leak the ping exists to bound. Verified by deleting the guard and
    /// watching the "0" case fail.
    #[test]
    fn ping_interval_defaults_and_rejects_a_useless_value() {
        // No other test reads this var, so setting it here races nothing.
        std::env::remove_var("ROOST_PING_SECS");
        assert_eq!(ping_interval(), std::time::Duration::from_secs(30), "unset");
        std::env::set_var("ROOST_PING_SECS", "5");
        assert_eq!(ping_interval(), std::time::Duration::from_secs(5), "explicit override");
        for bad in ["0", "-1", "", "soon"] {
            std::env::set_var("ROOST_PING_SECS", bad);
            assert_eq!(
                ping_interval(),
                std::time::Duration::from_secs(30),
                "{bad:?} must fall back to the default rather than disabling or busy-looping"
            );
        }
        std::env::remove_var("ROOST_PING_SECS");
    }

    /// `ROOST_MAX_UPLOAD` is process-global and these tests write it, so they
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
        std::env::remove_var("ROOST_MAX_UPLOAD");
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
        std::env::set_var("ROOST_MAX_UPLOAD", "1234");
        assert_eq!(max_upload_from(&global), 1234);
        std::env::remove_var("ROOST_MAX_UPLOAD");
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
        std::env::remove_var("ROOST_MAX_UPLOAD");
        assert_eq!(max_upload_from(&global), DEFAULT_MAX_UPLOAD, "0 must not mean unlimited");

        for bad in ["banana", "0", "-5"] {
            std::env::set_var("ROOST_MAX_UPLOAD", bad);
            assert_eq!(
                max_upload_from(&global),
                DEFAULT_MAX_UPLOAD,
                "ROOST_MAX_UPLOAD={bad} must fall back, not disable the ceiling"
            );
        }
        std::env::remove_var("ROOST_MAX_UPLOAD");
    }

    /// The property that makes `roots` global-only, pinned rather than left
    /// to the shape of `Settings`.
    ///
    /// Discriminating on purpose: the theme assertion proves the project file
    /// really is read and merged over the global one, so the empty roots
    /// result cannot be explained away as "the project file was ignored
    /// entirely". Adding `roots` to the per-project cascade later would let a
    /// cloned repo declare itself the parent of directories it has no
    /// business seeing, and this is what would fail.
    #[test]
    fn a_project_config_cannot_contribute_a_project_root() {
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("global.toml");
        std::fs::write(&global, "theme = \"dawn\"\n").unwrap();
        std::fs::create_dir_all(d.path().join(".resh")).unwrap();
        let project = d.path().join(".resh/config.toml");
        std::fs::write(&project, "theme = \"midnight\"\nroots = [\"/etc\"]\n").unwrap();

        // The project file is genuinely read and genuinely wins on a key the
        // cascade does carry.
        let settings = load(&[&global, &project]);
        assert_eq!(settings.theme, "midnight", "the project file must really be parsed and merged");

        // And yet it contributes no root: `roots` consults the global path
        // alone, so the project's entry is never in the list at all.
        assert!(
            crate::projects::roots_from(None, &global).is_empty(),
            "the project file declared /etc as a root and it must not appear"
        );
    }

}
