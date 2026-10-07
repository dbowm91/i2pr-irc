//! Corrective 033: the inbound `STREAM ACCEPT` topology and the raw transition after it.
//!
//! Plan 032 measured "no application bytes traversed" from a harness whose peer issued
//! `STREAM ACCEPT` on the `SESSION CREATE` control socket. That socket has nowhere to
//! deliver an inbound connection, so nothing traversed, and the finding was an artefact of
//! the harness rather than a property of the router or of the client.
//!
//! These tests pin the corrected shape with no router at all:
//!
//! - the peer creates its session on a control socket and arms `STREAM ACCEPT` on a
//!   **separate** socket, which is the topology SAM 3.1 requires;
//! - the production [`SamProvider`] connects into that armed accept;
//! - the accept socket carries the peer-Destination line and then raw application bytes,
//!   and bytes coalesced with that line are not lost;
//! - both directions carry an exact binary fixture, and a second stream reuses the same
//!   session;
//! - each failure mode of the transition fails closed with a specific reason rather than
//!   hanging or silently truncating.

#![cfg(test)]

use std::time::Duration;

use i2pr_irc_core::{I2pEndpoint, I2pStreamProvider, NetworkId};
use i2pr_irc_sam::client::{SamClientConfig, SamTimeouts};
use i2pr_irc_sam::endpoint::SamBridgeEndpoint;
use i2pr_irc_sam::fake::{
    AcceptConfig, AcceptError, FakeSamPeer, MAX_ACCEPT_DESTINATION_LINE, Prelude,
};
use i2pr_irc_sam::provider::SamProvider;
use i2pr_irc_sam::session_id::os_random;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A fixed test-only scope. A `NetworkId` is local process state and never reaches a
/// router, so a constant here is a scope label and not an identifier.
const NETWORK: NetworkId = NetworkId(1);

const PATIENCE: Duration = Duration::from_secs(30);

/// A binary fixture that no line-oriented assumption survives.
///
/// NUL, an invalid UTF-8 byte, a bare CRLF, and bytes that spell a SAM status line. If any
/// layer is doing the wrong thing with framing, text, or the raw transition, this fixture
/// finds it.
fn fixture(seed: u8) -> Vec<u8> {
    vec![0x00, 0xFF, 0xFE, b'S', seed, b'\r', b'\n', b'\n', 0x01]
}

fn provider_for(endpoint: SamBridgeEndpoint) -> SamProvider {
    SamProvider::with_config(SamClientConfig {
        bridge: endpoint,
        // Short deadlines: every failure mode here is a local fixture behaving as
        // designed, so waiting the production ten seconds only makes a failure slower to
        // observe without making it more informative.
        timeouts: SamTimeouts {
            bridge_connect: Duration::from_secs(10),
            hello: Duration::from_secs(10),
            session_create: Duration::from_secs(20),
            stream_connect: Duration::from_secs(30),
        },
        random: os_random(),
    })
}

/// One stream, bounded so a stuck exchange fails the test instead of hanging it.
async fn connect_within(
    provider: &SamProvider,
    destination: &I2pEndpoint,
) -> Result<Box<dyn i2pr_irc_core::ByteStream>, String> {
    tokio::time::timeout(PATIENCE, provider.connect(NETWORK, destination))
        .await
        .map_err(|_| "the connect exceeded its budget".to_string())?
        .map_err(|error| error.to_string())
}

#[tokio::test]
async fn an_inbound_accept_on_its_own_socket_receives_the_providers_stream() {
    let peer = FakeSamPeer::start(AcceptConfig::default()).await;

    // The session lives on its own control socket, which the test holds for the whole run.
    // An accept issued here would have nowhere to deliver a connection.
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");

    // The accept is armed on a *second* socket, as SAM 3.1 requires.
    let armed = peer
        .open_accept("peer")
        .await
        .expect("the accept is armed and acknowledged");
    assert_eq!(peer.armed(), 1, "exactly one accept is waiting");

    let provider = provider_for(peer.endpoint());
    let mut stream = connect_within(&provider, &destination)
        .await
        .expect("the production provider connects");

    // The inbound side now reads the accept socket: status, Destination line, raw bytes.
    let (mut inbound, destination_line) = armed
        .incoming()
        .await
        .expect("the accept completes its raw transition");
    let destination_line = String::from_utf8(destination_line).expect("the line is ascii");
    assert!(
        destination_line.len() >= 516,
        "the peer-Destination line is a Destination, not a control line: {} characters",
        destination_line.len()
    );

    let forward = fixture(0x11);
    let reply = fixture(0x22);

    stream
        .write_all(&forward)
        .await
        .expect("the provider writes its payload");
    let received = inbound
        .read_exact(forward.len())
        .await
        .expect("the accept socket receives the payload");
    assert_eq!(
        received, forward,
        "the bytes that arrived are the bytes that were sent, including the ones \
         coalesced with the Destination line"
    );

    inbound
        .write_all(&reply)
        .await
        .expect("the peer writes its reply");
    let mut got = vec![0u8; reply.len()];
    tokio::time::timeout(PATIENCE, stream.read_exact(&mut got))
        .await
        .expect("the reply arrives within its budget")
        .expect("the provider reads its reply");
    assert_eq!(
        got, reply,
        "the reply crosses unchanged in the other direction"
    );
}

#[tokio::test]
async fn bytes_coalesced_with_the_destination_line_are_not_lost() {
    // The prelude and the first application bytes are written in one segment, so a reader
    // that discards whatever it read past the newline loses the payload. This is the
    // specific hazard the raw transition carries.
    let peer = FakeSamPeer::start(AcceptConfig {
        prelude: Prelude::Destination {
            destination: vec![b'Q'; 600],
            payload: b"coalesced".to_vec(),
        },
        ..AcceptConfig::default()
    })
    .await;
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");
    let armed = peer
        .open_accept("peer")
        .await
        .expect("the accept is armed and acknowledged");

    let provider = provider_for(peer.endpoint());
    let stream = connect_within(&provider, &destination)
        .await
        .expect("the production provider connects");

    let (mut inbound, _line) = armed
        .incoming()
        .await
        .expect("the accept completes its raw transition");
    let coalesced = inbound
        .read_exact(b"coalesced".len())
        .await
        .expect("the coalesced payload survives the line split");
    assert_eq!(
        coalesced, b"coalesced",
        "bytes that shared a segment with the Destination line are still application bytes"
    );
    drop(stream);
}

#[tokio::test]
async fn one_session_serves_a_second_stream_without_recreating_it() {
    let peer = FakeSamPeer::start(AcceptConfig::default()).await;
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");
    let provider = provider_for(peer.endpoint());

    for (index, seed) in [0x41u8, 0x42u8].into_iter().enumerate() {
        let armed = peer
            .open_accept("peer")
            .await
            .expect("each stream gets its own accept");
        let mut stream = connect_within(&provider, &destination)
            .await
            .unwrap_or_else(|error| panic!("stream {index} connects: {error}"));
        let (mut inbound, _line) = armed
            .incoming()
            .await
            .expect("the accept completes its raw transition");
        let forward = fixture(seed);
        stream
            .write_all(&forward)
            .await
            .expect("the provider writes its payload");
        assert_eq!(
            inbound
                .read_exact(forward.len())
                .await
                .expect("the payload arrives"),
            forward,
            "stream {index} carries its own exact bytes"
        );
    }

    let diagnostics = provider.diagnostics();
    assert_eq!(
        diagnostics.session_creations, 1,
        "two streams over one provider create one session, not two"
    );
    assert_eq!(diagnostics.stream_successes, 2, "both streams succeeded");
}

#[tokio::test]
async fn releasing_the_network_scope_leaves_no_live_scope() {
    let peer = FakeSamPeer::start(AcceptConfig::default()).await;
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");
    let provider = provider_for(peer.endpoint());

    let armed = peer
        .open_accept("peer")
        .await
        .expect("the accept is armed and acknowledged");
    let stream = connect_within(&provider, &destination)
        .await
        .expect("the production provider connects");
    let (_inbound, _line) = armed
        .incoming()
        .await
        .expect("the accept completes its raw transition");
    drop(stream);

    provider.release(NETWORK).await.expect("the scope releases");
    let diagnostics = provider.diagnostics();
    assert_eq!(
        diagnostics.live_scopes, 0,
        "a released Network leaves nothing behind for a live owner to keep alive"
    );
    assert_eq!(diagnostics.releases, 1, "release is explicit and counted");
}

#[tokio::test]
async fn a_refused_accept_status_is_not_treated_as_an_accept() {
    // The bridge answers `STREAM ACCEPT` with a non-OK status. A peer that ignored the
    // status would sit on a socket the router will never use and time out later, which
    // reads as a network problem rather than as a refusal.
    let peer = FakeSamPeer::start(AcceptConfig {
        accept_status: Some(i2pr_irc_sam::fake::line("STREAM STATUS RESULT=I2P_ERROR")),
        ..AcceptConfig::default()
    })
    .await;
    assert_eq!(
        peer.open_accept("peer").await.err(),
        Some(AcceptError::Refused),
        "an accept that was not acknowledged RESULT=OK is refused here, not half-open"
    );
}

#[tokio::test]
async fn a_missing_destination_line_times_out_boundedly() {
    // No Destination line before the raw bytes. Treating that as "no destination, carry on"
    // would put a control line where a Destination belongs, which is the bug class this
    // whole corrective exists to remove.
    let peer = FakeSamPeer::start(AcceptConfig {
        prelude: Prelude::Missing,
        ..AcceptConfig::default()
    })
    .await;
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");
    let armed = peer
        .open_accept("peer")
        .await
        .expect("the accept is armed and acknowledged");
    let provider = provider_for(peer.endpoint());
    let stream = connect_within(&provider, &destination)
        .await
        .expect("the production provider connects");

    assert_eq!(
        armed.incoming().await.err(),
        Some(AcceptError::TimedOut),
        "a prelude with no terminated Destination line ends in a bounded wait rather than \
         an unbounded read or a silent pass"
    );
    drop(stream);
}

#[tokio::test]
async fn an_oversized_destination_line_is_refused_at_the_ceiling() {
    let peer = FakeSamPeer::start(AcceptConfig {
        prelude: Prelude::Oversized,
        ..AcceptConfig::default()
    })
    .await;
    let (_control, destination) = peer
        .create_session()
        .await
        .expect("the peer creates a session");
    let destination = I2pEndpoint::parse(std::str::from_utf8(&destination).expect("base64 ascii"))
        .expect("the bridge mints a well-formed destination");
    let armed = peer
        .open_accept("peer")
        .await
        .expect("the accept is armed and acknowledged");
    let provider = provider_for(peer.endpoint());
    let stream = connect_within(&provider, &destination)
        .await
        .expect("the production provider connects");

    assert_eq!(
        armed.incoming().await.err(),
        Some(AcceptError::PreludeTooLong),
        "a line past the {MAX_ACCEPT_DESTINATION_LINE} byte ceiling is refused rather than buffered"
    );
    drop(stream);
}

#[tokio::test]
async fn the_bridge_endpoint_is_refused_for_a_host_name() {
    // The qualification tool must not hold a weaker authority than the code it
    // qualifies. Both refuse a name, for the same reason: resolving one would let a hosts
    // file decide which router is reached.
    for name in ["localhost:7656", "router.local:7656", "127.0.0.1:0"] {
        assert!(
            SamBridgeEndpoint::parse(name).is_err(),
            "{name:?} must not resolve to a bridge endpoint"
        );
    }
    assert!(
        SamBridgeEndpoint::parse("127.0.0.1:7656").is_ok(),
        "a numeric loopback literal is the only accepted form"
    );
}
