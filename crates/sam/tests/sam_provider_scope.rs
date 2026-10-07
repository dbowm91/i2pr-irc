//! Plan 031 / R001-C qualification: the per-Network SAM scope map.
//!
//! Everything here drives the real `SamProvider` against a real loopback bridge. The
//! bridge is scripted, so a test controls exactly what the router answers and can assert
//! the *counts* that matter: how many sessions were created, how many were lost, how many
//! streams succeeded.
//!
//! The claim under test throughout is that **one durable Network has one router-side
//! identity, for as long as it stays healthy**. Every failure mode that would break that
//! — a peer being unreachable, an IRC reconnect, a control socket closing — has a test
//! proving it does *not* churn the session, and every failure that genuinely means the
//! session is gone has a test proving it creates exactly one replacement.

#![cfg(test)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use i2pr_irc_core::{I2pEndpoint, I2pStreamProvider, NetworkId};
use i2pr_irc_sam::{
    SamClientConfig, SamError, SamProvider, SamTimeouts,
    fake::{FakeBridge, Script, hello_ok, line, session_ok_with_destination, stream_ok},
    session_id::RandomSource,
};

/// Ceiling for every bounded wait in this suite.
const CEILING: Duration = Duration::from_secs(10);

/// A deterministic stand-in for OS randomness.
///
/// Present so a session ID is reproducible, not so a test can count identities. Counting
/// was tried here and was wrong: `HELLO` mints an ID for every *socket*, and one connect
/// opens two of them, so a draw count measures sockets rather than the router-side
/// identities scoping actually governs. Those claims are asserted against the SESSION
/// CREATE lines on the wire instead.
#[derive(Default)]
struct DeterministicRandom(AtomicU64);

impl RandomSource for DeterministicRandom {
    fn fill(&self, out: &mut [u8]) -> Result<(), i2pr_irc_sam::session_id::RandomUnavailable> {
        let mut next = self.0.load(Ordering::SeqCst);
        for slot in out.iter_mut() {
            next = next.wrapping_add(1);
            *slot = next as u8;
        }
        self.0.store(next, Ordering::SeqCst);
        Ok(())
    }
}

fn endpoint() -> I2pEndpoint {
    I2pEndpoint::parse("irc.example.i2p").expect("the test endpoint parses")
}

/// A provider aimed at `bridge`.
fn provider_for(bridge: &FakeBridge) -> Arc<SamProvider> {
    Arc::new(
        SamProvider::with_config(SamClientConfig {
            bridge: bridge.endpoint(),
            timeouts: SamTimeouts::default(),
            random: Arc::new(DeterministicRandom::default()) as Arc<dyn RandomSource>,
        })
        .with_limits(64, Duration::from_secs(5)),
    )
}

/// Answers N healthy connects.
///
/// A connect is four requests, not one: the provider opens a control socket for the
/// `HELLO` + `SESSION CREATE`, then a *separate* data socket for its own `HELLO` +
/// `STREAM CONNECT`. Both sockets share the session ID, which is the point — a second
/// `SESSION CREATE` would be a second router identity.
fn healthy_replies(streams: usize) -> Script {
    Script::healthy(streams)
}

/// Answers `networks` Networks, one session each.
fn scoped_replies(networks: usize, streams: usize) -> Script {
    Script::scoped(networks, streams)
}

async fn wait_for(mut check: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + CEILING;
    while !check() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ------------------------------------------------------- per-Network session ownership

/// Two Networks get two sessions with two distinct identities.
///
/// This is the entire point of scoping. With one shared session, every configured IRC
/// Network would present the same I2P Destination to every router it talked to, and an
/// observer could correlate them all.
#[tokio::test]
async fn two_networks_get_two_distinct_sessions() {
    let bridge = FakeBridge::start(scoped_replies(2, 1)).await;
    let provider = provider_for(&bridge);
    let _one = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("first connects");
    let _two = provider
        .connect(NetworkId(2), &endpoint())
        .await
        .expect("second connects");

    assert_eq!(
        provider.diagnostics().session_creations,
        2,
        "one session per Network"
    );
    // Deliberately not an assertion about drawn IDs: `HELLO` mints one ID per *socket*,
    // and every connect opens two of them. The identity claim is about the SESSION CREATE
    // lines below, which is what the router actually binds.
    wait_for(|| bridge.requests().len() >= 4, "both exchanges").await;
    let requests = bridge.requests();
    let creates: Vec<&String> = requests
        .iter()
        .filter(|request| request.starts_with("SESSION CREATE"))
        .collect();
    assert_eq!(creates.len(), 2, "two SESSION CREATEs crossed the wire");
    assert_ne!(creates[0], creates[1], "with different IDs");
}

/// One Network keeps one session across many IRC generations.
///
/// A hundred reconnects must not churn the router identity. Each generation is simulated
/// by connecting again; in production the same thing happens every time an IRC socket ends
/// and the owner retries.
#[tokio::test]
async fn one_network_across_many_reconnects_keeps_one_session() {
    let bridge = FakeBridge::start(healthy_replies(101)).await;
    let provider = provider_for(&bridge);
    for _ in 0..100 {
        let _stream = provider
            .connect(NetworkId(1), &endpoint())
            .await
            .expect("each generation connects");
    }
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "a hundred IRC reconnects must not churn the router identity"
    );
    // Not a drawn-ID count: `HELLO` mints one per socket and a connect opens two, so a
    // hundred connects draws two hundred. The claim is that only one of them became a
    // router-side session, which is what the wire below shows.
    let creates = bridge
        .requests()
        .iter()
        .filter(|request| request.starts_with("SESSION CREATE"))
        .count();
    assert_eq!(
        creates, 1,
        "a hundred IRC generations produced exactly one SESSION CREATE"
    );
    assert_eq!(provider.diagnostics().stream_successes, 100);
}

/// A peer that cannot be reached does not destroy the session.
///
/// `CANT_REACH_PEER` says the *peer* is unreachable, not that the session is gone. Tearing
/// the identity down here would churn it on every IRC outage, which is the failure the
/// long-lived-session property exists to prevent.
#[tokio::test]
async fn a_peer_failure_does_not_destroy_a_healthy_session() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok(), hello_ok(), hello_ok()],
        session: vec![session_ok_with_destination()],
        stream: vec![
            line(r#"STREAM STATUS RESULT=ERROR MESSAGE="Can't reach peer""#),
            stream_ok(),
        ],
        ..Script::default()
    })
    .await;
    let provider = provider_for(&bridge);
    let failed = provider.connect(NetworkId(1), &endpoint()).await;
    assert!(failed.is_err(), "an unreachable peer is a connect failure");
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "the session survives a peer failure"
    );
    // The next attempt reuses it.
    let _stream = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("the next attempt succeeds over the same session");
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "still one session: a peer problem is not a router problem"
    );
}

/// A control socket that ends invalidates the session, and exactly one replaces it.
///
/// The first connect still succeeds: the router drops the control socket *after* saying
/// `RESULT=OK`, so the stream is fine and the identity is not. What must not happen is the
/// scope going on to reuse that identity — which is what makes the second connect create a
/// replacement rather than attaching to a session the router has already discarded.
#[tokio::test]
async fn a_closed_control_socket_invalidates_and_creates_one_replacement() {
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok(), hello_ok(), hello_ok(), hello_ok()],
        session: vec![session_ok_with_destination(), session_ok_with_destination()],
        stream: vec![stream_ok(), stream_ok()],
        close_after_session: true,
        ..Script::default()
    })
    .await;
    let provider = provider_for(&bridge);
    let _first = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("a control EOF after RESULT=OK does not spoil the stream already open");
    // The EOF is noticed by the socket watcher, which runs on its own task; waiting for the
    // loss to be recorded is what makes the next assertion about replacement rather than
    // about scheduling.
    wait_for(
        || provider.diagnostics().session_losses >= 1,
        "the control socket end to be observed",
    )
    .await;

    let _second = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("the replacement session connects");
    let diagnostics = provider.diagnostics();
    assert_eq!(
        diagnostics.session_creations, 2,
        "one replacement, not a reused identity and not a storm"
    );
    assert_eq!(
        diagnostics.session_losses, diagnostics.session_creations,
        "every session this bridge opened also had its control socket closed, and every \
         one of those ends was noticed: the count cannot exceed what was created"
    );
    assert_eq!(
        diagnostics.live_scopes, 1,
        "the Network still has exactly one scope"
    );
}

/// `INVALID_ID` invalidates the session before the EOF is observed.
///
/// This is what stops the next connect from attaching to a session the router has already
/// discarded: without it, every attempt would keep failing the same way until a control
/// socket EOF happened to arrive.
#[tokio::test]
async fn an_invalid_session_id_invalidates_immediately() {
    let bridge = FakeBridge::start(Script {
        // Four hellos for two connects: each connect opens a control socket and a data
        // socket, and each says HELLO. Scripting three left the second data socket
        // talking to a silent bridge until its deadline.
        hello: vec![hello_ok(), hello_ok(), hello_ok(), hello_ok()],
        session: vec![session_ok_with_destination(), session_ok_with_destination()],
        stream: vec![
            line(r#"STREAM STATUS RESULT=ERROR MESSAGE="INVALID ID""#),
            stream_ok(),
        ],
        ..Script::default()
    })
    .await;
    let provider = provider_for(&bridge);
    let _ = provider.connect(NetworkId(1), &endpoint()).await;
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "one session existed when INVALID_ID came back"
    );
    assert_eq!(
        provider.diagnostics().session_losses,
        1,
        "and it was recorded as lost"
    );
    let _ = provider.connect(NetworkId(1), &endpoint()).await;
    assert_eq!(
        provider.diagnostics().session_creations,
        2,
        "the next attempt creates exactly one replacement"
    );
}

// -------------------------------------------------------------------- release

/// Release destroys the scope, and releasing again is success.
#[tokio::test]
async fn release_destroys_the_scope_and_is_idempotent() {
    let bridge = FakeBridge::start(healthy_replies(1)).await;
    let provider = provider_for(&bridge);
    let _stream = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("connects");
    assert_eq!(provider.diagnostics().live_scopes, 1);

    provider
        .release(NetworkId(1))
        .await
        .expect("release succeeds");
    assert_eq!(
        provider.diagnostics().live_scopes,
        0,
        "the scope is gone from the map"
    );
    provider
        .release(NetworkId(1))
        .await
        .expect("a repeated release is a no-op, not an error");
    provider
        .release(NetworkId(99))
        .await
        .expect("releasing an unknown Network is a no-op, not an error");
    assert_eq!(provider.diagnostics().releases, 3);
}

/// Releasing one Network leaves the others untouched.
#[tokio::test]
async fn releasing_one_network_leaves_the_others_alone() {
    let bridge = FakeBridge::start(scoped_replies(3, 1)).await;
    let provider = provider_for(&bridge);
    for network in [1u64, 2, 3] {
        let _stream = provider
            .connect(NetworkId(network), &endpoint())
            .await
            .expect("each connects");
    }
    assert_eq!(provider.diagnostics().live_scopes, 3);
    provider
        .release(NetworkId(2))
        .await
        .expect("release succeeds");
    let diagnostics = provider.diagnostics();
    assert_eq!(diagnostics.live_scopes, 2, "one scope went");
    assert_eq!(
        diagnostics.session_creations, 3,
        "and no session was recreated for the survivors"
    );
}

/// A new connect after a release creates a fresh session.
///
/// Recreating is correct rather than wrong: the scope was destroyed, so the next connect
/// legitimately starts a new lifetime. What would be wrong is silently reusing the
/// destroyed session's identity.
#[tokio::test]
async fn a_connect_after_release_creates_a_fresh_session() {
    // Two sessions, because a release destroys the identity and the next connect
    // legitimately has to establish another. Scripting only one left the second attempt
    // waiting on a reply the bridge had none of.
    let bridge = FakeBridge::start(Script {
        hello: vec![hello_ok(), hello_ok(), hello_ok(), hello_ok()],
        session: vec![session_ok_with_destination(), session_ok_with_destination()],
        stream: vec![stream_ok(), stream_ok()],
        ..Script::default()
    })
    .await;
    let provider = provider_for(&bridge);
    let _first = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("connects");
    provider
        .release(NetworkId(1))
        .await
        .expect("release succeeds");
    let _second = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("a connect after release works");
    assert_eq!(provider.diagnostics().session_creations, 2);
    let creates: Vec<String> = bridge
        .requests()
        .into_iter()
        .filter(|request| request.starts_with("SESSION CREATE"))
        .collect();
    assert_eq!(creates.len(), 2, "two SESSION CREATEs crossed the wire");
    assert_ne!(
        creates[0], creates[1],
        "and the replacement carried a different identity, not the destroyed one"
    );
}

// --------------------------------------------------------------------- bounds

/// The scope map is bounded, and the bound is the runtime's.
#[tokio::test]
async fn the_scope_map_is_bounded() {
    let bridge = FakeBridge::start(scoped_replies(2, 1)).await;
    let provider = SamProvider::with_config(SamClientConfig {
        bridge: bridge.endpoint(),
        timeouts: SamTimeouts::default(),
        random: Arc::new(DeterministicRandom::default()) as Arc<dyn RandomSource>,
    })
    .with_limits(2, Duration::from_secs(5));
    let provider = Arc::new(provider);
    for network in 1u64..=2 {
        let _stream = provider
            .connect(NetworkId(network), &endpoint())
            .await
            .expect("a scope within the bound connects");
    }
    let refused = provider.connect(NetworkId(3), &endpoint()).await;
    assert!(
        refused.is_err(),
        "a third scope is refused rather than growing the map"
    );
    assert_eq!(provider.diagnostics().live_scopes, 2);
}

/// A scope is created once, even under concurrent first connects.
///
/// Two callers racing on a cold Network must not produce two scope owners, which would mean
/// two router identities for one Network — the exact thing scoping forbids.
#[tokio::test]
async fn concurrent_first_connects_create_one_scope() {
    let bridge = FakeBridge::start(healthy_replies(4)).await;
    let provider = provider_for(&bridge);
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let provider = Arc::clone(&provider);
        tasks.push(tokio::spawn(async move {
            provider.connect(NetworkId(1), &endpoint()).await
        }));
    }
    let mut connected = 0;
    for task in tasks {
        if task.await.expect("the task joins").is_ok() {
            connected += 1;
        }
    }
    assert!(
        connected >= 1,
        "at least one caller succeeds; refusals are the queue's job, not the map's"
    );
    assert_eq!(
        provider.diagnostics().live_scopes,
        1,
        "four concurrent first connects produced exactly one scope owner"
    );
    assert_eq!(
        provider.diagnostics().session_creations,
        1,
        "and exactly one router session"
    );
}

/// Diagnostics settle back to a known baseline after churn.
///
/// A peak proves a ceiling held; only a return to baseline proves nothing leaked. This is
/// the discipline Plan 017 established for the bouncer's own resource ledger, applied to
/// the scope map.
#[tokio::test]
async fn diagnostics_settle_after_churn() {
    let bridge = FakeBridge::start(scoped_replies(4, 4)).await;
    let provider = provider_for(&bridge);
    let baseline = provider.diagnostics();
    assert_eq!(baseline, Default::default());

    for network in 1u64..=4 {
        for _ in 0..4 {
            let _ = provider.connect(NetworkId(network), &endpoint()).await;
        }
        provider
            .release(NetworkId(network))
            .await
            .expect("release succeeds");
    }
    let after = provider.diagnostics();
    assert_eq!(after.live_scopes, 0, "every scope was released");
    assert!(
        after.session_creations >= 4 && after.stream_attempts >= 16,
        "the work really happened: {after:?}"
    );
    assert_eq!(
        after.healthy_scopes, 0,
        "and nothing is left claiming to be healthy"
    );
    assert!(
        after.session_losses <= after.session_creations,
        "a session cannot be lost before it was created: {after:?}"
    );
    assert_eq!(
        after.releases, 4,
        "every scope was released, and the count outlived them"
    );
}

/// Diagnostics never carry anything that could identify a Network or a session.
#[tokio::test]
async fn diagnostics_are_secret_free() {
    let bridge = FakeBridge::start(healthy_replies(1)).await;
    let provider = provider_for(&bridge);
    let _stream = provider
        .connect(NetworkId(1), &endpoint())
        .await
        .expect("connects");
    let rendered = format!("{:?}", provider.diagnostics());
    for forbidden in [
        "SessionId",
        "Destination",
        "irc.example.i2p",
        "MESSAGE",
        "bot",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "{forbidden:?} must not appear in provider diagnostics: {rendered}"
        );
    }
    assert!(
        provider.diagnostics().max_epoch >= 1,
        "the epoch counter is reported so an operator can tell retry from recreation"
    );
}

// --------------------------------------------------------- cancellation and bounds

/// A release interrupts a connect that is still waiting on a silent router.
///
/// Without this the release would sit on its own deadline waiting for an exchange the
/// caller has already given up on, and the delete that asked for it would report a timeout
/// for a Network that is already gone. The bridge here answers nothing at all, which is
/// the only way to have a connect genuinely in flight when the release arrives.
#[tokio::test]
async fn a_release_answers_a_connect_that_is_still_pending() {
    let bridge = FakeBridge::start(Script::default()).await;
    let provider = provider_for(&bridge);
    let waiter = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move { provider.connect(NetworkId(1), &endpoint()).await })
    };
    wait_for(
        || provider.diagnostics().live_scopes == 1,
        "the scope to be created",
    )
    .await;
    // The connect is now parked on a bridge that will never answer.
    provider
        .release(NetworkId(1))
        .await
        .expect("a release must not wait out the pending exchange");
    let outcome = tokio::time::timeout(CEILING, waiter)
        .await
        .expect("the pending connect is answered rather than abandoned")
        .expect("the connect task joins");
    assert!(
        outcome.is_err(),
        "a released scope must not hand back a stream it no longer owns"
    );
    assert_eq!(provider.diagnostics().live_scopes, 0, "and it is gone");
}

/// An abandoned caller does not leave the scope wedged.
///
/// The connect future is dropped mid-flight, which is what a shutdown or a control-plane
/// timeout does to work in progress. The scope has to survive it: still one owner, still
/// no half-built session, and no scope per abandoned attempt.
#[tokio::test]
async fn an_abandoned_connect_leaves_the_scope_usable() {
    let bridge = FakeBridge::start(Script::default()).await;
    let provider = provider_for(&bridge);
    let task = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move { provider.connect(NetworkId(1), &endpoint()).await })
    };
    wait_for(
        || provider.diagnostics().live_scopes == 1,
        "the scope to be created",
    )
    .await;
    task.abort();
    let _ = task.await;

    let diagnostics = provider.diagnostics();
    assert_eq!(diagnostics.live_scopes, 1, "still exactly one scope owner");
    assert_eq!(
        diagnostics.session_creations, 0,
        "no session was built, so none has to be torn down"
    );
    // Releasing afterwards still converges, which is what a shutdown would do next.
    provider
        .release(NetworkId(1))
        .await
        .expect("an abandoned connect does not wedge the release path");
}

/// The request queue refuses rather than growing.
///
/// A burst against one Network is bounded at the ceiling. Refusing is the honest answer
/// because the caller already holds a process-wide reconnect permit for the attempt, so
/// queueing here would park a permit against work that may never run.
#[tokio::test]
async fn the_request_queue_is_bounded_and_refuses_rather_than_growing() {
    // A bridge that never answers, so every attempt stays in flight and the queue fills.
    let bridge = FakeBridge::start(Script::default()).await;
    let provider = provider_for(&bridge);
    let mut attempts = Vec::new();
    for _ in 0..i2pr_irc_sam::provider::SAM_SCOPE_REQUEST_CAPACITY + 4 {
        let provider = Arc::clone(&provider);
        attempts.push(tokio::spawn(async move {
            provider.connect(NetworkId(1), &endpoint()).await
        }));
    }
    // The scope serves one request at a time, so let the queue reach its ceiling.
    let deadline = tokio::time::Instant::now() + CEILING;
    while provider.diagnostics().peak_queued
        < i2pr_irc_sam::provider::SAM_SCOPE_REQUEST_CAPACITY as u64
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the queue never reached its ceiling: {:?}",
            provider.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        provider.diagnostics().live_scopes,
        1,
        "a burst still creates exactly one scope owner"
    );
    assert!(
        provider.diagnostics().peak_queued
            <= i2pr_irc_sam::provider::SAM_SCOPE_REQUEST_CAPACITY as u64,
        "and never exceeded it: {:?}",
        provider.diagnostics()
    );
    for attempt in attempts {
        attempt.abort();
    }
    provider
        .release(NetworkId(1))
        .await
        .expect("release succeeds after a burst");
}

// ---------------------------------------------------------------- error mapping

/// The mapping is coarse and deliberate.
///
/// The runtime's scheduler keys off retryability, not off SAM's distinctions. Mapping every
/// router class onto its own provider error would mean the scheduler had to understand SAM
/// to be correct.
#[tokio::test]
async fn sam_errors_map_to_bounded_provider_classes() {
    use i2pr_irc_core::ProviderError;
    use i2pr_irc_sam::provider::map_error;
    for error in [
        SamError::BridgeUnavailable,
        SamError::SessionLost,
        SamError::Closed,
        SamError::PeerUnavailable {
            rejection: i2pr_irc_sam::StreamRejection::CantReachPeer,
        },
        SamError::Timeout {
            phase: i2pr_irc_sam::SamPhase::StreamConnect,
        },
    ] {
        assert_eq!(
            map_error(error.clone()),
            ProviderError::Unavailable,
            "{error:?} is retryable and must not be permanent"
        );
    }
    for error in [
        SamError::UnsupportedVersion,
        SamError::DestinationRejected,
        SamError::RandomUnavailable,
        SamError::InvalidBridge(i2pr_irc_sam::SamBridgeEndpoint::parse("x:1").unwrap_err()),
        SamError::Malformed {
            reason: i2pr_irc_sam::error::MalformedReason::UnexpectedVerb,
        },
    ] {
        assert_eq!(
            map_error(error.clone()),
            ProviderError::Failed,
            "{error:?} cannot succeed on a retry and must not be retried forever"
        );
    }
}

/// A missing bridge is refused as unavailable rather than hanging.
#[tokio::test]
async fn an_absent_bridge_is_refused_not_hung() {
    let absent = i2pr_irc_sam::fake::absent_endpoint().await;
    let provider = SamProvider::with_config(SamClientConfig {
        bridge: absent,
        timeouts: SamTimeouts::default(),
        random: Arc::new(DeterministicRandom::default()) as Arc<dyn RandomSource>,
    });
    let outcome = provider.connect(NetworkId(1), &endpoint()).await;
    assert!(
        outcome.is_err(),
        "an absent bridge is a failure, not a hang"
    );
}
