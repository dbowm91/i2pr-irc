//! Bounded SAM control-line framing.
//!
//! # Why this is not the IRC line decoder
//!
//! `i2pr-irc-wire` frames IRC messages, and its decoder trusts an IRC peer in ways a SAM
//! bridge must not be trusted: IRC has no option tokens, no quoted `MESSAGE`, and its
//! maximum line is smaller than a Destination. Reusing it would mean either weakening
//! the IRC decoder for a protocol it never sees, or writing a second decoder that looks
//! like the first. This module is a separate, deliberately narrow reader.
//!
//! # The property that matters most
//!
//! **An over-long line is dropped, and the line after it is still readable.** A reader
//! that grew a buffer until it found a newline would let a router — or anything that can
//! reach the bridge socket — make this process allocate without bound. So overflow is a
//! state, not an error: bytes are consumed and discarded until a newline arrives, and the
//! next line is parsed normally. The overflow is reported so a caller can see it
//! happened, but it is not fatal and it does not poison the reader.

use std::collections::VecDeque;
use thiserror::Error;
use zeroize::Zeroizing;

/// Ceiling on one control line, including its terminator.
///
/// Frozen by Plan 030 section 5. It is also the application endpoint ceiling, so a
/// Destination can fill a control line without the line envelope and the endpoint
/// envelope being two different numbers that can disagree.
pub const MAX_SAM_LINE_BYTES: usize = 4096;

/// Ceiling on whitespace-separated tokens in one line.
pub const MAX_SAM_TOKENS: usize = 64;

/// Ceiling on one option key, such as `STREAM` or `i2cp.leaseSetEncType`.
pub const MAX_SAM_KEY_BYTES: usize = 64;

/// Ceiling on one option value.
///
/// A Destination is the largest value this profile carries, and the largest real one is
/// well under 4096; 3072 leaves room for the `KEY=value` envelope inside the line.
pub const MAX_SAM_VALUE_BYTES: usize = 3072;

/// What a completed line turned out to be.
///
/// `Overflowed` is a line that was discarded. It is deliberately *not* an error in the
/// reading loop: the reader stays usable, which is the property above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamLine {
    /// A complete, well-formed line with its terminator removed.
    ///
    /// `end` is how many bytes the reader had been fed when this line's terminator
    /// arrived. It is what lets a caller that is switching protocols mid-stream recover
    /// the bytes that followed the line: anything past `end` is not a line at all, and
    /// without this the first application bytes could be lost.
    Complete {
        /// The line text, terminator removed.
        text: Zeroizing<String>,
        /// Bytes fed to the reader through this line's terminator, inclusive.
        end: usize,
    },
    /// Bytes exceeding [`MAX_SAM_LINE_BYTES`] were discarded up to the next newline.
    ///
    /// The caller sees this instead of the line so a router that answers with something
    /// enormous is observable rather than silently skipped.
    Overflowed { discarded_bytes: usize },
}

/// One parsed control line: a verb, then bounded key/value options.
///
/// Options are kept in a `Vec` rather than a map because SAM reply ordering is
/// meaningful to a caller classifying a failure, and because insertion order is the only
/// order available for a reply whose keys are not known ahead of time. The vector is
/// bounded by [`MAX_SAM_TOKENS`], so this cannot grow without limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamReply {
    verb: Zeroizing<String>,
    options: Vec<(Zeroizing<String>, Option<Zeroizing<String>>)>,
}

impl SamReply {
    /// The leading verb, e.g. `HELLO` or `STREAM`.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// Every option in the order it appeared.
    pub fn options(&self) -> &[(Zeroizing<String>, Option<Zeroizing<String>>)] {
        &self.options
    }

    /// The first value for `key`, compared case-insensitively.
    ///
    /// SAM option keys are case-insensitive in practice across Java I2P and i2pd, and
    /// comparing case-insensitively here is what keeps the caller from having to know
    /// which router it is talking to.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| value.as_ref())
            .map(|value| value.as_str())
    }

    /// The first value for `key`, requiring it to equal `expected` case-insensitively.
    pub fn has(&self, key: &str, expected: &str) -> bool {
        self.value(key)
            .is_some_and(|value| value.eq_ignore_ascii_case(expected))
    }

    /// Whether `key` appears at all, whatever its value.
    pub fn contains(&self, key: &str) -> bool {
        self.options
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(key))
    }

    /// How many times `key` appears.
    ///
    /// Present because Plan 030 section 5 requires duplicate-option behaviour to be
    /// explicit per reply type, and the only way a caller can be explicit is to be able
    /// to ask.
    pub fn count(&self, key: &str) -> usize {
        self.options
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(key))
            .count()
    }
}

/// Why a complete line was refused.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum LineError {
    #[error("control line exceeded the token ceiling")]
    TooManyTokens,
    #[error("control option key exceeded its ceiling")]
    KeyTooLong,
    #[error("control option value exceeded its ceiling")]
    ValueTooLong,
    /// A NUL, or a CR before the LF, inside a line. SAM has no use for either, and
    /// accepting them would let a peer smuggle a terminator past this parser.
    #[error("control line contained an embedded NUL or CR")]
    EmbeddedControl,
    /// A quote that never closes. Checked because a `MESSAGE` is quoted, and an
    /// unterminated quote would otherwise silently swallow the rest of the line.
    #[error("control line contained an unterminated quoted value")]
    UnterminatedQuote,
    #[error("control line was empty")]
    Empty,
}

/// A bounded, incremental SAM line reader.
///
/// Holds at most one partial line plus the small overflow-discard remainder, so its
/// steady-state memory is bounded by [`MAX_SAM_LINE_BYTES`] regardless of what a peer
/// sends.
#[derive(Debug, Default)]
pub struct LineReader {
    partial: Zeroizing<Vec<u8>>,
    /// Set once a line has outgrown the ceiling, so the next newline ends it as overflow
    /// instead of being appended to a buffer that is already too large.
    overflowing: bool,
    discarded: usize,
    /// Terminator handling: a CR is only meaningful immediately before the LF, so it is
    /// held back one byte rather than being stripped eagerly.
    pending_cr: bool,
    /// Bytes fed to this reader since it was created.
    ///
    /// Never bounded, but it only ever grows by one per fed byte and is reset by
    /// [`Self::reset_fed`], which a caller that has consumed everything it cares about
    /// invokes. A `usize` counter cannot be a denial-of-service vector.
    fed: usize,
}

impl LineReader {
    /// A reader with no state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes and yields every line that completed.
    ///
    /// Partial input is retained. A caller feeding from a socket feeds whatever it read
    /// and handles whatever lines came back; a caller with a whole buffer in hand feeds
    /// it once and handles all of them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SamLine> {
        let mut lines = Vec::new();
        for &byte in bytes {
            self.fed = self.fed.saturating_add(1);
            self.push_byte(byte, &mut lines);
        }
        lines
    }

    /// How many bytes have been discarded by overflow since the reader was created.
    pub fn discarded_bytes(&self) -> usize {
        self.discarded
    }

    /// Whether bytes of an over-long line have been seen without its newline yet.
    ///
    /// A diagnostic, so an operator can tell "the router sent something enormous" from
    /// "the connection was idle".
    pub fn is_overflowing(&self) -> bool {
        self.overflowing
    }

    /// How many bytes have been fed to this reader.
    pub fn fed(&self) -> usize {
        self.fed
    }

    /// Declares the first `count` bytes consumed, so subsequent offsets are relative.
    ///
    /// Used by a caller that has finished with the bytes before a point, which is what
    /// keeps `fed` meaningful across a long-lived control channel.
    pub fn reset_fed(&mut self, count: usize) {
        self.fed = self.fed.saturating_sub(count);
    }

    /// Takes the bytes of an incomplete trailing line.
    ///
    /// A caller reading from a socket needs these: two replies arriving in one TCP
    /// segment means the bytes after the first newline are the whole of the second reply,
    /// and dropping them would hang the next read. Taken rather than borrowed because the
    /// caller has to hand them back on the next `push`.
    pub fn take_partial(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.partial).to_vec()
    }

    fn push_byte(&mut self, byte: u8, lines: &mut Vec<SamLine>) {
        // A CR is only a terminator when the LF follows. Holding it back means an
        // embedded CR is detected rather than silently treated as a line break, which is
        // what would let a peer inject a line boundary.
        if self.pending_cr {
            self.pending_cr = false;
            if byte == b'\n' {
                self.end_line(lines, true);
                return;
            }
            // Not a terminator. The CR is recorded as ordinary content so `finish` sees
            // it and rejects the line; dropping it would silently *repair* an injected
            // line boundary, which is exactly what this reader must not do.
            self.push_content(b'\r');
        }
        match byte {
            b'\n' => self.end_line(lines, false),
            b'\r' => self.pending_cr = true,
            _ => self.push_content(byte),
        }
    }

    fn push_content(&mut self, byte: u8) {
        if self.overflowing {
            // Discarded, counted, and nothing buffered. A peer that never sends a
            // newline therefore costs one integer, not one growing allocation, which is
            // the whole point of overflow being a state rather than an error.
            self.discarded = self.discarded.saturating_add(1);
            return;
        }
        if self.partial.len() >= MAX_SAM_LINE_BYTES - 1 {
            // The ceiling includes the terminator, so a payload of `MAX_SAM_LINE_BYTES`
            // bytes would need one more for the newline. Switching to discard here is
            // what keeps the reader's own accounting identical to the ceiling it claims.
            self.overflowing = true;
            self.discarded = self.discarded.saturating_add(self.partial.len());
            self.partial.clear();
            return;
        }
        self.partial.push(byte);
    }

    fn end_line(&mut self, lines: &mut Vec<SamLine>, _crlf: bool) {
        if self.overflowing {
            let discarded = self.discarded;
            self.overflowing = false;
            self.discarded = 0;
            self.partial.clear();
            lines.push(SamLine::Overflowed {
                discarded_bytes: discarded,
            });
            return;
        }
        let bytes = std::mem::take(&mut self.partial);
        if let Ok(text) = self.finish(&bytes) {
            lines.push(SamLine::Complete {
                text,
                end: self.fed,
            });
        }
        // A refused line is dropped, and the reader stays usable. Same property as
        // overflow: one bad line cannot wedge the connection.
    }

    fn finish(&self, bytes: &[u8]) -> Result<Zeroizing<String>, LineError> {
        // UTF-8 is validated rather than assumed: a router message is text, and a line
        // that is not text is malformed rather than something to lossily repair.
        let text = std::str::from_utf8(bytes).map_err(|_| LineError::EmbeddedControl)?;
        if text.contains('\0') || text.contains('\r') {
            return Err(LineError::EmbeddedControl);
        }
        if text.trim().is_empty() {
            return Err(LineError::Empty);
        }
        Ok(Zeroizing::new(text.to_owned()))
    }
}

/// Parses a complete line into its verb and options.
///
/// Split out from [`LineReader`] so a caller that already has a line — a test, or a
/// reply assembled from a bridge handshake — parses it by the same code path the socket
/// reader uses. There is no second grammar.
pub fn parse_line(text: &str) -> Result<SamReply, LineError> {
    if text.contains('\0') || text.contains('\r') {
        return Err(LineError::EmbeddedControl);
    }
    let mut tokens: VecDeque<String> = tokenize(text)?;
    if tokens.len() > MAX_SAM_TOKENS {
        return Err(LineError::TooManyTokens);
    }
    let verb = tokens.pop_front().ok_or(LineError::Empty)?;
    if verb.len() > MAX_SAM_KEY_BYTES {
        return Err(LineError::KeyTooLong);
    }
    let mut options = Vec::new();
    for token in tokens {
        let Some((key, value)) = token.split_once('=') else {
            if token.len() > MAX_SAM_VALUE_BYTES {
                return Err(LineError::ValueTooLong);
            }
            options.push((Zeroizing::new(token), None));
            continue;
        };
        if key.len() > MAX_SAM_KEY_BYTES {
            return Err(LineError::KeyTooLong);
        }
        // A `KEY="..."` value may contain spaces, so the tokenizer kept it whole. The
        // quotes are stripped here, where the token is known to be a key/value pair.
        let value = value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(value);
        if value.len() > MAX_SAM_VALUE_BYTES {
            return Err(LineError::ValueTooLong);
        }
        options.push((
            Zeroizing::new(key.to_owned()),
            Some(Zeroizing::new(value.to_owned())),
        ));
    }
    Ok(SamReply {
        verb: Zeroizing::new(verb),
        options,
    })
}

/// Splits a line into whitespace-separated tokens, treating a quoted span as one token.
///
/// Splitting naively on whitespace is wrong in both directions for SAM: a `MESSAGE`
/// whose text contains spaces becomes several apparent options, and a value whose text
/// contains `=` is truncated at the first one. Both would turn a router's free-form
/// message into structure the bouncer then acts on.
///
/// Stops at one token past the ceiling rather than counting everything, so a hostile line
/// of a million tokens costs one token of work before being refused.
fn tokenize(text: &str) -> Result<VecDeque<String>, LineError> {
    let bytes = text.as_bytes();
    let mut tokens = VecDeque::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let start = index;
        let mut quoted = false;
        while index < bytes.len() {
            if !quoted && bytes[index].is_ascii_whitespace() {
                break;
            }
            // A quoted span is whitespace-significant, so the loop must run past it.
            if bytes[index] == b'"' {
                quoted = !quoted;
            }
            index += 1;
        }
        if quoted {
            // A quote that opened and never closed, or one that closed twice.
            return Err(LineError::UnterminatedQuote);
        }
        // Stripped only when the *whole* token is quoted. A `KEY="value"` token keeps
        // its key here and has its quotes removed in `parse_line`, which is the only
        // place that knows the token was a key/value pair rather than a bare quoted
        // word.
        let raw = &text[start..index];
        let token = raw
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(raw);
        // No length check here. The line reader has already bounded the whole line by
        // `MAX_SAM_LINE_BYTES`, so a token cannot be arbitrarily long; deciding which
        // ceiling applies is a question about whether the token turned out to be a key or
        // a value, and only the caller knows that.
        tokens.push_back(token.to_owned());
        if tokens.len() > MAX_SAM_TOKENS {
            return Err(LineError::TooManyTokens);
        }
    }
    Ok(tokens)
}

/// Converts one [`SamLine`] into a reply, discarding overflow.
pub fn reply_of(line: &SamLine) -> Result<SamReply, LineError> {
    match line {
        SamLine::Overflowed { .. } => Err(LineError::Empty),
        SamLine::Complete { text, .. } => parse_line(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(unused_variables)]
    fn lines_of(input: &[u8]) -> Vec<String> {
        let mut reader = LineReader::new();
        reader
            .push(input)
            .into_iter()
            .filter_map(|line| match line {
                SamLine::Complete { text, .. } => Some(text.to_string()),
                SamLine::Overflowed { .. } => None,
            })
            .collect()
    }

    #[test]
    fn both_terminators_are_accepted() {
        assert_eq!(lines_of(b"HELLO OK\n"), vec!["HELLO OK"]);
        assert_eq!(lines_of(b"HELLO OK\r\n"), vec!["HELLO OK"]);
        // A bare CR is not a terminator: it is rejected as embedded, because accepting
        // it would let a peer inject a line boundary.
        assert!(lines_of(b"HELLO OK\rSTREAM OK\n").is_empty());
    }

    /// The property the whole module exists for.
    #[test]
    fn an_over_long_line_is_dropped_and_the_next_line_survives() {
        let mut oversized = vec![b'X'; MAX_SAM_LINE_BYTES + 4096];
        oversized.push(b'\n');
        oversized.extend_from_slice(b"STREAM STATUS RESULT=OK\n");
        let mut reader = LineReader::new();
        let produced = reader.push(&oversized);
        assert!(
            matches!(produced.first(), Some(SamLine::Overflowed { discarded_bytes }) if *discarded_bytes > MAX_SAM_LINE_BYTES),
            "an over-long line is reported, not silently dropped: {produced:?}"
        );
        let survivors: Vec<String> = produced
            .iter()
            .filter_map(|line| match line {
                SamLine::Complete { text, .. } => Some(text.to_string()),
                SamLine::Overflowed { .. } => None,
            })
            .collect();
        assert_eq!(
            survivors,
            vec!["STREAM STATUS RESULT=OK"],
            "the reader stays usable after overflow"
        );
    }

    #[test]
    fn the_exact_maximum_line_is_accepted() {
        // The ceiling includes the terminator, so the payload is one shorter.
        let payload = vec![b'A'; MAX_SAM_LINE_BYTES - 1];
        let mut input = payload.clone();
        input.push(b'\n');
        let produced = LineReader::new().push(&input);
        assert!(
            matches!(produced.as_slice(), [SamLine::Complete { text, .. }] if text.len() == MAX_SAM_LINE_BYTES - 1),
            "a line at the maximum is accepted: {produced:?}"
        );

        let mut over = payload;
        over.push(b'A');
        over.push(b'\n');
        let produced = LineReader::new().push(&over);
        assert!(
            matches!(produced.as_slice(), [SamLine::Overflowed { .. }]),
            "one byte over the maximum overflows: {produced:?}"
        );
    }

    /// Partial reads are the normal case on a socket, so a byte-at-a-time feed has to
    /// produce the same lines as one whole-buffer feed.
    #[test]
    fn fragmented_input_is_read_identically_to_whole_input() {
        let whole =
            b"HELLO OK\r\nSESSION STATUS RESULT=OK ID=abc\nSTREAM STATUS STREAM=CANT_REACH_PEER\n";
        assert_eq!(lines_of(whole).len(), 3);
        let mut reader = LineReader::new();
        let mut produced = Vec::new();
        for byte in whole {
            produced.extend(reader.push(&[*byte]));
        }
        let texts: Vec<String> = produced
            .iter()
            .filter_map(|line| match line {
                SamLine::Complete { text, .. } => Some(text.to_string()),
                SamLine::Overflowed { .. } => None,
            })
            .collect();
        assert_eq!(texts, lines_of(whole));
    }

    #[test]
    fn nul_and_embedded_control_are_refused() {
        let mut input = b"HELLO\0OK\n".to_vec();
        input.extend_from_slice(b"STREAM OK\n");
        assert_eq!(
            lines_of(&input),
            vec!["STREAM OK"],
            "a NUL is refused and the following line is still read"
        );
    }

    #[test]
    fn options_are_bounded_at_every_ceiling() {
        // Token ceiling.
        let mut tokens: Vec<String> = vec!["VERB".to_owned()];
        for index in 0..MAX_SAM_TOKENS + 2 {
            tokens.push(format!("K{index}=v"));
        }
        assert_eq!(
            parse_line(&tokens.join(" ")),
            Err(LineError::TooManyTokens),
            "more than the token ceiling is refused"
        );
        // Exactly at the ceiling is accepted.
        let mut at_limit: Vec<String> = vec!["VERB".to_owned()];
        for index in 0..MAX_SAM_TOKENS - 1 {
            at_limit.push(format!("K{index}=v"));
        }
        assert!(
            parse_line(&at_limit.join(" ")).is_ok(),
            "exactly the token ceiling is accepted"
        );
        // Key ceiling.
        let long_key = "K".repeat(MAX_SAM_KEY_BYTES + 1);
        assert_eq!(
            parse_line(&format!("VERB {long_key}=v")),
            Err(LineError::KeyTooLong)
        );
        assert!(parse_line(&format!("VERB {} =v", "K".repeat(MAX_SAM_KEY_BYTES))).is_ok());
        // Value ceiling.
        let long_value = "v".repeat(MAX_SAM_VALUE_BYTES + 1);
        assert_eq!(
            parse_line(&format!("VERB K={long_value}")),
            Err(LineError::ValueTooLong)
        );
        assert!(parse_line(&format!("VERB K={}", "v".repeat(MAX_SAM_VALUE_BYTES))).is_ok());
    }

    /// A `MESSAGE` is the one value that legitimately contains spaces, so it must be
    /// skipped whole rather than mistaken for several options.
    #[test]
    fn a_quoted_message_is_one_value_not_several_options() {
        let reply = parse_line(r#"STREAM STATUS RESULT=ERROR MESSAGE="cannot reach peer""#)
            .expect("a quoted message parses");
        assert_eq!(reply.verb(), "STREAM");
        assert!(reply.has("RESULT", "ERROR"));
        assert_eq!(reply.value("MESSAGE"), Some("cannot reach peer"));
        assert_eq!(
            reply.options().len(),
            3,
            "STATUS, RESULT, and one MESSAGE rather than four tokens: {:?}",
            reply.options()
        );
    }

    #[test]
    fn an_unterminated_quote_is_refused() {
        assert_eq!(
            parse_line(r#"STREAM STATUS RESULT=ERROR MESSAGE="never closed"#),
            Err(LineError::UnterminatedQuote)
        );
    }

    #[test]
    fn keys_and_values_are_matched_case_insensitively() {
        let reply = parse_line("STREAM STATUS result=OK stream=ID:7").expect("parses");
        assert!(reply.has("RESULT", "ok"));
        assert!(reply.has("result", "OK"));
        assert!(reply.contains("STREAM"));
        assert_eq!(reply.value("Stream"), Some("ID:7"));
    }

    /// Duplicate handling has to be observable for a caller to be explicit about it.
    #[test]
    fn duplicate_options_are_preserved_and_countable() {
        let reply = parse_line("SESSION STATUS RESULT=OK RESULT=ERROR").expect("parses");
        assert_eq!(reply.count("RESULT"), 2);
        assert_eq!(
            reply.value("RESULT"),
            Some("OK"),
            "the first occurrence is the one a first-wins caller reads"
        );
    }

    /// A `Destination` in a session reply is private key material. The buffer it came
    /// from is zeroized, so a caller cannot print it by accident and cannot find it in a
    /// core dump of a long-lived buffer.
    #[test]
    fn parsed_values_are_wrapped_so_they_are_zeroized() {
        let reply = parse_line("SESSION STATUS RESULT=OK DESTINATION=secretkeymaterial").unwrap();
        // The return type is `&str` so callers cannot accidentally keep it, but the
        // backing `String` is a `Zeroizing`, which is what actually guarantees the
        // bytes are wiped when the reply drops. Asserting that here keeps a future
        // refactor from swapping the wrapper for a plain `String` without a test
        // noticing.
        fn assert_zeroized(_: &zeroize::Zeroizing<String>) {}
        let value = reply.options()[1].1.as_ref().expect("the value is present");
        assert_zeroized(value);
        assert_eq!(reply.value("DESTINATION"), Some("secretkeymaterial"));
    }
}
