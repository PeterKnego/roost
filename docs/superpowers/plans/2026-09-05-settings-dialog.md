# Settings Dialog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A settings dialog behind the header's gear: a Settings pane that edits the display keys of either config file, and a Theme pane with live preview, both writing through the project websocket.

**Architecture:** One new intent, `SetSetting`, validated and written by the project hub with `toml_edit` so hand-edited files keep their comments; the state snapshot gains a `settings` block the dialog renders from; the client swaps theme stylesheets in place for preview and follows the snapshot after Save. A small new module `src/themes.rs` owns the theme catalogue so `render.rs` (page head) and `config.rs` (validation, snapshot) share one list.

**Tech Stack:** Rust (toml_edit 0.22, already in the tree under `toml`), plain JS in `static/app.js` and `static/dialog.js`, deno browser tests in `tests/browser/`.

**Spec:** `docs/superpowers/specs/2026-09-05-settings-dialog-design.md`

## Global Constraints

- Every browser-side state change is a websocket intent; the HTTP surface stays at `GET` plus `POST /upload` and `POST /paste`. No new endpoint.
- A config file that does not parse is refused with its parse error and left byte for byte alone. Never rewrite a file we could not read.
- Writes are atomic: temp file with a pid-unique name in the same directory, then `rename`.
- Never hold a lock across blocking I/O other than the one small file write, exactly as `SetClaudeHooks` does.
- Read-only keys (`allowed_origins`, `max_upload_bytes`, `ide`, `roots`) have no write path at all: not in the allowlist, so a forged intent is refused.
- Global-only keys (`share_selection`, `worktree_prompt`) are writable only in `Scope::Global`.
- Nothing in the dialog builds HTML strings from data; `textContent`/`createElement` only.
- `cargo test -- --test-threads=1`, never `--release`. Browser tests one at a time.
- Every new test is revert-checked: apply the broken version, watch it fail, restore, and record the failure in the test's comment.

---

## File structure

| File | Responsibility |
|---|---|
| `src/themes.rs` (new) | The theme catalogue: `DAISY_THEMES`, `kind(name)`, `catalogue()` with roost tile colours parsed from the embedded theme files. |
| `src/render.rs` | `theme_head` uses `themes::kind`; the `#dlg-settings` shell; structural CSS additions; the gear's title. |
| `src/config.rs` | `Scope`, `SettingValue` re-exports; the per-scope allowlist; `validate`; `write_setting` (toml_edit); `set_setting`; `settings_view` for the snapshot. |
| `src/proto.rs` | `Intent::SetSetting`, `Scope`, `SettingValue`, `SettingsView`, `SettingRow`, `ThemeEntry`, `WorkspaceView.settings`. |
| `src/hub.rs` | The `SetSetting` arm; `snapshot_event` fills `settings`. |
| `static/dialog.js` | `openSettings(...)`: the dialog's own state, both panes, Save/Cancel. |
| `static/app.js` | `applyTheme`, theme follow on `State`, live autosave, the gear's click handler, feeding the open dialog. |
| `static/style.css` | Rules for the tabs, scope switch, rows, tiles. |
| `tests/browser/settings.mjs` (new) | The end-to-end proof in Chromium. |
| `docs/deploy.md`, `tests/browser/README.md` | The user-facing description and the test list line. |

---

### Task 1: The theme catalogue module

**Files:**
- Create: `src/themes.rs`
- Modify: `src/lib.rs` (add `pub mod themes;`), `src/render.rs` (move `DAISY_THEMES`, use `themes::kind` in `theme_head`)

**Interfaces:**
- Produces: `themes::DAISY_THEMES: [&str; 35]`, `themes::ThemeKind { Roost, Daisy }`, `themes::kind(name: &str) -> Option<ThemeKind>`, `themes::roost_names() -> Vec<String>`, `themes::catalogue() -> Vec<crate::proto::ThemeEntry>` (Task 3 defines `ThemeEntry`; until then this task returns `Vec<(String, &'static str, [String; 3])>` — see Step 3 — and Task 3 switches it).

- [ ] **Step 1: Write the failing tests** at the bottom of the new `src/themes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roost_files_win_over_daisyui_names_and_unknown_is_none() {
        assert_eq!(kind("dark"), Some(ThemeKind::Roost), "dark is a roost file even though daisyUI has one");
        assert_eq!(kind("darcula"), Some(ThemeKind::Roost));
        assert_eq!(kind("nord"), Some(ThemeKind::Daisy));
        assert_eq!(kind("someones-own"), None, "a user-directory theme is not in the catalogue");
        assert_eq!(kind("_daisy"), None);
    }

    #[test]
    fn the_catalogue_lists_roost_first_then_daisyui_in_its_own_order() {
        let c = catalogue();
        let names: Vec<&str> = c.iter().map(|e| e.0.as_str()).collect();
        let roost = roost_names();
        assert_eq!(&names[..roost.len()], roost.as_slice());
        assert_eq!(&names[roost.len()..], &DAISY_THEMES[..]);
        // Every roost entry carries the three tile colours read from its file;
        // every daisyUI entry carries none (the browser resolves them).
        for e in &c[..roost.len()] {
            assert!(e.2.iter().all(|c| c.starts_with('#') && c.len() == 7), "{}: {:?}", e.0, e.2);
        }
        for e in &c[roost.len()..] {
            assert!(e.2.iter().all(String::is_empty), "{}: {:?}", e.0, e.2);
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib themes:: -- --test-threads=1`
Expected: compile error, `themes` module missing.

- [ ] **Step 3: Implement `src/themes.rs`**

```rust
//! The theme catalogue: which names `theme = "…"` may take, and where each
//! one comes from. `render::theme_head` (the page head), `config::validate`
//! (refusing a name the page would not resolve) and the settings snapshot
//! (the picker's tiles) all read this one list, so they cannot disagree.
//!
//! Two sources. roost's own theme files are the embedded `themes/*.css`, one
//! `:root { … }` of literal colours each. daisyUI 5's 35 built-in themes
//! come from the vendored `vendor/daisyui-themes.css`, keyed by `data-theme`
//! on `<html>` and mapped onto roost's variables by `daisy-bridge.css`. A
//! roost file wins over a daisyUI name (`dark`, `light` exist on both sides)
//! because an existing config must keep meaning what it meant.

/// daisyUI 5's built-in themes, in its own order.
pub const DAISY_THEMES: [&str; 35] = [
    "light", "dark", "cupcake", "bumblebee", "emerald", "corporate", "synthwave", "retro",
    "cyberpunk", "valentine", "halloween", "garden", "forest", "aqua", "lofi", "pastel",
    "fantasy", "wireframe", "black", "luxury", "dracula", "cmyk", "autumn", "business",
    "acid", "lemonade", "night", "coffee", "winter", "dim", "nord", "sunset", "caramellatte",
    "abyss", "silk",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Roost,
    Daisy,
}

pub fn kind(name: &str) -> Option<ThemeKind> {
    if crate::assets::get(&format!("themes/{name}.css")).is_some() {
        Some(ThemeKind::Roost)
    } else if DAISY_THEMES.contains(&name) {
        Some(ThemeKind::Daisy)
    } else {
        None
    }
}

/// The embedded roost theme names, sorted, from the asset table.
pub fn roost_names() -> Vec<String> {
    let mut v: Vec<String> = crate::assets::names()
        .filter_map(|rel| rel.strip_prefix("themes/")?.strip_suffix(".css").map(str::to_string))
        .collect();
    v.sort();
    v
}

/// `--name: #rrggbb` from a theme file, or an empty string. Only the three
/// colours a picker tile needs; the browser resolves daisyUI's itself.
fn var_of(css: &str, name: &str) -> String {
    let Some(at) = css.find(&format!("{name}:")) else { return String::new() };
    let rest = css[at + name.len() + 1..].trim_start();
    let hex: String = rest.chars().take_while(|c| *c == '#' || c.is_ascii_alphanumeric()).collect();
    if hex.len() == 7 && hex.starts_with('#') { hex } else { String::new() }
}

/// (name, kind, [bg, fg, accent]) for every theme, roost first.
pub fn catalogue() -> Vec<(String, &'static str, [String; 3])> {
    let mut out = Vec::new();
    for name in roost_names() {
        let css = crate::assets::get(&format!("themes/{name}.css"))
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        out.push((name, "roost", [var_of(&css, "--bg"), var_of(&css, "--fg"), var_of(&css, "--accent")]));
    }
    for name in DAISY_THEMES {
        out.push((name.to_string(), "daisy", [String::new(), String::new(), String::new()]));
    }
    out
}
```

`crate::assets::names()` does not exist yet. Add it to `src/assets.rs` next to `get`:

```rust
/// Every embedded asset path, for callers that enumerate a directory of them.
pub fn names() -> impl Iterator<Item = &'static str> {
    ASSETS.iter().map(|(k, _)| *k)
}
```

Add `pub mod themes;` to `src/lib.rs` in alphabetical position among the other `pub mod` lines.

In `src/render.rs`, delete the `DAISY_THEMES` const and its doc comment (the block that begins `/// daisyUI 5's built-in themes, in its own order.`) and change `theme_head` to:

```rust
fn theme_head(theme: &str) -> (String, String) {
    match crate::themes::kind(theme) {
        Some(crate::themes::ThemeKind::Daisy) => (
            format!("<html data-theme=\"{theme}\">"),
            "<link rel=\"stylesheet\" href=\"/static/vendor/daisyui-themes.css\">\n\
             <link rel=\"stylesheet\" href=\"/static/daisy-bridge.css\">"
                .into(),
        ),
        // A roost file, or an unknown name linked as a file: that is how a
        // theme in the user directory is reached.
        _ => ("<html>".into(), format!("<link rel=\"stylesheet\" href=\"/static/themes/{}.css\">", esc(theme))),
    }
}
```

Replace the one remaining reference in `docs/deploy.md` from `render::DAISY_THEMES` to `themes::DAISY_THEMES`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib themes:: -- --test-threads=1 && cargo test --lib render::tests -- --test-threads=1`
Expected: both PASS; the two existing render theme tests (`a_daisyui_theme_name_sets_data_theme_and_links_the_vendored_themes`, `roost_theme_files_and_unknown_names_are_linked_as_files_with_no_data_theme`) still pass.

- [ ] **Step 5: Revert-check.** In `kind`, swap the two branches so the daisyUI check runs first; run `cargo test --lib themes::` — `roost_files_win_over_daisyui_names_and_unknown_is_none` must fail on `dark`. Restore. Note it in the test's comment.

- [ ] **Step 6: Commit**

```bash
git add src/themes.rs src/lib.rs src/assets.rs src/render.rs docs/deploy.md
git commit -m "themes: one catalogue module for the page head, validation and the picker"
```

---

### Task 2: Protocol types

**Files:**
- Modify: `src/proto.rs` (after `pub enum Launch`, and the `WorkspaceView` struct)
- Modify: `src/themes.rs` (`catalogue()` returns `Vec<ThemeEntry>`)

**Interfaces:**
- Produces: `proto::Scope { Global, Project }` (serde lowercase), `proto::SettingValue { Bool(bool), Str(String), List(Vec<String>) }` (serde untagged), `Intent::SetSetting { scope, key, value: Option<SettingValue> }`, `proto::SettingRow`, `proto::ThemeEntry`, `proto::SettingsView`, `WorkspaceView.settings: SettingsView`.

- [ ] **Step 1: Write the failing tests** in `src/proto.rs`'s `mod tests`:

```rust
    #[test]
    fn set_setting_decodes_each_value_shape_and_a_clear() {
        let b: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"project","key":"autosave","value":false}"#).unwrap();
        assert_eq!(b, Intent::SetSetting { scope: Scope::Project, key: "autosave".into(), value: Some(SettingValue::Bool(false)) });
        let s: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"global","key":"theme","value":"nord"}"#).unwrap();
        assert_eq!(s, Intent::SetSetting { scope: Scope::Global, key: "theme".into(), value: Some(SettingValue::Str("nord".into())) });
        let l: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"project","key":"hide","value":["dist","out"]}"#).unwrap();
        assert_eq!(l, Intent::SetSetting { scope: Scope::Project, key: "hide".into(), value: Some(SettingValue::List(vec!["dist".into(), "out".into()])) });
        let c: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"project","key":"theme"}"#).unwrap();
        assert_eq!(c, Intent::SetSetting { scope: Scope::Project, key: "theme".into(), value: None });
    }

    #[test]
    fn a_settings_view_serialises_nulls_for_absent_scopes() {
        let row = SettingRow {
            key: "theme".into(), kind: "str", writable: vec!["project", "global"],
            effective: SettingValue::Str("nord".into()), project: Some(SettingValue::Str("nord".into())),
            global: None, default: SettingValue::Str("darcula".into()), reload: false,
        };
        let j = serde_json::to_string(&row).unwrap();
        assert!(j.contains(r#""global":null"#), "{j}");
        assert!(j.contains(r#""effective":"nord""#), "{j}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib proto::tests::set_setting -- --test-threads=1`
Expected: compile error, `SetSetting`/`Scope`/`SettingValue` not found.

- [ ] **Step 3: Add the types.** After `pub enum Launch { Claude }`:

```rust
/// Which config file a `SetSetting` edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Project,
}

/// A config value as the dialog carries it. Untagged: `true`, `"nord"` and
/// `["dist"]` are unambiguous on the wire and in TOML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Str(String),
    List(Vec<String>),
}
```

In `pub enum Intent`, after `SetClaudeHooks { on: bool },`:

```rust
    /// Write one key into one config file, or clear it (`value: None`) so
    /// inheritance resumes. Validated in `config::validate` before any
    /// file is touched; see the settings-dialog spec.
    SetSetting {
        scope: Scope,
        key: String,
        #[serde(default)]
        value: Option<SettingValue>,
    },
```

Before `pub struct WorkspaceView`:

```rust
/// One row of the settings dialog. `writable` is the scopes the hub will
/// accept a write for (the same table `config::validate` refuses by), so
/// the dialog cannot offer a control the server would refuse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingRow {
    pub key: String,
    pub kind: &'static str,
    pub writable: Vec<&'static str>,
    pub effective: SettingValue,
    pub project: Option<SettingValue>,
    pub global: Option<SettingValue>,
    pub default: SettingValue,
    /// True when the page only reads this key at load, so the row says so.
    pub reload: bool,
}

/// A picker tile. roost themes carry their three colours; daisyUI tiles
/// carry empty strings and resolve their own through `data-theme`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThemeEntry {
    pub name: String,
    pub kind: &'static str,
    pub bg: String,
    pub fg: String,
    pub accent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct SettingsView {
    pub keys: Vec<SettingRow>,
    pub themes: Vec<ThemeEntry>,
    pub project_file: String,
    pub global_file: String,
    /// `Settings::warning` — a config file that did not parse, named.
    pub warning: Option<String>,
}
```

In `pub struct WorkspaceView`, after `pub claude_hooks: Option<bool>,`:

```rust
    /// Filled by `hub::snapshot_event` from a fresh `config::settings_view`;
    /// never persisted (it is a read of the files, not workspace state).
    #[serde(default)]
    pub settings: SettingsView,
```

`WorkspaceView` is constructed in `Workspace::view()` (`src/workspace.rs`); add `settings: SettingsView::default(),` there so it compiles (search for `claude_hooks: None,` in that function and add the line after it).

Change `themes::catalogue()` to return `Vec<crate::proto::ThemeEntry>`:

```rust
pub fn catalogue() -> Vec<crate::proto::ThemeEntry> {
    use crate::proto::ThemeEntry;
    let mut out = Vec::new();
    for name in roost_names() {
        let css = crate::assets::get(&format!("themes/{name}.css"))
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        out.push(ThemeEntry { name, kind: "roost", bg: var_of(&css, "--bg"), fg: var_of(&css, "--fg"), accent: var_of(&css, "--accent") });
    }
    for name in DAISY_THEMES {
        out.push(ThemeEntry { name: name.to_string(), kind: "daisy", bg: String::new(), fg: String::new(), accent: String::new() });
    }
    out
}
```

and update Task 1's catalogue test to read `e.name`, `e.kind`, and `[&e.bg, &e.fg, &e.accent]` instead of the tuple fields.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib proto::tests -- --test-threads=1 && cargo test --lib themes:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/proto.rs src/workspace.rs src/themes.rs
git commit -m "proto: SetSetting intent and the settings block of the snapshot"
```

---

### Task 3: Validation and the allowlist

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Consumes: `proto::Scope`, `proto::SettingValue`, `themes::kind`.
- Produces: `config::PROJECT_KEYS`, `config::GLOBAL_ONLY_KEYS`, `config::READ_ONLY_KEYS`, `config::writable_in(key) -> &'static [&'static str]`, `config::validate(scope, key, value: Option<&SettingValue>) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests** in `src/config.rs`'s `mod tests`:

```rust
    use crate::proto::{Scope, SettingValue as V};

    #[test]
    fn the_allowlist_refuses_by_name_and_scope() {
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib config::tests::the_allowlist -- --test-threads=1`
Expected: compile error, `validate` not found.

- [ ] **Step 3: Implement**, above `pub fn global_config_path`:

```rust
use crate::proto::{Scope, SettingValue};

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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::tests -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Revert-check.** Change `if scope == Scope::Project && !allowed.contains(&"project")` to `if false &&` … — `the_allowlist_refuses_by_name_and_scope` fails on `share_selection`. Restore. Record in the test comment.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "config: the per-scope allowlist and value validation for SetSetting"
```

---

### Task 4: Writing a key with toml_edit

**Files:**
- Modify: `Cargo.toml` (add `toml_edit = "0.22"` under `[dependencies]`), `src/config.rs`

**Interfaces:**
- Produces: `config::write_setting(path: &Path, key: &str, value: Option<&SettingValue>) -> Result<(), String>`, `config::set_setting(scope: Scope, project_dir: &Path, key: &str, value: Option<&SettingValue>) -> Result<PathBuf, String>` (validates, picks the file, locks the global one, writes; returns the path written), `config::raw_setting(path: &Path, key: &str) -> Option<SettingValue>`.

- [ ] **Step 1: Write the failing tests** in `src/config.rs`'s `mod tests`:

```rust
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
        assert!(!d.path().join("config.toml.tmp.0").exists());
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
```

`ENV_LOCK` already exists in `config.rs`'s `mod tests` (a `static Mutex<()>` the tests that set `ROOST_CONFIG` take); use it as the other env-setting tests there do.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib config::tests::writing_a_key -- --test-threads=1`
Expected: compile error, `write_setting` not found.

- [ ] **Step 3: Implement.** Add `toml_edit = "0.22"` to `[dependencies]` in `Cargo.toml` directly under `toml = "0.8"`. In `src/config.rs`:

```rust
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
            let item = match v {
                SettingValue::Bool(b) => toml_edit::value(*b),
                SettingValue::Str(s) => toml_edit::value(s.as_str()),
                SettingValue::List(l) => {
                    let mut a = toml_edit::Array::new();
                    for s in l {
                        a.push(s.as_str());
                    }
                    toml_edit::value(a)
                }
            };
            doc[key] = item;
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::tests -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Revert-checks**, each restored afterwards and recorded in the test:
  1. Replace `text.parse().map_err(...)?` with `text.parse().unwrap_or_default()` — `an_unparsable_file_is_refused_and_untouched` fails on the byte compare.
  2. Replace the toml_edit body of `write_setting` with `std::fs::write(path, format!("{key} = ..."))` for the string case — `writing_a_key_keeps_comments_and_other_keys_byte_for_byte` fails on the comment.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs
git commit -m "config: write one key with toml_edit, atomically, refusing a file that does not parse"
```

---

### Task 5: The settings view for the snapshot

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Consumes: `raw_setting`, `load`, `writable_in`, `themes::catalogue`, the global-only readers.
- Produces: `config::settings_view(project_dir: &Path) -> proto::SettingsView`.

- [ ] **Step 1: Write the failing test** in `src/config.rs`'s `mod tests`:

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib config::tests::the_settings_view -- --test-threads=1`
Expected: compile error, `settings_view` not found.

- [ ] **Step 3: Implement**, after `raw_setting`:

```rust
/// Everything the settings dialog renders from. A fresh read of both files:
/// config is never cached, and this runs once per snapshot, not per
/// keystroke (only `SetSetting` and the usual layout intents re-snapshot).
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
```

`share_selection` is marked `reload: true` because `SHARE_SELECTION` is a page-load constant in `app.js`; `autosave` becomes live in Task 8 but stays `reload: true` for other browsers, which is what the row's hint means.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::tests -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "config: settings_view — effective, project, global and default per key for the dialog"
```

---

### Task 6: The hub arm

**Files:**
- Modify: `src/hub.rs` (`handle`, next to the `Intent::SetClaudeHooks` arm; `snapshot_event`), `src/workspace.rs` (the `SetShowHidden` comment)

**Interfaces:**
- Consumes: `config::set_setting`, `config::settings_view`.
- Produces: `Intent::SetSetting` handled; `WorkspaceView.settings` filled in every snapshot.

- [ ] **Step 1: Write the failing test** in `src/hub.rs`'s `mod tests`, modelled on `toggling_hidden_files_reaches_every_client_and_survives_a_reload` (two subscribers, `drain`):

```rust
    /// Two subscribers, deliberately: with one, `broadcast` and `send_to`
    /// are indistinguishable, and a setting that reached only the saving
    /// browser would look right in every single-client test.
    #[test]
    fn set_setting_writes_the_project_file_and_every_client_sees_the_new_value() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path().join("state"));
        std::env::set_var("ROOST_CONFIG", d.path().join("global.toml"));
        let proj = d.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let mut h = Hub::new("set_setting", proj.clone());
        h.ws.show_hidden = Some(false); // the header toggle's override
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);
        let before = h.ws.version;

        h.handle(&b, Intent::SetSetting {
            scope: crate::proto::Scope::Project,
            key: "show_hidden".into(),
            value: Some(crate::proto::SettingValue::Bool(true)),
        });

        let file = std::fs::read_to_string(proj.join(".roost/config.toml")).unwrap();
        assert!(file.contains("show_hidden = true"), "{file}");
        assert_eq!(h.ws.show_hidden, None, "writing the file clears the toggle's override");
        assert!(h.ws.version > before);
        for (who, msgs) in [("the other client", drain(&rx_a)), ("the saving client", drain(&rx_b))] {
            assert!(
                msgs.iter().any(|m| m.contains(r#""key":"show_hidden""#) && m.contains(r#""effective":true"#)),
                "{who} must see the new effective value; got {msgs:?}"
            );
        }

        // A refused write: error to the sender only, file untouched.
        h.handle(&b, Intent::SetSetting {
            scope: crate::proto::Scope::Project,
            key: "allowed_origins".into(),
            value: Some(crate::proto::SettingValue::List(vec!["https://evil".into()])),
        });
        let to_b = drain(&rx_b);
        assert!(to_b.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("allowed_origins")), "{to_b:?}");
        assert!(drain(&rx_a).is_empty(), "a refusal is not broadcast");
        assert!(!std::fs::read_to_string(proj.join(".roost/config.toml")).unwrap().contains("evil"));
        std::env::remove_var("ROOST_CONFIG");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib hub::tests::set_setting_writes -- --test-threads=1`
Expected: FAIL — the intent falls through to whatever `handle`'s default does; the file assertion fails with "No such file".

- [ ] **Step 3: Implement.** In `handle`, after the `Intent::SetClaudeHooks { on } => { … }` arm:

```rust
            Intent::SetSetting { scope, key, value } => {
                // A file write under the hub lock, like SetClaudeHooks: one
                // small file, and what it says next is what every client of
                // this project is about to be sent. The global file takes a
                // process-wide lock inside set_setting as well.
                if let Err(e) = crate::config::set_setting(*scope, &self.dir, key, value.as_ref()) {
                    self.send_to(from, &Event::Error { msg: e });
                    return;
                }
                if key == "show_hidden" {
                    // The header toggle's override outranks the file in both
                    // directions (workspace.rs). Writing the file from the
                    // dialog is the one gesture that means "follow the file
                    // again"; without this a person who set the file to
                    // true and sees no change has no way to find out why.
                    self.ws.show_hidden = None;
                    self.persist();
                }
                self.ws.version += 1;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                return;
            }
```

In `snapshot_event`, after `ws.claude_hooks = cached;`:

```rust
        ws.settings = crate::config::settings_view(&self.dir);
```

In `src/workspace.rs`, the comment on the `SetShowHidden` arm says there is no gesture that means "go back to following the config file". Replace that sentence with: `The settings dialog writing show_hidden is that gesture: hub.rs clears this override when it does.`

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib hub::tests -- --test-threads=1`
Expected: PASS, including the existing show-hidden test.

- [ ] **Step 5: Revert-check.** Delete the `if key == "show_hidden" { … }` block — the override assertion fails. Restore. Replace `self.broadcast(&snap)` with `self.send_to(from, &snap)` — "the other client" fails. Restore. Record both.

- [ ] **Step 6: Commit**

```bash
git add src/hub.rs src/workspace.rs
git commit -m "hub: SetSetting writes the file, clears the show_hidden override, broadcasts the snapshot"
```

---

### Task 7: The dialog shell and the structural lock

**Files:**
- Modify: `src/render.rs` (the shells after `#dlg-choice`; `DIALOG_STRUCTURAL_CSS`; the `#settings` button; tests), `static/style.css`

- [ ] **Step 1: Write the failing tests** in `src/render.rs`'s `mod tests`: extend `the_workspace_page_ships_empty_dialog_shells` — the `for id in [...]` list gains `"dlg-settings"`, the `for frag in [...]` list gains `"<dialog id=\"dlg-settings\" class=\"roost\" hidden"`, and add:

```rust
        assert!(html.contains(r#"<button id="settings" title="settings">"#), "the gear is no longer 'not implemented'");
```

Extend `dialog_structural_css_lands_after_theme_css` with:

```rust
        for cls in [".dlg-tabs", ".dlg-scope", ".dlg-rows", ".dlg-row", ".dlg-themes", ".dlg-tile"] {
            assert!(DIALOG_STRUCTURAL_CSS.contains(cls), "the structural CSS does not lock {cls}");
        }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib render::tests::the_workspace_page_ships_empty_dialog_shells -- --test-threads=1 && cargo test --lib render::tests::dialog -- --test-threads=1`
Expected: both FAIL ("no dlg-settings shell", "does not lock .dlg-tabs").

- [ ] **Step 3: Implement.** After the `#dlg-choice` shell in the workspace template:

```html
<dialog id="dlg-settings" class="roost dlg-wide">
  <h2 class="dlg-title">Settings</h2>
  <div class="dlg-tabs" role="tablist"></div>
  <div class="dlg-scope"></div>
  <div class="dlg-rows" hidden></div>
  <div class="dlg-themes" hidden></div>
  <div class="dlg-buttons">
    <button type="button" class="dlg-cancel">Cancel</button>
    <button type="button" class="dlg-ok">Save</button>
  </div>
</dialog>
```

Change the gear: `<button id="settings" title="settings">{SVG_GEAR}</button>`.

In `DIALOG_STRUCTURAL_CSS`, extend the `.dlg-body, .dlg-buttons, .dlg-items { display: flex !important; … }` rule's selector list with `.dlg-tabs, .dlg-scope, .dlg-rows:not([hidden]), .dlg-themes:not([hidden]), .dlg-row` and add `.dlg-tile { display: revert !important; visibility: visible !important; opacity: 1 !important; }`. Extend the generated-content rule's selector list with `.dlg-tile::before, .dlg-tile::after, .dlg-row::before, .dlg-row::after`. Add `.dlg-rows` to the `flex-direction: column !important;` rule's selector list; `.dlg-themes` wraps its tiles and is left out of it.

In `static/style.css`, after the `.dlg-item:hover, .dlg-item:focus` rule:

```css
/* ---- settings dialog ---- */
dialog.roost.dlg-wide { width: min(680px, 96vw); }
.dlg-tabs { display: flex; gap: 2px; padding: 8px 14px 0; border-bottom: 1px solid var(--border); }
.dlg-tab { font: inherit; padding: 5px 12px; border: 1px solid transparent; border-bottom: none; border-radius: 6px 6px 0 0; background: none; color: var(--muted); cursor: pointer; }
.dlg-tab[aria-selected="true"] { color: var(--fg); border-color: var(--border); background: var(--bg); }
.dlg-scope { display: flex; gap: 8px; align-items: center; padding: 10px 14px; font-size: 12px; color: var(--muted); }
.dlg-scope button { font: inherit; padding: 3px 10px; border: 1px solid var(--border); border-radius: 4px; background: none; color: var(--muted); cursor: pointer; }
.dlg-scope button[aria-pressed="true"] { color: var(--fg); border-color: var(--accent); }
.dlg-rows { display: flex; flex-direction: column; max-height: 55vh; overflow: auto; padding: 0 14px 8px; }
.dlg-row { display: grid; grid-template-columns: 140px 1fr auto; gap: 8px 12px; align-items: start; padding: 8px 0; border-bottom: 1px solid var(--border); font-size: 12px; }
.dlg-row label { color: var(--fg); padding-top: 3px; }
.dlg-row .hint { grid-column: 2 / 4; color: var(--muted); font-size: 11px; }
.dlg-row input[type="text"], .dlg-row textarea { width: 100%; font: inherit; background: var(--bg); color: var(--fg); border: 1px solid var(--border); border-radius: 4px; padding: 3px 6px; }
.dlg-row textarea { min-height: 3em; resize: vertical; font-family: var(--mono); }
.dlg-row .ro { color: var(--muted); font-family: var(--mono); overflow-wrap: anywhere; }
.dlg-row .clear { font: inherit; font-size: 11px; background: none; border: 1px solid var(--border); border-radius: 4px; color: var(--muted); cursor: pointer; padding: 2px 8px; }
.dlg-row.disabled label, .dlg-row.disabled .hint { opacity: .5; }
.dlg-themes { padding: 8px 14px; max-height: 55vh; overflow: auto; }
.dlg-themes h3 { font-size: 11px; font-weight: 600; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; margin: 8px 0 6px; }
.dlg-tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(120px, 1fr)); gap: 8px; }
.dlg-tile { display: flex; flex-direction: column; gap: 4px; padding: 8px 10px; border: 2px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; text-align: left; }
.dlg-tile[data-theme] { background: var(--color-base-100); color: var(--color-base-content); }
.dlg-tile .swatch { display: block; width: 100%; height: 6px; border-radius: 3px; background: var(--tile-accent, var(--color-primary)); }
.dlg-tile[aria-pressed="true"] { border-color: var(--accent); }
.dlg-tile:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib render::tests -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs static/style.css
git commit -m "ui: the settings dialog shell, its stylesheet, and the structural lock over it"
```

---

### Task 8: Client — theme switching, live autosave, feeding the dialog

**Files:**
- Modify: `static/app.js`

**Interfaces:**
- Produces (globals in the non-module script): `applyTheme(name)`, `appliedTheme` (string), `settingsOpen` (object or null, set by Task 9), `AUTOSAVE` becomes `let`.

- [ ] **Step 1: Write the failing browser assertions.** Create `tests/browser/settings.mjs` with the harness boilerplate and only section A for now (the rest is added in Task 10):

```js
//! The settings dialog: live theme preview, Save/Cancel, both scopes, the
//! rows the snapshot describes, and that a read-only key has no write path.
//! Rust proves the intent, the file and the snapshot; only a browser can
//! prove the cascade repaints and that a second browser follows.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "# global\ntheme = \"dark\"\n");
const projToml = `${fx.dir}/.roost/config.toml`;
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${roost.port}/proj`;

const probe = (evalIn, expr) => evalIn(`(() => { const e = document.createElement("i"); e.style.color = ${JSON.stringify(expr)};
  document.body.appendChild(e); const c = getComputedStyle(e).color; e.remove(); return c; })()`);

let one, two;
try {
  one = await openPage(browser.port, url);
  two = await openPage(browser.port, url);
  for (const p of [one, two]) await until(() => p.evalIn("ctrl && ctrl.readyState === 1 && !!state && !!state.settings"), 30, "app");

  console.log("A. applyTheme switches the cascade in place, both directions");
  const darkBg = await probe(one.evalIn, "var(--bg)");
  ok(darkBg === "rgb(13, 17, 23)", `the page opened on dark.css (${darkBg})`);
  await one.evalIn(`applyTheme("nord"); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", 5, "data-theme"), "a daisyUI name sets data-theme");
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === (await probe(one.evalIn, "var(--color-base-100)")), 10, "bridge"), "and --bg follows nord's base once the bridge loads");
  await one.evalIn(`applyTheme("light"); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === undefined, 5, "no data-theme"), "a roost name removes data-theme");
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(255, 255, 255)", 10, "light"), "and light.css paints");
  ok((await one.evalIn(`document.querySelectorAll('link[href="/static/daisy-bridge.css"]').length`)) === 0, "the bridge link is gone");
  await one.evalIn(`applyTheme("dark"); 0`);
  await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(13, 17, 23)", 10, "back to dark");
} finally {
  try { await one?.close(); } catch {}
  try { await two?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
```

`light.css` defines `--bg: #ffffff`, which is why the assertion expects `rgb(255, 255, 255)`.

- [ ] **Step 2: Run to verify it fails**

Run: `deno run -A tests/browser/settings.mjs`
Expected: the `applyTheme` eval throws "applyTheme is not defined".

- [ ] **Step 3: Implement** in `static/app.js`. Change line 40 to `let AUTOSAVE = document.body.dataset.autosave === "1";`. Add near the theme/notice helpers (before `hookState`):

```js
// ---- themes -------------------------------------------------------------
// The theme the page is currently painted with. Initialised from the first
// snapshot rather than a data- attribute: the snapshot is what a later
// change arrives through, so both readings come from one place.
let appliedTheme = null;
// The settings dialog while it is open (dialog.js sets and clears this).
// While open, a theme in the snapshot is not applied over the preview.
let settingsOpen = null;

// Which of the two mechanisms `render::theme_head` would have used for
// `name`, expressed in the client so a preview matches a reload: a roost
// file is one <link>; a daisyUI name is data-theme on <html> plus the
// vendored variables and the bridge. The vendored file goes FIRST in
// <head>: its `:root` block defines --border as a width, and only a roost
// theme file linked after it wins that back (the bridge does for daisyUI).
function applyTheme(name) {
  const head = document.head;
  const styleLink = head.querySelector('link[href="/static/style.css"]');
  const ensure = (id, href, first) => {
    let l = document.getElementById(id);
    if (!l) {
      l = document.createElement("link");
      l.id = id; l.rel = "stylesheet"; l.href = href;
      if (first) head.insertBefore(l, head.firstChild); else head.insertBefore(l, styleLink);
    }
    return l;
  };
  const drop = (id) => { const l = document.getElementById(id); if (l) l.remove(); };
  const daisy = state && state.settings && state.settings.themes.some((t) => t.name === name && t.kind === "daisy");
  // The server-rendered roost link has no id; adopt it once.
  const rendered = head.querySelector('link[href^="/static/themes/"]');
  if (rendered && !rendered.id) rendered.id = "theme-roost";
  if (daisy) {
    ensure("theme-daisy", "/static/vendor/daisyui-themes.css", true);
    ensure("theme-bridge", "/static/daisy-bridge.css", false);
    drop("theme-roost");
    document.documentElement.dataset.theme = name;
  } else {
    delete document.documentElement.dataset.theme;
    drop("theme-bridge");
    // The vendored variables stay if present: harmless behind a roost file
    // (which wins --border by coming later), and the picker's tiles need it.
    const l = ensure("theme-roost", `/static/themes/${encodeURIComponent(name)}.css`, false);
    l.href = `/static/themes/${encodeURIComponent(name)}.css`;
  }
  appliedTheme = name;
}

// Called on every State: follow a theme change made elsewhere (another
// browser's Save), and keep AUTOSAVE live for this page.
function followSettings() {
  const s = state && state.settings;
  if (!s) return;
  const row = (k) => s.keys.find((r) => r.key === k);
  const theme = row("theme");
  if (theme) {
    if (appliedTheme === null) appliedTheme = theme.effective; // first snapshot: the page is already painted with it
    else if (!settingsOpen && theme.effective !== appliedTheme) applyTheme(theme.effective);
  }
  const auto = row("autosave");
  if (auto) AUTOSAVE = auto.effective === true;
  if (settingsOpen) settingsOpen.onSnapshot(s);
}
```

In `onEvent`'s `case "State":`, after `renderNotices();` add `followSettings();`.

Wire the gear (near the `bell.onclick` block):

```js
const settingsBtn = document.getElementById("settings");
if (settingsBtn) settingsBtn.onclick = () => { if (state && state.settings) openSettings(state.settings); };
```

`openSettings` is defined in Task 9; until then the button is inert but the script loads.

- [ ] **Step 4: Run the test**

Run: `deno run -A tests/browser/settings.mjs`
Expected: section A passes (7 ok).

- [ ] **Step 5: Revert-check.** In `applyTheme`, remove the `drop("theme-bridge")` line — "the bridge link is gone" fails. Restore. Change `first` to `false` for the vendored link — "and light.css paints" fails (the `--border` collision does not show as a colour test; the `--bg` one passes) — so instead assert additionally in section A that `--border` on light is not `1px`:

```js
  ok(!/^\d/.test(await one.evalIn(`getComputedStyle(document.documentElement).getPropertyValue("--border").trim()`)), "--border is a colour under a roost theme with the vendored file loaded");
```

With `first` set to `false` this fails with `"1px"`. Restore. Record both in the test.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/settings.mjs
git commit -m "ui: applyTheme swaps the theme cascade in place; the page follows the snapshot's theme and autosave"
```

---

### Task 9: The dialog itself

**Files:**
- Modify: `static/dialog.js` (new `openSettings`), `static/app.js` (nothing new; it calls `openSettings`)

**Interfaces:**
- Consumes: `runDialog`, `applyTheme`, `appliedTheme`, `settingsOpen`, `send`, `state.settings` shape from Task 2.
- Produces: `openSettings(settings)` → Promise resolving `true` after Save, `false` on Cancel; sets `settingsOpen = { onSnapshot(s) }` while open.

- [ ] **Step 1: Write the failing browser assertions.** Append to `tests/browser/settings.mjs` after section A, inside the `try`:

```js
  console.log("\nB. the gear opens the dialog with the rows the snapshot describes");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  ok(await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog"), "the dialog opened in-page");
  const labels = await one.evalIn(`[...document.querySelectorAll("#dlg-settings .dlg-row label")].map((l) => l.textContent).join(",")`);
  ok(labels === "theme,hide,show_hidden,autosave,share_selection,worktree_prompt,allowed_origins,max_upload_bytes,ide,roots", `rows in the spec's order (${labels})`);
  ok((await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="share_selection"]').classList.contains("disabled")`)), "a global-only row is disabled in Project scope");
  ok((await one.evalIn(`document.querySelectorAll('#dlg-settings .dlg-row[data-key="allowed_origins"] input, #dlg-settings .dlg-row[data-key="allowed_origins"] textarea').length`)) === 0, "a read-only row has no control");
  ok(/global config file/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="allowed_origins"] .hint').textContent`)), "and says to edit the file by hand");
  ok(/from global/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="theme"] .hint').textContent`)), "theme's hint says it comes from global");

  console.log("\nC. preview then Cancel leaves the page as it was");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tile[data-name="nord"]').click(); 0`);
  ok(await until(async () => (await one.evalIn(`document.documentElement.dataset.theme`)) === "nord", 5, "preview"), "clicking a tile previews it");
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click(); 0`);
  ok(await until(async () => (await probe(one.evalIn, "var(--bg)")) === "rgb(13, 17, 23)", 10, "reverted"), "Cancel restores the theme the dialog opened with");
  ok((await one.evalIn(`document.documentElement.dataset.theme`)) === undefined, "and removes data-theme");

  console.log("\nD. preview then Save writes the project file and the other browser follows");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog again");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="theme"]').click(); 0`);
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-tile[data-name="nord"]').click(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => { try { return /theme = "nord"/.test(await Deno.readTextFile(projToml)); } catch { return false; } }, 10, "file"), "the project file holds theme = \"nord\"");
  ok(/# global\ntheme = "dark"/.test(await Deno.readTextFile(globalToml)), "the global file is untouched");
  ok(await until(async () => (await two.evalIn(`document.documentElement.dataset.theme`)) === "nord", 10, "mirror"), "the other browser switched to nord without a reload");
  ok(await until(async () => !(await one.evalIn(`document.getElementById("dlg-settings").open`)), 5, "closed"), "Save closed the dialog");

  console.log("\nE. Clear removes the project key; the hint says the value now comes from global");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  ok(/from project/.test(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="theme"] .hint').textContent`)), "theme's hint now says from project");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="theme"] .clear').click(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => !/theme/.test(await Deno.readTextFile(projToml)), 10, "cleared"), "the key is gone from the project file");
  ok(await until(async () => (await two.evalIn(`document.documentElement.dataset.theme`)) === undefined, 10, "back"), "and both browsers are back on the global dark");

  console.log("\nF. Global scope writes the global file, keeping its comment");
  await one.evalIn(`document.getElementById("settings").click(); 0`);
  await until(() => one.evalIn(`document.getElementById("dlg-settings").open`), 5, "dialog");
  await one.evalIn(`document.querySelector('#dlg-settings .dlg-scope button[data-scope="global"]').click(); 0`);
  ok(!(await one.evalIn(`document.querySelector('#dlg-settings .dlg-row[data-key="worktree_prompt"]').classList.contains("disabled")`)), "worktree_prompt is enabled in Global scope");
  await one.evalIn(`(() => { const c = document.querySelector('#dlg-settings .dlg-row[data-key="worktree_prompt"] input[type="checkbox"]'); c.checked = false; c.dispatchEvent(new Event("change")); })(); 0`);
  await one.evalIn(`document.querySelector("#dlg-settings .dlg-ok").click(); 0`);
  ok(await until(async () => /worktree_prompt = false/.test(await Deno.readTextFile(globalToml)), 10, "global"), "the global file gained worktree_prompt = false");
  ok(/^# global\n/.test(await Deno.readTextFile(globalToml)), "and kept its comment");

  console.log("\nG. a forged write to a read-only key is refused and changes nothing");
  await one.evalIn(`window.__errs = []; const _oe = onEvent; window.onEvent = (ev) => { if (ev.t === "Error") window.__errs.push(ev.msg); return _oe(ev); };
     ctrl.onmessage = (m) => window.onEvent(JSON.parse(m.data));
     send({ t: "SetSetting", scope: "global", key: "allowed_origins", value: ["https://evil.example"] }); 0`);
  ok(await until(async () => Number(await one.evalIn(`window.__errs.length`)) === 1, 5, "error"), "the server answered with an error");
  ok(/allowed_origins/.test(await one.evalIn(`window.__errs[0]`)), "naming the key");
  ok(!/evil/.test(await Deno.readTextFile(globalToml)), "and the file is unchanged");
```

`connectControl` in `app.js` assigns `ctrl.onmessage = (e) => onEvent(JSON.parse(e.data))`, and `onEvent` is a top-level function declaration, so reassigning `window.onEvent` is what the two re-routing lines rely on.

- [ ] **Step 2: Run to verify it fails**

Run: `deno run -A tests/browser/settings.mjs`
Expected: section B's first assertion fails (the dialog never opens; `openSettings` is not defined, the click handler throws).

- [ ] **Step 3: Implement `openSettings`** at the end of `static/dialog.js`:

```js
// The settings dialog. Unlike the ask* shapes it stays open across several
// intents and snapshots, so it keeps its own state: which pane, which
// scope, what has been edited, and the theme the page opened with (for
// Cancel). `runDialog` still owns the modal mechanics. Everything rendered
// here comes from the snapshot through textContent/createElement — a hide
// entry or a root path is text from a config file in a cloned repository.
function openSettings(settings) {
  const el = document.getElementById("dlg-settings");
  const themeBefore = appliedTheme;
  let view = settings;
  let pane = "settings";
  let scope = "project";
  // key → { value, clear } for this scope only; reset when the scope changes.
  let edits = new Map();
  let previewTheme = null;

  const tabs = el.querySelector(".dlg-tabs");
  const scopeBar = el.querySelector(".dlg-scope");
  const rows = el.querySelector(".dlg-rows");
  const themes = el.querySelector(".dlg-themes");
  const okBtn = el.querySelector(".dlg-ok");
  const cancelBtn = el.querySelector(".dlg-cancel");

  const row = (k) => view.keys.find((r) => r.key === k);
  const inScope = (r) => (scope === "project" ? r.project : r.global);
  const fileName = () => (scope === "project" ? view.project_file : view.global_file);

  function renderTabs() {
    tabs.replaceChildren();
    for (const [id, label] of [["settings", "Settings"], ["theme", "Theme"]]) {
      const b = document.createElement("button");
      b.type = "button"; b.className = "dlg-tab"; b.dataset.tab = id; b.textContent = label;
      b.setAttribute("role", "tab"); b.setAttribute("aria-selected", String(pane === id));
      b.onclick = () => { pane = id; render(); };
      tabs.appendChild(b);
    }
  }
  function renderScope() {
    scopeBar.replaceChildren();
    const lab = document.createElement("span"); lab.textContent = "Scope:"; scopeBar.appendChild(lab);
    for (const [id, label] of [["project", "Project"], ["global", "Global"]]) {
      const b = document.createElement("button");
      b.type = "button"; b.dataset.scope = id; b.textContent = label;
      b.setAttribute("aria-pressed", String(scope === id));
      b.onclick = () => { if (scope !== id) { scope = id; edits = new Map(); render(); } };
      scopeBar.appendChild(b);
    }
    const f = document.createElement("span"); f.className = "file"; f.textContent = fileName(); scopeBar.appendChild(f);
  }
  function hintFor(r) {
    if (r.writable.length === 0) return "read-only — edit it by hand in the global config file";
    if (scope === "project" && !r.writable.includes("project")) return "global only";
    const src = r.project !== null ? "from project" : r.global !== null ? "from global" : "default";
    const tail = r.reload ? " · other tabs pick this up on reload" : "";
    return `${src}${tail}`;
  }
  function control(r) {
    const cur = edits.has(r.key) ? edits.get(r.key).value : (inScope(r) ?? r.effective);
    if (r.kind === "bool") {
      const c = document.createElement("input"); c.type = "checkbox"; c.checked = cur === true;
      c.onchange = () => { edits.set(r.key, { value: c.checked, clear: false }); };
      return c;
    }
    if (r.kind === "list") {
      const t = document.createElement("textarea"); t.value = (Array.isArray(cur) ? cur : []).join("\n");
      t.oninput = () => { edits.set(r.key, { value: t.value.split("\n").map((s) => s.trim()).filter(Boolean), clear: false }); };
      return t;
    }
    const i = document.createElement("input"); i.type = "text"; i.value = String(cur ?? "");
    i.oninput = () => { edits.set(r.key, { value: i.value.trim(), clear: false }); };
    return i;
  }
  function renderRows() {
    rows.replaceChildren();
    for (const r of view.keys) {
      const div = document.createElement("div");
      div.className = "dlg-row"; div.dataset.key = r.key;
      const writable = r.writable.includes(scope);
      if (!writable) div.classList.add("disabled");
      const lab = document.createElement("label"); lab.textContent = r.key; div.appendChild(lab);
      if (r.writable.length === 0) {
        const ro = document.createElement("span"); ro.className = "ro";
        ro.textContent = Array.isArray(r.effective) ? r.effective.join(", ") : String(r.effective);
        div.appendChild(ro);
        div.appendChild(document.createElement("span"));
      } else {
        const c = control(r); c.disabled = !writable; div.appendChild(c);
        const side = document.createElement("span");
        if (writable && inScope(r) !== null && !(edits.get(r.key) || {}).clear) {
          const clr = document.createElement("button"); clr.type = "button"; clr.className = "clear"; clr.textContent = "Clear";
          clr.title = `remove ${r.key} from ${fileName()} so the inherited value applies`;
          clr.onclick = () => { edits.set(r.key, { value: null, clear: true }); render(); };
          side.appendChild(clr);
        }
        div.appendChild(side);
      }
      const hint = document.createElement("div"); hint.className = "hint";
      hint.textContent = (edits.get(r.key) || {}).clear ? "will be cleared on Save" : hintFor(r);
      div.appendChild(hint);
      rows.appendChild(div);
    }
  }
  function renderThemes() {
    themes.replaceChildren();
    const current = previewTheme || (row("theme") || {}).effective;
    for (const [kind, title] of [["roost", "roost"], ["daisy", "daisyUI"]]) {
      const h = document.createElement("h3"); h.textContent = title; themes.appendChild(h);
      const grid = document.createElement("div"); grid.className = "dlg-tiles";
      for (const t of view.themes.filter((x) => x.kind === kind)) {
        const b = document.createElement("button");
        b.type = "button"; b.className = "dlg-tile"; b.dataset.name = t.name;
        b.setAttribute("aria-pressed", String(t.name === current));
        if (kind === "daisy") b.dataset.theme = t.name;
        else { b.style.background = t.bg; b.style.color = t.fg; b.style.setProperty("--tile-accent", t.accent); }
        const name = document.createElement("span"); name.textContent = t.name; b.appendChild(name);
        const sw = document.createElement("span"); sw.className = "swatch"; b.appendChild(sw);
        b.onclick = () => { previewTheme = t.name; applyTheme(t.name); edits.set("theme", { value: t.name, clear: false }); renderThemes(); };
        grid.appendChild(b);
      }
      themes.appendChild(grid);
    }
    // daisyUI tiles resolve their colours from the vendored variables, which
    // are only linked when a daisyUI theme is active; make sure they exist.
    if (!document.getElementById("theme-daisy")) {
      const l = document.createElement("link"); l.id = "theme-daisy"; l.rel = "stylesheet"; l.href = "/static/vendor/daisyui-themes.css";
      document.head.insertBefore(l, document.head.firstChild);
    }
  }
  function render() {
    renderTabs(); renderScope();
    rows.hidden = pane !== "settings"; themes.hidden = pane !== "theme";
    if (pane === "settings") renderRows(); else renderThemes();
  }

  return runDialog(el, (finish) => {
    settingsOpen = {
      onSnapshot(s) {
        view = s;
        // Re-render only what is not being typed into: rows keep the
        // person's edits (they live in `edits`, re-applied by control()),
        // and hints/source labels are what a fresh snapshot changes.
        render();
      },
    };
    okBtn.textContent = "Save"; okBtn.disabled = false; okBtn.classList.remove("danger");
    okBtn.onclick = () => {
      for (const [key, e] of edits) {
        const r = row(key);
        if (!r || !r.writable.includes(scope)) continue;
        send({ t: "SetSetting", scope, key, ...(e.clear ? {} : { value: e.value }) });
      }
      // The theme is now what was previewed (or unchanged); the snapshot
      // that follows the write confirms it. Do not revert.
      settingsOpen = null;
      finish(true);
    };
    cancelBtn.onclick = () => { settingsOpen = null; if (previewTheme) applyTheme(themeBefore); finish(false); };
    // Escape and the backdrop go through runDialog's own finish; hook the
    // revert onto the dialog's close so every exit restores the preview.
    el.addEventListener("close", function onClose() {
      el.removeEventListener("close", onClose);
      if (settingsOpen) { settingsOpen = null; if (previewTheme) applyTheme(themeBefore); }
    });
    render();
    return () => tabs.querySelector(".dlg-tab").focus();
  }, false);
}
```

- [ ] **Step 4: Run the test**

Run: `deno run -A tests/browser/settings.mjs`
Expected: sections A–G pass.

- [ ] **Step 5: Revert-checks**, restored and recorded in the test:
  1. In `cancelBtn.onclick`, remove `if (previewTheme) applyTheme(themeBefore);` — section C's "Cancel restores" fails.
  2. In `okBtn.onclick`, change `send(...)` to send only for `scope === "project"` — section F fails.
  3. In `hintFor`, return `"default"` unconditionally — B's and E's hint assertions fail.

- [ ] **Step 6: Commit**

```bash
git add static/dialog.js tests/browser/settings.mjs
git commit -m "ui: the settings dialog — two panes, two scopes, live theme preview kept by Save"
```

---

### Task 10: Docs, the test list, and the full run

**Files:**
- Modify: `docs/deploy.md` (a "Settings dialog" subsection after the Autosave one), `tests/browser/README.md` (the list line), `docs/superpowers/specs/2026-09-05-settings-dialog-design.md` (status line)

- [ ] **Step 1: deploy.md.** After the Autosave subsection add:

```markdown
### The settings dialog

The header's gear opens it. Two panes, **Settings** and **Theme**, and a
scope switch, **Project** (`{project}/.roost/config.toml`) or **Global**
(`~/.config/roost/config.toml`, or `$ROOST_CONFIG`). It writes the display
keys only — `theme`, `hide`, `show_hidden`, `autosave`, and in Global scope
also `share_selection` and `worktree_prompt`. `allowed_origins`,
`max_upload_bytes`, `ide` and `roots` are shown read-only and have no write
path from a page at all: the hub refuses them by name. Writes go through
`toml_edit`, so comments and layout in a hand-edited file survive; a file
that does not parse is refused with its error and left alone. A theme
click previews at once in that browser; Save keeps it and every browser on
the project follows without a reload. A global change reaches other
projects on their next load.
```

- [ ] **Step 2: README.** After the `themes.mjs` line in the run list:

```
deno run -A tests/browser/settings.mjs   # the settings dialog: live theme preview, Save/Cancel, both scopes, read-only keys refused
```

- [ ] **Step 3: Spec status.** Change `*2026-09-05. Status: design, awaiting review.*` to `*2026-09-05. Status: implemented (see the plan of the same date).*`.

- [ ] **Step 4: The full run**

Run: `cargo test -- --test-threads=1` then, one at a time, `deno run -A tests/browser/settings.mjs`, `deno run -A tests/browser/themes.mjs`, `deno run -A tests/browser/dialogs.mjs`, `deno run -A tests/browser/claudehooks.mjs` (the show-hidden/hook rows share the snapshot path).
Expected: all pass. Paste the four summary lines into the commit message.

- [ ] **Step 5: Commit**

```bash
git add docs/deploy.md tests/browser/README.md docs/superpowers/specs/2026-09-05-settings-dialog-design.md
git commit -m "docs: the settings dialog"
```
