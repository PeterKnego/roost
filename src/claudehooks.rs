//! The commands roost owns in a project's `.claude/settings.local.json`.
//!
//! Two of them. `roost claude-hook` (`Kind::Notify`) answers `SessionStart`,
//! `Notification` and `Stop` because Claude Code raises no notification of
//! its own. `roost worktree-hook` (`Kind::Worktree`) answers `WorktreeCreate`
//! alone, with a much longer timeout, because it waits on a human in a
//! browser rather than writing a line to a terminal. This module is the only
//! code that reads or writes that file, and for either kind it touches
//! exactly the entries whose command is that kind's exact string — a user's
//! other hooks, other keys and their order survive every write. It writes
//! the *local* settings file, the one Claude Code keeps personal and
//! gitignored, never the committed one and never the global one: a clone of
//! the repo must not inherit a hook that runs roost.
//!
//! Reading has three outcomes, not two. A file that exists but cannot be
//! parsed is `Unknown`, and `Unknown` refuses to write: rewriting a file we
//! could not read is how a hand-edited settings file gets destroyed.
//!
//! The symlink checks in `load` and `set_kind` are advisory, not the
//! enforcement: a same-user race can swap a symlink in between the
//! `symlink_metadata` check and the later open. What actually stops a write
//! from landing outside the project is `OpenOptions::create_new` on the
//! backup (and now the temp-file) path, which refuses to open through an
//! existing symlink at all rather than trusting a stat taken moments
//! earlier.

use std::path::Path;

/// The command `Kind::Notify` installs. Ownership is this exact string,
/// nothing looser: a user who writes their own `roost notify` hook keeps it.
pub const COMMAND: &str = "roost claude-hook";
/// The command `Kind::Worktree` installs.
pub const WORKTREE_COMMAND: &str = "roost worktree-hook";
/// `SessionStart` is not for notifying — it produces no message — but for the
/// id it carries. `Stop` fires when a turn *ends*, so a session interrupted
/// before its first turn completed would never announce itself, and that is
/// the session worth recovering. See `claudesess`.
const EVENTS: [&str; 3] = ["SessionStart", "Notification", "Stop"];
/// `WorktreeCreate` is the only event `Kind::Worktree` ever installs under;
/// see `merge`'s doc comment for why *disabling* still looks further.
const WORKTREE_EVENTS: [&str; 1] = ["WorktreeCreate"];
const REL: &str = ".claude/settings.local.json";

/// The two owned commands. Each is a distinct file-shape (its own events,
/// command string and timeout) sharing one reader/writer so both get the
/// same atomic-write, symlink-refusal and backup behaviour for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Notify,
    Worktree,
}

impl Kind {
    fn command(self) -> &'static str {
        match self {
            Kind::Notify => COMMAND,
            Kind::Worktree => WORKTREE_COMMAND,
        }
    }
    /// The events this kind installs itself under. Not the only events its
    /// command may ever be *found* under — see `merge`'s disable path.
    fn events(self) -> &'static [&'static str] {
        match self {
            Kind::Notify => &EVENTS,
            Kind::Worktree => &WORKTREE_EVENTS,
        }
    }
    /// Notify's hook writes one line to a terminal and returns; Worktree's
    /// waits on a human clicking a dialog in a browser, bounded by
    /// `hub::ASK_TIMEOUT` (60s) plus margin for the round trip, so 5s would
    /// make Claude Code kill it before roost ever answers.
    fn timeout(self) -> u64 {
        match self {
            Kind::Notify => 5,
            Kind::Worktree => 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookState {
    /// Every one of the kind's own events carries that kind's entry.
    Present,
    /// No file, or fewer than all of the kind's own events carry one.
    Absent,
    /// The file exists but could not be read or is not the shape this
    /// module knows how to rewrite. The string says why, for the UI.
    Unknown(String),
}

/// The parsed document, `None` for a missing file, `Err` for `Unknown`.
fn load(project_dir: &Path) -> Result<Option<serde_json::Value>, String> {
    // `.claude` may not exist yet for a project roost has never touched.
    // `safe_resolve_parent` requires its parent to exist so it can
    // canonicalize it, so that absence is checked here, first, and read as
    // Absent rather than as Unknown (a directory that genuinely cannot be
    // read — EACCES, say — falls through to the `Err` arm below instead).
    match std::fs::symlink_metadata(project_dir.join(".claude")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{REL}: cannot stat .claude: {e}")),
        Ok(_) => {}
    }
    // Confined like every other read of a project-relative path.
    let p = crate::projects::safe_resolve_parent(project_dir, REL)
        .map_err(|e| format!("{REL}: cannot confine: {e}"))?;
    // A symlinked settings file is refused outright rather than followed:
    // `set` below rewrites through this same path with `rename`, which
    // would silently replace whatever the link points at — including a
    // file outside the project — with roost's own, stamped with a fresh
    // regular file's mode instead of the link target's.
    match std::fs::symlink_metadata(&p) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!("{REL}: settings.local.json is a symlink"))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{REL}: cannot stat: {e}")),
    }
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{REL}: cannot read: {e}")),
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{REL}: not valid JSON: {e}"))?;
    if !v.is_object() {
        return Err(format!("{REL}: top level is not an object"));
    }
    if let Some(h) = v.get("hooks") {
        if !h.is_object() {
            return Err(format!("{REL}: \"hooks\" is not an object"));
        }
        // An event key whose value isn't an array is refused, not skipped:
        // `event_has_ours`/`merge` treat "absent" and "not an array" alike,
        // so silently accepting this made enable a no-op `Ok` that
        // installed nothing under that event. Checked for both kinds' events
        // here, in one shared `load`, because a single document can be read
        // by either kind — a malformed `WorktreeCreate` must not read as
        // `Absent` to `state_of(.., Kind::Worktree)` any more than a
        // malformed `Notification` does to `Kind::Notify`.
        let hobj = h.as_object().expect("checked is_object above");
        for event in EVENTS.iter().chain(WORKTREE_EVENTS.iter()) {
            if let Some(ev) = hobj.get(*event) {
                if !ev.is_array() {
                    return Err(format!("{REL}: hooks.{event} is not an array"));
                }
            }
        }
    }
    Ok(Some(v))
}

fn is_ours(entry: &serde_json::Value, command: &str) -> bool {
    entry.get("command").and_then(|c| c.as_str()) == Some(command)
}

fn our_entry(kind: Kind) -> serde_json::Value {
    serde_json::json!({ "type": "command", "command": kind.command(), "timeout": kind.timeout() })
}

/// Whether `event`'s array in `hooks` holds an entry matching `command` in
/// any group.
fn event_has_ours(hooks: &serde_json::Value, event: &str, command: &str) -> bool {
    hooks
        .get(event)
        .and_then(|a| a.as_array())
        .map_or(false, |groups| {
            groups.iter().any(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array())
                    .map_or(false, |entries| entries.iter().any(|e| is_ours(e, command)))
            })
        })
}

/// `state(dir)` / `set(dir, on)` are `Kind::Notify`'s half of `state_of` /
/// `set_kind` — kept as the original names and signatures so every call site
/// and every pre-existing test from before `Kind` existed is untouched.
pub fn state(project_dir: &Path) -> HookState {
    state_of(project_dir, Kind::Notify)
}

pub fn state_of(project_dir: &Path, kind: Kind) -> HookState {
    match load(project_dir) {
        Err(why) => HookState::Unknown(why),
        Ok(None) => HookState::Absent,
        Ok(Some(v)) => {
            let hooks = v.get("hooks").cloned().unwrap_or(serde_json::Value::Null);
            if kind.events().iter().all(|e| event_has_ours(&hooks, e, kind.command())) {
                HookState::Present
            } else {
                HookState::Absent
            }
        }
    }
}

/// Adds or removes `kind`'s entries in `doc`, touching nothing else.
///
/// Enable only ever writes under `kind.events()` — `Kind::Worktree` installs
/// under `WorktreeCreate` alone, never anywhere else, by construction.
/// Disable is asymmetric on purpose: it scans *every* event key already
/// present in `hooks`, not just `kind.events()`, so a stray entry carrying
/// the kind's command under the wrong event (a hand edit, or a future bug)
/// is still something disable can clean up. "Never installed elsewhere"
/// is a promise about install; disable's job is removal of whatever matches,
/// wherever it is.
fn merge(doc: &mut serde_json::Value, kind: Kind, on: bool) {
    let obj = doc.as_object_mut().expect("load guarantees an object");
    if on {
        let hooks = obj
            .entry("hooks")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        for event in kind.events() {
            if event_has_ours(hooks, event, kind.command()) {
                continue;
            }
            let groups = hooks
                .as_object_mut()
                .expect("load guarantees an object")
                .entry(*event)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(arr) = groups.as_array_mut() {
                // A group of our own, never an entry inside a foreign group:
                // disabling then removes it without deciding what to do with
                // a group we share.
                arr.push(serde_json::json!({ "hooks": [our_entry(kind)] }));
            }
        }
        return;
    }
    // Every container is pruned only if *this call* emptied it (final
    // review e). An empty array, an empty group or an empty `hooks` that
    // was already empty is the user's — a placeholder, a half-finished hand
    // edit — and dropping it rewrote a file roost had nothing to remove from.
    let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else { return };
    let mut emptied_an_event = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(|a| a.as_array_mut()) else { continue };
        let mut emptied_a_group = false;
        groups.retain_mut(|g| {
            let Some(entries) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) else { return true };
            let before = entries.len();
            entries.retain(|e| !is_ours(e, kind.command()));
            let emptied_here = before > 0 && entries.is_empty();
            emptied_a_group |= emptied_here;
            !emptied_here
        });
        if emptied_a_group && groups.is_empty() {
            hooks.remove(&event);
            emptied_an_event = true;
        }
    }
    if emptied_an_event && hooks.is_empty() {
        obj.remove("hooks");
    }
}

/// A per-process counter appended to the temp filename alongside the pid:
/// roost is thread-per-connection, so two `set` calls racing in the same
/// process (two browser tabs toggling the same project at once) must not
/// collide on one `settings.local.json.tmp.<pid>` name.
static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Enables or disables roost's hooks in the project's local settings.
///
/// Refuses on `Unknown`: the file is left byte-for-byte alone (this
/// includes a symlinked settings file, refused by `load`). Otherwise a
/// no-op `merge` — already in the requested state, or nothing to remove —
/// returns without touching the filesystem at all: no rewrite, no backup,
/// no `.claude` directory conjured up for a `set(.., false)` that had
/// nothing to disable. A real change writes temp-then-rename in the same
/// directory (a crash mid-write leaves the old file intact, and a reader
/// never sees a half-written one), keeps an existing file's mode, and
/// copies the pre-roost file to `.bak` the first time it replaces one —
/// never again, so the backup stays the state before roost touched
/// anything, and never through a symlink planted at the backup's own path.
pub fn set(project_dir: &Path, on: bool) -> Result<(), String> {
    set_kind(project_dir, Kind::Notify, on)
}

/// `set`'s generalisation: see its doc comment for the write path, which is
/// the same for either `kind`.
pub fn set_kind(project_dir: &Path, kind: Kind, on: bool) -> Result<(), String> {
    let original = load(project_dir)?;
    let mut doc = original
        .clone()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    merge(&mut doc, kind, on);

    // Nothing to write: either the file already carries what `on` asks
    // for, or there was no file and `on` is `false`, which has nothing to
    // remove. Comparing to `original` (not a freshly-loaded empty object)
    // is what distinguishes "no file" from "file exists but empty".
    let unchanged = match &original {
        Some(v) => *v == doc,
        None => !on,
    };
    if unchanged {
        return Ok(());
    }

    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    text.push('\n');

    // Confined like every other creation path: the parent is canonicalised
    // and the final component validated, because the file may not exist yet.
    let dir = crate::projects::safe_resolve_parent(project_dir, ".claude")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{REL}: cannot create .claude: {e}"))?;
    let target = crate::projects::safe_resolve_parent(project_dir, REL)?;
    // `symlink_metadata`, not `exists()`: "cannot look" must not read as
    // "absent" and skip the backup of a file that is there.
    let existing = match std::fs::symlink_metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{REL}: cannot stat: {e}")),
    };

    // The pre-roost file, kept once and never overwritten.
    if let Some(meta) = &existing {
        let bak = dir.join("settings.local.json.bak");
        match std::fs::symlink_metadata(&bak) {
            // Nothing at `bak` yet: write it by creating the destination
            // ourselves (`create_new` never follows a symlink — unlike
            // `fs::copy`, which opens the destination for writing and so
            // would happily write through a symlink planted at `bak`,
            // landing outside the project if the link pointed there).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let orig = std::fs::read(&target)
                    .map_err(|e| format!("{REL}: cannot read for backup: {e}"))?;
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                // Set atomically at creation, not chmod'ed afterward: a
                // window where the backup briefly sits at whatever mode
                // `open` defaults to (typically world-readable) would leak
                // a 0600 settings file's bytes to it, the same widening
                // the temp-file `set_permissions` call above exists to
                // avoid, one file over.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                    opts.mode(meta.permissions().mode());
                }
                let mut f = opts
                    .open(&bak)
                    .map_err(|e| format!("{REL}.bak: cannot create backup: {e}"))?;
                use std::io::Write as _;
                f.write_all(&orig)
                    .map_err(|e| format!("{REL}.bak: cannot write backup: {e}"))?;
            }
            // Something is already there — an earlier backup, or a
            // symlink (dangling or not) that isn't ours to touch: leave it
            // exactly as it is either way.
            Ok(_) => {}
            Err(e) => return Err(format!("{REL}.bak: cannot stat backup: {e}")),
        }
    }

    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("settings.local.json.tmp.{}.{n}", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        // Set atomically at creation, not chmod'ed afterward: `fs::write`
        // followed by a separate `set_permissions` left a window where a
        // 0600 settings file's bytes sat in a temp file at whatever mode
        // `create` defaults to (typically world-readable) — the same
        // widening the backup path above avoids the same way.
        #[cfg(unix)]
        if let Some(meta) = &existing {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(meta.permissions().mode());
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("{REL}: cannot create temp file: {e}"))?;
        use std::io::Write as _;
        if let Err(e) = f.write_all(text.as_bytes()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{REL}: cannot write: {e}"));
        }
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{REL}: cannot replace: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn proj() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
    fn read(p: &Path) -> String {
        std::fs::read_to_string(p.join(".claude/settings.local.json")).unwrap()
    }
    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.join(".claude")).unwrap();
        std::fs::write(p.join(".claude/settings.local.json"), s).unwrap();
    }
    fn read_json(p: &Path) -> serde_json::Value {
        serde_json::from_str(&read(p)).unwrap()
    }

    const OURS: &str = r#"{ "type": "command", "command": "roost claude-hook", "timeout": 5 }"#;

    /// Verified this can fail: making `load`'s `NotFound` arm return `Err`
    /// instead of `Ok(None)` failed the first assertion with `left: Unknown(
    /// ".claude/settings.local.json: cannot read: No such file or directory
    /// (os error 2)") right: Absent`.
    #[test]
    fn a_missing_file_is_absent_and_enable_writes_exactly_our_entries() {
        let d = proj();
        assert_eq!(state(d.path()), HookState::Absent);
        set(d.path(), true).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{OURS}]}}],"Notification":[{{"hooks":[{OURS}]}}],"Stop":[{{"hooks":[{OURS}]}}]}}}}"#
        )).unwrap();
        let got: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(got, expected);
        assert!(read(d.path()).ends_with("}\n"), "two-space pretty JSON with a trailing newline");
        assert_eq!(state(d.path()), HookState::Present);
    }

    #[test]
    fn enable_is_idempotent_byte_for_byte() {
        let d = proj();
        set(d.path(), true).unwrap();
        let once = read(d.path());
        set(d.path(), true).unwrap();
        assert_eq!(read(d.path()), once);
    }

    /// Foreign content survives: other hooks on Stop, an unrelated event,
    /// unrelated keys, and their order.
    ///
    /// Verified this can fail: dropping `features = ["preserve_order"]` from
    /// `serde_json` in Cargo.toml failed the "top-level order kept" assertion
    /// with `left: ["hooks", "permissions", "zeta"] right: ["permissions",
    /// "hooks", "zeta"]` — serde_json's default map sorts keys alphabetically.
    #[test]
    fn enable_keeps_every_foreign_byte_of_content_and_key_order() {
        let d = proj();
        write(d.path(), r#"{
  "permissions": { "allow": ["Bash(ls *)"] },
  "hooks": {
    "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ],
    "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "lint" } ] } ]
  },
  "zeta": 1
}
"#);
        set(d.path(), true).unwrap();
        let s = read(d.path());
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls *)");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done", "foreign Stop group first");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], "roost claude-hook", "ours appended");
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(v["hooks"]["Notification"][0]["hooks"][0]["command"], "roost claude-hook");
        assert_eq!(v["zeta"], 1);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["permissions", "hooks", "zeta"], "top-level order kept");
        let hooks: Vec<&str> = v["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            hooks,
            ["Stop", "PreToolUse", "SessionStart", "Notification"],
            "existing event order kept; the two missing ones appended in EVENTS order"
        );
        assert_eq!(state(d.path()), HookState::Present);
    }

    /// Verified this can fail: replacing `entries.retain(|e| !is_ours(e))`
    /// with `entries.clear()` in `merge` panicked on the "foreign entry kept"
    /// assertion's own `.unwrap()`: clearing the whole group (not just our
    /// entry) emptied it, which then got pruned, so indexing
    /// `v["hooks"]["Stop"][0]["hooks"]` returned `Null` and `.as_array()`
    /// gave `None` — "called `Option::unwrap()` on a `None` value" at that
    /// line.
    #[test]
    fn disable_removes_only_ours_and_prunes_what_it_empties() {
        let d = proj();
        write(d.path(), &format!(r#"{{
  "hooks": {{
    "Stop": [ {{ "hooks": [ {{ "type": "command", "command": "say done" }}, {OURS} ] }} ],
    "Notification": [ {{ "hooks": [ {OURS} ] }} ],
    "SessionStart": [ {{ "hooks": [ {OURS} ] }} ]
  }},
  "other": true
}}
"#));
        assert_eq!(state(d.path()), HookState::Present);
        set(d.path(), false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(), 1, "foreign entry kept");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert!(v["hooks"].get("Notification").is_none(), "an emptied event is dropped");
        assert_eq!(v["other"], true);
        assert_eq!(state(d.path()), HookState::Absent);

        // Nothing but ours: `hooks` itself goes.
        let d = proj();
        set(d.path(), true).unwrap();
        set(d.path(), false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert!(v.get("hooks").is_none(), "{v}");
    }

    /// Final-review e (M2): disable drops an event array or a group only
    /// when *this call* emptied it. A foreign empty `"PreToolUse": []`, a
    /// foreign group with an empty `hooks`, a foreign non-array value and a
    /// foreign empty `"hooks": {}` are the user's, not leftovers of ours;
    /// pruning them rewrote a hand-written file roost had nothing to remove
    /// from.
    ///
    /// Revert-checked (final fix wave): with the pre-fix `merge` restored
    /// (prune every empty array/group, and `hooks` whenever empty), fails on
    /// "a disable with nothing of ours to remove rewrote the file" — the
    /// three foreign empties were pruned and the file rewritten.
    #[test]
    fn disable_keeps_foreign_containers_it_did_not_empty() {
        // Nothing of ours anywhere: the file must not be touched at all.
        let d = proj();
        let foreign = r#"{
  "hooks": {
    "PreToolUse": [],
    "PostToolUse": [ { "matcher": "Bash", "hooks": [] } ],
    "UserPromptSubmit": "not an array"
  }
}
"#;
        write(d.path(), foreign);
        set(d.path(), false).unwrap();
        assert_eq!(read(d.path()), foreign, "a disable with nothing of ours to remove rewrote the file");
        set_kind(d.path(), Kind::Worktree, false).unwrap();
        assert_eq!(read(d.path()), foreign, "nor may the worktree kind's disable");

        let d = proj();
        write(d.path(), "{\"hooks\": {}}\n");
        set(d.path(), false).unwrap();
        assert_eq!(read(d.path()), "{\"hooks\": {}}\n", "a foreign empty hooks object is not ours to drop");

        // Ours beside them: ours goes, and every foreign container stays as
        // it was, value for value.
        let d = proj();
        write(d.path(), &format!(r#"{{
  "hooks": {{
    "PreToolUse": [],
    "PostToolUse": [ {{ "matcher": "Bash", "hooks": [] }} ],
    "UserPromptSubmit": "not an array",
    "Stop": [ {{ "hooks": [] }}, {{ "hooks": [ {OURS} ] }} ],
    "Notification": [ {{ "hooks": [ {OURS} ] }} ]
  }}
}}
"#));
        set(d.path(), false).unwrap();
        let v = read_json(d.path());
        assert_eq!(v["hooks"]["PreToolUse"], serde_json::json!([]), "foreign empty array kept");
        assert_eq!(v["hooks"]["PostToolUse"], serde_json::json!([{ "matcher": "Bash", "hooks": [] }]), "foreign empty group kept");
        assert_eq!(v["hooks"]["UserPromptSubmit"], "not an array", "foreign non-array kept");
        assert_eq!(v["hooks"]["Stop"], serde_json::json!([{ "hooks": [] }]), "the group we emptied goes, the foreign empty one beside it stays");
        assert!(v["hooks"].get("Notification").is_none(), "an event this call emptied is dropped");
        assert_eq!(state(d.path()), HookState::Absent);
    }

    /// Verified this can fail: replacing `.all(` with `.any(` in `state`
    /// failed the first assertion with `left: Present right: Absent`.
    #[test]
    fn one_event_present_is_absent_and_enable_adds_only_the_missing_one() {
        let d = proj();
        write(d.path(), &format!(r#"{{"hooks":{{"Stop":[{{"hooks":[{OURS}]}}]}}}}"#));
        assert_eq!(state(d.path()), HookState::Absent);
        set(d.path(), true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1, "not duplicated");
        assert_eq!(v["hooks"]["Notification"].as_array().unwrap().len(), 1);
        assert_eq!(state(d.path()), HookState::Present);
    }

    /// Unknown refuses both directions and touches nothing.
    #[test]
    fn invalid_json_is_unknown_and_never_written() {
        let d = proj();
        write(d.path(), "{ not json");
        assert!(matches!(state(d.path()), HookState::Unknown(_)));
        let e = set(d.path(), true).unwrap_err();
        assert!(e.contains("settings.local.json"), "{e}");
        assert!(set(d.path(), false).is_err());
        assert_eq!(read(d.path()), "{ not json");
        assert!(!d.path().join(".claude/settings.local.json.bak").exists(), "no backup of a refused write");

        let d = proj();
        write(d.path(), r#"{"hooks": 5}"#);
        assert!(matches!(state(d.path()), HookState::Unknown(_)), "hooks that is not an object");
        let d = proj();
        write(d.path(), r#"[]"#);
        assert!(matches!(state(d.path()), HookState::Unknown(_)), "a top-level array");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        write(d.path(), "{}");
        let p = d.path().join(".claude/settings.local.json");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file anyway; then this test cannot
        // arrange the condition and says so rather than passing vacuously.
        let arranged = std::fs::read_to_string(&p).is_err();
        let s = state(d.path());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        if !arranged {
            eprintln!("skipped: running as a user that can read a mode-000 file");
            return;
        }
        assert!(matches!(s, HookState::Unknown(_)), "{s:?}");
    }

    /// Verified this can fail: dropping the `if !bak.exists()` guard in
    /// `set` failed the "still the pre-roost file" assertion with `left:
    /// "{\n  \"pristine\": true\n}\n" right: "{\"pristine\": true}"` — the
    /// backup got overwritten with the post-enable, pretty-printed file
    /// instead of staying the original.
    #[test]
    fn the_backup_is_written_once_and_never_overwritten() {
        let d = proj();
        write(d.path(), r#"{"pristine": true}"#);
        set(d.path(), true).unwrap();
        let bak = d.path().join(".claude/settings.local.json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"pristine": true}"#);
        set(d.path(), false).unwrap();
        set(d.path(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"pristine": true}"#, "still the pre-roost file");

        // A file that did not exist has nothing to back up.
        let d = proj();
        set(d.path(), true).unwrap();
        assert!(!d.path().join(".claude/settings.local.json.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writes_are_atomic_and_keep_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        write(d.path(), "{}");
        let p = d.path().join(".claude/settings.local.json");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        set(d.path(), true).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join(".claude")).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// `fs::copy` used to carry the source's mode onto the backup; the
    /// `OpenOptions::create_new` replacement (added for the symlink fix
    /// above) defaults to the OS's own create mode — typically `0666 &
    /// !umask`, world-readable — unless the source's mode is set on the
    /// `OpenOptions` itself before `open`, atomically, so there is never a
    /// window where a 0600 settings file's bytes sit in a more-permissive
    /// `.bak`.
    ///
    /// Verified this can fail: dropping the `#[cfg(unix)] { .. opts.mode(..)
    /// }` block so the backup was created with the `OpenOptions` default
    /// failed with:
    /// ```text
    /// assertion `left == right` failed
    ///   left: 436
    ///  right: 384
    /// ```
    /// (436 = 0o664, 384 = 0o600 — this host's umask is `0002`, so
    /// `OpenOptions`'s own default of 0o666 came out group- and
    /// world-readable instead of matching the settings file's 0o600).
    #[cfg(unix)]
    #[test]
    fn the_backup_is_created_with_the_source_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        write(d.path(), r#"{"pristine": true}"#);
        let p = d.path().join(".claude/settings.local.json");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        set(d.path(), true).unwrap();
        let bak = d.path().join(".claude/settings.local.json.bak");
        assert_eq!(std::fs::metadata(&bak).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// A dangling symlink at the backup's own path is something that
    /// exists (`symlink_metadata` succeeds on the link itself, target or
    /// no target), so `set` leaves it alone and proceeds rather than
    /// following it — `fs::copy` would have opened it for writing and
    /// created the link's target, possibly outside the project entirely.
    ///
    /// Verified this can fail: restoring the old `if !bak.exists()` guard
    /// (which follows the symlink to check whether *its target* exists,
    /// found nothing, and so proceeded to `fs::copy` through the link)
    /// failed with "the symlink's target must never be created" — `escaped`
    /// existed afterward, created through the link outside the project.
    #[cfg(unix)]
    #[test]
    fn a_backup_symlink_is_left_alone_not_followed() {
        let d = proj();
        write(d.path(), r#"{"pristine": true}"#);
        let outside = tempfile::tempdir().unwrap();
        let escaped = outside.path().join("escaped");
        let bak = d.path().join(".claude/settings.local.json.bak");
        std::os::unix::fs::symlink(&escaped, &bak).unwrap();

        set(d.path(), true).unwrap();

        assert!(!escaped.exists(), "the symlink's target must never be created");
        let meta = std::fs::symlink_metadata(&bak).unwrap();
        assert!(meta.file_type().is_symlink(), "the .bak path is still a symlink, untouched");
        assert_eq!(state(d.path()), HookState::Present, "set still succeeded");
    }

    /// A symlinked settings file is refused, not followed: rewriting
    /// through it via `rename` would replace whatever the link points at
    /// (which could be outside the project) with roost's own file.
    ///
    /// Verified this can fail: dropping the symlink check in `load` (so it
    /// fell straight through to `read_to_string`, which follows symlinks)
    /// failed the first assertion — `state` returned `Absent` instead of
    /// `Unknown`, panicking with just `Absent`, since the link's target
    /// parsed fine as valid, empty JSON with no hooks at all.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_settings_file_is_unknown_and_never_replaced() {
        let d = proj();
        std::fs::create_dir_all(d.path().join(".claude")).unwrap();
        let real = d.path().join("real-settings.json");
        std::fs::write(&real, "{}").unwrap();
        let link = d.path().join(".claude/settings.local.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let s = state(d.path());
        assert!(matches!(&s, HookState::Unknown(msg) if msg.contains("symlink")), "{s:?}");
        assert!(set(d.path(), true).is_err());

        // Neither the link nor its target moved.
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{}");
    }

    /// An event whose value under `hooks` isn't an array is refused, not
    /// silently skipped: `merge`'s `entry(event).or_insert_with(..)` finds
    /// the key already occupied by a non-array and does nothing with it,
    /// so treating this shape as readable turned `set(.., true)` into a
    /// silent no-op that reported `Ok` while installing nothing.
    ///
    /// Verified this can fail: dropping the per-event array check in `load`
    /// failed the first assertion — `state` returned `Absent` instead of
    /// `Unknown`, panicking with just `Absent` (the malformed `Notification`
    /// value makes `event_has_ours` false, which reads exactly like a
    /// project roost has never touched).
    #[test]
    fn a_non_array_event_is_unknown_and_never_written() {
        let d = proj();
        write(d.path(), r#"{"hooks": {"Notification": 5}}"#);
        assert!(matches!(state(d.path()), HookState::Unknown(_)), "{:?}", state(d.path()));
        assert!(set(d.path(), true).is_err());
        assert_eq!(read(d.path()), r#"{"hooks": {"Notification": 5}}"#);
    }

    /// `set` is a no-op, filesystem untouched, when there is nothing to
    /// change: disabling a project roost has never seen must not conjure
    /// `.claude/settings.local.json` into existence, and enabling an
    /// already-enabled project must not rewrite, back up, or bump the
    /// file's mtime.
    ///
    /// Verified this can fail: dropping the `if unchanged { return Ok(())
    /// }` early return failed case (a) with "no .claude directory created"
    /// — `.claude` now existed, holding a freshly written `{}` from a
    /// `set(.., false)` that had nothing to disable. (Case (b)'s mtime
    /// check never got to run in that revert, since the panic in (a) stops
    /// the test; the early return is what both cases share, so (a) alone
    /// is enough to show the code path was reached.)
    #[test]
    fn an_existing_two_event_install_reads_as_absent_and_upgrades_in_place() {
        // The user-visible effect of adding a third event. Every project that
        // had the bell on before this release has only `Notification` and
        // `Stop`, so its bell now reads off — which is honest (roost's hooks
        // are not all installed) and is what makes turning it back on install
        // the missing one. What must NOT happen is the rest of the file being
        // rewritten around it, so this asserts the foreign entry survives.
        let d = proj();
        write(
            d.path(),
            &format!(
                r#"{{"hooks":{{"Notification":[{{"hooks":[{OURS}]}}],"Stop":[{{"hooks":[{{"type":"command","command":"say done"}},{OURS}]}}]}}}}"#
            ),
        );
        assert_eq!(
            state(d.path()),
            HookState::Absent,
            "two of the three events is not all of them"
        );
        set(d.path(), true).unwrap();
        assert_eq!(state(d.path()), HookState::Present, "and enabling adds the missing one");
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(),
            2,
            "the user's own Stop hook is still there"
        );
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert_eq!(v["hooks"]["SessionStart"][0]["hooks"][0]["command"], COMMAND);
    }

    #[test]
    fn worktree_kind_installs_only_on_worktree_create_with_a_long_timeout() {
        let d = tempfile::tempdir().unwrap();
        set_kind(d.path(), Kind::Worktree, true).unwrap();
        let v = read_json(d.path());
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["WorktreeCreate"]);
        assert_eq!(v["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"], "roost worktree-hook");
        assert_eq!(v["hooks"]["WorktreeCreate"][0]["hooks"][0]["timeout"], 120);
    }

    #[test]
    fn kinds_are_independent() {
        let d = tempfile::tempdir().unwrap();
        set_kind(d.path(), Kind::Notify, true).unwrap();
        set_kind(d.path(), Kind::Worktree, true).unwrap();
        set_kind(d.path(), Kind::Notify, false).unwrap();
        assert_eq!(state_of(d.path(), Kind::Worktree), HookState::Present);
        assert_eq!(state_of(d.path(), Kind::Notify), HookState::Absent);
    }

    #[test]
    fn a_non_array_worktree_create_is_unknown_and_refuses_to_write() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), r#"{"hooks": {"WorktreeCreate": 5}}"#);
        assert!(matches!(state_of(d.path(), Kind::Worktree), HookState::Unknown(_)));
        assert!(set_kind(d.path(), Kind::Worktree, true).is_err());
        assert_eq!(read(d.path()), r#"{"hooks": {"WorktreeCreate": 5}}"#);
    }

    /// Spec: "A Worktree entry found under any event other than
    /// WorktreeCreate is left alone on enable and removed on disable (never
    /// installed elsewhere)." Plants a stray Worktree entry under `Stop`
    /// (where it has no business being), then exercises the toggle: enable
    /// must not touch it (it isn't under WorktreeCreate, so `event_has_ours`
    /// never looks there), and disable must still find and remove it, since
    /// disable's job is cleanup of whatever carries the command, not just
    /// the kind's own events.
    ///
    /// Verified this can fail: see the revert check in the module doc above
    /// `merge` — restricting disable's scan to `kind.events()` instead of
    /// every key in `hooks` leaves the stray in place after disable, failing
    /// the `"stray"` assertion below.
    #[test]
    fn a_stray_worktree_entry_elsewhere_survives_enable_and_dies_on_disable() {
        let d = proj();
        write(d.path(), r#"{"hooks": {"Stop": [{"hooks": [
            {"type":"command","command":"roost worktree-hook","timeout":120}
        ]}]}}"#);
        set_kind(d.path(), Kind::Worktree, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "roost worktree-hook", "stray left alone on enable");
        assert_eq!(v["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"], "roost worktree-hook", "and installed under its own event");

        set_kind(d.path(), Kind::Worktree, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&read(d.path())).unwrap();
        assert!(v["hooks"].get("Stop").is_none(), "stray removed on disable: {v}");
        assert!(v["hooks"].get("WorktreeCreate").is_none());
    }

    #[test]
    fn a_no_op_set_touches_nothing() {
        // (a) Disabling a project roost has never touched creates nothing.
        let d = proj();
        set(d.path(), false).unwrap();
        assert!(!d.path().join(".claude").exists(), "no .claude directory created");

        // (b) Enabling twice doesn't rewrite, back up, or bump mtime the
        // second time.
        let d = proj();
        set(d.path(), true).unwrap();
        let p = d.path().join(".claude/settings.local.json");
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        set(d.path(), true).unwrap();
        let after = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(before, after, "mtime changed on a no-op enable");
        assert!(!d.path().join(".claude/settings.local.json.bak").exists(), "no backup on a no-op enable");
    }
}
