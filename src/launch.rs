//! What the ✻ button starts, and whether this host can start it.
//!
//! A launch is keystrokes typed into the shell a new terminal gets anyway,
//! not a different command handed to dtach. That keeps the user's shell when
//! the program exits, inherits the `PATH` their `.bashrc`/`.zshrc` builds
//! (which is where `claude` usually lives), and if the program is missing
//! they see `command not found` in a live shell rather than a tab that
//! closes itself. The cost is one assumption: that the shell does not flush
//! tty input while it starts. bash's readline and zsh both set the terminal
//! with `TCSADRAIN`, which keeps typed-ahead input, so the bytes wait in the
//! PTY's input queue until the shell reads its first line. Measured, not
//! assumed: `tests/browser/claudeterm.mjs` types through a real dtach into a
//! real `bash -l` the instant the PTY exists, and the program runs at the
//! first prompt. The browser README's "typing before the prompt" trap was
//! observed for keystrokes sent from the *browser*; why those are lost and
//! these are kept was not investigated here, so that trap still stands for
//! tests, and this path has its own test rather than an argument.
//!
//! Whether the program is installed is checked once, at startup, in the
//! background, and the ✻ button is hidden only when that check positively
//! said no. It is asked of a login interactive shell — the shell a terminal
//! gets — not of resh's own environment, because a service's `PATH` is not
//! the user's: here `claude` lives in `~/.local/bin`, which `.profile` adds
//! and `systemd` does not.
//!
//! To offer another program: add a `proto::Launch` variant, give it a row in
//! `keystrokes` and `program`, and probe it from `probe_all_in_background`.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::proto::Launch;

/// The bytes typed into the shell for a launch. `\r` is Enter on a PTY.
///
/// `session_id` is typed only when it is exactly a lowercase v4-shaped uuid:
/// it lands on a command line, and the validation is the whole boundary
/// between "resh chose this id" and "something typed a shell command". A
/// malformed id degrades to the bare program, which still starts.
pub fn keystrokes(launch: Launch, session_id: Option<&str>) -> Vec<u8> {
    match (launch, session_id) {
        (Launch::Claude, Some(id)) if valid_session_id(id) => {
            format!("claude --session-id {id}\r").into_bytes()
        }
        (Launch::Claude, _) => b"claude\r".to_vec(),
    }
}

/// `8-4-4-4-12` lowercase hex, and nothing else.
pub fn valid_session_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
    })
}

/// A fresh v4 uuid from `/dev/urandom`. `None` when the kernel would not
/// give sixteen bytes — the launch then goes without an id rather than with
/// a weak one.
pub fn new_session_id() -> Option<String> {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}

/// The executable `keystrokes` runs, as `command -v` would look it up.
fn program(launch: Launch) -> &'static str {
    match launch {
        Launch::Claude => "claude",
    }
}

/// What the startup check found. Three outcomes, not two: the check can
/// fail to run at all — no shell, a profile that dies, a hang — and that is
/// not the same as the shell looking and not finding. Only `Absent` hides
/// the button; see `offered_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Present,
    Absent,
    Unknown,
}

/// Whether the button for `launch` is shown. `Unknown` offers it: the cost
/// of a wrong button is `command not found` in a live shell, the cost of a
/// wrong absence is a feature that silently isn't there.
pub fn offered_for(a: Availability) -> bool {
    a != Availability::Absent
}

const FOUND: &str = "ROOST_FOUND";
const MISSING: &str = "ROOST_MISSING";

/// Asks `shell` (as a login interactive shell, the way a terminal gets it)
/// whether `program` is on its `PATH`. The answer is a sentinel on stdout,
/// not an exit code: `command -v` exits 1 under bash and 127 under dash for
/// the same "not found", and a profile that fails can exit anything, so the
/// code alone cannot tell "looked and said no" from "never got to look".
/// The sentinels can: one of them is printed only if our command ran to its
/// end, and neither is printed if it did not.
pub fn probe(shell: &str, program: &str, timeout: Duration) -> Availability {
    let script = format!("command -v {program} >/dev/null 2>&1 && echo {FOUND} || echo {MISSING}");
    let mut child = match std::process::Command::new(shell)
        .args(["-lic", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Availability::Unknown,
    };
    // Drained on its own thread so a chatty profile cannot fill the pipe and
    // turn a fast shell into a timed-out one.
    let mut out = child.stdout.take().expect("stdout was piped");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = out.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            // Timed out, or could not even ask: either way we do not know.
            // Killing closes the pipe, which lets the reader thread finish.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Availability::Unknown;
            }
        }
    }
    let out = reader.join().unwrap_or_default();
    if out.lines().any(|l| l.trim() == FOUND) {
        Availability::Present
    } else if out.lines().any(|l| l.trim() == MISSING) {
        Availability::Absent
    } else {
        Availability::Unknown
    }
}

/// What the startup check found for each launch. `Unknown` until the check
/// finishes, so a page rendered during the first second of a process shows
/// the button — which is the right side to err on (see `offered_for`).
static CLAUDE: Mutex<Availability> = Mutex::new(Availability::Unknown);

fn slot(launch: Launch) -> &'static Mutex<Availability> {
    match launch {
        Launch::Claude => &CLAUDE,
    }
}

pub fn availability(launch: Launch) -> Availability {
    *slot(launch).lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the button for `launch` is shown on pages rendered now.
pub fn offered(launch: Launch) -> bool {
    offered_for(availability(launch))
}

/// The launches a page rendered now may offer, by their wire names — what
/// `render::workspace_page` puts in `data-launches` and the client sends
/// back in `NewTerminal.launch`.
pub fn offered_names() -> Vec<&'static str> {
    ALL.iter().copied().filter(|l| offered(*l)).map(wire_name).collect()
}

/// Every launch there is, for the probe and the page. Adding a variant to
/// `proto::Launch` without adding it here is a compile error in `wire_name`
/// but a silent omission here, so keep the two together.
const ALL: &[Launch] = &[Launch::Claude];

/// The name `proto::Launch` deserializes from. Spelled out rather than
/// derived, so the page and the wire cannot drift apart without
/// `a_wire_name_round_trips_through_the_intent` noticing.
pub fn wire_name(launch: Launch) -> &'static str {
    match launch {
        Launch::Claude => "claude",
    }
}

/// Runs every launch's probe on a background thread at startup. Not on the
/// request path: a login shell is tens of milliseconds when the profile is
/// quiet and unbounded when it is not, and nothing a browser asks for should
/// wait on it. Logged once, because a hidden button has no other way to say
/// why it is hidden.
pub fn probe_all_in_background() {
    std::thread::spawn(|| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        for l in ALL.iter().copied() {
            let a = probe(&shell, program(l), Duration::from_secs(10));
            *slot(l).lock().unwrap_or_else(|e| e.into_inner()) = a;
            match a {
                Availability::Present => {}
                Availability::Absent => eprintln!(
                    "resh: `{}` is not on {shell}'s login PATH; the ✻ button is hidden",
                    program(l)
                ),
                Availability::Unknown => eprintln!(
                    "resh: could not ask {shell} whether `{}` is installed; offering the ✻ button anyway",
                    program(l)
                ),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_is_typed_with_its_session_id_and_enter() {
        // Revert-checked: when keystrokes returns b"claude\r" unconditionally,
        // this fails at src/launch.rs:226:9: assertion `left == right` failed,
        // left: [99, 108, 97, 117, 100, 101, 13] (claude\r),
        // right: [99, 108, 97, 117, 100, 101, 32, 45, 45, 115, ...] (claude --session-id ...\r).
        let id = "0123abcd-0123-4abc-8abc-0123456789ab";
        assert_eq!(
            keystrokes(Launch::Claude, Some(id)),
            format!("claude --session-id {id}\r").into_bytes()
        );
    }

    #[test]
    fn without_an_id_the_bare_command_is_typed() {
        // Revert-checked: when fallback arm returns b"claude --session-id broken-id\r",
        // this fails at src/launch.rs:239:9: assertion `left == right` failed,
        // left: [99, 108, 97, 117, 100, 101, 32, 45, 45, ...] (claude --session-id broken-id\r),
        // right: [99, 108, 97, 117, 100, 101, 13] (claude\r).
        assert_eq!(keystrokes(Launch::Claude, None), b"claude\r".to_vec());
    }

    #[test]
    fn a_malformed_id_is_never_typed() {
        // The id lands on a command line. Anything that is not exactly a
        // uuid falls back to the bare command — the metacharacter must be
        // absent from what is typed, not merely quoted.
        // Revert-checked: when guard on valid_session_id(id) is removed,
        // this fails at src/launch.rs:253:9: assertion `left == right` failed,
        // left: [99, 108, 97, 117, 100, 101, 32, 45, 45, 115, 101, 115, 115, 105, 111, 110, 45, 105, 100, 32, ..., 59, ...] (contains injected semicolon),
        // right: [99, 108, 97, 117, 100, 101, 13] (claude\r).
        let bad = "0123abcd-0123-4abc-8abc-0123456789ab; rm -rf ~";
        let typed = keystrokes(Launch::Claude, Some(bad));
        assert_eq!(typed, b"claude\r".to_vec());
        assert!(!String::from_utf8_lossy(&typed).contains(';'));
        assert!(!valid_session_id(bad));
        assert!(!valid_session_id("0123ABCD-0123-4abc-8abc-0123456789ab"), "uppercase is not the form claude prints");
        assert!(valid_session_id("0123abcd-0123-4abc-8abc-0123456789ab"));
    }

    #[test]
    fn a_minted_id_is_a_valid_v4_uuid() {
        // Revert-checked: when nibble-setting lines are dropped from new_session_id,
        // the version nibble (position 14, should be '4') and variant nibble (position 19, should be 8-b)
        // are random. The test fails ~93.75% of runs on the variant check at src/launch.rs:268:9:
        // panicked at assertion `matches!(...), "variant nibble: <uuid>"`, e.g. uuid "266d78b4-1a5d-4884-20fd-279048848c18"
        // with variant nibble '2' instead of 8-b. Mint 8 ids to make deterministic (probability of all
        // 8 passing accidentally is ~6.3e-14).
        for _ in 0..8 {
            let id = new_session_id().expect("/dev/urandom is readable on a test host");
            assert!(valid_session_id(&id), "{id}");
            assert_eq!(&id[14..15], "4", "version nibble: {id}");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "variant nibble: {id}");
        }
        assert_ne!(new_session_id().unwrap(), new_session_id().unwrap(), "two mints differ");
    }

    use std::time::Duration;

    const T: Duration = Duration::from_secs(10);

    /// A fake `$SHELL`: a script that ignores its arguments and does `body`.
    fn fake_shell(dir: &std::path::Path, body: &str) -> String {
        let p = dir.join("shell.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.to_string_lossy().into_owned()
    }

    // The real /bin/sh, not a fake, for the two answers that matter: these
    // prove the probe's command line is one a shell actually understands and
    // that `command -v` is what decides, not an exit code we guessed at.
    #[test]
    fn a_program_on_the_login_shells_path_is_present() {
        assert_eq!(probe("/bin/sh", "sh", T), Availability::Present);
    }

    #[test]
    fn a_program_the_login_shell_cannot_find_is_absent() {
        assert_eq!(probe("/bin/sh", "resh-no-such-program-4b1c", T), Availability::Absent);
    }

    // "I could not determine X" is a third outcome, never folded into
    // "X is false" (CLAUDE.md). Every way the check can fail to run must
    // come back Unknown, because Unknown keeps the button and Absent hides
    // it — and hiding a working feature on a broken check is the wrong way
    // round.
    #[test]
    fn a_shell_that_cannot_be_started_is_unknown_not_absent() {
        assert_eq!(probe("/nonexistent/resh-test-shell", "sh", T), Availability::Unknown);
    }

    #[test]
    fn a_shell_that_dies_before_answering_is_unknown_not_absent() {
        let d = tempfile::tempdir().unwrap();
        let sh = fake_shell(d.path(), "echo 'profile exploded' >&2; exit 2");
        assert_eq!(probe(&sh, "sh", T), Availability::Unknown);
    }

    #[test]
    fn a_shell_that_hangs_is_unknown_and_does_not_hold_startup_hostage() {
        let d = tempfile::tempdir().unwrap();
        let sh = fake_shell(d.path(), "sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(probe(&sh, "sh", Duration::from_millis(300)), Availability::Unknown);
        assert!(started.elapsed() < Duration::from_secs(5), "the timeout must be honoured");
    }

    #[test]
    fn a_wire_name_round_trips_through_the_intent() {
        for l in ALL.iter().copied() {
            let json = format!(r#"{{"t":"NewTerminal","pane":3,"launch":"{}"}}"#, wire_name(l));
            match crate::proto::decode(&json) {
                Ok(crate::proto::Intent::NewTerminal { launch: Some(got), .. }) => assert_eq!(got, l),
                other => panic!("{json} must decode to a launch of {l:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_button_is_offered_unless_the_check_positively_said_no() {
        assert!(offered_for(Availability::Present));
        assert!(offered_for(Availability::Unknown), "a failed check must not hide a working feature");
        assert!(!offered_for(Availability::Absent));
    }
}
