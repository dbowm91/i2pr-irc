//! Runs the independently authored conformance corpus in
//! `research/irc-conformance/vectors/state.txt` and `cap.txt` against the owned
//! IRC state model and downstream registration/CAP behavior.
//!
//! The corpus states expectations derived from primary specifications, so a
//! failure here is a research finding about the owned implementation.

use i2pr_irc_core::{ClientId, ConnectionGeneration};
use i2pr_irc_runtime::OutboundIntent;
use i2pr_irc_runtime::downstream::{DownstreamContext, DownstreamDisposition, DownstreamSession};

use i2pr_irc_runtime::state::NetworkState;
use i2pr_irc_wire::Message;
use tokio::sync::mpsc;

/// One decoded corpus record.
struct Vector {
    id: String,
    title: String,
    /// Field order is preserved: capability vectors assert progressively after
    /// each client line, so evaluation order is part of the corpus meaning.
    fields: Vec<(String, String)>,
}

impl Vector {
    fn describe(&self) -> String {
        format!("{} {}", self.id, self.title)
    }
}

/// Expands the corpus escapes so byte-exact cases stay exact.
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
                fields: Vec::new(),
            });
        } else if let Some((key, value)) = line.split_once(':')
            && let Some(vector) = current.as_mut()
        {
            vector
                .fields
                .push((key.trim().to_owned(), value.trim().to_owned()));
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

fn listed(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value
            .split(',')
            .map(|item| item.trim().to_owned())
            .collect()
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(value, "true" | "yes" | "1")
}

/// `channel: value` and `key=a,b=c` corpus field helpers.
fn channel_field<'a>(raw: &'a str, vector: &Vector) -> (&'a str, &'a str) {
    let (channel, value) = raw.split_once(':').unwrap_or_else(|| {
        panic!(
            "{}: expected `<channel>: <value>` in {raw:?}",
            vector.describe()
        )
    });
    (channel.trim(), value.trim())
}

/// A fresh generation per vector: state vectors are about interpretation, not
/// about accumulation across vectors. Fields are evaluated in written order so a
/// staged assertion observes the state produced by the lines above it.
fn apply_vector(vector: &Vector) -> NetworkState {
    let mut state = NetworkState::new("bot", &[]);
    let mut last_line = String::new();
    for (key, raw) in &vector.fields {
        match key.as_str() {
            "line" => {
                let message = Message::parse(&unescape(raw)).unwrap_or_else(|error| {
                    panic!("{}: upstream line {raw:?}: {error:?}", vector.describe())
                });
                let outcome = state.apply_line(&message);
                assert!(
                    outcome != i2pr_irc_runtime::state::LineOutcome::Malformed,
                    "{}: upstream line {raw:?} was malformed",
                    vector.describe()
                );
                last_line = raw.clone();
            }
            "check" => {
                assert!(
                    !last_line.is_empty(),
                    "{}: a check group must follow an upstream line",
                    vector.describe()
                );
            }
            "id" | "title" | "spec" | "status" | "why" | "utf8" => {}
            other => assert_state_field(vector, other, raw, &state),
        }
    }
    state
}

/// Evaluates one corpus field against the current state.
fn assert_state_field(vector: &Vector, key: &str, raw: &str, state: &NetworkState) {
    match key {
        "joined" => assert_eq!(
            state.joined_channels(),
            listed(raw),
            "{}: observed membership",
            vector.describe()
        ),
        "pending" => {
            let mut expected = listed(raw);
            expected.sort();
            let mut actual = state.pending_joins();
            actual.sort();
            assert_eq!(actual, expected, "{}: pending attempts", vector.describe());
        }
        "rejected" => {
            let mut expected: Vec<(String, String)> = listed(raw)
                .iter()
                .map(|entry| {
                    let (channel, numeric) = entry.split_once('=').unwrap_or_else(|| {
                        panic!("{}: rejected entry {entry:?}", vector.describe())
                    });
                    (channel.to_owned(), numeric.to_owned())
                })
                .collect();
            expected.sort();
            let mut actual: Vec<(String, String)> = state
                .rejected_joins()
                .into_iter()
                .map(|(channel, numeric)| (channel, numeric.to_owned()))
                .collect();
            actual.sort();
            assert_eq!(actual, expected, "{}: rejected attempts", vector.describe());
        }
        "nick_same" => {
            let comparison = raw;
            let (pair, expected) = comparison
                .rsplit_once('=')
                .unwrap_or_else(|| panic!("{}: nick_same {comparison:?}", vector.describe()));
            let (left, right) = pair
                .split_once(',')
                .unwrap_or_else(|| panic!("{}: nick_same {comparison:?}", vector.describe()));
            let left = String::from_utf8_lossy(&unescape(left.trim())).into_owned();
            let right = String::from_utf8_lossy(&unescape(right.trim())).into_owned();
            assert_eq!(
                state.same_nick(&left, &right),
                parse_bool(expected),
                "{}: casemapping {left:?} vs {right:?}",
                vector.describe()
            );
        }
        "members" => {
            let (channel, value) = channel_field(raw, vector);
            let room = state
                .channels
                .get(channel)
                .unwrap_or_else(|| panic!("{}: unknown channel {channel}", vector.describe()));
            let mut expected = listed(value);
            expected.sort();
            let mut actual: Vec<String> = room
                .members
                .iter()
                .map(|member| match member.symbol {
                    Some(symbol) => format!("{symbol}{}", member.nick),
                    None => member.nick.clone(),
                })
                .collect();
            actual.sort();
            assert_eq!(
                actual,
                expected,
                "{}: members of {channel}",
                vector.describe()
            );
        }
        "symbols" => {
            let (channel, value) = channel_field(raw, vector);
            let room = state
                .channels
                .get(channel)
                .unwrap_or_else(|| panic!("{}: unknown channel {channel}", vector.describe()));
            let mut expected: Vec<(String, String)> = listed(value)
                .iter()
                .map(|entry| {
                    let (nick, symbol) = entry.split_once('=').unwrap_or((entry.as_str(), ""));
                    (nick.trim().to_owned(), symbol.trim().to_owned())
                })
                .collect();
            expected.sort();
            let mut actual: Vec<(String, String)> = room
                .members
                .iter()
                .map(|member| {
                    (
                        member.nick.clone(),
                        member
                            .symbol
                            .map(|symbol| symbol.to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            actual.sort();
            assert_eq!(
                actual,
                expected,
                "{}: symbols of {channel}",
                vector.describe()
            );
        }
        "topic" => {
            let (channel, value) = channel_field(raw, vector);
            let room = state
                .channels
                .get(channel)
                .unwrap_or_else(|| panic!("{}: unknown channel {channel}", vector.describe()));
            let expected = if value == "-" { None } else { Some(value) };
            assert_eq!(
                room.topic.as_deref(),
                expected,
                "{}: topic of {channel}",
                vector.describe()
            );
        }
        "modes" => {
            let (channel, value) = channel_field(raw, vector);
            let room = state
                .channels
                .get(channel)
                .unwrap_or_else(|| panic!("{}: unknown channel {channel}", vector.describe()));
            let rendered = format!("+{}", room.modes.render().unwrap_or_default());
            assert_eq!(rendered, value, "{}: modes of {channel}", vector.describe());
        }
        "modes_complete" => {
            let (channel, value) = channel_field(raw, vector);
            let room = state
                .channels
                .get(channel)
                .unwrap_or_else(|| panic!("{}: unknown channel {channel}", vector.describe()));
            assert_eq!(
                room.modes.is_complete(),
                parse_bool(value),
                "{}: mode completeness of {channel}",
                vector.describe()
            );
        }
        "members_complete" => {
            let (channel, value) = channel_field(raw, vector);
            let complete = state
                .channels
                .get(channel)
                .is_some_and(|room| room.members_complete);
            assert_eq!(
                complete,
                parse_bool(value),
                "{}: membership completeness of {channel}",
                vector.describe()
            );
        }
        "isupport" => {
            // Tokens are separated by spaces because a token may itself contain commas.
            let mut expected: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
            expected.sort();
            let mut actual: Vec<String> = state.isupport.iter().cloned().collect();
            actual.sort();
            assert_eq!(actual, expected, "{}: retained ISUPPORT", vector.describe());
        }
        "total_members" => assert_eq!(
            state.total_members(),
            raw.parse::<usize>().expect("numeric"),
            "{}: aggregate member count",
            vector.describe()
        ),
        other => panic!("{}: unknown state assertion {other:?}", vector.describe()),
    }
}

#[test]
fn state_vectors_match_the_owned_state_model() {
    let vectors = corpus("state.txt");
    let mut checked = 0usize;
    for vector in &vectors {
        apply_vector(vector);
        checked += 1;
    }
    assert!(checked >= 25, "state corpus unexpectedly small: {checked}");
}

/// A session plus the queues a vector observes directly.
struct Session {
    inner: DownstreamSession<tokio::io::DuplexStream>,
    normal_rx: mpsc::Receiver<Vec<u8>>,
    control_rx: mpsc::Receiver<Vec<u8>>,
    upstream_normal_rx: mpsc::Receiver<OutboundIntent>,
    upstream_control: mpsc::Sender<Vec<u8>>,
    upstream_normal: mpsc::Sender<OutboundIntent>,
}

impl Session {
    fn new() -> Self {
        let (client_side, _peer) = tokio::io::duplex(4096);
        let (read, _write) = tokio::io::split(client_side);
        let (control_tx, control_rx) = mpsc::channel(8);
        let (normal_tx, normal_rx) = mpsc::channel(64);
        let (upstream_control, _uc) = mpsc::channel(8);
        let (upstream_normal, upstream_normal_rx) = mpsc::channel(64);
        Self {
            inner: DownstreamSession::new(ClientId(1), read, control_tx, normal_tx),
            normal_rx,
            control_rx,
            upstream_normal_rx,
            upstream_control,
            upstream_normal,
        }
    }

    async fn send(&mut self, state: &NetworkState, line: &[u8]) {
        let context = DownstreamContext {
            generation: ConnectionGeneration(7),
            state,
            upstream_control: &self.upstream_control,
            upstream_normal: &self.upstream_normal,
        };
        self.inner.handle_line(line, &context).expect("accepted");
    }

    async fn drain(&mut self) -> String {
        let mut out = String::new();
        while let Ok(chunk) = self.normal_rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&chunk));
        }
        while let Ok(chunk) = self.control_rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&chunk));
        }
        out
    }

    /// Bytes the session forwarded upstream, which must never contain client CAP.
    async fn upstream_bytes(&mut self) -> String {
        let mut out = String::new();
        while let Ok(intent) = self.upstream_normal_rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&intent.wire));
        }
        out
    }
}

#[tokio::test]
async fn cap_vectors_match_the_owned_session() {
    let vectors = corpus("cap.txt");
    let mut checked = 0usize;
    for vector in &vectors {
        let mut state = NetworkState::new("bot", &[]);
        let mut session = Session::new();
        let mut transcript = String::new();
        // The corpus is walked in written order so each expectation group is
        // evaluated against the session state produced by the client line above
        // it. `check:` marks the boundary of a group for human readers.
        let mut last_client = String::new();
        let mut saw_check = false;
        for (key, raw) in &vector.fields {
            match key.as_str() {
                "state_line" => {
                    let message = Message::parse(&unescape(raw)).unwrap_or_else(|error| {
                        panic!("{}: state line {raw:?}: {error:?}", vector.describe())
                    });
                    state.apply_line(&message);
                }
                "client" => {
                    session.send(&state, &unescape(raw)).await;
                    transcript.push_str(&session.drain().await);
                    last_client = raw.clone();
                    saw_check = false;
                }
                "check" => {
                    assert!(
                        !last_client.is_empty(),
                        "{}: a check group must follow a client line",
                        vector.describe()
                    );
                    saw_check = true;
                }
                "expect" => {
                    assert!(
                        saw_check,
                        "{}: every expectation group must start with `check:`",
                        vector.describe()
                    );
                    let (expectation, value) = raw.split_once('=').unwrap_or((raw.as_str(), ""));
                    let expectation = expectation.trim();
                    let value = value.trim();
                    match expectation {
                        "ready" => assert_eq!(
                            session.inner.is_ready(),
                            parse_bool(value),
                            "{}: ready after {last_client:?}",
                            vector.describe()
                        ),
                        "negotiating" => assert_eq!(
                            session.inner.is_cap_negotiating(),
                            parse_bool(value),
                            "{}: negotiating after {last_client:?}",
                            vector.describe()
                        ),
                        "welcome_count" => assert_eq!(
                            transcript.matches("001 bot").count(),
                            value.parse::<usize>().expect("numeric"),
                            "{}: welcome count after {last_client:?}",
                            vector.describe()
                        ),
                        "contains" => {
                            let needle = String::from_utf8_lossy(&unescape(value)).into_owned();
                            assert!(
                                transcript.contains(&needle),
                                "{}: {value:?} must appear after {last_client:?}; transcript {transcript:?}",
                                vector.describe()
                            );
                        }
                        "absent" => {
                            let needle = String::from_utf8_lossy(&unescape(value)).into_owned();
                            assert!(
                                !transcript.contains(&needle),
                                "{}: {value:?} must not appear after {last_client:?}; transcript {transcript:?}",
                                vector.describe()
                            );
                        }
                        other => panic!("{}: unknown expectation {other:?}", vector.describe()),
                    }
                }
                _ => {}
            }
        }
        // No client CAP traffic may ever be forwarded upstream.
        assert!(
            !session
                .upstream_bytes()
                .await
                .to_uppercase()
                .contains("CAP"),
            "{}: client CAP must not reach upstream",
            vector.describe()
        );
        checked += 1;
    }
    assert!(checked >= 15, "cap corpus unexpectedly small: {checked}");
}

#[tokio::test]
async fn a_client_line_that_violates_the_contract_is_an_explicit_error() {
    // The corpus only asserts accepted flows; this asserts the rejection path
    // still returns an explicit result rather than panicking.
    let state = NetworkState::new("bot", &[]);
    let mut session = Session::new();
    let context = DownstreamContext {
        generation: ConnectionGeneration(7),
        state: &state,
        upstream_control: &session.upstream_control,
        upstream_normal: &session.upstream_normal,
    };
    assert!(
        session
            .inner
            .handle_line(b":evil!u@h PRIVMSG #room :hi\r\n", &context)
            .is_err(),
        "a client prefix is a protocol violation"
    );
    assert_eq!(
        session
            .inner
            .handle_line(b"NICK bot\r\n", &context)
            .expect("accepted"),
        DownstreamDisposition::Attached
    );
}
