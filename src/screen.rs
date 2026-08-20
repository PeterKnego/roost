//! Which screen buffer a session is on, and what a newly attached client has
//! to be told to be on the same one.
//!
//! A full-screen app — Claude Code, `vim`, `less` — declares the alternate
//! screen exactly once (`ESC [ ? 1049 h`), paints frames for as long as it
//! runs, and gives it back exactly once at exit. resh reconstructs a client's
//! screen from a byte log, and a byte log with a bounded head cannot carry a
//! declaration made an arbitrary time ago: once that one sequence falls off
//! the front of the ring, every browser that attaches paints the app's frames
//! into the *normal* buffer while the app still believes it is on the
//! alternate one. Nothing looks wrong until the app exits — its `ESC[?1049l`
//! then reaches a terminal that was never switched, so instead of restoring
//! the pre-app screen it restores a cursor that was never saved, and the exit
//! banner prints from the top of the screen over the leftover frame. That is
//! the "garbled screen when exiting claude" report, and a local terminal
//! cannot reach it: nothing there ever rebuilds its state from a log.
//!
//! So the switch is not replayed from the log at all. It is tracked here and
//! synthesized at attach time, where it cannot age out. Tracking it also buys
//! a ring per screen, which is what stops an app from evicting the scrollback
//! it is going to hand back.
//!
//! Stateful because the pump reads 8 KiB at a time and a sequence is under no
//! obligation to arrive whole. Every bound here exists because this parser is
//! fed attacker-influenced bytes — anything written to a terminal, including
//! `cat` of a hostile file.
use std::collections::VecDeque;

/// Per screen, not per session: an app on the alternate screen must not be
/// able to evict the normal screen the user will be returned to.
pub const MAX_SCROLLBACK: usize = 1_000_000;

/// Longest parameter run held for an in-flight CSI sequence. The ones this
/// module cares about are at most `?1049;1000`; anything longer is either not
/// ours or not real, and must not be a way to make resh allocate.
pub const MAX_PARAMS: usize = 64;

/// Sent in place of an alternate-screen exit whose entry this process never
/// saw. See `Screens::ingest`.
const RECONCILE: &[u8] = b"\x1b[H\x1b[2J";

/// An app moving between the normal and the alternate screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// Offset of the sequence's first byte within the chunk just fed. A
    /// sequence that began in an earlier chunk reports 0 — the bytes before it
    /// in *this* chunk are all there is left to attribute to the screen it is
    /// leaving.
    pub start: usize,
    /// One past the sequence's final byte.
    pub end: usize,
    /// 47, 1047 or 1049 — whichever spelling the app used, because they are
    /// not interchangeable: only 1049 saves and restores the cursor, so
    /// re-declaring a `less` with 1049 would move its cursor when it exits.
    pub mode: u16,
    pub enter: bool,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Esc,
    Csi,
}

/// Scans a PTY byte stream for alternate-screen switches. Pure: it holds no
/// content, so the pump can run it *outside* the session registry lock, the
/// way `osc::Parser` already is.
#[derive(Default)]
pub struct Scanner {
    state: State,
    /// Parameter bytes of an in-flight CSI sequence, `?` included.
    params: Vec<u8>,
    /// The sequence carried an intermediate byte, or more parameter bytes than
    /// are held. Either way it is not one of ours, and must not be matched on
    /// the truncated prefix that was kept — `ESC[?1049$p` *asks* whether the
    /// mode is set, and reading a question as an answer would drop a client
    /// onto the alternate screen because something was curious about it.
    unusable: bool,
    /// Where the in-flight sequence started in the chunk being fed.
    start: usize,
}

impl Scanner {
    pub fn new() -> Scanner {
        Scanner::default()
    }

    #[cfg(test)]
    pub fn buffered_len(&self) -> usize {
        self.params.len()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Switch> {
        let mut out = Vec::new();
        // A sequence already in flight began before this chunk, so everything
        // in this chunk up to its end belongs to the screen it is leaving.
        if self.state != State::Ground {
            self.start = 0;
        }
        for (i, &b) in chunk.iter().enumerate() {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                        self.start = i;
                    }
                }
                State::Esc => self.after_esc(b),
                State::Csi => match b {
                    // Parameter bytes: digits, `;`, and the private markers.
                    0x30..=0x3f => {
                        if self.params.len() < MAX_PARAMS {
                            self.params.push(b);
                        } else {
                            self.unusable = true;
                        }
                    }
                    // An intermediate byte. No mode set or reset has one.
                    0x20..=0x2f => self.unusable = true,
                    0x40..=0x7e => {
                        if let Some(sw) = self.dispatch(b, i) {
                            out.push(sw);
                        }
                        self.state = State::Ground;
                    }
                    // A bare ESC abandons the sequence and starts a new one,
                    // which is what a real terminal does too.
                    0x1b => {
                        self.state = State::Esc;
                        self.start = i;
                    }
                    // C0 controls are executed where they land and the
                    // sequence carries on around them.
                    _ => {}
                },
            }
        }
        out
    }

    fn after_esc(&mut self, b: u8) {
        match b {
            b'[' => {
                self.state = State::Csi;
                self.params.clear();
                self.unusable = false;
            }
            // A string sequence (OSC, DCS, SOS, PM, APC) needs no state of
            // its own: a CSI can only open on `ESC [`, so a payload's text
            // cannot fabricate one, and a *bare* ESC inside a string abandons
            // it here exactly as it does in the browser's emulator. Keeping
            // those two in step is the whole job, so there is nothing to
            // swallow and nothing to skip.
            0x1b => self.state = State::Esc,
            _ => self.state = State::Ground,
        }
    }

    fn dispatch(&self, final_byte: u8, i: usize) -> Option<Switch> {
        if self.unusable {
            return None;
        }
        let enter = match final_byte {
            b'h' => true,
            b'l' => false,
            _ => return None,
        };
        // Private modes only: `ESC[?1049h` switches screens, `ESC[1049h`
        // (no `?`) is a different, unrelated mode space.
        let rest = self.params.strip_prefix(b"?".as_slice())?;
        let mode = rest
            .split(|&c| c == b';')
            .filter_map(|p| std::str::from_utf8(p).ok()?.parse::<u16>().ok())
            .find(|m| matches!(*m, 47 | 1047 | 1049))?;
        Some(Switch { start: self.start, end: i + 1, mode, enter })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    /// This process has never seen a switch for this session — it attached to
    /// a `dtach` socket that predates it. Not the same as `Normal`: what is
    /// true is that every client resh has spoken to is on the normal buffer,
    /// not that the *app* is.
    Unknown,
    Normal,
    Alt(u16),
}

/// A session's output, kept per screen, plus which screen it is on.
pub struct Screens {
    normal: VecDeque<u8>,
    alt: VecDeque<u8>,
    at: At,
}

impl Default for Screens {
    fn default() -> Screens {
        Screens::new()
    }
}

impl Screens {
    pub fn new() -> Screens {
        Screens { normal: VecDeque::new(), alt: VecDeque::new(), at: At::Unknown }
    }

    /// Files a chunk of PTY output under the screen it was written on, and
    /// returns what to send to attached clients — the same bytes, except for
    /// one case.
    ///
    /// That case: an exit whose entry this process never saw. After a restart
    /// the ring is empty, the entry is unrecoverable, and every attached
    /// client is therefore on the normal buffer — resh has never sent them
    /// anything else. Forwarding the exit verbatim is exactly what paints the
    /// app's parting words over its own leftover frame, so it is replaced with
    /// a clear. That is a degradation, not a repair: the pre-app screen went
    /// with the process that held it, and nothing recovers it. It ends the
    /// moment any switch is observed, which is why it is keyed on `Unknown`
    /// rather than on "not currently on the alternate screen" — an app that
    /// leaves and re-enters must never have its screen cleared under it.
    pub fn ingest(&mut self, chunk: &[u8], switches: &[Switch]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        let mut cursor = 0;
        for sw in switches {
            let end = sw.end.min(chunk.len());
            if end < cursor {
                continue;
            }
            let start = sw.start.clamp(cursor, end);
            let head = &chunk[cursor..start];
            self.push(head);
            out.extend_from_slice(head);
            if !sw.enter && self.at == At::Unknown {
                self.push(RECONCILE);
                out.extend_from_slice(RECONCILE);
            } else {
                // Forwarded to clients that are already following along, but
                // never stored: `replay` synthesizes it instead, so it cannot
                // age out of the ring the way the app's own copy does.
                out.extend_from_slice(&chunk[start..end]);
            }
            cursor = end;
            // Both directions drop the alternate screen's content: entering
            // starts a blank one, and leaving throws it away. (`?47h` and
            // `?1047h` technically preserve what was there from a previous
            // run; keeping a dead app's frame to hand to the next one is not
            // worth a second ring.)
            self.alt.clear();
            self.at = if sw.enter { At::Alt(sw.mode) } else { At::Normal };
        }
        let tail = &chunk[cursor..];
        self.push(tail);
        out.extend_from_slice(tail);
        out
    }

    /// Everything a client attaching now needs in order to be looking at the
    /// same screen as the app.
    pub fn replay(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.normal.len() + self.alt.len() + 8);
        out.extend(self.normal.iter().copied());
        if let At::Alt(mode) = self.at {
            out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
            out.extend(self.alt.iter().copied());
        }
        out
    }

    fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let ring = match self.at {
            At::Alt(_) => &mut self.alt,
            _ => &mut self.normal,
        };
        push_bounded(ring, data);
    }
}

pub fn push_bounded(ring: &mut VecDeque<u8>, data: &[u8]) {
    ring.extend(data.iter().copied());
    while ring.len() > MAX_SCROLLBACK {
        ring.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pump's two steps in one call: scan with the lock released, file the
    /// result under the lock. Holding both here keeps the tests honest about
    /// the scanner being stateful across chunks.
    struct Pump {
        scanner: Scanner,
        screens: Screens,
    }

    impl Pump {
        fn new() -> Pump {
            Pump { scanner: Scanner::new(), screens: Screens::new() }
        }
        fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
            let switches = self.scanner.feed(chunk);
            self.screens.ingest(chunk, &switches)
        }
        fn replay(&self) -> Vec<u8> {
            self.screens.replay()
        }
    }

    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_enter_and_an_exit_are_both_seen() {
        let mut s = Scanner::new();
        let sw = s.feed(b"before\x1b[?1049hafter");
        assert_eq!(sw.len(), 1);
        assert!(sw[0].enter);
        assert_eq!(sw[0].mode, 1049);
        assert_eq!((sw[0].start, sw[0].end), (6, 14));
        let sw = s.feed(b"\x1b[?1049l");
        assert_eq!(sw.len(), 1);
        assert!(!sw[0].enter);
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_seen() {
        // The pump reads 8 KiB at a time and a switch is under no obligation
        // to arrive whole. This is the case that makes the scanner stateful.
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b[?10").is_empty());
        let sw = s.feed(b"49hframe");
        assert_eq!(sw.len(), 1);
        assert!(sw[0].enter);
        assert_eq!((sw[0].start, sw[0].end), (0, 3));
    }

    #[test]
    fn a_query_about_the_mode_is_not_a_switch() {
        // DECRQM. Reading it as a set would move a client onto the alternate
        // screen because something asked whether it was there.
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b[?1049$p").is_empty());
        assert_eq!(s.feed(b"\x1b[?1049h").len(), 1, "and the parser is not wedged");
    }

    #[test]
    fn a_mode_riding_along_with_others_is_still_seen() {
        let sw = Scanner::new().feed(b"\x1b[?1049;1000h");
        assert_eq!(sw.len(), 1);
        assert_eq!(sw[0].mode, 1049);
        assert!(sw[0].enter);
    }

    #[test]
    fn the_same_number_without_the_private_marker_is_a_different_mode() {
        assert!(Scanner::new().feed(b"\x1b[1049h").is_empty());
    }

    #[test]
    fn the_older_spellings_are_seen_and_kept_apart() {
        for (bytes, mode) in [(&b"\x1b[?47h"[..], 47u16), (&b"\x1b[?1047h"[..], 1047)] {
            let sw = Scanner::new().feed(bytes);
            assert_eq!(sw.len(), 1, "{bytes:?}");
            assert_eq!(sw[0].mode, mode, "the spelling the app used must survive");
        }
    }

    #[test]
    fn the_text_of_a_window_title_cannot_fabricate_a_switch() {
        // A title's payload is whatever the program felt like putting there.
        // Only an `ESC [` opens a sequence, so the bytes alone are just text —
        // which is a claim about this scanner that a substring search for
        // `[?1049h` would quietly break.
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]0;[?1049h\x07").is_empty());
        assert_eq!(s.feed(b"\x1b[?1049h").len(), 1, "and a real one right after still lands");
    }

    #[test]
    fn a_real_switch_written_inside_a_title_is_still_a_switch() {
        // The other half: a bare ESC abandons the string sequence it is inside,
        // here and in the browser's emulator alike. A scanner that swallowed
        // string payloads whole would miss a switch xterm.js is about to act
        // on — and the two disagreeing is the entire bug this module exists for.
        assert_eq!(Scanner::new().feed(b"\x1b]0;title\x1b[?1049h").len(), 1);
    }

    #[test]
    fn a_private_sequence_that_is_neither_set_nor_reset_is_not_a_switch() {
        // Only `h` (set) and `l` (reset) say the screen moved. Anything else
        // ending a `?1049` sequence is asking or reporting, not switching.
        assert!(Scanner::new().feed(b"\x1b[?1049n").is_empty());
    }

    #[test]
    fn a_sequence_carrying_an_intermediate_byte_is_not_a_mode_set() {
        // `CSI ? 1049 SP h` is some other sequence that happens to end in `h`.
        assert!(Scanner::new().feed(b"\x1b[?1049 h").is_empty());
    }

    #[test]
    fn a_truncated_parameter_run_does_not_match_on_the_part_that_fit() {
        // The bound keeps the first MAX_PARAMS bytes, and those can look like
        // a switch on their own. Dropping the sequence is the only safe read:
        // whatever this was, it was not `ESC[?1049h`.
        let mut junk = b"\x1b[?1049;".to_vec();
        junk.extend(std::iter::repeat(b'9').take(MAX_PARAMS * 2));
        junk.push(b'h');
        assert!(Scanner::new().feed(&junk).is_empty());
    }

    #[test]
    fn an_unbounded_parameter_run_is_dropped_rather_than_buffered() {
        let mut s = Scanner::new();
        let mut junk = vec![0x1b, b'[', b'?'];
        junk.extend(std::iter::repeat(b'1').take(100_000));
        assert!(s.feed(&junk).is_empty());
        assert!(s.buffered_len() <= MAX_PARAMS, "buffered {}", s.buffered_len());
        assert!(s.feed(b"h").is_empty(), "an over-long sequence stays dropped");
        assert_eq!(s.feed(b"\x1b[?1049h").len(), 1, "and the next real one is seen");
    }

    #[test]
    fn a_client_attaching_mid_app_is_put_on_the_alternate_screen() {
        // The whole bug: the app declares the alternate screen once, and that
        // declaration ages out long before the app exits.
        let mut p = Pump::new();
        p.feed(b"shell history\n");
        p.feed(b"\x1b[?1049hframe");
        let replay = p.replay();
        let at = replay
            .windows(8)
            .position(|w| w == b"\x1b[?1049h")
            .expect("the replay must declare the screen it is handing over");
        assert!(replay[..at].ends_with(b"shell history\n"), "normal screen first");
        assert!(replay[at + 8..].ends_with(b"frame"), "then the alternate screen's content");
    }

    #[test]
    fn the_spelling_the_app_used_is_the_one_replayed() {
        let mut p = Pump::new();
        p.feed(b"\x1b[?47h");
        assert!(has(&p.replay(), b"\x1b[?47h"));
        assert!(!has(&p.replay(), b"\x1b[?1049h"));
    }

    #[test]
    fn the_app_does_not_evict_the_screen_it_will_return_to() {
        // One shared ring means a full-screen app pushes the user's own shell
        // history out of the replay within a minute or two of running.
        let mut p = Pump::new();
        p.feed(b"shell history\n");
        p.feed(b"\x1b[?1049h");
        let frame = vec![b'f'; 10_000];
        for _ in 0..(MAX_SCROLLBACK / 10_000 + 10) {
            p.feed(&frame);
        }
        assert!(
            has(&p.replay(), b"shell history"),
            "the pre-app screen survived an app that outran the ring"
        );
    }

    #[test]
    fn each_screen_is_bounded_on_its_own() {
        let mut p = Pump::new();
        let block = vec![b'n'; 10_000];
        for _ in 0..(MAX_SCROLLBACK / 10_000 + 10) {
            p.feed(&block);
        }
        p.feed(b"\x1b[?1049h");
        let frame = vec![b'f'; 10_000];
        for _ in 0..(MAX_SCROLLBACK / 10_000 + 10) {
            p.feed(&frame);
        }
        assert!(p.screens.normal.len() <= MAX_SCROLLBACK);
        assert!(p.screens.alt.len() <= MAX_SCROLLBACK);
    }

    #[test]
    fn leaving_the_alternate_screen_drops_its_content_and_every_switch() {
        let mut p = Pump::new();
        p.feed(b"shell history\n");
        p.feed(b"\x1b[?1049hframe\x1b[?1049l");
        let replay = p.replay();
        assert!(!has(&replay, b"frame"), "the frame is gone");
        assert!(!has(&replay, b"\x1b[?1049"), "and so is every switch");
        assert!(replay.ends_with(b"shell history\n"));
    }

    #[test]
    fn a_second_run_does_not_show_the_first_runs_frame() {
        let mut p = Pump::new();
        p.feed(b"\x1b[?1049hfirst\x1b[?1049l");
        p.feed(b"\x1b[?1049hsecond");
        let replay = p.replay();
        assert!(!has(&replay, b"first"));
        assert!(replay.ends_with(b"second"));
    }

    #[test]
    fn an_exit_with_no_entry_this_process_saw_is_reconciled_not_forwarded() {
        // After a restart the ring is empty and the entry is unrecoverable, so
        // every attached client is on the normal buffer. Forwarding the exit
        // verbatim is what paints the banner over the leftover frame.
        let mut p = Pump::new();
        let out = p.feed(b"\x1b[?1049lResume this session with:");
        assert!(!has(&out, b"\x1b[?1049"), "the exit was not forwarded");
        assert!(out.starts_with(RECONCILE), "the screen was cleared instead: {out:?}");
        assert!(out.ends_with(b"Resume this session with:"));
    }

    #[test]
    fn an_exit_after_an_entry_is_forwarded_untouched() {
        // The reconciliation above is only ever right while the entry is
        // unknown. An app that leaves and re-enters must not have its screen
        // cleared out from under it.
        let mut p = Pump::new();
        p.feed(b"\x1b[?1049h");
        let out = p.feed(b"\x1b[?1049lback");
        assert_eq!(out, b"\x1b[?1049lback".to_vec());
    }

    #[test]
    fn everything_else_is_forwarded_byte_for_byte() {
        // The one rewrite above is the only thing this module may do to the
        // stream. A terminal is not a place to be creative.
        let mut p = Pump::new();
        let chunk = b"\x1b[31mred\x1b]0;title\x07\x1b[?25l plain \xf0\x9f\x8e\x89";
        assert_eq!(p.feed(chunk), chunk.to_vec());
    }
}
