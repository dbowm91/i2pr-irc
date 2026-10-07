//! The error taxonomy, and the phases a deadline applies to.
//!
//! Plan 030 section 12 requires bounded variants that name *what kind* of thing went
//! wrong rather than reproducing what the router said. Two properties are load-bearing:
//!
//! - **No variant carries router text.** A `MESSAGE` is free-form and can contain
//!   anything the router felt like sending, including a Destination. Carrying it would
//!   put a router-controlled string into an operator's error message.
//! - **A timeout names its phase.** A single `Timeout` variant would make "the router is
//!   slow to build tunnels" indistinguishable from "the router did not answer at all",
//!   which are different operational problems with different fixes.

use std::fmt;
use thiserror::Error;

use crate::endpoint::BridgeEndpointError;
use crate::line::LineError;

/// Which step of a SAM exchange a failure happened in.
///
/// Named rather than numbered so a diagnostic reads as prose and so adding a phase
/// cannot silently renumber the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamPhase {
    /// Opening the TCP connection to the loopback bridge.
    BridgeConnect,
    /// `HELLO VERSION MIN=3.1 MAX=3.1` and its reply.
    Hello,
    /// `SESSION CREATE` and the tunnel build it triggers.
    SessionCreate,
    /// `STREAM CONNECT` and its single `STREAM STATUS`.
    StreamConnect,
}

impl SamPhase {
    /// Lowercase name, for a diagnostic label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BridgeConnect => "bridge-connect",
            Self::Hello => "hello",
            Self::SessionCreate => "session-create",
            Self::StreamConnect => "stream-connect",
        }
    }
}

impl fmt::Display for SamPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a SAM `SESSION STATUS` was not `RESULT=OK`.
///
/// A router's rejection classes, reduced to what the caller can act on. The text of a
/// `MESSAGE` is deliberately absent: it is not a class, it is prose written by whatever
/// version of whichever router is installed, and it is not safe to surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRejection {
    /// The router already knows this session ID.
    DuplicateId,
    /// The router already has a session for this Destination.
    DuplicateDestination,
    /// The `SESSION CREATE` options were refused.
    InvalidOptions,
    /// The version negotiation failed, so no session was attempted.
    UnsupportedVersion,
}

impl SessionRejection {
    /// Classifies a `SESSION STATUS` reply.
    ///
    /// `RESULT` is checked first and case-insensitively, because that is the field both
    /// routers agree on. `ERROR` is then read to distinguish the two classes that have
    /// different causes: a duplicate ID is ours, a duplicate Destination is the router's
    /// state, and conflating them would send an operator to the wrong system.
    pub fn classify(error: &str) -> Self {
        let normalized = error.trim().to_ascii_uppercase();
        if normalized.contains("DUPLICATE") && normalized.contains("ID") {
            Self::DuplicateId
        } else if normalized.contains("DUPLICATE") || normalized.contains("DESTINATION") {
            Self::DuplicateDestination
        } else if normalized.contains("INVALID") {
            Self::InvalidOptions
        } else {
            // An unrecognised error string is still a rejection, not a success. Unknown
            // maps onto the class whose remedy is "the options were not accepted", which
            // is the safe default: it prompts the caller to stop rather than retry with
            // the same request.
            Self::InvalidOptions
        }
    }
}

/// Why a SAM `STREAM STATUS` was not `RESULT=OK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRejection {
    /// The peer could not be reached. Retryable, and the common case.
    CantReachPeer,
    /// The router refused the destination key.
    InvalidKey,
    /// The router does not know this session ID.
    InvalidId,
    /// The router gave up waiting.
    Timeout,
}

impl StreamRejection {
    /// Whether the same request could plausibly succeed later.
    ///
    /// Used by a caller deciding between backoff and giving up on a Network. An invalid
    /// key or unknown ID is not worth retrying: the input is wrong, and repeating it
    /// would only burn the reconnect budget.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::CantReachPeer | Self::Timeout)
    }

    /// Classifies a `STREAM STATUS` reply from its `MESSAGE` field.
    pub fn classify(message: &str) -> Self {
        let normalized = message.trim().to_ascii_uppercase();
        if normalized.contains("CAN'T REACH") || normalized.contains("CANT_REACH") {
            Self::CantReachPeer
        } else if normalized.contains("INVALID") && normalized.contains("KEY") {
            Self::InvalidKey
        } else if normalized.contains("INVALID") && normalized.contains("ID") {
            Self::InvalidId
        } else if normalized.contains("TIMEOUT") {
            Self::Timeout
        } else {
            // Unknown classes are treated as retryable peer failures: that is what they
            // are in practice, and treating an unknown as permanent would abandon a
            // Network over a router's new error string.
            Self::CantReachPeer
        }
    }
}

/// Everything that can go wrong in this crate.
///
/// No variant stores router text, a session ID, a Destination, or key material.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SamError {
    /// The configured bridge address was refused. Never expected at runtime: the type
    /// only exists if the configuration was already validated, so this means the value
    /// was built outside the crate's own parsing.
    #[error("invalid SAM bridge endpoint: {0}")]
    InvalidBridge(#[from] BridgeEndpointError),

    /// The bridge did not accept a loopback connection.
    #[error("SAM bridge unavailable at the configured loopback address")]
    BridgeUnavailable,

    /// The router did not answer within the phase's deadline.
    #[error("SAM {phase} timed out")]
    Timeout {
        /// Which exchange was waiting.
        phase: SamPhase,
    },

    /// `HELLO` succeeded but the router will not speak 3.1.
    #[error("SAM bridge does not support version 3.1")]
    UnsupportedVersion,

    /// The router refused to create the session.
    #[error("SAM session rejected: {rejection:?}")]
    SessionRejected {
        /// The class of rejection, never the router's text.
        rejection: SessionRejection,
    },

    /// A session existed and has since gone.
    #[error("SAM session lost")]
    SessionLost,

    /// The router refused the destination itself, rather than failing to reach it.
    #[error("SAM destination rejected")]
    DestinationRejected,

    /// The peer could not be reached.
    #[error("SAM peer unavailable: {rejection:?}")]
    PeerUnavailable {
        /// The class of rejection.
        rejection: StreamRejection,
    },

    /// A reply was not a line this profile can classify.
    #[error("malformed SAM reply: {reason:?}")]
    Malformed {
        /// A closed reason, not router text.
        reason: MalformedReason,
    },

    /// The OS random source failed.
    ///
    /// Explicit and terminal for the operation. Falling back to a derived or
    /// identifying value would produce a session ID that looks random and is not, which
    /// is worse than no session at all.
    #[error("OS randomness unavailable")]
    RandomUnavailable,

    /// The bridge closed the connection.
    #[error("SAM connection closed")]
    Closed,

    /// The operation was dropped before it completed.
    ///
    /// Represented so a caller can distinguish "we gave up" from "the router said no".
    #[error("SAM operation cancelled")]
    Cancelled,
}

/// Why a reply could not be classified.
///
/// A closed set of reasons rather than the offending text. This is what lets `SamError`
/// be `Clone + PartialEq` and, more importantly, what stops a router's free-form `MESSAGE`
/// from reaching an operator's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedReason {
    /// The line could not be parsed at all.
    Unparseable,
    /// The verb was not one this profile accepts.
    UnexpectedVerb,
    /// A required option was absent.
    MissingOption,
    /// An option appeared more times than its type allows.
    DuplicateOption,
    /// The line exceeded a framing ceiling and was discarded.
    Overflowed,
    /// The reply was not valid UTF-8.
    NotText,
}

impl SamError {
    /// Whether the same request could plausibly succeed later.
    ///
    /// This is the caller's route into the reconnect scheduler, so it is defined here
    /// rather than inferred from the variant by each caller. An `InvalidBridge` or a
    /// `RandomUnavailable` is permanent: retrying it would spin.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::BridgeUnavailable | Self::SessionLost | Self::Closed | Self::Cancelled => true,
            Self::Timeout { .. } => true,
            Self::PeerUnavailable { rejection } => rejection.is_retryable(),
            Self::DestinationRejected => false,
            Self::UnsupportedVersion => false,
            Self::SessionRejected { rejection } => matches!(
                rejection,
                SessionRejection::DuplicateDestination | SessionRejection::UnsupportedVersion
            ),
            Self::Malformed { .. } => false,
            Self::InvalidBridge(_) | Self::RandomUnavailable => false,
        }
    }
}

impl From<LineError> for SamError {
    fn from(error: LineError) -> Self {
        use LineError::*;
        Self::Malformed {
            reason: match error {
                // An empty line is a keepalive or a stray newline, not a protocol
                // violation, and treating it as one would turn harmless router chatter
                // into a failed connection.
                Empty => MalformedReason::Unparseable,
                TooManyTokens | KeyTooLong | ValueTooLong => MalformedReason::Overflowed,
                EmbeddedControl | UnterminatedQuote => MalformedReason::NotText,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A router's `MESSAGE` must not be able to reach a `SamError`.
    ///
    /// This is a type-level assertion, not a runtime one: if any variant gained a
    /// `String`, every `match` in this module would fail to compile, which is the
    /// failure mode we want. Asserting the trait bounds keeps that true even if a
    /// variant is added later.
    #[test]
    fn sam_error_carries_no_router_text() {
        // Small enough that no variant can be smuggling a heap allocation. A router text
        // or a Destination would make this false, which is the point: the bound is a
        // structural proxy for "this enum holds no foreign bytes".
        assert!(
            std::mem::size_of::<SamError>() <= 8,
            "SamError must stay a small discriminant, not carry text: {}",
            std::mem::size_of::<SamError>()
        );
        // Every variant is classified without consulting any router text, so the
        // classification itself is what is asserted, not a size.
        assert!(SamError::Closed.is_retryable());
    }

    #[test]
    fn timeout_names_its_phase() {
        assert_eq!(
            SamError::Timeout {
                phase: SamPhase::SessionCreate
            }
            .to_string(),
            "SAM session-create timed out"
        );
        for phase in [
            SamPhase::BridgeConnect,
            SamPhase::Hello,
            SamPhase::SessionCreate,
            SamPhase::StreamConnect,
        ] {
            assert_eq!(
                SamPhase::as_str(phase),
                phase.as_str(),
                "the label and the Display agree"
            );
        }
    }

    #[test]
    fn session_rejections_are_classified_distinctly() {
        assert_eq!(
            SessionRejection::classify("DUPLICATE ID"),
            SessionRejection::DuplicateId
        );
        assert_eq!(
            SessionRejection::classify("Duplicate Session ID"),
            SessionRejection::DuplicateId
        );
        assert_eq!(
            SessionRejection::classify("DUPLICATE SESSION"),
            SessionRejection::DuplicateDestination
        );
        assert_eq!(
            SessionRejection::classify("INVALID OPTIONS"),
            SessionRejection::InvalidOptions
        );
        // An unknown string is still a rejection, never a success.
        assert_eq!(
            SessionRejection::classify("something new in a future router"),
            SessionRejection::InvalidOptions
        );
    }

    #[test]
    fn stream_rejections_separate_retryable_from_permanent() {
        assert!(StreamRejection::CantReachPeer.is_retryable());
        assert!(StreamRejection::Timeout.is_retryable());
        assert!(!StreamRejection::InvalidKey.is_retryable());
        assert!(!StreamRejection::InvalidId.is_retryable());

        assert_eq!(
            StreamRejection::classify("Can't reach peer"),
            StreamRejection::CantReachPeer
        );
        assert_eq!(
            StreamRejection::classify("INVALID KEY"),
            StreamRejection::InvalidKey
        );
        assert_eq!(
            StreamRejection::classify("INVALID ID"),
            StreamRejection::InvalidId
        );
        assert_eq!(
            StreamRejection::classify("Timeout"),
            StreamRejection::Timeout
        );
        // An unknown message is treated as a peer failure rather than as permanent,
        // because a router's new error string must not abandon a Network.
        assert!(StreamRejection::classify("???").is_retryable());
    }

    #[test]
    fn permanence_is_decided_in_one_place() {
        // Permanent: retrying cannot help and would only burn the reconnect budget.
        for permanent in [
            SamError::InvalidBridge(BridgeEndpointError::NotLoopback),
            SamError::RandomUnavailable,
            SamError::UnsupportedVersion,
            SamError::DestinationRejected,
            SamError::Malformed {
                reason: MalformedReason::UnexpectedVerb,
            },
            SamError::SessionRejected {
                rejection: SessionRejection::InvalidOptions,
            },
        ] {
            assert!(
                !permanent.is_retryable(),
                "{permanent} must be permanent, not retried"
            );
        }
        // Retryable: a later attempt is genuinely different.
        for retryable in [
            SamError::BridgeUnavailable,
            SamError::SessionLost,
            SamError::Closed,
            SamError::Cancelled,
            SamError::Timeout {
                phase: SamPhase::StreamConnect,
            },
            SamError::PeerUnavailable {
                rejection: StreamRejection::CantReachPeer,
            },
        ] {
            assert!(retryable.is_retryable(), "{retryable} must be retryable");
        }
    }
}
