//! A deterministic loopback SAM bridge, for tests only.
//!
//! # What this is and is not
//!
//! It is a scripted server that speaks the subset of SAM 3.1 this crate sends, bound to
//! the loopback interface. It exists so the client can be driven through every branch —
//! both terminators, fragmented replies, an over-long line followed by a good one,
//! malformed replies, a delayed reply, a control close, and a payload that looks like SAM
//! — without a router anywhere.
//!
//! It is **not** a second SAM implementation. It has no session table, no tunnels, and no
//! Destination handling. It is reachable only behind the `testkit` feature, which no
//! production build enables, and this file is inside `crates/sam` rather than in
//! `crates/testkit` so that the boundary scan sees it and can hold it to the same rule as
//! everything else in the crate.
//!
//! # Why the boundary scan permits it
//!
//! It opens a loopback listener. That is the one socket operation a test needs that
//! production must not have, and it is safe here for a specific reason: it binds
//! `127.0.0.1:0` and accepts only on that socket, so it cannot be reached from outside
//! the host, and it is compiled out of every non-test build.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// The only address the fake ever binds.
///
/// A named constant so the boundary scan and this test can both assert it, and so a
/// future edit that widens the bind has exactly one place to change.
pub const FAKE_BIND: &str = "127.0.0.1:0";

/// Ceiling on streams the bridge will hold open for a test to drive.
///
/// Explicit because the registry is a collection fed by network-triggered events. A test
/// that exceeded it is not testing anything the plan asked for, and the bridge records the
/// refusal rather than growing — so `FakeIrcPeer::refused` is a checkable assertion rather
/// than an assumption.
pub const MAX_ESTABLISHED_STREAMS: usize = 64;

/// A loopback endpoint nothing is listening on.
///
/// For the "the router is not running" case. The port is bound and dropped so it is
/// almost certainly free, which is the closest a test can get to a router that has not
/// started. Lives here rather than in a test so that the socket name stays inside one of
/// the two files the boundary scan permits to hold it.
pub async fn absent_endpoint() -> crate::endpoint::SamBridgeEndpoint {
    let listener = TcpListener::bind(FAKE_BIND)
        .await
        .expect("a loopback port binds");
    let addr = listener
        .local_addr()
        .expect("the bound address is readable");
    drop(listener);
    crate::endpoint::SamBridgeEndpoint::parse(&addr.to_string())
        .expect("a bound loopback address is a valid bridge endpoint")
}

/// What the bridge answers to one kind of request.
///
/// Keyed by request verb rather than by arrival order. Order-keyed scripting was tried
/// first and it does not work for the provider: two Networks connecting concurrently
/// interleave their sockets, so a single queue hands one Network the other's reply and the
/// test fails for a reason that has nothing to do with the claim under test. Keying by verb
/// makes each reply independent of how the connections interleave.
#[derive(Debug, Clone, Default)]
pub struct Script {
    /// Replies for a `HELLO`, in order. A second `HELLO` on the same connection takes the
    /// second entry.
    pub hello: Vec<Vec<u8>>,
    /// Replies for a `SESSION CREATE`.
    pub session: Vec<Vec<u8>>,
    /// Replies for a `STREAM CONNECT`.
    pub stream: Vec<Vec<u8>>,
    /// Sent once every scripted reply is exhausted, with no request to trigger it.
    pub trailing: Vec<u8>,
    /// Whether to hang up once every scripted reply has been served.
    ///
    /// Across the whole script rather than scoped to `STREAM CONNECT`. Both shapes a test
    /// needs are the same shape — "the bridge has said all it is going to say, then the
    /// connection ends" — and the two differ only in how many replies precede it. One with
    /// an empty `stream` script closes right after the session create, which is what a
    /// router dropping a control socket mid-exchange looks like; one with a scripted
    /// stream closes after the trailing payload, which is a completed stream ending.
    ///
    /// A request with nothing scripted for it does not trigger the close: silence is a
    /// separate behaviour, and conflating the two would make a deadline test unable to
    /// distinguish "the router is not answering" from "the router hung up".
    pub close_after: bool,
    /// Whether to hang up on the connection that answered a `SESSION CREATE`.
    ///
    /// Separate from `close_after` because the two describe different failures. This one
    /// is the router dropping a control socket while the session is still nominally fine,
    /// which the provider can only notice by watching the socket; `close_after` is the
    /// bridge reaching the end of everything it had to say.
    pub close_after_session: bool,
    /// Write each reply one byte at a time.
    ///
    /// Forces the client's partial-read path to reassemble, which a loopback socket that
    /// delivers whole writes would otherwise never exercise.
    pub fragment: bool,
    /// Write each reply in two writes split between its trailing CR and LF, pausing
    /// briefly between them.
    ///
    /// A deterministic read-boundary regression for Corrective 049: unlike `fragment`,
    /// which splits everywhere probabilistically, this pins the one boundary that
    /// once dropped the whole reply — a terminator straddling two TCP segments.
    /// Mutually exclusive with `fragment`; `fragment` wins when both are set.
    pub split_crlf: bool,
}

impl Script {
    /// A bridge that answers `streams` connects from **one** Network.
    ///
    /// This is what a healthy provider connect costs: each connect is two sockets, and
    /// each socket says `HELLO`, so `streams` connects need `2 * streams` hellos. The
    /// original allocation of `streams + 1` was right only for a single connect and ran
    /// the script dry on the second, where the client then waited out its full hello
    /// deadline and the test reported a ten-second stall instead of a missing reply.
    ///
    /// Only one `SESSION CREATE` is scripted, because that is the property under test:
    /// reconnecting does **not** mean re-identifying. A test that needs several Networks
    /// says so with an explicit script, since the number of sessions is precisely what it
    /// is asserting about.
    pub fn healthy(streams: usize) -> Self {
        Self {
            hello: (0..streams.saturating_mul(2)).map(|_| hello_ok()).collect(),
            session: vec![session_ok_with_destination()],
            stream: (0..streams).map(|_| stream_ok()).collect(),
            ..Self::default()
        }
    }

    /// A bridge that answers one session and `streams` connects per Network, for
    /// `networks` Networks.
    ///
    /// The join the single-verb script cannot express: N Networks means N sessions, and
    /// getting it wrong would silently collapse the very distinction the test exists to
    /// draw.
    pub fn scoped(networks: usize, streams: usize) -> Self {
        Self {
            hello: (0..networks.saturating_mul(streams).saturating_mul(2))
                .map(|_| hello_ok())
                .collect(),
            session: (0..networks)
                .map(|_| session_ok_with_destination())
                .collect(),
            stream: (0..networks.saturating_mul(streams))
                .map(|_| stream_ok())
                .collect(),
            ..Self::default()
        }
    }
}

/// What the fake observed.
#[derive(Debug, Default)]
pub struct Observed {
    /// Every request line received.
    ///
    /// Behind a `Mutex` rather than shared by reference because the serving task writes
    /// it and the test reads it, and a test that reads it has to be able to wait for the
    /// write rather than assume it already happened.
    requests: std::sync::Mutex<Vec<String>>,
}

impl Observed {
    /// Every request line received so far.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the observed log is readable")
            .clone()
    }
    fn record(&self, line: String) {
        self.requests
            .lock()
            .expect("the observed log is writable")
            .push(line);
    }
}

/// A running fake bridge.
pub struct FakeBridge {
    addr: SocketAddr,
    observed: Arc<Observed>,
    connections: Arc<AtomicUsize>,
    peer: Arc<FakeIrcPeer>,
    handle: tokio::task::JoinHandle<()>,
}

impl FakeBridge {
    /// Binds a loopback listener and answers with `script` on every connection.
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind(FAKE_BIND)
            .await
            .expect("the fake bridge binds a loopback port");
        let addr = listener
            .local_addr()
            .expect("the bound address is readable");
        let observed = Arc::new(Observed::default());
        let connections = Arc::new(AtomicUsize::new(0));
        let streams = Arc::new(std::sync::Mutex::new(Vec::new()));
        let refused = Arc::new(AtomicUsize::new(0));
        let peer = Arc::new(FakeIrcPeer::new(Arc::clone(&streams), Arc::clone(&refused)));
        let handle = tokio::spawn(serve(
            listener,
            script,
            Arc::clone(&observed),
            Arc::clone(&connections),
            streams,
            refused,
        ));
        Self {
            addr,
            observed,
            connections,
            peer,
            handle,
        }
    }

    /// The loopback address to hand to a client.
    pub fn endpoint(&self) -> crate::endpoint::SamBridgeEndpoint {
        crate::endpoint::SamBridgeEndpoint::parse(&self.addr.to_string())
            .expect("a bound loopback address is a valid bridge endpoint")
    }

    /// Every request line received so far.
    pub fn requests(&self) -> Vec<String> {
        self.observed.requests()
    }

    /// How many connections were accepted.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// The IRC side of every stream this bridge has established.
    pub fn peer(&self) -> Arc<FakeIrcPeer> {
        Arc::clone(&self.peer)
    }
}

impl Drop for FakeBridge {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn serve(
    listener: TcpListener,
    script: Script,
    observed: Arc<Observed>,
    connections: Arc<AtomicUsize>,
    streams: Arc<std::sync::Mutex<Vec<Stream>>>,
    refused: Arc<AtomicUsize>,
) {
    let fragment = script.fragment;
    let split_crlf = script.split_crlf && !script.fragment;
    let close_after = script.close_after;
    let close_after_session = script.close_after_session;
    let trailing = script.trailing;
    // One shared, mutex-guarded cursor per verb. Shared because the provider opens
    // several sockets concurrently and a per-connection cursor would let each replay the
    // whole script from the beginning.
    let hello = Cursor::new(script.hello);
    let session = Cursor::new(script.session);
    let stream = Cursor::new(script.stream);
    while let Ok((mut socket, _)) = listener.accept().await {
        connections.fetch_add(1, Ordering::SeqCst);
        let hello = Arc::clone(&hello);
        let session = Arc::clone(&session);
        let stream = Arc::clone(&stream);
        let observed = Arc::clone(&observed);
        let trailing = trailing.clone();
        let streams = Arc::clone(&streams);
        let refused = Arc::clone(&refused);
        tokio::spawn(async move {
            let mut reader = LineAccumulator::default();
            let mut chunk = [0u8; 512];
            loop {
                // Answer only once a whole request line has arrived, so the client cannot
                // receive a reply before it has finished sending.
                let Some(request) = reader.take_line() else {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(count) => reader.push(&chunk[..count]),
                    }
                    continue;
                };
                observed.record(request.clone());
                let reply = if request.starts_with("HELLO") {
                    hello.take()
                } else if request.starts_with("SESSION CREATE") {
                    session.take()
                } else if request.starts_with("STREAM CONNECT") {
                    stream.take()
                } else {
                    None
                };
                let Some(reply) = reply else {
                    // Nothing scripted for this request. Staying silent is a real router
                    // behaviour and is what the deadline tests need.
                    continue;
                };
                // Whether this reply was the last one the script had to give.
                //
                // Checked across every verb, so a script that never scripted a stream
                // still closes once the session create is answered — the case that has
                // to read as a hang-up rather than as a router that stopped talking.
                let script_spent = hello.is_empty() && session.is_empty() && stream.is_empty();
                // Captured before the write, because the fragmented path consumes `reply`.
                let acknowledged_stream = reply.starts_with(b"STREAM STATUS RESULT=OK");
                if fragment {
                    for byte in reply {
                        if socket.write_all(&[byte]).await.is_err() {
                            return;
                        }
                    }
                } else if split_crlf && reply.len() >= 2 && reply.ends_with(b"\r\n") {
                    // A deterministic read-boundary split: the first write ends after
                    // the CR and a short pause makes it unlikely the two writes
                    // coalesce into one segment before the client reads. The
                    // regression this pins is a reply whose terminator straddles the
                    // boundary (Corrective 049).
                    let head = reply.len() - 1;
                    if socket.write_all(&reply[..head]).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                    tokio::time::sleep(SPLIT_PAUSE).await;
                    if socket.write_all(&reply[head..]).await.is_err() {
                        return;
                    }
                } else if socket.write_all(&reply).await.is_err() {
                    return;
                }
                // Trailing bytes belong to the connection that just reached `RESULT=OK`.
                // Attaching them to any other reply would put application data into the
                // middle of an exchange. Written *before* the close, because a close
                // immediately after `RESULT=OK` would cut the payload short — and a
                // truncated payload is exactly the failure this fixture exists to detect.
                if !trailing.is_empty() && acknowledged_stream {
                    let _ = socket.write_all(&trailing).await;
                }
                if close_after_session && request.starts_with("SESSION CREATE") {
                    return;
                }
                if close_after && script_spent {
                    return;
                }
                // An acknowledged stream stops being protocol and becomes an IRC
                // conversation, so hand the socket to the test rather than looping on it.
                // Registering after the write is what guarantees a test that saw
                // `RESULT=OK` can find the socket without polling for it.
                if acknowledged_stream && socket.flush().await.is_ok() {
                    let mut registry = streams.lock().expect("the stream registry is writable");
                    if registry.len() >= MAX_ESTABLISHED_STREAMS {
                        refused.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    registry.push(Arc::new(tokio::sync::Mutex::new((
                        socket,
                        IrcSide::default(),
                    ))));
                    return;
                }
            }
        });
    }
}

/// A shared, bounded queue of scripted replies for one request verb.
///
/// `take` hands out the next reply or `None`. It never blocks and never grows, so a peer
/// that sends more requests than were scripted simply gets silence after the last one.
#[derive(Debug)]
struct Cursor {
    replies: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
}

impl Cursor {
    fn new(replies: Vec<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            replies: std::sync::Mutex::new(replies.into()),
        })
    }
    fn take(&self) -> Option<Vec<u8>> {
        self.replies
            .lock()
            .expect("the scripted reply queue is writable")
            .pop_front()
    }
    /// Whether nothing is left to answer.
    fn is_empty(&self) -> bool {
        self.replies
            .lock()
            .expect("the scripted reply queue is readable")
            .is_empty()
    }
}

/// Accumulates bytes until a complete CRLF line is available.
///
/// The mirror of the client's own reader, and deliberately simple: the fake has to
/// reproduce the *peer's* obligation to terminate a request, not validate it.
#[derive(Default)]
struct LineAccumulator {
    bytes: Vec<u8>,
}

impl LineAccumulator {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn take_line(&mut self) -> Option<String> {
        let newline = self.bytes.iter().position(|byte| *byte == b'\n')?;
        let line: Vec<u8> = self.bytes.drain(..=newline).collect();
        let text = String::from_utf8_lossy(&line).into_owned();
        Some(text.trim_end_matches(['\r', '\n']).to_owned())
    }
}

/// Convenience: a complete line.
pub fn line(text: &str) -> Vec<u8> {
    format!("{text}\r\n").into_bytes()
}

/// Convenience: a `HELLO OK` reply, in the short form Java I2P uses.
pub fn hello_ok() -> Vec<u8> {
    line("HELLO OK")
}

/// Convenience: a successful session reply carrying a Destination.
///
/// The Destination is fake key material. Including it is the point: a client that
/// retained it would fail this test's counterpart in `session_id` and
/// `protocol`, which assert that nothing private is kept.
pub fn session_ok_with_destination() -> Vec<u8> {
    line(&format!(
        "SESSION STATUS RESULT=OK ID=abc DESTINATION={}",
        "Q".repeat(600)
    ))
}

/// Convenience: a successful stream reply.
pub fn stream_ok() -> Vec<u8> {
    line("STREAM STATUS RESULT=OK")
}

// --------------------------------------------------------------- the IRC side

/// One established stream and the log of what the client sent over it.
///
/// The log is per-stream but read back merged, so a test can assert across a reconnect —
/// "this line must never appear twice" is the whole point of the no-replay property, and
/// inspecting one stream at a time would make a resent line look like one line on each
/// side of a fresh connection.
#[derive(Debug, Default)]
struct IrcSide {
    log: std::sync::Mutex<Vec<u8>>,
}

/// A registered stream: the socket plus its read log.
///
/// An async mutex because the peer holds it across a socket read, and only the test ever
/// touches it, so there is no contention to justify anything heavier.
type Stream = Arc<tokio::sync::Mutex<(TcpStream, IrcSide)>>;

/// The far side of every stream the bridge has established.
///
/// This is the fake IRC upstream. Once a `STREAM CONNECT` is answered `RESULT=OK`, the
/// socket stops being SAM and becomes an IRC conversation, so a test can drive a real
/// `RuntimeController` end to end without a router: the runtime opens a genuine socket
/// through the provider, and this is what it is talking to.
pub struct FakeIrcPeer {
    streams: Arc<std::sync::Mutex<Vec<Stream>>>,
    refused: Arc<AtomicUsize>,
}

impl FakeIrcPeer {
    fn new(streams: Arc<std::sync::Mutex<Vec<Stream>>>, refused: Arc<AtomicUsize>) -> Self {
        Self { streams, refused }
    }

    fn snapshot(&self) -> Vec<Stream> {
        self.streams
            .lock()
            .expect("the stream registry is readable")
            .clone()
    }

    /// How many streams are currently established.
    pub fn live_streams(&self) -> usize {
        self.streams
            .lock()
            .expect("the stream registry is readable")
            .len()
    }

    /// How many established streams the ceiling refused.
    ///
    /// Zero in a passing test. Exposed because a silently dropped stream would look to the
    /// runtime like a router failure and so mask the defect the test was written to catch.
    pub fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }

    /// Writes IRC server bytes to every established stream.
    ///
    /// To all of them rather than to one, because the test does not own the reconnect
    /// schedule: whichever stream the runtime opened next is the one that receives this.
    pub async fn send(&self, bytes: &[u8]) {
        for stream in self.snapshot() {
            let mut guard = stream.lock().await;
            if guard.0.write_all(bytes).await.is_err() {
                continue;
            }
            let _ = guard.0.flush().await;
        }
    }

    /// Everything the client has sent, across every stream, in registry order.
    ///
    /// Merged rather than per-stream on purpose: the no-replay property is about the whole
    /// conversation across a reconnect, and a per-stream view would make a resent line
    /// look like one line on each side of a fresh stream.
    pub async fn received(&self) -> Vec<u8> {
        let mut all = Vec::new();
        for stream in self.snapshot() {
            let guard = stream.lock().await;
            all.extend_from_slice(&guard.1.log.lock().expect("the IRC read log is readable"));
        }
        all
    }

    /// Everything the client sent on each stream, in stream order.
    ///
    /// This is the view the no-replay property needs. A merged log would let a line that
    /// appeared on both sides of a reconnect look like one line; keeping the streams apart
    /// is what makes "the new connection re-sent something the old one already sent"
    /// expressible as an assertion rather than as an impression.
    pub async fn received_per_stream(&self) -> Vec<Vec<u8>> {
        let mut all = Vec::new();
        for stream in self.snapshot() {
            let guard = stream.lock().await;
            all.push(
                guard
                    .1
                    .log
                    .lock()
                    .expect("the IRC read log is readable")
                    .clone(),
            );
        }
        all
    }

    /// Whether every stream's log so far contains `needle`.
    pub async fn contains(&self, needle: &[u8]) -> bool {
        let needle = needle.to_vec();
        for stream in self.snapshot() {
            let guard = stream.lock().await;
            let log = guard.1.log.lock().expect("the IRC read log is readable");
            if log.windows(needle.len()).any(|window| window == needle) {
                return true;
            }
        }
        false
    }

    /// Reads until the client has sent something containing `needle`, or `timeout` elapses.
    ///
    /// Reads rather than only inspecting, because the log is populated by the reads
    /// themselves: nothing pumps these sockets except this fixture, so a test that merely
    /// looked at the log would wait for a reading that only it could produce.
    pub async fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.contains(needle).await {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let _ = self.recv(remaining.min(POLL * 8)).await;
        }
    }

    /// Waits until the client has sent something, or `timeout` elapses.
    ///
    /// Explicit pumping rather than a background reader task: a reader would need its own
    /// half of each socket, and a read half held outside this registry is precisely what
    /// would stop [`FakeIrcPeer::close`] from being able to close anything.
    pub async fn recv(&self, timeout: Duration) -> Option<Vec<u8>> {
        let streams = self.snapshot();
        tokio::time::timeout(timeout, async {
            loop {
                for stream in &streams {
                    let mut guard = stream.lock().await;
                    let mut chunk = [0u8; 512];
                    // A short inner poll so a stream with nothing to say cannot monopolise
                    // the wait while another one is producing data.
                    let Ok(result) = tokio::time::timeout(POLL, guard.0.read(&mut chunk)).await
                    else {
                        continue;
                    };
                    match result {
                        Ok(0) | Err(_) => continue,
                        Ok(count) => {
                            guard
                                .1
                                .log
                                .lock()
                                .expect("the IRC read log is writable")
                                .extend_from_slice(&chunk[..count]);
                            return chunk[..count].to_vec();
                        }
                    }
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .ok()
    }

    /// Shuts every established stream down, which the client sees as an IRC EOF.
    ///
    /// `shutdown` rather than dropping the socket: dropping one end of a loopback socket
    /// leaves the connection half-open, and the runtime would then be retrying a stream
    /// that had never ended rather than reconnecting after a genuine EOF.
    ///
    /// The streams stay in the registry afterwards, so a test can still read what was sent
    /// on the connection that just died. That is the whole point for the no-replay
    /// property — the bytes of the old connection are the evidence.
    pub async fn close(&self) {
        for stream in self.snapshot() {
            let mut guard = stream.lock().await;
            let _ = guard.0.shutdown().await;
        }
    }
}

/// How long one pass of [`FakeIrcPeer::recv`] waits on a single stream before moving on.
const POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Pause between the two writes of a [`Script::split_crlf`] reply.
///
/// Long enough that the client's first read returns the head before the tail is sent,
/// so the CR/LF boundary is genuinely split across reads; short enough to keep the
/// suite fast. Test-only.
const SPLIT_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

// ------------------------------------------------- the inbound (ACCEPT) side

/// Ceiling on the peer-Destination line an accept socket delivers before raw bytes.
///
/// The same bound the client's own line reader enforces. This is a fixture, but a fixture
/// that read an unbounded line would model the exact defect the fixture exists to detect,
/// so the ceiling is the one a conforming peer needs.
pub const MAX_ACCEPT_DESTINATION_LINE: usize = crate::line::MAX_SAM_LINE_BYTES;

/// Ceiling on accepts a peer may have armed at once.
///
/// Explicit because the registry is fed by network-triggered events. Refusal is returned
/// rather than silently dropped, so a test that exceeded it can say so.
pub const MAX_ARMED_ACCEPTS: usize = 8;

/// Ceiling on one inbound application read, so no read in this file is unbounded.
const MAX_INBOUND_READ: usize = 16 * 1024;

/// How long an armed accept waits for a connection, a line, or a payload before giving up.
///
/// Short, because everything here is a loopback fixture: a long budget would only make a
/// designed failure take longer to observe. Bounded is the property; generous is not.
const ACCEPT_BUDGET: Duration = Duration::from_secs(10);

/// What the bridge delivers on the accept socket once a connection lands.
///
/// Three shapes, because the raw transition is the property under test and a fixture that
/// only produces the happy case cannot test it.
#[derive(Debug, Clone)]
pub enum Prelude {
    /// A Destination line and the first application bytes, written in one `write_all`.
    ///
    /// One write is the point: it makes the two arrive in one TCP segment, so a reader
    /// that discards whatever it read past the newline loses bytes here.
    Destination {
        destination: Vec<u8>,
        payload: Vec<u8>,
    },
    /// No Destination line at all: raw bytes start immediately.
    Missing,
    /// A Destination line longer than [`MAX_ACCEPT_DESTINATION_LINE`].
    Oversized,
}

impl Default for Prelude {
    fn default() -> Self {
        Self::Destination {
            // Fake key material, same shape as `session_ok_with_destination`. No coalesced
            // payload by default, so the default prelude is exactly one line; the
            // coalescing case is its own configuration and its own test.
            destination: "Q".repeat(600).into_bytes(),
            payload: Vec::new(),
        }
    }
}

/// How the fake bridge answers the inbound side.
#[derive(Debug, Clone, Default)]
pub struct AcceptConfig {
    /// The reply to `STREAM ACCEPT`, when it is not the conforming `RESULT=OK`.
    ///
    /// `None` means a conforming bridge. `Some(...)` exists so a test can check the
    /// *peer's* fail-closed behaviour, which is the other half of the raw transition.
    pub accept_status: Option<Vec<u8>>,
    /// What the accept socket carries once a connection arrives.
    pub prelude: Prelude,
}

/// Why an inbound accept could not be armed or completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptError {
    /// The accept socket closed before the expected bytes arrived.
    Closed,
    /// A line exceeded the ceiling.
    PreludeTooLong,
    /// No connection or line arrived within the budget.
    TimedOut,
    /// The bridge refused the accept, or never acknowledged it `RESULT=OK`.
    Refused,
}

/// An accept that has been armed on its own socket and is waiting for a connection.
///
/// Owned by the test. Holding the accept socket here is what keeps the peer session alive
/// for the accept's lifetime, which is the property the previous live harness got wrong
/// by reusing the `SESSION CREATE` control socket.
pub struct ArmedAccept {
    /// The accept socket itself.
    ///
    /// Held so the connection the bridge acknowledged stays open for the accept's whole
    /// lifetime. Dropping it would end the accept, not just release the socket.
    _socket: TcpStream,
    /// The connection the bridge routed here, and the prelude it put on this accept.
    arriving: tokio::sync::oneshot::Receiver<Arrived>,
}

/// A connection the bridge routed to an armed accept.
struct Arrived {
    /// The connecting socket, now raw in both directions.
    connecting: TcpStream,
    /// What the bridge wrote on the accept before the application started.
    ///
    /// Bytes the bridge authored, not the test's. A test that could write its own
    /// Destination line would be able to make the transition pass by construction.
    prelude: Vec<u8>,
}

impl ArmedAccept {
    /// Completes the raw transition and returns the application stream.
    ///
    /// Reads the accept socket's prelude in order — the peer-Destination line, then raw
    /// application bytes — and only then returns a stream on which application bytes can
    /// appear. Bytes already read past the line are kept rather than dropped, which is the
    /// property this fixture exists to check.
    ///
    /// A missing line fails on a bounded wait rather than by parsing: waiting is what a
    /// real peer does, and it is the behaviour worth having under test.
    pub async fn incoming(self) -> Result<(PeerStream, Vec<u8>), AcceptError> {
        let ArmedAccept { _socket, arriving } = self;
        let Arrived {
            connecting,
            prelude,
        } = match tokio::time::timeout(ACCEPT_BUDGET, arriving).await {
            Ok(Ok(arrived)) => arrived,
            Ok(Err(_)) => return Err(AcceptError::Refused),
            Err(_) => return Err(AcceptError::TimedOut),
        };

        // After the transition both ends are raw, in both directions, until either closes.
        // The pairing is read-from-one/write-to-the-other in each direction; crossing them
        // would compile-fail here rather than silently unidirectional.
        let (mine, theirs) = tokio::io::duplex(MAX_INBOUND_READ);

        // The prelude goes in before the relay starts, so the Destination line is read
        // ahead of any application byte. Written as one segment where the config says so,
        // which is what makes a reader that discards its read tail lose bytes here.
        let mut opening = mine;
        opening
            .write_all(&prelude)
            .await
            .map_err(|_| AcceptError::Closed)?;
        let _ = opening.flush().await;

        let (mut peer_reader, mut peer_writer) = tokio::io::split(opening);
        let (mut client_reader, mut client_writer) = tokio::io::split(connecting);
        // Connecting peer -> accepting peer.
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_reader, &mut peer_writer).await;
        });
        // Accepting peer -> connecting peer.
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut peer_reader, &mut client_writer).await;
        });

        // The Destination line comes off the same stream the application bytes will, so
        // this read is the transition rather than a separate handshake.
        let mut stream = PeerStream {
            inner: theirs,
            buffered: Vec::new(),
        };
        let destination_line = stream.read_destination_line().await?;
        Ok((stream, destination_line))
    }
}

/// The raw application stream, as the accepting peer sees it.
///
/// Bytes read past the Destination newline are kept here rather than discarded, so the
/// first application read is exact even when the bridge coalesced the prelude and the
/// payload into one segment.
pub struct PeerStream {
    inner: tokio::io::DuplexStream,
    buffered: Vec<u8>,
}

impl PeerStream {
    /// Reads the peer-Destination line, keeping any bytes that arrived past its newline.
    ///
    /// Bounded on both ends. A ceiling rather than "read until a newline shows up", since
    /// an unbounded line is exactly the thing a router on the other end controls.
    async fn read_destination_line(&mut self) -> Result<Vec<u8>, AcceptError> {
        loop {
            if let Some(end) = line_end(&self.buffered) {
                if end > MAX_ACCEPT_DESTINATION_LINE {
                    return Err(AcceptError::PreludeTooLong);
                }
                return Ok(self.buffered.drain(..end).collect());
            }
            if self.buffered.len() > MAX_ACCEPT_DESTINATION_LINE {
                return Err(AcceptError::PreludeTooLong);
            }
            self.fill().await?;
        }
    }

    /// Reads exactly `count` bytes, taking anything already buffered first.
    pub async fn read_exact(&mut self, count: usize) -> Result<Vec<u8>, AcceptError> {
        while self.buffered.len() < count {
            self.fill().await?;
        }
        let taken: Vec<u8> = self.buffered.drain(..count).collect();
        Ok(taken)
    }

    /// Writes application bytes toward the connecting peer.
    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), AcceptError> {
        self.inner
            .write_all(bytes)
            .await
            .map_err(|_| AcceptError::Closed)
    }

    /// Pulls one bounded read into the buffer, or reports why it could not.
    async fn fill(&mut self) -> Result<(), AcceptError> {
        let mut chunk = vec![0u8; MAX_INBOUND_READ];
        let read = tokio::time::timeout(ACCEPT_BUDGET, self.inner.read(&mut chunk))
            .await
            .map_err(|_| AcceptError::TimedOut)?;
        let count = read.map_err(|_| AcceptError::Closed)?;
        if count == 0 {
            return Err(AcceptError::Closed);
        }
        self.buffered.extend_from_slice(&chunk[..count]);
        Ok(())
    }
}

/// The index just past the first `\n` in a buffer, if it holds one.
fn line_end(buffered: &[u8]) -> Option<usize> {
    buffered
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
}

/// A running fake bridge that serves both the connecting side and the inbound side.
///
/// Separate from [`FakeBridge`] because the two answer different questions. This one
/// exists to check that a `STREAM CONNECT` reaches an accept armed on its **own** socket,
/// which is the topology the previous live harness never built.
pub struct FakeSamPeer {
    addr: SocketAddr,
    accepts: Arc<std::sync::Mutex<Vec<PendingAccept>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl FakeSamPeer {
    /// Binds a loopback listener that answers both directions.
    pub async fn start(config: AcceptConfig) -> Self {
        let listener = TcpListener::bind(FAKE_BIND)
            .await
            .expect("the fake peer binds a loopback port");
        let addr = listener
            .local_addr()
            .expect("the bound address is readable");
        let accepts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handle = tokio::spawn(serve_inbound(listener, config, Arc::clone(&accepts)));
        Self {
            addr,
            accepts,
            handle,
        }
    }

    /// The loopback address to hand to a client.
    pub fn endpoint(&self) -> crate::endpoint::SamBridgeEndpoint {
        crate::endpoint::SamBridgeEndpoint::parse(&self.addr.to_string())
            .expect("a bound loopback address is a valid bridge endpoint")
    }

    /// How many accepts are armed and waiting for a connection.
    pub fn armed(&self) -> usize {
        self.accepts
            .lock()
            .expect("the accept registry is readable")
            .len()
    }

    /// Opens a control socket, says `HELLO`, and creates a session.
    ///
    /// Returns the socket and the Destination the bridge minted. The socket is returned so
    /// the caller can hold the session open; an accept issued on *this* socket has nowhere
    /// to deliver a connection, which is why [`FakeSamPeer::open_accept`] opens another.
    pub async fn create_session(&self) -> Result<(TcpStream, Vec<u8>), AcceptError> {
        let mut socket = TcpStream::connect(self.addr)
            .await
            .map_err(|_| AcceptError::Refused)?;
        let (mut lines, mut chunk) = say_hello(&mut socket).await?;
        socket
            .write_all(&line(
                "SESSION CREATE STYLE=STREAM ID=peer DESTINATION=TRANSIENT",
            ))
            .await
            .map_err(|_| AcceptError::Refused)?;
        let reply = read_line(&mut socket, &mut lines, &mut chunk)
            .await
            .ok_or(AcceptError::Closed)?;
        let destination = reply
            .split_whitespace()
            .find_map(|token| token.strip_prefix("DESTINATION="))
            .ok_or(AcceptError::Refused)?
            .as_bytes()
            .to_vec();
        Ok((socket, destination))
    }

    /// Opens a second socket, says `HELLO`, and arms `STREAM ACCEPT` for `session_id`.
    ///
    /// Returns once the bridge has acknowledged the accept `RESULT=OK`. The caller then
    /// drives it with [`ArmedAccept::incoming`], which is where the raw transition happens.
    pub async fn open_accept(&self, session_id: &str) -> Result<ArmedAccept, AcceptError> {
        {
            let registry = self
                .accepts
                .lock()
                .expect("the accept registry is readable");
            if registry.len() >= MAX_ARMED_ACCEPTS {
                return Err(AcceptError::Refused);
            }
        }
        let mut socket = TcpStream::connect(self.addr)
            .await
            .map_err(|_| AcceptError::Refused)?;
        let (mut lines, mut chunk) = say_hello(&mut socket).await?;
        socket
            .write_all(&line(&format!(
                "STREAM ACCEPT ID={session_id} SILENT=false"
            )))
            .await
            .map_err(|_| AcceptError::Refused)?;
        let status = read_line(&mut socket, &mut lines, &mut chunk)
            .await
            .ok_or(AcceptError::Closed)?;
        if !status.contains("RESULT=OK") {
            return Err(AcceptError::Refused);
        }
        // Registered only once the accept is acknowledged, so a `STREAM CONNECT` that
        // arrives cannot find a half-armed accept.
        let (arrived, arriving) = tokio::sync::oneshot::channel();
        self.accepts
            .lock()
            .expect("the accept registry is writable")
            .push(PendingAccept {
                session_id: session_id.to_owned(),
                arrived,
            });
        Ok(ArmedAccept {
            _socket: socket,
            arriving,
        })
    }
}

impl Drop for FakeSamPeer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Sends `HELLO` and drains the acknowledgement, so later reads start clean.
async fn say_hello(socket: &mut TcpStream) -> Result<(LineAccumulator, [u8; 512]), AcceptError> {
    socket
        .write_all(&line("HELLO VERSION MIN=3.1 MAX=3.1"))
        .await
        .map_err(|_| AcceptError::Refused)?;
    let mut reader = LineAccumulator::default();
    let mut chunk = [0u8; 512];
    while reader.take_line().is_none() {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|_| AcceptError::Refused)?;
        if count == 0 {
            return Err(AcceptError::Refused);
        }
        reader.push(&chunk[..count]);
    }
    Ok((reader, chunk))
}

/// Reads one more complete line, returning `None` if the peer hangs up first.
async fn read_line(
    socket: &mut TcpStream,
    reader: &mut LineAccumulator,
    chunk: &mut [u8],
) -> Option<String> {
    loop {
        match socket.read(chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => reader.push(&chunk[..count]),
        }
        if let Some(line) = reader.take_line() {
            return Some(line);
        }
    }
}

/// An accept waiting inside a serving task for a connection to be routed to it.
struct PendingAccept {
    session_id: String,
    arrived: tokio::sync::oneshot::Sender<Arrived>,
}

/// Pops the oldest armed accept, if any.
///
/// Its own function so the `std::sync::MutexGuard` is released before the caller's first
/// `await`; a guard held across a suspension would make every caller `!Send`.
fn take_pending(accepts: &Arc<std::sync::Mutex<Vec<PendingAccept>>>) -> Option<PendingAccept> {
    let mut registry = accepts.lock().expect("the accept registry is writable");
    (!registry.is_empty()).then(|| registry.remove(0))
}

/// Serves the connecting side and the inbound side from one listener.
///
/// One task per connection, because SAM is one socket per I2P socket: the control socket,
/// the accept socket, and each connecting socket are separate connections with their own
/// lifetimes.
async fn serve_inbound(
    listener: TcpListener,
    config: AcceptConfig,
    accepts: Arc<std::sync::Mutex<Vec<PendingAccept>>>,
) {
    while let Ok((mut socket, _)) = listener.accept().await {
        let accepts = Arc::clone(&accepts);
        let accept_status = config.accept_status.clone();
        let prelude = config.prelude.clone();
        tokio::spawn(async move {
            let mut reader = LineAccumulator::default();
            let mut chunk = [0u8; 1024];
            loop {
                let Some(request) = reader.take_line() else {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(count) => reader.push(&chunk[..count]),
                    }
                    continue;
                };
                if request.starts_with("HELLO") {
                    if socket.write_all(&hello_ok()).await.is_err() {
                        return;
                    }
                } else if request.starts_with("SESSION CREATE") {
                    if socket
                        .write_all(&session_ok_with_destination())
                        .await
                        .is_err()
                    {
                        return;
                    }
                } else if request.starts_with("STREAM ACCEPT") {
                    let status = accept_status.clone().unwrap_or_else(stream_ok);
                    if socket.write_all(&status).await.is_err() {
                        return;
                    }
                } else if request.starts_with("STREAM CONNECT") {
                    if socket.write_all(&stream_ok()).await.is_err() {
                        return;
                    }
                    // Route the connecting socket to the armed accept. FIFO rather than
                    // matched on destination: the fixture has one peer identity, so
                    // matching on destination would only be testing itself.
                    // Taken in a helper so the `std::sync` guard never spans an await: it
                    // is not `Send`, and holding one across a suspension would make this
                    // task `!Send`.
                    let Some(pending) = take_pending(&accepts) else {
                        return;
                    };
                    let _ = pending.session_id;
                    // The prelude is authored by the bridge and handed over with the
                    // connection, so the accepting side reads it off the same stream its
                    // application bytes will arrive on. Coalesced with the first payload
                    // where the config says so, which is what makes a reader that discards
                    // its read tail lose bytes here.
                    let prelude: Vec<u8> = match &prelude {
                        Prelude::Destination {
                            destination,
                            payload,
                        } => {
                            let mut line = destination.clone();
                            line.extend_from_slice(b"\r\n");
                            line.extend_from_slice(payload);
                            line
                        }
                        Prelude::Missing => Vec::new(),
                        Prelude::Oversized => {
                            let mut line = vec![b'Q'; MAX_ACCEPT_DESTINATION_LINE + 64];
                            line.extend_from_slice(b"\r\n");
                            line
                        }
                    };
                    let _ = pending.arrived.send(Arrived {
                        connecting: socket,
                        prelude,
                    });
                    // Everything after `RESULT=OK` on this socket belongs to the relay that
                    // `ArmedAccept::incoming` starts, so this connection's task is done.
                    return;
                }
            }
        });
    }
}
