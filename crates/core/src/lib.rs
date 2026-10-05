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
}
