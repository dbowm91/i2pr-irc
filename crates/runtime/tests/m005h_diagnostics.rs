//! Plan 027 — M005-H operator diagnostics, configuration, and registration actions.
//!
//! The diagnostics surface is the only part of M005-H an Operator sees directly, and its
//! whole purpose is to be *legible during an incident*. Three properties make it so, and
//! each has a failure mode that a green test would otherwise miss:
//!
//! - **Nothing secret has a field to render.** This is not a redaction pass applied while
//!   rendering; the report type has no endpoint, no `Destination`, no SASL value, and no
//!   filesystem path in any variant, so a rendering bug cannot leak one either. The test
//!   that proves this walks every Network holding a credential and an endpoint and asserts
//!   neither appears anywhere in the reply.
//! - **A truncated list says so.** A channel sample that silently stops at 64 is
//!   indistinguishable from a Network with six channels, and the reader would conclude the
//!   wrong thing from a truthful-looking report.
//! - **A classification is a closed set.** `away=manual` is parsed by whatever reads this.
//!   If the spelling could drift, the report would be decorative.
//!
//! The rest of the plan -- versioned configuration snapshots and durable registration
//! actions -- is covered in the later sections of this file.

use std::sync::Arc;
use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{ControlSnapshot, RuntimeControlHandle, RuntimeController};
use i2pr_irc_store::{NetworkRecord, Store, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CEILING: Duration = Duration::from_secs(10);

fn b32() -> String {
    format!("{}.b32.i2p", "a".repeat(52))
}

fn record(network: u64, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse(&b32()).expect("a test destination"),
        failover_group: None,
        nick: "bot".to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        sasl: None,
        desired_channels: channels
            .iter()
            .enumerate()
            .map(|(index, target)| i2pr_irc_store::DesiredChannelRecord {
                target: (*target).to_owned(),
                position: index,
                detached: false,
                activity: i2pr_irc_store::ChannelActivityPolicy::default(),
            })
            .collect(),
        auto_away: false,
        keep_nick: false,
    }
}
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

    async fn plain_peer(&self) -> (ScriptedStream, i2pr_irc_testkit::FaultController) {
        let peer = self.0.take_peer().await;
        let controller = self.0.take_controller().await;
        (peer, controller)
    }

    /// Takes the next upstream peer *and* the controller that can end it.
    ///
    /// A reconnect test has to be able to drop a generation; the controller is the only
    /// handle that can close one side of the fixture, so it is kept rather than discarded
    /// the way an ordinary peer does not need it.
    async fn closable_peer(&self) -> (ScriptedStream, i2pr_irc_testkit::FaultController) {
        let peer = self.0.take_peer().await;
        let controller = self.0.take_controller().await;
        (peer, controller)
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

struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    upstreams: Vec<ScriptedStream>,
    /// Kept only for the peers a test may need to end, to prove a reconnect.
    upstreams_closable: Vec<Option<i2pr_irc_testkit::FaultController>>,
    _store: Store,
}

impl Runtime {
    async fn start() -> Self {
        let store = Store::open(&StorePath::Memory).expect("store opens");
        let handle = store.handle_clone();
        let provider = Provider::new();
        let (mut controller, control) =
            RuntimeController::with_durable(provider.clone(), handle.clone(), Arc::new(handle));
        let task = tokio::spawn(async move { controller.serve().await });
        wait_for(&control, |_| true).await;
        Self {
            control,
            provider,
            task,
            upstreams: Vec::new(),
            upstreams_closable: Vec::new(),
            _store: store,
        }
    }

    /// Brings a Network online with `cap_ls` offered upstream.
    ///
    /// `isupport` and `own_join` are parameters because they decide what the bouncer may
    /// legitimately claim about itself, and a diagnostic must report the bouncer's own
    /// claims rather than the test's hopes.
    async fn bring_online(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
    ) -> usize {
        self.bring_online_with_names(network, channels, cap_ls, isupport, own_join, "")
            .await
    }

    /// As [`Runtime::bring_online`], with a credential already stored on the Network.
    ///
    /// Written into the durable record before the Network starts, rather than through
    /// `SASL SET` afterwards: a configuration change restarts the owner and detaches every
    /// session attached to it, including the session that issued the change. Both routes
    /// produce the same stored credential; only this one leaves a client alive afterwards to
    /// read a diagnostic that must not contain it.
    async fn bring_online_credentialed(&mut self, network: u64, channels: &[&str]) -> usize {
        let mut candidate = record(network, channels);
        candidate.sasl = Some((
            "bob".to_owned(),
            i2pr_irc_store::StoredSecret::new("hunter2".to_owned()),
        ));
        self.control
            .create(candidate)
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        self.drive_registration(
            network,
            channels,
            // `sasl=PLAIN`, not a bare `sasl`: the owner requires an advertised
            // *mechanism*, because a server offering SASL without naming one cannot be
            // authenticated against. A Network holding a credential against a server
            // that offers neither is refused registration outright rather than
            // silently connecting unauthenticated -- so a fixture that omitted this
            // would be testing the no-credential path under a credentialed Network's
            // name.
            "message-tags server-time batch sasl=PLAIN",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
            "",
        )
        .await
    }

    /// As [`Runtime::bring_online`], plus `extra_names` added to each channel's `NAMES`.
    ///
    /// Membership a test depends on has to exist before any client attaches: a `353`
    /// written afterwards fans out verbatim, which would make the test measure the fanout
    /// rather than the bouncer's own rendering of the same facts.
    async fn bring_online_with_names(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
        extra_names: &str,
    ) -> usize {
        self.control
            .create(record(network, channels))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        self.drive_registration(network, channels, cap_ls, isupport, own_join, extra_names)
            .await
    }

    /// As [`Runtime::bring_online_with_names`], for a Network that already exists.
    ///
    /// Used when actions had to be stored before the Network started, which needs the
    /// Network to exist first. Creating it twice is an explicit duplicate-identity refusal,
    /// not a silent overwrite.
    async fn bring_existing_online(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
    ) -> usize {
        self.drive_registration(network, channels, cap_ls, isupport, own_join, "")
            .await
    }

    /// Stores actions on a Network *before* it comes online.
    ///
    /// Written directly through the controller rather than with `ACTION SET`, because a
    /// Network has to already exist to be administered and these tests need its actions in
    /// place before the first generation registers -- which is the only moment a replay can
    /// be observed without provoking a reconnect.
    async fn create_with_actions(&self, network: u64, modes: &[&str]) {
        let actions: Vec<i2pr_irc_runtime::action::RegistrationAction> = modes
            .iter()
            .map(|modes| {
                i2pr_irc_runtime::action::RegistrationAction::mode(modes).expect("an action")
            })
            .collect();
        self.control
            .create(record(network, &["#room"]))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        self.control
            .set_actions(
                NetworkId(network),
                i2pr_irc_runtime::action::ActionSet::new(actions).expect("a bounded set"),
            )
            .await
            .unwrap_or_else(|error| panic!("set actions {network}: {error:?}"));
    }

    /// Takes the upstream peer for the next connection generation.
    ///
    /// Reached by having the owner retry: the reconnect scheduler is what decides to come
    /// back, so a test that simply handed itself a new peer would be testing a fiction.
    async fn next_peer(&mut self) -> usize {
        let (upstream, controller) = self.provider.closable_peer().await;
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        self.upstreams_closable.push(Some(controller));
        peer
    }

    /// Ends one generation's upstream, which is what makes the owner reconnect.
    ///
    /// Closing the *peer's* write half is what the owner observes as a disconnect: the
    /// owner's read then returns end-of-file, which ends the generation immediately.
    /// Dropping the local end would instead leave the owner waiting on a fixture whose
    /// other half is gone, which is a hang rather than a reconnect.
    async fn drop_generation(&mut self, peer: usize) {
        let controller = self
            .upstreams_closable
            .get_mut(peer)
            .and_then(|slot| slot.take())
            .unwrap_or_else(|| {
                panic!(
                    "generation {peer} has no controller, so ending it would be a no-op \
                     and the test would go on to measure whatever eventually tore the \
                     generation down instead"
                )
            });
        // `connect()` hands side 0 to the *owner* and the peer on side 1 to the test.
        // The upstream server is side 1, so `close_write(1)` is "the server hung up":
        // side 0's read sees end-of-file and the generation ends at once.
        //
        // This is deliberately *not* `close_write(0)`, which would close the owner's own
        // write half. That models "our writes broke", which a bouncer cannot detect
        // until it next writes -- and since an idle bouncer writes only its keepalive
        // probe, the generation would survive until `LIVENESS_DEADLINE`. It is a real
        // condition with a real detection limit, but it is not "the upstream ended",
        // and a test that calls it that would be measuring the keepalive timer.
        controller.close_write(1);
    }

    async fn upstream(&mut self, peer: usize) -> &mut ScriptedStream {
        &mut self.upstreams[peer]
    }

    /// Completes registration on a peer that already received `CAP LS`.
    ///
    /// A reconnect has to run the same negotiation the first generation did: acknowledging
    /// each `CAP REQ` with exactly what was asked for and only then sending `001`. Skipping
    /// it would leave the generation stuck before it is ever online, which is not a replay
    /// failure -- the replay simply never had a chance to happen.
    async fn finish_registration(&mut self, peer: usize) {
        loop {
            let line = read_line(self.upstream(peer).await).await;
            if line.contains("CAP END") {
                break;
            }
            if let Some(request) = line.strip_prefix("CAP REQ :") {
                let requested = request.trim_end_matches("\r\n");
                self.send_upstream(peer, &format!(":srv CAP * ACK :{requested}\r\n"))
                    .await;
            }
        }
        self.send_upstream(peer, ":srv 001 bot :welcome\r\n").await;
    }

    async fn drive_registration(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
        extra_names: &str,
    ) -> usize {
        let (mut upstream, controller) = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        let ack = cap_ls
            .split_whitespace()
            .map(|name| name.to_owned())
            .collect::<Vec<_>>()
            .join(" ");
        // Only the capability advertisement goes out here. `001` is deliberately withheld
        // until `CAP END`: a real server sends the welcome *after* authentication, and
        // sending it early ends this generation before the bouncer has finished
        // negotiating, which is the opposite of what a fixture meant to exercise a
        // credentialed registration needs.
        upstream
            .write_all(format!(":srv CAP * LS :{ack}\r\n").as_bytes())
            .await
            .expect("upstream advertises capabilities");
        // Answering with exactly what the bouncer asked for -- rather than the whole
        // advertisement -- keeps each test honest about which capabilities were actually
        // granted, since an ACK naming a capability that was never requested takes a
        // different branch of registration than the real one.
        loop {
            let line = read_line(&mut upstream).await;
            if line.contains("CAP END") {
                upstream
                    .write_all(b":srv 001 bot :welcome\r\n")
                    .await
                    .expect("upstream welcomes after negotiation");
                break;
            }
            // SASL is answered, not stubbed. A Network with a credential runs a different
            // registration path from one without, and a fixture that skipped it would be
            // testing the path with no credential while claiming to test the one with.
            if line.starts_with("AUTHENTICATE PLAIN") {
                upstream
                    .write_all(b":srv AUTHENTICATE +\r\n")
                    .await
                    .expect("upstream starts sasl");
                continue;
            }
            if let Some(payload) = line.strip_prefix("AUTHENTICATE ") {
                assert!(
                    !payload.trim().is_empty(),
                    "the credential must be offered, not offered empty"
                );
                upstream
                    .write_all(b":srv 903 bot :SASL authentication successful\r\n")
                    .await
                    .expect("upstream accepts the credential");
                continue;
            }
            if let Some(request) = line.strip_prefix("CAP REQ :") {
                let requested = request.trim_end_matches("\r\n");
                upstream
                    .write_all(format!(":srv CAP * ACK :{requested}\r\n").as_bytes())
                    .await
                    .expect("upstream acks");
            }
        }
        // The bouncer's own channel membership is the extended form, which is what gives
        // a reattaching client something to observe about itself. `NAMELEN` is published
        // because `setname` obliges the server to say how long a realname may be.
        for channel in channels {
            let mut lines = vec![
                format!(":srv 005 bot {isupport}"),
                own_join.replace("{channel}", channel),
            ];
            // An extra `353` only when there are names to put in it: a memberless `353` is
            // not a line any server sends, and it would claim a NAMES list was seen.
            if !extra_names.is_empty() {
                lines.push(format!(":srv 353 bot = {channel} :{extra_names}"));
            }
            lines.push(format!(":srv 366 bot {channel} :End of /NAMES list."));
            let frame = lines
                .into_iter()
                .map(|line| format!("{line}\r\n"))
                .collect::<String>();
            upstream
                .write_all(frame.as_bytes())
                .await
                .expect("upstream joins");
        }
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        // The controller is retained for *every* generation, not only the ones a test
        // expects to end. A peer registered as `None` here is a peer no test can drop, and
        // a test that then calls `drop_generation` on it waits -- silently, and for a full
        // liveness interval -- for a generation that was never actually ended.
        self.upstreams_closable.push(Some(controller));
        // Liveness alone is not enough. The advertisement is published just after upstream
        // negotiation finishes, and a client that registers in between would be answered
        // against the fallback list -- which is a real ordering this test must not race.
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
        })
        .await;
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if !self
                .control
                .advertisement(NetworkId(network))
                .await
                .unwrap_or_default()
                .is_empty()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the owner never published an advertisement for {network}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Neither liveness nor the advertisement proves the startup frames were *applied*.
        // They were only written, and the owner applies them later, in order. A probe client
        // settles that ordering: its barrier is a frame it is owed anyway, so seeing it
        // proves every earlier line -- including any `353` -- was applied first.
        //
        // Without this the suite passes in isolation and fails under parallel load, which is
        // the worst possible failure mode for a harness.
        let mut probe = register(&*self, NetworkId(network), SessionId(u64::MAX), "").await;
        sync(self, peer, &mut probe).await;
        drop(probe);
        peer
    }

    async fn send_upstream(&mut self, peer: usize, frame: &str) {
        self.upstreams[peer]
            .write_all(frame.as_bytes())
            .await
            .expect("upstream accepts traffic");
    }

    async fn stop(self) {
        self.control.request_stop();
        self.task
            .await
            .expect("controller task joins")
            .expect("controller reports success");
        self._store.shutdown().expect("store shuts down");
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

struct Client {
    end: ScriptedStream,
    _script: i2pr_irc_testkit::FaultController,
    /// Retained so a client is only dropped when the test ends it. Dropping the task
    /// would cancel admission rather than ending it, which is not what a test that just
    /// wants a live client means.
    _outcome: tokio::task::JoinHandle<AdmissionOutcome>,
    seen: String,
}

impl Client {
    async fn until(&mut self, needle: &str) {
        self.await_new(0, needle).await
    }

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
            assert!(
                count > 0,
                "client closed waiting for {needle:?}; saw {:?}",
                &self.seen[mark..]
            );
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
    }

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

    async fn send(&mut self, frame: &str) {
        self.end
            .write_all(frame.as_bytes())
            .await
            .expect("the client writes");
    }

    fn mark(&self) -> usize {
        self.seen.len()
    }

    fn since(&self, mark: usize) -> String {
        self.seen[mark..].to_owned()
    }
}

fn admit(runtime: &Runtime, network: NetworkId, session: SessionId) -> Client {
    let (end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let control = runtime.control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::new(
            Some(NetworkSelection {
                network,
                expected_nick: "bot".to_owned(),
            }),
            control,
            session,
            ClientId(7),
        )
        .run(stream)
        .await
    });
    Client {
        end,
        _script: script,
        _outcome: outcome,
        seen: String::new(),
    }
}

async fn register(
    runtime: &Runtime,
    network: NetworkId,
    session: SessionId,
    capabilities: &str,
) -> Client {
    let mut client = admit(runtime, network, session);
    // Requested *during* registration, which is the only moment a negotiated surface can
    // apply to the projection: the projection is sent once, at attach.
    let request = if capabilities.is_empty() {
        "NICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n".to_owned()
    } else {
        format!("CAP REQ :{capabilities}\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n")
    };
    client.send(&request).await;
    client.until("001 bot").await;
    if !capabilities.is_empty() {
        assert!(
            !client.seen.contains("NAK"),
            "an advertised capability must be acknowledgeable during registration: {:?}",
            client.seen
        );
    }
    client.settle().await;
    client
}

async fn read_line(stream: &mut ScriptedStream) -> String {
    let mut all = Vec::new();
    let mut buf = [0u8; 1];
    loop {
        let count = tokio::time::timeout(CEILING, stream.read(&mut buf))
            .await
            .expect("upstream produces a line within the ceiling")
            .expect("the upstream stream does not fail");
        assert!(count > 0, "upstream stream ended mid-line");
        all.push(buf[0]);
        if all.ends_with(b"\r\n") {
            return String::from_utf8_lossy(&all).into_owned();
        }
    }
}

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

/// Sends `frames` upstream and waits until a session has observed everything before them.
///
/// Frames written upstream are only *queued*; the owner applies them later, and in order.
/// Asserting against a projection straight after writing upstream races that queue, and a
/// test that passes when it wins is not a test. The marker is a frame this client is owed
/// anyway, so seeing it proves every earlier line was applied first.
async fn sync(runtime: &mut Runtime, peer: usize, client: &mut Client) {
    let mark = client.mark();
    runtime
        .send_upstream(peer, ":sync!u@h PRIVMSG #room :barrier\r\n")
        .await;
    client.await_new(mark, "barrier").await;
}

/// Sends a `BouncerServ` command and returns everything it said, reassembled.
///
/// The export arrives as chunked `NOTICE`s, so the harness has to stitch the chunks back
/// into lines before anything can be parsed. That stitching is part of what a real client
/// does, and a test that skipped it would be testing a convenience the Operator does not
/// have.
async fn config(client: &mut Client, argument: &str) -> String {
    let mark = client.mark();
    client
        .send(&format!("PRIVMSG BouncerServ :{argument}\r\n"))
        .await;
    client.await_new(mark, "realname=").await;
    client.settle().await;
    let raw = client.since(mark);
    // One `NOTICE` per snapshot line, so concatenating the bodies *is* the document. If the
    // bouncer ever chunked a line across two NOTICEs this would silently produce garbage,
    // which is why the harness reassembles rather than reaching for a single frame.
    raw.lines()
        .filter_map(|line| line.split_once(" :").map(|(_, text)| text))
        .collect::<Vec<_>>()
        .join("\r\n")
}

// ------------------------------------------------------- diagnostics tests

/// Asks `BouncerServ` for a report and returns everything it said.
async fn diag(client: &mut Client, argument: &str) -> String {
    let mark = client.mark();
    client
        .send(&format!("PRIVMSG BouncerServ :{argument}\r\n"))
        .await;
    client.await_new(mark, "rejected_joins=").await;
    client.settle().await;
    client.since(mark)
}

#[tokio::test]
async fn diagnostics_report_the_running_state_of_a_live_network() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag network 1").await;

    assert!(reply.contains("netid=1"), "{reply}");
    assert!(
        reply.contains("name=net-1"),
        "the Operator's own label: {reply}"
    );
    assert!(
        reply.contains("phase=online"),
        "a phase an Operator can act on: {reply}"
    );
    assert!(reply.contains("generation="), "{reply}");
    assert!(
        reply.contains("visible=1"),
        "the joined channel is counted: {reply}"
    );
    assert!(reply.contains("sample=#room"), "{reply}");
    assert!(
        reply.contains("counts=") && reply.contains("recorded="),
        "a bouncer that silently drops history must be visible: {reply}"
    );
    assert!(
        reply.contains("dropped=0"),
        "dropping history is the one count that means data was lost, and it is reported: {reply}"
    );
    assert!(
        reply.contains("next_retry_delay=none"),
        "a connected Network has no retry scheduled, and says so rather than claiming zero: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn diagnostics_carry_no_endpoint_and_no_credential() {
    // The strongest statement in the plan: there is no field to render a secret in. This
    // test gives the bouncer both, then reads the whole reply and looks for them.
    //
    // The credential is written durably *before* the client attaches rather than with
    // `SASL SET`, because a configuration change restarts the owner and detaches every
    // session on it -- including the one that issued it. Both paths produce the same
    // durable record; only this one leaves a client alive to read the report.
    let mut runtime = Runtime::start().await;
    runtime.bring_online_credentialed(1, &["#room"]).await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag network 1").await;

    let destination = b32();
    assert!(
        !reply.contains(&destination),
        "an endpoint is not an Operator-facing field: {reply}"
    );
    assert!(
        !reply.contains("hunter2"),
        "a password must never be rendered: {reply}"
    );
    assert!(
        !reply.contains("b32.i2p"),
        "not even the shape of one: {reply}"
    );
    // The SASL *name* is not a secret, and `SASL STATUS` reports it deliberately. It is
    // not a diagnostics field, so a report must not carry it either -- a reader scanning
    // diagnostics would otherwise have to decide which surface is safe to paste.
    assert!(
        !reply.contains("bob"),
        "the credential's name is not a diagnostic field: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_diagnostics_list_that_is_truncated_says_how_many_are_missing() {
    let mut runtime = Runtime::start().await;
    // More joined channels than the sample ceiling, so the report has to decide what to
    // do with the tail. A report that quietly stops is indistinguishable from a short
    // Network, which is the failure this pair of tests exists to prevent.
    let many: Vec<String> = (0..i2pr_irc_runtime::diagnostics::MAX_REPORTED_CHANNELS + 9)
        .map(|index| format!("#room{index}"))
        .collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    runtime
        .bring_online(
            1,
            &names,
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag network 1").await;

    assert!(
        reply.contains(&format!(
            "visible={}",
            i2pr_irc_runtime::diagnostics::MAX_REPORTED_CHANNELS + 9
        )),
        "the true count is reported alongside the truncated sample: {reply}"
    );
    assert!(
        reply.contains("overflow=9"),
        "a truncated list must report what it left out: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_short_list_reports_no_overflow() {
    // The other half of the previous test: if `overflow` were always non-zero the first
    // test would prove nothing.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag network 1").await;

    assert!(reply.contains("visible=1"), "{reply}");
    assert!(
        reply.contains("overflow=0"),
        "an untruncated list must say nothing was left out: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn diagnostics_for_a_network_that_does_not_exist_is_not_a_silent_empty_report() {
    let mut runtime = Runtime::start().await;
    // Network 1 exists, so the client can bind; Network 99 is the typo this test types.
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :diag network 99\r\n")
        .await;
    client.await_new(mark, "FAIL BOUNCER").await;

    let reply = client.since(mark);
    assert!(reply.contains("no network with id 99"), "{reply}");
    assert!(
        !reply.contains("counts="),
        "a failed request must not also send a report the Operator could read as success: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_whole_process_report_covers_every_live_network() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    runtime
        .bring_online(
            2,
            &["#other"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag").await;

    assert!(reply.contains("netid=1"), "{reply}");
    assert!(
        reply.contains("netid=2"),
        "a process report is not a first-Network report: {reply}"
    );
    assert!(
        reply.contains("revision="),
        "a reader can tell two reports apart: {reply}"
    );
    assert!(
        reply.contains("owner_tasks="),
        "the process gauges are here: {reply}"
    );
    assert!(reply.contains("store_queue="), "{reply}");
    runtime.stop().await;
}

#[tokio::test]
async fn a_diagnostics_line_is_tagged_and_bounded() {
    let mut runtime = Runtime::start().await;
    let many: Vec<String> = (0..i2pr_irc_runtime::diagnostics::MAX_REPORTED_CHANNELS)
        .map(|index| format!("#room{index}"))
        .collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    runtime
        .bring_online(
            1,
            &names,
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let reply = diag(&mut client, "diag network 1").await;

    assert!(
        reply.contains("@bouncer-diag "),
        "a client must be able to separate this from administration traffic: {reply}"
    );
    for line in reply.lines() {
        let frame = line.trim_end_matches("\r");
        if frame.is_empty() {
            continue;
        }
        assert!(
            frame.len() <= 512,
            "a diagnostic must never be the thing that splits a message: {frame}"
        );
    }
    runtime.stop().await;
}

// ------------------------------------------------- configuration snapshot tests

#[tokio::test]
async fn a_configuration_export_round_trips_through_the_snapshot_format() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let exported = config(&mut client, "config export").await;
    let parsed = i2pr_irc_runtime::config_snapshot::parse(&exported).unwrap_or_else(|error| {
        panic!("the bouncer's own export must parse: {error:?} in {exported:?}")
    });

    assert_eq!(parsed.networks.len(), 1);
    assert_eq!(parsed.networks[0].network, NetworkId(1));
    assert_eq!(parsed.networks[0].display_name, "net-1");
    assert_eq!(parsed.networks[0].desired_channels.len(), 1);
    assert_eq!(parsed.networks[0].desired_channels[0].target, "#room");
    runtime.stop().await;
}

#[tokio::test]
async fn a_configuration_export_carries_no_credential() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online_credentialed(1, &["#room"]).await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let exported = config(&mut client, "config export").await;

    assert!(!exported.contains("hunter2"), "{exported}");
    assert!(!exported.contains("bob"), "{exported}");
    assert!(
        !exported.contains("sasl"),
        "not even the name of a field that could hold one: {exported}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_live_configuration_is_reported_as_a_valid_snapshot() {
    // The round trip the format's whole usability rests on: if the bouncer cannot read
    // back its own export, an Operator's export is worthless.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#a", "#b"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let mark = client.mark();
    client.send("PRIVMSG BouncerServ :config plan\r\n").await;
    client.until("config plan").await;

    assert!(
        !client.since(mark).contains("invalid"),
        "a bouncer's own configuration must validate: {}",
        client.since(mark)
    );
    assert!(
        !client.since(mark).contains("diverged"),
        "and must round trip exactly: {}",
        client.since(mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_import_applies_a_snapshot_one_network_at_a_time_and_reports_progress() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;

    let snapshot = i2pr_irc_runtime::config_snapshot::ConfigSnapshot {
        networks: vec![i2pr_irc_runtime::config_snapshot::SnapshotNetwork {
            network: NetworkId(7),
            display_name: "restored".to_owned(),
            endpoint: i2pr_irc_core::I2pEndpoint::parse(&b32()).expect("a destination"),
            failover_group: None,
            retain_existing_failover: false,
            nick: "bot".to_owned(),
            username: "user".to_owned(),
            realname: "bouncer".to_owned(),
            auto_away: false,
            keep_nick: false,
            desired_channels: vec![i2pr_irc_store::DesiredChannelRecord {
                target: "#restored".to_owned(),
                position: 0,
                detached: false,
                activity: i2pr_irc_store::ChannelActivityPolicy::default(),
            }],
            action_count: 0,
            action_phase_counts: [0, 0, 0],
        }],
    };

    let outcome = runtime
        .control
        .import_config(snapshot)
        .await
        .expect("the import is accepted");

    assert!(outcome.complete(), "{outcome:?}");
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.remaining, 0);
    assert!(outcome.stopped_at.is_none());
    let exported = runtime.control.export_config().await.expect("an export");
    assert_eq!(exported.networks.len(), 2);
    assert!(exported.networks.iter().any(|n| n.network == NetworkId(7)));
    runtime.stop().await;
}

#[tokio::test]
async fn an_import_that_names_a_different_network_under_one_identity_stops_without_writing_it() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let before = runtime.control.export_config().await.expect("an export");

    // Same identity, different Operator-chosen name: a snapshot from another bouncer.
    let mut entry = i2pr_irc_runtime::config_snapshot::SnapshotNetwork {
        network: NetworkId(1),
        display_name: "a-different-network".to_owned(),
        endpoint: i2pr_irc_core::I2pEndpoint::parse(&b32()).expect("a destination"),
        failover_group: None,
        retain_existing_failover: false,
        nick: "bot".to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        auto_away: false,
        keep_nick: false,
        desired_channels: Vec::new(),
        action_count: 0,
        action_phase_counts: [0, 0, 0],
    };
    // Network 2 sorts after Network 1, so this would have been applied had Network 1 not
    // conflicted. That ordering is what makes "stopped" a meaningful claim.
    let second = i2pr_irc_runtime::config_snapshot::SnapshotNetwork {
        network: NetworkId(2),
        display_name: "second".to_owned(),
        endpoint: entry.endpoint.clone(),
        failover_group: None,
        retain_existing_failover: false,
        nick: "bot".to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        auto_away: false,
        keep_nick: false,
        desired_channels: Vec::new(),
        action_count: 0,
        action_phase_counts: [0, 0, 0],
    };
    entry.desired_channels = Vec::new();

    let outcome = runtime
        .control
        .import_config(i2pr_irc_runtime::config_snapshot::ConfigSnapshot {
            networks: vec![entry, second],
        })
        .await
        .expect("the plan is accepted");

    assert!(!outcome.complete(), "{outcome:?}");
    assert_eq!(outcome.stopped_at, Some(NetworkId(1)), "{outcome:?}");
    assert_eq!(
        outcome.applied, 0,
        "a conflict is discovered before any write"
    );
    let after = runtime.control.export_config().await.expect("an export");
    assert_eq!(
        after, before,
        "a conflicting import must leave nothing behind"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_import_does_not_erase_a_stored_credential() {
    // The asymmetry that makes a non-secret export safe to import: applying a snapshot
    // writes `sasl: None`, and a record whose credential is set must survive that write
    // rather than being overwritten with the absence of one.
    let mut runtime = Runtime::start().await;
    runtime.bring_online_credentialed(1, &["#room"]).await;

    let snapshot = runtime.control.export_config().await.expect("an export");
    let outcome = runtime
        .control
        .import_config(snapshot)
        .await
        .expect("the import is accepted");

    assert!(outcome.complete(), "{outcome:?}");
    let record = runtime
        .control
        .network_record(NetworkId(1))
        .await
        .expect("a record")
        .expect("the Network still exists");
    assert!(
        record.sasl.is_some(),
        "an import must not silently clear a credential it could not have exported"
    );
    runtime.stop().await;
}

// ----------------------------------------------- registration action tests

#[tokio::test]
async fn a_stored_action_is_replayed_after_a_successful_registration() {
    let mut runtime = Runtime::start().await;
    // Actions are stored before the Network starts, because the replay happens at
    // registration and a Network that is already online has already registered.
    runtime.create_with_actions(1, &["+B", "+i"]).await;
    let peer = runtime
        .bring_existing_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;

    let seen = read_until(runtime.upstream(peer).await, b"MODE bot +B\r\n").await;
    assert!(seen.contains("MODE bot +B"), "{seen:?}");
    assert!(
        seen.contains("MODE bot +i"),
        "both actions replay, in the order they were written: {seen:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn action_phases_bracket_join_and_recovery_follows_a_fallback_nick() {
    use i2pr_irc_runtime::action::{ActionSet, RegistrationAction};
    use i2pr_irc_store::RegistrationActionPhase as Phase;

    let mut runtime = Runtime::start().await;
    let mut configured = record(1, &["#room"]);
    configured.keep_nick = true;
    runtime
        .control
        .create(configured)
        .await
        .expect("network creates with keep-nick policy");
    let actions = ActionSet::new(vec![
        RegistrationAction::message_in_phase("NickServ", "IDENTIFY pre-secret", Phase::PreJoin)
            .expect("pre-join service action"),
        RegistrationAction::mode_in_phase("+B", Phase::PostJoin).expect("post-join mode"),
        RegistrationAction::message_in_phase(
            "NickServ",
            "RECOVER bot recovery-secret",
            Phase::FallbackRecovery,
        )
        .expect("fallback recovery action"),
    ])
    .expect("bounded phased actions");
    runtime
        .control
        .set_actions(NetworkId(1), actions)
        .await
        .expect("actions persist");

    let (mut upstream, controller) = runtime.provider.closable_peer().await;
    read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :\r\n")
        .await
        .expect("minimal server finishes CAP LS");
    read_until(&mut upstream, b"CAP END\r\n").await;
    upstream
        .write_all(b":srv 433 bot bot :nickname in use\r\n")
        .await
        .expect("preferred nick collides");
    let fallback = read_until(&mut upstream, b"NICK bot_").await;
    assert!(fallback.contains("NICK bot_"));
    let fallback_nick = fallback
        .trim_end_matches("\r\n")
        .split_once(' ')
        .expect("fallback NICK has a parameter")
        .1;
    upstream
        .write_all(b":srv 001 bot_ :welcome\r\n")
        .await
        .expect("fallback identity completes registration");
    let transcript = read_until(
        &mut upstream,
        b"PRIVMSG NickServ :RECOVER bot recovery-secret\r\n",
    )
    .await;
    let text = transcript;
    assert!(text.contains("PRIVMSG NickServ :IDENTIFY pre-secret"));
    assert!(text.contains("JOIN #room"));
    let postjoin_mode = format!("MODE {fallback_nick} +B");
    assert!(text.contains(&postjoin_mode), "{text}");
    assert!(text.contains("PRIVMSG NickServ :RECOVER bot recovery-secret"));
    assert!(
        text.find("IDENTIFY pre-secret").unwrap() < text.find("JOIN #room").unwrap()
            && text.find("JOIN #room").unwrap() < text.find(&postjoin_mode).unwrap()
            && text.find(&postjoin_mode).unwrap()
                < text.find("RECOVER bot recovery-secret").unwrap(),
        "phase order is PreJoin, joins, PostJoin, FallbackRecovery: {text}"
    );
    runtime.upstreams.push(upstream);
    runtime.upstreams_closable.push(Some(controller));
    runtime.stop().await;
}

#[tokio::test]
async fn fallback_recovery_is_skipped_without_both_fallback_and_keep_nick() {
    use i2pr_irc_runtime::action::{ActionSet, RegistrationAction};
    use i2pr_irc_store::RegistrationActionPhase as Phase;

    let mut runtime = Runtime::start().await;
    for (network, keep_nick, collide) in [(1, true, false), (2, false, true)] {
        let mut configured = record(network, &["#room"]);
        configured.keep_nick = keep_nick;
        runtime
            .control
            .create(configured)
            .await
            .expect("network creates");
        let actions = ActionSet::new(vec![
            RegistrationAction::message_in_phase(
                "NickServ",
                "RECOVER bot recovery-secret",
                Phase::FallbackRecovery,
            )
            .expect("fallback recovery action"),
        ])
        .expect("bounded actions");
        runtime
            .control
            .set_actions(NetworkId(network), actions)
            .await
            .expect("actions persist");

        let (mut upstream, controller) = runtime.provider.closable_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n")
            .await
            .expect("CAP LS");
        read_until(&mut upstream, b"CAP END\r\n").await;
        if collide {
            upstream
                .write_all(b":srv 433 bot bot :nickname in use\r\n")
                .await
                .expect("preferred nick collides");
            read_until(&mut upstream, b"NICK bot_").await;
            upstream
                .write_all(b":srv 001 bot_1 :welcome\r\n")
                .await
                .expect("fallback registration completes");
        } else {
            upstream
                .write_all(b":srv 001 bot :welcome\r\n")
                .await
                .expect("preferred nick registration completes");
        }
        read_until(&mut upstream, b"JOIN #room\r\n").await;
        let mut received = Vec::new();
        let until = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
        while tokio::time::Instant::now() < until {
            let mut probe = [0_u8; 256];
            let remaining = until.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Ok(count)) = tokio::time::timeout(remaining, upstream.read(&mut probe)).await
            else {
                break;
            };
            received.extend_from_slice(&probe[..count]);
            assert!(
                !String::from_utf8_lossy(&probe[..count]).contains("RECOVER"),
                "recovery must not run: {}",
                String::from_utf8_lossy(&probe[..count])
            );
            if received
                .windows(b"PING :bouncer-".len())
                .any(|w| w == b"PING :bouncer-")
            {
                let ping = String::from_utf8_lossy(&received);
                if let Some(token) = ping.lines().find_map(|line| line.strip_prefix("PING :")) {
                    upstream
                        .write_all(format!("PONG :{token}\r\n").as_bytes())
                        .await
                        .expect("answer the keepalive");
                }
                received.clear();
            }
        }
        assert!(
            !String::from_utf8_lossy(&received).contains("RECOVER"),
            "recovery must not run: {}",
            String::from_utf8_lossy(&received)
        );
        runtime.upstreams.push(upstream);
        runtime.upstreams_closable.push(Some(controller));
    }
    runtime.stop().await;
}

#[tokio::test]
async fn an_identify_line_with_a_space_replays_whole() {
    // `IDENTIFY hunter2` is one message containing a space. A parser that took a single
    // whitespace-separated word would store `IDENTIFY` and drop the password, which fails at
    // the service while looking exactly like a working configuration.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    client
        .send("PRIVMSG BouncerServ :action set 1 message=NickServ text=IDENTIFY hunter2\r\n")
        .await;
    client.until("Set 1 actions").await;

    let stored = runtime
        .control
        .action_list(NetworkId(1))
        .await
        .expect("actions are readable");
    let frame = stored.actions()[0].frame("bot").expect("a frame");
    assert_eq!(
        frame, "PRIVMSG NickServ :IDENTIFY hunter2\r\n",
        "the whole message survives, spaces included"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn phase_scoped_action_set_replaces_only_that_phase() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    client
        .send("PRIVMSG BouncerServ :action set 1 phase=pre-join message=NickServ text=IDENTIFY first-secret\r\n")
        .await;
    client.until("Set 1 actions").await;
    client
        .send("PRIVMSG BouncerServ :action set 1 phase=fallback-recovery message=NickServ text=RECOVER bot second-secret\r\n")
        .await;
    client.until("Set 2 actions").await;

    let actions = runtime
        .control
        .action_list(NetworkId(1))
        .await
        .expect("actions read");
    assert_eq!(actions.len(), 2);
    assert_eq!(
        actions.actions()[0].phase,
        i2pr_irc_store::RegistrationActionPhase::PreJoin
    );
    assert_eq!(
        actions.actions()[1].phase,
        i2pr_irc_store::RegistrationActionPhase::FallbackRecovery
    );

    let clear_mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :action set 1 phase=pre-join\r\n")
        .await;
    client.await_new(clear_mark, "Set 1 actions").await;
    let remaining = runtime
        .control
        .action_list(NetworkId(1))
        .await
        .expect("remaining phase survives");
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining.actions()[0].phase,
        i2pr_irc_store::RegistrationActionPhase::FallbackRecovery
    );
    runtime.stop().await;
}

#[tokio::test]
async fn simultaneous_phase_updates_from_two_sessions_do_not_overwrite_each_other() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut first = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut second = register(&runtime, NetworkId(1), SessionId(2), "").await;
    let first_mark = first.mark();
    let second_mark = second.mark();
    let (_, _) = tokio::join!(
        first.send(
            "PRIVMSG BouncerServ :action set 1 phase=pre-join message=NickServ text=IDENTIFY first-secret\r\n"
        ),
        second.send(
            "PRIVMSG BouncerServ :action set 1 phase=fallback-recovery message=NickServ text=RECOVER bot second-secret\r\n"
        ),
    );
    first.await_new(first_mark, "Set 1 actions").await;
    second.await_new(second_mark, "Set 2 actions").await;

    let actions = runtime
        .control
        .action_list(NetworkId(1))
        .await
        .expect("actions read");
    assert_eq!(actions.len(), 2);
    assert_eq!(
        actions
            .actions_in_phase(i2pr_irc_store::RegistrationActionPhase::PreJoin)
            .count(),
        1
    );
    assert_eq!(
        actions
            .actions_in_phase(i2pr_irc_store::RegistrationActionPhase::FallbackRecovery)
            .count(),
        1
    );
    runtime.stop().await;
}

/// A reconnect replays the whole action sequence, from the beginning.
///
/// This test used to take about two minutes, and the reason it recorded was wrong. It
/// claimed the bouncer does not begin a new generation until roughly `CONNECT_TIMEOUT`
/// after the upstream stream ends. The bouncer does not have that behaviour: an upstream
/// server that hangs up produces end-of-file on the owner's read, which ends the
/// generation at once. What actually happened is that this module registered the first
/// generation with no fault controller, so `drop_generation` was a silent no-op, the
/// generation was never ended, and the test simply waited for `LIVENESS_DEADLINE` to end
/// it -- and then asserted its real subject against that accidental reconnect.
///
/// That distinction is the whole point of the test. A replay on reconnect is a claim that
/// only a *deliberately caused* reconnect can establish; measured against a keepalive
/// timeout it was asserting the timer, not the bouncer.
#[tokio::test]
async fn a_reconnect_replays_the_action_sequence_intentionally() {
    // The plan's explicit semantics: these are per-generation setup, not an ambiguous user
    // message. Replaying them on every generation is the feature, and every one is
    // idempotent, which is what makes that correct.
    let mut runtime = Runtime::start().await;
    runtime.create_with_actions(1, &["+B"]).await;
    let first = runtime
        .bring_existing_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    read_until(runtime.upstream(first).await, b"MODE bot +B\r\n").await;

    runtime.drop_generation(first).await;
    // The retry is the reconnect scheduler's decision, so the test waits for the peer the
    // owner actually asks for rather than handing itself one.
    let second = runtime.next_peer().await;
    read_until(
        runtime.upstream(second).await,
        b"USER user 0 * :bouncer\r\n",
    )
    .await;
    runtime
        .send_upstream(second, ":srv CAP * LS :message-tags\r\n")
        .await;
    runtime.finish_registration(second).await;
    let seen = read_until(runtime.upstream(second).await, b"MODE bot +B\r\n").await;
    assert!(
        seen.contains("MODE bot +B"),
        "the second generation replays the sequence from its beginning: {seen:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_action_status_reports_a_count_and_never_a_payload() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    client
        .send("PRIVMSG BouncerServ :action set 1 message=NickServ text=IDENTIFY hunter2\r\n")
        .await;
    client.until("Set 1 actions").await;

    let mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :action status 1\r\n")
        .await;
    client.until("actions 1").await;

    let reply = client.since(mark);
    assert!(!reply.contains("hunter2"), "{reply}");
    assert!(!reply.contains("NickServ"), "not even the target: {reply}");
    runtime.stop().await;
}

#[tokio::test]
async fn config_snapshot_round_trips_phase_counts_without_action_payloads() {
    use i2pr_irc_runtime::action::{ActionSet, RegistrationAction};
    use i2pr_irc_store::RegistrationActionPhase as Phase;

    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let actions = ActionSet::new(vec![
        RegistrationAction::message_in_phase("NickServ", "IDENTIFY pre-secret", Phase::PreJoin)
            .expect("pre-join action"),
        RegistrationAction::message("NickServ", "IDENTIFY post-secret").expect("post-join action"),
        RegistrationAction::message_in_phase(
            "NickServ",
            "RECOVER recovery-secret",
            Phase::FallbackRecovery,
        )
        .expect("fallback action"),
    ])
    .expect("bounded action set");
    runtime
        .control
        .set_actions(NetworkId(1), actions)
        .await
        .expect("actions persist");

    let snapshot = runtime
        .control
        .export_config()
        .await
        .expect("snapshot exports");
    assert_eq!(snapshot.networks()[0].action_count, 3);
    assert_eq!(snapshot.networks()[0].action_phase_counts, [1, 1, 1]);
    let rendered = i2pr_irc_runtime::config_snapshot::render(&snapshot);
    assert!(rendered.contains("action_phases=1,1,1"), "{rendered}");
    for secret in ["pre-secret", "post-secret", "recovery-secret"] {
        assert!(
            !rendered.contains(secret),
            "snapshots omit action payloads: {rendered}"
        );
    }
    assert_eq!(
        i2pr_irc_runtime::config_snapshot::parse(&rendered).expect("snapshot parses"),
        snapshot
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_action_refusal_names_the_problem_without_echoing_the_text() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    // The target is not a service, so the whole action is refused.
    let mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :action set 1 message=bot text=IDENTIFY hunter2\r\n")
        .await;
    client.await_new(mark, "FAIL BOUNCER").await;

    let reply = client.since(mark);
    assert!(
        !reply.contains("hunter2"),
        "a refusal must not echo the text: {reply}"
    );
    assert!(
        reply.contains("NickServ") || reply.contains("service"),
        "the refusal must say what to change: {reply}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn no_forbidden_command_can_be_reached_through_the_action_surface() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    for attempt in [
        "action set 1 NICK=other",
        "action set 1 JOIN=#room",
        "action set 1 QUIT=bye",
        "action set 1 CAP=LS",
        "action set 1 AUTHENTICATE=PLAIN",
        "action set 1 BOUNCER=LISTNETWORKS",
        "action set 1 raw=JOIN #room",
        "action set 1 line=QUIT :bye",
    ] {
        let mark = client.mark();
        client
            .send(&format!("PRIVMSG BouncerServ :{attempt}\r\n"))
            .await;
        client.settle().await;
        let reply = client.since(mark);
        assert!(
            reply.contains("FAIL BOUNCER"),
            "{attempt} must be refused: {reply}"
        );
        assert!(
            !reply.contains("Set "),
            "{attempt} must not have been stored: {reply}"
        );
    }

    // And the Network still has none.
    let mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :action status 1\r\n")
        .await;
    client.await_new(mark, "actions 0").await;
    runtime.stop().await;
}

#[tokio::test]
async fn an_action_count_above_the_ceiling_is_refused_and_stores_nothing() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;

    let many: Vec<String> = (0..=i2pr_irc_runtime::action::MAX_REGISTRATION_ACTIONS)
        .map(|_| "mode=+B".to_owned())
        .collect();
    let mark = client.mark();
    client
        .send(&format!(
            "PRIVMSG BouncerServ :action set 1 {}\r\n",
            many.join(" ")
        ))
        .await;
    client.await_new(mark, "FAIL BOUNCER").await;

    let mark = client.mark();
    client
        .send("PRIVMSG BouncerServ :action status 1\r\n")
        .await;
    client.await_new(mark, "actions 0").await;
    runtime.stop().await;
}

#[tokio::test]
async fn an_action_set_replaces_the_previous_list_rather_than_appending() {
    let mut runtime = Runtime::start().await;
    runtime.create_with_actions(1, &["+B", "+i"]).await;
    // Online first: a client cannot register against a Network that has no live owner.
    runtime
        .bring_existing_online(
            1,
            &["#room"],
            "message-tags server-time batch",
            "CHANTYPES=#",
            ":bot!u@h JOIN {channel}",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    client
        .send("PRIVMSG BouncerServ :action set 1 mode=+w\r\n")
        .await;
    client.until("Set 1 actions").await;

    let stored = runtime
        .control
        .action_list(NetworkId(1))
        .await
        .expect("actions are readable");
    assert_eq!(
        stored.actions().len(),
        1,
        "the second write replaced rather than appended: {stored:?}"
    );
    assert_eq!(stored.actions()[0].target, "+w");
    runtime.stop().await;
}
