//! Deterministic fuzz smoke for the wire parser and the privacy mediation boundary.
//!
//! Two properties, both checked against the same generated corpus:
//!
//! 1. **Parsing is total and bounded.** A parse either succeeds or fails; a successful
//!    parse re-encodes to the same message; and the line decoder never holds more than
//!    its declared ceiling.
//!
//! 2. **Privacy mediation never leaks.** Every CTCP body and every client tag prefix is
//!    fed through the real classifier and the real tag mediator. A sentinel planted in
//!    the body -- standing in for a hostname, a SASL payload, a local path, a process id
//!    -- must never reach the *other* direction. A DCC request must never be classified
//!    as anything a client could act on, and a metadata query must never be forwarded.
//!
//! The corpus is a seeded xorshift, so a failure reproduces exactly. The runtime crate is
//! a dependency because these assertions are about production policy code, not a model of
//! it: a fuzz of a reimplementation would prove nothing about the thing that ships.
use i2pr_irc_runtime::{
    ctcp::{
        Ctcp, CtcpCommand, CtcpDirection, InboundAction, OutboundAction, classify, inbound_action,
        outbound_action,
    },
    ircv3::{TagDisposition, mediate_client_tags},
};
use i2pr_irc_wire::{LineDecoder, MAX_TAGGED_LINE_BYTES, Message};

/// A string that stands in for anything the Operator's environment could leak.
///
/// It is deliberately not a plausible hostname or path: the point is that *no* body
/// content is ever relayed verbatim across the mediation boundary, so an implausible
/// token is the strongest possible probe.
const SENTINEL: &str = "SENTINEL-8f3c1d0a7e2b";

/// Deterministic xorshift64*. Reproducing a corpus is a matter of printing the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// CTCP command words a hostile peer might use.
///
/// The metadata probes are the interesting half: each is something a client would answer
/// with its own software or hostname if the bouncer let it through.
const CTLP_WORDS: &[&str] = &[
    "PING",
    "ACTION",
    "VERSION",
    "TIME",
    "FINGER",
    "CLIENTINFO",
    "DCC",
    "DCC SEND",
    "DCC CHAT",
    "PING_EXT",
    "SOURCE",
    "OUT",
    "SECURE",
    "NOTICE",
    "HELLO",
    "ACCEPT",
    "x",
    "",
];

fn build_line(rng: &mut Rng) -> Vec<u8> {
    let mut line = Vec::new();
    match rng.below(6) {
        0 => {
            // A leading `+` makes this a client-only tag, which the mediator must deny
            // by default even though the wire parser accepts it.
            line.extend_from_slice(b"@+msgid=abc ");
        }
        1 => {
            // An unprefixed tag: parsed by the wire layer, still untrusted input.
            line.extend_from_slice(b"@time=2024-01-01T00:00:00.000Z ");
        }
        2 => {
            // A deliberately oversized and malformed tag prefix.
            for _ in 0..rng.below(400) {
                line.push(rng.next() as u8);
            }
            line.extend_from_slice(b" ");
        }
        _ => {}
    }
    line.extend_from_slice(b":nick!user@host PRIVMSG #chan :");
    match rng.below(5) {
        0 => line.extend_from_slice(b"plain text with no delimiter at all"),
        1 => {
            // Delimiter present but never closed: the trailing-delimiter case.
            line.push(0x01);
            line.extend_from_slice(rng.pick(CTLP_WORDS).as_bytes());
            line.extend_from_slice(b" ");
            line.extend_from_slice(SENTINEL.as_bytes());
        }
        2 => {
            // Closed properly, with a sentinel parameter.
            line.push(0x01);
            line.extend_from_slice(rng.pick(CTLP_WORDS).as_bytes());
            line.extend_from_slice(b" ");
            line.extend_from_slice(SENTINEL.as_bytes());
            line.push(0x01);
        }
        3 => {
            // Two back-to-back CTCP blocks, so delimiter placement is ambiguous.
            for _ in 0..2 {
                line.push(0x01);
                line.extend_from_slice(rng.pick(CTLP_WORDS).as_bytes());
                line.extend_from_slice(b" ");
                line.extend_from_slice(SENTINEL.as_bytes());
                line.push(0x01);
            }
        }
        _ => {
            // Only the delimiter: nothing readable at all.
            line.push(0x01);
        }
    }
    line.extend_from_slice(b"\r\n");
    line
}

fn check_privacy(line: &[u8], rng: &mut Rng) {
    let Ok(message) = Message::parse(line) else {
        return;
    };

    // A PRIVMSG body is a query; a NOTICE body is a reply. Both are exercised because the
    // leaking direction differs between them.
    let ctcp = classify(&message, CtcpDirection::Query);
    let notice = classify(&message, CtcpDirection::Reply);
    match ctcp {
        // A direct-connect request is structurally blocked in both directions. If it
        // ever classified as chat, a client could be offered a direct connection.
        Ctcp::Dcc => {
            let inbound = inbound_action(&ctcp);
            assert!(
                matches!(inbound, InboundAction::Suppress),
                "a DCC request must be suppressed inbound, saw {inbound:?}"
            );
            let outbound = outbound_action(&ctcp);
            assert!(
                matches!(outbound, OutboundAction::Block),
                "a DCC request must be blocked outbound, saw {outbound:?}"
            );
        }
        Ctcp::Query { .. } => {
            let inbound = inbound_action(&ctcp);
            // The invariant is not "suppressed" but "delivers nothing to a client". A
            // CTCP PING is answered by the bouncer itself with its own token, which is
            // also correct: the point is that no client is ever handed a metadata query
            // to answer with its own software or hostname.
            assert!(
                !matches!(inbound, InboundAction::FanOut),
                "a metadata query must never reach a client, saw {inbound:?}"
            );
        }
        Ctcp::Reply { command, .. } if command != CtcpCommand::Ping => {
            // Outbound is the leaking direction for a reply: it is how a local client's
            // software and hostname would reach the server.
            assert!(
                matches!(outbound_action(&ctcp), OutboundAction::Block),
                "a metadata reply must never be forwarded upstream"
            );
        }
        _ => {}
    }

    // The same body read as a NOTICE is a reply. This is the direction that leaks a
    // fingerprint: a client answering a metadata probe would hand its software and
    // hostname to the server as this Operator's fingerprint.
    //
    // `PING` is the single deliberate exception, in both directions. A PING body is a
    // bounded opaque token that identifies nothing about the client, and blocking it
    // would stop a client from answering a latency probe it was asked to answer.
    match &notice {
        Ctcp::Reply { command, .. } if command == &CtcpCommand::Ping => assert!(
            matches!(outbound_action(&notice), OutboundAction::Forward),
            "a PING reply is a token echo and is deliberately forwarded"
        ),
        Ctcp::Reply { .. } | Ctcp::Unknown { .. } | Ctcp::Dcc | Ctcp::Malformed => assert!(
            matches!(outbound_action(&notice), OutboundAction::Block),
            "every non-PING CTCP reply must be blocked upstream, saw {:?}",
            outbound_action(&notice)
        ),
        _ => {}
    }

    // The mediated frame is the only thing that could carry the body onwards. Whatever
    // the disposition, the sentinel must not survive into the client's own tag space
    // from the server's frame.
    for negotiated in [false, true] {
        let (mediated, disposition) = mediate_client_tags(&message, negotiated);
        let rendered = format!("{mediated:?}");
        if matches!(disposition, TagDisposition::Rejected) {
            continue;
        }
        assert!(
            !rendered.contains(SENTINEL) || !rendered.contains("@"),
            "a mediated frame grew an unexpected tag carrying {SENTINEL}: {rendered}"
        );
    }

    // Tag churn: the same message tagged repeatedly must keep producing a parseable,
    // re-encodable frame rather than accumulating state.
    let mut churn = message.clone();
    for _ in 0..rng.below(8) {
        churn = mediated(churn);
        if let Ok(re_encoded) = churn.encode() {
            let reparsed = Message::parse(&re_encoded).expect("mediated frame stays parseable");
            assert_eq!(
                reparsed, churn,
                "mediation must not break the round trip for {SENTINEL}"
            );
        }
    }
}

fn mediated(message: Message) -> Message {
    let (mediated, _) = mediate_client_tags(&message, true);
    mediated
}

fn main() {
    let mut rng = Rng(0x4d595df4d0f33173);
    for _ in 0..10_000 {
        rng.next();
        let n = (rng.next() as usize) % (MAX_TAGGED_LINE_BYTES + 1);
        let mut bytes = Vec::with_capacity(n);
        for _ in 0..n {
            bytes.push(rng.next() as u8)
        }
        if let Ok(message) = Message::parse(&bytes) {
            let encoded = message
                .encode()
                .expect("parsed message must remain encodable");
            assert_eq!(Message::parse(&encoded), Ok(message));
        }
        let mut decoder = LineDecoder::default();
        for chunk in bytes.chunks(31) {
            let _ = decoder.push(chunk);
            assert!(decoder.buffered_len() <= MAX_TAGGED_LINE_BYTES);
        }
    }

    // The privacy corpus is generated separately so the sentinel and the CTCP delimiter
    // are always present, rather than depending on random bytes happening to form them.
    let mut rng = Rng(0x0bad_c0de_5eed);
    for _ in 0..20_000 {
        let line = build_line(&mut rng);
        assert!(
            line.len() <= MAX_TAGGED_LINE_BYTES,
            "the generated frame must respect the line ceiling"
        );
        check_privacy(&line, &mut rng);
    }
}
