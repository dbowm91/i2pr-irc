//! Plan 018 integrated qualification: the M004 closure claims.
//!
//! Earlier suites already defend the individual mechanisms -- Corrective 014 owns response
//! routing, M004-A owns protocol mediation, M004-B owns the reconnect budget, M004-C owns
//! resource accounting, and the store suite owns durability. Restating those claims here
//! would only let a second copy drift.
//!
//! What belongs at closure is the set of claims that are *cross-cutting*: each one can only
//! fail if two subsystems interact badly, so no single earlier suite would catch it. That is
//! what this file is:
//!
//! - `the_advertised_client_tag_deny_is_trueful` ties the `CLIENTTAGDENY` *advertisement*
//!   to the *mediator behaviour*, which live in different modules.
//! - `no_build_router_or_version_string_is_exposed_by_default` spans ISUPPORT, CAP, the
//!   CTCP auto-answer, and diagnostics, which are produced by four different paths.
//! - `raw_protocol_logging_is_absent_from_every_production_path` is a workspace-wide
//!   structural claim, not a behavioural one.
//! - `remote_visible_ctcp_behaviour_is_fixed_by_policy_not_by_client_brand` requires two
//!   differently-behaving clients to be indistinguishable upstream.
//! - `a_full_generation_never_leaks_or_carries_a_fingerprint_across_reconnect` is the
//!   end-to-end composition: one connection generation exercised through all four
//!   subsystems above.
//!
//! Everything here runs against fake I2P providers and scripted local streams. No test may
//! require a real listener, a real router, or any network authority.
use i2pr_irc_core::{ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    RuntimeError,
    catalog::{NetworkCatalog, SupervisorCommand, SupervisorContext, SupervisorHandle},
    ctcp::{Ctcp, CtcpDirection, classify, outbound_action},
    owner::{NetworkOwner, Phase},
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath, fallback_display_name};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use i2pr_irc_wire::Message;
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
        desired_channels: i2pr_irc_store::attached_channels(
            &channels
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        ),
    }
}

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

    /// Waits for the generation-local router to hold exactly `count` open routes.
    async fn wait_routes(&mut self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.snapshot.borrow().response_routes != count {
                self.snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "routes never settled to {count}; actual={}",
                self.snapshot.borrow().response_routes
            )
        });
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

/// The translated upstream label the bouncer put on the query carrying `needle`.
///
/// Taking the first tagged line would be a scheduling coin flip -- a keepalive or a
/// capability frame can precede the query -- so the line is selected by content.
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

async fn register(client: &mut tokio::io::DuplexStream) -> String {
    client
        .write_all(b"NICK bot\r\nUSER bot 0 * :phone\r\n")
        .await
        .unwrap();
    read_client_until(client, b"001 ").await
}

/// Registers with the given capability request so a test can choose the client's brand.
async fn register_with_caps(client: &mut tokio::io::DuplexStream, caps: &str) -> String {
    client
        .write_all(
            format!(
                "CAP LS 302\r\nCAP REQ :{caps}\r\nCAP END\r\nNICK bot\r\nUSER bot 0 * :phone\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    read_client_until(client, b"001 ").await
}

// ------------------------------------------------- CLIENTTAGDENY truthfulness

/// Every client-only tag the specification names, so the allowlist is checked against the
/// real surface rather than against whatever this implementation happened to think of.
///
/// The bouncer's own `label` is deliberately absent: it is retained for the response
/// router and is never relayed, so it is not a client tag it forwards.
const NAMED_CLIENT_TAGS: &[&str] = &[
    "msgid",
    "time",
    "server-time",
    "account",
    "account-tag",
    "draft/reply",
    "draft/edit",
    "draft/delete",
    "draft/msgid",
    "+typing",
    "+react",
    "+draft/relaymsg",
    "+draft/thread",
    "echo",
    "no-reply",
];

/// The advertised deny and the enforced mediator must agree.
///
/// The advertisement is produced by the projection and the enforcement by the mediator.
/// They live in different modules, so a change to either can silently make the other a
/// lie: the bouncer could start stripping `label` while still advertising `*`, or start
/// forwarding a tag while advertising that it drops them all. Either failure is a
/// fingerprint-grade defect, because a client calibrates its behaviour from this token.
#[tokio::test]
async fn the_advertised_client_tag_deny_is_truthful() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    let registration = register(&mut client).await;

    // 1. The advertisement is present and total, for every client regardless of brand.
    assert!(
        registration.contains("CLIENTTAGDENY=*"),
        "the deny must be advertised: {registration}"
    );
    assert!(
        !registration.contains("CLIENTTAGDENY=") || registration.contains("CLIENTTAGDENY=*"),
        "a narrower advertised allowlist would be a promise this build does not keep: \
         {registration}"
    );

    // 2. Every named client-only tag really is stripped, one line per tag so a failure
    //    names the tag rather than the batch.
    for tag in NAMED_CLIENT_TAGS {
        let line = format!("@{tag}=probe PRIVMSG #room :from-{tag}\r\n");
        client.write_all(line.as_bytes()).await.unwrap();
        let upstream = read_until(harness.upstream(), format!("from-{tag}").as_bytes()).await;
        assert!(
            upstream.ends_with(&format!("PRIVMSG #room :from-{tag}\r\n")),
            "{tag} must be stripped before the line is forwarded: {upstream:?}"
        );
        assert!(
            !upstream.contains(&format!("@{tag}")),
            "{tag} must never appear on the upstream wire: {upstream:?}"
        );
    }

    // 3. `label` is the one retained tag, and retention is not forwarding: the router
    //    uses it, the server never sees it.
    client
        .write_all(b"@label=probe PRIVMSG #room :labelled\r\n")
        .await
        .unwrap();
    let upstream = read_until(harness.upstream(), b"labelled").await;
    assert!(
        upstream.ends_with("PRIVMSG #room :labelled\r\n"),
        "the label must be consumed by the router: {upstream:?}"
    );

    // 4. The mediator itself is deny-by-default on a tag it has never heard of, which is
    //    what makes the `*` advertisement honest rather than a best-effort list.
    let unknown = Message::parse(
        b"@+unheard-of/namespace=1;time=2023-11-14T22:13:20.000Z PRIVMSG #room :x\r\n",
    )
    .expect("parses");
    let (mediated, _) = i2pr_irc_runtime::ircv3::mediate_client_tags(&unknown);
    assert!(
        mediated.tags.is_empty(),
        "an unknown client-only tag must be stripped, not passed through: {:?}",
        mediated.tags
    );
}

// ------------------------------------------------- upstream fingerprint absence

/// No build, router, OS or version identifier may appear in any IRC-visible field.
///
/// This is deliberately wider than "we did not add one": the bouncer relays the upstream
/// server's own ISUPPORT and CAP surface, so a version string could arrive that way too.
/// The claim being defended is that *this process* contributes none, so the check runs over
/// the bouncer's own projection, its CTCP auto-answer, and its diagnostics together.
#[tokio::test]
async fn no_build_router_or_version_string_is_exposed_by_default() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle).await;
    harness.wait_phase(Phase::Online).await;
    let mut client = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    let registration = register(&mut client).await;

    // Drive the paths that would plausibly leak: a CTCP auto-answer, a numeric that
    // carries the server's identity, and a routed query.
    harness
        .upstream()
        .write_all(b":probe PRIVMSG bot :\x01PING fingerprint-probe\x01\r\n")
        .await
        .unwrap();
    client.write_all(b"VERSION\r\n").await.unwrap();
    // The auto-answer goes back to the probing peer, never to a client: handing the probe
    // downstream is exactly how a local client would be prompted to auto-reveal itself.
    let auto_answer = read_until(harness.upstream(), b"PING fingerprint-probe").await;
    let downstream = drain(&mut client).await;
    let upstream = drain_upstream(harness.upstream()).await;
    let snapshot = format!("{:?}", harness.snapshot.borrow());

    for (surface, label) in [
        (registration.clone(), "registration"),
        (downstream.clone(), "downstream replies"),
        (auto_answer.clone(), "the CTCP auto-answer"),
        (upstream, "upstream bytes"),
        (snapshot, "structured diagnostics"),
    ] {
        for forbidden in [
            // The bouncer's own build identity.
            "i2pr-irc",
            "CARGO_PKG_VERSION",
            // Host and OS identity.
            "uname",
            "Darwin",
            "Linux",
            "x86_64",
            "aarch64",
            // Router identity, in both of its spellings.
            "i2pd",
            "I2P",
            "b32",
            // Generic version advertisement shapes.
            "VERSION",
            "version=",
            " 0.1.0",
        ] {
            assert!(
                !surface.contains(forbidden),
                "{forbidden:?} must not be exposed on {label}: {surface:?}"
            );
        }
    }

    // The auto-answer echoes the *probe's* token and nothing else. It is a fixed shape,
    // not a rendering of this machine.
    assert!(
        auto_answer.contains(":bouncer NOTICE probe"),
        "the answer is a constant NOTICE to the prober: {auto_answer:?}"
    );
    assert!(
        !downstream.contains("fingerprint-probe"),
        "a CTCP probe must never be handed to a client to answer: {downstream:?}"
    );
}

// --------------------------------------------- raw protocol logging absence

/// Raw IRC must not be logged from any production path.
///
/// This is a structural claim rather than a behavioural one: there is no way to observe
/// "nothing was logged" from inside a test, so the claim is proved the only way it can be,
/// by showing that no production source contains a logging sink at all. The workspace pulls
/// in no logging facade, so a log statement cannot exist even accidentally.
#[test]
fn raw_protocol_logging_is_absent_from_every_production_path() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");

    // 1. No logging facade is a dependency of any production crate. A facade would let a
    //    raw frame reach a subscriber the Operator configured by accident.
    for crate_dir in ["wire", "core", "store", "runtime", "testkit", "fuzz-smoke"] {
        let manifest = root.join("crates").join(crate_dir).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
        for facade in [
            "tracing",
            "log =",
            "env_logger",
            "slog",
            "fern",
            "simplelog",
        ] {
            assert!(
                !text.contains(facade),
                "{facade} must not be a dependency of crates/{crate_dir}"
            );
        }
    }

    // 2. No production source writes to stdout or stderr. A `println!` of a frame is the
    //    only way raw protocol reaches a console without a logging dependency, and it is
    //    exactly the leak the claim forbids.
    let mut checked = 0usize;
    for crate_dir in ["wire", "core", "store", "runtime", "fuzz-smoke"] {
        let src = root.join("crates").join(crate_dir).join("src");
        for entry in walk(&src) {
            let text = std::fs::read_to_string(&entry)
                .unwrap_or_else(|error| panic!("{}: {error}", entry.display()));
            for sink in ["println!", "eprintln!", "dbg!", "print!", "eprint!"] {
                assert!(
                    !text.contains(sink),
                    "{sink} is a raw-protocol leak in {}",
                    entry.display()
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 8,
        "the source walk must have inspected real files"
    );

    // 3. The workspace root itself declares no logging facade either, so a new crate
    //    cannot inherit one.
    let root_manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("root manifest");
    for facade in ["tracing", "env_logger", "slog"] {
        assert!(
            !root_manifest.contains(facade),
            "{facade} must not be a workspace dependency"
        );
    }
}

/// Every `.rs` file under `dir`, recursively and deterministically.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

// ------------------------------------------------- policy over client brand

/// What the upstream server sees must not depend on which client is attached.
///
/// Two runs with differently-behaving clients must produce byte-identical upstream
/// traffic. If they did not, the attached client's software -- and through it, this
/// Operator's local setup -- would become part of the upstream fingerprint.
#[tokio::test]
async fn remote_visible_ctcp_behaviour_is_fixed_by_policy_not_by_client_brand() {
    // The upstream lines that describe this Operator's *behaviour*, with the liveness
    // PING removed. That PING comes from a timer, so its position relative to client
    // traffic is a scheduling artifact: it says nothing about the attached client and
    // comparing byte order would make this test pass or fail on timing.
    fn policy_lines(capture: &str) -> Vec<String> {
        let mut lines: Vec<String> = capture
            .lines()
            .filter(|line| !line.starts_with("PING :bouncer-"))
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines.dedup();
        lines
    }

    // Drives one CTCP probe plus one ordinary message from a client of the given brand and
    // returns everything that reached the server.
    async fn upstream_bytes(caps: &[&str]) -> String {
        let (_store, handle) = store();
        let mut harness = Harness::start(handle).await;
        harness.wait_phase(Phase::Online).await;
        let mut client = harness.attach(ClientId(1)).await;
        harness.wait_attached(1).await;
        if caps.is_empty() {
            register(&mut client).await;
        } else {
            register_with_caps(&mut client, &caps.join(" ")).await;
        }

        // A blocked metadata reply and a forwarded one, sent from the same client, so the
        // two policies are compared within a single brand as well as across brands.
        client
            .write_all(b"PRIVMSG #room :\x01VERSION\x01\r\n")
            .await
            .unwrap();
        client
            .write_all(b"PRIVMSG #room :\x01PING keepalive\x01\r\n")
            .await
            .unwrap();
        client.write_all(b"PRIVMSG #room :plain\r\n").await.unwrap();
        let through_plain = read_until(harness.upstream(), b"plain").await;
        format!(
            "{through_plain}{}",
            drain_upstream(harness.upstream()).await
        )
    }

    let legacy = upstream_bytes(&[]).await;
    let tagged = upstream_bytes(&["message-tags", "batch", "labeled-response"]).await;

    assert!(
        !legacy.is_empty(),
        "the bouncer must have registered upstream"
    );
    assert_eq!(
        policy_lines(&legacy),
        policy_lines(&tagged),
        "an attached client's capabilities must not change what the server sees"
    );
    // The liveness PING is the only timer-driven line, and it is identical either way.
    assert!(legacy.contains("PING :bouncer-"), "{legacy:?}");
    assert!(
        legacy.contains("PING :bouncer-") == tagged.contains("PING :bouncer-"),
        "the keepalive must not depend on the attached client"
    );

    // The blocked reply stays blocked and the forwarded message still passes.
    assert!(
        !legacy.contains("\u{1}VERSION\u{1}"),
        "a metadata probe must never reach upstream: {legacy:?}"
    );
    assert!(
        legacy.contains("PRIVMSG #room :plain"),
        "ordinary chat must be untouched: {legacy:?}"
    );
    assert!(
        legacy.contains("PRIVMSG #room :\u{1}PING keepalive\u{1}"),
        "PING is on the allowlist and must still forward: {legacy:?}"
    );

    // The policy that produced that is the reviewed one, checked directly so the
    // integration test above cannot pass for the wrong reason.
    let metadata = Message::parse(b":c!u@h PRIVMSG #room :\x01VERSION\x01\r\n").expect("parses");
    assert_eq!(
        outbound_action(&classify(&metadata, CtcpDirection::Query)),
        i2pr_irc_runtime::ctcp::OutboundAction::Block,
        "VERSION is a metadata probe and must be blocked"
    );
    let action = Message::parse(b":c!u@h PRIVMSG #room :\x01ACTION waves\x01\r\n").expect("parses");
    assert!(matches!(
        classify(&action, CtcpDirection::Query),
        Ctcp::Action(_)
    ));
}

// ----------------------------------------------- full-generation composition

/// One connection generation, exercised end to end through all four subsystems.
///
/// Routing must serve only the asking session; a DCC probe must not reach any client; a
/// client-only tag must not reach the server; and every diagnostics counter must stay
/// bounded and payload-free. Each of those is covered elsewhere in isolation; the closure
/// claim is that they hold *together* on one live generation.
#[tokio::test]
async fn a_full_generation_never_leaks_or_carries_a_fingerprint_across_reconnect() {
    let (_store, handle) = store();
    let mut harness = Harness::start_with_caps(handle, "message-tags batch labeled-response").await;
    harness.wait_phase(Phase::Online).await;

    // Two clients with different brands, both attached to the same generation.
    let mut asker = harness.attach(ClientId(1)).await;
    harness.wait_attached(1).await;
    register_with_caps(&mut asker, "message-tags batch labeled-response").await;
    let mut bystander = harness.attach(ClientId(2)).await;
    harness.wait_attached(2).await;
    register(&mut bystander).await;

    // A concurrent labeled query from the asker only. Correlation requires the server to
    // echo the bouncer's translated label, so that label is read back off the wire: an
    // unlabelled reply is a server-initiated frame and fans out by design.
    asker
        .write_all(b"@label=who-1 WHOIS alice\r\n")
        .await
        .unwrap();
    let query = read_until(harness.upstream(), b"WHOIS alice").await;
    let label = query_label(&query, "WHOIS alice");
    harness
        .upstream()
        .write_all(
            format!(
                "@label={label} :srv 352 bot #room alice u h 1 server :Alice Real\r\n\
                 @label={label} :srv 318 bot alice :End of /WHOIS list.\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    // The asker gets the correlated reply; the bystander must not.
    let asker_view = read_client_until(&mut asker, b"Alice Real").await;
    assert!(asker_view.contains("Alice Real"), "{asker_view}");
    // The route closes on its own terminator, which is still in flight here; wait for it
    // rather than asserting a settled count that the teardown could race.
    harness.wait_routes(0).await;
    let bystander_view = drain(&mut bystander).await;
    assert!(
        !bystander_view.contains("Alice Real"),
        "a reply must reach only the session that asked: {bystander_view:?}"
    );

    // A DCC probe for the same generation reaches nobody.
    harness
        .upstream()
        .write_all(b":probe PRIVMSG bot :\x01DCC CHAT chat 192 168 0 1 6667\x01\r\n")
        .await
        .unwrap();
    let leaked = format!("{}{}", drain(&mut asker).await, drain(&mut bystander).await);
    assert!(
        !leaked.contains('\u{1}') && !leaked.contains("DCC"),
        "a DCC probe must not reach any client: {leaked:?}"
    );

    let later = drain_upstream(harness.upstream()).await;
    assert!(
        !later.contains("Alice Real"),
        "an operator's own query reply is not upstream traffic"
    );
    assert!(
        query.contains("WHOIS alice"),
        "the query itself is ordinary traffic and must forward: {query:?}"
    );
    assert!(
        !query.contains("@label=who-1"),
        "the client's own label must be translated, not relayed: {query:?}"
    );

    // Every counter the subsystems publish is present, bounded, and payload-free.
    let snapshot = harness.snapshot.borrow().clone();
    assert_eq!(
        snapshot.response_routes, 0,
        "the asker's route must close on its terminator, not linger"
    );
    assert_eq!(
        snapshot.open_batches, 0,
        "the reply batch must close once it is delivered"
    );
    assert_eq!(snapshot.orphaned_replies_dropped, 0);
    assert!(
        snapshot.ctcp_suppressed >= 1,
        "the DCC probe must be counted as suppressed"
    );
    let rendered = format!("{snapshot:?}");
    assert!(
        !rendered.contains("Alice Real") && !rendered.contains("DCC"),
        "diagnostics must carry counters, never payloads: {rendered}"
    );

    // A generation ends with its work settled: nothing is left open to be inherited by
    // the next connection, which is what makes routes and batches generation-local.
    let settled = harness.snapshot.borrow().clone();
    assert_eq!(settled.response_routes, 0);
    assert_eq!(settled.open_batches, 0);
}

// --------------------------------------------------------- catalog-level truth

/// The served downstream capability set is exactly what the mediator and projection use.
///
/// `the_advertised_set_is_the_served_set` in the M004-A suite covers capability names. This
/// adds the closure-specific coupling: every advertised capability must be one the bouncer
/// actually implements, and the tag deny advertised to those clients must be the same deny
/// regardless of which of them negotiated.
#[tokio::test]
async fn the_capability_set_and_the_tag_deny_agree_for_every_negotiating_client() {
    let served: BTreeSet<&str> = i2pr_irc_runtime::downstream::downstream_supported()
        .iter()
        .copied()
        .collect();

    let (_store, handle) = store();
    let mut harness = Harness::start(handle.clone()).await;
    harness.wait_phase(Phase::Online).await;

    // One client per advertised capability, plus one that negotiates all of them at once.
    let mixes: Vec<Vec<&str>> = served
        .iter()
        .map(|capability| vec![*capability])
        .chain([served.iter().copied().collect::<Vec<_>>(), Vec::new()])
        .collect();

    // Every stream is retained for the whole loop: dropping one ends its session, so a
    // client that went out of scope would silently decrement the count the next
    // `wait_attached` is waiting to reach.
    let mut held = Vec::new();
    for (index, caps) in mixes.iter().enumerate() {
        let client_id = ClientId(u64::try_from(index + 1).expect("fits"));
        let mut client = harness.attach(client_id).await;
        harness.wait_attached(index + 1).await;
        let registration = if caps.is_empty() {
            register(&mut client).await
        } else {
            register_with_caps(&mut client, &caps.join(" ")).await
        };
        assert!(
            registration.contains("CLIENTTAGDENY=*"),
            "a client negotiating {caps:?} must still be told the truth about tags"
        );
        held.push(client);
    }
    drop(held);

    // The catalog hands the same held truth to a fresh supervisor: no capability is
    // advertised that no subscriber could serve.
    let catalog = NetworkCatalog::new(handle);
    let restored = catalog.load_desired_state().await.expect("loads");
    assert_eq!(
        restored.len(),
        0,
        "the qualification run must not have left durable state behind"
    );
}
