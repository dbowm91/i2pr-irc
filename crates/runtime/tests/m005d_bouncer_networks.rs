//! Plan 023 — M005-D interoperability and boundary qualification.
//!
//! The unit suites cover the parsers. This suite covers the thing the plan is actually
//! about: that an ordinary IRC client, one that has never heard of this bouncer, can
//! connect, discover what Networks exist, pick one, and administrate it — and that none of
//! that can be turned into clearnet egress, arbitrary IRC injection, or a credential leak.

use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, I2pEndpointKind, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{
    ControlSnapshot, MAX_CONTROL_SNAPSHOT_NETWORKS, RuntimeControlHandle, RuntimeController,
};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CEILING: Duration = Duration::from_secs(10);

/// A canonical b32 destination: 52 base32 characters before the suffix.
fn b32() -> String {
    format!("{}.b32.i2p", "a".repeat(52))
}

/// A hostname-form `.i2p` destination.
fn hostname() -> String {
    format!("{}.i2p", "a".repeat(63))
}

/// A canonical 516-character raw `Destination` value.
fn destination() -> String {
    "z".repeat(516)
}

fn record(network: u64, nick: &str) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse(&b32()).expect("a test destination"),
        nick: nick.to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        sasl: None,
        desired_channels: Vec::new(),
        auto_away: false,
        keep_nick: false,
    }
}

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

/// A provider that hands out scripted streams to both the runtime and the test.
///
/// The test and the controller share one provider on purpose: the controller asks for a
/// connection and the test must then be able to claim that connection's other end.
#[derive(Clone)]
struct Provider(Arc<FakeI2pStreamProvider>);

impl Provider {
    fn new() -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..16 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("provider queue has room");
        }
        Self(provider)
    }

    /// The upstream end of the next connection this provider hands out.
    async fn plain_peer(&self) -> ScriptedStream {
        let peer = self.0.take_peer().await;
        self.0.take_controller().await;
        peer
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

// ----------------------------------------------------------------- harness

struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    store: (Store, StoreHandle),
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    /// Upstream peers parked for the lifetime of the test.
    ///
    /// Retained deliberately: dropping a scripted peer closes the socket, which ends the
    /// owner's generation exactly as a real upstream disconnect would.
    upstreams: Vec<ScriptedStream>,
}

impl Runtime {
    async fn start() -> Self {
        let (store, handle) = store();
        Self::start_with(store, handle).await
    }

    /// Builds a runtime over an already-open store, so a restart test can reuse it.
    async fn start_with(store: Store, handle: StoreHandle) -> Self {
        let provider = Provider::new();
        let (mut controller, control) = RuntimeController::with_durable(
            provider.clone(),
            handle.clone(),
            Arc::new(handle.clone()),
        );
        let task = tokio::spawn(async move { controller.serve().await });
        wait_for(&control, |_| true).await;
        Self {
            control,
            provider,
            store: (store, handle),
            task,
            upstreams: Vec::new(),
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

    async fn create(&self, network: u64) {
        self.control
            .create(record(network, "bot"))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
    }

    /// Every durable Network the store currently holds.
    async fn durable(&self) -> Vec<NetworkRecord> {
        self.store.1.load_networks().await.expect("durable read")
    }

    /// Creates a Network and parks its upstream peer so the owner reaches "live".
    async fn bring_online(&mut self, network: u64, desired_channels: Vec<(&str, bool)>) {
        let mut candidate = record(network, "bot");
        candidate.desired_channels = desired_channels
            .into_iter()
            .enumerate()
            .map(
                |(index, (target, detached))| i2pr_irc_store::DesiredChannelRecord {
                    target: target.to_owned(),
                    position: index,
                    detached,
                },
            )
            .collect();
        self.control
            .create(candidate)
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
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
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
            .expect("the controller publishes a snapshot");
    }
}

/// One admitted downstream client, with everything it has received retained.
///
/// The buffer is load-bearing. A helper that stopped at the frame it was asked for would
/// discard whatever arrived in the same read, and a scripted stream can deliver several
/// frames at once — so the remainder is exactly the frame the next assertion is about.
struct Client {
    end: ScriptedStream,
    _script: i2pr_irc_testkit::FaultController,
    outcome: tokio::task::JoinHandle<AdmissionOutcome>,
    seen: String,
}

impl Client {
    /// Reads until `needle` appears in the whole buffer.
    async fn until(&mut self, needle: &str) {
        self.await_new(0, needle).await
    }

    /// Reads until any of `needles` appears after `mark`.
    ///
    /// Used where two answers are both correct and the test does not know which will
    /// come: waiting for one specific answer would make the test pass or fail depending
    /// on where the ceiling happened to land.
    async fn await_either(&mut self, mark: usize, needles: &[&str]) {
        let deadline = tokio::time::Instant::now() + CEILING;
        while !needles
            .iter()
            .any(|needle| self.seen[mark..].contains(*needle))
        {
            let mut chunk = [0u8; 1024];
            let count = tokio::time::timeout_at(deadline, self.end.read(&mut chunk))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {needles:?}; saw {:?}",
                        &self.seen[mark..]
                    )
                })
                .expect("the client stream does not fail");
            if count == 0 {
                panic!(
                    "client closed waiting for {needles:?}; saw {:?}",
                    &self.seen[mark..]
                );
            }
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
    }

    /// Reads until `needle` appears *after* `mark`.
    ///
    /// Searching the whole buffer would make the second assertion in a loop pass on the
    /// first one's answer, which is how a test can report that seven hostile hosts were
    /// each refused while only ever reading one refusal.
    async fn await_new(&mut self, mark: usize, needle: &str) {
        let deadline = tokio::time::Instant::now() + CEILING;
        while !self.seen[mark..].contains(needle) {
            let mut chunk = [0u8; 1024];
            let count = tokio::time::timeout_at(deadline, self.end.read(&mut chunk))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {needle:?}; saw {:?}",
                        &self.seen[mark..]
                    )
                })
                .expect("the client stream does not fail");
            if count == 0 {
                panic!(
                    "client closed waiting for {needle:?}; saw {:?}",
                    &self.seen[mark..]
                );
            }
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
    }

    /// Closes the connection and reports how admission ended.
    ///
    /// An unbound control session is *designed* to keep running until the client goes
    /// away, so awaiting its outcome without closing the socket would wait forever — and
    /// the ceiling here is the test failing loudly rather than hanging the suite.
    async fn finish(self) -> AdmissionOutcome {
        drop(self.end);
        tokio::time::timeout(CEILING, self.outcome)
            .await
            .expect("admission ends when the client disconnects")
            .expect("admission task joins")
    }

    async fn send(&mut self, frame: &str) {
        self.end
            .write_all(frame.as_bytes())
            .await
            .expect("the client writes");
    }

    /// Marks the current end of the buffer, for an assertion about what came *after*.
    fn mark(&self) -> usize {
        self.seen.len()
    }

    fn since(&self, mark: usize) -> String {
        self.seen[mark..].to_owned()
    }

    /// Reads for a bounded window without waiting for a frame that may never come.
    ///
    /// Used for negative assertions, where "nothing arrived" is the expected answer and
    /// waiting for a needle would be the test's own bug.
    async fn settle(&mut self) {
        let mut chunk = [0u8; 1024];
        loop {
            match tokio::time::timeout(Duration::from_millis(250), self.end.read(&mut chunk)).await
            {
                Ok(Ok(count)) if count > 0 => {
                    self.seen
                        .push_str(&String::from_utf8_lossy(&chunk[..count]));
                }
                _ => break,
            }
        }
    }
}

fn admit(runtime: &Runtime, selected: Option<NetworkSelection>, session: SessionId) -> Client {
    let (end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let control = runtime.control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::new(selected, control, session, ClientId(7))
            .run(stream)
            .await
    });
    Client {
        end,
        _script: script,
        outcome,
        seen: String::new(),
    }
}

/// Admits one connection that selected no Network: the control-only session.
fn admit_unbound(runtime: &Runtime, session: SessionId) -> Client {
    admit(runtime, None, session)
}

/// Admits one connection that named a Network before it connected.
fn admit_bound(runtime: &Runtime, network: NetworkId, session: SessionId) -> Client {
    admit(
        runtime,
        Some(NetworkSelection {
            network,
            expected_nick: "bot".to_owned(),
        }),
        session,
    )
}

/// Registers a control session with the draft negotiated, and reads its welcome.
async fn register_control(client: &mut Client) {
    client
        .send("CAP LS 302\r\nCAP REQ :soju.im/bouncer-networks soju.im/bouncer-networks-notify\r\nNICK phone\r\nUSER phone 0 * :phone\r\nCAP END\r\n")
        .await;
    client.until("376 ").await;
}

/// Registers a control session with no capability negotiated at all.
async fn register_plain(client: &mut Client) {
    client
        .send("NICK phone\r\nUSER user 0 * :phone\r\nCAP END\r\n")
        .await;
    client.until("376 ").await;
}

/// Registers a client bound to a Network with the draft negotiated.
async fn register_bound(runtime: &Runtime, network: NetworkId, session: SessionId) -> Client {
    let mut client = admit_bound(runtime, network, session);
    client
        .send(
            "CAP REQ :soju.im/bouncer-networks\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n",
        )
        .await;
    client.until("001 bot").await;
    client
}

/// Reads until `needle`, taking everything already buffered as well.
async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "upstream stream ended");
            all.extend_from_slice(&buf[..count]);
        }
        while let Ok(Ok(count)) = tokio::time::timeout(Duration::ZERO, stream.read(&mut buf)).await
        {
            if count == 0 {
                break;
            }
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(CEILING, read)
        .await
        .unwrap_or_else(|_| panic!("upstream never sent {}", String::from_utf8_lossy(needle)));
    String::from_utf8_lossy(&all).into_owned()
}

// ------------------------------------------------- discovery and listing

#[tokio::test]
async fn an_unbound_session_can_discover_every_network() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;
    runtime.create(2).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client.send("BOUNCER LISTNETWORKS\r\n").await;
    client.until("netid=2").await;
    client.settle().await;

    assert!(
        client.seen.contains("BOUNCER NET netid=1 name=net-1"),
        "a control session lists every Network: {}",
        client.seen
    );
    assert!(
        client.seen.contains("BOUNCER NET netid=2 name=net-2"),
        "a control session lists every Network: {}",
        client.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_network_listing_never_carries_an_endpoint_or_an_identity() {
    let runtime = Runtime::start().await;
    let mut candidate = record(1, "secretnick");
    candidate.sasl = Some((
        "sasluser".to_owned(),
        i2pr_irc_store::StoredSecret::new("hunter2".to_owned()),
    ));
    runtime.control.create(candidate).await.expect("create");

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client.send("BOUNCER LISTNETWORKS\r\n").await;
    client.until("state=").await;
    client.settle().await;

    for forbidden in ["secretnick", "sasluser", "hunter2", ".i2p", &b32()] {
        assert!(
            !client.seen.contains(forbidden),
            "a Network listing must not carry {forbidden:?}: {}",
            client.seen
        );
    }
    runtime.stop().await;
}

// --------------------------------------------------------------- selection

#[tokio::test]
async fn a_pre_registration_bind_claims_the_connection() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, Vec::new()).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    client
        .send("CAP REQ :soju.im/bouncer-networks\r\nBOUNCER BIND 1\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n")
        .await;
    client.until("001 bot").await;

    assert_eq!(
        client.finish().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(101),
        },
        "a Network claimed during registration becomes this connection's Network"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_registered_session_cannot_bind_late() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    let mark = client.mark();
    client.send("BOUNCER BIND 1\r\n").await;
    client.until("FAIL BOUNCER BIND").await;

    assert!(
        client
            .since(mark)
            .contains("registered session cannot bind"),
        "a late BIND is refused by name: {}",
        client.since(mark)
    );
    assert_eq!(
        client.finish().await,
        AdmissionOutcome::Unbound {
            session: SessionId(101),
        },
        "a refused BIND leaves the session unbound rather than re-homing it silently"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_bind_to_a_network_that_does_not_exist_is_refused_during_registration() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    client
        .send("CAP REQ :soju.im/bouncer-networks\r\nBOUNCER BIND 9\r\nNICK phone\r\nUSER user 0 * :client\r\nCAP END\r\n")
        .await;
    client.until("FAIL BOUNCER BIND").await;

    assert!(
        client.seen.contains("no network with id 9"),
        "the refusal names the netid that was asked for: {}",
        client.seen
    );
    assert_eq!(
        client.finish().await,
        AdmissionOutcome::Unbound {
            session: SessionId(101),
        },
        "a refused BIND leaves the session usable as a control connection"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_non_canonical_netid_is_refused() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;

    for netid in ["01", "+1", "one", "9x9"] {
        // Sent during registration: that is the only window a BIND exists in, so testing
        // the grammar anywhere else would be testing the wrong refusal.
        let mut client = admit_unbound(&runtime, SessionId(101));
        let mark = client.mark();
        client
            .send(&format!(
                "CAP REQ :soju.im/bouncer-networks\r\nBOUNCER BIND {netid}\r\nNICK phone\r\nUSER user 0 * :client\r\nCAP END\r\n"
            ))
            .await;
        client.await_new(mark, "FAIL BOUNCER BIND").await;
        assert!(
            client.since(mark).contains("malformed network id"),
            "{netid:?} is not a canonical netid: {}",
            client.since(mark)
        );
    }
    runtime.stop().await;
}

#[tokio::test]
async fn a_netid_survives_a_restart() {
    // A netid comes from the durable record, not from a counter. That is the property
    // this test pins: restart the controller over a database that already holds a
    // Network, and the same identity must come back. If it did not, every client's saved
    // configuration would point at a Network that no longer exists.
    let (store, handle) = store();
    let mut durable = record(7, "bot");
    durable.display_name = "kept".to_owned();
    handle
        .save_network(&durable)
        .await
        .expect("the Network is durable before any controller runs");

    let runtime = Runtime::start_with(store, handle).await;
    let snapshot = runtime.control.status().await.expect("status");
    assert_eq!(
        snapshot.networks.len(),
        1,
        "startup restore found the Network"
    );
    assert_eq!(
        snapshot.networks[0].network,
        NetworkId(7),
        "a netid is the durable record's identity, not a freshly allocated one"
    );
    assert_eq!(snapshot.networks[0].display_name, "kept");

    // A newly created Network is still given a distinct identity rather than reusing one.
    runtime.create(8).await;
    let mut ids: Vec<u64> = runtime
        .control
        .status()
        .await
        .expect("status")
        .networks
        .iter()
        .map(|entry| entry.network.0)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![7, 8], "two identities, both distinct");
    runtime.stop().await;
}

// -------------------------------------------------------------- attributes

#[tokio::test]
async fn only_i2p_destinations_can_be_configured() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;

    for host in [
        "irc.example.org",
        "irc.example.org:6697",
        "https://irc.example.org",
        "user@irc.example.org",
        "127.0.0.1",
        "8.8.8.8",
        "",
    ] {
        let mark = client.mark();
        client
            .send(&format!("BOUNCER ADDNETWORK name=x host={host}\r\n"))
            .await;
        client.await_new(mark, "FAIL BOUNCER ADDNETWORK").await;
        assert!(
            client
                .since(mark)
                .contains("host must be an I2P destination"),
            "{host:?} must never become a configured endpoint: {}",
            client.since(mark)
        );
    }
    assert!(
        runtime.durable().await.is_empty(),
        "no refused host reached durable state"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn both_configurable_i2p_destination_forms_are_accepted() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;

    // A canonical raw `Destination` is 516 characters and cannot fit inside a 512-byte
    // IRC line, so it is not configurable through this surface at all. The two forms
    // that do fit are the ones the draft's clients will actually send.
    for (index, host) in [b32(), hostname()].into_iter().enumerate() {
        let mark = client.mark();
        client
            .send(&format!(
                "BOUNCER ADDNETWORK name=ok{index} host={host}\r\n"
            ))
            .await;
        client.await_new(mark, "Added network").await;
    }

    let mut durable = runtime.durable().await;
    durable.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    assert_eq!(durable.len(), 2, "every accepted form became durable");
    let kinds: Vec<I2pEndpointKind> = durable.iter().map(|entry| entry.endpoint.kind()).collect();
    assert_eq!(
        kinds,
        vec![I2pEndpointKind::StandardBase32, I2pEndpointKind::Hostname],
        "each form kept its own typed endpoint kind rather than being flattened"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn recognised_but_unsupported_attributes_are_refused_by_name() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;

    for attribute in ["port=6697", "tls=true", "pass=hunter2"] {
        let mark = client.mark();
        client
            .send(&format!(
                "BOUNCER ADDNETWORK name=x host={} {attribute}\r\n",
                b32()
            ))
            .await;
        client.await_new(mark, "FAIL BOUNCER ADDNETWORK").await;
        assert!(
            client.since(mark).contains("is not supported"),
            "{attribute} is recognised and refused rather than dropped: {}",
            client.since(mark)
        );
        assert!(
            !client.since(mark).contains("hunter2"),
            "a refused credential argument is not echoed: {}",
            client.since(mark)
        );
    }
    assert!(
        runtime.durable().await.is_empty(),
        "no refused attribute produced a durable Network"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_line_past_the_reader_ceiling_ends_the_session() {
    // Two distinct bounds with two distinct answers. An over-long *attribute* is a
    // request the bouncer understood and refused; an over-long *line* is never decoded,
    // so the only honest answer is to end the connection rather than parse a prefix of it.
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client
        .send(&format!("BOUNCER ADDNETWORK name={}\r\n", "y".repeat(9000)))
        .await;
    assert_eq!(
        client.finish().await,
        AdmissionOutcome::Unbound {
            session: SessionId(101)
        },
        "an undecodable line ends the session rather than being partially parsed"
    );
    assert!(
        runtime.durable().await.is_empty(),
        "nothing durable was written"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn read_only_attributes_cannot_be_written() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;

    for attribute in ["state=connected", "error=none"] {
        let mark = client.mark();
        client
            .send(&format!("BOUNCER CHANGENETWORK 1 {attribute}\r\n"))
            .await;
        client.await_new(mark, "FAIL BOUNCER CHANGENETWORK").await;
        assert!(
            client.since(mark).contains("is read-only"),
            "{attribute} is a report, not a request: {}",
            client.since(mark)
        );
    }
    runtime.stop().await;
}

#[tokio::test]
async fn unknown_attributes_are_refused_rather_than_ignored() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client
        .send(&format!(
            "BOUNCER ADDNETWORK name=x host={} colour=red\r\n",
            b32()
        ))
        .await;
    client.until("FAIL BOUNCER ADDNETWORK").await;
    assert!(
        client.seen.contains("unknown attribute colour"),
        "an attribute this build cannot honour is named: {}",
        client.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_display_name_cannot_smuggle_a_parameter() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    let mark = client.mark();
    client
        .send(&format!(
            "BOUNCER ADDNETWORK name=evil,:bot host={}\r\n",
            b32()
        ))
        .await;
    client.until("FAIL BOUNCER ADDNETWORK").await;
    assert!(
        client.since(mark).contains("out of range") || client.since(mark).contains("malformed"),
        "a name carrying a parameter separator is refused: {}",
        client.since(mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn oversized_input_is_refused_and_nothing_durable_is_written() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    // Long enough to exceed the per-attribute ceiling, short enough to stay a legal IRC
    // line. A line that exceeds the *reader* ceiling is a different case, tested below.
    let long = "x".repeat(400);
    let mark = client.mark();
    client
        .send(&format!(
            "BOUNCER ADDNETWORK name={long} host={}\r\n",
            b32()
        ))
        .await;
    client.until("FAIL BOUNCER ADDNETWORK").await;
    assert!(
        client.since(mark).contains("too long"),
        "an unbounded attribute is refused: {}",
        client.since(mark)
    );
    assert!(
        runtime.durable().await.is_empty(),
        "nothing reached durable state"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn the_catalog_ceiling_is_enforced_rather_than_grown() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;

    let mut refused = false;
    for index in 0..=i2pr_irc_runtime::catalog::MAX_SUPERVISED_NETWORKS {
        let mark = client.mark();
        client
            .send(&format!(
                "BOUNCER ADDNETWORK name=n{index} host={}\r\n",
                b32()
            ))
            .await;
        client
            .await_either(mark, &["Added network", "FAIL BOUNCER ADDNETWORK"])
            .await;
        if client.since(mark).contains("FAIL BOUNCER ADDNETWORK") {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "the catalog refuses rather than growing past its ceiling"
    );
    assert!(
        runtime.durable().await.len() <= i2pr_irc_runtime::catalog::MAX_SUPERVISED_NETWORKS,
        "durable state never exceeded the ceiling"
    );
    runtime.stop().await;
}

// ------------------------------------------------------------ notifications

#[tokio::test]
async fn notify_delivers_the_initial_batch_and_then_deltas() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client.until("netid=1").await;

    let mark = client.mark();
    client
        .send(&format!("BOUNCER ADDNETWORK name=two host={}\r\n", b32()))
        .await;
    client.until("BOUNCER NET +netid=2").await;
    assert!(
        client.since(mark).contains("name=two"),
        "an added Network is announced as a `+` delta: {}",
        client.since(mark)
    );

    let mark = client.mark();
    client.send("BOUNCER DELNETWORK 2\r\n").await;
    client.until("BOUNCER NET -netid=2").await;
    assert!(
        client.since(mark).contains("Deleted network"),
        "the change is acknowledged as well as announced: {}",
        client.since(mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_client_that_never_reads_does_not_make_the_process_remember_it() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;

    // A control session that negotiates notifications and then never reads its socket.
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    let _ = &mut client;

    for index in 0..12 {
        runtime
            .control
            .create(record(100 + index, "bot"))
            .await
            .expect("create");
        runtime
            .control
            .delete(NetworkId(100 + index))
            .await
            .expect("delete");
    }

    let snapshot = runtime.control.status().await.expect("status");
    assert!(
        snapshot.networks.len() <= MAX_CONTROL_SNAPSHOT_NETWORKS,
        "the controller's view stays bounded however much changed"
    );
    // The session is still alive and still answerable: notifications are bounded, not a
    // reason to end a connection.
    client.send("BOUNCER LISTNETWORKS\r\n").await;
    client.until("netid=1").await;
    runtime.stop().await;
}

// ------------------------------------------------- administration surface

#[tokio::test]
async fn a_bound_session_can_administer_too() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, Vec::new()).await;

    // Administration is not a privilege of being unbound: a session bound to a Network is
    // still the local Operator's own connection.
    let bound = register_bound(&runtime, NetworkId(1), SessionId(202)).await;
    assert_eq!(
        bound.finish().await,
        AdmissionOutcome::Bound {
            network: NetworkId(1),
            session: SessionId(202),
        }
    );

    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, Vec::new()).await;
    let mut bound = register_bound(&runtime, NetworkId(1), SessionId(203)).await;
    bound
        .send(&format!(
            "BOUNCER ADDNETWORK name=from-bound host={}\r\n",
            b32()
        ))
        .await;
    bound.until("Added network").await;
    assert!(
        runtime.durable().await.len() == 2,
        "a bound session's administrative change is durable"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn the_local_service_administers_presence_and_channel_policy() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, vec![("#room", false)]).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_plain(&mut client).await;

    client
        .send("PRIVMSG BouncerServ :channel status 1 #room\r\n")
        .await;
    client.until("#room attached").await;
    client
        .send("PRIVMSG BouncerServ :channel detach 1 #room\r\n")
        .await;
    client.until("Detached channel").await;
    client
        .send("PRIVMSG BouncerServ :channel status 1 #room\r\n")
        .await;
    client.until("#room detached").await;
    client
        .send("PRIVMSG BouncerServ :channel attach 1 #room\r\n")
        .await;
    client.until("Attached channel").await;

    client
        .send("PRIVMSG BouncerServ :presence set 1 auto_away=on\r\n")
        .await;
    client.until("Updated presence policy").await;
    client
        .send("PRIVMSG BouncerServ :nick set 1 keep_nick=on\r\n")
        .await;
    client.until("Updated nick policy").await;

    let durable = runtime.durable().await;
    assert!(durable[0].auto_away, "the presence change is durable");
    assert!(durable[0].keep_nick, "the nick change is durable");
    assert!(
        !durable[0].desired_channels[0].detached,
        "reattaching through the service cleared the durable flag"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_service_argument_cannot_inject_a_second_irc_line() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_plain(&mut client).await;

    let mark = client.mark();
    for attempt in ["network update 1 :evil", "network name=x\r\nQUIT now"] {
        client
            .send(&format!("PRIVMSG BouncerServ :{attempt}\r\n"))
            .await;
    }
    client.settle().await;
    let after = client.since(mark);
    assert!(
        !after.contains("QUIT") && !after.contains("JOIN #evil"),
        "a service argument is never re-parsed as protocol: {after}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn the_service_has_no_command_that_can_reach_the_host() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_plain(&mut client).await;

    let mark = client.mark();
    for attempt in [
        "run id",
        "exec /bin/sh",
        "file read /etc/passwd",
        "http get https://example.org",
        "plugin load ./x.so",
        "router reseed",
        "admin adduser someone",
        "network create name=x host=irc.example.org",
    ] {
        client
            .send(&format!("PRIVMSG BouncerServ :{attempt}\r\n"))
            .await;
    }
    client.settle().await;
    let after = client.since(mark);
    assert!(
        !after.contains("Added network"),
        "a clearnet host is not a configurable endpoint: {after}"
    );
    assert!(
        !after.contains("/bin/sh") && !after.contains("/etc/passwd"),
        "no service command reaches the host: {after}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_credential_never_appears_in_a_service_reply() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_plain(&mut client).await;

    client
        .send("PRIVMSG BouncerServ :sasl set 1 user=bob pass=hunter2\r\n")
        .await;
    client.until("Updated credential").await;
    client.send("PRIVMSG BouncerServ :sasl status 1\r\n").await;
    client.until("sasl set bob").await;
    client
        .send("PRIVMSG BouncerServ :sasl set 1 user=bob\r\n")
        .await;
    client.until("FAIL BOUNCER SASL").await;
    client.settle().await;

    assert!(
        !client.seen.contains("hunter2"),
        "the credential comes back from no reply, no status, and no refusal: {}",
        client.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn deleting_a_network_leaves_no_owner_behind() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, Vec::new()).await;

    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client.send("BOUNCER DELNETWORK 1\r\n").await;
    client.until("Deleted network").await;

    wait_for(&runtime.control, |snapshot| snapshot.networks.is_empty()).await;
    assert!(
        runtime.durable().await.is_empty(),
        "the Network is gone durably, not merely hidden from the listing"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_default_network_carries_only_bouncer_owned_identity() {
    let runtime = Runtime::start().await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_control(&mut client).await;
    client
        .send(&format!(
            "BOUNCER ADDNETWORK name=defaults host={}\r\n",
            b32()
        ))
        .await;
    client.until("Added network").await;

    let durable = runtime.durable().await;
    assert_eq!(durable.len(), 1);
    // If a default identity were ever derived from the host, every join would publish it.
    assert_eq!(durable[0].nick, "bouncer");
    assert_eq!(durable[0].username, "bouncer");
    assert_eq!(durable[0].realname, "bouncer");
    runtime.stop().await;
}

// ----------------------------------------------------------- the boundary

#[tokio::test]
async fn the_bouncer_line_ceiling_is_the_wire_ceiling_and_not_a_separate_claim() {
    // A bound the decoder does not enforce would describe a line that cannot arrive.
    assert_eq!(
        i2pr_irc_runtime::bouncer_networks::MAX_BOUNCER_LINE_BYTES,
        i2pr_irc_wire::MAX_LINE_BYTES,
        "the adapter's bound is the wire's bound, not a larger number nobody enforces"
    );
    // The consequence is real and is stated rather than hidden: a canonical raw
    // `Destination` cannot be configured over IRC, because it does not fit.
    assert!(
        format!("BOUNCER ADDNETWORK host={}", destination()).len() > i2pr_irc_wire::MAX_LINE_BYTES,
        "a raw Destination is genuinely unreachable through this surface"
    );
    assert!(
        format!("BOUNCER ADDNETWORK host={}", b32()).len() < i2pr_irc_wire::MAX_LINE_BYTES,
        "the b32 form does fit, which is why it is supported"
    );
}

#[tokio::test]
async fn no_host_or_environment_value_can_reach_this_control_plane() {
    // Structural, not environmental: `unsafe` is denied at build, so the suite cannot set
    // a sentinel variable and read it back. Instead the construction sites are read as
    // source and asserted not to interpolate anything derived from the host.
    for source in [
        include_str!("../src/bouncer_networks.rs"),
        include_str!("../src/bouncerserv.rs"),
        include_str!("../src/control_session.rs"),
    ] {
        for forbidden in [
            "std::env",
            "hostname",
            "whoami",
            "current_exe",
            "process::id",
            "os_release",
            "CARGO_PKG_VERSION",
        ] {
            assert!(
                !source.contains(forbidden),
                "no host or environment value may be interpolated: {forbidden}"
            );
        }
    }
    // The service identity is a constant, so a rendered reply cannot carry one.
    assert_eq!(
        i2pr_irc_runtime::bouncer_networks::SERVICE_NICK,
        "BouncerServ",
        "the service identity is fixed, not derived"
    );
}

#[tokio::test]
async fn a_capability_is_required_before_the_control_plane_answers() {
    let runtime = Runtime::start().await;
    runtime.create(1).await;
    let mut client = admit_unbound(&runtime, SessionId(101));
    register_plain(&mut client).await;

    let mark = client.mark();
    client.send("BOUNCER LISTNETWORKS\r\n").await;
    client.settle().await;
    assert!(
        !client.since(mark).contains("BOUNCER NET"),
        "a client that never negotiated the draft gets no control-plane answers: {}",
        client.since(mark)
    );
    runtime.stop().await;
}
