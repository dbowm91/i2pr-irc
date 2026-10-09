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

use crate::PROVIDER_RELEASE_TIMEOUT;
use crate::{
    RuntimeError,
    action::ActionSet,
    catalog::{NetworkCatalog, SupervisorContext, SupervisorHandle},
    config_snapshot::{self, ApplyOutcome, ConfigSnapshot, ImportStep, SnapshotNetwork},
    diagnostics::{self, ProcessDiagnostics},
    owner::{NetworkOwner, NetworkSnapshot},
};
use async_trait::async_trait;
use i2pr_irc_core::{I2pStreamProvider, NetworkId};
use i2pr_irc_store::{
    BufferKind, BufferRetentionPolicy, NetworkRecord, SavedNetwork, StoreError, StoreHandle,
};
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
    /// The capability names this Network currently advertises to clients.
    ///
    /// Published here rather than recomputed by the reader because it is a function of
    /// the *upstream* negotiation, which only the owner knows. A client that asks for a
    /// capability during registration is answered before any owner exists for it, so it
    /// needs this value before registration finishes -- and an empty list here would mean
    /// "this Network offers nothing", not "this Network has not answered yet".
    pub advertisement: Vec<String>,
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
    /// Read one Network's current downstream advertisement.
    Advertisement {
        network: NetworkId,
        reply: oneshot::Sender<Vec<String>>,
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
    ChannelActivityPolicy {
        network: NetworkId,
        channel: String,
        activity: i2pr_irc_store::ChannelActivityPolicy,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    WatchRules {
        network: NetworkId,
        rules: Option<Vec<i2pr_irc_store::WatchRule>>,
        reply: oneshot::Sender<Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError>>,
    },
    WatchRuleChange {
        network: NetworkId,
        change: WatchRuleChange,
        reply: oneshot::Sender<Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError>>,
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
    /// Read or replace one bounded per-buffer privacy policy.
    BufferRetention {
        network: NetworkId,
        kind: BufferKind,
        target: String,
        policy: Option<Option<BufferRetentionPolicy>>,
        reply: oneshot::Sender<Result<BufferRetentionPolicy, RuntimeError>>,
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
    /// Read the bounded, secret-free diagnostics report.
    ///
    /// `network` selects one Network; `None` reports the process alongside every Network
    /// the controller has a live owner for. Routed through the controller because it reads
    /// every owner's snapshot plus the scheduler and the ledger, and assembling that from
    /// an operator session would need reach into all three.
    Diagnostics {
        network: Option<NetworkId>,
        reply: oneshot::Sender<Result<ProcessDiagnostics, RuntimeError>>,
    },
    /// Read the whole durable configuration as a versioned, secret-free snapshot.
    ExportConfig {
        reply: oneshot::Sender<Result<ConfigSnapshot, RuntimeError>>,
    },
    /// Apply a validated configuration snapshot, one Network at a time.
    ///
    /// Takes the snapshot by value so the controller cannot mutate anything until the whole
    /// of it has been validated: the caller had to produce a `ConfigSnapshot`, and the only
    /// way to hold one is to have parsed it. A command that could pass unvalidated text in
    /// would be able to apply a Network before failing on the one after it.
    ImportConfig {
        snapshot: ConfigSnapshot,
        reply: oneshot::Sender<ApplyOutcome>,
    },
    /// Replace one Network's whole stored registration-action list.
    ActionSet {
        network: NetworkId,
        actions: ActionSet,
        reply: oneshot::Sender<Result<usize, RuntimeError>>,
    },
    /// Replace one phase while preserving other phases as one serialized controller intent.
    ActionSetPhase {
        network: NetworkId,
        phase: i2pr_irc_store::RegistrationActionPhase,
        actions: ActionSet,
        reply: oneshot::Sender<Result<usize, RuntimeError>>,
    },
    /// Read one Network's stored registration actions.
    ///
    /// Returns the typed model rather than the durable rows, so a caller cannot bypass the
    /// replay ceilings by reading storage and writing the frames itself.
    ActionList {
        network: NetworkId,
        reply: oneshot::Sender<Result<ActionSet, RuntimeError>>,
    },
    /// Stop every Network and end the controller.
    Stop { reply: oneshot::Sender<()> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchRuleChange {
    Add(i2pr_irc_store::WatchRule),
    Delete(u32),
    Clear,
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

    /// Reads the capability names one Network currently advertises.
    ///
    /// Read from the live owner rather than from the published snapshot, because the two
    /// answer different questions. The published snapshot is a copy taken at the last
    /// controller revision, and an owner negotiates with its upstream moments after it is
    /// inserted -- so a copy can report an advertisement that is still empty while the
    /// owner has long since published a real one. A client negotiating during
    /// registration asks precisely in that window.
    ///
    /// Empty means the Network has no live owner yet, or its owner has not finished
    /// negotiating upstream; the caller falls back to what the build serves
    /// unconditionally.
    pub async fn advertisement(&self, network: NetworkId) -> Result<Vec<String>, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Advertisement { network, reply })?;
        response.await.map_err(|_| RuntimeError::Stopped)
    }

    /// Reads the bounded diagnostics report.
    ///
    /// Reads the live owners rather than the published `ControlSnapshot`, for the same
    /// reason [`Self::advertisement`] does: the controller's published copy is taken at a
    /// revision boundary, and a diagnostic that reported a stale queue depth or a
    /// reconnect count would be actively misleading during exactly the incidents an
    /// Operator opens it for.
    pub async fn diagnostics(
        &self,
        network: Option<NetworkId>,
    ) -> Result<ProcessDiagnostics, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::Diagnostics { network, reply })?;
        response.await.map_err(|_| RuntimeError::Stopped)?
    }

    /// Reads one Network's stored registration actions.
    pub async fn action_list(&self, network: NetworkId) -> Result<ActionSet, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ActionList { network, reply })?;
        response.await.map_err(|_| RuntimeError::Stopped)?
    }

    /// Replaces one Network's whole stored registration-action list.
    ///
    /// Wholesale rather than incremental, because the only thing an Operator means by "set"
    /// is "this is now the list". An append-only verb would make *removing* an action a
    /// second operation with its own semantics to get wrong.
    pub async fn set_actions(
        &self,
        network: NetworkId,
        actions: ActionSet,
    ) -> Result<usize, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ActionSet {
            network,
            actions,
            reply,
        })?;
        response.await.map_err(|_| RuntimeError::Stopped)?
    }

    /// Replaces exactly one phase of a Network's registration-action list.
    pub async fn set_action_phase(
        &self,
        network: NetworkId,
        phase: i2pr_irc_store::RegistrationActionPhase,
        actions: ActionSet,
    ) -> Result<usize, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ActionSetPhase {
            network,
            phase,
            actions,
            reply,
        })?;
        response.await.map_err(|_| RuntimeError::Stopped)?
    }

    /// Exports the whole durable configuration as a versioned snapshot.
    ///
    /// Reads the controller's own `records` map rather than the store: the controller is the
    /// only component that mutates durable Networks, so its map is the store, and an export
    /// taken from it cannot race an in-flight write.
    pub async fn export_config(&self) -> Result<ConfigSnapshot, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ExportConfig { reply })?;
        response.await.map_err(|_| RuntimeError::Stopped)?
    }

    /// Applies a validated snapshot, one Network at a time.
    ///
    /// Not transactional across Networks, and the [`ApplyOutcome`] says how far it got
    /// rather than implying a rollback. The store is one bounded worker behind a request
    /// queue: a multi-Network transaction would have to stay open across the owner restarts
    /// that each write causes, and that is a second writer on the one durable surface.
    /// Planning-then-applying is the honest boundary, and the guarantee that matters --
    /// nothing is written until the whole snapshot validated -- is held by the type.
    pub async fn import_config(
        &self,
        snapshot: ConfigSnapshot,
    ) -> Result<ApplyOutcome, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ImportConfig { snapshot, reply })?;
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

    pub async fn set_channel_activity_policy(
        &self,
        network: NetworkId,
        channel: String,
        activity: i2pr_irc_store::ChannelActivityPolicy,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::ChannelActivityPolicy {
            network,
            channel,
            activity,
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn watch_rules(
        &self,
        network: NetworkId,
    ) -> Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::WatchRules {
            network,
            rules: None,
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn set_watch_rules(
        &self,
        network: NetworkId,
        rules: Vec<i2pr_irc_store::WatchRule>,
    ) -> Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError> {
        if rules.len() > i2pr_irc_store::MAX_WATCH_RULES
            || rules.iter().any(|rule| rule.validate().is_err())
        {
            return Err(RuntimeError::InvalidConfig);
        }
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::WatchRules {
            network,
            rules: Some(rules),
            reply,
        })?;
        response.await.unwrap_or(Err(RuntimeError::Stopped))
    }

    pub async fn change_watch_rules(
        &self,
        network: NetworkId,
        change: WatchRuleChange,
    ) -> Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::WatchRuleChange {
            network,
            change,
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

    pub async fn buffer_retention(
        &self,
        network: NetworkId,
        kind: BufferKind,
        target: String,
        policy: Option<Option<BufferRetentionPolicy>>,
    ) -> Result<BufferRetentionPolicy, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(ControlRequest::BufferRetention {
            network,
            kind,
            target,
            policy,
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
                Some(request) = self.requests.recv() => {
                    self.dispatch(request).await;
                    // Republished after every request, not only after a mutation, so a
                    // `subscribe_status` watcher learns about owner-side movement (a
                    // session attaching, a generation ending, an advertisement changing)
                    // rather than only about the control-plane edits this controller
                    // happens to be handling. Bounded by the same request queue that
                    // already bounds control traffic.
                    self.publish();
                }
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
    /// Loads a Network's stored actions into the bounded runtime model.
    ///
    /// A stored set that breaches the runtime's ceilings is refused rather than truncated. A
    /// table written by a future build, or by an older one with looser bounds, must not be
    /// silently replayed as a prefix of what the Operator configured -- a bouncer that
    /// identifies on reconnect because it replayed the first of nine stored actions is
    /// worse than one that says nothing.
    async fn load_actions(&self, network: NetworkId) -> Result<ActionSet, RuntimeError> {
        let rows = self
            .catalog
            .store()
            .load_registration_actions(network)
            .await
            .map_err(map_store)?;
        let mut actions = Vec::with_capacity(rows.len());
        for row in rows {
            let kind = match row.kind {
                i2pr_irc_store::RegistrationActionKind::Mode => crate::action::ActionKind::Mode,
                i2pr_irc_store::RegistrationActionKind::Message => {
                    crate::action::ActionKind::Message
                }
            };
            actions.push(
                crate::action::RegistrationAction::from_parts(
                    kind,
                    row.target,
                    row.payload,
                    row.phase,
                )
                .map_err(|_| RuntimeError::InvalidConfig)?,
            );
        }
        ActionSet::new(actions).map_err(|_| RuntimeError::InvalidConfig)
    }

    /// Writes a Network's whole action list durably, reporting how many rows it stored.
    ///
    /// The count is compared against what was submitted rather than returned blindly: a
    /// conversion that dropped an action would otherwise report a smaller number than the
    /// Operator stored and leave them believing the missing one was configured.
    async fn store_actions(
        &self,
        network: NetworkId,
        actions: &ActionSet,
    ) -> Result<(), RuntimeError> {
        let mut rows = Vec::with_capacity(actions.len());
        for action in actions.actions() {
            rows.push(action.to_stored().ok_or(RuntimeError::InvalidConfig)?);
        }
        self.catalog
            .store()
            .save_registration_actions(network, &rows)
            .await
            .map_err(map_store)?;
        Ok(())
    }

    async fn store_watch_rules(
        &self,
        network: NetworkId,
        rules: Vec<i2pr_irc_store::WatchRule>,
    ) -> Result<Vec<i2pr_irc_store::WatchRule>, RuntimeError> {
        let store = self.catalog.store();
        if let Err(error) = store.replace_watch_rules(network, &rules).await {
            if error.commit_state() != i2pr_irc_store::CommitState::Unknown {
                return Err(map_store(error));
            }
            let durable = store.load_watch_rules(network).await.map_err(map_store)?;
            if durable != rules {
                return Err(map_store(error));
            }
        }
        if let Some(owner) = self.catalog.get(network) {
            owner.set_watch_rules(rules).await?;
        }
        store.load_watch_rules(network).await.map_err(map_store)
    }

    /// Builds the durable configuration as a secret-free snapshot.
    ///
    /// A record's credential becomes `None`, so an exported snapshot cannot carry one even
    /// by accident -- and, because import writes `sasl: None`, importing that snapshot over
    /// a store that *does* hold a credential cannot erase it either. That asymmetry is
    /// deliberate: an export is safe to paste anywhere, and applying one is safe to run
    /// anywhere, because neither direction touches a secret.
    async fn export_snapshot(&self) -> ConfigSnapshot {
        let mut networks: Vec<SnapshotNetwork> = Vec::with_capacity(self.records.len());
        for record in self.records.values() {
            // Read live rather than from a cache the controller does not keep. A count that
            // is wrong in the safe direction -- understated -- cannot mislead an Operator
            // into thinking an action is missing; one that is overstated would send them
            // looking for an action that is not there.
            let actions = self
                .catalog
                .store()
                .load_registration_actions(record.network)
                .await
                .unwrap_or_default();
            let action_count = actions.len() as u32;
            let mut action_phase_counts = [0_u32; 3];
            for action in &actions {
                let index = match action.phase {
                    i2pr_irc_store::RegistrationActionPhase::PreJoin => 0,
                    i2pr_irc_store::RegistrationActionPhase::PostJoin => 1,
                    i2pr_irc_store::RegistrationActionPhase::FallbackRecovery => 2,
                };
                action_phase_counts[index] = action_phase_counts[index].saturating_add(1);
            }
            networks.push(SnapshotNetwork {
                network: record.network,
                display_name: record.display_name.clone(),
                endpoint: record.endpoint.clone(),
                failover_group: record.failover_group.clone(),
                retain_existing_failover: false,
                nick: record.nick.clone(),
                username: record.username.clone(),
                realname: record.realname.clone(),
                auto_away: record.auto_away,
                keep_nick: record.keep_nick,
                desired_channels: record.desired_channels.clone(),
                action_count,
                action_phase_counts,
            });
        }
        networks.sort_by_key(|entry| entry.network);
        ConfigSnapshot { networks }
    }

    /// Applies a validated snapshot, one Network at a time.
    ///
    /// Stops at the first step it cannot complete and reports where. The two failure modes
    /// are distinguished because an Operator needs to act differently on them: a conflict
    /// means this snapshot came from a bouncer where `netid=N` named something else, and no
    /// retry helps; a write refusal means the store or the process was overloaded, and
    /// retrying the *same* plan is safe because every step is idempotent.
    async fn apply_snapshot(&mut self, snapshot: ConfigSnapshot) -> ApplyOutcome {
        let existing: Vec<NetworkRecord> = self.records.values().cloned().collect();
        let plan = config_snapshot::plan(&snapshot, &existing);
        let total = plan.steps.len();
        let mut applied = 0usize;
        let mut stopped_at = None;
        for step in plan.steps {
            match step {
                ImportStep::Conflict(network) => {
                    stopped_at = Some(network);
                    break;
                }
                ImportStep::Create(record) | ImportStep::Update(record) => {
                    let identity = record.network;
                    let outcome = if self.records.contains_key(&identity) {
                        self.change(record).await
                    } else {
                        self.create(record).await.map(|_| ())
                    };
                    match outcome {
                        Ok(()) => applied += 1,
                        Err(_) => {
                            stopped_at = Some(identity);
                            break;
                        }
                    }
                }
            }
        }
        ApplyOutcome {
            applied,
            remaining: total.saturating_sub(applied),
            stopped_at,
        }
    }

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
        .map(|owner| owner.with_command_pacer(self.catalog.command_pacer().clone()))
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

    /// Releases this Network's provider scope, once the owner task has ended.
    ///
    /// Called only after [`Self::stop_owner`] has joined, which is what makes it safe:
    /// no generation for this Network can still be inside `connect`, so the release
    /// cannot race a connect that would re-acquire the resources it is tearing down.
    ///
    /// Idempotency is the provider's half of this contract, not this component's: a
    /// release of an unknown or already-released Network succeeds, so retrying a delete
    /// after a timeout that may or may not have reached the adapter converges instead of
    /// wedging the Network permanently undeletable.
    async fn release_provider(&self, network: NetworkId) -> Result<(), RuntimeError> {
        match crate::timeout_bounded(PROVIDER_RELEASE_TIMEOUT, self.provider.release(network)).await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(RuntimeError::Provider(error)),
            Err(_) => Err(RuntimeError::Timeout),
        }
    }

    /// The process-wide values every Network's projection reads.
    ///
    /// Read live, at projection time, rather than taken from the published snapshot for the
    /// same reason the advertisement is: a diagnostic whose numbers lag the thing being
    /// diagnosed is worse than no diagnostic.
    fn diagnostic_inputs(&self) -> diagnostics::ProcessInputs {
        diagnostics::ProcessInputs {
            upstream: self.catalog.reconnect().diagnostics(),
            resources: self.catalog.resources().snapshot(),
            store_queue_depth: self.catalog.store_queue_depth(),
            controller_revision: self.revision,
        }
    }

    /// Projects one Network, pairing its live snapshot with the durable label it is listed
    /// under.
    ///
    /// The label comes from the durable record rather than the snapshot because the record
    /// is the Operator's own name for the Network, and a diagnostic that showed an internal
    /// identity instead would be the wrong one to copy into a support request.
    fn project_one(
        &self,
        network: NetworkId,
        inputs: &diagnostics::ProcessInputs,
    ) -> Option<diagnostics::NetworkDiagnostics> {
        let owner = self.live.get(&network)?;
        let name = self
            .records
            .get(&network)
            .map_or(String::new(), |record| record.display_name.clone());
        diagnostics::project_network(&owner.snapshot.borrow(), &name, inputs)
    }

    async fn dispatch(&mut self, request: ControlRequest) {
        match request {
            ControlRequest::Status { reply } => {
                // Recomputed rather than read from the last published value. Every
                // owner-owned field in a `ControlNetwork` -- phase, attached sessions, the
                // upstream advertisement -- is owned by a task this controller does not
                // drive, and none of them changes because a *control-plane* mutation
                // happened. Republishing only from `commit` therefore left `status()`
                // answering from a snapshot taken before the last client attached: a LIST
                // reported zero attached sessions for a Network with clients on it, and a
                // stale phase. Republishing here is what makes the answer an observation
                // rather than a memory.
                self.publish();
                let _ = reply.send(self.status.borrow().clone());
            }
            ControlRequest::Advertisement { network, reply } => {
                let advertisement = self
                    .live
                    .get(&network)
                    .map(|owner| owner.snapshot.borrow().advertisement.clone())
                    .unwrap_or_default();
                let _ = reply.send(advertisement);
            }
            ControlRequest::Diagnostics { network, reply } => {
                let inputs = self.diagnostic_inputs();
                let report = match network {
                    // A named Network reports only itself, and reports `NotFound` when it
                    // has no live owner. Returning an empty-but-successful report would
                    // let an Operator read "no problems" out of a typo.
                    Some(network) => match self.project_one(network, &inputs) {
                        Some(projected) => {
                            Ok(diagnostics::project_process(&inputs)).map(|mut process| {
                                process.networks = vec![projected];
                                process
                            })
                        }
                        None => Err(RuntimeError::UnknownNetwork),
                    },
                    None => {
                        let mut process = diagnostics::project_process(&inputs);
                        // Sorted so two reports of the same state are byte-identical: an
                        // Operator diffing two diagnostics reads the order as meaning.
                        let mut projected: Vec<_> = self
                            .live
                            .keys()
                            .filter_map(|network| self.project_one(*network, &inputs))
                            .collect();
                        projected.sort_by_key(|left| left.network);
                        process.networks = projected;
                        Ok(process)
                    }
                };
                let _ = reply.send(report);
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
            ControlRequest::ChannelActivityPolicy {
                network,
                channel,
                activity,
                reply,
            } => {
                let outcome = match self.live.get(&network) {
                    Some(owner) => {
                        owner
                            .handle
                            .set_channel_activity_policy(network, channel, activity)
                            .await
                    }
                    None => Err(RuntimeError::InvalidConfig),
                };
                if outcome.is_ok() {
                    self.reread().await;
                }
                let _ = reply.send(outcome);
            }
            ControlRequest::WatchRules {
                network,
                rules,
                reply,
            } => {
                let result = async {
                    if !self.records.contains_key(&network) {
                        return Err(RuntimeError::InvalidConfig);
                    }
                    if let Some(rules) = rules {
                        self.store_watch_rules(network, rules).await
                    } else {
                        self.catalog
                            .store()
                            .load_watch_rules(network)
                            .await
                            .map_err(map_store)
                    }
                }
                .await;
                let _ = reply.send(result);
            }
            ControlRequest::WatchRuleChange {
                network,
                change,
                reply,
            } => {
                let result = async {
                    if !self.records.contains_key(&network) {
                        return Err(RuntimeError::InvalidConfig);
                    }
                    let mut rules = self
                        .catalog
                        .store()
                        .load_watch_rules(network)
                        .await
                        .map_err(|error| crate::catalog::classify(error.kind()))?;
                    match change {
                        WatchRuleChange::Add(mut rule) => {
                            if rules.len() >= i2pr_irc_store::MAX_WATCH_RULES
                                || rule.network != network
                            {
                                return Err(RuntimeError::InvalidConfig);
                            }
                            rule.id = (1..=i2pr_irc_store::MAX_WATCH_RULES as u32)
                                .find(|id| rules.iter().all(|item| item.id != *id))
                                .ok_or(RuntimeError::InvalidConfig)?;
                            if let Some(target) = rule.target.as_deref() {
                                let buffer = self
                                    .catalog
                                    .store()
                                    .resolve_buffer(network, rule.kind, target)
                                    .await
                                    .map_err(map_store)?;
                                rule.buffer = Some(buffer.buffer);
                                rule.target = Some(buffer.target);
                            } else {
                                rule.buffer = None;
                            }
                            rule.validate().map_err(|_| RuntimeError::InvalidConfig)?;
                            rules.push(rule);
                        }
                        WatchRuleChange::Delete(id) => {
                            let old_len = rules.len();
                            rules.retain(|rule| rule.id != id);
                            if rules.len() == old_len {
                                return Err(RuntimeError::InvalidConfig);
                            }
                        }
                        WatchRuleChange::Clear => rules.clear(),
                    }
                    self.store_watch_rules(network, rules).await
                }
                .await;
                let _ = reply.send(result);
            }
            ControlRequest::CreateNext { candidate, reply } => {
                let _ = reply.send(self.create_next(candidate).await);
            }
            ControlRequest::Record { network, reply } => {
                let _ = reply.send(Ok(self.records.get(&network).cloned()));
            }
            ControlRequest::ActionList { network, reply } => {
                let _ = reply.send(self.load_actions(network).await);
            }
            ControlRequest::ActionSet {
                network,
                actions,
                reply,
            } => {
                let count = actions.len();
                let stored = self.store_actions(network, &actions).await;
                let _ = reply.send(stored.map(|()| count));
            }
            ControlRequest::ActionSetPhase {
                network,
                phase,
                actions,
                reply,
            } => {
                let result = async {
                    let current = self.load_actions(network).await?;
                    let merged = current
                        .actions()
                        .iter()
                        .filter(|action| action.phase != phase)
                        .cloned()
                        .chain(actions.actions().iter().cloned())
                        .collect();
                    let merged = ActionSet::new(merged).map_err(|_| RuntimeError::InvalidConfig)?;
                    let count = merged.len();
                    self.store_actions(network, &merged).await?;
                    Ok(count)
                }
                .await;
                let _ = reply.send(result);
            }
            ControlRequest::ExportConfig { reply } => {
                let _ = reply.send(Ok(self.export_snapshot().await));
            }
            ControlRequest::ImportConfig { snapshot, reply } => {
                let outcome = self.apply_snapshot(snapshot).await;
                self.commit();
                let _ = reply.send(outcome);
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
            ControlRequest::BufferRetention {
                network,
                kind,
                target,
                policy,
                reply,
            } => {
                let result = async {
                    if !self.records.contains_key(&network) {
                        return Err(RuntimeError::InvalidConfig);
                    }
                    let buffer = self
                        .catalog
                        .store()
                        .resolve_buffer(network, kind, &target)
                        .await
                        .map_err(|error| crate::catalog::classify(error.kind()))?
                        .buffer;
                    if let Some(policy) = policy {
                        self.catalog
                            .store()
                            .set_buffer_retention(buffer, policy.unwrap_or_default())
                            .await
                            .map_err(|error| crate::catalog::classify(error.kind()))?;
                        if let Some(owner) = self.catalog.get(network) {
                            owner
                                .sync_buffer_privacy(
                                    kind,
                                    target.clone(),
                                    policy.unwrap_or_default(),
                                )
                                .await?;
                        }
                    }
                    self.catalog
                        .store()
                        .get_buffer_retention(buffer)
                        .await
                        .map_err(|error| crate::catalog::classify(error.kind()))
                }
                .await;
                let _ = reply.send(result);
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

    /// Stops one Network, releases its provider scope, then forgets it durably.
    ///
    /// The order is the whole point. Stopping first means a failed step leaves a Network
    /// that is still configured but not running, which the Operator can retry. Releasing
    /// before the durable delete means a failed release also leaves it configured, so the
    /// retry still has a scope it can release. Forgetting the row first would leave a
    /// durable record whose owner is gone, whose router session is still live, and which
    /// nothing can ever address again.
    async fn delete(&mut self, network: NetworkId) -> Result<bool, RuntimeError> {
        if !self.records.contains_key(&network) {
            return Ok(false);
        }
        self.stop_owner(network).await?;
        // Republished before the release, for the same reason as `change`: the owner is
        // already stopped, and the snapshot must not say otherwise. Republished *before*
        // release too, so a Network that is quiesced but still configured reads as
        // stopped rather than as still owning a scope.
        self.commit();
        // The owner task has ended, so no generation can still be inside `connect` and
        // the release cannot race one. This happens before storage is touched: a release
        // that fails leaves the Network configured but stopped, which the Operator can
        // retry, and retrying calls release again. Forgetting the durable row first
        // would make the scope unreachable and therefore unreleasable forever.
        self.release_provider(network).await?;
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

    /// Stops every owner, awaits every task, then releases every provider scope.
    ///
    /// Each Network is removed from the live map before its task is awaited, so a
    /// shutdown that is itself observed cannot re-enter and start the same Network
    /// twice.
    async fn shutdown(&mut self) {
        let mut networks: Vec<NetworkId> = self.live.keys().copied().collect();
        // Durable rows with no live owner still hold a provider scope. They are released
        // here for the same reason the live ones are: "no owner" is not "no resources",
        // and a shutdown that only walked `live` would leave a router session behind for
        // every Network that was configured but stopped.
        networks.extend(
            self.records
                .keys()
                .copied()
                .filter(|network| !self.live.contains_key(network)),
        );
        networks.sort_unstable();
        networks.dedup();
        for network in networks {
            let _ = self.stop_owner(network).await;
            // Best-effort and reported nowhere: shutdown has no caller to return an error
            // to, and it must not abandon the remaining Networks over one release.
            let _ = self.release_provider(network).await;
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
                        // Bounded by the reviewed advertisement size, and cloned under the
                        // same borrow that reads the rest of the gauge. The borrow is
                        // dropped before `publish` returns, and nothing here writes to the
                        // snapshot.
                        advertisement: gauge.advertisement.clone(),
                    }
                }
                None => ControlNetwork {
                    network: *network,
                    display_name: record.display_name.clone(),
                    live: false,
                    phase: None,
                    attached_sessions: 0,
                    last_session_disposition: None,
                    // A Network with no live owner has negotiated nothing. Its clients are
                    // held in admission until one appears, and the owner replaces this
                    // value on attach, so the static list is the correct answer here and
                    // not a placeholder.
                    advertisement: crate::downstream::downstream_supported()
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect(),
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
        Kind::SchemaTooNew
        | Kind::ForeignDatabase
        | Kind::SqliteTooOld
        | Kind::KeyRejected
        | Kind::EncryptionUnavailable
        | Kind::Open => RuntimeError::Stopped,
        Kind::Sqlite => RuntimeError::InvalidConfig,
    }
}
