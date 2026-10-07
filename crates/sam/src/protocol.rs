//! Typed replies and the strict sequencing a SAM exchange has to follow.
//!
//! # Why typed, not a string map
//!
//! Plan 030 section 6 requires it, and the reason is that a router's reply vocabulary
//! differs between Java I2P and i2pd in ways a string map cannot surface. `HELLO` answers
//! `HELLO OK` on one and `HELLO OK VERSION MIN=3.1 MAX=3.1` on the other; a
//! `STREAM STATUS` carries the failure class in `MESSAGE` on one and in `REASON` on the
//! other. If the parser produced a map, every caller would re-derive those rules, and they
//! would drift.
//!
//! # Why the sequencing is strict
//!
//! `new -> hello -> session-or-stream -> active-or-raw`. Each step permits exactly one
//! next step. A reply that arrives out of order is a protocol failure rather than
//! something to ignore: ignoring it is how a client ends up treating a `SESSION STATUS`
//! as a `STREAM STATUS` and believing it has a stream it does not have.

use zeroize::Zeroizing;

use crate::{
    error::{MalformedReason, SamError, SessionRejection, StreamRejection},
    line::{SamReply, parse_line},
};

/// A classified `HELLO` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloReply {
    /// The router speaks 3.1.
    Ok31 {
        /// The router's minimum version, when it reported one.
        min: Option<u16>,
        /// The router's maximum version, when it reported one.
        max: Option<u16>,
    },
    /// `HELLO NOVERSION`. The router requires a protocol this client does not speak.
    NoVersion,
}

/// A classified `SESSION STATUS` reply.
///
/// Carries no `DESTINATION`. A successful `SESSION CREATE` on a transient session makes
/// the router return the private Destination it generated, and Plan 030 section 8 says not
/// to retain it. The type has no field for it, so it cannot be retained by accident; the
/// value is parsed into a zeroizing buffer that is dropped when the line is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionReply {
    /// `RESULT=OK`. The session exists.
    Ok,
    /// `RESULT=ERROR`, classified.
    Error(SessionRejection),
}

/// A classified `STREAM STATUS` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReply {
    /// `RESULT=OK`. The socket is now a raw byte stream.
    Ok,
    /// `RESULT=ERROR`, classified.
    Error(StreamRejection),
}

/// Where a SAM client is in its exchange.
///
/// The sequencing is in the type: the only way to obtain a state is through the function
/// that consumes the previous one, so an out-of-order exchange cannot be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamState {
    /// The socket is open and nothing has been sent.
    HelloPending,
    /// `HELLO VERSION MIN=3.1 MAX=3.1` is awaiting its reply.
    AwaitingHello,
    /// `SESSION CREATE` is awaiting its reply.
    AwaitingSession,
    /// The session exists and the socket is a control channel.
    SessionActive,
    /// `STREAM CONNECT` is awaiting its reply.
    AwaitingStream,
    /// `RESULT=OK` was received; the socket carries raw bytes.
    RawStream,
}

impl SamState {
    /// Whether raw application bytes may cross this socket now.
    ///
    /// After a `STREAM STATUS` with `RESULT=OK` and only then. This is the single place
    /// that answers "may I stop parsing SAM on this socket?", so the answer cannot
    /// disagree with the parser.
    pub fn is_raw(self) -> bool {
        matches!(self, Self::RawStream)
    }
}

/// Parses one reply line into the shape a given phase expects.
///
/// `phase` decides which classification applies, which is what makes "a `SESSION STATUS`
/// arrived where a `STREAM STATUS` was expected" a typed failure.
pub fn classify(phase: SamState, line: &str) -> Result<Transition, SamError> {
    let reply: SamReply = parse_line(line)?;
    match phase {
        SamState::AwaitingHello => match reply.verb() {
            "HELLO" => {
                // Two documented success shapes, and this client must speak to both routers.
                //
                // The specification's canonical reply is
                // `HELLO REPLY RESULT=OK VERSION=3.1`, which is what i2pd sends.
                // Java I2P answers a bare `HELLO OK`, and some builds answer
                // `HELLO OK VERSION ...`. The bare form has no `=` in the token at all,
                // so it arrives as a valueless option rather than as `RESULT=OK`.
                //
                // Accepting only the Java form made every connect to i2pd fail at the
                // handshake, and nothing else in the workspace could have found it: the
                // scripted bridge answers `HELLO OK`, so the deterministic suite agreed
                // with the narrower rule. That is the shape of defect a real-router
                // qualification exists to catch, and it is why this comment names the
                // router rather than only the tokens.
                let ok =
                    reply.contains("OK") || reply.has("OK", "true") || reply.has("RESULT", "OK");
                if ok {
                    Ok(Transition::Hello(HelloReply::Ok31 {
                        min: reply.value("MIN").and_then(|value| value.parse().ok()),
                        max: reply.value("MAX").and_then(|value| value.parse().ok()),
                    }))
                } else if reply.has("RESULT", "NOVERSION") || reply.contains("NOVERSION") {
                    Ok(Transition::Hello(HelloReply::NoVersion))
                } else {
                    // Notably `HELLO REPLY RESULT=I2P_ERROR MESSAGE="..."`, which is a
                    // failed handshake rather than a version disagreement. Mapping it to
                    // `NoVersion` would send the operator looking at a protocol version
                    // when the bridge was reporting something else entirely.
                    Err(unexpected("HELLO"))
                }
            }
            _ => Err(unexpected(reply.verb())),
        },
        SamState::AwaitingSession => match reply.verb() {
            "SESSION" => {
                // `SESSION ID <value>` follows a successful create and `SESSION STATUS
                // ...` is the status. Both have the verb `SESSION`, so the second word is
                // what distinguishes them; without this check a `SESSION ID` line would
                // be read as a status with no `RESULT`, i.e. as a success.
                if !reply.contains("STATUS") {
                    return Err(unexpected("SESSION"));
                }
                match result_of(&reply)? {
                    "OK" => Ok(Transition::Session(SessionReply::Ok)),
                    "ERROR" => {
                        let error = reply
                            .value("ERROR")
                            .or_else(|| reply.value("MESSAGE"))
                            .unwrap_or_default();
                        Ok(Transition::Session(SessionReply::Error(
                            SessionRejection::classify(error),
                        )))
                    }
                    other => Err(unexpected(other)),
                }
            }
            _ => Err(unexpected(reply.verb())),
        },
        SamState::AwaitingStream => match reply.verb() {
            "STREAM" => match result_of(&reply)? {
                "OK" => Ok(Transition::Stream(StreamReply::Ok)),
                "ERROR" => {
                    // Java I2P puts the class in `MESSAGE`; i2pd has historically used
                    // `REASON`. Reading both is what makes one parser work on both.
                    let reason = reply
                        .value("MESSAGE")
                        .or_else(|| reply.value("REASON"))
                        .unwrap_or_default();
                    Ok(Transition::Stream(StreamReply::Error(
                        StreamRejection::classify(reason),
                    )))
                }
                other => Err(unexpected(other)),
            },
            _ => Err(unexpected(reply.verb())),
        },
        // A reply that arrives in any other state is a protocol failure, not a no-op.
        SamState::HelloPending | SamState::SessionActive | SamState::RawStream => {
            Err(SamError::Malformed {
                reason: MalformedReason::UnexpectedVerb,
            })
        }
    }
}

/// What a classified reply means for the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The hello exchange finished.
    Hello(HelloReply),
    /// The session exchange finished.
    Session(SessionReply),
    /// The stream exchange finished.
    Stream(StreamReply),
}

impl Transition {
    /// The state the client is in after this reply, if it succeeded.
    ///
    /// A failure is not a state: the caller must decide what to do, and collapsing that
    /// decision into an enum would hide it.
    pub fn next_state(self, current: SamState) -> Option<SamState> {
        match (self, current) {
            (Transition::Hello(HelloReply::Ok31 { .. }), SamState::AwaitingHello) => {
                Some(SamState::AwaitingSession)
            }
            (Transition::Session(SessionReply::Ok), SamState::AwaitingSession) => {
                Some(SamState::SessionActive)
            }
            (Transition::Stream(StreamReply::Ok), SamState::AwaitingStream) => {
                Some(SamState::RawStream)
            }
            _ => None,
        }
    }

    /// The error this transition represents, if it is a failure.
    pub fn as_error(self) -> Option<SamError> {
        match self {
            Transition::Hello(HelloReply::Ok31 { .. }) => None,
            Transition::Hello(HelloReply::NoVersion) => Some(SamError::UnsupportedVersion),
            Transition::Session(SessionReply::Ok) => None,
            Transition::Session(SessionReply::Error(rejection)) => {
                Some(SamError::SessionRejected { rejection })
            }
            Transition::Stream(StreamReply::Ok) => None,
            Transition::Stream(StreamReply::Error(StreamRejection::CantReachPeer)) => {
                Some(SamError::PeerUnavailable {
                    rejection: StreamRejection::CantReachPeer,
                })
            }
            Transition::Stream(StreamReply::Error(StreamRejection::InvalidKey)) => {
                Some(SamError::DestinationRejected)
            }
            Transition::Stream(StreamReply::Error(rejection)) => {
                Some(SamError::PeerUnavailable { rejection })
            }
        }
    }
}

fn result_of(reply: &SamReply) -> Result<&str, SamError> {
    reply.value("RESULT").ok_or(SamError::Malformed {
        reason: MalformedReason::MissingOption,
    })
}

fn unexpected(_verb: &str) -> SamError {
    SamError::Malformed {
        reason: MalformedReason::UnexpectedVerb,
    }
}

/// The exact `HELLO` line this profile sends.
///
/// Frozen rather than parameterised: a profile that lets a caller edit the negotiated
/// version is a profile that can negotiate something this client cannot speak.
pub fn hello_request() -> String {
    "HELLO VERSION MIN=3.1 MAX=3.1\r\n".to_owned()
}

/// Builds the one `SESSION CREATE` line this profile is allowed to send.
///
/// Plan 030 section 8 freezes the fields. Every one is emitted unconditionally and
/// unconditionally in this order, from a literal: there is no parameter through which a
/// caller could inject an option fragment, so there is no way to send arbitrary SAM
/// options even by accident.
///
/// `SIGNATURE_TYPE=7` is the recommended type; the explicit tunnel quantities keep
/// behaviour independent of whatever the router's defaults happen to be, which is what
/// would otherwise differ between Java I2P and i2pd.
pub fn session_create_request(id: &str) -> String {
    let mut line = String::with_capacity(MAX_REQUEST_BYTES);
    line.push_str("SESSION CREATE STYLE=STREAM ID=");
    line.push_str(id);
    line.push_str(" DESTINATION=TRANSIENT");
    line.push_str(" SIGNATURE_TYPE=7");
    line.push_str(" i2cp.leaseSetEncType=4");
    line.push_str(" i2cp.dontPublishLeaseSet=true");
    line.push_str(" inbound.quantity=2");
    line.push_str(" outbound.quantity=2");
    line.push_str("\r\n");
    debug_assert!(line.len() <= MAX_REQUEST_BYTES);
    line
}

/// Ceiling on a request line this crate builds, terminator included.
pub const MAX_REQUEST_BYTES: usize = crate::line::MAX_SAM_LINE_BYTES;

/// Builds the one `STREAM CONNECT` line this profile is allowed to send.
///
/// `SILENT=false` is deliberate. It asks the router to log the attempt at its own
/// console, which is the only place an operator can see *why* a peer was unreachable
/// without this bouncer reproducing router internals it must not depend on.
///
/// The destination is written verbatim because the router needs the key. It is never
/// logged, and the built line is wrapped so it is zeroized rather than left on the heap.
pub fn stream_connect_request(id: &str, destination: &str) -> Zeroizing<String> {
    let mut line = String::with_capacity(MAX_REQUEST_BYTES);
    line.push_str("STREAM CONNECT ID=");
    line.push_str(id);
    line.push_str(" DESTINATION=");
    line.push_str(destination);
    line.push_str(" SILENT=false\r\n");
    Zeroizing::new(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both routers' `HELLO` spellings, because they genuinely differ.
    #[test]
    fn hello_ok_is_recognised_in_both_router_spellings() {
        // The specification's canonical reply, which i2pd sends, and Java I2P's bare form.
        // The first of these is the regression: an owned client that only accepted the
        // bare `HELLO OK` could not complete a handshake with i2pd at all, and the
        // scripted bridge — which answers the bare form — agreed with it.
        for line in [
            "HELLO OK",
            "HELLO OK VERSION MIN=3.1 MAX=3.1",
            "HELLO REPLY RESULT=OK VERSION=3.1",
            "HELLO REPLY RESULT=OK",
        ] {
            let transition = classify(SamState::AwaitingHello, line).expect("a hello parses");
            assert!(
                matches!(transition, Transition::Hello(HelloReply::Ok31 { .. })),
                "{line}"
            );
            assert_eq!(
                transition.next_state(SamState::AwaitingHello),
                Some(SamState::AwaitingSession),
                "a successful hello advances to session-create"
            );
        }
    }

    #[test]
    fn hello_noversion_is_a_terminal_failure() {
        for line in ["HELLO NOVERSION", "HELLO REPLY RESULT=NOVERSION"] {
            let transition = classify(SamState::AwaitingHello, line).expect("a hello parses");
            assert_eq!(transition, Transition::Hello(HelloReply::NoVersion));
            assert_eq!(transition.as_error(), Some(SamError::UnsupportedVersion));
            assert_eq!(transition.next_state(SamState::AwaitingHello), None);
        }
    }

    /// A bridge reporting a handshake error is not a version disagreement.
    ///
    /// Mapping `RESULT=I2P_ERROR` onto `NoVersion` would send an operator looking at a
    /// protocol version when the bridge was reporting something else entirely.
    #[test]
    fn hello_i2p_error_is_a_failure_and_not_a_version_disagreement() {
        let error = classify(
            SamState::AwaitingHello,
            "HELLO REPLY RESULT=I2P_ERROR MESSAGE=\"Timeout waiting for HELLO VERSION\"",
        )
        .expect_err("an I2P_ERROR hello is not a success");
        assert!(
            !matches!(error, SamError::UnsupportedVersion),
            "a handshake error must not read as an unsupported version: {error:?}"
        );
    }

    /// The sequencing claim, in every wrong order.
    #[test]
    fn a_reply_in_the_wrong_phase_is_a_protocol_failure() {
        let cases = [
            // A session status where a hello was expected.
            (SamState::AwaitingHello, "SESSION STATUS RESULT=OK"),
            // A stream status where a hello was expected.
            (SamState::AwaitingHello, "STREAM STATUS RESULT=OK"),
            // A hello reply where a session status was expected.
            (SamState::AwaitingSession, "HELLO OK"),
            // A stream status where a session status was expected.
            (SamState::AwaitingSession, "STREAM STATUS RESULT=OK"),
            // A session status where a stream status was expected.
            (SamState::AwaitingStream, "SESSION STATUS RESULT=OK"),
            // Anything at all once the session is live.
            (SamState::SessionActive, "STREAM STATUS RESULT=OK"),
        ];
        for (state, line) in cases {
            assert_eq!(
                classify(state, line),
                Err(SamError::Malformed {
                    reason: MalformedReason::UnexpectedVerb
                }),
                "{line:?} must be refused in {state:?}"
            );
        }
    }

    /// `SESSION ID` is a distinct message that a naive `RESULT`-only parser would
    /// mistake for a session status.
    #[test]
    fn a_session_id_message_is_not_a_session_status() {
        assert_eq!(
            classify(SamState::AwaitingSession, "SESSION ID 7f000001"),
            Err(SamError::Malformed {
                reason: MalformedReason::UnexpectedVerb
            })
        );
    }

    /// `REASON` is i2pd's spelling of what Java I2P calls `MESSAGE`.
    #[test]
    fn stream_failures_are_read_from_either_routers_field_name() {
        for line in [
            r#"STREAM STATUS RESULT=ERROR MESSAGE="Can't reach peer""#,
            r#"STREAM STATUS RESULT=ERROR REASON="Can't reach peer""#,
        ] {
            let transition =
                classify(SamState::AwaitingStream, line).expect("a stream status parses");
            assert_eq!(
                transition,
                Transition::Stream(StreamReply::Error(StreamRejection::CantReachPeer)),
                "{line}"
            );
            assert_eq!(
                transition.next_state(SamState::AwaitingStream),
                None,
                "a failed stream does not become a raw stream"
            );
        }
    }

    /// The single most consequential transition in the crate.
    #[test]
    fn only_a_successful_stream_status_produces_a_raw_socket() {
        let ok = classify(SamState::AwaitingStream, "STREAM STATUS RESULT=OK")
            .expect("a stream status parses");
        assert_eq!(
            ok.next_state(SamState::AwaitingStream),
            Some(SamState::RawStream)
        );
        assert!(SamState::RawStream.is_raw());

        let failed = classify(
            SamState::AwaitingStream,
            "STREAM STATUS RESULT=ERROR MESSAGE=\"nope\"",
        )
        .expect("a stream status parses");
        assert_eq!(failed.next_state(SamState::AwaitingStream), None);
        assert!(!SamState::AwaitingStream.is_raw());
        assert!(!SamState::SessionActive.is_raw());
        assert!(!SamState::HelloPending.is_raw());
    }

    #[test]
    fn a_missing_result_is_a_missing_option_not_a_success() {
        assert_eq!(
            classify(SamState::AwaitingSession, "SESSION STATUS ID=abc"),
            Err(SamError::Malformed {
                reason: MalformedReason::MissingOption
            })
        );
        assert_eq!(
            classify(SamState::AwaitingStream, "STREAM STATUS STREAM=ID:7"),
            Err(SamError::Malformed {
                reason: MalformedReason::MissingOption
            })
        );
    }

    /// A successful transient session returns the private Destination. The reply type has
    /// nowhere to put it, which is the point.
    #[test]
    fn a_successful_session_reply_cannot_carry_the_private_destination() {
        let line = format!(
            "SESSION STATUS RESULT=OK ID=abc DESTINATION={}",
            "Q".repeat(600)
        );
        let transition =
            classify(SamState::AwaitingSession, &line).expect("a session status parses");
        assert_eq!(transition, Transition::Session(SessionReply::Ok));
        // `SessionReply` is `Copy` and one byte of enum: the Destination was parsed,
        // never stored, so there is nothing on this value for it to hide in.
        assert_eq!(
            std::mem::size_of::<SessionReply>(),
            1,
            "a session reply must be small enough that it cannot be holding a Destination"
        );
    }

    /// The exact bytes this profile is allowed to send.
    #[test]
    fn the_session_create_line_is_the_frozen_profile() {
        assert_eq!(
            session_create_request("0123456789abcdef0123456789abcdef"),
            "SESSION CREATE STYLE=STREAM ID=0123456789abcdef0123456789abcdef \
             DESTINATION=TRANSIENT SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4 \
             i2cp.dontPublishLeaseSet=true inbound.quantity=2 outbound.quantity=2\r\n"
                .replace(" \\\n             ", "")
        );
    }

    #[test]
    fn the_hello_line_is_the_frozen_profile() {
        assert_eq!(hello_request(), "HELLO VERSION MIN=3.1 MAX=3.1\r\n");
    }

    /// Every request line this crate builds must fit the envelope it also parses with.
    #[test]
    fn every_request_line_fits_the_line_ceiling() {
        for line in [
            hello_request(),
            session_create_request(&"f".repeat(crate::session_id::SESSION_ID_CHARS)),
            // A realistic largest Destination: the I2P transport puts a Destination well
            // under 1400 characters for every signature type.
            stream_connect_request(
                &"f".repeat(crate::session_id::SESSION_ID_CHARS),
                &"A".repeat(1400),
            )
            .to_string(),
        ] {
            assert!(
                line.len() <= MAX_REQUEST_BYTES,
                "a request line of {} bytes must fit {MAX_REQUEST_BYTES}",
                line.len()
            );
        }
    }

    /// The two ceilings are independent, and this states the gap rather than hiding it.
    ///
    /// `I2pEndpoint` accepts a Destination up to 4096 characters because that is the
    /// *endpoint* ceiling. A `STREAM CONNECT` line also carries a 33-character prefix and
    /// a 16-character suffix, so an endpoint at the endpoint ceiling does not fit a
    /// control line. Real Destinations are far shorter — every I2P signature type
    /// produces well under 1400 — so this is not reachable in practice.
    ///
    /// What matters is that the interaction is *bounded and reported*: an endpoint too
    /// long for the control line is refused, never truncated. A truncated Destination
    /// would be a silently wrong target, which is the failure this crate exists to make
    /// impossible.
    #[test]
    fn a_destination_at_the_endpoint_ceiling_does_not_fit_a_control_line() {
        let at_endpoint_ceiling = "A".repeat(i2pr_irc_core::MAX_I2P_ENDPOINT_BYTES);
        let line = stream_connect_request(&"f".repeat(32), &at_endpoint_ceiling);
        assert!(
            line.len() > MAX_REQUEST_BYTES,
            "the two ceilings genuinely differ, which is why this is documented"
        );
        // And the refusal is the client's decision to make, with the number available.
        let overhead = MAX_REQUEST_BYTES - crate::session_id::SESSION_ID_CHARS;
        assert!(
            overhead > 0 && i2pr_irc_core::MAX_I2P_ENDPOINT_BYTES > overhead,
            "a caller can see how much of a Destination the control line can carry"
        );
    }
}
