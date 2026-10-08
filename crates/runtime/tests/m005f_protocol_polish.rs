//! Plan 025 — M005-F downstream IRCv3 protocol polish.
//!
//! The other M005 suites cover the surfaces they introduced. This one covers the
//! *boundary between them*: a capability that is advertised, negotiated per session, and
//! then actually honoured for that session and no other.
//!
//! Each capability here has a failure mode that is invisible from a single session:
//!
//! - `server-time` — a client with `message-tags` but without it must lose the `time`
//!   tag and keep every other tag. Two states would either strip what it asked for or
//!   deliver what it did not.
//! - `standard-replies` — a client that never negotiated it must receive a numeric, not
//!   an unrequested `FAIL` frame.
//! - `draft/no-implicit-names` — membership must be omitted for the client that asked,
//!   and still delivered to the client that did not, on the same Network.
//! - `cap-notify` — a server capability change must reach only the sessions that asked.
//! - `echo-message` — a local message must become history on the upstream echo, as one
//!   event, labelled as outgoing.

use std::sync::Arc;
use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{ControlSnapshot, RuntimeControlHandle, RuntimeController};
use i2pr_irc_store::{
    BufferKind, HistoryQuery, HistoryQueryBound, NetworkRecord, Store, StorePath,
};
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

struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    upstreams: Vec<ScriptedStream>,
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
            _store: store,
        }
    }

    /// Brings a Network online with `cap_ls` offered upstream, which is what decides
    /// whether `echo-message` is available to serve downstream.
    async fn bring_online(&mut self, network: u64, channels: &[&str], cap_ls: &str) -> usize {
        self.control
            .create(record(network, channels))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        let ack = cap_ls
            .split_whitespace()
            .map(|name| name.to_owned())
            .collect::<Vec<_>>()
            .join(" ");
        upstream
            .write_all(format!(":srv CAP * LS :{ack}\r\n:srv 001 bot :welcome\r\n").as_bytes())
            .await
            .expect("upstream accepts registration");
        // The bouncer requests what it wants and waits for the ACK. Answering with
        // whatever it asked for -- rather than with the whole advertisement -- keeps the
        // test honest about which capabilities were actually granted, since an ACK that
        // names a capability the bouncer never requested would be answered by a
        // different branch of registration than the real one.
        let mut output = String::new();
        loop {
            let line = read_line(&mut upstream).await;
            output.push_str(&line);
            if line.contains("CAP END") {
                break;
            }
            if let Some(request) = line.strip_prefix("CAP REQ :") {
                let requested = request.trim_end_matches("\r\n");
                upstream
                    .write_all(format!(":srv CAP * ACK :{requested}\r\n").as_bytes())
                    .await
                    .expect("upstream acks");
            }
        }
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

    async fn reconnect_peer(&mut self, network: u64, channels: &[&str], cap_ls: &str) -> String {
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(format!(":srv CAP * LS :{cap_ls}\r\n").as_bytes())
            .await
            .expect("replacement upstream offers capabilities");
        let mut output = String::new();
        loop {
            let line = read_line(&mut upstream).await;
            output.push_str(&line);
            if line.contains("CAP END") {
                break;
            }
            if let Some(request) = line.strip_prefix("CAP REQ :") {
                let requested = request.trim_end_matches("\r\n");
                upstream
                    .write_all(format!(":srv CAP * ACK :{requested}\r\n").as_bytes())
                    .await
                    .expect("replacement upstream acks");
            }
        }
        upstream
            .write_all(b":srv 001 bot :welcome\r\n")
            .await
            .expect("replacement upstream welcomes");
        for channel in channels {
            output.push_str(&read_until(&mut upstream, format!("JOIN {channel}").as_bytes()).await);
        }
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
        })
        .await;
        let mut bytes = [0u8; 1024];
        while let Ok(Ok(count)) =
            tokio::time::timeout(Duration::from_millis(20), upstream.read(&mut bytes)).await
        {
            if count == 0 {
                break;
            }
            output.push_str(&String::from_utf8_lossy(&bytes[..count]));
        }
        self.upstreams.push(upstream);
        output
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

fn admit_as(runtime: &Runtime, network: NetworkId, session: SessionId, client: ClientId) -> Client {
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
            client,
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
    register_as(runtime, network, session, capabilities, ClientId(7)).await
}

async fn register_as(
    runtime: &Runtime,
    network: NetworkId,
    session: SessionId,
    capabilities: &str,
    client_id: ClientId,
) -> Client {
    let mut client = admit_as(runtime, network, session, client_id);
    let request = if capabilities.is_empty() {
        "NICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n".to_owned()
    } else {
        format!("CAP REQ :{capabilities}\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n")
    };
    client.send(&request).await;
    client.until("001 bot").await;
    client
}

/// Reads exactly one CRLF-terminated line from the upstream peer.
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

async fn upstream_barrier(runtime: &mut Runtime, peer: usize, token: &str) {
    runtime.upstreams[peer]
        .write_all(format!(":srv PING :{token}\r\n").as_bytes())
        .await
        .expect("upstream writes ordering barrier");
    let response = read_until(
        &mut runtime.upstreams[peer],
        format!("PONG :{token}").as_bytes(),
    )
    .await;
    assert!(response.contains(&format!("PONG :{token}")), "{response:?}");
}

fn parsed_line_with(text: &str, marker: &str) -> i2pr_irc_wire::Message {
    let line = text
        .lines()
        .find(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("no frame containing {marker:?}: {text:?}"));
    i2pr_irc_wire::Message::parse(format!("{line}\r\n").as_bytes())
        .expect("rendered IRC frame parses")
}

// ----------------------------------------------------------------- tests

#[tokio::test]
async fn the_advertised_set_names_exactly_what_this_build_serves() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mark = client.mark();
    client.send("CAP LS\r\n").await;
    client.await_new(mark, "CAP bot LS").await;
    let listing = client.since(mark);

    for served in [
        "server-time",
        "standard-replies",
        "cap-notify",
        "draft/no-implicit-names",
        "message-tags",
        "batch",
        "labeled-response",
    ] {
        assert!(
            listing.contains(served),
            "{served} is served downstream and must be advertised: {listing:?}"
        );
    }
    assert!(
        !listing.contains("echo-message"),
        "upstream did not negotiate echo-message, so the bouncer must not offer it: {listing:?}"
    );
    assert!(
        !listing.to_ascii_lowercase().contains("otr"),
        "OTR is opaque payload carried by endpoints, not a bouncer IRC capability: {listing:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn otr_queries_fragments_and_whitespace_remain_opaque_across_tags_fanout_and_history() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time account-tag batch labeled-response",
        )
        .await;
    upstream_barrier(&mut runtime, peer, "m009a-otr-ready").await;

    let mut modern = register_as(
        &runtime,
        NetworkId(1),
        SessionId(11),
        "message-tags server-time account-tag",
        ClientId(101),
    )
    .await;
    let mut passive = register_as(
        &runtime,
        NetworkId(1),
        SessionId(12),
        "message-tags",
        ClientId(102),
    )
    .await;
    let mut legacy = register_as(&runtime, NetworkId(1), SessionId(13), "", ClientId(103)).await;
    for client in [&mut modern, &mut passive, &mut legacy] {
        client.settle().await;
    }
    let marks = [modern.mark(), passive.mark(), legacy.mark()];

    // Synthetic query/start form with exact trailing TAB/SPACE bytes. The extra
    // opaque tag and the body are deliberately unrelated; neither is interpreted.
    let body = b"?OTRv3?\t \x20 ";
    let mut incoming =
        b"@account=alice;time=2026-01-01T00:00:00.000Z;+custom=kept :alice!u@h PRIVMSG #room :"
            .to_vec();
    incoming.extend_from_slice(body);
    incoming.extend_from_slice(b"\r\n");
    runtime.upstreams[peer]
        .write_all(&incoming)
        .await
        .expect("upstream writes opaque query");
    for (client, mark) in [
        (&mut modern, marks[0]),
        (&mut passive, marks[1]),
        (&mut legacy, marks[2]),
    ] {
        client.await_new(mark, "?OTRv3?").await;
    }
    for (client, mark) in [
        (&mut modern, marks[0]),
        (&mut passive, marks[1]),
        (&mut legacy, marks[2]),
    ] {
        client.settle().await;
        let rendered = client.since(mark);
        let message = parsed_line_with(&rendered, "?OTRv3?");
        assert_eq!(
            message.params.last().map(Vec::as_slice),
            Some(body.as_slice())
        );
    }
    let modern_line = modern.since(marks[0]);
    let passive_line = passive.since(marks[1]);
    let legacy_line = legacy.since(marks[2]);
    assert!(modern_line.contains("account=alice"), "{modern_line:?}");
    assert!(modern_line.contains("time=2026-01-01T00:00:00.000Z"));
    assert!(modern_line.contains("+custom=kept"));
    assert!(!passive_line.contains("account=alice"));
    assert!(!passive_line.contains("time="));
    assert!(passive_line.contains("+custom=kept"));
    assert!(!legacy_line.starts_with('@'));

    // Two OTR fragments remain two distinct ordered IRC events.
    let mark = modern.mark();
    runtime.upstreams[peer]
        .write_all(
            b":alice!u@h PRIVMSG #room :?OTR,1,2,fragmentA+/==\r\n:alice!u@h PRIVMSG #room :?OTR,2,2,fragmentB+/==\r\n",
        )
        .await
        .expect("upstream writes distinct fragments");
    modern.await_new(mark, "fragmentB+/==").await;
    modern.settle().await;
    let fragment_window = modern.since(mark);
    let first_pos = fragment_window.find("fragmentA+/==").expect("fragment one");
    let second_pos = fragment_window.find("fragmentB+/==").expect("fragment two");
    assert!(
        first_pos < second_pos,
        "fragment order changed: {fragment_window:?}"
    );
    assert_eq!(fragment_window.matches("?OTR,").count(), 2);

    // The exact observed messages, including trailing whitespace, are durable.
    let handle = runtime._store.handle_clone();
    let buffer = handle
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("history buffer resolves")
        .buffer;
    handle.flush().await.expect("history writes flush");
    let events = handle
        .query_history(&HistoryQuery {
            buffer,
            bound: HistoryQueryBound {
                after: None,
                before: None,
                limit: 10,
            },
        })
        .await
        .expect("opaque history reads");
    assert_eq!(events.len(), 3);
    assert_eq!(
        i2pr_irc_wire::Message::parse(&[events[0].payload.as_slice(), b"\r\n"].concat())
            .expect("history event parses")
            .params
            .last()
            .map(Vec::as_slice),
        Some(body.as_slice()),
        "history preserves ciphertext-bearing payload bytes"
    );
    assert!(events[1].payload.ends_with(b"?OTR,1,2,fragmentA+/=="));
    assert!(events[2].payload.ends_with(b"?OTR,2,2,fragmentB+/=="));
    runtime.stop().await;
}

#[tokio::test]
async fn outbound_otr_body_is_forwarded_byte_exactly_and_near_line_limit() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    upstream_barrier(&mut runtime, peer, "m009a-otr-outbound-ready").await;
    let mut client = register_as(&runtime, NetworkId(1), SessionId(21), "", ClientId(201)).await;
    client.settle().await;

    let ordinary = "PRIVMSG #room :?OTR:AKE+/== punctuation?!\t  \r\n";
    client.send(ordinary).await;
    let forwarded = read_until(&mut runtime.upstreams[peer], b"?OTR:AKE+/==").await;
    assert!(
        forwarded
            .as_bytes()
            .windows(ordinary.len())
            .any(|w| w == ordinary.as_bytes())
    );

    let notice = "NOTICE #room :?OTR:NOTICE-opaque+/==  \t\r\n";
    client.send(notice).await;
    let forwarded = read_until(&mut runtime.upstreams[peer], b"?OTR:NOTICE-opaque").await;
    assert!(
        forwarded
            .as_bytes()
            .windows(notice.len())
            .any(|window| window == notice.as_bytes())
    );

    let large_body = format!("?OTR:{}", "A".repeat(490));
    let near_limit = format!("PRIVMSG #room :{large_body}\r\n");
    assert_eq!(near_limit.len(), i2pr_irc_wire::MAX_LINE_BYTES);
    client.send(&near_limit).await;
    let forwarded = read_until(&mut runtime.upstreams[peer], b"?OTR:AAAA").await;
    assert!(
        forwarded
            .as_bytes()
            .windows(near_limit.len())
            .any(|window| window == near_limit.as_bytes()),
        "maximum-size fragment was changed: {} bytes",
        forwarded.len()
    );

    // The server may have received the complete opaque chat even though the
    // connection dies before any confirmation. The replacement connection must
    // identify and rejoin, but must not replay this non-idempotent user frame.
    runtime.upstreams[peer]
        .shutdown()
        .await
        .expect("old upstream disconnects");
    let replacement = runtime
        .reconnect_peer(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    assert!(
        !replacement.contains("?OTR"),
        "ambiguous OTR chat replayed: {replacement:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn echo_message_is_offered_only_when_upstream_negotiated_it() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response echo-message",
        )
        .await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mark = client.mark();
    client.send("CAP LS\r\n").await;
    client.await_new(mark, "CAP bot LS").await;
    let listing = client.since(mark);
    assert!(
        listing.contains("echo-message"),
        "the upstream will echo, so the bouncer can confirm: {listing:?}"
    );

    // And it is negotiable, rather than advertised and then refused.
    let mut client = register(&runtime, NetworkId(1), SessionId(2), "").await;
    let mark = client.mark();
    client.send("CAP REQ :echo-message\r\n").await;
    client.await_new(mark, "ACK").await;
    assert!(
        client.since(mark).contains("CAP bot ACK :echo-message"),
        "an advertised capability must be acknowledgeable: {:?}",
        client.since(mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_time_tag_reaches_only_the_session_that_negotiated_server_time() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;

    // One client negotiates both, one negotiates only the tag surface, one negotiates
    // neither. All three are attached to the same Network at the same moment, so any
    // difference between them is per-session and cannot be an upstream difference.
    let mut both = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "message-tags server-time",
    )
    .await;
    let mut tags_only = register(&runtime, NetworkId(1), SessionId(2), "message-tags").await;
    let mut neither = register(&runtime, NetworkId(1), SessionId(3), "").await;
    for client in [&mut both, &mut tags_only, &mut neither] {
        client.settle().await;
    }

    let mark_both = both.mark();
    let mark_tags = tags_only.mark();
    let mark_none = neither.mark();

    runtime.upstreams[peer]
        .write_all(
            b"@time=2026-01-01T00:00:00.000Z;+custom=kept :alice!u@h PRIVMSG #room :stamped line\r\n",
        )
        .await
        .expect("upstream writes");

    both.await_new(mark_both, "stamped line").await;
    tags_only.await_new(mark_tags, "stamped line").await;
    neither.await_new(mark_none, "stamped line").await;

    let with_time = both.since(mark_both);
    let without_time = tags_only.since(mark_tags);
    let bare = neither.since(mark_none);

    // Substring, not prefix: the tag encoder orders tags by the server's choice, and an
    // assertion about tag position would be testing the encoder rather than this policy.
    assert!(
        with_time.contains("time=2026-01-01T00:00:00.000Z"),
        "a session that negotiated server-time receives the tag: {with_time:?}"
    );
    assert!(
        with_time.contains("+custom=kept"),
        "and every other tag it asked for: {with_time:?}"
    );

    assert!(
        !without_time.contains("time="),
        "a session with message-tags but without server-time must not receive it: {without_time:?}"
    );
    assert!(
        without_time.contains("+custom=kept"),
        "and keeps every other tag, because those are what it asked for: {without_time:?}"
    );

    assert!(
        bare.contains(":alice!u@h PRIVMSG #room :stamped line"),
        "a session with neither capability receives a bare frame: {bare:?}"
    );
    // A prefix always contains `@`; what must be absent is the *tag* prefix, so the
    // frame has to begin with the message rather than with `@`.
    assert!(!bare.starts_with('@'), "{bare:?}");
    runtime.stop().await;
}

#[tokio::test]
async fn history_replay_carries_a_time_only_for_a_session_that_asked() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    runtime.upstreams[peer]
        .write_all(b":alice!u@h PRIVMSG #room :replayed line\r\n")
        .await
        .expect("upstream writes");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut stamped = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "draft/chathistory batch server-time message-tags",
    )
    .await;
    let mark = stamped.mark();
    stamped.send("CHATHISTORY LATEST #room * 10\r\n").await;
    stamped.await_new(mark, "BATCH -").await;
    let reply = stamped.since(mark);
    assert!(
        reply.contains("replayed line") && reply.contains("time="),
        "a replay for a session that negotiated server-time is stamped: {reply:?}"
    );

    let mut bare = register(
        &runtime,
        NetworkId(1),
        SessionId(2),
        "draft/chathistory batch",
    )
    .await;
    let mark = bare.mark();
    bare.send("CHATHISTORY LATEST #room * 10\r\n").await;
    bare.await_new(mark, "BATCH -").await;
    let reply = bare.since(mark);
    assert!(
        reply.contains("replayed line") && !reply.contains("time="),
        "a replay for a session that did not is not stamped: {reply:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_refusal_is_a_numeric_until_the_session_negotiates_standard_replies() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;

    let mut plain = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "draft/chathistory batch",
    )
    .await;
    let mark = plain.mark();
    plain
        .send("CHATHISTORY BEFORE #room timestamp=1700000000 10\r\n")
        .await;
    plain.await_new(mark, "461 ").await;
    let numeric = plain.since(mark);
    assert!(
        !numeric.contains("FAIL"),
        "a client that never negotiated standard-replies gets the numeric it has always \
         parsed, not an unrequested frame: {numeric:?}"
    );
    assert!(
        numeric.contains("Invalid timestamp"),
        "and the reason survives: {numeric:?}"
    );

    let mut modern = register(
        &runtime,
        NetworkId(1),
        SessionId(2),
        "draft/chathistory batch standard-replies",
    )
    .await;
    let mark = modern.mark();
    modern
        .send("CHATHISTORY BEFORE #room timestamp=1700000000 10\r\n")
        .await;
    modern.await_new(mark, "FAIL CHATHISTORY").await;
    let fail = modern.since(mark);
    assert!(
        fail.contains("INVALID_PARAMS") && fail.contains("BEFORE"),
        "a client that negotiated it gets the standard reply naming the failure: {fail:?}"
    );
    assert!(
        !fail.contains("461"),
        "and only one of the two forms: {fail:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_search_refusal_follows_the_same_rule() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;

    let mut plain = register(&runtime, NetworkId(1), SessionId(1), "soju.im/search").await;
    let mark = plain.mark();
    plain.send("SEARCH colour=red\r\n").await;
    plain.await_new(mark, "461 ").await;
    assert!(
        !plain.since(mark).contains("FAIL"),
        "{:?}",
        plain.since(mark)
    );

    let mut modern = register(
        &runtime,
        NetworkId(1),
        SessionId(2),
        "soju.im/search standard-replies",
    )
    .await;
    let mark = modern.mark();
    modern.send("SEARCH colour=red\r\n").await;
    modern.await_new(mark, "FAIL SEARCH").await;
    let fail = modern.since(mark);
    assert!(
        fail.contains("unknown search selector") && fail.contains("BATCH +"),
        "the standard reply and the complete batch both arrive: {fail:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn implicit_names_reach_everyone_except_the_session_that_declined_them() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    // A complete membership snapshot is the precondition for projecting names at all.
    runtime.upstreams[peer]
        .write_all(b":srv 353 bot = #room :bot @Alice\r\n:srv 366 bot #room :End of NAMES list\r\n")
        .await
        .expect("upstream writes the name list");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut declined = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "draft/no-implicit-names",
    )
    .await;
    let mut accepted = register(&runtime, NetworkId(1), SessionId(2), "").await;

    declined.until("001 bot").await;
    accepted.until("001 bot").await;
    accepted.until("366 bot #room").await;

    assert!(
        !declined.since(0).contains("353 bot"),
        "a session that negotiated no-implicit-names is not sent a membership block: {:?}",
        declined.since(0)
    );
    assert!(
        accepted.since(0).contains("353 bot") && accepted.since(0).contains("Alice"),
        "and every other session still is, on the same Network: {:?}",
        accepted.since(0)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_upstream_capability_change_reaches_only_the_sessions_that_asked() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;

    let mut listening = register(&runtime, NetworkId(1), SessionId(1), "cap-notify").await;
    let mut quiet = register(&runtime, NetworkId(1), SessionId(2), "").await;
    listening.until("001 bot").await;
    quiet.until("001 bot").await;
    let quiet_mark = quiet.mark();

    runtime.upstreams[peer]
        .write_all(b":srv CAP * NEW :echo-message\r\n")
        .await
        .expect("upstream announces a new capability");
    // The bouncer asks before it can serve: an offered capability is not an enabled one,
    // and enabling it is what changes what a local client can be offered.
    read_until(&mut runtime.upstreams[peer], b"CAP REQ :echo-message").await;
    runtime.upstreams[peer]
        .write_all(b":srv CAP * ACK :echo-message\r\n")
        .await
        .expect("upstream grants the capability");
    listening.await_new(0, "CAP * NEW").await;
    assert!(
        listening.since(0).contains("echo-message"),
        "{:?}",
        listening.since(0)
    );

    quiet.settle().await;
    assert!(
        !quiet.since(quiet_mark).contains("CAP * NEW"),
        "a client that never negotiated cap-notify is not sent unsolicited CAP lines: {:?}",
        quiet.since(quiet_mark)
    );

    // And a withdrawal is announced the same way, to the same sessions only.
    let listening_mark = listening.mark();
    runtime.upstreams[peer]
        .write_all(b":srv CAP * DEL :echo-message\r\n")
        .await
        .expect("upstream withdraws a capability");
    // No request follows a withdrawal: asking for something the server just took back
    // would be a request for nothing.
    listening.await_new(listening_mark, "CAP * DEL").await;
    assert!(
        listening.since(listening_mark).contains("echo-message"),
        "{:?}",
        listening.since(listening_mark)
    );
    let quiet_mark = quiet.mark();
    quiet.settle().await;
    assert!(
        !quiet.since(quiet_mark).contains("CAP *"),
        "{:?}",
        quiet.since(quiet_mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_upstream_announcement_about_something_unserved_is_not_reported() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    let mut listening = register(&runtime, NetworkId(1), SessionId(1), "cap-notify").await;
    listening.until("001 bot").await;

    runtime.upstreams[peer]
        .write_all(b":srv CAP * NEW :chghost extended-monitor\r\n")
        .await
        .expect("upstream announces capabilities this build does not serve");
    // Give the notification a bounded window to arrive, so a line that *would* have been
    // sent is observed as arriving rather than merely not-yet-arriving.
    listening.settle().await;
    assert!(
        !listening.since(0).contains("chghost") && !listening.since(0).contains("extended-monitor"),
        "a capability the bouncer does not serve changes nothing a client can act on, so it \
         is not reported: {:?}",
        listening.since(0)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn account_and_invite_capability_changes_follow_upstream_ack_and_del() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response",
        )
        .await;
    let mut listening = register(&runtime, NetworkId(1), SessionId(1), "cap-notify").await;
    listening.until("001 bot").await;
    runtime.upstreams[peer]
        .write_all(b":srv CAP * NEW :account-tag invite-notify\r\n")
        .await
        .expect("upstream adds mediated capabilities");
    read_until(
        &mut runtime.upstreams[peer],
        b"CAP REQ :account-tag invite-notify",
    )
    .await;
    runtime.upstreams[peer]
        .write_all(b":srv CAP * ACK :account-tag invite-notify\r\n")
        .await
        .expect("upstream enables both capabilities");
    listening.await_new(0, "CAP * NEW").await;
    assert!(listening.since(0).contains("account-tag invite-notify"));
    let mark = listening.mark();
    runtime.upstreams[peer]
        .write_all(b":srv CAP * DEL :account-tag invite-notify\r\n")
        .await
        .expect("upstream withdraws both capabilities");
    listening.await_new(mark, "CAP * DEL").await;
    assert!(listening.since(mark).contains("account-tag invite-notify"));
    runtime.stop().await;
}

#[tokio::test]
async fn a_local_message_enters_history_once_on_the_echo_and_is_labelled_outgoing() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response echo-message",
        )
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "echo-message").await;
    client.until("001 bot").await;
    client.settle().await;

    // Nothing is recorded on the local write: the queue accepting the bytes says nothing
    // about whether the server received them.
    let before = client.mark();
    client.send("PRIVMSG #room :mine\r\n").await;
    read_until(&mut runtime.upstreams[peer], b"PRIVMSG #room :mine").await;

    let mut search = register(&runtime, NetworkId(1), SessionId(2), "soju.im/search batch").await;
    let mark = search.mark();
    search.send("SEARCH in=#room text=mine\r\n").await;
    search.await_new(mark, "BATCH -").await;
    assert!(
        !search.since(mark).contains("mine"),
        "a local socket write is not evidence of upstream delivery, so nothing is recorded \
         yet: {:?}",
        search.since(mark)
    );

    // The upstream echo is the confirmation event, and it is recorded exactly once.
    runtime.upstreams[peer]
        .write_all(b"@time=2026-01-01T00:00:00.000Z :bot!u@h PRIVMSG #room :mine\r\n")
        .await
        .expect("upstream echoes");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let after = search.mark();
    search.send("SEARCH in=#room text=mine\r\n").await;
    search.await_new(after, "BATCH -").await;
    let reply = search.since(after);
    let hits = reply
        .lines()
        .filter(|line| line.contains("SOV SEARCH"))
        .count();
    assert_eq!(
        hits, 1,
        "the echo is the canonical event and appears exactly once: {reply:?}"
    );
    assert!(reply.contains("sender=bot"), "{reply:?}");

    // The initiator saw the echo too, which is what echo-message promises: confirmation
    // arrives as an ordinary frame from the server, not as a synthetic local one.
    client.settle().await;
    assert!(
        client.since(before).contains("PRIVMSG #room :mine"),
        "the initiating client receives the echo it negotiated: {:?}",
        client.since(before)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_unstamped_message_from_our_own_nick_is_not_treated_as_a_confirmation() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(
            1,
            &["#room"],
            "message-tags server-time batch labeled-response echo-message",
        )
        .await;
    // Someone else's line, from a Network where we happen to share the nick, with no
    // `time`. Treating it as our own echo would put a stranger's words in the Operator's
    // own outbound history.
    runtime.upstreams[peer]
        .write_all(b":bot!u@h PRIVMSG #room :not actually ours\r\n")
        .await
        .expect("upstream writes");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut search = register(&runtime, NetworkId(1), SessionId(2), "soju.im/search batch").await;
    let mark = search.mark();
    search.send("SEARCH in=#room text=actually\r\n").await;
    search.await_new(mark, "BATCH -").await;
    assert!(
        search.since(mark).contains("not actually ours"),
        "an unstamped line is still conversation and is retained: {:?}",
        search.since(mark)
    );
    runtime.stop().await;
}
