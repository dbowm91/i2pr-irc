//! An owned SAM 3.1 STREAM client foundation.
//!
//! # What this crate is
//!
//! The first production code in this repository permitted to open a TCP socket, and the
//! only one permitted to open one at all. Everything it can reach is a loopback SAM
//! bridge, and that restriction is a type rather than a check: [`SamBridgeEndpoint`] can
//! only be constructed from a loopback literal, so there is no value a future edit could
//! connect to that this crate would accept.
//!
//! # What it is not
//!
//! It is not a general SAM client. There is no `send_command`, no option passthrough, and
//! no way for a caller to add an option to a line. The three requests this crate can send
//! are [`protocol::hello_request`], [`protocol::session_create_request`], and
//! [`protocol::stream_connect_request`], each built from a literal. A caller that needs a
//! fourth has to add a fourth function here, in review, which is the point.
//!
//! # What it retains
//!
//! Nothing private. A successful `SESSION CREATE` makes the router return the transient
//! Destination it generated; the reply type has no field for it, so it is parsed into a
//! buffer that is zeroized and dropped. The session object keeps an opaque random ID, a
//! control socket, and non-secret epoch metadata.
//!
//! # Composition
//!
//! Plan 030 produces the building blocks; Plan 031 composes one long-lived session per
//! configured Network behind [`i2pr_irc_core::I2pStreamProvider`]. Nothing here knows
//! about a Network, and nothing here is allowed to: a `NetworkId` is a local scope and
//! must never reach a router.

pub mod client;
pub mod endpoint;
pub mod error;
#[cfg(any(test, feature = "testkit"))]
pub mod fake;
pub mod line;
pub mod protocol;
pub mod provider;
pub mod session_id;

pub use client::{SamClient, SamClientConfig, SamRawStream, SamTimeouts};
pub use endpoint::{DEFAULT_SAM_BRIDGE, DEFAULT_SAM_BRIDGE_PORT, SamBridgeEndpoint};
pub use error::{SamError, SamPhase, SessionRejection, StreamRejection};
pub use provider::{SAM_SCOPE_REQUEST_CAPACITY, SamDiagnostics, SamProvider};
pub use session_id::SamSessionId;
