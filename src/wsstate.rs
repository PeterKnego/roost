//! Workspace persistence. Lives in $DEADLIGHT_STATE_DIR, never inside a
//! project — following zellij, so pane drags never show up in git status.
use crate::proto::{Sizes, Tab};
use crate::workspace::{Buffer, Pane, Workspace};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tests mutate the process-global DEADLIGHT_STATE_DIR; cargo runs them in
/// parallel threads. Every test that touches that variable — here and in
/// `hub` — must hold this lock.
#[cfg(test)]
pub static STATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// On-disk shape is deliberately narrower than `Workspace`: derived fields
/// (`version`, per-buffer `base_mtime`/`base_hash`/`stale`) are recomputed on
/// load rather than trusted from a file that may be stale or hand-edited.
#[derive(Serialize, Deserialize)]
struct PaneDisk {
    tabs: Vec<Tab>,
    active: usize,
}

#[derive(Serialize, Deserialize)]
struct BufferDisk {
    text: String,
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct Disk {
    sizes: Sizes,
    panes: Vec<PaneDisk>,
    buffers: std::collections::HashMap<String, BufferDisk>,
}

/// Honours `DEADLIGHT_STATE_DIR` for tests and operators who want state
/// elsewhere; otherwise the XDG-ish default, never inside the project tree.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DEADLIGHT_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state/deadlight")
}

fn path_for(project: &str) -> PathBuf {
    state_dir().join(format!("{project}.json"))
}

pub fn save(project: &str, w: &Workspace) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let disk = Disk {
        sizes: w.sizes,
        panes: w
            .panes
            .iter()
            .map(|p| PaneDisk { tabs: p.tabs.clone(), active: p.active })
            .collect(),
        buffers: w
            .buffers
            .iter()
            .map(|(k, b)| (k.clone(), BufferDisk { text: b.text.clone(), dirty: b.dirty }))
            .collect(),
    };
    let json = serde_json::to_string(&disk).map_err(|e| e.to_string())?;
    // Write-then-rename: a crash mid-write leaves the old file intact rather
    // than a half-written one that would report as corrupt next boot.
    let tmp = path_for(project).with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path_for(project)).map_err(|e| e.to_string())
}

/// Never fails outright: a missing file is the normal first-run case, and a
/// corrupt one must not take the workspace down with it — callers get a
/// usable default layout either way, with a warning only when something is
/// actually wrong.
pub fn load(project: &str) -> (Workspace, Option<String>) {
    let mut w = Workspace::default_layout();
    let Ok(text) = std::fs::read_to_string(path_for(project)) else {
        return (w, None); // never saved is normal, not a warning
    };
    match serde_json::from_str::<Disk>(&text) {
        Ok(d) => {
            w.sizes = d.sizes;
            if d.panes.len() == w.panes.len() {
                w.panes = d
                    .panes
                    .into_iter()
                    .map(|p| {
                        let active = p.active.min(p.tabs.len().saturating_sub(1));
                        Pane { tabs: p.tabs, active }
                    })
                    .collect();
            }
            // MAX_BUFFERS is enforced on the live insert path (apply_layout);
            // without a matching cap here, a corrupt or hand-edited state
            // file restores unboundedly. HashMap iteration order is
            // unspecified, so sort the keys before truncating: otherwise
            // which buffers survive a too-big file would differ from one
            // load to the next, which would make this untestable and the
            // dropped-buffer set effectively random.
            let mut keys: Vec<String> = d.buffers.keys().cloned().collect();
            keys.sort();
            let warn = if keys.len() > crate::workspace::MAX_BUFFERS {
                let dropped = keys.len() - crate::workspace::MAX_BUFFERS;
                keys.truncate(crate::workspace::MAX_BUFFERS);
                Some(format!(
                    "workspace state had {} buffers, kept {} (dropped {dropped})",
                    keys.len() + dropped,
                    crate::workspace::MAX_BUFFERS,
                ))
            } else {
                None
            };
            let mut buffers = d.buffers;
            for k in keys {
                if let Some(b) = buffers.remove(&k) {
                    w.buffers.insert(
                        k,
                        Buffer {
                            base_hash: crate::workspace::hash_text(&b.text),
                            text: b.text,
                            dirty: b.dirty,
                            ..Buffer::default()
                        },
                    );
                }
            }
            (w, warn)
        }
        Err(e) => (w, Some(format!("workspace state unreadable: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, Mode};

    fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
        let _g = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        let out = f();
        std::env::remove_var("DEADLIGHT_STATE_DIR");
        out
    }

    #[test]
    fn round_trips_layout_and_buffers() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            w.sizes = Sizes { left_w: 111, right_w: 222, left_split: 33 };
            w.panes[proto::MIDDLE as usize].tabs =
                vec![Tab::File { rel: "a.rs".into(), mode: Mode::Edit }];
            w.buffers.insert(
                "a.rs".into(),
                Buffer { text: "unsaved".into(), dirty: true, ..Buffer::default() },
            );
            save("proj", &w).unwrap();

            let (got, warn) = load("proj");
            assert!(warn.is_none());
            assert_eq!(got.sizes.left_w, 111);
            assert_eq!(got.panes[proto::MIDDLE as usize].tabs.len(), 1);
            assert_eq!(got.buffers["a.rs"].text, "unsaved", "unsaved text is crash-safe");
            assert!(got.buffers["a.rs"].dirty);
        });
    }

    #[test]
    fn missing_file_yields_defaults_without_warning() {
        with_state_dir(|| {
            let (w, warn) = load("never-saved");
            assert!(warn.is_none());
            assert_eq!(w.panes[proto::LEFT_TOP as usize].tabs, vec![Tab::Tree]);
        });
    }

    #[test]
    fn corrupt_file_yields_defaults_with_a_warning() {
        with_state_dir(|| {
            std::fs::create_dir_all(state_dir()).unwrap();
            std::fs::write(state_dir().join("broken.json"), "{ not json").unwrap();
            let (w, warn) = load("broken");
            assert!(warn.is_some(), "corruption must be visible, not silent");
            assert_eq!(w.panes.len(), proto::PANE_COUNT);
        });
    }

    #[test]
    fn out_of_range_active_index_is_clamped_on_load() {
        with_state_dir(|| {
            // Written by hand, not via save(): save() can never produce an
            // out-of-range `active`, so it would not exercise the clamp.
            let raw = r#"{
                "sizes": {"left_w": 260, "right_w": 520, "left_split": 60},
                "panes": [
                    {"tabs": [{"k": "Tree"}], "active": 9},
                    {"tabs": [], "active": 4},
                    {"tabs": [], "active": 0},
                    {"tabs": [], "active": 0}
                ],
                "buffers": {}
            }"#;
            std::fs::create_dir_all(state_dir()).unwrap();
            std::fs::write(state_dir().join("clamp.json"), raw).unwrap();

            let (w, warn) = load("clamp");
            assert!(warn.is_none());
            assert_eq!(
                w.panes[proto::LEFT_TOP as usize].active, 0,
                "active must clamp into a single-tab pane, not stay past the end"
            );
            assert_eq!(
                w.panes[proto::LEFT_BOTTOM as usize].active, 0,
                "empty tabs is where saturating_sub must prevent underflow"
            );
        });
    }

    #[test]
    fn load_caps_restored_buffers_deterministically() {
        // MAX_BUFFERS is enforced going in (apply_layout) but a state file
        // can also arrive corrupted, hand-edited, or from an older binary
        // with a looser cap — load() must not trust it to already be small.
        with_state_dir(|| {
            std::fs::create_dir_all(state_dir()).unwrap();
            let n = crate::workspace::MAX_BUFFERS + 7;
            let mut buffers = serde_json::Map::new();
            for i in 0..n {
                buffers.insert(
                    format!("f{i:03}.txt"),
                    serde_json::json!({"text": "x", "dirty": false}),
                );
            }
            let raw = serde_json::json!({
                "sizes": {"left_w": 260, "right_w": 520, "left_split": 60},
                "panes": [
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0},
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0}
                ],
                "buffers": buffers,
            });
            std::fs::write(state_dir().join("oversized.json"), raw.to_string()).unwrap();

            let (w, warn) = load("oversized");
            assert_eq!(w.buffers.len(), crate::workspace::MAX_BUFFERS, "must be capped on load too");
            assert!(warn.is_some(), "a truncated restore must be visible, not silent");

            // Determinism: sorted-key truncation means the kept set is
            // exactly the first MAX_BUFFERS names in lexical order, every
            // time — not whatever HashMap iteration happened to yield.
            let mut expected: Vec<String> = (0..n).map(|i| format!("f{i:03}.txt")).collect();
            expected.sort();
            expected.truncate(crate::workspace::MAX_BUFFERS);
            let mut got: Vec<String> = w.buffers.keys().cloned().collect();
            got.sort();
            assert_eq!(got, expected);
        });
    }

    #[test]
    fn state_file_is_not_world_readable() {
        with_state_dir(|| {
            save("proj", &Workspace::default_layout()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let m = std::fs::metadata(state_dir().join("proj.json")).unwrap();
                assert_eq!(m.permissions().mode() & 0o077, 0, "buffer text may be secret");
            }
        });
    }
}
