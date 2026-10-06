//! Plan 015 qualification: the anonymity protocol mediation boundary.
//!
//! Everything here runs against fake I2P providers and scripted local streams. No test
//! may require a real listener, a real router, or any network authority.
//!
//! The claims this file defends are privacy claims, so they are stated as the thing that
//! must never happen: an attached client must not be able to learn about, or be prompted
//! to disclose, anything about this Operator beyond what it deliberately asked for.
use i2pr_irc_core::{ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    RuntimeError,
    catalog::{NetworkCatalog, SupervisorCommand, SupervisorContext, SupervisorHandle},
    ctcp::{CtcpDirection, OutboundAction, classify, outbound_action, parse_body},
    owner::{NetworkOwner, Phase},
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath, fallback_display_name};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
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
        sasl: None,
        desired_channels: channels.iter().map(|value| (*value).to_owned()).collect(),
    }
}

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

struct Harness {
    provider: Arc<FakeI2pStreamProvider>,
    commands: mpsc::Sender<SupervisorCommand>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    snapshot: watch::Receiver<i2pr_irc_runtime::owner::NetworkSnapshot>,
    upstream: Option<ScriptedStream>,
}

impl Harness {
    async fn start(store: StoreHandle) -> Self {
        Self::start_with_caps(store, "").await
    }

    /// `labels` is the upstream capability offer, so a test can choose whether the
    /// bouncer negotiates the label surface.
    async fn start_with_caps(store: StoreHandle, labels: &str) -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..4 {
            provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        }
        let reconnect = ReconnectScheduler::default();
        let context = SupervisorContext {
            network: NetworkId(1),
            record: Arc::new(record(1, "bot", &[])),
            store: store.clone(),
            status: watch::channel(Default::default()).0,
            resources: ResourceLedger::new(reconnect.clone(), store.clone()),
        };
        let owner = NetworkOwner::new(Shared(provider.clone()), context, store, reconnect)
            .expect("owner constructs");
        let snapshot = owner.subscribe_snapshot();
        let (command_tx, command_rx) = mpsc::channel(64);
        let _handle = SupervisorHandle::new(NetworkId(1), command_tx.clone());
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(async move { owner.serve(command_rx, stop_rx).await });
        let mut harness = Self {
            provider,
            commands: command_tx,
            stop,
            task,
            snapshot,
            upstream: None,
        };
        let mut upstream = harness.provider.take_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        if labels.is_empty() {
            upstream
                .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
                .await
                .unwrap();
        } else {
            upstream
                .write_all(format!(":srv CAP * LS :{labels}\r\n").as_bytes())
                .await
                .unwrap();
            read_until(&mut upstream, b"CAP REQ").await;
            upstream
                .write_all(format!(":srv CAP * ACK :{labels}\r\n").as_bytes())
                .await
                .unwrap();
            read_until(&mut upstream, b"CAP END").await;
            upstream
                .write_all(b":srv 001 bot :welcome\r\n")
                .await
                .unwrap();
        }
        harness.upstream = Some(upstream);
        harness
    }

    fn upstream(&mut self) -> &mut ScriptedStream {
        self.upstream.as_mut().expect("online")
    }

    async fn wait_phase(&mut self, phase: Phase) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.snapshot.borrow().phase != Some(phase) {
                self.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never reached {phase:?}"));
    }

    async fn wait_attached(&mut self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.snapshot.borrow().attached_sessions != count {
                self.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never reached {count} attached"));
    }

    async fn attach(&self, client: ClientId) -> tokio::io::DuplexStream {
        let (client_side, peer) = tokio::io::duplex(64 * 1024);
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Attach {
                session: SessionId(client.0),
                client,
                stream: Box::new(client_side),
                reply,
            })
            .expect("attach fits");
        response.await.expect("answered").expect("accepted");
        peer
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(
                count > 0,
                "stream ended waiting for {}",
                String::from_utf8_lossy(needle)
            );
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .unwrap_or_else(|_| panic!("expected {}", String::from_utf8_lossy(needle)));
    String::from_utf8_lossy(&all).into_owned()
}

async fn read_client_until(stream: &mut tokio::io::DuplexStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "client stream ended");
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .unwrap_or_else(|_| panic!("client never received {}", String::from_utf8_lossy(needle)));
    String::from_utf8_lossy(&all).into_owned()
}

/// Reads whatever the upstream peer has buffered, without blocking indefinitely.
///
/// Used where the assertion is about what must *not* have reached the server.
async fn drain_upstream(stream: &mut ScriptedStream) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
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

/// Reads whatever a client has buffered without blocking indefinitely.
///
/// Used where the assertion is about what a client must *not* receive.
async fn drain(stream: &mut tokio::io::DuplexStream) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
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

async fn register(client: &mut tokio::io::DuplexStream) -> String {
    client
        .write_all(b"NICK bot\r\nUSER bot 0 * :phone\r\n")
        .await
        .unwrap();
    read_client_until(client, b"001 ").await
}

/// One upstream DCC probe, written as a byte string so the delimiter is unambiguous.
const DCC_PROBE: &[u8] = b":probe PRIVMSG bot :\x01DCC CHAT chat 192 168 0 1 6667\x01\r\n";

// ------------------------------------------------------- upstream -> downstream

/// Every upstream metadata probe must be suppressed, and none may reach the client.
#[tokio::test]
async fn an_upstream_metadata_probe_never_reaches_a_client() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    for probe in [
        "VERSION",
        "TIME",
        "USERINFO",
        "SOURCE",
        "FINGER",
        "CLIENTINFO",
        "SOMETHINGELSE",
    ] {
        let before = harness.snapshot.borrow().ctcp_suppressed;
        harness
            .upstream()
            .write_all(format!(":probe PRIVMSG bot :\x01{probe}\x01\r\n").as_bytes())
            .await
            .unwrap();
        // Wait for the frame to be counted as suppressed before asserting.
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.snapshot.borrow().ctcp_suppressed <= before {
                harness.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{probe} was not suppressed"));
    }
    let seen = drain(&mut client).await;
    assert!(
        !seen.contains("VERSION") && !seen.contains("CLIENTINFO") && !seen.contains('\u{1}'),
        "a metadata probe must never be shown to a client: {seen}"
    );
}

/// An upstream DCC request must be suppressed and must never become actionable.
#[tokio::test]
async fn an_upstream_dcc_request_is_suppressed_entirely() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    let before = harness.snapshot.borrow().ctcp_suppressed;
    harness.upstream().write_all(DCC_PROBE).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.snapshot.borrow().ctcp_suppressed <= before {
            harness.snapshot.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("DCC was not suppressed"));
    let seen = drain(&mut client).await;
    assert!(
        !seen.contains("DCC") && !seen.contains("6667") && !seen.contains('\u{1}'),
        "no part of a DCC request may reach a client: {seen}"
    );
}

/// An upstream PING is answered by the bouncer, and the client never sees it.
#[tokio::test]
async fn an_upstream_ctcp_ping_is_answered_by_the_bouncer_itself() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    harness
        .upstream()
        .write_all(b":probe PRIVMSG bot :\x01PING token-123\x01\r\n")
        .await
        .unwrap();
    let answered = read_until(harness.upstream(), b"PING token-123").await;
    let text = answered.as_str();
    assert!(
        text.contains(":bouncer NOTICE probe"),
        "the reply must go back to the probe's sender: {text}"
    );
    let seen = drain(&mut client).await;
    assert!(
        !seen.contains('\u{1}'),
        "a client must never be handed a CTCP probe to answer: {seen}"
    );
}

/// ACTION stays ordinary chat in both directions.
#[tokio::test]
async fn an_action_is_ordinary_chat_in_both_directions() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    // Upstream to client.
    harness
        .upstream()
        .write_all(b":alice PRIVMSG #room :\x01ACTION waves\x01\r\n")
        .await
        .unwrap();
    let seen = read_client_until(&mut client, b"ACTION waves").await;
    assert!(seen.contains("ACTION waves"), "{seen}");

    // Client to upstream.
    client
        .write_all(b"PRIVMSG #room :\x01ACTION replies\x01\r\n")
        .await
        .unwrap();
    let upstream = read_until(harness.upstream(), b"ACTION replies").await;
    assert!(
        upstream.as_str().contains("ACTION replies"),
        "an action must reach upstream"
    );
}

// ------------------------------------------------------- downstream -> upstream

/// A client metadata reply must never reach the upstream server.
#[tokio::test]
async fn a_client_metadata_reply_never_reaches_the_upstream_server() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    for reply in [
        "VERSION MyCoolClient 3.2",
        "TIME",
        "USERINFO secret person",
        "SOURCE secret@example.i2p",
        "FINGER",
        "CLIENTINFO",
        "SOMETHING 1 2",
    ] {
        let before = harness.snapshot.borrow().client_frames_blocked;
        client
            .write_all(format!("NOTICE probe :\x01{reply}\x01\r\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.snapshot.borrow().client_frames_blocked <= before {
                harness.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{reply} was not blocked"));
    }
    // Drain upstream: nothing at all may have been written.
    let upstream = drain_upstream(harness.upstream()).await;
    for leak in ["MyCoolClient", "secret", "SOURCE", "CLIENTINFO", "\u{1}"] {
        assert!(
            !upstream.contains(leak),
            "{leak:?} must never reach upstream: {upstream}"
        );
    }
}

/// DCC from a client is blocked in every shape.
#[tokio::test]
async fn a_client_dcc_request_is_blocked_in_every_shape() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    for body in [
        "DCC CHAT chat 192 168 0 1 6667",
        "DCC SEND file 192 168 0 1 6667",
        "DCC RESUME 12345",
        "DCC ACCEPT 192 168 0 1",
    ] {
        let before = harness.snapshot.borrow().client_frames_blocked;
        client
            .write_all(format!("PRIVMSG probe :\x01{body}\x01\r\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.snapshot.borrow().client_frames_blocked <= before {
                harness.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{body} was not blocked"));
    }
    let upstream = drain_upstream(harness.upstream()).await;
    assert!(
        !upstream.contains("DCC") && !upstream.contains('\u{1}'),
        "no DCC may reach upstream: {upstream}"
    );
}

// ----------------------------------------------------------------- tag policy

/// Client tags are denied by default, and the label is the only exception.
#[tokio::test]
async fn client_tags_are_denied_except_the_bouncers_own_label() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    client
        .write_all(
            b"@msgid=spoofed;time=2023-11-14T22:13:20.000Z;+typing=active;+vendor/x=1 PRIVMSG #room :hi\r\n",
        )
        .await
        .unwrap();
    let upstream = read_until(harness.upstream(), b"PRIVMSG #room :hi").await;
    let text = upstream.as_str();
    assert!(text.contains("PRIVMSG #room :hi"), "{text}");
    for forbidden in [
        "spoofed",
        "+typing",
        "+vendor/x",
        "2023-11-14",
        "@msgid",
        "@time",
        "@+",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden:?} must never reach upstream: {text}"
        );
    }
}

/// A flood of denied tags must stay inside the bounded queue and never end the client.
///
/// The point is not that the queue is empty -- the owner drains asynchronously -- but
/// that a client cannot use tag pressure to grow the bouncer's own state, and cannot
/// make itself disappear as a side effect.
#[tokio::test]
async fn a_flood_of_denied_tags_costs_no_extra_queue_capacity() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register(&mut client).await;

    let mut peak = 0usize;
    for round in 0..i2pr_irc_runtime::NORMAL_QUEUE_CAPACITY {
        client
            .write_all(format!("@+a=1;+b=2;+c=3;+d=4 PRIVMSG #room :flood{round}\r\n").as_bytes())
            .await
            .unwrap();
        peak = peak.max(harness.snapshot.borrow().upstream_normal_queue_depth);
    }

    // Every frame still arrives, so the flood was stripped and forwarded rather than
    // swallowed or turned into an error.
    let last = format!(
        "PRIVMSG #room :flood{}",
        i2pr_irc_runtime::NORMAL_QUEUE_CAPACITY - 1
    );
    read_until(harness.upstream(), last.as_bytes()).await;

    assert!(
        peak <= i2pr_irc_runtime::NORMAL_QUEUE_CAPACITY,
        "tag pressure must stay inside the bounded queue, saw {peak}"
    );
    assert_eq!(
        harness.snapshot.borrow().attached_sessions,
        1,
        "a client must not be able to detach itself with denied tags"
    );
    assert_eq!(harness.snapshot.borrow().client_frames_blocked, 0);
}

// ------------------------------------------------- fingerprint / environment

/// The upstream-visible registration must not depend on which client is attached.
#[tokio::test]
async fn the_upstream_registration_is_identical_for_every_client_mix() {
    let (_store, handle) = store();

    async fn registration_bytes(store: StoreHandle, attach: &[ClientId], caps: &[&str]) -> String {
        let mut harness =
            Harness::start_with_caps(store, "message-tags server-time batch labeled-response")
                .await;
        harness.wait_phase(Phase::Online).await;
        let mut clients = Vec::new();
        for (index, client_id) in attach.iter().enumerate() {
            let mut client = harness.attach(*client_id).await;
            harness.wait_attached(index + 1).await;
            if caps.is_empty() {
                register(&mut client).await;
            } else {
                client
                    .write_all(b"CAP LS 302\r\nCAP END\r\nNICK bot\r\nUSER bot 0 * :phone\r\n")
                    .await
                    .unwrap();
                read_client_until(&mut client, b"001 ").await;
            }
            clients.push(client);
        }
        // Everything the bouncer wrote upstream for this generation.
        drain_upstream(harness.upstream()).await
    }

    let none = registration_bytes(handle.clone(), &[], &[]).await;
    let one_legacy = registration_bytes(handle.clone(), &[ClientId(1)], &[]).await;
    let one_tagged = registration_bytes(handle.clone(), &[ClientId(1)], &["message-tags"]).await;
    let many = registration_bytes(
        handle.clone(),
        &[ClientId(1), ClientId(2), ClientId(3)],
        &["labeled-response"],
    )
    .await;

    assert!(
        !none.is_empty(),
        "the bouncer must have registered upstream"
    );
    for other in [&one_legacy, &one_tagged, &many] {
        assert_eq!(
            none, *other,
            "attached clients must not change what the bouncer says upstream"
        );
    }
}

/// No host environment value may appear in any IRC-visible field or diagnostic.
///
/// The workspace forbids `unsafe`, so the environment cannot be poisoned in-process.
/// Reading the *real* host values is the stronger check anyway: these are the exact
/// strings a leak would expose, and they are unique to this machine, so a match is
/// attributable rather than merely suspicious.
#[tokio::test]
async fn no_host_environment_value_reaches_the_wire_or_a_diagnostic() {
    let mut secrets: Vec<String> = Vec::new();
    for key in ["USER", "LOGNAME", "HOSTNAME", "HOME", "TMPDIR", "PWD"] {
        if let Ok(value) = std::env::var(key)
            && value.len() >= 4
            && value != "/"
        {
            secrets.push(value);
        }
    }
    assert!(
        !secrets.is_empty(),
        "the host environment must expose something to look for"
    );

    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    let registration = register(&mut client).await;

    // Exercise a CTCP probe, a routed query, and the diagnostics those produce.
    harness
        .upstream()
        .write_all(b":probe PRIVMSG bot :\x01CLIENTINFO\x01\r\n")
        .await
        .unwrap();
    client.write_all(b"WHOIS alice\r\n").await.unwrap();
    read_until(harness.upstream(), b"WHOIS alice").await;
    harness
        .upstream()
        .write_all(b":srv 318 bot alice :End of /WHOIS list.\r\n")
        .await
        .unwrap();

    let upstream = drain_upstream(harness.upstream()).await;
    let downstream = drain(&mut client).await;
    let snapshot = format!("{:?}", harness.snapshot.borrow());

    for (surface, label) in [
        (registration.clone(), "registration"),
        (upstream, "upstream bytes"),
        (downstream, "downstream replies"),
        (snapshot, "structured diagnostics"),
    ] {
        for secret in &secrets {
            assert!(
                !surface.contains(secret.as_str()),
                "a host environment value leaked into {label}"
            );
        }
    }
}

/// A SASL secret must never be rendered, even in a diagnostic.
#[tokio::test]
async fn a_sasl_secret_never_reaches_a_diagnostic() {
    let (_store, handle) = store();
    let mut with_secret = record(1, "bot", &[]);
    with_secret.sasl = Some((
        "bot".into(),
        i2pr_irc_store::StoredSecret::new("s3cr3t-value".into()),
    ));
    handle.save_network(&with_secret).await.expect("saved");

    let catalog = NetworkCatalog::new(handle);
    let restored = catalog.load_desired_state().await.expect("loads");
    assert!(
        !format!("{restored:?}").contains("s3cr3t-value"),
        "restored configuration must not render a credential"
    );
}

/// The static boundary guard must hold, including its own positive controls.
///
/// This asserts the guard rather than restating its token list: the guard is the single
/// authority, and a test that duplicated its words would drift from it. The guard also
/// runs `positive_control_failures()`, so a coverage regression in the guard itself
/// fails here rather than silently narrowing the boundary.
#[test]
fn the_static_network_boundary_guard_passes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let script = root.join("scripts/check-network-boundary.py");
    assert!(script.exists(), "the boundary guard must exist");

    let output = std::process::Command::new("python3")
        .arg(&script)
        .current_dir(root)
        .output()
        .expect("the boundary guard runs");
    assert!(
        output.status.success(),
        "the network boundary guard failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ------------------------------------------------------------------- policy

/// The pure policy is what the live tests above are asserting about.
#[test]
fn the_directional_policy_is_deny_by_default() {
    let blocked = [
        ("\x01VERSION\x01", CtcpDirection::Query),
        ("\x01VERSION x\x01", CtcpDirection::Reply),
        ("\x01DCC CHAT chat 1 2 3\x01", CtcpDirection::Query),
        ("\x01DCC SEND f 1 2 3\x01", CtcpDirection::Reply),
        ("\x01UNKNOWN 1\x01", CtcpDirection::Reply),
    ];
    for (body, direction) in blocked {
        let parsed = parse_body(body.as_bytes(), direction);
        assert_eq!(
            outbound_action(&parsed),
            OutboundAction::Block,
            "{body} must be blocked outbound"
        );
    }
    let allowed = [
        ("\x01ACTION waves\x01", CtcpDirection::Query),
        ("\x01PING 1\x01", CtcpDirection::Query),
        ("\x01PING 1\x01", CtcpDirection::Reply),
    ];
    for (body, direction) in allowed {
        let parsed = parse_body(body.as_bytes(), direction);
        assert_eq!(
            outbound_action(&parsed),
            OutboundAction::Forward,
            "{body} must be allowed outbound"
        );
    }
}

/// A NOTICE never becomes an actionable query, whatever it contains.
#[test]
fn a_notice_never_becomes_an_actionable_query() {
    let message =
        i2pr_irc_wire::Message::parse(b":a!b@c NOTICE bot :\x01PING x\x01\r\n").expect("parses");
    assert!(matches!(
        classify(&message, CtcpDirection::Reply),
        i2pr_irc_runtime::ctcp::Ctcp::Reply { .. }
    ));
    assert_eq!(
        i2pr_irc_runtime::ctcp::inbound_action(&classify(&message, CtcpDirection::Reply)),
        i2pr_irc_runtime::ctcp::InboundAction::Suppress,
        "an unsolicited PING reply must not make the bouncer answer"
    );
}

/// The capability set served downstream is exactly the history drafts plus tags.
#[test]
fn the_advertised_set_is_the_served_set() {
    let served: BTreeSet<&str> = i2pr_irc_runtime::downstream::downstream_supported()
        .iter()
        .copied()
        .collect();
    for capability in [
        "message-tags",
        "batch",
        "labeled-response",
        "draft/chathistory",
        "draft/read-marker",
    ] {
        assert!(served.contains(capability), "{capability} must be served");
    }
    for withheld in ["server-time", "echo-message", "away-notify", "sasl"] {
        assert!(
            !served.contains(withheld),
            "{withheld} must not be advertised"
        );
    }
}
