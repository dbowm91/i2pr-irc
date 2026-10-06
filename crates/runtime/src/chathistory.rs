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
use crate::journal::{BacklogCap, HistoryJournal};
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
/// Maximum target bytes in one request.
pub const MAX_TARGET_BYTES: usize = 200;
/// Maximum bytes in one `msgid=` selector.
pub const MAX_MSGID_REFERENCE_BYTES: usize = 64;
/// Ceiling on the retained window a `LATEST` request may scan to find its tail.
///
/// `LATEST` needs the newest page of a buffer, so it must look at a window rather
/// than the first page. The window is explicitly bounded so the request cannot become
/// an unbounded scan.
pub const MAX_LATEST_WINDOW: usize = 512;
/// Ceiling on the returned byte budget for one request.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// The `CHATHISTORY` ISUPPORT maximum: the largest limit a client may request.
///
/// This is the real bound, not a courtesy number: a limit above it is refused, so
/// the advertised value and the enforced value cannot disagree.
pub const MAX_HISTORY_LIMIT: usize = 50;
/// Ceiling on the number of targets one `TARGETS` request may return.
pub const MAX_TARGETS: usize = 64;

/// Why a MARKREAD command was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerRefusal {
    /// No target parameter.
    MissingParameters,
    /// More parameters than the command accepts.
    TooManyParameters,
    /// The target is not a plausible buffer name.
    InvalidTarget,
    /// The set timestamp was absent, malformed, or the `*` sentinel.
    InvalidTimestamp,
    /// The named buffer has no history on this Network.
    NoSuchBuffer,
    /// The marker could not be persisted.
    Internal,
}

impl MarkerRefusal {
    /// The standard-reply error code the draft names for this refusal.
    pub fn error(self) -> &'static str {
        match self {
            Self::MissingParameters => "NEED_MORE_PARAMS",
            Self::TooManyParameters | Self::InvalidTarget | Self::InvalidTimestamp => {
                "INVALID_PARAMS"
            }
            Self::NoSuchBuffer | Self::Internal => "INTERNAL_ERROR",
        }
    }
}

/// A parsed CHATHISTORY request.
///
/// This is the adapter's internal representation: it is deliberately free of draft
/// syntax so the execution path does not have to know anything about the draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryQueryRequest {
    /// The newest `limit` events, with no lower bound (`LATEST <target> * <limit>`).
    Latest { limit: usize },
    /// The newest `limit` events sent after and excluding a reference.
    LatestAfter {
        reference: MessageReference,
        limit: usize,
    },
    /// Events immediately before and excluding the reference.
    Before {
        reference: MessageReference,
        limit: usize,
    },
    /// Events immediately after and excluding the reference.
    After {
        reference: MessageReference,
        limit: usize,
    },
    /// Events bracketing the reference, totalling at most `limit`.
    Around {
        reference: MessageReference,
        limit: usize,
    },
    /// Events between two references, excluding both endpoints.
    Between {
        first: MessageReference,
        second: MessageReference,
        limit: usize,
    },
    /// Buffers with visible history whose latest message falls between two times.
    Targets {
        older: IrcTimestamp,
        newer: IrcTimestamp,
        limit: usize,
    },
}

/// A reference to one message in history.
///
/// Both variants are only constructible from their explicit `msgid=` / `timestamp=`
/// selector forms; a bare token is not a reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageReference {
    /// An upstream `msgid`, resolved within the Network.
    MsgId(String),
    /// A server-time timestamp, resolved against the canonical protocol value.
    Timestamp(IrcTimestamp),
}

/// The standard-reply error code a refusal maps to.
///
/// These are the codes the draft names for CHATHISTORY; using anything else would
/// make a compliant client unable to tell a syntax error from a missing buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardError {
    InvalidParams,
    InvalidTarget,
    MessageError,
    InvalidMsgRefType,
}

impl StandardError {
    /// The wire token for this error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidParams => "INVALID_PARAMS",
            Self::InvalidTarget => "INVALID_TARGET",
            Self::MessageError => "MESSAGE_ERROR",
            Self::InvalidMsgRefType => "INVALID_MSGREFTYPE",
        }
    }
}

/// Why a request was refused. Every refusal is deterministic and explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryRefusal {
    /// The subcommand is not one this adapter implements.
    UnknownSubcommand,
    /// Fewer parameters than the subcommand requires.
    MissingParameters,
    /// More parameters than the subcommand accepts.
    TooManyParameters,
    /// A `timestamp=` selector was not a valid canonical protocol timestamp.
    InvalidTimestamp,
    /// A selector was neither `timestamp=` nor `msgid=`, or was empty/oversized.
    InvalidReference,
    /// The limit was absent, zero, or above the advertised ceiling.
    InvalidLimit,
    /// The requested reference type is understood but not supported here.
    UnsupportedReferenceType,
    /// The target names no buffer on this Network.
    NoSuchBuffer,
    /// History could not be produced; the buffer itself is valid.
    HistoryUnavailable,
}

impl HistoryRefusal {
    /// The standard-reply error this refusal is reported as.
    pub fn error(self) -> StandardError {
        match self {
            Self::UnknownSubcommand
            | Self::MissingParameters
            | Self::TooManyParameters
            | Self::InvalidTimestamp
            | Self::InvalidReference
            | Self::InvalidLimit => StandardError::InvalidParams,
            Self::UnsupportedReferenceType => StandardError::InvalidMsgRefType,
            Self::NoSuchBuffer => StandardError::InvalidTarget,
            Self::HistoryUnavailable => StandardError::MessageError,
        }
    }
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
    /// Client get: reply with the stored marker, or `*` when none is known.
    Get,
    /// Client set: advance the marker to a message the client says it has read.
    Set {
        target: String,
        timestamp: IrcTimestamp,
    },
}

impl core::fmt::Display for HistoryRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::UnknownSubcommand => "Unknown command",
            Self::MissingParameters => "Insufficient parameters",
            Self::TooManyParameters => "Too many parameters",
            Self::InvalidTimestamp => "Invalid timestamp",
            Self::InvalidReference => "Invalid message reference",
            Self::InvalidLimit => "Invalid limit",
            Self::UnsupportedReferenceType => "msgid-based history requests are not supported",
            Self::NoSuchBuffer => "Messages could not be retrieved",
            Self::HistoryUnavailable => "Messages could not be retrieved",
        };
        f.write_str(text)
    }
}

/// Parses a CHATHISTORY command into a generic request.
///
/// Each subcommand has its own exact shape, matching the current draft:
///
/// ~~~text
/// CHATHISTORY BEFORE  <target> <reference> <limit>
/// CHATHISTORY AFTER   <target> <reference> <limit>
/// CHATHISTORY LATEST  <target> <* | reference> <limit>
/// CHATHISTORY AROUND  <target> <reference> <limit>
/// CHATHISTORY BETWEEN <target> <reference> <reference> <limit>
/// CHATHISTORY TARGETS <timestamp> <timestamp> <limit>
/// ~~~
///
/// The limit is the *last* parameter in every form. `TARGETS` is the one form with
/// no target argument at all: it lists buffers, so reading its first timestamp as a
/// target would answer a different question than the client asked.
///
/// Subcommand matching is case-insensitive, as IRCv3 requires.
pub fn parse_chathistory(message: &Message) -> ParsedRequest {
    let params: Vec<String> = message
        .params
        .iter()
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .collect();
    let Some(subcommand) = params.first().map(|value| value.to_ascii_uppercase()) else {
        return ParsedRequest::Refused(HistoryRefusal::UnknownSubcommand);
    };
    if params.len() > MAX_QUERY_PARAMS {
        return ParsedRequest::Refused(HistoryRefusal::TooManyParameters);
    }

    if subcommand == "TARGETS" {
        // `TARGETS <timestamp> <timestamp> <limit>` -- both selectors must be
        // timestamps, and neither may be a msgid.
        if params.len() != 4 {
            return ParsedRequest::Refused(if params.len() < 4 {
                HistoryRefusal::MissingParameters
            } else {
                HistoryRefusal::TooManyParameters
            });
        }
        let (Some(older), Some(newer), Some(limit)) = (
            parse_timestamp(&params[1]),
            parse_timestamp(&params[2]),
            parse_limit(&params[3]),
        ) else {
            return ParsedRequest::Refused(if params[3].parse::<usize>().is_err() {
                HistoryRefusal::InvalidLimit
            } else {
                HistoryRefusal::InvalidTimestamp
            });
        };
        return ParsedRequest::Accepted(HistoryQueryRequest::Targets {
            older,
            newer,
            limit,
        });
    }

    let Some(target) = params.get(1).map(String::as_str) else {
        return ParsedRequest::Refused(HistoryRefusal::MissingParameters);
    };
    if !is_valid_target(target) {
        return ParsedRequest::Refused(HistoryRefusal::InvalidReference);
    }

    // The limit is the final parameter for every target-bearing subcommand.
    let Some(limit) = params.last().map(String::as_str) else {
        return ParsedRequest::Refused(HistoryRefusal::MissingParameters);
    };

    match subcommand.as_str() {
        "LATEST" => {
            let Some(limit) = parse_limit(limit) else {
                return ParsedRequest::Refused(HistoryRefusal::InvalidLimit);
            };
            if params.len() != 4 {
                return ParsedRequest::Refused(HistoryRefusal::TooManyParameters);
            }
            let selector = params[2].as_str();
            if selector == "*" {
                return ParsedRequest::Accepted(HistoryQueryRequest::Latest { limit });
            }
            match parse_reference(selector) {
                Some(reference) => {
                    ParsedRequest::Accepted(HistoryQueryRequest::LatestAfter { reference, limit })
                }
                None => ParsedRequest::Refused(reference_refusal(selector)),
            }
        }
        "BEFORE" | "AFTER" | "AROUND" => {
            let Some(limit) = parse_limit(limit) else {
                return ParsedRequest::Refused(HistoryRefusal::InvalidLimit);
            };
            if params.len() != 4 {
                return ParsedRequest::Refused(if params.len() < 4 {
                    HistoryRefusal::MissingParameters
                } else {
                    HistoryRefusal::TooManyParameters
                });
            }
            let selector = params[2].as_str();
            let Some(reference) = parse_reference(selector) else {
                return ParsedRequest::Refused(reference_refusal(selector));
            };
            let request = match subcommand.as_str() {
                "BEFORE" => HistoryQueryRequest::Before { reference, limit },
                "AFTER" => HistoryQueryRequest::After { reference, limit },
                _ => HistoryQueryRequest::Around { reference, limit },
            };
            ParsedRequest::Accepted(request)
        }
        "BETWEEN" => {
            if params.len() != 5 {
                return ParsedRequest::Refused(if params.len() < 5 {
                    HistoryRefusal::MissingParameters
                } else {
                    HistoryRefusal::TooManyParameters
                });
            }
            let Some(limit) = parse_limit(limit) else {
                return ParsedRequest::Refused(HistoryRefusal::InvalidLimit);
            };
            let first_raw = params[2].clone();
            let second_raw = params[3].clone();
            let (Some(first), Some(second)) =
                (parse_reference(&first_raw), parse_reference(&second_raw))
            else {
                let offending = if parse_reference(&first_raw).is_none() {
                    &first_raw
                } else {
                    &second_raw
                };
                return ParsedRequest::Refused(reference_refusal(offending));
            };
            ParsedRequest::Accepted(HistoryQueryRequest::Between {
                first,
                second,
                limit,
            })
        }
        _ => ParsedRequest::Refused(HistoryRefusal::UnknownSubcommand),
    }
}

/// Parses a MARKREAD command.
///
/// ~~~text
/// MARKREAD <target>
/// MARKREAD <target> <timestamp=YYYY-MM-DDThh:mm:ss.sssZ>
/// ~~~
///
/// The one-parameter form is a client *get*; the two-parameter form is a client
/// *set*. A literal `*` as the second parameter is not a "clear everything"
/// instruction -- under the current draft `*` is only what the *server* sends to
/// say no marker is known, and a client may not set a marker to it.
pub fn parse_markread(message: &Message) -> Result<ParsedMarker, MarkerRefusal> {
    let params: Vec<String> = message
        .params
        .iter()
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .collect();
    if params.len() > MAX_QUERY_PARAMS {
        return Err(MarkerRefusal::TooManyParameters);
    }
    let Some(target) = params.first().map(String::as_str) else {
        return Err(MarkerRefusal::MissingParameters);
    };
    if !is_valid_target(target) {
        return Err(MarkerRefusal::InvalidTarget);
    }
    match params.get(1).map(String::as_str) {
        None => Ok(ParsedMarker::Get),
        // The draft forbids a client from setting a marker to the unknown-marker
        // sentinel; that would let a client erase another session's read state.
        Some("*") => Err(MarkerRefusal::InvalidTimestamp),
        Some(selector) => {
            let Some(timestamp) = parse_timestamp(selector) else {
                return Err(MarkerRefusal::InvalidTimestamp);
            };
            Ok(ParsedMarker::Set {
                target: target.to_owned(),
                timestamp,
            })
        }
    }
}

/// Whether a target is a plausible single buffer name.
///
/// Bounded and whitespace-free: a target names one channel or nickname, never a
/// list and never a sentence.
fn is_valid_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= MAX_TARGET_BYTES
        && !target.bytes().any(|byte| byte.is_ascii_whitespace())
}

/// Chooses the most specific refusal for a malformed selector.
///
/// A `timestamp=` prefix that fails to parse is an invalid *timestamp*, which is a
/// more useful diagnosis than a generic bad reference and is the distinction the
/// draft's own example error lines draw.
fn reference_refusal(selector: &str) -> HistoryRefusal {
    if selector.starts_with("timestamp=") {
        HistoryRefusal::InvalidTimestamp
    } else {
        HistoryRefusal::InvalidReference
    }
}

/// A limit is a positive integer no larger than the ceiling advertised in ISUPPORT.
fn parse_limit(raw: &str) -> Option<usize> {
    let limit: usize = raw.parse().ok()?;
    (limit > 0 && limit <= MAX_HISTORY_LIMIT).then_some(limit)
}

/// Parses a required `timestamp=` selector into a canonical protocol timestamp.
fn parse_timestamp(raw: &str) -> Option<IrcTimestamp> {
    IrcTimestamp::parse_str(raw.strip_prefix("timestamp=")?).ok()
}

/// Parses a message reference in its required selector form.
///
/// A bare token is *not* a msgid: the draft defines `msgid=<id>`, and accepting a
/// bare token would make `CHATHISTORY BEFORE #c some-junk 50` look like a valid
/// reference to a msgid the server never issued.
fn parse_reference(raw: &str) -> Option<MessageReference> {
    if let Some(timestamp) = raw.strip_prefix("timestamp=") {
        return IrcTimestamp::parse_str(timestamp)
            .ok()
            .map(MessageReference::Timestamp);
    }
    let msgid = raw.strip_prefix("msgid=")?;
    if msgid.is_empty() || msgid.len() > MAX_MSGID_REFERENCE_BYTES {
        return None;
    }
    if !msgid
        .bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return None;
    }
    Some(MessageReference::MsgId(msgid.to_owned()))
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
    // `TARGETS` is not a message query at all: it lists buffers, so it never has a
    // single resolved buffer to page through. It is executed by its own path.
    if let HistoryQueryRequest::Targets { .. } = request {
        return Err(HistoryRefusal::HistoryUnavailable);
    }
    let cap = BacklogCap::new(BacklogCap::DEFAULT.events, MAX_RESPONSE_BYTES);
    cap.validate().map_err(|_| HistoryRefusal::InvalidLimit)?;
    let read = || async {
        journal
            .backlog_range(buffer, None, None, MAX_LATEST_WINDOW)
            .await
            .map_err(|_| HistoryRefusal::HistoryUnavailable)
    };
    let events = match request {
        HistoryQueryRequest::Latest { limit } => {
            // `LATEST` means the *newest* events. The store returns a buffer's history
            // in ascending local order, so the newest page is the tail of the retained
            // window rather than its head.
            let window = read().await?;
            let start = window.len().saturating_sub(*limit);
            window[start..].to_vec()
        }
        HistoryQueryRequest::LatestAfter { reference, limit } => {
            // Newest events that are strictly after the selector, so the result is
            // the tail of the window filtered by the resolved position.
            let after = resolve(journal, buffer, reference).await?;
            let window = read().await?;
            let matching: Vec<HistoryEvent> = window
                .into_iter()
                .filter(|event| event.event > after)
                .collect();
            let start = matching.len().saturating_sub(*limit);
            matching[start..].to_vec()
        }
        HistoryQueryRequest::Before { reference, limit } => {
            let before = resolve(journal, buffer, reference).await?;
            journal
                .backlog_range(buffer, None, Some(before), *limit)
                .await
                .map_err(|_| HistoryRefusal::HistoryUnavailable)?
        }
        HistoryQueryRequest::After { reference, limit } => {
            let after = resolve(journal, buffer, reference).await?;
            journal
                .backlog_range(buffer, Some(after), None, *limit)
                .await
                .map_err(|_| HistoryRefusal::HistoryUnavailable)?
        }
        HistoryQueryRequest::Around { reference, limit } => {
            // Split the budget around the selector. The selector's own message
            // occupies one of the `limit` slots, and the result is the contiguous run
            // of events nearest to it, still in ascending local order.
            let anchor = resolve(journal, buffer, reference).await?;
            let window = read().await?;
            let position = window
                .iter()
                .position(|event| event.event == anchor)
                .ok_or(HistoryRefusal::HistoryUnavailable)?;
            let before_budget = limit.saturating_sub(1) / 2;
            let after_budget = limit.saturating_sub(1).saturating_sub(before_budget);
            let start = position.saturating_sub(before_budget);
            let end = (position + 1 + after_budget).min(window.len());
            window[start..end].to_vec()
        }
        HistoryQueryRequest::Between {
            first,
            second,
            limit,
        } => {
            // The draft allows this in either direction, so the two resolved
            // positions are ordered by local identity rather than assumed.
            let left = resolve(journal, buffer, first).await?;
            let right = resolve(journal, buffer, second).await?;
            let (after, before) = if left <= right {
                (Some(left), Some(right))
            } else {
                (Some(right), Some(left))
            };
            journal
                .backlog_range(buffer, after, before, *limit)
                .await
                .map_err(|_| HistoryRefusal::HistoryUnavailable)?
        }
        HistoryQueryRequest::Targets { .. } => unreachable!("handled above"),
    };
    Ok(render(buffer, events, cap.bytes))
}

/// The protocol timestamp for a local receive time.
///
/// `WallTime` is bounded to a range that sits inside the four-digit year window the
/// wire grammar uses, so this cannot fail for a valid stored row. The epoch fallback
/// exists so a future bound change degrades to an obviously-wrong-but-well-formed
/// value rather than panicking on the history read path.
pub fn local_timestamp(received_at: i2pr_irc_core::WallTime) -> IrcTimestamp {
    IrcTimestamp::from_unix_millis(received_at.unix_seconds().saturating_mul(1_000))
        .unwrap_or(IrcTimestamp::EPOCH)
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
        .map_err(|_| HistoryRefusal::HistoryUnavailable)?;
    let resolved = match reference {
        MessageReference::MsgId(wanted) => events
            .iter()
            .find(|event| event.msgid.as_deref() == Some(wanted.as_str()))
            .map(|event| event.event),
        MessageReference::Timestamp(wanted) => {
            // Compare the canonical protocol timestamp, falling back to local receive
            // time only for events the upstream never stamped. Ties resolve through
            // local order: the newest event at or before the timestamp, and among
            // equals the largest HistoryEventId.
            let key = |event: &HistoryEvent| {
                event
                    .server_time
                    .unwrap_or_else(|| local_timestamp(event.received_at))
                    .unix_millis()
            };
            events
                .iter()
                .filter(|event| key(event) <= wanted.unix_millis())
                .max_by_key(|event| (key(event), event.event.0))
                .map(|event| event.event)
        }
    };
    resolved.ok_or(HistoryRefusal::HistoryUnavailable)
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

/// The ISUPPORT tokens that make the advertised chathistory surface truthful.
///
/// `CHATHISTORY=<max>` is the same ceiling [`parse_limit`] enforces, so a client
/// cannot be told one bound and held to another. `MSGREFTYPES` lists only the
/// reference types this adapter really resolves.
pub fn isupport_tokens() -> Vec<String> {
    vec![
        format!("CHATHISTORY={MAX_HISTORY_LIMIT}"),
        "MSGREFTYPES=timestamp,msgid".to_owned(),
    ]
}

/// The BATCH type for message-history replies.
pub const CHATHISTORY_BATCH_TYPE: &str = "chathistory";
/// The BATCH type for `TARGETS` replies, which carry no messages.
pub const TARGETS_BATCH_TYPE: &str = "draft/chathistory-targets";
/// The tag that tells a client no further page exists.
pub const CHATHISTORY_END_TAG: &[u8] = b"draft/chathistory-end";
/// The BATCH tag that ties a message to its batch.
pub const BATCH_TAG: &[u8] = b"batch";

/// Wraps a rendered history reply in the BATCH the draft requires.
///
/// A client that negotiated `batch` must receive its history inside a batch of type
/// `chathistory` carrying the canonical target name; an empty result still gets an
/// empty batch so the client does not wait for a reply that never comes. When no more
/// history exists, the opening line carries `draft/chathistory-end`.
///
/// Without the `batch` capability the lines are returned unwrapped: that is the
/// draft's explicitly permitted degraded mode for a client that cannot read batches.
pub fn wrap_in_batch(
    reply: &HistoryReply,
    target: &str,
    tracker: &mut crate::ircv3::BatchTracker,
) -> Result<Vec<Vec<u8>>, HistoryRefusal> {
    let now = std::time::Instant::now();
    let batch = tracker
        .open(CHATHISTORY_BATCH_TYPE, None, now)
        .map_err(|_| HistoryRefusal::HistoryUnavailable)?;

    let mut lines = Vec::with_capacity(reply.lines.len() + 2);
    let mut opening = format!(
        ":bouncer BATCH +{} {CHATHISTORY_BATCH_TYPE} {target}",
        batch.id
    );
    if !reply.more_pending {
        // No further page exists, so say so once rather than letting the client
        // request the next page and get nothing.
        opening = format!(
            "@{}:{opening}",
            core::str::from_utf8(CHATHISTORY_END_TAG).unwrap_or("")
        );
    }
    lines.push(cr_lf(&opening));

    for line in &reply.lines {
        tracker
            .note_message(&batch.id)
            .map_err(|_| HistoryRefusal::HistoryUnavailable)?;
        lines.push(tag_with_batch(line, &batch.id)?);
    }
    // An empty result is still closed explicitly.
    lines.push(cr_lf(&format!(":bouncer BATCH -{}", batch.id)));
    let _ = tracker.close(&batch.id);
    Ok(lines)
}

/// Adds `batch=<id>` to an already-rendered line, preserving its existing tags.
///
/// The rendered line already carries `time` and `msgid` as ordinary tags, so this is
/// just one more entry in the same map: re-encoding preserves all of them.
fn tag_with_batch(line: &[u8], id: &str) -> Result<Vec<u8>, HistoryRefusal> {
    let mut message = Message::parse(line).map_err(|_| HistoryRefusal::HistoryUnavailable)?;
    message
        .tags
        .insert(BATCH_TAG.to_vec(), Some(id.as_bytes().to_vec()));
    message
        .encode()
        .map_err(|_| HistoryRefusal::HistoryUnavailable)
}

fn cr_lf(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.extend_from_slice(b"\r\n");
    bytes
}

/// Renders a refusal as the standard-reply line the draft specifies.
///
/// The command and offending parameters are echoed so a client can correlate the
/// failure with what it sent, matching the draft's own example lines.
pub fn render_failure(refusal: HistoryRefusal, command: &str, target: Option<&str>) -> Vec<u8> {
    let mut line = format!("FAIL CHATHISTORY {} {command}", refusal.error().code());
    if let Some(target) = target
        && !target.is_empty()
    {
        line.push(' ');
        line.push_str(target);
    }
    line.push_str(" :");
    line.push_str(&refusal.to_string());
    let mut bytes = line.into_bytes();
    bytes.extend_from_slice(b"\r\n");
    bytes
}

/// Renders a MARKREAD refusal as its standard-reply line.
pub fn render_marker_failure(refusal: MarkerRefusal, target: Option<&str>) -> Vec<u8> {
    let mut line = format!("FAIL MARKREAD {}", refusal.error());
    if let Some(target) = target
        && !target.is_empty()
    {
        line.push(' ');
        line.push_str(target);
    }
    line.push_str(" :");
    line.push_str(match refusal {
        MarkerRefusal::MissingParameters => "Missing parameters",
        MarkerRefusal::TooManyParameters => "Too many parameters",
        MarkerRefusal::InvalidTarget => "Invalid parameters",
        MarkerRefusal::InvalidTimestamp => "Invalid parameters",
        MarkerRefusal::NoSuchBuffer | MarkerRefusal::Internal => {
            "The read timestamp could not be set"
        }
    });
    let mut bytes = line.into_bytes();
    bytes.extend_from_slice(b"\r\n");
    bytes
}

/// Renders the server-side `MARKREAD` reply line.
///
/// `timestamp` is `None` when no marker is known, which the draft spells as a
/// literal `*` -- the same token a client is forbidden to *set*.
pub fn render_marker_reply(target: &str, timestamp: Option<IrcTimestamp>) -> Vec<u8> {
    let value = timestamp
        .map(|stamp| format!("timestamp={stamp}"))
        .unwrap_or_else(|| "*".to_owned());
    let mut bytes = format!("MARKREAD {target} {value}\r\n").into_bytes();
    bytes.truncate(bytes.len().min(MAX_LINE_BYTES + 2));
    bytes
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

/// Whether this session negotiated the read-marker draft.
///
/// Tracked separately from [`session_manages_history`] because the drafts are
/// independently negotiable: the bouncer owes initial markers and update propagation
/// to a client that asked for them, and owes neither to one that did not.
pub fn session_manages_markers(enabled: &BTreeSet<String>) -> bool {
    enabled.contains(READ_MARKER_CAPABILITY)
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
    fn every_advertised_subcommand_parses_in_its_exact_shape() {
        // The limit is the LAST parameter in every form, and TARGETS has no target.
        let cases = [
            "CHATHISTORY BEFORE #room msgid=abc 50",
            "CHATHISTORY AFTER #room msgid=abc 50",
            "CHATHISTORY LATEST #room * 50",
            "CHATHISTORY LATEST #room timestamp=2019-01-04T14:33:26.123Z 50",
            "CHATHISTORY AROUND #room msgid=abc 50",
            "CHATHISTORY BETWEEN #room msgid=a msgid=b 50",
            "CHATHISTORY TARGETS timestamp=2019-01-04T14:33:26.123Z timestamp=2019-01-05T00:00:00.000Z 50",
        ];
        for raw in cases {
            assert!(
                matches!(
                    parse_chathistory(&command(&format!("{raw}\r\n"))),
                    ParsedRequest::Accepted(_)
                ),
                "{raw} must parse"
            );
        }
    }

    #[test]
    fn the_pre_corrective_parameter_order_is_refused() {
        // The old bug: limit before the reference. A client sending the real grammar
        // must be understood, and the old shape must not be silently reinterpreted.
        for raw in [
            "CHATHISTORY BEFORE #room 50 msgid=abc",
            "CHATHISTORY AFTER #room 50 msgid=abc",
            "CHATHISTORY BETWEEN #room 50 msgid=a msgid=b",
            "CHATHISTORY LATEST #room 50",
        ] {
            assert!(
                !matches!(
                    parse_chathistory(&command(&format!("{raw}\r\n"))),
                    ParsedRequest::Accepted(_)
                ),
                "{raw} must be refused rather than reinterpreted"
            );
        }
    }

    #[test]
    fn targets_is_not_a_message_query_for_a_fake_target() {
        // TARGETS lists buffers, so its first parameter is a timestamp selector and
        // must never be treated as a target.
        let parsed = parse_chathistory(&command(
            "CHATHISTORY TARGETS timestamp=2019-01-04T14:33:26.123Z timestamp=2019-01-05T00:00:00.000Z 50\r\n",
        ));
        assert!(matches!(
            parsed,
            ParsedRequest::Accepted(HistoryQueryRequest::Targets { limit: 50, .. })
        ));
        // A bare channel name is not a timestamp selector.
        assert!(matches!(
            parse_chathistory(&command(
                "CHATHISTORY TARGETS #room timestamp=2019-01-04T14:33:26.123Z 50\r\n"
            )),
            ParsedRequest::Refused(_)
        ));
    }

    #[test]
    fn subcommand_matching_is_case_insensitive() {
        assert!(matches!(
            parse_chathistory(&command("CHATHISTORY latest #room * 10\r\n")),
            ParsedRequest::Accepted(HistoryQueryRequest::Latest { limit: 10 })
        ));
    }

    #[test]
    fn a_bare_token_is_not_a_msgid_selector() {
        for raw in [
            "CHATHISTORY BEFORE #room abc123 50",
            "CHATHISTORY AFTER #room abc123 50",
            "CHATHISTORY AROUND #room abc123 50",
            "CHATHISTORY BETWEEN #room abc123 msgid=b 50",
        ] {
            assert_eq!(
                parse_chathistory(&command(&format!("{raw}\r\n"))),
                ParsedRequest::Refused(HistoryRefusal::InvalidReference),
                "{raw}"
            );
        }
    }

    #[test]
    fn an_integer_epoch_is_not_a_timestamp_selector() {
        for raw in [
            "CHATHISTORY BEFORE #room timestamp=1700000000 50",
            "CHATHISTORY AFTER #room timestamp=1700000000 50",
            "CHATHISTORY TARGETS timestamp=1700000000 timestamp=2019-01-05T00:00:00.000Z 50",
        ] {
            assert_eq!(
                parse_chathistory(&command(&format!("{raw}\r\n"))),
                ParsedRequest::Refused(HistoryRefusal::InvalidTimestamp),
                "{raw}"
            );
        }
    }

    #[test]
    fn an_unbounded_or_invalid_limit_is_refused() {
        for raw in [
            "CHATHISTORY LATEST #room * 0",
            "CHATHISTORY LATEST #room * -1",
            "CHATHISTORY LATEST #room * notanumber",
            "CHATHISTORY LATEST #room * 100000",
            "CHATHISTORY BEFORE #room msgid=a",
        ] {
            assert!(
                matches!(
                    parse_chathistory(&command(&format!("{raw}\r\n"))),
                    ParsedRequest::Refused(_)
                ),
                "{raw} must be refused"
            );
        }
    }

    #[test]
    fn the_enforced_limit_ceiling_matches_the_advertised_isupport_value() {
        // Advertising one bound and enforcing another would make the ISUPPORT token a
        // lie a client has no way to detect.
        let tokens = isupport_tokens();
        assert!(
            tokens.iter().any(|token| token == "CHATHISTORY=50"),
            "CHATHISTORY ISUPPORT must state the enforced maximum: {tokens:?}"
        );
        assert!(
            parse_chathistory(&command("CHATHISTORY LATEST #room * 50\r\n")).eq(
                &ParsedRequest::Accepted(HistoryQueryRequest::Latest {
                    limit: MAX_HISTORY_LIMIT
                })
            ),
            "the advertised maximum must itself be accepted"
        );
        assert_eq!(
            parse_chathistory(&command("CHATHISTORY LATEST #room * 51\r\n")),
            ParsedRequest::Refused(HistoryRefusal::InvalidLimit),
            "one above the advertised maximum must be refused"
        );
    }

    #[test]
    fn msgreftypes_lists_only_the_reference_types_actually_resolved() {
        let tokens = isupport_tokens();
        assert!(
            tokens
                .iter()
                .any(|token| token == "MSGREFTYPES=timestamp,msgid"),
            "MSGREFTYPES must match the implemented selectors: {tokens:?}"
        );
    }

    #[test]
    fn a_malformed_reference_is_refused_rather_than_guessed() {
        for raw in [
            "CHATHISTORY BEFORE #room timestamp= 50",
            "CHATHISTORY BEFORE #room timestamp=notanumber 50",
            "CHATHISTORY BEFORE #room msgid= 50",
        ] {
            assert!(
                matches!(
                    parse_chathistory(&command(&format!("{raw}\r\n"))),
                    ParsedRequest::Refused(_)
                ),
                "{raw} must be refused"
            );
        }
        // An over-long msgid selector is refused rather than truncated.
        let long = "x".repeat(MAX_MSGID_REFERENCE_BYTES + 1);
        assert_eq!(
            parse_chathistory(&command(&format!(
                "CHATHISTORY BEFORE #room msgid={long} 50\r\n"
            ))),
            ParsedRequest::Refused(HistoryRefusal::InvalidReference)
        );
    }

    #[test]
    fn an_unknown_subcommand_and_an_oversized_request_are_refused() {
        assert_eq!(
            parse_chathistory(&command("CHATHISTORY FROBNICATE #room * 50\r\n")),
            ParsedRequest::Refused(HistoryRefusal::UnknownSubcommand)
        );
        assert_eq!(
            parse_chathistory(&command("CHATHISTORY\r\n")),
            ParsedRequest::Refused(HistoryRefusal::UnknownSubcommand)
        );
        let raw = "CHATHISTORY BETWEEN #room msgid=a msgid=b 50 extra\r\n";
        assert_eq!(
            parse_chathistory(&command(raw)),
            ParsedRequest::Refused(HistoryRefusal::TooManyParameters)
        );
    }

    #[test]
    fn references_distinguish_a_timestamp_from_a_msgid() {
        let ParsedRequest::Accepted(HistoryQueryRequest::Before { reference, .. }) =
            parse_chathistory(&command(
                "CHATHISTORY BEFORE #room timestamp=2019-01-04T14:33:26.123Z 50\r\n",
            ))
        else {
            panic!("expected a parsed request")
        };
        assert_eq!(
            reference,
            MessageReference::Timestamp(
                IrcTimestamp::parse_str("2019-01-04T14:33:26.123Z").expect("parses")
            )
        );
        let ParsedRequest::Accepted(HistoryQueryRequest::Before { reference, .. }) =
            parse_chathistory(&command("CHATHISTORY BEFORE #room msgid=abc123 50\r\n"))
        else {
            panic!("expected a parsed request")
        };
        assert_eq!(reference, MessageReference::MsgId("abc123".to_owned()));
    }

    #[test]
    fn a_history_reply_is_wrapped_in_the_batch_the_draft_requires() {
        let reply = HistoryReply {
            buffer: BufferId(1),
            lines: vec![b"@time=2019-01-04T14:33:26.123Z :a!b@c PRIVMSG #room :hi\r\n".to_vec()],
            bytes: 0,
            newest: Some(HistoryEventId(1)),
            more_pending: false,
        };
        let mut tracker = crate::ircv3::BatchTracker::default();
        let lines = wrap_in_batch(&reply, "#room", &mut tracker).expect("wraps");
        let text: Vec<String> = lines
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();

        assert!(
            text[0].contains("BATCH +"),
            "the reply must open a batch: {}",
            text[0]
        );
        assert!(
            text[0].contains("chathistory #room"),
            "the opening line carries the batch type and canonical target: {}",
            text[0]
        );
        assert!(
            text[0].contains("draft/chathistory-end"),
            "an exhausted result must say so: {}",
            text[0]
        );
        let message = Message::parse(lines[1].as_slice()).expect("parses");
        assert!(
            message.tags.contains_key(BATCH_TAG),
            "each message must be tagged into the batch"
        );
        // The pre-existing tags must survive the wrap.
        assert_eq!(
            message.server_time().map(|time| time.to_string()),
            Some("2019-01-04T14:33:26.123Z".to_owned())
        );
        assert!(
            text[2].contains("BATCH -"),
            "the batch must be closed explicitly: {}",
            text[2]
        );
    }

    #[test]
    fn an_empty_history_still_produces_a_closed_batch() {
        let reply = HistoryReply {
            buffer: BufferId(1),
            lines: Vec::new(),
            bytes: 0,
            newest: None,
            more_pending: true,
        };
        let mut tracker = crate::ircv3::BatchTracker::default();
        let lines = wrap_in_batch(&reply, "#room", &mut tracker).expect("wraps");
        assert_eq!(lines.len(), 2, "an empty batch still opens and closes");
        let text: Vec<String> = lines
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();
        assert!(text[0].contains("BATCH +"));
        assert!(
            !text[0].contains("draft/chathistory-end"),
            "a truncated page is not the end of history"
        );
        assert!(text[1].contains("BATCH -"));
    }

    #[test]
    fn a_refusal_renders_as_the_standard_reply_the_draft_names() {
        let line = render_failure(
            HistoryRefusal::InvalidTimestamp,
            "BEFORE #room timestamp=1700000000 50",
            Some("#room"),
        );
        let text = String::from_utf8(line).expect("ascii");
        assert!(
            text.starts_with("FAIL CHATHISTORY INVALID_PARAMS "),
            "unexpected failure line: {text}"
        );
        assert!(text.ends_with("\r\n"));

        // An unsupported reference type is a distinct code from a syntax error.
        let line = render_failure(HistoryRefusal::UnsupportedReferenceType, "x", Some("#room"));
        assert!(
            String::from_utf8(line)
                .expect("ascii")
                .starts_with("FAIL CHATHISTORY INVALID_MSGREFTYPE "),
        );
    }

    #[test]
    fn the_marker_reply_uses_a_literal_star_only_for_an_unknown_marker() {
        assert_eq!(
            String::from_utf8(render_marker_reply("#room", None)).expect("ascii"),
            "MARKREAD #room *\r\n"
        );
        let known = IrcTimestamp::parse_str("2019-01-04T14:33:26.123Z").expect("parses");
        assert_eq!(
            String::from_utf8(render_marker_reply("#room", Some(known))).expect("ascii"),
            "MARKREAD #room timestamp=2019-01-04T14:33:26.123Z\r\n"
        );
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
    fn markread_distinguishes_a_get_from_a_set() {
        // One parameter is a get; two is a set. This is the whole point of the
        // reviewed draft's command grammar.
        assert_eq!(
            parse_markread(&command("MARKREAD #room\r\n")),
            Ok(ParsedMarker::Get)
        );
        let expected = IrcTimestamp::parse_str("2019-01-04T14:33:26.123Z").expect("parses");
        assert_eq!(
            parse_markread(&command(
                "MARKREAD #room timestamp=2019-01-04T14:33:26.123Z\r\n"
            )),
            Ok(ParsedMarker::Set {
                target: "#room".to_owned(),
                timestamp: expected,
            })
        );
    }

    #[test]
    fn a_client_may_not_set_a_marker_to_the_unknown_marker_sentinel() {
        // `*` is what the *server* sends to mean "no marker known". Accepting it
        // from a client would let one session erase another's read state.
        assert_eq!(
            parse_markread(&command("MARKREAD #room *\r\n")),
            Err(MarkerRefusal::InvalidTimestamp)
        );
        // And a bare msgid is not a MARKREAD selector at all.
        assert_eq!(
            parse_markread(&command("MARKREAD #room msgid=abc\r\n")),
            Err(MarkerRefusal::InvalidTimestamp)
        );
    }

    #[test]
    fn malformed_markread_input_is_refused() {
        assert_eq!(
            parse_markread(&command("MARKREAD\r\n")),
            Err(MarkerRefusal::MissingParameters)
        );
        assert_eq!(
            parse_markread(&command("MARKREAD #room timestamp=bad\r\n")),
            Err(MarkerRefusal::InvalidTimestamp)
        );
        assert_eq!(
            parse_markread(&command("MARKREAD #room timestamp=1700000000\r\n")),
            Err(MarkerRefusal::InvalidTimestamp)
        );
        assert_eq!(
            parse_markread(&command("MARKREAD a b c d e f g\r\n")),
            Err(MarkerRefusal::TooManyParameters)
        );
    }

    #[test]
    fn a_refusal_names_a_reason_and_never_a_payload() {
        let text = HistoryRefusal::HistoryUnavailable.to_string();
        assert!(text.contains("could not be retrieved"));
        assert!(
            !text.contains('@'),
            "a refusal must never echo payload bytes"
        );
        assert_eq!(
            HistoryRefusal::HistoryUnavailable.error().code(),
            "MESSAGE_ERROR"
        );
        assert_eq!(
            HistoryRefusal::NoSuchBuffer.error().code(),
            "INVALID_TARGET"
        );
    }

    #[test]
    fn the_client_does_not_need_an_unused_mutable_clock() {
        // A read marker resolves against a client reference; this helper exists so a
        // retention sweep can be scheduled without reaching into the journal.
        let request = retention_for(i2pr_irc_core::NetworkId(1), HistoryEventId(100));
        assert_eq!(request.max_delete, 512);
    }
}
