//! The `roost notify` subcommand.
//!
//! It runs *inside* the terminal roost is already reading, so there is no
//! IPC here and no socket to connect to: printing the escape sequence to the
//! controlling terminal IS the mechanism. That is also why this never binds a
//! port or touches the notice store.
use std::io::{IsTerminal, Write};

/// `/dev/tty` rather than stdout, because the intended caller is a Claude
/// Code hook and Claude Code captures hook stdout — a hook printing to stdout
/// would be swallowed before it ever reached the PTY. `/dev/tty` is the
/// controlling terminal, which is the PTY roost is reading.
///
/// But a hook has none. Claude Code (2.1.260) spawns hook commands in their
/// own session, so `/dev/tty` fails with ENXIO from inside one — three real
/// `Stop` runs in one session all landed on the `Nowhere` notice while the
/// same bytes written to the pty by hand were delivered. The terminal the
/// hook lost is still its ancestor's: the claude process sits on the dtach
/// pty roost reads, so the fallback climbs `/proc` to the nearest ancestor
/// with a controlling terminal and opens that device instead. Linux only;
/// elsewhere the walk finds nothing and the outcome is as before.
fn tty() -> Option<std::fs::File> {
    let open = |path: &str| std::fs::OpenOptions::new().write(true).open(path).ok();
    if let Some(f) = open("/dev/tty") {
        return Some(f);
    }
    let read_stat = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
    ancestor_tty(std::process::id(), &read_stat).and_then(|p| open(&p))
}

/// `(ppid, tty_nr)` from one `/proc/<pid>/stat` line. The comm field is
/// the process's own name in parentheses and may itself contain spaces and
/// parentheses, so the fields are counted from the *last* `)`.
pub fn parse_stat(stat: &str) -> Option<(u32, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    // After the comm: state, ppid, pgrp, session, tty_nr.
    let ppid = fields.nth(1)?.parse().ok()?;
    let tty = fields.nth(2)?.parse().ok()?;
    Some((ppid, tty))
}

/// The device path for a `tty_nr`, if it is a Unix98 pty. The kernel writes
/// the number with the major in bits 8-19 and the minor split between bits
/// 0-7 and 20-31, so `/dev/pts/300` is *not* low-byte 44. Anything that is
/// not a pty — a console, or 0 for none — is not a terminal roost is
/// reading, so it is `None` rather than a path to write into.
pub fn pts_path(tty_nr: u64) -> Option<String> {
    const UNIX98_PTY_SLAVE_MAJOR: u64 = 136;
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xff_f00);
    (major == UNIX98_PTY_SLAVE_MAJOR).then(|| format!("/dev/pts/{minor}"))
}

/// The controlling terminal of the nearest ancestor of `pid` that has one,
/// climbing parent by parent through `read_stat`. The starting process is
/// skipped: this is called precisely because it has none.
///
/// The walk never guesses past a gap, and not by a guard: each parent is
/// known only from its child's stat line, so an ancestor that cannot be
/// read or parsed is also the last one reachable, and the answer is `None`.
/// A notice in a stranger's session would be worse than none.
pub fn ancestor_tty(pid: u32, read_stat: &dyn Fn(u32) -> Option<String>) -> Option<String> {
    let (mut ppid, _) = parse_stat(&read_stat(pid)?)?;
    // Bounded, and stopped at init or a self-parented row: a hook is a few
    // levels below its terminal, never dozens.
    for _ in 0..32 {
        if ppid <= 1 {
            return None;
        }
        let (next, tty) = parse_stat(&read_stat(ppid)?)?;
        if let Some(path) = pts_path(tty) {
            return Some(path);
        }
        if next == ppid {
            return None;
        }
        ppid = next;
    }
    None
}

pub fn notify_sequence(title: &str, body: &str) -> String {
    // The parser this feeds (osc.rs) abandons a sequence outright on any
    // embedded CR/LF or ESC — that's exactly what real multi-line tool
    // output or ANSI-coloured output contains, so emitting raw text here
    // would make this command silently produce nothing, with exit status 0,
    // for precisely the inputs it exists to carry (see the module doc).
    // Reusing osc::sanitise means this can never emit a sequence its own
    // parser would reject: whatever it strips here, the parser would have
    // stripped or abandoned on anyway.
    let title = crate::osc::sanitise(title, crate::osc::MAX_TITLE);
    let body = crate::osc::sanitise(body, crate::osc::MAX_BODY);
    // Only the title's ';' is structural to the parser's own split (the
    // first three ';' delimit 777/notify/title/body; the body's own ';'s
    // are always literal) — so a ';' in the title, and only the title,
    // would shift the parse boundary into the body. Replacing it here means
    // the title/body split the parser recovers always matches what was
    // asked for.
    let title = title.replace(';', ",");
    format!("\x1b]777;notify;{title};{body}\x07")
}

/// Where the escape sequence can actually be delivered.
#[derive(Debug, PartialEq, Eq)]
pub enum Sink {
    /// The controlling terminal — the PTY roost is reading.
    Tty,
    /// stdout, but *only* when it is itself a terminal.
    Stdout,
    /// Nowhere it could possibly be read. Must not be reported as success.
    Nowhere,
}

/// Kept pure so the whole matrix is testable: whether a process has a
/// controlling terminal, and whether its stdout is one, are both environmental
/// facts a unit test cannot arrange for itself.
///
/// The case that matters is `(false, false)` — no `/dev/tty`, stdout a pipe.
/// This used to write the sequence into that pipe and return 0, which is a
/// silent no-op wearing a success exit code: an OSC sequence in a pipe has no
/// terminal to interpret it, so nothing can ever come of it. That is exactly the
/// failure this command's own module doc says it exists to prevent, and exactly
/// the shape a Claude Code *subagent* hook runs in. Verified against the real
/// binary: it printed `^[]777;notify;hook;finished^G` to the captured pipe,
/// exited 0, and delivered nothing.
///
/// `Stdout` stays a real option rather than being dropped, because the
/// fallback's stated purpose — "the interactive case where it is a tty anyway"
/// — is genuine: a process can have stdout on a terminal without holding that
/// terminal as its controlling one.
pub fn choose_sink(tty_available: bool, stdout_is_terminal: bool) -> Sink {
    if tty_available {
        Sink::Tty
    } else if stdout_is_terminal {
        Sink::Stdout
    } else {
        Sink::Nowhere
    }
}

pub fn run_notify(args: &[String]) -> i32 {
    let Some(title) = args.first() else {
        eprintln!("usage: roost notify <title> [body]");
        return 2;
    };
    let body = args.get(1).map(String::as_str).unwrap_or("");
    let seq = notify_sequence(title, body);

    let mut tty_file = tty();
    match choose_sink(tty_file.is_some(), std::io::stdout().is_terminal()) {
        Sink::Tty => {
            let f = tty_file.as_mut().expect("Tty implies the file opened");
            if f.write_all(seq.as_bytes()).is_ok() && f.flush().is_ok() {
                return 0;
            }
            eprintln!("roost notify: could not write to the controlling terminal");
            1
        }
        Sink::Stdout => {
            let mut out = std::io::stdout();
            if out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok() {
                return 0;
            }
            eprintln!("roost notify: could not write to stdout");
            1
        }
        // Loud, not silent: a misconfigured hook that quietly did nothing would
        // look exactly like a feature that does not work.
        Sink::Nowhere => {
            eprintln!(
                "roost notify: no controlling terminal, and stdout is not one either — \
                 nothing would read the sequence, so no notification was sent. \
                 This is what a hook invoked without a terminal (e.g. a subagent) looks like."
            );
            1
        }
    }
}

/// What a Claude Code hook event says to the user, or `None` for the
/// events and notification types this command deliberately ignores.
///
/// Pure, so the whole table is one unit test. The event shapes are Claude
/// Code's documented hook input: `hook_event_name` on every event,
/// `notification_type` on `Notification`, `last_assistant_message` on
/// `Stop`. Anything not matched here is silence, not an error: a hook fires
/// for every event it is registered on, and the ones registered are only
/// `Notification` and `Stop`, but a future Claude Code may send types this
/// table has never heard of.
pub fn hook_message(v: &serde_json::Value) -> Option<(String, String)> {
    let event = v.get("hook_event_name")?.as_str()?;
    let (title, body) = match event {
        "Notification" => {
            let body = match v.get("notification_type")?.as_str()? {
                "permission_prompt" => "wants permission to run a tool",
                "idle_prompt" => "is waiting for your input",
                "agent_needs_input" => "an agent needs your input",
                "elicitation_dialog" | "elicitation_url_dialog" => "is asking a question",
                _ => return None,
            };
            ("Claude needs you", body.to_string())
        }
        "Stop" => {
            // First line only, then a hard cap: a notification is a glance.
            // `sanitise` strips control characters and applies the parser's
            // own cap; the 120 here is tighter on purpose.
            let last = v.get("last_assistant_message").and_then(|m| m.as_str()).unwrap_or("");
            let line = last.lines().next().unwrap_or("");
            let clean = crate::osc::sanitise(line, crate::osc::MAX_BODY);
            ("Claude finished", clean.chars().take(120).collect())
        }
        _ => return None,
    };
    Some((title.to_string(), body))
}

/// The one thing `roost claude-hook` may put on stderr, named so the
/// integration test can assert on it without holding a second copy of the
/// wording. It held one until 2026-09-04, and when the message grew the
/// "any ancestor" clause the copies drifted: the assertion only runs where
/// stderr is *not* empty, which is only where no ancestor has a pty — never
/// on a dev box inside a roost terminal, always on CI. Green here, red
/// there, for four commits.
pub const NO_TERMINAL_NOTICE: &str =
    "roost claude-hook: no terminal on this process or any ancestor to notify through; nothing sent";

/// The `roost claude-hook` subcommand: Claude Code pipes the event as JSON on
/// stdin; this turns it into one notification, or nothing.
///
/// Always exits 0. A `Stop` hook that exits non-zero shows an error in the
/// transcript, and none of the ways this can have nothing to do is the
/// user's mistake: a Claude run outside roost (no `ROOST_NOTIFY`), an event
/// the table ignores, or a subagent's hook with no terminal (the `Nowhere`
/// sink). `roost notify` keeps its loud exit 1 for the hand-written case;
/// this command is installed into a project's settings by the bell and has
/// to be silent wherever that project is opened without roost.
pub fn run_claude_hook() -> i32 {
    use std::io::Read;
    if std::env::var_os("ROOST_NOTIFY").is_none() {
        return 0;
    }
    let mut input = String::new();
    // Bounded: this reads whatever Claude Code pipes in, a process this
    // command does not control, and an unbounded read is a memory sink for
    // no benefit — the largest legitimate payload (a `Stop` event's last
    // assistant message) is capped to 120 characters long before it is ever
    // used, so 1 MiB is already far more slack than any real event needs.
    if std::io::stdin().take(1 << 20).read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else { return 0 };
    let Some((title, body)) = hook_message(&v) else { return 0 };
    let seq = notify_sequence(&title, &body);
    let mut tty_file = tty();
    match choose_sink(tty_file.is_some(), std::io::stdout().is_terminal()) {
        Sink::Tty => {
            if let Some(f) = tty_file.as_mut() {
                // Still exit 0 either way (see the doc above): this is
                // logged, not surfaced as a hook failure, so a user who
                // looks can see the notification did not land instead of
                // it disappearing with no trace at all.
                if f.write_all(seq.as_bytes()).and_then(|_| f.flush()).is_err() {
                    eprintln!("roost claude-hook: could not write to the controlling terminal");
                }
            }
        }
        // Not stdout even when it is a terminal: Claude Code reads hook
        // stdout as a decision, and a sequence there would be parsed as one.
        Sink::Stdout | Sink::Nowhere => {
            eprintln!("{NO_TERMINAL_NOTICE}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_is_the_777_form_the_parser_accepts() {
        let s = notify_sequence("Build done", "42 tests");
        assert_eq!(s, "\x1b]777;notify;Build done;42 tests\x07");
        // The real check: our own parser must accept what we emit.
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("Build done"));
        assert_eq!(got[0].body, "42 tests");
    }

    #[test]
    fn a_semicolon_in_the_title_cannot_forge_a_body_boundary() {
        // A ';' in the title used to shift the parse boundary into the body
        // (the emitted title "a" and the body gained a spurious "b;"
        // prefix) — this is the bug the name describes. Now the title's
        // ';' is replaced before emission, so the boundary cannot shift:
        // the title/body the parser recovers matches what was asked for.
        let s = notify_sequence("a;b", "body");
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("a,b"), "the title's ';' must not survive to shift the parse");
        assert_eq!(got[0].body, "body", "the body must not gain a spurious prefix from the title");
    }

    #[test]
    fn multiline_and_ansi_input_still_produces_a_notice() {
        // The exact failure this command exists to prevent (see the module
        // doc): raw multi-line tool output or ANSI-coloured output used to
        // hit the parser's own CR/LF-abandons and ESC-abandons rules,
        // producing nothing with exit status 0. Sanitising before
        // interpolation means the emitter can no longer produce a sequence
        // its own parser rejects.
        let s = notify_sequence("Build", "line one\nline two\r\n\x1b[31mred\x1b[0m");
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1, "sanitised body must not abandon the sequence");
        assert_eq!(got[0].title.as_deref(), Some("Build"));
        assert!(
            !got[0].body.contains('\n') && !got[0].body.contains('\r') && !got[0].body.contains('\x1b'),
            "got {:?}",
            got[0].body
        );
    }

    #[test]
    fn a_missing_body_is_allowed() {
        let s = notify_sequence("just a title", "");
        let mut p = crate::osc::Parser::new();
        let got = p.feed(s.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "");
    }

    #[test]
    fn no_arguments_is_an_error_not_a_silent_success() {
        assert_ne!(run_notify(&[]), 0, "a hook with no title must fail loudly");
    }

    /// The whole delivery matrix. The `(false, false)` row is the regression:
    /// with no controlling terminal and a piped stdout, this used to write the
    /// sequence into the pipe and return 0 — a silent no-op reported as
    /// success, in precisely the shape a Claude Code subagent hook runs in.
    #[test]
    fn a_sequence_nothing_can_read_is_never_treated_as_delivered() {
        assert_eq!(choose_sink(true, true), Sink::Tty, "a controlling terminal always wins");
        assert_eq!(choose_sink(true, false), Sink::Tty, "…including when stdout is a pipe");
        assert_eq!(
            choose_sink(false, true),
            Sink::Stdout,
            "no controlling terminal but stdout IS one: the fallback is genuine and must stay"
        );
        assert_eq!(
            choose_sink(false, false),
            Sink::Nowhere,
            "no terminal anywhere: an OSC sequence written to a pipe can never be read, so this \
             must not be reported as delivered"
        );
    }

    fn msg(json: &str) -> Option<(String, String)> {
        hook_message(&serde_json::from_str(json).unwrap())
    }

    /// The table from the spec, one row per assertion, so a row that
    /// changes fails by name.
    /// Verified this can fail: changing idle_prompt to return None produces
    /// "assertion `left == right` failed: left: None, right: Some(...)".
    #[test]
    fn hook_message_maps_each_handled_event() {
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#),
            Some(("Claude needs you".into(), "wants permission to run a tool".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#),
            Some(("Claude needs you".into(), "is waiting for your input".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Notification","notification_type":"agent_needs_input"}"#),
            Some(("Claude needs you".into(), "an agent needs your input".into()))
        );
        for t in ["elicitation_dialog", "elicitation_url_dialog"] {
            assert_eq!(
                msg(&format!(r#"{{"hook_event_name":"Notification","notification_type":"{t}"}}"#)),
                Some(("Claude needs you".into(), "is asking a question".into())),
                "{t}"
            );
        }
        assert_eq!(
            msg(r#"{"hook_event_name":"Stop","last_assistant_message":"Done.\nSecond line."}"#),
            Some(("Claude finished".into(), "Done.".into()))
        );
        assert_eq!(
            msg(r#"{"hook_event_name":"Stop"}"#),
            Some(("Claude finished".into(), String::new()))
        );
    }

    /// Everything else is silence: unhandled types and events, and input
    /// that is JSON but not an object.
    #[test]
    fn hook_message_is_none_for_everything_else() {
        assert_eq!(msg(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"Notification","notification_type":"agent_completed"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"Notification"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"SubagentStop","last_assistant_message":"x"}"#), None);
        assert_eq!(msg(r#"{"hook_event_name":"PreToolUse"}"#), None);
        assert_eq!(msg(r#"{}"#), None);
        assert_eq!(msg(r#"[1,2]"#), None);
    }

    /// A glance, not a transcript: first line, at most 120 characters,
    /// control characters stripped by the same sanitiser `notify` uses.
    /// Verified this can fail: changing take(120) to take(500) produces
    /// "assertion `left == right` failed: left: 300, right: 120".
    #[test]
    fn stop_body_is_the_first_line_capped_and_sanitised() {
        let long = "x".repeat(300);
        let (_, body) = msg(&format!(r#"{{"hook_event_name":"Stop","last_assistant_message":"{long}"}}"#)).unwrap();
        assert_eq!(body.chars().count(), 120);
        let (_, body) = msg(r#"{"hook_event_name":"Stop","last_assistant_message":"a\u001b[31mb\tc"}"#).unwrap();
        // `ESC` and `\t` are JSON escapes, so serde hands `hook_message`
        // a real ESC and a real tab; the sanitiser must strip both.
        assert!(!body.contains('\u{1b}') && !body.contains('\t'), "{body:?}");
        assert!(body.starts_with('a'), "{body:?}");
    }

    // --- finding the terminal from a detached hook ---
    //
    // Claude Code 2.1.260 spawns hook commands in their own session, so a
    // hook has no controlling terminal and `/dev/tty` fails with ENXIO
    // (three real `Stop` runs in one session all logged the `Nowhere`
    // notice). The terminal the hook *would* have had is its ancestor's:
    // the claude process sits on the dtach pty roost is reading. These
    // pin the `/proc` walk that recovers it.

    /// `new_encode_dev` as Linux writes it into `/proc/<pid>/stat`:
    /// major in bits 8-19, minor split across bits 0-7 and 20-31.
    fn tty_nr(major: u64, minor: u64) -> u64 {
        (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
    }

    #[test]
    fn stat_parse_survives_a_comm_with_spaces_and_parens() {
        // comm is `(my prog))` — the only safe split is at the *last* ')'.
        let line = format!("10 (my prog)) S 9 10 10 {} 10 0 0", tty_nr(136, 5));
        assert_eq!(parse_stat(&line), Some((9, tty_nr(136, 5))));
    }

    #[test]
    fn a_pts_number_above_255_decodes_from_the_split_minor() {
        // pts/300 puts 44 in the low byte and 1 in bit 20; reading only the
        // low byte would name pts/44, someone else's terminal.
        assert_eq!(pts_path(tty_nr(136, 300)).as_deref(), Some("/dev/pts/300"));
        assert_eq!(pts_path(tty_nr(136, 5)).as_deref(), Some("/dev/pts/5"));
    }

    #[test]
    fn no_terminal_and_a_console_are_both_not_a_roost_terminal() {
        assert_eq!(pts_path(0), None, "tty_nr 0 is no controlling terminal");
        // /dev/tty1 is major 4: a console cannot be a pty roost is reading.
        assert_eq!(pts_path(tty_nr(4, 1)), None);
    }

    /// A fake process table: pid → (ppid, tty_nr), rendered as stat lines.
    fn table(rows: &[(u32, u32, u64)]) -> impl Fn(u32) -> Option<String> + '_ {
        move |pid| {
            rows.iter()
                .find(|(p, _, _)| *p == pid)
                .map(|(p, pp, t)| format!("{p} (x) S {pp} 1 1 {t} 0 0 0"))
        }
    }

    #[test]
    fn ancestor_walk_takes_the_nearest_terminal() {
        // hook(10) → sh(9) → claude(8, pts/5) → bash(7, pts/3) → dtach(6)
        let rows = [(10, 9, 0), (9, 8, 0), (8, 7, tty_nr(136, 5)), (7, 6, tty_nr(136, 3)), (6, 1, 0)];
        assert_eq!(ancestor_tty(10, &table(&rows)).as_deref(), Some("/dev/pts/5"));
    }

    #[test]
    fn ancestor_walk_starts_above_the_hook_itself() {
        // The hook's own stat has no tty by construction; the walk must not
        // return early on it. With only the hook in the table, nothing.
        let rows = [(10, 1, 0)];
        assert_eq!(ancestor_tty(10, &table(&rows)), None);
    }

    #[test]
    fn an_unreadable_ancestor_ends_the_walk_with_nothing() {
        // 9 is missing from the table, and 8 above it has a terminal. This
        // pins the contract, not a branch: no mutation of the walk makes it
        // reach 8, because 8 is only named by 9's line. (Revert-check: a
        // version that treated an unparsable parent as "no tty, climb on"
        // still passed — it could not learn 9's parent either.) It would
        // fail only if the walk gained another way to find parents, which
        // is the change that would need this test.
        let rows = [(10, 9, 0), (8, 1, tty_nr(136, 5))];
        assert_eq!(ancestor_tty(10, &table(&rows)), None);
    }

    #[test]
    fn a_cycle_or_pid_1_ends_the_walk() {
        assert_eq!(ancestor_tty(10, &table(&[(10, 10, 0)])), None, "self-parented");
        assert_eq!(ancestor_tty(10, &table(&[(10, 1, 0), (1, 0, 0)])), None, "init has no tty");
    }
}
