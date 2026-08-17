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
    MarkNoticeRead { id: u64 },
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
    fn decodes_the_notice_intents() {
        let i = decode(r#"{"t":"MarkNoticeRead","id":7}"#).unwrap();
        assert!(matches!(i, Intent::MarkNoticeRead { id: 7 }));
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
}
