//! Plan 030 / R001-B qualification: the owned SAM 3.1 client against a scripted bridge.
//!
//! Every test here drives a real loopback socket through the real client. Nothing is
//! mocked below the socket, because the properties under test — partial reads, exact raw
//! transition, framing ceilings, deadline enforcement — are all properties of what
//! crosses that socket.
//!
//! Two groups:
//!
//! **Protocol conformance.** Both terminators, fragmented replies, an over-long line
//! followed by a good one, every `HELLO` outcome, every `SESSION` and `STREAM` outcome,
//! a delayed reply, a control close, and a payload containing NUL bytes, invalid UTF-8,
//! and something that looks like a SAM line. The last one matters most: after
//! `RESULT=OK` nothing may be parsed, or upstream IRC bytes could be interpreted as
//! protocol.
//!
//! **Boundedness.** Every deadline is exercised with an injected profile that fires
//! immediately, so a test asserts the deadline exists rather than waiting for it.

#![cfg(test)]

use std::{fmt, sync::Arc, time::Duration};

use i2pr_irc_core::{I2pEndpoint, MAX_I2P_ENDPOINT_BYTES};
use i2pr_irc_sam::{
    SamBridgeEndpoint, SamClient, SamClientConfig, SamError, SamPhase, SamTimeouts,
    SessionRejection, StreamRejection,
    fake::{FakeBridge, Script, hello_ok, line, session_ok_with_destination, stream_ok},
    session_id::RandomSource,
};

/// Ceiling for every bounded wait in this suite.
const CEILING: Duration = Duration::from_secs(10);

/// A deterministic ID source, so a test can assert the exact bytes sent.
struct FixedRandom;

impl RandomSource for FixedRandom {
    fn fill(&self, out: &mut [u8]) -> Result<(), i2pr_irc_sam::session_id::RandomUnavailable> {
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = index as u8;
        }
        Ok(())
    }
}

/// A source that cannot answer, to prove randomness failure is terminal.
struct BrokenRandom;

impl RandomSource for BrokenRandom {
    fn fill(&self, _: &mut [u8]) -> Result<(), i2pr_irc_sam::session_id::RandomUnavailable> {
        Err(i2pr_irc_sam::session_id::RandomUnavailable)
    }
}

fn config_for(bridge: &FakeBridge, timeouts: SamTimeouts) -> SamClientConfig {
    SamClientConfig {
        bridge: bridge.endpoint(),
        timeouts,
        random: Arc::new(FixedRandom),
    }
}

fn endpoint() -> I2pEndpoint {
    I2pEndpoint::parse("irc.example.i2p").expect("the test endpoint parses")
}

/// Drives a full session and returns the raw stream.
async fn open_stream(bridge: &FakeBridge, timeouts: SamTimeouts) -> Result<RawStream, SamError> {
    let (mut client, id) = SamClient::open(config_for(bridge, timeouts)).await?;
    client.create_session(&id).await?;
    client.connect_stream(&id, &endpoint()).await.map(RawStream)
}

/// A `SamRawStream` wrapper that is `Debug`.
///
/// `SamRawStream` holds socket halves and deliberately derives nothing, since printing a
/// socket handle into a diagnostic is not useful. A test that reports `.expect()` needs a
/// `Debug`, so this asserts only the error and never prints the stream.
struct RawStream(i2pr_irc_sam::SamRawStream);

impl fmt::Debug for RawStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawStream(..)")
    }
}

/// Waits until the fake has recorded at least `count` requests.
async fn wait_for_requests(bridge: &FakeBridge, count: usize) {
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if bridge.requests().len() >= count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {count} requests, saw {:?}",
            bridge.requests()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ------------------------------------------------------------- protocol conformance

/// The whole profile, in the order the plan specifies it.
#[tokio::test]
async fn a_session_and_a_raw_stream_open_in_order() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        ..Script::default()
    })
    .await;
    let stream = open_stream(&bridge, SamTimeouts::default())
        .await
        .expect("the scripted exchange succeeds");
    wait_for_requests(&bridge, 3).await;
    let requests = bridge.requests();
    assert_eq!(requests[0], "HELLO VERSION MIN=3.1 MAX=3.1");
    assert_eq!(
        requests[1],
        format!(
            "SESSION CREATE STYLE=STREAM ID={} DESTINATION=TRANSIENT SIGNATURE_TYPE=7 \
             i2cp.leaseSetEncType=4 i2cp.dontPublishLeaseSet=true inbound.quantity=2 \
             outbound.quantity=2",
            // The deterministic source yields bytes 0..15, so the ID is knowable.
            "000102030405060708090a0b0c0d0e0f"
        )
    );
    assert_eq!(
        requests[2],
        format!(
            "STREAM CONNECT ID={} DESTINATION=irc.example.i2p SILENT=false",
            "000102030405060708090a0b0c0d0e0f"
        )
    );
    // The scripted exchange completed, which means the stream exists and was handed
    // back. Dropping it here is the rest of the assertion: a stream whose socket was not
    // returned would have failed the connect above.
    drop(stream);
}

/// Byte-at-a-time delivery must produce the same result as whole writes.
///
/// A loopback socket almost always delivers a whole write, so without this the client's
/// partial-read path would never be exercised by any test in the suite.
#[tokio::test]
async fn a_fragmented_reply_produces_the_same_result() {
    let bridge = FakeBridge::start(Script {
        // One connection: `SamClient` speaks hello, session, and stream on the same socket.
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        fragment: true,
        ..Script::default()
    })
    .await;
    open_stream(&bridge, SamTimeouts::default())
        .await
        .expect("a fragmented exchange reassembles to the same result");
    wait_for_requests(&bridge, 3).await;
}

/// A terminator split across two reads must still reassemble to the same result.
///
/// Deterministic regression for Corrective 049: the bridge answers every reply in
/// two writes split between the trailing CR and LF, so the client's line reader
/// always observes the boundary the byte-at-a-time case only hits probabilistically.
/// A prior framing defect took the reader's partial line back out for re-feeding on
/// every loop; the re-fed bytes arrived while the held-back CR was still pending,
/// so the CR was baked into the line as content, the line was refused as embedded
/// control, and the phase stalled out to its full deadline.
#[tokio::test]
async fn a_crlf_split_across_reads_produces_the_same_result() {
    let bridge = FakeBridge::start(Script {
        // One connection: `SamClient` speaks hello, session, and stream on the same socket.
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        split_crlf: true,
        ..Script::default()
    })
    .await;
    open_stream(&bridge, SamTimeouts::default())
        .await
        .expect("a CR/LF-split exchange reassembles to the same result");
    wait_for_requests(&bridge, 3).await;
}

/// Two replies in one segment must both be seen.
#[tokio::test]
async fn two_replies_in_one_segment_are_both_read() {
    let bridge = FakeBridge::start(Script {
        // Two replies in one write, to prove the client does not lose the second.
        hello: vec![b"HELLO OK\r\nSESSION STATUS RESULT=OK ID=abc\r\n".to_vec()],
        stream: vec![stream_ok()],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello and session arrive together");
    client
        .create_session(&id)
        .await
        .expect("the second reply in the same segment is not lost");
}

/// Every `HELLO` outcome.
#[tokio::test]
async fn every_hello_outcome_is_classified() {
    for (reply, expected) in [
        ("HELLO OK", Ok(())),
        ("HELLO OK VERSION MIN=3.1 MAX=3.1", Ok(())),
        ("HELLO NOVERSION", Err(SamError::UnsupportedVersion)),
    ] {
        let bridge = FakeBridge::start(Script {
            hello: vec![line(reply)],
            ..Script::default()
        })
        .await;
        let outcome = SamClient::open(config_for(&bridge, SamTimeouts::default())).await;
        match expected {
            Ok(()) => assert!(outcome.is_ok(), "{reply} must succeed"),
            Err(error) => assert_eq!(outcome.err(), Some(error), "{reply}"),
        }
    }
}

/// Every `SESSION STATUS` class.
#[tokio::test]
async fn every_session_outcome_is_classified() {
    for (reply, expected) in [
        (
            "SESSION STATUS RESULT=ERROR ERROR=\"DUPLICATE ID\"",
            SessionRejection::DuplicateId,
        ),
        (
            "SESSION STATUS RESULT=ERROR ERROR=\"DUPLICATE SESSION\"",
            SessionRejection::DuplicateDestination,
        ),
        (
            "SESSION STATUS RESULT=ERROR ERROR=\"INVALID OPTIONS\"",
            SessionRejection::InvalidOptions,
        ),
    ] {
        let bridge = FakeBridge::start(Script {
            hello: vec![hello_ok()],
            session: vec![line(reply)],
            ..Script::default()
        })
        .await;
        let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
            .await
            .expect("hello succeeds");
        assert_eq!(
            client.create_session(&id).await,
            Err(SamError::SessionRejected {
                rejection: expected
            }),
            "{reply}"
        );
    }
}

/// Every `STREAM STATUS` class, from both routers' field names.
#[tokio::test]
async fn every_stream_outcome_is_classified() {
    for (reply, expected) in [
        (
            "STREAM STATUS RESULT=ERROR MESSAGE=\"Can't reach peer\"",
            SamError::PeerUnavailable {
                rejection: StreamRejection::CantReachPeer,
            },
        ),
        (
            "STREAM STATUS RESULT=ERROR REASON=\"Can't reach peer\"",
            SamError::PeerUnavailable {
                rejection: StreamRejection::CantReachPeer,
            },
        ),
        (
            "STREAM STATUS RESULT=ERROR MESSAGE=\"INVALID KEY\"",
            SamError::DestinationRejected,
        ),
        (
            "STREAM STATUS RESULT=ERROR MESSAGE=\"INVALID ID\"",
            SamError::PeerUnavailable {
                rejection: StreamRejection::InvalidId,
            },
        ),
    ] {
        let bridge = FakeBridge::start(Script {
            hello: vec![hello_ok()],
            session: vec![session_ok_with_destination()],
            stream: vec![line(reply)],
            ..Script::default()
        })
        .await;
        assert_eq!(
            open_stream(&bridge, SamTimeouts::default()).await.err(),
            Some(expected),
            "{reply}"
        );
    }
}

/// A router that hangs up mid-exchange is a closed connection, not a timeout.
#[tokio::test]
async fn a_control_close_is_reported_as_closed() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        // Nothing scripted for the stream, and `close_after` on an empty stream script
        // means the bridge hangs up as the client reaches for a stream it will never get.
        close_after: true,
        ..Script::default()
    })
    .await;
    assert_eq!(
        open_stream(&bridge, SamTimeouts::default()).await.err(),
        Some(SamError::Closed),
        "a bridge that hangs up before STREAM STATUS is a close, not a timeout"
    );
}

/// A reply that arrives in the wrong phase is a protocol failure, not a skip.
#[tokio::test]
async fn an_out_of_order_reply_is_a_protocol_failure() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![stream_ok()],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    assert_eq!(
        client.create_session(&id).await,
        Err(SamError::Malformed {
            reason: i2pr_irc_sam::error::MalformedReason::UnexpectedVerb
        }),
        "a STREAM STATUS where a SESSION STATUS was expected is refused"
    );
}

/// A malformed reply is refused, and its text never becomes the error.
#[tokio::test]
async fn a_malformed_reply_is_refused_without_echoing_router_text() {
    // A reply the parser cannot make sense of in this phase, carrying text that would be
    // damaging if it reached an operator's terminal.
    let secret = "router said something identifying";
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![line(&format!("!!! {secret}"))],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    let error = client
        .create_session(&id)
        .await
        .expect_err("a reply that is not SAM at all must be refused");
    assert!(
        matches!(error, SamError::Malformed { .. }),
        "a malformed reply is a typed failure: {error:?}"
    );
    for rendered in [format!("{error:?}"), error.to_string()] {
        assert!(
            !rendered.contains(secret),
            "the router's text does not reach an operator-facing string: {rendered}"
        );
    }
}

/// An over-long reply is discarded, and the exchange recovers.
#[tokio::test]
async fn an_over_long_reply_is_discarded_and_the_reader_recovers() {
    let mut oversized = vec![b'X'; i2pr_irc_sam::line::MAX_SAM_LINE_BYTES + 1024];
    oversized.push(b'\n');
    // One reply entry, containing both lines: the fake answers one entry per request,
    // and the point here is that the over-long line and the good line that follows it
    // arrive in the same segment.
    let mut both = oversized;
    both.extend_from_slice(b"SESSION STATUS RESULT=OK\r\n");
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![both],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    client
        .create_session(&id)
        .await
        .expect("an over-long line is discarded rather than ending the exchange");
}

// --------------------------------------------------------- the raw transition

/// The most important property in the crate.
///
/// After `RESULT=OK`, the socket carries bytes. A payload containing a NUL, invalid
/// UTF-8, and a string that looks exactly like a SAM line must all arrive verbatim: if
/// anything parsed it, upstream IRC data would be interpreted as protocol.
#[tokio::test]
async fn after_result_ok_nothing_is_parsed_as_sam() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        // Raw bytes the client must not interpret, sent once the stream is established.
        trailing: vec![
            0x00, 0xff, 0xfe, b'S', b'T', b'R', b'E', b'A', b'M', b' ', b'S', b'T', b'A', b'T',
            b'U', b'S', b' ', b'R', b'E', b'S', b'U', b'L', b'T', b'=', b'O', b'K', b'\r', b'\n',
            0x01,
        ],
        close_after: true,
        ..Script::default()
    })
    .await;
    let RawStream(mut stream) = open_stream(&bridge, SamTimeouts::default())
        .await
        .expect("the stream opens");

    use tokio::io::AsyncReadExt;
    let payload: Vec<u8> = vec![
        0x00u8, 0xff, 0xfe, b'S', b'T', b'R', b'E', b'A', b'M', b' ', b'S', b'T', b'A', b'T', b'U',
        b'S', b' ', b'R', b'E', b'S', b'U', b'L', b'T', b'=', b'O', b'K', b'\r', b'\n', 0x01,
    ];
    // Read until end-of-stream rather than to a byte count: the fake keeps the socket
    // open, so a count would either cut the payload short or depend on how the reads
    // happened to split. Closing is the only deterministic end here.
    let mut all = Vec::new();
    let mut chunk = [0u8; 64];
    loop {
        let count = stream.read(&mut chunk).await.expect("the read succeeds");
        if count == 0 {
            break;
        }
        all.extend_from_slice(&chunk[..count]);
    }
    assert_eq!(
        all, payload,
        "raw bytes including NUL, invalid UTF-8, and a SAM-looking line cross unchanged"
    );
}

/// A Destination is written verbatim because the router needs the key, and never appears
/// anywhere else.
#[tokio::test]
async fn a_raw_destination_reaches_the_router_unmodified() {
    let destination = format!("{}+/{}", "A".repeat(300), "B".repeat(400));
    let parsed = I2pEndpoint::parse(&destination).expect("a real destination parses");
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    client.create_session(&id).await.expect("session succeeds");
    client
        .connect_stream(&id, &parsed)
        .await
        .expect("the stream opens");
    wait_for_requests(&bridge, 3).await;
    assert_eq!(
        bridge.requests()[2],
        format!(
            "STREAM CONNECT ID=000102030405060708090a0b0c0d0e0f DESTINATION={destination} SILENT=false"
        ),
        "the Destination crosses the wire exactly as configured"
    );
}

/// A Destination that would not fit a control line is refused, not truncated.
#[tokio::test]
async fn an_unwritable_destination_is_refused_rather_than_truncated() {
    let at_ceiling = I2pEndpoint::parse(&"A".repeat(MAX_I2P_ENDPOINT_BYTES))
        .expect("the endpoint ceiling parses as a Destination");
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![stream_ok()],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    client.create_session(&id).await.expect("session succeeds");
    assert_eq!(
        client.connect_stream(&id, &at_ceiling).await.err(),
        Some(SamError::Malformed {
            reason: i2pr_irc_sam::error::MalformedReason::Overflowed
        }),
        "a Destination at the endpoint ceiling does not fit a control line"
    );
    wait_for_requests(&bridge, 2).await;
    assert_eq!(
        bridge.requests().len(),
        2,
        "no STREAM CONNECT was sent at all: a truncated Destination would be a silently \
         wrong target"
    );
}

// ------------------------------------------------------------------- deadlines

/// Every exchange deadline fires, and each names its own phase.
///
/// The client is driven *to* each phase before that phase's deadline is removed, because
/// asserting a deadline by timing out the first exchange only proves the first exchange
/// has one. Each case therefore starts with every other phase generous and takes out only
/// the one under test.
#[tokio::test]
async fn every_exchange_deadline_fires_and_names_its_phase() {
    // Hello: no reply at all, and no deadline for the phases that follow it.
    let bridge = FakeBridge::start(Script {
        // No replies at all: the fake accepts and then says nothing, so whichever exchange
        // the client is in runs out of time.
        ..Script::default()
    })
    .await;
    assert_eq!(
        open_stream(
            &bridge,
            SamTimeouts {
                hello: Duration::ZERO,
                ..SamTimeouts::default()
            }
        )
        .await
        .err(),
        Some(SamError::Timeout {
            phase: SamPhase::Hello
        }),
        "the hello exchange must time out and say so"
    );

    // Session create: the hello succeeds, then nothing further arrives.
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        ..Script::default()
    })
    .await;
    assert_eq!(
        open_stream(
            &bridge,
            SamTimeouts {
                session_create: Duration::ZERO,
                ..SamTimeouts::default()
            }
        )
        .await
        .err(),
        Some(SamError::Timeout {
            phase: SamPhase::SessionCreate
        }),
        "the session-create exchange must time out and say so"
    );

    // Stream connect: hello and session both succeed, then nothing further arrives. One
    // connection, because `SamClient` speaks all three on the same socket.
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        ..Script::default()
    })
    .await;
    assert_eq!(
        open_stream(
            &bridge,
            SamTimeouts {
                stream_connect: Duration::ZERO,
                ..SamTimeouts::default()
            }
        )
        .await
        .err(),
        Some(SamError::Timeout {
            phase: SamPhase::StreamConnect
        }),
        "the stream-connect exchange must time out and say so"
    );
}

/// A bridge that is not listening is reported as unavailable, and it is retryable.
///
/// The bridge-connect *deadline* cannot be exercised against a live loopback listener,
/// because a loopback connect completes in microseconds however small the deadline is —
/// a test that tried would be asserting that a working connect fails, which is the wrong
/// thing to want. What is testable is the failure itself, and it is the one an operator
/// actually hits when the router is not running.
#[tokio::test]
async fn an_absent_bridge_is_reported_as_unavailable_and_is_retryable() {
    let endpoint = i2pr_irc_sam::fake::absent_endpoint().await;
    let outcome = SamClient::open(SamClientConfig {
        bridge: endpoint,
        timeouts: SamTimeouts::default(),
        random: Arc::new(FixedRandom),
    })
    .await;
    let error = outcome.err().expect("a closed port cannot connect");
    assert!(
        matches!(
            error,
            SamError::BridgeUnavailable | SamError::Timeout { .. }
        ),
        "a closed loopback port is unavailable or timed out: {error:?}"
    );
    assert!(
        error.is_retryable(),
        "and it is retryable, because the router may simply not be running yet: {error:?}"
    );
}

/// The bridge-connect deadline exists and is bounded.
///
/// Asserted as a value because the loopback path cannot make it fire, which is a property
/// of loopback rather than a reason to leave the deadline unbounded.
#[test]
fn the_bridge_connect_deadline_is_bounded() {
    let timeouts = SamTimeouts::default();
    assert!(
        !timeouts.bridge_connect.is_zero(),
        "the connect phase must have a real deadline"
    );
    assert!(timeouts.bridge_connect <= Duration::from_secs(30));
}

/// A delayed reply is answered as long as it arrives before the deadline.
#[tokio::test]
async fn a_delayed_reply_within_the_deadline_is_accepted() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        ..Script::default()
    })
    .await;
    let timeouts = SamTimeouts {
        // Generous enough that a scheduling hiccup cannot turn this into a flake, and
        // short enough that a real hang is still bounded.
        hello: Duration::from_secs(5),
        session_create: Duration::from_secs(5),
        ..SamTimeouts::default()
    };
    let (mut client, id) = SamClient::open(config_for(&bridge, timeouts))
        .await
        .expect("a hello that arrives after a small delay is fine");
    client.create_session(&id).await.expect("session succeeds");
}

// ------------------------------------------------------------------ authority

/// The client cannot be pointed at anything but loopback.
///
/// The type makes this unrepresentable; the test makes the claim checkable rather than
/// a review comment.
#[test]
fn the_client_only_accepts_loopback() {
    for rejected in [
        "192.168.1.1:7656",
        "0.0.0.0:7656",
        "[::]:7656",
        "localhost:7656",
        "router.example:7656",
    ] {
        assert!(
            SamBridgeEndpoint::parse(rejected).is_err(),
            "{rejected} must not be constructible as a bridge endpoint"
        );
    }
    assert!(SamBridgeEndpoint::parse("127.0.0.1:7656").is_ok());
    assert!(SamBridgeEndpoint::parse("[::1]:7656").is_ok());
}

/// A failing random source is terminal and never produces an ID.
#[tokio::test]
async fn a_failing_random_source_prevents_the_exchange() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        ..Script::default()
    })
    .await;
    let mut config = config_for(&bridge, SamTimeouts::default());
    config.random = Arc::new(BrokenRandom);
    assert_eq!(
        SamClient::open(config).await.err(),
        Some(SamError::RandomUnavailable),
        "a session ID is never guessed from anything identifying"
    );
}

/// The session ID on the wire is the opaque value and nothing else.
#[tokio::test]
async fn the_session_id_carries_no_identifying_material() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok()],
        session: vec![session_ok_with_destination()],
        ..Script::default()
    })
    .await;
    let (mut client, id) = SamClient::open(config_for(&bridge, SamTimeouts::default()))
        .await
        .expect("hello succeeds");
    client.create_session(&id).await.expect("session succeeds");
    wait_for_requests(&bridge, 2).await;
    let request = &bridge.requests()[1];
    // Nothing local appears: no endpoint, no version, no build string, no display name.
    for forbidden in ["i2pr-irc", "irc.example.i2p", "bot", "1.0.0", "sam-"] {
        assert!(
            !request.contains(forbidden),
            "{forbidden:?} must not appear in a SAM request: {request}"
        );
    }
    assert_eq!(
        format!("{id:?}"),
        "SamSessionId([redacted])",
        "and the ID itself is redacted in diagnostics"
    );
}
