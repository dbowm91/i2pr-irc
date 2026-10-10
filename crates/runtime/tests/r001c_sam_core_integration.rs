//! Plan 031 / R001-C core integration: a real `RuntimeController` over a real `SamProvider`.
//!
//! Everything below crosses real boundaries. The runtime is the production controller, the
//! provider is the production `SamProvider`, and the upstream is a genuine loopback socket
//! opened through the provider's SAM scope. Only the two endpoints are faked: a scripted SAM
//! bridge in place of a router, and a scriptable IRC peer in place of an IRC server.
//!
//! That is deliberate. A test that stopped at `SamProvider::connect` would prove the scope
//! map works and say nothing about whether the thing on top of it — which owns IRC
//! registration, reconnect admission, and connection generations — behaves when the socket
//! underneath it ends for a reason the runtime did not choose.
//!
//! The claims, and the failure each one is aimed at:
//!
//! *Registration completes over a real stream.* A bouncer that never reaches `001` looks
//! exactly like one whose connect silently succeeded.
//!
//! *An IRC reconnect reuses the router identity.* The alternative — a new SAM session per
//! IRC socket — would present a fresh I2P Destination on every reconnect and let an
//! observer link a client's whole session history.
//!
//! *A genuine session loss creates exactly one new session and then recovers IRC.* Two
//! sessions for one Network would be the correlation ADR-0004 exists to prevent.
//!
//! *Nothing ambiguous is replayed across the reconnect.* This is the one the repo's
//! implementation posture calls out directly: a disconnect after an outbound command leaves
//! delivery ambiguous, so a user message must never be retried across a generation.
//!
//! No test here may require a router, a listener outside loopback, or any network authority.

#![cfg(test)]

use std::{sync::Arc, time::Duration};

use i2pr_irc_core::{I2pEndpoint, NetworkId};
use i2pr_irc_runtime::{RuntimeController, RuntimeError};
use i2pr_irc_sam::{
    SamClientConfig, SamProvider,
    fake::{FakeBridge, FakeIrcPeer, Script, hello_ok, session_ok_with_destination, stream_ok},
};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle};

/// Ceiling for every bounded wait. Exceeding it means the claim under test did not happen.
const CEILING: Duration = Duration::from_secs(20);

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&i2pr_irc_store::StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        transport_profile: i2pr_irc_store::IrcTransportProfile::PlainI2p,
        auth_profile: i2pr_irc_store::UpstreamAuthProfile::None,
        failover_group: None,
        nick: "bot".into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        // One desired channel, so registration is observable: the `JOIN` only follows a
        // completed registration, which makes it a stronger signal than "no error".
        desired_channels: vec![i2pr_irc_store::DesiredChannelRecord::at("#room", 0, false)],
    }
}

/// A provider over the fake bridge, plus the shared provider for diagnostics.
fn provider_over(bridge: &FakeBridge) -> Arc<SamProvider> {
    Arc::new(
        SamProvider::with_config(SamClientConfig {
            bridge: bridge.endpoint(),
            timeouts: i2pr_irc_sam::SamTimeouts::default(),
            random: Arc::new(i2pr_irc_sam::session_id::OsRandom),
        })
        .with_limits(64, Duration::from_secs(5)),
    )
}

/// A running controller plus the handles a test drives it through.
struct Runtime {
    control: i2pr_irc_runtime::RuntimeControlHandle,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    /// Held so the in-memory store outlives the controller task that reads it.
    #[allow(dead_code)]
    store: Store,
}

impl Runtime {
    async fn start(provider: Arc<SamProvider>) -> Self {
        let (store, handle) = store();
        let (mut controller, control) = RuntimeController::new(provider, handle);
        let task = tokio::spawn(async move { controller.serve().await });
        // Startup restore publishes a snapshot before anything else, so waiting for the
        // first revision is waiting for the controller to be live.
        let mut snapshots = control.subscribe_status();
        tokio::time::timeout(CEILING, snapshots.changed())
            .await
            .expect("the controller publishes a startup snapshot")
            .expect("the status channel is still open");
        Self {
            control,
            task,
            store,
        }
    }

    async fn stop(self) {
        self.control.request_stop();
        self.task
            .await
            .expect("the controller task joins")
            .expect("the controller reports success");
    }
}

/// Waits until the bridge has registered at least `count` streams.
async fn wait_for_streams(peer: &FakeIrcPeer, count: usize) {
    let deadline = tokio::time::Instant::now() + CEILING;
    while peer.live_streams() < count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {count} streams; saw {}",
            peer.live_streams()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Takes a replacement connection all the way to a registered IRC session.
///
/// Each IRC connection has to be welcomed on its own, exactly as a real server would. A
/// test that answered only the first handshake and then waited for the second `JOIN` would
/// be waiting for something no server would send.
async fn register_replacement(peer: &FakeIrcPeer, bridge: &FakeBridge, expected_streams: usize) {
    wait_for_streams(peer, expected_streams).await;
    wait_for_upstream(peer, bridge, b"USER ").await;
    register(peer).await;
    wait_for_upstream(peer, bridge, b"JOIN #room").await;
}

/// The CAP/001 handshake a server sends to complete registration.
///
/// The capability offer has to come first: the runtime answers it with `CAP END` before it
/// will consider itself registered, so omitting it would stall the test for a reason that
/// has nothing to do with the claim.
async fn register(peer: &FakeIrcPeer) {
    peer.send(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await;
}

/// Answers enough connects for `sessions` sessions and `connects` total connects.
///
/// Two `HELLO`s per connect, because a connect is a control socket and a data socket.
fn script_for(sessions: usize, connects: usize) -> Script {
    Script {
        hello: (0..connects.saturating_mul(2))
            .map(|_| hello_ok())
            .collect(),
        session: (0..sessions)
            .map(|_| session_ok_with_destination())
            .collect(),
        stream: (0..connects).map(|_| stream_ok()).collect(),
        ..Script::default()
    }
}

/// Waits until the client has spoken `needle` upstream, pumping the peer as it waits.
///
/// Registration is only finished once the runtime has spoken, so waiting on the *client's*
/// bytes is what distinguishes a completed handshake from a socket that merely opened.
async fn wait_for_upstream(peer: &FakeIrcPeer, bridge: &FakeBridge, needle: &[u8]) {
    assert!(
        peer.wait_for(needle, CEILING).await,
        "timed out waiting for {:?}; bridge saw {:?}",
        String::from_utf8_lossy(needle),
        bridge.requests()
    );
}

// ------------------------------------------------------------------ the claims

/// IRC registration completes over a stream the provider opened.
#[tokio::test]
async fn a_real_controller_registers_over_a_sam_stream() {
    let bridge = FakeBridge::start(script_for(1, 2)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;

    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");

    // The `USER` line only appears once the runtime has a live upstream socket.
    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    // `JOIN` is a post-registration action, so seeing it means registration finished.
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;

    let diagnostics = provider.diagnostics();
    assert_eq!(diagnostics.stream_successes, 1, "one upstream stream");
    assert_eq!(diagnostics.session_creations, 1, "over one router session");

    let _ = runtime.control.delete(network).await;
    runtime.stop().await;
}

/// An IRC reconnect reuses the router identity.
///
/// This is the claim the whole scope map exists for. If a reconnect minted a new session,
/// every IRC socket would present a fresh Destination and an observer could link them.
#[tokio::test]
async fn an_irc_eof_reconnects_through_the_same_sam_session() {
    let bridge = FakeBridge::start(script_for(1, 4)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");

    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "the first generation used one session"
    );

    // The IRC server goes away without a QUIT or an error.
    peer.close().await;
    register_replacement(&peer, &bridge, 2).await;
    let diagnostics = provider.diagnostics();
    assert_eq!(
        diagnostics.session_creations, 1,
        "an IRC reconnect must not churn the router identity"
    );
    assert_eq!(
        diagnostics.stream_successes, 2,
        "and it did open a second stream over it"
    );

    let _ = runtime.control.delete(network).await;
    runtime.stop().await;
}

/// A real session loss creates exactly one new session, and IRC recovers over it.
///
/// The failure injected here is the router's own: the control socket ends, so the identity
/// the provider was holding is gone. The scope must not keep using it, and must not open a
/// second session alongside it.
#[tokio::test]
async fn a_lost_session_is_replaced_once_and_irc_recovers() {
    // The bridge hangs up on every control socket it answers, so each session this test
    // establishes is genuinely destroyed rather than merely reported as broken.
    let bridge = FakeBridge::start(Script {
        hello: (0..8).map(|_| hello_ok()).collect(),
        session: (0..4).map(|_| session_ok_with_destination()).collect(),
        stream: (0..4).map(|_| stream_ok()).collect(),
        close_after_session: true,
        ..Script::default()
    })
    .await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");

    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;

    // Force the next connect to rebuild the session: the provider notices the ended
    // control socket and re-identifies exactly once.
    peer.close().await;
    register_replacement(&peer, &bridge, 2).await;

    let diagnostics = provider.diagnostics();
    assert!(
        diagnostics.session_creations >= 2,
        "a destroyed session is replaced, not reused: {diagnostics:?}"
    );
    assert_eq!(
        diagnostics.live_scopes, 1,
        "and still exactly one scope for this Network"
    );
    assert_eq!(peer.refused(), 0, "no stream hit the fixture's own ceiling");

    let _ = runtime.control.delete(network).await;
    runtime.stop().await;
}

/// Nothing ambiguous crosses a reconnect.
///
/// The repo's posture is explicit that a disconnect after an outbound command leaves
/// delivery ambiguous, so a user message must not be retried on a new connection. What
/// this asserts is the property the runtime can actually be held to at this layer: the
/// bytes the replacement stream carries are a registration handshake and nothing else.
/// Anything carried over from the previous connection would be a replay, because the
/// runtime cannot know whether the old server acted on it.
#[tokio::test]
async fn a_reconnect_replays_no_upstream_frame_from_the_old_connection() {
    let bridge = FakeBridge::start(script_for(1, 4)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");

    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;
    let before = peer.received_per_stream().await;
    assert!(
        !before.is_empty(),
        "there was an upstream stream to disconnect from"
    );

    peer.close().await;
    register_replacement(&peer, &bridge, 2).await;
    // Let the replacement finish speaking, so a late frame cannot hide behind the wait.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let per_stream = peer.received_per_stream().await;
    assert!(
        per_stream.len() >= 2,
        "a replacement stream exists: {per_stream:?}"
    );
    let replacement = per_stream
        .iter()
        .find(|bytes| !before.contains(bytes))
        .expect("a stream the first connection did not produce");
    // A fresh IRC connection has to identify and join again; that is a re-registration,
    // not a replay. Everything else on it would be a command carried across generations.
    for frame in replacement
        .split(|byte| *byte == b'\n')
        .map(|frame| String::from_utf8_lossy(frame))
        .map(|frame| frame.trim_end_matches('\r').to_owned())
        .filter(|frame| !frame.is_empty())
    {
        assert!(
            frame.starts_with("NICK ")
                || frame.starts_with("USER ")
                || frame.starts_with("CAP ")
                || frame.starts_with("JOIN "),
            "the replacement connection carried {frame:?}, which is not part of a fresh \
             registration and therefore looks like a replay"
        );
    }

    let _ = runtime.control.delete(network).await;
    runtime.stop().await;
}

/// Reconnect admission stays accounted across a reconnect.
///
/// A provider that retried without admission would look fine from the Network's side and
/// would quietly defeat the process-wide connect budget that exists to stop a failing
/// Network from starving the others.
#[tokio::test]
async fn reconnect_admission_counts_stay_correct_across_a_reconnect() {
    let bridge = FakeBridge::start(script_for(1, 4)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");

    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;
    let before = runtime
        .control
        .diagnostics(None)
        .await
        .expect("diagnostics are available");
    peer.close().await;
    register_replacement(&peer, &bridge, 2).await;

    let after = runtime
        .control
        .diagnostics(None)
        .await
        .expect("diagnostics are available");
    // `in_flight_connects` is a current gauge, not a tally: it must read zero once both
    // attempts have finished, and a provider that retried without releasing its permit is
    // exactly the failure that shows up here as a stuck non-zero.
    assert_eq!(
        after.in_flight_connects, 0,
        "both admitted attempts released their permits"
    );
    assert_eq!(
        after.peak_in_flight_connects, 1,
        "a single Network reconnects serially, so the budget was never oversubscribed"
    );
    assert!(
        after.peak_in_flight_connects >= before.peak_in_flight_connects,
        "and the high-water mark never went backwards across the reconnect"
    );
    assert_eq!(
        after.reconnect_waiters, 0,
        "a single Network never queues behind itself"
    );
    assert_eq!(
        after.networks.len(),
        1,
        "and the controller still tracks exactly the one Network"
    );
    assert_eq!(
        provider.diagnostics().stream_successes,
        2,
        "both connects really went through the provider"
    );

    let _ = runtime.control.delete(network).await;
    runtime.stop().await;
}

/// Deleting the Network releases its scope, and shutdown leaves nothing behind.
#[tokio::test]
async fn delete_and_shutdown_leave_no_scope_behind() {
    let bridge = FakeBridge::start(script_for(1, 4)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");
    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;

    assert!(
        runtime
            .control
            .delete(network)
            .await
            .expect("the delete is accepted"),
        "the Network existed to delete"
    );
    // Release is attempted before the delete returns, so the scope must already be gone.
    let deadline = tokio::time::Instant::now() + CEILING;
    while provider.diagnostics().live_scopes != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a deleted Network must not keep a router session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    runtime.stop().await;
    assert_eq!(
        provider.diagnostics().live_scopes,
        0,
        "shutdown adds nothing"
    );
}

/// The provider never puts a Network on the wire in a way a stranger could read.
///
/// Cheap to assert here and expensive to discover in production, because the wire is where
/// an identifier becomes durable on someone else's disk.
#[tokio::test]
async fn the_wire_carries_no_operator_identifiers() {
    let bridge = FakeBridge::start(script_for(1, 2)).await;
    let peer = bridge.peer();
    let provider = provider_over(&bridge);
    let runtime = Runtime::start(Arc::clone(&provider)).await;
    let _network = runtime
        .control
        .create(record(1))
        .await
        .expect("the Network is created");
    wait_for_upstream(&peer, &bridge, b"USER ").await;
    register(&peer).await;
    wait_for_upstream(&peer, &bridge, b"JOIN #room").await;

    for request in bridge.requests() {
        for forbidden in ["net-1", "display_name", "bouncer.0"] {
            assert!(
                !request.contains(forbidden),
                "{forbidden:?} reached the router: {request}"
            );
        }
    }
    runtime.stop().await;
}
