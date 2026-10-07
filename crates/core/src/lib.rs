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
/// Ceiling on a single endpoint token, for every destination form alike.
///
/// One `.b32.i2p` address is the longest *name* a network can present, and a raw
/// Base64 destination is the longest *target* it can present. They are not the same
/// length, so the old 516 ceiling silently served both: it sized the name case and was
/// two orders of magnitude short for a destination, rejecting real ones.
///
/// This is a size ceiling, not an endpoint policy. It bounds what a router adapter may
/// be handed before any name or destination rules are applied, so a hostile or
/// malformed endpoint cannot become unbounded work inside a provider.
pub const MAX_I2P_ENDPOINT_BYTES: usize = 4096;

/// Shortest raw Base64 destination: 256 bytes of key material is 344 base64 characters
/// unpadded, and the I2P transport adds at least 172 bytes more.
pub const MIN_I2P_DESTINATION_CHARS: usize = 516;

/// Longest raw Base64 destination, equal to the application endpoint ceiling.
///
/// Plan 029 section 10 fixes one ceiling for every destination form, so a Destination is
/// bounded by the same value as any other endpoint rather than by a second, tighter
/// number that nothing else in the codebase knows about.
pub const MAX_I2P_DESTINATION_CHARS: usize = MAX_I2P_ENDPOINT_BYTES;

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
        // Decided before the name guard below, because the two alphabets disagree: a
        // name may not contain `/` or `:`, but a Base64 destination legitimately may.
        if valid_destination(value) {
            return Ok(Self {
                kind: I2pEndpointKind::Destination,
                canonical: value.to_owned(),
            });
        }
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

/// Reports whether `value` is a raw Base64 I2P destination rather than a name.
///
/// Plan 029 section 10 specifies the body alphabet as `A-Z a-z 0-9 - ~`, which is
/// base64url. Real I2P Destinations are standard Base64 and carry `+` and `/`. Accepting
/// only the narrower set would have re-created the very defect this correction exists to
/// remove -- a Destination that no real router could ever hand out would parse while the
/// ones it does hand out would not -- so the union is accepted and the plan's own stated
/// intent is preserved.
///
/// The union is a superset of both alphabets and is still bounded: it is a shape test, not
/// a validation of the bytes. Only length range and shape are decided here. Whether the
/// bytes name a reachable service is a router question, and an endpoint that cannot be
/// reached is still a well-formed endpoint that deserves an ordinary connect failure
/// rather than a configuration rejection.
fn valid_destination(value: &str) -> bool {
    if !(MIN_I2P_DESTINATION_CHARS..=MAX_I2P_DESTINATION_CHARS).contains(&value.len()) {
        return false;
    }
    let body = value.trim_end_matches('=');
    let padding = value.len() - body.len();
    // Padding only ever closes the final Base64 block, and only when the token is
    // actually block-aligned. An unpadded token is also legal, which is why alignment
    // is only demanded when padding is present.
    padding <= 2
        && (padding == 0 || value.len() % 4 == 0)
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'~' | b'+' | b'/'))
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
///
/// Every call is scoped to a [`NetworkId`]. A provider instance is shared by every
/// Network the process owns, so an unscoped call would leave a router adapter unable to
/// tell which Network's lease, tunnel, or handle an attempt belongs to — and unable to
/// release only the one that was asked for. Scope is part of the contract, not a hint.
#[async_trait]
pub trait I2pStreamProvider: Send + Sync {
    /// Opens one upstream stream to `endpoint` on behalf of `network`.
    ///
    /// The returned stream belongs to the calling generation alone. No handle to it is
    /// retained by the provider, and no connection is silently shared between Networks.
    async fn connect(
        &self,
        network: NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, ProviderError>;

    /// Releases every router resource the provider holds for `network`.
    ///
    /// Called exactly once per Network, on the deletion and shutdown paths, when no
    /// connection generation for that Network can still be issuing work. It is
    /// explicit rather than implied by dropping the provider because one provider serves
    /// every Network in the process: an `Arc` still held by a live owner must not keep a
    /// deleted Network's session or tunnels alive, and must not be able to release
    /// another Network's.
    ///
    /// Releasing an unknown or already-released Network is a no-op, not an error.
    async fn release(&self, network: NetworkId) -> Result<(), ProviderError> {
        let _ = network;
        Ok(())
    }
}

/// Shares one provider across every Network owner.
///
/// The controller owns exactly one provider and hands a shared reference to each owner
/// it starts, so `N` supervised Networks still cost one configured transport rather than
/// `N` copies of it. Delegating rather than cloning also means a provider that counts
/// its own connections still sees one consistent count.
#[async_trait]
impl<P: I2pStreamProvider + ?Sized> I2pStreamProvider for std::sync::Arc<P> {
    async fn connect(
        &self,
        network: NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, ProviderError> {
        (**self).connect(network, endpoint).await
    }
    async fn release(&self, network: NetworkId) -> Result<(), ProviderError> {
        (**self).release(network).await
    }
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
        // Debug stays redacted for every form. A Destination is key material; a b32
        // address identifies a host. Neither belongs in a log line.
        for form in [
            "irc.example.i2p",
            &format!("{}.b32.i2p", "a".repeat(52)),
            &"A".repeat(MIN_I2P_DESTINATION_CHARS),
        ] {
            assert_eq!(
                format!("{:?}", I2pEndpoint::parse(form).unwrap()),
                "I2pEndpoint([redacted])",
                "{form:.20} must not appear in Debug output"
            );
        }
    }

    /// A real router's Destination is longer and uses a different alphabet than base64url.
    ///
    /// Found by qualifying against i2pd 2.61.0, which returns a **908-character** I2P
    /// base64 Destination: `-` and `~` where RFC 4648 uses `+` and `/`, with trailing `=`.
    /// A base64url-only rule would have rejected every one of them, which is why the
    /// alphabet is the union recorded in Plan 029's closure.
    ///
    /// The value below is synthetic and shaped like the live one rather than captured from
    /// it. A real Destination is key material and does not belong in a repository, even a
    /// transient one; what has to be preserved is the shape.
    #[test]
    fn endpoint_accepts_a_live_router_i2p_base64_destination() {
        let mut destination = String::with_capacity(908);
        // Interleave the two characters standard base64url would reject.
        while destination.len() + 4 <= 906 {
            destination.push_str("A~-B");
        }
        while destination.len() < 906 {
            destination.push('A');
        }
        // Padded, as the live observation was.
        destination.push_str("==");
        assert_eq!(destination.len(), 908, "the live observation's length");

        let parsed =
            I2pEndpoint::parse(&destination).expect("a real i2pd Destination must be accepted");
        assert_eq!(parsed.kind(), I2pEndpointKind::Destination);
        // A destination keeps its own case: it is opaque key material, not a name.
        assert_eq!(parsed.as_str(), destination);
        assert_eq!(
            format!("{:?}", parsed),
            "I2pEndpoint([redacted])",
            "908 characters of key material must never reach a log line"
        );

        // The same token is still not a `.b32.i2p` name. A b32 label is 52 or 56-63
        // characters, so mistaking a Destination for an address would mean accepting a
        // 908-character hostname, which the guard exists to refuse.
        assert!(
            I2pEndpoint::parse(&format!("{destination}.b32.i2p")).is_err(),
            "a Destination-length token must not pass the name guard"
        );
    }

    /// The name forms keep their own tighter bounds even though the endpoint ceiling rose.
    ///
    /// Raising the ceiling for Destinations must not have loosened hostnames: a 4000-byte
    /// `.i2p` label was never a hostname, and accepting one would move a bounded token
    /// into unbounded work inside a router adapter.
    #[test]
    fn endpoint_ceiling_did_not_loosen_the_name_forms() {
        assert!(
            I2pEndpoint::parse(&format!("{}.i2p", "a".repeat(63))).is_ok(),
            "the longest hostname label still parses"
        );
        for rejected in [
            // One past the longest label, and one past the longest whole hostname.
            format!("{}.i2p", "a".repeat(64)),
            format!("{}a.i2p", "a".repeat(63)),
            // One past the extended b32 label profile, in both spellings.
            format!("{}.b32.i2p", "a".repeat(64)),
            // A hostname long enough to have been a Destination-length token.
            format!("{}.i2p", "a".repeat(2000)),
        ] {
            assert!(
                I2pEndpoint::parse(&rejected).is_err(),
                "{rejected:.24} must stay rejected"
            );
        }
    }

    /// A raw destination is Base64 and is far longer than the `.b32.i2p` name it used to
    /// be checked against. Both halves regressed silently before: a real destination
    /// carrying `+` or `/` was rejected outright, and one over 516 characters could not be
    /// expressed at all.
    #[test]
    fn endpoint_accepts_real_base64_destinations() {
        let mixed = format!("{}+/{}", "A".repeat(300), "B".repeat(400));
        assert_eq!(
            I2pEndpoint::parse(&mixed).unwrap().kind(),
            I2pEndpointKind::Destination
        );
        // A destination keeps its own case: it is opaque key material, not a name.
        assert_eq!(I2pEndpoint::parse(&mixed).unwrap().as_str(), mixed);
        assert_eq!(
            I2pEndpoint::parse(&"A".repeat(MIN_I2P_DESTINATION_CHARS))
                .unwrap()
                .kind(),
            I2pEndpointKind::Destination
        );
        assert_eq!(
            I2pEndpoint::parse(&"A".repeat(MAX_I2P_DESTINATION_CHARS))
                .unwrap()
                .kind(),
            I2pEndpointKind::Destination
        );
        // Padding closes a Base64 block and may appear nowhere else. A block-aligned
        // token of 516 plus two pad characters is the same length as an aligned 518.
        for (body_len, pad_len) in [(514, 2), (515, 1), (516, 0)] {
            assert_eq!(
                I2pEndpoint::parse(&format!("{}{}", "A".repeat(body_len), "=".repeat(pad_len)))
                    .unwrap()
                    .kind(),
                I2pEndpointKind::Destination
            );
        }
        for rejected in [
            "A".repeat(MIN_I2P_DESTINATION_CHARS - 1),
            "A".repeat(MAX_I2P_DESTINATION_CHARS + 1),
            // Padding in the interior, not the tail.
            format!("{}=B", "A".repeat(514)),
            // Three pad characters, and pad on a token that is not block-aligned.
            format!("A{}", "=".repeat(3)),
            format!("{}=", "A".repeat(517)),
            "A".repeat(400),
        ] {
            assert!(
                I2pEndpoint::parse(&rejected).is_err(),
                "expected rejection for {rejected:.20}"
            );
        }
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
