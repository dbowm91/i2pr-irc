//! Domain identities and explicit stream/time capability contracts.
use async_trait::async_trait;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub u64);
    };
}
id!(NetworkId);
id!(ClientId);
id!(ConnectionGeneration);
id!(BufferId);
id!(HistoryEventId);
/// Ephemeral identity for exactly one live downstream attachment.
///
/// A `SessionId` is allocated locally when a client attaches, is never written to
/// durable storage, and is never restored after a process restart. It answers
/// "which connection owns this CAP state, queue, and response route?", which is a
/// different question from the durable [`ClientId`] lineage that owns playback and
/// read state.
///
/// A durable `ClientId` that reconnects always receives a fresh `SessionId`, so a
/// late reply from a previous attachment can never be delivered to its replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SessionId(pub u64);
/// Explicit ceiling on distinct attachments one process may identify in a lifetime.
///
/// Exhaustion is reported instead of wrapping, because a reused `SessionId` would
/// make an old response route indistinguishable from a current one.
pub const MAX_SESSION_IDS: u64 = 1 << 32;
/// Bounded local allocator for ephemeral attachment identity.
#[derive(Clone, Debug, Default)]
pub struct SessionIdAllocator(Arc<AtomicU64>);
impl SessionIdAllocator {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(1)))
    }
    /// Allocates the next attachment identity, or `None` once [`MAX_SESSION_IDS`] is
    /// reached. It never wraps and never reissues a previous value.
    pub fn allocate(&self) -> Option<SessionId> {
        let next = self.0.fetch_add(1, Ordering::SeqCst);
        if next >= MAX_SESSION_IDS {
            self.0.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(SessionId(next))
    }
}
pub const MAX_I2P_ENDPOINT_BYTES: usize = 516;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum I2pEndpointKind {
    Hostname,
    StandardBase32,
    ExtendedBase32,
    Destination,
}
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct I2pEndpoint {
    kind: I2pEndpointKind,
    canonical: String,
}
impl fmt::Debug for I2pEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("I2pEndpoint([redacted])")
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("invalid I2P endpoint")]
    Invalid,
}
impl I2pEndpoint {
    pub fn parse(value: &str) -> Result<Self, EndpointError> {
        if value.is_empty()
            || value.len() > MAX_I2P_ENDPOINT_BYTES
            || value.ends_with('.')
            || value.bytes().any(|b| {
                b.is_ascii_whitespace() || b.is_ascii_control() || matches!(b, b'/' | b':' | b'@')
            })
        {
            return Err(EndpointError::Invalid);
        }
        let lower = value.to_ascii_lowercase();
        let (kind, canonical) = if let Some(label) = lower.strip_suffix(".b32.i2p") {
            if label.len() == 52
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
            {
                (I2pEndpointKind::StandardBase32, lower)
            } else if (56..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
            {
                (I2pEndpointKind::ExtendedBase32, lower)
            } else {
                return Err(EndpointError::Invalid);
            }
        } else if lower.ends_with(".i2p") {
            let valid = lower.len() <= 67
                && lower.split('.').all(|part| {
                    !part.is_empty()
                        && part.len() <= 63
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        && part
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && part
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                });
            if !valid {
                return Err(EndpointError::Invalid);
            }
            (I2pEndpointKind::Hostname, lower)
        } else if value.len() == 516
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'~'))
        {
            (I2pEndpointKind::Destination, value.to_owned())
        } else {
            return Err(EndpointError::Invalid);
        };
        Ok(Self { kind, canonical })
    }
    pub fn as_str(&self) -> &str {
        &self.canonical
    }
    pub fn kind(&self) -> I2pEndpointKind {
        self.kind
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    #[error("destination unavailable")]
    Unavailable,
    #[error("connection timed out")]
    Timeout,
    #[error("provider stopped")]
    Cancelled,
    #[error("provider failure")]
    Failed,
}
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ByteStream for T {}
/// A provider future owns no detached connection task. Dropping the future cancels a
/// pending attempt; any returned stream belongs to exactly one connection generation.
#[async_trait]
pub trait I2pStreamProvider: Send + Sync {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError>;
}
/// Accepts only the local downstream side. It is not an upstream connector.
pub trait LocalAcceptor: Send + Sync {
    type Stream: ByteStream;
    fn accept(
        &self,
    ) -> impl std::future::Future<Output = Result<(ClientId, Self::Stream), ProviderError>> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct MonoTime(pub u64);
pub const MAX_VIRTUAL_TIMERS: usize = 4096;
pub trait Clock: Send + Sync {
    fn now(&self) -> MonoTime;
}
pub trait Timer: Send + Sync {
    type Sleep: Future<Output = Result<(), TimerError>> + Send;
    fn sleep_until(&self, deadline: MonoTime) -> Self::Sleep;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerError {
    CapacityExceeded,
}
struct ClockState {
    now: AtomicU64,
    next_id: AtomicU64,
    waiters: Mutex<Vec<(u64, MonoTime, Waker)>>,
}
#[derive(Clone)]
pub struct VirtualClock(Arc<ClockState>);
impl Default for VirtualClock {
    fn default() -> Self {
        Self(Arc::new(ClockState {
            now: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            waiters: Mutex::new(Vec::new()),
        }))
    }
}
impl VirtualClock {
    pub fn advance(&self, d: Duration) {
        let nanos = d.as_nanos().min(u64::MAX as u128) as u64;
        let now = self
            .0
            .now
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(v.saturating_add(nanos))
            })
            .unwrap_or_else(|v| v)
            .saturating_add(nanos);
        let mut wake = Vec::new();
        if let Ok(mut waiters) = self.0.waiters.lock() {
            waiters.sort_by_key(|(id, deadline, _)| (deadline.0, *id));
            waiters.retain(|(_, deadline, waker)| {
                if deadline.0 <= now {
                    wake.push(waker.clone());
                    false
                } else {
                    true
                }
            });
        }
        for waker in wake {
            waker.wake();
        }
    }
    pub fn sleep_for(&self, d: Duration) -> VirtualSleep {
        let delta = d.as_nanos().min(u64::MAX as u128) as u64;
        self.sleep_until(MonoTime(self.now().0.saturating_add(delta)))
    }
}
impl Clock for VirtualClock {
    fn now(&self) -> MonoTime {
        MonoTime(self.0.now.load(Ordering::SeqCst))
    }
}
pub struct VirtualSleep {
    state: Arc<ClockState>,
    id: u64,
    deadline: MonoTime,
    done: bool,
}
/// Dropping this future cancels its timer registration. VirtualClock bounds live timers.
impl Future for VirtualSleep {
    type Output = Result<(), TimerError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), TimerError>> {
        let state = self.state.clone();
        let id = self.id;
        let deadline = self.deadline;
        let mut waiters = state.waiters.lock().expect("virtual clock lock poisoned");
        if state.now.load(Ordering::SeqCst) >= deadline.0 {
            drop(waiters);
            self.done = true;
            return Poll::Ready(Ok(()));
        }
        if let Some((_, _, waker)) = waiters
            .iter_mut()
            .find(|(waiter_id, _, _)| *waiter_id == id)
        {
            if !waker.will_wake(cx.waker()) {
                *waker = cx.waker().clone()
            }
        } else {
            if waiters.len() >= MAX_VIRTUAL_TIMERS {
                drop(waiters);
                self.done = true;
                return Poll::Ready(Err(TimerError::CapacityExceeded));
            }
            waiters.push((id, deadline, cx.waker().clone()))
        }
        Poll::Pending
    }
}
impl Drop for VirtualSleep {
    fn drop(&mut self) {
        if !self.done
            && let Ok(mut waiters) = self.state.waiters.lock()
        {
            waiters.retain(|(id, _, _)| *id != self.id)
        }
    }
}
impl Timer for VirtualClock {
    type Sleep = VirtualSleep;
    fn sleep_until(&self, deadline: MonoTime) -> Self::Sleep {
        VirtualSleep {
            state: self.0.clone(),
            id: self.0.next_id.fetch_add(1, Ordering::Relaxed),
            deadline,
            done: false,
        }
    }
}

/// Bounded receive wall time, in whole seconds since the Unix epoch.
///
/// This is metadata on a durable history event, never its order. Canonical history
/// order is [`HistoryEventId`], so a skewed, missing, or repeated server timestamp
/// cannot reorder retained history. Values are bounded to a representable window so a
/// corrupt durable row fails validation instead of wrapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WallTime(pub i64);
pub const MAX_WALL_TIME_SECONDS: i64 = 32_503_680_000;
impl WallTime {
    /// Rejects a timestamp outside the representable window.
    ///
    /// The bound is checked with unsigned comparison so a corrupt durable row
    /// carrying `i64::MIN` fails validation instead of overflowing.
    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        let magnitude = seconds.unsigned_abs();
        (magnitude <= MAX_WALL_TIME_SECONDS.unsigned_abs()).then_some(Self(seconds))
    }
    pub fn unix_seconds(self) -> i64 {
        self.0
    }
}
/// Injectable civil-time source, deliberately separate from the monotonic [`Clock`].
///
/// Reconnect backoff and liveness need a monotonic source that cannot jump. Durable
/// history needs civil time that survives a restart. Mixing them would make one of
/// those two properties false.
pub trait WallClock: Send + Sync {
    fn now(&self) -> WallTime;
}
/// Production civil-time source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWallClock;
impl WallClock for SystemWallClock {
    fn now(&self) -> WallTime {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .and_then(WallTime::from_unix_seconds)
            .unwrap_or(WallTime(0))
    }
}
/// Deterministic civil-time source for tests. It starts at the Unix epoch and only
/// moves when a test advances it, so durable receive timestamps are reproducible.
#[derive(Clone, Debug)]
pub struct VirtualWallClock(Arc<Mutex<VirtualWallState>>);
#[derive(Debug)]
struct VirtualWallState {
    now: i64,
}
impl Default for VirtualWallClock {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(VirtualWallState { now: 0 })))
    }
}
impl VirtualWallClock {
    /// Moves civil time forward. It refuses to leave the representable window.
    pub fn advance(&self, seconds: i64) -> Option<WallTime> {
        let mut state = self.0.lock().expect("virtual wall clock lock poisoned");
        let now = state.now.checked_add(seconds)?;
        WallTime::from_unix_seconds(now).inspect(|value| state.now = value.0)
    }
}
impl WallClock for VirtualWallClock {
    fn now(&self) -> WallTime {
        WallTime(self.0.lock().expect("virtual wall clock lock poisoned").now)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Casemapping {
    Ascii,
    StrictRfc1459,
    Rfc1459,
}
impl Casemapping {
    pub fn fold(self, input: &[u8]) -> Vec<u8> {
        input
            .iter()
            .map(|b| {
                let b = b.to_ascii_lowercase();
                match (self, b) {
                    (Self::Rfc1459, b'[') => b'{',
                    (Self::Rfc1459, b']') => b'}',
                    (Self::Rfc1459, b'\\') => b'|',
                    (Self::Rfc1459, b'^') => b'~',
                    (Self::StrictRfc1459, b'[') => b'{',
                    (Self::StrictRfc1459, b']') => b'}',
                    (Self::StrictRfc1459, b'\\') => b'|',
                    _ => b,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_rejects_generic_network_forms() {
        for s in [
            "irc.example.com",
            "1.2.3.4:6667",
            "https://x.i2p",
            "x.i2p:80",
            " foo.i2p",
        ] {
            assert!(I2pEndpoint::parse(s).is_err(), "{s}")
        }
        assert!(I2pEndpoint::parse("irc.example.i2p").is_ok());
    }
    #[test]
    fn endpoint_recognizes_i2p_forms_without_resolving() {
        let standard = format!("{}.b32.i2p", "a".repeat(52));
        let extended = format!("{}.b32.i2p", "b".repeat(56));
        let destination = "A".repeat(516);
        assert_eq!(
            I2pEndpoint::parse(&standard).unwrap().kind(),
            I2pEndpointKind::StandardBase32
        );
        assert_eq!(
            I2pEndpoint::parse(&extended).unwrap().kind(),
            I2pEndpointKind::ExtendedBase32
        );
        assert_eq!(
            I2pEndpoint::parse(&destination).unwrap().kind(),
            I2pEndpointKind::Destination
        );
        for invalid in [
            format!("{}.b32.i2p", "0".repeat(52)),
            format!("{}.b32.i2p", "c".repeat(54)),
            "-bad.i2p".to_owned(),
            "a..i2p".to_owned(),
        ] {
            assert!(I2pEndpoint::parse(&invalid).is_err(), "{invalid}");
        }
        assert!(I2pEndpoint::parse(&"A".repeat(MAX_I2P_ENDPOINT_BYTES + 1)).is_err());
        assert!(I2pEndpoint::parse(&format!("{}.i2p", "a".repeat(63))).is_ok());
        assert!(I2pEndpoint::parse(&format!("{}.i2p", "a".repeat(64))).is_err());
    }
    #[tokio::test]
    async fn virtual_timer_advances_and_drop_cancels() {
        use std::task::Waker;
        let clock = VirtualClock::default();
        let first = clock.sleep_for(Duration::from_nanos(10));
        tokio::pin!(first);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(first.as_mut().poll(&mut cx).is_pending());
        clock.advance(Duration::from_nanos(10));
        assert!(first.as_mut().poll(&mut cx).is_ready());
        let old = clock.sleep_for(Duration::from_nanos(5));
        drop(old);
        assert!(clock.0.waiters.lock().unwrap().is_empty());
        let fresh = clock.sleep_for(Duration::from_nanos(10));
        tokio::pin!(fresh);
        assert!(fresh.as_mut().poll(&mut cx).is_pending());
        clock.advance(Duration::from_nanos(5));
        assert!(fresh.as_mut().poll(&mut cx).is_pending());
        clock.advance(Duration::from_nanos(5));
        assert!(fresh.as_mut().poll(&mut cx).is_ready());
    }
    #[test]
    fn equal_deadline_wakers_are_ordered_by_timer_id() {
        use std::task::{Wake, Waker};
        struct Recorder {
            label: u8,
            log: Arc<Mutex<Vec<u8>>>,
        }
        impl Wake for Recorder {
            fn wake(self: Arc<Self>) {
                self.log.lock().unwrap().push(self.label)
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.log.lock().unwrap().push(self.label)
            }
        }
        let clock = VirtualClock::default();
        let deadline = MonoTime(100);
        let mut a = Box::pin(clock.sleep_until(deadline));
        let mut b = Box::pin(clock.sleep_until(deadline));
        let log = Arc::new(Mutex::new(Vec::new()));
        let wa = Waker::from(Arc::new(Recorder {
            label: b'a',
            log: log.clone(),
        }));
        let wb = Waker::from(Arc::new(Recorder {
            label: b'b',
            log: log.clone(),
        }));
        assert!(a.as_mut().poll(&mut Context::from_waker(&wa)).is_pending());
        assert!(b.as_mut().poll(&mut Context::from_waker(&wb)).is_pending());
        clock.advance(Duration::from_nanos(100));
        assert_eq!(*log.lock().unwrap(), b"ab");
    }
    #[test]
    fn virtual_timer_count_is_bounded_and_deadlines_saturate() {
        let clock = VirtualClock::default();
        let mut cx = Context::from_waker(Waker::noop());
        let mut sleeps: Vec<_> = (0..MAX_VIRTUAL_TIMERS)
            .map(|_| Box::pin(clock.sleep_until(MonoTime(u64::MAX))))
            .collect();
        for sleep in &mut sleeps {
            assert!(sleep.as_mut().poll(&mut cx).is_pending());
        }
        let mut overflow = Box::pin(clock.sleep_until(MonoTime(u64::MAX)));
        assert_eq!(
            overflow.as_mut().poll(&mut cx),
            Poll::Ready(Err(TimerError::CapacityExceeded))
        );
        drop(sleeps);
        assert_eq!(clock.0.waiters.lock().unwrap().len(), 0);
        assert_eq!(clock.sleep_for(Duration::MAX).deadline, MonoTime(u64::MAX));
        clock.advance(Duration::MAX);
        assert_eq!(clock.now(), MonoTime(u64::MAX));
    }
    #[test]
    fn casemap() {
        assert_eq!(Casemapping::Rfc1459.fold(b"Nick[\\^"), b"nick{|~");
    }

    #[test]
    fn session_identity_is_ephemeral_bounded_and_never_reissued() {
        let allocator = SessionIdAllocator::new();
        assert_eq!(allocator.allocate(), Some(SessionId(1)));
        assert_eq!(allocator.allocate(), Some(SessionId(2)));
        // A durable client lineage is distinct from an attachment: the same client
        // reconnecting receives a new session identity, never a recycled one.
        let first: SessionId = allocator.allocate().unwrap();
        let second: SessionId = allocator.allocate().unwrap();
        assert_ne!(first, second);
        assert_ne!(first.0, 0);
    }

    #[test]
    fn session_identity_exhaustion_is_reported_not_wrapped() {
        let exhaustion = Arc::new(AtomicU64::new(MAX_SESSION_IDS - 1));
        let at_edge = SessionIdAllocator(exhaustion.clone());
        assert_eq!(at_edge.allocate(), Some(SessionId(MAX_SESSION_IDS - 1)));
        assert_eq!(at_edge.allocate(), None);
        // A refused allocation must not burn the final identity, and repeated
        // refusals must stay refused instead of wrapping to an earlier value.
        assert_eq!(at_edge.allocate(), None);
        assert_eq!(exhaustion.load(Ordering::SeqCst), MAX_SESSION_IDS);
    }

    #[test]
    fn wall_time_is_bounded_and_separate_from_monotonic_time() {
        assert_eq!(WallTime::from_unix_seconds(0), Some(WallTime(0)));
        assert_eq!(WallTime::from_unix_seconds(-1), Some(WallTime(-1)));
        assert_eq!(WallTime::from_unix_seconds(MAX_WALL_TIME_SECONDS + 1), None);
        assert_eq!(
            WallTime::from_unix_seconds(i64::MIN),
            None,
            "a corrupt durable timestamp must fail validation, not wrap"
        );
        // Monotonic time has no relationship to civil time; the bouncer must not
        // derive one from the other.
        let clock = VirtualClock::default();
        assert_eq!(clock.now(), MonoTime(0));
        assert!(SystemWallClock.now().unix_seconds() > 0);
    }

    #[test]
    fn virtual_wall_clock_is_deterministic_and_bounded() {
        let wall = VirtualWallClock::default();
        assert_eq!(wall.now(), WallTime(0));
        assert_eq!(wall.advance(1700000000), Some(WallTime(1700000000)));
        assert_eq!(wall.now(), WallTime(1700000000));
        assert_eq!(wall.advance(0), Some(WallTime(1700000000)));
        // Refusing to leave the window leaves the retained value unchanged.
        assert_eq!(wall.advance(MAX_WALL_TIME_SECONDS), None);
        assert_eq!(wall.now(), WallTime(1700000000));
    }
}
