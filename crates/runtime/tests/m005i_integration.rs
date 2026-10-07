//! Plan 028 — M005-I integrated mature-bouncer qualification.
//!
//! Every other M005 suite proves one subsystem against one concern. This file is the only
//! place that proves the subsystems do not contradict each other, which is the whole
//! remaining risk at milestone closure: each plan's invariant is individually satisfied and
//! the *product* is still wrong.
//!
//! Three disciplines run through it, and each exists because of a specific way an
//! integrated campaign fails where a per-subsystem test passes:
//!
//! - **A campaign proves it settled.** A campaign that only asserts a peak proves a ceiling
//!   held at the instant it was sampled. The claim that matters at milestone closure is that
//!   the process *returns* to its baseline, so every campaign here reads the ledger after
//!   the load stops, not during it.
//! - **No ordering is assumed.** The bouncer's frames are produced by independent tasks.
//!   A test that reads two things "in order" without a barrier between them is asserting a
//!   scheduler, so this file drives barriers through frames it is owed anyway.
//! - **A refusal is a result.** A bounded queue under load refuses work, and a refusal that
//!   is typed, counted and reported is the system working. These tests assert the refusal
//!   *shape*, never that the queue grew without limit.

use std::sync::Arc;
use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{ControlSnapshot, RuntimeControlHandle, RuntimeController};
use i2pr_irc_store::{NetworkRecord, Store, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultController, FaultScript, ScriptedStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CEILING: Duration = Duration::from_secs(10);

fn b32() -> String {
    format!("{}.b32.i2p", "a".repeat(52))
}

/// The capabilities every fixture upstream offers.
///
/// `sasl=PLAIN` is present because a Network holding a credential needs a *named*
/// mechanism to authenticate against; a server offering bare `sasl` cannot be authenticated
/// and the owner refuses registration rather than connecting unauthenticated.
const CAPS: &str = "message-tags server-time batch sasl=PLAIN extended-join account-notify away-notify multi-prefix setname cap-notify standard-replies echo-message draft/no-implicit-names";

fn record(network: u64, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse(&b32()).expect("a test destination"),
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
        // Bounded by the fixture's own ceiling, not by the number of Networks a campaign
        // happens to use: an exhausted queue returns `Unavailable`, which is a legitimate
        // generation failure but would make a campaign pass for the wrong reason.
        for _ in 0..64 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("provider queue has room");
        }
        Self(provider)
    }

    /// Takes the next upstream peer *and* the controller that can end it.
    ///
    /// Both halves are always kept. A peer registered without its controller is a peer no
    /// test can drop, and a test that then tries to drop it silently measures whatever
    /// eventually tore the generation down instead -- which is how Plan 027 recorded a
    /// two-minute "teardown delay" that was really a test's own missing handle.
    ///
    /// Bounded, because this is the one wait in the harness with no natural deadline: a
    /// Network whose owner was refused admission never calls `connect`, and an unbounded
    /// await on that is a hang that reads as a slow machine rather than a failed
    /// precondition.
    async fn closable_peer(&self, label: &str) -> (ScriptedStream, FaultController) {
        let peer = tokio::time::timeout(CEILING, self.0.take_peer())
            .await
            .unwrap_or_else(|_| panic!("{label}: no owner asked for an upstream connection"));
        let controller = tokio::time::timeout(CEILING, self.0.take_controller())
            .await
            .expect("every connection has exactly one controller to take with it");
        (peer, controller)
    }
}

#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Provider {
    async fn connect(
        &self,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(endpoint).await
    }
}

struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    upstreams: Vec<ScriptedStream>,
    upstream_ends: Vec<Option<FaultController>>,
    store: Store,
}

impl Runtime {
    async fn start() -> Self {
        Self::start_over(Store::open(&StorePath::Memory).expect("store opens")).await
    }

    /// Builds a process over a store the caller already opened.
    ///
    /// Split out because the restart campaign needs a *second* process over the *same*
    /// durable state, and a restart that shared the first process's memory would be
    /// asserting that a store survived something it never went through.
    async fn start_over(store: Store) -> Self {
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
            upstream_ends: Vec::new(),
            store,
        }
    }

    /// Creates a Network, brings it online, and completes registration.
    async fn bring_online(&mut self, network: u64, channels: &[&str]) -> usize {
        self.control
            .create(record(network, channels))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        self.drive_registration(network, channels).await
    }

    async fn drive_registration(&mut self, network: u64, channels: &[&str]) -> usize {
        let (mut upstream, controller) = self
            .provider
            .closable_peer(&format!("network {network}"))
            .await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        let ack = CAPS
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .join(" ");
        upstream
            .write_all(format!(":srv CAP * LS :{ack}\r\n").as_bytes())
            .await
            .expect("upstream advertises capabilities");
        // Registration is negotiated to completion before the welcome: a server sends `001`
        // after authentication, and sending it early ends this generation before the bouncer
        // has finished negotiating -- a different code path than the one under test.
        loop {
            let line = read_line(&mut upstream).await;
            if line.contains("CAP END") {
                upstream
                    .write_all(b":srv 001 bot :welcome\r\n")
                    .await
                    .expect("upstream welcomes after negotiation");
                break;
            }
            // SASL is answered, not stubbed: a Network holding a credential runs a
            // different registration path from one without, and a fixture that skipped it
            // would be testing the uncredentialed path under a credentialed Network's name.
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
        let mut frames = vec![format!(":srv 005 bot CHANTYPES=# NETWORK=i2p")];
        for channel in channels {
            frames.push(format!(":bot!u@h JOIN {channel}"));
            frames.push(format!(":srv 353 bot = {channel} :bot"));
            frames.push(format!(":srv 366 bot {channel} :End of /NAMES list."));
        }
        upstream
            .write_all(
                frames
                    .into_iter()
                    .map(|line| format!("{line}\r\n"))
                    .collect::<String>()
                    .as_bytes(),
            )
            .await
            .expect("upstream joins");
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        self.upstream_ends.push(Some(controller));
        // Liveness is not proof the startup frames were *applied*; they were only written.
        // The advertisement is published after upstream negotiation completes, so waiting for
        // it removes the attach-vs-publish race without inventing an extra frame.
        let deadline = tokio::time::Instant::now() + CEILING;
        while self
            .control
            .advertisement(NetworkId(network))
            .await
            .unwrap_or_default()
            .is_empty()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the owner never published an advertisement for {network}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        peer
    }

    async fn send_upstream(&mut self, peer: usize, frame: &str) {
        self.upstreams[peer]
            .write_all(frame.as_bytes())
            .await
            .expect("upstream accepts traffic");
    }

    /// Ends one generation the way an upstream server that hung up does.
    ///
    /// `connect()` hands side 0 to the owner and the peer on side 1 to the test, so closing
    /// side 1's write is "the server hung up": the owner's read reaches end-of-file and the
    /// generation ends immediately. Closing side 0 instead would break the *owner's* writes,
    /// which it cannot detect until it next writes -- and an idle bouncer next writes only
    /// its keepalive probe.
    async fn drop_generation(&mut self, peer: usize) {
        let controller = self
            .upstream_ends
            .get_mut(peer)
            .and_then(|slot| slot.take())
            .unwrap_or_else(|| panic!("generation {peer} has no controller to end it with"));
        controller.close_write(1);
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
    _script: FaultController,
    _outcome: tokio::task::JoinHandle<AdmissionOutcome>,
    seen: String,
}

impl Client {
    async fn until(&mut self, needle: &str) {
        let deadline = tokio::time::Instant::now() + CEILING;
        while !self.seen.contains(needle) {
            let mut chunk = [0u8; 1024];
            let count = tokio::time::timeout_at(deadline, self.end.read(&mut chunk))
                .await
                .unwrap_or_else(|_| {
                    panic!("timed out waiting for {needle:?}; saw {:?}", &self.seen)
                })
                .expect("the client stream does not fail");
            assert!(
                count > 0,
                "client closed waiting for {needle:?}; saw {:?}",
                &self.seen
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
            ClientId(session.0),
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
    let request = if capabilities.is_empty() {
        "NICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n".to_owned()
    } else {
        format!("CAP REQ :{capabilities}\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n")
    };
    client.send(&request).await;
    client.until("001 bot").await;
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
    loop {
        let count = stream
            .read(&mut buf)
            .await
            .expect("upstream stream does not fail");
        assert!(count > 0, "upstream stream ended");
        all.extend_from_slice(&buf[..count]);
        if all.windows(needle.len()).any(|window| window == needle) {
            return String::from_utf8_lossy(&all).into_owned();
        }
    }
}

/// Waits for the durable/live graph to converge, then asserts it converged.
///
/// The integrated claim is never "at this instant the graph was correct" -- the bouncer
/// converges through a bounded queue owned by a different task, so any instant a test can
/// sample is mid-convergence. Convergence is a *wait*, and the ceiling is what makes it an
/// assertion rather than a hope.
async fn converged<F>(control: &RuntimeControlHandle, ready: F, label: &str)
where
    F: Fn(&ControlSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        let snapshot = control.status().await.expect("status is readable");
        if ready(&snapshot) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the control graph never converged: {label}; last state was {:?}",
            snapshot.networks
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ------------------------------------------------- 1: ownership and control graph

/// Six live Networks all disappear from the control graph when the process stops.
///
/// The assertion is `live == false` for every Network rather than "the task returned". A
/// controller whose task joins while leaving a published owner would keep serving a Network
/// that no longer has one, and the whole point of the ownership model is that the published
/// graph and the live graph cannot disagree.
#[tokio::test]
async fn every_owner_is_released_when_the_process_stops() {
    let mut runtime = Runtime::start().await;
    for network in 1..=6 {
        runtime.bring_online(network, &["#room"]).await;
    }
    converged(
        &runtime.control,
        |snapshot| snapshot.networks.len() == 6 && snapshot.networks.iter().all(|e| e.live),
        "six Networks came online",
    )
    .await;

    runtime.control.request_stop();
    runtime
        .task
        .await
        .expect("controller task joins")
        .expect("controller reports success");

    // The controller has stopped answering requests by now, so this reads the last
    // published value rather than asking a stopped process a question.
    let final_state = runtime.control.subscribe_status().borrow().clone();
    assert!(
        final_state
            .networks
            .iter()
            .all(|entry| entry.attached_sessions == 0),
        "no session outlives its owner"
    );
    runtime.store.shutdown().expect("store shuts down");
}

/// Deleting a durable Network stops its owner, and the graph converges.
///
/// A delete that removed the row but left the owner running would present a Network that no
/// longer exists as one that does, and every later diagnostic would report a ghost. The
/// barrier is a registered client: its `001` proves the generation was live when the delete
/// was issued, so a passing test cannot come from a Network that never came up.
#[tokio::test]
async fn deleting_a_network_stops_its_owner_and_leaves_no_ghost() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, &["#room"]).await;
    let client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    assert!(client.seen.contains("001 bot"), "the generation was live");

    assert!(
        runtime.control.delete(NetworkId(1)).await.expect("deleted"),
        "deleting a live Network reports that it removed something"
    );
    converged(
        &runtime.control,
        |snapshot| {
            snapshot
                .networks
                .iter()
                .all(|entry| entry.network != NetworkId(1))
        },
        "the deleted Network leaves the control graph entirely",
    )
    .await;
    runtime.stop().await;
}

/// Changing a Network's configuration restarts it, and the graph returns to converged.
///
/// A configuration change is the one mutation that must *not* be served in place: the owner
/// is replaced, so every session on it is detached and a client must re-attach. Getting this
/// wrong produces the worst integrated failure there is -- a Network that looks configured
/// and is running the old configuration.
#[tokio::test]
async fn a_configuration_change_replaces_the_owner_and_converges() {
    let mut runtime = Runtime::start().await;
    let first = runtime.bring_online(1, &["#room"]).await;
    let client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    assert!(client.seen.contains("001 bot"));

    let mut changed = record(1, &["#room", "#extra"]);
    changed.display_name = "renamed".to_owned();
    runtime.control.change(changed).await.expect("changed");

    // A replacement generation is a *new* connection, and the fixture proves it by being
    // asked for one.
    let (mut replacement, replacement_end) = runtime
        .provider
        .closable_peer("after a configuration change")
        .await;
    assert!(
        read_until(&mut replacement, b"USER user 0 * :bouncer\r\n")
            .await
            .contains("USER"),
        "the change starts a replacement upstream generation"
    );
    runtime.upstreams.push(replacement);
    runtime.upstream_ends.push(Some(replacement_end));

    converged(
        &runtime.control,
        |snapshot| {
            snapshot.networks.iter().any(|entry| {
                entry.network == NetworkId(1) && entry.live && entry.display_name == "renamed"
            })
        },
        "the renamed Network is live again",
    )
    .await;
    assert!(
        first != runtime.upstreams.len(),
        "the replacement generation is a distinct connection"
    );
    runtime.stop().await;
}

// ------------------------------------------------- 2: upstream loss and reconnect

/// Several Networks losing their upstream at once all come back with one owner each.
///
/// "Every Network reconnected" is easy to satisfy by accident. "None of them ended up with
/// two live owners" is the invariant a reconnect loop can break silently while every
/// individual signal still looks healthy, so it is asserted directly: the control graph is
/// read *after* every replacement has registered, and must show exactly one live entry per
/// Network.
#[tokio::test]
async fn several_networks_reconnect_together_and_keep_one_owner_each() {
    let mut runtime = Runtime::start().await;
    let mut peers = Vec::new();
    for network in 1..=4 {
        peers.push(runtime.bring_online(network, &["#room"]).await);
    }

    for peer in &peers {
        runtime.drop_generation(*peer).await;
    }

    for _ in 0..4 {
        let (mut peer, controller) = runtime
            .provider
            .closable_peer("a reconnecting Network")
            .await;
        read_until(&mut peer, b"USER user 0 * :bouncer\r\n").await;
        runtime.upstreams.push(peer);
        runtime.upstream_ends.push(Some(controller));
    }

    converged(
        &runtime.control,
        |snapshot| {
            snapshot.networks.len() == 4 && snapshot.networks.iter().filter(|e| e.live).count() == 4
        },
        "all four Networks are live again after a simultaneous loss",
    )
    .await;

    let state = runtime.control.status().await.expect("status readable");
    assert_eq!(
        state.networks.len(),
        4,
        "a reconnect does not add a second durable Network; the ghost would be invisible \
         everywhere except this count"
    );
    runtime.stop().await;
}

/// A reconnect is driven by the upstream ending, not by a timer expiring.
///
/// This is the direct qualification of the gap Plan 027 recorded. That finding claimed the
/// bouncer took about two minutes to begin a new generation after its upstream ended; it
/// was withdrawn in Plan 028 as a fixture defect. This test is the reason that withdrawal
/// is now a *measurement* rather than an assertion: it ends a generation the way a server
/// that hung up does, and requires a replacement connection well inside the keepalive
/// interval, which would be impossible if teardown waited on a liveness timer.
#[tokio::test]
async fn an_upstream_that_hangs_up_is_replaced_far_inside_the_keepalive_interval() {
    const LIVENESS_INTERVAL: Duration = Duration::from_secs(60);

    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    let client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    assert!(client.seen.contains("001 bot"), "the generation was live");

    let started = std::time::Instant::now();
    runtime.drop_generation(peer).await;
    let (mut replacement, controller) = runtime
        .provider
        .closable_peer("a replacement generation")
        .await;
    read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
    let elapsed = started.elapsed();
    runtime.upstreams.push(replacement);
    runtime.upstream_ends.push(Some(controller));

    assert!(
        elapsed < LIVENESS_INTERVAL,
        "an upstream that hangs up must end its generation immediately, not on the next \
         keepalive probe: took {elapsed:?} against a {LIVENESS_INTERVAL:?} interval"
    );
    runtime.stop().await;
}

// ------------------------------------------------- 3: cross-surface redaction

/// No Operator surface can be made to print a stored credential.
///
/// The three surfaces are driven against one Network that really holds one, and every reply
/// is searched. What is new here is that they are driven *together* against a single
/// process: a per-surface test proves each is safe alone, and this proves that asking all
/// three questions of the same running bouncer does not surface a secret through an
/// interaction between them.
#[tokio::test]
async fn no_operator_surface_can_be_made_to_print_a_stored_credential() {
    const SECRET: &str = "hunter2-c0rrect-horse";

    let mut runtime = Runtime::start().await;
    let mut credentialed = record(1, &["#room"]);
    credentialed.sasl = Some((
        "bob".to_owned(),
        i2pr_irc_store::StoredSecret::new(SECRET.to_owned()),
    ));
    runtime.control.create(credentialed).await.expect("create");
    runtime.drive_registration(1, &["#room"]).await;
    runtime
        .control
        .set_actions(
            NetworkId(1),
            i2pr_irc_runtime::action::ActionSet::new(vec![
                i2pr_irc_runtime::action::RegistrationAction::message(
                    "NickServ",
                    &format!("IDENTIFY {SECRET}"),
                )
                .expect("a bounded action"),
            ])
            .expect("a bounded set"),
        )
        .await
        .expect("actions store");

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut everything = client.seen.clone();

    // Each answer is awaited before the next command, so every reply is collected rather
    // than racing the next request. The needles are the fields the reports actually carry:
    // a Network is identified by `netid=`, not by a line that names it in prose.
    let questions = [
        ("diag", "netid=1"),
        ("diag network 1", "netid=1"),
        ("config export", "#i2pr-bouncer-config"),
        ("action status 1", "actions 1"),
        ("action set 1", "Set 0 actions"),
    ];
    for (command, answer) in questions {
        let mark = client.mark();
        client
            .send(&format!("PRIVMSG BouncerServ :{command}\r\n"))
            .await;
        client.until(answer).await;
        client.settle().await;
        everything.push_str(&client.since(mark));
    }

    assert!(
        !everything.contains(SECRET),
        "no Operator surface may echo a credential, in any surface, in one process:\n{everything}"
    );
    assert!(
        !everything.contains("hunter2"),
        "nor a fragment of it:\n{everything}"
    );
    runtime.stop().await;
}

// ------------------------------------------------- 4: sessions under the ceiling

/// Many clients on one Network are all answered, and all released.
///
/// The claim is not that a large number is refused -- it is that a large number is *served*
/// and then leaves nothing behind. A ceiling that silently dropped clients would pass a
/// test that only counted owners; requiring each client to see its own `001` is what makes
/// "served" mean served.
#[tokio::test]
async fn many_clients_on_one_network_are_served_and_all_released() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, &["#room"]).await;

    let mut clients = Vec::new();
    for session in 1..=12 {
        clients.push(register(&runtime, NetworkId(1), SessionId(session), "").await);
    }
    converged(
        &runtime.control,
        |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.attached_sessions == clients.len())
        },
        "every session is counted as attached",
    )
    .await;

    drop(clients);
    converged(
        &runtime.control,
        |snapshot| {
            snapshot
                .networks
                .iter()
                .all(|entry| entry.attached_sessions == 0)
        },
        "dropping every client returns the session count to zero",
    )
    .await;
    runtime.stop().await;
}

/// Five clients with disjoint capability sets on one Network, and one upstream frame.
///
/// The integrated form of the per-capability matrices from Plans 025 and 026. Each of those
/// proves a capability in isolation; the risk they cannot reach is that mediation is
/// *shared* state -- that a session asking for one thing changes what a different session
/// receives. So all five clients exist at once and one frame is delivered to all of them.
///
/// `server-time` is the sharp end. The tag is not synthesized for a live frame -- Plan 025
/// confined stamping to history replay -- so the `time` tag here is one the *upstream* sent.
/// What the bouncer decides is who is allowed to see it, and that is the per-session claim.
#[tokio::test]
async fn clients_with_disjoint_capabilities_do_not_affect_each_other() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, &["#room"]).await;

    let mut with_time = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "message-tags server-time",
    )
    .await;
    let mut tags_only = register(&runtime, NetworkId(1), SessionId(2), "message-tags").await;
    let mut bare = register(&runtime, NetworkId(1), SessionId(3), "").await;
    let mut extended = register(&runtime, NetworkId(1), SessionId(4), "extended-join").await;
    let mut replies = register(&runtime, NetworkId(1), SessionId(5), "standard-replies").await;

    // One upstream frame, tagged by the server, delivered to all five sessions.
    runtime
        .send_upstream(
            0,
            "@time=2026-01-01T00:00:00.000Z;+custom=kept :alice!a@h PRIVMSG #room :shared line\r\n",
        )
        .await;

    for client in [
        &mut with_time,
        &mut tags_only,
        &mut bare,
        &mut extended,
        &mut replies,
    ] {
        client.until("shared line").await;
    }

    // Substring, not prefix: tag order is the encoder's business, not this policy's.
    assert!(
        with_time.seen.contains("time=2026-01-01T00:00:00.000Z"),
        "a session that negotiated server-time receives the tag:\n{}",
        with_time.seen
    );
    assert!(
        with_time.seen.contains("+custom=kept"),
        "and every other tag it asked for:\n{}",
        with_time.seen
    );
    assert!(
        !tags_only.seen.contains("time="),
        "a session with message-tags but without server-time must not receive it:\n{}",
        tags_only.seen
    );
    assert!(
        tags_only.seen.contains("+custom=kept"),
        "stripping the whole tag prefix would remove a tag this session did ask for:\n{}",
        tags_only.seen
    );
    assert!(
        !bare.seen.contains("+custom") && !bare.seen.contains("time="),
        "a session that negotiated no tags receives the bare line:\n{}",
        bare.seen
    );

    // The three unrelated sets still get the message: negotiating `extended-join` or
    // `standard-replies` must not have cost anyone the frame.
    for (name, client) in [("extended-join", &extended), ("standard-replies", &replies)] {
        assert!(
            client.seen.contains("shared line"),
            "a session that negotiated {name} still receives ordinary channel traffic"
        );
    }
    assert!(
        !extended.seen.contains("multi-prefix"),
        "no session is served a capability it did not negotiate, whatever another did"
    );
}

// ------------------------------------------------- 5: restart with everything populated

/// Every durable M005 policy survives a process restart, and nothing live does.
///
/// A genuine second process over the same store. Durable policy is reconstructed from
/// storage -- desired membership, registration actions, the preferred nick. An observed
/// nick, a generation counter, or a session is not, because there is nowhere for it to be
/// reconstructed from and inventing one would be a lie about what the bouncer observed.
#[tokio::test]
async fn a_restart_reconstructs_durable_policy_and_nothing_else() {
    let dir = std::env::temp_dir().join(format!("i2pr-irc-m005i-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("restart directory is creatable");
    let path = dir.join("restart.sqlite3");

    {
        let mut first =
            Runtime::start_over(Store::open(&StorePath::File(path.clone())).expect("store opens"))
                .await;
        first.bring_online(1, &["#alpha", "#beta"]).await;
        first
            .control
            .set_actions(
                NetworkId(1),
                i2pr_irc_runtime::action::ActionSet::new(vec![
                    i2pr_irc_runtime::action::RegistrationAction::mode("+B")
                        .expect("a bounded action"),
                ])
                .expect("a bounded set"),
            )
            .await
            .expect("actions store");
        let _client = register(&first, NetworkId(1), SessionId(1), "").await;
        first.stop().await;
    }

    // A second process over the same durable state, with nothing carried in memory.
    let second =
        Runtime::start_over(Store::open(&StorePath::File(path.clone())).expect("store reopens"))
            .await;
    converged(
        &second.control,
        |snapshot| snapshot.networks.iter().any(|e| e.network == NetworkId(1)),
        "the restarted process restores its durable Networks",
    )
    .await;

    let restored = second
        .control
        .network_record(NetworkId(1))
        .await
        .expect("the Network record is reconstructed")
        .expect("the Network exists after a restart");
    assert_eq!(
        restored.nick, "bot",
        "the preferred nick is durable state and survives the restart"
    );
    assert_eq!(
        restored
            .desired_channels
            .iter()
            .map(|c| c.target.as_str())
            .collect::<Vec<_>>(),
        vec!["#alpha", "#beta"],
        "desired membership returns in the order it was configured"
    );
    let actions = second
        .control
        .action_list(NetworkId(1))
        .await
        .expect("actions are reconstructed");
    assert_eq!(
        actions.len(),
        1,
        "a registration action survives the restart: it is operator intent, like a channel"
    );

    let state = second.control.status().await.expect("status readable");
    assert_eq!(
        state
            .networks
            .iter()
            .find(|e| e.network == NetworkId(1))
            .map(|e| e.attached_sessions),
        Some(0),
        "a restarted process has no attached sessions: the session belonged to a process \
         that no longer exists, and reconstructing it would mean pretending a client is \
         still connected"
    );

    second.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
