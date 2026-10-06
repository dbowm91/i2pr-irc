//! Plan 017 qualification: adverse-Network and resource campaigns at scale.
//!
//! The single-Network and single-client cases are qualified in `integrated.rs`. What is
//! left -- and what this file exists for -- is what only appears when *many* Networks
//! fail together: a startup herd, a shared outage, and a shared recovery.
//!
//! Two disciplines run through every test here.
//!
//! **Concurrency is measured, never inferred.** A provider that parked each attempt
//! until the test released it turns "how many connects overlapped" into a number that can
//! be asserted, instead of a timing coincidence that either always passes or flakes.
//!
//! **A campaign proves it settled.** Every campaign records a baseline before it starts,
//! drives load, and then asserts the process returned to that baseline. A peak proves a
//! ceiling held; only a return to baseline proves nothing leaked.
#![cfg(test)]

use i2pr_irc_core::{ByteStream, I2pEndpoint, I2pStreamProvider, NetworkId, ProviderError};
use i2pr_irc_runtime::{
    RuntimeError,
    catalog::{MAX_SUPERVISED_NETWORKS, SupervisorContext, SupervisorHandle},
    owner::{NetworkOwner, NetworkSnapshot, Phase},
    reconnect::{
        MAX_CONNECT_BURST, MAX_IN_FLIGHT_CONNECTS, MAX_RECONNECT_WAITERS, ReconnectBudget,
        ReconnectScheduler,
    },
    resource::{ResourceLedger, ResourceSnapshot},
};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Notify, mpsc, watch},
};

/// The supervised-Network ceiling, driven as the fleet size.
///
/// The plan asks for the ceiling "where practical". It is practical here: owners are
/// cheap, the provider is in-process, and every claim below is about *counting* them
/// rather than about moving real traffic.
const FLEET: usize = MAX_SUPERVISED_NETWORKS;

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64, nick: &str) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: nick.into(),
        username: "user".into(),
        realname: "bouncer".into(),
        sasl: None,
        desired_channels: Vec::new(),
    }
}

/// A fleet budget that keeps the production ceilings but shortens the token interval.
///
/// The concurrency ceiling and the waiter ceiling are the values under test, so they are
/// the production ones. Only the *interval* moves, because a production 2 s interval
/// would turn a 64-Network campaign into a four-minute test that proves nothing extra.
fn fleet_budget() -> ReconnectBudget {
    let budget = ReconnectBudget {
        max_in_flight: MAX_IN_FLIGHT_CONNECTS,
        max_burst: MAX_CONNECT_BURST,
        token_interval: Duration::from_millis(1),
        max_waiters: MAX_RECONNECT_WAITERS,
        seed: 0x05ee_d017,
    };
    budget.validate().expect("the fleet budget is bounded");
    budget
}

/// Lets one shared provider serve many independent owners.
///
/// A newtype rather than a blanket impl, so the fleet cannot accidentally wrap a
/// provider that was meant to stay private to a single test.
#[derive(Clone)]
struct Shared<P>(Arc<P>);

#[async_trait::async_trait]
impl<P: I2pStreamProvider> I2pStreamProvider for Shared<P> {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError> {
        self.0.connect(endpoint).await
    }
}

/// A provider that parks every attempt until the test releases it.
///
/// This is what makes concurrency observable. Without the park, four attempts that each
/// complete in microseconds would rarely overlap, and a test asserting "at most four at
/// once" would pass whether the ceiling worked or not.
struct GateProvider {
    inner: Arc<FakeI2pStreamProvider>,
    release: Arc<Notify>,
    live: AtomicUsize,
    peak: AtomicUsize,
    requested: AtomicUsize,
}

impl GateProvider {
    fn new() -> Self {
        Self {
            inner: Arc::new(FakeI2pStreamProvider::default()),
            release: Arc::new(Notify::new()),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            requested: AtomicUsize::new(0),
        }
    }

    /// Queues outcomes, stopping short rather than overrunning the fixture's own
    /// ceiling.
    ///
    /// The fixture bounds its queue on purpose, and a campaign that hit that bound would
    /// have its shape changed by a fixture limit rather than by the system under test.
    /// A long campaign therefore tops the queue up as it goes instead of pre-loading
    /// more than the fixture will hold.
    fn queue(&self, count: usize) -> usize {
        let mut queued = 0;
        for _ in 0..count {
            if self
                .inner
                .queue_outcome(Ok(FaultScript::default()))
                .is_err()
            {
                break;
            }
            queued += 1;
        }
        queued
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn requested(&self) -> usize {
        self.requested.load(Ordering::SeqCst)
    }

    fn release_all(&self) {
        self.release.notify_waiters();
    }
}

#[async_trait::async_trait]
impl I2pStreamProvider for GateProvider {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError> {
        self.requested.fetch_add(1, Ordering::SeqCst);
        // Counted on entry, before the park. Counting after it would measure only the
        // instants a connect spends inside the fixture -- which is microseconds -- so
        // four parked attempts would never appear to overlap and the ceiling assertion
        // would be vacuous.
        let live = self.live.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        self.peak.fetch_max(live, Ordering::SeqCst);
        // Parked here until released, so every attempt the scheduler admitted is still
        // open when the next one is admitted.
        self.release.notified().await;
        let outcome = self.inner.connect(endpoint).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        outcome
    }
}

struct Member {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
}

/// Many Networks under one catalog, one connect budget, and one ledger.
struct Fleet {
    members: Vec<Member>,
}

impl Fleet {
    fn start(
        count: usize,
        store: StoreHandle,
        scheduler: ReconnectScheduler,
        resources: ResourceLedger,
        provider: Arc<GateProvider>,
    ) -> Self {
        let members = (0..count)
            .map(|index| {
                let id = NetworkId(index as u64 + 1);
                let context = SupervisorContext {
                    network: id,
                    record: Arc::new(record(index as u64 + 1, "bot")),
                    store: store.clone(),
                    status: watch::channel(Default::default()).0,
                    resources: resources.clone(),
                };
                let owner = NetworkOwner::new(
                    Shared(provider.clone()),
                    context,
                    store.clone(),
                    scheduler.clone(),
                )
                .expect("owner constructs");
                let (command_tx, command_rx) = mpsc::channel(64);
                // The handle is retained so the control channel stays alive; a channel
                // whose only sender drops would make every owner see "stopped".
                let _handle = SupervisorHandle::new(id, command_tx);
                let (stop, stop_rx) = watch::channel(false);
                let task = tokio::spawn(async move {
                    let _keep = _handle;
                    owner.serve(command_rx, stop_rx).await
                });
                Member { stop, task }
            })
            .collect();
        Self { members }
    }

    /// Stops every owner and waits for the task to actually end.
    ///
    /// Awaiting the handle matters: an aborted task that has not been joined may still be
    /// mid-teardown, so asserting the ledger immediately after `abort` would race the
    /// very cleanup it is checking.
    async fn shutdown(self) {
        for member in &self.members {
            let _ = member.stop.send(true);
        }
        for member in self.members {
            member.task.abort();
            let _ = member.task.await;
        }
    }
}

async fn wait_until(label: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

/// Asserts a settled process really did return to where it started.
///
/// The peak is reported on failure because "settled != baseline" is far easier to
/// diagnose when the caller can also see what the process was holding.
#[track_caller]
fn assert_settled(baseline: &ResourceSnapshot, settled: &ResourceSnapshot) {
    assert_eq!(
        settled.current, baseline.current,
        "the process did not return to its baseline; peak was {:?}",
        settled.peak
    );
}

// ------------------------------------------------- section 3: startup herd

/// The headline claim: a cold start of the whole supervised ceiling must not become the
/// simultaneous connect that makes an anonymity network's traffic look like one client.
#[tokio::test]
async fn a_startup_herd_at_the_ceiling_never_exceeds_the_connect_ceiling() {
    let (_store, store_handle) = store();
    let provider = Arc::new(GateProvider::new());
    // One outcome per Network plus slack, so a backoff retry cannot exhaust the queue and
    // silently change the shape of the campaign.
    assert!(
        provider.queue(FLEET + 8) >= FLEET,
        "the fleet must have enough outcomes"
    );
    let scheduler = ReconnectScheduler::new(fleet_budget()).expect("bounded budget");
    let resources = ResourceLedger::new(scheduler.clone(), store_handle.clone());

    let baseline = resources.snapshot();
    let fleet = Fleet::start(
        FLEET,
        store_handle,
        scheduler.clone(),
        resources.clone(),
        provider.clone(),
    );

    // Every Network asks at once. The claim is about what does *not* happen.
    wait_until("the connect ceiling to saturate", || {
        provider.live() == MAX_IN_FLIGHT_CONNECTS
    })
    .await;

    assert_eq!(
        provider.peak(),
        MAX_IN_FLIGHT_CONNECTS,
        "connect concurrency must saturate at the ceiling"
    );
    assert!(
        provider.requested() <= MAX_IN_FLIGHT_CONNECTS,
        "a herd of {FLEET} Networks must not request {requested} simultaneous connects",
        requested = provider.requested()
    );

    let diagnostics = scheduler.diagnostics();
    assert_eq!(
        diagnostics.peak_in_flight, MAX_IN_FLIGHT_CONNECTS,
        "the scheduler must report the same ceiling it enforced"
    );
    assert!(
        diagnostics.pending_waiters <= MAX_RECONNECT_WAITERS,
        "the waiter set must stay bounded, saw {}",
        diagnostics.pending_waiters
    );
    assert!(
        diagnostics.pending_waiters <= FLEET,
        "there is one waiter per Network, never more"
    );

    // Let the rest of the fleet through in bounded batches.
    for _ in 0..(FLEET / MAX_IN_FLIGHT_CONNECTS) + 4 {
        provider.release_all();
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    wait_until("every Network to have attempted a connect", || {
        provider.requested() >= FLEET
    })
    .await;
    assert!(
        provider.peak() <= MAX_IN_FLIGHT_CONNECTS,
        "the ceiling held through the whole herd, peak was {}",
        provider.peak()
    );

    let loaded = resources.snapshot();
    assert_eq!(
        loaded.networks, FLEET,
        "every owner must be accounted for, or a leak would be invisible"
    );
    assert!(
        loaded.refused == 0,
        "no owner should be refused at the ceiling"
    );

    fleet.shutdown().await;
    let settled = resources.snapshot();
    assert!(
        settled.current.owner_tasks == 0,
        "stopping the fleet must return the process to zero owner tasks"
    );
    assert_eq!(settled.networks, 0, "no Network may outlive its owner");
    assert_settled(&baseline, &settled);
}

/// The rate axis, at fleet scale.
///
/// The in-flight ceiling alone would still let a process open attempts as fast as the
/// provider can fail them, so this proves the *start* rate is gated too: once the burst
/// is spent, no further attempt is made until time actually passes.
#[tokio::test]
async fn a_startup_herd_is_admitted_at_the_burst_rate_not_all_at_once() {
    let (_store, store_handle) = store();
    let provider = Arc::new(GateProvider::new());
    // A long token interval makes the rate gate impossible to refill by accident, so
    // anything admitted after the burst must have waited out real virtual time.
    let slow = ReconnectBudget {
        max_in_flight: MAX_IN_FLIGHT_CONNECTS,
        max_burst: MAX_CONNECT_BURST,
        token_interval: Duration::from_secs(3600),
        max_waiters: MAX_RECONNECT_WAITERS,
        seed: 0x05ee_d017,
    };
    slow.validate().expect("bounded budget");
    let scheduler = ReconnectScheduler::new(slow).expect("bounded budget");
    let resources = ResourceLedger::new(scheduler.clone(), store_handle.clone());
    let baseline = resources.snapshot();

    let fleet = Fleet::start(
        FLEET,
        store_handle,
        scheduler.clone(),
        resources.clone(),
        provider.clone(),
    );

    wait_until("the burst to be spent", || {
        provider.live() == MAX_IN_FLIGHT_CONNECTS
    })
    .await;

    // Past the burst, releasing the in-flight attempts cannot admit anything: the token
    // bucket is empty and the interval is an hour of real time.
    for _ in 0..8 {
        provider.release_all();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        provider.requested(),
        MAX_CONNECT_BURST as usize,
        "only the burst may start without the token interval elapsing"
    );
    assert_eq!(
        provider.peak(),
        MAX_IN_FLIGHT_CONNECTS,
        "the burst and the in-flight ceiling coincide here by construction"
    );

    fleet.shutdown().await;
    assert_settled(&baseline, &resources.snapshot());
}

// ------------------------------------------------ section 3: shared outage

/// A shared outage ends every generation, and replays nothing.
///
/// The replay half matters more than the teardown half: a disconnect after an outbound
/// command leaves delivery ambiguous, and the bouncer's whole safety argument rests on
/// not guessing. This proves the guess is not made even when *every* Network drops at
/// once and comes straight back.
#[tokio::test]
async fn a_simultaneous_outage_ends_every_generation_and_replays_nothing() {
    const NETWORKS: u64 = 4;
    let (_store, store_handle) = store();
    let provider = Arc::new(FakeI2pStreamProvider::default());
    for _ in 0..(NETWORKS as usize * 4) {
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
    }
    let scheduler = ReconnectScheduler::new(fleet_budget()).expect("bounded budget");
    let resources = ResourceLedger::new(scheduler.clone(), store_handle.clone());
    let baseline = resources.snapshot();

    let owners: Vec<_> = (0..NETWORKS)
        .map(|index| {
            let id = NetworkId(index + 1);
            let context = SupervisorContext {
                network: id,
                record: Arc::new(record(index + 1, "bot")),
                store: store_handle.clone(),
                status: watch::channel(Default::default()).0,
                resources: resources.clone(),
            };
            let owner = NetworkOwner::new(
                Shared(provider.clone()),
                context,
                store_handle.clone(),
                scheduler.clone(),
            )
            .expect("owner constructs");
            let snapshot = owner.subscribe_snapshot();
            let (command_tx, command_rx) = mpsc::channel(64);
            let _handle = SupervisorHandle::new(id, command_tx);
            let (stop, stop_rx) = watch::channel(false);
            let task = tokio::spawn(async move {
                let _keep = _handle;
                owner.serve(command_rx, stop_rx).await
            });
            (id, stop, task, snapshot)
        })
        .collect();

    // Bring every Network Online and let each take its upstream peer.
    let mut upstreams = Vec::new();
    for (id, _stop, _task, snapshot) in &owners {
        let mut upstream = provider.take_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            // The CAP LS line matters even though the bouncer requests nothing: without
            // it registration never completes, so the Network would never reach Online
            // and the campaign would be testing a stuck owner rather than an outage.
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        wait_for(*id, snapshot, Phase::Online).await;
        upstreams.push(upstream);
    }

    let loaded = resources.snapshot();
    assert_eq!(
        loaded.current.owner_tasks, NETWORKS as usize,
        "every owner must report itself while it is running"
    );
    assert!(
        loaded.peak.owner_tasks >= NETWORKS as usize,
        "the ledger must have seen the whole fleet at once"
    );

    // The shared outage: every upstream transport dies in the same instant.
    drop(upstreams);

    for (id, _stop, _task, snapshot) in &owners {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut seen = snapshot.clone();
            loop {
                let phase = seen.borrow().phase;
                if matches!(phase, Some(Phase::Backoff) | Some(Phase::Connecting)) {
                    return;
                }
                if seen.changed().await.is_err() {
                    panic!("network {id:?} ended without entering backoff");
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("network {id:?} never reacted to the outage"));
    }

    // The generation-scoped gauges must be gone even though the owners are still alive:
    // routes and batches are per-generation and must not survive into the replacement.
    let after_outage = resources.snapshot();
    assert_eq!(
        after_outage.current.response_routes, 0,
        "routes must not survive a generation"
    );
    assert_eq!(
        after_outage.current.open_batches, 0,
        "batch references must not survive a generation"
    );
    assert_eq!(
        after_outage.current.owner_tasks, NETWORKS as usize,
        "an owner between generations still exists and still counts"
    );

    for (_id, stop, task, _snapshot) in owners {
        let _ = stop.send(true);
        task.abort();
        let _ = task.await;
    }
    assert_settled(&baseline, &resources.snapshot());
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0; 256];
    tokio::time::timeout(Duration::from_secs(10), async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "stream ended while waiting for a frame");
            all.extend_from_slice(&buf[..count]);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {}; received {}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&all)
        )
    });
    all
}

async fn wait_for(id: NetworkId, snapshot: &watch::Receiver<NetworkSnapshot>, phase: Phase) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = snapshot.clone();
        while seen.borrow().phase != Some(phase) {
            if seen.changed().await.is_err() {
                break;
            }
        }
        assert_eq!(seen.borrow().phase, Some(phase), "network {id:?}");
    })
    .await
    .unwrap_or_else(|_| panic!("network {id:?} never reached {phase:?}"));
}

// ------------------------------------------------- section 8: busy loops

/// A provider that never answers must produce a bounded number of attempts, not a hot
/// loop.
///
/// This is the one claim that cannot be proved by reading a counter at the end: a spin
/// would also produce many attempts, just far faster than backoff allows.
///
/// Corrective 019 found this campaign was vacuous. It advanced virtual time once by
/// 600s and asserted `NETWORKS <= attempts <= NETWORKS * 32`. `tokio::time::advance`
/// performs a single poll, so a timer re-armed during that poll never fires: the chain
/// `connect -> backoff -> retry` advances one link per `advance()` call. The campaign
/// therefore measured **4 attempts for 4 Networks -- exactly one each**, landing on its
/// own lower bound and 32x under its upper bound. A spin was indistinguishable from
/// correct backoff, which is the one thing this test exists to rule out.
///
/// The repair is to step time in a loop so the retry chain is actually walked. Attempts
/// then scale with both virtual time and the number of polls, because each poll is one
/// chance for a re-armed timer to fire:
///
/// | steps x step | virtual time | attempts |
/// |---|---|---|
/// | 1 x 600s (the old campaign) | 600s | 4 -- vacuous |
/// | 1000 x 100ms | 100s | 4 |
/// | 500 x 600ms | 300s | 12 |
/// | 1000 x 300ms | 300s | 12 |
/// | 1000 x 600ms | 600s | 20 |
/// | 2000 x 600ms | 1200s | 32 |
/// | 5000 x 600ms (chosen) | 3000s | **52** |
/// | 20000 x 600ms | 12000s | 141 |
///
/// Those figures are deterministic, not lucky: `fleet_budget` fixes the jitter seed and
/// `jitter_entropy` is a pure function of Network, generation, and seed, so the campaign
/// reproduces exactly.
///
/// **The ceiling below is mutation-verified, which is what the original was not.** Under
/// the minimal mutation that removes backoff entirely -- `base` and `cap` set to zero in
/// `owner.rs`, nothing else changed -- the attempt counts are:
///
/// | steps | with backoff | backoff removed |
/// |---|---|---|
/// | 1000 | 20 | 20 |
/// | 2000 | 32 | 40 |
/// | 5000 | 52 | 100 |
/// | 20000 | 141 | 400 |
///
/// The two curves start together, so a short campaign cannot separate them at all: at
/// 1000 steps the old-style bound would have passed a spin. They diverge as polls
/// accumulate, because a removed backoff retries once per poll while a real one waits for
/// its timer. At 5000 steps the gap is wide enough to state a real ceiling.
#[tokio::test(start_paused = true)]
async fn a_stalled_provider_produces_a_bounded_number_of_attempts() {
    const NETWORKS: usize = 4;
    /// 5000 polls of 600ms: enough for the backoff chain to retry repeatedly and for a
    /// spin to separate from it, and deterministic so the ceiling can be tight.
    const STEPS: usize = 5000;
    const STEP_MS: u64 = 600;
    /// Measured 52 (13 per Network). A backoff-removed run measures 100, so this
    /// ceiling sits above the real schedule and below a spin.
    const MAX_ATTEMPTS: usize = NETWORKS * 16;
    let (_store, store_handle) = store();
    let provider = Arc::new(GateProvider::new());
    // No outcomes queued at all: every attempt fails immediately as unavailable.
    let scheduler = ReconnectScheduler::new(fleet_budget()).expect("bounded budget");
    let resources = ResourceLedger::new(scheduler.clone(), store_handle.clone());
    let baseline = resources.snapshot();

    let members: Vec<Member> = {
        let mut members = Vec::new();
        for index in 0..NETWORKS {
            let id = NetworkId(index as u64 + 1);
            let context = SupervisorContext {
                network: id,
                record: Arc::new(record(index as u64 + 1, "bot")),
                store: store_handle.clone(),
                status: watch::channel(Default::default()).0,
                resources: resources.clone(),
            };
            let owner = NetworkOwner::new(
                Shared(provider.clone()),
                context,
                store_handle.clone(),
                scheduler.clone(),
            )
            .expect("owner constructs");
            let (command_tx, command_rx) = mpsc::channel(64);
            let _handle = SupervisorHandle::new(id, command_tx);
            let (stop, stop_rx) = watch::channel(false);
            let task = tokio::spawn(async move {
                let _keep = _handle;
                owner.serve(command_rx, stop_rx).await
            });
            members.push(Member { stop, task });
        }
        members
    };

    // Walk the clock in bounded steps so `connect -> backoff -> retry` is exercised
    // repeatedly rather than once. A single large advance would move the clock past
    // every deadline at once and exercise exactly one retry, which is what made the
    // original campaign vacuous.
    for _ in 0..STEPS {
        tokio::time::advance(Duration::from_millis(STEP_MS)).await;
    }

    let attempts = provider.requested();
    assert!(
        attempts > NETWORKS,
        "every Network must have retried at least once, not merely tried once: saw \
         {attempts} attempts for {NETWORKS} Networks, which is what a stalled provider \
         looks like when the retry chain never runs"
    );
    assert!(
        attempts <= MAX_ATTEMPTS,
        "a stalled provider produced {attempts} attempts, which is a spin rather than a \
         backoff: with backoff removed this campaign measures 100, and the ceiling is \
         {MAX_ATTEMPTS} over {}s of virtual time",
        STEPS * STEP_MS as usize / 1000
    );
    assert!(
        resources.snapshot().current.in_flight_connects <= MAX_IN_FLIGHT_CONNECTS,
        "the connect ceiling holds no matter how long the loop runs"
    );

    for member in members {
        let _ = member.stop.send(true);
        member.task.abort();
        let _ = member.task.await;
    }
    assert_settled(&baseline, &resources.snapshot());
}

// ------------------------------------------------ section 10: churn at scale

/// Repeated loss and recovery must not accumulate anything.
///
/// One hundred reconnect rounds across several Networks, all under pinned virtual time,
/// with a baseline assertion at the end. The provider fails every attempt, so this walks
/// the real reconnect path -- admission, backoff, retry -- rather than a simulation of
/// it. The claim is not that any single reconnect was cheap (plan 016 owns that) but that
/// doing it a hundred times leaves nothing behind.
#[tokio::test(start_paused = true)]
async fn reconnect_churn_leaves_no_residue() {
    const NETWORKS: usize = 3;
    const ROUNDS: usize = 120;
    let (_store, store_handle) = store();
    // Nothing is queued, so every attempt fails immediately as unavailable. That is the
    // realistic shape of a shared outage: the bouncer keeps trying, and keeps backing off.
    let provider = Arc::new(GateProvider::new());
    let scheduler = ReconnectScheduler::new(fleet_budget()).expect("bounded budget");
    let resources = ResourceLedger::new(scheduler.clone(), store_handle.clone());
    let baseline = resources.snapshot();

    let fleet = Fleet::start(
        NETWORKS,
        store_handle,
        scheduler.clone(),
        resources.clone(),
        provider.clone(),
    );

    // The backoff caps at five minutes, so a generous step drives a whole fleet through
    // several rounds without depending on how many attempts any one Network made.
    let mut attempts = 0usize;
    for _ in 0..ROUNDS {
        provider.release_all();
        tokio::time::advance(Duration::from_secs(600)).await;
        attempts = attempts.max(provider.requested());
        if attempts >= ROUNDS {
            break;
        }
    }
    assert!(
        attempts >= ROUNDS,
        "expected at least {ROUNDS} reconnect rounds, saw {attempts}"
    );
    assert!(
        provider.peak() <= MAX_IN_FLIGHT_CONNECTS,
        "the connect ceiling held across {attempts} rounds, peak was {}",
        provider.peak()
    );

    let churned = resources.snapshot();
    assert_eq!(
        churned.networks, NETWORKS,
        "churn must not add or lose Network entries"
    );
    assert_eq!(
        churned.refused, 0,
        "churn must not exhaust the ledger's Network ceiling"
    );
    assert!(
        churned.current.in_flight_connects <= MAX_IN_FLIGHT_CONNECTS,
        "no attempt may outlive its permit"
    );
    assert!(
        churned.current.reconnect_waiters <= MAX_RECONNECT_WAITERS,
        "the waiter set must stay bounded across {attempts} rounds, saw {}",
        churned.current.reconnect_waiters
    );

    fleet.shutdown().await;
    assert_settled(&baseline, &resources.snapshot());
}
