//! Explicit external qualification of the production SAM provider through eggchaos.
//!
//! This suite is ignored in ordinary runs. `scripts/qualify-m007-eggchaos.py` supplies a
//! pinned eggchaos binary and runs it when the external qualification target is requested.
//!
//! # What is and is not claimed here
//!
//! Every test in this file runs the **production** path: the real [`SamProvider`], the
//! real owned SAM 3.1 client, the real [`RuntimeController`], and the real socket to the
//! provider's upstream, with the pinned eggchaos stream proxy in between. That is a
//! stronger claim than a generic echo-server smoke, and it is the only evidence in this
//! repository that recovery works *across a process socket boundary* rather than across
//! an in-memory duplex pair.
//!
//! Two things are deliberately **not** claimed here, and pretending otherwise is exactly
//! how an evidence claim rots:
//!
//! - **The accepted-stream multi-client boundary.** This repository has no standalone
//!   downstream listener, and Corrective 042 explicitly forbids adding one. Sessions are
//!   therefore not attachable through this path, so `attached_sessions` is asserted to be
//!   `0` before and after recovery rather than being quietly claimed as covered. The
//!   multi-client claim stays where it is honest: the in-process owner suites.
//! - **Stream loss as packet loss.** eggchaos stream-loss drops arbitrary application
//!   bytes with no TCP semantics, so whether a connection survives it depends on which
//!   byte was dropped. It is qualified against a generic echo peer in
//!   `qualification/m007/fault-smoke.py` as a fault-tool capability and is not asserted
//!   here as equivalent to real packet loss. See `qualification/m007/scenarios-v2.md`.
#![cfg(test)]

use std::{
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use i2pr_irc_core::{I2pEndpoint, NetworkId};
use i2pr_irc_runtime::{LIVENESS_DEADLINE, LIVENESS_INTERVAL, RuntimeController};
use i2pr_irc_sam::{
    SamClientConfig, SamProvider, SamTimeouts,
    fake::{FakeBridge, FakeIrcPeer, Script},
};
use i2pr_irc_store::{NetworkRecord, Store, StorePath};
use tokio::time::sleep;

/// Ceiling for a bounded wait that is not deliberately long.
///
/// A wait that exceeds it has failed to observe an event, which is a failure of the claim
/// under test rather than a flake to retry.
const CEILING: Duration = Duration::from_secs(30);

/// Ceiling for the liveness deadline to end a blackholed generation.
///
/// The production constants are a 60 s probe interval and a 120 s deadline, and the first
/// probe is queued as soon as the generation is online. A blackhole produces no EOF at
/// all, so this ceiling is what turns "the transport went quiet" into "the generation
/// ended" rather than leaving the test waiting on a socket that will never speak again.
/// The margin is generous on purpose: the claim being qualified is that recovery happens
/// *within a bounded time*, not that it happens at one particular second.
const LIVENESS_CEILING: Duration =
    Duration::from_secs(LIVENESS_INTERVAL.as_secs() + LIVENESS_DEADLINE.as_secs() + 30);

/// Ceiling for a reconnect to complete once the transport is healthy again.
///
/// A disruptive fault takes the SAM control connection down with the IRC one, so the
/// attempt already in flight when the loss is observed has to fail before a later one can
/// win. That failure waits out the provider's own 90 s `STREAM CONNECT` deadline, so the
/// ceiling is set above one such attempt rather than above the happy path.
const RECOVERY_CEILING: Duration = Duration::from_secs(180);

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The proxy name every scenario in this file configures.
const PROXY: &str = "sam-loopback";
/// The dormant silent-drop fault, armed by the blackhole scenario.
const BLACKHOLE: &str = "silent-drop";
/// The dormant hard-close fault, armed by the disconnect scenario.
const DISCONNECT: &str = "hard-close";

/// Router sessions the fake bridge will script.
///
/// One is the healthy steady state. The extra room covers re-identification after a fault
/// takes the SAM control connection down, which is normal behaviour rather than a failure.
const ROUTER_SESSIONS: usize = 6;

/// One product-path qualification fixture.
///
/// Owns the proxy child, the provider, the controller, and the fake bridge, and tears all
/// of them down in the right order. The order matters: releasing the Network scope
/// before stopping the controller is what proves the provider returns to its baseline
/// rather than being dropped on the floor.
struct ProductPath {
    binary: std::ffi::OsString,
    admin: SocketAddr,
    child: ChildGuard,
    provider: Arc<SamProvider>,
    peer: Arc<FakeIrcPeer>,
    bridge: FakeBridge,
    server: tokio::task::JoinHandle<()>,
    control: i2pr_irc_runtime::RuntimeControlHandle,
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    store: Store,
    config_path: PathBuf,
}

impl ProductPath {
    /// Starts bridge, proxy, provider, controller, and one Network at registration.
    ///
    /// `streams` is how many IRC streams the bridge must be able to answer across the whole
    /// run.
    ///
    /// The bridge is scripted with several router *sessions*, not one. A fault that takes
    /// the SAM control connection down — which is what a blackholed or severed transport
    /// really does — legitimately forces the provider to re-identify with the router
    /// before it can reconnect. A fixture able to answer only one session would then be
    /// measuring its own exhaustion: every rebuild request goes unanswered, the client
    /// waits out its deadline, and the Network sits in backoff for a reason that has
    /// nothing to do with the behaviour under qualification.
    async fn start(streams: usize) -> Self {
        let binary =
            std::env::var_os("EGGCHAOS_BIN").expect("qualification script sets EGGCHAOS_BIN");
        let bridge = FakeBridge::start(Script::scoped(ROUTER_SESSIONS, streams)).await;
        let proxy = reserve_addr().await;
        let admin = reserve_addr().await;

        let config_path = std::env::temp_dir().join(format!(
            "i2pr-irc-c042-{}-{streams}.toml",
            std::process::id()
        ));
        // The two disruptive faults are declared with `probability = 0.0` so they exist
        // and are addressable by id without ever being armed by accident. A scenario arms
        // one deliberately, through the same control plane an operator would use.
        let config = format!(
            r#"
version = 1
seed = 410042
[admin]
bind = "{admin}"
[[proxy]]
name = "{PROXY}"
listen = "{proxy}"
upstream = "{}"
seed = 410042
max_connections = 16
[[proxy.fault]]
id = "{BLACKHOLE}"
direction = "downstream"
type = "blackhole"
probability = 0.0
[[proxy.fault]]
id = "{DISCONNECT}"
direction = "upstream"
type = "disconnect"
delay = "0s"
# A hard reset, not a graceful close. A graceful termination of one direction leaves the
# client's socket half-open: the bouncer's writes vanish with no error and no EOF, so it
# waits in `registering` forever and the scenario reads as "recovery never happens" when
# nothing about recovery was ever tested. A reset is both what a hard disconnect means and
# what makes the failure observable to the peer.
hard_reset = true
probability = 0.0
"#,
            bridge.endpoint().socket_addr()
        );
        std::fs::write(&config_path, config).expect("temporary eggchaos config writes");
        let mut child = ChildGuard(
            Command::new(&binary)
                .arg("serve")
                .arg("--config")
                .arg(&config_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("pinned eggchaos starts"),
        );
        wait_admin(&binary, admin, &mut child.0).await;

        let bridge_endpoint = i2pr_irc_sam::SamBridgeEndpoint::parse(&proxy.to_string())
            .expect("loopback proxy address is a SAM bridge endpoint");
        let provider = Arc::new(SamProvider::with_config(SamClientConfig {
            bridge: bridge_endpoint,
            timeouts: SamTimeouts::default(),
            random: Arc::new(i2pr_irc_sam::session_id::OsRandom),
        }));
        let store = Store::open(&StorePath::Memory).expect("store opens");
        let handle = store.handle_clone();
        let (mut controller, control) = RuntimeController::new(Arc::clone(&provider), handle);
        let task = tokio::spawn(async move { controller.serve().await });
        let mut status = control.subscribe_status();
        tokio::time::timeout(CEILING, status.changed())
            .await
            .expect("controller starts")
            .expect("status watch remains open");
        control.create(record()).await.expect("Network creates");

        let peer = bridge.peer();
        let server = stand_up_server(Arc::clone(&peer));
        assert!(
            peer.wait_for(b"USER user", CEILING).await,
            "the SAM stream carries IRC registration through the proxy"
        );
        assert!(
            peer.wait_for(b"JOIN #room", CEILING).await,
            "registration and the desired join reconcile through the proxy"
        );
        Self {
            binary,
            admin,
            child,
            provider,
            peer,
            bridge,
            server,
            control,
            task,
            store,
            config_path,
        }
    }

    /// Arms or disarms one configured fault through the pinned admin control plane.
    ///
    /// Probability rather than redefinition is the mechanism, because the fault must keep
    /// its identity across the change: arming a *different* fault id would be a different
    /// fault, and the evidence would be about the wrong thing.
    fn set_fault(&self, id: &str, probability: f64) {
        let status = Command::new(&self.binary)
            .args([
                "--admin",
                &format!("http://{}", self.admin),
                "fault",
                "set",
                PROXY,
                id,
                "--probability",
                &format!("{probability}"),
            ])
            .status()
            .expect("eggchaos admin endpoint is reachable");
        assert!(
            status.success(),
            "eggchaos accepts {id} at probability {probability}"
        );
    }

    /// Disarms a fault and waits for the proxy to have applied it.
    ///
    /// The admin call returning is not the same instant as the data plane changing, and a
    /// replacement connection opened in between would be severed by the fault the run has
    /// just switched off — which reads as "recovery never happens" when the recovery was
    /// simply attempted too early. A short settle makes the scenario deterministic
    /// without weakening what it asserts.
    async fn heal(&self, id: &str) {
        self.set_fault(id, 0.0);
        sleep(Duration::from_secs(2)).await;
    }

    /// Waits until this Network's reported phase matches, or fails with the last phase.
    async fn wait_phase(&self, phase: &str) {
        let deadline = tokio::time::Instant::now() + RECOVERY_CEILING;
        let mut seen = String::from("none");
        loop {
            let snapshot = self
                .control
                .status()
                .await
                .expect("diagnostics answer while waiting");
            if let Some(network) = snapshot
                .networks
                .iter()
                .find(|entry| entry.network == NETWORK)
            {
                seen = network.phase.clone().unwrap_or_else(|| "none".to_owned());
                if seen == phase {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "phase {phase} was not reached; last observed {seen}\n\
                     provider: {:?}\nnetwork: {:?}\nbridge requests: {:?}\n\
                     irc streams: {:?}",
                    self.provider.diagnostics(),
                    self.network().await,
                    self.bridge.requests(),
                    self.peer.received_per_stream().await,
                );
            }
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the current generation stops being online, and returns promptly.
    ///
    /// This is the signal a disruptive scenario restores the transport on. It is
    /// deliberately *not* "wait for the next generation to appear": the catalog sleeps
    /// about a second in backoff before it tries again, so leaving a fault armed across
    /// that gap kills the replacement connection too, and one fault becomes an unbounded
    /// reconnect fight. Observing the loss and healing immediately is what a real
    /// transport does, and it keeps the stream-attempt count an honest record of the
    /// reconnects the claim actually needs rather than of its own fault.
    async fn wait_offline(&self, ceiling: Duration) {
        let deadline = tokio::time::Instant::now() + ceiling;
        let mut seen = String::from("none");
        loop {
            let snapshot = self
                .control
                .status()
                .await
                .expect("diagnostics answer while waiting");
            if let Some(network) = snapshot
                .networks
                .iter()
                .find(|entry| entry.network == NETWORK)
            {
                seen = network.phase.clone().unwrap_or_else(|| "none".to_owned());
                if seen != "online" {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the Network stayed online; last observed {seen}"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// The single Network's diagnostics row.
    async fn network(&self) -> i2pr_irc_runtime::diagnostics::NetworkDiagnostics {
        self.control
            .diagnostics(Some(NETWORK))
            .await
            .expect("diagnostics answer")
            .networks
            .first()
            .cloned()
            .expect("the Network is owned")
    }

    /// Waits until a stream opened *after* `before` has identified and rejoined.
    ///
    /// Compared by position rather than by content, because two registrations can be
    /// byte-identical and matching on content would let a stale stream satisfy the
    /// assertion. Streams are registered in arrival order, so anything past the original
    /// count is by definition a replacement connection.
    async fn wait_rejoined(&self, before: &[Vec<u8>]) -> bool {
        let deadline = tokio::time::Instant::now() + RECOVERY_CEILING;
        loop {
            let per_stream = self.peer.received_per_stream().await;
            let rejoined = per_stream.iter().skip(before.len()).any(|stream| {
                stream
                    .windows(b"USER user".len())
                    .any(|window| window == b"USER user")
                    && stream
                        .windows(b"JOIN #room".len())
                        .any(|window| window == b"JOIN #room")
            });
            if rejoined {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Everything the qualification must show returned to a resting state.
    async fn shutdown(self) {
        self.control.delete(NETWORK).await.expect("Network deletes");
        let release_deadline = tokio::time::Instant::now() + CEILING;
        while self.provider.diagnostics().live_scopes != 0 {
            assert!(
                tokio::time::Instant::now() < release_deadline,
                "the provider scope returns to baseline after deletion"
            );
            sleep(Duration::from_millis(20)).await;
        }
        self.server.abort();
        self.control.request_stop();
        tokio::time::timeout(CEILING, self.task)
            .await
            .expect("controller stops")
            .expect("controller joins")
            .expect("controller stops cleanly");
        self.store.shutdown().expect("store shuts down");
        drop(self.child);
        let _ = std::fs::remove_file(self.config_path);
    }
}

const NETWORK: NetworkId = NetworkId(41);

/// Keeps answering IRC registration for as long as the fixture lives.
///
/// The fake bridge is driven entirely by the test, so nothing answers a re-registration
/// unless the test does. After a disruptive fault every replacement generation has to be
/// answered again, and a run that answered only the first one would sit in `registering`
/// until it timed out — a failure that reads exactly like broken recovery but is really a
/// fixture that stopped being a server. A real upstream would have kept answering, which
/// is the whole premise of a recovery scenario: the disruption was the transport, not the
/// IRC server.
fn stand_up_server(peer: Arc<FakeIrcPeer>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // One greeting per registration, counted across streams. Polling "have I seen USER
        // yet?" against the retained log instead would match it forever and re-send
        // `CAP LS` several times a second, which is not a server being friendly -- it is a
        // server that re-arms CAP negotiation after every `001` and never finishes
        // registering.
        let mut greeted = 0usize;
        loop {
            // Pump first. The bridge has no background reader by design, so a stream is
            // only read when the test asks for it. Skipping this leaves the bouncer's
            // registration sitting unread in the kernel, and the scenario then times out
            // on a fixture that stopped reading rather than on a bouncer that stopped
            // talking.
            peer.recv(Duration::from_millis(200)).await;
            let registrations = peer
                .received_per_stream()
                .await
                .iter()
                .filter(|log| contains(log, b"USER user"))
                .count();
            if registrations > greeted {
                greeted = registrations;
                peer.send(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
                    .await;
            }
        }
    })
}

/// Reserves a loopback address, then releases it, returning the address eggchaos should
/// listen on.
///
/// A port chosen by binding and immediately closing is free for the next allocation too,
/// and the process that claims it next is usually one of these tests' own outbound
/// connections. eggchaos then cannot bind, and the failure surfaces much later as a
/// connection refusal against a fault nobody changed.
///
/// The bind itself goes through the fake bridge's own helper rather than a listener owned
/// here, because the network boundary scan permits exactly two files in the SAM crate to
/// name a socket and this is neither of them.
async fn reserve_addr() -> SocketAddr {
    i2pr_irc_sam::fake::absent_endpoint().await.socket_addr()
}

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

async fn wait_admin(binary: &std::ffi::OsStr, admin: SocketAddr, child: &mut Child) {
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if child
            .try_wait()
            .expect("eggchaos status is readable")
            .is_some()
        {
            panic!("eggchaos exited before binding its admin endpoint");
        }
        let endpoint = format!("http://{admin}");
        if Command::new(binary)
            .args(["--admin", &endpoint, "health"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "eggchaos admin endpoint did not start"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn record() -> NetworkRecord {
    NetworkRecord {
        network: NETWORK,
        display_name: "eggchaos-qualification".into(),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("I2P endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "qualification".into(),
        auto_away: false,
        // Keep-nick is deliberately off: this fixture qualifies transport recovery, and a
        // reclaim thread writing `NICK` on a schedule would add upstream traffic that has
        // nothing to do with the claim under test.
        keep_nick: false,
        sasl: None,
        desired_channels: vec![i2pr_irc_store::DesiredChannelRecord::at("#room", 0, false)],
    }
}

#[tokio::test]
#[ignore = "run only through the explicit pinned eggchaos qualification target"]
async fn production_sam_provider_registers_through_jitter_bandwidth_and_slicing() {
    let binary = std::env::var_os("EGGCHAOS_BIN").expect("qualification script sets EGGCHAOS_BIN");
    let bridge = FakeBridge::start(Script::healthy(1)).await;
    let proxy = reserve_addr().await;
    let admin = reserve_addr().await;
    let config_path =
        std::env::temp_dir().join(format!("i2pr-irc-m007-{}.toml", std::process::id()));
    let config = format!(
        r#"
version = 1
seed = 410041
[admin]
bind = "{admin}"
[[proxy]]
name = "{PROXY}"
listen = "{proxy}"
upstream = "{}"
seed = 410041
max_connections = 8
[[proxy.fault]]
id = "latency-jitter"
direction = "upstream"
type = "latency"
delay = "10ms"
jitter = "5ms"
[[proxy.fault]]
id = "bandwidth-cap"
direction = "upstream"
type = "bandwidth"
bytes_per_second = 262144
burst_bytes = 65536
[[proxy.fault]]
id = "slicing"
direction = "downstream"
type = "slice"
average_size = 7
variation = 3
"#,
        bridge.endpoint().socket_addr()
    );
    std::fs::write(&config_path, config).expect("temporary eggchaos config writes");
    let mut child = ChildGuard(
        Command::new(&binary)
            .arg("serve")
            .arg("--config")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("pinned eggchaos starts"),
    );
    wait_admin(&binary, admin, &mut child.0).await;

    let bridge_endpoint = i2pr_irc_sam::SamBridgeEndpoint::parse(&proxy.to_string())
        .expect("loopback proxy address is a SAM bridge endpoint");
    let provider = Arc::new(SamProvider::with_config(SamClientConfig {
        bridge: bridge_endpoint,
        timeouts: SamTimeouts::default(),
        random: Arc::new(i2pr_irc_sam::session_id::OsRandom),
    }));
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    let (mut controller, control) = RuntimeController::new(Arc::clone(&provider), handle);
    let task = tokio::spawn(async move { controller.serve().await });
    let mut status = control.subscribe_status();
    tokio::time::timeout(CEILING, status.changed())
        .await
        .expect("controller starts")
        .expect("status watch remains open");
    control.create(record()).await.expect("Network creates");

    let peer = bridge.peer();
    assert!(
        peer.wait_for(b"USER user", CEILING).await,
        "SAM stream carries IRC registration"
    );
    peer.send(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await;
    assert!(
        peer.wait_for(b"JOIN #room", CEILING).await,
        "registration and desired join survive the proxy"
    );
    let initial_stream_attempts = provider.diagnostics().stream_attempts;
    sleep(Duration::from_secs(20)).await;
    let snapshot = control
        .status()
        .await
        .expect("diagnostics answer after stable baseline");
    assert!(
        snapshot
            .networks
            .iter()
            .any(|network| network.network == NETWORK && network.live)
    );
    assert_eq!(
        provider.diagnostics().stream_attempts,
        initial_stream_attempts,
        "the stable baseline does not replace the upstream generation"
    );
    let admin_url = format!("http://{admin}");
    let high_latency = Command::new(&binary)
        .args([
            "--admin",
            &admin_url,
            "fault",
            "set",
            PROXY,
            "latency-jitter",
            "--kind",
            "latency",
            "--delay-ms",
            "50",
            "--jitter-ms",
            "20",
        ])
        .status()
        .expect("eggchaos applies the elevated latency profile");
    assert!(
        high_latency.success(),
        "eggchaos accepts the elevated profile"
    );
    sleep(Duration::from_secs(90)).await;
    let snapshot = control
        .status()
        .await
        .expect("diagnostics answer after elevated latency");
    assert!(
        snapshot
            .networks
            .iter()
            .any(|network| network.network == NETWORK && network.live),
        "bounded latency and jitter do not cause a false reconnect"
    );
    assert_eq!(
        provider.diagnostics().stream_attempts,
        initial_stream_attempts,
        "elevated latency does not replace the upstream generation"
    );
    let normal_latency = Command::new(&binary)
        .args([
            "--admin",
            &admin_url,
            "fault",
            "set",
            PROXY,
            "latency-jitter",
            "--kind",
            "latency",
            "--delay-ms",
            "10",
            "--jitter-ms",
            "5",
        ])
        .status()
        .expect("eggchaos restores baseline latency");
    assert!(
        normal_latency.success(),
        "eggchaos restores baseline profile"
    );
    assert_eq!(peer.refused(), 0);
    let peak_provider = provider.diagnostics();
    assert_eq!(peak_provider.live_scopes, 1);
    assert_eq!(peak_provider.healthy_scopes, 1);
    assert_eq!(peak_provider.session_creations, 1);

    control.delete(NETWORK).await.expect("Network deletes");
    let release_deadline = tokio::time::Instant::now() + CEILING;
    while provider.diagnostics().live_scopes != 0 {
        assert!(
            tokio::time::Instant::now() < release_deadline,
            "SAM scope returns to baseline after deletion"
        );
        sleep(Duration::from_millis(20)).await;
    }
    control.request_stop();
    tokio::time::timeout(CEILING, task)
        .await
        .expect("controller stops")
        .expect("controller joins")
        .expect("controller stops cleanly");
    store.shutdown().expect("store shuts down");
    let _ = std::fs::remove_file(config_path);
    println!(
        "PASS product-path latency/bandwidth/slicing: stream_attempts={initial_stream_attempts} \
         live_scopes=1 healthy_scopes=1 session_creations=1"
    );
}

#[tokio::test]
#[ignore = "run only through the explicit pinned eggchaos qualification target"]
async fn a_blackholed_generation_ends_under_liveness_and_recovers_through_the_product_path() {
    // Scenario A. A silent drop on the way back from the server is the failure this whole
    // qualification exists for, because it produces no EOF, no RST, and no error: the
    // socket stays nominally open forever. The claim under test is that the bouncer
    // notices on a deadline and comes back, rather than sitting on a dead socket.
    let path = ProductPath::start(3).await;
    let baseline = path.network().await;
    assert_eq!(
        baseline.generation,
        Some(1),
        "the first generation is online"
    );
    let streams_before = path.peer.received_per_stream().await;
    assert_eq!(
        baseline.sessions_attached, 0,
        "this path has no accepted-stream boundary, so no session is attached"
    );
    let attempts_before = path.provider.diagnostics().stream_attempts;

    // Silence, not death. A quiet connection must not be mistaken for a dead one: the
    // generation has to survive a bounded observation window untouched, or every healthy
    // but idle Network would be reconnecting forever.
    path.set_fault(BLACKHOLE, 1.0);
    sleep(Duration::from_secs(20)).await;
    let quiet = path.network().await;
    assert_eq!(
        quiet.generation,
        Some(1),
        "a silent transport is not a dead transport: no premature generation loss"
    );
    assert_eq!(
        path.provider.diagnostics().stream_attempts,
        attempts_before,
        "and no premature reconnect attempt either"
    );

    // The deterministic ceiling. With no EOF coming, the liveness deadline is the only
    // thing that can end this generation, so the test waits for it rather than papering
    // over it with a forced close.
    path.wait_offline(LIVENESS_CEILING).await;
    println!("PASS product-path blackhole: liveness ended the generation");

    // Restore the transport the moment the loss is observed, which is ahead of the first
    // reconnect attempt rather than racing it.
    path.heal(BLACKHOLE).await;
    path.wait_phase("online").await;

    // The replacement generation really re-established on a *new* stream, and really
    // re-identified and rejoined rather than assuming the previous connection still held
    // the channel. The standing server answers it; this checks that it got there.
    assert!(
        path.wait_rejoined(&streams_before).await,
        "durable desired channel state reconciles on the replacement generation"
    );

    // The durable Network is the same one. A router session may have been rebuilt, but
    // the Network's identity, policy, and single live owner are unchanged, which is what
    // keeps a reconnect from becoming a second bouncer on the same nick.
    let recovered = path.network().await;
    assert!(
        recovered
            .generation
            .is_some_and(|generation| generation >= 2),
        "the Network is served by a later generation than the one that died"
    );
    assert_eq!(
        recovered.nick.as_deref(),
        Some("bot"),
        "the same durable nick"
    );
    assert_eq!(
        recovered.preferred_nick.as_deref(),
        Some("bot"),
        "the same preferred nick and policy"
    );
    assert_eq!(
        recovered.sessions_attached, 0,
        "no session was attached or invented"
    );
    assert_eq!(
        recovered.response_routes, 0,
        "no response route outlived its generation"
    );
    assert_eq!(
        recovered.desired_reconcile_pending, 0,
        "desired channel intent is not left pending"
    );
    let provider = path.provider.diagnostics();
    assert_eq!(provider.live_scopes, 1, "exactly one live provider scope");
    assert_eq!(provider.healthy_scopes, 1, "and it is healthy again");
    assert_eq!(
        provider.queue_refusals, 0,
        "recovery never stampedes the bounded request queue"
    );
    assert!(
        provider.stream_attempts > attempts_before,
        "the provider really opened a new stream: {attempts_before} -> {}",
        provider.stream_attempts
    );
    assert_eq!(path.peer.refused(), 0, "the bridge refused no stream");
    println!(
        "PASS product-path blackhole: generation={} session_creations={} \
         stream_attempts={attempts_before}->{} queue_refusals=0 live_scopes=1",
        recovered.generation.unwrap_or_default(),
        provider.session_creations,
        provider.stream_attempts,
    );
    path.shutdown().await;
}

#[tokio::test]
#[ignore = "run only through the explicit pinned eggchaos qualification target"]
async fn a_hard_disconnect_replaces_the_generation_and_recovers_through_the_product_path() {
    // Scenario B. Unlike the blackhole this produces an immediate EOF on the real socket,
    // so it exercises the fast path: generation loss observed, bounded backoff, a new
    // generation, and reconciled state.
    let path = ProductPath::start(3).await;
    let before = path.network().await;
    assert_eq!(before.generation, Some(1));
    let streams_before = path.peer.received_per_stream().await;
    let provider_before = path.provider.diagnostics();
    assert_eq!(
        provider_before.session_creations, 1,
        "one router session so far"
    );

    path.set_fault(DISCONNECT, 1.0);
    path.wait_offline(RECOVERY_CEILING).await;
    path.heal(DISCONNECT).await;
    path.wait_phase("online").await;

    assert!(
        path.wait_rejoined(&streams_before).await,
        "the replacement generation identifies and rejoins on a new stream"
    );

    let recovered = path.network().await;
    assert_eq!(recovered.generation, Some(2));
    assert_eq!(recovered.nick.as_deref(), Some("bot"));
    assert_eq!(recovered.sessions_attached, 0);
    assert_eq!(recovered.response_routes, 0);
    assert_eq!(recovered.desired_reconcile_pending, 0);

    // A hard close takes the SAM control connection down with the IRC one, so the provider
    // is *expected* to re-identify with the router: refusing to rebuild a session that no
    // longer exists would strand the Network forever. What must not change is the durable
    // Network behind it — one live scope, same nick, same policy, same desired channels —
    // because that is what keeps a reconnect from becoming a second bouncer on one nick.
    //
    // That the *SAM session* alone survives a pure IRC reconnect is a separate, narrower
    // claim. It cannot be qualified here, because a fault on this proxy necessarily
    // reaches the control connection too; it is covered in-process in
    // `r001c_sam_core_integration`, where only the IRC stream can be dropped.
    let provider_after = path.provider.diagnostics();
    assert_eq!(
        provider_after.live_scopes, 1,
        "the router session was rebuilt inside the same Network scope"
    );
    assert_eq!(
        provider_after.healthy_scopes, 1,
        "and the scope is healthy again"
    );
    assert_eq!(
        provider_after.queue_refusals, 0,
        "recovery never stampedes the bounded request queue"
    );
    assert!(
        provider_after.stream_successes > provider_before.stream_successes,
        "a new stream really was opened: {} -> {}",
        provider_before.stream_successes,
        provider_after.stream_successes
    );
    println!(
        "PASS product-path disconnect: generation=2 session_creations={} \
         session_losses={} stream_attempts={} stream_failures={} live_scopes=1",
        provider_after.session_creations,
        provider_after.session_losses,
        provider_after.stream_attempts,
        provider_after.stream_failures,
    );
    path.shutdown().await;
}

#[tokio::test]
#[ignore = "run only through the explicit pinned eggchaos qualification target"]
async fn a_disruptive_recovery_replays_no_ambiguous_user_traffic() {
    // The property that makes reconnect recovery safe rather than merely successful: a
    // disconnect can happen after an outbound command was written and before the server
    // confirmed it, so the bouncer cannot know whether the server saw it. Replaying that
    // frame would double a user's message. Re-registration is not replay, because the new
    // connection genuinely needs to identify and join again.
    let path = ProductPath::start(4).await;
    let per_stream_before = path.peer.received_per_stream().await;
    assert!(
        !per_stream_before.is_empty(),
        "there is an upstream stream to disrupt"
    );

    path.set_fault(DISCONNECT, 1.0);
    path.wait_offline(RECOVERY_CEILING).await;
    path.heal(DISCONNECT).await;
    path.wait_phase("online").await;
    assert!(
        path.wait_rejoined(&per_stream_before).await,
        "the replacement generation identifies and rejoins on a new stream"
    );
    // Let the replacement finish speaking, so a late frame cannot hide behind the wait.
    sleep(Duration::from_millis(300)).await;

    let per_stream = path.peer.received_per_stream().await;
    assert!(
        per_stream.len() >= 2,
        "a replacement stream exists: {per_stream:?}"
    );
    let replacement = per_stream
        .iter()
        .find(|bytes| !per_stream_before.contains(bytes))
        .expect("a stream the first generation did not produce");
    for frame in replacement
        .split(|byte| *byte == b'\n')
        .map(|frame| String::from_utf8_lossy(frame))
        .map(|frame| frame.trim_end_matches('\r').to_owned())
        .filter(|frame| !frame.is_empty())
    {
        assert!(
            frame.starts_with("NICK ")
                || frame.starts_with("USER ")
                || frame.starts_with("CAP ")
                || frame.starts_with("JOIN ")
                // The generation's own liveness probe. A fresh connection is expected to
                // send one, and it carries nothing a client ever wrote: allowing it is not
                // a relaxation of the claim, it is the difference between a frame the
                // bouncer generates and a frame a user would have to have typed.
                || frame.starts_with("PING "),
            "only re-identification, desired membership, and liveness may be written on a \
             fresh connection; {frame:?} would be a replay across generations"
        );
    }
    println!("PASS product-path replay disposition: no ambiguous user frame replayed");
    path.shutdown().await;
}
