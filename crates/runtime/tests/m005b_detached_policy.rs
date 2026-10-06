//! M005-B qualification: durable detached-channel policy.
//!
//! One claim is under test here.
//!
//! *Hiding a channel is not leaving it.* A desired channel can be hidden from every
//! attached session while the bouncer stays joined upstream and keeps collecting bounded
//! history; the policy that decides that is durable, so it survives a restart; and
//! reattaching restores truthful current state and non-duplicating bounded history
//! through the client's existing cursor rather than through a second history model.
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
    attached_channels, testing as store_testing,
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
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(endpoint).await
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
    network: u64,
    channels: Vec<DesiredChannelRecord>,
}

impl Harness {
    async fn start(channels: &[DesiredChannelRecord]) -> (Self, Arc<Mutex<usize>>) {
        Self::build(channels).await
    }

    async fn build(channels: &[DesiredChannelRecord]) -> (Self, Arc<Mutex<usize>>) {
        let (store, handle) = store();
        handle
            .save_network(&record(1, channels))
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
            record: Arc::new(record(1, channels)),
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
                network: 1,
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
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
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

        let mut registration = Vec::from(&b"NICK bot\r\nUSER user 0 * :phone\r\n"[..]);
        if !caps.is_empty() {
            registration.extend_from_slice(format!("CAP REQ :{}\r\n", caps.join(" ")).as_bytes());
            registration.extend_from_slice(b"CAP END\r\n");
        }
        client_side
            .write_all(&registration)
            .await
            .expect("client registers");
        let mut client = Client {
            stream: client_side,
            seen: String::new(),
        };
        client.until("005 bot CLIENTTAGDENY=*").await;
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

    async fn wait_detached(&mut self, count: u64) {
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if self.snapshot.borrow().channels_detached == count {
                return;
            }
            let _ = tokio::time::timeout_at(deadline, self.snapshot.changed())
                .await
                .expect("the owner reports the expected detach");
        }
    }

    async fn wait_reattached(&mut self, count: u64) {
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if self.snapshot.borrow().channels_reattached == count {
                return;
            }
            let _ = tokio::time::timeout_at(deadline, self.snapshot.changed())
                .await
                .expect("the owner reports the expected reattach");
        }
    }

    /// The durable record for one desired channel.
    async fn durable(&self, channel: &str) -> DesiredChannelRecord {
        self.store
            .1
            .load_networks()
            .await
            .expect("networks load")
            .into_iter()
            .find(|entry| entry.network == NetworkId(self.network))
            .and_then(|entry| {
                entry
                    .desired_channels
                    .into_iter()
                    .find(|entry| entry.target.eq_ignore_ascii_case(channel))
            })
            .unwrap_or_else(|| panic!("{channel} is still a desired channel"))
    }

    /// Every durable history event this Network recorded.
    async fn history_len(&self, buffer: i2pr_irc_core::BufferId) -> usize {
        self.store
            .1
            .query_history(&i2pr_irc_store::HistoryQuery {
                buffer,
                bound: i2pr_irc_store::HistoryQueryBound {
                    after: None,
                    before: None,
                    limit: 64,
                },
            })
            .await
            .expect("history query")
            .len()
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

/// Pushes a chat line from the fake upstream and waits until the owner has processed it.
///
/// Waiting on the owner's own PONG is what makes a negative assertion meaningful: the
/// owner has read everything before that point, so a frame that never arrived as live
/// fanout never arrived at all.
async fn push_chat(upstream: &mut ScriptedStream, channel: &str, text: &str) {
    upstream
        .write_all(format!(":alice!a@h PRIVMSG {channel} :{text}\r\n").as_bytes())
        .await
        .expect("upstream accepts chat");
    upstream
        .write_all(b"PING :probe\r\n")
        .await
        .expect("upstream accepts a probe");
    let _ = read_until(upstream, b"PONG :probe\r\n").await;
    settle(upstream).await;
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
// ------------------------------------------------------- projection visibility

#[tokio::test]
async fn a_detached_channel_is_omitted_from_a_later_clients_projection() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#visible", "#hidden"])).await;
    harness.bring_online().await;

    let mut first = harness.attach(1, &[]).await;
    let projection = first.until("366 bot #hidden").await;
    assert!(
        projection.contains(":bot JOIN #visible"),
        "an attached channel is projected: {projection}"
    );
    assert!(
        projection.contains(":bot JOIN #hidden"),
        "before any detach, every desired channel is shown: {projection}"
    );

    first
        .send(&format!("PART #hidden :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    let synthetic = first.until("PART #hidden").await;
    assert!(
        synthetic.contains(":bouncer PART #hidden"),
        "the transition is visibly bouncer-owned, not attributed to a person: {synthetic}"
    );

    // A client that connects after the detach must not learn the channel exists.
    let mut second = harness.attach(2, &[]).await;
    let projection = second.until("366 bot #visible").await;
    assert!(
        !projection.contains("#hidden"),
        "a fresh client is shown no trace of a detached channel: {projection}"
    );
    assert!(
        projection.contains(":bot JOIN #visible"),
        "the channel that is not detached is still shown: {projection}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn one_client_detaching_a_channel_hides_it_for_every_attached_session() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;

    let mut requester = harness.attach(1, &[]).await;
    let mut bystander = harness.attach(2, &[]).await;
    requester.until("366 bot #room").await;
    bystander.until("366 bot #room").await;

    requester
        .send(&format!("PART #room :{DETACH_SHORTHAND}\r\n"))
        .await;

    // Both sessions are told. A client that stayed silent would keep showing a channel it
    // can no longer see, with no explanation for it disappearing.
    for (name, client) in [("requester", &mut requester), ("bystander", &mut bystander)] {
        let seen = client.until("PART #room").await;
        assert!(
            seen.contains(":bouncer PART #room"),
            "{name} is told the channel was detached by bouncer policy: {seen}"
        );
    }
    assert!(
        harness.snapshot.borrow().attached_sessions == 2,
        "detaching is not a client detach: both sessions stay attached"
    );

    harness.shutdown().await;
}

// --------------------------------------------------- upstream and history truth

#[tokio::test]
async fn a_detached_channel_stays_joined_upstream_and_keeps_collecting_history() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;

    // Two chat lines before the detach, so the buffer exists and has content in it.
    push_chat(harness.upstream(upstream), "#room", "before one").await;
    push_chat(harness.upstream(upstream), "#room", "before two").await;
    client
        .send(&format!("PART #room :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #room").await;

    // No upstream PART is ever written: the bouncer is still in the room.
    let seen = drain(harness.upstream(upstream), Duration::from_millis(400)).await;
    assert!(
        !seen.contains("PART #room"),
        "detaching never leaves the room upstream: {seen}"
    );

    let mark = client.mark();
    push_chat(harness.upstream(upstream), "#room", "while detached").await;
    client.drain(Duration::from_millis(400)).await;
    let live = client.since(mark);
    assert!(
        !live.contains("while detached"),
        "detached traffic is not live-fanned out: {live}"
    );

    // ...but it is still recorded, which is the whole reason detaching is not leaving.
    let buffer = harness
        .store
        .1
        .resolve_buffer(NetworkId(1), i2pr_irc_store::BufferKind::Channel, "#room")
        .await
        .expect("channel buffer")
        .buffer;
    assert_eq!(
        harness.history_len(buffer).await,
        3,
        "all three lines are durable: hiding a channel downstream does not destroy it"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn a_quit_naming_a_detached_channel_alongside_a_visible_one_is_redacted() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#open", "#secret"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #secret").await;
    client
        .send(&format!("PART #secret :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #secret").await;
    client.drain(Duration::from_millis(200)).await;
    // Everything before this point legitimately names the channel: it was attached.
    let mark = client.mark();

    harness
        .upstream(upstream)
        .write_all(b":alice!a@h QUIT :#secret,#open\r\n")
        .await
        .expect("server sends a quit");
    harness
        .upstream(upstream)
        .write_all(b"PING :probe\r\n")
        .await
        .expect("upstream accepts a probe");
    read_until(harness.upstream(upstream), b"PONG :probe\r\n").await;

    client.drain(Duration::from_millis(400)).await;
    let seen = client.since(mark);
    assert!(
        seen.contains("QUIT"),
        "a quit that concerns a visible channel is still delivered: {seen}"
    );
    assert!(
        !seen.contains("#secret"),
        "the detached channel's name is removed rather than delivered: {seen}"
    );
    assert!(
        seen.contains("#open"),
        "the visible channel is left in place, because the frame is about both: {seen}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn a_channel_scoped_event_about_a_detached_channel_is_not_fanned_out() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#open", "#secret"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #secret").await;
    client
        .send(&format!("PART #secret :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #secret").await;
    client.drain(Duration::from_millis(200)).await;
    let mark = client.mark();

    for frame in [
        ":alice!a@h TOPIC #secret :a new topic\r\n",
        ":alice!a@h MODE #secret +o bob\r\n",
        ":alice!a@h JOIN #secret\r\n",
        ":alice!a@h PART #secret\r\n",
    ] {
        harness
            .upstream(upstream)
            .write_all(frame.as_bytes())
            .await
            .expect("upstream accepts the frame");
    }
    push_chat(harness.upstream(upstream), "#open", "unrelated").await;

    client.drain(Duration::from_millis(400)).await;
    let seen = client.since(mark);
    assert!(
        seen.contains("unrelated"),
        "an ordinary visible channel is unaffected: {seen}"
    );
    assert!(
        !seen.contains("a new topic"),
        "topic state for a detached channel is withheld: {seen}"
    );
    assert!(
        !seen.contains("+o bob"),
        "mode state for a detached channel is withheld: {seen}"
    );

    harness.shutdown().await;
}

// ------------------------------------------------------------- reattach truth

#[tokio::test]
async fn reattaching_projects_truthful_state_before_any_history() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    push_chat(harness.upstream(upstream), "#room", "one").await;
    push_chat(harness.upstream(upstream), "#room", "two").await;
    client
        .send(&format!("PART #room :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #room").await;
    client.drain(Duration::from_millis(200)).await;

    client
        .send(&format!("PART #room :{ATTACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_reattached(1).await;
    let seen = client.drain(Duration::from_millis(600)).await;

    let join = seen
        .find("JOIN #room")
        .expect("the channel is joined again");
    let first_message = seen.find("one").expect("retained history follows");
    assert!(
        join < first_message,
        "current state is projected before history, so a client never sees a replay \
         before the state it replays into: {seen}"
    );
    assert!(
        seen.contains(":bouncer JOIN #room"),
        "the synthetic reattach is visibly bouncer-owned: {seen}"
    );
    assert!(
        !(harness.durable("#room").await).detached,
        "the durable policy was cleared"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn reattaching_a_channel_the_server_has_parted_joins_before_it_is_projected() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    client
        .send(&format!("PART #room :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #room").await;

    // The server parts the bouncer while the channel is hidden. The client never learns
    // this: it was not watching that channel and must not be told about it.
    let mark = client.mark();
    harness
        .upstream(upstream)
        .write_all(b":srv KICK #room bot :bye\r\n")
        .await
        .expect("upstream parts the bouncer");
    push_chat(harness.upstream(upstream), "#room", "after the kick").await;
    client.drain(Duration::from_millis(300)).await;
    let quiet = client.since(mark);
    assert!(
        !quiet.contains("after the kick"),
        "traffic for a channel the bouncer no longer holds stays hidden: {quiet}"
    );

    client
        .send(&format!("PART #room :{ATTACH_SHORTHAND}\r\n"))
        .await;

    // Reattaching with no observed membership writes a JOIN; nothing is projected until
    // the server confirms, so membership is never fabricated.
    let seen = read_until(harness.upstream(upstream), b"JOIN #room\r\n").await;
    assert!(!seen.contains("366"), "no projection precedes membership");
    client.drain(Duration::from_millis(300)).await;
    let early = client.since(mark);
    assert!(
        !early.contains("JOIN #room"),
        "the client is not told it joined a channel the bouncer has not joined: {early}"
    );

    // Marked here, not earlier: the registration projection legitimately contained a
    // `332` for this same channel, and reusing it would prove nothing about the reveal.
    let reveal = client.mark();
    harness
        .upstream(upstream)
        .write_all(b":bot JOIN #room\r\n:srv 332 bot #room :welcome back\r\n")
        .await
        .expect("server confirms membership with a topic");
    harness
        .upstream(upstream)
        .write_all(b"PING :probe\r\n")
        .await
        .expect("upstream accepts a probe");
    read_until(harness.upstream(upstream), b"PONG :probe\r\n").await;

    client.until("welcome back").await;
    let seen = client.since(reveal);
    assert!(
        seen.contains(":bouncer JOIN #room"),
        "the reveal is bouncer-owned and happens once membership is observed: {seen}"
    );
    assert!(
        seen.contains("welcome back"),
        "the projection carries the topic the server actually sent: {seen}"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn reattaching_never_sends_an_upstream_part_or_leaves_durable_intent() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;

    client
        .send(&format!("PART #room :{DETACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_detached(1).await;
    client.until("PART #room").await;
    client
        .send(&format!("PART #room :{ATTACH_SHORTHAND}\r\n"))
        .await;
    harness.wait_reattached(1).await;
    client.drain(Duration::from_millis(400)).await;

    let seen = drain(harness.upstream(upstream), Duration::from_millis(400)).await;
    assert!(
        !seen.contains("PART #room"),
        "neither direction of the policy writes an upstream PART: {seen}"
    );
    assert_eq!(
        harness.durable("#room").await,
        DesiredChannelRecord::at("#room", 0, false),
        "the channel is still one desired channel, at its original position"
    );

    harness.shutdown().await;
}

// --------------------------------------------------------------- leave semantics

#[tokio::test]
async fn an_ordinary_part_still_leaves_upstream_and_forgets_desired_intent() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    let upstream = harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;

    client.send("PART #room :goodbye\r\n").await;
    let seen = read_until(harness.upstream(upstream), b"PART #room\r\n").await;
    assert!(
        seen.contains("PART #room"),
        "an ordinary part is still sent upstream: {seen}"
    );
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        let channels = harness
            .store
            .1
            .load_networks()
            .await
            .expect("networks load")[0]
            .desired_channels
            .clone();
        if channels.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "durable desired membership is forgotten, not left behind"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    harness.shutdown().await;
}

#[tokio::test]
async fn detaching_a_channel_this_network_does_not_hold_changes_nothing() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    client.drain(Duration::from_millis(200)).await;
    let mark = client.mark();

    client.send("PART #ghost :detach\r\n").await;
    let all = client.until("NOTICE bot").await;
    let seen = &all[mark..];
    assert!(
        seen.contains("does not hold that channel"),
        "the request is refused in terms the client can act on, not silently dropped: {seen}"
    );
    assert!(
        !seen.contains("PART #ghost"),
        "no synthetic transition is emitted for a channel that was never shown: {seen}"
    );
    assert_eq!(
        harness.snapshot.borrow().channels_detached,
        0,
        "nothing was detached"
    );

    harness.shutdown().await;
}

// --------------------------------------------------------- ambiguous commits

#[tokio::test]
async fn a_store_refusal_applies_no_live_detach_transition() {
    let (mut harness, ambiguous) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    client.drain(Duration::from_millis(200)).await;

    // The durable policy changes underneath the owner, and the *reply* to the owner's own
    // write is lost: it cannot tell whether its mutation landed, so it must re-read.
    harness
        .store
        .1
        .set_desired_channel_detached(NetworkId(1), "#room", true)
        .await
        .expect("the durable policy is changed underneath the owner");
    *ambiguous.lock().await = 1;
    let mark = client.mark();

    client.send("PART #room :detach\r\n").await;
    let all = client.until("NOTICE bot").await;
    let seen = &all[mark..];
    assert!(
        seen.contains("re-read durable state"),
        "the client is told the decision came from a re-read rather than from the \
         outcome this process happened to observe: {seen}"
    );
    assert!(
        seen.contains(":bouncer PART #room"),
        "durable state said detached, so presentation follows it: {seen}"
    );
    assert_eq!(harness.snapshot.borrow().channels_detached, 1);

    harness.shutdown().await;
}

#[tokio::test]
async fn an_indeterminate_detach_commit_that_did_not_land_changes_nothing() {
    let (mut harness, ambiguous) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    client.drain(Duration::from_millis(200)).await;

    // The owner answers `CommitState::Unknown` without applying the mutation, so the
    // re-read it is obliged to perform finds the channel still attached.
    *ambiguous.lock().await = 1;
    let mark = client.mark();

    client.send("PART #room :detach\r\n").await;
    let all = client.until("NOTICE bot").await;
    let seen = &all[mark..];
    assert!(
        !seen.contains(":bouncer PART #room"),
        "no live transition is claimed for a policy that did not change: {seen}"
    );
    assert_eq!(harness.snapshot.borrow().channels_detached, 0);
    assert!(
        !harness.durable("#room").await.detached,
        "durable state is unchanged, and that is what presentation followed"
    );

    harness.shutdown().await;
}

// --------------------------------------------------------- restart and reconnect

#[tokio::test]
async fn a_detach_survives_a_process_restart_and_is_reapplied_without_being_asked() {
    let durable = i2pr_irc_store::DesiredChannelRecord::at("#room", 0, true);
    let (mut first, _) = Harness::start(std::slice::from_ref(&durable)).await;
    first.bring_online().await;
    let mut client = first.attach(1, &[]).await;
    let projection = client.until("005 bot CLIENTTAGDENY=*").await;
    assert!(
        !projection.contains("#room"),
        "a policy read from storage is applied before any client is told anything: {projection}"
    );

    // A fresh process over the same durable state: nothing re-asks for the detach.
    first.shutdown().await;

    let (store, handle) = store();
    handle
        .save_network(&record(1, &[durable]))
        .await
        .expect("durable record saves");
    let provider = Arc::new(FakeI2pStreamProvider::default());
    for _ in 0..4 {
        provider
            .queue_outcome(Ok(FaultScript::default()))
            .expect("provider queue has room");
    }
    let reconnect = ReconnectScheduler::default();
    let resources = ResourceLedger::new(reconnect.clone(), handle.clone());
    let context = SupervisorContext {
        network: NetworkId(1),
        record: Arc::new(record(1, &[DesiredChannelRecord::at("#room", 0, true)])),
        store: handle.clone(),
        status: watch::channel(Default::default()).0,
        resources,
    };
    let owner = NetworkOwner::new(Shared(provider.clone()), context, handle.clone(), reconnect)
        .expect("owner constructs");
    let (_commands, command_rx) = mpsc::channel(64);
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move { owner.serve(command_rx, stop_rx).await });
    let mut restored = provider.take_peer().await;
    read_until(&mut restored, b"USER user 0 * :bouncer\r\n").await;
    restored
        .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await
        .expect("upstream accepts registration");
    // One read covers registration completion and the JOIN that follows it: a scripted
    // stream can deliver both in one chunk, so reading for them separately loses the
    // second frame.
    let joined = read_until(&mut restored, b"JOIN #room\r\n").await;
    assert!(
        joined.contains("JOIN #room"),
        "durable desired membership is restored across a restart, detached or not: {joined}"
    );
    assert!(
        joined.contains("CAP END"),
        "and registration completed normally first: {joined}"
    );
    restored
        .write_all(b":bot JOIN #room\r\n")
        .await
        .expect("server confirms membership");
    push_chat(&mut restored, "#room", "after restart").await;

    let _ = stop.send(true);
    let _ = tokio::time::timeout(CEILING, task).await;
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_reconnect_rejoins_a_detached_channel_and_keeps_it_hidden() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;
    client.send("PART #room :detach\r\n").await;
    harness.wait_detached(1).await;
    client.until("PART #room").await;
    client.drain(Duration::from_millis(200)).await;

    // The generation ends. The bouncer does not.
    harness.upstreams.clear();
    let mut next = harness.provider.take_peer().await;
    read_until(&mut next, b"USER user 0 * :bouncer\r\n").await;
    next.write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await
        .expect("upstream accepts registration");
    harness.upstreams.push(next);

    // A fresh generation re-reads the durable policy rather than the record its owner was
    // born with, so a detach performed mid-generation is restored here too.
    let joined = read_until(harness.upstream(0), b"JOIN #room\r\n").await;
    assert!(
        joined.contains("JOIN #room"),
        "the channel is rejoined upstream after a reconnect: {joined}"
    );
    assert!(
        joined.contains("CAP END"),
        "the new generation completed registration first: {joined}"
    );
    harness
        .upstream(0)
        .write_all(b":bot JOIN #room\r\n")
        .await
        .expect("server confirms membership");
    let mark = client.mark();
    push_chat(harness.upstream(0), "#room", "after reconnect").await;

    client.drain(Duration::from_millis(400)).await;
    let seen = client.since(mark);
    assert!(
        !seen.contains("after reconnect"),
        "the channel stays hidden after a reconnect: {seen}"
    );
    assert!(
        !seen.contains("JOIN #room"),
        "and nothing announces the channel either: {seen}"
    );

    harness.shutdown().await;
}

// ------------------------------------------------------------- bounds and cost

#[tokio::test]
async fn repeated_detach_and_reattach_leaves_every_queue_and_count_settled() {
    let (mut harness, _) = Harness::start(&attached_channels(&["#room"])).await;
    harness.bring_online().await;
    let mut client = harness.attach(1, &[]).await;
    client.until("366 bot #room").await;

    for round in 0..12u64 {
        client.send("PART #room :detach\r\n").await;
        harness.wait_detached(round + 1).await;
        client.send("PART #room :attach\r\n").await;
        harness.wait_reattached(round + 1).await;
    }
    // Let the last projection drain before the gauges are read.
    client.drain(Duration::from_millis(400)).await;

    let snapshot = harness.snapshot.borrow().clone();
    assert_eq!(snapshot.channels_detached, 12);
    assert_eq!(snapshot.channels_reattached, 12);
    assert_eq!(
        snapshot.attached_sessions, 1,
        "no client was lost along the way"
    );
    assert_eq!(
        snapshot.upstream_normal_queue_depth, 0,
        "no upstream intent accumulated: neither policy direction writes upstream"
    );
    assert_eq!(
        snapshot.upstream_control_queue_depth, 0,
        "synthetic transitions live on the downstream side, so the upstream control \
         queue is untouched by them"
    );
    assert_eq!(
        snapshot.response_routes, 0,
        "no reply route was opened for a policy change"
    );
    assert!(
        !harness.durable("#room").await.detached,
        "the durable policy ends where it started"
    );

    harness.shutdown().await;
}
