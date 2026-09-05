//! Settings cascade: global ~/.config/roost/config.toml, then
//! {project}/.roost/config.toml. Re-read on every request — never cached.
use crate::proto::{Scope, SettingValue};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    theme: Option<String>,
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
    pub hide: Vec<String>,
    pub show_hidden: bool,
    /// Whether the editor writes a buffer out on its own — a display-level
    /// preference like `show_hidden`, so a project may set it either way for
    /// itself. Unlike `allowed_origins` and `max_upload_bytes`, nothing a
    /// hostile checkout could set here widens a boundary: it only decides
    /// whether the person editing that project's own files has to press ⌘S.
    pub autosave: bool,
    /// Off unless a project asks for it. This ships file contents to Claude
    /// with no explicit user action, and roost has no permission system to
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
            hide: vec![],
            show_hidden: false,
            autosave: true,
            warning: None,
        }
    }
}

/// Keys a project file may set — display-level, nothing a hostile checkout
/// could widen a boundary with. In this order in the dialog.
pub const PROJECT_KEYS: &[&str] = &["theme", "hide", "show_hidden", "autosave"];
/// Keys only the global file may set; see the readers below for why each.
pub const GLOBAL_ONLY_KEYS: &[&str] = &["share_selection", "worktree_prompt"];
/// Keys no page may write. Shown read-only; not in any allowlist, so a
/// forged intent is refused too.
pub const READ_ONLY_KEYS: &[&str] = &["allowed_origins", "max_upload_bytes", "ide", "roots"];

/// The scopes a write is accepted for. The dialog's scope column and the
/// hub's refusal both read this, so they cannot disagree.
pub fn writable_in(key: &str) -> &'static [&'static str] {
    if PROJECT_KEYS.contains(&key) {
        &["project", "global"]
    } else if GLOBAL_ONLY_KEYS.contains(&key) {
        &["global"]
    } else {
        &[]
    }
}

/// Everything checked before a file is touched. Errors name the key and say
/// what would have been accepted, because they land in a banner.
pub fn validate(scope: Scope, key: &str, value: Option<&SettingValue>) -> Result<(), String> {
    let allowed = writable_in(key);
    if allowed.is_empty() {
        if READ_ONLY_KEYS.contains(&key) {
            return Err(format!("{key} can only be changed by hand, in the global config file"));
        }
        return Err(format!("{key} is not a setting"));
    }
    if scope == Scope::Project && !allowed.contains(&"project") {
        return Err(format!("{key} is a global setting; switch the scope to global"));
    }
    let Some(v) = value else { return Ok(()) };
    match (key, v) {
        ("theme", SettingValue::Str(name)) => match crate::themes::kind(name) {
            Some(_) => Ok(()),
            None => Err(format!("{name} is not a theme roost knows")),
        },
        ("theme", _) => Err("theme takes a name".into()),
        ("hide", SettingValue::List(items)) => {
            for it in items {
                if it.is_empty() {
                    return Err("hide: an empty entry".into());
                }
                if it.contains('/') || it.contains('\\') || it == "." || it == ".." {
                    return Err(format!("hide: {it} is not a single name"));
                }
            }
            Ok(())
        }
        ("hide", _) => Err("hide takes a list of names".into()),
        ("show_hidden" | "autosave" | "share_selection" | "worktree_prompt", SettingValue::Bool(_)) => Ok(()),
        (k, _) => Err(format!("{k} takes true or false")),
    }
}

/// `ROOST_CONFIG` overrides the location, which is what lets a test drive a
/// *global-only* setting without touching the developer's real
/// `~/.config/roost/config.toml` — the same reason `ROOST_STATE_DIR` exists.
/// Operators get the same knob for free: a second instance can carry its own
/// origins and caps without a second home directory.
pub fn global_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("ROOST_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config/roost/config.toml")
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
/// `.roost/config.toml` must never be able to allowlist an origin, or a
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

/// Not configurable, deliberately. This expresses a product decision — roost is
/// not a project transfer tool, `git` and `scp` are — rather than fitting a
/// machine, and a tunable would only invite the decision to be configured away.
pub const MAX_UPLOAD_PARTS: usize = 16;

/// Aggregate bytes one upload request may carry.
///
/// Global-only, exactly like [`allowed_origins`] and for the same reason: a
/// per-project `.roost/config.toml` ships inside the repository, so a cloned
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
/// Off means roost starts no ide listener, writes no lock file, and puts no
/// `CLAUDE_CODE_SSE_PORT` in a spawned shell — so `claude` simply never
/// discovers roost and falls back to its own terminal diffs. That is the only
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
        &project_dir.join(".roost/config.toml"),
    ])
}

/// One process-wide lock for the global file: every project's hub can
/// reach it, and two hubs editing it at once would race the rename.
static GLOBAL_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Validate, pick the file for `scope`, write. Returns the path written so
/// the caller can name it. The project file is `{project}/.roost/config.toml`.
pub fn set_setting(scope: Scope, project_dir: &Path, key: &str, value: Option<&SettingValue>) -> Result<PathBuf, String> {
    validate(scope, key, value)?;
    match scope {
        Scope::Project => {
            let p = project_dir.join(".roost").join("config.toml");
            write_setting(&p, key, value)?;
            Ok(p)
        }
        Scope::Global => {
            let p = global_config_path();
            let _g = GLOBAL_WRITE.lock().unwrap_or_else(|e| e.into_inner());
            write_setting(&p, key, value)?;
            Ok(p)
        }
    }
}

/// Set or remove one top-level key, keeping every other byte of the file:
/// comments, order, formatting. `toml_edit` is what makes that possible; a
/// `RawConfig` round-trip would drop all of it. A file that does not parse
/// is refused and left alone — rewriting a file we could not read is how a
/// hand-edited one gets destroyed. Atomic via temp file and rename.
pub fn write_setting(path: &Path, key: &str, value: Option<&SettingValue>) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    match value {
        None => {
            doc.as_table_mut().remove(key);
        }
        Some(v) => {
            let new_value = match v {
                SettingValue::Bool(b) => toml_edit::Value::from(*b),
                SettingValue::Str(s) => toml_edit::Value::from(s.as_str()),
                SettingValue::List(l) => {
                    let mut a = toml_edit::Array::new();
                    for s in l {
                        a.push(s.as_str());
                    }
                    toml_edit::Value::Array(a)
                }
            };
            // Replacing the whole `Item` drops its decor — the inline
            // comment trailing the old value — because the freshly built
            // one carries none. Keeping it means editing an existing
            // value's content in place rather than swapping the `Item`.
            match doc.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
                Some(existing) => *existing = new_value.decorated(
                    existing.decor().prefix().cloned().unwrap_or_default(),
                    existing.decor().suffix().cloned().unwrap_or_default(),
                ),
                None => doc[key] = toml_edit::Item::Value(new_value),
            }
        }
    }
    if let Some(parent) = path.parent() {
        // Only the immediate parent (`.roost/`) may be created here, not
        // `create_dir_all`'s whole chain above it: a project write whose
        // project directory is itself gone must fail, not resurrect it from
        // a stale path the hub still holds.
        match std::fs::create_dir(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("{}: {e}", parent.display())),
        }
    }
    // Full filename plus suffix, matching the temp-file convention used by
    // every other atomic write in this repo (claudehooks, registry, worktree).
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// The raw value of one key in one file, no cascade, no defaults: what the
/// dialog shows as "project: …" and "global: …". Integers come back as
/// their decimal text (only read-only keys carry them). A file that does
/// not parse reads as absent; `Settings::warning` reports the parse error
/// separately.
pub fn raw_setting(path: &Path, key: &str) -> Option<SettingValue> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    let item = doc.get(key)?;
    let v = item.as_value()?;
    if let Some(b) = v.as_bool() {
        return Some(SettingValue::Bool(b));
    }
    if let Some(s) = v.as_str() {
        return Some(SettingValue::Str(s.to_string()));
    }
    if let Some(i) = v.as_integer() {
        return Some(SettingValue::Str(i.to_string()));
    }
    if let Some(a) = v.as_array() {
        return Some(SettingValue::List(a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()));
    }
    None
}

/// Everything the settings dialog renders from. A fresh read of both files —
/// this function itself caches nothing. It is called from one place,
/// `hub::snapshot_event`, which does cache the result (`Hub::settings`; see
/// that field's doc comment), so the roughly twenty file reads here are paid
/// once per invalidation, not once per snapshot — a snapshot goes out on
/// every debounced keystroke.
pub fn settings_view(project_dir: &Path) -> crate::proto::SettingsView {
    use crate::proto::{SettingRow, SettingsView, SettingValue as V};
    let global = global_config_path();
    let project = project_dir.join(".roost").join("config.toml");
    let s = load(&[&global, &project]);
    let defaults = Settings::default();
    let raw = |key: &str| (raw_setting(&project, key), raw_setting(&global, key));
    let mut keys = Vec::new();
    let mut push = |key: &str, kind: &'static str, effective: V, default: V, reload: bool| {
        let (p, g) = raw(key);
        keys.push(SettingRow {
            key: key.to_string(),
            kind,
            writable: writable_in(key).to_vec(),
            effective,
            project: p,
            global: g,
            default,
            reload,
        });
    };
    push("theme", "str", V::Str(s.theme.clone()), V::Str(defaults.theme.clone()), false);
    push("hide", "list", V::List(s.hide.clone()), V::List(vec![]), false);
    push("show_hidden", "bool", V::Bool(s.show_hidden), V::Bool(false), false);
    push("autosave", "bool", V::Bool(s.autosave), V::Bool(true), true);
    push("share_selection", "bool", V::Bool(share_selection()), V::Bool(false), true);
    push("worktree_prompt", "bool", V::Bool(worktree_prompt()), V::Bool(true), false);
    push("allowed_origins", "list", V::List(allowed_origins()), V::List(vec![]), false);
    push("max_upload_bytes", "str", V::Str(max_upload_bytes().to_string()), V::Str(DEFAULT_MAX_UPLOAD.to_string()), false);
    push("ide", "bool", V::Bool(ide_enabled()), V::Bool(true), false);
    push(
        "roots",
        "list",
        V::List(configured_roots().iter().map(|p| p.display().to_string()).collect()),
        V::List(vec![]),
        false,
    );
    SettingsView {
        keys,
        themes: crate::themes::catalogue(),
        project_file: ".roost/config.toml".into(),
        global_file: global.display().to_string(),
        warning: s.warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Scope, SettingValue as V};
    use std::fs;

    #[test]
    fn defaults_when_no_files() {
        let d = tempfile::tempdir().unwrap();
        let s = load(&[&d.path().join("none.toml")]);
        assert_eq!(s, Settings::default());
    }

    /// `default_tab` was removed on 2026-09-05 (a v2 "which view opens"
    /// setting the four-pane client never read). Files that still set it
    /// must keep loading silently: an unknown key is not a warning.
    #[test]
    fn a_removed_key_in_an_old_file_is_ignored_without_a_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("old.toml");
        fs::write(&p, "default_tab = \"files\"\ntheme = \"light\"").unwrap();
        let s = load(&[&p]);
        assert_eq!(s.theme, "light");
        assert!(s.warning.is_none(), "{:?}", s.warning);
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
    /// `Settings`. A project's `.roost/config.toml` ships inside the repository,
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
        std::fs::create_dir_all(d.path().join(".roost")).unwrap();
        let project = d.path().join(".roost/config.toml");
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

    #[test]
    fn the_allowlist_refuses_by_name_and_scope() {
        // Revert-checked: changing `if scope == Scope::Project &&` to `if false &&`
        // failed this test when it tried to validate(Scope::Project, "share_selection", ...)
        // with `thread panicked at ... called Result::unwrap_err() on an Ok value: ()`.
        // This confirms the scope check is essential for rejecting project-level writes
        // to global-only keys. Then restored.
        assert!(validate(Scope::Project, "theme", Some(&V::Str("nord".into()))).is_ok());
        assert!(validate(Scope::Global, "share_selection", Some(&V::Bool(true))).is_ok());
        let e = validate(Scope::Project, "share_selection", Some(&V::Bool(true))).unwrap_err();
        assert!(e.contains("share_selection") && e.contains("global"), "{e}");
        for scope in [Scope::Project, Scope::Global] {
            let e = validate(scope, "allowed_origins", Some(&V::List(vec!["https://x".into()]))).unwrap_err();
            assert!(e.contains("allowed_origins") && e.contains("by hand"), "{e}");
            let e = validate(scope, "no_such_key", None).unwrap_err();
            assert!(e.contains("no_such_key"), "{e}");
        }
    }

    #[test]
    fn values_must_match_the_key_and_a_theme_must_exist() {
        let e = validate(Scope::Project, "autosave", Some(&V::Str("yes".into()))).unwrap_err();
        assert!(e.contains("autosave") && e.contains("true or false"), "{e}");
        let e = validate(Scope::Project, "theme", Some(&V::Str("not-a-theme".into()))).unwrap_err();
        assert!(e.contains("not-a-theme"), "{e}");
        let e = validate(Scope::Project, "hide", Some(&V::List(vec!["a/b".into()]))).unwrap_err();
        assert!(e.contains("a/b") && e.contains("single name"), "{e}");
        let e = validate(Scope::Project, "hide", Some(&V::List(vec!["".into()]))).unwrap_err();
        assert!(e.contains("empty"), "{e}");
        assert!(validate(Scope::Project, "hide", Some(&V::List(vec!["dist".into(), ".cache".into()]))).is_ok());
        // Clearing needs no value check, only the allowlist.
        assert!(validate(Scope::Global, "worktree_prompt", None).is_ok());
    }

    #[test]
    fn writable_in_mirrors_the_allowlist() {
        assert_eq!(writable_in("theme"), &["project", "global"]);
        assert_eq!(writable_in("worktree_prompt"), &["global"]);
        assert_eq!(writable_in("roots"), &[] as &[&str]);
        assert_eq!(writable_in("nope"), &[] as &[&str]);
    }

    #[test]
    fn writing_a_key_keeps_comments_and_other_keys_byte_for_byte() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        let before = "# my config\ntheme = \"dark\"   # the old one\nhide = [\"dist\"]\n\n[unrelated]\nx = 1\n";
        fs::write(&p, before).unwrap();
        write_setting(&p, "theme", Some(&V::Str("nord".into()))).unwrap();
        let after = fs::read_to_string(&p).unwrap();
        // Revert-check: replacing the toml_edit body with
        // `toml::to_string(&RawConfig{..})` loses the comment and the table.
        assert_eq!(after, "# my config\ntheme = \"nord\"   # the old one\nhide = [\"dist\"]\n\n[unrelated]\nx = 1\n");
        write_setting(&p, "autosave", Some(&V::Bool(false))).unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("autosave = false"));
        write_setting(&p, "hide", Some(&V::List(vec!["a".into(), "b".into()]))).unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("hide = [\"a\", \"b\"]"));
    }

    #[test]
    fn clearing_removes_the_key_and_a_missing_file_is_created() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("config.toml");
        write_setting(&p, "theme", Some(&V::Str("nord".into()))).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "theme = \"nord\"\n");
        write_setting(&p, "theme", None).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap().trim(), "");
        // Clearing an absent key is a no-op, not an error.
        write_setting(&p, "theme", None).unwrap();
    }

    #[test]
    fn an_unparsable_file_is_refused_and_untouched() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        fs::write(&p, "theme = \"dark\"\nthis is not toml\n").unwrap();
        let e = write_setting(&p, "theme", Some(&V::Str("nord".into()))).unwrap_err();
        assert!(e.contains("config.toml"), "{e}");
        // Revert-check: swapping `?` on the parse for `.unwrap_or_default()`
        // writes a one-line file here and this compare fails.
        assert_eq!(fs::read_to_string(&p).unwrap(), "theme = \"dark\"\nthis is not toml\n");
        // The literal "config.toml.tmp.0" never existed regardless of
        // whether a temp file leaked, since the real one carries this
        // process's actual pid — check the directory instead of one guessed
        // name, or a leaked temp file with a different pid would pass silently.
        let leaked = fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("config.toml.tmp."));
        assert!(!leaked, "a temp file was left behind after a refused write");
    }

    #[test]
    fn a_missing_project_directory_is_not_recreated() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("gone").join(".roost").join("config.toml");
        let e = write_setting(&p, "theme", Some(&V::Str("nord".into()))).unwrap_err();
        assert!(e.contains("gone"), "{e}");
        // Revert-check: putting `create_dir_all` back in place of `create_dir`
        // makes this fail — the whole `gone/.roost` chain gets created and
        // `write_setting` succeeds instead of erroring.
        assert!(!d.path().join("gone").exists(), "a deleted project directory must not be resurrected");
    }

    #[test]
    fn set_setting_picks_the_file_by_scope_and_validates_first() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("global.toml");
        std::env::set_var("ROOST_CONFIG", &global);
        let proj = d.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        let written = set_setting(Scope::Project, &proj, "theme", Some(&V::Str("nord".into()))).unwrap();
        assert_eq!(written, proj.join(".roost/config.toml"));
        assert!(fs::read_to_string(&written).unwrap().contains("theme = \"nord\""));
        assert!(!global.exists(), "a project write must not touch the global file");
        let written = set_setting(Scope::Global, &proj, "worktree_prompt", Some(&V::Bool(false))).unwrap();
        assert_eq!(written, global);
        let e = set_setting(Scope::Project, &proj, "worktree_prompt", Some(&V::Bool(false))).unwrap_err();
        assert!(e.contains("global"), "{e}");
        assert!(!fs::read_to_string(proj.join(".roost/config.toml")).unwrap().contains("worktree_prompt"));
        std::env::remove_var("ROOST_CONFIG");
    }

    #[test]
    fn raw_setting_reads_one_file_without_the_cascade() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c.toml");
        fs::write(&p, "theme = \"nord\"\nautosave = false\nhide = [\"x\"]\nmax_upload_bytes = 5\n").unwrap();
        assert_eq!(raw_setting(&p, "theme"), Some(V::Str("nord".into())));
        assert_eq!(raw_setting(&p, "autosave"), Some(V::Bool(false)));
        assert_eq!(raw_setting(&p, "hide"), Some(V::List(vec!["x".into()])));
        assert_eq!(raw_setting(&p, "max_upload_bytes"), Some(V::Str("5".into())));
        assert_eq!(raw_setting(&p, "show_hidden"), None);
        assert_eq!(raw_setting(&d.path().join("none.toml"), "theme"), None);
    }

    #[test]
    fn the_settings_view_reports_effective_project_global_and_default_per_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let global = d.path().join("global.toml");
        fs::write(&global, "theme = \"dark\"\nworktree_prompt = false\nmax_upload_bytes = 7\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &global);
        let proj = d.path().join("proj");
        fs::create_dir_all(proj.join(".roost")).unwrap();
        fs::write(proj.join(".roost/config.toml"), "theme = \"nord\"\n").unwrap();
        let v = settings_view(&proj);
        let row = |k: &str| v.keys.iter().find(|r| r.key == k).unwrap_or_else(|| panic!("no row {k}"));
        let t = row("theme");
        assert_eq!(t.effective, V::Str("nord".into()));
        assert_eq!(t.project, Some(V::Str("nord".into())));
        assert_eq!(t.global, Some(V::Str("dark".into())));
        assert_eq!(t.default, V::Str("darcula".into()));
        assert_eq!(t.writable, vec!["project", "global"]);
        let w = row("worktree_prompt");
        assert_eq!(w.effective, V::Bool(false));
        assert_eq!(w.writable, vec!["global"]);
        assert!(w.project.is_none());
        let m = row("max_upload_bytes");
        assert_eq!(m.effective, V::Str("7".into()));
        assert!(m.writable.is_empty());
        assert!(row("autosave").reload, "autosave is embedded at page load");
        assert!(!row("theme").reload);
        // Order: project keys, global-only keys, read-only keys.
        let keys: Vec<&str> = v.keys.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["theme", "hide", "show_hidden", "autosave", "share_selection", "worktree_prompt", "allowed_origins", "max_upload_bytes", "ide", "roots"]);
        assert_eq!(v.themes.len(), 5 + 35);
        assert!(v.global_file.ends_with("global.toml"));
        assert_eq!(v.project_file, ".roost/config.toml");
        assert!(v.warning.is_none());
        std::env::remove_var("ROOST_CONFIG");
    }
}
