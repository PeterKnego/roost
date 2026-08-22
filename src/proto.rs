//! Wire types for the workspace socket. Intents travel up, events down.
//! Externally tagged on "t" so the JSON reads like the spec's examples.
use serde::{Deserialize, Serialize};

pub type PaneId = u8;
pub const LEFT_TOP: PaneId = 0;
pub const LEFT_BOTTOM: PaneId = 1;
pub const MIDDLE: PaneId = 2;
pub const RIGHT: PaneId = 3;
pub const PANE_COUNT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Preview,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum Tab {
    Tree,
    Changes,
    File { rel: String, mode: Mode },
    Diff { rel: Option<String> },
    Terminal { session: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sizes {
    pub left_w: u32,
    pub right_w: u32,
    pub left_split: u32,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes { left_w: 260, right_w: 520, left_split: 60 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t")]
pub enum Intent {
    OpenTab { pane: PaneId, tab: Tab },
    CloseTab { pane: PaneId, idx: usize },
    ActivateTab { pane: PaneId, idx: usize },
    MoveTab { from: PaneId, idx: usize, to: PaneId, at: usize },
    Resize { sizes: Sizes },
    SetMode { rel: String, mode: Mode },
    EditBuffer { rel: String, text: String },
    SaveBuffer { rel: String, force: bool },
    CloseBuffer { rel: String },
    CreateFile { rel: String },
    CreateDir { rel: String },
    DeleteFile { rel: String },
    RenamePath { from: String, to: String },
    RequestState,
    StartTerminal { session: String },
    /// Ends one session outright — the shell and its dtach master, not just
    /// this browser's view of it. Deliberately *not* a flag on `CloseTab`:
    /// closing a tab rearranges layout and is reversible, while this destroys
    /// a running shell, and the two must not be reachable by the same message
    /// shape. `CloseTab` on a terminal remains the detach path.
    EndSession { session: String },
    /// Opens a terminal on a server-allocated name (`term`, `term1`, …).
    /// Separate from `OpenTab` because only the server can choose safely: a
    /// client sees the sessions it has tabs for, never the detached ones, and
    /// attaching creates only when absent — so a client-chosen name would
    /// eventually drop the user into somebody else's old shell.
    NewTerminal { pane: PaneId },
    /// A span matched in terminal output, sent **verbatim** —
    /// `~/projects/resh/src/a.rs:42` and all. Deliberately not pre-parsed by
    /// the client: the parser and the confinement it feeds belong together, in
    /// Rust, next to `safe_resolve`.
    ///
    /// Separate from `OpenTab` because `OpenTab` validates nothing —
    /// `apply_layout` pushes the tab straight into the layout that is then
    /// broadcast to every connected browser. A path a regex guessed at cannot
    /// be allowed down that road.
    OpenPath { text: String },
    InitGit,
    CloseProject,
    /// Tree visibility for this workspace, overriding the config file's
    /// `show_hidden` for everyone looking at this project. A workspace that
    /// has never been toggled carries `None` and follows the config instead —
    /// see `WorkspaceView::show_hidden`.
    SetShowHidden { on: bool },
    MarkNoticeRead { id: u64 },
    MarkAllNoticesRead,
    ClearNotices,
}

/// Snapshot sent as `Event::State`. Deliberately carries buffer *metadata*
/// only — text moves in `BufferText`, or every keystroke would rebroadcast
/// every open buffer to every client.
#[derive(Debug, Clone, Serialize)]
pub struct BufferMeta {
    pub rel: String,
    pub dirty: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneView {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    pub sizes: Sizes,
    pub panes: Vec<PaneView>,
    pub buffers: Vec<BufferMeta>,
    pub watch_degraded: bool,
    /// Session names currently running for this project. A Terminal tab whose
    /// name is absent renders its start placeholder instead of attaching.
    pub live_sessions: Vec<String>,
    /// Whether the project directory is a git repository — drives the
    /// initialise-git offer on the placeholder.
    pub is_git: bool,
    /// `None` until someone uses the header toggle: the workspace is
    /// following the config file's `show_hidden`. Deliberately sent
    /// unresolved. Resolving it here would mean reading a config file on
    /// every snapshot, and a snapshot goes out on every `EditBuffer` — i.e.
    /// on every debounced keystroke. The page embeds the config value once,
    /// and the client resolves `show_hidden ?? SHOW_HIDDEN_DEFAULT`.
    pub show_hidden: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Event {
    State { version: u64, origin: String, ws: WorkspaceView },
    BufferText { rel: String, text: String, origin: String },
    BufferStale { rel: String },
    SaveOk { rel: String },
    SaveConflict { rel: String, diff_html: String },
    FileChanged { rel: String },
    TreeChanged,
    StatusChanged,
    Error { msg: String },
    /// A terminal link that would not resolve.
    ///
    /// Distinct from `Error` for two reasons: `Error` funnels to the workspace
    /// banner, which is the wrong shape for a link that missed and is already
    /// on the backlog to be redesigned; and it carries no way back to the
    /// terminal that was clicked (see the `Error` case in app.js, which says
    /// exactly that). Sent with `send_to` and never broadcast — one person's
    /// mis-click must not flash every window in the project.
    PathRefused { text: String, msg: String },
    TerminalStarted { session: String },
    GitInit { ok: bool, msg: String },
    CloseRefused { dirty: Vec<String> },
    ProjectClosed { ended: usize },
    /// One live notice. Deliberately not folded into `WorkspaceView`: that
    /// snapshot goes out on every workspace change, and history does not
    /// belong on that path.
    Notice { notice: crate::notify::Notice },
    /// The whole store — every project's notices, not just this client's —
    /// sent on connect and after any read-state change, so no two browsers
    /// disagree about the badge count.
    Notices { list: Vec<crate::notify::Notice> },
}

pub fn decode(s: &str) -> Result<Intent, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

pub fn encode(e: &Event) -> String {
    serde_json::to_string(e).unwrap_or_else(|_| r#"{"t":"Error","msg":"encode failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_open_tab() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"src/main.rs","mode":"Preview"}}"#).unwrap();
        match i {
            Intent::OpenTab { pane, tab } => {
                assert_eq!(pane, 2);
                assert_eq!(tab, Tab::File { rel: "src/main.rs".into(), mode: Mode::Preview });
            }
            other => panic!("wrong intent: {other:?}"),
        }
    }

    #[test]
    fn decodes_move_and_terminal_tabs() {
        let i = decode(r#"{"t":"MoveTab","from":2,"idx":0,"to":3,"at":1}"#).unwrap();
        assert!(matches!(i, Intent::MoveTab { from: 2, idx: 0, to: 3, at: 1 }));
        let i = decode(r#"{"t":"OpenTab","pane":3,"tab":{"k":"Terminal","session":"shell"}}"#).unwrap();
        assert!(matches!(i, Intent::OpenTab { tab: Tab::Terminal { .. }, .. }));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"t":"NoSuchIntent"}"#).is_err());
        assert!(decode(r#"{"t":"MoveTab","from":2}"#).is_err()); // missing fields
        assert!(decode("").is_err());
    }

    #[test]
    fn encodes_events_with_tag() {
        let s = encode(&Event::Error { msg: "bad".into() });
        assert!(s.contains(r#""t":"Error""#));
        assert!(s.contains(r#""msg":"bad""#));
        let s = encode(&Event::TreeChanged);
        assert_eq!(s, r#"{"t":"TreeChanged"}"#);
    }

    #[test]
    fn diff_tab_none_is_the_full_diff_entry() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"Diff","rel":null}}"#).unwrap();
        assert!(matches!(i, Intent::OpenTab { tab: Tab::Diff { rel: None }, .. }));
    }

    #[test]
    fn decodes_the_new_project_intents() {
        assert!(matches!(
            decode(r#"{"t":"StartTerminal","session":"shell"}"#).unwrap(),
            Intent::StartTerminal { .. }
        ));
        assert!(matches!(decode(r#"{"t":"InitGit"}"#).unwrap(), Intent::InitGit));
        assert!(matches!(decode(r#"{"t":"CloseProject"}"#).unwrap(), Intent::CloseProject));
        assert!(matches!(
            decode(r#"{"t":"EndSession","session":"term1"}"#).unwrap(),
            Intent::EndSession { session } if session == "term1"
        ));
        assert!(matches!(
            decode(r#"{"t":"NewTerminal","pane":2}"#).unwrap(),
            Intent::NewTerminal { pane } if pane == 2
        ));
    }

    #[test]
    fn encodes_the_new_project_events() {
        let s = encode(&Event::ProjectClosed { ended: 3 });
        assert!(s.contains(r#""t":"ProjectClosed""#) && s.contains(r#""ended":3"#));
        let s = encode(&Event::CloseRefused { dirty: vec!["a.rs".into()] });
        assert!(s.contains(r#""t":"CloseRefused""#) && s.contains("a.rs"));
        let s = encode(&Event::GitInit { ok: false, msg: "boom".into() });
        assert!(s.contains(r#""ok":false"#) && s.contains("boom"));
    }

    #[test]
    fn decodes_the_notice_intents() {
        let i = decode(r#"{"t":"MarkNoticeRead","id":7}"#).unwrap();
        assert!(matches!(i, Intent::MarkNoticeRead { id: 7 }));
        let i = decode(r#"{"t":"MarkAllNoticesRead"}"#).unwrap();
        assert!(matches!(i, Intent::MarkAllNoticesRead));
        let i = decode(r#"{"t":"ClearNotices"}"#).unwrap();
        assert!(matches!(i, Intent::ClearNotices));
    }

    #[test]
    fn encodes_a_notice_with_its_attribution() {
        let n = crate::notify::Notice {
            id: 3,
            project: "karpie/src".into(),
            session: "claude".into(),
            title: "done".into(),
            body: "42 tests".into(),
            at: 1_700_000_000,
            read: false,
        };
        let s = encode(&Event::Notice { notice: n.clone() });
        assert!(s.contains(r#""t":"Notice""#));
        // Attribution must reach the client, or it cannot route a click.
        assert!(s.contains(r#""project":"karpie/src""#), "got {s}");
        assert!(s.contains(r#""session":"claude""#), "got {s}");
        assert!(s.contains(r#""id":3"#), "got {s}");
        let s = encode(&Event::Notices { list: vec![n] });
        assert!(s.contains(r#""t":"Notices""#));
        assert!(s.contains(r#""list":["#), "got {s}");
    }

    #[test]
    fn decodes_open_path_verbatim() {
        let i = decode(r#"{"t":"OpenPath","text":"~/p/resh/src/a.rs:42"}"#).unwrap();
        // Verbatim is the contract: the client does no parsing, so the suffix
        // and the tilde must both survive the wire intact.
        match i {
            Intent::OpenPath { text } => assert_eq!(text, "~/p/resh/src/a.rs:42"),
            other => panic!("decoded to {other:?}"),
        }
    }

    #[test]
    fn encodes_path_refused_with_both_fields() {
        let s = encode(&Event::PathRefused {
            text: "src/gone.rs".into(),
            msg: "cannot read src/gone.rs".into(),
        });
        // The client matches on `text` to find the terminal that was clicked,
        // so dropping it would leave the message with nowhere to go.
        assert!(s.contains(r#""t":"PathRefused""#), "got {s}");
        assert!(s.contains(r#""text":"src/gone.rs""#), "got {s}");
        assert!(s.contains(r#""msg":"cannot read src/gone.rs""#), "got {s}");
    }
}
