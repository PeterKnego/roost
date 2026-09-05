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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roost_files_win_over_daisyui_names_and_unknown_is_none() {
        // Revert-checked: swapping the two branches in kind() so the daisyUI check runs first
        // causes "dark" to return Some(Daisy) instead of Some(Roost), causing this assertion to fail:
        // assertion `left == right` failed: dark is a roost file even though daisyUI has one
        //   left: Some(Daisy)
        //  right: Some(Roost)
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
