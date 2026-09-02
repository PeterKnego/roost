//! Parses desktop-notification escape sequences out of a PTY byte stream.
//!
//! Terminals carry out-of-band messages as OSC sequences (`ESC ] … BEL`), and
//! notification sequences are the convention every other terminal already
//! implements — which is why roost accepts them rather than inventing an
//! ingress of its own: anything that can already notify iTerm2 or kitty
//! notifies roost unchanged, with no knowledge that roost exists.
//!
//! Stateful because a sequence can straddle a read boundary; the pump reads
//! 8 KiB at a time and a notification is under no obligation to arrive whole.
//! Every bound here exists because this parser is fed attacker-influenced
//! bytes — anything written to a terminal, including `cat` of a hostile file.

pub const MAX_SEQUENCE: usize = 4096;
pub const MAX_TITLE: usize = 100;
pub const MAX_BODY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub title: Option<String>,
    pub body: String,
}

#[derive(Default)]
pub struct Parser {
    /// Bytes of an in-flight sequence, payload only (after `ESC ]`).
    buf: Vec<u8>,
    /// Inside a sequence, i.e. `ESC ]` seen and no terminator yet.
    in_seq: bool,
    /// Last byte was `ESC` — could open a sequence, or close one as `ESC \`.
    esc: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    #[cfg(test)]
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Parsed> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.in_seq {
                // ESC inside a sequence is only meaningful as the ST pair
                // `ESC \`; a bare ESC starts a new sequence, abandoning this
                // one, which is what a real terminal does too.
                if self.esc {
                    self.esc = false;
                    if b == b'\\' {
                        self.finish(&mut out);
                        continue;
                    }
                    if b == b']' {
                        self.buf.clear();
                        continue;
                    }
                    self.reset();
                    continue;
                }
                match b {
                    0x1b => self.esc = true,
                    0x07 => self.finish(&mut out),
                    // Real OSC never spans a line. Bailing here is what stops
                    // `cat` of a binary file from parking bytes in `buf`
                    // until MAX_SEQUENCE, once per stray `ESC ]`.
                    b'\n' | b'\r' => self.reset(),
                    _ => {
                        self.buf.push(b);
                        if self.buf.len() > MAX_SEQUENCE {
                            self.reset();
                        }
                    }
                }
            } else if self.esc {
                self.esc = false;
                if b == b']' {
                    self.in_seq = true;
                    self.buf.clear();
                } else if b == 0x1b {
                    self.esc = true; // ESC ESC — still pending
                }
            } else if b == 0x1b {
                self.esc = true;
            }
        }
        out
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.in_seq = false;
        self.esc = false;
    }

    fn finish(&mut self, out: &mut Vec<Parsed>) {
        let payload = String::from_utf8_lossy(&self.buf).into_owned();
        self.reset();
        if let Some(p) = parse_payload(&payload) {
            out.push(p);
        }
    }
}

/// `777;notify;TITLE;BODY` or `9;BODY`. Anything else is some other OSC code
/// (window title, clipboard) and is not ours.
fn parse_payload(payload: &str) -> Option<Parsed> {
    if let Some(rest) = payload.strip_prefix("777;notify;") {
        // splitn(2): only the delimiter between TITLE and BODY is structural,
        // so a body may contain ';' without escaping.
        let mut it = rest.splitn(2, ';');
        let title = it.next().unwrap_or("");
        let body = it.next().unwrap_or("");
        return Some(Parsed {
            title: Some(sanitise(title, MAX_TITLE)),
            body: sanitise(body, MAX_BODY),
        });
    }
    if let Some(body) = payload.strip_prefix("9;") {
        return Some(Parsed { title: None, body: sanitise(body, MAX_BODY) });
    }
    None
}

/// Strips C0/C1 controls and truncates. The text reaches a browser and an OS
/// notification centre, and it arrived over a terminal, so it is untrusted:
/// leaving an ESC in would let a notification re-colour or reposition
/// whatever renders it.
///
/// `pub(crate)` so `cli::notify_sequence` can sanitise before it emits,
/// rather than relying solely on this parser to reject what it produces —
/// see cli.rs's module doc for why that mattered in practice.
pub(crate) fn sanitise(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(*c, '\u{80}'..='\u{9f}'))
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(bytes: &[u8]) -> Parsed {
        let mut p = Parser::new();
        let mut out = p.feed(bytes);
        assert_eq!(out.len(), 1, "expected exactly one notice from {bytes:?}");
        out.pop().unwrap()
    }

    #[test]
    fn parses_the_777_form() {
        let n = one(b"\x1b]777;notify;Build done;42 tests passed\x07");
        assert_eq!(n.title.as_deref(), Some("Build done"));
        assert_eq!(n.body, "42 tests passed");
    }

    #[test]
    fn parses_the_iterm_form_without_a_title() {
        let n = one(b"\x1b]9;needs your input\x07");
        assert_eq!(n.title, None);
        assert_eq!(n.body, "needs your input");
    }

    #[test]
    fn accepts_st_as_well_as_bel() {
        let n = one(b"\x1b]9;done\x1b\\");
        assert_eq!(n.body, "done");
    }

    #[test]
    fn semicolons_inside_the_body_are_literal() {
        let n = one(b"\x1b]777;notify;t;a;b;c\x07");
        assert_eq!(n.title.as_deref(), Some("t"));
        assert_eq!(n.body, "a;b;c", "only the first three ';' are structural");
    }

    // The reason this parser is stateful at all. A stateless implementation
    // passes every test above and fails this one.
    #[test]
    fn a_sequence_split_at_any_offset_still_parses() {
        let seq = b"\x1b]777;notify;Title here;Body here\x07";
        for cut in 1..seq.len() {
            let mut p = Parser::new();
            let mut got = p.feed(&seq[..cut]);
            got.extend(p.feed(&seq[cut..]));
            assert_eq!(got.len(), 1, "split at {cut} produced {} notices", got.len());
            assert_eq!(got[0].body, "Body here", "split at {cut}");
            assert_eq!(got[0].title.as_deref(), Some("Title here"), "split at {cut}");
        }
    }

    #[test]
    fn surrounding_terminal_output_passes_through_without_confusing_the_parser() {
        let mut p = Parser::new();
        let got = p.feed(b"$ cargo test\r\n\x1b]9;done\x07ok\r\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "done");
        assert_eq!(p.buffered_len(), 0, "trailing plain output must not be buffered");
    }

    // The bug being prevented is unbounded accumulation, not a spurious
    // notice — so assert the buffer drained, not merely that nothing came out.
    #[test]
    fn an_overlong_sequence_is_abandoned_and_drains_the_buffer() {
        let mut p = Parser::new();
        let mut junk = Vec::from(&b"\x1b]777;notify;t;"[..]);
        junk.extend(std::iter::repeat_n(b'x', MAX_SEQUENCE + 100));
        let got = p.feed(&junk);
        assert!(got.is_empty(), "overlong sequence must not emit");
        assert_eq!(p.buffered_len(), 0, "overlong sequence must not stay buffered");
    }

    #[test]
    fn a_newline_abandons_an_in_flight_sequence() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]777;notify;t;oops\nplain text\r\n");
        assert!(got.is_empty(), "real OSC never contains a newline");
        assert_eq!(p.buffered_len(), 0);
    }

    #[test]
    fn binary_noise_containing_esc_bracket_yields_nothing_and_no_state() {
        let mut p = Parser::new();
        let noise: Vec<u8> = (0u8..=255).chain(b"\x1b]".iter().copied()).chain(0u8..=255).collect();
        let got = p.feed(&noise);
        assert!(got.is_empty(), "binary noise must not produce notices");
        assert_eq!(p.buffered_len(), 0, "noise contains newlines, which must drain the buffer");
    }

    // Assert the sanitised *result*, not that sanitising was attempted.
    // Non-ESC control bytes: an ESC inside a sequence ends it (see the next
    // test), so no ESC can ever reach `sanitise` to be stripped.
    #[test]
    fn control_characters_are_stripped_from_the_fields() {
        let n = one(b"\x1b]777;notify;Ti\x01tle;bo\x02dy\x1b\\");
        assert_eq!(n.title.as_deref(), Some("Title"), "control byte left in the title");
        assert_eq!(n.body, "body");
    }

    // Real terminals end an OSC at any ESC that is not the ST pair `ESC \`.
    // Following that convention is also what keeps this parser bounded: there
    // is no state in which it consumes input without either buffering it
    // against MAX_SEQUENCE or ending the sequence outright. Any "skip the
    // ANSI sequence and carry on" cleverness reintroduces exactly that
    // unbounded state — see the wedge test below.
    #[test]
    fn an_esc_inside_a_sequence_abandons_it() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]777;notify;Ti\x1b[31mtle;body\x07");
        assert!(got.is_empty(), "ESC inside a sequence must abandon it");
        assert_eq!(p.buffered_len(), 0);
    }

    // No look-ahead: a sequence ends at its own first terminator, and a later
    // unrelated ST elsewhere in the chunk must not retroactively demote it.
    #[test]
    fn the_first_terminator_wins_even_if_another_appears_later() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]9;first\x07 trailing \x1b\\ more \x1b]9;second\x07");
        assert_eq!(got.len(), 2, "each sequence ends at its own first terminator");
        assert_eq!(got[0].body, "first");
        assert_eq!(got[1].body, "second");
    }

    // Regression: a stray CSI intro inside a sequence must not put the parser
    // into a state that consumes unbounded input, swallows the next
    // sequence's prefix, or fabricates a notice by merging two sequences.
    #[test]
    fn an_unterminated_csi_cannot_wedge_the_parser() {
        let mut p = Parser::new();
        let mut junk = Vec::from(&b"\x1b]9;AAA\x1b["[..]);
        junk.extend(std::iter::repeat_n(b'9', MAX_SEQUENCE * 10));
        junk.extend_from_slice(b"\x1b]9;real\x07");
        let got = p.feed(&junk);
        assert_eq!(got.len(), 1, "exactly the well-formed sequence, got {got:?}");
        assert_eq!(got[0].body, "real", "no bytes from the abandoned sequence may leak in");
        assert_eq!(p.buffered_len(), 0);
    }

    #[test]
    fn oversized_fields_are_truncated_to_the_caps() {
        let mut seq = Vec::from(&b"\x1b]777;notify;"[..]);
        seq.extend(std::iter::repeat_n(b'T', 300));
        seq.push(b';');
        seq.extend(std::iter::repeat_n(b'B', 900));
        seq.push(0x07);
        let n = one(&seq);
        assert_eq!(n.title.as_deref().unwrap().chars().count(), MAX_TITLE);
        assert_eq!(n.body.chars().count(), MAX_BODY);
    }

    #[test]
    fn invalid_utf8_does_not_panic_and_yields_a_lossy_body() {
        let n = one(b"\x1b]9;caf\xff\x07");
        assert!(n.body.starts_with("caf"), "got {:?}", n.body);
    }

    #[test]
    fn two_sequences_in_one_chunk_both_parse() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]9;one\x07 middle \x1b]9;two\x07");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].body, "one");
        assert_eq!(got[1].body, "two");
    }

    #[test]
    fn an_unrelated_osc_code_is_ignored() {
        let mut p = Parser::new();
        let got = p.feed(b"\x1b]0;window title\x07"); // OSC 0 = set title
        assert!(got.is_empty(), "OSC 0 is not a notification");
        assert_eq!(p.buffered_len(), 0);
    }
}
