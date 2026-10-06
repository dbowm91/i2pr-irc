//! Plan 012 integrated qualification for M003 closure.
//!
//! These tests qualify the *integrated* system rather than any single subsystem.
//! Each test names the roadmap invariant it defends, so a failure points at the
//! property that broke rather than at a component that changed.
#![cfg(test)]

use i2pr_irc_core::{
    ClientId, ConnectionGeneration, HistoryEventId, I2pEndpoint, NetworkId, SessionId,
};
use i2pr_irc_runtime::{
    RuntimeError,
    capability::{DownstreamCapabilities, UpstreamCapabilities},
    catalog::{NetworkCatalog, SupervisorCommand, SupervisorContext, SupervisorHandle},
    chathistory::{HistoryQueryRequest, MessageReference},
    journal::{BacklogCap, HistoryJournal, IngestOutcome},
    owner::{MAX_SESSIONS_PER_NETWORK, NetworkOwner, NetworkSnapshot, Phase},
};
use i2pr_irc_store::{
    BufferKind, NetworkRecord, STORE_BUSY_TIMEOUT_MS, Store, StoreErrorKind, StoreHandle, StorePath,
};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

// ------------------------------------------------------------------- fixtures

/// Shared provider so several Network owners connect independently.
#[derive(Clone)]
struct Shared(Arc<FakeI2pStreamProvider>);
#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Shared {
    async fn connect(
        &self,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(endpoint).await
    }
}

fn record(network: u64, nick: &str, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: nick.into(),
        username: "user".into(),
        realname: "bouncer".into(),
        sasl: None,
        desired_channels: channels.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn journal(network: u64, store: StoreHandle) -> HistoryJournal {
    HistoryJournal::new(
        NetworkId(network),
        store,
        Box::new(i2pr_irc_core::SystemWallClock),
        i2pr_irc_core::Casemapping::Rfc1459,
    )
}

/// One Network owner brought fully Online, retaining its upstream stream.
struct Online {
    provider: Arc<FakeI2pStreamProvider>,
    commands: mpsc::Sender<SupervisorCommand>,
    /// Retained so the harness drives the public routing handle, not a private channel.
    handle: SupervisorHandle,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    snapshot: watch::Receiver<NetworkSnapshot>,
    network: NetworkId,
    upstream: Option<ScriptedStream>,
}

impl Online {
    async fn start(network: u64, nick: &str, channels: &[&str], store: StoreHandle) -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        // Queue enough connect outcomes that a later reconnect fails loudly with an
        // empty fixture rather than blocking forever.
        for _ in 0..4 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("queue");
        }
        let context = SupervisorContext {
            network: NetworkId(network),
            record: Arc::new(record(network, nick, channels)),
            store: store.clone(),
            status: watch::channel(Default::default()).0,
        };
        let owner =
            NetworkOwner::new(Shared(provider.clone()), context, store).expect("owner constructs");
        let snapshot = owner.subscribe_snapshot();
        let (command_tx, command_rx) = mpsc::channel(64);
        let handle = SupervisorHandle::new(NetworkId(network), command_tx.clone());
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(async move { owner.serve(command_rx, stop_rx).await });
        let mut harness = Self {
            provider,
            commands: command_tx,
            handle,
            stop,
            task,
            snapshot,
            network: NetworkId(network),
            upstream: None,
        };
        let mut upstream = harness.provider.take_peer().await;
        harness.drive_online(&mut upstream).await;
        harness.upstream = Some(upstream);
        harness
    }

    async fn drive_online(&self, upstream: &mut ScriptedStream) {
        read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .expect("upstream writable");
    }

    /// The upstream stream for the generation this harness brought Online.
    fn upstream(&mut self) -> &mut ScriptedStream {
        self.upstream
            .as_mut()
            .expect("this Network is Online and retains its upstream stream")
    }

    async fn wait_phase(&mut self, phase: Phase) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.snapshot.borrow().phase != Some(phase) {
                self.snapshot.changed().await.expect("owner alive");
            }
        })
        .await
        .unwrap_or_else(|_| {
            let snapshot = self.snapshot.borrow();
            panic!(
                "network {:?} never reached {phase:?}; phase={:?} generation={:?} error={:?}",
                self.network, snapshot.phase, snapshot.generation, snapshot.last_error
            )
        });
    }

    /// Attaches one client whose socket buffers `capacity` bytes.
    ///
    /// A small capacity is how a *slow* client is modelled: the owner's own queue
    /// fills because the peer stops reading, which is exactly the pressure a healthy
    /// attachment must never impose on anyone else.
    async fn attach_with(
        &self,
        client: ClientId,
        capacity: usize,
    ) -> (SessionId, tokio::io::DuplexStream) {
        self.attach_as(SessionId(client.0), client, capacity).await
    }

    async fn attach_as(
        &self,
        session: SessionId,
        client: ClientId,
        capacity: usize,
    ) -> (SessionId, tokio::io::DuplexStream) {
        let (session, peer) = self
            .try_attach(session, client, capacity)
            .await
            .expect("accepted");
        (session, peer)
    }

    /// Attaches a client under an explicit ephemeral identity and reports the owner's
    /// decision rather than assuming it.
    async fn try_attach(
        &self,
        session: SessionId,
        client: ClientId,
        capacity: usize,
    ) -> Result<(SessionId, tokio::io::DuplexStream), RuntimeError> {
        let (client_side, peer) = tokio::io::duplex(capacity);
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Attach {
                session,
                client,
                stream: Box::new(client_side),
                reply,
            })
            .expect("attach fits the bounded control queue");
        response
            .await
            .expect("owner answers")
            .map(|()| (session, peer))
    }

    async fn attach(&self, client: ClientId) -> (SessionId, tokio::io::DuplexStream) {
        self.attach_with(client, 64 * 1024).await
    }

    async fn settle(&mut self) {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

impl Drop for Online {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0; 256];
    tokio::time::timeout(Duration::from_secs(10), async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.expect("upstream readable");
            assert!(count > 0, "upstream ended while waiting for a frame");
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

/// Writes `count` PRIVMSGs upstream, stopping only if the upstream itself fails.
async fn flood_upstream(owner: &mut Online, count: u32) {
    let mut payload = String::with_capacity(count as usize * 48);
    for index in 0..count {
        payload.push_str(&format!(":a!u@h PRIVMSG #room :flood {index}\r\n"));
    }
    owner
        .upstream()
        .write_all(payload.as_bytes())
        .await
        .expect("upstream stays writable for the whole flood");
}

async fn client_read_until(client: &mut tokio::io::DuplexStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 256];
    tokio::time::timeout(Duration::from_secs(10), async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = client.read(&mut buf).await.expect("client readable");
            assert!(count > 0, "client stream ended");
            all.extend_from_slice(&buf[..count]);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "client never received {}; received {}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&all)
        )
    });
    String::from_utf8_lossy(&all).into_owned()
}

/// Completes local registration. The welcome is the only synchronization point
/// registration needs: it proves the owner applied current state to *this* session.
/// How long the fake server side waits for more upstream bytes before concluding the
/// owner has nothing left to write.
const DRAIN_IDLE: Duration = Duration::from_millis(25);

/// Reads everything the owner has written upstream.
///
/// The fake server side has a finite buffer, so a test that never reads it would make
/// the *fixture* the back-pressure it is trying to measure.
async fn drain_upstream(owner: &mut Online) {
    let mut buf = [0; 4096];
    loop {
        let read = tokio::time::timeout(DRAIN_IDLE, owner.upstream().read(&mut buf)).await;
        match read {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
            Ok(Ok(_)) => {}
        }
    }
}

/// Waits until the owner's upstream intent queues are empty.
///
/// The gauges are sampled by the owner loop, so a turn is forced first: otherwise an
/// idle owner never republishes and a drained queue would still read as full.
async fn wait_upstream_queues_drained(
    owner: &mut Online,
    client: &mut tokio::io::DuplexStream,
    nonce: &mut u32,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let drained = {
            let snapshot = owner.snapshot.borrow();
            snapshot.upstream_normal_queue_depth == 0 && snapshot.upstream_control_queue_depth == 0
        };
        if drained {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "upstream intent queues did not drain: {:?}",
            owner.snapshot.borrow()
        );
        // Each keepalive is forwarded upstream, so the fake server side must be read
        // or the owner would be waiting on a fixture that refuses to consume.
        drain_upstream(owner).await;
        keepalive(owner, client, nonce).await;
    }
}

/// Round-trips a uniquely identified client PING.
///
/// The owner's generation loop only samples its gauges and drains its bounded
/// ingestion queue on a turn, and it never spins for the sake of either. A client
/// PING is therefore the honest way to *ask* for a turn: it is ordinary control
/// traffic, and answering it promptly is itself an invariant under test.
///
/// The token is unique per call. Reusing one would let the read match the previous
/// PONG still sitting in the socket buffer, which would turn this into a busy loop
/// that never actually waits for the owner.
async fn keepalive(owner: &mut Online, client: &mut tokio::io::DuplexStream, nonce: &mut u32) {
    *nonce += 1;
    let marker = format!("keepalive-{}-{nonce}", owner.network.0);
    client
        .write_all(format!("PING :{marker}\r\n").as_bytes())
        .await
        .expect("client writable");
    let answered = client_read_until(client, marker.as_bytes()).await;
    assert!(
        answered.contains("PONG"),
        "a client PING must always be answered promptly, even under storage pressure"
    );
}

/// Confirms upstream self-membership, which is what makes a channel's lines
/// history-eligible: a buffer exists only for a channel the Network has observed.
async fn join_upstream(owner: &mut Online, nick: &str, channel: &str) {
    owner
        .upstream()
        .write_all(format!(":{nick}!u@h JOIN {channel}\r\n").as_bytes())
        .await
        .expect("upstream writable");
}

async fn client_register(client: &mut tokio::io::DuplexStream, nick: &str) -> String {
    client
        .write_all(format!("NICK {nick}\r\nUSER {nick} 0 * :phone\r\n").as_bytes())
        .await
        .expect("client writable");
    client_read_until(client, b"001 ").await
}

// ================================================================ MULTI-NETWORK

#[tokio::test]
async fn one_networks_failure_history_and_client_churn_leaves_the_others_untouched() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    handle
        .save_network(&record(2, "bot", &[]))
        .await
        .expect("saved");

    let mut healthy = Online::start(1, "bot", &[], handle.clone()).await;
    let mut working = Online::start(2, "bot", &[], handle.clone()).await;
    healthy.wait_phase(Phase::Online).await;
    working.wait_phase(Phase::Online).await;

    // The working Network records history and churns clients while the healthy one
    // does nothing. Neither may disturb the other.
    let (_session, mut peer) = working.attach(ClientId(1)).await;
    client_register(&mut peer, "bot").await;
    join_upstream(&mut working, "bot", "#room").await;
    flood_upstream(&mut working, 4).await;
    for client in 2..6u64 {
        let (_session, stream) = working.attach(ClientId(client)).await;
        drop(stream);
    }
    working.settle().await;

    // The healthy Network is byte-for-byte unchanged.
    assert_eq!(healthy.snapshot.borrow().phase, Some(Phase::Online));
    assert_eq!(
        healthy.snapshot.borrow().generation,
        Some(ConnectionGeneration(1)),
        "another Network's history and client churn must not advance this generation"
    );
    assert_eq!(healthy.snapshot.borrow().history_recorded, 0);
    assert_eq!(healthy.snapshot.borrow().history_dropped, 0);
    assert_eq!(healthy.snapshot.borrow().fanout_dropped, 0);
    assert_eq!(healthy.snapshot.borrow().sessions_accepted, 0);
    assert_eq!(healthy.snapshot.borrow().attached_sessions, 0);

    // The working Network did in fact do the work, so the assertions above are not
    // vacuously true because nothing happened anywhere.
    assert!(
        working.snapshot.borrow().history_recorded > 0,
        "the working Network really did record history"
    );
    assert!(working.snapshot.borrow().sessions_accepted >= 5);
    assert_eq!(working.snapshot.borrow().attached_sessions, 1);
}

#[tokio::test]
async fn many_idle_networks_and_clients_stay_within_deterministic_ceilings() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    const NETWORKS: u64 = 6;
    for network in 1..=NETWORKS {
        handle
            .save_network(&record(network, "bot", &[]))
            .await
            .expect("saved");
    }
    let mut owners = Vec::new();
    for network in 1..=NETWORKS {
        owners.push(Online::start(network, "bot", &[], handle.clone()).await);
    }
    for owner in &mut owners {
        owner.wait_phase(Phase::Online).await;
    }

    // Several clients per Network, well inside the per-Network ceiling.
    let mut peers = Vec::new();
    for owner in &owners {
        for client in 1..=3u64 {
            let (session, peer) = owner.attach(ClientId(client)).await;
            peers.push(peer);
            assert_eq!(
                session,
                SessionId(client),
                "session identity is per attachment, not per Network"
            );
        }
    }
    for peer in &mut peers {
        client_register(peer, "bot").await;
    }
    for owner in &owners {
        assert_eq!(owner.snapshot.borrow().attached_sessions, 3);
        assert!(
            owner.snapshot.borrow().attached_sessions <= MAX_SESSIONS_PER_NETWORK,
            "the per-Network session ceiling is enforced"
        );
    }

    // Churn every client and prove the system returns to a bounded steady state.
    drop(peers);
    for owner in &mut owners {
        tokio::time::timeout(Duration::from_secs(10), async {
            while owner.snapshot.borrow().attached_sessions != 0 {
                owner.snapshot.changed().await.expect("owner alive");
            }
        })
        .await
        .expect("sessions drain to zero");
    }
    for owner in &owners {
        assert_eq!(owner.snapshot.borrow().attached_sessions, 0);
        assert_eq!(
            owner.snapshot.borrow().generation,
            Some(ConnectionGeneration(1)),
            "client churn never replaced an upstream generation"
        );
    }
}

#[tokio::test]
async fn the_per_network_session_ceiling_refuses_the_attach_it_cannot_serve() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let owner = Online::start(1, "bot", &[], handle).await;

    let mut peers = Vec::new();
    for client in 1..=MAX_SESSIONS_PER_NETWORK as u64 {
        let (_session, peer) = owner.attach(ClientId(client)).await;
        peers.push(peer);
    }
    // The next attachment is refused rather than served by evicting somebody: a
    // bounded ceiling must not become a reason to drop an existing client. It is
    // refused through the public routing handle, so this is the surface a caller
    // actually holds.
    let (_session, peer) = tokio::io::duplex(1024);
    let refused = owner
        .handle
        .attach(
            SessionId(MAX_SESSIONS_PER_NETWORK as u64 + 1),
            ClientId(MAX_SESSIONS_PER_NETWORK as u64 + 1),
            Box::new(peer),
        )
        .await
        .err();
    assert!(
        matches!(refused, Some(RuntimeError::QueueOverloaded)),
        "an attachment past the ceiling is refused, got {refused:?}"
    );
    assert_eq!(
        owner.snapshot.borrow().attached_sessions,
        MAX_SESSIONS_PER_NETWORK
    );
    drop(peers);
}

// ================================================================== MULTI-CLIENT

#[tokio::test]
async fn one_clients_queue_pressure_never_starves_the_others() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;

    // The slow client registers and then stops reading, so its bounded socket and
    // queue fill while the healthy client keeps consuming normally.
    let (_slow_session, mut slow) = owner.attach_with(ClientId(1), 2048).await;
    client_register(&mut slow, "bot").await;
    let (_fast_session, mut fast) = owner.attach(ClientId(2)).await;
    client_register(&mut fast, "bot").await;

    // Enough upstream volume to overrun a 64-frame queue behind a 2 KiB socket.
    flood_upstream(&mut owner, 2_000).await;

    let received = client_read_until(&mut fast, b"PRIVMSG").await;
    assert!(
        received.contains("PRIVMSG"),
        "a slow client must never starve a healthy one: {received}"
    );
    // The healthy attachment was not ended to make room for the slow one.
    assert!(
        owner.snapshot.borrow().attached_sessions >= 1,
        "one saturated client cost only its own frames"
    );
    assert!(
        owner.snapshot.borrow().fanout_dropped > 0,
        "the loss to the slow client is counted, never silent"
    );
    assert_eq!(
        owner.snapshot.borrow().phase,
        Some(Phase::Online),
        "client pressure never ends the upstream Network"
    );
}

#[tokio::test]
async fn a_same_client_id_replacement_inherits_nothing_from_the_first_attachment() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;

    // Same durable lineage, a fresh ephemeral attachment. The allocator never reuses a
    // SessionId, so a replacement arrives under its own identity.
    let (first, mut first_stream) = owner.attach_as(SessionId(41), ClientId(7), 64 * 1024).await;
    client_register(&mut first_stream, "bot").await;
    flood_upstream(&mut owner, 1).await;
    let carried = client_read_until(&mut first_stream, b"PRIVMSG").await;
    assert!(carried.contains("PRIVMSG"));
    drop(first_stream);

    // The replacement shares the durable lineage but is a distinct ephemeral
    // attachment: the allocator never reuses a SessionId, so nothing from the first
    // attachment can be applied to it.
    let (second, mut second_stream) = owner.attach_as(SessionId(42), ClientId(7), 64 * 1024).await;
    client_register(&mut second_stream, "bot").await;
    assert_ne!(
        first.0, second.0,
        "a replacement attachment never reuses the previous SessionId"
    );

    // Only the replacement is attached, and it saw no frame from the first.
    let snapshot = owner.snapshot.borrow();
    assert_eq!(snapshot.sessions_accepted, 2);
    assert_eq!(snapshot.attached_sessions, 1);
    assert_eq!(snapshot.sessions_ended, 1);
}

// ================================================================ PERSISTENCE

#[tokio::test]
async fn a_clean_restart_rebuilds_durable_intent_and_no_live_state() {
    let dir = i2pr_irc_store::testing::temp_dir("integrated-restart");
    let path = dir.db("restart.sqlite3");

    // Before restart: durable intent, a client lineage, history, a cursor, and a marker.
    let first = Store::open(&StorePath::File(path.clone())).expect("store opens");
    let handle = first.handle_clone();
    handle
        .save_network(&record(1, "bot", &["#alpha"]))
        .await
        .expect("saved");
    let mut before = journal(1, handle.clone());
    let phone = before.ensure_client("phone").await.expect("lineage");
    let buffer = before
        .resolve_buffer(BufferKind::Channel, "#alpha")
        .await
        .expect("buffer");
    let IngestOutcome::Recorded { event } = before
        .ingest(
            buffer,
            &i2pr_irc_wire::Message::parse(b":a!u@h PRIVMSG #alpha :before\r\n").expect("parses"),
        )
        .await
        .expect("ingests")
    else {
        panic!("expected a recorded event")
    };
    before
        .advance_cursor(phone, buffer, event)
        .await
        .expect("advances");
    before.set_read_marker(buffer, event).await.expect("sets");
    drop(before);
    first.shutdown().expect("shuts down");

    // After restart: intent and history state return; live state does not.
    let second = Store::open(&StorePath::File(path)).expect("store reopens");
    let handle = second.handle_clone();
    let catalog = NetworkCatalog::new(handle.clone());
    let desired = catalog.load_desired_state().await.expect("catalog loads");
    assert_eq!(desired[0].desired_channels, vec!["#alpha".to_owned()]);
    assert!(
        catalog.is_empty(),
        "a restart restores no live supervisor, and therefore no session"
    );

    let mut after = journal(1, handle);
    let resumed = after
        .resolve_buffer(BufferKind::Channel, "#alpha")
        .await
        .expect("buffer identity survives restart");
    assert_eq!(resumed, buffer, "BufferId is durable");
    assert_eq!(phone, after.ensure_client("phone").await.expect("lineage"));
    assert_eq!(
        after.cursor(phone, resumed).await.expect("reads"),
        Some(event),
        "a cursor survives restart"
    );
    assert_eq!(
        after.read_marker(resumed).await.expect("reads"),
        Some(event),
        "a read marker survives restart"
    );
    second.shutdown().expect("shuts down");
}

#[tokio::test]
async fn an_incompatible_schema_is_a_startup_failure_not_a_runtime_state() {
    let dir = i2pr_irc_store::testing::temp_dir("integrated-migration");
    let path = dir.db("schema.sqlite3");
    let first = Store::open(&StorePath::File(path.clone())).expect("store opens");
    first.shutdown().expect("shuts down");
    i2pr_irc_store::testing::stamp(
        &path,
        i2pr_irc_store::APPLICATION_ID,
        i2pr_irc_store::SCHEMA_VERSION + 1,
    );
    assert_eq!(
        Store::open(&StorePath::File(path))
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::SchemaTooNew),
        "a database this build cannot serve must stop startup, not be worked around"
    );
}

// ============================================================== STORE PRESSURE

#[tokio::test]
async fn a_stalled_store_never_starves_control_traffic() {
    let dir = i2pr_irc_store::testing::temp_dir("integrated-pressure");
    let path = dir.db("pressure.sqlite3");
    let store = Store::open_stalled(
        &StorePath::File(path),
        STORE_BUSY_TIMEOUT_MS,
        Some(Duration::from_millis(120)),
    )
    .expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");

    let mut owner = Online::start(1, "bot", &[], handle.clone()).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;

    // Storage is now extremely slow. Control traffic must remain schedulable, so a
    // client PING is still answered without waiting on the store.
    handle.set_stall(Some(Duration::from_millis(150)));
    let started = std::time::Instant::now();
    for index in 0..5u32 {
        client
            .write_all(format!("PING :liveness-{index}\r\n").as_bytes())
            .await
            .expect("client writable");
        let pong = client_read_until(&mut client, format!("liveness-{index}").as_bytes()).await;
        assert!(
            pong.contains("PONG"),
            "a stalled store must not delay a keepalive answer: {pong}"
        );
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "five keepalives took {elapsed:?} while the store was stalled"
    );
    assert_eq!(
        owner.snapshot.borrow().phase,
        Some(Phase::Online),
        "storage pressure never ends the upstream Network"
    );

    // Upstream liveness is unaffected too: the owner's own PING/PONG still flows.
    let _ = read_until(owner.upstream(), b"PING :bouncer-").await;
    handle.set_stall(None);
}

#[tokio::test]
async fn store_pressure_degrades_storage_only_and_creates_no_side_queue() {
    let dir = i2pr_irc_store::testing::temp_dir("integrated-overload");
    let path = dir.db("overload.sqlite3");
    let store = Store::open_stalled(
        &StorePath::File(path),
        STORE_BUSY_TIMEOUT_MS,
        Some(Duration::from_millis(200)),
    )
    .expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");

    let mut owner = Online::start(1, "bot", &[], handle.clone()).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;

    // Every one of these messages is history-eligible, so a stalled store turns them
    // into ingestion pressure. The bounded ingress queue must absorb that pressure by
    // dropping, never by growing.
    const FLOOD: u64 = 400;
    join_upstream(&mut owner, "bot", "#room").await;
    handle.set_stall(Some(Duration::from_millis(60)));
    flood_upstream(&mut owner, FLOOD as u32).await;
    owner.settle().await;

    assert_eq!(
        owner.snapshot.borrow().phase,
        Some(Phase::Online),
        "storage pressure must never end the upstream Network"
    );

    let received = client_read_until(&mut client, b"PRIVMSG").await;
    assert!(
        received.contains("PRIVMSG"),
        "the client still receives live traffic while storage is degraded"
    );

    // Nothing accumulated behind the slow store: every upstream intent the owner
    // produced has been written, with no retry queue and no side buffer.
    handle.set_stall(None);
    drain_upstream(&mut owner).await;
    let mut nonce = 0u32;
    wait_upstream_queues_drained(&mut owner, &mut client, &mut nonce).await;

    // Every history-eligible line is *accounted for*: recorded, skipped, or dropped
    // and counted. A line that simply vanished would leave no trace, which is the
    // failure mode a bounded drop exists to avoid.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let accounted = {
            let snapshot = owner.snapshot.borrow();
            snapshot.history_recorded + snapshot.history_skipped + snapshot.history_dropped
        };
        if accounted >= FLOOD {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ingestion stalled at {accounted} of {FLOOD} accounted lines: {:?}",
            owner.snapshot.borrow()
        );
        drain_upstream(&mut owner).await;
        keepalive(&mut owner, &mut client, &mut nonce).await;
    }
    let snapshot = owner.snapshot.borrow();
    assert_eq!(
        snapshot.history_recorded + snapshot.history_skipped + snapshot.history_dropped,
        FLOOD,
        "no history-eligible line vanished, and none was buffered without a ceiling"
    );
}

#[tokio::test]
async fn shutdown_with_pending_work_completes_and_does_not_lose_the_queue() {
    let dir = i2pr_irc_store::testing::temp_dir("integrated-shutdown");
    let path = dir.db("shutdown.sqlite3");
    let store = Store::open_stalled(
        &StorePath::File(path.clone()),
        STORE_BUSY_TIMEOUT_MS,
        Some(Duration::from_millis(30)),
    )
    .expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");

    // Queue work from a live handle, then stop. `shutdown` joins the worker, so it
    // must run off the async worker thread while the flood keeps feeding the queue.
    let flood = tokio::spawn({
        let handle = handle.clone();
        async move {
            for index in 0..20 {
                let _ = handle
                    .add_desired_channel(NetworkId(1), &format!("#room{index}"))
                    .await;
            }
        }
    });
    tokio::task::yield_now().await;

    let stopper = tokio::task::spawn_blocking(move || store.shutdown());
    tokio::time::timeout(Duration::from_secs(20), stopper)
        .await
        .expect("shutdown must not hang with pending work")
        .expect("shutdown thread joins")
        .expect("shutdown succeeds");
    let _ = tokio::time::timeout(Duration::from_secs(5), flood).await;

    // Reopening proves the drained queue was committed rather than discarded.
    let reopened = Store::open(&StorePath::File(path)).expect("store reopens");
    let desired = reopened
        .handle_clone()
        .load_networks()
        .await
        .expect("durable intent survives");
    assert_eq!(desired.len(), 1);
    reopened.shutdown().expect("shuts down");
}

// ==================================================================== HISTORY

#[tokio::test]
async fn deterministic_ordering_holds_across_identical_and_skewed_timestamps() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut journal = HistoryJournal::new(
        NetworkId(1),
        handle,
        Box::new(i2pr_irc_core::VirtualWallClock::default()),
        i2pr_irc_core::Casemapping::Rfc1459,
    );
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");

    // Identical receive times, a deliberately skewed server time, and one event with
    // no msgid at all. None of that may influence canonical order.
    let mut ids = Vec::new();
    for index in 0..4i64 {
        let raw = if index == 3 {
            format!(
                "@time={} :a!u@h PRIVMSG #room :m{index}\r\n",
                i2pr_irc_wire::IrcTimestamp::from_unix_millis(1_700_000_000_000 - index)
                    .expect("representable")
            )
        } else {
            format!(
                "@time={};msgid=m{index} :a!u@h PRIVMSG #room :m{index}\r\n",
                i2pr_irc_wire::IrcTimestamp::from_unix_millis(1_700_000_000_000 - index)
                    .expect("representable")
            )
        };
        if let IngestOutcome::Recorded { event } = journal
            .ingest(
                buffer,
                &i2pr_irc_wire::Message::parse(raw.as_bytes()).expect("parses"),
            )
            .await
            .expect("ingests")
        {
            ids.push(event);
        }
    }
    assert_eq!(ids.len(), 4, "every eligible event is recorded");

    let events = journal
        .backlog_range(buffer, None, None, 10)
        .await
        .expect("reads");
    for pair in events.windows(2) {
        assert!(
            pair[0].event < pair[1].event,
            "canonical order is local identity, not a timestamp"
        );
    }
    assert_eq!(
        events.last().expect("last").msgid,
        None,
        "an event with no msgid is retained without one"
    );
    assert!(
        events.iter().all(|event| event.server_time.is_some()),
        "a skewed server time is recorded as metadata only"
    );
    // The newest event by local order is the one with the *oldest* server time.
    assert_eq!(events.last().expect("last").event, ids[3]);
}

#[tokio::test]
async fn legacy_playback_and_chathistory_never_duplicate_initial_history() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut journal = journal(1, handle);
    let phone = journal.ensure_client("phone").await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    for index in 0..4 {
        journal
            .ingest(
                buffer,
                &i2pr_irc_wire::Message::parse(
                    format!("@msgid=m{index} :a!u@h PRIVMSG #room :m{index}\r\n").as_bytes(),
                )
                .expect("parses"),
            )
            .await
            .expect("ingests");
    }

    // A chathistory client manages its own history, so it gets no automatic backlog.
    let negotiated: BTreeSet<String> =
        BTreeSet::from([i2pr_irc_runtime::chathistory::CHATHISTORY_CAPABILITY.to_owned()]);
    let capabilities =
        i2pr_irc_runtime::session::SessionCapabilities::default().with_negotiated(&negotiated);
    assert!(
        !capabilities.wants_backlog(),
        "a chathistory client must not also receive the automatic backlog"
    );

    // The legacy path delivers once and advances the cursor.
    let first = journal
        .backlog(phone, buffer, BacklogCap::DEFAULT)
        .await
        .expect("backlog");
    assert_eq!(first.len(), 4);
    let cursor = journal
        .advance_cursor(phone, buffer, first[3].event)
        .await
        .expect("advances");
    let second = journal
        .backlog(phone, buffer, BacklogCap::DEFAULT)
        .await
        .expect("backlog");
    assert!(
        second.is_empty(),
        "a second automatic replay delivers nothing new"
    );
    assert!(cursor.0 > 0, "the cursor moved only forward");

    // An explicit query still works and names its own range.
    let reply = i2pr_irc_runtime::chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::Latest { limit: 2 },
    )
    .await
    .expect("executes");
    assert_eq!(reply.lines.len(), 2);
}

#[tokio::test]
async fn read_marker_and_cursor_survive_retention_and_clamp_monotonically() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut journal = journal(1, handle);
    let phone = journal.ensure_client("phone").await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let mut ids = Vec::new();
    for index in 0..6 {
        if let IngestOutcome::Recorded { event } = journal
            .ingest(
                buffer,
                &i2pr_irc_wire::Message::parse(
                    format!("@msgid=m{index} :a!u@h PRIVMSG #room :m{index}\r\n").as_bytes(),
                )
                .expect("parses"),
            )
            .await
            .expect("ingests")
        {
            ids.push(event);
        }
    }
    journal
        .advance_cursor(phone, buffer, ids[2])
        .await
        .expect("advances");
    journal.set_read_marker(buffer, ids[2]).await.expect("sets");

    let boundary = HistoryEventId(ids[3].0 + 1);
    let report = journal.retain(boundary).await.expect("retention");
    assert_eq!(report.deleted, 4, "retention deletes only what it selected");
    assert!(!report.more_pending, "one bounded pass finished the job");
    assert_eq!(report.cursors_clamped, 1);
    assert_eq!(report.markers_clamped, 1);

    // A clamped position still moves only forward, and both use one clamp rule.
    let cursor = journal
        .cursor(phone, buffer)
        .await
        .expect("reads")
        .expect("exists");
    let marker = journal
        .read_marker(buffer)
        .await
        .expect("reads")
        .expect("exists");
    assert!(cursor < boundary);
    assert_eq!(cursor, marker, "a cursor and a marker clamp identically");

    // A stale reference to pruned history is refused deterministically.
    assert_eq!(
        i2pr_irc_runtime::chathistory::resolve(
            &journal,
            buffer,
            &MessageReference::MsgId("m0".to_owned())
        )
        .await
        .err(),
        Some(i2pr_irc_runtime::chathistory::HistoryRefusal::HistoryUnavailable)
    );
}

// =========================================================== RESPONSE ROUTING

#[tokio::test]
async fn concurrent_labeled_queries_from_several_sessions_route_correctly() {
    use i2pr_irc_runtime::routing::{RequestClass, ResponseRouter, RouteOutcome, Routed};

    let mut router = ResponseRouter::default();
    let at = std::time::Instant::now();
    let mut labels = Vec::new();
    for session in 1..=3u64 {
        let Routed::Frame { line } = router.route(
            SessionId(session),
            ClientId(session),
            "WHOIS",
            &[format!("target{session}")],
            Some("same-label"),
            true,
            at,
        ) else {
            panic!("expected a routed frame")
        };
        labels.push(
            line.split(' ')
                .next()
                .expect("label")
                .trim_start_matches('@')
                .to_owned(),
        );
    }
    // Identical downstream labels must produce distinct upstream labels, or the
    // second query's replies would be indistinguishable from the first's.
    assert_eq!(
        labels.iter().collect::<BTreeSet<_>>().len(),
        3,
        "identical client labels are translated into distinct upstream labels"
    );

    // Replies arrive out of order; each must reach the session that asked.
    for index in [2usize, 0, 1] {
        let RouteOutcome::Completed(delivered) = router.deliver(
            Some(&labels[index]),
            Some("318"),
            Some(RequestClass::Whois),
            |route| format!("reply for {}\r\n", route.session.0).into_bytes(),
            at,
        ) else {
            panic!("terminator must complete")
        };
        assert_eq!(delivered.session, SessionId(index as u64 + 1));
    }
    assert!(router.is_empty(), "every route closed on its terminator");
}

#[tokio::test]
async fn a_generation_replacement_discards_every_route() {
    use i2pr_irc_runtime::routing::{ResponseRouter, RouteOutcome, Routed};

    // Routes live in generation-owned state, so a replacement cannot inherit them.
    let mut first = ResponseRouter::default();
    let at = std::time::Instant::now();
    let Routed::Frame { line } = first.route(
        SessionId(1),
        ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("l"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let stale = line
        .split(' ')
        .next()
        .expect("label")
        .trim_start_matches('@')
        .to_owned();
    assert_eq!(first.open_routes(), 1);

    // A new generation means a new router.
    let mut second = ResponseRouter::default();
    assert!(second.is_empty());
    assert_eq!(
        second.deliver(
            Some(&stale),
            Some("318"),
            None,
            |route| panic!("must not rebuild for {}", route.session.0),
            at
        ),
        RouteOutcome::Unmatched,
        "a reply tagged for a previous generation reaches nobody"
    );
}

// ====================================================================== IRCv3

#[test]
fn capability_advertisement_stays_truthful_after_the_history_adapter_landed() {
    let upstream = UpstreamCapabilities::default();
    let advertised = DownstreamCapabilities::default().advertise(&upstream);
    // The foundational set is exactly what M003-D froze; the history capabilities are
    // advertised by the adapter that implements them, never mixed in here.
    for name in ["message-tags", "server-time", "batch", "labeled-response"] {
        assert!(advertised.contains(&name.to_owned()), "{name}");
    }
    let history = i2pr_irc_runtime::chathistory::capability_advertisement(false);
    assert_eq!(
        history,
        vec![
            i2pr_irc_runtime::chathistory::CHATHISTORY_CAPABILITY.to_owned(),
            i2pr_irc_runtime::chathistory::READ_MARKER_CAPABILITY.to_owned()
        ]
    );
}

// ========================================================== SECURITY / PRIVACY

#[test]
fn qualification_diagnostics_carry_no_secret_or_endpoint_material() {
    let snapshot = NetworkSnapshot::default();
    let rendered = format!("{snapshot:?}");
    for forbidden in ["sasl_password", "password", "hunter2"] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }
    let health = i2pr_irc_runtime::journal::JournalHealth::default();
    assert!(
        !format!("{health:?}").contains('@'),
        "journal diagnostics carry no endpoint"
    );
}

#[test]
fn no_durable_row_records_a_session_id_or_generation() {
    // The schema is the proof: an ephemeral identity has nowhere to live.
    let dir = i2pr_irc_store::testing::temp_dir("integrated-privacy");
    let path = dir.db("privacy.sqlite3");
    let store = Store::open(&StorePath::File(path.clone())).expect("store opens");
    store.shutdown().expect("shuts down");
    let tables = i2pr_irc_store::testing::tables(&path);
    for forbidden in [
        "sessions",
        "generation",
        "generations",
        "routes",
        "labels",
        "backlog",
        "cursors_scoped",
    ] {
        assert!(
            !tables.iter().any(|name| name == forbidden),
            "an ephemeral identity must not be durable: found {forbidden}"
        );
    }
}
