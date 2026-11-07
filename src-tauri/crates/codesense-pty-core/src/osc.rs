// SPDX-License-Identifier: MIT
//! Incremental OSC escape-sequence parser for the PTY byte stream.
//!
//! A stateful side-tap that sniffs a streaming byte feed for the two OSC
//! sequences codesense's shell-init emits — OSC 7 (working directory) and
//! OSC 133 (semantic prompt markers) — and surfaces them as [`OscEvent`]s.
//! It never mutates or blocks the stream; bytes are observed and pass through
//! to subscribers unchanged.
//!
//! Like the batcher's `emit_valid_utf8`, the parser retains partial state
//! across calls so a sequence split across read/batch boundaries (4096-byte
//! reads, 16ms batch windows) is still recognized.

use std::path::PathBuf;

/// Upper bound on a single OSC payload. An unterminated sequence beyond this
/// length is abandoned so a malformed stream can't grow the buffer unbounded.
const MAX_OSC_PAYLOAD: usize = 4096;

/// A semantic event extracted from the PTY stream.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OscEvent {
    /// OSC 7 — working-directory report. Carries the decoded absolute path.
    Cwd(PathBuf),
    /// OSC 133;A — prompt start.
    PromptStart,
    /// OSC 133;B — command start (user submitted a command line).
    CommandStart,
    /// OSC 133;D — command end, with the reported exit code when present.
    CommandEnd(Option<i32>),
}

/// An [`OscEvent`] tagged with the byte offset, within the slice passed to
/// the completing [`OscParser::feed`] call, where the sequence's `ESC`
/// introducer began. The warm-pool output gate slices `data[offset..]` to
/// deliver from the prompt-start marker onward — byte-exact, no cd echo. When
/// the introducer was consumed by a *prior* `feed` call (a sequence split across
/// chunks), `offset` is `0` (deliver from the start of this slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscEventAt {
    pub offset: usize,
    pub event: OscEvent,
}

#[derive(Debug, Default, PartialEq)]
enum State {
    /// Scanning plain bytes for the OSC introducer `ESC ]`.
    #[default]
    Normal,
    /// Saw `ESC` (0x1b); the next byte decides whether this is an OSC.
    EscSeen,
    /// Inside an OSC payload, collecting until a terminator.
    Osc,
    /// Inside an OSC payload and saw `ESC`; a following `\` is the ST terminator.
    OscEsc,
}

/// Incremental OSC scanner. Feed it byte chunks; it appends completed events.
///
/// Contract: a repeated `133;D` counts once. A second D counts again only when
/// another 133 mark (A, B or C) came between the two.
#[derive(Debug, Default)]
pub struct OscParser {
    state: State,
    payload: Vec<u8>,
    /// The last OSC 133 mark was a command end (D). A further D before any
    /// other 133 mark repeats the same command's end (fish 4 emits its own
    /// beside the integration's) and is dropped.
    command_ended: bool,
}

impl OscParser {
    /// Feed a chunk of bytes, appending any completed events to `out`.
    ///
    /// Only `ESC ]` (0x1b 0x5d) opens an OSC; the single-byte C1 form (0x9d)
    /// is deliberately not recognized (almost no shell emits it).
    ///
    /// Tracks `intro_off`, the offset (within this slice) of the `ESC`
    /// that introduced the in-flight OSC, and stamps it on completed events.
    /// `None` means the introducer was consumed in a prior `feed` call, so the
    /// reported offset is `0` (the sequence spans chunks).
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<OscEventAt>) {
        let mut intro_off: Option<usize> = None;
        for (i, &b) in bytes.iter().enumerate() {
            match self.state {
                State::Normal => {
                    if b == 0x1b {
                        self.state = State::EscSeen;
                        intro_off = Some(i);
                    }
                }
                State::EscSeen => match b {
                    b']' => {
                        self.state = State::Osc;
                        self.payload.clear();
                    }
                    0x1b => intro_off = Some(i), // consecutive ESC — re-anchor
                    _ => {
                        self.state = State::Normal;
                        intro_off = None;
                    }
                },
                State::Osc => match b {
                    0x07 => self.finish(out, intro_off.unwrap_or(0)), // BEL terminator
                    0x1b => self.state = State::OscEsc,               // possible ST
                    _ => {
                        self.payload.push(b);
                        if self.payload.len() > MAX_OSC_PAYLOAD {
                            self.abort();
                            intro_off = None;
                        }
                    }
                },
                State::OscEsc => {
                    if b == b'\\' {
                        self.finish(out, intro_off.unwrap_or(0)); // ST (ESC \) terminator
                    } else {
                        // Not ST: abandon this OSC. The `ESC` we consumed plus
                        // this byte may introduce a NEW sequence (e.g. `ESC ]`),
                        // so reprocess `b` through the EscSeen logic instead of
                        // dropping it. That introducer ESC sits one byte back
                        // (or in a prior chunk when `i == 0` → offset 0).
                        self.payload.clear();
                        match b {
                            b']' => {
                                self.state = State::Osc;
                                intro_off = i.checked_sub(1);
                            }
                            0x1b => {
                                self.state = State::EscSeen;
                                intro_off = Some(i);
                            }
                            _ => {
                                self.state = State::Normal;
                                intro_off = None;
                            }
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<OscEventAt>, offset: usize) {
        let event = parse_osc_payload(&self.payload);
        let is_end = matches!(event, Some(OscEvent::CommandEnd(_)));
        if is_end && self.command_ended {
            self.abort();
            return;
        }
        // Only a D that produced an event latches, so an unreadable D cannot
        // swallow the next real one; any other 133 mark, even the ignored C, clears.
        if let Some(mark) = self.payload.strip_prefix(b"133;") {
            if is_end {
                self.command_ended = true;
            } else if mark.first() != Some(&b'D') {
                self.command_ended = false;
            }
        }
        if let Some(event) = event {
            out.push(OscEventAt { offset, event });
        }
        self.abort();
    }

    fn abort(&mut self) {
        self.payload.clear();
        self.state = State::Normal;
    }
}

/// Parse a complete OSC payload (the bytes between `ESC ]` and the terminator).
fn parse_osc_payload(payload: &[u8]) -> Option<OscEvent> {
    let s = std::str::from_utf8(payload).ok()?;

    if let Some(rest) = s.strip_prefix("7;") {
        // OSC 7: `file://<host>/<path>` or `file:///<path>` (host empty).
        let after = rest.strip_prefix("file://")?;
        let slash = after.find('/')?;
        let path = percent_decode(&after[slash..]);
        return Some(OscEvent::Cwd(PathBuf::from(path)));
    }

    if let Some(rest) = s.strip_prefix("133;") {
        return match rest.as_bytes().first()? {
            b'A' => Some(OscEvent::PromptStart),
            b'B' => Some(OscEvent::CommandStart),
            b'D' => {
                // "D" or "D;<exit>".
                let exit = rest[1..]
                    .strip_prefix(';')
                    .and_then(|c| c.trim().parse::<i32>().ok());
                Some(OscEvent::CommandEnd(exit))
            }
            _ => None, // 133;C (output start) and unknown subkinds: ignored in v1
        };
    }

    None
}

/// Decode `%xx` escapes in an OSC 7 path. Invalid escapes are left literal.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: feed an entire byte slice in one call and collect events
    /// (dropping offsets — most tests assert on the semantic event only).
    fn parse_all(bytes: &[u8]) -> Vec<OscEvent> {
        let mut p = OscParser::default();
        let mut out = Vec::new();
        p.feed(bytes, &mut out);
        out.into_iter().map(|e| e.event).collect()
    }

    /// A repeated D for the same command (fish 4 emits its own next to ours) is
    /// one command end; any other 133 mark, even a C that is otherwise
    /// ignored, starts the next command.
    #[test]
    fn repeated_command_end_is_one_end_per_command() {
        use OscEvent::{CommandEnd as D, CommandStart as B, PromptStart as A};
        let cases: [(&str, &[u8], Vec<OscEvent>); 9] = [
            ("single D", b"\x1b]133;D;1\x1b\\", vec![D(Some(1))]),
            ("doubled D", b"\x1b]133;D;1\x1b\\\x1b]133;D;1\x1b\\", vec![D(Some(1))]),
            (
                "doubled D with different codes keeps the first",
                b"\x1b]133;D;1\x1b\\\x1b]133;D;0\x1b\\",
                vec![D(Some(1))],
            ),
            (
                "doubled D, then a real next command",
                b"\x1b]133;D;1\x1b\\\x1b]133;D;1\x1b\\\x1b]133;B\x1b\\\x1b]133;D;0\x1b\\\x1b]133;D;0\x1b\\",
                vec![D(Some(1)), B, D(Some(0))],
            ),
            (
                "D, A, D without any B",
                b"\x1b]133;D;1\x1b\\\x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;D;0\x1b\\",
                vec![D(Some(1)), A, D(Some(0))],
            ),
            (
                "D, C, D (C is not surfaced, but ends the command)",
                b"\x1b]133;D;1\x1b\\\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\",
                vec![D(Some(1)), D(Some(0))],
            ),
            (
                "an unreadable D does not latch, so the next D counts",
                b"\x1b]133;D;\xff\x1b\\\x1b]133;D;1\x1b\\",
                vec![D(Some(1))],
            ),
            (
                "D with no preceding command",
                b"\x1b]133;D\x1b\\",
                vec![D(None)],
            ),
            (
                "D before any mark at all, then D after a prompt",
                b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\\x1b]133;D;0\x1b\\",
                vec![D(Some(0)), A, D(Some(0))],
            ),
        ];
        for (name, bytes, want) in cases {
            assert_eq!(parse_all(bytes), want, "{name}");
        }
    }

    /// The latch survives a chunk boundary: the second D arrives in a later feed.
    #[test]
    fn repeated_command_end_across_feeds_is_one_end() {
        let mut p = OscParser::default();
        let mut out = Vec::new();
        p.feed(b"\x1b]133;D;1\x1b\\\x1b]133", &mut out);
        p.feed(b";D;1\x1b\\", &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn osc7_with_hostname_st_terminator() {
        let ev = parse_all(b"\x1b]7;file://myhost/Users/me/dev\x1b\\");
        assert_eq!(ev, vec![OscEvent::Cwd(PathBuf::from("/Users/me/dev"))]);
    }

    #[test]
    fn osc7_no_hostname_bel_terminator() {
        let ev = parse_all(b"\x1b]7;file:///Users/me\x07");
        assert_eq!(ev, vec![OscEvent::Cwd(PathBuf::from("/Users/me"))]);
    }

    #[test]
    fn osc7_percent_decoded_path() {
        let ev = parse_all(b"\x1b]7;file://host/Users/me/my%20dir\x1b\\");
        assert_eq!(ev, vec![OscEvent::Cwd(PathBuf::from("/Users/me/my dir"))]);
    }

    #[test]
    fn osc133_markers() {
        assert_eq!(parse_all(b"\x1b]133;A\x1b\\"), vec![OscEvent::PromptStart]);
        assert_eq!(parse_all(b"\x1b]133;B\x07"), vec![OscEvent::CommandStart]);
        assert_eq!(
            parse_all(b"\x1b]133;D;0\x1b\\"),
            vec![OscEvent::CommandEnd(Some(0))]
        );
        assert_eq!(
            parse_all(b"\x1b]133;D;130\x1b\\"),
            vec![OscEvent::CommandEnd(Some(130))]
        );
        assert_eq!(
            parse_all(b"\x1b]133;D\x1b\\"),
            vec![OscEvent::CommandEnd(None)]
        );
    }

    #[test]
    fn full_interaction_sequence_matches_ac9() {
        // echo hello; ls → A B D;0 A B D;0 A
        let stream = b"\x1b]133;A\x1b\\prompt$ echo hello\r\n\
                       \x1b]133;B\x1b\\hello\r\n\
                       \x1b]133;D;0\x1b\\\
                       \x1b]133;A\x1b\\prompt$ ls\r\n\
                       \x1b]133;B\x1b\\file1 file2\r\n\
                       \x1b]133;D;0\x1b\\\
                       \x1b]133;A\x1b\\";
        assert_eq!(
            parse_all(stream),
            vec![
                OscEvent::PromptStart,
                OscEvent::CommandStart,
                OscEvent::CommandEnd(Some(0)),
                OscEvent::PromptStart,
                OscEvent::CommandStart,
                OscEvent::CommandEnd(Some(0)),
                OscEvent::PromptStart,
            ]
        );
    }

    #[test]
    fn sequence_split_across_chunks_mid_escape() {
        // Split the OSC 7 sequence at every byte boundary; the parser must
        // still recover exactly one event regardless of where the cut falls.
        let full = b"\x1b]7;file://h/tmp/x\x1b\\".to_vec();
        for cut in 1..full.len() {
            let mut p = OscParser::default();
            let mut out = Vec::new();
            p.feed(&full[..cut], &mut out);
            p.feed(&full[cut..], &mut out);
            let events: Vec<OscEvent> = out.iter().map(|e| e.event.clone()).collect();
            assert_eq!(
                events,
                vec![OscEvent::Cwd(PathBuf::from("/tmp/x"))],
                "failed when split at byte {cut}"
            );
        }
    }

    #[test]
    fn plain_bytes_interleaved_are_transparent() {
        let ev = parse_all(b"some output\x1b]133;A\x1b\\more output\r\n");
        assert_eq!(ev, vec![OscEvent::PromptStart]);
    }

    #[test]
    fn osc_introducer_after_unterminated_esc_in_payload_is_recovered() {
        // A non-ST `ESC` inside a prior OSC payload abandons that OSC, but the
        // `ESC ]` introducer of the FOLLOWING sequence must still be recognized.
        let ev = parse_all(b"\x1b]0;title\x1b]133;A\x1b\\");
        assert_eq!(ev, vec![OscEvent::PromptStart]);
    }

    #[test]
    fn c1_single_byte_osc_form_rejected() {
        // 0x9d is the C1 OSC introducer — must NOT be recognized in v1.
        let ev = parse_all(b"\x9d133;A\x1b\\");
        assert!(ev.is_empty());
    }

    #[test]
    fn malformed_partial_sequence_yields_nothing() {
        assert!(parse_all(b"\x1b]7;not-a-uri\x1b\\").is_empty());
        assert!(parse_all(b"\x1b]999;whatever\x07").is_empty());
        assert!(parse_all(b"\x1b]7;file://h").is_empty()); // never terminated
    }

    #[test]
    fn unterminated_oversized_payload_is_abandoned() {
        let mut bytes = b"\x1b]7;".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD + 10));
        let mut p = OscParser::default();
        let mut out = Vec::new();
        p.feed(&bytes, &mut out);
        assert!(out.is_empty());
        // Parser recovered to Normal and can still parse a following sequence.
        p.feed(b"\x1b]133;A\x1b\\", &mut out);
        let events: Vec<OscEvent> = out.iter().map(|e| e.event.clone()).collect();
        assert_eq!(events, vec![OscEvent::PromptStart]);
    }

    /// The reported offset points at the `ESC` introducer of the marker
    /// mid-buffer, so the warm-pool gate can slice `data[offset..]` to deliver
    /// from the prompt-start onward.
    #[test]
    fn offset_points_at_introducer_mid_buffer() {
        let stream = b"cd /t && clear\r\n\x1b]133;A\x1b\\prompt$ ";
        let mut p = OscParser::default();
        let mut out = Vec::new();
        p.feed(stream, &mut out);
        assert_eq!(out.len(), 1);
        let at = &out[0];
        assert_eq!(at.event, OscEvent::PromptStart);
        // The reported offset is exactly where `ESC]133;A` begins.
        let expected = stream.windows(2).position(|w| w == b"\x1b]").unwrap();
        assert_eq!(at.offset, expected);
        assert_eq!(&stream[at.offset..at.offset + 2], b"\x1b]");
    }

    /// When the introducer was consumed in a prior `feed`, the completing
    /// feed reports offset 0 (deliver from the start of this chunk).
    #[test]
    fn offset_is_zero_when_introducer_split_across_feeds() {
        let mut p = OscParser::default();
        let mut out = Vec::new();
        // First chunk ends mid-sequence (introducer + partial payload).
        p.feed(b"\x1b]133;", &mut out);
        assert!(out.is_empty());
        // Completing chunk: the introducer is not in THIS slice → offset 0.
        p.feed(b"A\x1b\\", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, OscEvent::PromptStart);
        assert_eq!(out[0].offset, 0);
    }

    /// After a non-ST `ESC` abandons a prior OSC, the recovered
    /// introducer offset points one byte back at the `ESC` of `ESC ]`.
    #[test]
    fn offset_after_osc_esc_recovery_points_at_prior_esc() {
        // `ESC]0;title` (abandoned via ESC]) then `ESC]133;A`.
        let stream = b"\x1b]0;title\x1b]133;A\x1b\\";
        let mut p = OscParser::default();
        let mut out = Vec::new();
        p.feed(stream, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, OscEvent::PromptStart);
        assert_eq!(&stream[out[0].offset..out[0].offset + 2], b"\x1b]");
    }
}
