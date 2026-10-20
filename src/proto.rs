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
    Terminal {
        session: String,
        /// The project this terminal's dtach socket actually lives under,
        /// when it differs from the page showing the tab (`home.rs`). Absent
        /// for the overwhelming common case — a native terminal, same as
        /// before this field existed — so an old saved layout, and every
        /// native tab a fresh one writes, round-trips byte-identical to the
        /// pre-re-homing wire shape; see
        /// `a_saved_layout_without_home_still_loads`. Never set by a client:
        /// only `hub::refresh_live_sessions` writes `Some`, the moment it
        /// finds a live resident with no tab yet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
    /// A change Claude has proposed and nobody has answered yet. Deliberately
    /// not `Tab::Diff`, which is a git diff of a tracked path: a proposal
    /// compares disk against content that has never been written, and it is
    /// keyed by a pending request rather than by a file.
    Proposal { id: String },
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

/// A program the client may ask a new terminal to start. A closed set on
/// purpose: the browser already owns a PTY, so this is no new capability, but
/// a message that says "run this command line" should not exist on the wire
/// when one that says "run claude" is all that is wanted. Each variant maps
/// to a fixed command in `launch.rs`; add a variant there to offer another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Launch {
    Claude,
    /// A Claude put on this repository's open pull requests, looping until
    /// they are finished. See `launch::PR_LOOP_PROMPT` for what it is told,
    /// including the bounds that stop it. #52.
    PrLoop,
}

/// What the `+` worktree dialog asks the new worktree's first terminal to
/// start. A sibling of `Launch`, not a reuse of it: `Fork` carries a value
/// (a terminal name, from which the server looks up a recorded Claude
/// session id — never the id itself, same reservation CLAUDE.md states for
/// `NewTerminal.resume`), so this cannot stay a plain wire string the way
/// `Launch` does. Tagged like `Tab` — a `k` field — rather than left
/// externally tagged (serde's default), so `Claude`/`Shell` and `Fork` share
/// one shape (`{"k":"..."}`) instead of one being a bare string and the
/// other an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum WorktreeLaunch {
    /// Continue the Claude running in terminal `session` of *this* project
    /// (not the new worktree) via `claude --resume <id> --fork-session`.
    /// `session` is a terminal name, a hint like `RemoveWorktree`'s own key:
    /// the server re-derives the id from `claudesess::recorded` at the
    /// moment this runs and refuses if nothing is recorded, rather than
    /// trusting a value the dialog could have shown stale.
    Fork { session: String },
    /// Start a fresh Claude in the new worktree.
    Claude,
    /// No program — a bare shell.
    Shell,
}

/// Which config file a `SetSetting` edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Project,
}

/// A config value as the dialog carries it. Untagged: `true`, `15`, `"nord"`
/// and `["dist"]` are unambiguous on the wire and in TOML. `Int` sits before
/// `Str` only for readability — a JSON number never matches a string — and a
/// quoted `"15"` stays a `Str`, which `config::validate` refuses for an
/// integer key rather than writing `code_font_size = "15"` into the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    /// Start the shell for a terminal tab that has none. The placeholder's
    /// two controls, in one message.
    StartTerminal {
        session: String,
        /// Continue the Claude roost recorded for this terminal, instead of
        /// giving it a bare shell (#18 step 2). The id is **not** here on
        /// purpose: the client asks by session name and the server looks it
        /// up, because the id lands on a command line and CLAUDE.md already
        /// reserves that class of choice to the server. `#[serde(default)]`
        /// so a client from before this field still parses.
        #[serde(default)]
        resume: bool,
    },
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
    NewTerminal {
        pane: PaneId,
        /// What to start in the new shell, if anything. Typed into the shell
        /// as keystrokes after it spawns (see `session::attach`), so the
        /// user keeps the shell when the program exits. Absent for the plain
        /// `+` button — the message it has always sent.
        #[serde(default)]
        launch: Option<Launch>,
        /// Skip the "a Claude is already here" prompt. Sent by the prompt's
        /// own "start here anyway" button. Not a boundary — typing `claude`
        /// was always one keystroke away.
        #[serde(default)]
        force: bool,
        /// Continue this past conversation instead of starting a fresh one:
        /// the id of a row in the ✻ menu (`claudehist`). Meaningful only with
        /// `launch: Claude`.
        ///
        /// Unlike `StartTerminal`'s `resume`, this one *is* a value from the
        /// client, because the menu is a list and the choice is which row. So
        /// the server re-derives the authorisation at use — `claudehist::has`,
        /// which validates the id and confirms this project actually has that
        /// conversation. The row that offered the button is a hint, exactly as
        /// `RemoveWorktree` says of its own.
        #[serde(default)]
        resume: Option<String>,
        /// The Claude session id to continue via `claude --resume <id>
        /// --fork-session`, carried as `?fork=` on a worktree page's URL
        /// (R4) rather than typed into it: only the first terminal of a
        /// freshly created worktree ever sets this, and only because the
        /// `+` dialog resolved it server-side before the page existed.
        ///
        /// Checked here against `claudesess::valid_session_id` — a shape
        /// check, not an authorisation. `resume`'s own check
        /// (`claudehist::has`) is different in kind: it asks whether *this
        /// project* has that conversation, because the id there comes from
        /// a menu row naming someone else's past session, and a stale,
        /// forged or cross-project row must not become a `--resume`. `fork`
        /// grants no comparable capability for the same shape check to
        /// withhold — the browser that can set `?fork=<id>` is, by the
        /// Origin check every workspace socket makes, already a page on
        /// this exact project, which can already type anything it likes
        /// into any of its own shells. The check here exists only to keep
        /// an unusable id off a command line (the same reason `Fresh`'s own
        /// minted id is checked too), not to decide whether the id is
        /// "allowed".
        #[serde(default)]
        fork: Option<String>,
    },
    /// A span matched in terminal output, sent **verbatim** —
    /// `~/projects/roost/src/a.rs:42` and all. Deliberately not pre-parsed by
    /// the client: the parser and the confinement it feeds belong together, in
    /// Rust, next to `safe_resolve`.
    ///
    /// Separate from `OpenTab` because `OpenTab` validates nothing —
    /// `apply_layout` pushes the tab straight into the layout that is then
    /// broadcast to every connected browser. A path a regex guessed at cannot
    /// be allowed down that road.
    OpenPath { text: String },
    /// One query from one browser. `seq` is that connection's own monotonic
    /// counter: the worker abandons a query whose seq is no longer the
    /// latest, and the reply carries it back so a late answer to a query the
    /// user has already typed past is dropped rather than rendered.
    ///
    /// Deliberately an intent and not an HTTP route, but equally deliberately
    /// *not* handled like one: `wsconn` diverts it before taking the hub
    /// lock, because a walk is blocking I/O. See the routing there.
    Search { q: String, seq: u64 },
    /// Open `rel` and scroll to `line`. Separate from `OpenTab` because the
    /// line is navigation, not layout: `Tab` is persisted, and a line has no
    /// business surviving a restart. See the plan's "Deviation from the spec".
    OpenAtLine { pane: PaneId, rel: String, line: u32 },
    InitGit,
    CloseProject,
    /// Create a worktree off this project's HEAD, from the `+` dialog (R4).
    /// `name` is optional and checked server-side by `wtname`/`create_named`
    /// — empty means the server generates one with `wtname::generate` — so
    /// nothing a browser typed reaches a path or a command line unvalidated.
    /// `launch` says what the new worktree's first terminal should start;
    /// it is echoed back (as a plain `Launch`, plus a resolved `fork` id for
    /// `WorktreeLaunch::Fork`) in `WorktreeReady` so the new tab knows what
    /// to start, without ever carrying the session id it was asked for —
    /// that is looked up fresh from `claudesess::recorded`.
    NewWorktree {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        launch: Option<WorktreeLaunch>,
    },
    /// Remove a finished worktree of this project. The server re-derives
    /// every "finished" check at this moment; the row that offered the
    /// button is a hint, not an authorisation.
    RemoveWorktree { key: String },
    /// Allow one git worktree outside roost's roots to open under
    /// `<parent>/.claude/outside/<name>` (#188). `parent` is carried
    /// explicitly rather than taken from the sending page: the switcher sits
    /// in every page's header, sibling worktrees' included, so the sender is
    /// not necessarily the parent. Every check — `parent` resolves, git lists
    /// `path` as one of its non-main worktrees — is made against `parent`
    /// itself, server-side, at this moment; the row is a hint.
    AllowOutsideWorktree { parent: String, path: String },
    /// Delete the allow record `name` under `parent`. Revocation touches no
    /// session, layout or `.base`: the page stops resolving and a live
    /// session stays alive.
    RevokeOutsideWorktree { parent: String, name: String },
    /// Drop `path` from this project's appeared list (#188 §3), for every
    /// browser on the project, not just the one that clicked.
    DismissAppeared { path: String },
    /// Tree visibility for this workspace, overriding the config file's
    /// `show_hidden` for everyone looking at this project. A workspace that
    /// has never been toggled carries `None` and follows the config instead —
    /// see `WorkspaceView::show_hidden`.
    SetShowHidden { on: bool },
    /// Enable or disable roost's two Claude Code hooks in this project's
    /// `.claude/settings.local.json`. Applies to the project, not the
    /// connection: every browser on it sees the new state.
    SetClaudeHooks { on: bool },
    /// Enable or disable `claudehooks::Kind::Worktree` (`roost
    /// worktree-hook`, on `WorktreeCreate` only) in this project's
    /// `.claude/settings.local.json`. A sibling of `SetClaudeHooks`, kept
    /// as its own intent rather than a second field on that one because the
    /// two toggle independently — see `Hub::worktree_hooks`.
    SetWorktreeHooks { on: bool },
    /// Write one key into one config file, or clear it (`value: None`) so
    /// inheritance resumes. Validated in `config::validate` before any
    /// file is touched; see the settings-dialog spec.
    SetSetting {
        scope: Scope,
        key: String,
        #[serde(default)]
        value: Option<SettingValue>,
    },
    MarkNoticeRead { id: u64 },
    MarkAllNoticesRead,
    ClearNotices,
    /// A file or selection the user wants Claude to look at. Resolved
    /// server-side and sent as `at_mentioned`, not pasted into a terminal:
    /// a paste lands in whatever state the terminal is in and competes with
    /// whatever Claude is doing at that instant.
    ///
    /// `session` is the terminal the browser had in focus, and it is what
    /// makes this reach one Claude rather than all of them. `None` means the
    /// browser had no terminal tab active — a real answer, not a missing
    /// one, and the server refuses rather than guessing when more than one
    /// Claude is connected. Optional on the wire so an older client's
    /// payload still parses.
    MentionPath {
        rel: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
        #[serde(default)]
        session: Option<String>,
    },
    /// The human's answer to an `openDiff` Claude is still blocked on. This
    /// intent **is** the permission answer, which is why `text` exists: a
    /// proposal the user edited before accepting must travel back as the
    /// content Claude writes, or Claude writes the version the user rejected.
    ///
    /// `accept: false` with a `text` is still a rejection — the text is only
    /// ever read on the accepting path.
    AnswerProposal { id: String, accept: bool, text: Option<String> },
    /// A browser's answer to a `WorktreeAsk` Claude's hook is blocked on.
    /// The first answer from any browser wins; an `id` that is no longer
    /// pending (someone else answered, or it timed out) is ignored.
    WorktreeAnswer { id: String, approve: bool },
    /// "Move tab there" / "Move back" (spec §7, #156 task 12) — the
    /// browser's answer to a `ClaudeElsewhere` offer, never automatic:
    /// `claudes::detect_moves` only ever *offers*, this is the one place
    /// that actually writes or clears the re-home record. `to: Some(url)`
    /// re-homes `session` into the worktree `url` (which must be a
    /// worktree of this project — re-checked server-side, never trusted
    /// from the banner that offered it); `to: None` forgets whatever
    /// record `session` has, moving it back to its home. Only a session
    /// native to this hub, or one of its own residents, can be named —
    /// the same confinement `EndSession`'s `may_end` already applies.
    Rehome { session: String, #[serde(default)] to: Option<String> },
    /// What a revert would do (#125). Answered to the requester only, with a
    /// token over exactly what the dialog will show. `None` is "all".
    /// Diverted in wsconn: a plan is several git calls.
    RevertPreview {
        #[serde(default)]
        rel: Option<String>,
    },
    /// Do it, if nothing changed since the preview: the server rebuilds the
    /// plan and refuses on a different token. `discard_buffers` names the
    /// unsaved roost buffers the dialog said would be discarded; a dirty one
    /// it did not name refuses.
    Revert {
        #[serde(default)]
        rel: Option<String>,
        token: String,
        #[serde(default)]
        discard_buffers: Vec<String>,
    },
    /// Restore this project from an archive the browser has already uploaded
    /// through `POST /upload` (#18 step 3).
    ///
    /// Deliberately **not** a third POST endpoint. CLAUDE.md caps the HTTP
    /// surface at two and then says what the alternative is: "Every other
    /// state change is a websocket intent." A restore is a state change; what
    /// it needs is a *body delivered*, and roost already has exactly one
    /// audited way to deliver one. Coming in here rather than over HTTP also
    /// puts it behind the handshake's `Origin` check, which is a stronger gate
    /// than a POST's — a `multipart/form-data` POST is a CORS simple request
    /// any page can submit cross-origin with no preflight, while a handshake
    /// from that page is refused before the first frame.
    ///
    /// `file` is project-relative and confined by `projects::safe_resolve`,
    /// like every other `rel` on this enum. Nothing *inside* the archive names
    /// a path at all — see `backup`'s module doc.
    RestoreWorkspace {
        file: String,
        /// Describe the restore without performing it. The dialog always
        /// sends this first: #18 calls restore the most destructive operation
        /// roost would have, and the failure worth catching — a transcript
        /// directory re-derived somewhere unexpected — is visible in a listing
        /// and invisible in a success message.
        #[serde(default)]
        dry_run: bool,
    },
    /// Download, verify, probe, swap and re-exec the newer release the
    /// version check found. Diverted in `wsconn` before the hub lock, like
    /// `Search`; refused for every shape but the two roost may replace.
    Update,
    /// `Later`: the dialog stops opening by itself for a day. The mark stays.
    DeferUpdate,
    /// `Skip <version>`: neither mark nor dialog for that version again. It
    /// carries the version so a click from a stale page cannot skip one it
    /// never saw. Both are global, not per project — a version is not about
    /// any project — and both push a fresh settings snapshot to every hub.
    SkipUpdate { version: String },
    /// The editor's current selection, sent as ambient context on a debounce
    /// from `static/app.js` — not a deliberate gesture like `MentionPath`'s
    /// Alt+K. `rel` is resolved and confined server-side exactly like
    /// `MentionPath`'s, never trusted as an absolute path; `text` is the
    /// selection's own content, which is the reason this whole intent exists
    /// behind `Settings::share_selection` — everything else on this enum
    /// either moves data Claude already had or data the user explicitly
    /// pointed at, and this ships file contents with neither.
    ShareSelection {
        rel: String,
        text: String,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    },
    /// This page's roost extension content script answered (`ready`), or
    /// its window took focus (`active`), or the script was orphaned by an
    /// extension reload (`ready: false`). See `browser.rs`.
    BridgeHello {
        #[serde(default)]
        active: bool,
        #[serde(default = "yes")]
        ready: bool,
    },
    /// The extension's content script in this page has a `BridgeCall` —
    /// proof the page is alive, sent before the call is acted on.
    BridgeAck { call: String },
    /// The extension's answer to a `BridgeCall` this connection relayed.
    /// Diverted in `wsconn` before the hub lock: it is a map lookup and a
    /// channel send, and nothing about it belongs to this project's hub.
    BridgeReply {
        call: String,
        ok: bool,
        #[serde(default)]
        result: serde_json::Value,
        #[serde(default)]
        error: String,
    },
    /// A message typed (by a person or a browser agent) into the handoff page.
    HandoffPost { text: String },
    /// The handoff page asking for the whole log, on load and on reconnect.
    HandoffSync,
}

fn yes() -> bool {
    true
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
    /// One sentence on what the key does, shown under its name. Lives here,
    /// not in the client, so the dialog and any future doc read one source.
    pub doc: &'static str,
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

/// What roost knows about newer versions of itself.
///
/// A **sibling** of `SettingsView::build`, not a field of `BuildInfo`: that
/// struct's doc comment promises it is constant for the life of the process,
/// and this is the one server fact on the About panel that is not.
///
/// `status` is one of `off`, `never`, `unknown`, `up-to-date`, `newer`. Five
/// values where `version::Latest` has three, because two of them — no state
/// file at all, and the setting turned off — are facts the *state file* knows
/// rather than outcomes the comparator produces. About is exactly where
/// someone goes to wonder about either, and both are assertable in the browser
/// test, which the silent alternatives were not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct UpdateView {
    pub status: String,
    /// The newest unyanked version the last successful check saw, or empty.
    /// Carried whole rather than folded into `status` because #65's step 4
    /// names it in a dialog title and in a skipped-version record.
    pub latest: String,
    /// Whether this copy gets the `[Update]` button: channel `release`,
    /// owner neither Homebrew nor a system package, write probe yes, **and**
    /// a public key compiled in. Decided here so the client never
    /// re-derives it.
    pub offer: bool,
    /// The version the user skipped, or empty. A newer `latest` un-skips.
    pub skipped: String,
    /// When `Later` expires, or 0 when not deferred. The mark stays either way.
    pub deferred_until: u64,
    /// `"<phase>: <message>"` from the last attempt at *this* `latest`, or
    /// empty. Not persisted: an exec is the success case.
    pub failure: String,
    /// The version whose file is in place but whose exec failed, or empty —
    /// the one state where the file and the display disagree, reported.
    pub installed: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct SettingsView {
    pub keys: Vec<SettingRow>,
    pub themes: Vec<ThemeEntry>,
    pub project_file: String,
    pub global_file: String,
    /// `Settings::warning` — a config file that did not parse, named.
    pub warning: Option<String>,
    /// What this binary is, for the About panel. Constant for the life of the
    /// process, and carried here rather than fetched separately because it is
    /// a server fact and this snapshot is already how server facts reach the
    /// dialog.
    pub build: BuildInfo,
    /// See `UpdateView`. Beside `build`, never inside it.
    pub update: UpdateView,
}

/// The identity of the running binary.
///
/// Every field can be the string `unknown`, and that is a real answer rather
/// than a gap to paper over: a release tarball has no `.git`, and a build box
/// may have no `git`. See `build.rs::git_hash` — a display that can be quietly
/// wrong is worse than one that says it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct BuildInfo {
    pub version: String,
    /// Short commit, with `-dirty` when the tree had uncommitted changes and a
    /// trailing `?` when `git status` itself could not be trusted.
    pub commit: String,
    /// Unix seconds. Formatted by the client, which knows the reader's
    /// timezone; 0 means unknown.
    pub built_epoch: u64,
    pub repository: String,
    /// How this binary was produced: `release`, `cargo`, `checkout` or
    /// `unknown`. Baked at build time, and deliberately no finer — Homebrew,
    /// the `.deb`, the `.rpm` and the tarball are the same bytes, so a
    /// compiled-in value cannot separate them (see `install.rs`).
    pub channel: String,
    /// Whether this process could replace its own executable: `yes`, `no` or
    /// `unknown`. Established by writing a probe file, not by reading
    /// permission bits, and three-valued because "could not find out" is not
    /// "no" — this is what decides whether an update button can work at all.
    pub replaceable: String,
    /// Best-effort guess at what manages this binary — `homebrew`,
    /// `system-package`, `cargo-bin`, `other`, `unknown`. Cosmetic: it words a
    /// suggested command and nothing acts on it, so a wrong guess costs a
    /// wrong sentence rather than a wrong operation.
    pub owner: String,
    /// The target triple this binary was built for, e.g.
    /// `x86_64-unknown-linux-musl`. Names the release tarball, so the About
    /// pane can offer the exact download rather than a page to browse — and
    /// on macOS a `curl` line is the fix for the quarantine hang, since only a
    /// browser sets the attribute.
    pub target: String,
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
    /// Terminal tabs the restored layout asked for whose shell is positively
    /// gone — a reboot, or a reaped socket. The placeholder says so, instead
    /// of rendering identically to a tab that never had a shell. Only ever
    /// populated from positive evidence; see `hub::lost_terminal_sessions`.
    pub lost_sessions: Vec<String>,
    /// Of the lost ones, those with a recorded Claude session the placeholder
    /// can offer to resume (#18 step 2). Names only: the id lands on a command
    /// line, so it stays server-side and the browser asks for the resume by
    /// session name (`Intent::StartTerminal`'s `resume`) rather than by value.
    ///
    /// Absence means *no record*, which is not *no Claude ran here* — hooks
    /// are per-project and opt-in, so a hand-typed `claude` in a project with
    /// the bell off records nothing. Nothing may render this as a claim that
    /// the tab had no Claude; see `claudesess`'s module doc.
    pub resumable_sessions: Vec<String>,
    /// Of those, the ones running a Claude — their tabs take the Claude mark
    /// in place of the terminal glyph. Derived in `hub::snapshot_event` from
    /// `claudes::cached_sessions`, never stored in the workspace: it is a
    /// fact about the host's processes, not about the layout.
    pub claude_sessions: Vec<String>,
    /// Resident name → its home, for every terminal re-homed into this
    /// project (`home.rs`). Filled in `hub::snapshot_event` from
    /// `Hub.session_homes`, the same way `claude_sessions` is derived there —
    /// never kept in `WsState`, since it is rebuilt from the live records and
    /// sockets on every refresh and a saved copy could only ever be stale.
    /// Each resident's own `Tab::Terminal.home` already carries the same
    /// fact for the client's own use (dialing the terminal and paste sockets
    /// at the session's real home); this is the same information keyed for
    /// a consumer that has a session name but no tab to read it off, such as
    /// a future "ending a resident" confirmation.
    #[serde(default)]
    pub session_homes: std::collections::BTreeMap<String, String>,
    /// Native live terminals with a recorded Claude session id
    /// (`claudesess::recorded`) — the `+` dialog's fork choices (#156).
    /// Filled in `hub::snapshot_event` from `Hub.forkable`, which
    /// `refresh_live_sessions` rebuilds the same way it rebuilds
    /// `session_homes`, for the same reason: never kept in `WsState`, and
    /// never read on every snapshot (`EditBuffer` debounces onto this path),
    /// since a recorded session is a small file read per candidate.
    #[serde(default)]
    pub forkable: Vec<String>,
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
    /// Whether roost's Claude hooks are installed in the project's local
    /// Claude settings: `Some(true)` present, `Some(false)` absent, `None`
    /// when the file cannot be read or parsed. Derived in
    /// `hub::snapshot_event` from `Hub::claude_hooks`, a cache read when it
    /// is invalidated (a write through the toggle, an explicit refresh, or a
    /// new connection — see that field's doc comment), not on every
    /// snapshot: a snapshot goes out on every `EditBuffer`, i.e. every
    /// debounced keystroke, and reading a file that often under the hub lock
    /// is exactly the cost `show_hidden`'s comment above refuses for the
    /// same reason. The file is still the truth; a hand edit shows at the
    /// next invalidation, not necessarily the next snapshot.
    pub claude_hooks: Option<bool>,
    /// Same shape and reasoning as `claude_hooks` immediately above, for
    /// `claudehooks::Kind::Worktree` instead: `Some(true)` present,
    /// `Some(false)` absent, `None` unreadable/unparseable. Derived from
    /// `Hub::worktree_hooks`, invalidated at the same three sites.
    pub worktree_hooks: Option<bool>,
    /// Filled by `hub::snapshot_event` from a fresh `config::settings_view`;
    /// never persisted (it is a read of the files, not workspace state).
    #[serde(default)]
    pub settings: SettingsView,
    /// Worktrees that appeared and are not yet dealt with (`appeared.rs`),
    /// oldest first. Filled by `hub::snapshot_event` from the hub's
    /// in-memory copy of the persisted list, so a page that connects late,
    /// or after a restart, still sees every banner.
    #[serde(default)]
    pub appeared: Vec<AppearedView>,
}

/// One entry of `WorkspaceView::appeared`, resolved for display. Built
/// fork-free under the hub lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppearedView {
    pub path: String,
    pub branch: String,
    /// The page to link to: the project under a root, or the virtual
    /// outside URL of an allowed one. `None` when there is nowhere to go.
    pub url: Option<String>,
    /// No root contains `path`.
    pub outside: bool,
    /// An allow record names `path` and still resolves to it.
    pub allowed: bool,
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
    /// A terminal link that `resolve_terminal_path` refused, but that
    /// `projects::project_for_abs` could place inside a *different* roost
    /// project (#158 §3) — `project` is that project's url, `rel` the
    /// file's path inside it, `line` the `:line` suffix the terminal text
    /// carried, if any. Sent with `send_to` and never broadcast, for the
    /// same reason `PathRefused` is: one terminal's click is not every
    /// browser's business. The client renders "That's in `<project>` —
    /// open", with the link navigating to the other project's page and
    /// nothing opening here — the new page's own server confines the
    /// `OpenPath` it sends once `?open=` is read there.
    PathElsewhere { project: String, rel: String, line: Option<u32> },
    /// Results for one query. Sent with `send_to` and never broadcast: a
    /// query is one browser's business, and the overlay that asked is
    /// client-local by design.
    SearchResults { seq: u64, results: crate::search::Results },
    /// Scroll whichever pane holds `rel` to `line`. Broadcast rather than
    /// sent to the asker, so a second browser mirroring the tab follows it
    /// there — but carried as an event rather than as workspace state, so
    /// nothing about it persists.
    RevealLine { rel: String, line: u32 },
    TerminalStarted { session: String },
    /// A ✻ click in a project roost has positive evidence a Claude is
    /// already running in. Sent to the clicker only; nothing was opened.
    ClaudeHere { pane: PaneId, terminals: Vec<String> },
    GitInit { ok: bool, msg: String },
    CloseRefused { dirty: Vec<String> },
    ProjectClosed { ended: usize },
    /// The worktree exists and is registered. Sent to the clicker only —
    /// it is the one holding the tab to navigate. `fork`, present only for
    /// `WorktreeLaunch::Fork`, is the Claude session id `claudesess::recorded`
    /// resolved — never the typed-at keystrokes, and never the terminal name
    /// the dialog offered: R4 puts the session id on the wire and nothing
    /// else, so the new page's `?fork=` carries a value the server will
    /// re-validate (`claudesess::valid_session_id`) before it ever reaches a
    /// command line.
    WorktreeReady { url: String, launch: Option<Launch>, fork: Option<String> },
    /// The allow record is written; `url` is the virtual
    /// `<parent>/.claude/outside/<name>` it now resolves under. Sent to the
    /// clicker only — it is the one that asked and may navigate; every other
    /// browser learns of the change through the refreshed snapshot and
    /// `ProjectsChanged`.
    OutsideAllowed { url: String },
    /// The allow record `parent`/`name` is deleted. Sent to the clicker
    /// only, after every switcher has heard `ProjectsChanged`: it is the
    /// one event that means *this* revoke was handled, where a
    /// `ProjectsChanged { parent }` can also come from a terminal spawn, a
    /// prune, or a cold hub's seed, and waiting on that hid a later
    /// `Error` (#188 final review).
    OutsideRevoked { parent: String, name: String },
    /// Claude in `session` asked for worktree `worktree-<name>` through the
    /// hook and is blocked until a browser answers with `WorktreeAnswer`.
    /// Broadcast to every browser of the project; `id` is a server counter,
    /// never derived from the request.
    WorktreeAsk { id: String, session: String, name: String },
    /// The ask `id` is settled — answered by some browser, or timed out —
    /// so every other browser's dialog for it closes.
    WorktreeAskClosed { id: String },
    /// The set of projects with live sessions changed — `project` started
    /// its first shell, ended its last, or was closed. Machine-wide, sent to
    /// every project's clients via `hub::broadcast_all` — unlike `Notice`,
    /// which is scoped to its own project — because the ◆ "running
    /// projects" panel is shown in every workspace and is
    /// otherwise refetched only by the tab that caused the change. Carries no
    /// roster: the client refetches the server-rendered fragment, which is
    /// the one place the count is computed (`registry::known_projects`).
    ProjectsChanged { project: String },
    /// One live notice, sent only to the clients of the project it belongs
    /// to (`hub::broadcast_to_project`). The store behind it is still
    /// machine-wide; the delivery is not, because a browser tab is opened on
    /// exactly one project key — and a worktree is a key of its own — so a
    /// foreign notice has no tab to focus and no honest place in the panel.
    ///
    /// Deliberately not folded into `WorkspaceView`: that snapshot goes out
    /// on every workspace change, and history does not belong on that path.
    Notice { notice: crate::notify::Notice },
    /// The two sides of a proposal tab, sent when the tab opens rather than
    /// carried in `WorkspaceView`: that snapshot goes out on every workspace
    /// change (every debounced keystroke, via `EditBuffer`), and two whole
    /// file bodies have no business on that path.
    Proposal { id: String, rel: String, old_text: String, new_text: String },
    /// What a restore would do, or has just done (#18 step 3).
    ///
    /// Sent with `send_to` and never broadcast: the archive was uploaded by
    /// one person, on one connection, and a second browser on the project has
    /// no context for a listing of paths it did not ask for. The workspace
    /// change a real restore causes reaches everyone the way every other one
    /// does — through the state snapshot.
    ///
    /// `refused` and an empty `lines` is a real combination and the renderer
    /// must show it: it is the whole of what the user is told when an archive
    /// is rejected before a single file is touched.
    RestoreReport { dry_run: bool, lines: Vec<String>, refused: Option<String> },
    /// The update pipeline's progress, to the connection that asked and
    /// nobody else. `phase` is `download`, `verify`, `unpack`, `probe`,
    /// `swap`, `restarting`, `failed` or `refused`; `detail` carries the
    /// message for the last two and the version for `restarting`.
    UpdateProgress { phase: String, detail: String },
    /// This project's notices — not the whole store — sent on connect and
    /// after any read-state change, so no two browsers on the same project
    /// disagree about the badge count.
    Notices { list: Vec<crate::notify::Notice> },
    RevertPlan {
        rel: Option<String>,
        paths: Vec<crate::revert::PlanPath>,
        staged: bool,
        skipped: crate::revert::Skipped,
        dirty: Vec<String>,
        /// Server-rendered and escaped: a diff (one file) or a path list.
        detail_html: String,
        token: String,
    },
    /// `stale`: refused because what the user saw no longer holds; the client
    /// re-asks for a preview rather than leaving them with a dead dialog.
    Reverted { rel: Option<String>, ok: bool, msg: String, stale: bool },
    /// A terminal this browser held a native tab for has just been re-homed
    /// into `owner` — `hub::refresh_live_sessions` found the tab here no
    /// longer resolving to this project and dropped it. Sent only to this
    /// (the home's) browsers, never to the owner's: the owner's own refresh
    /// is what gives the tab a home there, carried in the ordinary `State`
    /// snapshot, and a second "it arrived" announcement on top of that would
    /// be the same fact said twice from two different events. Client-side
    /// this is a transient banner and a link to `owner`, not workspace
    /// state — nothing here is persisted.
    Rehomed { session: String, owner: String },
    /// A Claude's cwd moving somewhere roost was not asked about (spec §7,
    /// `claudes::detect_moves`). `worktree: Some(url)` means a native
    /// session's Claude is now sitting inside that worktree of this
    /// project — the client offers "Move tab there". `worktree: None`
    /// means a resident's Claude has cd'd back out to its home's main
    /// checkout — the client offers "Move back". Either way this is a
    /// dismissible banner, never a workspace change by itself: nothing
    /// here is persisted, and no record changes until `Intent::Rehome`
    /// answers it.
    ClaudeElsewhere { session: String, worktree: Option<String> },
    /// Claude Code removed a resident's worktree when its session exited
    /// (spec §7's "A worktree that Claude Code removed"). Sent to the
    /// *home's* browsers once per (home, session) per process lifetime
    /// (`claudes::sweep_gone_owners`'s `announced` set) — the owner's own
    /// tab already lost its terminal the moment `resolves` went false, and
    /// R18 has by now also forgotten the re-home record, so the session is
    /// `Here` again. This event is only the explanation for why.
    WorktreeGone { worktree: String, session: String },
    /// A worktree of this checkout that roost did not make has appeared —
    /// `git worktree add` from any tool, at any path (#158 §2). Broadcast to
    /// this project's browsers once per worktree; one that already existed
    /// when the watcher started, or that roost itself is making, never sends
    /// this. `url` is the worktree's roost project when its directory is one
    /// under a root, `None` when it lies outside every root (the banner then
    /// has nothing to link to).
    WorktreeAppeared { path: String, branch: String, url: Option<String> },
    /// A project or worktree directory was renamed, and its live terminals
    /// followed it (#158 §5): `from` is the old URL, `to` the new one. Sent
    /// to both projects' browsers once per rename — the old page gets a link
    /// to the new one, the new page the explanation for its new tabs.
    ProjectMoved { from: String, to: String },
    /// A browser tool call for this page to hand to the extension, unread.
    /// `payload` is opaque JSON text and `sig` its HMAC (`browser::sign`):
    /// the extension verifies the bytes it is given, so this page — which
    /// relays it and could be running injected script — cannot invent one.
    /// Sent to one connection, never broadcast.
    BridgeCall { call: String, payload: String, sig: String },
    /// Handoff messages: the whole log when `full` (an answer to
    /// `HandoffSync`), else new ones for every client of the project.
    Handoff { messages: Vec<crate::handoff::Message>, full: bool },
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
    fn new_terminal_without_a_launch_is_the_plain_plus_button() {
        // The `+` button predates `launch`; its message must keep parsing
        // unchanged, and parse to "no launch", not to some default program.
        let i = decode(r#"{"t":"NewTerminal","pane":3}"#).unwrap();
        assert!(matches!(i, Intent::NewTerminal { pane: 3, launch: None, .. }), "got {i:?}");
    }

    #[test]
    fn new_terminal_can_ask_for_claude_by_name() {
        let i = decode(r#"{"t":"NewTerminal","pane":3,"launch":"claude"}"#).unwrap();
        assert!(matches!(i, Intent::NewTerminal { pane: 3, launch: Some(Launch::Claude), .. }), "got {i:?}");
        // A closed set: the client names a program roost knows, never a
        // command line of its own.
        assert!(decode(r#"{"t":"NewTerminal","pane":3,"launch":"rm -rf /"}"#).is_err());
    }

    /// R4: the worktree page's `?fork=` carries a Claude session id, decoded
    /// the same way `resume` already is — present only when the dialog's
    /// own lookup resolved one, absent on every older/plainer request.
    #[test]
    fn new_terminal_decodes_a_fork_id() {
        let i = decode(r#"{"t":"NewTerminal","pane":3,"launch":"claude","fork":"864ee734-e3ab-434e-8278-745a850b16ad"}"#).unwrap();
        assert!(
            matches!(i, Intent::NewTerminal { pane: 3, launch: Some(Launch::Claude), fork: Some(ref f), .. } if f == "864ee734-e3ab-434e-8278-745a850b16ad"),
            "got {i:?}"
        );
        // Absent on an older/plainer request — `#[serde(default)]`, not a
        // required field.
        let i = decode(r#"{"t":"NewTerminal","pane":3}"#).unwrap();
        assert!(matches!(i, Intent::NewTerminal { fork: None, .. }), "got {i:?}");
    }

    /// The `+` dialog's three wire shapes (#156): a tag field (`k`), the
    /// same style `Tab` uses, so `Claude`/`Shell` and `Fork`'s own payload
    /// share one shape instead of one being a bare string and the other an
    /// object serde's default external tagging would produce.
    #[test]
    fn new_worktree_decodes_each_launch_shape_and_an_optional_name() {
        let i = decode(r#"{"t":"NewWorktree","name":"bright-fox","launch":{"k":"Fork","session":"term3"}}"#).unwrap();
        assert!(
            matches!(i, Intent::NewWorktree { name: Some(ref n), launch: Some(WorktreeLaunch::Fork { ref session }) } if n == "bright-fox" && session == "term3"),
            "got {i:?}"
        );
        let i = decode(r#"{"t":"NewWorktree","launch":{"k":"Claude"}}"#).unwrap();
        assert!(matches!(i, Intent::NewWorktree { name: None, launch: Some(WorktreeLaunch::Claude) }), "got {i:?}");
        let i = decode(r#"{"t":"NewWorktree","launch":{"k":"Shell"}}"#).unwrap();
        assert!(matches!(i, Intent::NewWorktree { launch: Some(WorktreeLaunch::Shell), .. }), "got {i:?}");
        // No launch at all — the name field alone, blank for "generate one".
        let i = decode(r#"{"t":"NewWorktree"}"#).unwrap();
        assert!(matches!(i, Intent::NewWorktree { name: None, launch: None }), "got {i:?}");
    }

    #[test]
    fn worktree_ready_carries_the_forked_session_id_never_the_terminal_name() {
        let s = encode(&Event::WorktreeReady {
            url: "repo/.claude/worktrees/worktree-fox".into(),
            launch: Some(Launch::Claude),
            fork: Some("864ee734-e3ab-434e-8278-745a850b16ad".into()),
        });
        assert!(s.contains(r#""fork":"864ee734-e3ab-434e-8278-745a850b16ad""#), "{s}");
        assert!(!s.contains("term3"), "the terminal name the dialog offered must never be on the wire here");
    }

    /// Pins the wire shape the switcher's Allow…/Revoke actions send: both
    /// fields required, `parent` explicit (never inferred from the page).
    #[test]
    fn outside_worktree_intents_decode_and_require_their_fields() {
        let i = decode(r#"{"t":"AllowOutsideWorktree","parent":"repo","path":"/home/u/scratch/wt"}"#).unwrap();
        assert!(
            matches!(i, Intent::AllowOutsideWorktree { ref parent, ref path } if parent == "repo" && path == "/home/u/scratch/wt"),
            "got {i:?}"
        );
        let i = decode(r#"{"t":"RevokeOutsideWorktree","parent":"repo","name":"wt"}"#).unwrap();
        assert!(
            matches!(i, Intent::RevokeOutsideWorktree { ref parent, ref name } if parent == "repo" && name == "wt"),
            "got {i:?}"
        );
        assert!(decode(r#"{"t":"AllowOutsideWorktree","path":"/x"}"#).is_err(), "parent is required");
        assert!(decode(r#"{"t":"RevokeOutsideWorktree","parent":"repo"}"#).is_err(), "name is required");
    }

    /// The client sends exactly this shape; a renamed field would decode as
    /// an error and the Dismiss button would do nothing.
    ///
    /// Revert-checked: with the variant's field serde-renamed to `p`, fails
    /// — observed: ``called `Result::unwrap()` on an `Err` value: "missing
    /// field `p`"``. Restored.
    #[test]
    fn dismiss_appeared_decodes_its_path() {
        let i = decode(r#"{"t":"DismissAppeared","path":"/x/wt"}"#).unwrap();
        assert!(matches!(i, Intent::DismissAppeared { ref path } if path == "/x/wt"), "got {i:?}");
        assert!(decode(r#"{"t":"DismissAppeared"}"#).is_err(), "path is required");
    }

    /// `url` must encode as `null`, not vanish: the client branches on it.
    ///
    /// Revert-checked: with `skip_serializing_if = "Option::is_none"` on
    /// `url`, fails — observed: `left:
    /// "{\"path\":\"/o/wt\",\"branch\":\"side\",\"outside\":true,\"allowed\":false}"`.
    /// Restored.
    #[test]
    fn appeared_view_encodes_every_field() {
        let v = AppearedView { path: "/o/wt".into(), branch: "side".into(), url: None, outside: true, allowed: false };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"path":"/o/wt","branch":"side","url":null,"outside":true,"allowed":false}"#);
    }

    /// Revert-checked: with the variant's `name` field renamed to `wt`
    /// (cp backup, restored), fails `left:
    /// "{\"t\":\"OutsideRevoked\",\"parent\":\"repo\",\"wt\":\"wt\"}"` — the
    /// client reads `ev.parent` and `ev.name`, so the wire names are the
    /// contract.
    #[test]
    fn outside_revoked_encodes_parent_and_name() {
        let s = encode(&Event::OutsideRevoked { parent: "repo".into(), name: "wt".into() });
        assert_eq!(s, r#"{"t":"OutsideRevoked","parent":"repo","name":"wt"}"#);
    }

    #[test]
    fn outside_allowed_encodes_its_url() {
        let s = encode(&Event::OutsideAllowed { url: "repo/.claude/outside/wt".into() });
        assert_eq!(s, r#"{"t":"OutsideAllowed","url":"repo/.claude/outside/wt"}"#);
    }

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

    /// `home` is new; every saved layout on disk and every client that has
    /// not been told about re-homing yet sends and expects the old shape —
    /// no `home` key at all. `#[serde(default)]` is what keeps the read side
    /// working, and `skip_serializing_if` is what keeps the write side from
    /// drifting away from it the moment a native tab round-trips through the
    /// server once: without it, a fresh `State` broadcast for a tab nobody
    /// ever re-homed would start carrying `"home":null`, which is still
    /// parseable but is no longer the byte-identical wire shape this pins.
    #[test]
    fn a_saved_layout_without_home_still_loads() {
        let t: Tab = serde_json::from_str(r#"{"k":"Terminal","session":"term"}"#).unwrap();
        assert_eq!(t, Tab::Terminal { session: "term".into(), home: None });
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"k":"Terminal","session":"term"}"#,
            "byte-identical when native"
        );
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

    /// The field a client from before #18 step 2 does not send. Its default
    /// has to be "plain shell", or an old browser's Enter on a placeholder
    /// would start resuming conversations nobody asked for.
    ///
    /// Revert-checked: removing `#[serde(default)]` makes the first case fail
    /// to decode at all — `Err("missing field `resume`")` — which is the
    /// louder half of the same defect: every existing client's placeholder
    /// stops working.
    #[test]
    fn start_terminal_resumes_only_when_asked_and_defaults_to_a_plain_shell() {
        assert!(
            matches!(
                decode(r#"{"t":"StartTerminal","session":"shell"}"#).unwrap(),
                Intent::StartTerminal { resume: false, .. }
            ),
            "a message with no `resume` must mean a plain shell"
        );
        assert!(matches!(
            decode(r#"{"t":"StartTerminal","session":"shell","resume":true}"#).unwrap(),
            Intent::StartTerminal { resume: true, session } if session == "shell"
        ));
        // No id on the wire, by design — so a message carrying one is not a
        // way to smuggle it in. serde ignores unknown fields, which is the
        // behaviour being pinned: the id is looked up server-side or not at
        // all.
        assert!(matches!(
            decode(r#"{"t":"StartTerminal","session":"shell","resume":true,"session_id":"x; rm -rf ~"}"#)
                .unwrap(),
            Intent::StartTerminal { resume: true, .. }
        ));
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
            Intent::NewTerminal { pane, .. } if pane == 2
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

    /// `to: None` ("Move back") must decode with no `to` key at all — the
    /// client never sends `"to":null` for it (see `a_saved_layout_...`'s
    /// own reasoning for why `#[serde(default)]` matters here too).
    #[test]
    fn rehome_decodes_both_directions() {
        let i = decode(r#"{"t":"Rehome","session":"term3","to":"repo/.claude/worktrees/w1"}"#).unwrap();
        assert!(matches!(i, Intent::Rehome { session, to: Some(to) } if session == "term3" && to == "repo/.claude/worktrees/w1"));
        let i = decode(r#"{"t":"Rehome","session":"term3"}"#).unwrap();
        assert!(matches!(i, Intent::Rehome { session, to: None } if session == "term3"));
    }

    #[test]
    fn claude_elsewhere_encodes_both_directions() {
        let s = encode(&Event::ClaudeElsewhere { session: "term3".into(), worktree: Some("repo/.claude/worktrees/w1".into()) });
        assert!(s.contains(r#""t":"ClaudeElsewhere""#) && s.contains(r#""worktree":"repo/.claude/worktrees/w1""#));
        let s = encode(&Event::ClaudeElsewhere { session: "term3".into(), worktree: None });
        assert!(s.contains(r#""worktree":null"#));
    }

    #[test]
    fn worktree_gone_encodes_both_fields() {
        let s = encode(&Event::WorktreeGone { worktree: "repo/.claude/worktrees/w1".into(), session: "term3".into() });
        assert!(s.contains(r#""t":"WorktreeGone""#) && s.contains("w1") && s.contains("term3"));
    }

    /// The client branches on `url` being null, so `None` must encode as
    /// `null` and not be skipped.
    ///
    /// Revert-checked: with `#[serde(skip_serializing_if =
    /// "Option::is_none")]` on `url`, this fails — observed:
    /// `{"t":"WorktreeAppeared","path":"/elsewhere/x","branch":"x"}`.
    /// Restored.
    #[test]
    fn worktree_appeared_encodes_its_url_or_null() {
        let s = encode(&Event::WorktreeAppeared { path: "/r/uc-rel".into(), branch: "docs/rel".into(), url: Some("uc-rel".into()) });
        assert!(s.contains(r#""t":"WorktreeAppeared""#) && s.contains(r#""branch":"docs/rel""#) && s.contains(r#""url":"uc-rel""#), "{s}");
        let s = encode(&Event::WorktreeAppeared { path: "/elsewhere/x".into(), branch: "x".into(), url: None });
        assert!(s.contains(r#""url":null"#), "{s}");
    }

    /// The banner reads both fields by name; a rename of either would
    /// leave it printing `undefined`.
    ///
    /// Revert-checked: with `#[serde(rename = "old")]` on `from`, fails —
    /// observed `{"t":"ProjectMoved","old":"repo/.claude/worktrees/worktree-a",…}`.
    /// Restored.
    #[test]
    fn project_moved_encodes_from_and_to() {
        let s = encode(&Event::ProjectMoved { from: "repo/.claude/worktrees/worktree-a".into(), to: "repo/.claude/worktrees/worktree-b".into() });
        assert!(s.contains(r#""t":"ProjectMoved""#), "{s}");
        assert!(s.contains(r#""from":"repo/.claude/worktrees/worktree-a""#) && s.contains(r#""to":"repo/.claude/worktrees/worktree-b""#), "{s}");
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
    fn decodes_mention_path_with_and_without_a_line_range() {
        let i = decode(r#"{"t":"MentionPath","rel":"src/hub.rs","line_start":12,"line_end":40}"#).unwrap();
        assert!(matches!(
            i,
            Intent::MentionPath { rel, line_start: Some(12), line_end: Some(40), .. } if rel == "src/hub.rs"
        ));
        let i = decode(r#"{"t":"MentionPath","rel":"README.md","line_start":null,"line_end":null}"#).unwrap();
        assert!(matches!(
            i,
            Intent::MentionPath { rel, line_start: None, line_end: None, .. } if rel == "README.md"
        ));
    }

    /// The field is optional on the wire so an older client's payload still
    /// parses. Pinning it means a later `#[serde(deny_unknown_fields)]` or a
    /// dropped `default` cannot silently start rejecting them.
    #[test]
    fn a_mention_without_a_session_still_decodes() {
        let i = decode(r#"{"t":"MentionPath","rel":"a.rs","line_start":null,"line_end":null}"#)
            .unwrap();
        assert!(matches!(i, Intent::MentionPath { session: None, .. }));
    }

    #[test]
    fn a_mention_carries_the_terminal_it_is_aimed_at() {
        let i = decode(
            r#"{"t":"MentionPath","rel":"a.rs","line_start":null,"line_end":null,"session":"term2"}"#,
        )
        .unwrap();
        assert!(matches!(i, Intent::MentionPath { session: Some(s), .. } if s == "term2"));
    }

    #[test]
    fn decodes_open_path_verbatim() {
        let i = decode(r#"{"t":"OpenPath","text":"~/p/roost/src/a.rs:42"}"#).unwrap();
        // Verbatim is the contract: the client does no parsing, so the suffix
        // and the tilde must both survive the wire intact.
        match i {
            Intent::OpenPath { text } => assert_eq!(text, "~/p/roost/src/a.rs:42"),
            other => panic!("decoded to {other:?}"),
        }
    }

    /// Revert-checked: marking `text` `#[serde(skip)]` — the shape a
    /// hand-written decoder would most plausibly land in — failed this test
    /// on the `Some(t)` arm, then restored.
    #[test]
    fn decodes_an_answer_with_and_without_edited_text() {
        // The `text` arm is how "the user changed my proposal before
        // accepting" reaches Claude. Decoding it as `None` would make Claude
        // write the text the user rejected.
        let i = decode(r#"{"t":"AnswerProposal","id":"p-1","accept":true,"text":"mine"}"#).unwrap();
        assert!(matches!(
            i,
            Intent::AnswerProposal { id, accept: true, text: Some(t) } if id == "p-1" && t == "mine"
        ));
        let i = decode(r#"{"t":"AnswerProposal","id":"p-1","accept":false,"text":null}"#).unwrap();
        assert!(matches!(i, Intent::AnswerProposal { accept: false, text: None, .. }));
    }

    /// Revert-checked: adding `#[serde(rename = "path")]` to the `rel` field
    /// (a plausible name mismatch between this decoder and the JS sender)
    /// failed this test — `decode(...)` returned
    /// `Err("missing field \`path\` ...")` on the `.unwrap()` — since the
    /// wire JSON here still carries `"rel"`, matching what `static/app.js`
    /// actually sends. Then restored.
    #[test]
    fn decodes_share_selection() {
        let i = decode(
            r#"{"t":"ShareSelection","rel":"src/a.rs","text":"let x = 1;","start_line":10,"start_col":4,"end_line":10,"end_col":14}"#,
        )
        .unwrap();
        assert!(matches!(
            i,
            Intent::ShareSelection { rel, text, start_line: 10, start_col: 4, end_line: 10, end_col: 14 }
                if rel == "src/a.rs" && text == "let x = 1;"
        ));
    }

    #[test]
    fn a_proposal_tab_round_trips_by_id() {
        // The id, not a path: two proposals for the same file are two tabs,
        // and the id is the only thing that can tell them apart.
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"Proposal","id":"diff-1-7"}}"#).unwrap();
        match i {
            Intent::OpenTab { tab, .. } => assert_eq!(tab, Tab::Proposal { id: "diff-1-7".into() }),
            other => panic!("decoded to {other:?}"),
        }
    }

    /// Revert-checked: renaming `old_text` on the wire (`#[serde(rename =
    /// "was")]`) failed this test — `got {"t":"Proposal","id":"p-1",
    /// "rel":"src/a.rs","was":"before","new_text":"after"}` — then restored.
    /// Removing the field outright does not compile, which is the other half
    /// of the guarantee.
    #[test]
    fn encodes_a_proposal_with_both_sides() {
        // Both sides on the wire, or the client has nothing to diff against:
        // the old side is disk, the new side has never been written anywhere.
        let s = encode(&Event::Proposal {
            id: "p-1".into(),
            rel: "src/a.rs".into(),
            old_text: "before".into(),
            new_text: "after".into(),
        });
        assert!(s.contains(r#""t":"Proposal""#), "got {s}");
        assert!(s.contains(r#""rel":"src/a.rs""#), "got {s}");
        assert!(s.contains(r#""old_text":"before""#), "got {s}");
        assert!(s.contains(r#""new_text":"after""#), "got {s}");
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

    /// The client branches on `line` being present, so `None` must encode as
    /// `null` and not be skipped — the same risk `worktree_appeared_encodes_its_url_or_null`
    /// (Task 5) pins for `WorktreeAppeared.url`.
    ///
    /// Revert-checked: adding `#[serde(skip_serializing_if = "Option::is_none")]`
    /// to `PathElsewhere.line` fails this test — observed:
    /// `{"t":"PathElsewhere","project":"other","rel":"a.md"}` (no `"line"` key
    /// at all, so the `Some` assertion below finds no `"line":7` and the
    /// `None` assertion's `contains(r#""line":null"#)` also fails). Restored.
    #[test]
    fn path_elsewhere_encodes_its_line_or_null() {
        let s = encode(&Event::PathElsewhere { project: "other".into(), rel: "a.md".into(), line: Some(7) });
        assert!(
            s.contains(r#""t":"PathElsewhere""#) && s.contains(r#""project":"other""#) && s.contains(r#""rel":"a.md""#) && s.contains(r#""line":7"#),
            "{s}"
        );
        let s = encode(&Event::PathElsewhere { project: "other".into(), rel: "a.md".into(), line: None });
        assert!(s.contains(r#""line":null"#), "{s}");
    }

    #[test]
    fn a_search_carries_its_sequence_number() {
        let i = decode(r#"{"t":"Search","q":"needle","seq":7}"#).unwrap();
        match i {
            Intent::Search { q, seq } => {
                assert_eq!(q, "needle");
                assert_eq!(seq, 7);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn open_at_line_carries_a_one_based_line() {
        let i = decode(r#"{"t":"OpenAtLine","pane":2,"rel":"src/hub.rs","line":412}"#).unwrap();
        match i {
            Intent::OpenAtLine { pane, rel, line } => {
                assert_eq!(pane, MIDDLE);
                assert_eq!(rel, "src/hub.rs");
                assert_eq!(line, 412);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// A File tab's JSON is what `wsstate` has already written to disk for
    /// every saved workspace on every host. Search must not change its shape:
    /// this test fails loudly if a `line` field is ever added to the tab
    /// itself, which is the design this plan deliberately avoided.
    #[test]
    fn a_file_tabs_wire_shape_is_unchanged_by_search() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"a.rs","mode":"Edit"}}"#).unwrap();
        assert!(
            matches!(i, Intent::OpenTab { tab: Tab::File { ref rel, mode: Mode::Edit }, .. } if rel == "a.rs"),
            "got {i:?}"
        );
    }

    #[test]
    fn set_setting_decodes_each_value_shape_and_a_clear() {
        // A JSON number is an Int, and a quoted one stays a Str — which is
        // what lets config::validate refuse "15" for an integer key instead
        // of writing a TOML string the loader cannot read as a size.
        let n: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"project","key":"code_font_size","value":15}"#).unwrap();
        assert_eq!(n, Intent::SetSetting { scope: Scope::Project, key: "code_font_size".into(), value: Some(SettingValue::Int(15)) });
        let q: Intent = serde_json::from_str(r#"{"t":"SetSetting","scope":"project","key":"code_font_size","value":"15"}"#).unwrap();
        assert_eq!(q, Intent::SetSetting { scope: Scope::Project, key: "code_font_size".into(), value: Some(SettingValue::Str("15".into())) });
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
            doc: "Colour theme.",
        };
        let j = serde_json::to_string(&row).unwrap();
        assert!(j.contains(r#""global":null"#), "{j}");
        assert!(j.contains(r#""effective":"nord""#), "{j}");
    }
}
