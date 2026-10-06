//! Process-level ownership of many upstream Networks and their downstream sessions.
//!
//! The catalog is the only place that decides *which* Networks exist. It holds one
//! control handle per Network and starts/stops them independently, so one Network's
//! failure, backoff, or stop can never alter another's phase, generation, or sessions.
//!
//! There is deliberately no process-wide `Arc<Mutex<NetworkState>>`. Each Network keeps
//! its own owner and its own observed state; the catalog only routes typed intents to
//! the right one. That is what keeps "one live owner per Network" true at scale.
use crate::{
    RuntimeError, downstream::DownstreamDisposition, session::SessionEvent, session::SessionIntent,
};
use i2pr_irc_core::{
    ByteStream, ClientId, ConnectionGeneration, NetworkId, SessionId, SessionIdAllocator,
};
use i2pr_irc_store::{NetworkRecord, StoreHandle};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot, watch};

/// Explicit ceiling on Networks supervised by one catalog.
///
/// A catalog is a process-lifetime owner, so the bound is on how many it can start,
/// not on how long it lives.
pub const MAX_SUPERVISED_NETWORKS: usize = 64;
/// Explicit ceiling on simultaneous sessions attached to one Network.
///
/// A full attachment queue is refused explicitly so a client flood cannot grow
/// unbounded state in the network owner.
pub const SESSION_QUEUE_CAPACITY: usize = 256;

/// Commands a catalog sends to one Network owner.
pub enum SupervisorCommand {
    /// Attach a new local client to this Network.
    Attach {
        session: SessionId,
        client: ClientId,
        stream: Box<dyn ByteStream>,
        /// Answers whether the attachment was accepted.
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// One session reported an intent or ended.
    Session {
        session: SessionId,
        event: SessionEvent,
    },
    /// Re-read durable configuration and reconcile.
    Reconcile {
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// End this Network's upstream session and every attached session.
    Stop { reply: oneshot::Sender<()> },
}

/// Bounded control handle to one Network owner.
#[derive(Clone)]
pub struct SupervisorHandle {
    network: NetworkId,
    commands: mpsc::Sender<SupervisorCommand>,
}

impl SupervisorHandle {
    /// Builds a bounded control handle for one Network owner.
    pub fn new(network: NetworkId, commands: mpsc::Sender<SupervisorCommand>) -> Self {
        Self { network, commands }
    }

    pub fn network(&self) -> NetworkId {
        self.network
    }

    /// Attaches a local client. The ceiling is enforced by the bounded queue, so a
    /// flood is refused as overload rather than queued without limit.
    pub async fn attach(
        &self,
        session: SessionId,
        client: ClientId,
        stream: Box<dyn ByteStream>,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Attach {
                session,
                client,
                stream,
                reply,
            })
            .map_err(|_| RuntimeError::QueueOverloaded)?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Re-reads durable configuration for this Network.
    pub async fn reconcile(&self) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(SupervisorCommand::Reconcile { reply })
            .map_err(|_| RuntimeError::QueueOverloaded)?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Stops this Network only.
    pub async fn stop(&self) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        if self
            .commands
            .try_send(SupervisorCommand::Stop { reply })
            .is_err()
        {
            return Err(RuntimeError::QueueOverloaded);
        }
        let _ = response.await;
        Ok(())
    }
}

/// Diagnostic projection of one supervised Network. It never carries payloads,
/// endpoints, or credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkStatus {
    pub network: NetworkId,
    pub phase: String,
    pub generation: Option<ConnectionGeneration>,
    /// Live sessions currently attached.
    pub attached_sessions: usize,
    pub sessions_accepted: u64,
    pub sessions_ended: u64,
    pub last_session_disposition: Option<&'static str>,
}

/// Read-only view of the catalog for diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogStatus {
    pub networks: Vec<NetworkStatus>,
}

/// Owns every Network supervisor and the durable store they reconcile against.
pub struct NetworkCatalog {
    store: StoreHandle,
    handles: BTreeMap<NetworkId, SupervisorHandle>,
    statuses: watch::Sender<CatalogStatus>,
    sessions: SessionIdAllocator,
}

impl NetworkCatalog {
    /// Builds a catalog over an already-open store.
    pub fn new(store: StoreHandle) -> Self {
        let (statuses, _) = watch::channel(CatalogStatus::default());
        Self {
            store,
            handles: BTreeMap::new(),
            statuses,
            sessions: SessionIdAllocator::new(),
        }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// Allocates the next ephemeral attachment identity.
    pub fn allocate_session(&self) -> Result<SessionId, RuntimeError> {
        self.sessions
            .allocate()
            .ok_or(RuntimeError::GenerationExhausted)
    }

    /// Registers one Network owner. The bound is explicit, so a catalog cannot grow
    /// past what it can actually supervise.
    pub fn insert(&mut self, handle: SupervisorHandle) -> Result<(), RuntimeError> {
        if !self.handles.contains_key(&handle.network())
            && self.handles.len() >= MAX_SUPERVISED_NETWORKS
        {
            return Err(RuntimeError::QueueOverloaded);
        }
        self.handles.insert(handle.network(), handle);
        Ok(())
    }

    pub fn remove(&mut self, network: NetworkId) -> Option<SupervisorHandle> {
        self.handles.remove(&network)
    }

    pub fn get(&self, network: NetworkId) -> Option<&SupervisorHandle> {
        self.handles.get(&network)
    }

    pub fn networks(&self) -> Vec<NetworkId> {
        self.handles.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<CatalogStatus> {
        self.statuses.subscribe()
    }

    /// Publishes the current catalog view.
    pub fn publish(&self, statuses: Vec<NetworkStatus>) {
        self.statuses
            .send_replace(CatalogStatus { networks: statuses });
    }

    /// Rebuilds the set of durable Networks from storage and returns each record.
    ///
    /// Restart uses this: stored DesiredState is loaded, and no live state is
    /// restored, so every supervisor starts from fresh ObservedState.
    pub async fn load_desired_state(&self) -> Result<Vec<NetworkRecord>, RuntimeError> {
        self.store
            .load_networks()
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Stops every Network independently. One failure never prevents the others from
    /// being told to stop.
    pub async fn stop_all(&self) {
        for handle in self.handles.values() {
            let _ = handle.stop().await;
        }
    }
}

/// Maps a typed store error onto the runtime's error vocabulary.
///
/// Storage pressure is deliberately *not* a network-liveness failure: a client
/// operation that cannot be persisted fails on its own terms while the upstream
/// Network keeps running.
pub(crate) fn classify(kind: &i2pr_irc_store::StoreErrorKind) -> RuntimeError {
    use i2pr_irc_store::StoreErrorKind as Kind;
    match kind {
        Kind::QueueOverloaded | Kind::Stopped => RuntimeError::QueueOverloaded,
        Kind::InvalidRequest(_) | Kind::LimitExceeded(_) | Kind::Corrupt(_) => {
            RuntimeError::InvalidConfig
        }
        Kind::SchemaTooNew | Kind::ForeignDatabase | Kind::SqliteTooOld | Kind::Open => {
            // A database this build cannot serve prevents normal startup; it is
            // surfaced rather than being worked around.
            RuntimeError::InvalidConfig
        }
        // A SQLite failure that is really a domain or constraint violation (a missing
        // client lineage, for example) must not be reported as a protocol failure.
        Kind::Sqlite => RuntimeError::InvalidConfig,
    }
}

/// Bounds the total sessions one catalog may hold at once, so many Networks cannot
/// together produce an unbounded session population.
pub const MAX_TOTAL_SESSIONS: usize = 1024;

/// Process-wide shutdown signal shared by every supervisor.
pub fn stop_signal() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

/// Marks a stop request once, so repeated callers do not queue redundant stops.
pub fn request_stop(sender: &watch::Sender<bool>, flag: &AtomicBool) {
    if !flag.swap(true, Ordering::SeqCst) {
        let _ = sender.send(true);
    }
}

/// Shared owner state passed to each supervisor task.
#[derive(Clone)]
pub struct SupervisorContext {
    pub network: NetworkId,
    pub record: Arc<NetworkRecord>,
    pub store: StoreHandle,
    pub status: watch::Sender<CatalogStatus>,
}

/// A stop request the catalog records for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StopRecord {
    pub network: NetworkId,
    pub sessions_ended: u64,
}

/// Attaches one session to a network handle and reports the outcome.
///
/// This is the only supported attachment path: it allocates the ephemeral session
/// identity, submits the bounded attach command, and returns the identity that the
/// caller must use for every later operation on this attachment.
pub async fn attach(
    handle: &SupervisorHandle,
    client: ClientId,
    allocator: &SessionIdAllocator,
    stream: Box<dyn ByteStream>,
) -> Result<SessionId, RuntimeError> {
    let session = allocator
        .allocate()
        .ok_or(RuntimeError::GenerationExhausted)?;
    handle.attach(session, client, stream).await?;
    Ok(session)
}

/// True when a disposition ends only the client, leaving the Network running.
pub fn is_client_local(disposition: DownstreamDisposition) -> bool {
    disposition.is_local_only()
}

/// Re-exported so a caller can classify a session intent without importing the module.
pub fn intent_class(intent: &SessionIntent) -> &'static str {
    match intent {
        SessionIntent::Forward { .. } => "forward",
        SessionIntent::Join { .. } => "join",
        SessionIntent::Part { .. } => "part",
        SessionIntent::RequestProjection => "projection",
        SessionIntent::Quit => "quit",
    }
}

/// Bounded session event queue for one Network owner.
pub fn session_events() -> (mpsc::Sender<SessionEvent>, mpsc::Receiver<SessionEvent>) {
    mpsc::channel(SESSION_QUEUE_CAPACITY)
}
