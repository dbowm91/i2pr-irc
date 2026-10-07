//! The state machines: one handshake, one session, one stream.
//!
//! # Why these are plain async fns and not tasks
//!
//! Plan 030 section 11 requires that dropping a pending client future drops its socket
//! and leaves no detached task. The cheapest way to get that property is to have no task:
//! a `TcpStream` held across an `await` in an ordinary future is dropped with the future,
//! and there is no handle to leak. A `tokio::spawn` would need a cancellation channel, a
//! join, and a state to report back through — three more places to be wrong, all in
//! exchange for concurrency this exchange does not want.
//!
//! # The stream's shape
//!
//! A successful `STREAM CONNECT` turns the *same* socket from a line protocol into a raw
//! byte stream. Nothing is buffered across that boundary and nothing is parsed after it,
//! so a router that sent a line immediately after `RESULT=OK` delivers those bytes
//! verbatim. That is correct: it is upstream data, and SAM framing has ended.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::timeout,
};

use i2pr_irc_core::I2pEndpoint;

use crate::{
    endpoint::SamBridgeEndpoint,
    error::{MalformedReason, SamError, SamPhase, StreamRejection},
    line::{LineReader, SamLine},
    protocol::{
        MAX_REQUEST_BYTES, SamState, classify, hello_request, session_create_request,
        stream_connect_request,
    },
    session_id::{RandomSource, SamSessionId},
};

/// The deadline for every phase of an exchange.
///
/// A struct rather than five constants so a test can inject a profile that finishes
/// immediately and no test can depend on a production sleep. Each value carries the
/// reasoning for itself, which is more useful than a comment at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamTimeouts {
    /// Opening the TCP connection to the bridge.
    pub bridge_connect: Duration,
    /// `HELLO` and its reply.
    pub hello: Duration,
    /// `SESSION CREATE`, including the tunnel build it triggers.
    ///
    /// The largest value here by a wide margin, because building a lease set and two
    /// inbound and two outbound tunnels on a cold router is genuinely slow. A deadline
    /// that fired during a normal first connect would turn a working router into a
    /// permanently failing Network.
    pub session_create: Duration,
    /// `STREAM CONNECT` and its single reply.
    ///
    /// Above the SAM documentation's approximately-one-minute router timeout on purpose.
    /// A client deadline *shorter* than the router's manufactures failures: the router is
    /// still working and the client has already given up. Being longer means the router's
    /// own timeout answers first, which is the better outcome because it carries a
    /// classified reason.
    pub stream_connect: Duration,
}

impl Default for SamTimeouts {
    fn default() -> Self {
        Self {
            bridge_connect: Duration::from_secs(10),
            hello: Duration::from_secs(10),
            session_create: Duration::from_secs(120),
            stream_connect: Duration::from_secs(90),
        }
    }
}

impl SamTimeouts {
    /// A profile that fails immediately, for tests asserting a deadline.
    pub fn immediate() -> Self {
        let instant = Duration::ZERO;
        Self {
            bridge_connect: instant,
            hello: instant,
            session_create: instant,
            stream_connect: instant,
        }
    }

    /// Every phase deadline, for a caller that has to bound one phase.
    fn of(&self, phase: SamPhase) -> Duration {
        match phase {
            SamPhase::BridgeConnect => self.bridge_connect,
            SamPhase::Hello => self.hello,
            SamPhase::SessionCreate => self.session_create,
            SamPhase::StreamConnect => self.stream_connect,
        }
    }
}

/// A byte stream that has left the SAM protocol.
pub struct SamRawStream {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    /// Bytes the reader had already consumed past the last SAM line.
    ///
    /// Drained before any socket read. A router that writes application bytes in the
    /// same segment as `RESULT=OK` would otherwise lose them, and losing the first bytes
    /// of a stream is exactly the failure this accounts for.
    buffered: std::collections::VecDeque<u8>,
}

impl SamRawStream {
    fn new(stream: TcpStream, leftover: Vec<u8>) -> Self {
        // Every byte here followed the `RESULT=OK` line, so it is application data by
        // definition: the reader consumed exactly up to the line's terminator and this is
        // everything after it.
        let (read, write) = stream.into_split();
        Self {
            read,
            write,
            buffered: leftover.into(),
        }
    }

    /// Whether any pre-read bytes are being served ahead of the socket.
    pub fn has_buffered(&self) -> bool {
        !self.buffered.is_empty()
    }
}

impl AsyncRead for SamRawStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.buffered.is_empty() {
            // Nothing here can fail: a non-empty `VecDeque` can always satisfy a
            // non-empty `ReadBuf`, and a zero-length request is answered as empty.
            let count = this.buffered.len().min(buf.remaining());
            let mut filled = buf.initialize_unfilled().to_vec();
            let count = count.min(filled.len());
            for slot in filled.iter_mut().take(count) {
                *slot = this.buffered.pop_front().expect("the length was checked");
            }
            buf.put_slice(&filled[..count]);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.read).poll_read(cx, buf)
    }
}

impl AsyncWrite for SamRawStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().write).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_shutdown(cx)
    }
}

impl Drop for SamRawStream {
    fn drop(&mut self) {
        // AsyncWrite needs a poll for a real shutdown, and a destructor cannot await.
        // What can be done synchronously is drop the write half, which closes its half of
        // the socket; the router observes the end of stream either way.
    }
}

/// Everything an exchange needs, supplied by the caller.
///
/// One struct rather than four constructor arguments, because the identity source and the
/// deadline profile have to travel together: a test that injects a deterministic ID source
/// and forgets to inject a fast timeout produces a test that takes two minutes.
pub struct SamClientConfig {
    /// The loopback bridge address. Unrepresentable other than loopback.
    pub bridge: SamBridgeEndpoint,
    /// Per-phase deadlines.
    pub timeouts: SamTimeouts,
    /// Source of session-ID randomness.
    pub random: Arc<dyn RandomSource>,
}

impl SamClientConfig {
    /// The production configuration: default loopback, production deadlines, OS
    /// randomness.
    pub fn production() -> Self {
        Self {
            bridge: SamBridgeEndpoint::DEFAULT,
            timeouts: SamTimeouts::default(),
            random: crate::session_id::os_random(),
        }
    }
}

/// A SAM control socket mid-exchange.
///
/// Holds the socket and the line reader for one control channel. One instance is one
/// socket: it is neither `Clone` nor `Send`-shared, so two callers cannot both be
/// reading the same control channel and each believe they own the session.
pub struct SamClient {
    config: SamClientConfig,
    state: SamState,
    reader: LineReader,
    stream: TcpStream,
    /// Bytes read from the socket that the reader has not yet turned into a line.
    ///
    /// Held here rather than inside the reader because the reader cannot know when a
    /// complete line ends in the middle of a chunk: two replies in one TCP segment is
    /// normal, and the second one must survive to the next read.
    pending: Vec<u8>,
    /// Complete lines already read but not yet consumed, each with the byte offset at
    /// which it ended.
    ///
    /// This is what makes two replies in one TCP segment work: feeding a chunk to the
    /// reader can yield several lines at once, and consuming only the first would discard
    /// the rest -- silently losing a reply the router really did send.
    ready: std::collections::VecDeque<(String, usize)>,
    /// Every byte fed to the reader that no consumed line has covered yet.
    ///
    /// This is what makes the raw transition exact rather than approximate. A router may
    /// write application bytes in the same TCP segment as `RESULT=OK`; those bytes sit
    /// behind the line in the reader's buffer and must be handed to the caller, or the
    /// first bytes of the upstream conversation are lost. Bounded by the line ceiling
    /// plus one read chunk, because consumed lines drain it on every read.
    inflight: std::collections::VecDeque<u8>,
    /// Bytes already accounted for by consumed lines.
    consumed: usize,
}

impl Clone for SamClientConfig {
    fn clone(&self) -> Self {
        Self {
            bridge: self.bridge,
            timeouts: self.timeouts,
            random: Arc::clone(&self.random),
        }
    }
}

impl SamClient {
    /// Opens a control socket and performs the hello exchange.
    ///
    /// Returns the client and the freshly minted session ID. The ID is minted *after*
    /// the hello rather than before so that a socket which never got a hello did not
    /// consume an ID; the retry gets a different one either way, which is the property
    /// that matters.
    pub async fn open(config: SamClientConfig) -> Result<(Self, SamSessionId), SamError> {
        let mut client = Self::open_socket(&config).await?;
        client.say(SamPhase::Hello, &hello_request()).await?;
        let id = SamSessionId::generate(config.random.as_ref())
            .map_err(|_| SamError::RandomUnavailable)?;
        // Advanced before the read rather than after the send: `classify` matches on the
        // state a reply is *expected* in, so the state has to be the awaiting one by the
        // time a reply can arrive.
        client.state = SamState::AwaitingHello;
        client.expect(SamPhase::Hello).await?;
        Ok((client, id))
    }

    /// Opens the socket without speaking. Used by tests and by the session path.
    pub async fn open_socket(config: &SamClientConfig) -> Result<Self, SamError> {
        let stream = timeout(
            config.timeouts.bridge_connect,
            TcpStream::connect(config.bridge.socket_addr()),
        )
        .await
        .map_err(|_| SamError::Timeout {
            phase: SamPhase::BridgeConnect,
        })?
        .map_err(|_| SamError::BridgeUnavailable)?;
        // Nagle would hold a handshake back waiting for bytes that are not coming.
        let _ = stream.set_nodelay(true);
        Ok(Self {
            config: config.clone(),
            state: SamState::HelloPending,
            reader: LineReader::new(),
            stream,
            pending: Vec::new(),
            ready: std::collections::VecDeque::new(),
            inflight: std::collections::VecDeque::new(),
            consumed: 0,
        })
    }

    /// The current sequencing state.
    pub fn state(&self) -> SamState {
        self.state
    }

    /// Performs `SESSION CREATE` on this control socket.
    ///
    /// On success the socket is a control channel: subsequent calls to
    /// [`Self::connect_stream`] may use the same session ID.
    pub async fn create_session(&mut self, id: &SamSessionId) -> Result<(), SamError> {
        if self.state != SamState::AwaitingSession {
            return Err(SamError::Malformed {
                reason: MalformedReason::UnexpectedVerb,
            });
        }
        self.say(
            SamPhase::SessionCreate,
            &session_create_request(id.as_str()),
        )
        .await?;
        self.expect(SamPhase::SessionCreate).await?;
        Ok(())
    }

    /// Issues `STREAM CONNECT` and returns the resulting raw byte stream.
    ///
    /// The returned stream is the same socket. From this point nothing on it is parsed
    /// as SAM.
    /// Issues `STREAM CONNECT` and returns the resulting raw byte stream.
    ///
    /// Takes `self` by value. The exchange consumes the control socket — after
    /// `RESULT=OK` that socket *is* the stream — so consuming the client here is what
    /// makes it impossible for two callers to believe they both own one session.
    pub async fn connect_stream(
        mut self,
        id: &SamSessionId,
        destination: &I2pEndpoint,
    ) -> Result<SamRawStream, SamError> {
        if self.state != SamState::AwaitingSession && self.state != SamState::SessionActive {
            return Err(SamError::Malformed {
                reason: MalformedReason::UnexpectedVerb,
            });
        }
        let line = stream_connect_request(id.as_str(), destination.as_str());
        if line.len() > MAX_REQUEST_BYTES {
            // Refused rather than truncated. A truncated Destination is a silently wrong
            // target, which is the one failure mode this crate must make impossible.
            return Err(SamError::Malformed {
                reason: MalformedReason::Overflowed,
            });
        }
        self.say(SamPhase::StreamConnect, &line).await?;
        // Advanced before the read, for the same reason as the hello above: `classify`
        // matches on the state a reply is expected in.
        self.state = SamState::AwaitingStream;
        self.expect(SamPhase::StreamConnect).await?;
        debug_assert_eq!(self.state, SamState::RawStream);
        // Anything the reader already buffered past the `RESULT=OK` line is upstream
        // data, not protocol. It is handed to the caller rather than parsed, because a
        // router may write application bytes immediately after acknowledging, and
        // treating those as a further SAM line would lose the first bytes of the
        // upstream conversation.
        let leftover: Vec<u8> = self.inflight.drain(..).collect();
        let stream = SamRawStream::new(self.stream, leftover);
        Ok(stream)
    }

    /// Marks every byte up to `end` as belonging to a consumed line.
    fn drain_to(&mut self, end: usize) {
        while self.consumed < end {
            if self.inflight.pop_front().is_none() {
                break;
            }
            self.consumed = self.consumed.saturating_add(1);
        }
    }

    /// Writes one request line under `phase`'s deadline.
    async fn say(&mut self, phase: SamPhase, line: &str) -> Result<(), SamError> {
        debug_assert!(
            line.ends_with("\r\n"),
            "every request line is CRLF-terminated: {line:.40}"
        );
        timeout(
            self.config.timeouts.of(phase),
            self.stream.write_all(line.as_bytes()),
        )
        .await
        .map_err(|_| SamError::Timeout { phase })?
        .map_err(|_| SamError::Closed)
    }

    /// Reads exactly one classifiable reply and applies it to the current state.
    ///
    /// Reads until a reply that this state can interpret arrives, rather than until the
    /// first line does. A bridge may interleave a status or keepalive line, and a reader
    /// that gave up on the first would fail a connection that is actually working. The
    /// phase deadline bounds the whole loop, so skipping lines cannot hang.
    async fn expect(&mut self, phase: SamPhase) -> Result<(), SamError> {
        match timeout(self.config.timeouts.of(phase), self.read_until(phase)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(SamError::Timeout { phase }),
        }
    }

    async fn read_until(&mut self, phase: SamPhase) -> Result<(), SamError> {
        loop {
            if let Some(text) = self.next_line(phase).await? {
                let transition = classify(self.state, &text)?;
                if let Some(error) = transition.as_error() {
                    return Err(error);
                }
                if let Some(next) = transition.next_state(self.state) {
                    self.state = next;
                    return Ok(());
                }
            }
            // A line that was unusable, or a classifiable reply that carried no state
            // change: keep reading within the caller's deadline.
        }
    }

    /// Reads the next complete line, or `None` when every line so far was unusable.
    ///
    /// The reader owns the partial-line buffer, so a `STREAM STATUS` split across two
    /// TCP segments is reassembled here rather than being lost at a read boundary.
    async fn next_line(&mut self, phase: SamPhase) -> Result<Option<String>, SamError> {
        loop {
            // Anything the previous chunk already completed is served first, before
            // another read is attempted: those bytes are already in hand.
            if let Some((text, end)) = self.ready.pop_front() {
                self.drain_to(end);
                return Ok(Some(text));
            }
            let chunk = std::mem::take(&mut self.pending);
            // Recorded before parsing, so the offset a completed line reports lines up
            // with what was fed. `drain_to` then removes exactly the consumed prefix.
            self.inflight.extend(chunk.iter().copied());
            let produced = self.reader.push(&chunk);
            self.pending = self.reader.take_partial();
            for line in produced {
                if let SamLine::Complete { text, end } = line {
                    self.ready.push_back((text.to_string(), end));
                }
                // An `Overflowed` line is dropped here: the reader has already counted
                // it and the exchange continues, which is the recovery property the
                // framing tests pin.
            }
            if let Some((text, end)) = self.ready.pop_front() {
                self.drain_to(end);
                return Ok(Some(text));
            }
            let mut chunk = [0u8; 1024];
            // Bounded by the *caller's* phase, not a fixed one, so a timeout is always
            // attributed to the exchange that was actually waiting. The outer `expect`
            // timeout still bounds the whole loop; this only stops one blocking read from
            // outliving it.
            let count = timeout(
                self.config.timeouts.of(phase).max(Duration::from_millis(1)),
                self.stream.read(&mut chunk),
            )
            .await
            .map_err(|_| SamError::Timeout { phase })?
            .map_err(|_| SamError::Closed)?;
            if count == 0 {
                return Err(SamError::Closed);
            }
            self.pending.extend_from_slice(&chunk[..count]);
        }
    }
}

/// Maps a `STREAM STATUS` failure onto the taxonomy the runtime maps onto reconnect.
pub fn stream_error(rejection: StreamRejection) -> SamError {
    SamError::PeerUnavailable { rejection }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_production_profile_matches_the_planned_values() {
        let timeouts = SamTimeouts::default();
        assert_eq!(timeouts.bridge_connect, Duration::from_secs(10));
        assert_eq!(timeouts.hello, Duration::from_secs(10));
        assert_eq!(timeouts.session_create, Duration::from_secs(120));
        assert_eq!(timeouts.stream_connect, Duration::from_secs(90));
    }

    /// The one deadline relationship that matters, asserted as a fact.
    ///
    /// A client `STREAM CONNECT` deadline below the router's approximately-one-minute
    /// attempt window manufactures failures: the router is still working when the client
    /// gives up.
    #[test]
    fn the_stream_deadline_exceeds_the_routers_own_attempt_window() {
        assert!(
            SamTimeouts::default().stream_connect >= Duration::from_secs(60),
            "the stream deadline must not preempt the router"
        );
    }

    /// The cold-session path does **not** fit the runtime's connect budget, and this is
    /// recorded as a finding rather than papered over.
    ///
    /// Plan 030 section 10 says "Plan 031 must reconcile the outer runtime
    /// provider-connect timeout with the complete cold-session path", so the mismatch is
    /// expected at this stage: 10 + 10 + 120 = 140 s of deadlines against a
    /// `CONNECT_TIMEOUT` of 120 s. A cold first connect would be cut off by the runtime
    /// while the router was still building tunnels, which fails a working Network.
    ///
    /// The numbers are asserted as *this* state so that a later plan which fixes the
    /// budget has to update this test deliberately. Silently changing either side is how
    /// the mismatch would have gone unnoticed.
    ///
    /// The runtime constant is inlined rather than imported: an adapter crate must not
    /// depend on the component that consumes it, and a duplicate that drifts is better
    /// than a dependency cycle.
    #[test]
    fn the_cold_session_path_exceeds_the_runtime_connect_budget() {
        const RUNTIME_CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
        let timeouts = SamTimeouts::default();
        let cold_session = timeouts.bridge_connect + timeouts.hello + timeouts.session_create;
        assert_eq!(
            cold_session,
            Duration::from_secs(140),
            "the SAM-side cold path is 140s against a 120s runtime budget"
        );
        assert!(
            cold_session > RUNTIME_CONNECT_TIMEOUT,
            "the mismatch Plan 031 must resolve is real, not hypothetical: {cold_session:?} > {RUNTIME_CONNECT_TIMEOUT:?}"
        );
    }

    #[test]
    fn the_test_profile_fails_immediately_rather_than_slowly() {
        let timeouts = SamTimeouts::immediate();
        assert!(timeouts.bridge_connect.is_zero());
        assert!(timeouts.session_create.is_zero());
    }

    /// `SamRawStream` must satisfy the core contract, or the runtime cannot use it at all.
    #[test]
    fn a_raw_stream_satisfies_the_core_byte_stream_contract() {
        fn assert_byte_stream<T: i2pr_irc_core::ByteStream>() {}
        assert_byte_stream::<SamRawStream>();
    }

    /// The production configuration reaches only loopback.
    #[test]
    fn the_production_configuration_points_at_loopback() {
        let config = SamClientConfig::production();
        assert!(config.bridge.ip().is_loopback());
        assert_eq!(config.bridge.port(), 7656);
    }

    #[test]
    fn every_phase_has_a_deadline() {
        let timeouts = SamTimeouts::default();
        for phase in [
            SamPhase::BridgeConnect,
            SamPhase::Hello,
            SamPhase::SessionCreate,
            SamPhase::StreamConnect,
        ] {
            assert!(
                !timeouts.of(phase).is_zero(),
                "{} must have a real deadline",
                phase.as_str()
            );
        }
    }
}
