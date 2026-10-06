//! Bounded CTCP parsing and the directional privacy policy.
//!
//! # Why CTCP is parsed at all
//!
//! CTCP is the mechanism through which an IRC client auto-reveals itself: it answers
//! `VERSION`, `TIME`, `USERINFO`, `SOURCE`, `FINGER` and `CLIENTINFO` unprompted, and
//! `DCC` requests a direct network connection. On an anonymity network the *client's*
//! metadata is the fingerprint that matters — the transport already hides the operator's
//! address, but a client that answers `CLIENTINFO` names its own software. So the bouncer
//! classifies CTCP rather than forwarding it, and answers a safe subset itself.
//!
//! # Why the parse is hand-written
//!
//! Not performance. A regex over an attacker-controlled body invites backtracking, and a
//! CTCP body here is untrusted input on both directions of the wire. The parser is a
//! bounded byte scan with explicit limits, so its cost is a function of the message limit
//! the wire crate already enforces, not of pattern complexity.
//!
//! # Why `Dcc` is a classification and not a parse
//!
//! [`Ctcp::Dcc`] deliberately carries **no parameters**. DCC exists here only so the
//! policy can name it as blocked. No host/port pair is ever produced, so there is no
//! value for a listener or dialer to be built from, and no parse of `DCC SEND`, `DCC
//! CHAT chat 1 2 3` or any passive form is reachable. See `scripts/check-network-boundary.py`.
use i2pr_irc_wire::Message;

/// The CTCP delimiter.
pub const CTCP_DELIMITER: u8 = 0x01;
/// Ceiling on CTCP parameters, so a body of many short tokens stays bounded.
pub const MAX_CTCP_PARAMS: usize = 8;
/// Ceiling on one CTCP parameter's bytes.
pub const MAX_CTCP_PARAM_BYTES: usize = 64;
/// Ceiling on the CTCP command word.
pub const MAX_CTCP_COMMAND_BYTES: usize = 16;

/// The metadata commands this build recognises well enough to name.
///
/// They are named so the policy can distinguish "a known fingerprint probe" from "an
/// unknown request". Both are suppressed inbound; being explicit keeps a future
/// allowlist decision reviewable rather than implicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CtcpCommand {
    Ping,
    Version,
    Time,
    Userinfo,
    Source,
    Finger,
    Clientinfo,
}

impl CtcpCommand {
    fn parse(word: &[u8]) -> Option<Self> {
        match word {
            b"PING" => Some(Self::Ping),
            b"VERSION" => Some(Self::Version),
            b"TIME" => Some(Self::Time),
            b"USERINFO" => Some(Self::Userinfo),
            b"SOURCE" => Some(Self::Source),
            b"FINGER" => Some(Self::Finger),
            b"CLIENTINFO" => Some(Self::Clientinfo),
            _ => None,
        }
    }
}

/// The result of classifying one message body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ctcp {
    /// `\x01ACTION <text>` — ordinary chat semantics, never a request.
    Action(Vec<u8>),
    /// A metadata query aimed at whoever sent it.
    Query {
        command: CtcpCommand,
        params: Vec<String>,
    },
    /// A metadata reply.
    Reply {
        command: CtcpCommand,
        params: Vec<String>,
    },
    /// A CTCP whose command word is not one this build names.
    Unknown {
        command: String,
        params: Vec<String>,
    },
    /// A direct-connect request, in any form. Structurally blocked.
    Dcc,
    /// Began with the delimiter but could not be read as CTCP.
    Malformed,
    /// Not CTCP at all. The message is ordinary text.
    OrdinaryText,
}

impl Ctcp {
    /// The CTCP command word, upper-cased, for diagnostics and policy tests.
    pub fn command_word(&self) -> String {
        match self {
            Self::Action(_) => "ACTION".to_owned(),
            Self::Query { command, .. } | Self::Reply { command, .. } => {
                format!("{command:?}").to_uppercase()
            }
            Self::Unknown { command, .. } => command.clone(),
            Self::Dcc => "DCC".to_owned(),
            Self::Malformed => "<malformed>".to_owned(),
            Self::OrdinaryText => String::new(),
        }
    }
}

/// Whether a body is a query or a reply.
///
/// PRIVMSG carries a query and NOTICE carries a reply in ordinary IRC usage. The
/// distinction decides the inbound policy, because answering a reply is not the same act
/// as answering a query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CtcpDirection {
    /// The body came from a PRIVMSG, so it is a query.
    Query,
    /// The body came from a NOTICE, so it is a reply.
    Reply,
}

/// Classifies one parsed message as CTCP or ordinary text.
///
/// Returns [`Ctcp::OrdinaryText`] for anything that is not a PRIVMSG/NOTICE, so callers
/// can classify an arbitrary line without pre-filtering on command.
pub fn classify(message: &Message, direction: CtcpDirection) -> Ctcp {
    if !matches!(
        &message.command[..],
        [b'P', b'R', b'I', b'V', b'M', b'S', b'G'] | [b'N', b'O', b'T', b'I', b'C', b'E']
    ) {
        return Ctcp::OrdinaryText;
    }
    let Some(body) = message.params.last() else {
        return Ctcp::OrdinaryText;
    };
    parse_body(body, direction)
}

/// Parses one trailing parameter as a CTCP body.
///
/// The final delimiter is tolerated when absent, because clients commonly omit it and
/// refusing would drop an ordinary-looking action. CTCP is **not** recognised inside
/// arbitrary mixed text: the body must *begin* with the delimiter, so a sentence
/// containing `\x01` is ordinary text rather than an unparsed command.
pub fn parse_body(body: &[u8], direction: CtcpDirection) -> Ctcp {
    if body.first() != Some(&CTCP_DELIMITER) {
        return Ctcp::OrdinaryText;
    }
    let mut inner = &body[1..];
    // Tolerate a missing final delimiter, and tolerate a doubled opening delimiter that
    // some clients emit, without ever searching the body for a delimiter.
    if inner.last() == Some(&CTCP_DELIMITER) {
        inner = &inner[..inner.len() - 1];
    }
    while inner.first() == Some(&CTCP_DELIMITER) {
        inner = &inner[1..];
    }
    let Some(split) = inner.iter().position(|byte| *byte == b' ') else {
        // A body with no space is a bare command word with no parameters.
        return classify_word(inner, &[], direction);
    };
    let (word, rest) = inner.split_at(split);
    let rest = &rest[1..];
    if word == b"ACTION" {
        return match direction {
            CtcpDirection::Query => Ctcp::Action(rest.to_vec()),
            // An ACTION carried in a NOTICE is not a chat action, and clients do emit
            // it. Treating it as ordinary text is safer than treating it as an action
            // that invites a reply.
            CtcpDirection::Reply => Ctcp::Malformed,
        };
    }
    if word.starts_with(b"DCC") {
        // Recognised only to be blocked. No parameter is ever produced.
        return Ctcp::Dcc;
    }
    let params = match bounded_params(rest) {
        Some(params) => params,
        None => return Ctcp::Malformed,
    };
    classify_word(word, &params, direction)
}

/// Classifies one already-split command word and its parameters.
fn classify_word(word: &[u8], params: &[String], direction: CtcpDirection) -> Ctcp {
    if word.is_empty() || word.len() > MAX_CTCP_COMMAND_BYTES {
        return Ctcp::Malformed;
    }
    // A command word must be plain uppercase ASCII; anything else is not a command name
    // this parser will act on.
    if !word
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Ctcp::Malformed;
    }
    if word.starts_with(b"DCC") {
        return Ctcp::Dcc;
    }
    match CtcpCommand::parse(word) {
        Some(command) => match direction {
            CtcpDirection::Query => Ctcp::Query {
                command,
                params: params.to_vec(),
            },
            CtcpDirection::Reply => Ctcp::Reply {
                command,
                params: params.to_vec(),
            },
        },
        None => Ctcp::Unknown {
            command: String::from_utf8_lossy(word).into_owned(),
            params: params.to_vec(),
        },
    }
}

/// Splits a parameter list under explicit count and length ceilings.
///
/// A body exceeding either ceiling is malformed rather than truncated: silently dropping
/// a trailing parameter could turn `DCC SEND file 1 2 3` into something that looks
/// harmless.
fn bounded_params(rest: &[u8]) -> Option<Vec<String>> {
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let mut params = Vec::new();
    for part in rest.split(|byte| *byte == b' ') {
        if part.is_empty() {
            // A doubled space is tolerated rather than treated as an empty parameter.
            continue;
        }
        if part.len() > MAX_CTCP_PARAM_BYTES {
            return None;
        }
        // NUL, CR and LF cannot reach here through the wire parser, but the bound is
        // enforced here too so this function is safe on its own.
        if part
            .iter()
            .any(|byte| *byte == 0 || *byte == b'\r' || *byte == b'\n')
        {
            return None;
        }
        params.push(String::from_utf8_lossy(part).into_owned());
        if params.len() > MAX_CTCP_PARAMS {
            return None;
        }
    }
    Some(params)
}

/// What the bouncer does with a CTCP arriving from upstream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundAction {
    /// Deliver to every attached client as ordinary chat.
    FanOut,
    /// Answer privately with this exact token, and deliver nothing to clients.
    AnswerPing(&'static str),
    /// Deliver to nobody.
    Suppress,
}

/// The upstream-to-downstream policy.
///
/// Only two outcomes survive: an action, or nothing. Every metadata query is suppressed,
/// so no attached client is ever prompted to auto-reveal itself, and `DCC` never reaches
/// a client as anything it could act on.
pub fn inbound_action(ctcp: &Ctcp) -> InboundAction {
    match ctcp {
        // Ordinary text is the overwhelming majority of chat and must be untouched.
        Ctcp::OrdinaryText | Ctcp::Action(_) => InboundAction::FanOut,
        // The bouncer answers this itself, so the query is never handed to a client that
        // would answer it with its own hostname and software version.
        Ctcp::Query {
            command: CtcpCommand::Ping,
            ..
        } => InboundAction::AnswerPing(TOKEN_REFLECT),
        // Everything else is either a fingerprint probe, a direct-connect request, an
        // unknown command, or a body that began with the delimiter but could not be
        // read. None of those may reach a client.
        Ctcp::Query { .. }
        | Ctcp::Reply { .. }
        | Ctcp::Unknown { .. }
        | Ctcp::Dcc
        | Ctcp::Malformed => InboundAction::Suppress,
    }
}

/// What the bouncer does with a CTCP arriving from a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundAction {
    /// Forward to upstream.
    Forward,
    /// Do not forward. The client is told nothing was sent.
    Block,
}

/// The downstream-to-upstream policy.
///
/// The allowlist is ACTION and PING, and nothing else. A metadata *reply* is the
/// dangerous direction: it is how a local client's software and hostname would reach the
/// upstream server and become this Operator's fingerprint, so all of them are blocked
/// regardless of how well-formed they are.
pub fn outbound_action(ctcp: &Ctcp) -> OutboundAction {
    match ctcp {
        // Ordinary text is the overwhelming majority of client traffic and must be
        // untouched, as must every command that is not PRIVMSG or NOTICE at all.
        Ctcp::OrdinaryText | Ctcp::Action(_) => OutboundAction::Forward,
        Ctcp::Query {
            command: CtcpCommand::Ping,
            ..
        }
        | Ctcp::Reply {
            command: CtcpCommand::Ping,
            ..
        } => OutboundAction::Forward,
        Ctcp::Query { .. }
        | Ctcp::Reply { .. }
        | Ctcp::Unknown { .. }
        | Ctcp::Dcc
        | Ctcp::Malformed => OutboundAction::Block,
    }
}

/// Marker meaning "reflect the first bounded parameter back".
pub const TOKEN_REFLECT: &str = "\u{0}reflect";
/// The fixed answer used when the request carried no usable token.
pub const TOKEN_PLACEHOLDER: &str = "bouncer";

/// The reply text for an answered inbound `PING`, bounded by the message limit.
pub fn ping_reply_text(ctcp: &Ctcp) -> Option<String> {
    match ctcp {
        Ctcp::Query {
            command: CtcpCommand::Ping,
            params,
        } => Some(match params.first() {
            Some(token) if token.len() <= MAX_CTCP_PARAM_BYTES => token.clone(),
            _ => TOKEN_PLACEHOLDER.to_owned(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_wire::Message;

    fn privmsg(body: &[u8]) -> Message {
        Message::parse(format!(":a!b@c PRIVMSG #r :{}\r\n", escape(body)).as_bytes())
            .expect("parses")
    }
    fn notice(body: &[u8]) -> Message {
        Message::parse(format!(":a!b@c NOTICE #r :{}\r\n", escape(body)).as_bytes())
            .expect("parses")
    }
    /// Embeds raw bytes in a way that survives `String`.
    fn escape(body: &[u8]) -> String {
        body.iter()
            .map(|byte| char::from(*byte))
            .collect::<String>()
            .replace(['\r', '\n'], "")
    }

    #[test]
    fn text_that_merely_contains_the_delimiter_is_not_ctcp() {
        // CTCP is not interpreted inside arbitrary mixed text: the body must begin with
        // the delimiter, so an embedded one cannot smuggle a command through.
        let mixed = b"look at this \x01VERSION later";
        assert_eq!(parse_body(mixed, CtcpDirection::Query), Ctcp::OrdinaryText);
    }

    #[test]
    fn a_missing_final_delimiter_is_tolerated() {
        // Clients commonly omit it; refusing would drop an ordinary-looking action.
        assert_eq!(
            parse_body(b"\x01ACTION waves", CtcpDirection::Query),
            Ctcp::Action(b"waves".to_vec())
        );
    }

    #[test]
    fn action_is_chat_in_both_directions() {
        let inbound = parse_body(b"\x01ACTION waves\x01", CtcpDirection::Query);
        assert_eq!(inbound_action(&inbound), InboundAction::FanOut);
        let outbound = parse_body(b"\x01ACTION waves\x01", CtcpDirection::Query);
        assert_eq!(outbound_action(&outbound), OutboundAction::Forward);
    }

    #[test]
    fn an_inbound_ping_is_answered_by_the_bouncer_not_the_client() {
        let ping = parse_body(b"\x01PING 12345\x01", CtcpDirection::Query);
        assert_eq!(
            inbound_action(&ping),
            InboundAction::AnswerPing(TOKEN_REFLECT)
        );
        assert_eq!(ping_reply_text(&ping).as_deref(), Some("12345"));
    }

    #[test]
    fn every_metadata_query_is_suppressed_inbound() {
        for body in [
            b"\x01VERSION\x01".as_slice(),
            b"\x01TIME\x01",
            b"\x01USERINFO\x01",
            b"\x01SOURCE\x01",
            b"\x01FINGER\x01",
            b"\x01CLIENTINFO\x01",
        ] {
            let parsed = parse_body(body, CtcpDirection::Query);
            assert_eq!(
                inbound_action(&parsed),
                InboundAction::Suppress,
                "{} must not prompt an attached client",
                parsed.command_word()
            );
        }
    }

    #[test]
    fn every_metadata_reply_is_blocked_outbound() {
        // This is the fingerprint direction: a client answering VERSION names its own
        // software to the upstream server as this Operator.
        for body in [
            b"\x01VERSION cool-irc 1.0\x01".as_slice(),
            b"\x01TIME\x01",
            b"\x01USERINFO\x01",
            b"\x01SOURCE\x01",
            b"\x01FINGER\x01",
            b"\x01CLIENTINFO\x01",
        ] {
            let parsed = parse_body(body, CtcpDirection::Reply);
            assert_eq!(
                outbound_action(&parsed),
                OutboundAction::Block,
                "{} must never reach upstream",
                parsed.command_word()
            );
        }
    }

    #[test]
    fn dcc_is_structurally_absent_in_every_form() {
        // No host/port pair is ever produced, in any of the shapes clients emit.
        for body in [
            b"\x01DCC CHAT chat 192 168 0 1 6667\x01".as_slice(),
            b"\x01DCC SEND file 192 168 0 1 6667\x01",
            b"\x01DCC RESUME 12345\x01",
            b"\x01DCC ACCEPT 192 168 0 1\x01",
            b"\x01DCC\x01",
        ] {
            let parsed = parse_body(body, CtcpDirection::Query);
            assert_eq!(
                parsed,
                Ctcp::Dcc,
                "DCC must carry no parameters at all: {}",
                String::from_utf8_lossy(body)
            );
            assert_eq!(outbound_action(&parsed), OutboundAction::Block);
            assert_eq!(inbound_action(&parsed), InboundAction::Suppress);
        }
    }

    #[test]
    fn an_unknown_ctcp_is_denied_by_default() {
        let parsed = parse_body(b"\x01SOMETHING 1 2\x01", CtcpDirection::Query);
        assert_eq!(
            parsed,
            Ctcp::Unknown {
                command: "SOMETHING".to_owned(),
                params: vec!["1".to_owned(), "2".to_owned()],
            }
        );
        assert_eq!(outbound_action(&parsed), OutboundAction::Block);
        assert_eq!(inbound_action(&parsed), InboundAction::Suppress);
    }

    #[test]
    fn malformed_bodies_are_never_actionable() {
        for body in [
            b"\x01\x01".as_slice(),
            b"\x01 \x01",
            b"\x01LOWERCASE\x01",
            b"\x01VERSION\x01\x01",
        ] {
            let parsed = parse_body(body, CtcpDirection::Query);
            assert!(
                matches!(
                    parsed,
                    Ctcp::Malformed | Ctcp::OrdinaryText | Ctcp::Unknown { .. }
                ),
                "{:?} must not be actionable",
                parsed
            );
        }
    }

    #[test]
    fn parameter_count_and_length_are_bounded() {
        let many: Vec<u8> = std::iter::once(b"\x01VERSION".to_vec())
            .chain(std::iter::repeat_n(b"a".to_vec(), MAX_CTCP_PARAMS + 2))
            .flatten()
            .chain(std::iter::once(0x01))
            .collect();
        assert_eq!(parse_body(&many, CtcpDirection::Query), Ctcp::Malformed);

        let long = format!("\x01PING {}\x01", "x".repeat(MAX_CTCP_PARAM_BYTES + 1));
        assert_eq!(
            parse_body(long.as_bytes(), CtcpDirection::Query),
            Ctcp::Malformed
        );
    }

    #[test]
    fn a_command_word_beyond_the_ceiling_is_malformed() {
        let body = format!("\x01{}\x01", "A".repeat(MAX_CTCP_COMMAND_BYTES + 1));
        assert_eq!(
            parse_body(body.as_bytes(), CtcpDirection::Query),
            Ctcp::Malformed
        );
    }

    #[test]
    fn classification_only_applies_to_privmsg_and_notice() {
        assert_eq!(
            classify(&privmsg(b"hello"), CtcpDirection::Query),
            Ctcp::OrdinaryText
        );
        assert_eq!(
            classify(&notice(b"\x01ACTION hi\x01"), CtcpDirection::Reply),
            Ctcp::Malformed,
            "an ACTION carried in a NOTICE is not a chat action"
        );
        assert!(matches!(
            classify(&privmsg(b"\x01VERSION\x01"), CtcpDirection::Query),
            Ctcp::Query { .. }
        ));
        assert!(matches!(
            classify(&notice(b"\x01VERSION x\x01"), CtcpDirection::Reply),
            Ctcp::Reply { .. }
        ));
    }

    #[test]
    fn a_reply_never_becomes_an_actionable_query() {
        // The same bytes mean different things by direction, and the inbound policy
        // depends on it.
        let as_reply = classify(&notice(b"\x01PING 1\x01"), CtcpDirection::Reply);
        assert!(matches!(as_reply, Ctcp::Reply { .. }));
        assert_eq!(
            inbound_action(&as_reply),
            InboundAction::Suppress,
            "an unsolicited PING reply must not make the bouncer answer"
        );
    }

    #[test]
    fn a_ping_with_no_token_gets_a_fixed_answer() {
        let parsed = parse_body(b"\x01PING\x01", CtcpDirection::Query);
        assert_eq!(ping_reply_text(&parsed).as_deref(), Some(TOKEN_PLACEHOLDER));
    }
}
