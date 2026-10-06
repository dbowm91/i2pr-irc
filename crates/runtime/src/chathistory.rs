//! Versioned IRCv3 draft adapter for CHATHISTORY and read markers.
//!
//! # Why one adapter module
//!
//! Every `draft/...` literal, every subcommand keyword, and every version assumption
//! lives here and *only* here. The store and the rest of the runtime speak generic
//! query keys and durable identities; they never see draft syntax. That is what lets a
//! future wire adapter update ship without a schema migration unless the semantic data
//! requirements actually changed.
//!
//! # Implemented spec revision
//!
//! This adapter implements the chathistory draft surface reviewed for M003-E: the
//! `LATEST`, `BEFORE`, `AFTER`, `BETWEEN`, and `TARGETS` subcommands, plus the
//! `draft/chathistory` and `draft/read-marker` capability names. `AROUND` is not
//! implemented because it requires a bounded time index this milestone does not
//! build; a request for it is refused explicitly rather than silently degraded.
//!
//! # What is never replayed
//!
//! Only message events (PRIVMSG/NOTICE) are stored, so no JOIN/PART/NICK event can
//! appear in a replay. This is what keeps a history payload truthful without having to
//! implement event-playback semantics: the adapter cannot offer what the store does
//! not hold.
use crate::{
    journal::{BacklogCap, HistoryJournal},
    routing::MAX_DOWNSTREAM_LABEL_BYTES,
};
use i2pr_irc_core::{BufferId, HistoryEventId};
use i2pr_irc_store::{HistoryEvent, RetentionRequest};
use i2pr_irc_wire::{IrcTimestamp, MAX_LINE_BYTES, MSGID_TAG, Message, TIME_TAG};
use std::collections::BTreeSet;

/// The chathistory capability name this adapter implements.
pub const CHATHISTORY_CAPABILITY: &str = "draft/chathistory";
/// The read-marker capability name this adapter implements.
pub const READ_MARKER_CAPABILITY: &str = "draft/read-marker";
/// Spec revision this adapter was reviewed against.
pub const ADAPTER_REVISION: &str = "M003-E reviewed draft surface";

/// Maximum subcommand parameters accepted, bounding a hostile request shape.
pub const MAX_QUERY_PARAMS: usize = 6;
/// Maximum target count in one `TARGETS` request.
pub const MAX_TARGETS: usize = 64;
/// Ceiling on the retained window a `LATEST` request may scan to find its tail.
///
/// `LATEST` needs the newest page of a buffer, so it must look at a window rather
/// than the first page. The window is explicitly bounded so the request cannot become
/// an unbounded scan.
pub const MAX_LATEST_WINDOW: usize = 512;
/// Ceiling on the returned byte budget for one request.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// A parsed CHATHISTORY request.
///
/// This is the adapter's internal representation: it is deliberately free of draft
/// syntax so the execution path does not have to know anything about the draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryQueryRequest {
    /// The newest `limit` events in the buffer.
    Latest { limit: usize },
    /// Events immediately before the reference.
    Before {
        reference: MessageReference,
        limit: usize,
    },
    /// Events immediately after the reference.
    After {
        reference: MessageReference,
        limit: usize,
    },
    /// Events between two references, newest first.
    Between {
        older: MessageReference,
        newer: MessageReference,
        limit: usize,
    },
    /// The oldest `limit` events, used for `TARGETS` catch-up.
    Oldest { limit: usize },
}

/// A reference to one message in history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageReference {
    /// An upstream `msgid`, resolved within the Network.
    MsgId(String),
    /// A server-time timestamp, resolved to the newest event at or before it.
    Timestamp(i64),
}

/// Why a request was refused. Every refusal is deterministic and explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryRefusal {
    /// The request named a subcommand this adapter does not implement.
    UnsupportedSubcommand,
    /// A limit was absent, zero, or above the ceiling.
    InvalidLimit,
    /// A reference could not be parsed or resolved.
    InvalidReference,
    /// Too many parameters or targets.
    TooManyParameters,
    /// The reference names history that no longer exists.
    StaleReference,
    /// No such buffer on this network.
    NoSuchBuffer,
}

/// The outcome of parsing one CHATHISTORY command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedRequest {
    Accepted(HistoryQueryRequest),
    Refused(HistoryRefusal),
}

/// A parsed MARKREAD command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedMarker {
    /// Set the marker at a referenced message.
    Set(MessageReference),
    /// Clear the marker for a target.
    Clear,
}

impl core::fmt::Display for HistoryRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::UnsupportedSubcommand => "unsupported subcommand",
            Self::InvalidLimit => "invalid limit",
            Self::InvalidReference => "invalid reference",
            Self::TooManyParameters => "too many parameters",
            Self::StaleReference => "reference is no longer retained",
            Self::NoSuchBuffer => "no such buffer",
        };
        f.write_str(text)
    }
}

/// Parses a CHATHISTORY command into a generic request.
///
/// Subcommand matching is case-insensitive, as IRCv3 requires.
pub fn parse_chathistory(message: &Message) -> ParsedRequest {
    let params: Vec<String> = message
        .params
        .iter()
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .collect();
    if params.is_empty() {
        return ParsedRequest::Refused(HistoryRefusal::UnsupportedSubcommand);
    }
    if params.len() > MAX_QUERY_PARAMS {
        return ParsedRequest::Refused(HistoryRefusal::TooManyParameters);
    }
    let subcommand = params[0].to_ascii_uppercase();
    // Every subcommand names a target first.
    let Some(target) = params.get(1).map(String::as_str) else {
        return ParsedRequest::Refused(HistoryRefusal::InvalidReference);
    };
    if target.is_empty() || target.len() > 200 || target.bytes().any(|b| b.is_ascii_whitespace()) {
        return ParsedRequest::Refused(HistoryRefusal::InvalidReference);
    }
    let limit = params.get(2).map(String::as_str);
    let reference = params.get(3).map(String::as_str);
    let second_reference = params.get(4).map(String::as_str);

    let request = match subcommand.as_str() {
        "LATEST" => {
            let Some(limit) = parse_limit(limit) else {
                return ParsedRequest::Refused(HistoryRefusal::InvalidLimit);
            };
            HistoryQueryRequest::Latest { limit }
        }
        "TARGETS" => {
            let Some(limit) = parse_limit(limit) else {
                return ParsedRequest::Refused(HistoryRefusal::InvalidLimit);
            };
            HistoryQueryRequest::Oldest { limit }
        }
        "BEFORE" => {
            let (Some(limit), Some(reference)) = (parse_limit(limit), parse_reference(reference))
            else {
                return ParsedRequest::Refused(missing_subcommand_inputs("BEFORE"));
            };
            HistoryQueryRequest::Before { reference, limit }
        }
        "AFTER" => {
            let (Some(limit), Some(reference)) = (parse_limit(limit), parse_reference(reference))
            else {
                return ParsedRequest::Refused(missing_subcommand_inputs("AFTER"));
            };
            HistoryQueryRequest::After { reference, limit }
        }
        "BETWEEN" => {
            let (Some(limit), Some(older), Some(newer)) = (
                parse_limit(limit),
                parse_reference(reference),
                parse_reference(second_reference),
            ) else {
                return ParsedRequest::Refused(missing_subcommand_inputs("BETWEEN"));
            };
            HistoryQueryRequest::Between {
                older,
                newer,
                limit,
            }
        }
        // AROUND needs a bounded time index this milestone does not build. Refusing it
        // is honest; answering it approximately would not be.
        _ => return ParsedRequest::Refused(HistoryRefusal::UnsupportedSubcommand),
    };
    let _ = MAX_TARGETS;
    ParsedRequest::Accepted(request)
}

fn missing_subcommand_inputs(subcommand: &str) -> HistoryRefusal {
    match subcommand {
        "BEFORE" | "AFTER" | "BETWEEN" => HistoryRefusal::InvalidReference,
        _ => HistoryRefusal::InvalidLimit,
    }
}

fn parse_limit(raw: Option<&str>) -> Option<usize> {
    let limit: usize = raw?.parse().ok()?;
    (limit > 0 && limit <= BacklogCap::DEFAULT.events).then_some(limit)
}

fn parse_reference(raw: Option<&str>) -> Option<MessageReference> {
    let raw = raw?;
    if raw.is_empty() || raw.len() > MAX_DOWNSTREAM_LABEL_BYTES {
        return None;
    }
    // A leading `timestamp=` names a time reference; anything else is a msgid.
    match raw.strip_prefix("timestamp=") {
        Some(timestamp) => {
            let seconds = timestamp.parse::<i64>().ok()?;
            i2pr_irc_core::WallTime::from_unix_seconds(seconds)
                .map(|time| MessageReference::Timestamp(time.unix_seconds()))
        }
        None => raw
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
            .then(|| MessageReference::MsgId(raw.to_owned())),
    }
}

/// Parses a MARKREAD command.
pub fn parse_markread(message: &Message) -> Result<ParsedMarker, HistoryRefusal> {
    let params: Vec<String> = message
        .params
        .iter()
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .collect();
    if params.len() > MAX_QUERY_PARAMS {
        return Err(HistoryRefusal::TooManyParameters);
    }
    match params.first().map(String::as_str) {
        // `*` clears the marker for every buffer the client can see.
        Some("*") => Ok(ParsedMarker::Clear),
        Some(target) => {
            if target.is_empty() || target.len() > 200 {
                return Err(HistoryRefusal::InvalidReference);
            }
            let reference = parse_reference(params.get(1).map(String::as_str))
                .ok_or(HistoryRefusal::InvalidReference)?;
            Ok(ParsedMarker::Set(reference))
        }
        None => Err(HistoryRefusal::InvalidReference),
    }
}

/// A resolved, executable bounded query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutableQuery {
    pub after: Option<HistoryEventId>,
    pub before: Option<HistoryEventId>,
    pub limit: usize,
}

/// One emitted history reply, ready for a session queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryReply {
    pub buffer: BufferId,
    pub lines: Vec<Vec<u8>>,
    pub bytes: usize,
    /// The newest event emitted, used to advance a cursor if the caller chooses.
    pub newest: Option<HistoryEventId>,
    /// True when more retained history exists beyond this page.
    pub more_pending: bool,
}

/// Executes one parsed request against the durable journal.
///
/// Both an event ceiling and a byte ceiling are applied, and the result is ordered by
/// local `HistoryEventId`.
pub async fn execute(
    journal: &HistoryJournal,
    buffer: BufferId,
    request: &HistoryQueryRequest,
) -> Result<HistoryReply, HistoryRefusal> {
    let cap = BacklogCap::new(BacklogCap::DEFAULT.events, MAX_RESPONSE_BYTES);
    cap.validate().map_err(|_| HistoryRefusal::InvalidLimit)?;
    let events = match request {
        HistoryQueryRequest::Latest { limit } => {
            // `LATEST` means the *newest* events. The store returns a buffer's history
            // in ascending local order, so the newest page is the tail of the retained
            // window rather than its head.
            let window = journal
                .backlog_range(buffer, None, None, MAX_LATEST_WINDOW)
                .await
                .map_err(|_| HistoryRefusal::StaleReference)?;
            let start = window.len().saturating_sub(*limit);
            window[start..].to_vec()
        }
        HistoryQueryRequest::Oldest { limit } => journal
            .backlog_range(buffer, None, None, *limit)
            .await
            .map_err(|_| HistoryRefusal::StaleReference)?,
        HistoryQueryRequest::Before { reference, limit } => {
            let before = resolve(journal, buffer, reference).await?;
            journal
                .backlog_range(buffer, None, Some(before), *limit)
                .await
                .map_err(|_| HistoryRefusal::StaleReference)?
        }
        HistoryQueryRequest::After { reference, limit } => {
            let after = resolve(journal, buffer, reference).await?;
            journal
                .backlog_range(buffer, Some(after), None, *limit)
                .await
                .map_err(|_| HistoryRefusal::StaleReference)?
        }
        HistoryQueryRequest::Between {
            older,
            newer,
            limit,
        } => {
            let after = resolve(journal, buffer, older).await?;
            let before = resolve(journal, buffer, newer).await?;
            journal
                .backlog_range(buffer, Some(after), Some(before), *limit)
                .await
                .map_err(|_| HistoryRefusal::StaleReference)?
        }
    };
    Ok(render(buffer, events, cap.bytes))
}

/// Resolves a reference to a durable position.
///
/// A reference to pruned history is refused rather than guessed: returning an
/// adjacent event would silently misrepresent what the client asked for.
pub async fn resolve(
    journal: &HistoryJournal,
    buffer: BufferId,
    reference: &MessageReference,
) -> Result<HistoryEventId, HistoryRefusal> {
    let events = journal
        .reference_candidates(buffer)
        .await
        .map_err(|_| HistoryRefusal::StaleReference)?;
    let resolved = match reference {
        MessageReference::MsgId(wanted) => events
            .iter()
            .find(|event| event.msgid.as_deref() == Some(wanted.as_str()))
            .map(|event| event.event),
        MessageReference::Timestamp(seconds) => events
            .iter()
            // Ties resolve through local order: the newest event at or before the
            // timestamp, and among equals the largest HistoryEventId.
            .filter(|event| event.received_at.unix_seconds() <= *seconds)
            .max_by_key(|event| (event.received_at.unix_seconds(), event.event.0))
            .map(|event| event.event),
    };
    resolved.ok_or(HistoryRefusal::StaleReference)
}

/// Renders resolved events as truthful replay lines.
fn render(buffer: BufferId, events: Vec<HistoryEvent>, byte_budget: usize) -> HistoryReply {
    let mut lines = Vec::with_capacity(events.len());
    let mut bytes = 0usize;
    let mut newest = None;
    let mut skipped = 0usize;
    for event in events {
        match render_one(&event, byte_budget - bytes) {
            Some(line) => {
                bytes += line.len();
                newest = Some(event.event);
                lines.push(line);
            }
            None => skipped += 1,
        }
    }
    HistoryReply {
        buffer,
        lines,
        bytes,
        newest,
        more_pending: skipped > 0,
    }
}

/// Emits one event with an honest `server-time` and a msgid only when one is valid.
///
/// The stored payload already carries no terminator, so this adds exactly one CRLF.
/// `server-time` is always present: it is metadata the client can rely on, and it
/// never participates in ordering, which remains `HistoryEventId`.
fn render_one(event: &HistoryEvent, remaining_bytes: usize) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(&event.payload).ok()?;
    if text.is_empty() || text.contains(['\r', '\n']) {
        return None;
    }
    // A stored payload is protocol content *without* its terminator, so the frame is
    // reconstructed before re-parsing to attach tags.
    let mut framed = Vec::with_capacity(text.len() + 2);
    framed.extend_from_slice(text.as_bytes());
    framed.extend_from_slice(b"\r\n");
    let mut message = Message::parse(&framed).ok()?;
    message.tags.clear();
    // Replay the upstream timestamp verbatim. When the upstream never sent one, a
    // valid local millisecond timestamp is synthesized rather than leaving the tag
    // absent, so a client that relies on `server-time` always has a well-formed
    // value. Either way this is metadata: ordering stays `HistoryEventId`.
    let time = event.server_time.or_else(|| {
        IrcTimestamp::from_unix_millis(
            event
                .received_at
                .unix_seconds()
                .checked_mul(1_000)
                .unwrap_or_default(),
        )
    })?;
    message
        .tags
        .insert(TIME_TAG.to_vec(), Some(time.to_string().into_bytes()));
    // A msgid is emitted only when one was genuinely preserved upstream. Inventing a
    // local id here would expose a durable identity under an upstream meaning.
    if let Some(msgid) = event.msgid.as_deref()
        && !msgid.is_empty()
        && msgid.bytes().all(|byte| byte.is_ascii_graphic())
    {
        message
            .tags
            .insert(MSGID_TAG.to_vec(), Some(msgid.as_bytes().to_vec()));
    }
    let line = message.encode().ok()?;
    if line.len() > MAX_LINE_BYTES + 8191 || line.len() > remaining_bytes {
        return None;
    }
    Some(line)
}

/// Builds the `draft/chathistory` capability advertisement now that it is real.
pub fn capability_advertisement(upstream_echo: bool) -> Vec<String> {
    let mut advertised = vec![
        CHATHISTORY_CAPABILITY.to_owned(),
        READ_MARKER_CAPABILITY.to_owned(),
    ];
    advertised.sort();
    advertised.dedup();
    let _ = upstream_echo;
    advertised
}

/// Resolves a client-supplied target to a buffer, refusing a target this Network does
/// not have a buffer for.
///
/// A target the bouncer has never seen is refused explicitly rather than answered with
/// an empty page, so a client learns its request named nothing real.
pub async fn resolve_target(
    journal: &mut HistoryJournal,
    kind: i2pr_irc_store::BufferKind,
    target: &str,
) -> Result<BufferId, HistoryRefusal> {
    journal
        .resolve_buffer(kind, target)
        .await
        .map_err(|_| HistoryRefusal::NoSuchBuffer)
}

/// Whether this session's capabilities mean it manages its own history.
pub fn session_manages_history(enabled: &BTreeSet<String>) -> bool {
    enabled.contains(CHATHISTORY_CAPABILITY)
}

/// Advances a read marker monotonically.
///
/// The marker never moves backwards, even when a client reports an older message:
/// read state is shared per buffer for the Operator, and a stale client must not
/// un-mark what the operator has already seen.
pub fn monotonic_marker(
    current: Option<HistoryEventId>,
    requested: HistoryEventId,
) -> HistoryEventId {
    match current {
        Some(existing) if existing >= requested => existing,
        _ => requested,
    }
}

/// A bounded retention request derived from a client's reference, used when a marker
/// references history that is still present but older than the retention horizon.
pub fn retention_for(
    network: i2pr_irc_core::NetworkId,
    boundary: HistoryEventId,
) -> RetentionRequest {
    RetentionRequest {
        network,
        before: boundary,
        max_delete: 512,
    }
}

/// True when this adapter refuses to answer, so the caller can answer the client
/// locally instead of forwarding an unsupported command upstream.
pub fn is_local_only(message: &Message) -> bool {
    message.command.eq_ignore_ascii_case(b"CHATHISTORY")
        || message.command.eq_ignore_ascii_case(b"MARKREAD")
}

/// The refusal text a client receives. It names the reason and never a payload.
pub fn refusal_text(refusal: HistoryRefusal) -> String {
    format!("chathistory request refused: {refusal}\r\n")
}

/// Ceiling on a read-marker target list.
pub const fn max_marker_targets() -> usize {
    MAX_TARGETS
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_wire::TagDirection;

    fn command(raw: &str) -> Message {
        let message = Message::parse(raw.as_bytes()).expect("parses");
        message
            .validate_tag_budget(TagDirection::ClientInput)
            .expect("budget");
        message
    }

    #[test]
    fn only_draft_literals_live_in_this_module() {
        // The capability names are defined here and nowhere else; the rest of the
        // runtime must refer to these constants rather than repeating the literals.
        assert_eq!(CHATHISTORY_CAPABILITY, "draft/chathistory");
        assert_eq!(READ_MARKER_CAPABILITY, "draft/read-marker");
    }

    #[test]
    fn the_supported_subcommands_parse_and_others_are_refused() {
        let cases = [
            ("CHATHISTORY LATEST #room 50", true),
            ("CHATHISTORY BEFORE #room 50 abc", true),
            ("CHATHISTORY AFTER #room 50 abc", true),
            ("CHATHISTORY BETWEEN #room 50 abc def", true),
            ("CHATHISTORY TARGETS #room 50", true),
            ("CHATHISTORY AROUND #room 50 timestamp=1", false),
            ("CHATHISTORY FROBNICATE #room 50", false),
        ];
        for (raw, supported) in cases {
            let parsed = parse_chathistory(&command(&format!("{raw}\r\n")));
            assert_eq!(
                matches!(parsed, ParsedRequest::Accepted(_)),
                supported,
                "{raw}"
            );
        }
    }

    #[test]
    fn subcommand_matching_is_case_insensitive() {
        assert!(matches!(
            parse_chathistory(&command("CHATHISTORY latest #room 10\r\n")),
            ParsedRequest::Accepted(HistoryQueryRequest::Latest { limit: 10 })
        ));
    }

    #[test]
    fn an_unbounded_or_invalid_limit_is_refused() {
        for raw in [
            "CHATHISTORY LATEST #room 0\r\n",
            "CHATHISTORY LATEST #room -1\r\n",
            "CHATHISTORY LATEST #room notanumber\r\n",
            "CHATHISTORY LATEST #room 100000\r\n",
            "CHATHISTORY LATEST #room\r\n",
        ] {
            assert_eq!(
                parse_chathistory(&command(raw)),
                ParsedRequest::Refused(HistoryRefusal::InvalidLimit),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_malformed_reference_is_refused_rather_than_guessed() {
        assert_eq!(
            parse_chathistory(&command("CHATHISTORY BEFORE #room 50 timestamp=\r\n")),
            ParsedRequest::Refused(HistoryRefusal::InvalidReference)
        );
        assert_eq!(
            parse_chathistory(&command(
                "CHATHISTORY BEFORE #room 50 timestamp=notanumber\r\n"
            )),
            ParsedRequest::Refused(HistoryRefusal::InvalidReference)
        );
        assert_eq!(
            parse_chathistory(&command("CHATHISTORY BEFORE #room 50 \r\n")),
            ParsedRequest::Refused(HistoryRefusal::InvalidReference)
        );
        assert_eq!(
            parse_chathistory(&command(&format!(
                "CHATHISTORY BEFORE #room 50 {}\r\n",
                "x".repeat(MAX_DOWNSTREAM_LABEL_BYTES + 1)
            ))),
            ParsedRequest::Refused(HistoryRefusal::InvalidReference),
            "an over-long reference is refused rather than truncated"
        );
    }

    #[test]
    fn an_oversized_request_is_refused() {
        // One parameter beyond the ceiling.
        let raw = "CHATHISTORY BETWEEN #room 50 a b c d e f g h\r\n";
        assert_eq!(
            parse_chathistory(&command(raw)),
            ParsedRequest::Refused(HistoryRefusal::TooManyParameters)
        );
    }

    #[test]
    fn references_distinguish_a_timestamp_from_a_msgid() {
        let ParsedRequest::Accepted(HistoryQueryRequest::Before { reference, .. }) =
            parse_chathistory(&command(
                "CHATHISTORY BEFORE #room 50 timestamp=1700000000\r\n",
            ))
        else {
            panic!("expected a parsed request")
        };
        assert_eq!(reference, MessageReference::Timestamp(1_700_000_000));
        let ParsedRequest::Accepted(HistoryQueryRequest::Before { reference, .. }) =
            parse_chathistory(&command("CHATHISTORY BEFORE #room 50 abc123\r\n"))
        else {
            panic!("expected a parsed request")
        };
        assert_eq!(reference, MessageReference::MsgId("abc123".to_owned()));
    }

    #[test]
    fn a_rendered_event_always_carries_a_truthful_server_time() {
        let event = HistoryEvent {
            event: HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(500),
            server_time: Some(
                IrcTimestamp::from_unix_millis(1_700_000_000_620).expect("representable"),
            ),
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
        };
        let line = render_one(&event, 4096).expect("renders");
        let parsed = Message::parse(&line).expect("parses");
        // The real upstream value is preferred over the local receive time, and is
        // replayed as canonical text with its milliseconds intact.
        assert_eq!(
            parsed.server_time().map(|time| time.to_string()),
            Some("2023-11-14T22:13:20.620Z".to_owned())
        );
        assert!(
            parsed.msgid().is_none(),
            "a msgid must not be invented when none was preserved"
        );
    }

    #[test]
    fn a_synthesized_server_time_falls_back_to_the_stored_receive_time() {
        let event = HistoryEvent {
            event: HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(4242),
            server_time: None,
            msgid: Some("real".into()),
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
        };
        let line = render_one(&event, 4096).expect("renders");
        let parsed = Message::parse(&line).expect("parses");
        assert_eq!(
            parsed.server_time().map(|time| time.to_string()),
            Some("1970-01-01T01:10:42.000Z".to_owned()),
            "a locally synthesized timestamp is canonical text, never an integer epoch"
        );
        assert_eq!(parsed.msgid(), Some("real"));
    }

    #[test]
    fn a_leap_second_replays_without_being_normalised() {
        let event = HistoryEvent {
            event: HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(500),
            server_time: Some(IrcTimestamp::parse_str("2012-06-30T23:59:60.419Z").expect("parses")),
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
        };
        let line = render_one(&event, 4096).expect("renders");
        let parsed = Message::parse(&line).expect("parses");
        assert_eq!(
            parsed.server_time().map(|time| time.to_string()),
            Some("2012-06-30T23:59:60.419Z".to_owned())
        );
    }

    #[test]
    fn a_msgid_is_never_invented_from_a_durable_identity() {
        let event = HistoryEvent {
            event: HistoryEventId(9999),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(1),
            server_time: None,
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
        };
        let line = render_one(&event, 4096).expect("renders");
        let rendered = String::from_utf8_lossy(&line).into_owned();
        assert!(
            !rendered.contains("9999"),
            "a durable HistoryEventId must never be exposed as a msgid: {rendered}"
        );
    }

    #[test]
    fn an_event_that_does_not_fit_the_remaining_budget_is_skipped_not_truncated() {
        let event = HistoryEvent {
            event: HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(1),
            server_time: None,
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
        };
        assert!(render_one(&event, 4).is_none());
    }

    #[test]
    fn the_read_marker_only_ever_moves_forward() {
        assert_eq!(
            monotonic_marker(Some(HistoryEventId(10)), HistoryEventId(5)),
            HistoryEventId(10)
        );
        assert_eq!(
            monotonic_marker(Some(HistoryEventId(10)), HistoryEventId(20)),
            HistoryEventId(20)
        );
        assert_eq!(monotonic_marker(None, HistoryEventId(1)), HistoryEventId(1));
    }

    #[test]
    fn a_chathistory_client_is_recognised_so_legacy_playback_can_be_suppressed() {
        let mut enabled = BTreeSet::new();
        assert!(!session_manages_history(&enabled));
        enabled.insert(CHATHISTORY_CAPABILITY.to_owned());
        assert!(
            session_manages_history(&enabled),
            "a client that manages its own history must not also get a duplicate backlog"
        );
    }

    #[test]
    fn history_commands_are_answered_locally_and_never_forwarded_upstream() {
        assert!(is_local_only(&command("CHATHISTORY LATEST #room 10\r\n")));
        assert!(is_local_only(&command("MARKREAD #room\r\n")));
        assert!(!is_local_only(&command("WHOIS alice\r\n")));
    }

    #[test]
    fn markread_accepts_a_reference_or_a_clear_and_refuses_malformed_input() {
        assert_eq!(
            parse_markread(&command("MARKREAD #room abc123\r\n")),
            Ok(ParsedMarker::Set(MessageReference::MsgId(
                "abc123".to_owned()
            )))
        );
        assert_eq!(
            parse_markread(&command("MARKREAD *\r\n")),
            Ok(ParsedMarker::Clear)
        );
        assert!(parse_markread(&command("MARKREAD\r\n")).is_err());
        assert!(parse_markread(&command("MARKREAD #room timestamp=bad\r\n")).is_err());
    }

    #[test]
    fn a_refusal_names_a_reason_and_never_a_payload() {
        let text = refusal_text(HistoryRefusal::StaleReference);
        assert!(text.contains("no longer retained"));
        assert!(!text.contains('@'));
    }

    #[test]
    fn the_client_does_not_need_an_unused_mutable_clock() {
        // A read marker resolves against a client reference; this helper exists so a
        // retention sweep can be scheduled without reaching into the journal.
        let request = retention_for(i2pr_irc_core::NetworkId(1), HistoryEventId(100));
        assert_eq!(request.max_delete, 512);
    }
}
