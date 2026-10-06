//! Process-wide reconnect admission.
//!
//! # Why a second gate on top of per-Network backoff
//!
//! Each Network already backs off exponentially, and that backoff is deliberately
//! independent. But independence is exactly the problem at scale: after a router
//! restart, thirty-two stored Networks each independently decide "it is time to retry",
//! and every one of them calls `connect()` in the same instant. Backoff decorrelates
//! *steady-state* retries; it does not bound the herd that follows a shared event.
//!
//! So a single scheduler gates every `connect()` in the process. It owns three things:
//! how many attempts may be in flight at once, how fast a new attempt may start, and
//! who goes next when both are exhausted.
//!
//! # What this deliberately is not
//!
//! It is not a replacement for per-Network backoff. Backoff decides *when a Network
//! wants* to try; this decides *when it is allowed to*. Keeping both means a Network
//! that has backed off for an hour still waits its turn rather than jumping the queue.
//!
//! It also has no view of IRC state, credentials, endpoints, or message content. It
//! knows a `NetworkId` and nothing else, so it cannot leak anything and cannot
//! influence anything beyond timing.
//!
//! # Fairness
//!
//! Waiters are served first-in-first-out. A Network that fails and re-queues goes to
//! the *back*, so a permanently broken Network cannot monopolise admission and starve
//! a healthy one. There is at most one waiter entry per Network: a duplicate acquire
//! coalesces rather than adding a second claim on the queue.
//!
//! # Nothing here is persisted
//!
//! Scheduler state is process-local by construction. Persisting it would be wrong: a
//! restart must not inherit a queue describing Networks whose connections no longer
//! exist.
use i2pr_irc_core::NetworkId;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    // The rate limiter must run on the same clock as the backoff sleep it gates.
    // `owner.rs` waits out its per-Network backoff with `tokio::time::sleep`, so if the
    // token bucket ran on a different clock the two would disagree under virtual time:
    // the sleep would advance while the bucket believed time had not moved, and a
    // qualification of "the scheduler sleeps between admissions" would be measuring the
    // wrong thing.
    time::Instant,
};

/// Ceiling on concurrent connect attempts across the whole process.
///
/// Conservative on purpose. An anonymity-network tunnel is expensive to establish, and
/// opening many at once is exactly the behaviour that looks like a client to the
/// network. Four allows a small working set without letting a cold start of dozens of
/// Networks stampede.
pub const MAX_IN_FLIGHT_CONNECTS: usize = 4;

/// Maximum burst of attempt starts before the rate limiter engages.
///
/// This is the same idea as the in-flight ceiling but on the *rate* axis: without it,
/// four in flight finishing quickly could still start attempts as fast as the provider
/// can fail them.
pub const MAX_CONNECT_BURST: u32 = 4;

/// How long one start token takes to regenerate.
///
/// At the default burst this permits four immediate starts and then one start every
/// `CONNECT_TOKEN_INTERVAL`, which is slow enough that a full outage cannot turn into a
/// request flood.
pub const CONNECT_TOKEN_INTERVAL: Duration = Duration::from_secs(2);

/// Ceiling on pending waiters.
///
/// Bounded by the supervised-Network ceiling, because there is at most one waiter per
/// Network. Exceeding it refuses rather than growing an unbounded queue.
pub const MAX_RECONNECT_WAITERS: usize = 64;

/// The frozen production policy.
///
/// Held as an explicit value so a test can inject a different one under virtual time
/// instead of depending on wall-clock production values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectBudget {
    /// Maximum connect attempts in flight at once.
    pub max_in_flight: usize,
    /// Maximum attempt starts before the rate limiter engages.
    pub max_burst: u32,
    /// How long one start token takes to regenerate.
    pub token_interval: Duration,
    /// Maximum pending waiters.
    pub max_waiters: usize,
    /// Entropy mixed into per-Network jitter.
    ///
    /// Process-local and injected, never derived from anything secret. Its only job is
    /// to decorrelate two Networks whose generations happen to match.
    pub seed: u64,
}

impl Default for ReconnectBudget {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_CONNECTS,
            max_burst: MAX_CONNECT_BURST,
            token_interval: CONNECT_TOKEN_INTERVAL,
            max_waiters: MAX_RECONNECT_WAITERS,
            seed: 0x9e3779b97f4a7c15,
        }
    }
}

impl ReconnectBudget {
    /// Rejects a policy that would silently mean "unlimited".
    ///
    /// A zero ceiling would otherwise read as "no limit" to a reader and behave as
    /// "nothing may ever connect" to the code, which is the wrong failure in both
    /// directions.
    pub fn validate(self) -> Result<(), ReconnectBudgetError> {
        if self.max_in_flight == 0 {
            return Err(ReconnectBudgetError::UnlimitedInFlight);
        }
        if self.max_burst == 0 {
            return Err(ReconnectBudgetError::UnlimitedBurst);
        }
        if self.token_interval.is_zero() {
            return Err(ReconnectBudgetError::UnlimitedRate);
        }
        if self.max_waiters == 0 {
            return Err(ReconnectBudgetError::UnlimitedWaiters);
        }
        Ok(())
    }
}

/// A production policy that would remove a bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectBudgetError {
    UnlimitedInFlight,
    UnlimitedBurst,
    UnlimitedRate,
    UnlimitedWaiters,
}

/// Why a Network stopped reconnecting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectDisposition {
    /// The failure may clear on its own, so keep backing off.
    Retryable,
    /// Retrying unchanged configuration cannot help.
    ///
    /// A Network in this state consumes no global attempts at all, so a permanently
    /// misconfigured Network cannot sit in the reconnect budget competing with healthy
    /// ones. Only a configuration reconciliation re-arms it.
    Terminal,
}

/// Classifies one failure.
///
/// The distinction is about whether *unchanged configuration* could fix the problem, not
/// about how bad the failure looks. A registration rejection means the credentials or
/// config were refused; retrying the same refusal forever would spend the process-wide
/// budget on something that cannot succeed.
pub fn classify(error: &RuntimeErrorLike) -> ReconnectDisposition {
    match error {
        // Refused credentials or configuration: retrying the identical request cannot
        // succeed, and each attempt would cost the whole process a permit.
        RuntimeErrorLike::Registration => ReconnectDisposition::Terminal,
        // Anything else is assumed transient: EOF, reset, timeout, transport loss, and
        // a provider reporting itself unavailable all clear on their own.
        _ => ReconnectDisposition::Retryable,
    }
}

/// The failure shapes the classifier distinguishes, decoupled from the runtime's own
/// error type so the policy stays testable on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeErrorLike {
    /// The server refused registration, credentials, or configuration.
    Registration,
    /// Any other runtime failure.
    Other,
}

/// Bounded, non-secret scheduler diagnostics.
///
/// Every field is a count or an enum. There is deliberately no field that could hold an
/// endpoint, a nick, a credential, or any message content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchedulerDiagnostics {
    /// Waiters currently queued.
    pub pending_waiters: usize,
    /// Connect attempts currently in flight.
    pub in_flight: usize,
    /// Total attempts admitted since process start.
    pub admitted: u64,
    /// Total attempts that had to wait for admission.
    pub delayed: u64,
    /// Highest concurrent in-flight count observed.
    pub peak_in_flight: usize,
    /// Attempts suppressed because the Network is terminally failed.
    pub terminal_suppressed: u64,
}

struct SchedulerState {
    /// FIFO of Networks waiting for admission.
    queue: VecDeque<NetworkId>,
    in_flight: usize,
    /// Token-bucket level for attempt starts.
    tokens: f64,
    last_refill: Instant,
    admitted: u64,
    delayed: u64,
    peak_in_flight: usize,
    terminal_suppressed: u64,
    /// Networks whose last failure was terminal. They consume no attempts.
    terminal: BTreeSet<NetworkId>,
    /// Networks that are already queued, so a duplicate acquire coalesces.
    queued: BTreeMap<NetworkId, ()>,
}

/// The process-wide reconnect scheduler.
///
/// Cloneable and shared by handle, because every Network owner needs it and none of them
/// may own it: ownership is what would let one Network's retry timing affect another's.
#[derive(Clone)]
pub struct ReconnectScheduler {
    inner: Arc<Inner>,
}

struct Inner {
    budget: ReconnectBudget,
    state: Mutex<SchedulerState>,
    /// Woken whenever in-flight capacity or a start token becomes available.
    ready: Notify,
}

impl Default for ReconnectScheduler {
    fn default() -> Self {
        // The production policy is known valid, so an infallible default is honest here
        // rather than a panicking `expect`.
        Self::new(ReconnectBudget::default()).expect("the production reconnect budget is bounded")
    }
}

impl ReconnectScheduler {
    /// Builds a scheduler over an explicit policy, rejecting an unlimited one.
    pub fn new(budget: ReconnectBudget) -> Result<Self, ReconnectBudgetError> {
        budget.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                budget,
                state: Mutex::new(SchedulerState {
                    queue: VecDeque::new(),
                    in_flight: 0,
                    tokens: budget.max_burst as f64,
                    last_refill: Instant::now(),
                    admitted: 0,
                    delayed: 0,
                    peak_in_flight: 0,
                    terminal_suppressed: 0,
                    terminal: BTreeSet::new(),
                    queued: BTreeMap::new(),
                }),
                ready: Notify::new(),
            }),
        })
    }

    /// The process-local entropy seed used for jitter.
    ///
    /// Not a secret: its only purpose is herd dispersion, so it is exposed rather than
    /// hidden behind an accessor that suggests otherwise.
    pub fn entropy_seed(&self) -> u64 {
        self.inner.budget.seed
    }

    pub fn diagnostics(&self) -> SchedulerDiagnostics {
        let state = lock(&self.inner.state);
        SchedulerDiagnostics {
            pending_waiters: state.queue.len(),
            in_flight: state.in_flight,
            admitted: state.admitted,
            delayed: state.delayed,
            peak_in_flight: state.peak_in_flight,
            terminal_suppressed: state.terminal_suppressed,
        }
    }

    /// Records that a Network's last failure was terminal.
    ///
    /// It stops consuming attempts immediately. Until [`Self::rearm`] it acquires
    /// without waiting and is refused, which is what keeps a permanently broken Network
    /// from competing for the process-wide budget.
    pub fn mark_terminal(&self, network: NetworkId) {
        let mut state = lock(&self.inner.state);
        state.terminal.insert(network);
        state.terminal_suppressed = state.terminal_suppressed.saturating_add(1);
        drop(state);
        self.inner.ready.notify_waiters();
    }

    /// Re-arms a terminally failed Network after a configuration reconciliation.
    pub fn rearm(&self, network: NetworkId) {
        lock(&self.inner.state).terminal.remove(&network);
        self.inner.ready.notify_waiters();
    }

    /// Whether a Network is currently terminally failed.
    pub fn is_terminal(&self, network: NetworkId) -> bool {
        lock(&self.inner.state).terminal.contains(&network)
    }

    /// Waits for admission and returns a permit that releases in-flight capacity on drop.
    ///
    /// Fairness is first-in-first-out. A Network already queued is not queued twice, so
    /// a duplicate acquire coalesces instead of gaining a second claim.
    pub async fn acquire(&self, network: NetworkId) -> Result<ConnectPermit, AcquireRefused> {
        let mut guard = WaiterGuard {
            scheduler: self.clone(),
            network: Some(network),
            armed: false,
        };
        loop {
            // Register before waiting, so a permit that frees up between this check and
            // the await cannot be missed.
            let notified = self.inner.ready.notified();
            {
                let mut state = lock(&self.inner.state);
                if state.terminal.contains(&network) {
                    state.terminal_suppressed = state.terminal_suppressed.saturating_add(1);
                    return Err(AcquireRefused::Terminal);
                }
                if !state.queued.contains_key(&network) {
                    if state.queue.len() >= self.inner.budget.max_waiters {
                        return Err(AcquireRefused::WaitersFull);
                    }
                    state.queue.push_back(network);
                    state.queued.insert(network, ());
                    state.delayed = state.delayed.saturating_add(1);
                    guard.armed = true;
                }
                refill(&mut state, self.inner.budget);
                // Strict FIFO: only the head of the queue may take the next permit, so a
                // late arrival cannot overtake one that has already waited.
                let at_front = state.queue.front() == Some(&network);
                if at_front && self.may_start(&state, self.inner.budget) {
                    state.queue.pop_front();
                    state.queued.remove(&network);
                    guard.armed = false;
                    // Both gates are consumed together: in-flight capacity and one start
                    // token. Without spending the token the rate limit would be a no-op,
                    // because a full bucket would admit every waiter as fast as they arrive.
                    state.tokens -= 1.0;
                    state.in_flight += 1;
                    state.admitted = state.admitted.saturating_add(1);
                    state.peak_in_flight = state.peak_in_flight.max(state.in_flight);
                    return Ok(ConnectPermit {
                        scheduler: self.clone(),
                    });
                }
            }
            notified.await;
        }
    }

    /// Whether both gates are open right now.
    fn may_start(&self, state: &SchedulerState, budget: ReconnectBudget) -> bool {
        state.in_flight < budget.max_in_flight && state.tokens >= 1.0
    }

    fn release(&self) {
        let mut state = lock(&self.inner.state);
        state.in_flight = state.in_flight.saturating_sub(1);
        drop(state);
        self.inner.ready.notify_waiters();
    }
}

/// Admission was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquireRefused {
    /// The Network is terminally failed and must be reconciled before retrying.
    Terminal,
    /// The bounded waiter set is full.
    WaitersFull,
}

/// Releases the Network's queue entry if it is cancelled while waiting.
///
/// This is what makes cancellation correct without a separate cancel call: dropping the
/// `acquire` future drops this guard, which removes the waiter. Without it, a stopped
/// Network would leave a permanent claim on the queue.
struct WaiterGuard {
    scheduler: ReconnectScheduler,
    network: Option<NetworkId>,
    armed: bool,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(network) = self.network else {
            return;
        };
        let mut state = lock(&self.scheduler.inner.state);
        state.queued.remove(&network);
        state.queue.retain(|queued| *queued != network);
    }
}

/// Releases in-flight capacity when dropped.
///
/// Drop is the whole mechanism: whether the attempt succeeded, failed, or was cancelled
/// mid-flight, the permit goes back. That is why there is no explicit `complete()` to
/// forget.
pub struct ConnectPermit {
    scheduler: ReconnectScheduler,
}

impl Drop for ConnectPermit {
    fn drop(&mut self) {
        self.scheduler.release();
    }
}

/// Regenerates start tokens for the elapsed interval.
fn refill(state: &mut SchedulerState, budget: ReconnectBudget) {
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(state.last_refill);
    if elapsed.is_zero() {
        return;
    }
    state.last_refill = now;
    let per_second = 1_000_000.0 / budget.token_interval.as_micros().max(1) as f64;
    state.tokens = (state.tokens + elapsed.as_secs_f64() * per_second).min(budget.max_burst as f64);
}

/// Deterministic herd-decorrelation jitter for one attempt.
///
/// The mix includes the NetworkId and the generation, so two Networks that happen to be
/// on the same attempt count still diverge. `seed` is process-local and injected, which
/// makes the sequence reproducible under test without making it secret.
pub fn jitter_entropy(network: NetworkId, generation: u64, seed: u64) -> u64 {
    let mut mixed = seed
        ^ network.0.wrapping_mul(0x9e3779b97f4a7c15)
        ^ generation.wrapping_mul(0xbf58476d1ce4e5b9);
    // splitmix64 finaliser: decorrelates equal generations without needing any
    // cryptographic randomness. The goal is herd dispersion, not secrecy.
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d049bb133111eb);
    mixed ^ (mixed >> 31)
}

/// A short lock helper, so a poisoned mutex surfaces as a panic with context rather than
/// being silently swallowed.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("reconnect scheduler mutex")
}
