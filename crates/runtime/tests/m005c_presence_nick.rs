//! M005-C qualification: presence and preferred-nick policy.
//!
//! Two claims are under test here.
//!
//! *Presence is not a socket count.* The bouncer represents one Operator across many
//! local clients, so "is the Operator here" is derived from what those clients say about
//! themselves, never from how many are connected. An explicit manual away outranks every
//! count, a passive background session is not an Operator at a keyboard, and only
//! transitions produce upstream traffic.
//!
//! *A nick collision is a collision, not a timeout.* Registration answers a refusal with
//! a bounded deterministic fallback that cannot be influenced by the host, exhausts into
//! a terminal failure rather than a storm, and reclaims the preferred nick only under an
//! Operator policy that is itself rate-bounded and generation-owned.
//!
//! Everything runs against fake providers and scripted streams. No test may require a
//! real listener, a real router, or any network authority.
#![cfg(test)]

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::{
    ChannelPolicy, RuntimeError,
    catalog::{SupervisorCommand, SupervisorContext},
    owner::{NetworkOwner, NetworkSnapshot},
    reconnect::ReconnectScheduler,
    resource::ResourceLedger,
    session::{ATTACH_SHORTHAND, DETACH_SHORTHAND},
};
use i2pr_irc_store::{
    DesiredChannelRecord, NetworkRecord, Store, StoreError, StoreHandle, StorePath,
    testing as store_testing,
};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, mpsc, oneshot, watch},
};

/// Ceiling for every bounded wait in this suite. A test that exceeds it has failed to
/// observe an event, which is a failure of the claim under test, not a flake to retry.
const CEILING: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

/// The durable presence policy one test wants.
#[derive(Clone, Copy, Debug, Default)]
struct Policy {
    auto_away: bool,
    keep_nick: bool,
}
impl Policy {
    const AUTO_AWAY: Self = Self {
        auto_away: true,
        keep_nick: false,
    };
    const KEEP_NICK: Self = Self {
        auto_away: false,
        keep_nick: true,
    };
    const BOTH: Self = Self {
        auto_away: true,
        keep_nick: true,
    };
}

fn record(network: u64, channels: &[DesiredChannelRecord]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: channels.to_vec(),
    }
}

/// A durable channel policy that really stores, and can be told to answer one commit
/// ambiguously.
///
/// Wrapping rather than replacing the store is deliberate: an ambiguous commit is only
/// meaningful against durable state that genuinely exists, because the behaviour under
/// test is "re-read and present whatever is on disk".
#[derive(Clone)]
struct FaultyPolicy {
    inner: StoreHandle,
    /// How many more `set_detached` calls must answer `CommitState::Unknown`.
    ///
    /// Armed separately from everything else because the owner performs its own durable
    /// reads while a test runs; a shared counter would let ordinary traffic consume the
    /// arming meant for the operation under test.
    ambiguous: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl ChannelPolicy for FaultyPolicy {
    async fn set_detached(
        &self,
        network: NetworkId,
        channel: &str,
        detached: bool,
    ) -> Result<bool, StoreError> {
        let mut remaining = self.ambiguous.lock().await;
        if *remaining > 0 {
            *remaining -= 1;
            // The mutation is deliberately *not* applied and the durable outcome is
            // unknown, which is exactly the case the owner must re-read rather than
            // assume either way.
            return Err(store_testing::unknown_commit());
        }
        self.inner
            .set_desired_channel_detached(network, channel, detached)
            .await
    }

    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError> {
        self.inner.load_networks().await
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
    ) -> Result<Box<dyn ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(_network, endpoint).await
    }
}

/// One Network owner, its fake upstream, and the clients attached to it.
struct Harness {
    commands: mpsc::Sender<SupervisorCommand>,
    snapshot: watch::Receiver<NetworkSnapshot>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), RuntimeError>>,
    provider: Arc<FakeI2pStreamProvider>,
    store: (Store, StoreHandle),
    /// Upstream peers this test has claimed.
    ///
    /// Retained deliberately: dropping a scripted peer closes the socket, which ends the
    /// generation exactly as a real upstream disconnect would -- correct behaviour, and
    /// not what a test that wants a stable online Network should do.
    upstreams: Vec<ScriptedStream>,
    /// The ISUPPORT block the fake server advertises at registration.
    isupport: String,
    /// The nickname attached clients claim.
    ///
    /// A client is projected under the nick the bouncer actually holds, so a Network that
    /// fell back to `bot_1` must register its clients as `bot_1`. Claiming the preferred
    /// nick would be refused -- correctly -- and the test would be measuring the refusal.
    downstream_nick: String,
    channels: Vec<DesiredChannelRecord>,
}

impl Harness {
    /// Builds an owner with an explicit durable policy and explicit extra ISUPPORT tokens.
    ///
    /// `extra` is a space-separated token list (`"MONITOR=4"`), not a raw tail. It is
    /// rendered into a terminated `005` line here rather than pasted into the stream by
    /// each test: an unterminated line would sit in the decoder forever and stall
    /// registration, which is a fixture bug that reads exactly like an owner bug.
    async fn build(
        channels: &[DesiredChannelRecord],
        policy: Policy,
        extra: &str,
    ) -> (Self, Arc<Mutex<usize>>) {
        let (store, handle) = store();
        let mut subject = record(1, channels);
        subject.auto_away = policy.auto_away;
        subject.keep_nick = policy.keep_nick;
        handle
            .save_network(&subject)
            .await
            .expect("durable record saves");

        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..6 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("provider queue has room");
        }
        let ambiguous = Arc::new(Mutex::new(0usize));
        let policy = FaultyPolicy {
            inner: handle.clone(),
            ambiguous: ambiguous.clone(),
        };
        let reconnect = ReconnectScheduler::default();
        let resources = ResourceLedger::new(reconnect.clone(), handle.clone());
        let context = SupervisorContext {
            network: NetworkId(1),
            record: Arc::new(subject),
            store: handle.clone(),
            status: watch::channel(Default::default()).0,
            resources,
        };
        // Every owner in this suite runs on the injectable policy, so one harness shape
        // covers both the ordinary case and the ambiguous-commit case.
        let owner = NetworkOwner::with_channel_policy(
            Shared(provider.clone()),
            context,
            handle.clone(),
            reconnect,
            Arc::new(policy.clone()),
        )
        .expect("owner constructs");
        let snapshot = owner.subscribe_snapshot();
        let (command_tx, command_rx) = mpsc::channel(64);
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(async move { owner.serve(command_rx, stop_rx).await });
        (
            Self {
                commands: command_tx,
                snapshot,
                stop,
                task,
                provider,
                store: (store, handle),
                upstreams: Vec::new(),
                isupport: if extra.is_empty() {
                    String::new()
                } else {
                    format!(":srv 005 bot {extra}\r\n")
                },
                downstream_nick: "bot".to_owned(),
                channels: channels.to_vec(),
            },
            ambiguous,
        )
    }

    /// Drives registration, then confirms every desired channel upstream.
    ///
    /// A detached channel is joined here exactly like an attached one. That is the whole
    /// point of the policy, so a test that did not do this would not be testing it.
    async fn bring_online(&mut self) -> usize {
        let mut upstream = self.provider.take_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        let isupport = self.isupport.clone();
        upstream
            .write_all(format!(":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n{isupport}").as_bytes())
            .await
            .expect("upstream accepts registration");
        // One read covers registration completion *and* the JOINs the owner writes
        // immediately afterwards. Reading for them one at a time is wrong on a scripted
        // stream: a single read can carry several frames, so the step that finds its
        // needle discards everything after it, including the next step's.
        let last = self
            .channels
            .last()
            .map(|channel| format!("JOIN {}\r\n", channel.target))
            .unwrap_or_else(|| "CAP END\r\n".to_owned());
        read_until(&mut upstream, last.as_bytes()).await;
        for channel in self.channels.clone() {
            // A complete, truthful channel view: membership, a topic, and a names list.
            // The projection emits `366` only once membership is known complete, so a
            // fixture that omits `353` would prove nothing about channel visibility.
            let confirm = format!(
                ":bot JOIN {0}\r\n:srv 332 bot {0} :the topic of {0}\r\n:srv 353 bot = {0} :@bot alice\r\n",
                channel.target
            );
            upstream
                .write_all(confirm.as_bytes())
                .await
                .expect("server confirms membership");
        }
        settle(&mut upstream).await;
        self.upstreams.push(upstream);
        let index = self.upstreams.len() - 1;
        // The generation applies its presence policy on the way up, so with no session
        // attached yet an auto-away Network is legitimately away here. Tests assert on
        // transitions, so this baseline is read and discarded before each observation
        // rather than being mistaken for one of them.
        self.drain_upstream().await;
        index
    }

    /// Brings the owner online through a single refused preferred nick.
    ///
    /// Reclaim only has anything to do when the bouncer ends up holding a different nick,
    /// and only a real collision gets it there. Every reclaim test starts from that state
    /// rather than pretending the bouncer chose a different nick for itself.
    async fn bring_online_collision(&mut self) -> usize {
        let mut upstream = self.provider.take_peer().await;
        // The preferred nick is the whole precondition, so it is read rather than assumed:
        // the burst up to `USER` carries it because `NICK` is written first.
        let opening = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        assert_eq!(
            nick_written(&opening),
            "bot",
            "the server refuses the *preferred* nick, not one the bouncer picked"
        );
        let isupport = self.isupport.clone();
        upstream
            .write_all(format!(":srv CAP * LS :\r\n{isupport}").as_bytes())
            .await
            .expect("upstream accepts the capability offer");
        upstream
            .write_all(b":srv 433 * bot :Nickname is already in use\r\n")
            .await
            .expect("upstream refuses the preferred nick");
        self.upstreams.push(upstream);
        self.upstreams.len() - 1
    }

    fn upstream(&mut self, index: usize) -> &mut ScriptedStream {
        &mut self.upstreams[index]
    }

    /// Attaches one client and completes its registration.
    ///
    /// Returns the client end of the socket, which the test keeps alive: a dropped socket
    /// is a closed socket, and a closed socket ends the attachment.
    async fn attach(&mut self, session: u64, caps: &[&str]) -> Client {
        self.attach_as(session, caps, false).await
    }

    /// Attaches one client that declared itself passive during registration.
    ///
    /// `PASSIVE` goes before `CAP END`, which is the draft's pre-registration semantics
    /// and the reason they exist: a background client that had to declare itself after
    /// connecting would make the bouncer flap away and back for every such client.
    async fn attach_passive(&mut self, session: u64, caps: &[&str]) -> Client {
        self.attach_as(session, caps, true).await
    }

    /// Reads and discards whatever upstream currently has queued.
    async fn drain_upstream(&mut self) {
        drain(self.upstream(0), Duration::from_millis(250)).await;
    }

    /// Waits for the expected presence state and then clears whatever upstream has.
    ///
    /// Draining without waiting first is a race: the frame a transition produces is
    /// written by the upstream writer task, which may not have run yet when the drain
    /// starts. Waiting on the snapshot proves the decision happened; the drain that
    /// follows proves which frames it produced.
    async fn settle_presence(&mut self, expected: Option<&str>) -> String {
        self.wait_away(expected).await;
        drain(self.upstream(0), Duration::from_millis(250)).await
    }

    async fn attach_as(&mut self, session: u64, caps: &[&str], passive: bool) -> Client {
        let (mut client_side, peer) = tokio::io::duplex(64 * 1024);
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Attach {
                session: SessionId(session),
                client: ClientId(session),
                stream: Box::new(peer),
                reply,
            })
            .expect("attach fits the bounded control queue");
        response
            .await
            .expect("owner answers")
            .expect("attach accepted");
        self.wait_attached().await;

        let nick = self.downstream_nick.clone();
        let mut registration =
            Vec::from(format!("NICK {nick}\r\nUSER user 0 * :phone\r\n").as_bytes());
        if !caps.is_empty() {
            registration.extend_from_slice(format!("CAP REQ :{}\r\n", caps.join(" ")).as_bytes());
            if passive {
                registration.extend_from_slice(b"PASSIVE\r\n");
            }
            registration.extend_from_slice(b"CAP END\r\n");
        } else {
            assert!(
                !passive,
                "a client cannot declare itself passive without negotiating the draft"
            );
        }
        client_side
            .write_all(&registration)
            .await
            .expect("client registers");
        let mut client = Client {
            stream: client_side,
            seen: String::new(),
        };
        let isupport = format!("005 {nick} CLIENTTAGDENY=*");
        client.until(&isupport).await;
        client
    }

    async fn wait_attached(&mut self) {
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if self.snapshot.borrow().attached_sessions >= 1 {
                return;
            }
            let _ = tokio::time::timeout_at(deadline, self.snapshot.changed())
                .await
                .expect("the owner reports the attachment");
        }
    }

    /// Waits until the generation reports the expected upstream away state.
    ///
    /// Waiting on the snapshot rather than on the wire is what makes the "emitted
    /// exactly once" assertions meaningful: the transition is recorded before the frame
    /// is written, so a snapshot read proves the decision happened and a later drain
    /// proves how many frames it produced.
    async fn wait_away(&mut self, expected: Option<&str>) {
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if self.snapshot.borrow().away.as_deref() == expected {
                return;
            }
            let _ = tokio::time::timeout_at(deadline, self.snapshot.changed())
                .await
                .expect("the owner reports the expected presence transition");
        }
    }

    async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(CEILING, self.task).await;
        self.store.0.shutdown().expect("store shuts down");
    }
}

/// One attached client socket plus everything already read from it.
///
/// The buffer is not decoration. `read_until` stops at the frame it was asked for, and
/// a scripted stream can deliver several frames in one read, so a caller that dropped
/// the remainder would silently lose whatever the owner wrote next -- which is exactly
/// the frame the next assertion is about.
struct Client {
    stream: tokio::io::DuplexStream,
    seen: String,
}

impl Client {
    /// Marks the current end of the buffer.
    ///
    /// A negative assertion has to look only at what arrived after the event it is
    /// about: the whole buffer legitimately contains a detached channel's name from the
    /// projection that preceded the detach.
    fn mark(&self) -> usize {
        self.seen.len()
    }

    /// Everything received since `mark`.
    fn since(&self, mark: usize) -> String {
        self.seen[mark..].to_owned()
    }

    /// Reads until `needle` has been seen, returning everything read so far.
    async fn until(&mut self, needle: &str) -> String {
        // The whole buffer is searched, not just the newly arrived tail: a frame a
        // previous read already delivered is a frame this client has, and asking for it
        // again must answer from what is in hand rather than block forever.
        while !self.seen.contains(needle) {
            let mut chunk = [0u8; 1024];
            let count = tokio::time::timeout(CEILING, self.stream.read(&mut chunk))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; saw {:?}", self.seen))
                .expect("the client stream does not fail");
            if count == 0 {
                panic!(
                    "client closed while waiting for {needle:?}; saw {:?}",
                    self.seen
                );
            }
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
        self.seen.clone()
    }

    /// Reads whatever arrives for `settle`, so a negative assertion cannot pass merely
    /// because the owner had not yet produced the frame it was supposed to withhold.
    async fn drain(&mut self, settle: Duration) -> String {
        let deadline = tokio::time::Instant::now() + settle;
        loop {
            let mut chunk = [0u8; 1024];
            let Ok(Ok(count)) =
                tokio::time::timeout_at(deadline, self.stream.read(&mut chunk)).await
            else {
                return self.seen.clone();
            };
            if count == 0 {
                return self.seen.clone();
            }
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
    }

    async fn send(&mut self, frame: &str) {
        self.stream
            .write_all(frame.as_bytes())
            .await
            .expect("the client writes");
    }
}

/// Returns the nick carried by the most recent `NICK ` frame in `frame`.
///
/// The owner writes `CAP LS`, `NICK` and `USER` as a single burst with `NICK` first, so
/// anything read up to `USER` has already delivered the nick. A helper that then waits
/// for that same `NICK` again is waiting for a frame that has been and gone.
fn nick_written(frame: &str) -> String {
    frame
        .rsplit("NICK ")
        .next()
        .map(|rest| rest.split("\r\n").next().unwrap_or_default().to_owned())
        .expect("a NICK frame")
}

async fn read_until<R: AsyncReadExt + Unpin>(stream: &mut R, needle: &[u8]) -> String {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = tokio::time::timeout(CEILING, stream.read(&mut chunk))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {:?}; saw {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&seen)
                )
            })
            .expect("the stream does not fail");
        if count == 0 {
            panic!(
                "stream closed while waiting for {:?}; saw {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&seen)
            );
        }
        seen.extend_from_slice(&chunk[..count]);
        if seen.windows(needle.len()).any(|window| window == needle) {
            return String::from_utf8_lossy(&seen).into_owned();
        }
    }
}

/// Reads whatever is available without waiting for a particular frame.
///
/// Used to prove the *absence* of a frame, which is the whole claim in several tests.
async fn drain<R: AsyncReadExt + Unpin>(stream: &mut R, settle: Duration) -> String {
    let mut seen = String::new();
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + settle;
    loop {
        let Ok(Ok(count)) = tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await else {
            return seen;
        };
        if count == 0 {
            return seen;
        }
        seen.push_str(&String::from_utf8_lossy(&chunk[..count]));
    }
}

/// Reads whatever upstream has for us and answers every liveness probe in it.
///
/// The owner sends `PING :bouncer-<generation>` on its own schedule and treats a PONG
/// that does not match as a protocol failure, so a fake upstream that ignores it ends
/// the generation for a reason that has nothing to do with the claim under test.
async fn settle(upstream: &mut ScriptedStream) -> String {
    let mut seen = String::new();
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        let Ok(Ok(count)) = tokio::time::timeout_at(deadline, upstream.read(&mut chunk)).await
        else {
            return seen;
        };
        if count == 0 {
            return seen;
        }
        let text = String::from_utf8_lossy(&chunk[..count]).into_owned();
        seen.push_str(&text);
        for token in probe_tokens(&text) {
            upstream
                .write_all(format!("PONG :{token}\r\n").as_bytes())
                .await
                .expect("upstream answers the probe");
        }
    }
}

/// The generation liveness tokens named in `text`.
/// The generation liveness tokens named in `text`, reconstructed in full.
///
/// The whole token has to come back, not the part after the prefix: a PONG answers a
/// PING only when it carries the identical token, and a truncated one ends the
/// generation as a protocol failure.
fn probe_tokens(text: &str) -> Vec<String> {
    const PREFIX: &str = "PING :bouncer-";
    text.split("\r\n")
        .filter_map(|line| line.strip_prefix(PREFIX))
        .map(|token| format!("bouncer-{token}"))
        .collect()
}

/// The compatibility shorthand tokens, pinned.
///
/// These are the exact trailing parameters `SessionReader` matches. A rename on one
/// side only would silently turn every detach request back into an ordinary part, which
/// no client would notice until the channel failed to disappear.
#[test]
fn the_compatibility_shorthand_tokens_are_exact() {
    assert_eq!(DETACH_SHORTHAND, "detach");
    assert_eq!(ATTACH_SHORTHAND, "attach");
    assert_ne!(
        DETACH_SHORTHAND, ATTACH_SHORTHAND,
        "the two directions must not share a token"
    );
}

// ------------------------------------------------------------------- presence

#[tokio::test]
async fn an_explicit_manual_away_outranks_every_session_count() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    let mut first = harness.attach(1, &[]).await;
    let _second = harness.attach(2, &[]).await;
    harness.settle_presence(None).await;

    first.send("AWAY :lunch\r\n").await;
    // Two clients are attached and auto-away is on, but an explicit away is a decision
    // and a count is not: upstream is told the Operator's own words.
    let seen = read_until(harness.upstream(upstream), b"AWAY :lunch\r\n").await;
    assert!(seen.contains("AWAY :lunch"), "{seen}");

    first.send("AWAY\r\n").await;
    let seen = read_until(harness.upstream(upstream), b"AWAY\r\n").await;
    // Clearing hands control back to the policy, which still has two active sessions.
    assert!(
        seen.contains("AWAY\r\n") && !seen.contains("AWAY :lunch"),
        "a bare AWAY is the protocol's way of saying back: {seen}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn the_last_active_detach_triggers_auto_away_exactly_once() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    let mut first = harness.attach(1, &[]).await;
    let _second = harness.attach(2, &[]).await;
    // The first attach returns the bouncer; that transition is the baseline the
    // assertion below is measured against.
    harness.settle_presence(None).await;
    assert_eq!(
        harness.snapshot.borrow().away,
        None,
        "two active clients means the Operator is present"
    );

    // Losing one of two active sessions is not a presence change, and saying so
    // silently is the point: a bouncer that goes away when a second window closes
    // would flap on every window switch.
    first.send("QUIT\r\n").await;
    harness.wait_away(None).await;
    let seen = drain(harness.upstream(upstream), Duration::from_millis(250)).await;
    assert!(
        !seen.contains("AWAY"),
        "one of two active sessions leaving is not a presence change: {seen}"
    );

    drop(_second);
    let seen = read_until(harness.upstream(upstream), b"AWAY").await;
    assert!(
        seen.contains(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT),
        "losing the last active session makes the bouncer away: {seen}"
    );
    assert_eq!(
        harness.snapshot.borrow().attached_sessions,
        0,
        "and nothing is left to make it present"
    );
    let repeated = drain(harness.upstream(upstream), Duration::from_millis(300)).await;
    assert!(
        !repeated.contains("AWAY"),
        "the away state is emitted on the transition and never again: {repeated}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn a_passive_session_does_not_clear_auto_away() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    // The bouncer arrived away: auto-away is on and nobody is attached yet.
    assert_eq!(
        harness.snapshot.borrow().away.as_deref(),
        Some(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT),
        "an auto-away Network with no clients is away before anyone connects"
    );

    // A background history sync connects and declares itself passive during
    // registration. It is a socket, and it is not the Operator at a keyboard.
    harness
        .attach_passive(1, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    harness
        .wait_away(Some(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT))
        .await;
    let seen = drain(harness.upstream(upstream), Duration::from_millis(300)).await;
    assert!(
        !seen.contains("AWAY"),
        "a passive client must not produce an upstream transition at all: {seen}"
    );

    // A second passive client changes nothing either.
    harness
        .attach_passive(2, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    harness
        .wait_away(Some(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT))
        .await;
    let seen = drain(harness.upstream(upstream), Duration::from_millis(300)).await;
    assert!(
        !seen.contains("AWAY"),
        "another passive client changes nothing upstream: {seen}"
    );
    assert_eq!(
        harness.snapshot.borrow().active_sessions,
        0,
        "presence is aggregated from session classification, not from the socket count"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn a_first_active_attach_clears_only_the_automatic_away() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    harness
        .attach_passive(1, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    harness
        .wait_away(Some(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT))
        .await;

    // An active client arriving is a presence change, and only a presence change.
    let _foreground = harness
        .attach(2, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    harness.wait_away(None).await;
    let seen = read_until(harness.upstream(upstream), b"AWAY\r\n").await;
    assert!(
        seen.contains("AWAY\r\n"),
        "an active session returns the bouncer, and the bare AWAY is the protocol's way \
         of saying back: {seen}"
    );

    // Now a manual away, and an unrelated passive client attaching on top of it.
    harness.settle_presence(None).await;
    let mut foreground = harness
        .attach(3, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    foreground.send("AWAY :head down\r\n").await;
    read_until(harness.upstream(upstream), b"AWAY :head down\r\n").await;
    harness.wait_away(Some("head down")).await;

    harness
        .attach_passive(4, &[i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY])
        .await;
    let seen = drain(harness.upstream(upstream), Duration::from_millis(400)).await;
    assert!(
        !seen.contains("AWAY"),
        "an unrelated session attaching must not clear an explicit manual away: {seen}"
    );
    assert_eq!(
        harness.snapshot.borrow().away.as_deref(),
        Some("head down"),
        "and the Operator's own words are what upstream still believes"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn auto_away_off_means_no_upstream_away_traffic_at_all() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "").await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("005 bot CLIENTTAGDENY=*").await;
    harness.settle_presence(None).await;
    client.send("QUIT\r\n").await;
    // The session goes away; without the policy nothing is written upstream.
    let seen = drain(harness.upstream(0), Duration::from_millis(500)).await;
    assert!(
        !seen.contains("AWAY"),
        "a migrated Network must not begin emitting AWAY traffic: {seen}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn an_unbounded_or_empty_away_message_is_refused() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("005 bot CLIENTTAGDENY=*").await;
    harness.settle_presence(None).await;

    let huge = "x".repeat(i2pr_irc_runtime::session::MAX_AWAY_TEXT_BYTES + 1);
    client.send(&format!("AWAY :{huge}\r\n")).await;
    // The reader refuses the frame, so the session ends rather than forwarding an
    // Operator string that never met the ceiling. The session ending is what the bouncer
    // reports upstream afterwards: with nobody left attached, automatic away is correct.
    client.drain(Duration::from_millis(600)).await;
    let seen = drain(harness.upstream(0), Duration::from_millis(300)).await;
    assert!(
        !seen.contains(&huge),
        "an away message past the ceiling never reaches an upstream-visible field: {seen}"
    );
    assert!(
        !seen.contains(&"x".repeat(40)),
        "nor does any prefix of it: {seen}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn pre_away_is_only_honoured_from_a_client_that_negotiated_it() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    // A client that never asked for the draft cannot silence the Operator's presence
    // with a command whose consequences it does not know.
    let mut client = harness.attach(1, &[]).await;
    client.until("005 bot CLIENTTAGDENY=*").await;
    harness.settle_presence(None).await;
    let mark = client.mark();
    client.send("PASSIVE\r\n").await;
    client.drain(Duration::from_millis(600)).await;
    let seen = client.since(mark);
    assert!(
        !seen.contains(i2pr_irc_runtime::presence::AUTO_AWAY_TEXT),
        "an unnegotiated PASSIVE has no away effect: {seen}"
    );
    let _ = upstream;

    harness.shutdown().await;
}

// ------------------------------------------------------- collision and fallback

/// Drives a registration the server keeps refusing, recording every `NICK` it wrote.
async fn collide(harness: &mut Harness, refusals: usize) -> Vec<String> {
    let mut upstream = harness.provider.take_peer().await;
    let opening = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
    let first = nick_written(&opening);
    let isupport = harness.isupport.clone();
    upstream
        .write_all(format!(":srv CAP * LS :\r\n{isupport}").as_bytes())
        .await
        .expect("upstream accepts the capability offer");
    // The first attempt is refused here rather than in the loop, because that attempt was
    // written before the loop existed to read it.
    upstream
        .write_all(format!(":srv 433 * {first} :Nickname is already in use\r\n").as_bytes())
        .await
        .expect("upstream refuses the nick");
    let mut seen = String::new();
    seen.push_str(&first);
    seen.push('\n');
    for _ in 1..refusals {
        let frame = read_until(&mut upstream, b"NICK ").await;
        let written = nick_written(&frame);
        upstream
            .write_all(format!(":srv 433 * {written} :Nickname is already in use\r\n").as_bytes())
            .await
            .expect("upstream refuses the nick");
        seen.push_str(&written);
        seen.push('\n');
    }
    harness.upstreams.push(upstream);
    seen.lines().map(|line| line.to_owned()).collect()
}

#[tokio::test]
async fn a_nick_collision_is_answered_rather_than_waited_out() {
    let (mut harness, _) = Harness::build(&[], Policy::default(), "").await;
    let mut upstream = harness.provider.take_peer().await;
    read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
    upstream
        .write_all(b":srv CAP * LS :\r\n:srv 433 * bot :Nickname is already in use\r\n")
        .await
        .expect("upstream refuses the nick");
    // The point of the test: the fallback arrives within the registration window rather
    // than after the ceiling, which would be indistinguishable from a dead network.
    let seen = read_until(&mut upstream, b"NICK bot_1\r\n").await;
    assert!(seen.contains("NICK bot_1"), "{seen}");
    assert_eq!(
        harness.snapshot.borrow().last_error,
        None,
        "a collision is an answer, not an error"
    );
    assert_eq!(
        harness.snapshot.borrow().phase,
        Some(i2pr_irc_runtime::owner::Phase::Registering),
        "the generation is still registering, not backing off"
    );
    harness.upstreams.push(upstream);
    harness.shutdown().await;
}

#[tokio::test]
async fn the_fallback_sequence_is_bounded_deterministic_and_distinct() {
    let (mut harness, _) = Harness::build(&[], Policy::default(), "").await;
    let attempts = collide(&mut harness, 3).await;
    assert_eq!(
        attempts,
        vec!["bot", "bot_1", "bot_2"],
        "the sequence is a function of the configured nick alone"
    );
    let mut unique = attempts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), attempts.len(), "and never repeats itself");
    harness.shutdown().await;
}

#[tokio::test]
async fn exhausting_the_fallback_is_terminal_rather_than_a_retry_storm() {
    let (mut harness, _) = Harness::build(&[], Policy::default(), "").await;
    let attempts = collide(
        &mut harness,
        i2pr_irc_runtime::presence::MAX_FALLBACK_NICK_ATTEMPTS,
    )
    .await;
    assert_eq!(
        attempts.len(),
        i2pr_irc_runtime::presence::MAX_FALLBACK_NICK_ATTEMPTS,
        "one attempt per candidate and no more"
    );

    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if harness.snapshot.borrow().phase == Some(i2pr_irc_runtime::owner::Phase::Stopped) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "exhaustion stops the Network: it does not retry a sequence that has already \
             been refused once per candidate"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        harness.snapshot.borrow().last_error,
        Some("nick exhausted"),
        "and the reason an operator would look for is recorded"
    );
    let later = drain(harness.upstream(0), Duration::from_millis(400)).await;
    assert!(
        !later.contains("NICK"),
        "a terminal Network writes nothing further upstream: {later}"
    );
    harness.shutdown().await;
}

// ------------------------------------------------------------ keep-nick reclaim

/// Registers a generation whose preferred nick the server refuses exactly once.
///
/// Reclaim only has anything to do when the bouncer ends up holding a *different* nick,
/// and only a real collision gets it there: the owner writes `NICK bot`, the server
/// answers `433`, the owner falls back, and the server accepts. Every reclaim test starts
/// from that state rather than pretending the bouncer chose a different nick.
async fn register_under_collision(harness: &mut Harness) {
    let index = harness.bring_online_collision().await;
    let frame = read_until(harness.upstream(index), b"NICK ").await;
    let landed = nick_written(&frame);
    assert_eq!(
        landed, "bot_1",
        "the first fallback is a function of the configured nick alone"
    );
    harness
        .upstream(index)
        .write_all(format!(":srv 001 {landed} :welcome\r\n").as_bytes())
        .await
        .expect("upstream accepts the fallback nick");
    harness.downstream_nick = landed;
}

#[tokio::test]
async fn keep_nick_disabled_emits_no_reclaim_traffic_at_all() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    register_under_collision(&mut harness).await;

    // The bouncer now holds `bot_1` and is not configured to want `bot` back.
    let _client = harness.attach(1, &[]).await;
    harness.wait_away(None).await;
    let seen = drain(harness.upstream(0), Duration::from_millis(500)).await;
    assert!(
        !seen.contains("MONITOR") && !seen.contains("ISON") && !seen.contains("NICK bot"),
        "a Network that never asked for keep-nick never probes: {seen}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn a_server_offering_monitor_is_asked_about_the_preferred_nick_once() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "MONITOR=4").await;
    register_under_collision(&mut harness).await;

    let seen = read_until(harness.upstream(0), b"MONITOR + bot\r\n").await;
    assert!(
        seen.contains("MONITOR + bot"),
        "the preferred nick is watched once, by name: {seen}"
    );
    assert!(
        !seen.contains("ISON"),
        "a server that offers MONITOR is not also probed with ISON: {seen}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn a_server_without_monitor_is_probed_bounded_and_not_on_client_activity() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "").await;
    register_under_collision(&mut harness).await;

    let seen = read_until(harness.upstream(0), b"ISON bot\r\n").await;
    assert!(
        seen.contains("ISON bot"),
        "a server with no MONITOR limit is asked the standard question: {seen}"
    );

    // Local activity must never make the bouncer ask again. This is the whole reason the
    // reclaim clock is generation-owned and independent of session events.
    let mut client = harness.attach(1, &[]).await;
    let nick = harness.downstream_nick.clone();
    client.until(&format!("005 {nick} CLIENTTAGDENY=*")).await;
    client.send("CAP LS\r\n").await;
    client.send("PING :client\r\n").await;
    client.drain(Duration::from_millis(400)).await;
    harness.drain_upstream().await;
    let seen = drain(harness.upstream(0), Duration::from_millis(400)).await;
    assert!(
        !seen.contains("ISON"),
        "a client attaching and talking must never make the bouncer poll faster: {seen}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn a_disabled_monitor_falls_back_to_probing_rather_than_waiting_forever() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "MONITOR=0").await;
    register_under_collision(&mut harness).await;
    let seen = read_until(harness.upstream(0), b"ISON bot\r\n").await;
    assert!(
        seen.contains("ISON bot"),
        "MONITOR=0 means the feature is disabled, so waiting for its notifications would \\
         wait forever: {seen}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn monitor_evidence_reclaims_the_preferred_nick() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "MONITOR=4").await;
    register_under_collision(&mut harness).await;
    read_until(harness.upstream(0), b"MONITOR + bot\r\n").await;

    // The server says the preferred nick is free. That is evidence, so the reclaim write
    // is due immediately rather than at the end of the schedule.
    harness
        .upstream(0)
        .write_all(b":srv 731 bot :bot\r\n")
        .await
        .expect("server reports the nick free");
    let seen = read_until(harness.upstream(0), b"NICK bot\r\n").await;
    assert!(
        seen.contains("NICK bot"),
        "the preferred nick is claimed once evidence says it is free: {seen}"
    );

    // The write is a request; the server's own frame is what confirms it, and holding the
    // nick means there is nothing further to reclaim.
    harness
        .upstream(0)
        .write_all(b":srv NICK bot :bot\r\n")
        .await
        .expect("server confirms the nick");
    let after = drain(harness.upstream(0), Duration::from_millis(300)).await;
    assert!(
        !after.contains("NICK bot"),
        "having reclaimed it, the bouncer stops asking: {after}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn monitor_online_is_not_free_evidence_and_offline_lists_match_by_nick() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "MONITOR=4").await;
    register_under_collision(&mut harness).await;
    read_until(harness.upstream(0), b"MONITOR + bot\r\n").await;

    harness
        .upstream(0)
        .write_all(b":srv 730 bot :bot!user@host\r\n")
        .await
        .expect("server reports preferred nick online");
    let online = drain(harness.upstream(0), Duration::from_millis(300)).await;
    assert!(
        !online.contains("NICK bot"),
        "730 online evidence must not trigger a reclaim: {online}"
    );

    harness
        .upstream(0)
        .write_all(b":srv 731 bot :other,bot\r\n")
        .await
        .expect("server reports a comma-separated offline list");
    let offline = read_until(harness.upstream(0), b"NICK bot\r\n").await;
    assert!(
        offline.contains("NICK bot"),
        "731 offline lists containing the preferred nick trigger reclaim: {offline}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn reclaim_writes_are_capped_per_generation() {
    use i2pr_irc_runtime::presence::{
        MAX_RECLAIM_WRITES_PER_GENERATION, RECLAIM_INTERVAL, ReclaimAttempt, note_reclaim_write,
    };
    let mut attempt = ReclaimAttempt::new("bot", "bot_1");
    let mut writes = 0;
    for _ in 0..MAX_RECLAIM_WRITES_PER_GENERATION + 3 {
        if attempt.should_write(RECLAIM_INTERVAL, RECLAIM_INTERVAL)
            && note_reclaim_write(&mut attempt, MAX_RECLAIM_WRITES_PER_GENERATION)
        {
            writes += 1;
        }
    }
    assert_eq!(
        writes, MAX_RECLAIM_WRITES_PER_GENERATION,
        "a reclaim that could write forever is indistinguishable from a stuck client"
    );
}

#[tokio::test]
async fn a_replaced_generations_reclaim_state_cannot_act_on_its_replacement() {
    let (mut harness, _) = Harness::build(&[], Policy::KEEP_NICK, "MONITOR=4").await;
    register_under_collision(&mut harness).await;
    read_until(harness.upstream(0), b"MONITOR + bot\r\n").await;

    // The generation is replaced while its reclaim attempt is live. Dropping the scripted
    // peer is what replaces it: the socket closes exactly as a real disconnect would.
    harness.upstreams.clear();
    let mut next = harness.provider.take_peer().await;
    read_until(&mut next, b"USER user 0 * :bouncer\r\n").await;
    next.write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await
        .expect("upstream accepts registration");
    harness.upstreams.push(next);

    // This generation got the nick it asked for, so it has no reclaim attempt at all.
    // Evidence arriving now must not resurrect the attempt the previous generation owned:
    // a surviving attempt would make one Network hold two live claims for the same nick,
    // and the loser would be whichever connection the server answered first.
    harness
        .upstream(0)
        .write_all(b":srv 731 bot :bot\r\n")
        .await
        .expect("server reports the nick free");
    let seen = drain(harness.upstream(0), Duration::from_millis(500)).await;
    assert!(
        !seen.contains("NICK") && !seen.contains("MONITOR") && !seen.contains("ISON"),
        "reclaim state is generation-owned and dies with the connection that created it: {seen}"
    );
    harness.shutdown().await;
}

// --------------------------------------------------------- client independence

#[tokio::test]
async fn the_upstream_capability_fingerprint_does_not_depend_on_attached_clients() {
    let (mut harness, _) = Harness::build(&[], Policy::BOTH, "").await;
    harness.bring_online().await;
    let empty = harness.snapshot.borrow().upstream_capabilities.clone();

    let _plain = harness.attach(1, &[]).await;
    let _rich = harness
        .attach(
            2,
            &[
                i2pr_irc_runtime::capability::MESSAGE_TAGS,
                i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY,
            ],
        )
        .await;
    let busy = harness.snapshot.borrow().upstream_capabilities.clone();
    assert_eq!(
        empty, busy,
        "upstream negotiation happens once per generation; which clients happen to be \
         connected cannot be part of it"
    );
    for token in [
        i2pr_irc_runtime::capability::MESSAGE_TAGS,
        i2pr_irc_runtime::presence::PRE_AWAY_CAPABILITY,
    ] {
        assert!(
            !empty.contains(&token.to_owned()),
            "{token} is a downstream capability and is never requested upstream"
        );
    }
    harness.shutdown().await;
}

#[tokio::test]
async fn a_manual_away_survives_a_reconnect_and_is_re_applied_upstream() {
    let (mut harness, _) = Harness::build(&[], Policy::AUTO_AWAY, "").await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("005 bot CLIENTTAGDENY=*").await;
    harness.settle_presence(None).await;
    client.send("AWAY :out for lunch\r\n").await;
    read_until(harness.upstream(upstream), b"AWAY :out for lunch\r\n").await;

    // The generation ends and a new one starts. Dropping the previous peer is what ends
    // the generation; the replacement is registered and becomes index 0, so the
    // assertions below watch the connection the bouncer is on now.
    harness.upstreams.clear();
    let mut next = harness.provider.take_peer().await;
    read_until(&mut next, b"USER user 0 * :bouncer\r\n").await;
    next.write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await
        .expect("upstream accepts registration");
    harness.upstreams.push(next);

    // The new generation has never been told anything about our away state, so saying it
    // again is a real transition rather than a repeat of something the server knows.
    let seen = read_until(harness.upstream(0), b"AWAY :out for lunch\r\n").await;
    assert!(
        seen.contains("AWAY :out for lunch"),
        "the Operator's away is re-applied, not restored from a connection that is gone: {seen}"
    );
    harness.shutdown().await;
}

#[test]
fn no_host_or_environment_value_can_reach_a_nick_or_an_away_message() {
    // The fallback generator's only inputs are the configured nick and the server's
    // advertised length. A source-level guard proves there is no code path that could
    // reach anything else, which a runtime test cannot prove: the absence of a value is
    // not observable from outside.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/presence.rs"),
    )
    .expect("presence module is readable");
    // Comments are prose and may legitimately name what the module refuses to read, so
    // the guard reads code only.
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "std::env",
        "env::var",
        "hostname",
        "HOSTNAME",
        "process::id",
        "whoami",
        "gethostname",
    ] {
        assert!(
            !code.contains(forbidden),
            "presence.rs must not be able to reach {forbidden}"
        );
    }
    // The generator itself takes exactly two arguments, which is the same statement from
    // the other direction.
    assert!(
        code.contains("pub fn new(preferred: &str, advertised: Option<usize>)"),
        "the fallback sequence is built from the configured nick and the server's length \
         and nothing else"
    );
}
