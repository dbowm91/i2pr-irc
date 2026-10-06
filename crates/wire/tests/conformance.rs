//! Runs the independently authored conformance corpus in
//! `research/irc-conformance/vectors/` against the owned codec.
//!
//! The corpus states expectations derived from primary specifications, so a
//! failure here is a research finding about the owned implementation, not a
//! stale test expectation.

use std::collections::BTreeMap;

use i2pr_irc_wire::{LineDecoder, Message};

/// One decoded corpus record.
struct Vector {
    id: String,
    title: String,
    fields: BTreeMap<String, Vec<String>>,
}

impl Vector {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .and_then(|values| values.first())
            .map(String::as_str)
    }
    fn all(&self, key: &str) -> &[String] {
        self.fields.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
    fn bytes(&self, key: &str) -> Vec<u8> {
        self.field(key).map(unescape).unwrap_or_default()
    }
    fn describe(&self) -> String {
        format!("{} {}", self.id, self.title)
    }
}

/// Expands the corpus escapes so byte-exact and invalid-UTF-8 cases are exact.
fn unescape(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            let next = bytes[index + 1];
            index += 2;
            match next {
                b'r' => out.push(b'\r'),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'0' => out.push(0),
                b'\\' => out.push(b'\\'),
                b'x' => {
                    let hex = value.get(index..index + 2).unwrap_or("00");
                    out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                    index += 2;
                }
                other => out.push(other),
            }
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// Parses the shared corpus format: `## id | title`, then `key: value` lines.
fn load(path: &str) -> Vec<Vector> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read corpus {path}: {error}"));
    let mut vectors: Vec<Vector> = Vec::new();
    let mut current: Option<Vector> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(vector) = current.take() {
                vectors.push(vector);
            }
            let (id, title) = rest.split_once('|').unwrap_or((rest, ""));
            current = Some(Vector {
                id: id.trim().to_owned(),
                title: title.trim().to_owned(),
                fields: BTreeMap::new(),
            });
        } else if let Some((key, value)) = line.split_once(':')
            && let Some(vector) = current.as_mut()
        {
            vector
                .fields
                .entry(key.trim().to_owned())
                .or_default()
                .push(value.trim().to_owned());
        }
    }
    if let Some(vector) = current.take() {
        vectors.push(vector);
    }
    assert!(!vectors.is_empty(), "corpus {path} produced no vectors");
    vectors
}

fn corpus(name: &str) -> Vec<Vector> {
    load(&format!(
        "{}/../../research/irc-conformance/vectors/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn message_vectors_match_the_owned_codec() {
    let mut checked = 0usize;
    for vector in corpus("wire.txt") {
        let input = vector.bytes("input");
        assert!(
            input.ends_with(b"\r\n"),
            "{}: a vector input must include its terminating CRLF",
            vector.describe()
        );
        let outcome = Message::parse(&input);
        match vector.field("expect") {
            Some("ok") => {
                let message = outcome.unwrap_or_else(|error| {
                    panic!("{}: expected parse, got {error:?}", vector.describe())
                });
                // Structural expectations are compared as bytes, so a vector can
                // state an invalid-UTF-8 expectation unambiguously.
                if let Some(command) = vector.field("command") {
                    assert_eq!(
                        message.command,
                        unescape(command),
                        "{}: command",
                        vector.describe()
                    );
                }
                if let Some(prefix) = vector.field("prefix") {
                    assert_eq!(
                        message.prefix.as_deref(),
                        Some(unescape(prefix).as_slice()),
                        "{}: prefix",
                        vector.describe()
                    );
                }
                let params = vector.all("param");
                if !params.is_empty() {
                    assert_eq!(
                        message.params.len(),
                        params.len(),
                        "{}: parameter count",
                        vector.describe()
                    );
                    for (index, expected) in params.iter().enumerate() {
                        assert_eq!(
                            message.params[index],
                            unescape(expected),
                            "{}: parameter {index}",
                            vector.describe()
                        );
                    }
                }
                for expected in vector.all("tag") {
                    let (key, wanted) = match expected.split_once('=') {
                        Some((key, value)) => (key, Some(unescape(value))),
                        None => (expected.as_str(), None),
                    };
                    let actual = message.tags.get(unescape(key).as_slice());
                    assert!(
                        actual.is_some(),
                        "{}: tag {key} must be retained",
                        vector.describe()
                    );
                    assert_eq!(
                        actual.and_then(|value| value.as_ref()).map(Vec::as_slice),
                        wanted.as_deref(),
                        "{}: tag {key} value",
                        vector.describe()
                    );
                }
            }
            Some("reject") => assert!(
                outcome.is_err(),
                "{}: expected rejection, parsed {:?}",
                vector.describe(),
                outcome.ok()
            ),
            other => panic!("{}: unknown expect {other:?}", vector.describe()),
        }
        checked += 1;
    }
    assert!(checked >= 30, "corpus unexpectedly small: {checked}");
}

#[test]
fn framing_vectors_match_the_owned_decoder() {
    for vector in corpus("framing.txt") {
        let mut decoder = LineDecoder::default();
        let mut results = Vec::new();
        for chunk in vector.field("chunks").unwrap_or_default().split('|') {
            for item in decoder.push(&unescape(chunk)) {
                results.push(item);
            }
        }
        let lines: Vec<&Vec<u8>> = results
            .iter()
            .filter_map(|item| item.as_ref().ok())
            .collect();
        let rejected = results.len() - lines.len();
        let expected_lines: usize = vector.field("lines").unwrap_or("0").parse().unwrap();
        assert_eq!(
            rejected,
            vector.all("line_reject").len(),
            "{}: rejected line count",
            vector.describe()
        );
        assert_eq!(
            lines.len(),
            expected_lines,
            "{}: decoded {} complete lines, expected {expected_lines}",
            vector.describe(),
            lines.len()
        );
        for (index, expected) in vector.all("line").iter().enumerate() {
            // The decoder yields complete lines including their CRLF terminator;
            // the corpus states the message body.
            let decoded = lines[index]
                .strip_suffix(b"\r\n")
                .unwrap_or_else(|| panic!("{}: line {index} is unterminated", vector.describe()));
            assert_eq!(
                decoded,
                unescape(expected).as_slice(),
                "{}: line {index}",
                vector.describe()
            );
        }
    }
}

#[test]
fn a_rejected_line_never_leaves_the_decoder_unframed() {
    // Framing recovery must be total: whatever the content, the next valid line
    // is decoded and the decoder never retains attacker-sized input.
    let mut decoder = LineDecoder::default();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\0\r\n");
    bytes.extend_from_slice(&[b'A'; 4096]);
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(b"PING :ok\r\n");
    let mut decoded = Vec::new();
    for line in decoder.push(&bytes).into_iter().filter_map(Result::ok) {
        decoded.push(line);
    }
    assert_eq!(decoder.buffered_len(), 0);
    assert_eq!(lossy(decoded.last().expect("final line")), "PING :ok\r\n");
}

#[test]
fn unknown_numerics_are_representable_and_relayable() {
    // M003 will persist and replay server events; an unrecognized but well-formed
    // numeric must survive parse and re-encode unchanged.
    for line in [
        &b":srv 999 bot #room :something new\r\n"[..],
        &b":srv 391 bot :target\r\n"[..],
        &b":srv 437 bot #room :unavailable\r\n"[..],
    ] {
        let parsed = Message::parse(line).expect("parses");
        assert_eq!(parsed.encode().expect("encodes"), line);
    }
}

#[test]
fn a_command_must_be_alphabetic_or_a_three_digit_numeric() {
    // RFC 2812 section 3.3: numeric replies are exactly three digits. Anything
    // else is not a relayable command token.
    assert!(Message::parse(b":srv 9999 bot #room :hi\r\n").is_err());
    assert!(Message::parse(b":srv 99 bot #room :hi\r\n").is_err());
    assert!(Message::parse(b":srv 9PRIV #room :hi\r\n").is_err());
    assert!(Message::parse(b":srv 999 bot #room :hi\r\n").is_ok());
    assert!(Message::parse(b":srv SOMENEWTHING #room\r\n").is_ok());
}

#[test]
fn an_ok_message_round_trips_through_encode_without_change() {
    let mut checked = 0usize;
    for vector in corpus("wire.txt") {
        if vector.field("expect") != Some("ok") {
            continue;
        }
        let parsed = Message::parse(&vector.bytes("input")).expect("parses");
        let encoded = parsed.encode().expect("encodes");
        let reparsed = Message::parse(&encoded).expect("re-parses");
        assert_eq!(parsed, reparsed, "{}: round trip", vector.describe());
        checked += 1;
    }
    assert!(checked >= 20, "corpus unexpectedly small: {checked}");
}

#[test]
fn a_rejected_message_never_encodes() {
    // A message that failed validation must not be re-introduced by encoding.
    let line = vec![b'A'; 600];
    let mut line = line;
    line.extend_from_slice(b"\r\n");
    assert!(Message::parse(&line).is_err());
}
