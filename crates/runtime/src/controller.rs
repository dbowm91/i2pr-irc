//! The single runtime controller: one task that owns every Network owner.
//!
//! # Why this exists
//!
//! Before M005-A the catalog held a [`SupervisorHandle`] per Network and nothing held
//! the owner *task*. Deleting a Network dropped a handle; it did not prove the owner had
//! finished, so a replaced owner and its replacement could briefly both be live for the
//! same `NetworkId`. That is exactly the two-live-owners-for-one-Network state the
//! network-ownership invariant forbids.
//!
//! The controller makes the owner set a single value the process actually owns: it
//! keeps the bounded [`SupervisorHandle`] *and* the [`JoinHandle`] together, so
//! "the Network is gone" and "the Network's task finished" are the same event rather
//! than two that can disagree.
//!
//! # What a caller may hold
//!
//! A caller may hold [`RuntimeControlHandle`]. It carries only a bounded request
//! channel, a stop signal, and a read-only status subscription. It is not a
//! `NetworkCatalog`, not a `StoreHandle`, and not a `SupervisorHandle`: no client task
//! and no downstream session can reach a supervisor directly, and none can reach
//! storage. Every mutation is a typed [`ControlRequest`] executed by this one task.

use crate::{
    RuntimeError,
    catalog::{NetworkCatalog, SupervisorContext, SupervisorHandle},
    owner::{NetworkOwner, NetworkSnapshot},
};
use async_trait::async_trait;
use i2pr_irc_core::{I2pStreamProvider, NetworkId};
use i2pr_irc_store::{NetworkRecord, SavedNetwork, StoreError, StoreHandle};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

/// The durable Network operations the controller performs, and nothing else.
///
/// Narrow on purpose. The controller holds one; no client task, downstream session, or
/// Network owner can obtain one, so "only the controller mutates durable Networks" is a
/// property of the types rather than of a review.
///
/// It is a trait rather than a bare `StoreHandle` for one specific reason: the
/// `CommitState::Unknown` path -- a mutation whose durable outcome cannot be inferred --
/// is the single most consequential branch in this file, and a real SQLite commit
/// failure cannot be provoked from a test. A test double can return that state exactly,
/// which is what makes the "re-read and start what is durable" behaviour provable.
#[async_trait]
pub trait DurableNetworks: Send + Sync {
    /// Every durable Network, in `NetworkId` order.
    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError>;
    /// Creates or replaces one Network's complete configuration.
    async fn save(&self, record: &NetworkRecord) -> Result<SavedNetwork, StoreError>;
    /// Forgets one Network durably, reporting whether a row existed.
    async fn remove(&self, network: NetworkId) -> Result<bool, StoreError>;
}

#[async_trait]
impl DurableNetworks for StoreHandle {
    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError> {
        self.load_networks().await
    }

    async fn save(&self, record: &NetworkRecord) -> Result<SavedNetwork, StoreError> {
        self.save_network(record).await
    }

    async fn remove(&self, network: NetworkId) -> Result<bool, StoreError> {
        self.remove_network(network).await
    }
}

/// Explicit ceiling on pending control requests.
///
/// Control work is operator-driven and interactive, so a small bound is enough. The
/// bound exists so a caller that cannot keep up is refused explicitly instead of
/// allocating an unbounded queue of requests this task would eventually have to hold
/// all at once.
pub const CONTROL_REQUEST_CAPACITY: usize = 64;

/// Upper bound on entries in one [`ControlSnapshot`].
///
/// The snapshot is bounded by the same ceiling the catalog enforces, so a caller that
/// trusted it can allocate against it without an independent guess.
pub const MAX_CONTROL_SNAPSHOT_NETWORKS: usize = crate::catalog::MAX_SUPERVISED_NETWORKS;

/// One bounded, revisioned view of the whole runtime.
///
/// Every field is a count, a stable name, or a fixed classification. It never carries an
/// endpoint, a payload, a nickname, a credential, or a path, so publishing it to a
/// diagnostics surface cannot leak what the invariant in `AGENTS.md` forbids leaking
/// into IRC-visible or operator-visible fields.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlSnapshot {
    /// Monotonic count of completed state changes.
    ///
    /// A reader that must prove it saw a specific change reads the revision before and
    /// after. Comparing only field values cannot distinguish "nothing changed" from
    /// "everything changed back", and a bouncer that re-registers a client looks a lot
    /// like one that did nothing.
    pub revision: u64,
    /// Networks currently owned, in durable `NetworkId` order.
    pub networks: Vec<ControlNetwork>,
}

/// One Network's entry in a [`ControlSnapshot`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlNetwork {
    pub network: NetworkId,
    /// The operator-chosen name this Network is listed under.
    pub display_name: String,
    /// Whether a live owner task exists for this Network.
    pub live: bool,
    /// Current phase, as the owner reported it. `None` when no owner is live.
    pub phase: Option<String>,
    pub attached_sessions: usize,
    /// Fixed classification of the last attach disposition, never a payload.
    pub last_session_disposition: Option<&'static str>,
}

/// A typed mutation or observation the controller performs on behalf of one caller.
///
/// There is no "run this closure against the runtime" variant. A closure would hand the
/// controller's invariants to its caller; an enum keeps every capability that exists
/// written down here, where it can be read and bounded.
pub enum ControlRequest {
    /// Read the current [`ControlSnapshot`].
    Status {
        reply: oneshot::Sender<ControlSnapshot>,
    },
    /// Re-read one Network's durable configuration and reconcile it.
    Reconcile {
        network: NetworkId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Create a Network from a complete candidate record.
    Create {
        candidate: NetworkRecord,
        reply: oneshot::Sender<Result<NetworkId, RuntimeError>>,
    },
    /// Replace one Network's durable configuration and restart exactly the result.
    Change {
        candidate: NetworkRecord,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Stop one Network and forget it durably.
    Delete {
        network: NetworkId,
        reply: oneshot::Sender<Result<bool, RuntimeError>>,
    },
    /// Change whether one desired channel is presented to sessions.
    ///
    /// Routed to the live owner rather than committed here, because the owner owns the
    /// durable/live ordering for that decision: it commits first and only then changes
    /// presentation. A controller that committed directly would have two writers for one
    /// fact.
    ChannelPolicy {
        network: NetworkId,
        channel: String,
        detached: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Create one Network, with the identity allocated by the controller.
    CreateNext {
        candidate: NetworkRecord,
        reply: oneshot::Sender<Result<NetworkId, RuntimeError>>,
    },
    /// Read one Network's complete durable record.
    ///
    /// Administration needs a *complete* record to build an update from, because an
    /// update that resubmitted only the fields it understood would silently reset
    /// everything else. Routing the read through the controller keeps that read and the
    /// subsequent write in one serialized place: two concurrent updates cannot both
    /// read the same record and each write back a version that undid the other.
    Record {
        network: NetworkId,
        reply: oneshot::Sender<Result<Option<NetworkRecord>, RuntimeError>>,
    },
    /// Change one Network's presence/nick policy.
    ///
    /// Partial by design: `None` leaves a field as it is. A caller that had to read,
    /// modify, and resubmit a whole `NetworkRecord` could clobber a change made in
    /// between, and an administration command that silently resets what it did not
    /// mention is worse than one that refuses.
    PresencePolicy {
        network: NetworkId,
        auto_away: Option<bool>,
        keep_nick: Option<bool>,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Hand one already-registered local session to a live owner.
    ///
    /// The session arrives with its read half, writer task, line decoder, and buffered
    /// post-registration lines already in place. See
    /// [`crate::admission::PreparedSession`].
    Bind {
        network: NetworkId,
        session: PreparedBinding,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Stop every Network and end the controller.
    Stop { reply: oneshot::Sender<()> },
}

/// A fully registered local session being handed to a Network owner.
///
/// Deliberately opaque here: the controller only forwards it, and the owner is the only
/// place that knows what it contains. That keeps the one-shot transfer property
/// enforceable in one file instead of spread across three.
pub struct PreparedBinding {
    pub(crate) inner: crate::admission::PreparedSession,
}

/// Bounded, cloneable control surface for the runtime.
///
/// Cloning is cheap and shares the same queue, stop signal, and status channel. Two
/// clones are two handles onto one controller, not two controllers.
#[derive(Clone)]
pub struct RuntimeControlHandle {
    requests: mpsc::Sender<ControlRequest>,
    status: watch::Receiver<ControlSnapshot>,
    stop: watch::Sender<bool>,
}

impl RuntimeControlHandle {
    /// Reads the current revisioned snapshot.
    pub async fn status(&self) -> Result<ControlSnapshot, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Status { reply })?;
        response.await.map_err(|_| RuntimeError::Stopped)
    }

    /// Subscribes to future revisions without consuming a queue slot.
    ///
    /// `watch` keeps only the newest value, so a slow reader observes the latest state
    /// rather than accumulating a backlog the controller would have to retain.
    pub fn subscribe_status(&self) -> watch::Receiver<ControlSnapshot> {
        self.status.clone()
    }

    pub async fn reconcile(&self, network: NetworkId) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Reconcile { network, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn create(&self, candidate: NetworkRecord) -> Result<NetworkId, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Create { candidate, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn change(&self, candidate: NetworkRecord) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Change { candidate, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn delete(&self, network: NetworkId) -> Result<bool, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Delete { network, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn bind(
        &self,
        network: NetworkId,
        session: crate::admission::PreparedSession,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Bind {
            network,
            session: PreparedBinding { inner: session },
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Creates one Network, with its identity allocated here rather than by the caller.
    pub async fn create_next(&self, candidate: NetworkRecord) -> Result<NetworkId, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::CreateNext { candidate, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Reads one Network's complete durable record.
    pub async fn network_record(
        &self,
        network: NetworkId,
    ) -> Result<Option<NetworkRecord>, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Record { network, reply })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Sets one desired channel's presentation flag.
    pub async fn set_channel_detached(
        &self,
        network: NetworkId,
        channel: String,
        detached: bool,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ChannelPolicy {
            network,
            channel,
            detached,
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Changes one Network's presence/nick policy.
    pub async fn set_presence_policy(
        &self,
        network: NetworkId,
        auto_away: Option<bool>,
        keep_nick: Option<bool>,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::PresencePolicy {
            network,
            auto_away,
            keep_nick,
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    /// Requests that the controller stop every Network and end.
    ///
    /// Delivered on a dedicated watch channel rather than as a queued request, so a
    /// saturated control queue can never make shutdown unreachable. Shutdown is the one
    /// operation that must always be available.
    pub fn request_stop(&self) {
        let _ = self.stop.send(true);
    }

    /// Whether shutdown has been requested.
    ///
    /// Reads the same watch channel `request_stop` writes, so a caller can tell the
    /// difference between "I asked it to stop" and "it is not listening".
    pub fn is_stopping(&self) -> bool {
        *self.stop.borrow()
    }

    fn send(&self, request: ControlRequest) -> Result<(), RuntimeError> {
        // `try_send`, never `send`: a caller that cannot keep up is refused as
        // overload rather than parked in a queue this controller must also bound.
        self.requests
            .try_send(request)
            .map_err(|_| RuntimeError::QueueOverloaded)
    }
}

/// One live owner: its bounded handle and the task that runs it.
///
/// The two are kept in one value on purpose. A caller that holds only a handle cannot
/// prove the task ended; a caller that holds only a task cannot address it. The
/// controller needs both to guarantee a Network is never briefly owned twice.
struct LiveOwner {
    handle: SupervisorHandle,
    /// A read-only view of the owner's own gauges.
    ///
    /// The controller reads it; it can never write or drive through it.
    snapshot: watch::Receiver<NetworkSnapshot>,
    /// A dedicated stop signal, separate from the bounded command queue.
    ///
    /// Shutdown must never queue behind work the owner has not got to yet. Awaiting
    /// the task after asking it to stop through a full command queue is a deadlock:
    /// the queue is full precisely because the owner is busy, so it will not drain
    /// until it stops, and it will not stop until the queue drains.
    stop: watch::Sender<bool>,
    join: JoinHandle<Result<(), RuntimeError>>,
}

/// Owns every Network owner, the durable store they reconcile against, and the one
/// bounded queue through which all of it is mutated.
pub struct RuntimeController<P: I2pStreamProvider + Send + Sync + 'static> {
    provider: Arc<P>,
    /// The only component that mutates durable Networks.
    durable: Arc<dyn DurableNetworks>,
    catalog: NetworkCatalog,
    /// Owner handles and their tasks, keyed by durable identity.
    live: BTreeMap<NetworkId, LiveOwner>,
    /// Durable records, so a snapshot can name each Network without touching storage.
    records: BTreeMap<NetworkId, NetworkRecord>,
    revision: u64,
    status: watch::Sender<ControlSnapshot>,
    requests: mpsc::Receiver<ControlRequest>,
    stop: watch::Receiver<bool>,
    /// Set once shutdown is observed, so the loop reports it instead of draining.
    stopping: bool,
    /// The controller's own handle onto itself.
    ///
    /// Owners are handed a clone so a session bound to a Network can administrate
    /// through the same bounded queue every other caller uses. The controller keeps one
    /// because it is the only thing that can build a handle in the first place, and the
    /// alternative -- reconstructing one from receivers it does not hold -- would be a
    /// second, unowned way to reach the control plane.
    handle: RuntimeControlHandle,
}

impl<P: I2pStreamProvider + Send + Sync + 'static> RuntimeController<P> {
    /// Builds a controller over an already-open store.
    ///
    /// Returns the controller and the handle a caller uses to drive it. Neither half
    /// starts anything: `serve` performs startup restore, so constructing a controller
    /// cannot open an upstream connection as a side effect.
    pub fn new(provider: P, store: StoreHandle) -> (Self, RuntimeControlHandle) {
        Self::with_reconnect(
            provider,
            store,
            crate::reconnect::ReconnectScheduler::default(),
        )
    }

    /// Builds a controller over an explicit durable surface.
    ///
    /// Exists so a test can present a durable layer that answers with an ambiguous
    /// commit state. Production uses [`RuntimeController::new`], where the durable
    /// layer is the store handle itself.
    pub fn with_durable(
        provider: P,
        store: StoreHandle,
        durable: Arc<dyn DurableNetworks>,
    ) -> (Self, RuntimeControlHandle) {
        Self::assemble(
            provider,
            store,
            durable,
            crate::reconnect::ReconnectScheduler::default(),
        )
    }

    /// Builds a controller over an explicit connect budget.
    ///
    /// Injected so tests can use a virtual-time-friendly policy instead of depending on
    /// the wall-clock production values.
    pub fn with_reconnect(
        provider: P,
        store: StoreHandle,
        reconnect: crate::reconnect::ReconnectScheduler,
    ) -> (Self, RuntimeControlHandle) {
        let durable: Arc<dyn DurableNetworks> = Arc::new(store.clone());
        Self::assemble(provider, store, durable, reconnect)
    }

    fn assemble(
        provider: P,
        store: StoreHandle,
        durable: Arc<dyn DurableNetworks>,
        reconnect: crate::reconnect::ReconnectScheduler,
    ) -> (Self, RuntimeControlHandle) {
        let catalog = NetworkCatalog::with_reconnect(store, reconnect);
        let (requests, request_rx) = mpsc::channel(CONTROL_REQUEST_CAPACITY);
        let (status, status_rx) = watch::channel(ControlSnapshot::default());
        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = RuntimeControlHandle {
            requests,
            status: status_rx,
            stop: stop_tx,
        };
        (
            Self {
                provider: Arc::new(provider),
                durable,
                catalog,
                live: BTreeMap::new(),
                records: BTreeMap::new(),
                revision: 0,
                status,
                requests: request_rx,
                stop: stop_rx,
                stopping: false,
                handle: handle.clone(),
            },
            handle,
        )
    }

    /// Restores durable Networks, then serves control requests until shutdown.
    ///
    /// Startup restore happens here rather than in `new` so a failure to read storage
    /// surfaces as a `serve` error rather than as a controller that looks usable and is
    /// not.
    pub async fn serve(&mut self) -> Result<(), RuntimeError> {
        self.restore().await?;
        self.publish();
        loop {
            if self.stop_requested() {
                break;
            }
            tokio::select! {
                biased;
                _ = wait_stopped(&mut self.stop) => {
                    self.stopping = true;
                    break;
                }
                Some(request) = self.requests.recv() => self.dispatch(request).await,
            }
        }
        self.shutdown().await;
        Ok(())
    }

    /// Re-reads durable state and starts exactly one owner per stored Network.
    ///
    /// A Network whose record fails validation is skipped and reported, never repaired:
    /// inventing a plausible configuration would reinterpret durable meaning. The
    /// remaining Networks still start, because one unusable row must not take the
    /// whole runtime down.
    async fn restore(&mut self) -> Result<(), RuntimeError> {
        let records = self.durable.load().await.map_err(map_store)?;
        for record in records {
            // A duplicate id cannot occur in the durable schema, but a hand-edited file
            // could carry one. Refusing the second is the only safe reading: two owners
            // for one Network is exactly what this controller exists to prevent.
            if self.records.contains_key(&record.network) {
                continue;
            }
            if record.validate().is_err() {
                continue;
            }
            let network = record.network;
            self.records.insert(network, record);
            self.start_owner(network);
        }
        Ok(())
    }

    /// Starts one owner for a Network already present in `records`.
    fn start_owner(&mut self, network: NetworkId) {
        let Some(record) = self.records.get(&network).cloned() else {
            return;
        };
        if self.live.contains_key(&network) {
            return;
        }
        if self.catalog.len() >= crate::catalog::MAX_SUPERVISED_NETWORKS {
            return;
        }
        let context = SupervisorContext {
            network,
            record: Arc::new(record),
            store: self.catalog.store().clone(),
            status: self.catalog.status_sender(),
            resources: self.catalog.resources().clone(),
        };
        let (owner_snapshot, snapshot_rx) = watch::channel(NetworkSnapshot::default());
        let owner = match NetworkOwner::with_snapshot_channel(
            self.provider.clone(),
            context,
            self.catalog.store().clone(),
            self.catalog.reconnect().clone(),
            owner_snapshot,
        )
        // A session bound to this Network is still the local Operator's own connection,
        // so the owner answers its administrative requests through the same controller
        // that owns every live owner. The owner holds a bounded sender and gains no
        // authority it did not already route here.
        .map(|owner| owner.with_control(self.handle.clone()))
        {
            Ok(owner) => owner,
            // Registration with process-wide accounting failed. The Network stays durable
            // and is reported as not live; it is never silently forgotten.
            Err(_) => return,
        };
        let (commands, receiver) = mpsc::channel(crate::catalog::SESSION_QUEUE_CAPACITY);
        let (owner_stop, stop_receiver) = crate::catalog::stop_signal();
        let owner_stop_handle = owner_stop.clone();
        let handle = SupervisorHandle::new(network, commands);
        if self.catalog.insert(handle.clone()).is_err() {
            return;
        }
        let join = tokio::spawn(async move {
            let result = owner.serve(receiver, stop_receiver).await;
            // The owner's own stop signal is a private copy; ask it to finish so an
            // owner task can never outlive the entry that holds it.
            let _ = owner_stop.send(true);
            result
        });
        self.live.insert(
            network,
            LiveOwner {
                handle,
                snapshot: snapshot_rx,
                stop: owner_stop_handle,
                join,
            },
        );
    }

    /// Stops one owner and waits for its task, so the Network is provably gone.
    async fn stop_owner(&mut self, network: NetworkId) -> Result<(), RuntimeError> {
        let Some(owner) = self.live.remove(&network) else {
            return Ok(());
        };
        // The signal first: it cannot be refused, and the owner ends its generation
        // and releases its sessions on seeing it. The command is then best-effort, so
        // the owner also ends cleanly at its next select turn.
        let _ = owner.stop.send(true);
        let _ = owner.handle.stop().await;
        // The join is the point of this method. Returning before it completes would
        // leave the replacement owner racing the old one for the same NetworkId.
        match owner.join.await {
            Ok(result) => result,
            Err(join_error) if join_error.is_panic() => Err(RuntimeError::Stopped),
            Err(_) => Err(RuntimeError::Stopped),
        }
    }

    async fn dispatch(&mut self, request: ControlRequest) {
        match request {
            ControlRequest::Status { reply } => {
                let _ = reply.send(self.status.borrow().clone());
            }
            ControlRequest::Reconcile { network, reply } => {
                let outcome = match self.live.get(&network) {
                    Some(owner) => owner.handle.reconcile().await,
                    None => Err(RuntimeError::InvalidConfig),
                };
                self.commit();
                let _ = reply.send(outcome);
            }
            ControlRequest::Create { candidate, reply } => {
                let _ = reply.send(self.create(candidate).await);
            }
            ControlRequest::Change { candidate, reply } => {
                let _ = reply.send(self.change(candidate).await);
            }
            ControlRequest::Delete { network, reply } => {
                let _ = reply.send(self.delete(network).await);
            }
            ControlRequest::Bind {
                network,
                session,
                reply,
            } => {
                let outcome = match self.live.get(&network) {
                    Some(owner) => owner.handle.attach_prepared(Box::new(session.inner)).await,
                    // A bound session is already registered with a local client and has
                    // a live socket. Refusing it without unwinding would leave the client
                    // registered with nothing serving it.
                    None => Err(RuntimeError::InvalidConfig),
                };
                let _ = reply.send(outcome);
            }
            ControlRequest::ChannelPolicy {
                network,
                channel,
                detached,
                reply,
            } => {
                let outcome = match self.live.get(&network) {
                    Some(owner) => {
                        owner
                            .handle
                            .set_channel_detached(network, channel, detached)
                            .await
                    }
                    // A Network with no live owner has no presentation to change, and
                    // pretending otherwise would tell the Operator a policy is in force
                    // when nothing is enforcing it.
                    None => Err(RuntimeError::InvalidConfig),
                };
                // The owner wrote the flag, so this cache is now behind durable state.
                // Re-reading is what keeps `channel status` from answering a question
                // about the world as it was before the command that was just answered.
                if outcome.is_ok() {
                    self.reread().await;
                    self.commit();
                }
                let _ = reply.send(outcome);
            }
            ControlRequest::CreateNext { candidate, reply } => {
                let _ = reply.send(self.create_next(candidate).await);
            }
            ControlRequest::Record { network, reply } => {
                let _ = reply.send(Ok(self.records.get(&network).cloned()));
            }
            ControlRequest::PresencePolicy {
                network,
                auto_away,
                keep_nick,
                reply,
            } => {
                let _ = reply.send(
                    self.set_presence_policy(network, auto_away, keep_nick)
                        .await,
                );
            }
            ControlRequest::Stop { reply } => {
                self.stopping = true;
                let _ = reply.send(());
            }
        }
    }

    /// Applies a partial presence/nick policy change to one Network.
    ///
    /// The read-modify-write happens here rather than in the caller, so two
    /// administration requests cannot both read the same record and each write back a
    /// value that silently undid the other. Only the fields the caller named are
    /// touched; everything else in the record — endpoint, nick, channels, secret — is
    /// carried through untouched.
    async fn set_presence_policy(
        &mut self,
        network: NetworkId,
        auto_away: Option<bool>,
        keep_nick: Option<bool>,
    ) -> Result<(), RuntimeError> {
        let Some(mut candidate) = self.records.get(&network).cloned() else {
            return Err(RuntimeError::InvalidConfig);
        };
        if let Some(auto_away) = auto_away {
            candidate.auto_away = auto_away;
        }
        if let Some(keep_nick) = keep_nick {
            candidate.keep_nick = keep_nick;
        }
        self.change(candidate).await
    }

    /// Creates one Network, allocating its identity here.
    ///
    /// The candidate's own `network` field is ignored and overwritten. Netids are
    /// allocated in exactly one place because a caller-chosen identity turns `ADDNETWORK`
    /// into a race: two clients creating a Network would both pick the same free id, and
    /// the loser would be refused for a reason that has nothing to do with what it asked
    /// for. Plan 023 is the first caller that needs this; everything else still chooses
    /// its own identity explicitly, which is fine for a caller that is the sole author of
    /// its catalog.
    async fn create_next(
        &mut self,
        mut candidate: NetworkRecord,
    ) -> Result<NetworkId, RuntimeError> {
        candidate.network = self.next_netid();
        self.create(candidate).await
    }

    /// The lowest free Network identity.
    ///
    /// Bounded by the catalog ceiling, so this is at most `MAX_SUPERVISED_NETWORKS`
    /// probes rather than an open-ended search over the `u64` space.
    fn next_netid(&self) -> NetworkId {
        (1..=crate::catalog::MAX_SUPERVISED_NETWORKS as u64)
            .map(NetworkId)
            .find(|candidate| !self.records.contains_key(candidate))
            // Unreachable: `create` refuses to exceed the ceiling, so a full catalog
            // fails the length check before this runs. Naming the ceiling rather than
            // panicking keeps the "refuse, never guess" property intact if that order is
            // ever changed.
            .unwrap_or(NetworkId(0))
    }

    /// Creates one Network from a complete candidate record.
    ///
    /// Durable configuration is committed before any owner starts. If activation then
    /// fails, the record stays durable and the Network is reported as not live: the
    /// operator's intent survives, and a later reconcile can activate it. The opposite
    /// order would leave a live Network the operator cannot see and cannot delete.
    async fn create(&mut self, candidate: NetworkRecord) -> Result<NetworkId, RuntimeError> {
        candidate
            .validate()
            .map_err(|_| RuntimeError::InvalidConfig)?;
        let network = candidate.network;
        if self.records.contains_key(&network) {
            return Err(RuntimeError::InvalidConfig);
        }
        if self.records.len() >= crate::catalog::MAX_SUPERVISED_NETWORKS {
            return Err(RuntimeError::QueueOverloaded);
        }
        match self.durable.save(&candidate).await {
            Ok(_) => {}
            Err(error) => {
                if error.may_have_committed() {
                    // The commit may have landed even though the reply did not arrive.
                    // Re-read rather than retrying a save that might now overwrite a
                    // row another actor changed.
                    self.reread().await;
                    if self.records.contains_key(&network) {
                        self.commit();
                        return Ok(network);
                    }
                }
                return Err(map_store(error));
            }
        }
        self.records.insert(network, candidate);
        self.start_owner(network);
        self.commit();
        Ok(network)
    }

    /// Replaces one Network's durable configuration and restarts exactly the result.
    ///
    /// The old owner is stopped *first* and awaited, so the replacement cannot race it.
    /// If the commit's durable state is unknown, the durable record is re-read and
    /// whatever is actually on disk is what gets started -- never the candidate.
    async fn change(&mut self, candidate: NetworkRecord) -> Result<(), RuntimeError> {
        candidate
            .validate()
            .map_err(|_| RuntimeError::InvalidConfig)?;
        let network = candidate.network;
        if !self.records.contains_key(&network) {
            return Err(RuntimeError::InvalidConfig);
        }
        self.stop_owner(network).await?;
        // The old owner is gone whatever storage then says, so the snapshot is
        // republished before storage is touched. A change that fails must not leave the
        // published state claiming a live owner that has already stopped: an untruthful
        // gauge is worse than no gauge, because an operator reading it waits for
        // something that is never coming.
        self.commit();
        let commit = self.durable.save(&candidate).await;
        let mut start = candidate.clone();
        if let Err(error) = &commit
            && error.may_have_committed()
        {
            self.reread().await;
            match self.records.get(&network) {
                Some(durable) => start = durable.clone(),
                // The row is gone: nothing to start, and the caller must learn that the
                // Network it asked to change no longer exists rather than see success.
                None => {
                    self.commit();
                    return Err(RuntimeError::InvalidConfig);
                }
            }
        }
        self.records.insert(network, start.clone());
        self.start_owner(network);
        self.commit();
        commit.map(|_| ()).map_err(map_store)
    }

    /// Stops one Network, then forgets it durably.
    ///
    /// Stopping before the durable delete means a failed delete leaves a Network that
    /// is still configured but temporarily not running, which the operator can retry.
    /// The reverse order would leave a durable row whose owner is gone and whose
    /// sessions have silently ended.
    async fn delete(&mut self, network: NetworkId) -> Result<bool, RuntimeError> {
        if !self.records.contains_key(&network) {
            return Ok(false);
        }
        self.stop_owner(network).await?;
        // Republished before storage is touched, for the same reason as `change`: the
        // owner is already stopped, and the snapshot must not say otherwise.
        self.commit();
        let removed = self.durable.remove(network).await.map_err(map_store)?;
        self.records.remove(&network);
        self.catalog.remove(network);
        self.commit();
        Ok(removed)
    }

    /// Re-reads durable Network records after an ambiguous commit.
    async fn reread(&mut self) {
        if let Ok(records) = self.durable.load().await {
            self.records = records
                .into_iter()
                .filter(|record| record.validate().is_ok())
                .map(|record| (record.network, record))
                .collect();
        }
    }

    /// Stops every owner and awaits every task.
    ///
    /// Each Network is removed from the live map before its task is awaited, so a
    /// shutdown that is itself observed cannot re-enter and start the same Network
    /// twice.
    async fn shutdown(&mut self) {
        let networks: Vec<NetworkId> = self.live.keys().copied().collect();
        for network in networks {
            let _ = self.stop_owner(network).await;
            self.catalog.remove(network);
        }
        self.commit();
        self.stopping = false;
    }

    fn stop_requested(&self) -> bool {
        self.stopping || *self.stop.borrow()
    }

    /// Bumps the revision and republishes, always together.
    fn commit(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.publish();
    }

    /// Recomputes the snapshot without claiming a state change.
    fn publish(&mut self) {
        let mut networks = Vec::with_capacity(self.records.len());
        for (network, record) in &self.records {
            let entry = match self.live.get(network) {
                Some(owner) => {
                    let gauge = owner.snapshot.borrow();
                    ControlNetwork {
                        network: *network,
                        display_name: record.display_name.clone(),
                        live: true,
                        phase: gauge.phase.map(|phase| phase.as_str().to_owned()),
                        attached_sessions: gauge.attached_sessions,
                        last_session_disposition: gauge.last_session_disposition,
                    }
                }
                None => ControlNetwork {
                    network: *network,
                    display_name: record.display_name.clone(),
                    live: false,
                    phase: None,
                    attached_sessions: 0,
                    last_session_disposition: None,
                },
            };
            networks.push(entry);
            if networks.len() >= MAX_CONTROL_SNAPSHOT_NETWORKS {
                break;
            }
        }
        let _ = self.status.send(ControlSnapshot {
            revision: self.revision,
            networks,
        });
    }
}

/// Waits for a stop request without spinning when none is available.
async fn wait_stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// Maps a typed store failure onto the runtime's classification.
fn map_store(error: StoreError) -> RuntimeError {
    use i2pr_irc_store::StoreErrorKind as Kind;
    match error.kind() {
        Kind::QueueOverloaded => RuntimeError::QueueOverloaded,
        Kind::Stopped => RuntimeError::Stopped,
        Kind::InvalidRequest(_) | Kind::Corrupt(_) | Kind::LimitExceeded(_) => {
            RuntimeError::InvalidConfig
        }
        Kind::SchemaTooNew | Kind::ForeignDatabase | Kind::SqliteTooOld | Kind::Open => {
            RuntimeError::Stopped
        }
        Kind::Sqlite => RuntimeError::InvalidConfig,
    }
}
