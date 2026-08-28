//! Workspace state and its pure transitions. No I/O lives here — that is
//! exactly what makes the transition table cheap to test.
use crate::proto::{self, Intent, PaneId, Sizes, Tab, PANE_COUNT};
use std::collections::HashMap;

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

/// What a buffer holds, which is *nothing* until the content actually differs
/// from the file.
///
/// An enum rather than `Option<String>` beside a `dirty` flag, because that
/// pair leaves `dirty: true` with no text expressible, and the only reading
/// of that state at save time is "write an empty file over the user's work".
/// Here `dirty` is the discriminant and cannot disagree with what is held.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Clean,
    Edited(String),
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub content: Content,
    /// What the content was based on when the file was opened. The one piece
    /// of a buffer that cannot be rebuilt later: derived at first edit
    /// instead, it would be the hash of whatever is on disk *then*, silently
    /// swallowing the change it exists to detect.
    pub base_hash: u64,
    pub stale: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer { content: Content::Clean, base_hash: 0, stale: false }
    }
}

impl Buffer {
    pub fn dirty(&self) -> bool {
        matches!(self.content, Content::Edited(_))
    }

    pub fn edited_text(&self) -> Option<&str> {
        match &self.content {
            Content::Edited(t) => Some(t),
            Content::Clean => None,
        }
    }

    /// The one place content becomes dirty. Compares against the base rather
    /// than trusting the caller: `pushEdit` in app.js sends the whole text
    /// before every save, including a ⌘S on a file nobody typed into, and
    /// an undone edit arrives the same way.
    pub fn set_text(&mut self, text: String) {
        self.content = if hash_text(&text) == self.base_hash {
            Content::Clean
        } else {
            Content::Edited(text)
        };
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
    /// Tree visibility, `None` while the workspace still follows the config
    /// file's `show_hidden`. Three states on purpose: `Some(false)` is a user
    /// who turned dot entries *off* here, which must survive a global
    /// `show_hidden = true`, and collapsing it into a bare bool would make
    /// that indistinguishable from "never asked".
    pub show_hidden: Option<bool>,
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
            show_hidden: None,
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

    /// Every file a tab currently shows, in either mode.
    ///
    /// Separate from `buffers.keys()` on purpose: the watcher used that, so a
    /// file open in Preview — which has no buffer — was classified as a
    /// generic tree change and its pane never heard that it had changed. A
    /// tab is the thing that means "somebody is looking at this file", which
    /// is the question the watcher is actually asking.
    pub fn open_file_rels(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match t {
                Tab::File { rel, .. } => Some(rel.clone()),
                _ => None,
            })
            .filter(|rel| seen.insert(rel.clone()))
            .collect()
    }

    pub fn view(&self) -> proto::WorkspaceView {
        let mut buffers: Vec<proto::BufferMeta> = self
            .buffers
            .iter()
            .map(|(rel, b)| proto::BufferMeta {
                rel: rel.clone(),
                dirty: b.dirty(),
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
            // Filled by `hub::snapshot_event`; `WsState` deliberately has no
            // such field (see the doc on `WorkspaceView::claude_sessions`).
            claude_sessions: vec![],
            is_git: self.is_git,
            show_hidden: self.show_hidden,
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
        // By id, never by the file it proposes to change: Claude may have two
        // proposals open for the same path, and they are two different
        // questions with two different pending requests behind them.
        (Tab::Proposal { id: x }, Tab::Proposal { id: y }) => x == y,
        (Tab::Tree, Tab::Tree) | (Tab::Changes, Tab::Changes) => true,
        _ => false,
    }
}

fn pane_mut(w: &mut Workspace, id: PaneId) -> Result<&mut Pane, String> {
    w.panes.get_mut(id as usize).ok_or_else(|| format!("no pane {id}"))
}

/// The tab an `OpenTab` actually produces.
///
/// A raw OpenTab (unlike the app.js client, which never asks for Edit here)
/// can name Edit directly, bypassing SetMode's guard below. Coerce rather
/// than reject: the user's intent was "open this file", so they should get it
/// — just in Preview, since `read_text_file` cannot fill a `<textarea>` for a
/// file `refuses_text_edit` names, and a save would truncate it. Coercing also
/// means a stale client's request still does something sensible instead of
/// erroring.
///
/// Public because the hub must dispatch its Edit-mode disk read off the
/// COERCED tab, not off the intent it was handed — otherwise it reads a file
/// this function just forced to Preview.
pub fn coerce_tab(tab: &Tab) -> Tab {
    match tab {
        Tab::File { rel, mode: proto::Mode::Edit } if crate::routes::refuses_text_edit(rel) => {
            Tab::File { rel: rel.clone(), mode: proto::Mode::Preview }
        }
        _ => tab.clone(),
    }
}

/// Tabs whose counterparty did not survive the process that created them.
///
/// A `Tab::Proposal` is one half of a JSON-RPC request Claude is blocked on;
/// the other half is a websocket that died when resh exited. Restoring it
/// from the persisted layout would draw a proposal nobody can answer —
/// Accept and Reject would both find no pending request, and the tab would
/// sit there forever looking live. Dropping it is the honest reload: Claude's
/// own call already failed when its socket closed.
///
/// Takes and returns the workspace by value because the only caller is the
/// load path, which is building one.
pub fn drop_dead_tabs(mut w: Workspace) -> Workspace {
    for p in w.panes.iter_mut() {
        let before = p.tabs.len();
        p.tabs.retain(|t| !matches!(t, Tab::Proposal { .. }));
        if p.tabs.len() != before {
            p.active = p.active.min(p.tabs.len().saturating_sub(1));
        }
    }
    w
}

/// Apply a pure (no-I/O) intent. `Ok(true)` means state changed and the hub
/// should bump the version and broadcast. I/O intents are the hub's job.
pub fn apply_layout(w: &mut Workspace, intent: &Intent) -> Result<bool, String> {
    match intent {
        Intent::OpenTab { pane, tab } => {
            let tab = coerce_tab(tab);
            if let Some((pi, ti)) = w.find_tab(&tab) {
                pane_mut(w, pi)?.active = ti;
                return Ok(true);
            }
            let p = pane_mut(w, *pane)?;
            p.tabs.push(tab);
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
        Intent::SetShowHidden { on } => {
            // Always `Some`, never back to `None`: the header toggle has two
            // positions, and there is no gesture in the UI that means "go
            // back to following the config file". Deleting the state file is
            // that gesture, and it is not one a button should perform.
            w.show_hidden = Some(*on);
            Ok(true)
        }
        Intent::SetMode { rel, mode } => {
            // See the test: Edit over an image is a data-loss path, not a display
            // glitch. app.js hides the toggle; this is what actually stops it.
            if *mode == proto::Mode::Edit && crate::routes::refuses_text_edit(rel) {
                return Ok(false);
            }
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
            // The actual chokepoint: SaveBuffer writes from w.buffers, so if no
            // buffer can ever exist for an image, the truncating save this task
            // exists to prevent is structurally impossible, not merely hard to
            // reach through the UI or through OpenTab's coercion above.
            if crate::routes::refuses_text_edit(rel) {
                return Err(format!("{rel} is an image; edits are refused"));
            }
            if text.len() > MAX_TEXT_BYTES {
                return Err(format!("buffer too large ({} bytes)", text.len()));
            }
            // The cap follows unsaved edits, not how many files have ever
            // been looked at: a buffer created merely by previewing or
            // opening a file (Content::Clean) must never itself count
            // against it or trip it. Mirrored in hub.rs's open_buffer_for.
            if !w.buffers.get(rel).map(|b| b.dirty()).unwrap_or(false)
                && w.buffers.values().filter(|b| b.dirty()).count() >= MAX_BUFFERS
            {
                return Err("too many unsaved files".into());
            }
            let b = w.buffers.entry(rel.clone()).or_default();
            b.set_text(text.clone());
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

    // Three states, and the third one is the point: `Some(false)` is a user
    // who turned dot entries off in a project whose config file turns them
    // on, and it has to be storable as something other than "never asked".
    #[test]
    fn set_show_hidden_records_a_decision_in_both_directions() {
        let mut w = Workspace::default_layout();
        assert_eq!(w.show_hidden, None, "a fresh workspace follows the config file");
        assert_eq!(apply_layout(&mut w, &Intent::SetShowHidden { on: true }), Ok(true));
        assert_eq!(w.show_hidden, Some(true));
        assert_eq!(apply_layout(&mut w, &Intent::SetShowHidden { on: false }), Ok(true));
        assert_eq!(w.show_hidden, Some(false), "off is a decision, not a reset to None");
    }

    // The view is what reaches the browser, and it carries the override
    // unresolved — the page supplies the config default. A view that dropped
    // the field would leave every client showing the config value with the
    // toggle apparently doing nothing.
    #[test]
    fn the_view_carries_the_override_unresolved() {
        let mut w = Workspace::default_layout();
        assert_eq!(w.view().show_hidden, None);
        apply_layout(&mut w, &Intent::SetShowHidden { on: true }).unwrap();
        assert_eq!(w.view().show_hidden, Some(true));
    }

    /// Its counterparty is a socket that died with the process. Restoring the
    /// tab would render a proposal nobody can answer.
    ///
    /// Revert-checked: making `drop_dead_tabs` return `w` untouched failed
    /// only this test and its sibling in `wsstate` — `left: [Proposal {..},
    /// Tree] / right: [Tree]` — then restored.
    #[test]
    fn a_proposal_tab_does_not_survive_a_restart() {
        let mut w = Workspace::default_layout();
        let mid = proto::MIDDLE as usize;
        w.panes[mid].tabs.push(Tab::Proposal { id: "p1".into() });
        w.panes[mid].tabs.push(Tab::Tree);
        w.panes[mid].active = 1;
        let reloaded = drop_dead_tabs(w);
        assert_eq!(reloaded.panes[mid].tabs, vec![Tab::Tree]);
        assert_eq!(reloaded.panes[mid].active, 0, "the active index must follow the tab it named");
        // Every other pane is untouched: dropping a dead tab is not a reset.
        assert_eq!(reloaded.panes[proto::LEFT_TOP as usize].tabs, vec![Tab::Tree]);
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

    /// Edit mounts a <textarea> seeded from `texts`, which the server cannot fill
    /// for a binary file — so an image in Edit shows an empty editor over a real
    /// file, and a save truncates it. app.js hides the toggle; this is the guard,
    /// because the client is not a boundary and another browser may hold a tab
    /// strip rendered before this shipped.
    ///
    /// Confirmed by deleting the guard in the `Intent::SetMode` arm and running
    /// this test: it failed with
    /// `left: File { rel: "shot.png", mode: Edit }` /
    /// `right: File { rel: "shot.png", mode: Preview }` at the "must stay in
    /// Preview" assertion — the image tab really did flip to Edit, exactly the
    /// data-loss path this guard exists to close.
    #[test]
    fn an_image_tab_cannot_be_switched_to_edit() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("shot.png") }).unwrap();
        apply_layout(&mut w, &Intent::SetMode { rel: "shot.png".into(), mode: Mode::Edit }).unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "shot.png".into(), mode: Mode::Preview },
            "an image must stay in Preview"
        );
        // The same intent on a text file must still work, or this test would pass
        // just as well with SetMode broken outright — which is the failure mode
        // `set_mode_rewrites_the_matching_file_tab` would not catch either, since
        // it never opens an image.
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::SetMode { rel: "a.rs".into(), mode: Mode::Edit }).unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[1],
            Tab::File { rel: "a.rs".into(), mode: Mode::Edit }
        );
    }

    /// SVG is on `IMAGE_EXT` but not on `NO_TEXT_EDIT_EXT`, and this test is
    /// what keeps the two lists from being merged back together. An SVG is
    /// text: `read_text_file` finds no NUL bytes in it and has always served
    /// it, so editing one worked before image tabs existed, and gating Edit
    /// on `is_image` silently took that away — leaving a tab whose keystrokes
    /// were all refused and whose ✎ toggle app.js had hidden.
    ///
    /// The `.png` half is not decoration: without it this test passes just as
    /// well with the guard removed altogether.
    ///
    /// Confirmed by pointing all three guards back at `crate::routes::is_image`
    /// and running this test: it failed at the first assertion with
    /// `left: File { rel: "logo.svg", mode: Preview }` /
    /// `right: File { rel: "logo.svg", mode: Edit }` — the SVG refusing to
    /// enter Edit at all.
    #[test]
    fn an_svg_is_text_and_stays_editable() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("logo.svg") })
            .unwrap();
        apply_layout(&mut w, &Intent::SetMode { rel: "logo.svg".into(), mode: Mode::Edit }).unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "logo.svg".into(), mode: Mode::Edit }
        );
        apply_layout(&mut w, &Intent::EditBuffer { rel: "logo.svg".into(), text: "<svg/>".into() })
            .unwrap();
        assert_eq!(w.buffers["logo.svg"].edited_text(), Some("<svg/>"));

        // OpenTab must not coerce it either — the path a raw frame takes.
        apply_layout(
            &mut w,
            &Intent::OpenTab {
                pane: proto::RIGHT,
                tab: Tab::File { rel: "icon.svg".into(), mode: Mode::Edit },
            },
        )
        .unwrap();
        assert_eq!(
            w.panes[proto::RIGHT as usize].tabs.last().unwrap(),
            &Tab::File { rel: "icon.svg".into(), mode: Mode::Edit }
        );

        // A real binary image must still be refused everywhere, or the split
        // above would have been a way to remove the guard rather than narrow it.
        assert!(apply_layout(
            &mut w,
            &Intent::EditBuffer { rel: "shot.png".into(), text: "x".into() }
        )
        .is_err());
    }

    /// A raw websocket frame can name Edit directly in OpenTab, skipping the
    /// ✎ toggle and SetMode's guard entirely — nothing about the wire format
    /// stops `{"t":"OpenTab","tab":{"k":"File","rel":"shot.png","mode":"Edit"}}`.
    /// The tab must still land, because the user asked to open the file and
    /// should get it, but coerced to Preview rather than the empty-textarea
    /// state that would let a save truncate the image.
    ///
    /// Confirmed by deleting the coercion in the `Intent::OpenTab` arm and
    /// running this test: it failed with
    /// `left: File { rel: "shot.png", mode: Edit }` /
    /// `right: File { rel: "shot.png", mode: Preview }` — the raw Edit request
    /// landed in workspace state and would have broadcast to every client.
    #[test]
    fn open_tab_coerces_an_image_requested_in_edit_to_preview() {
        let mut w = Workspace::default_layout();
        apply_layout(
            &mut w,
            &Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "shot.png".into(), mode: Mode::Edit },
            },
        )
        .unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "shot.png".into(), mode: Mode::Preview },
            "an image opened straight into Edit must still land, just coerced"
        );

        // A text file requested in Edit must still open in Edit, or this test
        // would pass just as well with the coercion applied unconditionally —
        // which would silently downgrade every ordinary Edit-mode open.
        apply_layout(
            &mut w,
            &Intent::OpenTab {
                pane: proto::MIDDLE,
                tab: Tab::File { rel: "a.rs".into(), mode: Mode::Edit },
            },
        )
        .unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[1],
            Tab::File { rel: "a.rs".into(), mode: Mode::Edit }
        );
    }

    /// The actual chokepoint: SaveBuffer writes whatever text `w.buffers`
    /// holds, so as long as EditBuffer refuses to create a buffer for an
    /// image, the truncating save is structurally impossible — regardless of
    /// what OpenTab or SetMode let through.
    ///
    /// Confirmed by deleting the `refuses_text_edit` check in the `Intent::EditBuffer`
    /// arm and running this test: it panicked at the `.unwrap_err()` with
    /// `called Result::unwrap_err() on an Ok value: true` — the call
    /// succeeded and created a "shot.png" buffer with the image-truncating
    /// text, exactly what SaveBuffer would then have written over the file.
    #[test]
    fn edit_buffer_refuses_an_image() {
        let mut w = Workspace::default_layout();
        let err = apply_layout(
            &mut w,
            &Intent::EditBuffer { rel: "shot.png".into(), text: "not really a png".into() },
        )
        .unwrap_err();
        assert!(err.contains("shot.png") && err.contains("image"), "unexpected error: {err}");
        assert!(!w.buffers.contains_key("shot.png"), "a refused edit must not create a buffer");

        // A text file must still be editable, or this test would pass just as
        // well with EditBuffer broken outright.
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "hi".into() }).unwrap();
        assert_eq!(w.buffers["a.rs"].edited_text(), Some("hi"));
    }

    #[test]
    fn edit_buffer_marks_dirty_and_caps_buffer_count() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "hi".into() }).unwrap();
        assert!(w.buffers["a.rs"].dirty());
        assert_eq!(w.buffers["a.rs"].edited_text(), Some("hi"));
        for i in 0..MAX_BUFFERS + 5 {
            let _ = apply_layout(
                &mut w,
                &Intent::EditBuffer { rel: format!("f{i}.rs"), text: "x".into() },
            );
        }
        assert!(w.buffers.len() <= MAX_BUFFERS, "buffer count must stay capped");
    }

    /// The cap bounds memory, and memory is text. Fifty open files is
    /// browsing; fifty unsaved edits is the thing worth refusing.
    #[test]
    fn the_cap_counts_edits_not_open_files() {
        let mut w = Workspace::default_layout();
        for i in 0..MAX_BUFFERS + 5 {
            w.buffers.insert(format!("f{i}.rs"), Buffer::default());
        }
        // Clean buffers past the cap are fine — a rel with no existing
        // buffer, not one already in the map, or the old `!contains_key`
        // guard would let this through too and this assertion would prove
        // nothing about the fix.
        assert!(apply_layout(&mut w, &Intent::EditBuffer {
            rel: "new.rs".into(), text: "typed\n".into(),
        }).is_ok());
        // …until that many of them hold edits.
        for i in 1..MAX_BUFFERS {
            w.buffers.get_mut(&format!("f{i}.rs")).unwrap().set_text(format!("edit {i}\n"));
        }
        let err = apply_layout(&mut w, &Intent::EditBuffer {
            rel: format!("f{}.rs", MAX_BUFFERS + 1), text: "one too many\n".into(),
        }).unwrap_err();
        assert!(err.contains("too many"), "got {err}");
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

    #[test]
    fn open_file_rels_lists_previewed_files_not_just_edited_ones() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
        }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab {
            pane: proto::RIGHT,
            tab: Tab::File { rel: "write.rs".into(), mode: Mode::Edit },
        }).unwrap();
        let mut got = w.open_file_rels();
        got.sort();
        // The Preview entry is the whole point: it has no buffer, so the old
        // buffers-only list could not contain it.
        assert_eq!(got, vec!["read.md".to_string(), "write.rs".to_string()]);
        assert!(w.buffers.is_empty(), "no buffer exists yet — that is why this list is needed");
    }

    /// The load-bearing half of the type: a save writes whatever the buffer
    /// holds, so "edited, but holding nothing" must not be expressible. This
    /// asserts the behaviour that replaces it — an edit that matches the base
    /// leaves the buffer clean and holding nothing at all.
    #[test]
    fn an_edit_that_matches_the_base_leaves_the_buffer_clean() {
        let mut w = Workspace::default_layout();
        let base = hash_text("on disk\n");
        w.buffers.insert(
            "a.rs".into(),
            Buffer { base_hash: base, ..Buffer::default() },
        );
        apply_layout(&mut w, &Intent::EditBuffer {
            rel: "a.rs".into(),
            text: "on disk\n".into(),
        }).unwrap();
        let b = &w.buffers["a.rs"];
        assert!(!b.dirty(), "text equal to the base is not an edit");
        assert_eq!(b.edited_text(), None, "and nothing is held for it");
    }

    /// ⌘S on a file that was only looked at goes through pushEdit, which
    /// sends the whole text unconditionally. Without the hash rule above that
    /// would mark the buffer dirty and rewrite the file identically.
    #[test]
    fn a_real_edit_is_held_and_an_undone_one_is_dropped_again() {
        let mut w = Workspace::default_layout();
        let base = hash_text("on disk\n");
        w.buffers.insert("a.rs".into(), Buffer { base_hash: base, ..Buffer::default() });

        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "typed\n".into() }).unwrap();
        assert!(w.buffers["a.rs"].dirty());
        assert_eq!(w.buffers["a.rs"].edited_text(), Some("typed\n"));

        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "on disk\n".into() }).unwrap();
        assert!(!w.buffers["a.rs"].dirty(), "typing a character and deleting it is not an edit");
        assert_eq!(w.buffers["a.rs"].edited_text(), None);
    }
}
