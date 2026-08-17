//! The `deadlight notify` subcommand.
//!
//! It runs *inside* the terminal deadlight is already reading, so there is no
//! IPC here and no socket to connect to: printing the escape sequence to the
//! controlling terminal IS the mechanism. That is also why this never binds a
//! port or touches the notice store.
use std::io::Write;

/// `/dev/tty` rather than stdout, because the intended caller is a Claude
/// Code hook and Claude Code captures hook stdout — a hook printing to stdout
/// would be swallowed before it ever reached the PTY. `/dev/tty` is the
/// controlling terminal, which is the PTY deadlight owns.
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

pub fn run_notify(args: &[String]) -> i32 {
    let Some(title) = args.first() else {
        eprintln!("usage: deadlight notify <title> [body]");
        return 2;
    };
    let body = args.get(1).map(String::as_str).unwrap_or("");
    let seq = notify_sequence(title, body);
    if let Some(mut f) = tty() {
        if f.write_all(seq.as_bytes()).is_ok() && f.flush().is_ok() {
            return 0;
        }
    }
    // Fall back to stdout for the interactive case where it is a tty anyway.
    let mut out = std::io::stdout();
    if out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok() {
        return 0;
    }
    // Loud, not silent: a misconfigured hook that quietly did nothing would
    // look exactly like a feature that does not work.
    eprintln!("deadlight notify: no controlling terminal and stdout unavailable");
    1
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
}
