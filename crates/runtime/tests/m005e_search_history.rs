//! Plan 024 — M005-E indexed history search and CHATHISTORY completion.
//!
//! The store and adapter suites cover the parse and query shapes. This suite covers the
//! things only a live session can answer:
//!
//! - a client that negotiated `soju.im/search` gets bounded results framed in a batch;
//! - a client that did not is refused, because the frames would be unsolicited;
//! - a result set never crosses a Network boundary, and never reaches another client;
//! - retention removes what a search can find;
//! - a restart answers identically, because the index is durable rather than rebuilt;
//! - `CHATHISTORY AROUND` brackets a selector at every edge, including one older than
//!   anything retained.
//!
//! Every assertion here is about what a real client would observe. A test that only
//! checked the store would pass whether or not the owner ever routed the request, and
//! that routing is the part this plan changes.

use std::sync::Arc;
use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{ControlSnapshot, RuntimeControlHandle, RuntimeController};
use i2pr_irc_runtime::search::{ADAPTER_REVISION, DEFAULT_SEARCH_LIMIT, SEARCH_BATCH_TYPE};
use i2pr_irc_store::{NetworkRecord, Store, StoreHandle, StorePath};
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

/// A provider that hands out scripted streams to both the runtime and the test.
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
    upstreams: Vec<ScriptedStream>,
}

impl Runtime {
    async fn start() -> Self {
        let store = Store::open(&StorePath::Memory).expect("store opens");
        let handle = store.handle_clone();
        Self::start_with(store, handle).await
    }

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

    /// Stops the controller while leaving the store open, so a restart can reopen it.
    async fn stop_keeping_store(self) -> (Store, StoreHandle) {
        self.control.request_stop();
        self.task
            .await
            .expect("controller task joins")
            .expect("controller reports success");
        let _ = self.upstreams;
        self.store
    }

    async fn stop(self) {
        let (store, _) = self.stop_keeping_store().await;
        store.shutdown().expect("store shuts down");
    }

    /// Creates a Network, parks its upstream peer, and waits for the owner to go live.
    /// Creates a Network, parks its upstream peer, waits for the owner to go live, and
    /// has the upstream self-join every channel it is configured to track.
    ///
    /// The self-join is what makes a channel's lines history-eligible: a buffer only
    /// exists once the Network has been observed in it, so a suite that skipped it would
    /// be searching an empty journal and passing for the wrong reason.
    async fn bring_online(&mut self, network: u64, channels: &[&str]) -> usize {
        self.control
            .create(record(network, channels))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .expect("upstream accepts registration");
        read_until(&mut upstream, b"CAP END\r\n").await;
        for channel in channels {
            upstream
                .write_all(format!(":bot!u@h JOIN {channel}\r\n").as_bytes())
                .await
                .expect("upstream joins");
        }
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
        })
        .await;
        peer
    }

    /// Waits for an already-durable Network to come back up after a restart.
    ///
    /// The controller rebuilds Networks from storage, so this must not try to create one
    /// again: a second `create` for the same identity is a configuration error, not a
    /// no-op, and failing on it would test the controller rather than the index.
    async fn reconnect(&mut self, network: u64) -> usize {
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .expect("upstream accepts registration");
        read_until(&mut upstream, b"CAP END\r\n").await;
        upstream
            .write_all(b":bot!u@h JOIN #room\r\n")
            .await
            .expect("upstream joins");
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
        })
        .await;
        peer
    }

    /// Delivers upstream history through one named connection.
    async fn say(&mut self, peer: usize, channel: &str, nick: &str, text: &str, at: &str) {
        self.upstreams[peer]
            .write_all(format!("@time={at} :{nick}!u@h PRIVMSG {channel} :{text}\r\n").as_bytes())
            .await
            .expect("upstream writes history");
    }

    /// Delivers one line carrying a `msgid` as well as a timestamp.
    async fn say_with_msgid(
        &mut self,
        peer: usize,
        channel: &str,
        text: &str,
        msgid: &str,
        at: &str,
    ) {
        self.upstreams[peer]
            .write_all(
                format!("@time={at};msgid={msgid} :alice!u@h PRIVMSG {channel} :{text}\r\n")
                    .as_bytes(),
            )
            .await
            .expect("upstream writes history");
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

async fn wait_for_history(runtime: &Runtime, network: NetworkId, targets: &[(&str, usize)]) {
    let mut buffers = Vec::with_capacity(targets.len());
    for (target, _) in targets {
        buffers.push(
            runtime
                .store
                .1
                .resolve_buffer(network, i2pr_irc_store::BufferKind::Channel, target)
                .await
                .expect("history buffer resolves")
                .buffer,
        );
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut ready = true;
            for ((_, minimum), buffer) in targets.iter().zip(&buffers) {
                let events = runtime
                    .store
                    .1
                    .query_history(&i2pr_irc_store::HistoryQuery {
                        buffer: *buffer,
                        bound: i2pr_irc_store::HistoryQueryBound {
                            after: None,
                            before: None,
                            limit: i2pr_irc_store::MAX_HISTORY_QUERY_EVENTS,
                        },
                    })
                    .await
                    .expect("history query succeeds");
                ready &= events.len() >= *minimum;
            }
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("upstream events reach durable history");
}

struct Client {
    end: ScriptedStream,
    _script: i2pr_irc_testkit::FaultController,
    outcome: tokio::task::JoinHandle<AdmissionOutcome>,
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
    /// Reads for a bounded window without waiting for a frame that may never come.
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

    /// Drops the socket and reports how admission ended.
    async fn finish(self) -> AdmissionOutcome {
        drop(self.end);
        tokio::time::timeout(CEILING, self.outcome)
            .await
            .expect("admission ends when the client disconnects")
            .expect("admission task joins")
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
        outcome,
        seen: String::new(),
    }
}

/// Registers one client, negotiating whatever capabilities the test names.
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
    client
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

/// Every search result line in a reply, in order.
///
/// Counts results rather than batch openings: the opening frame is framing, not an
/// answer, and a test that counted it would assert one more than it means to.
fn result_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.contains("SOV SEARCH"))
        .map(str::to_owned)
        .collect()
}

/// Asserts one reply is a single complete, empty search batch.
fn assert_empty_batch(reply: &str) {
    assert!(
        result_lines(reply).is_empty(),
        "an empty result carries no results: {reply:?}"
    );
    assert_eq!(
        reply.matches("BATCH +").count(),
        1,
        "an empty result is still one opened batch: {reply:?}"
    );
    let close = reply
        .rfind("BATCH -")
        .unwrap_or_else(|| panic!("the batch was never closed: {reply:?}"));
    let open = reply
        .find("BATCH +")
        .unwrap_or_else(|| panic!("the batch was never opened: {reply:?}"));
    assert!(open < close, "the batch closes after it opens: {reply:?}");
}

// ----------------------------------------------------------------- tests

#[tokio::test]
async fn a_search_finds_retained_messages_and_answers_in_one_batch() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "the quick brown fox",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "bob",
            "lazy dog sleeping",
            "2026-01-01T00:00:01.000Z",
        )
        .await;
    // A JOIN is stored-adjacent traffic that must never be searchable: widening the
    // surface to "anything the bouncer saw" would let a client find events the plan
    // never claimed it stored.
    runtime.upstreams[0]
        .write_all(b":alice!u@h JOIN #room\r\n")
        .await
        .expect("upstream writes");

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search batch").await;
    // Drain the live fan-out first. The upstream messages above are still reaching this
    // socket, and asserting against a buffer that still contains them would test what
    // the client was fanned rather than what it was told.
    client.settle().await;
    let mark = client.mark();
    client.send("SEARCH in=#room text=quick\r\n").await;
    client.await_new(mark, "BATCH -").await;
    let reply = client.since(mark);
    assert_eq!(
        result_lines(&reply).len(),
        1,
        "one request is answered by exactly one batch: {reply:?}"
    );
    assert!(
        reply.contains("the quick brown fox"),
        "the matching message is returned: {reply:?}"
    );
    assert!(
        !reply.contains("lazy dog"),
        "a non-matching retained message is not: {reply:?}"
    );
    assert!(
        !reply.contains("JOIN"),
        "a JOIN is not searchable: {reply:?}"
    );
    assert!(
        reply.contains(SEARCH_BATCH_TYPE),
        "the batch names its own type, so a client can tell a search reply from any other: \
         {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_with_no_matches_is_a_complete_empty_batch() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "nothing like this",
            "2026-01-01T00:00:00.000Z",
        )
        .await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    client.send("SEARCH in=#room text=absent\r\n").await;
    client.await_new(0, "BATCH -").await;
    let reply = client.since(0);
    // A client that heard nothing would have to decide whether to wait. A closed empty
    // batch is the only answer that ends the question.
    assert_empty_batch(&reply);
    assert!(
        !reply.contains("absent"),
        "an empty result reports nothing: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_client_that_did_not_negotiate_search_is_refused_rather_than_answered() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "findable",
            "2026-01-01T00:00:00.000Z",
        )
        .await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mark = client.mark();
    client.send("SEARCH in=#room text=findable\r\n").await;
    client.await_new(mark, "421 ").await;
    let reply = client.since(mark);
    assert!(
        !reply.contains("findable") && !reply.contains("BATCH +"),
        "an unnegotiated client receives no results and no unsolicited batch: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_refused_search_says_why_and_still_closes_its_batch() {
    let mut runtime = Runtime::start().await;
    runtime.bring_online(1, &["#room"]).await;
    let mut client = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "soju.im/search standard-replies",
    )
    .await;

    for hostile in [
        "colour=red",
        "text",
        "after=yesterday",
        "limit=0",
        "in=#nowhere",
        "text=hello*",
    ] {
        let mark = client.mark();
        client.send(&format!("SEARCH {hostile}\r\n")).await;
        client.await_new(mark, "BATCH -").await;
        let reply = client.since(mark);
        assert!(
            reply.contains("FAIL SEARCH"),
            "{hostile:?} is refused explicitly: {reply:?}"
        );
        assert_empty_batch(&reply);
    }
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_never_crosses_a_network_or_reaches_another_client() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    let peer_two = runtime.bring_online(2, &["#room"]).await;
    runtime
        .say(
            peer_two,
            "#room",
            "carol",
            "network two entirely",
            "2026-01-01T00:00:02.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "network one only",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    wait_for_history(&runtime, NetworkId(1), &[("#room", 1)]).await;
    wait_for_history(&runtime, NetworkId(2), &[("#room", 1)]).await;

    let mut first = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mut second = register(&runtime, NetworkId(2), SessionId(2), "soju.im/search").await;

    let mark_first = first.mark();
    first.send("SEARCH text=only\r\n").await;
    first.await_new(mark_first, "BATCH -").await;
    assert!(
        first.since(mark_first).contains("network one only"),
        "the owning Network finds its own message: {:?}",
        first.since(mark_first)
    );

    let mark_second = second.mark();
    second.send("SEARCH text=only\r\n").await;
    second.await_new(mark_second, "BATCH -").await;
    let other = second.since(mark_second);
    assert!(
        !other.contains("network one only"),
        "a different Network cannot read this Network's history: {other:?}"
    );
    assert_empty_batch(&other);

    // The reply is addressed to the session that asked, not fanned out.
    let idle_mark = second.mark();
    first.send("SEARCH text=one\r\n").await;
    first.await_new(0, "network one only").await;
    second.settle().await;
    assert!(
        !second.since(idle_mark).contains("network one only"),
        "one client's search never reaches another client's socket: {:?}",
        second.since(idle_mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_spans_several_buffers_of_one_network() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room", "#other"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "shared word here",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#other",
            "alice",
            "shared word there",
            "2026-01-01T00:00:01.000Z",
        )
        .await;
    wait_for_history(&runtime, NetworkId(1), &[("#room", 1), ("#other", 1)]).await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let both = client.mark();
    client.send("SEARCH text=shared\r\n").await;
    client.await_new(both, "BATCH -").await;
    assert_eq!(
        result_lines(&client.since(both)).len(),
        2,
        "one hit from each buffer: {:?}",
        client.since(both)
    );

    let scoped = client.mark();
    client.send("SEARCH in=#other text=shared\r\n").await;
    client.await_new(scoped, "BATCH -").await;
    let reply = client.since(scoped);
    assert!(
        reply.contains("shared word there") && !reply.contains("shared word here"),
        "an `in=` selector narrows the result to the named buffer: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_scoped_by_sender_and_time_answers_exactly_what_was_asked() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "needle first",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "bob",
            "needle second",
            "2026-01-01T00:00:05.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "needle third",
            "2026-01-01T00:00:10.000Z",
        )
        .await;
    wait_for_history(&runtime, NetworkId(1), &[("#room", 3)]).await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    // Drain the live fan-out first. The upstream messages above are still reaching this
    // socket, and asserting against a buffer that still contains them would test what
    // the client was fanned rather than what it was told.
    client.settle().await;

    let by_sender = client.mark();
    client
        .send("SEARCH in=#room from=alice text=needle\r\n")
        .await;
    client.await_new(by_sender, "BATCH -").await;
    let reply = client.since(by_sender);
    assert_eq!(result_lines(&reply).len(), 2, "{reply:?}");
    assert!(
        reply.contains("needle first") && reply.contains("needle third"),
        "{reply:?}"
    );
    assert!(!reply.contains("needle second"), "{reply:?}");

    // Half-open `[after, before)`: the lower endpoint is inside, the upper is not.
    let by_time = client.mark();
    client
        .send("SEARCH in=#room after=2026-01-01T00:00:00.000Z before=2026-01-01T00:00:10.000Z text=needle\r\n")
        .await;
    client.await_new(by_time, "BATCH -").await;
    let reply = client.since(by_time);
    assert!(
        reply.contains("needle first") && reply.contains("needle second"),
        "the window includes its lower endpoint: {reply:?}"
    );
    assert!(
        !reply.contains("needle third"),
        "and excludes its upper endpoint: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_result_is_bounded_by_its_limit() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    for index in 0..40 {
        runtime
            .say(
                peer,
                "#room",
                "alice",
                &format!("haystack {index}"),
                "2026-01-01T00:00:00.000Z",
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mark = client.mark();
    client
        .send("SEARCH in=#room text=haystack limit=5\r\n")
        .await;
    client.await_new(mark, "BATCH -").await;
    assert_eq!(
        result_lines(&client.since(mark)).len(),
        5,
        "the limit is the limit, whatever the journal holds: {:?}",
        client.since(mark)
    );

    // A default is a default, not an absence of one.
    let defaulted = client.mark();
    client.send("SEARCH in=#room text=haystack\r\n").await;
    client.await_new(defaulted, "BATCH -").await;
    let defaulted_count = result_lines(&client.since(defaulted)).len();
    assert!(
        defaulted_count > 5 && defaulted_count <= DEFAULT_SEARCH_LIMIT,
        "an unstated limit still applies, and never exceeds the whole journal: {defaulted_count} \
         of at most {DEFAULT_SEARCH_LIMIT} in {:?}",
        client.since(defaulted)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_never_becomes_an_expression_evaluator() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "harmless words",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    wait_for_history(&runtime, NetworkId(1), &[("#room", 1)]).await;

    let mut client = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "soju.im/search standard-replies",
    )
    .await;
    // Drain the live fan-out first. The upstream messages above are still reaching this
    // socket, and asserting against a buffer that still contains them would test what
    // the client was fanned rather than what it was told.
    client.settle().await;
    for hostile in [
        r#"text=harmless" OR 1=1 --"#,
        "text=NEAR(a b)",
        "text=a*",
        "text=col:val",
        "text=' OR ''='",
        "text=x; DROP TABLE history_events",
    ] {
        let mark = client.mark();
        client.send(&format!("SEARCH {hostile}\r\n")).await;
        client.await_new(mark, "BATCH -").await;
        let reply = client.since(mark);
        assert!(
            reply.contains("FAIL SEARCH") && !reply.contains("harmless words"),
            "{hostile:?} is refused rather than evaluated: {reply:?}"
        );
    }
    // The database is untouched: a search is a read, and a refused one reads nothing.
    let mark = client.mark();
    client.send("SEARCH in=#room text=harmless\r\n").await;
    client.await_new(mark, "BATCH -").await;
    assert!(
        client.since(mark).contains("harmless words"),
        "history is still there after every hostile term: {:?}",
        client.since(mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn retention_removes_what_a_search_can_find() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "ephemeral words",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "durable words",
            "2026-01-01T00:00:01.000Z",
        )
        .await;
    let buffer = runtime
        .store
        .1
        .resolve_buffer(NetworkId(1), i2pr_irc_store::BufferKind::Channel, "#room")
        .await
        .expect("channel buffer resolves")
        .buffer;
    let retained = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let retained = runtime
                .store
                .1
                .query_history(&i2pr_irc_store::HistoryQuery {
                    buffer,
                    bound: i2pr_irc_store::HistoryQueryBound {
                        after: None,
                        before: None,
                        limit: 10,
                    },
                })
                .await
                .expect("history query");
            if retained.len() == 2 {
                break retained;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both upstream events reached history");
    assert_eq!(retained.len(), 2, "both upstream events reached history");

    // The store is driven directly here: retention is an Operator action, not something
    // a client can cause over the wire.
    let boundary = runtime
        .store
        .1
        .recent_targets(
            NetworkId(1),
            i2pr_irc_wire::IrcTimestamp::parse(b"2026-01-01T00:00:00.500Z").expect("parses"),
            i2pr_irc_wire::IrcTimestamp::parse(b"9999-12-31T23:59:59.999Z").expect("parses"),
            1,
        )
        .await
        .expect("targets read");
    let cutoff = boundary
        .first()
        .map(|target| target.newest_event)
        .expect("the newest event in the window");
    let report = runtime
        .store
        .1
        .retain(&i2pr_irc_store::RetentionRequest {
            network: NetworkId(1),
            before: cutoff,
            max_delete: 64,
        })
        .await
        .expect("retention runs");
    assert_eq!(report.deleted, 1, "only the earlier message was removed");

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mark = client.mark();
    client.send("SEARCH in=#room text=ephemeral\r\n").await;
    client.await_new(mark, "BATCH -").await;
    assert!(
        !client.since(mark).contains("ephemeral words"),
        "a message the Operator had deleted cannot still be found: {:?}",
        client.since(mark)
    );

    let kept = client.mark();
    client.send("SEARCH in=#room text=durable\r\n").await;
    client.await_new(kept, "BATCH -").await;
    assert!(
        client.since(kept).contains("durable words"),
        "and the retained message still is: {:?}",
        client.since(kept)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_restarted_runtime_answers_the_same_search_identically() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    runtime
        .say(
            peer,
            "#room",
            "alice",
            "first retained",
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    runtime
        .say(
            peer,
            "#room",
            "bob",
            "second retained",
            "2026-01-01T00:00:01.000Z",
        )
        .await;
    wait_for_history(&runtime, NetworkId(1), &[("#room", 2)]).await;

    let mut before = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mark = before.mark();
    before.send("SEARCH in=#room text=retained\r\n").await;
    before.await_new(mark, "BATCH -").await;
    let first = result_lines(&before.since(mark))
        .iter()
        .map(|line| strip_batch_prefix(line))
        .collect::<Vec<_>>();
    assert_eq!(first.len(), 2, "{first:?}");
    // Closing the client is what ends its admission, so it must actually go away before
    // the controller is stopped.
    let _ = before.finish().await;

    // The same store, reopened by a new controller. An index that had to be rebuilt from
    // the retained rows would answer here, and so would an index that had silently lost
    // them; only a durable one gives the same answer.
    let (store, handle) = runtime.stop_keeping_store().await;
    let mut restarted = Runtime::start_with(store, handle).await;
    restarted.reconnect(1).await;

    let mut after = register(&restarted, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mark = after.mark();
    after.send("SEARCH in=#room text=retained\r\n").await;
    after.await_new(mark, "BATCH -").await;
    let second = result_lines(&after.since(mark))
        .iter()
        .map(|line| strip_batch_prefix(line))
        .collect::<Vec<_>>();
    assert_eq!(
        first, second,
        "a restart does not change what history contains"
    );
    restarted.stop().await;
}

/// Removes the per-batch identifier from a line so two replies can be compared.
///
/// The batch id is deliberately generation-local, so it differs between runs by design.
/// Comparing it would fail for the right reason and hide the thing under test, which is
/// that the *results* are identical.
fn strip_batch_prefix(line: &str) -> String {
    match line.split_once(' ') {
        Some((_, rest)) if rest.starts_with("BATCH +") => rest
            .split_once(' ')
            .map(|(_, tail)| tail.to_owned())
            .unwrap_or_else(|| rest.to_owned()),
        _ => line.to_owned(),
    }
}

#[tokio::test]
async fn around_brackets_a_selector_at_every_edge_of_the_retained_window() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    for index in 0..9 {
        runtime
            .say(
                peer,
                "#room",
                "alice",
                &format!("message {index}"),
                &format!("2026-01-01T00:00:0{index}.000Z"),
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut client = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "draft/chathistory batch",
    )
    .await;

    // On an event: the bracket is the event plus its neighbours, never the whole buffer.
    let at = client.mark();
    client
        .send("CHATHISTORY AROUND #room timestamp=2026-01-01T00:00:04.000Z 5\r\n")
        .await;
    client.await_new(at, "BATCH -").await;
    let reply = client.since(at);
    for index in 2..=6 {
        assert!(
            reply.contains(&format!("message {index}")),
            "message {index} is inside the bracket: {reply:?}"
        );
    }
    assert!(
        !reply.contains("message 0") && !reply.contains("message 8"),
        "and nothing outside it is: {reply:?}"
    );

    // Past the newest event: the window clamps to the end rather than failing.
    let past = client.mark();
    client
        .send("CHATHISTORY AROUND #room timestamp=9999-01-01T00:00:00.000Z 3\r\n")
        .await;
    client.await_new(past, "BATCH -").await;
    let reply = client.since(past);
    assert!(
        reply.contains("message 8"),
        "a reference past the end still answers from the newest page: {reply:?}"
    );

    // Before the oldest retained event: the budget is spent where the messages are.
    let before = client.mark();
    client
        .send("CHATHISTORY AROUND #room timestamp=1970-01-01T00:00:00.000Z 3\r\n")
        .await;
    client.await_new(before, "BATCH -").await;
    let reply = client.since(before);
    assert!(
        reply.contains("message 0") && reply.contains("message 2"),
        "a reference before the beginning answers with the oldest page: {reply:?}"
    );
    assert!(
        !reply.contains("message 8"),
        "and does not silently fall back to the newest: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn msgid_and_timestamp_references_resolve_through_the_index() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    for index in 0..5 {
        runtime
            .say_with_msgid(
                peer,
                "#room",
                &format!("message {index}"),
                &format!("id{index}"),
                &format!("2026-01-01T00:00:0{index}.000Z"),
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut client = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "draft/chathistory batch standard-replies",
    )
    .await;

    let by_msgid = client.mark();
    client.send("CHATHISTORY AFTER #room msgid=id1 2\r\n").await;
    client.await_new(by_msgid, "BATCH -").await;
    let reply = client.since(by_msgid);
    assert!(
        reply.contains("message 2") && reply.contains("message 3"),
        "a msgid reference resolves to a durable position: {reply:?}"
    );
    assert!(
        !reply.contains("message 1"),
        "and the selector itself is excluded: {reply:?}"
    );

    let unknown = client.mark();
    client
        .send("CHATHISTORY AFTER #room msgid=never-issued 2\r\n")
        .await;
    client.await_new(unknown, "FAIL").await;
    assert!(
        client.since(unknown).contains("No retained message"),
        "an id nothing carries is reported as unknown rather than as a failure to read: {:?}",
        client.since(unknown)
    );

    // A duplicate id is a real condition, not a lookup failure, and it gets its own
    // answer. Picking the lowest local id would answer a question the client did not ask
    // while looking authoritative.
    for body in ["duplicate one", "duplicate two"] {
        runtime
            .say_with_msgid(peer, "#room", body, "shared-id", "2026-01-01T00:00:06.000Z")
            .await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let ambiguous = client.mark();
    client
        .send("CHATHISTORY AFTER #room msgid=shared-id 2\r\n")
        .await;
    client.await_new(ambiguous, "FAIL").await;
    assert!(
        client
            .since(ambiguous)
            .contains("More than one retained message"),
        "a duplicate id is reported as ambiguous rather than resolved: {:?}",
        client.since(ambiguous)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn heavy_history_search_does_not_starve_upstream_liveness() {
    let mut runtime = Runtime::start().await;
    let peer = runtime.bring_online(1, &["#room"]).await;
    for index in 0..60 {
        runtime
            .say(
                peer,
                "#room",
                "alice",
                &format!("load {index}"),
                "2026-01-01T00:00:00.000Z",
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    // Two broad searches back to back, with a PING arriving in between. Storage work is
    // behind one bounded worker; if a search could hold it, the PONG would wait.
    for _ in 0..2 {
        client.send("SEARCH in=#room text=load\r\n").await;
    }
    let upstream = &mut runtime.upstreams[0];
    upstream
        .write_all(b"PING :srv\r\n")
        .await
        .expect("upstream pings");
    read_until(upstream, b"PONG").await;
    client.await_new(0, "BATCH -").await;
    runtime.stop().await;
}

#[tokio::test]
async fn the_adapter_states_the_draft_surface_it_implements() {
    // The exact revision is part of the deliverable, not a comment. A future draft that
    // adds a selector must not be able to land without someone noticing this changed.
    assert!(
        ADAPTER_REVISION.contains("soju.im/search"),
        "the revision must name the draft it implements: {ADAPTER_REVISION:?}"
    );
    assert!(
        !ADAPTER_REVISION.contains("MATCH"),
        "raw FTS syntax must never be part of the advertised surface: {ADAPTER_REVISION:?}"
    );
}
