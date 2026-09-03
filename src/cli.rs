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
/// controlling terminal, which is the PTY roost owns.
fn tty() -> Option<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open("/dev/tty").ok()
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
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else { return 0 };
    let Some((title, body)) = hook_message(&v) else { return 0 };
    let seq = notify_sequence(&title, &body);
    let mut tty_file = tty();
    match choose_sink(tty_file.is_some(), std::io::stdout().is_terminal()) {
        Sink::Tty => {
            if let Some(f) = tty_file.as_mut() {
                let _ = f.write_all(seq.as_bytes()).and_then(|_| f.flush());
            }
        }
        // Not stdout even when it is a terminal: Claude Code reads hook
        // stdout as a decision, and a sequence there would be parsed as one.
        Sink::Stdout | Sink::Nowhere => {
            eprintln!("roost claude-hook: no controlling terminal to notify through; nothing sent");
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
        // `` and `\t` are JSON escapes, so serde hands `hook_message`
        // a real ESC and a real tab; the sanitiser must strip both.
        assert!(!body.contains('\u{1b}') && !body.contains('\t'), "{body:?}");
        assert!(body.starts_with('a'), "{body:?}");
    }
}
