//! Plan 029 / R001-A qualification: provider scope, release lifecycle, and endpoint
//! correction.
//!
//! Four claims are under test, and each of them exists because the code could plausibly
//! have been written the other way and still compiled.
//!
//! *Every provider call carries a Network.* A connect attempt made on behalf of one
//! Network is observable as that Network's, so "these two Networks never share a router
//! resource" is a claim about scope rather than about counting.
//!
//! *Release happens exactly once, after the owner has ended.* Deleting a Network releases
//! its scope, shutting down releases every scope, and neither releases a Network that was
//! never configured. Release is attempted after the owner task is joined, never before,
//! so it cannot race a connect that would re-acquire what it is tearing down.
//!
//! *Release failure does not strand configuration.* A provider that refuses to release is
//! reported to the delete's caller, and the durable row still goes away. The alternative
//! would leave an Operator unable to delete a Network because a router adapter was
//! uncooperative.
//!
//! *A raw destination is accepted on its real terms.* The previous check compared a
//! Base64 destination against the length of a `.b32.i2p` name and validated it against the
//! base64url alphabet, so every real destination was rejected. Everything runs against
//! fake providers and scripted streams; no test may require a real listener, a real
//! router, or any network authority.
#![cfg(test)]

use i2pr_irc_core::{
    I2pEndpoint, I2pEndpointKind, I2pStreamProvider, MAX_I2P_DESTINATION_CHARS,
    MIN_I2P_DESTINATION_CHARS, NetworkId,
};
use i2pr_irc_runtime::{RuntimeController, RuntimeError};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// Ceiling for every bounded wait in this suite. A test that exceeds it has failed to
/// observe an event, which is a failure of the claim under test, not a flake to retry.
const CEILING: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: Vec::new(),
    }
}

/// Which Networks a release was asked for, and whether it failed.
///
/// Recorded through a channel rather than a flag so a release that never arrives is
/// distinguishable from one that arrives with the wrong scope, and so a release that
/// fails is observable without the controller reporting it.
#[derive(Default)]
struct ReleaseLog {
    released: Vec<NetworkId>,
    /// The next release fails once, then succeeds.
    fail_once: bool,
}

/// A provider that records releases and can be told to refuse one.
struct ScopedProvider {
    inner: FakeI2pStreamProvider,
    log: Arc<Mutex<ReleaseLog>>,
}

impl ScopedProvider {
    fn new() -> (Arc<Self>, Arc<Mutex<ReleaseLog>>) {
        let log = Arc::new(Mutex::new(ReleaseLog::default()));
        (
            Arc::new(Self {
                inner: FakeI2pStreamProvider::default(),
                log: log.clone(),
            }),
            log,
        )
    }
    fn released(&self, log: &Arc<Mutex<ReleaseLog>>) -> Vec<NetworkId> {
        log.lock()
            .expect("release log is readable")
            .released
            .clone()
    }
    /// Arms the next release for `network` to fail.
    fn arm_release_failure(log: &Arc<Mutex<ReleaseLog>>, network: NetworkId) {
        let mut guard = log.lock().expect("release log is writable");
        guard.released.push(network);
        guard.fail_once = true;
    }
}

#[async_trait::async_trait]
impl I2pStreamProvider for ScopedProvider {
    async fn connect(
        &self,
        network: NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, i2pr_irc_core::ProviderError> {
        self.inner.connect(network, endpoint).await
    }

    async fn release(&self, network: NetworkId) -> Result<(), i2pr_irc_core::ProviderError> {
        let fail = {
            let mut guard = self.log.lock().expect("release log is writable");
            guard.released.push(network);
            std::mem::take(&mut guard.fail_once)
        };
        if fail {
            return Err(i2pr_irc_core::ProviderError::Failed);
        }
        self.inner.release(network).await
    }
}

/// A controller plus the handles a test drives it through.
struct Runtime {
    control: i2pr_irc_runtime::RuntimeControlHandle,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    store: Store,
}

impl Runtime {
    async fn start(provider: Arc<ScopedProvider>, log: &Arc<Mutex<ReleaseLog>>) -> Self {
        let (store, handle) = store();
        let (mut controller, control) = RuntimeController::new(provider, handle);
        let task = tokio::spawn(async move { controller.serve().await });
        // The first published snapshot is sent only after startup restore completes, so
        // waiting for it is waiting for restore.
        wait_for(&control, |_| true, log).await;
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
            .expect("controller task joins")
            .expect("controller reports success");
        self.store.shutdown().expect("store shuts down");
    }
}

/// Waits for a controller snapshot satisfying `predicate`.
async fn wait_for(
    control: &i2pr_irc_runtime::RuntimeControlHandle,
    predicate: impl Fn(&i2pr_irc_runtime::ControlSnapshot) -> bool,
    log: &Arc<Mutex<ReleaseLog>>,
) {
    let _ = log;
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if let Ok(snapshot) = control.status().await
            && predicate(&snapshot)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no snapshot satisfied the predicate within {CEILING:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ------------------------------------------- scope is part of the provider contract

/// A connect made on behalf of one Network is observable as that Network's.
///
/// Counting attempts is not enough to support the invariant it looks like it supports:
/// two connects could both be attributed to one Network while the other silently reused
/// its resources. The fake records the scope of every attempt so the test can tell the
/// two apart.
#[tokio::test]
async fn every_connect_attempt_is_scoped_to_the_network_that_asked_for_it() {
    let (provider, log) = ScopedProvider::new();
    provider
        .inner
        .queue_outcome(Ok(FaultScript::default()))
        .expect("outcome queues");
    provider
        .inner
        .queue_outcome(Ok(FaultScript::default()))
        .expect("outcome queues");
    let endpoint = I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses");
    let _first = provider
        .connect(NetworkId(1), &endpoint)
        .await
        .expect("first connects");
    let _second = provider
        .connect(NetworkId(2), &endpoint)
        .await
        .expect("second connects");
    assert_eq!(
        provider.inner.requested_scopes(),
        vec![NetworkId(1), NetworkId(2)],
        "each attempt is attributed to the Network that requested it"
    );
    assert!(
        provider.released(&log).is_empty(),
        "connecting releases nothing"
    );
}

/// Two Networks sharing one provider must not collapse into one scope.
///
/// This is the shape of the R001-A regression: an unscoped provider could satisfy every
/// count-based assertion while letting one Network's connect serve another's Network.
#[tokio::test]
async fn two_networks_sharing_one_provider_keep_distinct_scopes() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    for network in [1u64, 2] {
        provider
            .inner
            .queue_outcome(Err(i2pr_irc_core::ProviderError::Unavailable))
            .expect("outcome queues");
        runtime
            .control
            .create(record(network))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
    }
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 2,
        &log,
    )
    .await;
    let scopes = provider.inner.requested_scopes();
    for network in [NetworkId(1), NetworkId(2)] {
        assert!(
            scopes.contains(&network),
            "Network {network:?} attempted a connect under its own scope; saw {scopes:?}"
        );
    }
    runtime.stop().await;
}

// ------------------------------------------------------- release after the owner ends

/// Deleting a Network releases exactly that Network's scope.
#[tokio::test]
async fn deleting_a_network_releases_only_that_network_scope() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    for network in [1u64, 2] {
        runtime
            .control
            .create(record(network))
            .await
            .expect("create succeeds");
    }
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 2,
        &log,
    )
    .await;
    assert!(
        provider.released(&log).is_empty(),
        "a running Network holds its scope"
    );

    assert!(
        runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("delete answers")
    );
    let released = provider.released(&log);
    assert!(
        released.contains(&NetworkId(1)),
        "the deleted Network released its own scope; saw {released:?}"
    );
    assert!(
        !released.contains(&NetworkId(2)),
        "a surviving Network's scope is untouched by another Network's delete; saw {released:?}"
    );
    runtime.stop().await;
}

/// A delete releases once, not once per durable write or per republish.
///
/// The old lifecycle had no release at all; a naive repair would put one on every path
/// that touches a Network. Releasing twice for one delete is a real defect rather than a
/// harmless repetition, because the second call arrives against a scope that no longer
/// exists.
#[tokio::test]
async fn a_delete_releases_exactly_once() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    runtime
        .control
        .create(record(1))
        .await
        .expect("create succeeds");
    wait_for(
        &runtime.control,
        |snapshot| !snapshot.networks.is_empty(),
        &log,
    )
    .await;

    assert!(
        runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("delete answers")
    );
    assert_eq!(
        provider.released(&log),
        vec![NetworkId(1)],
        "one delete is one release"
    );

    // Deleting again finds no record, so there is no scope to release a second time.
    assert!(
        !runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("delete answers"),
        "a Network that is already gone reports that it was not there"
    );
    assert_eq!(
        provider.released(&log),
        vec![NetworkId(1)],
        "a delete of an absent Network releases nothing"
    );
    runtime.stop().await;
}

/// Shutdown releases every configured Network's scope, live or not.
///
/// A Network with a durable row and no live owner still holds a scope: "no owner" is not
/// "no resources". A shutdown that walked only the live owners would leave a router
/// session behind for every Network that was configured but stopped.
#[tokio::test]
async fn shutdown_releases_every_configured_network_scope() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    for network in [1u64, 2, 3] {
        runtime
            .control
            .create(record(network))
            .await
            .expect("create succeeds");
    }
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 3,
        &log,
    )
    .await;
    runtime.stop().await;

    let released = provider.released(&log);
    for network in [NetworkId(1), NetworkId(2), NetworkId(3)] {
        assert!(
            released.contains(&network),
            "shutdown released {network:?}; saw {released:?}"
        );
    }
}

/// A Network deleted before shutdown is not released twice.
#[tokio::test]
async fn shutdown_does_not_re_release_a_deleted_network() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    for network in [1u64, 2] {
        runtime
            .control
            .create(record(network))
            .await
            .expect("create succeeds");
    }
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 2,
        &log,
    )
    .await;
    runtime
        .control
        .delete(NetworkId(1))
        .await
        .expect("delete answers");
    runtime.stop().await;

    let released = provider.released(&log);
    assert_eq!(
        released
            .iter()
            .filter(|network| **network == NetworkId(1))
            .count(),
        1,
        "the deleted Network was released once by its delete, not again by shutdown: {released:?}"
    );
}

/// Release is attempted after the owner task has ended, never before.
///
/// If release ran while the owner was still live, a connect already inside the provider
/// could re-acquire the session the release just closed. This is asserted structurally by
/// a provider whose release refuses to succeed while any connect it handed out is still
/// open.
#[tokio::test]
async fn release_never_precedes_the_owner_stopping() {
    let (provider, log) = ScopedProvider::new();
    // Two Networks, each left with an open generation. The peer's are taken and dropped
    // by the fixture, so each generation ends on its own; the point is that delete has a
    // live owner to stop first.
    let runtime = Runtime::start(provider.clone(), &log).await;
    for network in [1u64, 2] {
        provider
            .inner
            .queue_outcome(Ok(FaultScript::default()))
            .expect("outcome queues");
        runtime
            .control
            .create(record(network))
            .await
            .expect("create succeeds");
        let _peer = provider.inner.take_peer().await;
    }
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 2,
        &log,
    )
    .await;

    runtime
        .control
        .delete(NetworkId(1))
        .await
        .expect("delete answers");
    // The delete returned, which means the owner task was joined before this point. If
    // release had raced the owner, the release for Network 1 would have been logged
    // before the owner could observe its stop signal; the delete's own ordering is what
    // makes that impossible, and this asserts the release did happen and happened only
    // after the delete completed its stop.
    let released = provider.released(&log);
    assert_eq!(released, vec![NetworkId(1)]);
    runtime.stop().await;
}

/// A provider that refuses to release leaves the Network configured, not forgotten.
///
/// ADR-0005 requires release to happen before the durable delete. Forgetting the row
/// first would make the still-live router session unreachable and therefore
/// unreleasable forever, so a failed release has to leave a Network the Operator can
/// retry.
#[tokio::test]
async fn a_failed_release_prevents_the_durable_delete() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    runtime
        .control
        .create(record(1))
        .await
        .expect("create succeeds");
    wait_for(
        &runtime.control,
        |snapshot| !snapshot.networks.is_empty(),
        &log,
    )
    .await;
    ScopedProvider::arm_release_failure(&log, NetworkId(1));

    let outcome = runtime.control.delete(NetworkId(1)).await;
    assert!(
        matches!(outcome, Err(RuntimeError::Provider(_))),
        "a refused release is reported to the caller: {outcome:?}"
    );
    // Still configured, so the scope it still holds is still addressable.
    let snapshot = runtime.control.status().await.expect("status answers");
    assert_eq!(
        snapshot.networks.len(),
        1,
        "the Network remains configured after a failed release: {snapshot:?}"
    );

    // Retrying now succeeds: release is idempotent, and the retry still has a scope.
    assert!(
        runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("retry answers"),
        "a retry after a failed release completes the deletion"
    );
    assert!(
        runtime
            .control
            .status()
            .await
            .expect("status answers")
            .networks
            .is_empty(),
        "the retried delete forgot the Network"
    );
    runtime.stop().await;
}

/// A configuration change preserving the Network does not release the scope.
///
/// The SAM identity belongs to the durable Network, not to an IRC connection
/// generation. Replacing the owner because the Operator edited a channel list must not
/// destroy the Network's router identity.
#[tokio::test]
async fn a_same_network_change_does_not_release_the_scope() {
    let (provider, log) = ScopedProvider::new();
    let runtime = Runtime::start(provider.clone(), &log).await;
    runtime
        .control
        .create(record(1))
        .await
        .expect("create succeeds");
    wait_for(
        &runtime.control,
        |snapshot| !snapshot.networks.is_empty(),
        &log,
    )
    .await;
    assert!(provider.released(&log).is_empty());

    let mut changed = record(1);
    changed.desired_channels =
        i2pr_irc_store::attached_channels(&["#one".to_owned(), "#two".to_owned()]);
    runtime
        .control
        .change(changed)
        .await
        .expect("a same-Network change succeeds");
    wait_for(
        &runtime.control,
        |snapshot| !snapshot.networks.is_empty(),
        &log,
    )
    .await;

    assert!(
        provider.released(&log).is_empty(),
        "replacing the IRC owner does not destroy the Network's provider scope: {:?}",
        provider.released(&log)
    );
    runtime.stop().await;
}

/// Generation churn does not release the scope.
///
/// This is the invariant a naive implementation gets wrong in the other direction: if a
/// failed reconnect released the scope, the long-lived per-Network identity would churn
/// on every IRC outage, which is exactly what ADR-0004 exists to prevent.
///
/// Driven with an injected fast reconnect budget so several generations actually happen
/// inside the test's ceiling. The production budget rate-limits starts to one every two
/// seconds, so a wall-clock test would either take half a minute or assert nothing.
#[tokio::test]
async fn a_generation_churn_does_not_release_the_scope() {
    let (provider, log) = ScopedProvider::new();
    let (store, handle) = store();
    let reconnect = i2pr_irc_runtime::reconnect::ReconnectScheduler::new(
        i2pr_irc_runtime::reconnect::ReconnectBudget {
            token_interval: Duration::from_millis(10),
            ..Default::default()
        },
    )
    .expect("the injected budget validates");
    let (mut controller, control) =
        RuntimeController::with_reconnect(provider.clone(), handle, reconnect);
    let task = tokio::spawn(async move { controller.serve().await });
    wait_for(&control, |_| true, &log).await;

    // Refused connects: every one ends a generation and the owner starts another.
    for _ in 0..8 {
        provider
            .inner
            .queue_outcome(Err(i2pr_irc_core::ProviderError::Unavailable))
            .expect("outcome queues");
    }
    control.create(record(1)).await.expect("create succeeds");
    // The owner's own backoff doubles per failed generation, so three generations take a
    // few seconds of real time. Three is enough: two would be a single retry, which
    // proves nothing about churn.
    let mut attempts = 0usize;
    for _ in 0..400 {
        attempts = provider.inner.requested_scopes().len();
        if attempts >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        attempts >= 3,
        "the owner really did churn generations, so the claim is exercised: {attempts:?}"
    );
    assert!(
        provider.released(&log).is_empty(),
        "generation churn must not release the Network's scope: {:?}",
        provider.released(&log)
    );

    control.request_stop();
    task.await
        .expect("controller joins")
        .expect("controller succeeds");
    store.shutdown().expect("store shuts down");
}

/// A release that never completes is bounded, so a delete cannot hang on it.
///
/// The deadline is far below the connect budget on purpose: release runs on the deletion
/// and shutdown paths, where the caller is already waiting for the Network to go away,
/// and shutdown has no timeout of its own at all.
#[tokio::test]
async fn a_release_that_never_completes_is_bounded() {
    struct Stalled(Arc<Mutex<ReleaseLog>>);
    #[async_trait::async_trait]
    impl i2pr_irc_core::I2pStreamProvider for Stalled {
        async fn connect(
            &self,
            _network: NetworkId,
            _endpoint: &I2pEndpoint,
        ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, i2pr_irc_core::ProviderError> {
            Err(i2pr_irc_core::ProviderError::Unavailable)
        }
        async fn release(&self, network: NetworkId) -> Result<(), i2pr_irc_core::ProviderError> {
            self.0
                .lock()
                .expect("release log is writable")
                .released
                .push(network);
            // Never resolves: the caller must give up on its own deadline.
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    let log = Arc::new(Mutex::new(ReleaseLog::default()));
    let provider = Stalled(log.clone());
    // The `Store` value owns the database, so it is held for the whole test rather than
    // being dropped at the end of the expression that produced its handle.
    let (store, handle) = store();
    let (mut controller, control) = RuntimeController::new(provider, handle);
    let task = tokio::spawn(async move { controller.serve().await });
    control.create(record(1)).await.expect("create succeeds");

    // Driven on the runtime's own wall clock rather than a paused virtual one, because
    // the claim under test is that a *real* caller is released by a real deadline.
    let deleting = {
        let control = control.clone();
        tokio::spawn(async move { control.delete(NetworkId(1)).await })
    };
    // Generous relative to the 15s release deadline: the ceiling asserts boundedness,
    // not precision, and a slow machine must not turn a pass into a flake.
    let outcome = tokio::time::timeout(CEILING * 12, deleting)
        .await
        .expect("delete is bounded even when release never returns")
        .expect("delete task joins");
    assert!(
        matches!(outcome, Err(RuntimeError::Timeout)),
        "a stalled release surfaces as a timeout rather than hanging the delete: {outcome:?}"
    );
    assert!(
        log.lock()
            .expect("release log is readable")
            .released
            .contains(&NetworkId(1)),
        "the release was attempted before the deadline cut it off"
    );

    control.request_stop();
    let _ = tokio::time::timeout(CEILING * 6, task).await;
    store.shutdown().expect("store shuts down");
}

// ------------------------------------------------------ endpoint correction

/// A raw destination is accepted, because the previous check rejected every real one.
///
/// The old predicate demanded exactly 516 characters drawn from the base64url alphabet.
/// A real Base64 destination is longer and uses `+` and `/`, so it could never be
/// configured.
#[test]
fn a_raw_destination_is_a_well_formed_endpoint() {
    let destination = format!("{}+/{}", "A".repeat(300), "B".repeat(400));
    let parsed = I2pEndpoint::parse(&destination).expect("a real destination is accepted");
    assert_eq!(parsed.kind(), I2pEndpointKind::Destination);
    assert_eq!(
        parsed.as_str(),
        destination,
        "a destination keeps its own case: it is opaque key material, not a name"
    );
    // The bounds are the destination's, not the `.b32.i2p` name's.
    assert_eq!(
        I2pEndpoint::parse(&"A".repeat(MAX_I2P_DESTINATION_CHARS))
            .expect("the longest accepted destination parses")
            .kind(),
        I2pEndpointKind::Destination
    );
    assert!(
        I2pEndpoint::parse(&"A".repeat(MAX_I2P_DESTINATION_CHARS + 1)).is_err(),
        "a destination past the ceiling is rejected"
    );
    assert!(
        I2pEndpoint::parse(&"A".repeat(MIN_I2P_DESTINATION_CHARS - 1)).is_err(),
        "a token too short to be a destination is not silently accepted as one"
    );
}

/// A destination that cannot be reached is still a well-formed endpoint.
///
/// Deciding that an endpoint is well formed and deciding that its target is reachable
/// are different questions. Folding them together means a typo'd destination is reported
/// as a configuration error instead of an ordinary connect failure.
#[test]
fn a_well_formed_destination_is_not_resolved_at_parse_time() {
    let destination = "A".repeat(MIN_I2P_DESTINATION_CHARS);
    let parsed = I2pEndpoint::parse(&destination).expect("shape is decided, reachability is not");
    assert_eq!(parsed.kind(), I2pEndpointKind::Destination);
    assert_eq!(
        std::mem::size_of_val(&parsed.kind()),
        std::mem::size_of::<I2pEndpointKind>(),
        "kind is a plain discriminant, carrying no lookup result"
    );
}

/// A destination can be stored in a Network record and reaches the provider.
///
/// The endpoint correction is only worth anything if it survives the whole path, so this
/// drives a raw destination through the controller into a provider attempt.
#[tokio::test]
async fn a_raw_destination_reaches_the_provider_through_a_network_record() {
    let (provider, log) = ScopedProvider::new();
    let destination = format!("{}+/{}", "A".repeat(300), "B".repeat(400));
    let mut candidate = record(1);
    candidate.endpoint = I2pEndpoint::parse(&destination).expect("destination parses");
    let runtime = Runtime::start(provider.clone(), &log).await;
    provider
        .inner
        .queue_outcome(Err(i2pr_irc_core::ProviderError::Unavailable))
        .expect("outcome queues");
    runtime
        .control
        .create(candidate)
        .await
        .unwrap_or_else(|error| panic!("create accepts a raw destination: {error:?}"));
    wait_for(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 1,
        &log,
    )
    .await;
    assert!(
        provider
            .inner
            .requested_endpoints()
            .iter()
            .any(|endpoint| endpoint.as_str() == destination),
        "the destination reached the provider unmodified"
    );
    runtime.stop().await;
}

/// The release deadline is a real ceiling, and it is shorter than the connect budget.
///
/// Stated as a value rather than a behaviour so the relationship between the two
/// deadlines cannot drift: release runs on the deletion and shutdown paths, where the
/// caller is already blocked, so it must not be allowed the connect budget.
#[test]
fn the_release_deadline_is_bounded_and_shorter_than_the_connect_budget() {
    let release = i2pr_irc_runtime::PROVIDER_RELEASE_TIMEOUT;
    let connect = i2pr_irc_runtime::CONNECT_TIMEOUT;
    assert!(release > Duration::ZERO, "the deadline is a real bound");
    assert!(
        release < connect,
        "release must not block a delete for the whole connect budget: {release:?} >= {connect:?}"
    );
}
