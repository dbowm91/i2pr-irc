//! The process-wide SAM scope map: one long-lived session per durable Network.
//!
//! # Why a task per Network rather than a shared pool
//!
//! A SAM `STREAM CONNECT` names a session ID, and a session belongs to one router-side
//! I2P identity. Two Networks sharing an identity would let an observer at the bridge
//! correlate their IRC conversations, which is the whole reason ADR-0004 scopes the
//! provider by `NetworkId`. One task per scope therefore owns one session, one control
//! socket, and one epoch, and no two scopes can be confused for each other.
//!
//! # Why the map is a plain `Mutex` and every guard is short
//!
//! The map holds senders, stop signals, and join handles — never an `await`. A lock held
//! across an await is how one Network's slow connect ends up stalling another's, and this
//! map sits on the path of every connect in the process. The rule is enforced by the type
//! shape and by the fact that the map itself is never `Send`-able across a guard.
//!
//! # What "healthy" means
//!
//! A scope is healthy when its control socket is open and its session has not been
//! invalidated by an EOF, an `INVALID_ID`, or a release. A peer-level failure —
//! `CANT_REACH_PEER`, a stream timeout — does **not** make a scope unhealthy: the session
//! is fine and the peer is not, and tearing down an identity because one IRC server was
//! down would churn the identity on every outage.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use i2pr_irc_core::{ByteStream, I2pEndpoint, I2pStreamProvider, NetworkId, ProviderError};
use tokio::sync::{mpsc, oneshot};

use crate::{
    client::{SamClient, SamClientConfig, SamRawStream},
    endpoint::SamBridgeEndpoint,
    error::{SamError, StreamRejection},
    session_id::SamSessionId,
};

/// Ceiling on concurrent pending requests per scope.
///
/// One owner at a time is the normal case. Four leaves room for a control-plane race
/// without letting a burst build an unbounded backlog per Network.
pub const SAM_SCOPE_REQUEST_CAPACITY: usize = 4;

/// Default ceiling on live scopes.
///
/// Overridden in production by the runtime's own supervised-Network ceiling, passed in at
/// construction rather than imported: `i2pr-irc-runtime` depends on this crate, so an
/// import here would be a cycle. Taking it as a value means there is exactly one number in
/// the process, and it is the runtime's.
pub const DEFAULT_MAX_SAM_SCOPES: usize = 64;

/// Default release deadline, likewise supplied by the consumer.
pub const DEFAULT_RELEASE_TIMEOUT: Duration = Duration::from_secs(15);

/// One caller waiting for a stream.
type Pending = oneshot::Sender<Result<Box<dyn ByteStream>, ProviderError>>;

/// The per-Network state, behind one short-lived lock.
struct ScopeEntry {
    requests: mpsc::Sender<ScopeRequest>,
    stop: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
    /// Bounded, redacted health for diagnostics.
    health: Arc<ScopeHealth>,
}

/// A request handed to a scope task.
///
/// There is deliberately no release variant. `release` removes the map entry, which drops
/// the request sender, which closes the channel, which ends the task's receive loop — and
/// separately signals the stop flag so a task parked mid-exchange stops immediately rather
/// than after its current deadline. A release message would be a third path to the same
/// outcome, and three paths to one event is where they start to disagree.
enum ScopeRequest {
    /// Acquire a stream to `endpoint`.
    Connect {
        endpoint: I2pEndpoint,
        reply: Pending,
    },
}

/// The per-Network liveness, behind one short-lived lock.
///
/// Deliberately not a counter store. A release removes the map entry, and a counter kept
/// on the entry would therefore be erased by the very event an operator most wants to
/// read afterwards: "how much did this Network cost before I deleted it?" Cumulative
/// numbers live in [`Totals`], which lives as long as the provider does.
#[derive(Debug, Default)]
pub struct ScopeHealth {
    /// The current SAM epoch, incremented on every session creation.
    epoch: AtomicU64,
    healthy: std::sync::atomic::AtomicBool,
}

impl ScopeHealth {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }
    fn set_healthy(&self, value: bool) {
        self.healthy.store(value, Ordering::SeqCst);
    }
    /// The current epoch. Zero means no session has been established.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

/// The process-wide cumulative ledger.
///
/// Shared by every scope and owned by the provider, so a count survives the release of the
/// scope that produced it. Every field is a counter or a high-water mark: there is nowhere
/// in this struct for a session ID, a Destination, a nickname, a `NetworkId`, or a router
/// message, which is what makes it safe to project into diagnostics without a filter.
#[derive(Debug, Default)]
struct Totals {
    session_creations: AtomicU64,
    session_losses: AtomicU64,
    stream_attempts: AtomicU64,
    stream_successes: AtomicU64,
    stream_failures: AtomicU64,
    /// Attempts turned away because the scope's queue was full.
    ///
    /// Kept apart from `stream_failures` on purpose: a refusal never reached the router, so
    /// folding it in with genuine exchange failures would make a saturated queue look like
    /// a failing router and send an Operator looking in the wrong place.
    queue_refusals: AtomicU64,
    peak_queued: AtomicU64,
}

/// A bounded, redacted snapshot of every scope.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SamDiagnostics {
    /// Scopes currently in the map, healthy or not.
    pub live_scopes: usize,
    /// Scopes whose control socket and session are intact.
    pub healthy_scopes: usize,
    /// Total session creations across all scopes.
    pub session_creations: u64,
    /// Total session losses across all scopes.
    pub session_losses: u64,
    /// Total stream connect attempts.
    pub stream_attempts: u64,
    /// Total successful stream connects.
    pub stream_successes: u64,
    /// Total failed stream connects, from an exchange that actually reached the router.
    pub stream_failures: u64,
    /// Attempts refused because the scope's request queue was full.
    pub queue_refusals: u64,
    /// Total releases.
    pub releases: u64,
    /// Highest SAM epoch reached by any *live* scope; a per-lifetime figure, so it drops
    /// back when the scope that held it is released. Use `session_creations` for the
    /// cumulative question.
    pub max_epoch: u64,
    /// Highest request-queue depth observed in any scope, including released ones.
    pub peak_queued: u64,
}

/// A per-Network `I2pStreamProvider` backed by one long-lived SAM session.
///
/// One instance serves the whole process. Construction does not connect to anything: the
/// first `connect` for a Network creates its scope lazily, so a configured-but-stopped
/// Network costs no router resource.
pub struct SamProvider {
    config: SamClientConfig,
    scopes: Mutex<BTreeMap<NetworkId, ScopeEntry>>,
    max_scopes: usize,
    release_timeout: Duration,
    /// Process-wide cumulative ledger. Outlives the scopes it counts.
    totals: Arc<Totals>,
    releases: AtomicU64,
}

impl SamProvider {
    /// The production provider: default loopback bridge, production deadlines, OS
    /// randomness, and conservative standalone bounds.
    ///
    /// The runtime overrides both bounds through [`SamProvider::with_limits`] when it
    /// constructs the provider, so the ceilings it enforces are its own.
    pub fn new() -> Self {
        Self::with_config(SamClientConfig::production())
    }

    /// A provider over an explicit client configuration.
    pub fn with_config(config: SamClientConfig) -> Self {
        Self {
            config,
            scopes: Mutex::new(BTreeMap::new()),
            max_scopes: DEFAULT_MAX_SAM_SCOPES,
            release_timeout: DEFAULT_RELEASE_TIMEOUT,
            totals: Arc::new(Totals::default()),
            releases: AtomicU64::new(0),
        }
    }

    /// Takes the scope ceiling and release deadline from the consuming runtime.
    ///
    /// The reason this is a parameter rather than an import: the runtime depends on this
    /// crate, so a constant pulled from it here would be a dependency cycle. Passing the
    /// value means the number the runtime enforces is literally the runtime's.
    pub fn with_limits(mut self, max_scopes: usize, release_timeout: Duration) -> Self {
        self.max_scopes = max_scopes;
        self.release_timeout = release_timeout;
        self
    }

    /// Overrides the scope ceiling.
    ///
    /// Exists so a test can prove the bound with a small number rather than by configuring
    /// 64 Networks.
    pub fn with_max_scopes(mut self, max: usize) -> Self {
        self.max_scopes = max;
        self
    }

    /// Overrides the release deadline.
    pub fn with_release_timeout(mut self, timeout: Duration) -> Self {
        self.release_timeout = timeout;
        self
    }

    /// The bridge this provider connects to.
    pub fn bridge(&self) -> SamBridgeEndpoint {
        self.config.bridge
    }

    /// Bounded, redacted health across every scope.
    ///
    /// Cumulative counters come from the provider-wide ledger and survive a release; the
    /// liveness figures come from the scopes alive right now. Splitting them that way is
    /// what lets an operator read "one Network cost three sessions and was then deleted"
    /// instead of finding every counter reset to zero by the delete.
    pub fn diagnostics(&self) -> SamDiagnostics {
        let scopes = self.scopes.lock().expect("the scope map is readable");
        SamDiagnostics {
            live_scopes: scopes.len(),
            healthy_scopes: scopes
                .values()
                .filter(|entry| entry.health.healthy())
                .count(),
            max_epoch: scopes
                .values()
                .map(|entry| entry.health.epoch())
                .max()
                .unwrap_or(0),
            session_creations: self.totals.session_creations.load(Ordering::SeqCst),
            session_losses: self.totals.session_losses.load(Ordering::SeqCst),
            stream_attempts: self.totals.stream_attempts.load(Ordering::SeqCst),
            stream_successes: self.totals.stream_successes.load(Ordering::SeqCst),
            stream_failures: self.totals.stream_failures.load(Ordering::SeqCst),
            queue_refusals: self.totals.queue_refusals.load(Ordering::SeqCst),
            peak_queued: self.totals.peak_queued.load(Ordering::SeqCst),
            releases: self.releases.load(Ordering::SeqCst),
        }
    }

    /// Creates the scope for `network` if it does not exist, returning its sender.
    ///
    /// The map lock is held only for the lookup and the insert. The task is spawned after
    /// the guard is dropped, because spawning while holding a lock a task might
    /// immediately contend on is how a map deadlock starts.
    fn ensure_scope(
        &self,
        network: NetworkId,
    ) -> Result<mpsc::Sender<ScopeRequest>, ProviderError> {
        // Fast path: an existing scope.
        if let Some(existing) = self
            .scopes
            .lock()
            .expect("the scope map is readable")
            .get(&network)
        {
            return Ok(existing.requests.clone());
        }
        // Slow path: decide under the lock, spawn outside it.
        let sender = {
            let mut scopes = self.scopes.lock().expect("the scope map is writable");
            if let Some(existing) = scopes.get(&network) {
                return Ok(existing.requests.clone());
            }
            if scopes.len() >= self.max_scopes {
                return Err(ProviderError::Failed);
            }
            let (sender, receiver) = mpsc::channel(SAM_SCOPE_REQUEST_CAPACITY);
            let (stop, stop_rx) = tokio::sync::watch::channel(false);
            let health = Arc::new(ScopeHealth::default());
            let health = Arc::clone(&health);
            let join = tokio::spawn(scope_task(
                self.config.clone(),
                receiver,
                stop_rx,
                Arc::clone(&health),
                Arc::clone(&self.totals),
            ));
            scopes.insert(
                network,
                ScopeEntry {
                    requests: sender.clone(),
                    stop,
                    join,
                    health,
                },
            );
            sender
        };
        Ok(sender)
    }
}

impl Default for SamProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl I2pStreamProvider for SamProvider {
    async fn connect(
        &self,
        network: NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, ProviderError> {
        self.totals.stream_attempts.fetch_add(1, Ordering::SeqCst);
        let sender = self.ensure_scope(network)?;
        let (reply, response) = oneshot::channel();
        // `try_send`, never `send`: the runtime holds a process-wide reconnect permit for
        // exactly this attempt, so a queue that could park would park against a permit
        // that is meant to be released on failure. Refusal is the honest answer.
        if sender
            .try_send(ScopeRequest::Connect {
                endpoint: endpoint.clone(),
                reply,
            })
            .is_err()
        {
            self.totals.queue_refusals.fetch_add(1, Ordering::SeqCst);
            return Err(ProviderError::Unavailable);
        }
        match response.await {
            Ok(result) => result,
            // The scope task ended without answering. Treated as unavailable so the
            // scheduler backs off rather than treating the Network as permanently broken.
            Err(_) => {
                self.totals.stream_failures.fetch_add(1, Ordering::SeqCst);
                Err(ProviderError::Unavailable)
            }
        }
    }

    async fn release(&self, network: NetworkId) -> Result<(), ProviderError> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        let entry = {
            let mut scopes = self.scopes.lock().expect("the scope map is writable");
            scopes.remove(&network)
        };
        let Some(entry) = entry else {
            // Idempotent by contract: releasing an unknown or already-released Network
            // succeeds so a delete retried after a timeout converges.
            return Ok(());
        };
        // Signal first: it cannot be refused, and a task parked mid-exchange stops on it
        // rather than after its deadline. Dropping `entry.requests` then closes the
        // channel, which ends the receive loop for a task that was idle.
        let _ = entry.stop.send(true);
        drop(entry.requests);
        match tokio::time::timeout(self.release_timeout, entry.join).await {
            Ok(Ok(())) => Ok(()),
            // The task panicked or was cancelled. The map entry is already gone, so the
            // scope is unreachable from this process regardless; reporting rather than
            // claiming success is what lets a delete decide.
            Ok(Err(_)) => Err(ProviderError::Failed),
            Err(_) => Err(ProviderError::Timeout),
        }
    }
}

/// Owns one Network's SAM session for as long as the scope lives.
async fn scope_task(
    config: SamClientConfig,
    mut requests: mpsc::Receiver<ScopeRequest>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    health: Arc<ScopeHealth>,
    totals: Arc<Totals>,
) {
    let mut session: Option<LiveSession> = None;

    loop {
        // The stop flag is selected on rather than checked between requests, so a release
        // interrupts a task that is parked waiting for work instead of leaving it alive
        // until its current exchange finishes.
        let request = tokio::select! {
            biased;
            _ = wait_stop(&mut stop) => break,
            request = requests.recv() => match request {
                Some(request) => request,
                // The channel closed, which is how a release that only removed the map
                // entry ends this loop.
                None => break,
            },
        };
        match request {
            ScopeRequest::Connect { endpoint, reply } => {
                // Depth as the router-side owner sees it: what is still waiting behind
                // this one. A counter of requests being processed would always read one,
                // because this loop is the only reader, and would bound nothing.
                let depth = requests.len() as u64 + 1;
                totals.peak_queued.fetch_max(depth, Ordering::SeqCst);
                // The exchange is itself interruptible. Selecting only around the receive
                // would leave a release waiting out a connect that may be parked on a
                // silent router for a hundred seconds, and the delete that asked for the
                // release would report a timeout for a Network that is already gone.
                let stopped = tokio::select! {
                    biased;
                    _ = wait_stop(&mut stop) => true,
                    outcome = acquire(&config, &mut session, &health, &totals, endpoint) => {
                        match outcome {
                            Ok(stream) => {
                                totals.stream_successes.fetch_add(1, Ordering::SeqCst);
                                let _ = reply.send(Ok(Box::new(stream) as Box<dyn ByteStream>));
                            }
                            Err(error) => {
                                totals.stream_failures.fetch_add(1, Ordering::SeqCst);
                                let _ = reply.send(Err(error));
                            }
                        }
                        false
                    }
                };
                // Dropping `reply` here is the answer to a caller still waiting on this
                // scope: its `oneshot` closes and the connect reports unavailable rather
                // than blocking until the caller's own deadline.
                if stopped {
                    break;
                }
            }
        }
    }
    // Dropping the session drops any control socket with it. Anything still waiting holds
    // a `oneshot::Sender`, and dropping it reports failure to the caller, so a request
    // that arrived during a release sees an error rather than hanging.
    drop(session);
}

/// Resolves when the scope's stop flag is set, or when its sender is dropped.
async fn wait_stop(stop: &mut tokio::sync::watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    // The sender is dropped with the map entry, so a `changed` failure also means stopped.
    let _ = stop.changed().await;
}

/// A session that exists and is believed usable.
struct LiveSession {
    /// The opaque router-side session identity, reused across IRC generations.
    id: SamSessionId,
    /// Cleared when the control socket ends, which is the router saying this identity is
    /// gone.
    ///
    /// A flag rather than a channel, because the scope task owns the session and has to be
    /// able to ask about it without being woken. A watcher that sent a message instead
    /// would give the scope task two ways to learn the same fact and one more place for
    /// them to disagree.
    valid: Arc<AtomicBool>,
    /// The control-socket reader, so an end-of-stream is noticed at all.
    ///
    /// Aborted on drop: left running it would outlive the session and hold a socket the
    /// scope believes it has already released.
    watch: tokio::task::JoinHandle<()>,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

/// Watches a scope's control socket and retires the session when the router closes it.
///
/// The loss is counted *here*, where the end-of-stream is observed, rather than in the
/// next `acquire` that notices the flag. Recording it later would make the count a
/// function of when the next connect happens, so a Network that stopped reconnecting
/// would report a healthy session it had already lost.
///
/// Without this a router that discarded the session would leave the scope reusing an
/// identity the router no longer has, and every later stream connect would fail
/// `INVALID_ID` for as long as the Network stayed configured.
async fn watch_control(
    client: SamClient,
    valid: Arc<AtomicBool>,
    health: Arc<ScopeHealth>,
    totals: Arc<Totals>,
) {
    crate::client::watch_control_socket(client.into_socket(), Arc::clone(&valid)).await;
    totals.session_losses.fetch_add(1, Ordering::SeqCst);
    health.set_healthy(false);
}

/// Ensures a usable session, then opens one stream through it.
async fn acquire(
    config: &SamClientConfig,
    session: &mut Option<LiveSession>,
    health: &Arc<ScopeHealth>,
    totals: &Arc<Totals>,
    endpoint: I2pEndpoint,
) -> Result<SamRawStream, ProviderError> {
    // A control socket that ended means the router discarded the identity. Checking here,
    // before the session is reused, is what stops this scope from attaching to a session
    // that no longer exists; without it every stream connect would fail `INVALID_ID` until
    // something else happened to invalidate the session.
    if session
        .as_ref()
        .is_some_and(|live| !live.valid.load(Ordering::SeqCst))
    {
        // Not counted here: the watcher that saw the end-of-stream already counted it.
        // Counting in both places would report one lost session as two, and an operator
        // reading the ratio against `session_creations` would conclude the scope is
        // churning when it is not.
        *session = None;
    }
    if session.is_none() {
        let (mut client, id) = SamClient::open(config.clone()).await.map_err(map_error)?;
        client.create_session(&id).await.map_err(|error| {
            // A rejected create must not leave a half-built session behind.
            totals.session_losses.fetch_add(1, Ordering::SeqCst);
            health.set_healthy(false);
            map_error(error)
        })?;
        // The epoch lives on the per-scope health record rather than here: it describes
        // this scope's own lifetime, and keeping it in one place means there is a single
        // number an operator reads rather than one per Network that could disagree with it.
        health.epoch.fetch_add(1, Ordering::SeqCst);
        totals.session_creations.fetch_add(1, Ordering::SeqCst);
        health.set_healthy(true);
        let valid = Arc::new(AtomicBool::new(true));
        let watch = tokio::spawn(watch_control(
            client,
            Arc::clone(&valid),
            health.clone(),
            Arc::clone(totals),
        ));
        *session = Some(LiveSession { id, valid, watch });
    }
    let live = session.as_ref().expect("a session was just established");
    // `connect_stream` consumes the client, so the scope keeps the session identity and
    // epoch rather than the control socket: a fresh control socket is opened per stream
    // anyway, and the identity is what the router binds the conversation to.
    let id = live.id.clone();
    match SamClient::open(config.clone()).await {
        Ok((client, _socket_id)) => {
            // A `STREAM CONNECT` on a *different* session ID would create a second
            // router-side identity, which is exactly the correlation the scoping exists to
            // prevent. The data socket therefore uses the scope's session ID, not the one
            // this socket's hello minted; the socket's own ID is discarded.
            match client.connect_stream(&id, &endpoint).await {
                Ok(stream) => Ok(stream),
                Err(error) => {
                    if invalidates_session(&error) {
                        totals.session_losses.fetch_add(1, Ordering::SeqCst);
                        health.set_healthy(false);
                        *session = None;
                    }
                    Err(map_error(error))
                }
            }
        }
        Err(error) => {
            totals.session_losses.fetch_add(1, Ordering::SeqCst);
            health.set_healthy(false);
            *session = None;
            Err(map_error(error))
        }
    }
}

/// Whether a failure means the router no longer knows this session.
///
/// `INVALID_ID` is the router telling us the session is gone. Observing it here rather than
/// waiting for a control-socket EOF is what stops the next connect from attaching to a
/// session the router has already discarded.
fn invalidates_session(error: &SamError) -> bool {
    matches!(
        error,
        SamError::PeerUnavailable {
            rejection: StreamRejection::InvalidId
        }
    )
}

/// Maps a SAM error onto the four provider classes.
///
/// Deliberately coarse. The runtime's scheduler keys off retryability, and mapping every
/// router class onto a distinct `ProviderError` would mean the scheduler had to know about
/// SAM to be correct.
pub fn map_error(error: SamError) -> ProviderError {
    match error {
        // Retryable: the router may recover, or the peer may come back.
        SamError::BridgeUnavailable
        | SamError::SessionLost
        | SamError::Closed
        | SamError::Cancelled
        | SamError::PeerUnavailable { .. }
        | SamError::Timeout { .. } => ProviderError::Unavailable,
        // Permanent for this attempt: the input or the router version is wrong.
        SamError::InvalidBridge(_)
        | SamError::RandomUnavailable
        | SamError::UnsupportedVersion
        | SamError::DestinationRejected
        | SamError::SessionRejected { .. }
        | SamError::Malformed { .. } => ProviderError::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime passes its own ceiling in rather than this crate importing one, so the
    /// production number lives in exactly one place. Asserted through the constructor.
    #[test]
    fn the_scope_ceiling_comes_from_the_consumer() {
        let provider = SamProvider::new().with_limits(7, Duration::from_millis(5));
        assert_eq!(provider.max_scopes, 7);
        assert_eq!(provider.release_timeout, Duration::from_millis(5));
    }

    #[test]
    fn diagnostics_start_empty() {
        let provider = SamProvider::new();
        assert_eq!(provider.diagnostics(), SamDiagnostics::default());
    }

    /// `SamDiagnostics` must be printable without leaking. There is nowhere in it to put a
    /// session ID, and this asserts the whole struct is the redacted view.
    #[test]
    fn diagnostics_are_bounded_and_count_only() {
        let provider = SamProvider::new();
        let rendered = format!("{:?}", provider.diagnostics());
        for forbidden in ["SessionId", "Destination", "MESSAGE", "nick", "endpoint"] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden:?} must not appear in diagnostics: {rendered}"
            );
        }
    }
}
