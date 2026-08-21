//! Workspace persistence. Lives in $RESH_STATE_DIR, never inside a
//! project — following zellij, so pane drags never show up in git status.
use crate::proto::{Sizes, Tab};
use crate::workspace::{Buffer, Content, Pane, Workspace};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tests mutate the process-global RESH_STATE_DIR; cargo runs them in
/// parallel threads. Every test that touches that variable — here and in
/// `hub` — must hold this lock.
///
/// A test needing both this and `session::SESSION_ENV_LOCK` takes **this one
/// first**; see that lock's doc comment for why the order has to be total
/// (an inversion deadlocks, and a deadlock hangs rather than fails, so no
/// number of green runs can rule it out).
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
    /// Absent for a clean buffer: its text is whatever the file says, and
    /// writing it here regardless is how a `.env` opened once ended up in
    /// this file for as long as its tab stayed open (see hub.rs). `default`
    /// so a state file written before this change — where `text` was a bare
    /// string — still loads: serde deserializes a present string value into
    /// `Some`, and a missing key into `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    dirty: bool,
    /// What this buffer's text was based on when it was opened — the one
    /// piece of buffer state that genuinely cannot be recomputed on load.
    /// For a dirty buffer `text` is the user's unsaved edit, so hashing it
    /// invents a base the edit was never made against, and every save then
    /// conflicts against content nobody wrote. That wedges the file for
    /// good: the save never lands, so the buffer stays dirty and is
    /// persisted again next time.
    ///
    /// Trusting a number out of the state file is not the exception to this
    /// module's "recompute derived fields" rule, because it is not derived:
    /// it is a fact about the past, exactly like `text` and `dirty`, which
    /// come from the same file. A wrong value's failure mode is a mismatch,
    /// which is a conflict — the safe direction, and one the banner's
    /// overwrite can be forced past.
    ///
    /// `Option` so every state file written before this existed still loads.
    #[serde(default)]
    base_hash: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct Disk {
    sizes: Sizes,
    panes: Vec<PaneDisk>,
    buffers: std::collections::HashMap<String, BufferDisk>,
    /// Absent in every state file written before the header toggle existed,
    /// and `None` is exactly right for those: they were written by a
    /// workspace that had never expressed an opinion, so it keeps following
    /// the config file. `#[serde(default)]` is belt and braces — serde
    /// already defaults a missing `Option` to `None`, verified by removing
    /// it — and is here so that changing this to a bare `bool` fails loudly
    /// at the test rather than silently rejecting every old file.
    #[serde(default)]
    show_hidden: Option<bool>,
}

/// Honours `RESH_STATE_DIR` for tests and operators who want state
/// elsewhere; otherwise the XDG-ish default, never inside the project tree.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RESH_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state/resh")
}

pub(crate) fn path_for(project: &str) -> PathBuf {
    // storage_key, not the raw project string: a nested project's `/`
    // would otherwise land literally in a filename (or, worse, get
    // interpreted as a directory separator by the OS). See its doc comment
    // for why this isn't `http::percent_encode`.
    state_dir().join(format!("{}.json", crate::projects::storage_key(project)))
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
        show_hidden: w.show_hidden,
        sizes: w.sizes,
        panes: w
            .panes
            .iter()
            .map(|p| PaneDisk { tabs: p.tabs.clone(), active: p.active })
            .collect(),
        buffers: w
            .buffers
            .iter()
            .map(|(k, b)| {
                (
                    k.clone(),
                    BufferDisk {
                        // A clean buffer's text is just the file's own,
                        // redundantly duplicated — and the case named in the
                        // module comment above (and in hub.rs's own comment)
                        // that this stops: a `.env` opened once must not sit
                        // in this file for as long as its tab stays open.
                        text: b.edited_text().map(|t| t.to_string()),
                        dirty: b.dirty(),
                        base_hash: Some(b.base_hash),
                    },
                )
            })
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
            w.show_hidden = d.show_hidden;
            if d.panes.len() == w.panes.len() {
                w.panes = d
                    .panes
                    .into_iter()
                    .map(|p| {
                        // This path assigns tabs verbatim: restored state
                        // never passes through `apply_layout`, so none of the
                        // guards enforced there apply to it. Without this
                        // coercion, a tab left in Edit on a file the editor
                        // now refuses comes back as a textarea whose every
                        // keystroke is rejected — and app.js hides the ✎
                        // toggle that would switch it back, so there is no
                        // way out of it.
                        let tabs = p.tabs.iter().map(crate::workspace::coerce_tab).collect();
                        let active = p.active.min(p.tabs.len().saturating_sub(1));
                        Pane { tabs, active }
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
                            // Falling back to the text's own hash is only
                            // right for a clean buffer, whose text *is* what
                            // was on disk; it is what pre-`base_hash` files
                            // leave us with, and for a dirty one it means a
                            // conflict the user has to force past rather
                            // than a silent overwrite of whatever is there.
                            // A clean buffer no longer carries text at all,
                            // so an absent value falls back to "" — the same
                            // answer a clean buffer's own (redundant) text
                            // would have hashed to.
                            base_hash: b.base_hash.unwrap_or_else(|| {
                                crate::workspace::hash_text(b.text.as_deref().unwrap_or(""))
                            }),
                            // A restored dirty buffer's saved text becomes
                            // its held edit. `dirty: true` with no text is a
                            // corrupt or hand-edited file — trusting it would
                            // write an empty file over the user's own on the
                            // next save — so it loads as clean rather than
                            // as an empty edit; that is also the case for an
                            // ordinary clean buffer, whose text was always
                            // just a redundant copy of the file.
                            content: match (b.dirty, b.text) {
                                (true, Some(t)) => Content::Edited(t),
                                (_, _) => Content::Clean,
                            },
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
        std::env::set_var("RESH_STATE_DIR", d.path());
        let out = f();
        std::env::remove_var("RESH_STATE_DIR");
        out
    }

    // Every state file written before the header toggle existed lacks this
    // key. It must load as None — "this workspace never expressed an
    // opinion" — and keep following the config file, rather than serde
    // refusing the whole file and silently resetting someone's layout.
    #[test]
    fn a_state_file_written_before_the_toggle_still_loads() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            w.show_hidden = Some(true);
            save("legacy_probe", &w).unwrap();
            let text = std::fs::read_to_string(path_for("legacy_probe")).unwrap();
            assert!(text.contains("show_hidden"), "the key must be written at all");

            // Strip it back out, the way a pre-toggle resh would have left it.
            let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
            json.as_object_mut().unwrap().remove("show_hidden");
            std::fs::write(path_for("legacy_probe"), json.to_string()).unwrap();

            let (loaded, warn) = load("legacy_probe");
            assert_eq!(loaded.show_hidden, None, "absent means following the config");
            assert!(warn.is_none(), "an old file is not a corrupt one");
            assert_eq!(loaded.panes.len(), w.panes.len(), "the rest of it still loaded");
        });
    }

    #[test]
    fn the_toggle_round_trips_through_disk() {
        with_state_dir(|| {
            for want in [Some(true), Some(false), None] {
                let mut w = Workspace::default_layout();
                w.show_hidden = want;
                save("roundtrip_probe", &w).unwrap();
                assert_eq!(load("roundtrip_probe").0.show_hidden, want);
            }
        });
    }

    #[test]
    fn the_default_state_dir_is_named_for_the_product() {
        let _g = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_STATE_DIR");
        let d = state_dir();
        assert!(
            d.ends_with(".local/state/resh"),
            "default state dir must follow the product name, got {d:?}"
        );
        assert!(
            !d.to_string_lossy().contains("deadlight"),
            "the old name must not survive in a path users will find on disk: {d:?}"
        );
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
                Buffer { content: Content::Edited("unsaved".into()), ..Buffer::default() },
            );
            save("proj", &w).unwrap();

            let (got, warn) = load("proj");
            assert!(warn.is_none());
            assert_eq!(got.sizes.left_w, 111);
            assert_eq!(got.panes[proto::MIDDLE as usize].tabs.len(), 1);
            assert_eq!(got.buffers["a.rs"].edited_text(), Some("unsaved"), "unsaved text is crash-safe");
            assert!(got.buffers["a.rs"].dirty());
        });
    }

    /// The .env case from hub.rs's own comment: a file opened and never typed
    /// into must leave nothing behind. Searched for as a literal in the whole
    /// serialised file rather than by key, so it fails if the text is stored
    /// anywhere under any name.
    #[test]
    fn a_clean_buffer_puts_no_file_content_in_the_state_file() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            w.buffers.insert(
                ".env".into(),
                Buffer {
                    base_hash: crate::workspace::hash_text("SECRET=hunter2\n"),
                    ..Buffer::default()
                },
            );
            save("proj", &w).unwrap();
            let raw = std::fs::read_to_string(path_for("proj")).unwrap();
            assert!(!raw.contains("hunter2"), "an unedited file's contents must not be persisted: {raw}");
            assert!(raw.contains(".env"), "the buffer itself is still recorded");
            // The two assertions above hold even for the pre-fix shape (a
            // clean `Buffer` never carries text in memory post Task 4, so
            // there is nothing for a naive revert to leak as "hunter2"
            // regardless of the on-disk shape) — confirmed by reverting
            // `BufferDisk.text` to a bare `String` written unconditionally:
            // both assertions above still passed, but the JSON gained a
            // `"text":""` key. This assertion is what actually catches that
            // revert.
            assert!(!raw.contains("\"text\""), "a clean buffer must carry no text key at all: {raw}");
        });
    }

    /// The other direction, and the existing guarantee: unsaved work survives
    /// a restart. This is the assertion that stops the fix above from being
    /// implemented by simply not persisting buffers.
    #[test]
    fn an_edited_buffer_still_round_trips_its_text() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            let mut b = Buffer {
                base_hash: crate::workspace::hash_text("on disk\n"),
                ..Buffer::default()
            };
            b.set_text("unsaved\n".into());
            w.buffers.insert("a.rs".into(), b);
            save("proj", &w).unwrap();
            let (got, _) = load("proj");
            assert_eq!(got.buffers["a.rs"].edited_text(), Some("unsaved\n"));
            assert!(got.buffers["a.rs"].dirty());
        });
    }

    /// A dirty buffer's `base_hash` is the one thing in a state file that
    /// cannot be recomputed from the rest of it: its `text` is the user's
    /// unsaved edit, not what is on disk, so hashing the text manufactures a
    /// base the edit was never made against and `fileops::save` then reports
    /// a conflict against content nobody edited. Hence it is written down.
    ///
    /// Both halves matter. The first says the real base survives; the second
    /// is what fails when it is recomputed from the text instead, which is
    /// exactly the shipped bug.
    #[test]
    fn a_dirty_buffer_keeps_the_base_it_was_edited_from() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            w.buffers.insert(
                "a.rs".into(),
                Buffer {
                    content: Content::Edited("mine".into()),
                    base_hash: crate::workspace::hash_text("on disk"),
                    ..Buffer::default()
                },
            );
            save("base_probe", &w).unwrap();

            let (got, _) = load("base_probe");
            assert_eq!(
                got.buffers["a.rs"].base_hash,
                crate::workspace::hash_text("on disk"),
                "the base a save is checked against must survive the round trip"
            );
            assert_ne!(
                got.buffers["a.rs"].base_hash,
                crate::workspace::hash_text("mine"),
                "hashing the buffer's own text is the bug: it makes every save conflict"
            );
        });
    }

    /// Every state file written before the base was recorded lacks the key,
    /// including the one sitting in a user's state dir at upgrade time. Those
    /// must still load — falling back to the old behaviour, which is right
    /// for a clean buffer (its text *is* the disk) and merely leaves a dirty
    /// one reporting a conflict it can be forced past, as it did before.
    #[test]
    fn a_state_file_written_before_base_hash_still_loads() {
        with_state_dir(|| {
            // Written by hand rather than round-tripped through
            // `save()`. The scenario is a *clean* buffer from a pre-base_hash
            // file — `{"text": "saved", "dirty": false}`, no base_hash key at
            // all — and `Content::Clean` can no longer represent "clean, but
            // holding this text" in memory to hand to `save()`, so the fixture
            // has to be the on-disk bytes an old resh actually wrote instead.
            std::fs::create_dir_all(state_dir()).unwrap();
            let raw = serde_json::json!({
                "sizes": {"left_w": 260, "right_w": 520, "left_split": 60},
                "panes": [
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0},
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0}
                ],
                "buffers": {"a.rs": {"text": "saved", "dirty": false}},
            });
            std::fs::write(state_dir().join("old_base_probe.json"), raw.to_string()).unwrap();

            let (got, warn) = load("old_base_probe");
            assert!(warn.is_none(), "an old file is not a corrupt one");
            // A clean buffer holds nothing of its own; the base_hash fallback
            // below is what actually exercises the behaviour this test covers.
            assert_eq!(got.buffers["a.rs"].edited_text(), None, "dirty:false loads clean");
            assert_eq!(
                got.buffers["a.rs"].base_hash,
                crate::workspace::hash_text("saved"),
                "with nothing recorded, the text is the best available base"
            );
        });
    }

    /// `dirty: true` with the `text` key entirely absent: a corrupt or
    /// hand-edited state file, not a real save (`save()` never omits `text`
    /// for a dirty buffer). The dangerous misreading is `if b.dirty {
    /// Edited(b.text.unwrap_or_default()) }` — the exact file-destroying
    /// shape this branch already shipped once in `wsconn` — which would
    /// hand back `Edited("")` and let the next save overwrite the user's
    /// real file with nothing. Loading this as `Clean` instead means any
    /// later save has to go through the ordinary dirty-buffer path again,
    /// which requires the user to have actually typed something.
    #[test]
    fn dirty_with_no_text_loads_clean_not_as_an_empty_edit() {
        with_state_dir(|| {
            std::fs::create_dir_all(state_dir()).unwrap();
            let raw = serde_json::json!({
                "sizes": {"left_w": 260, "right_w": 520, "left_split": 60},
                "panes": [
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0},
                    {"tabs": [], "active": 0}, {"tabs": [], "active": 0}
                ],
                "buffers": {"a.rs": {"dirty": true}},
            });
            std::fs::write(state_dir().join("dirty_no_text_probe.json"), raw.to_string()).unwrap();

            let (got, _) = load("dirty_no_text_probe");
            assert_eq!(got.buffers["a.rs"].edited_text(), None, "no text to hold an edit");
            assert!(!got.buffers["a.rs"].dirty(), "must come back Clean, so no later save can write from it");
        });
    }

    /// State written before the Edit guards existed (or by an older build, or
    /// by hand) can name a tab this server would now never create. `load`
    /// assigns `p.tabs` verbatim — restored state never passes through
    /// `apply_layout` — so every guard enforced there has to be reapplied
    /// here or the upgrade leaves a textarea over a PNG whose keystrokes are
    /// all refused, with the ✎ toggle that would escape it hidden.
    ///
    /// The `.rs` half is the discriminating half: without it this test passes
    /// with the coercion applied unconditionally, which would silently
    /// downgrade every restored Edit-mode tab in the workspace.
    ///
    /// Confirmed by removing the `coerce_tab` call from `load` and running
    /// this test: it failed with
    /// `left: File { rel: "shot.png", mode: Edit }` /
    /// `right: File { rel: "shot.png", mode: Preview }` — the dead editor
    /// restored intact.
    #[test]
    fn a_restored_tab_gets_the_edit_guards_apply_layout_would_have_applied() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            // Built by hand rather than through apply_layout: the point is
            // state that could only have come from outside those guards.
            w.panes[proto::MIDDLE as usize].tabs = vec![
                Tab::File { rel: "shot.png".into(), mode: Mode::Edit },
                Tab::File { rel: "a.rs".into(), mode: Mode::Edit },
                Tab::File { rel: "logo.svg".into(), mode: Mode::Edit },
            ];
            save("proj", &w).unwrap();

            let (got, _) = load("proj");
            let tabs = &got.panes[proto::MIDDLE as usize].tabs;
            assert_eq!(tabs[0], Tab::File { rel: "shot.png".into(), mode: Mode::Preview });
            assert_eq!(tabs[1], Tab::File { rel: "a.rs".into(), mode: Mode::Edit });
            // SVG is text and stays editable — see NO_TEXT_EDIT_EXT.
            assert_eq!(tabs[2], Tab::File { rel: "logo.svg".into(), mode: Mode::Edit });
        });
    }

    #[test]
    fn nested_project_state_lands_in_an_encoded_filename() {
        with_state_dir(|| {
            save("karpie/src", &Workspace::default_layout()).unwrap();
            // the `/` must not land in the filename literally (it would be
            // parsed as a directory separator) or in a subdirectory (that
            // would need its own creation, which save() never does)
            assert!(state_dir().join("karpie%2Fsrc.json").is_file());
            let (got, warn) = load("karpie/src");
            assert!(warn.is_none());
            assert_eq!(got.sizes.left_w, Workspace::default_layout().sizes.left_w);
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
