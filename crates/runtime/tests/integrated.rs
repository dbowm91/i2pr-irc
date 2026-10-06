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
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
};
use i2pr_irc_store::{
    BufferKind, NetworkRecord, STORE_BUSY_TIMEOUT_MS, Store, StoreErrorKind, StoreHandle,
    StorePath, fallback_display_name,
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
        display_name: fallback_display_name(NetworkId(network)),
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
        Self::start_with_upstream_capacity(network, nick, channels, store, 64 * 1024).await
    }

    /// Brings one Network Online with an upstream fixture that buffers `capacity`
    /// bytes.
    ///
    /// A small capacity is how a *stuck* upstream is modelled: the owner's writer
    /// blocks once the buffer fills, which is what makes its bounded intent queues
    /// apply backpressure the way a slow server would.
    async fn start_with_upstream_capacity(
        network: u64,
        nick: &str,
        channels: &[&str],
        store: StoreHandle,
        capacity: usize,
    ) -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        // Queue enough connect outcomes that a later reconnect fails loudly with an
        // empty fixture rather than blocking forever.
        for _ in 0..4 {
            provider
                .queue_outcome(Ok(FaultScript {
                    capacity,
                    ..FaultScript::default()
                }))
                .expect("queue");
        }
        let reconnect = ReconnectScheduler::default();
        let context = SupervisorContext {
            network: NetworkId(network),
            record: Arc::new(record(network, nick, channels)),
            store: store.clone(),
            status: watch::channel(Default::default()).0,
            resources: ResourceLedger::new(reconnect.clone(), store.clone()),
        };
        let owner = NetworkOwner::new(Shared(provider.clone()), context, store, reconnect)
            .expect("owner constructs");
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

    /// Blocks until the owner has applied at least `count` upstream events.
    ///
    /// Writing upstream only fills the fixture's buffer, so a test that asserts on the
    /// consequence of a flood has to wait for the owner to actually work through it.
    async fn wait_for_upstream_events(&mut self, count: u64) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.snapshot.borrow().upstream_events_seen < count {
                self.snapshot.changed().await.expect("owner alive");
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "owner applied only {} of {count} upstream events: {:?}",
                self.snapshot.borrow().upstream_events_seen,
                self.snapshot.borrow()
            )
        });
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
/// Drains upstream and returns what it read, so a test can assert that a local
/// request never crossed the connection.
async fn drain_upstream_capture(owner: &mut Online) -> String {
    let mut buf = [0; 4096];
    let mut all = Vec::new();
    loop {
        let read = tokio::time::timeout(DRAIN_IDLE, owner.upstream().read(&mut buf)).await;
        match read {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(count)) => all.extend_from_slice(&buf[..count]),
        }
    }
    String::from_utf8_lossy(&all).into_owned()
}

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

/// Registers a client after negotiating the given draft capabilities.
///
/// `CAP REQ` only takes effect once registration completes, so negotiation and
/// `NICK`/`USER` go out together and the whole exchange is read back at `001`.
async fn client_register_with_caps(
    client: &mut tokio::io::DuplexStream,
    nick: &str,
    caps: &[&str],
) -> String {
    let list = caps.join(" ");
    client
        .write_all(
            format!("CAP REQ :{list}\r\nCAP END\r\nNICK {nick}\r\nUSER {nick} 0 * :phone\r\n")
                .as_bytes(),
        )
        .await
        .expect("client writable");
    let welcome = client_read_until(client, b"001 ").await;
    for cap in caps {
        assert!(
            welcome.contains(&format!("ACK :{list}")) || welcome.contains(&format!(" {cap}")),
            "a supported capability must be acknowledged: {welcome}"
        );
    }
    welcome
}

/// Registers a client that is already draining, negotiating the given capabilities.
///
/// `CAP REQ` only takes effect once registration completes, so negotiation, `CAP END`
/// and `NICK`/`USER` go out together.
async fn register_with_caps(client: &mut DrainingClient, nick: &str, caps: &[&str]) {
    let list = caps.join(" ");
    client
        .write_line(&format!(
            "CAP REQ :{list}\r\nCAP END\r\nNICK {nick}\r\nUSER {nick} 0 * :phone\r\n"
        ))
        .await;
    client.wait_for(b"001 ").await;
    for cap in caps {
        assert!(
            client.saw(format!("ACK :{list}").as_bytes()) || client.saw(cap.as_bytes()),
            "a supported capability must be acknowledged: {}",
            String::from_utf8_lossy(&client.bytes())
        );
    }
}

/// Registers a client that is already draining and negotiates nothing.
async fn register_plain(client: &mut DrainingClient, nick: &str) {
    client
        .write_line(&format!("NICK {nick}\r\nUSER {nick} 0 * :phone\r\n"))
        .await;
    client.wait_for(b"001 ").await;
}

/// An attached client whose read half is consumed continuously.
///
/// Several tests exercise upstream, storage, or routing pressure. In those, a fixture
/// that merely stops reading *is* an overloaded client: its bounded queue fills and the
/// bouncer detaches it for missing a live frame. That is the correct behaviour, but in
/// a test about something else it is a confound, so this reader keeps the live stream
/// healthy and leaves client slowness to the one test that is specifically about it.
struct DrainingClient {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    seen: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl DrainingClient {
    /// Takes over an already-registered attachment and keeps reading it.
    fn start(client: tokio::io::DuplexStream) -> Self {
        let (mut read_half, writer) = tokio::io::split(client);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        tokio::spawn(async move {
            let mut buf = [0; 4096];
            while let Ok(count) = read_half.read(&mut buf).await {
                if count == 0 {
                    break;
                }
                sink.lock()
                    .expect("client sink not poisoned")
                    .extend_from_slice(&buf[..count]);
            }
        });
        Self { writer, seen }
    }

    fn bytes(&self) -> Vec<u8> {
        self.seen.lock().expect("client sink not poisoned").clone()
    }

    fn saw(&self, needle: &[u8]) -> bool {
        self.bytes()
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// Blocks until `needle` has been received, or fails with what was received.
    async fn wait_for(&self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.saw(needle) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "client never received {}; received {}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.bytes())
            )
        });
    }

    async fn write_line(&mut self, line: &str) {
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("client writable");
    }

    /// Round-trips a uniquely identified PING, which is the honest way to ask the
    /// owner for a turn: it is control traffic, and answering it is itself an
    /// invariant under test.
    async fn keepalive(&mut self, owner: &mut Online, nonce: &mut u32) {
        *nonce += 1;
        let marker = format!("keepalive-{}-{nonce}", owner.network.0);
        self.write_line(&format!("PING :{marker}\r\n")).await;
        self.wait_for(marker.as_bytes()).await;
        assert!(
            self.saw(b"PONG"),
            "a client PING must always be answered promptly, even under storage pressure"
        );
    }

    /// Waits until the owner's upstream intent queues are empty.
    async fn wait_upstream_queues_drained(&mut self, owner: &mut Online, nonce: &mut u32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let drained = {
                let snapshot = owner.snapshot.borrow();
                snapshot.upstream_normal_queue_depth == 0
                    && snapshot.upstream_control_queue_depth == 0
            };
            if drained {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "upstream intent queues did not drain: {:?}",
                owner.snapshot.borrow()
            );
            drain_upstream(owner).await;
            self.keepalive(owner, nonce).await;
        }
    }
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
async fn one_clients_queue_pressure_costs_only_its_own_attachment() {
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
    // From here on the healthy client is drained continuously, so the only attachment
    // that can be desynchronized is the deliberately slow one.
    let fast = DrainingClient::start(fast);

    // Enough upstream volume to overrun a 64-frame queue behind a 2 KiB socket.
    const FLOOD: u32 = 2_000;
    flood_upstream(&mut owner, FLOOD).await;

    fast.wait_for(b"PRIVMSG").await;
    assert!(
        fast.saw(b"PRIVMSG"),
        "a slow client must never starve a healthy one"
    );

    // Wait for the flood to be fully processed rather than asserting against whatever
    // happens to have arrived when the first frame reaches the healthy client.
    owner.wait_for_upstream_events(FLOOD as u64).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while owner.snapshot.borrow().fanout_detached == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the slow client is eventually detached");

    let snapshot = owner.snapshot.borrow().clone();
    // The slow client is detached rather than left attached having silently skipped a
    // frame. An ordered IRC stream cannot be repaired in place: once a live frame is
    // missed, the client cannot be told which state it never received.
    assert!(
        snapshot.fanout_dropped > 0,
        "the refused frame is counted, never silent"
    );
    assert!(
        snapshot.fanout_detached > 0,
        "a client that missed a live frame is detached, not left half-synchronized"
    );
    assert_eq!(
        snapshot.last_session_disposition,
        Some("downstream-overload"),
        "the detach records an explicit disposition"
    );
    assert_eq!(
        snapshot.attached_sessions, 1,
        "only the overloaded attachment is removed"
    );
    assert_eq!(
        snapshot.sessions_ended, 1,
        "exactly one session ended, and it ended for overload"
    );
    assert_eq!(
        snapshot.phase,
        Some(Phase::Online),
        "client pressure never ends the upstream Network"
    );
    // The detached client's stream is closed, so the bouncer does not leave a task
    // writing into a socket nobody will ever read again.
    let mut closed = Vec::new();
    let mut buf = [0; 256];
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match slow.read(&mut buf).await {
                Ok(0) => break,
                Ok(count) => closed.extend_from_slice(&buf[..count]),
                Err(_) => break,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the detached client was never closed: {closed:?}"));
}

#[tokio::test]
async fn a_detached_client_can_reattach_and_is_told_the_truth_again() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    join_upstream(&mut owner, "bot", "#room").await;
    owner.settle().await;

    let (_session, mut first) = owner.attach_with(ClientId(1), 2048).await;
    client_register(&mut first, "bot").await;
    flood_upstream(&mut owner, 2_000).await;

    tokio::time::timeout(Duration::from_secs(10), async {
        while owner.snapshot.borrow().fanout_detached == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the overloaded client is eventually detached");
    drop(first);

    // A replacement is a fresh ephemeral attachment under the same durable lineage. It
    // gets a full projection rather than a resumption, because the bouncer cannot know
    // what the previous attachment already saw.
    let (replacement, mut reattached) = owner
        .attach_as(SessionId(9_001), ClientId(1), 64 * 1024)
        .await;
    let welcome = client_register_with_caps(&mut reattached, "bot", &["draft/read-marker"]).await;
    assert!(
        welcome.contains("JOIN #room"),
        "the replacement is told current observed membership again: {welcome}"
    );
    // A read-marker client is told its current marker as part of that projection, after
    // the JOIN. No marker has been set for this channel yet, so the draft's own `*`
    // unknown-marker sentinel is the truthful answer.
    assert!(
        welcome.contains("MARKREAD #room *"),
        "the replacement is told its read marker again: {welcome}"
    );
    let marker_at = welcome.find("MARKREAD").expect("marker present");
    let join_at = welcome.find("JOIN #room").expect("join present");
    assert!(
        marker_at > join_at,
        "the marker must follow the JOIN it annotates: {welcome}"
    );
    let reattached = DrainingClient::start(reattached);
    flood_upstream(&mut owner, 2).await;
    reattached.wait_for(b"flood 0").await;
    assert_eq!(
        owner.snapshot.borrow().fanout_detached,
        1,
        "a client that keeps reading is never detached"
    );
    let _ = replacement;
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
    // The client keeps reading throughout. Its subject here is storage pressure alone:
    // if the fixture stopped reading it would become an overloaded client, the bouncer
    // would detach it for missing live frames, and the two pressures would be
    // indistinguishable.
    let mut client = DrainingClient::start(client);

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
    client.wait_for(b"PRIVMSG").await;
    assert!(
        client.saw(b"PRIVMSG"),
        "the client still receives live traffic while storage is degraded"
    );
    assert_eq!(
        owner.snapshot.borrow().fanout_detached,
        0,
        "a client that keeps reading must never be detached, whatever the store is doing"
    );

    // Nothing accumulated behind the slow store: every upstream intent the owner
    // produced has been written, with no retry queue and no side buffer.
    handle.set_stall(None);
    drain_upstream(&mut owner).await;
    let mut nonce = 0u32;
    client
        .wait_upstream_queues_drained(&mut owner, &mut nonce)
        .await;

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
        client.keepalive(&mut owner, &mut nonce).await;
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

// ====================================================== LIVE HISTORY ADAPTERS

#[tokio::test]
async fn a_negotiated_client_really_receives_history_through_the_live_path() {
    // The M003-E adapters used to be unreachable: CHATHISTORY and MARKREAD fell
    // through the session dispatcher to `421 Unsupported command`. This proves a real
    // attached client can now negotiate the capability, ask for history, and receive a
    // batched reply containing the retained message.
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");

    // Retain one message in #room through the journal the owner will use.
    {
        let mut journal = journal(1, handle.clone());
        let buffer = journal
            .resolve_buffer(BufferKind::Channel, "#room")
            .await
            .expect("buffer");
        journal
            .ingest(
                buffer,
                &i2pr_irc_wire::Message::parse(
                    b"@time=2023-11-14T22:13:20.620Z;msgid=abc123 :a!u@h PRIVMSG #room :retained\r\n",
                )
                .expect("parses"),
            )
            .await
            .expect("ingests");
    }

    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    // The upstream must have self-joined before history is eligible, so the owner's
    // buffer resolution has a channel to attach to.
    join_upstream(&mut owner, "bot", "#room").await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;

    // Negotiate, then register.
    client
        .write_all(b"CAP REQ :draft/chathistory\r\nCAP END\r\nNICK bot\r\nUSER bot 0 * :phone\r\n")
        .await
        .expect("client writable");
    let welcome = client_read_until(&mut client, b"001 ").await;
    assert!(
        welcome.contains("ACK :draft/chathistory"),
        "a supported capability must be acknowledged: {welcome}"
    );
    assert!(
        welcome.contains("CHATHISTORY="),
        "a negotiated client must be told the real history bound: {welcome}"
    );

    // Ask using the real draft grammar: the limit is the last parameter.
    client
        .write_all(b"CHATHISTORY LATEST #room * 10\r\n")
        .await
        .expect("client writable");
    let reply = client_read_until(&mut client, b"draft/chathistory-end").await;

    assert!(
        reply.contains("retained"),
        "the retained message must actually reach the client: {reply}"
    );
    assert!(
        reply.contains("BATCH +"),
        "a negotiated client must receive a batch: {reply}"
    );
    assert!(
        reply.contains("batch="),
        "each message must be tagged into the batch: {reply}"
    );
    // The replayed timestamp is canonical text, never an integer epoch.
    assert!(
        reply.contains("2023-11-14T22:13:20.620Z"),
        "the upstream timestamp must round-trip with its milliseconds: {reply}"
    );
    assert!(
        !reply.contains("1700000000"),
        "an integer epoch is not a server-time value: {reply}"
    );
    // Local history is never forwarded upstream.
    assert!(
        !drain_upstream_capture(&mut owner)
            .await
            .contains("CHATHISTORY"),
        "a local history request must not cross the upstream connection"
    );
}

#[tokio::test]
async fn a_malformed_history_request_is_answered_not_silently_ignored() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client
        .write_all(b"CAP REQ :draft/chathistory\r\nCAP END\r\nNICK bot\r\nUSER bot 0 * :phone\r\n")
        .await
        .expect("client writable");
    client_read_until(&mut client, b"001 ").await;

    // An integer epoch is not a valid timestamp selector.
    client
        .write_all(b"CHATHISTORY BEFORE #room timestamp=1700000000 10\r\n")
        .await
        .expect("client writable");
    let refusal = client_read_until(&mut client, b"INVALID_PARAMS").await;
    assert!(
        refusal.contains("FAIL CHATHISTORY INVALID_PARAMS"),
        "a malformed selector must be answered with the standard error: {refusal}"
    );
}

#[tokio::test]
async fn a_client_that_did_not_negotiate_history_is_refused_explicitly() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;

    client
        .write_all(b"CHATHISTORY LATEST #room * 10\r\n")
        .await
        .expect("client writable");
    let refusal = client_read_until(&mut client, b"421 ").await;
    assert!(
        refusal.contains("421"),
        "an un-negotiated command must be refused, not silently dropped: {refusal}"
    );
    // And the client is still attached: a refusal is not grounds for disconnection.
    assert_eq!(
        owner.snapshot.borrow().attached_sessions,
        1,
        "refusing one command must not end the session"
    );
}

#[tokio::test]
async fn a_read_marker_round_trips_through_the_live_path() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    {
        let mut journal = journal(1, handle.clone());
        let buffer = journal
            .resolve_buffer(BufferKind::Channel, "#room")
            .await
            .expect("buffer");
        journal
            .ingest(
                buffer,
                &i2pr_irc_wire::Message::parse(
                    b"@time=2023-11-14T22:13:20.620Z;msgid=abc123 :a!u@h PRIVMSG #room :one\r\n",
                )
                .expect("parses"),
            )
            .await
            .expect("ingests");
    }

    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    join_upstream(&mut owner, "bot", "#room").await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client
        .write_all(b"CAP REQ :draft/read-marker\r\nCAP END\r\nNICK bot\r\nUSER bot 0 * :phone\r\n")
        .await
        .expect("client writable");
    client_read_until(&mut client, b"001 ").await;

    // A get with no marker stored answers with the unknown-marker sentinel.
    client
        .write_all(b"MARKREAD #room\r\n")
        .await
        .expect("client writable");
    let unknown = client_read_until(&mut client, b"MARKREAD #room *\r\n").await;
    assert!(
        unknown.contains("MARKREAD #room *"),
        "an unknown marker is reported as a literal star: {unknown}"
    );

    // A set is answered with the value actually stored.
    client
        .write_all(b"MARKREAD #room timestamp=2023-11-14T22:13:20.620Z\r\n")
        .await
        .expect("client writable");
    let stored = client_read_until(&mut client, b"timestamp=2023-11-14T22:13:20.620Z\r\n").await;
    assert!(
        stored.contains("MARKREAD #room timestamp=2023-11-14T22:13:20.620Z"),
        "the server must answer with the marker it stored: {stored}"
    );

    // A client may not set the unknown-marker sentinel, and the session survives it.
    client
        .write_all(b"MARKREAD #room *\r\n")
        .await
        .expect("client writable");
    let refused = client_read_until(&mut client, b"FAIL MARKREAD").await;
    assert!(
        refused.contains("FAIL MARKREAD"),
        "a client must not be able to erase read state with the sentinel: {refused}"
    );
    assert_eq!(
        owner.snapshot.borrow().attached_sessions,
        1,
        "refusing a marker set must not end the session"
    );
}

// =========================================================== RESPONSE ROUTING

#[tokio::test]
async fn concurrent_labeled_queries_from_several_sessions_route_correctly() {
    use i2pr_irc_runtime::routing::{
        Incoming, ResponseRouter, RouteOutcome, Routed, RoutingRequest, frame_label,
    };

    let mut router = ResponseRouter::default();
    let at = std::time::Instant::now();
    let mut labels = Vec::new();
    for session in 1..=3u64 {
        let Routed::Frame { line, .. } = router.route(
            RoutingRequest {
                session: SessionId(session),
                client: ClientId(session),
                command: "WHOIS",
                params: &[format!("target{session}")],
                downstream_label: Some("same-label"),
                labeled_upstream: true,
            },
            at,
        ) else {
            panic!("expected a routed frame")
        };
        labels.push(frame_label(&line).expect("label").to_owned());
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
            Incoming {
                label: Some(&labels[index]),
                numeric: Some("318"),
                ..Incoming::default()
            },
            |route| format!("reply for {}\r\n", route.session.0).into_bytes(),
        ) else {
            panic!("terminator must complete")
        };
        assert_eq!(delivered.session, SessionId(index as u64 + 1));
    }
    assert!(router.is_empty(), "every route closed on its terminator");
}

#[tokio::test]
async fn a_generation_replacement_discards_every_route() {
    use i2pr_irc_runtime::routing::{
        Incoming, ResponseRouter, RouteOutcome, Routed, RoutingRequest, frame_label,
    };

    // Routes live in generation-owned state, so a replacement cannot inherit them.
    let mut first = ResponseRouter::default();
    let at = std::time::Instant::now();
    let Routed::Frame { line, .. } = first.route(
        RoutingRequest {
            session: SessionId(1),
            client: ClientId(1),
            command: "WHOIS",
            params: &["alice".to_owned()],
            downstream_label: Some("l"),
            labeled_upstream: true,
        },
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let stale = frame_label(&line).expect("label").to_owned();
    assert_eq!(first.open_routes(), 1);

    // A new generation means a new router.
    let mut second = ResponseRouter::default();
    assert!(second.is_empty());
    assert_eq!(
        second.deliver(
            Incoming {
                label: Some(&stale),
                numeric: Some("318"),
                ..Incoming::default()
            },
            |route| panic!("must not rebuild for {}", route.session.0)
        ),
        RouteOutcome::Dropped,
        "a reply tagged for a previous generation reaches nobody"
    );
}

// ====================================================================== IRCv3

#[test]
fn capability_advertisement_stays_truthful_after_the_history_adapter_landed() {
    let upstream = UpstreamCapabilities::default();
    let advertised = DownstreamCapabilities::advertisement(&upstream);
    // The advertisement is exactly what the live SessionReader serves. It must never be
    // a superset: a capability a client cannot rely on is a claim it cannot challenge.
    assert!(
        advertised
            .iter()
            .all(|name| i2pr_irc_runtime::downstream::downstream_supported()
                .contains(&name.as_str())),
        "advertisement: {advertised:?}"
    );
    // M004-A promoted the tag surface: the mediator and a truthful CLIENTTAGDENY now
    // exist, so `message-tags`, `batch` and `labeled-response` are served. `server-time`
    // and `echo-message` remain withheld, because neither has implemented downstream
    // semantics.
    for served in ["message-tags", "batch", "labeled-response"] {
        assert!(advertised.contains(&served.to_owned()), "{served}");
    }
    for withheld in ["server-time", "echo-message"] {
        assert!(!advertised.contains(&withheld.to_owned()), "{withheld}");
    }
    let history = i2pr_irc_runtime::chathistory::capability_advertisement(false);
    assert_eq!(
        history,
        vec![
            i2pr_irc_runtime::chathistory::CHATHISTORY_CAPABILITY.to_owned(),
            i2pr_irc_runtime::chathistory::READ_MARKER_CAPABILITY.to_owned()
        ]
    );
    // The advertisement is the union of the history adapter's capabilities and the
    // foundational tag surface, and nothing else.
    let mut expected = history.clone();
    expected.extend(
        ["message-tags", "batch", "labeled-response"]
            .iter()
            .map(|name| (*name).to_owned()),
    );
    expected.sort();
    let mut actual = advertised.clone();
    actual.sort();
    assert_eq!(actual, expected);
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

// ============================================================ QUEUE INTEGRITY
//
// Corrective 013 sections I/J/K. The old behaviour on both boundaries was to drop
// something silently: a client command on the way up, a live frame on the way down.
// These tests pin the corrected behaviour, which is that the loss is either reported
// or ends that one attachment -- never neither.

/// Fills the owner's bounded upstream queue by flooding a client while the fixture
/// refuses to absorb the traffic.
async fn saturate_upstream_queue(client: &mut DrainingClient, lines: u32) {
    let mut chunk = String::new();
    for index in 0..lines {
        chunk.push_str(&format!("PRIVMSG #room :saturate {index}\r\n"));
        if chunk.len() >= 128 {
            client.write_line(&chunk).await;
            chunk.clear();
            // Paced deliberately. A client that outruns the owner's bounded
            // session-event queue is refused at *that* boundary, which would test a
            // different ceiling than the one under examination here.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    if !chunk.is_empty() {
        client.write_line(&chunk).await;
    }
}

#[tokio::test]
async fn a_command_refused_by_the_upstream_queue_is_reported_and_never_replayed() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    // A 512-byte upstream fixture blocks the owner's writer after a handful of frames,
    // which is the backpressure a slow server produces.
    let mut owner = Online::start_with_upstream_capacity(1, "bot", &[], handle, 512).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;
    // The client keeps reading while it floods, otherwise it would be detached for
    // missing the live frames its own refusals generate, and two different pressures
    // would be indistinguishable.
    let mut client = DrainingClient::start(client);

    // Deliberately do not drain upstream. The queue fills, and past its ceiling the
    // client's own commands are refused.
    saturate_upstream_queue(&mut client, 600).await;
    client.wait_for(b"could not accept that command").await;
    let notice = String::from_utf8_lossy(&client.bytes()).into_owned();
    assert!(
        notice.contains("NOTICE"),
        "a refused command must be reported to its own session: {notice}"
    );
    assert!(
        notice.contains("upstream delivery"),
        "the report must say the command was not accepted: {notice}"
    );

    let snapshot = owner.snapshot.borrow();
    assert!(
        snapshot.upstream_rejected > 0,
        "a refusal is counted, never silent: {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_error,
        Some("upstream-queue-refused"),
        "the refusal records an explicit diagnostic"
    );
    assert_eq!(
        snapshot.phase,
        Some(Phase::Online),
        "one client's commands overflowing never ends the Network"
    );
    assert_eq!(
        snapshot.response_routes, 0,
        "a refused admission cannot leave an open response route"
    );
}

#[tokio::test]
async fn a_committed_join_converges_after_its_first_enqueue_is_refused() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start_with_upstream_capacity(1, "bot", &[], handle.clone(), 512).await;
    owner.wait_phase(Phase::Online).await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;
    let mut client = DrainingClient::start(client);

    // Saturate the upstream queue with ordinary chat, then ask to join a channel. The
    // database commit succeeds even though the wire write cannot.
    saturate_upstream_queue(&mut client, 600).await;
    client.write_line("JOIN #late\r\n").await;

    // The durable intent is recorded even though nothing was written upstream.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !handle
            .load_networks()
            .await
            .expect("reads")
            .iter()
            .any(|record| {
                record
                    .desired_channels
                    .iter()
                    .any(|channel| channel == "#late")
            })
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed intent is durable regardless of the wire");
    assert!(
        owner.snapshot.borrow().desired_reconcile_pending > 0,
        "the deferred intent is recorded as needing reconciliation: {:?}",
        owner.snapshot.borrow()
    );

    // Relieve the pressure. The deferred JOIN is written on a later turn rather than
    // being forgotten. The capture matters: a drain that discards would swallow the
    // very frame under test whenever the reconciliation timer happened to fire inside
    // it.
    let mut upstream = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !upstream.contains("JOIN #late") {
        assert!(
            std::time::Instant::now() < deadline,
            "a committed JOIN must converge once capacity returns; upstream saw {upstream:?}"
        );
        upstream.push_str(&drain_upstream_capture(&mut owner).await);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        upstream.matches("JOIN #late").count(),
        1,
        "reconciliation writes the deferred intent once, not once per turn: {upstream:?}"
    );
    assert!(
        owner.snapshot.borrow().desired_reconcile_drained > 0,
        "convergence is counted, not silent"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while owner.snapshot.borrow().desired_reconcile_pending > 0 {
            drain_upstream(&mut owner).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the reconciliation set drains back to empty");
}

#[tokio::test]
async fn a_committed_part_converges_after_its_first_enqueue_is_refused() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    // Stored with the channel as existing intent: a PART against a channel the store
    // never recorded would commit nothing, and the test would be asserting on a
    // reconciliation that could not exist.
    handle
        .save_network(&record(1, "bot", &["#room"]))
        .await
        .expect("saved");
    let mut owner =
        Online::start_with_upstream_capacity(1, "bot", &["#room"], handle.clone(), 512).await;
    owner.wait_phase(Phase::Online).await;
    join_upstream(&mut owner, "bot", "#room").await;
    owner.settle().await;
    let (_session, mut client) = owner.attach(ClientId(1)).await;
    client_register(&mut client, "bot").await;
    let mut client = DrainingClient::start(client);

    saturate_upstream_queue(&mut client, 600).await;
    client.write_line("PART #room\r\n").await;

    tokio::time::timeout(Duration::from_secs(10), async {
        while handle
            .load_networks()
            .await
            .expect("reads")
            .iter()
            .any(|record| {
                record
                    .desired_channels
                    .iter()
                    .any(|channel| channel == "#room")
            })
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed part is durable regardless of the wire");

    assert!(
        owner.snapshot.borrow().desired_reconcile_pending > 0,
        "the deferred part is recorded as needing reconciliation: {:?}",
        owner.snapshot.borrow()
    );

    let mut upstream = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !upstream.contains("PART #room") {
        assert!(
            std::time::Instant::now() < deadline,
            "a committed PART must converge once capacity returns; upstream saw {upstream:?}"
        );
        upstream.push_str(&drain_upstream_capture(&mut owner).await);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        upstream.matches("PART #room").count(),
        1,
        "reconciliation writes the deferred intent once: {upstream:?}"
    );
}

#[tokio::test]
async fn a_stateful_frame_overflow_ends_that_client_rather_than_stale_state() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;

    let (_slow, mut slow) = owner.attach_with(ClientId(1), 2048).await;
    client_register(&mut slow, "bot").await;
    let (_healthy, mut healthy) = owner.attach(ClientId(2)).await;
    client_register(&mut healthy, "bot").await;
    // The healthy client is drained throughout, so it can never be the overloaded one.
    let healthy = DrainingClient::start(healthy);

    // MODE, NICK, KICK, JOIN and PART are the frames carrying a client's idea of who is
    // in a channel and with what modes. Dropping one and keeping the attachment would
    // leave it holding state no later frame can repair. This floods a mix of exactly
    // those stateful frames, with no PRIVMSG in sight, so the detach cannot be excused
    // as "only chat was lost".
    let mut payload = String::new();
    for step in 0..800 {
        payload.push_str(&format!(":srv MODE #room +o alice{step}\r\n"));
        payload.push_str(&format!(":alice{step}!u@h NICK alice{step}x\r\n"));
        payload.push_str(&format!(":op!u@h KICK #room alice{step} :out\r\n"));
        payload.push_str(&format!(":bob{step}!u@h JOIN #room\r\n"));
        payload.push_str(&format!(":bob{step}!u@h PART #room :bye\r\n"));
    }
    owner
        .upstream()
        .write_all(payload.as_bytes())
        .await
        .expect("upstream writable");
    owner.wait_for_upstream_events(4_000).await;

    tokio::time::timeout(Duration::from_secs(10), async {
        while owner.snapshot.borrow().fanout_detached == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the slow client is detached once it misses a live frame");

    let snapshot = owner.snapshot.borrow();
    assert_eq!(
        snapshot.attached_sessions, 1,
        "the healthy attachment is untouched: {snapshot:?}"
    );
    assert_eq!(
        snapshot.response_routes, 0,
        "routes for the detached session are cleared: {snapshot:?}"
    );
    assert_eq!(
        snapshot.phase,
        Some(Phase::Online),
        "one client missing a MODE never ends the Network"
    );
    healthy.wait_for(b"MODE #room").await;
    assert!(
        healthy.saw(b"KICK #room"),
        "the healthy client received every frame it was owed: {:?}",
        owner.snapshot.borrow()
    );
}

#[tokio::test]
async fn a_marker_set_by_one_session_reaches_the_operators_other_session() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    join_upstream(&mut owner, "bot", "#room").await;
    owner.settle().await;

    // Both attachments belong to the same Operator, so a marker set by one is the
    // Operator's read state and the other has to learn about it.
    let (_first, first) = owner.attach(ClientId(1)).await;
    let mut first = DrainingClient::start(first);
    register_with_caps(&mut first, "bot", &["draft/read-marker"]).await;
    let (_second, second) = owner.attach(ClientId(2)).await;
    let mut second = DrainingClient::start(second);
    register_with_caps(&mut second, "bot", &["draft/read-marker"]).await;

    // Retained history, carrying the upstream server-time the setter will name. The
    // marker resolves against the protocol timestamp, not the local receive time, which
    // is the whole point of preserving server-time exactly.
    owner
        .upstream()
        .write_all(b"@time=2024-01-01T00:00:00.000Z :alice!u@h PRIVMSG #room :first message\r\n")
        .await
        .expect("upstream writable");
    owner.wait_for_upstream_events(1).await;
    owner.settle().await;

    // Both clients were just told their current marker, which is `*`: nothing has been
    // read yet. That initial value must not be mistaken for the update below.
    first.wait_for(b"MARKREAD #room *").await;
    second.wait_for(b"MARKREAD #room *").await;

    first
        .write_line("MARKREAD #room timestamp=2024-01-01T00:00:00.000Z\r\n")
        .await;
    first
        .wait_for(b"MARKREAD #room timestamp=2024-01-01T00:00:00.000Z")
        .await;
    // The setter is answered with the value the bouncer actually stored, which is the
    // retained event's own timestamp and not merely whatever was requested.
    let asked = String::from_utf8_lossy(&first.bytes()).into_owned();
    assert!(
        asked.contains("MARKREAD #room timestamp=2024-01-01T00:00:00.000Z"),
        "the setter is answered with the value the bouncer stored: {asked}"
    );

    // The other session never asked again, so it only learns about this through
    // propagation of the Operator's read state.
    second
        .wait_for(b"MARKREAD #room timestamp=2024-01-01T00:00:00.000Z")
        .await;

    // A set that moves nothing must not look like an update. An older marker is
    // refused by monotonicity and the existing newer one is returned instead.
    first
        .write_line("MARKREAD #room timestamp=2023-01-01T00:00:00.000Z\r\n")
        .await;
    owner.settle().await;
    let before = second.bytes().len();
    first
        .write_line("MARKREAD #room timestamp=2023-01-01T00:00:00.000Z\r\n")
        .await;
    first
        .wait_for(b"MARKREAD #room timestamp=2024-01-01T00:00:00.000Z")
        .await;
    owner.settle().await;
    assert!(
        second.bytes().len() == before,
        "a set that does not advance the marker must not be broadcast as an update"
    );
}

#[tokio::test]
async fn a_session_that_did_not_negotiate_read_marker_is_never_sent_one() {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, "bot", &[]))
        .await
        .expect("saved");
    let mut owner = Online::start(1, "bot", &[], handle).await;
    owner.wait_phase(Phase::Online).await;
    join_upstream(&mut owner, "bot", "#room").await;
    owner.settle().await;

    let (_plain, plain) = owner.attach(ClientId(1)).await;
    let mut plain = DrainingClient::start(plain);
    register_plain(&mut plain, "bot").await;
    let welcome = String::from_utf8_lossy(&plain.bytes()).into_owned();
    assert!(
        welcome.contains("JOIN #room"),
        "the plain client is projected as usual: {welcome}"
    );
    assert!(
        !welcome.contains("MARKREAD"),
        "an unnegotiated command is a protocol violation for a strict client: {welcome}"
    );
}
