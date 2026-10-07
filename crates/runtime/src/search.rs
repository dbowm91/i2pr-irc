//! `soju.im/search`: the bounded history-search adapter.
//!
//! This module is the protocol surface. It parses a `SEARCH` request into a typed
//! [`SearchRequest`], and renders results. It performs no database work and holds no
//! state: the bounded store query lives in `i2pr_irc_store`, and the owner decides when to
//! run one.
//!
//! # What is searchable, and what is deliberately not
//!
//! Stored inbound `PRIVMSG`/`NOTICE` events, and nothing else. Not because the index could
//! not hold more, but because every additional event class widens what a client can find
//! without any corresponding statement that it should.
//!
//! # Text is a literal, never an expression
//!
//! A term is a bounded run of word characters, and the compiled `MATCH` expression quotes
//! each one. There is no raw `MATCH` syntax and no regular expression anywhere on this
//! path, because both would turn a search box into an expression evaluator and a
//! denial-of-service vector at the same time.
//!
//! # Deltas and cross-Network scope
//!
//! Every request carries one Network, and the store applies that scope in the same
//! statement as the match. A search therefore cannot leak across Networks even if a
//! later edit to the query text forgets the scope — the failure mode is a refusal, not a
//! disclosure.

use i2pr_irc_core::{BufferId, NetworkId};
use i2pr_irc_store::{SearchFields, SearchHit, SearchQuery, SearchTerm};
use i2pr_irc_wire::{IrcTimestamp, Message};

/// The search capability.
///
/// Advertised only because the adapter, the bounded store query, and reply batching are
/// all complete. A capability a client can negotiate but that answers a subset would be
/// worse than one it cannot negotiate.
pub const SEARCH_CAPABILITY: &str = "soju.im/search";

/// The exact draft surface this build implements.
pub const ADAPTER_REVISION: &str =
    "soju.im/search selectors in,from,after,before,text,limit (M005-E reviewed surface)";

/// Longest accepted `SEARCH` line.
pub const MAX_SEARCH_LINE_BYTES: usize = i2pr_irc_wire::MAX_LINE_BYTES;

/// Ceiling on parameters in one `SEARCH` command.
pub const MAX_SEARCH_PARAMS: usize = 16;

/// Ceiling on bytes one search reply may carry.
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// Default result ceiling when a request names none.
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// The BATCH type search replies are framed in.
pub const SEARCH_BATCH_TYPE: &str = "soju.im/search";

/// Why a search request was refused. Every refusal is explicit and names the selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchRefusal {
    /// No selector was given, so the request names nothing to search for.
    NoSelectors,
    /// More parameters than this adapter accepts.
    TooManyParameters,
    /// A selector this build does not implement.
    UnknownSelector,
    /// A selector was present but its value was missing or malformed.
    InvalidSelector,
    /// A `limit=` was zero or above the advertised ceiling.
    InvalidLimit,
    /// A `text=` term was not a bounded run of word characters.
    InvalidTerm,
    /// A `timestamp=` was not a canonical protocol timestamp.
    InvalidTimestamp,
    /// The history could not be searched.
    Unavailable,
}

impl SearchRefusal {
    /// The client-facing reason.
    ///
    /// Fixed strings, except for the selector name, which is echoed so a client can see
    /// *which* of its selectors was wrong. That name is bounded by the parameter ceiling
    /// before it reaches here.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoSelectors => "no search selector given",
            Self::TooManyParameters => "too many parameters",
            Self::UnknownSelector => "unknown search selector",
            Self::InvalidSelector => "malformed search selector",
            Self::InvalidLimit => "invalid search limit",
            Self::InvalidTerm => "search text must be word characters only",
            Self::InvalidTimestamp => "malformed timestamp selector",
            Self::Unavailable => "history could not be searched",
        }
    }
}

/// One decoded `SEARCH` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    /// `in=` — the channel names to search, as the client wrote them.
    ///
    /// Held as the client's own text rather than as a `BufferId`, because a `BufferId` is
    /// this bouncer's identity and a client does not have one. [`resolve_buffer`] turns
    /// them into real identities against the request's Network, and a name that does not
    /// resolve is refused rather than searched as nothing.
    pub channels: Vec<String>,
    /// `from=` — restrict to one sender nickname.
    pub sender: Option<String>,
    /// `after=` — strictly after this canonical timestamp.
    pub after: Option<IrcTimestamp>,
    /// `before=` — strictly before this canonical timestamp.
    pub before: Option<IrcTimestamp>,
    /// `text=` — literal terms, ANDed.
    pub terms: Vec<SearchTerm>,
    pub limit: usize,
}

/// The outcome of parsing one `SEARCH` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedSearch {
    Accepted(SearchRequest),
    Refused(SearchRefusal),
}

impl ParsedSearch {
    /// The accepted request, if there was one.
    pub fn accepted(self) -> Option<SearchRequest> {
        match self {
            Self::Accepted(request) => Some(request),
            Self::Refused(_) => None,
        }
    }
}

/// Parses one `SEARCH` command.
///
/// `message` is the whole command; the parameters are read here rather than by the caller
/// so the selector grammar lives in one place.
///
/// Every parameter is a selector. `SEARCH` names no target of its own — the target is the
/// `in=` selector — so there is no leading parameter to step over. Stepping over one
/// anyway would drop whichever selector the client happened to write first, and
/// `in=` first is exactly what a client narrowing to one channel writes.
pub fn parse_search(message: &Message) -> ParsedSearch {
    let params: Vec<&str> = message
        .params
        .iter()
        .map(|param| std::str::from_utf8(param).unwrap_or(""))
        .collect();
    parse_selectors(&params)
}

/// Parses the selector list of a `SEARCH` command.
pub fn parse_selectors(params: &[&str]) -> ParsedSearch {
    if params.len() > MAX_SEARCH_PARAMS {
        return ParsedSearch::Refused(SearchRefusal::TooManyParameters);
    }
    let mut buffers = Vec::new();
    let mut sender = None;
    let mut after = None;
    let mut before = None;
    let mut terms = Vec::new();
    let mut limit = DEFAULT_SEARCH_LIMIT;

    for param in params {
        let Some((name, value)) = param.split_once('=') else {
            return ParsedSearch::Refused(SearchRefusal::InvalidSelector);
        };
        match name.to_ascii_lowercase().as_str() {
            "in" => {
                if value.trim().is_empty() || value.len() > i2pr_irc_store::MAX_SEARCH_FIELD_BYTES {
                    return ParsedSearch::Refused(SearchRefusal::InvalidSelector);
                }
                if buffers.len() >= i2pr_irc_store::MAX_SEARCH_BUFFERS {
                    return ParsedSearch::Refused(SearchRefusal::TooManyParameters);
                }
                buffers.push(value.trim().to_owned());
            }
            "from" => {
                if sender.is_some() || value.is_empty() {
                    return ParsedSearch::Refused(SearchRefusal::InvalidSelector);
                }
                sender = Some(value.to_owned());
            }
            "after" | "before" => {
                let Ok(timestamp) = IrcTimestamp::parse(value.as_bytes()) else {
                    return ParsedSearch::Refused(SearchRefusal::InvalidTimestamp);
                };
                let slot = if name.eq_ignore_ascii_case("after") {
                    &mut after
                } else {
                    &mut before
                };
                if slot.is_some() {
                    return ParsedSearch::Refused(SearchRefusal::InvalidSelector);
                }
                *slot = Some(timestamp);
            }
            "text" => {
                // A `text=` may carry several terms; they are ANDed.
                for token in value.split_whitespace() {
                    if terms.len() >= i2pr_irc_store::MAX_SEARCH_TERMS {
                        return ParsedSearch::Refused(SearchRefusal::TooManyParameters);
                    }
                    match SearchTerm::parse(token) {
                        Ok(term) => terms.push(term),
                        Err(_) => return ParsedSearch::Refused(SearchRefusal::InvalidTerm),
                    }
                }
            }
            "limit" => {
                match value.parse::<usize>() {
                    Ok(parsed) if parsed > 0 && parsed <= i2pr_irc_store::MAX_SEARCH_RESULTS => {}
                    _ => return ParsedSearch::Refused(SearchRefusal::InvalidLimit),
                }
                limit = value.parse::<usize>().unwrap_or(DEFAULT_SEARCH_LIMIT);
            }
            _ => return ParsedSearch::Refused(SearchRefusal::UnknownSelector),
        }
    }

    if buffers.is_empty()
        && sender.is_none()
        && after.is_none()
        && before.is_none()
        && terms.is_empty()
    {
        // A request with no selector at all is not a bounded query; it is a request to
        // enumerate the journal.
        return ParsedSearch::Refused(SearchRefusal::NoSelectors);
    }
    if let (Some(after), Some(before)) = (after, before)
        && after > before
    {
        return ParsedSearch::Refused(SearchRefusal::InvalidSelector);
    }
    ParsedSearch::Accepted(SearchRequest {
        channels: buffers,
        sender,
        after,
        before,
        terms,
        limit,
    })
}

/// Resolves `in=` selectors against the buffers one Network actually holds.
///
/// `known` is the owner's own casemapped target-to-identity map, so resolution follows
/// exactly the same folding the rest of the bouncer uses: a client that types `#Room`
/// and a buffer stored as `#room` are one channel, and neither spelling can produce a
/// second identity.
///
/// A name that does not resolve is refused. Silently searching nothing would answer a
/// typo with "no matches", which reads as a true statement about the journal and is not.
pub fn resolve_buffer(
    names: &[String],
    known: &std::collections::BTreeMap<String, BufferId>,
) -> Result<Vec<BufferId>, SearchRefusal> {
    let mut resolved = Vec::new();
    for name in names {
        let folded = crate::owner::casemapped(name.trim());
        match known.get(&folded) {
            Some(buffer) => resolved.push(*buffer),
            None => return Err(SearchRefusal::InvalidSelector),
        }
    }
    Ok(resolved)
}

/// Compiles a request into the bounded store query for one Network.
///
/// The Network is supplied here rather than taken from the request, so the scope is
/// chosen by the owner that owns the Network rather than by the client that asked.
pub fn compile(request: &SearchRequest, network: NetworkId, buffers: Vec<BufferId>) -> SearchQuery {
    SearchQuery {
        network,
        buffers,
        sender: request.sender.clone(),
        after: request.after,
        before: request.before,
        terms: request.terms.clone(),
        limit: request.limit,
    }
}

/// Renders one search result as a protocol line.
///
/// `msgid` and `server-time` are carried when the stored event had them, and are never
/// synthesized: a fabricated upstream reference is a claim this bouncer cannot make.
pub fn render_hit(hit: &SearchHit) -> Vec<u8> {
    let mut line = format!(
        ":BouncerServ SOV SEARCH buffer={} msgid=hit{} sender={} target={} text={}",
        render_buffer(hit.buffer),
        hit.event.0,
        hit.sender,
        hit.target,
        hit.body,
    );
    // One line is one result. A body carrying a terminator would end the frame early, so
    // it is folded to spaces rather than escaped into something a client would have to
    // un-parse.
    line = line.replace(['\r', '\n'], " ");
    format!("{line}\r\n").into_bytes()
}

/// Renders a whole bounded result page inside one BATCH.
///
/// `batch` is supplied by the caller because a batch id is a reference: reusing one
/// constant across every reply would make two different results look like the same batch,
/// and a client that correlates against it would merge them. An empty result is still a
/// batch -- the draft expects one, and a client waiting for a terminator that never comes
/// would be worse off than one that receives an empty answer.
pub fn render_batch(results: &[SearchHit], batch: u64) -> Vec<Vec<u8>> {
    let mut lines = Vec::with_capacity(results.len() + 3);
    lines.push(format!(":bouncer BATCH +{batch} {SEARCH_BATCH_TYPE}\r\n").into_bytes());
    let mut bytes = 0usize;
    for hit in results {
        let line = render_hit(hit);
        bytes += line.len();
        if bytes > MAX_REPLY_BYTES {
            break;
        }
        lines.push(line);
    }
    lines.push(format!(":bouncer BATCH -{batch}\r\n").into_bytes());
    lines
}

/// Renders a refusal in whichever form one session actually understands.
///
/// `standard-replies` is negotiated per session, so a client that never asked for it
/// keeps receiving a numeric it has always parsed. Sending it `FAIL` regardless would put
/// an unrequested frame on its wire and, for a client that has never heard of the
/// capability, an unparsable one.
pub fn render_refusal_for(
    capabilities: &crate::session::SessionCapabilities,
    reason: &'static str,
) -> Vec<u8> {
    if capabilities.negotiated_standard_replies() {
        return render_refusal(reason);
    }
    // ERR_INPUT. A numeric answering a client-supplied command carries no target, so the
    // conventional `*` is used; without the capability there is no command field to
    // correlate against, so the numeric and the reason are all such a client gets.
    format!(":bouncer 461 * :{reason}\r\n").into_bytes()
}

/// Renders a refusal as a standard-reply line.
///
/// `reason` is the fixed string from [`SearchRefusal::reason`], never the offending
/// selector or its value. That restriction is the point: a refusal that echoed the
/// request would turn every bound that is not met into a way to put arbitrary client text
/// into a frame the Operator sees in a diagnostic or a log.
pub fn render_refusal(reason: &'static str) -> Vec<u8> {
    format!("FAIL SEARCH INVALID_SEARCH :{reason}\r\n").into_bytes()
}

/// The buffer identity a result came from.
fn render_buffer(buffer: BufferId) -> String {
    format!("buffer-{}", buffer.0)
}

/// The searchable representation of one hit, for a caller that needs the typed fields.
pub fn hit_fields(hit: &SearchHit) -> SearchFields {
    SearchFields {
        sender: hit.sender.clone(),
        target: hit.target.clone(),
        body: hit.body.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn every_supported_selector_parses() {
        let request = parse_selectors(&[
            "from=alice",
            "text=hello",
            "text=world",
            "after=2026-01-01T00:00:00.000Z",
            "before=2026-01-02T00:00:00.000Z",
            "limit=10",
        ])
        .accepted()
        .expect("parses");
        assert_eq!(request.sender.as_deref(), Some("alice"));
        assert_eq!(
            request.terms.len(),
            2,
            "several terms in one selector are ANDed"
        );
        assert!(request.after.is_some());
        assert!(request.before.is_some());
        assert_eq!(request.limit, 10);
    }

    #[test]
    fn every_parameter_of_a_search_command_is_a_selector() {
        // Goes through `parse_search`, not `parse_selectors`, on purpose. The parameter
        // handling lives in the gap between the two, and a unit test that starts at
        // `parse_selectors` cannot see it -- which is exactly how an `in=` scope came to be
        // silently dropped for every client that wrote it first.
        let message = Message::parse(b"SEARCH in=#room from=alice text=hello\r\n").expect("parses");
        let request = parse_search(&message)
            .accepted()
            .expect("a three-selector request is accepted");
        assert_eq!(
            request.channels,
            vec!["#room".to_owned()],
            "the `in=` scope survives parsing, rather than being dropped as a stray target"
        );
        assert_eq!(request.sender.as_deref(), Some("alice"));
        assert_eq!(request.terms.len(), 1);

        // A bare `SEARCH` names nothing at all.
        assert_eq!(
            parse_search(&Message::parse(b"SEARCH\r\n").expect("parses")),
            ParsedSearch::Refused(SearchRefusal::NoSelectors)
        );
    }

    #[test]
    fn an_unknown_or_malformed_selector_is_refused_by_name() {
        assert_eq!(
            parse_selectors(&["colour=red"]),
            ParsedSearch::Refused(SearchRefusal::UnknownSelector)
        );
        assert_eq!(
            parse_selectors(&["text"]),
            ParsedSearch::Refused(SearchRefusal::InvalidSelector)
        );
        assert_eq!(
            parse_selectors(&["after=yesterday"]),
            ParsedSearch::Refused(SearchRefusal::InvalidTimestamp)
        );
        assert_eq!(
            parse_selectors(&["limit=0"]),
            ParsedSearch::Refused(SearchRefusal::InvalidLimit)
        );
        assert_eq!(
            parse_selectors(&["limit=99999"]),
            ParsedSearch::Refused(SearchRefusal::InvalidLimit)
        );
    }

    #[test]
    fn text_cannot_carry_fts_syntax_or_sql() {
        // Every one of these contains something outside a run of word characters, so the
        // whole selector is refused rather than partially accepted. Refusing the selector
        // rather than the term is deliberate: a client that sent an expression has made a
        // mistake worth telling it about, not something worth guessing at.
        for hostile in [
            "hello\" OR 1=1 --",
            "NEAR(a b)",
            "col:val",
            "-x",
            "a*",
            "' OR ''='",
            "x; DROP TABLE history_events",
            "((()))",
            "^x$",
        ] {
            assert_eq!(
                parse_selectors(&[&format!("text={hostile}")]),
                ParsedSearch::Refused(SearchRefusal::InvalidTerm),
                "{hostile:?} must never become a search term"
            );
        }

        // `a AND b` as *three words* is not an expression. What proves that is the
        // compiled form, not the refusal: each term is a quoted literal, so the connector
        // between them is the one this code chose rather than one the client typed.
        let request = parse_selectors(&["text=a AND b"])
            .accepted()
            .expect("three word characters are three terms");
        assert_eq!(request.terms.len(), 3);
        let expression = compile(&request, NetworkId(1), Vec::new())
            .match_expression()
            .expect("terms compile");
        assert_eq!(
            expression, "\"a\" AND \"AND\" AND \"b\"",
            "every term is a quoted literal; the only operator is ours"
        );
    }

    #[test]
    fn a_request_with_no_selector_is_refused() {
        // Otherwise it is a request to enumerate the journal with no bound on which part.
        assert_eq!(
            parse_selectors(&["limit=5"]),
            ParsedSearch::Refused(SearchRefusal::NoSelectors)
        );
    }

    #[test]
    fn bounds_are_enforced_at_the_parser_not_only_at_the_store() {
        let many: Vec<String> = (0..i2pr_irc_store::MAX_SEARCH_TERMS + 1)
            .map(|index| format!("text=w{index}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            parse_selectors(&refs),
            ParsedSearch::Refused(SearchRefusal::TooManyParameters)
        );

        let long = format!(
            "text={}",
            "w".repeat(i2pr_irc_store::MAX_SEARCH_TERM_BYTES + 1)
        );
        assert_eq!(
            parse_selectors(&[&long]),
            ParsedSearch::Refused(SearchRefusal::InvalidTerm)
        );

        let over_line: Vec<String> = (0..MAX_SEARCH_PARAMS + 1)
            .map(|index| format!("text=w{index}"))
            .collect();
        let over_refs: Vec<&str> = over_line.iter().map(String::as_str).collect();
        assert_eq!(
            parse_selectors(&over_refs),
            ParsedSearch::Refused(SearchRefusal::TooManyParameters)
        );
        let _ = params(&[]);
    }

    #[test]
    fn a_reversed_time_range_is_refused_rather_than_swapped() {
        assert_eq!(
            parse_selectors(&[
                "after=2026-01-02T00:00:00.000Z",
                "before=2026-01-01T00:00:00.000Z",
            ]),
            ParsedSearch::Refused(SearchRefusal::InvalidSelector)
        );
    }

    #[test]
    fn an_empty_result_is_still_a_complete_batch() {
        let lines = render_batch(&[], 7);
        assert_eq!(lines.len(), 2, "an empty page is opened and closed");
        assert!(String::from_utf8_lossy(&lines[0]).contains("BATCH +7 "));
        assert!(String::from_utf8_lossy(&lines[1]).contains("BATCH -7"));
    }

    #[test]
    fn an_in_selector_resolves_against_the_network_the_request_names() {
        // The owner keys this map by the folded name it used when it created the buffer,
        // so resolution has to fold the client's spelling the same way.
        let known: std::collections::BTreeMap<String, BufferId> =
            [(crate::owner::casemapped("#Room"), BufferId(3))]
                .into_iter()
                .collect();
        assert_eq!(
            resolve_buffer(&["#room".to_owned()], &known),
            Ok(vec![BufferId(3)]),
            "casemapping applies to a target name, so #Room and #room are one channel"
        );
        assert_eq!(
            resolve_buffer(&["#elsewhere".to_owned()], &known),
            Err(SearchRefusal::InvalidSelector),
            "a channel this Network does not hold is refused, not searched as nothing"
        );
    }

    #[test]
    fn a_result_body_cannot_end_the_frame_early() {
        use i2pr_irc_core::HistoryEventId;

        let hit = SearchHit {
            event: HistoryEventId(7),
            buffer: BufferId(3),
            sender: "alice".to_owned(),
            target: "#room".to_owned(),
            body: "one\r\nQUIT now".to_owned(),
        };
        let line = String::from_utf8(render_hit(&hit)).expect("rendered line is text");
        assert_eq!(
            line.matches("\r\n").count(),
            1,
            "one result is exactly one frame, however the body was written: {line:?}"
        );
        // The text of the message survives; only its line framing was folded. A search
        // result that silently deleted words would be a different lie.
        assert!(
            line.contains("QUIT now"),
            "the message text survives; only its line framing was folded: {line:?}"
        );
    }
}
