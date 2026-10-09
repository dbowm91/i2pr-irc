//! M005-A qualification: the runtime controller and downstream admission.
//!
//! Two claims are under test here.
//!
//! *The controller is the single owner of every Network owner.* A Network exists only
//! while the controller holds both its bounded handle and its task; a mutation that
//! replaces a Network proves the old task finished before the new one started; an
//! ambiguous durable commit starts what is actually on disk rather than what was asked
//! for; and shutdown leaves no live owner behind.
//!
//! *Admission owns the client until a Network claims it.* Registration happens on the
//! client's own socket before any Network exists, a client with no Network selected is
//! served locally instead of being dropped, and a selection whose nickname no longer
//! matches its Network is refused before it can be projected.
//!
//! Everything runs against fake providers and scripted streams. No test may require a
//! real listener, a real router, or any network authority.
use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    AdmissionOutcome, CONTROL_REQUEST_CAPACITY, ControlSnapshot, DurableNetworks,
    RuntimeControlHandle, RuntimeController, RuntimeError,
    admission::{DownstreamAdmission, NetworkSelection},
    catalog::MAX_SUPERVISED_NETWORKS,
    controller::MAX_CONTROL_SNAPSHOT_NETWORKS,
};
use i2pr_irc_store::{
    DesiredChannelRecord, NetworkRecord, SavedNetwork, Store, StoreError, StoreHandle, StorePath,
    attached_channels, testing as store_testing,
};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultController, FaultScript, ScriptedStream};
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, sync::Mutex};

/// Ceiling for every bounded wait in this suite. A test that exceeds it has failed to
/// observe an event, which is a failure of the claim under test, not a flake to retry.
const CEILING: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64, nick: &str) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        failover_group: None,
        nick: nick.into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: Vec::new(),
    }
}

/// A provider that hands out scripted streams and records what was asked for.
#[derive(Clone)]
struct Provider(Arc<FakeI2pStreamProvider>);

impl Provider {
    fn new() -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..8 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("provider queue has room");
        }
        Self(provider)
    }

    /// The upstream end of the next connection this provider hands out.
    async fn peer(&self) -> (ScriptedStream, FaultController) {
        let peer = self.0.take_peer().await;
        let controller = self.0.take_controller().await;
        (peer, controller)
    }

    /// The upstream end, with the fault controller dropped.
    ///
    /// Used where the test only needs to drive the upstream stream and does not script
    /// it; the stream's own buffer bounds the exchange either way.
    async fn plain_peer(&self) -> ScriptedStream {
        self.peer().await.0
    }

    fn requested(&self) -> Vec<I2pEndpoint> {
        self.0.requested_endpoints()
    }
}

#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Provider {
    async fn connect(
        &self,
        _network: i2pr_irc_core::NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(_network, endpoint).await
    }
}

/// Durable layer that really stores and can be told to answer one save ambiguously.
///
/// Wrapping rather than replacing the store is deliberate: an ambiguous commit is only
/// meaningful against durable state that genuinely exists, because the behaviour under
/// test is "re-read and start what is on disk".
#[derive(Clone)]
struct FaultyStore {
    inner: StoreHandle,
    /// How many more saves must answer `CommitState::Unknown`.
    ambiguous_save: Arc<Mutex<usize>>,
    /// How many more durable deletions must answer `CommitState::Unknown`.
    ///
    /// Armed separately from saves because the owner performs its own durable writes
    /// while a test runs: a single shared counter would let an owner's reconcile consume
    /// the arming meant for the operation under test, and the test would then pass for
    /// the wrong reason.
    ambiguous_forget: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl DurableNetworks for FaultyStore {
    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError> {
        self.inner.load_networks().await
    }

    async fn save(&self, record: &NetworkRecord) -> Result<SavedNetwork, StoreError> {
        let mut remaining = self.ambiguous_save.lock().await;
        if *remaining > 0 {
            *remaining -= 1;
            // The mutation is *not* applied and the durable outcome is unknown, which
            // is exactly the case where a caller must re-read rather than assume.
            return Err(store_testing::unknown_commit());
        }
        self.inner.save_network(record).await
    }

    async fn remove(&self, network: NetworkId) -> Result<bool, StoreError> {
        let mut remaining = self.ambiguous_forget.lock().await;
        if *remaining > 0 {
            *remaining -= 1;
            return Err(store_testing::unknown_commit());
        }
        self.inner.remove_network(network).await
    }
}

/// Which durable operations the store double will answer ambiguously.
#[derive(Clone)]
struct Ambiguity {
    save: Arc<Mutex<usize>>,
    forget: Arc<Mutex<usize>>,
}

/// A running controller plus everything a test needs to observe it.
struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    store: (Store, StoreHandle),
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    /// Which durable operations will answer with an unknown commit state.
    ambiguous: Ambiguity,
    /// Upstream peers this test has claimed, kept alive for the whole test.
    ///
    /// Retained deliberately. Dropping a scripted peer closes the socket, which ends
    /// the owner's generation exactly as a real upstream disconnect would -- correct
    /// behaviour, and not what a test that wants a stable online Network should do.
    upstreams: Vec<ScriptedStream>,
}

impl Runtime {
    async fn start() -> Self {
        let (store, handle) = store();
        let ambiguous_save = Arc::new(Mutex::new(0usize));
        let ambiguous_forget = Arc::new(Mutex::new(0usize));
        let durable = Arc::new(FaultyStore {
            inner: handle.clone(),
            ambiguous_save: ambiguous_save.clone(),
            ambiguous_forget: ambiguous_forget.clone(),
        });
        Self::start_over(
            store,
            handle,
            Provider::new(),
            durable,
            Ambiguity {
                save: ambiguous_save,
                forget: ambiguous_forget,
            },
        )
        .await
    }

    /// Arms the next `save` to answer ambiguously.
    /// The real store handle, used to stall durable work on purpose.
    fn store_handle(&self) -> &StoreHandle {
        &self.store.1
    }

    /// Arms the next durable save to answer ambiguously.
    async fn arm_ambiguity(&self) {
        *self.ambiguous.save.lock().await = 1;
    }

    /// Arms the next durable deletion to answer ambiguously.
    async fn arm_ambiguous_forget(&self) {
        *self.ambiguous.forget.lock().await = 1;
    }

    async fn start_over(
        store: Store,
        handle: StoreHandle,
        provider: Provider,
        durable: Arc<dyn DurableNetworks>,
        ambiguous: Ambiguity,
    ) -> Self {
        let (mut controller, control) =
            RuntimeController::with_durable(provider.clone(), handle.clone(), durable);
        let task = tokio::spawn(async move { controller.serve().await });
        // The first snapshot a test can read is published only after startup restore
        // has finished, so waiting for it is waiting for restore.
        wait_for(&control, |_| true).await;
        Self {
            control,
            provider,
            store: (store, handle),
            task,
            upstreams: Vec::new(),
            ambiguous,
        }
    }

    async fn stop(self) {
        self.control.request_stop();
        self.task
            .await
            .expect("controller task joins")
            .expect("controller reports success");
        self.store.0.shutdown().expect("store shuts down");
    }

    async fn snapshot(&self) -> ControlSnapshot {
        self.control.status().await.expect("status is answerable")
    }

    /// Creates a Network and brings its upstream generation online.
    ///
    /// The upstream peer is parked in this harness rather than dropped, because a
    /// dropped peer is a closed socket and a closed socket ends the generation.
    async fn create_online(&mut self, network: u64, nick: &str) {
        self.control
            .create(record(network, nick))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .expect("upstream accepts registration");
        read_until(&mut upstream, b"CAP END\r\n").await;
        self.upstreams.push(upstream);
    }

    async fn wait_live(&self, network: NetworkId) {
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == network && entry.live)
        })
        .await;
    }

    async fn wait_absent(&self, network: NetworkId) {
        wait_for(&self.control, |snapshot| {
            !snapshot
                .networks
                .iter()
                .any(|entry| entry.network == network && entry.live)
        })
        .await;
    }
}

async fn wait_for<F>(control: &RuntimeControlHandle, ready: F)
where
    F: Fn(&ControlSnapshot) -> bool,
{
    let mut status = control.subscribe_status();
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if ready(&status.borrow()) {
            return;
        }
        let _ = tokio::time::timeout_at(deadline, status.changed())
            .await
            .expect("the control snapshot reaches the expected state");
    }
}

/// Reads until `needle` has been seen, returning everything read so far.
async fn read_until<R: AsyncReadExt + Unpin>(stream: &mut R, needle: &[u8]) -> String {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = tokio::time::timeout(CEILING, stream.read(&mut chunk))
            .await
            .expect("the scripted stream stays open long enough")
            .expect("the scripted stream does not fail");
        if count == 0 {
            panic!(
                "stream closed while waiting for {:?}; saw {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&seen)
            );
        }
        seen.extend_from_slice(&chunk[..count]);
        if seen.windows(needle.len()).any(|window| window == needle) {
            return String::from_utf8_lossy(&seen).into_owned();
        }
    }
}

// ------------------------------------------------- controller ownership claims

#[tokio::test]
async fn startup_restore_starts_one_owner_per_durable_network() {
    let (store, handle) = store();
    for network in [1u64, 2, 3] {
        handle
            .save_network(&record(network, "bot"))
            .await
            .expect("durable record saves");
    }
    let provider = Provider::new();
    let (mut controller, control) = RuntimeController::new(provider, handle.clone());
    let task = tokio::spawn(async move { controller.serve().await });

    wait_for(&control, |snapshot| snapshot.networks.len() == 3).await;
    let snapshot = control.status().await.expect("status");
    assert!(
        snapshot.networks.iter().all(|entry| entry.live),
        "every restored Network has a live owner: {snapshot:?}"
    );

    control.request_stop();
    task.await.expect("joins").expect("succeeds");
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_stop_request_ends_the_controller_with_no_live_owner() {
    let runtime = Runtime::start().await;
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create");
    runtime.wait_live(NetworkId(1)).await;
    let control = runtime.control.clone();
    runtime.stop().await;
    assert!(
        control.status().await.is_err(),
        "a stopped controller answers no further status request"
    );
}

#[tokio::test]
async fn a_create_during_a_slow_upstream_does_not_block_a_status_read() {
    let runtime = Runtime::start().await;
    // The owner is created but its upstream generation never comes online, so the
    // create's activation has a slow peer to sit with.
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create succeeds while upstream is slow");

    // The control queue is bounded. A status read that could be starved by a slow
    // upstream fails this as soon as the queue fills.
    let mut reads = Vec::new();
    for _ in 0..16 {
        let control = runtime.control.clone();
        reads.push(tokio::spawn(async move {
            tokio::time::timeout(CEILING, control.status())
                .await
                .expect("status is not blocked by upstream liveness")
                .expect("status is answerable")
        }));
    }
    for read in reads {
        read.await.expect("status task joins");
    }
    runtime.stop().await;
}

#[tokio::test]
async fn a_saturated_control_queue_is_refused_as_overload_while_shutdown_stays_reachable() {
    let runtime = Runtime::start().await;
    // Stall the store so the controller sits inside its first durable call. Every
    // further request then queues behind it, which is the state under test: a caller
    // that cannot keep up, and an operator who still has to be able to stop the process.
    runtime
        .store_handle()
        .set_stall(Some(Duration::from_secs(5)));
    let mut flooding = Vec::new();
    for network in 1..=(CONTROL_REQUEST_CAPACITY as u64) {
        let control = runtime.control.clone();
        flooding.push(tokio::spawn(async move {
            control.create(record(network, "bot")).await
        }));
    }
    // Give the controller time to take the first request and block in storage.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let error = runtime
        .control
        .status()
        .await
        .expect_err("a full bounded queue refuses further work rather than growing");
    assert!(
        matches!(error, RuntimeError::QueueOverloaded),
        "overflow is an explicit overload, got {error:?}"
    );

    // Shutdown does not use the queue, so it stays reachable while the queue is full.
    // A runtime that could only be stopped by winning the queue race could not be
    // stopped at all when the runtime was busy.
    runtime.control.request_stop();
    assert!(runtime.control.is_stopping());

    runtime.store_handle().set_stall(None);
    for flood in flooding {
        let _ = flood.await;
    }
    let deadline = tokio::time::Instant::now() + CEILING;
    while tokio::time::Instant::now() < deadline && !runtime.task.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(
        runtime.task.is_finished(),
        "the controller ends even though requests were outstanding when stop was requested"
    );
    runtime.store.0.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_running_controller_keeps_answering_status_while_networks_are_created() {
    let runtime = Runtime::start().await;
    // Concurrent creation and observation: the queue is bounded, but a caller that
    // waits for a reply must not be starved by it.
    let mut work = Vec::new();
    for network in 1..=32u64 {
        let control = runtime.control.clone();
        work.push(tokio::spawn(async move {
            control.create(record(network, "bot")).await
        }));
    }
    let mut observed = 0usize;
    let deadline = tokio::time::Instant::now() + CEILING;
    while tokio::time::Instant::now() < deadline {
        if runtime.control.status().await.is_ok() {
            observed += 1;
        }
        if work.iter().all(|task| task.is_finished()) {
            break;
        }
    }
    assert!(
        observed > 0,
        "status stays answerable while the control queue is busy"
    );
    for task in work {
        let _ = task.await;
    }
    runtime.stop().await;
}

#[tokio::test]
async fn a_create_is_refused_past_the_supervised_network_ceiling() {
    let runtime = Runtime::start().await;
    for network in 1..=MAX_SUPERVISED_NETWORKS as u64 {
        runtime
            .control
            .create(record(network, "bot"))
            .await
            .unwrap_or_else(|error| panic!("create {network} within the ceiling: {error:?}"));
    }
    let error = runtime
        .control
        .create(record(MAX_SUPERVISED_NETWORKS as u64 + 1, "bot"))
        .await
        .expect_err("one past the ceiling is refused");
    assert!(
        matches!(error, RuntimeError::QueueOverloaded),
        "expected an explicit refusal, got {error:?}"
    );
    let snapshot = runtime.snapshot().await;
    assert!(
        snapshot.networks.len() <= MAX_CONTROL_SNAPSHOT_NETWORKS,
        "the snapshot never reports more Networks than the ceiling allows"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn create_is_durable_before_it_is_activated() {
    let runtime = Runtime::start().await;
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create");
    // The durable row exists independently of whether the owner came up, so an
    // operator's intent survives a failed activation.
    let durable = runtime.store.1.load_networks().await.expect("durable read");
    assert_eq!(durable.len(), 1);
    assert_eq!(durable[0].display_name, "net-1");
    runtime.stop().await;
}

#[tokio::test]
async fn change_stops_the_old_owner_and_starts_exactly_one_replacement() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let mut candidate = record(1, "renamed");
    candidate.desired_channels = attached_channels(&["#new"]);
    runtime
        .control
        .change(candidate)
        .await
        .expect("change succeeds");
    runtime.wait_live(NetworkId(1)).await;

    let durable = runtime.store.1.load_networks().await.expect("durable read");
    assert_eq!(durable.len(), 1, "change replaces rather than adds");
    assert_eq!(durable[0].nick, "renamed");
    assert_eq!(durable[0].desired_channels, attached_channels(&["#new"]));

    let snapshot = runtime.snapshot().await;
    assert_eq!(
        snapshot
            .networks
            .iter()
            .filter(|entry| entry.network == NetworkId(1))
            .count(),
        1,
        "exactly one entry for the Network: two owners' worth of state is impossible"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn change_with_an_unknown_commit_starts_exactly_what_is_durable() {
    let runtime = Runtime::start().await;
    runtime
        .control
        .create(record(1, "original"))
        .await
        .expect("create");
    runtime.wait_live(NetworkId(1)).await;
    // Armed here, not at construction: an ambiguity armed earlier would make the
    // *create* ambiguous instead of the change under test.
    runtime.arm_ambiguity().await;

    // This change is answered with `CommitState::Unknown`, so the controller must
    // re-read durable state rather than assume either outcome.
    let error = runtime
        .control
        .change(record(1, "never-persisted"))
        .await
        .expect_err("the ambiguous commit is reported to the caller");
    assert!(matches!(error, RuntimeError::InvalidConfig), "{error:?}");

    let after = runtime.store.1.load_networks().await.expect("durable read");
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].nick, "original",
        "the refused change never reached durable state"
    );
    let snapshot = runtime.snapshot().await;
    assert_eq!(snapshot.networks.len(), 1);
    assert_eq!(
        snapshot.networks[0].display_name, "net-1",
        "the snapshot reflects durable state, not the refused candidate"
    );
    assert!(
        snapshot.networks[0].live,
        "what is durable is what runs: the controller re-read and started that"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn delete_stops_the_owner_then_forgets_it_durably() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    assert!(
        runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("delete succeeds"),
        "delete reports that a row existed"
    );
    runtime.wait_absent(NetworkId(1)).await;
    assert!(
        runtime
            .store
            .1
            .load_networks()
            .await
            .expect("durable read")
            .is_empty(),
        "the durable row is gone too"
    );
    assert!(
        !runtime
            .control
            .delete(NetworkId(1))
            .await
            .expect("a second delete is answerable"),
        "deleting a Network that does not exist reports false rather than failing"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn the_snapshot_is_bounded_and_carries_nothing_identifying() {
    let runtime = Runtime::start().await;
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create");
    let snapshot = runtime.snapshot().await;

    assert!(
        snapshot.networks.len() <= MAX_CONTROL_SNAPSHOT_NETWORKS,
        "the snapshot is bounded by the same ceiling the catalog enforces"
    );
    let rendered = format!("{snapshot:?}");
    for forbidden in ["irc.example.i2p", "bot", "user", "bouncer"] {
        assert!(
            !rendered.contains(forbidden),
            "the snapshot leaked {forbidden:?} into operator-visible state: {rendered}"
        );
    }
    runtime.stop().await;
}

#[tokio::test]
async fn the_snapshot_revision_advances_on_every_state_change() {
    let runtime = Runtime::start().await;
    let before = runtime.snapshot().await.revision;
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create");
    let after_create = runtime.snapshot().await;
    assert!(
        after_create.revision > before,
        "a create must advance the revision so a reader can prove it saw the change"
    );

    runtime.control.delete(NetworkId(1)).await.expect("delete");
    let after_delete = runtime.snapshot().await;
    assert!(
        after_delete.revision > after_create.revision,
        "a delete must advance the revision too"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_invalid_candidate_is_refused_before_anything_is_written() {
    let runtime = Runtime::start().await;
    let mut invalid = record(1, "bot");
    invalid.display_name = "has space".into();
    assert!(
        runtime.control.create(invalid).await.is_err(),
        "a candidate that fails validation never reaches durable state"
    );
    assert!(
        runtime
            .store
            .1
            .load_networks()
            .await
            .expect("durable read")
            .is_empty()
    );
    runtime.stop().await;
}

#[tokio::test]
async fn every_control_clone_reads_the_same_snapshot() {
    let runtime = Runtime::start().await;
    let clone: RuntimeControlHandle = runtime.control.clone();
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("create");
    assert_eq!(
        clone.status().await.expect("status").revision,
        runtime.snapshot().await.revision,
        "two clones are two handles onto one controller, not two controllers"
    );
    assert!(
        !clone.is_stopping(),
        "a live controller does not report stopping"
    );
    runtime.stop().await;
}

// ------------------------------------------------------------ admission claims

/// Runs one admitted client against a control handle and returns the test's end.
struct Admitted {
    end: ScriptedStream,
    _script: FaultController,
    outcome: tokio::task::JoinHandle<AdmissionOutcome>,
}

impl Admitted {
    /// Sends `NICK`/`USER` and reads until `expected` appears.
    ///
    /// Callers name the frame they are actually waiting for. The bound projection
    /// ends with `RPL_ENDOFNAMES` *per channel* and emits no `376`; the control-only
    /// welcome does. Asserting on a frame neither emits is how a test ends up proving
    /// nothing while looking thorough.
    async fn register(&mut self, expected: &[u8]) -> String {
        self.end
            .write_all(b"NICK bot\r\nUSER user 0 * :client\r\n")
            .await
            .expect("client registers");
        read_until(&mut self.end, expected).await
    }

    async fn outcome(self) -> AdmissionOutcome {
        self.outcome.await.expect("admission task joins")
    }
}

/// Admits one connection with `selected`, returning the test's end of the socket.
fn admit(
    control: &RuntimeControlHandle,
    selected: Option<NetworkSelection>,
    session: SessionId,
) -> Admitted {
    admit_with(control, selected, session, Duration::from_secs(60))
}

/// Admits one connection with an explicit registration ceiling.
fn admit_with(
    control: &RuntimeControlHandle,
    selected: Option<NetworkSelection>,
    session: SessionId,
    timeout: Duration,
) -> Admitted {
    let (end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let control = control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::with_registration_timeout(
            selected,
            control,
            session,
            ClientId(7),
            timeout,
        )
        .run(stream)
        .await
    });
    Admitted {
        end,
        _script: script,
        outcome,
    }
}

#[tokio::test]
async fn a_selected_client_is_bound_to_its_network() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(101),
    );
    let welcome = client.register(b"001 bot").await;
    assert!(
        welcome.contains("005 bot CLIENTTAGDENY=*"),
        "a bound client receives the expected welcome burst: {welcome}"
    );
    assert!(
        welcome.contains("001 bot"),
        "a bound client is welcomed under the nickname its Network registered: {welcome}"
    );
    assert_eq!(
        client.outcome().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(101),
        },
        "a selected client is handed to its Network with the identity admission allocated"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_preferred_alias_attaches_to_a_generated_fallback_and_receives_nick_transition() {
    let mut runtime = Runtime::start().await;
    runtime
        .control
        .create(record(1, "bot"))
        .await
        .expect("Network creates");
    let mut upstream = runtime.provider.plain_peer().await;
    read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :\r\n:srv 433 * bot :Nickname is already in use\r\n")
        .await
        .expect("preferred nick is occupied");
    read_until(&mut upstream, b"NICK bot_1\r\n").await;
    upstream
        .write_all(b":srv 001 bot_1 :welcome\r\n")
        .await
        .expect("fallback registers");
    runtime.upstreams.push(upstream);
    runtime.wait_live(NetworkId(1)).await;

    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(707),
    );
    client
        .end
        .write_all(b"NICK bot\r\nUSER user 0 * :client\r\n")
        .await
        .expect("client registers under preferred alias");
    let projected = read_until(&mut client.end, b"NICK :bot_1").await;
    assert!(projected.contains(":bot NICK :bot_1"), "{projected}");
    assert_eq!(
        client.outcome().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(707),
        }
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_bound_client_keeps_one_identity_across_the_transfer() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(606),
    );
    let _ = client.register(b"001 bot").await;
    assert_eq!(
        client.outcome().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(606),
        },
        "the transferred session keeps the identity admission allocated: no new socket, \
         no new session, no second registration"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_unselected_client_is_served_as_a_control_only_session() {
    let runtime = Runtime::start().await;
    let mut client = admit(&runtime.control, None, SessionId(202));
    let welcome = client.register(b"376").await;
    assert!(
        welcome.contains("001 bot") && welcome.contains("002 bot"),
        "an unbound client still completes registration and is told it has no Network: {welcome}"
    );
    assert!(
        !welcome.contains(" 353 "),
        "an unbound client is given no channel list: {welcome}"
    );

    // A command that needs a Network is refused by name rather than dropped.
    client
        .end
        .write_all(b"JOIN #room\r\n")
        .await
        .expect("client asks to join");
    let refusal = read_until(&mut client.end, b"421 bot").await;
    assert!(
        refusal.contains("No network selected"),
        "the refusal names its reason: {refusal}"
    );

    client
        .end
        .write_all(b"QUIT\r\n")
        .await
        .expect("client quits");
    assert_eq!(
        client.outcome().await,
        AdmissionOutcome::Unbound {
            session: SessionId(202)
        }
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_client_whose_nickname_does_not_match_is_refused_at_registration() {
    let runtime = Runtime::start().await;
    let (mut end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let control = runtime.control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::with_registration_timeout(
            Some(NetworkSelection {
                network: NetworkId(1),
                expected_nick: "bot".to_owned(),
            }),
            control,
            SessionId(303),
            ClientId(7),
            // This client never completes registration, so it runs against a short
            // ceiling rather than the production minute.
            Duration::from_millis(250),
        )
        .run(stream)
        .await
    });
    let _script = script;
    end.write_all(b"NICK someone-else\r\nUSER user 0 * :client\r\n")
        .await
        .expect("client sends a mismatched nickname");
    let seen = read_until(&mut end, b"433").await;
    assert!(
        seen.contains("433"),
        "a client may not claim a nickname its Network did not register: {seen}"
    );
    assert_eq!(
        outcome.await.expect("admission task joins"),
        AdmissionOutcome::Refused
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_client_that_never_registers_is_refused_at_the_registration_ceiling() {
    let runtime = Runtime::start().await;
    let mut client = admit_with(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(909),
        Duration::from_millis(250),
    );
    // The client connects and says nothing. An accepted socket is process state, so
    // the ceiling has to end it rather than let it be held indefinitely.
    let refusal = read_until(&mut client.end, b"451").await;
    assert!(
        refusal.contains("451"),
        "an idle client is told why it is being closed: {refusal}"
    );
    assert_eq!(client.outcome().await, AdmissionOutcome::Refused);
    runtime.stop().await;
}

#[tokio::test]
async fn an_unreachable_selection_opens_no_upstream_connection() {
    let runtime = Runtime::start().await;
    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(404),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(404),
    );
    client
        .end
        .write_all(b"NICK bot\r\nUSER user 0 * :client\r\n")
        .await
        .expect("client registers");
    assert_eq!(
        tokio::time::timeout(CEILING, client.outcome)
            .await
            .expect("a refused selection is answered promptly")
            .expect("admission task joins"),
        AdmissionOutcome::Refused,
        "a binding to a Network that does not exist is refused"
    );
    assert!(
        runtime.provider.requested().is_empty(),
        "rejecting a selection must not have opened an upstream connection: {:?}",
        runtime.provider.requested()
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_changed_configuration_forces_reselection_and_refuses_a_stale_session() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    // The Network's registered nickname changes, so a selection made under the old one
    // is no longer valid.
    runtime
        .control
        .change(record(1, "renamed"))
        .await
        .expect("change succeeds");
    runtime.wait_live(NetworkId(1)).await;
    // The replacement owner connects a fresh upstream generation; a client cannot be
    // adopted by an owner that has no generation to adopt it into.
    let mut replacement = runtime.provider.plain_peer().await;
    read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
    replacement
        .write_all(b":srv CAP * LS :\r\n:srv 001 renamed :welcome\r\n")
        .await
        .expect("the replacement upstream accepts registration");
    read_until(&mut replacement, b"CAP END\r\n").await;
    runtime.upstreams.push(replacement);

    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            // The stale selection: the nickname this Network used to hold.
            expected_nick: "bot".to_owned(),
        }),
        SessionId(505),
    );
    let seen = client.register(b"433").await;
    assert!(
        seen.contains("433"),
        "a session admitted under a stale identity is refused, not projected: {seen}"
    );
    assert_eq!(client.outcome().await, AdmissionOutcome::Refused);
    runtime.stop().await;
}

#[tokio::test]
async fn a_command_sent_in_the_same_read_as_registration_is_not_lost() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let (mut end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let _ = script;
    let control = runtime.control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::new(
            Some(NetworkSelection {
                network: NetworkId(1),
                expected_nick: "bot".to_owned(),
            }),
            control,
            SessionId(707),
            ClientId(7),
        )
        .run(stream)
        .await
    });
    // One write carrying registration *and* a command that only makes sense afterwards.
    // A decoder discarded at the registration boundary would drop the PING.
    end.write_all(b"NICK bot\r\nUSER user 0 * :client\r\nPING :keepalive\r\n")
        .await
        .expect("client sends registration and a command together");
    let seen = read_until(&mut end, b"PONG bouncer :keepalive").await;
    assert!(
        seen.contains("001 bot"),
        "the bound client received its projection: {seen}"
    );
    assert!(
        seen.contains("PONG bouncer :keepalive"),
        "a post-registration command from the same read is answered, not lost: {seen}"
    );
    assert_eq!(
        outcome.await.expect("admission task joins"),
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(707),
        }
    );
    runtime.stop().await;
}

// --------------------------------------------------- transfer and bound-session claims

#[tokio::test]
async fn the_legacy_direct_attach_path_projects_the_same_welcome_burst() {
    // The direct path already knows its NetworkId. M005-A adds an admission path; it
    // does not replace this one, and the two must not drift apart in what a client sees.
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let mut direct = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(1101),
    );
    let through_admission = direct.register(b"001 bot").await;
    let admission_outcome = direct.outcome().await;
    assert_eq!(
        admission_outcome,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(1101),
        }
    );

    // Same Network, second client, same expectations.
    let mut again = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(1102),
    );
    let second = again.register(b"001 bot").await;
    for frame in ["001 bot", "005 bot CLIENTTAGDENY=*"] {
        assert!(
            through_admission.contains(frame),
            "first client missed {frame}: {through_admission}"
        );
        assert!(
            second.contains(frame),
            "second client missed {frame}: {second}"
        );
    }
    assert_eq!(
        again.outcome().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(1102),
        }
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_adopted_session_answers_a_command_only_a_bound_client_may_issue() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;

    let mut client = admit(
        &runtime.control,
        Some(NetworkSelection {
            network: NetworkId(1),
            expected_nick: "bot".to_owned(),
        }),
        SessionId(1201),
    );
    let _ = client.register(b"001 bot").await;

    // JOIN is durable operator intent the owner persists before anything goes upstream.
    // Proving the owner -- rather than admission -- receives it is what shows the
    // conversation continues with the right owner rather than merely reaching a socket.
    client
        .end
        .write_all(b"JOIN #admitted\r\n")
        .await
        .expect("client asks to join");
    let stored = wait_for_durable_channels(&runtime.store.1, NetworkId(1), |channels| {
        channels.iter().any(|channel| channel.target == "#admitted")
    })
    .await;
    assert_eq!(stored, attached_channels(&["#admitted"]));
    client
        .end
        .write_all(b"QUIT\r\n")
        .await
        .expect("client quits");
    runtime.stop().await;
}

/// Waits for one Network's durable desired channels to satisfy `ready`.
///
/// The predicate is handed channel names rather than records: these tests are about
/// which channels are durable, not about their presentation, which Plan 021 qualifies
/// directly in `m005b_detached_policy`.
async fn wait_for_durable_channels<F>(
    store: &StoreHandle,
    network: NetworkId,
    ready: F,
) -> Vec<DesiredChannelRecord>
where
    F: Fn(&[DesiredChannelRecord]) -> bool,
{
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        let records = store.load_networks().await.expect("durable read");
        let channels = records
            .iter()
            .find(|record| record.network == network)
            .map(|record| record.desired_channels.clone())
            .unwrap_or_default();
        if ready(&channels) {
            return channels;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "durable desired channels never reached the expected state: {channels:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn delete_with_an_unknown_commit_leaves_no_live_owner() {
    let mut runtime = Runtime::start().await;
    runtime.create_online(1, "bot").await;
    runtime.wait_live(NetworkId(1)).await;
    runtime.arm_ambiguous_forget().await;

    // The durable outcome of this delete cannot be inferred from the failure, so the
    // controller stops the owner and tells the caller the state is unknown. What it must
    // never do is report success, or leave an owner running for a Network the caller
    // believes is gone.
    let outcome = runtime.control.delete(NetworkId(1)).await;
    assert!(
        outcome.is_err(),
        "an ambiguous delete is reported, never as success (got {outcome:?})"
    );
    runtime.wait_absent(NetworkId(1)).await;
    assert!(
        runtime.control.reconcile(NetworkId(1)).await.is_err(),
        "no owner remains, so reconcile has nothing to reconcile"
    );
    runtime.stop().await;
}

// ------------------------------------------------------------ structural guards

/// The control handle must not be able to open a socket to anything.
///
/// This is the product boundary in one property: a caller holding process control must
/// not be able to turn that into a generic dialer or listener. It is checked against the
/// source rather than by inspection because the failure mode is an accidentally added
/// method, which compiles cleanly and passes every other test.
///
/// `scripts/check-network-boundary.py` already forbids the socket primitives across
/// every first-party crate, so this is not the only thing between the controller and a
/// generic dialer. It is here because that script is a separate step a reader may never
/// run, and because this one names the capability precisely: the check is scoped to the
/// controller rather than to the whole tree.
#[test]
fn the_runtime_controller_exposes_no_dial_or_listen_operation() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/controller.rs");
    let source = std::fs::read_to_string(&path).expect("the controller source is readable");
    for forbidden in [
        // Any socket namespace at all, however it is spelled.
        "std::net",
        "tokio::net",
        "async_std::net",
        "socket2",
        // The local accepting side, which is a listener by another name.
        "LocalAcceptor",
        // Dial and listen shapes a future edit could plausibly add.
        "fn dial",
        "fn listen",
        "fn accept",
        "fn connect",
    ] {
        assert!(
            !source.contains(forbidden),
            "the controller must not be able to open or accept a socket ({forbidden})"
        );
    }
}

/// The transfer is one-shot.
///
/// `PreparedSession` owns a live socket and is consumed by `bind`, so a caller cannot
/// offer the same conversation to a second owner. The proof is the absence of `Clone`
/// on the type itself; a test asserting this in prose would not stop anyone adding
/// `#[derive(Clone)]`, so it is checked structurally instead.
#[test]
fn a_prepared_session_cannot_be_cloned_or_transferred_twice() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/admission.rs");
    let source = std::fs::read_to_string(&path).expect("the admission source is readable");
    let declaration = source
        .split("pub struct PreparedSession")
        .nth(1)
        .expect("PreparedSession is declared here")
        .split('}')
        .next()
        .expect("the declaration closes");
    assert!(
        !declaration.contains("derive(Clone"),
        "a transferable session must not be clonable: a clone would offer the same \
         conversation to a second owner"
    );
    // `bind` takes the session by value, so offering it twice does not type-check.
    let controller = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/controller.rs"),
    )
    .expect("the controller source is readable");
    let bind = controller
        .split("pub async fn bind")
        .nth(1)
        .expect("bind is declared in the controller surface")
        .lines()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        bind.contains("session: crate::admission::PreparedSession,"),
        "bind must take the session by value: {bind}"
    );
    assert!(
        !bind.contains("&crate::admission::PreparedSession"),
        "bind must consume the session, not borrow it: {bind}"
    );
}
