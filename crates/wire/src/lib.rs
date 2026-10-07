//! Bounded byte-oriented IRC/IRCv3 message representation.
use std::collections::BTreeMap;

pub mod timestamp;

pub use timestamp::{IrcTimestamp, TIMESTAMP_BYTES, TimestampError};

/// Maximum ordinary IRC message size, including CRLF (RFC 2812 §2.3).
pub const MAX_LINE_BYTES: usize = 512;
/// Maximum tag prefix, including `@` and the separating space (IRCv3 message-tags).
pub const MAX_TAG_PREFIX_BYTES: usize = 8191;
/// Maximum complete tagged line: tag prefix plus ordinary message remainder.
/// The nick portion of a wire prefix, if it names one.
///
/// A prefix is `nick!user@host`, any part of which may be absent, so the nick is whatever
/// precedes the first `!` or `@`. Returns `None` for a prefix that is not valid UTF-8
/// rather than substituting a lossy string: a caller comparing this against a known
/// nickname must not be told it matched a nick that merely decoded to the same bytes
/// after replacement characters were substituted.
///
/// This lives here rather than in each consumer because three subsystems need it -- the
/// journal's sender field, the echo-message direction judgement, and the store's
/// migration backfill -- and three copies of "the nick is the part before the first
/// separator" is how they come to disagree about the same frame.
pub fn prefix_nick(prefix: &[u8]) -> Option<&str> {
    let nick = prefix
        .split(|byte| *byte == b'!' || *byte == b'@')
        .next()
        .unwrap_or(prefix);
    // An empty nick is not a nick: `!user@host` has none, and returning an empty string
    // would compare equal to a bouncer whose nickname is empty.
    if nick.is_empty() {
        return None;
    }
    std::str::from_utf8(nick).ok()
}

pub const MAX_TAGGED_LINE_BYTES: usize = MAX_TAG_PREFIX_BYTES + MAX_LINE_BYTES;
/// Maximum opaque tag data from one origin, excluding separators.
pub const MAX_TAG_DATA_BYTES: usize = 4094;
pub const MAX_PARAMS: usize = 15;
pub const MAX_TAGS: usize = 128;
pub const MAX_TOKEN_BYTES: usize = 512;
pub const MAX_DECODED_MESSAGES_PER_PUSH: usize = 256;
/// IRCv3 `server-time` tag name.
pub const TIME_TAG: &[u8] = b"time";
/// IRCv3 `msgid` tag name.
pub const MSGID_TAG: &[u8] = b"msgid";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Empty,
    MissingTerminator,
    TooLong,
    InvalidFraming,
    InvalidCommand,
    TooManyParams,
    TooManyTags,
    TokenTooLong,
    InvalidUtf8TagValue,
    TagBudgetExceeded,
    TooManyMessages,
}

/// Origin-dependent policy for validating tag-data budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagDirection {
    ClientInput,
    ServerOutput,
}

#[derive(Clone, Debug)]
pub struct Message {
    /// Case-sensitive opaque tag keys; a duplicate key retains its final value.
    pub tags: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    pub prefix: Option<Vec<u8>>,
    pub command: Vec<u8>,
    pub params: Vec<Vec<u8>>,
    trailing_parameter: bool,
    raw_client_tag_data_len: Option<usize>,
    raw_server_tag_data_len: Option<usize>,
}
impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.tags == other.tags
            && self.prefix == other.prefix
            && self.command == other.command
            && self.params == other.params
    }
}
impl Eq for Message {}

impl Message {
    pub fn new(command: impl Into<Vec<u8>>) -> Self {
        Self {
            tags: BTreeMap::new(),
            prefix: None,
            command: command.into(),
            params: Vec::new(),
            trailing_parameter: false,
            raw_client_tag_data_len: None,
            raw_server_tag_data_len: None,
        }
    }
    pub fn parse(line: &[u8]) -> Result<Self, WireError> {
        let tagged = line.first() == Some(&b'@');
        if line.len()
            > if tagged {
                MAX_TAGGED_LINE_BYTES
            } else {
                MAX_LINE_BYTES
            }
        {
            return Err(WireError::TooLong);
        }
        if !line.ends_with(b"\r\n") {
            return Err(WireError::MissingTerminator);
        }
        let body = &line[..line.len() - 2];
        if body.is_empty() {
            return Err(WireError::Empty);
        }
        if body.iter().any(|b| *b == 0 || *b == b'\r' || *b == b'\n') {
            return Err(WireError::InvalidFraming);
        }
        let mut rest = body;
        let mut tags = BTreeMap::new();
        let mut raw_client_tag_data_len = None;
        let mut raw_server_tag_data_len = None;
        if tagged {
            let end = rest
                .iter()
                .position(|b| *b == b' ')
                .ok_or(WireError::InvalidFraming)?;
            if end + 1 > MAX_TAG_PREFIX_BYTES {
                return Err(WireError::TooLong);
            }
            if end == 1 {
                return Err(WireError::InvalidFraming);
            }
            let tag_data = &rest[1..end];
            let field_count = tag_data.split(|b| *b == b';').count();
            if field_count > MAX_TAGS {
                return Err(WireError::TooManyTags);
            }
            let fields: Vec<_> = tag_data.split(|b| *b == b';').collect();
            raw_client_tag_data_len = Some(end - 1);
            let server_fields: Vec<_> = fields
                .iter()
                .filter(|field| field.first() != Some(&b'+'))
                .collect();
            raw_server_tag_data_len = Some(
                server_fields
                    .iter()
                    .map(|field| field.len())
                    .sum::<usize>()
                    .saturating_add(server_fields.len().saturating_sub(1)),
            );
            for field in fields {
                if field.is_empty() {
                    return Err(WireError::InvalidFraming);
                }
                let (key, value) = match field.iter().position(|b| *b == b'=') {
                    Some(i) => {
                        let unescaped = unescape_tag(&field[i + 1..]);
                        let value = match std::str::from_utf8(&unescaped) {
                            Ok(s) if !s.is_empty() => Some(s.as_bytes().to_vec()),
                            _ => None,
                        };
                        (field[..i].to_vec(), value)
                    }
                    None => (field.to_vec(), None),
                };
                // Keys are opaque. Delimiters cannot occur here because this slice was split.
                tags.insert(key, value);
                if tags.len() > MAX_TAGS {
                    return Err(WireError::TooManyTags);
                }
            }
            rest = &rest[end + 1..];
            if rest.len() + 2 > MAX_LINE_BYTES {
                return Err(WireError::TooLong);
            }
        }
        let mut prefix = None;
        if rest.first() == Some(&b':') {
            let end = rest
                .iter()
                .position(|b| *b == b' ')
                .ok_or(WireError::InvalidFraming)?;
            if end < 2 || end - 1 > MAX_TOKEN_BYTES {
                return Err(WireError::InvalidFraming);
            }
            prefix = Some(rest[1..end].to_vec());
            rest = &rest[end + 1..];
        }
        if rest.is_empty() || rest[0] == b' ' {
            return Err(WireError::InvalidCommand);
        }
        let end = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
        let command = rest[..end].to_vec();
        let valid_command = command.iter().all(u8::is_ascii_alphabetic)
            || (command.len() == 3 && command.iter().all(u8::is_ascii_digit));
        if command.is_empty() || command.len() > MAX_TOKEN_BYTES || !valid_command {
            return Err(WireError::InvalidCommand);
        }
        rest = if end < rest.len() {
            &rest[end + 1..]
        } else {
            &[]
        };
        let mut params = Vec::new();
        let mut trailing_parameter = false;
        while !rest.is_empty() {
            if params.len() == MAX_PARAMS {
                return Err(WireError::TooManyParams);
            }
            if rest[0] == b' ' {
                return Err(WireError::InvalidFraming);
            }
            if rest[0] == b':' {
                trailing_parameter = true;
                params.push(rest[1..].to_vec());
                break;
            }
            let e = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
            let p = rest[..e].to_vec();
            if p.len() > MAX_TOKEN_BYTES {
                return Err(WireError::TokenTooLong);
            }
            params.push(p);
            rest = if e < rest.len() { &rest[e + 1..] } else { &[] };
        }
        Ok(Self {
            tags,
            prefix,
            command,
            params,
            trailing_parameter,
            raw_client_tag_data_len,
            raw_server_tag_data_len,
        })
    }

    /// The upstream `server-time` tag, if present and well formed.
    ///
    /// This is metadata only. It never participates in ordering: canonical history
    /// order is the locally assigned sequence, so a skewed, repeated, or missing
    /// server time cannot reorder retained history.
    ///
    /// Returns [`None`] for a tag that is absent, valueless, or not the canonical
    /// `YYYY-MM-DDThh:mm:ss.sssZ` form — including a bare integer epoch, which is
    /// *not* a valid `server-time` and must not be interpreted as one.
    pub fn server_time(&self) -> Option<IrcTimestamp> {
        let value = self.tags.get(TIME_TAG)?.as_ref()?;
        IrcTimestamp::parse(value).ok()
    }
    /// The upstream `msgid` tag, if present and well formed.
    ///
    /// Also metadata only: a dedup index keyed by it would be Network-scoped, because
    /// it is not globally unique and is not the local primary key.
    pub fn msgid(&self) -> Option<&str> {
        let value = self.tags.get(MSGID_TAG)?.as_ref()?;
        std::str::from_utf8(value)
            .ok()
            .filter(|text| !text.is_empty())
    }

    /// The nick this message's prefix names, if it names one.
    ///
    /// See [`prefix_nick`] for the rule and for why it lives here rather than in each
    /// consumer.
    pub fn prefix_nick(&self) -> Option<&str> {
        prefix_nick(self.prefix.as_deref()?)
    }

    pub fn validate_tag_budget(&self, direction: TagDirection) -> Result<(), WireError> {
        if let Some(raw) = match direction {
            TagDirection::ClientInput => self.raw_client_tag_data_len,
            TagDirection::ServerOutput => self.raw_server_tag_data_len,
        } {
            return if raw <= MAX_TAG_DATA_BYTES {
                Ok(())
            } else {
                Err(WireError::TagBudgetExceeded)
            };
        }
        let mut bytes = 0usize;
        for (key, value) in &self.tags {
            let client_only = key.first() == Some(&b'+');
            if direction == TagDirection::ClientInput || !client_only {
                bytes = bytes
                    .saturating_add(key.len())
                    .saturating_add(value.as_ref().map_or(0, |v| v.len()));
            }
        }
        if bytes > MAX_TAG_DATA_BYTES {
            return Err(WireError::TagBudgetExceeded);
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        if self.params.len() > MAX_PARAMS {
            return Err(WireError::TooManyParams);
        }
        if self.tags.len() > MAX_TAGS {
            return Err(WireError::TooManyTags);
        }
        let mut out = Vec::new();
        if !self.tags.is_empty() {
            out.push(b'@');
            for (i, (key, value)) in self.tags.iter().enumerate() {
                if key.is_empty() && value.is_none() {
                    return Err(WireError::InvalidFraming);
                }
                if key
                    .iter()
                    .any(|b| *b == b';' || *b == b'=' || b.is_ascii_whitespace() || *b == 0)
                {
                    return Err(WireError::InvalidFraming);
                }
                if i > 0 {
                    out.push(b';');
                }
                out.extend(key);
                if let Some(value) = value {
                    std::str::from_utf8(value).map_err(|_| WireError::InvalidUtf8TagValue)?;
                    if !value.is_empty() {
                        out.push(b'=');
                        out.extend(escape_tag(value));
                    }
                }
            }
            out.push(b' ');
        }
        if let Some(prefix) = &self.prefix {
            if prefix.is_empty()
                || prefix.len() > MAX_TOKEN_BYTES
                || prefix
                    .iter()
                    .any(|b| b.is_ascii_whitespace() || *b == 0 || *b == b'\r' || *b == b'\n')
            {
                return Err(WireError::InvalidFraming);
            }
            out.push(b':');
            out.extend(prefix);
            out.push(b' ');
        }
        let valid_command = self.command.iter().all(u8::is_ascii_alphabetic)
            || (self.command.len() == 3 && self.command.iter().all(u8::is_ascii_digit));
        if !valid_command {
            return Err(WireError::InvalidCommand);
        }
        out.extend(&self.command);
        for (i, param) in self.params.iter().enumerate() {
            if param.iter().any(|b| *b == 0 || *b == b'\r' || *b == b'\n') {
                return Err(WireError::InvalidFraming);
            }
            out.push(b' ');
            if i + 1 == self.params.len()
                && (self.trailing_parameter
                    || param.is_empty()
                    || param.contains(&b' ')
                    || param.first() == Some(&b':'))
            {
                out.push(b':');
            } else if param.is_empty() || param.contains(&b' ') {
                return Err(WireError::InvalidFraming);
            }
            out.extend(param);
        }
        out.extend(b"\r\n");
        let tagged = !self.tags.is_empty();
        if out.len()
            > if tagged {
                MAX_TAGGED_LINE_BYTES
            } else {
                MAX_LINE_BYTES
            }
        {
            return Err(WireError::TooLong);
        }
        // Apply independent body and tag-prefix ceilings on output too.
        if tagged {
            let separator = out
                .iter()
                .position(|b| *b == b' ')
                .ok_or(WireError::InvalidFraming)?;
            if separator + 1 > MAX_TAG_PREFIX_BYTES || out.len() - separator - 1 > MAX_LINE_BYTES {
                return Err(WireError::TooLong);
            }
        }
        Ok(out)
    }
}

fn unescape_tag(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'\\' {
            i += 1;
            if i == value.len() {
                break;
            }
            out.push(match value[i] {
                b':' => b';',
                b's' => b' ',
                b'\\' => b'\\',
                b'r' => b'\r',
                b'n' => b'\n',
                other => other,
            });
        } else {
            out.push(value[i]);
        }
        i += 1;
    }
    out
}
fn escape_tag(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for byte in value {
        out.extend_from_slice(match byte {
            b';' => b"\\:",
            b' ' => b"\\s",
            b'\\' => b"\\\\",
            b'\r' => b"\\r",
            b'\n' => b"\\n",
            _ => std::slice::from_ref(byte),
        });
    }
    out
}

#[derive(Debug, Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
    dropping: bool,
}
impl LineDecoder {
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Vec<u8>, WireError>> {
        let mut out = Vec::with_capacity(MAX_DECODED_MESSAGES_PER_PUSH + 1);
        for (index, byte) in bytes.iter().enumerate() {
            if self.dropping {
                if *byte == b'\n' {
                    self.dropping = false;
                }
                continue;
            }
            self.buf.push(*byte);
            let cap = if self.buf.first() == Some(&b'@') {
                MAX_TAGGED_LINE_BYTES
            } else {
                MAX_LINE_BYTES
            };
            if self.buf.len() > cap {
                self.buf.clear();
                // An over-long line is discarded through its own terminating LF.
                // When the overflowing byte is that LF the line is already complete,
                // so the next byte starts a fresh line; otherwise the rest of this
                // line is still pending and must be dropped through its LF.
                self.dropping = *byte != b'\n';
                if out.len() == MAX_DECODED_MESSAGES_PER_PUSH {
                    self.dropping = bytes[index + 1..].last().is_some_and(|tail| *tail != b'\n');
                    out.push(Err(WireError::TooManyMessages));
                    break;
                }
                out.push(Err(WireError::TooLong));
                continue;
            }
            if *byte == b'\n' {
                if out.len() == MAX_DECODED_MESSAGES_PER_PUSH {
                    self.buf.clear();
                    self.dropping = bytes[index + 1..].last().is_some_and(|tail| *tail != b'\n');
                    out.push(Err(WireError::TooManyMessages));
                    break;
                }
                let line = std::mem::take(&mut self.buf);
                out.push(Message::parse(&line).map(|_| line));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tags_unknown_command_and_colon_roundtrip() {
        let m = Message::parse(b"@x=one\\stwo;x=final :nick FUTURE #c ::leading\r\n").unwrap();
        assert_eq!(m.tags.get(b"x" as &[u8]), Some(&Some(b"final".to_vec())));
        assert_eq!(Message::parse(&m.encode().unwrap()).unwrap(), m);
    }
    #[test]
    fn duplicate_empty_and_missing_tag_values_follow_spec() {
        let message = Message::parse(b"@empty=;missing;empty=last CMD\r\n").unwrap();
        assert_eq!(
            message.tags.get(b"empty" as &[u8]),
            Some(&Some(b"last".to_vec()))
        );
        assert_eq!(message.tags.get(b"missing" as &[u8]), Some(&None));
        let normalized = Message::parse(b"@empty CMD\r\n").unwrap();
        assert_eq!(Message::parse(b"@empty= CMD\r\n").unwrap(), normalized);
    }
    #[test]
    fn tag_escapes_follow_spec() {
        assert_eq!(unescape_tag(b"a\\:b\\sc\\r\\n\\q\\"), b"a;b c\r\nq");
    }
    #[test]
    fn tag_opaque_keys_and_invalid_utf8_value() {
        let m = Message::parse(b"@bad.key=\xff;=empty;flag CMD\r\n").unwrap();
        assert!(m.tags.contains_key(b"bad.key" as &[u8]));
        assert!(m.tags.contains_key(b"" as &[u8]));
        let m = Message::parse(b"@a.b=\xff;+client=x CMD\r\n").unwrap();
        assert_eq!(m.tags.get(b"a.b" as &[u8]), Some(&None));
        assert!(m.validate_tag_budget(TagDirection::ClientInput).is_ok());
    }
    #[test]
    fn commands_and_framing_are_strict() {
        for line in [
            b"CMD123\r\n".as_slice(),
            b"12\r\n",
            b"1234\r\n",
            b"CMD  a\r\n",
        ] {
            assert!(Message::parse(line).is_err(), "{line:?}")
        }
        assert!(Message::parse(b"999 x\r\n").is_ok());
        assert!(Message::parse(b"FUTURE x\r\n").is_ok());
        for malformed in [
            b"CMD x\0y\r\n".as_slice(),
            b"CMD x\ry\r\n",
            b"CMD x\ny\r\n",
            b"CMD x\n",
        ] {
            assert!(Message::parse(malformed).is_err(), "{malformed:?}")
        }
    }
    #[test]
    fn ordinary_max_and_max_plus_one() {
        let good = vec![b'A'; MAX_LINE_BYTES - 2];
        let mut line = good;
        line.extend(b"\r\n");
        assert_eq!(
            Message::parse(&line).unwrap().command.len(),
            MAX_LINE_BYTES - 2
        );
        let mut too = vec![b'A'; MAX_LINE_BYTES - 1];
        too.extend(b"\r\n");
        assert_eq!(Message::parse(&too), Err(WireError::TooLong));
    }
    #[test]
    fn prefix_maximal_fitting_token_is_accepted() {
        let prefix_len = MAX_LINE_BYTES - 5;
        let mut line = vec![b':'];
        line.extend(std::iter::repeat_n(b'n', prefix_len));
        line.extend_from_slice(b" X\r\n");
        assert!(Message::parse(&line).is_ok());
        line.insert(1, b'n');
        assert_eq!(Message::parse(&line), Err(WireError::TooLong));
    }
    #[test]
    fn tag_prefix_and_body_are_independent_limits() {
        let mut prefix = vec![b'@'];
        prefix.extend(std::iter::repeat_n(b'a', MAX_TAG_PREFIX_BYTES - 2));
        prefix.push(b' ');
        prefix.extend_from_slice(b"X\r\n");
        assert!(Message::parse(&prefix).is_ok());
        let mut over = vec![b'@'];
        over.extend(std::iter::repeat_n(b'a', MAX_TAG_PREFIX_BYTES - 1));
        over.extend_from_slice(b" X\r\n");
        assert_eq!(Message::parse(&over), Err(WireError::TooLong));
        let mut body = vec![b'@'];
        body.push(b'a');
        body.push(b' ');
        body.extend(std::iter::repeat_n(b'X', MAX_LINE_BYTES - 2));
        body.extend_from_slice(b"\r\n");
        assert!(Message::parse(&body).is_ok());
        body.insert(body.len() - 2, b'X');
        assert_eq!(Message::parse(&body), Err(WireError::TooLong));
    }
    #[test]
    fn tag_data_budget_max_and_max_plus_one() {
        let at_limit = format!("@x={} CMD\r\n", "a".repeat(MAX_TAG_DATA_BYTES - 2));
        let over = format!("@x={} CMD\r\n", "a".repeat(MAX_TAG_DATA_BYTES - 1));
        assert!(
            Message::parse(at_limit.as_bytes())
                .unwrap()
                .validate_tag_budget(TagDirection::ClientInput)
                .is_ok()
        );
        assert_eq!(
            Message::parse(over.as_bytes())
                .unwrap()
                .validate_tag_budget(TagDirection::ClientInput),
            Err(WireError::TagBudgetExceeded)
        );
        let mixed = Message::parse(
            format!(
                "@server=x;+client={}: CMD\r\n",
                "z".repeat(MAX_TAG_DATA_BYTES)
            )
            .as_bytes(),
        )
        .unwrap();
        assert!(
            mixed
                .validate_tag_budget(TagDirection::ServerOutput)
                .is_ok()
        );
        assert_eq!(
            mixed.validate_tag_budget(TagDirection::ClientInput),
            Err(WireError::TagBudgetExceeded)
        );
    }
    #[test]
    fn parameter_count_max_and_max_plus_one() {
        let max = format!("CMD{}\r\n", " a".repeat(MAX_PARAMS));
        let over = format!("CMD{}\r\n", " a".repeat(MAX_PARAMS + 1));
        assert_eq!(
            Message::parse(max.as_bytes()).unwrap().params.len(),
            MAX_PARAMS
        );
        assert_eq!(
            Message::parse(over.as_bytes()),
            Err(WireError::TooManyParams)
        );
    }
    #[test]
    fn tag_count_max_and_max_plus_one() {
        let at_limit = format!(
            "@{} CMD\r\n",
            (0..MAX_TAGS)
                .map(|i| format!("t{i}"))
                .collect::<Vec<_>>()
                .join(";")
        );
        let over = format!(
            "@{} CMD\r\n",
            (0..=MAX_TAGS)
                .map(|i| format!("t{i}"))
                .collect::<Vec<_>>()
                .join(";")
        );
        assert_eq!(
            Message::parse(at_limit.as_bytes()).unwrap().tags.len(),
            MAX_TAGS
        );
        assert_eq!(Message::parse(over.as_bytes()), Err(WireError::TooManyTags));
    }
    #[test]
    fn line_decoder_handles_every_split_and_concatenation() {
        let line = b"@time=now :nick PRIVMSG #chan ::colon and spaces\r\n";
        for split in 0..=line.len() {
            let mut d = LineDecoder::default();
            let mut decoded = d.push(&line[..split]);
            decoded.extend(d.push(&line[split..]));
            assert_eq!(decoded.len(), 1, "split={split}");
            assert_eq!(
                Message::parse(decoded[0].as_ref().unwrap()).unwrap(),
                Message::parse(line).unwrap()
            );
        }
        let mut d = LineDecoder::default();
        assert_eq!(d.push(b"PING :a\r\nPONG :b\r\n").len(), 2);
    }
    #[test]
    fn deterministic_arbitrary_bytes_never_panic_or_exceed_decoder_bound() {
        let mut seed = 0x8a5cd789635d2dffu64;
        for _ in 0..5000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let n = (seed as usize) % MAX_TAGGED_LINE_BYTES;
            let mut bytes = Vec::with_capacity(n);
            for _ in 0..n {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                bytes.push(seed as u8)
            }
            let parsed = Message::parse(&bytes);
            if let Ok(message) = parsed {
                let encoded = message.encode().unwrap();
                assert_eq!(Message::parse(&encoded), Ok(message));
            }
            let mut decoder = LineDecoder::default();
            for chunk in bytes.chunks(17) {
                let _ = decoder.push(chunk);
                assert!(decoder.buffered_len() <= MAX_TAGGED_LINE_BYTES);
            }
        }
    }
    #[test]
    fn line_decoder_discards_oversize_and_resumes() {
        let mut d = LineDecoder::default();
        assert!(d.push(b"PING :x\r").is_empty());
        assert_eq!(d.push(b"\n").len(), 1);
        let mut x = vec![b'A'; MAX_LINE_BYTES + 1];
        x.extend_from_slice(b"\r\nPING :ok\r\n");
        let out = d.push(&x);
        assert_eq!(out.len(), 2);
        assert!(out[0].is_err());
        assert!(out[1].is_ok());
        assert!(d.buffered_len() <= MAX_TAGGED_LINE_BYTES);
    }
    #[test]
    fn an_over_long_line_is_discarded_through_its_own_terminator_only() {
        // The byte that overflows the ceiling is often the line's own LF. Recovery
        // must not then wait for a second LF, which would silently discard the
        // following real line.
        let mut decoder = LineDecoder::default();
        let mut input = vec![b'A'; MAX_LINE_BYTES + 1];
        input.extend_from_slice(b"\r\nPING :after\r\n");
        let out = decoder.push(&input);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0], Err(WireError::TooLong));
        assert_eq!(
            out[1].as_ref().expect("second line decodes"),
            b"PING :after\r\n"
        );
        assert_eq!(decoder.buffered_len(), 0);
    }
    #[test]
    fn a_mid_line_overflow_discards_through_the_pending_terminator() {
        let mut decoder = LineDecoder::default();
        let mut input = vec![b'A'; MAX_LINE_BYTES + 1];
        input.extend_from_slice(b"tail\r\nPING :after\r\n");
        let out = decoder.push(&input);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0], Err(WireError::TooLong));
        assert_eq!(
            out[1].as_ref().expect("second line decodes"),
            b"PING :after\r\n"
        );
        assert_eq!(decoder.buffered_len(), 0);
    }
    #[test]
    fn consecutive_over_long_lines_cost_exactly_one_error_each() {
        let mut decoder = LineDecoder::default();
        let mut input = Vec::new();
        for index in 0..3 {
            input.extend(std::iter::repeat_n(b'A', MAX_LINE_BYTES + index));
            input.extend_from_slice(b"\r\n");
        }
        input.extend_from_slice(b"PING :after\r\n");
        let out = decoder.push(&input);
        assert_eq!(out.len(), 4, "{out:?}");
        for item in &out[..3] {
            assert_eq!(item, &Err(WireError::TooLong));
        }
        assert_eq!(
            out[3].as_ref().expect("last line decodes"),
            b"PING :after\r\n"
        );
        assert_eq!(decoder.buffered_len(), 0);
    }
    #[test]
    fn an_over_long_line_split_across_chunks_still_recovers() {
        let mut decoder = LineDecoder::default();
        let mut head = vec![b'A'; MAX_LINE_BYTES + 1];
        head.push(b't');
        assert_eq!(decoder.push(&head), vec![Err(WireError::TooLong)]);
        let mut tail = b"ail\r\nPING :after\r\n".to_vec();
        tail.extend_from_slice(&[b'B'; MAX_TAGGED_LINE_BYTES]);
        let out = decoder.push(&tail);
        assert_eq!(out[0].as_ref().expect("recovered line"), b"PING :after\r\n");
        assert_eq!(decoder.buffered_len(), 0);
    }
    #[test]
    fn a_tagged_over_long_line_recovers_at_its_own_terminator() {
        // A tagged line uses the larger aggregate ceiling, so its terminator can
        // overflow for the same reason.
        let mut decoder = LineDecoder::default();
        let mut input = b"@".to_vec();
        input.extend(std::iter::repeat_n(b'A', MAX_TAGGED_LINE_BYTES));
        input.extend_from_slice(b"\r\nPING :after\r\n");
        let out = decoder.push(&input);
        assert_eq!(out.len(), 2, "{:?}", out.len());
        assert_eq!(out[0], Err(WireError::TooLong));
        assert_eq!(
            out[1].as_ref().expect("second line decodes"),
            b"PING :after\r\n"
        );
        assert_eq!(decoder.buffered_len(), 0);
    }
    #[test]
    fn line_decoder_bounds_outputs_per_push() {
        let mut input = b"PING\r\n".repeat(MAX_DECODED_MESSAGES_PER_PUSH + 1000);
        input.extend_from_slice(b"PI");
        let mut decoder = LineDecoder::default();
        let output = decoder.push(&input);
        assert_eq!(output.len(), MAX_DECODED_MESSAGES_PER_PUSH + 1);
        assert_eq!(output.last(), Some(&Err(WireError::TooManyMessages)));
        assert_eq!(decoder.buffered_len(), 0);
        let resumed = decoder.push(b"NG\r\nPONG\r\n");
        assert_eq!(resumed.len(), 1);
        assert!(Message::parse(resumed[0].as_ref().unwrap()).is_ok());
    }
}
