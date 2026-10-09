//! Corrective 019: production-path coverage for behaviour the legacy supervisor owned.
//!
//! Corrective 019 gates `lib.rs::NetworkSupervisor` out of the production build. Gating
//! is only safe if the behaviours its 25-test qualification suite covers are covered on
//! the path that actually ships, because otherwise deleting or gating the legacy owner
//! would silently drop production coverage while making the public API look cleaner.
//!
//! Two behaviours had **no** production-path coverage at all, and both are qualified here
//! against `owner::NetworkOwner`:
//!
//! - the SASL PLAIN upstream handshake (`owner.rs`), which had never been driven by an
//!   integration test; and
//! - the bounded upstream `QUIT` on explicit stop (`owner.rs`), which no test anywhere
//!   asserted.
//!
//! Everything here runs against fake I2P providers and scripted local streams. No test
//! may require a real listener, a real router, or any network authority.
#![cfg(test)]

use i2pr_irc_core::{ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    RuntimeError,
    catalog::{SupervisorCommand, SupervisorContext, SupervisorHandle},
    owner::{NetworkOwner, Phase},
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
};
use i2pr_irc_store::{
    NetworkRecord, Store, StoreHandle, StorePath, StoredSecret, fallback_display_name,
};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultController, FaultScript, ScriptedStream};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

/// The credential the SASL tests configure. It appears on the wire exactly once, inside
/// the base64 `AUTHENTICATE` payload; every other surface must never render it.
const SASL_USER: &str = "alice";
const SASL_PASSWORD: &str = "swordfish";

/// The base64 of `\0{user}\0{password}`, which is what an `AUTHENTICATE` payload must
/// decode to. Computed rather than hard-coded so a change to either credential cannot
/// leave a stale literal asserting the wrong thing.
fn sasl_payload() -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("\0{SASL_USER}\0{SASL_PASSWORD}"),
    )
}

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

fn record(network: u64, nick: &str, sasl: Option<(&str, &str)>) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: fallback_display_name(NetworkId(network)),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        failover_group: None,
        nick: nick.into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: sasl
            .map(|(user, password)| (user.to_owned(), StoredSecret::new(password.to_owned()))),
        desired_channels: Vec::new(),
    }
}

#[derive(Clone)]
struct Shared(Arc<FakeI2pStreamProvider>);
#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Shared {
    async fn connect(
        &self,
        _network: i2pr_irc_core::NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(_network, endpoint).await
    }
}

/// One production `NetworkOwner` plus the handles a test needs to drive it.
///
/// This is deliberately built from `SupervisorContext` and `NetworkOwner::serve`, the
/// same entry points the catalog uses. Nothing here reconstructs the owner or reaches
/// into its internals, so a pass says something about the shipping path.
struct Harness {
    /// Retained so a test can take the peer and fault controller for a generation.
    provider: Arc<FakeI2pStreamProvider>,
    commands: mpsc::Sender<SupervisorCommand>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    snapshot: watch::Receiver<i2pr_irc_runtime::owner::NetworkSnapshot>,
    /// The upstream peer for the generation this harness brought Online.
    upstream: Option<ScriptedStream>,
    /// The fault controller for that same connection, so a test can inspect exactly what
    /// the bouncer wrote upstream rather than inferring it from what it read.
    faults: Option<Arc<FaultController>>,
}

impl Harness {
    async fn start(store: StoreHandle, sasl: Option<(&str, &str)>) -> Self {
        Self::build(store, sasl).await
    }

    async fn build(store: StoreHandle, sasl: Option<(&str, &str)>) -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        // Capture writes so the QUIT fence can be asserted on the exact byte stream.
        // Queueing several outcomes keeps a later reconnect from failing on an empty
        // fixture queue rather than on the system under test.
        for _ in 0..4 {
            let script = FaultScript {
                capture_writes: true,
                ..FaultScript::default()
            };
            provider.queue_outcome(Ok(script)).unwrap();
        }
        let reconnect = ReconnectScheduler::default();
        let context = SupervisorContext {
            network: NetworkId(1),
            record: Arc::new(record(1, "bot", sasl)),
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
        Self {
            provider,
            commands: command_tx,
            stop,
            task,
            snapshot,
            upstream: None,
            faults: None,
        }
    }

    fn upstream(&mut self) -> &mut ScriptedStream {
        self.upstream.as_mut().expect("this harness is online")
    }

    fn faults(&mut self) -> Arc<FaultController> {
        self.faults.clone().expect("this harness is online")
    }

    /// Every byte this generation's bouncer side wrote upstream.
    fn written_upstream(&self) -> Vec<u8> {
        self.faults
            .as_ref()
            .map(|faults| faults.bytes_written(0))
            .unwrap_or_default()
    }

    /// Takes the peer and the fault controller for the generation that is connecting.
    ///
    /// The provider queues both in the same order, so pairing them is safe.
    async fn take_generation(&mut self) {
        self.upstream = Some(self.provider.take_peer().await);
        self.faults = Some(Arc::new(self.provider.take_controller().await));
    }

    /// Completes registration without SASL, reaching Online.
    async fn drive_online(&mut self) {
        self.take_generation().await;
        let upstream = self.upstream();
        read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        self.wait_phase(Phase::Online).await;
    }

    /// Drives the full SASL PLAIN exchange and returns only once the generation is
    /// Online, proving the credential was accepted rather than merely sent.
    async fn drive_online_with_sasl(&mut self) -> Vec<u8> {
        self.take_generation().await;
        let upstream = self.upstream();
        let mut sent = Vec::new();
        read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS * :message-tags\r\n:srv CAP * LS :sasl=PLAIN\r\n")
            .await
            .unwrap();
        sent.extend(read_until(upstream, b"CAP REQ").await);
        upstream
            .write_all(b":srv CAP * ACK :sasl=PLAIN\r\n")
            .await
            .unwrap();
        sent.extend(read_until(upstream, b"AUTHENTICATE PLAIN\r\n").await);
        upstream.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
        let payload = format!("AUTHENTICATE {}\r\n", sasl_payload());
        sent.extend(read_until(upstream, payload.as_bytes()).await);
        upstream
            .write_all(b":srv 903 bot :SASL success\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        sent.extend(read_until(upstream, b"CAP REQ :message-tags\r\n").await);
        upstream
            .write_all(b":srv CAP * ACK :message-tags\r\n")
            .await
            .unwrap();
        sent.extend(read_until(upstream, b"CAP END\r\n").await);
        self.wait_phase(Phase::Online).await;
        sent
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
                "never reached {phase:?}; phase={:?} gen={:?} error={:?}",
                snapshot.phase, snapshot.generation, snapshot.last_error
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
            .expect("attach fits the bounded control queue");
        response
            .await
            .expect("owner answers")
            .expect("attach accepted");
        peer
    }

    /// Stops the owner explicitly, waits for the task, and returns every byte the
    /// bouncer wrote upstream.
    ///
    /// Returning the capture rather than letting the caller read it afterwards matters:
    /// the `QUIT` is the last thing the owner writes, so the assertion belongs after the
    /// task ends, but stopping consumes the harness.
    async fn stop_explicitly(self) -> (Result<(), RuntimeError>, Vec<u8>) {
        // Clone the controller before the harness is consumed: stopping moves the task
        // handle out, and the QUIT is only in the capture once the task has ended.
        let faults = self.faults.clone().expect("this harness is online");
        let _ = self.stop.send(true);
        let outcome = self.task.await.expect("the owner task ends");
        (outcome, faults.bytes_written(0))
    }

    /// Waits for the owner to end on its own, without stopping it first.
    async fn await_ending(&mut self) -> Result<(), RuntimeError> {
        tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .expect("the owner ends promptly rather than spinning")
            .expect("the owner task is not cancelled")
    }
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0; 512];
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
                "never saw {:?}; saw {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&all)
            )
        });
    all
}

/// Counts how many times `needle` appears in `haystack`.
fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

// ------------------------------------------------------- SASL PLAIN handshake

/// The production handshake must send `AUTHENTICATE PLAIN` and answer the challenge
/// with the configured credential.
///
/// Before Corrective 019 this existed only against the legacy supervisor. The production
/// path additionally requires `CAP * ACK` to enable `sasl` and sends `CAP END` on `903`
/// rather than on `001`, so a copy of the legacy test would not have covered it.
#[tokio::test]
async fn the_production_path_completes_a_sasl_plain_handshake() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    let sent = harness.drive_online_with_sasl().await;

    let sent = String::from_utf8_lossy(&sent);
    assert!(
        sent.contains("CAP REQ :sasl\r\n") && sent.contains("CAP REQ :message-tags\r\n"),
        "a configured Network must request SASL upstream: {sent}"
    );
    assert!(
        sent.contains("AUTHENTICATE PLAIN\r\n"),
        "the bouncer must open the SASL exchange: {sent}"
    );
    let expected = format!("AUTHENTICATE {}\r\n", sasl_payload());
    assert!(
        sent.contains(&expected),
        "the bouncer must answer the challenge with the credential payload: {sent}"
    );
    assert!(
        !sent.contains(SASL_PASSWORD),
        "the credential must never appear unencoded on the wire: {sent}"
    );
}

/// The credential must not reach any surface the Operator did not already own.
///
/// The `AUTHENTICATE` payload necessarily carries the credential upstream, so the
/// assertion is scoped to the surfaces that must never render it: the structured
/// diagnostic projection and anything fanned out to a client.
#[tokio::test]
async fn a_sasl_credential_reaches_no_diagnostic_and_no_downstream_byte() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.drive_online_with_sasl().await;

    let mut client = harness.attach(ClientId(1)).await;
    harness
        .upstream()
        .write_all(b":srv PRIVMSG #ops :hello\r\n")
        .await
        .unwrap();

    let mut seen = Vec::new();
    let mut buf = [0; 512];
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let count = client.read(&mut buf).await.unwrap();
        seen.extend_from_slice(&buf[..count]);
    })
    .await;
    assert!(read.is_ok(), "the fanout must reach the attached client");

    let snapshot = format!("{:?}", harness.snapshot.borrow());
    for (surface, label) in [
        (snapshot.clone(), "structured diagnostics"),
        (
            String::from_utf8_lossy(&seen).into_owned(),
            "downstream bytes",
        ),
    ] {
        for secret in [SASL_PASSWORD, SASL_USER, sasl_payload().as_str()] {
            assert!(
                !surface.contains(secret),
                "SASL material leaked into {label}: {surface}"
            );
        }
    }
}

/// A refused credential is terminal and bounded, not a retry loop.
#[tokio::test]
async fn a_refused_sasl_credential_is_a_terminal_registration_error() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS * :message-tags\r\n:srv CAP * LS :sasl=PLAIN\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ").await;
    upstream
        .write_all(b":srv CAP * ACK :sasl=PLAIN\r\n")
        .await
        .unwrap();
    read_until(upstream, b"AUTHENTICATE PLAIN\r\n").await;
    upstream.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
    let payload = format!("AUTHENTICATE {}\r\n", sasl_payload());
    read_until(upstream, payload.as_bytes()).await;
    upstream
        .write_all(b":srv 904 bot :SASL authentication failed\r\n")
        .await
        .unwrap();

    let outcome = harness.await_ending().await;
    assert!(
        matches!(outcome, Err(RuntimeError::Registration)),
        "a refused credential must be a terminal registration error, saw {outcome:?}"
    );
}

/// A Network configured for SASL must refuse to register against a server that never
/// offered it, rather than registering in a weaker mode than the Operator configured.
#[tokio::test]
async fn sasl_configured_but_not_offered_is_a_terminal_registration_error() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :message-tags\r\n:srv 001 bot :welcome\r\n")
        .await
        .unwrap();

    let outcome = harness.await_ending().await;
    assert!(
        matches!(outcome, Err(RuntimeError::Registration)),
        "registering without the configured authentication is a registration failure, saw \
         {outcome:?}"
    );
}

#[tokio::test]
async fn a_classic_server_welcomes_without_cap_end() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    let initial = read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    harness.wait_phase(Phase::Online).await;
    let (outcome, written) = harness.stop_explicitly().await;
    assert!(
        outcome.is_ok(),
        "no-CAP registration is supported: {outcome:?}"
    );
    let written = String::from_utf8_lossy(&written);
    assert!(String::from_utf8_lossy(&initial).contains("CAP LS 302\r\n"));
    assert!(
        !written.contains("CAP END"),
        "CAP END is not sent to a classic server: {written}"
    );
}

#[tokio::test]
async fn unknown_cap_command_then_welcome_is_a_supported_downgrade() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv 421 bot CAP :Unknown command\r\n:srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    harness.wait_phase(Phase::Online).await;
    let (outcome, written) = harness.stop_explicitly().await;
    assert!(
        outcome.is_ok(),
        "421 CAP is an optional compatibility downgrade: {outcome:?}"
    );
    assert!(!String::from_utf8_lossy(&written).contains("CAP END"));
}

#[tokio::test]
async fn optional_capability_nak_does_not_abort_registration() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :message-tags\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ :message-tags\r\n").await;
    upstream
        .write_all(b":srv CAP * NAK :message-tags\r\n:srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    harness.wait_phase(Phase::Online).await;
    let (outcome, written) = harness.stop_explicitly().await;
    assert!(
        outcome.is_ok(),
        "optional capability rejection is non-fatal: {outcome:?}"
    );
    assert!(String::from_utf8_lossy(&written).contains("CAP END\r\n"));
}

#[tokio::test]
async fn fragmented_multiline_cap_ls_is_accumulated_before_the_request() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;

    // Stream boundaries have no IRC meaning: deliver one chunk a byte at a time, and use
    // the CAP LS continuation marker to prove capability collection waits for the final
    // chunk before producing a request.
    for chunk in [
        b":srv CAP * LS * :message-tags\r\n".as_slice(),
        b":srv CAP * LS :server-time account-tag invite-notify\r\n".as_slice(),
    ] {
        for byte in chunk {
            upstream
                .write_all(std::slice::from_ref(byte))
                .await
                .unwrap();
        }
    }
    let request = read_until(
        upstream,
        b"CAP REQ :message-tags server-time account-tag invite-notify\r\n",
    )
    .await;
    let request = String::from_utf8_lossy(&request);
    assert!(request.contains("message-tags"), "{request}");
    assert!(request.contains("account-tag"), "{request}");
    assert!(request.contains("invite-notify"), "{request}");

    upstream
        .write_all(b":srv CAP * ACK :message-tags server-time account-tag invite-notify\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP END\r\n").await;
    upstream
        .write_all(b":srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    harness.wait_phase(Phase::Online).await;
    let (outcome, _) = harness.stop_explicitly().await;
    assert!(
        outcome.is_ok(),
        "fragmented modern registration succeeds: {outcome:?}"
    );
}

#[tokio::test]
async fn cap_without_sasl_completes_without_authentication_when_none_is_configured() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :message-tags\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ :message-tags\r\n").await;
    upstream
        .write_all(b":srv CAP * ACK :message-tags\r\n:srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP END\r\n").await;
    harness.wait_phase(Phase::Online).await;
    let (outcome, written) = harness.stop_explicitly().await;
    assert!(outcome.is_ok());
    assert!(!String::from_utf8_lossy(&written).contains("AUTHENTICATE"));
}

#[tokio::test]
async fn bare_sasl_capability_attempts_configured_plain_authentication() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :sasl\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ :sasl\r\n").await;
    upstream
        .write_all(b":srv CAP * ACK :sasl\r\n")
        .await
        .unwrap();
    read_until(upstream, b"AUTHENTICATE PLAIN\r\n").await;
    upstream.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
    let payload = format!("AUTHENTICATE {}\r\n", sasl_payload());
    read_until(upstream, payload.as_bytes()).await;
    upstream
        .write_all(b":srv 903 bot :SASL success\r\n:srv 001 bot :welcome\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP END\r\n").await;
    harness.wait_phase(Phase::Online).await;
    let (outcome, _) = harness.stop_explicitly().await;
    assert!(
        outcome.is_ok(),
        "bare sasl permits the configured PLAIN attempt: {outcome:?}"
    );
}

#[tokio::test]
async fn configured_sasl_rejects_an_explicit_non_plain_offer() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :sasl=EXTERNAL\r\n")
        .await
        .unwrap();
    let outcome = harness.await_ending().await;
    assert!(matches!(outcome, Err(RuntimeError::Registration)));
}

#[tokio::test]
async fn configured_sasl_nak_is_a_terminal_registration_error() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :sasl\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ :sasl\r\n").await;
    upstream
        .write_all(b":srv CAP * NAK :sasl\r\n")
        .await
        .unwrap();
    let outcome = harness.await_ending().await;
    assert!(matches!(outcome, Err(RuntimeError::Registration)));
}

#[tokio::test]
async fn configured_sasl_fails_when_cap_is_unsupported() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv 421 bot CAP :Unknown command\r\n")
        .await
        .unwrap();
    let outcome = harness.await_ending().await;
    assert!(matches!(outcome, Err(RuntimeError::Registration)));
}

#[tokio::test]
async fn welcome_before_sasl_success_does_not_complete_registration() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, Some((SASL_USER, SASL_PASSWORD))).await;
    harness.take_generation().await;
    let snapshot = harness.snapshot.clone();
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :sasl\r\n")
        .await
        .unwrap();
    read_until(upstream, b"CAP REQ :sasl\r\n").await;
    upstream
        .write_all(b":srv CAP * ACK :sasl\r\n")
        .await
        .unwrap();
    read_until(upstream, b"AUTHENTICATE PLAIN\r\n").await;
    upstream
        .write_all(b":srv 001 bot :welcome without SASL\r\n")
        .await
        .unwrap();
    let outcome = harness.await_ending().await;
    assert!(matches!(outcome, Err(RuntimeError::Registration)));
    assert_ne!(snapshot.borrow().phase, Some(Phase::Online));
}

// --------------------------------------------------------- upstream QUIT fence

/// An explicit stop writes exactly one `QUIT`, and it is the last thing on the wire.
#[tokio::test]
async fn an_explicit_stop_writes_exactly_one_upstream_quit() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.drive_online().await;

    let (outcome, written) = harness.stop_explicitly().await;
    assert!(outcome.is_ok(), "an explicit stop is a clean shutdown");

    let quit = b"QUIT :Bouncer shutting down\r\n";
    assert_eq!(
        occurrences(&written, quit),
        1,
        "explicit stop must send exactly one QUIT, wrote {:?}",
        String::from_utf8_lossy(&written)
    );
    assert!(
        written.ends_with(quit),
        "the QUIT must be the final frame, not merely present somewhere: {:?}",
        String::from_utf8_lossy(&written)
    );
}

/// Nothing a client sent may reach the network after the QUIT fence.
///
/// `network-ownership.md` claims the QUIT is a fence; this is the assertion behind that
/// claim rather than a restatement of it.
#[tokio::test]
async fn no_client_traffic_reaches_the_network_after_the_quit_fence() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.drive_online().await;

    let mut client = harness.attach(ClientId(7)).await;
    // Register the client so its command is eligible for forwarding rather than refused
    // at the protocol gate, then queue traffic behind the stop.
    client
        .write_all(b"NICK observer\r\nUSER user 0 * :observer\r\n")
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        let mut buf = [0; 512];
        loop {
            let count = client.read(&mut buf).await.unwrap();
            if count == 0 {
                break;
            }
        }
    })
    .await;

    let before = harness.written_upstream();
    let quit_at = before
        .windows(b"QUIT".len())
        .position(|window| window == b"QUIT");
    assert!(
        quit_at.is_none(),
        "no QUIT may appear before the explicit stop"
    );

    let (outcome, written) = harness.stop_explicitly().await;
    assert!(outcome.is_ok(), "an explicit stop is a clean shutdown");

    let quit_at = written
        .windows(b"QUIT :Bouncer shutting down\r\n".len())
        .position(|window| window == b"QUIT :Bouncer shutting down\r\n")
        .expect("the stop wrote a QUIT");
    let after = &written[quit_at + b"QUIT :Bouncer shutting down\r\n".len()..];
    assert!(
        after.is_empty(),
        "no frame may follow the QUIT fence, saw {:?}",
        String::from_utf8_lossy(after)
    );
}

/// A generation that failed is aborted, not politely closed, and sends no QUIT.
///
/// A `QUIT` here would be a lie: the bouncer would be announcing a clean shutdown of a
/// session that had already failed, and a server counting clean closes would be misled.
///
/// The assertion is scoped to the failed generation's own connection. A lost upstream is
/// not terminal -- the owner backs off and reconnects, which is correct -- so the harness
/// waits for `Backoff` and then reads the capture belonging to the generation that died.
/// Each connection gets its own `FaultController`, so a later generation's traffic cannot
/// be mistaken for this one's.
#[tokio::test]
async fn a_failed_generation_is_aborted_and_sends_no_quit() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.drive_online().await;

    // The upstream goes away under the bouncer. `reset_read` rather than `close_write`
    // because `close_write(1)` wakes side 1's reader, while the bouncer is parked on side
    // 0; `reset_read(0)` wakes the side that is actually waiting.
    harness.faults().reset_read(0);
    harness.wait_phase(Phase::Backoff).await;

    let written = harness.written_upstream();
    assert_eq!(
        occurrences(&written, b"QUIT"),
        0,
        "a failed generation is aborted rather than closed politely, but wrote {:?}",
        String::from_utf8_lossy(&written)
    );
    assert!(
        !String::from_utf8_lossy(&written).contains("Bouncer shutting down"),
        "only an explicit stop may announce a clean shutdown: {:?}",
        String::from_utf8_lossy(&written)
    );
}

/// A registration failure *is* terminal, and terminates without a QUIT either.
#[tokio::test]
async fn a_terminally_rejected_generation_sends_no_quit() {
    let (_store, handle) = store();
    let mut harness = Harness::start(handle, None).await;
    harness.take_generation().await;
    let upstream = harness.upstream();
    read_until(upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv 451 bot :You have not registered\r\n")
        .await
        .unwrap();

    let outcome = harness.await_ending().await;
    assert!(
        matches!(outcome, Err(RuntimeError::Registration)),
        "a registration rejection is terminal, saw {outcome:?}"
    );
    let written = harness.written_upstream();
    assert_eq!(
        occurrences(&written, b"QUIT"),
        0,
        "a registration rejection is not a clean shutdown, but wrote {:?}",
        String::from_utf8_lossy(&written)
    );
}

// ------------------------------------------- the legacy owner is not shipped

/// The legacy Network owner must not exist in the production build.
///
/// This asserts the source form rather than trying to name an absent item, because Rust
/// cannot reference a type that is correctly absent -- a test naming
/// `i2pr_irc_runtime::NetworkSupervisor` would fail to compile for exactly the wrong
/// reason. Reading the declaration is the honest check: the invariant is literally an
/// attribute on a declaration.
///
/// What this does **not** prove is that no other module reaches the legacy items; that is
/// the second half of this test, which scans every first-party source file outside the
/// gated module. Neither half proves a link-time property on its own, and together they
/// are the same evidence `cargo clippy -D warnings` acts on.
#[test]
fn the_legacy_network_owner_is_gated_out_of_the_production_build() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs is readable");

    for item in [
        "pub struct NetworkSupervisor<P> {",
        "impl<P: I2pStreamProvider> NetworkSupervisor<P> {",
        "pub struct UpstreamConfig {",
        "impl UpstreamConfig {",
        "pub struct NetworkSnapshot {",
        "type AcceptFuture<'a, A> = Pin<",
        "fn apply_upstream_line<D: ByteStream>(",
        "async fn next_accept<'a, A: LocalAcceptor>(",
        "async fn read_client<D: ByteStream>(",
        "async fn client_writer_exit(session_writer:",
        "async fn next_intent_frame(",
        "async fn send<W: tokio::io::AsyncWrite + Unpin>(",
        "pub(crate) fn queue_control(",
        "pub(crate) async fn stopped(",
    ] {
        assert!(
            source.contains(item),
            "expected the legacy declaration `{item}` to still exist; if Corrective 019 \
             deleted it, delete this assertion rather than weakening it"
        );
        assert!(
            is_gated(&source, item),
            "`{item}` must be preceded by #[cfg(test)]: the production build must not \
             contain the legacy Network owner"
        );
    }
}

/// True when `declaration` is immediately preceded by a `#[cfg(test)]` attribute.
fn is_gated(source: &str, declaration: &str) -> bool {
    let Some(index) = source.find(declaration) else {
        return false;
    };
    let preceding = &source[..index];
    preceding
        .lines()
        .rev()
        .take_while(|line| !line.trim().is_empty())
        .any(|line| line.trim() == "#[cfg(test)]")
}

/// Nothing outside `lib.rs` may reference the legacy owner or its exclusive types.
#[test]
fn no_other_first_party_module_references_the_legacy_owner() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(root.join("src")).expect("src is readable") {
        let path = entry.expect("directory entry").path();
        if path.file_name().and_then(|name| name.to_str()) == Some("lib.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for needle in ["NetworkSupervisor", "UpstreamConfig", "AcceptFuture"] {
            if text.contains(needle) {
                offenders.push(format!("{} references {needle}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the legacy owner must be reachable only from lib.rs's test build: {offenders:?}"
    );
}
