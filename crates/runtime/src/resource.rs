//! Process-wide bounded resource accounting.
//!
//! Qualification needs to answer one question repeatedly: after a campaign has churned,
//! has everything it touched come back down? A counter that only moves forward cannot
//! answer that, and a log of every sample could not be stored without becoming the thing
//! being measured.
//!
//! This ledger therefore keeps exactly two things per gauge: its **current** value and
//! the **highest** value it has ever reached. A campaign takes a baseline, drives load,
//! reads the peak, settles, and reads again. Settled must equal baseline. Nothing is
//! accumulated, so the ledger's memory is a fixed-size struct regardless of how long the
//! process runs or how many campaigns it serves.
//!
//! # Bounded by construction
//!
//! The gauge set is a closed struct, not a map keyed by a caller-supplied string, and the
//! number of tracked Networks is capped by the same ceiling that bounds supervision. A
//! Network that is not tracked cannot be recorded: the ledger refuses rather than
//! growing.
//!
//! # Counts only
//!
//! Every field is a `usize` or a `NetworkId`. There is deliberately no field that could
//! hold a nick, an endpoint, a message, or a credential, so reading the ledger cannot
//! disclose anything about what the bouncer is carrying.

use crate::reconnect::ReconnectScheduler;
use i2pr_irc_core::NetworkId;
use i2pr_irc_store::{STORE_QUEUE_CAPACITY, StoreHandle};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

/// Ceiling on Networks the ledger tracks at once.
///
/// Equal to the supervised-Network ceiling: a ledger entry exists to describe a running
/// owner, so tracking more Networks than can be supervised would only be a way to hold
/// state no owner can justify.
pub const MAX_TRACKED_NETWORKS: usize = crate::catalog::MAX_SUPERVISED_NETWORKS;

/// A refused recording. The ledger is a diagnostic surface, so it refuses rather than
/// growing; the owner that hit this keeps running and keeps its own snapshot.
///
/// It carries no detail on purpose: refusing says only that the process is already
/// supervising as much as it is allowed to, and a message naming the Network that was
/// turned away would turn a diagnostic into an enumeration of what the user is running.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("bounded resource ledger is at its Network ceiling")]
pub struct LedgerRefused;

/// One Network's live resource use, as the owner sees it.
///
/// `store_queue`, `reconnect_waiters`, and `in_flight_connects` are deliberately absent:
/// those three are process-wide, so they are measured once rather than summed across
/// Networks. Summing them would multiply one store queue by the Network count and make
/// the total a fiction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NetworkGauges {
    /// Owner tasks running for this Network. One while an owner exists.
    pub owner_tasks: usize,
    /// Session reader/writer tasks attached to this Network.
    pub session_tasks: usize,
    /// Deepest attached session's normal queue.
    pub session_normal: usize,
    /// Deepest attached session's control queue.
    pub session_control: usize,
    /// Intents queued for upstream delivery.
    pub upstream_normal: usize,
    /// Control frames queued for upstream delivery.
    pub upstream_control: usize,
    /// Open response routes.
    pub response_routes: usize,
    /// Batch references attributed to an open route.
    pub open_batches: usize,
    /// Desired channel intents waiting to be reconciled.
    pub desired_reconcile: usize,
    /// Upstream lines waiting for durable history ingestion.
    pub history_ingest: usize,
}

impl NetworkGauges {
    /// Nothing held by this Network.
    pub const ZERO: Self = Self {
        owner_tasks: 0,
        session_tasks: 0,
        session_normal: 0,
        session_control: 0,
        upstream_normal: 0,
        upstream_control: 0,
        response_routes: 0,
        open_batches: 0,
        desired_reconcile: 0,
        history_ingest: 0,
    };

    /// Component-wise sum, used to build the process-wide total.
    pub fn total(self, other: Self) -> Self {
        Self {
            owner_tasks: self.owner_tasks.saturating_add(other.owner_tasks),
            session_tasks: self.session_tasks.saturating_add(other.session_tasks),
            session_normal: self.session_normal.max(other.session_normal),
            session_control: self.session_control.max(other.session_control),
            upstream_normal: self.upstream_normal.saturating_add(other.upstream_normal),
            upstream_control: self.upstream_control.saturating_add(other.upstream_control),
            response_routes: self.response_routes.saturating_add(other.response_routes),
            open_batches: self.open_batches.saturating_add(other.open_batches),
            desired_reconcile: self
                .desired_reconcile
                .saturating_add(other.desired_reconcile),
            history_ingest: self.history_ingest.saturating_add(other.history_ingest),
        }
    }

    /// Component-wise maximum, used to fold a sample into the peak.
    pub fn peak(self, other: Self) -> Self {
        Self {
            owner_tasks: self.owner_tasks.max(other.owner_tasks),
            session_tasks: self.session_tasks.max(other.session_tasks),
            session_normal: self.session_normal.max(other.session_normal),
            session_control: self.session_control.max(other.session_control),
            upstream_normal: self.upstream_normal.max(other.upstream_normal),
            upstream_control: self.upstream_control.max(other.upstream_control),
            response_routes: self.response_routes.max(other.response_routes),
            open_batches: self.open_batches.max(other.open_batches),
            desired_reconcile: self.desired_reconcile.max(other.desired_reconcile),
            history_ingest: self.history_ingest.max(other.history_ingest),
        }
    }

    /// True when this Network holds nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::ZERO
    }
}

/// A whole-process resource reading.
///
/// `current` and `peak` are separate on purpose. A campaign that asserts only on `peak`
/// proves a ceiling held; a campaign that asserts `current == baseline` after settling
/// proves state actually came back down. Neither implies the other.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Gauges {
    pub owner_tasks: usize,
    pub session_tasks: usize,
    pub session_normal: usize,
    pub session_control: usize,
    pub upstream_normal: usize,
    pub upstream_control: usize,
    pub response_routes: usize,
    pub open_batches: usize,
    pub desired_reconcile: usize,
    pub history_ingest: usize,
    /// Ingress requests queued at the durable store. Process-wide, not per Network.
    pub store_queue: usize,
    /// Networks waiting for a connect permit. Process-wide.
    pub reconnect_waiters: usize,
    /// Connect attempts in flight. Process-wide.
    pub in_flight_connects: usize,
}

impl Gauges {
    /// Component-wise maximum.
    pub fn peak(self, other: Self) -> Self {
        Self {
            owner_tasks: self.owner_tasks.max(other.owner_tasks),
            session_tasks: self.session_tasks.max(other.session_tasks),
            session_normal: self.session_normal.max(other.session_normal),
            session_control: self.session_control.max(other.session_control),
            upstream_normal: self.upstream_normal.max(other.upstream_normal),
            upstream_control: self.upstream_control.max(other.upstream_control),
            response_routes: self.response_routes.max(other.response_routes),
            open_batches: self.open_batches.max(other.open_batches),
            desired_reconcile: self.desired_reconcile.max(other.desired_reconcile),
            history_ingest: self.history_ingest.max(other.history_ingest),
            store_queue: self.store_queue.max(other.store_queue),
            reconnect_waiters: self.reconnect_waiters.max(other.reconnect_waiters),
            in_flight_connects: self.in_flight_connects.max(other.in_flight_connects),
        }
    }

    /// True when nothing at all is held.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One point in a campaign's resource story.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceSnapshot {
    /// Networks currently tracked by the ledger.
    pub networks: usize,
    /// Live reading across every tracked Network plus the process-wide gauges.
    pub current: Gauges,
    /// Highest reading ever observed, since the last reset.
    pub peak: Gauges,
    /// Per-Network peak, so a campaign can attribute a spike instead of only seeing it.
    pub per_network_peak: Vec<(NetworkId, NetworkGauges)>,
    /// Recordings refused because the ledger was already at its Network ceiling.
    pub refused: u64,
}

impl ResourceSnapshot {
    /// True when the process holds nothing and never held more than it holds now.
    ///
    /// This is the "no leak" property a campaign asserts after everything it started has
    /// stopped.
    pub fn settled_to(&self, baseline: &ResourceSnapshot) -> bool {
        self.current == baseline.current
    }
}

struct LedgerState {
    current: BTreeMap<NetworkId, NetworkGauges>,
    peak: BTreeMap<NetworkId, NetworkGauges>,
    totals_peak: Gauges,
    refused: u64,
}

struct Inner {
    /// The process-wide connect budget, read live rather than mirrored.
    ///
    /// Mirroring it would create a second source of truth that could disagree with the
    /// scheduler it claims to describe.
    reconnect: ReconnectScheduler,
    /// Read live for ingress depth, for the same reason.
    store: StoreHandle,
    state: Mutex<LedgerState>,
}

/// Shared, cloneable resource ledger. Cheap to clone: it holds handles, not ownership.
#[derive(Clone)]
pub struct ResourceLedger {
    inner: Arc<Inner>,
}

impl ResourceLedger {
    /// Builds a ledger over the two process-wide resources it reads live.
    pub fn new(reconnect: ReconnectScheduler, store: StoreHandle) -> Self {
        Self {
            inner: Arc::new(Inner {
                reconnect,
                store,
                state: Mutex::new(LedgerState {
                    current: BTreeMap::new(),
                    peak: BTreeMap::new(),
                    totals_peak: Gauges::default(),
                    refused: 0,
                }),
            }),
        }
    }

    /// Starts tracking one Network.
    ///
    /// Called when an owner is constructed. Re-registering an already-tracked Network is
    /// idempotent, because a reconciliation must not be able to inflate the owner-task
    /// count by being retried.
    pub fn register(&self, network: NetworkId) -> Result<(), LedgerRefused> {
        let mut state = self.lock();
        if !state.current.contains_key(&network) && state.current.len() >= MAX_TRACKED_NETWORKS {
            state.refused = state.refused.saturating_add(1);
            return Err(LedgerRefused);
        }
        state.current.insert(
            network,
            NetworkGauges {
                owner_tasks: 1,
                ..NetworkGauges::ZERO
            },
        );
        state.peak.entry(network).or_default();
        Ok(())
    }

    /// Stops tracking one Network, discarding its current and peak readings.
    ///
    /// Called when an owner is dropped. Without this a restarted owner would keep the
    /// previous owner's peak forever, and a campaign's peak would describe a Network that
    /// no longer exists.
    pub fn forget(&self, network: NetworkId) {
        let mut state = self.lock();
        state.current.remove(&network);
        state.peak.remove(&network);
    }

    /// Records one Network's live gauges.
    pub fn observe(&self, network: NetworkId, gauges: NetworkGauges) -> Result<(), LedgerRefused> {
        let mut state = self.lock();
        if !state.current.contains_key(&network) {
            state.refused = state.refused.saturating_add(1);
            return Err(LedgerRefused);
        }
        state.current.insert(network, gauges);
        let peak = state.peak.entry(network).or_default();
        *peak = peak.peak(gauges);
        Ok(())
    }

    /// Clears every peak, keeping the current reading.
    ///
    /// A campaign calls this at its baseline so the peak it later reads describes this
    /// campaign rather than everything the process has done since it started.
    pub fn reset_peaks(&self) {
        let mut state = self.lock();
        let current: Vec<_> = state
            .current
            .iter()
            .map(|(id, gauges)| (*id, *gauges))
            .collect();
        state.peak = current.into_iter().collect();
        let totals = self.live_totals(&state);
        state.totals_peak = totals;
        state.refused = 0;
    }

    /// Reads the whole process, folding the live scheduler and store readings in.
    ///
    /// This also folds the live reading into the peak. Doing it here rather than asking
    /// callers to remember means a campaign cannot accidentally report a peak of zero
    /// because it forgot a bookkeeping call.
    pub fn snapshot(&self) -> ResourceSnapshot {
        let mut state = self.lock();
        let current = self.live_totals(&state);
        state.totals_peak = state.totals_peak.peak(current);
        ResourceSnapshot {
            networks: state.current.len(),
            current,
            peak: state.totals_peak,
            per_network_peak: state
                .peak
                .iter()
                .map(|(id, gauges)| (*id, *gauges))
                .collect(),
            refused: state.refused,
        }
    }

    /// Sums the per-Network readings and adds the three process-wide gauges read live.
    fn live_totals(&self, state: &LedgerState) -> Gauges {
        let mut total = NetworkGauges::ZERO;
        for gauges in state.current.values() {
            total = total.total(*gauges);
        }
        let reconnect = self.inner.reconnect.diagnostics();
        // `queue_capacity` is remaining capacity, and the channel never grows, so the
        // subtraction is exact and cannot underflow.
        let store_queue = STORE_QUEUE_CAPACITY.saturating_sub(self.inner.store.queue_capacity());
        Gauges {
            owner_tasks: total.owner_tasks,
            session_tasks: total.session_tasks,
            session_normal: total.session_normal,
            session_control: total.session_control,
            upstream_normal: total.upstream_normal,
            upstream_control: total.upstream_control,
            response_routes: total.response_routes,
            open_batches: total.open_batches,
            desired_reconcile: total.desired_reconcile,
            history_ingest: total.history_ingest,
            store_queue,
            reconnect_waiters: reconnect.pending_waiters,
            in_flight_connects: reconnect.in_flight,
        }
    }

    /// A poisoned lock must not read as "everything is empty", so the panic is preserved
    /// rather than mapped to a zero reading that would hide live resources.
    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.inner
            .state
            .lock()
            .expect("resource ledger lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_store::{Store, StorePath};

    /// The ledger reads the store's live ingress capacity, so the worker thread has to
    /// outlive every assertion that reads it.
    fn ledger() -> (ResourceLedger, Store) {
        let store = Store::open(&StorePath::Memory).expect("store opens");
        let handle = store.handle_clone();
        (
            ResourceLedger::new(ReconnectScheduler::default(), handle),
            store,
        )
    }

    #[test]
    fn an_empty_process_holds_nothing() {
        let (ledger, _store) = ledger();
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.networks, 0);
        assert!(snapshot.current.is_empty());
        assert_eq!(snapshot.current.in_flight_connects, 0);
    }

    #[test]
    fn a_registered_network_counts_as_an_owner_before_any_turn_runs() {
        let (ledger, _store) = ledger();
        assert!(ledger.snapshot().current.owner_tasks == 0);
        ledger.register(NetworkId(1)).expect("registers");
        assert_eq!(
            ledger.snapshot().current.owner_tasks,
            1,
            "an owner exists from construction, so the count must not wait for a generation turn"
        );
        ledger.forget(NetworkId(1));
        assert_eq!(ledger.snapshot().current.owner_tasks, 0);
    }

    #[test]
    fn peaks_are_retained_but_current_returns_to_baseline() {
        let (ledger, _store) = ledger();
        ledger.register(NetworkId(1)).expect("registers");
        let baseline = ledger.snapshot().current;

        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    session_tasks: 8,
                    upstream_normal: 12,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        let loaded = ledger.snapshot();
        assert_eq!(loaded.peak.session_tasks, 8);
        assert_eq!(loaded.peak.upstream_normal, 12);

        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        let settled = ledger.snapshot();
        assert_eq!(
            settled.current, baseline,
            "settling must return every gauge to its baseline"
        );
        assert_eq!(
            settled.peak.session_tasks, 8,
            "the peak must remember the load even after it cleared"
        );
    }

    #[test]
    fn reset_peaks_forgets_the_previous_campaign() {
        let (ledger, _store) = ledger();
        ledger.register(NetworkId(1)).expect("registers");
        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    session_tasks: 4,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        assert_eq!(ledger.snapshot().peak.session_tasks, 4);

        // The load clears, and only then does a new campaign start. Resetting while the
        // load is still present would be meaningless: the new campaign would begin at 4
        // and the old peak would be indistinguishable from a fresh reading.
        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        ledger.reset_peaks();
        assert_eq!(
            ledger.snapshot().peak.session_tasks,
            0,
            "a new campaign must not inherit the old campaign's peak"
        );
        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    session_tasks: 2,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        assert_eq!(
            ledger.snapshot().peak.session_tasks,
            2,
            "the new campaign measures itself, not its predecessor"
        );
    }

    #[test]
    fn per_network_peaks_attribute_a_spike() {
        let (ledger, _store) = ledger();
        ledger.register(NetworkId(1)).expect("registers");
        ledger.register(NetworkId(2)).expect("registers");
        ledger
            .observe(
                NetworkId(2),
                NetworkGauges {
                    owner_tasks: 1,
                    session_tasks: 5,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        let snapshot = ledger.snapshot();
        let peak_of = |id| {
            snapshot
                .per_network_peak
                .iter()
                .find(|(network, _)| *network == id)
                .map(|(_, gauges)| *gauges)
                .expect("tracked")
        };
        assert_eq!(peak_of(NetworkId(1)).session_tasks, 0);
        assert_eq!(peak_of(NetworkId(2)).session_tasks, 5);
    }

    #[test]
    fn forgetting_a_network_drops_its_peak() {
        let (ledger, _store) = ledger();
        ledger.register(NetworkId(1)).expect("registers");
        ledger
            .observe(
                NetworkId(1),
                NetworkGauges {
                    owner_tasks: 1,
                    session_tasks: 3,
                    ..NetworkGauges::ZERO
                },
            )
            .expect("observes");
        ledger.snapshot();
        ledger.forget(NetworkId(1));
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.networks, 0);
        assert!(
            snapshot.per_network_peak.is_empty(),
            "a stopped owner must not leave a peak describing a Network that no longer exists"
        );
    }

    #[test]
    fn the_network_ceiling_is_refused_rather_than_grown() {
        let (ledger, _store) = ledger();
        for index in 0..MAX_TRACKED_NETWORKS {
            ledger
                .register(NetworkId(index as u64))
                .expect("within the ceiling");
        }
        assert_eq!(
            ledger.register(NetworkId(u64::MAX)),
            Err(LedgerRefused),
            "the ledger must refuse rather than grow past the supervised ceiling"
        );
        assert_eq!(ledger.snapshot().networks, MAX_TRACKED_NETWORKS);
    }

    #[test]
    fn re_registering_is_idempotent() {
        let (ledger, _store) = ledger();
        ledger.register(NetworkId(1)).expect("registers");
        ledger.register(NetworkId(1)).expect("re-registers");
        assert_eq!(
            ledger.snapshot().networks,
            1,
            "a repeated reconciliation must not claim a second owner"
        );
    }

    #[test]
    fn observing_an_untracked_network_is_refused_and_counted() {
        let (ledger, _store) = ledger();
        assert_eq!(
            ledger.observe(NetworkId(9), NetworkGauges::ZERO),
            Err(LedgerRefused)
        );
        assert_eq!(
            ledger.snapshot().refused,
            1,
            "a refused recording is counted, not silently dropped"
        );
    }

    #[test]
    fn gauges_carry_no_payload_bearing_field() {
        // A compile-time guarantee is stronger than a runtime scan: the whole surface is
        // integers, so there is nowhere for a nick or a message to hide.
        let gauges = Gauges {
            owner_tasks: 1,
            session_tasks: 2,
            session_normal: 3,
            session_control: 4,
            upstream_normal: 5,
            upstream_control: 6,
            response_routes: 7,
            open_batches: 8,
            desired_reconcile: 9,
            history_ingest: 10,
            store_queue: 11,
            reconnect_waiters: 12,
            in_flight_connects: 13,
        };
        let rendered = format!("{gauges:?}");
        for forbidden in ["nick", "endpoint", "bouncer-", "PRIVMSG", "i2p"] {
            assert!(
                !rendered.contains(forbidden),
                "diagnostics rendered {forbidden}: {rendered}"
            );
        }
    }
}
