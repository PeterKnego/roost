//! The chat view: a Claude terminal shown as a conversation, for a phone.
//!
//! A phone is the wrong shape for a terminal: a fixed character grid under a
//! soft keyboard, output wrapped for someone else's width, menus driven by
//! arrow keys the keyboard does not have. This is a second *view* of the same
//! session, not a second Claude — the terminal stays the source of truth, a
//! desktop can keep using it at the same time, and everything the chat sends
//! goes into the same PTY a keystroke would.
//!
//! It is assembled from three sources, each used only for what it is reliable
//! at:
//!
//! - **The conversation** comes from the transcript Claude Code writes itself,
//!   `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` — structured, with
//!   real message boundaries, appended block by block as a turn progresses.
//!   Parsing the TUI's repainting output into a transcript was the obvious
//!   alternative and the fragile one: a wrong boundary shows a truncated
//!   answer. Which transcript is found through `~/.claude/sessions/<pid>.json`
//!   (Claude Code's own pid → session record, checked against the process's
//!   start time so a reused pid cannot hand over someone else's
//!   conversation), with the hook record `claudesess` as a fallback.
//! - **What is live** — a menu waiting for an answer, which option is
//!   selected, whether Claude is working — comes from the rendered screen
//!   (`session::screen_snapshot`, a vt100 grid at the PTY's size). That is a
//!   small surface with a recognisable shape, and the only part that has to
//!   be read off the screen at all.
//! - **Input** is written to the PTY exactly as the terminal would: a message
//!   as a bracketed paste and a carriage return, a menu choice as the arrow
//!   presses that move the selection there and an Enter.
//!
//! **A menu that is not recognised with confidence offers no buttons.** A
//! button that sends the wrong choice (a "Yes" to a delete) is worse than no
//! button: the client falls back to "open the terminal". And a choice is
//! re-checked against the screen when it arrives, so a menu that changed
//! between render and tap is refused rather than answered blind.
//!
//! The transcript lives outside every project root. It is only ever read, and
//! only at a path that resolves inside `~/.claude/projects` — never a path a
//! browser supplied.

use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

/// Entries kept per transcript. A long session runs to thousands; a phone
/// scrolling back that far would rather open the terminal.
const MAX_ENTRIES: usize = 400;
/// How much of an existing transcript is read on first open, from its end.
const INITIAL_TAIL_BYTES: u64 = 3 * 1024 * 1024;
/// Bound on one poll's read, so a transcript that grew by a lot is caught up
/// over several ticks rather than buffered whole.
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;
/// A tool's output as shown when expanded. The terminal has the rest.
const MAX_RESULT_CHARS: usize = 4000;
const MAX_SUMMARY_CHARS: usize = 300;
/// A message typed into the chat. Generous for a phone, and far below the
/// workspace's 2 MB file cap.
const MAX_SEND_BYTES: usize = 64 * 1024;

// ---- the conversation -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "k")]
pub enum Item {
    /// What the user typed (or a slash command, shown as typed).
    User { text: String },
    /// Claude's prose, already rendered by `render::markdown_html` — the same
    /// allowlist sanitizer a markdown preview goes through.
    Assistant { html: String },
    /// One tool call, and its result once it arrives.
    Tool {
        name: String,
        summary: String,
        /// Project-relative when the tool names a file inside the project,
        /// so the client can make it a link that opens the file.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<ToolResult>,
        /// The call's own id when it started a subagent, which is what the
        /// client asks for to open that subagent's conversation.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// Something that happened rather than something said: an interruption,
    /// a compaction, a local command's output.
    Note { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolResult {
    pub ok: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: u64,
    /// Bumped whenever the entry changes (a tool's result arriving), so the
    /// client can upsert by id and the server can send only what changed.
    pub rev: u64,
    #[serde(flatten)]
    pub item: Item,
}

/// An incrementally read transcript.
pub struct Transcript {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    entries: Vec<Entry>,
    tools: HashMap<String, u64>,
    next_id: u64,
    next_rev: u64,
    /// Set when entries were dropped off the front, so a client holding them
    /// has to start over rather than upsert.
    reset: bool,
    project: String,
    dir: PathBuf,
    /// A subagent's transcript, where every line is a sidechain line.
    sidechain: bool,
}

impl Transcript {
    pub fn open(path: PathBuf, project: &str, dir: &Path) -> Transcript {
        Transcript::open_as(path, project, dir, false)
    }

    /// A subagent's own transcript (`subagents/agent-<id>.jsonl`), whose
    /// lines all carry `isSidechain` — the mark that, in the main file, means
    /// "not this conversation".
    pub fn open_subagent(path: PathBuf, project: &str, dir: &Path) -> Transcript {
        Transcript::open_as(path, project, dir, true)
    }

    fn open_as(path: PathBuf, project: &str, dir: &Path, sidechain: bool) -> Transcript {
        Transcript {
            path,
            offset: 0,
            partial: Vec::new(),
            entries: Vec::new(),
            tools: HashMap::new(),
            next_id: 1,
            next_rev: 1,
            reset: true,
            project: project.to_string(),
            dir: dir.to_path_buf(),
            sidechain,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what was appended since the last poll. Returns whether anything
    /// changed. A file that cannot be read changes nothing — the entries
    /// already shown stay shown.
    pub fn poll(&mut self) -> bool {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return false };
        let Ok(meta) = f.metadata() else { return false };
        let len = meta.len();
        if len < self.offset {
            // Truncated or replaced: start over.
            *self = Transcript::open_as(self.path.clone(), &self.project.clone(), &self.dir.clone(), self.sidechain);
        }
        let mut skip_first_line = false;
        if self.offset == 0 && len > INITIAL_TAIL_BYTES {
            self.offset = len - INITIAL_TAIL_BYTES;
            skip_first_line = true;
        }
        if len == self.offset {
            return false;
        }
        let want = (len - self.offset).min(MAX_READ_BYTES);
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut buf = Vec::with_capacity(want as usize);
        if f.take(want).read_to_end(&mut buf).is_err() {
            return false;
        }
        self.offset += buf.len() as u64;
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(&buf);
        if skip_first_line {
            match data.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    data.drain(..=i);
                }
                None => return false,
            }
        }
        let mut changed = false;
        let mut start = 0;
        while let Some(i) = data[start..].iter().position(|b| *b == b'\n') {
            let line = &data[start..start + i];
            start += i + 1;
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
                changed |= self.ingest(&v);
            }
        }
        self.partial = data[start..].to_vec();
        if self.entries.len() > MAX_ENTRIES {
            let drop = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop);
            self.reset = true;
        }
        changed
    }

    /// Entries the client has not seen at `since` (a rev), and whether it
    /// must clear what it has first.
    pub fn take_changes(&mut self, since: u64) -> (bool, Vec<Entry>, u64) {
        let reset = std::mem::take(&mut self.reset);
        let items = self
            .entries
            .iter()
            .filter(|e| reset || e.rev > since)
            .cloned()
            .collect();
        (reset, items, self.next_rev - 1)
    }

    fn push(&mut self, item: Item) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let rev = self.next_rev;
        self.next_rev += 1;
        self.entries.push(Entry { id, rev, item });
        id
    }

    /// One transcript line. Returns whether it produced or changed an entry.
    pub fn ingest(&mut self, v: &serde_json::Value) -> bool {
        // A subagent's own turns are not this conversation's — unless this is
        // the subagent's own file.
        if !self.sidechain && v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
            return false;
        }
        if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
            self.push(Item::Note { text: "Conversation compacted".into() });
            return true;
        }
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let Some(msg) = v.get("message") else { return false };
        let content = msg.get("content");
        match ty {
            "user" => {
                if v.get("isMeta").and_then(|b| b.as_bool()) == Some(true) {
                    return false;
                }
                match content {
                    Some(serde_json::Value::String(s)) => self.user_string(s),
                    Some(serde_json::Value::Array(blocks)) => {
                        let mut changed = false;
                        let mut texts = Vec::new();
                        for b in blocks {
                            match b.get("type").and_then(|t| t.as_str()) {
                                Some("text") => {
                                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                        texts.push(t.to_string());
                                    }
                                }
                                Some("image") => texts.push("[image]".into()),
                                Some("tool_result") => changed |= self.tool_result(b),
                                _ => {}
                            }
                        }
                        let joined = texts.join("\n");
                        if !joined.trim().is_empty() {
                            changed |= self.user_string(&joined);
                        }
                        changed
                    }
                    _ => false,
                }
            }
            "assistant" => {
                let Some(serde_json::Value::Array(blocks)) = content else { return false };
                let mut changed = false;
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            let t = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            if !t.trim().is_empty() {
                                let html = crate::render::markdown_html(t, &self.project, "");
                                self.push(Item::Assistant { html });
                                changed = true;
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|t| t.as_str()).unwrap_or("tool").to_string();
                            let input = b.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            let (summary, path) = tool_summary(&name, &input, &self.dir);
                            let tid = b.get("id").and_then(|t| t.as_str()).map(str::to_string);
                            let agent = tid.clone().filter(|t| starts_subagent(&name) && valid_tool_id(t));
                            let id = self.push(Item::Tool { name, summary, path, result: None, agent });
                            if let Some(tid) = tid {
                                self.tools.insert(tid, id);
                            }
                            changed = true;
                        }
                        _ => {}
                    }
                }
                changed
            }
            _ => false,
        }
    }

    fn user_string(&mut self, s: &str) -> bool {
        let t = s.trim();
        if t.is_empty() {
            return false;
        }
        if t.starts_with("[Request interrupted") {
            self.push(Item::Note { text: "Interrupted".into() });
            return true;
        }
        if let Some(name) = tag(t, "command-name") {
            let args = tag(t, "command-args").unwrap_or_default();
            let text = if args.trim().is_empty() { name } else { format!("{name} {}", args.trim()) };
            self.push(Item::User { text });
            return true;
        }
        if let Some(out) = tag(t, "local-command-stdout") {
            let out = strip_ansi(&out);
            if out.trim().is_empty() {
                return false;
            }
            self.push(Item::Note { text: truncate(out.trim(), MAX_RESULT_CHARS) });
            return true;
        }
        if let Some(cmd) = tag(t, "bash-input") {
            self.push(Item::User { text: format!("! {cmd}") });
            return true;
        }
        if t.starts_with("<bash-stdout>") || t.starts_with("<bash-stderr>") {
            let out = format!(
                "{}{}",
                tag(t, "bash-stdout").unwrap_or_default(),
                tag(t, "bash-stderr").unwrap_or_default()
            );
            let out = strip_ansi(&out);
            if out.trim().is_empty() {
                return false;
            }
            self.push(Item::Note { text: truncate(out.trim(), MAX_RESULT_CHARS) });
            return true;
        }
        // Harness chatter that reaches the transcript as a user line.
        if t.starts_with("<command-message>") || t.starts_with("<system-reminder>") {
            return false;
        }
        self.push(Item::User { text: t.to_string() });
        true
    }

    fn tool_result(&mut self, b: &serde_json::Value) -> bool {
        let Some(tid) = b.get("tool_use_id").and_then(|t| t.as_str()) else { return false };
        let Some(&id) = self.tools.get(tid) else { return false };
        let ok = b.get("is_error").and_then(|e| e.as_bool()) != Some(true);
        let text = match b.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(|t| t.as_str()) {
                    Some("text") => p.get("text").and_then(|t| t.as_str()).map(str::to_string),
                    Some("image") => Some("[image]".into()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        let rev = self.next_rev;
        let Some(e) = self.entries.iter_mut().find(|e| e.id == id) else { return false };
        if let Item::Tool { result, .. } = &mut e.item {
            *result = Some(ToolResult { ok, text: truncate(&strip_ansi(&text), MAX_RESULT_CHARS) });
            e.rev = rev;
            self.next_rev += 1;
            return true;
        }
        false
    }
}

/// The text between `<name>` and `</name>`, if both are there.
fn tag(s: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let a = s.find(&open)? + open.len();
    let b = s[a..].find(&close)? + a;
    Some(s[a..b].to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Colour codes from a local command's captured output, which the transcript
/// keeps verbatim.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// One line saying what a tool call does, and the file it names when that
/// file is inside the project.
pub fn tool_summary(name: &str, input: &serde_json::Value, dir: &Path) -> (String, Option<String>) {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let mut path = None;
    let summary = match name {
        "Bash" => s("command").unwrap_or_default(),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let p = s("file_path").or_else(|| s("notebook_path")).unwrap_or_default();
            path = project_rel(&p, dir);
            path.clone().unwrap_or(p)
        }
        "Grep" => match s("path") {
            Some(p) => format!("{} in {}", s("pattern").unwrap_or_default(), project_rel(&p, dir).unwrap_or(p)),
            None => s("pattern").unwrap_or_default(),
        },
        "Glob" => s("pattern").unwrap_or_default(),
        "WebFetch" => s("url").unwrap_or_default(),
        "WebSearch" => s("query").unwrap_or_default(),
        "Task" | "Agent" => s("description").unwrap_or_default(),
        "AskUserQuestion" => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(|q| q.as_str())
            .unwrap_or_default()
            .to_string(),
        "TodoWrite" => {
            let n = input.get("todos").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0);
            format!("{n} todos")
        }
        _ => input
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default(),
    };
    (truncate(summary.lines().next().unwrap_or(""), MAX_SUMMARY_CHARS), path)
}

/// `p` relative to the project directory, when it is inside it. Lexical on
/// purpose: this only decides whether to *offer* a link, and opening it goes
/// through `OpenPath`, which confines it properly.
fn project_rel(p: &str, dir: &Path) -> Option<String> {
    let rel = Path::new(p).strip_prefix(dir).ok()?;
    let r = rel.to_str()?;
    (!r.is_empty() && !r.split('/').any(|seg| seg == "..")).then(|| r.to_string())
}

// ---- what is live on screen -------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opt {
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    /// The line directly above the options — usually the question itself.
    pub question: String,
    /// Lines above that, up to the box's edge: what the question is about.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    pub options: Vec<Opt>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Live {
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Prompt>,
    /// Nothing on screen at all — a session rejoined after a restart that has
    /// not repainted yet. Said, so the client can explain an absent menu.
    pub blank: bool,
}

const MARK: char = '❯';
const REVIEW_HEADING: &str = "Review your answers";

fn is_rule(row: &str) -> bool {
    let t = row.trim();
    t.chars().count() >= 3 && t.chars().all(|c| c == '─' || c == '━')
}

/// Column (in chars) of the first non-space char.
fn indent(row: &str) -> usize {
    row.chars().take_while(|c| *c == ' ').count()
}

pub fn live_from_rows(rows: &[String]) -> Live {
    let rows: Vec<String> = rows.iter().map(|r| r.trim_end().to_string()).collect();
    let blank = rows.iter().all(|r| r.is_empty());
    let mut end = rows.len();
    while end > 0 && rows[end - 1].is_empty() {
        end -= 1;
    }
    let bottom = &rows[end.saturating_sub(6)..end];
    let busy = bottom.iter().any(|r| r.contains("esc to interrupt"));
    let mode = bottom.iter().rev().find_map(|r| mode_of(r));
    Live { busy, mode, prompt: parse_prompt(&rows[..end]), blank }
}

fn mode_of(row: &str) -> Option<String> {
    let t = row.trim();
    let i = t.find(" on")?;
    let head = &t[..i + 3];
    if !(head.ends_with("mode on") || head.ends_with("accept edits on")) {
        return None;
    }
    // Drop the leading glyphs (⏵⏵, ⏸) — the words are the mode.
    let words = head.trim_start_matches(|c: char| !c.is_alphabetic());
    Some(words.to_string())
}

/// The menu Claude Code has up, if the screen shows one this can read with
/// confidence. `rows` are trimmed and end at the last non-blank row.
pub fn parse_prompt(rows: &[String]) -> Option<Prompt> {
    // The footer is the anchor: every selectable menu Claude Code draws ends
    // with a hint line naming Enter or Esc, and nothing else on screen does.
    // Joined over the last three rows, because at phone width it wraps.
    let n = rows.len();
    let tail: Vec<&str> = rows[n.saturating_sub(3)..].iter().map(|s| s.trim()).collect();
    let foot = tail.join(" ");
    let hinted = ["Esc to cancel", "Enter to confirm", "Enter to select", "esc to cancel"]
        .iter()
        .any(|k| foot.contains(k));
    // The one menu without a hint: AskUserQuestion's closing review of a
    // multi-question ask, "Review your answers" over "1. Submit answers /
    // 2. Cancel". Anchored on that heading plus the screen ending in that
    // exact last option, so a transcript merely quoting the words is not it.
    let review = !hinted
        && rows.last().map(|r| r.trim_start().trim_start_matches(MARK).trim() == "2. Cancel").unwrap_or(false)
        && rows[n.saturating_sub(45)..].iter().any(|r| r.trim() == REVIEW_HEADING);
    if !hinted && !review {
        return None;
    }
    // The selected option: the last ❯ row above the footer that is not the
    // input box (which sits between two rules).
    let lo = n.saturating_sub(45);
    let sel_row = (lo..n).rev().find(|&i| {
        let r = &rows[i];
        let t = r.trim_start();
        t.starts_with(MARK)
            && indent(r) <= 3
            && !(i > 0 && is_rule(&rows[i - 1]) && i + 1 < n && is_rule(&rows[i + 1]))
    })?;
    let mark_col = indent(&rows[sel_row]);
    let text_col = mark_col + 2;

    // An option row: blank up to text_col (or the mark), text starting at
    // text_col. Numbered rows carry `N. `.
    let option_text = |r: &str, i: usize| -> Option<String> {
        let chars: Vec<char> = r.chars().collect();
        if chars.len() <= text_col {
            return None;
        }
        let lead: String = chars[..text_col].iter().collect();
        let lead_ok = if i == sel_row {
            lead.trim() == MARK.to_string()
        } else {
            lead.trim().is_empty()
        };
        if !lead_ok || chars[text_col] == ' ' {
            return None;
        }
        Some(chars[text_col..].iter().collect())
    };
    let numbered = option_text(&rows[sel_row], sel_row)
        .map(|t| number_of(&t).is_some())
        .unwrap_or(false);

    // Walk up from the selected row to the top of the block, then down to the
    // bottom, collecting options and their continuation lines.
    let is_member = |i: usize| -> bool {
        let r = &rows[i];
        if r.is_empty() {
            return false;
        }
        if option_text(r, i).is_some() {
            return true;
        }
        // Continuation: deeper than the text column, and no mark.
        indent(r) > text_col && !r.trim_start().starts_with(MARK)
    };
    let mut top = sel_row;
    while top > 0 && is_member(top - 1) {
        top -= 1;
    }
    // A continuation line cannot start the block.
    while top < sel_row && option_text(&rows[top], top).is_none() {
        top += 1;
    }
    let mut bottom = sel_row;
    loop {
        let next = bottom + 1;
        if next >= n {
            break;
        }
        if is_member(next) {
            bottom = next;
            continue;
        }
        // AskUserQuestion draws a rule between its typed answer and "Chat
        // about this": step over a rule only when an option follows it.
        if is_rule(&rows[next]) && next + 1 < n && option_text(&rows[next + 1], next + 1).is_some() {
            bottom = next + 1;
            continue;
        }
        break;
    }

    let mut options: Vec<Opt> = Vec::new();
    let mut selected = None;
    for (i, r) in rows.iter().enumerate().take(bottom + 1).skip(top) {
        if is_rule(r) {
            continue;
        }
        if let Some(t) = option_text(r, i) {
            let label = if numbered {
                match number_of(&t) {
                    Some((num, rest)) => {
                        // Numbers must count up from 1 in order, or this is
                        // not a menu read correctly.
                        if num != options.len() + 1 {
                            return None;
                        }
                        rest
                    }
                    None => return None,
                }
            } else {
                t.trim().to_string()
            };
            if i == sel_row {
                selected = Some(options.len());
            }
            options.push(Opt { label, detail: String::new() });
        } else if let Some(o) = options.last_mut() {
            if !o.detail.is_empty() {
                o.detail.push(' ');
            }
            o.detail.push_str(r.trim());
        }
    }
    let selected = selected?;
    if options.len() < 2 {
        return None;
    }

    // The question: the paragraph directly above the block, stopping at a
    // rule (the box's edge) or a second blank row.
    let mut above: Vec<String> = Vec::new();
    let mut i = top;
    let mut blanks = 0;
    while i > 0 && above.len() < 6 {
        i -= 1;
        let r = rows[i].trim();
        if is_rule(r) {
            break;
        }
        if r.is_empty() {
            if !above.is_empty() {
                break;
            }
            blanks += 1;
            if blanks > 1 {
                break;
            }
            continue;
        }
        above.push(r.to_string());
    }
    above.reverse();
    // The review lists every question with its answer: all of it is
    // context, and the heading is the question being asked.
    let question = if review { REVIEW_HEADING.to_string() } else { above.pop().unwrap_or_default() };
    // A permission prompt puts what is being permitted — the command, the
    // file, the warning — above a blank row over "Do you want to proceed?",
    // so the paragraph walk alone found a bare yes/no with nothing to judge.
    // Only when the walk stopped at a blank row: a rule means the paragraph
    // was already the box's whole text, and a full walk means it was long.
    let mut context = if review || is_rule(&rows[i]) || !rows[i].trim().is_empty() {
        Vec::new()
    } else {
        prompt_body(&rows[..i]).unwrap_or_default()
    };
    context.extend(above);
    Some(Prompt { question, context, options, selected })
}

/// The longest prompt body carried whole. Beyond it the head is kept, since
/// that is where a tool prompt names the tool and the command starts.
const MAX_BODY_ROWS: usize = 40;

/// The rows of the box a menu is drawn in, from just under its top edge to
/// `rows`' end, cleaned for reading: the `│` gutter a command is drawn in
/// removed, the `╌` rules around it and blank rows dropped.
///
/// `None` unless the box is unmistakable: a full `─` rule above, or — when a tall command has pushed that edge off the
/// screen — a `╌` rule, which only a tool prompt draws. Anything else would
/// read the transcript above as part of the question.
fn prompt_body(rows: &[String]) -> Option<Vec<String>> {
    let dashed = |r: &str| {
        let t = r.trim();
        t.chars().count() >= 3 && t.chars().all(|c| c == '╌')
    };
    let start = match rows.iter().rposition(|r| is_rule(r)) {
        Some(edge) => edge + 1,
        None if rows.iter().any(|r| dashed(r)) => 0,
        None => return None,
    };
    let mut body: Vec<String> = rows[start..]
        .iter()
        .filter(|r| !dashed(r))
        .map(|r| {
            let t = r.trim();
            t.strip_prefix('│').map(str::trim).unwrap_or(t).to_string()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if body.len() > MAX_BODY_ROWS {
        body.truncate(MAX_BODY_ROWS);
        body.push("…".into());
    }
    Some(body)
}

fn number_of(t: &str) -> Option<(usize, String)> {
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix(". ")?;
    Some((digits.parse().ok()?, rest.trim().to_string()))
}

// ---- input --------------------------------------------------------------

pub fn key_bytes(k: &str, app_cursor: bool) -> Option<&'static [u8]> {
    Some(match (k, app_cursor) {
        ("up", true) => b"\x1bOA",
        ("up", false) => b"\x1b[A",
        ("down", true) => b"\x1bOB",
        ("down", false) => b"\x1b[B",
        ("right", true) => b"\x1bOC",
        ("right", false) => b"\x1b[C",
        ("left", true) => b"\x1bOD",
        ("left", false) => b"\x1b[D",
        ("enter", _) => b"\r",
        ("esc", _) => b"\x1b",
        ("tab", _) => b"\t",
        ("shifttab", _) => b"\x1b[Z",
        ("ctrlc", _) => b"\x03",
        _ => return None,
    })
}

/// The presses that move a menu's selection from `from` to `to`, then Enter.
pub fn choice_bytes(from: usize, to: usize, app_cursor: bool) -> Vec<u8> {
    let (k, steps) = if to >= from { ("down", to - from) } else { ("up", from - to) };
    let arrow = key_bytes(k, app_cursor).unwrap_or(b"");
    let mut out = Vec::new();
    for _ in 0..steps {
        out.extend_from_slice(arrow);
    }
    out.push(b'\r');
    out
}

/// A message as it is pasted. Control characters are dropped — an `ESC`
/// inside the text could end the bracketed paste early and turn the rest of
/// the message into keystrokes.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let clean: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    let mut out = Vec::new();
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(clean.replace('\n', " ").as_bytes());
    }
    out
}

// ---- finding the transcript --------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Found(PathBuf),
    /// No Claude process in this terminal.
    NoClaude,
    /// A Claude is there, but which transcript is its could not be told.
    Unknown(String),
    /// A Claude is there and its transcript is known, but it does not exist
    /// yet: Claude Code creates it on the first message.
    Empty,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The transcript of the Claude running in `project`'s terminal `session`.
pub fn locate(proc_root: &Path, home: &Path, project: &str, session: &str) -> Source {
    let pids = claude_pids(proc_root, project, session);
    let mut best: Option<(u64, PathBuf)> = None;
    let mut saw = !pids.is_empty();
    for pid in &pids {
        let Ok(raw) = std::fs::read_to_string(home.join(".claude/sessions").join(format!("{pid}.json"))) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { continue };
        let Some(id) = v.get("sessionId").and_then(|s| s.as_str()) else { continue };
        if !crate::claudesess::valid_session_id(id) {
            continue;
        }
        // A pid is only this record's if the process started when the record
        // says it did; otherwise the pid was reused.
        let recorded = v.get("procStart").and_then(|s| s.as_str()).unwrap_or("");
        if recorded.is_empty() || start_time(proc_root, *pid).as_deref() != Some(recorded) {
            continue;
        }
        let Some(cwd) = v.get("cwd").and_then(|s| s.as_str()) else { continue };
        let Some(tdir) = crate::claudehist::transcript_dir_in(home, Path::new(cwd)) else { continue };
        let started = v.get("startedAt").and_then(|s| s.as_u64()).unwrap_or(0);
        let path = tdir.join(format!("{id}.jsonl"));
        if best.as_ref().map(|(s, _)| started >= *s).unwrap_or(true) {
            best = Some((started, path));
        }
        saw = true;
    }
    if let Some((_, p)) = best {
        if let Some(p) = confined(home, &p) {
            return Source::Found(p);
        }
        if matches!(std::fs::symlink_metadata(&p), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound) {
            return Source::Empty;
        }
    }
    // The hook's record, when hooks are on.
    if let Some(rec) = crate::claudesess::recorded(project, session) {
        if let Some(p) = rec.transcript_path.as_deref().and_then(|p| confined(home, Path::new(p))) {
            return Source::Found(p);
        }
    }
    if saw {
        Source::Unknown("Claude is running, but its conversation file was not found".into())
    } else {
        Source::NoClaude
    }
}

/// `p`, if it resolves to a `.jsonl` inside `~/.claude/projects`.
fn confined(home: &Path, p: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(home.join(".claude/projects")).ok()?;
    let real = std::fs::canonicalize(p).ok()?;
    (real.starts_with(&root) && real.extension().and_then(|e| e.to_str()) == Some("jsonl")).then_some(real)
}

/// The tools whose call starts a subagent: `Agent`, and `Task`, its name
/// before Claude Code renamed it.
fn starts_subagent(name: &str) -> bool {
    name == "Agent" || name == "Task"
}

/// A tool-use id as the API mints them (`toolu_` and base62). It arrives from
/// the browser and is only ever compared, never joined into a path, but a
/// bound on its shape keeps anything else from being looked up at all.
pub fn valid_tool_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Subagent records looked at per lookup. A session that has run thousands
/// of subagents has a phone user scrolling back further than this view is for.
const MAX_AGENT_RECORDS: usize = 2000;

/// What Claude Code records about a subagent beside its transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentInfo {
    pub description: String,
    pub agent_type: String,
    /// `finished` in the record. Absent while it runs.
    pub finished: bool,
}

/// The transcript of the subagent that tool call `tool` started in the
/// conversation at `main`.
///
/// Claude Code writes each subagent to `<session>/subagents/agent-<id>.jsonl`
/// beside a `.meta.json` naming the `toolUseId` that started it — written at
/// spawn, so a running subagent is found too. A nested subagent's record is
/// in the same directory, keyed by its own call in its parent's transcript,
/// so one lookup serves every depth. The directory is derived from `main`,
/// which `locate` already confined; the file found is confined again before
/// it is returned. `None` covers both "no such subagent" and "cannot tell":
/// nothing here is destroyed, only not shown.
pub fn locate_agent(home: &Path, main: &Path, tool: &str) -> Option<(PathBuf, AgentInfo)> {
    if !valid_tool_id(tool) {
        return None;
    }
    let dir = main.with_extension("").join("subagents");
    let rd = std::fs::read_dir(&dir).ok()?;
    for e in rd.flatten().take(MAX_AGENT_RECORDS * 2) {
        let name = e.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".meta.json")) else { continue };
        let Ok(f) = std::fs::File::open(e.path()) else { continue };
        let mut raw = String::new();
        if f.take(64 * 1024).read_to_string(&mut raw).is_err() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { continue };
        if v.get("toolUseId").and_then(|t| t.as_str()) != Some(tool) {
            continue;
        }
        let path = confined(home, &dir.join(format!("{stem}.jsonl")))?;
        let text = |k: &str| v.get(k).and_then(|t| t.as_str()).unwrap_or("").to_string();
        let info = AgentInfo {
            description: text("description"),
            agent_type: text("agentType"),
            finished: v.get("finished").and_then(|b| b.as_bool()) == Some(true),
        };
        return Some((path, info));
    }
    None
}

fn claude_pids(proc_root: &Path, project: &str, session: &str) -> Vec<u32> {
    let Ok(rd) = std::fs::read_dir(proc_root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
        if comm.trim() != "claude" {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("environ")) else { continue };
        let (mut p, mut s) = (None, None);
        for kv in raw.split(|b| *b == 0) {
            let Ok(kv) = std::str::from_utf8(kv) else { continue };
            if let Some(v) = kv.strip_prefix("ROOST_PROJECT=") {
                p = Some(v.to_string());
            } else if let Some(v) = kv.strip_prefix("ROOST_SESSION=") {
                s = Some(v.to_string());
            }
        }
        if let (Some(p), Some(s)) = (p, s) {
            if s == session && (p == project || crate::home::effective_owner(&p, &s) == project) {
                out.push(pid);
            }
        }
    }
    out
}

/// Field 22 of `/proc/<pid>/stat`, split after the last `)` because `comm`
/// may contain spaces and parentheses (see `procsess`).
fn start_time(proc_root: &Path, pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19).map(str::to_string)
}

// ---- the websocket -------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(tag = "t")]
enum In {
    #[serde(rename = "send")]
    Send { text: String },
    #[serde(rename = "key")]
    Key { k: String },
    #[serde(rename = "choose")]
    Choose { index: usize, label: String },
}

/// `/ws/{project}/chat/{session}`. The same `Origin` rule as every other
/// browser-facing socket, including refusing a handshake that carries none:
/// this one writes into a shell.
pub fn handle_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut path = String::new();
    let allowed = crate::config::allowed_origins();
    let config = WebSocketConfig::default().max_message_size(Some(MAX_SEND_BYTES * 2));
    let Ok(probe) = stream.try_clone() else { return };
    let accepted = tungstenite::accept_hdr_with_config(
        stream,
        |req: &WsRequest, resp: WsResponse| {
            path = req.uri().path().to_string();
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("roost: rejected chat ws origin={origin:?}");
                return Err(tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("origin not allowed".to_string()))
                    .expect("static 403"));
            }
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };
    let rest = path.trim_start_matches("/ws/");
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 || segs[segs.len() - 2] != "chat" {
        let _ = ws.close(None);
        return;
    }
    let name = segs[segs.len() - 1].to_string();
    let project = segs[..segs.len() - 2].join("/");
    if !crate::session::valid_name(&name) {
        let _ = ws.close(None);
        return;
    }
    let Some(dir) = crate::projects::resolve_project(roots, &project) else {
        let _ = ws.close(None);
        return;
    };
    // A plain subscriber: it never sends a size, so it never narrows the PTY,
    // and `attach` spawns nothing without a reservation.
    let att = match crate::session::attach(&project, &name, &dir) {
        Ok(a) => a,
        Err(e) => {
            let _ = ws.send(Message::Text(json(&serde_json::json!({"t": "ended", "why": e})).into()));
            let _ = ws.close(None);
            return;
        }
    };
    let _ = probe.set_read_timeout(Some(Duration::from_millis(150)));
    let key = att.key.clone();
    let id = att.id;
    // Catch anything this loop might panic on, so the attachment is always
    // released.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&mut ws, &att, &project, &name, &dir);
    }));
    crate::session::detach(&key, id);
    let _ = ws.close(None);
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "{}".into())
}

fn run(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    att: &crate::session::Attachment,
    project: &str,
    name: &str,
    dir: &Path,
) {
    let send = |ws: &mut tungstenite::WebSocket<TcpStream>, v: serde_json::Value| -> bool {
        ws.send(Message::Text(json(&v).into())).is_ok()
    };
    if !send(ws, serde_json::json!({"t": "hello", "session": name})) {
        return;
    }
    let proc_root = Path::new("/proc");
    let home = home();
    let mut transcript: Option<Transcript> = None;
    let mut source_said = String::new();
    let mut sent_rev = 0u64;
    let mut last_live: Option<Live> = None;
    let mut last_locate: Option<Instant> = None;
    let mut last_live_at = Instant::now() - Duration::from_secs(10);
    // The subagent being looked at, by the call that started it, and what
    // was last said about it. One at a time: the client keeps the stack.
    let mut agent: Option<AgentView> = None;
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                if let Some(want) = agent_request(&t) {
                    agent = want.map(AgentView::new);
                    continue;
                }
                let reply = handle_in(&t, &att.key);
                if let Some(err) = reply {
                    if !send(ws, serde_json::json!({"t": "error", "msg": err})) {
                        return;
                    }
                }
                last_live_at = Instant::now() - Duration::from_secs(10);
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return,
        }
        let mut output = false;
        loop {
            match att.rx.try_recv() {
                Ok(_) => output = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let _ = send(ws, serde_json::json!({"t": "ended", "why": "the terminal ended"}));
                    return;
                }
            }
        }
        // Which transcript, re-asked every few seconds: `/clear` and
        // `--resume` change it under a running Claude.
        if last_locate.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true) {
            last_locate = Some(Instant::now());
            let src = match &home {
                Some(h) => locate(proc_root, h, project, name),
                None => Source::Unknown("HOME is not set".into()),
            };
            let (state, note) = match &src {
                Source::Found(p) => {
                    if transcript.as_ref().map(|t| t.path() != p.as_path()).unwrap_or(true) {
                        transcript = Some(Transcript::open(p.clone(), project, dir));
                        sent_rev = 0;
                    }
                    ("found", String::new())
                }
                Source::NoClaude => ("none", "No Claude is running in this terminal.".to_string()),
                Source::Unknown(why) => ("unknown", why.clone()),
                Source::Empty => ("empty", "No messages yet.".to_string()),
            };
            let said = format!("{state}:{note}");
            if said != source_said {
                source_said = said;
                if !send(ws, serde_json::json!({"t": "source", "state": state, "note": note})) {
                    return;
                }
            }
        }
        if let Some(t) = transcript.as_mut() {
            t.poll();
            let (reset, items, rev) = t.take_changes(sent_rev);
            if reset || !items.is_empty() {
                sent_rev = rev;
                if !send(ws, serde_json::json!({"t": "entries", "reset": reset, "items": items})) {
                    return;
                }
            }
        }
        if let Some(a) = agent.as_mut() {
            let main = transcript.as_ref().map(|t| t.path().to_path_buf());
            for v in a.tick(home.as_deref(), main.as_deref(), project, dir) {
                if !send(ws, v) {
                    return;
                }
            }
        }
        if output || last_live_at.elapsed() > Duration::from_millis(1500) {
            last_live_at = Instant::now();
            if let Some(snap) = crate::session::screen_snapshot(&att.key) {
                let live = live_from_rows(&snap.rows);
                if last_live.as_ref() != Some(&live) {
                    let mut v = serde_json::to_value(&live).unwrap_or_default();
                    v["t"] = "live".into();
                    if !send(ws, v) {
                        return;
                    }
                    last_live = Some(live);
                }
            }
        }
    }
}

/// A subagent opened in the chat: its transcript, read the same way as the
/// main one, and its record re-read now and then so "running" turns into
/// "finished" without the client asking again.
struct AgentView {
    tool: String,
    transcript: Option<Transcript>,
    sent_rev: u64,
    said: Option<(bool, Option<AgentInfo>)>,
    last_look: Option<Instant>,
}

impl AgentView {
    fn new(tool: String) -> AgentView {
        AgentView { tool, transcript: None, sent_rev: 0, said: None, last_look: None }
    }

    /// What to send this tick: the record when it changed, then entries.
    fn tick(&mut self, home: Option<&Path>, main: Option<&Path>, project: &str, dir: &Path) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        if self.last_look.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true) {
            self.last_look = Some(Instant::now());
            let found = match (home, main) {
                (Some(h), Some(m)) => locate_agent(h, m, &self.tool),
                _ => None,
            };
            if let Some((p, _)) = &found {
                if self.transcript.as_ref().map(|t| t.path() != p.as_path()).unwrap_or(true) {
                    self.transcript = Some(Transcript::open_subagent(p.clone(), project, dir));
                    self.sent_rev = 0;
                }
            }
            let said = (found.is_some(), found.map(|(_, i)| i));
            if self.said.as_ref() != Some(&said) {
                out.push(serde_json::json!({
                    "t": "agent", "tool": self.tool, "found": said.0, "info": said.1,
                }));
                self.said = Some(said);
            }
        }
        if let Some(t) = self.transcript.as_mut() {
            t.poll();
            let (reset, items, rev) = t.take_changes(self.sent_rev);
            if reset || !items.is_empty() {
                self.sent_rev = rev;
                out.push(serde_json::json!({
                    "t": "agent_entries", "tool": self.tool, "reset": reset, "items": items,
                }));
            }
        }
        out
    }
}

/// `Some(want)` when `text` is the client choosing which subagent to look at
/// (`Some(None)` to look at none); `None` when it is anything else. A tool id
/// of the wrong shape reads as none, never as a lookup.
fn agent_request(text: &str) -> Option<Option<String>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("agent") {
        return None;
    }
    Some(v.get("tool").and_then(|t| t.as_str()).filter(|t| valid_tool_id(t)).map(str::to_string))
}

/// One message from the client. `Some(reason)` is a refusal to show.
fn handle_in(text: &str, key: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<In>(text) else { return Some("unreadable message".into()) };
    let Some(snap) = crate::session::screen_snapshot(key) else {
        return Some("the terminal is not attached here".into());
    };
    let write = |b: &[u8]| crate::session::write_input(key, b).err();
    match msg {
        In::Send { text } => {
            if text.len() > MAX_SEND_BYTES {
                return Some("message too long".into());
            }
            if text.trim().is_empty() {
                return None;
            }
            if let Some(e) = write(&paste_bytes(&text, snap.bracketed_paste)) {
                return Some(e);
            }
            // Its own write, after a beat: a carriage return inside the same
            // write as the paste end can be taken as part of the paste.
            std::thread::sleep(Duration::from_millis(60));
            write(b"\r")
        }
        In::Key { k } => match key_bytes(&k, snap.application_cursor) {
            Some(b) => write(b),
            None => Some(format!("unknown key {k:?}")),
        },
        In::Choose { index, label } => {
            let live = live_from_rows(&snap.rows);
            let Some(p) = live.prompt else {
                return Some("that question is no longer on screen".into());
            };
            match p.options.get(index) {
                Some(o) if o.label == label => write(&choice_bytes(p.selected, index, snap.application_cursor)),
                _ => Some("the question changed; look again".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    // Captured from Claude Code 2.1.330 in tmux at 44x40: the last screen of
    // an AskUserQuestion with two questions. It is the one menu with no hint
    // footer, so without its own anchor it rendered no buttons and the ask
    // could not be submitted from the chat.
    const REVIEW: &str = "\
────────────────────────────────────────────
←  ☒ Color options ☒ Size options ✔ Submit →


Review your answers

 ● Which color do you prefer?
   → Red
 ● Which size do you prefer?
   → Small

❯ 1. Submit answers
  2. Cancel
";

    #[test]
    fn the_review_of_a_multi_question_ask_is_a_menu() {
        let p = live_from_rows(&rows(REVIEW)).prompt.expect("the review screen must parse as a menu");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Submit answers", "Cancel"]);
        assert_eq!(p.selected, 0);
        assert_eq!(p.question, "Review your answers");
        assert_eq!(p.context.first().map(String::as_str), Some("● Which color do you prefer?"));
        assert_eq!(p.context.last().map(String::as_str), Some("→ Small"));
    }

    #[test]
    fn review_words_without_the_menu_under_them_are_not_a_menu() {
        // The heading quoted in a transcript, with a ❯ list above it but the
        // screen not ending in the review's "2. Cancel".
        let screen = "\
Review your answers

❯ 1. Submit answers
  2. Cancel

● Done, submitted.
";
        assert!(live_from_rows(&rows(screen)).prompt.is_none());
    }

    // Captured from Claude Code 2.1.327 in tmux at 80x30 (the permission
    // prompt and the trust dialog) and 46x30 (AskUserQuestion), trailing
    // blank rows trimmed.
    const PERMISSION: &str = "\
❯ create a file named hello.txt containing the word hi, using the Write tool

● Write(/tmp/claude-1001/-home-claude-projects-roost-56b84660-5f85-4…/hello.txt)

────────────────────────────────────────────────────────────────────────────────
 Opened changes in roost ⧉
 …e-projects-roost-56b84660-5f85-4803-8c5f-4c222a475e24/scratchpad/cc/hello.txt
 Do you want to create hello.txt?
 ❯ 1. Yes
   2. Yes, and switch to accept edits (auto-approve file edits and common file
      commands) for this session; Yes, and always allow access to
      /tmp/claude-1001/-home-claude-projects-roost-56b84660-5f85-4803-8c5f-4c22
      2a475e24/scratchpad/cc for this session (shift+tab)
   3. No

 Esc to cancel · Tab to amend
";

    const ASK_NARROW: &str = "\
❯ Use the AskUserQuestion tool to ask me
  which color I prefer, options red, green or
  blue. Do nothing else.
──────────────────────────────────────────────
 ☐ Color

Which color do you prefer?

  1. Red
     Choose red
❯ 2. Green
     Choose green
  3. Blue
     Choose blue
  4. Type something.
──────────────────────────────────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to
cancel
";

    const TRUST: &str = "\
────────────────────────────────────────────────────────────────────────────────
 Accessing workspace:

 /tmp/claude-1001/scratchpad/cc

 Quick safety check: Is this a project you created or one you trust? (Like your
 own code, a well-known open source project, or work from your team). If not,
 take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    const IDLE: &str = "\
✻ Brewed for 1s · done 11:41 PM

──────────────────────────────────────────────
❯
──────────────────────────────────────────────
  ⏸ manual mode on · ? for shortcuts
";

    const WORKING: &str = "\
❯ run the shell command: ls -la /etc/hostname

  Listing 1 directory… (ctrl+o to expand)
  ⎿  $ ls -la /etc/hostname

✢ Proofing… (1s · ↓ 72 tokens)

────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · ← for agents
                                                            ◐ medium · /effort
";

    // A Bash permission prompt at 44 columns, captured from Claude Code
    // 2.1.330 in default permission mode. The command sits in a `│` gutter
    // between `╌` rules, above a blank row over the question.
    //
    // Revert-checks performed, all restored: not calling `prompt_body` fails
    // both Bash tests (the old bare "proceed? yes/no"); dropping its `╌`
    // fallback fails the scrolled-off one; keeping the `│` gutter fails both.
    const BASH_NARROW: &str = "\
● Bash(mkdir -p demo && cd demo && printf
      \"a\\nb\\nc\\n\" > one.txt && printf
      \"x\\ny\\n\" > two.txt && ls -la && wc -l
      one.txt two.txt)
  ⎿  Waiting…

────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for
 you — choose \"switch to auto mode\" below
 Write two demo files
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ mkdir -p demo && cd demo && printf
 │ \"a\\nb\\nc\\n\" > one.txt && printf \"x\\ny\\n\"
 │ > two.txt && ls -la && wc -l one.txt
 │ two.txt
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Compound command contains cd with write
 │ operation - manual approval required to
 │ prevent path resolution bypass

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for mkdir -p
      demo, \"printf \\\"a\\\\nb\\\\nc\\\\n\\\"\", and
      \"printf \\\"x\\\\ny\\\\n\\\"\" commands in
      /tmp/claude-1001/-home-claude-project
      s-roost/56b84660-5f85-4803-8c5f-4c222
      a475e24/scratchpad/permcap
   3. Yes, and switch to auto mode · auto
      mode handles these prompts for you
   4. No

 Esc to cancel · Tab to amend
";

    #[test]
    fn a_bash_prompt_carries_the_command_it_asks_about() {
        let p = live_from_rows(&rows(BASH_NARROW)).prompt.expect("a menu");
        assert_eq!(p.question, "Do you want to proceed?");
        assert_eq!(p.context.first().map(String::as_str), Some("Bash command"), "{:?}", p.context);
        assert!(p.context.iter().any(|c| c == "Write two demo files"), "the description: {:?}", p.context);
        assert!(p.context.iter().any(|c| c == "mkdir -p demo && cd demo && printf"),
            "the command, out of its gutter: {:?}", p.context);
        assert!(p.context.iter().any(|c| c.starts_with("Compound command contains cd")), "the warning: {:?}", p.context);
        assert!(!p.context.iter().any(|c| c.contains('╌') || c.starts_with('│') || c.is_empty()), "{:?}", p.context);
        assert!(!p.context.iter().any(|c| c.contains("Waiting")), "nothing from above the box: {:?}", p.context);
        assert_eq!(p.options.len(), 4);
    }

    /// A command taller than the screen pushes the box's top edge off it.
    /// The `╌` rule still says this is a tool prompt, so the screen's top
    /// is the box's.
    #[test]
    fn a_bash_prompt_whose_top_edge_scrolled_off_still_carries_the_command() {
        let all = rows(BASH_NARROW);
        let edge = all.iter().position(|r| is_rule(r)).expect("the fixture has its edge");
        let p = live_from_rows(&all[edge + 1..]).prompt.expect("a menu");
        assert_eq!(p.context.first().map(String::as_str), Some("Bash command"), "{:?}", p.context);
        assert!(p.context.iter().any(|c| c == "> two.txt && ls -la && wc -l one.txt"), "{:?}", p.context);
    }

    /// Without a rule of either kind nothing marks where the prompt starts,
    /// and the transcript above must not be read as its question.
    #[test]
    fn a_menu_with_no_box_above_takes_no_transcript_as_context() {
        let screen = "\
● Earlier the assistant said something long.

 Pick one?
 ❯ 1. This
   2. That

 Esc to cancel
";
        let p = live_from_rows(&rows(screen)).prompt.expect("a menu");
        assert_eq!(p.question, "Pick one?");
        assert!(p.context.is_empty(), "{:?}", p.context);
    }

    #[test]
    fn reads_a_permission_prompt() {
        let live = live_from_rows(&rows(PERMISSION));
        let p = live.prompt.expect("a menu");
        assert_eq!(p.question, "Do you want to create hello.txt?");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels[0], "Yes");
        assert!(labels[1].starts_with("Yes, and switch to accept edits"), "{labels:?}");
        assert_eq!(labels[2], "No");
        assert_eq!(p.options.len(), 3);
        assert!(p.options[1].detail.contains("for this session (shift+tab)"), "{:?}", p.options[1]);
        assert_eq!(p.selected, 0);
        assert!(!live.busy);
    }

    #[test]
    fn reads_an_ask_user_question_menu_at_phone_width() {
        let p = live_from_rows(&rows(ASK_NARROW)).prompt.expect("a menu");
        assert_eq!(p.question, "Which color do you prefer?");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Red", "Green", "Blue", "Type something.", "Chat about this"]);
        assert_eq!(p.options[0].detail, "Choose red");
        // The ❯ is on Green, not on the first row — selection follows the mark.
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn reads_an_unnumbered_menu() {
        let p = live_from_rows(&rows(TRUST)).prompt.expect("a menu");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["No, exit", "Yes, I trust this folder"]);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn the_input_box_is_not_a_menu() {
        // Its ❯ sits between two rules; without that check this would be read
        // as a one-option menu — and without the footer anchor, as anything.
        let live = live_from_rows(&rows(IDLE));
        assert_eq!(live.prompt, None);
        assert!(!live.busy);
        assert_eq!(live.mode.as_deref(), Some("manual mode on"));
    }

    #[test]
    fn working_is_busy_with_no_menu() {
        let live = live_from_rows(&rows(WORKING));
        assert!(live.busy);
        assert_eq!(live.prompt, None);
        assert_eq!(live.mode.as_deref(), Some("auto mode on"));
    }

    #[test]
    fn a_menu_without_its_footer_offers_nothing() {
        // The failure direction this module chose: no anchor, no buttons.
        let cut: Vec<String> = rows(PERMISSION).into_iter().filter(|r| !r.contains("Esc to cancel")).collect();
        assert_eq!(live_from_rows(&cut).prompt, None);
    }

    #[test]
    fn numbers_out_of_order_are_not_a_menu() {
        let broken = PERMISSION.replace("3. No", "4. No");
        assert_eq!(live_from_rows(&rows(&broken)).prompt, None);
    }

    #[test]
    fn a_blank_screen_says_so() {
        let live = live_from_rows(&vec![String::new(); 24]);
        assert!(live.blank);
        assert_eq!(live.prompt, None);
    }

    #[test]
    fn choice_moves_from_the_selection_then_presses_enter() {
        assert_eq!(choice_bytes(1, 3, false), b"\x1b[B\x1b[B\r");
        assert_eq!(choice_bytes(2, 0, true), b"\x1bOA\x1bOA\r");
        assert_eq!(choice_bytes(1, 1, false), b"\r");
    }

    #[test]
    fn a_paste_cannot_smuggle_an_escape() {
        let b = paste_bytes("hi\x1b[201~\x03 there\nline2", true);
        assert_eq!(b, b"\x1b[200~hi[201~ there\nline2\x1b[201~".to_vec());
        // Revert check: without the control filter the inner ESC survives and
        // the paste ends early.
        assert_eq!(b.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
    }

    fn line(v: serde_json::Value) -> serde_json::Value {
        v
    }

    #[test]
    fn a_transcript_becomes_messages_tools_and_results() {
        let dir = Path::new("/p/proj");
        let mut t = Transcript::open(PathBuf::from("/nonexistent.jsonl"), "proj", dir);
        let lines = [
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"run ls"}})),
            line(serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":""}]}})),
            line(serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"Edit","input":{"file_path":"/p/proj/src/a.rs","old_string":"x","new_string":"y"}}]}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done, see `src/a.rs`."}]}})),
            line(serde_json::json!({"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: ignore"}})),
            line(serde_json::json!({"type":"user","isSidechain":true,"message":{"role":"user","content":"subagent prompt"}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-args>opus</command-args>"}})),
            line(serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}})),
        ];
        for l in &lines {
            t.ingest(l);
        }
        let (reset, items, _) = t.take_changes(0);
        assert!(reset);
        let kinds: Vec<&Item> = items.iter().map(|e| &e.item).collect();
        assert_eq!(kinds.len(), 5, "{kinds:?}");
        assert_eq!(kinds[0], &Item::User { text: "run ls".into() });
        match kinds[1] {
            Item::Tool { name, summary, path, result, agent } => {
                assert_eq!(name, "Edit");
                assert_eq!(agent, &None, "an Edit starts no subagent");
                assert_eq!(summary, "src/a.rs");
                assert_eq!(path.as_deref(), Some("src/a.rs"));
                assert_eq!(result, &Some(ToolResult { ok: true, text: "ok".into() }));
            }
            other => panic!("{other:?}"),
        }
        match kinds[2] {
            Item::Assistant { html } => assert!(html.contains("<code>src/a.rs</code>"), "{html}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(kinds[3], &Item::User { text: "/model opus".into() });
        assert_eq!(kinds[4], &Item::Note { text: "Interrupted".into() });
    }

    /// The subagent's own file is all sidechain lines. Read as the main
    /// conversation it is empty — which is right there and was the whole
    /// subagent view before this — and read as a subagent's it is the
    /// subagent's conversation.
    ///
    /// Revert-checked, restored: dropping the `!self.sidechain &&` guard
    /// leaves the subagent's transcript empty and this fails on its count.
    #[test]
    fn a_subagent_file_reads_as_its_own_conversation() {
        let lines = [
            serde_json::json!({"type":"user","isSidechain":true,"agentId":"a1","message":{"role":"user","content":"Find the config loader"}}),
            serde_json::json!({"type":"assistant","isSidechain":true,"agentId":"a1","message":{"content":[{"type":"tool_use","id":"tu9","name":"Grep","input":{"pattern":"load_config"}}]}}),
            serde_json::json!({"type":"user","isSidechain":true,"agentId":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"tu9","content":"src/config.rs"}]}}),
            serde_json::json!({"type":"assistant","isSidechain":true,"agentId":"a1","message":{"content":[{"type":"text","text":"It is in src/config.rs."}]}}),
        ];
        let mut main = Transcript::open(PathBuf::from("/m.jsonl"), "proj", Path::new("/p"));
        let mut sub = Transcript::open_subagent(PathBuf::from("/s.jsonl"), "proj", Path::new("/p"));
        for l in &lines {
            main.ingest(l);
            sub.ingest(l);
        }
        assert!(main.take_changes(0).1.is_empty(), "the main view still skips sidechain lines");
        let items: Vec<Item> = sub.take_changes(0).1.into_iter().map(|e| e.item).collect();
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(items[0], Item::User { text: "Find the config loader".into() });
        assert!(matches!(&items[1], Item::Tool { name, result: Some(r), .. } if name == "Grep" && r.text == "src/config.rs"), "{items:?}");
    }

    /// The card a subagent's call makes carries the call's id; no other card
    /// does. `Task` is the same tool under its older name.
    #[test]
    fn an_agent_call_is_marked_with_the_id_that_opens_it() {
        let mut t = Transcript::open(PathBuf::from("/m.jsonl"), "proj", Path::new("/p"));
        t.ingest(&serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"toolu_01Abc","name":"Agent","input":{"description":"Survey the tests","prompt":"..."}},
            {"type":"tool_use","id":"toolu_02Def","name":"Task","input":{"description":"Older name","prompt":"..."}},
            {"type":"tool_use","id":"toolu_03Ghi","name":"Bash","input":{"command":"ls"}},
            {"type":"tool_use","id":"../../etc","name":"Agent","input":{"description":"odd id"}}
        ]}}));
        let agents: Vec<Option<String>> = t
            .take_changes(0)
            .1
            .into_iter()
            .map(|e| match e.item {
                Item::Tool { agent, .. } => agent,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(agents, [Some("toolu_01Abc".into()), Some("toolu_02Def".into()), None, None]);
    }

    /// A `~/.claude/projects/<dir>/<session>.jsonl` with its `subagents/`.
    fn agent_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let tdir = d.path().join(".claude/projects/-p-proj");
        let subs = tdir.join("sess1/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let main = tdir.join("sess1.jsonl");
        std::fs::write(&main, "").unwrap();
        // A sibling first, so the lookup has to match rather than take the
        // first record it sees.
        std::fs::write(subs.join("agent-a0.meta.json"), r#"{"toolUseId":"toolu_other","description":"other"}"#).unwrap();
        std::fs::write(subs.join("agent-a0.jsonl"), "").unwrap();
        std::fs::write(
            subs.join("agent-a1.meta.json"),
            r#"{"agentType":"Explore","description":"Survey the tests","toolUseId":"toolu_01Abc"}"#,
        )
        .unwrap();
        std::fs::write(subs.join("agent-a1.jsonl"), "{}\n").unwrap();
        // Nested: started by a call inside a1's own transcript, recorded in
        // the same directory.
        std::fs::write(
            subs.join("agent-a2.meta.json"),
            r#"{"agentType":"general-purpose","description":"Deeper","toolUseId":"toolu_09Nest","finished":true}"#,
        )
        .unwrap();
        std::fs::write(subs.join("agent-a2.jsonl"), "{}\n").unwrap();
        let main = std::fs::canonicalize(main).unwrap();
        (d, main, subs)
    }

    #[test]
    fn locate_agent_finds_the_subagent_its_call_started() {
        let (d, main, subs) = agent_fixture();
        let (p, info) = locate_agent(d.path(), &main, "toolu_01Abc").expect("found");
        assert_eq!(p, std::fs::canonicalize(subs.join("agent-a1.jsonl")).unwrap());
        assert_eq!(info, AgentInfo { description: "Survey the tests".into(), agent_type: "Explore".into(), finished: false });
        let (p2, info2) = locate_agent(d.path(), &main, "toolu_09Nest").expect("a nested one too");
        assert!(p2.ends_with("agent-a2.jsonl"));
        assert!(info2.finished);
        assert_eq!(locate_agent(d.path(), &main, "toolu_nobody"), None);
    }

    /// The id comes from the browser. A shape the API never mints is not
    /// looked up at all, and a record whose transcript resolves outside
    /// `~/.claude/projects` is not followed there.
    ///
    /// Revert-checked, restored: returning `dir.join(..)` without `confined`
    /// fails the symlink case.
    #[test]
    fn locate_agent_refuses_odd_ids_and_files_outside_claude_projects() {
        let (d, main, subs) = agent_fixture();
        // A record whose toolUseId is literally the odd string, so only the
        // shape check can be what refuses it.
        std::fs::write(subs.join("agent-x.meta.json"), r#"{"toolUseId":"../x"}"#).unwrap();
        std::fs::write(subs.join("agent-x.jsonl"), "").unwrap();
        assert_eq!(locate_agent(d.path(), &main, "../x"), None);
        assert_eq!(locate_agent(d.path(), &main, ""), None);

        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.jsonl");
        std::fs::write(&secret, "{}\n").unwrap();
        std::fs::write(subs.join("agent-ev.meta.json"), r#"{"toolUseId":"toolu_evil"}"#).unwrap();
        std::os::unix::fs::symlink(&secret, subs.join("agent-ev.jsonl")).unwrap();
        assert_eq!(locate_agent(d.path(), &main, "toolu_evil"), None);
    }

    #[test]
    fn the_agent_message_is_told_apart_from_the_others() {
        assert_eq!(agent_request(r#"{"t":"agent","tool":"toolu_01Abc"}"#), Some(Some("toolu_01Abc".into())));
        assert_eq!(agent_request(r#"{"t":"agent","tool":null}"#), Some(None));
        assert_eq!(agent_request(r#"{"t":"agent","tool":"../../x"}"#), Some(None), "an odd id means none");
        assert_eq!(agent_request(r#"{"t":"send","text":"hi"}"#), None);
        assert_eq!(agent_request("not json"), None);
    }

    #[test]
    fn a_late_tool_result_is_sent_as_a_change_to_its_entry() {
        let mut t = Transcript::open(PathBuf::from("/x.jsonl"), "proj", Path::new("/p"));
        t.ingest(&serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Bash","input":{"command":"make"}}]}}));
        let (_, first, rev) = t.take_changes(0);
        assert_eq!(first.len(), 1);
        t.ingest(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"boom","is_error":true}]}}));
        let (reset, again, _) = t.take_changes(rev);
        assert!(!reset);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, first[0].id, "the same entry, upserted");
        match &again[0].item {
            Item::Tool { result: Some(r), .. } => assert!(!r.ok && r.text == "boom"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_transcript_is_read_incrementally_and_a_partial_line_waits() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.jsonl");
        std::fs::write(&p, "{\"type\":\"user\",\"message\":{\"content\":\"one\"}}\n{\"type\":\"user\",\"mess").unwrap();
        let mut t = Transcript::open(p.clone(), "proj", d.path());
        assert!(t.poll());
        let (_, items, rev) = t.take_changes(0);
        assert_eq!(items.len(), 1);
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"age\":{\"content\":\"two\"}}\n").unwrap();
        assert!(t.poll());
        let (_, items, _) = t.take_changes(rev);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item, Item::User { text: "two".into() });
    }

    #[test]
    fn locate_follows_the_pid_record_and_refuses_a_reused_pid() {
        let d = tempfile::tempdir().unwrap();
        let proc_root = d.path().join("proc");
        let home = d.path().join("home");
        let pid = 4242u32;
        let pdir = proc_root.join(pid.to_string());
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("comm"), "claude\n").unwrap();
        std::fs::write(pdir.join("environ"), b"ROOST_PROJECT=proj\0ROOST_SESSION=term1\0".as_slice()).unwrap();
        let stat = |start: &str| format!("{pid} (claude) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0\n");
        std::fs::write(pdir.join("stat"), stat("777")).unwrap();
        let id = "0a78c354-edc5-4371-80a9-5cf073fed569";
        let cwd = "/w/proj";
        std::fs::create_dir_all(home.join(".claude/sessions")).unwrap();
        std::fs::write(
            home.join(".claude/sessions").join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{cwd}","startedAt":1,"procStart":"777"}}"#),
        )
        .unwrap();
        let tdir = home.join(".claude/projects/-w-proj");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join(format!("{id}.jsonl")), "").unwrap();
        match locate(&proc_root, &home, "proj", "term1") {
            Source::Found(p) => assert!(p.ends_with(format!("-w-proj/{id}.jsonl")), "{p:?}"),
            other => panic!("{other:?}"),
        }
        // Another terminal's Claude is not this one's.
        assert_eq!(locate(&proc_root, &home, "proj", "term2"), Source::NoClaude);
        // The same pid, started at a different time: the record is someone
        // else's. Revert check: dropping the procStart comparison makes this
        // return Found.
        std::fs::write(pdir.join("stat"), stat("778")).unwrap();
        match locate(&proc_root, &home, "proj", "term1") {
            Source::Unknown(why) => assert!(why.contains("not found"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_transcript_outside_claude_projects_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
        let outside = d.path().join("secret.jsonl");
        std::fs::write(&outside, "").unwrap();
        assert_eq!(confined(&home, &outside), None);
        let link = home.join(".claude/projects/evil.jsonl");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(confined(&home, &link), None, "a symlink out is still out");
        let inside = home.join(".claude/projects/ok.jsonl");
        std::fs::write(&inside, "").unwrap();
        assert!(confined(&home, &inside).is_some());
    }

    #[test]
    fn tool_summaries_name_the_thing_acted_on() {
        let dir = Path::new("/p");
        assert_eq!(tool_summary("Bash", &serde_json::json!({"command":"ls -la\nmore"}), dir).0, "ls -la");
        assert_eq!(tool_summary("Read", &serde_json::json!({"file_path":"/etc/passwd"}), dir), ("/etc/passwd".into(), None));
        assert_eq!(tool_summary("Read", &serde_json::json!({"file_path":"/p/../etc/x"}), dir).1, None);
        assert_eq!(
            tool_summary("AskUserQuestion", &serde_json::json!({"questions":[{"question":"Which?"}]}), dir).0,
            "Which?"
        );
    }
}
