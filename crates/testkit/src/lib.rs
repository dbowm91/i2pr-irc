//! Deterministic ordered bounded duplex streams and an I2P provider fixture.
use async_trait::async_trait;
use i2pr_irc_core::{
    ByteStream, ClientId, ConnectionGeneration, I2pEndpoint, I2pStreamProvider, LocalAcceptor,
    ProviderError,
};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::Notify,
};

pub const MAX_DUPLEX_CAPACITY: usize = 1024 * 1024;
pub const MAX_TRACE_CAPACITY: usize = 4096;
pub const MAX_PROVIDER_QUEUE: usize = 256;
pub const MAX_DEFERRED_COMPLETIONS: usize = 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureCapacityError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultScript {
    pub seed: u64,
    pub capacity: usize,
    pub max_read: [usize; 2],
    pub max_write: [usize; 2],
    pub read_stalled: [bool; 2],
    pub write_stalled: [bool; 2],
    pub eof_after_read: [Option<usize>; 2],
    pub reset_after_read: [Option<usize>; 2],
    pub capture_writes: bool,
    pub trace_capacity: usize,
}
impl Default for FaultScript {
    fn default() -> Self {
        Self {
            seed: 0,
            capacity: 64 * 1024,
            max_read: [usize::MAX; 2],
            max_write: [usize::MAX; 2],
            read_stalled: [false; 2],
            write_stalled: [false; 2],
            eof_after_read: [None; 2],
            reset_after_read: [None; 2],
            capture_writes: false,
            trace_capacity: 4096,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReproductionDescriptor {
    pub script: FaultScript,
    pub scenario: &'static str,
}
struct State {
    input: [VecDeque<u8>; 2],
    write_closed: [bool; 2],
    read_closed: [bool; 2],
    read_reset: [bool; 2],
    read_stalled: [bool; 2],
    write_stalled: [bool; 2],
    read_count: [usize; 2],
    read_wakers: [Option<Waker>; 2],
    write_wakers: [Option<Waker>; 2],
    captured: [Vec<u8>; 2],
    trace_truncated: [bool; 2],
    script: FaultScript,
}
struct Shared(Mutex<State>);
#[derive(Clone)]
pub struct FaultController(Arc<Shared>);
impl FaultController {
    pub fn stall_read(&self, side: usize, stalled: bool) {
        if side > 1 {
            return;
        }
        let mut s = self.0.0.lock().expect("fixture lock poisoned");
        s.read_stalled[side] = stalled;
        if !stalled {
            wake(&mut s.read_wakers[side]);
        }
    }
    pub fn stall_write(&self, side: usize, stalled: bool) {
        if side > 1 {
            return;
        }
        let mut s = self.0.0.lock().expect("fixture lock poisoned");
        s.write_stalled[side] = stalled;
        if !stalled {
            wake(&mut s.write_wakers[side]);
        }
    }
    pub fn reset_read(&self, side: usize) {
        if side > 1 {
            return;
        }
        let mut s = self.0.0.lock().expect("fixture lock poisoned");
        s.read_reset[side] = true;
        wake(&mut s.read_wakers[side]);
    }
    pub fn close_write(&self, side: usize) {
        if side > 1 {
            return;
        }
        let mut s = self.0.0.lock().expect("fixture lock poisoned");
        s.write_closed[side] = true;
        wake(&mut s.read_wakers[side]);
    }
    pub fn bytes_written(&self, side: usize) -> Vec<u8> {
        if side > 1 {
            return Vec::new();
        }
        self.0.0.lock().expect("fixture lock poisoned").captured[side].clone()
    }
    pub fn buffered_len(&self, side: usize) -> usize {
        if side > 1 {
            return 0;
        }
        self.0.0.lock().expect("fixture lock poisoned").input[side].len()
    }
    pub fn trace_truncated(&self, side: usize) -> bool {
        side < 2
            && self
                .0
                .0
                .lock()
                .expect("fixture lock poisoned")
                .trace_truncated[side]
    }
    pub fn descriptor(&self, scenario: &'static str) -> ReproductionDescriptor {
        ReproductionDescriptor {
            script: self
                .0
                .0
                .lock()
                .expect("fixture lock poisoned")
                .script
                .clone(),
            scenario,
        }
    }
}
fn wake(slot: &mut Option<Waker>) {
    if let Some(w) = slot.take() {
        w.wake()
    }
}
pub struct ScriptedStream {
    shared: Arc<Shared>,
    side: usize,
}
impl ScriptedStream {
    pub fn pair(mut script: FaultScript) -> (Self, Self, FaultController) {
        script.capacity = script.capacity.clamp(1, MAX_DUPLEX_CAPACITY);
        script.trace_capacity = script.trace_capacity.min(MAX_TRACE_CAPACITY);
        let state = State {
            input: [VecDeque::new(), VecDeque::new()],
            write_closed: [false; 2],
            read_closed: [false; 2],
            read_reset: [false; 2],
            read_stalled: script.read_stalled,
            write_stalled: script.write_stalled,
            read_count: [0; 2],
            read_wakers: [None, None],
            write_wakers: [None, None],
            captured: [Vec::new(), Vec::new()],
            trace_truncated: [false; 2],
            script,
        };
        let shared = Arc::new(Shared(Mutex::new(state)));
        (
            Self {
                shared: shared.clone(),
                side: 0,
            },
            Self {
                shared: shared.clone(),
                side: 1,
            },
            FaultController(shared),
        )
    }
}
impl AsyncRead for ScriptedStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let side = self.side;
        let mut s = self.shared.0.lock().expect("fixture lock poisoned");
        if s.read_reset[side] {
            s.read_reset[side] = false;
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "scripted reset",
            )));
        }
        if s.read_stalled[side] {
            s.read_wakers[side] = Some(cx.waker().clone());
            return Poll::Pending;
        }
        if s.script.reset_after_read[side].is_some_and(|n| s.read_count[side] >= n) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "scripted byte-boundary reset",
            )));
        }
        if s.script.eof_after_read[side].is_some_and(|n| s.read_count[side] >= n) {
            return Poll::Ready(Ok(()));
        }
        if s.input[side].is_empty() {
            if s.write_closed[1 - side] {
                return Poll::Ready(Ok(()));
            }
            s.read_wakers[side] = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let until_reset = s.script.reset_after_read[side]
            .map(|n| n.saturating_sub(s.read_count[side]))
            .unwrap_or(usize::MAX);
        let until_eof = s.script.eof_after_read[side]
            .map(|n| n.saturating_sub(s.read_count[side]))
            .unwrap_or(usize::MAX);
        let n = buf
            .remaining()
            .min(s.script.max_read[side].max(1))
            .min(s.input[side].len())
            .min(until_reset)
            .min(until_eof);
        if n == 0 {
            return Poll::Ready(Ok(()));
        }
        let bytes: Vec<u8> = (0..n).filter_map(|_| s.input[side].pop_front()).collect();
        buf.put_slice(&bytes);
        s.read_count[side] = s.read_count[side].saturating_add(n);
        wake(&mut s.write_wakers[1 - side]);
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for ScriptedStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let side = self.side;
        let peer = 1 - side;
        let mut s = self.shared.0.lock().expect("fixture lock poisoned");
        if s.write_closed[side] || s.read_closed[peer] {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "peer closed",
            )));
        }
        if s.write_stalled[side] {
            s.write_wakers[side] = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let available = s.script.capacity.saturating_sub(s.input[peer].len());
        if available == 0 {
            s.write_wakers[side] = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = bytes
            .len()
            .min(s.script.max_write[side].max(1))
            .min(available);
        s.input[peer].extend(&bytes[..n]);
        if s.script.capture_writes {
            let held = s.captured[side].len();
            let keep = n.min(s.script.trace_capacity.saturating_sub(held));
            s.captured[side].extend_from_slice(&bytes[..keep]);
            if keep < n {
                s.trace_truncated[side] = true;
            }
        }
        wake(&mut s.read_wakers[peer]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let side = self.side;
        let mut s = self.shared.0.lock().expect("fixture lock poisoned");
        s.write_closed[side] = true;
        wake(&mut s.read_wakers[1 - side]);
        Poll::Ready(Ok(()))
    }
}
impl Drop for ScriptedStream {
    fn drop(&mut self) {
        let mut s = self.shared.0.lock().expect("fixture lock poisoned");
        s.read_closed[self.side] = true;
        s.write_closed[self.side] = true;
        wake(&mut s.write_wakers[1 - self.side]);
        wake(&mut s.read_wakers[1 - self.side]);
    }
}

#[derive(Default)]
pub struct FakeI2pStreamProvider {
    requested: Mutex<Vec<I2pEndpoint>>,
    outcomes: Mutex<VecDeque<Result<FaultScript, ProviderError>>>,
    peers: Mutex<VecDeque<ScriptedStream>>,
    peer_ready: Notify,
}
impl FakeI2pStreamProvider {
    pub fn queue_outcome(
        &self,
        outcome: Result<FaultScript, ProviderError>,
    ) -> Result<(), FixtureCapacityError> {
        let mut q = self.outcomes.lock().expect("provider lock poisoned");
        if q.len() >= MAX_PROVIDER_QUEUE {
            return Err(FixtureCapacityError);
        }
        q.push_back(outcome);
        Ok(())
    }
    pub fn requested_endpoints(&self) -> Vec<I2pEndpoint> {
        self.requested
            .lock()
            .expect("provider lock poisoned")
            .clone()
    }
    pub fn clear_requested(&self) {
        self.requested
            .lock()
            .expect("provider lock poisoned")
            .clear()
    }
    pub async fn take_peer(&self) -> ScriptedStream {
        loop {
            if let Some(peer) = self
                .peers
                .lock()
                .expect("provider lock poisoned")
                .pop_front()
            {
                return peer;
            }
            self.peer_ready.notified().await;
        }
    }
}

/// Holds delayed completions so tests can resolve work from an old generation after replacement.
pub struct DeferredCompletions<T> {
    current: ConnectionGeneration,
    pending: VecDeque<(ConnectionGeneration, T)>,
}
impl<T> DeferredCompletions<T> {
    pub fn new(current: ConnectionGeneration) -> Self {
        Self {
            current,
            pending: VecDeque::new(),
        }
    }
    pub fn replace(&mut self, generation: ConnectionGeneration) {
        self.current = generation;
    }
    pub fn defer(
        &mut self,
        generation: ConnectionGeneration,
        value: T,
    ) -> Result<(), FixtureCapacityError> {
        if self.pending.len() >= MAX_DEFERRED_COMPLETIONS {
            return Err(FixtureCapacityError);
        }
        self.pending.push_back((generation, value));
        Ok(())
    }
    pub fn resolve_next(&mut self) -> Option<T> {
        while let Some((generation, value)) = self.pending.pop_front() {
            if generation == self.current {
                return Some(value);
            }
        }
        None
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}
#[async_trait]
impl I2pStreamProvider for FakeI2pStreamProvider {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError> {
        {
            let mut requested = self.requested.lock().expect("provider lock poisoned");
            if requested.len() >= MAX_PROVIDER_QUEUE {
                return Err(ProviderError::Failed);
            }
            requested.push(endpoint.clone());
        }
        let script = self
            .outcomes
            .lock()
            .expect("provider lock poisoned")
            .pop_front()
            .unwrap_or(Err(ProviderError::Unavailable))?;
        let (client, peer, _) = ScriptedStream::pair(script);
        {
            let mut peers = self.peers.lock().expect("provider lock poisoned");
            if peers.len() >= MAX_PROVIDER_QUEUE {
                return Err(ProviderError::Failed);
            }
            peers.push_back(peer);
        }
        self.peer_ready.notify_one();
        Ok(Box::new(client))
    }
}

#[derive(Default)]
pub struct FakeLocalAcceptor {
    outcomes: Mutex<VecDeque<Result<(ClientId, FaultScript), ProviderError>>>,
    peers: Mutex<VecDeque<ScriptedStream>>,
    peer_ready: Notify,
}
impl FakeLocalAcceptor {
    pub fn queue_outcome(
        &self,
        outcome: Result<(ClientId, FaultScript), ProviderError>,
    ) -> Result<(), FixtureCapacityError> {
        let mut q = self.outcomes.lock().expect("local acceptor lock poisoned");
        if q.len() >= MAX_PROVIDER_QUEUE {
            return Err(FixtureCapacityError);
        }
        q.push_back(outcome);
        Ok(())
    }
    pub async fn take_peer(&self) -> ScriptedStream {
        loop {
            if let Some(peer) = self
                .peers
                .lock()
                .expect("local acceptor lock poisoned")
                .pop_front()
            {
                return peer;
            }
            self.peer_ready.notified().await;
        }
    }
}
impl LocalAcceptor for FakeLocalAcceptor {
    type Stream = ScriptedStream;
    async fn accept(&self) -> Result<(ClientId, Self::Stream), ProviderError> {
        let (client_id, script) = self
            .outcomes
            .lock()
            .expect("local acceptor lock poisoned")
            .pop_front()
            .unwrap_or(Err(ProviderError::Unavailable))?;
        let (stream, peer, _) = ScriptedStream::pair(script);
        {
            let mut peers = self.peers.lock().expect("local acceptor lock poisoned");
            if peers.len() >= MAX_PROVIDER_QUEUE {
                return Err(ProviderError::Failed);
            }
            peers.push_back(peer);
        }
        self.peer_ready.notify_one();
        Ok((client_id, stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn short_io_remains_ordered() {
        let (a, mut b, c) = ScriptedStream::pair(FaultScript {
            max_read: [2, 3],
            max_write: [2, 3],
            capture_writes: true,
            ..Default::default()
        });
        let writer = tokio::spawn(async move {
            let mut a = a;
            a.write_all(b"abcdef").await.unwrap();
        });
        let mut bytes = Vec::new();
        b.read_to_end(&mut bytes).await.unwrap();
        writer.await.unwrap();
        assert_eq!(bytes, b"abcdef");
        assert_eq!(c.bytes_written(0), b"abcdef");
    }
    #[tokio::test]
    async fn bounded_backpressure_resumes_after_read() {
        let (a, mut b, _) = ScriptedStream::pair(FaultScript {
            capacity: 3,
            ..Default::default()
        });
        let writer = tokio::spawn(async move {
            let mut a = a;
            a.write_all(b"abcdef").await.unwrap();
        });
        let mut bytes = [0; 6];
        b.read_exact(&mut bytes).await.unwrap();
        writer.await.unwrap();
        assert_eq!(&bytes, b"abcdef");
    }
    #[tokio::test]
    async fn independent_stall_and_resume() {
        let (a, mut b, c) = ScriptedStream::pair(FaultScript {
            read_stalled: [false, true],
            ..Default::default()
        });
        let mut a = a;
        let read = tokio::spawn(async move {
            let mut byte = [0];
            b.read_exact(&mut byte).await.unwrap();
            byte
        });
        a.write_all(b"x").await.unwrap();
        tokio::task::yield_now().await;
        c.stall_read(1, false);
        assert_eq!(read.await.unwrap(), [b'x']);
    }
    #[tokio::test]
    async fn reset_is_distinct_from_eof() {
        let (a, mut b, c) = ScriptedStream::pair(FaultScript::default());
        let mut a = a;
        a.write_all(b"x").await.unwrap();
        c.reset_read(1);
        let mut byte = [0];
        assert_eq!(
            b.read(&mut byte).await.unwrap_err().kind(),
            io::ErrorKind::ConnectionReset
        );
        assert_eq!(b.read(&mut byte).await.unwrap(), 1);
    }
    #[tokio::test]
    async fn eof_after_scripted_bytes() {
        let (a, mut b, _) = ScriptedStream::pair(FaultScript {
            eof_after_read: [None, Some(1)],
            ..Default::default()
        });
        let mut a = a;
        a.write_all(b"xy").await.unwrap();
        let mut bytes = Vec::new();
        b.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"x");
    }
    #[tokio::test]
    async fn reset_after_scripted_byte_boundary() {
        let (a, mut b, _) = ScriptedStream::pair(FaultScript {
            reset_after_read: [None, Some(1)],
            ..Default::default()
        });
        let mut a = a;
        a.write_all(b"xy").await.unwrap();
        let mut first = [0];
        b.read_exact(&mut first).await.unwrap();
        assert_eq!(first, [b'x']);
        assert_eq!(
            b.read(&mut first).await.unwrap_err().kind(),
            io::ErrorKind::ConnectionReset
        );
    }
    #[test]
    fn stale_generation_completion_is_discarded() {
        let old = ConnectionGeneration(1);
        let new = ConnectionGeneration(2);
        let mut pending = DeferredCompletions::new(old);
        pending.defer(old, "old").unwrap();
        pending.replace(new);
        pending.defer(new, "new").unwrap();
        assert_eq!(pending.resolve_next(), Some("new"));
        assert_eq!(pending.pending_len(), 0);
    }
    #[test]
    fn fixture_collections_have_explicit_ceilings() {
        let mut pending = DeferredCompletions::new(ConnectionGeneration(1));
        for n in 0..MAX_DEFERRED_COMPLETIONS {
            pending.defer(ConnectionGeneration(1), n).unwrap();
        }
        assert_eq!(
            pending.defer(ConnectionGeneration(1), MAX_DEFERRED_COMPLETIONS),
            Err(FixtureCapacityError)
        );
        let provider = FakeI2pStreamProvider::default();
        for _ in 0..MAX_PROVIDER_QUEUE {
            provider
                .queue_outcome(Err(ProviderError::Unavailable))
                .unwrap();
        }
        assert_eq!(
            provider.queue_outcome(Err(ProviderError::Unavailable)),
            Err(FixtureCapacityError)
        );
    }
    #[tokio::test]
    async fn fake_provider_records_endpoint_and_connects_duplex_peer() {
        let provider = FakeI2pStreamProvider::default();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let endpoint = I2pEndpoint::parse("irc.example.i2p").unwrap();
        let mut stream = provider.connect(&endpoint).await.unwrap();
        let mut peer = provider.take_peer().await;
        assert_eq!(provider.requested_endpoints(), vec![endpoint]);
        tokio::io::AsyncWriteExt::write_all(&mut peer, b"PING\r\n")
            .await
            .unwrap();
        let mut bytes = [0; 6];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut bytes)
            .await
            .unwrap();
        assert_eq!(&bytes, b"PING\r\n");
    }
    #[tokio::test]
    async fn local_acceptor_is_a_separate_local_stream_fixture() {
        let acceptor = FakeLocalAcceptor::default();
        acceptor
            .queue_outcome(Ok((ClientId(7), FaultScript::default())))
            .unwrap();
        let (id, mut stream) = acceptor.accept().await.unwrap();
        assert_eq!(id, ClientId(7));
        let mut peer = acceptor.take_peer().await;
        tokio::io::AsyncWriteExt::write_all(&mut peer, b"NICK x\r\n")
            .await
            .unwrap();
        let mut bytes = [0; 8];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut bytes)
            .await
            .unwrap();
        assert_eq!(&bytes, b"NICK x\r\n");
    }
    #[tokio::test]
    async fn stalled_writer_wakes_after_gate_release() {
        let (a, mut b, control) = ScriptedStream::pair(FaultScript {
            write_stalled: [true, false],
            capacity: 2,
            ..Default::default()
        });
        let writer = tokio::spawn(async move {
            let mut a = a;
            a.write_all(b"ok").await.unwrap();
        });
        tokio::task::yield_now().await;
        assert_eq!(control.buffered_len(1), 0);
        control.stall_write(0, false);
        let mut bytes = [0; 2];
        b.read_exact(&mut bytes).await.unwrap();
        writer.await.unwrap();
        assert_eq!(&bytes, b"ok");
    }
    #[test]
    fn capture_is_bounded_and_disabled_by_default() {
        let (a, _peer, c) = ScriptedStream::pair(FaultScript {
            capture_writes: true,
            trace_capacity: 2,
            ..Default::default()
        });
        let mut a = a;
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            std::pin::Pin::new(&mut a).poll_write(&mut cx, b"secret"),
            Poll::Ready(Ok(6))
        ));
        assert_eq!(c.bytes_written(0), b"se");
        assert!(c.trace_truncated(0));
        assert_eq!(c.descriptor("bounded").script.seed, 0);
    }
}
