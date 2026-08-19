//! Workspace state and its pure transitions. No I/O lives here — that is
//! exactly what makes the transition table cheap to test.
use crate::proto::{self, Intent, PaneId, Sizes, Tab, PANE_COUNT};
use std::collections::HashMap;
use std::time::SystemTime;

pub const MAX_BUFFERS: usize = 50;

/// Same ceiling as the file read/write path (`projects::MAX_FILE_BYTES`,
/// `fileops::MAX_WRITE_BYTES`): the spec's 2 MB file cap is meant to bound
/// what a client can push into server memory here too, not just what lands
/// on disk — otherwise a single oversized `EditBuffer` frame lands in
/// memory uncapped and then gets re-serialized whole into the state file on
/// every keystroke. Duplicated rather than imported: those two constants
/// are private to their own modules and this crate has no shared "limits"
/// module yet.
pub const MAX_TEXT_BYTES: usize = 2_000_000;

#[derive(Debug, Clone)]
pub struct Buffer {
    pub text: String,
    pub base_mtime: Option<SystemTime>,
    pub base_hash: u64,
    pub dirty: bool,
    pub stale: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer { text: String::new(), base_mtime: None, base_hash: 0, dirty: false, stale: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub version: u64,
    pub sizes: Sizes,
    pub panes: Vec<Pane>,
    pub buffers: HashMap<String, Buffer>,
    pub watch_degraded: bool,
    /// Session names with a live pty attached. Empty on a freshly opened
    /// project — the default layout still lists a Terminal tab, but listing
    /// it is not the same as running it; the client uses this to decide
    /// whether that tab attaches immediately or shows its start placeholder.
    pub live_sessions: Vec<String>,
    /// Whether the project root is a git repository, cached here so the
    /// hub doesn't re-stat the filesystem on every view.
    pub is_git: bool,
}

/// Stable content hash used as the conflict guard. FNV-1a: no dependency,
/// and collision risk on a save-conflict check is not a security boundary.
pub fn hash_text(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl Workspace {
    pub fn default_layout() -> Workspace {
        let mut panes = vec![Pane::default(); PANE_COUNT];
        panes[proto::LEFT_TOP as usize].tabs = vec![Tab::Tree];
        panes[proto::LEFT_BOTTOM as usize].tabs = vec![Tab::Changes];
        panes[proto::RIGHT as usize].tabs = vec![Tab::Terminal { session: "term".into() }];
        Workspace {
            version: 0,
            sizes: Sizes::default(),
            panes,
            buffers: HashMap::new(),
            watch_degraded: false,
            live_sessions: vec![],
            is_git: false,
        }
    }

    pub fn find_tab(&self, want: &Tab) -> Option<(PaneId, usize)> {
        for (pi, p) in self.panes.iter().enumerate() {
            if let Some(ti) = p.tabs.iter().position(|t| tab_identity_eq(t, want)) {
                return Some((pi as PaneId, ti));
            }
        }
        None
    }

    pub fn view(&self) -> proto::WorkspaceView {
        let mut buffers: Vec<proto::BufferMeta> = self
            .buffers
            .iter()
            .map(|(rel, b)| proto::BufferMeta {
                rel: rel.clone(),
                dirty: b.dirty,
                stale: b.stale,
            })
            .collect();
        buffers.sort_by(|a, b| a.rel.cmp(&b.rel)); // deterministic for tests and diffing
        proto::WorkspaceView {
            sizes: self.sizes,
            panes: self
                .panes
                .iter()
                .map(|p| proto::PaneView { tabs: p.tabs.clone(), active: p.active })
                .collect(),
            buffers,
            watch_degraded: self.watch_degraded,
            live_sessions: self.live_sessions.clone(),
            is_git: self.is_git,
        }
    }
}

/// Two tabs are "the same tab" when they address the same thing. A File tab
/// differing only in Mode is still the same file, so switching to Edit must
/// not open a second tab.
fn tab_identity_eq(a: &Tab, b: &Tab) -> bool {
    match (a, b) {
        (Tab::File { rel: x, .. }, Tab::File { rel: y, .. }) => x == y,
        (Tab::Diff { rel: x }, Tab::Diff { rel: y }) => x == y,
        (Tab::Terminal { session: x }, Tab::Terminal { session: y }) => x == y,
        (Tab::Tree, Tab::Tree) | (Tab::Changes, Tab::Changes) => true,
        _ => false,
    }
}

fn pane_mut(w: &mut Workspace, id: PaneId) -> Result<&mut Pane, String> {
    w.panes.get_mut(id as usize).ok_or_else(|| format!("no pane {id}"))
}

/// Apply a pure (no-I/O) intent. `Ok(true)` means state changed and the hub
/// should bump the version and broadcast. I/O intents are the hub's job.
pub fn apply_layout(w: &mut Workspace, intent: &Intent) -> Result<bool, String> {
    match intent {
        Intent::OpenTab { pane, tab } => {
            if let Some((pi, ti)) = w.find_tab(tab) {
                pane_mut(w, pi)?.active = ti;
                return Ok(true);
            }
            let p = pane_mut(w, *pane)?;
            p.tabs.push(tab.clone());
            p.active = p.tabs.len() - 1;
            Ok(true)
        }
        Intent::CloseTab { pane, idx } => {
            let p = pane_mut(w, *pane)?;
            if *idx >= p.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            p.tabs.remove(*idx);
            p.active = p.active.min(p.tabs.len().saturating_sub(1));
            Ok(true)
        }
        // Not addressed by (pane, idx) like CloseTab: a session can be open in
        // several panes at once, and in other browsers' layouts too. Ending the
        // shell must clear every one of those tabs — leaving a stray one behind
        // would offer a click that silently starts a *new* shell under a name
        // the user thought they had just closed.
        Intent::EndSession { session } => {
            let mut changed = false;
            for p in w.panes.iter_mut() {
                let before = p.tabs.len();
                p.tabs.retain(|t| !matches!(t, Tab::Terminal { session: s } if s == session));
                if p.tabs.len() != before {
                    p.active = p.active.min(p.tabs.len().saturating_sub(1));
                    changed = true;
                }
            }
            Ok(changed)
        }
        Intent::ActivateTab { pane, idx } => {
            let p = pane_mut(w, *pane)?;
            if *idx >= p.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            p.active = *idx;
            Ok(true)
        }
        Intent::MoveTab { from, idx, to, at } => {
            let src = pane_mut(w, *from)?;
            if *idx >= src.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            let tab = src.tabs.remove(*idx);
            src.active = src.active.min(src.tabs.len().saturating_sub(1));
            let dst = pane_mut(w, *to)?;
            let at = (*at).min(dst.tabs.len());
            dst.tabs.insert(at, tab);
            dst.active = at;
            Ok(true)
        }
        Intent::Resize { sizes } => {
            w.sizes = *sizes;
            Ok(true)
        }
        Intent::SetMode { rel, mode } => {
            let mut hit = false;
            for p in w.panes.iter_mut() {
                for t in p.tabs.iter_mut() {
                    if let Tab::File { rel: r, mode: m } = t {
                        if r == rel {
                            *m = *mode;
                            hit = true;
                        }
                    }
                }
            }
            if hit {
                Ok(true)
            } else {
                Err(format!("no file tab for {rel}"))
            }
        }
        Intent::EditBuffer { rel, text } => {
            if text.len() > MAX_TEXT_BYTES {
                return Err(format!("buffer too large ({} bytes)", text.len()));
            }
            if !w.buffers.contains_key(rel) && w.buffers.len() >= MAX_BUFFERS {
                return Err("too many open buffers".into());
            }
            let b = w.buffers.entry(rel.clone()).or_default();
            b.text = text.clone();
            b.dirty = true;
            Ok(true)
        }
        Intent::CloseBuffer { rel } => {
            w.buffers.remove(rel);
            Ok(true)
        }
        // I/O intents are dispatched by the hub, not here.
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Mode;

    fn file(rel: &str) -> Tab {
        Tab::File { rel: rel.into(), mode: Mode::Preview }
    }

    #[test]
    fn default_layout_matches_the_spec() {
        let w = Workspace::default_layout();
        assert_eq!(w.panes.len(), PANE_COUNT);
        assert_eq!(w.panes[proto::LEFT_TOP as usize].tabs, vec![Tab::Tree]);
        assert_eq!(w.panes[proto::LEFT_BOTTOM as usize].tabs, vec![Tab::Changes]);
        assert!(w.panes[proto::MIDDLE as usize].tabs.is_empty());
        assert_eq!(
            w.panes[proto::RIGHT as usize].tabs,
            vec![Tab::Terminal { session: "term".into() }]
        );
    }

    /// A session can be open in more than one pane — and in another browser's
    /// layout. Ending the shell must clear every one of those tabs; a survivor
    /// would be a click that silently starts a *new* shell under the name the
    /// user just ended.
    #[test]
    fn ending_a_session_clears_its_tabs_from_every_pane() {
        let mut w = Workspace::default_layout();
        let term = |n: &str| Tab::Terminal { session: n.to_string() };
        // default_layout seeds RIGHT with a Terminal tab of its own; clear it
        // so the only sessions in play are the ones under test.
        for p in w.panes.iter_mut() {
            p.tabs.retain(|t| !matches!(t, Tab::Terminal { .. }));
            p.active = 0;
        }
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: term("term") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: term("term") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: term("term1") }).unwrap();

        let changed = apply_layout(&mut w, &Intent::EndSession { session: "term".into() }).unwrap();

        assert!(changed);
        let live: Vec<&Tab> = w.panes.iter().flat_map(|p| p.tabs.iter()).collect();
        assert!(
            !live.iter().any(|t| matches!(t, Tab::Terminal { session } if session == "term")),
            "no pane may keep a tab for the ended session"
        );
        assert!(
            live.iter().any(|t| matches!(t, Tab::Terminal { session } if session == "term1")),
            "a sibling session must survive"
        );
        for p in &w.panes {
            assert!(p.active < p.tabs.len().max(1), "active index must stay addressable");
        }

        // Nothing to remove is not a change — it must not bump the version.
        assert!(!apply_layout(&mut w, &Intent::EndSession { session: "gone".into() }).unwrap());
    }

    #[test]
    fn open_tab_appends_and_activates() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("b.rs") }).unwrap();
        let p = &w.panes[proto::MIDDLE as usize];
        assert_eq!(p.tabs.len(), 2);
        assert_eq!(p.active, 1);
    }

    #[test]
    fn opening_an_already_open_tab_focuses_it_instead_of_duplicating() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("b.rs") }).unwrap();
        // reopening a.rs, even targeting a different pane, focuses the existing tab
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: file("a.rs") }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].tabs.len(), 2);
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 0);
        assert_eq!(w.panes[proto::RIGHT as usize].tabs.len(), 1); // unchanged
    }

    #[test]
    fn two_terminals_with_different_names_coexist() {
        let mut w = Workspace::default_layout();
        let t = Tab::Terminal { session: "claude".into() };
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: t }).unwrap();
        assert_eq!(w.panes[proto::RIGHT as usize].tabs.len(), 2);
    }

    #[test]
    fn closing_the_active_tab_clamps_the_index() {
        let mut w = Workspace::default_layout();
        for n in ["a.rs", "b.rs", "c.rs"] {
            apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file(n) }).unwrap();
        }
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 2);
        apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 2 }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 1, "active must not dangle past the end");
        apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 0 }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].tabs.len(), 1);
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 0);
    }

    #[test]
    fn move_tab_between_panes_preserves_the_tab() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(
            &mut w,
            &Intent::MoveTab { from: proto::MIDDLE, idx: 0, to: proto::RIGHT, at: 0 },
        )
        .unwrap();
        assert!(w.panes[proto::MIDDLE as usize].tabs.is_empty());
        assert_eq!(w.panes[proto::RIGHT as usize].tabs[0], file("a.rs"));
    }

    #[test]
    fn out_of_range_intents_error_rather_than_panic() {
        let mut w = Workspace::default_layout();
        assert!(apply_layout(&mut w, &Intent::CloseTab { pane: 99, idx: 0 }).is_err());
        assert!(apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 7 }).is_err());
        assert!(apply_layout(
            &mut w,
            &Intent::MoveTab { from: proto::MIDDLE, idx: 0, to: proto::RIGHT, at: 0 }
        )
        .is_err()); // middle is empty
        assert!(apply_layout(&mut w, &Intent::ActivateTab { pane: proto::MIDDLE, idx: 3 }).is_err());
    }

    #[test]
    fn set_mode_rewrites_the_matching_file_tab() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::SetMode { rel: "a.rs".into(), mode: Mode::Edit }).unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "a.rs".into(), mode: Mode::Edit }
        );
    }

    #[test]
    fn edit_buffer_marks_dirty_and_caps_buffer_count() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "hi".into() }).unwrap();
        assert!(w.buffers["a.rs"].dirty);
        assert_eq!(w.buffers["a.rs"].text, "hi");
        for i in 0..MAX_BUFFERS + 5 {
            let _ = apply_layout(
                &mut w,
                &Intent::EditBuffer { rel: format!("f{i}.rs"), text: "x".into() },
            );
        }
        assert!(w.buffers.len() <= MAX_BUFFERS, "buffer count must stay capped");
    }

    #[test]
    fn edit_buffer_rejects_oversize_text() {
        // MAX_BUFFERS caps buffer *count*; this is the separate cap on a
        // single buffer's *size*, matching the 2 MB read/write cap so a
        // huge frame can't land in memory and then get re-serialized whole
        // into the state file.
        let mut w = Workspace::default_layout();
        let huge = "x".repeat(MAX_TEXT_BYTES + 1);
        let err = apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: huge })
            .unwrap_err();
        assert!(err.contains("too large"), "unexpected error: {err}");
        assert!(!w.buffers.contains_key("a.rs"), "a rejected write must not create a buffer");
    }

    #[test]
    fn view_exposes_metadata_without_text() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "secret".into() })
            .unwrap();
        let v = w.view();
        assert_eq!(v.buffers.len(), 1);
        assert_eq!(v.buffers[0].rel, "a.rs");
        assert!(v.buffers[0].dirty);
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("secret"), "State must never carry buffer text");
    }

    #[test]
    fn view_reports_live_sessions_and_git_status() {
        let mut w = Workspace::default_layout();
        w.live_sessions = vec!["shell".into()];
        w.is_git = true;
        let v = w.view();
        assert_eq!(v.live_sessions, vec!["shell".to_string()]);
        assert!(v.is_git);
        // Default is neither: a fresh workspace has spawned nothing.
        let fresh = Workspace::default_layout().view();
        assert!(fresh.live_sessions.is_empty(), "opening a project must not imply a session");
    }
}
