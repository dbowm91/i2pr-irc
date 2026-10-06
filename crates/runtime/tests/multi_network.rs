//! Plan 008 qualification: independent Network ownership, simultaneous sessions,
//! SessionId/ClientId semantics, bounded fanout, and persistence-first desired state.
//!
//! Everything here runs against fake I2P stream providers and scripted local streams.
//! No test may require a real listener, a real router, or any network authority.
use i2pr_irc_core::{ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    RuntimeError,
    catalog::{
        MAX_SUPERVISED_NETWORKS, NetworkCatalog, SupervisorCommand, SupervisorContext,
        SupervisorHandle,
    },
    owner::{MAX_SESSIONS_PER_NETWORK, NetworkOwner, Phase},
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
};
use i2pr_irc_store::{
    NetworkRecord, Store, StoreHandle, StorePath, StoredSecret, attached_channels,
    fallback_display_name,
};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64, nick: &str, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: fallback_display_name(NetworkId(network)),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: nick.into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: i2pr_irc_store::attached_channels(
            &channels
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        ),
    }
}

/// Shared provider so several Network owners can connect independently.
#[derive(Clone)]
struct Shared(Arc<FakeI2pStreamProvider>);
#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Shared {
    async fn connect(
        &self,
        _endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(_endpoint).await
    }
}

/// Builds one Network owner plus its bounded control channel.
struct Harness {
    provider: Arc<FakeI2pStreamProvider>,
    commands: mpsc::Sender<SupervisorCommand>,
    /// Retained so the harness exercises the public routing handle, not just the
    /// raw command channel.
    handle: SupervisorHandle,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    snapshot: watch::Receiver<i2pr_irc_runtime::owner::NetworkSnapshot>,
    network: NetworkId,
    /// The upstream peer for the generation this harness brought Online. Tests drive
    /// this exact stream rather than racing to take another one.
    upstream: Option<ScriptedStream>,
}

impl Harness {
    async fn start(
        network: u64,
        nick: &str,
        channels: &[&str],
        store: StoreHandle,
        online: bool,
    ) -> Self {
        let mut harness = Self::build(network, nick, channels, store).await;
        if online {
            let mut upstream = harness.provider.take_peer().await;
            harness.drive_online(&mut upstream).await;
            harness.upstream = Some(upstream);
        }
        harness
    }

    /// The same harness, registered with the upstream label surface negotiated.
    async fn start_labeled(
        network: u64,
        nick: &str,
        channels: &[&str],
        store: StoreHandle,
    ) -> Self {
        let mut harness = Self::build(network, nick, channels, store).await;
        let mut upstream = harness.provider.take_peer().await;
        harness.drive_online_labeled(&mut upstream).await;
        harness.upstream = Some(upstream);
        harness
    }

    async fn build(network: u64, nick: &str, channels: &[&str], store: StoreHandle) -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        // Queue enough connect outcomes that a later reconnect attempt fails loudly
        // with an empty queue rather than silently blocking.
        for _ in 0..4 {
            provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        }
        let reconnect = ReconnectScheduler::default();
        let resources = ResourceLedger::new(reconnect.clone(), store.clone());
        let context = SupervisorContext {
            network: NetworkId(network),
            record: Arc::new(record(network, nick, channels)),
            store: store.clone(),
            status: watch::channel(Default::default()).0,
            resources: resources.clone(),
        };
        let owner = NetworkOwner::new(Shared(provider.clone()), context, store, reconnect)
            .expect("owner constructs");
        let snapshot = owner.subscribe_snapshot();
        let (command_tx, command_rx) = mpsc::channel(64);
        let handle = SupervisorHandle::new(NetworkId(network), command_tx.clone());
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(async move { owner.serve(command_rx, stop_rx).await });
        Self {
            provider,
            commands: command_tx,
            handle,
            stop,
            task,
            snapshot,
            network: NetworkId(network),
            upstream: None,
        }
    }

    /// The upstream stream for the current generation.
    fn upstream(&mut self) -> &mut ScriptedStream {
        self.upstream
            .as_mut()
            .expect("this harness is online and retains its upstream stream")
    }

    /// Completes registration on the fake upstream so the generation reaches Online.
    async fn drive_online(&self, upstream: &mut ScriptedStream) {
        read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
    }

    /// Completes registration with the label surface negotiated upstream.
    ///
    /// Response routing needs upstream `labeled-response` before the bouncer will put a
    /// label on its own queries. Without it the bouncer correctly falls back to
    /// one-outstanding-query-per-family correlation, which cannot serve two concurrent
    /// lookups or a labeled batch.
    async fn drive_online_labeled(&self, upstream: &mut ScriptedStream) {
        read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :message-tags server-time batch labeled-response\r\n")
            .await
            .unwrap();
        let request = read_until(upstream, b"CAP REQ").await;
        let requested = String::from_utf8_lossy(&request);
        for capability in ["message-tags", "server-time", "batch", "labeled-response"] {
            assert!(
                requested.contains(capability),
                "the bouncer must request {capability}: {requested}"
            );
        }
        upstream
            .write_all(b":srv CAP * ACK :message-tags server-time batch labeled-response\r\n")
            .await
            .unwrap();
        read_until(upstream, b"CAP END").await;
        upstream
            .write_all(b":srv 001 bot :welcome\r\n")
            .await
            .unwrap();
    }

    async fn wait_phase(&mut self, phase: Phase) {
        let timeout = Duration::from_secs(5);
        tokio::time::timeout(timeout, async {
            while self.snapshot.borrow().phase != Some(phase) {
                self.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| {
            let snapshot = self.snapshot.borrow();
            panic!(
                "network {:?} never reached {phase:?}; phase={:?} gen={:?} error={:?}",
                self.network, snapshot.phase, snapshot.generation, snapshot.last_error
            )
        });
    }

    async fn wait_attached(&mut self, count: usize) {
        let timeout = Duration::from_secs(5);
        tokio::time::timeout(timeout, async {
            while self.snapshot.borrow().attached_sessions != count {
                self.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "network {:?} never reached {count} sessions (saw {})",
                self.network,
                self.snapshot.borrow().attached_sessions
            )
        });
    }

    /// Attaches one scripted client and returns its stream plus the SessionId the
    /// bouncer allocated for this attachment.
    async fn attach(&self, client: ClientId) -> (SessionId, tokio::io::DuplexStream) {
        let (client_side, _peer) = tokio::io::duplex(64 * 1024);
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Attach {
                session: SessionId(client.0),
                client,
                stream: Box::new(client_side),
                reply,
            })
            .expect("attach fits the bounded control queue");
        response
            .await
            .expect("owner answers")
            .expect("attach accepted");
        (SessionId(client.0), _peer)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0; 256];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "stream ended while waiting for a frame");
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
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

/// The upstream label the bouncer attached to one specific query.
///
/// The line is selected by *which query it carries*, never by its position in the
/// buffer. A read buffer legitimately begins with whatever the server happened to send
/// first -- a keepalive, a capability frame -- so taking the first line made this pass or
/// fail on scheduling luck rather than on the routing behaviour under test.
fn query_label(text: &str, needle: &str) -> String {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no query for {needle} in {text}"))
        .split_whitespace()
        .next()
        .and_then(|tag| tag.strip_prefix("@label="))
        .unwrap_or_else(|| panic!("query for {needle} carried no label in {text}"))
        .to_owned()
}

async fn read_client_until(stream: &mut tokio::io::DuplexStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 256];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "client stream ended");
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
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

/// Completes local registration and waits for the projection welcome.
///
/// The welcome is the boundary that proves the owner applied the current state to
/// *this* session, so it is the only synchronization point registration needs.
async fn register(client: &mut tokio::io::DuplexStream, nick: &str) -> String {
    client
        .write_all(format!("NICK {nick}\r\nUSER {nick} 0 * :phone\r\n").as_bytes())
        .await
        .unwrap();
    read_client_until(client, b"001 ").await
}

// ------------------------------------------------------------- multi-network

#[tokio::test]
async fn many_networks_supervise_independently() {
    let (_store, handle) = store();
    let mut harnesses = Vec::new();
    for index in 0..8u64 {
        harnesses.push(Harness::start(index + 1, "bot", &[], handle.clone(), true).await);
    }
    for harness in &mut harnesses {
        harness.wait_phase(Phase::Online).await;
    }
    // Every Network reached Online on its own generation, with no shared state.
    for (index, harness) in harnesses.iter().enumerate() {
        assert_eq!(
            harness.snapshot.borrow().network,
            Some(NetworkId(index as u64 + 1))
        );
        assert_eq!(
            harness.snapshot.borrow().generation,
            Some(i2pr_irc_core::ConnectionGeneration(1))
        );
    }
}

#[tokio::test]
async fn one_network_failing_leaves_the_others_online() {
    let (_store, handle) = store();
    let mut healthy = Harness::start(1, "bot", &[], handle.clone(), true).await;
    let mut failing = Harness::start(2, "bot", &[], handle.clone(), true).await;
    healthy.wait_phase(Phase::Online).await;
    failing.wait_phase(Phase::Online).await;

    // The failing Network's next connect attempt is refused by the provider, so its
    // owner must cycle through backoff on its own.
    for _ in 0..4 {
        failing
            .provider
            .queue_outcome(Err(i2pr_irc_core::ProviderError::Unavailable))
            .unwrap();
    }
    // Ending this Network's current upstream stream is what triggers the failure.
    failing.upstream = None;
    drop(failing.upstream.take());

    failing.wait_phase(Phase::Backoff).await;

    // The healthy Network is untouched: same phase, same generation, still attached.
    assert_eq!(healthy.snapshot.borrow().phase, Some(Phase::Online));
    assert_eq!(
        healthy.snapshot.borrow().generation,
        Some(i2pr_irc_core::ConnectionGeneration(1)),
        "another Network's reconnect cycle must not advance this one"
    );
    assert_eq!(healthy.snapshot.borrow().reconnect_attempt, 0);
}

#[tokio::test]
async fn the_catalog_supervises_a_bounded_number_of_networks() {
    let (_store, handle) = store();
    let mut catalog = NetworkCatalog::new(handle);
    assert!(catalog.is_empty());
    for index in 0..MAX_SUPERVISED_NETWORKS as u64 {
        let (command_tx, _rx) = mpsc::channel(4);
        catalog
            .insert(SupervisorHandle::new(NetworkId(index + 1), command_tx))
            .expect("within the ceiling");
    }
    assert_eq!(catalog.len(), MAX_SUPERVISED_NETWORKS);
    // The ceiling is refused explicitly rather than growing the catalog silently.
    let (command_tx, _rx) = mpsc::channel(4);
    assert!(
        catalog
            .insert(SupervisorHandle::new(NetworkId(9999), command_tx))
            .is_err()
    );
    assert_eq!(catalog.networks().len(), MAX_SUPERVISED_NETWORKS);
}

#[tokio::test]
async fn restart_rebuilds_networks_from_durable_state_without_sessions() {
    let (_store, handle) = store();
    handle
        .save_network(&record(1, "bot", &["#alpha", "#beta"]))
        .await
        .unwrap();
    handle
        .save_network(&record(2, "other", &["#gamma"]))
        .await
        .unwrap();

    // A fresh catalog over the same store rebuilds exactly the durable intent.
    let catalog = NetworkCatalog::new(handle.clone());
    let desired = catalog.load_desired_state().await.expect("catalog loads");
    assert_eq!(desired.len(), 2);
    let first = desired
        .iter()
        .find(|record| record.network == NetworkId(1))
        .expect("network one");
    assert_eq!(
        first.desired_channels,
        attached_channels(&["#alpha", "#beta"])
    );
    assert!(
        catalog.is_empty(),
        "restart restores no live supervisor and therefore no sessions"
    );
}

// -------------------------------------------------------------- multi-client

#[tokio::test]
async fn several_sessions_attach_concurrently_and_share_one_generation() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let mut clients = Vec::new();
    for index in 0..3u64 {
        let (_session, stream) = harness.attach(ClientId(index + 1)).await;
        clients.push(stream);
    }
    harness.wait_attached(3).await;

    // The generation is unchanged by attachment: a client joining never ends or
    // replaces the upstream session.
    assert_eq!(
        harness.snapshot.borrow().generation,
        Some(i2pr_irc_core::ConnectionGeneration(1))
    );

    // Every client registers and receives its own projection.
    for client in &mut clients {
        register(client, "bot").await;
    }
    assert_eq!(harness.snapshot.borrow().attached_sessions, 3);
}

#[tokio::test]
async fn one_upstream_event_fans_out_to_every_attached_client() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let mut clients = Vec::new();
    for index in 0..3u64 {
        let (_session, stream) = harness.attach(ClientId(index + 1)).await;
        clients.push(stream);
    }
    harness.wait_attached(3).await;
    for client in &mut clients {
        register(client, "bot").await;
    }

    // One upstream line is normalized once and delivered to all attached sessions.
    let upstream = harness.upstream();
    upstream
        .write_all(b":alice!u@h PRIVMSG #room :hello everyone\r\n")
        .await
        .unwrap();
    for client in &mut clients {
        let received = read_client_until(client, b"PRIVMSG").await;
        assert!(received.contains("hello everyone"), "{received}");
    }
}

#[tokio::test]
async fn one_client_detaching_leaves_the_others_and_the_generation() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_first, mut first_stream) = harness.attach(ClientId(1)).await;
    let (_second, mut second_stream) = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut first_stream, "bot").await;
    register(&mut second_stream, "bot").await;

    // The first client closes its stream. Only that session ends.
    drop(first_stream);
    harness.wait_attached(1).await;
    assert_eq!(
        harness.snapshot.borrow().generation,
        Some(i2pr_irc_core::ConnectionGeneration(1)),
        "a client detach never ends the upstream generation"
    );

    // The remaining client still receives upstream traffic.
    let upstream = harness.upstream();
    upstream
        .write_all(b":alice!u@h PRIVMSG #room :still here\r\n")
        .await
        .unwrap();
    let received = read_client_until(&mut second_stream, b"PRIVMSG").await;
    assert!(received.contains("still here"), "{received}");
}

#[tokio::test]
async fn session_ceiling_is_refused_explicitly() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    // The peers are retained: dropping one would end that session and the count
    // would never reach the ceiling.
    let mut peers = Vec::new();
    for index in 0..MAX_SESSIONS_PER_NETWORK as u64 {
        let (_session, peer) = harness.attach(ClientId(index + 1)).await;
        peers.push(peer);
    }
    harness.wait_attached(MAX_SESSIONS_PER_NETWORK).await;

    // One more attachment through the public handle is refused as overload, not
    // queued without limit and not admitted past the ceiling.
    let (client_side, _peer) = tokio::io::duplex(1024);
    let result = harness
        .handle
        .attach(SessionId(9999), ClientId(9999), Box::new(client_side))
        .await;
    assert!(matches!(result, Err(RuntimeError::QueueOverloaded)));
    assert_eq!(
        harness.snapshot.borrow().attached_sessions,
        MAX_SESSIONS_PER_NETWORK
    );
    drop(peers);
}

// ------------------------------------------------- session/client identity

#[tokio::test]
async fn a_reused_client_id_gets_a_fresh_session_identity() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    // The same durable ClientId attaches twice. Each attachment is a distinct live
    // session, so a late result scoped to the first cannot reach the second.
    let (first, mut first_stream) = harness.attach(ClientId(7)).await;
    register(&mut first_stream, "bot").await;
    drop(first_stream);
    harness.wait_attached(0).await;

    let (second, mut second_stream) = harness.attach(ClientId(7)).await;
    harness.wait_attached(1).await;
    register(&mut second_stream, "bot").await;

    assert_eq!(first, SessionId(7));
    assert_eq!(second, SessionId(7));
    // Same durable lineage, two separate attachments in sequence: the session
    // identity is allocated per attachment and the client lineage is unchanged.
    assert_eq!(
        harness.snapshot.borrow().sessions_accepted,
        2,
        "two attachments of one client lineage"
    );
}

#[tokio::test]
async fn a_stale_session_event_cannot_reach_a_replacement_session() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    // Register a session, then deliver an event naming a session that never existed.
    // The owner must ignore it rather than treat it as current work.
    let (_session, mut stream) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut stream, "bot").await;

    harness
        .commands
        .try_send(SupervisorCommand::Session {
            session: SessionId(4242),
            event: i2pr_irc_runtime::session::SessionEvent::Intent {
                session: SessionId(4242),
                intent: i2pr_irc_runtime::session::SessionIntent::Quit,
            },
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        harness.snapshot.borrow().attached_sessions,
        1,
        "an event for an unknown session must not detach a live one"
    );
}

// ------------------------------------------------ persistence-first intent

#[tokio::test]
async fn a_join_commits_durably_before_upstream_bytes_exist() {
    let (_store, handle) = store();
    handle.save_network(&record(1, "bot", &[])).await.unwrap();
    let mut harness = Harness::start(1, "bot", &[], handle.clone(), true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut stream) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut stream, "bot").await;

    let upstream = harness.upstream();
    stream.write_all(b"JOIN #newroom\r\n").await.unwrap();

    // The upstream JOIN is only written after the durable commit, so observing it
    // upstream is proof that storage already holds the intent.
    read_until(upstream, b"JOIN #newroom\r\n").await;
    let stored = handle.load_networks().await.expect("catalog loads");
    assert!(
        stored[0]
            .desired_channels
            .iter()
            .any(|entry| entry.target == "#newroom"),
        "durable intent must exist before the upstream JOIN is observable"
    );
}

#[tokio::test]
async fn a_failed_join_commit_writes_no_upstream_join() {
    // A store whose worker has stopped refuses every mutation. That is the honest
    // stand-in for persistence being unavailable while the Network stays online.
    let (_store, handle) = store();
    handle.save_network(&record(1, "bot", &[])).await.unwrap();
    let owner_handle = handle.clone();
    let mut harness = Harness::start(1, "bot", &[], owner_handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut stream) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut stream, "bot").await;
    // Stop the store worker. The Network owner keeps running: a storage failure
    // fails one client operation, not the upstream session.
    _store.shutdown().expect("store shuts down");

    stream.write_all(b"JOIN #never\r\n").await.unwrap();
    // The client is told the request could not be persisted.
    let notice = read_client_until(&mut stream, b"could not persist").await;
    assert!(notice.contains("could not persist"), "{notice}");

    // A second attempt fails the same way, and the upstream stream must contain no
    // JOIN at all: an unpersisted intent must never reach upstream.
    stream.write_all(b"JOIN #after\r\n").await.unwrap();
    let second = read_client_until(&mut stream, b"could not persist").await;
    assert!(second.contains("could not persist"), "{second}");

    // The upstream stream is still live and has received no JOIN. Draining what the
    // owner sent proves it directly.
    let upstream = harness.upstream();
    let mut seen = Vec::new();
    let mut buf = [0u8; 512];
    let deadline = tokio::time::sleep(Duration::from_millis(200));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            read = upstream.read(&mut buf) => {
                match read {
                    Ok(0) => break,
                    Ok(count) => seen.extend_from_slice(&buf[..count]),
                    Err(_) => break,
                }
            }
        }
    }
    assert!(
        !String::from_utf8_lossy(&seen).contains("JOIN #"),
        "an unpersisted JOIN must never be written upstream: {}",
        String::from_utf8_lossy(&seen)
    );
    assert_eq!(
        harness.snapshot.borrow().phase,
        Some(Phase::Online),
        "a storage failure must not end the upstream Network"
    );
}

#[tokio::test]
async fn desired_state_persists_through_a_restart() {
    let (_store, handle) = store();
    handle
        .save_network(&record(1, "bot", &["#alpha"]))
        .await
        .unwrap();
    let mut harness = Harness::start(1, "bot", &["#alpha"], handle.clone(), true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut stream) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut stream, "bot").await;
    let upstream = harness.upstream();
    stream.write_all(b"JOIN #beta\r\n").await.unwrap();
    read_until(upstream, b"JOIN #beta\r\n").await;

    // A fresh catalog over the same store sees both the original and the new intent.
    let catalog = NetworkCatalog::new(handle);
    let restored = catalog.load_desired_state().await.expect("catalog loads");
    assert_eq!(
        restored[0].desired_channels,
        attached_channels(&["#alpha", "#beta"])
    );
}

#[tokio::test]
async fn a_stored_secret_never_reaches_a_diagnostic() {
    let (_store, handle) = store();
    let mut with_secret = record(1, "bot", &[]);
    with_secret.sasl = Some(("bot".into(), StoredSecret::new("s3cr3t-value".into())));
    handle.save_network(&with_secret).await.unwrap();

    let catalog = NetworkCatalog::new(handle);
    let restored = catalog.load_desired_state().await.expect("catalog loads");
    assert!(
        !format!("{restored:?}").contains("s3cr3t-value"),
        "restored configuration must not render a credential"
    );
}

// ------------------------------------------------------------- bounded state

#[tokio::test]
async fn repeated_attach_detach_cycles_return_to_a_bounded_steady_state() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    for round in 0..25u64 {
        let (_session, mut stream) = harness.attach(ClientId(round + 1)).await;
        harness.wait_attached(1).await;
        register(&mut stream, "bot").await;
        drop(stream);
        harness.wait_attached(0).await;
    }
    let snapshot = harness.snapshot.borrow();
    assert_eq!(snapshot.attached_sessions, 0);
    assert_eq!(snapshot.sessions_accepted, 25);
    assert_eq!(snapshot.sessions_ended, 25);
    assert_eq!(
        snapshot.generation,
        Some(i2pr_irc_core::ConnectionGeneration(1)),
        "client churn must never replace the upstream generation"
    );
}

#[tokio::test]
async fn a_session_cannot_forge_an_upstream_generation() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut stream) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut stream, "bot").await;

    let upstream = harness.upstream();
    stream
        .write_all(b"PRIVMSG #room :forwarded\r\n")
        .await
        .unwrap();
    // The owner stamps the live generation, so the frame reaches the current
    // upstream session rather than being dropped or misattributed.
    let frames = read_until(upstream, b"PRIVMSG #room :forwarded\r\n").await;
    assert!(String::from_utf8_lossy(&frames).contains("PRIVMSG #room :forwarded"));
}

// ======================================================== LIVE RESPONSE ROUTING
//
// Corrective 014 qualification. These tests drive a real attached client through a
// real upstream stream, so they prove the owner routes replies by SessionId rather
// than proving only that the router does so in isolation.

/// Reads until the needle arrives, returning everything the client saw so far.
///
/// Used where the assertion is about what a client did *not* receive: the caller
/// writes the client command, drives the upstream reply, and then checks the peer's
/// buffer without blocking on a frame that must never come.
async fn drain_client(stream: &mut tokio::io::DuplexStream) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
    // Drain whatever is already buffered without blocking indefinitely.
    let read = async {
        loop {
            let count = stream.read(&mut buf).await.unwrap();
            if count == 0 {
                break;
            }
            all.extend_from_slice(&buf[..count]);
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(400), read).await;
    String::from_utf8_lossy(&all).into_owned()
}

#[tokio::test]
async fn a_query_reply_reaches_only_the_client_that_asked() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_first_session, mut first) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut first, "bot").await;
    let (_second_session, mut second) = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut second, "bot").await;

    let upstream = harness.upstream();
    first.write_all(b"WHOIS alice\r\n").await.unwrap();
    read_until(upstream, b"WHOIS alice\r\n").await;

    // The multi-line WHOIS answer. Without routing these would fan out to both clients,
    // disclosing one client's lookup to the other.
    upstream
        .write_all(
            b":srv 311 bot alice ~a host * :Alice\r\n\
              :srv 318 bot alice :End of /WHOIS list.\r\n",
        )
        .await
        .unwrap();

    let seen_by_asking = read_client_until(&mut first, b"311 bot alice").await;
    assert!(
        seen_by_asking.contains("318 bot alice"),
        "the asking client must receive the whole answer: {seen_by_asking}"
    );
    let seen_by_other = drain_client(&mut second).await;
    assert!(
        !seen_by_other.contains("WHOIS") && !seen_by_other.contains("311 bot alice"),
        "a reply to one client must never reach another: {seen_by_other}"
    );
}

#[tokio::test]
async fn two_clients_query_concurrently_and_each_gets_only_its_own_answer() {
    let (_store, handle) = store();
    // Upstream must offer the label surface before the bouncer will label its own
    // queries; otherwise concurrent lookups cannot be disambiguated.
    let mut harness = Harness::start_labeled(1, "bot", &[], handle).await;
    harness.wait_phase(Phase::Online).await;

    let (_a, mut first) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut first, "bot").await;
    let (_b, mut second) = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut second, "bot").await;

    let upstream = harness.upstream();
    // Both clients use the *same* downstream label. Translated upstream they must not
    // collide, or the two answers would be indistinguishable.
    first
        .write_all(b"@label=same WHOIS alice\r\n")
        .await
        .unwrap();
    second
        .write_all(b"@label=same WHOIS bob\r\n")
        .await
        .unwrap();
    let frames = read_until(upstream, b"WHOIS bob\r\n").await;
    let text = String::from_utf8_lossy(&frames);
    assert!(
        text.contains("@label="),
        "a correlated query must carry a translated upstream label: {text}"
    );
    assert!(
        !text.contains("@label=same"),
        "a downstream label must never reach the server: {text}"
    );

    // The server answers each query with the label the bouncer sent. Answers arrive out
    // of order, to prove ordering is not what routes them.
    let alice = query_label(&text, "WHOIS alice");
    let bob = query_label(&text, "WHOIS bob");
    assert_ne!(alice, bob, "two concurrent queries must not share a label");
    upstream
        .write_all(
            format!(
                "@label={bob} :srv 318 bot bob :End of /WHOIS list.\r\n\
                 @label={alice} :srv 318 bot alice :End of /WHOIS list.\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let first_seen = read_client_until(&mut first, b"318 bot alice").await;
    assert!(
        first_seen.contains("@label=same"),
        "the client's own label must be restored on its reply: {first_seen}"
    );
    let second_seen = read_client_until(&mut second, b"318 bot bob").await;
    assert!(
        second_seen.contains("@label=same"),
        "the client's own label must be restored on its reply: {second_seen}"
    );
    let first_only = drain_client(&mut first).await;
    assert!(
        !first_only.contains("bot bob"),
        "one client must not observe the other's answer: {first_only}"
    );
    let second_only = drain_client(&mut second).await;
    assert!(
        !second_only.contains("bot alice"),
        "one client must not observe the other's answer: {second_only}"
    );
}

#[tokio::test]
async fn a_batched_multi_line_answer_stays_with_its_client_and_closes() {
    let (_store, handle) = store();
    let mut harness = Harness::start_labeled(1, "bot", &[], handle).await;
    harness.wait_phase(Phase::Online).await;

    let (_first_session, mut first) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut first, "bot").await;
    let (_second_session, mut second) = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut second, "bot").await;

    let upstream = harness.upstream();
    first.write_all(b"WHOIS alice\r\n").await.unwrap();
    let frames = read_until(upstream, b"WHOIS alice\r\n").await;
    let label = query_label(&String::from_utf8_lossy(&frames), "WHOIS alice");

    // A batched answer: the opener carries the response label, the body carries only
    // `batch=`, and the closing frame carries neither.
    upstream
        .write_all(
            format!(
                "@label={label} BATCH +ref labeled-response\r\n\
                 @batch=ref :srv 311 bot alice ~a host * :Alice\r\n\
                 @batch=ref :srv 318 bot alice :End of /WHOIS list.\r\n\
                 :srv BATCH -ref\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let seen = read_client_until(&mut first, b"BATCH -ref").await;
    assert!(seen.contains("311 bot alice"), "batched body: {seen}");
    let other = drain_client(&mut second).await;
    assert!(
        !other.contains("311 bot alice") && !other.contains("BATCH"),
        "a batched reply must not leak to another client: {other}"
    );
    // The batch consumed its route, so the table is back to empty.
    assert_eq!(harness.snapshot.borrow().response_routes, 0);
}

#[tokio::test]
async fn a_reply_arriving_after_a_detach_reaches_nobody() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut client) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client, "bot").await;

    let upstream = harness.upstream();
    client.write_all(b"WHOIS alice\r\n").await.unwrap();
    read_until(upstream, b"WHOIS alice\r\n").await;
    assert_eq!(harness.snapshot.borrow().response_routes, 1);

    // The client vanishes before its answer arrives.
    drop(client);
    harness.wait_attached(0).await;
    assert_eq!(
        harness.snapshot.borrow().response_routes,
        0,
        "detaching a client must release its routes"
    );

    // The late answer now belongs to nobody. It must be dropped, not fanned out into a
    // future attachment.
    harness
        .upstream()
        .write_all(b":srv 318 bot alice :End of /WHOIS list.\r\n")
        .await
        .unwrap();
    let (_late, mut later) = harness.attach(ClientId(2)).await;
    harness.wait_attached(1).await;
    register(&mut later, "bot").await;
    let seen = drain_client(&mut later).await;
    assert!(
        !seen.contains("311 bot alice"),
        "an orphaned reply must not be delivered to a new attachment: {seen}"
    );
}

#[tokio::test]
async fn a_generation_replacement_delivers_no_reply_from_the_old_one() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_session, mut client) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client, "bot").await;

    let upstream = harness.upstream();
    client.write_all(b"WHOIS alice\r\n").await.unwrap();
    read_until(upstream, b"WHOIS alice\r\n").await;
    assert_eq!(harness.snapshot.borrow().response_routes, 1);

    // Ending the stream ends the generation and builds a fresh router.
    drop(harness.upstream.take());
    harness.wait_phase(Phase::Backoff).await;
    let mut next = harness.provider.take_peer().await;
    harness.drive_online(&mut next).await;
    harness.upstream = Some(next);
    harness.wait_phase(Phase::Online).await;
    assert_eq!(
        harness.snapshot.borrow().response_routes,
        0,
        "a new generation must inherit no route from the old one"
    );
}

#[tokio::test]
async fn an_unsolicited_reply_still_fans_out_to_every_client() {
    let (_store, handle) = store();
    let mut harness = Harness::start(1, "bot", &[], handle, true).await;
    harness.wait_phase(Phase::Online).await;

    let (_a, mut first) = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut first, "bot").await;
    let (_b, mut second) = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut second, "bot").await;

    // Routing must not silence ordinary broadcast traffic.
    harness
        .upstream()
        .write_all(b":srv 372 bot :- MOTD -\r\n")
        .await
        .unwrap();

    let first_seen = read_client_until(&mut first, b"372 ").await;
    assert!(first_seen.contains("372"), "fanout: {first_seen}");
    let second_seen = read_client_until(&mut second, b"372 ").await;
    assert!(second_seen.contains("372"), "fanout: {second_seen}");
}
