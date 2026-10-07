//! Bounded owned SQLite storage worker and typed durable operations.
//!
//! This crate owns the durable substrate for the bouncer. Every SQLite call happens
//! on one owned worker thread behind an explicitly bounded request queue, so
//! blocking database work can never run on a network owner or downstream Tokio task.
//!
//! The store is deliberately *not* authority for live network state. It persists
//! DesiredState, identity, and history; it never restores observed membership,
//! connection generations, join attempts, or downstream sessions. Restart rebuilds
//! fresh supervisors and reconciles stored intent.
pub mod error;
pub mod model;
mod ops;
mod schema;
pub mod search;
pub use schema::{APPLICATION_ID, OpenDisposition, SCHEMA_VERSION};
#[doc(hidden)]
pub mod testing;
mod worker;

pub use error::{CommitState, StoreError, StoreErrorKind};
pub use model::{
    BufferKind, BufferRecord, ClientRecord, DesiredChannelRecord, EventDirection,
    HistoryAppendResult, HistoryAround, HistoryEvent, HistoryQuery, HistoryQueryBound,
    MAX_DISPLAY_NAME_BYTES, MsgidLookup, NearestEvent, NetworkRecord, NetworkSummary,
    NewHistoryEvent, RecentTarget, RetentionReport, RetentionRequest, SavedNetwork, SearchFields,
    SearchHit, SearchQuery, SearchTerm, StoreHealth, StorePath, StoredSecret, attached_channels,
    fallback_display_name,
};
// The explicit storage ceilings, re-exported so a caller bounds its own behavior with
// the same numbers the store enforces instead of duplicating (and drifting from) them.
pub use model::{
    MAX_BUFFERS_PER_NETWORK, MAX_CLIENTS, MAX_DESIRED_CHANNELS, MAX_HISTORY_BATCH,
    MAX_HISTORY_PAYLOAD_BYTES, MAX_HISTORY_QUERY_BYTES, MAX_HISTORY_QUERY_EVENTS, MAX_NETWORKS,
    MAX_RETENTION_DELETE, MAX_SEARCH_BUFFERS, MAX_SEARCH_FIELD_BYTES, MAX_SEARCH_RESULTS,
    MAX_SEARCH_TERM_BYTES, MAX_SEARCH_TERMS,
};
// The durable identities below are re-exported so a caller needs one import for the
// whole storage vocabulary and cannot accidentally mix them up with runtime types.
pub use i2pr_irc_core::{BufferId, ClientId, HistoryEventId, NetworkId};
// The single definition of "when in history this event sits". Re-exported because the
// runtime renders a `time=` tag from the same rule and must not carry its own copy.
pub use search::effective_time;
pub use worker::{STORE_BUSY_TIMEOUT_MS, STORE_QUEUE_CAPACITY, Store, StoreHandle};
