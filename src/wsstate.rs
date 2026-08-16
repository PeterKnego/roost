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
            for (k, b) in d.buffers {
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
            (w, None)
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
