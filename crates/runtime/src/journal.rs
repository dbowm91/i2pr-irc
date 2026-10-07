//! Durable bounded history journal for one Network.
//!
//! The journal is the runtime's adapter over the durable store. It owns:
//!
//! - `BufferId` resolution and its in-memory cache, so a wire target resolves to one
//!   stable buffer for as long as that identity remains unambiguous;
//! - a bounded ingestion path — incoming PRIVMSG/NOTICE after wire validation, in
//!   bounded batches, never blocking the network owner;
//! - bounded legacy backlog queries and monotonic cursor advance;
//! - bounded retention in bounded chunks.
//!
//! ## Local outgoing policy
//!
//! Local outgoing PRIVMSG/NOTICE are **omitted** until an upstream echo confirms them.
//! This plan ships before `echo-message` (M003-D), and a local socket write is not
//! evidence of upstream delivery: a disconnect can leave delivery ambiguous, so
//! storing it would be labeling an unconfirmed write as history. When M003-D adds
//! `echo-message`, the upstream echo becomes the canonical confirmed event instead.
//!
//! ## Casemapping
//!
//! Durable resolution uses the negotiated casemapping of the live generation. If a
//! re-resolution would map two existing durable targets onto the same identity, the
//! journal fails closed with a diagnostic rather than merging two histories.
use crate::{RuntimeError, catalog::classify};
use i2pr_irc_core::{Casemapping, NetworkId, WallClock, WallTime};
use i2pr_irc_store::{
    BufferId, BufferKind, EventDirection, HistoryEvent, HistoryEventId, HistoryQuery,
    HistoryQueryBound, MAX_HISTORY_BATCH, MAX_HISTORY_QUERY_BYTES, MAX_HISTORY_QUERY_EVENTS,
    NewHistoryEvent, RetentionReport, RetentionRequest, StoreHandle,
};
use i2pr_irc_wire::IrcTimestamp;
use i2pr_irc_wire::Message;
use std::collections::BTreeMap;

/// Bounded automatic backlog delivered to one legacy client, in events and bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BacklogCap {
    pub events: usize,
    pub bytes: usize,
}
impl BacklogCap {
    pub const fn new(events: usize, bytes: usize) -> Self {
        Self { events, bytes }
    }
    /// The default automatic backlog ceiling for a newly registered client.
    ///
    /// It is bounded in both dimensions on purpose: an event ceiling alone still
    /// admits an arbitrarily large payload per event.
    pub const DEFAULT: Self = Self::new(50, 64 * 1024);
}
impl Default for BacklogCap {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl BacklogCap {
    pub fn validate(self) -> Result<(), RuntimeError> {
        if self.events == 0
            || self.events > MAX_HISTORY_QUERY_EVENTS
            || self.bytes == 0
            || self.bytes > MAX_HISTORY_QUERY_BYTES
        {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(())
    }
}

/// Configurable bounded retention plus a hard safety ceiling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    /// Retain at most this many events per buffer in memory-facing queries.
    pub max_events_per_buffer: usize,
    /// Delete at most this many rows per pass, so no single transaction grows
    /// arbitrarily large.
    pub max_delete_per_pass: usize,
    /// Never pass more than this many times per retention cycle.
    pub max_passes_per_cycle: usize,
}
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_events_per_buffer: 2000,
            max_delete_per_pass: 512,
            max_passes_per_cycle: 4,
        }
    }
}
impl RetentionPolicy {
    pub fn validate(self) -> Result<(), RuntimeError> {
        if self.max_events_per_buffer == 0
            || self.max_delete_per_pass == 0
            || self.max_delete_per_pass > i2pr_irc_store::MAX_RETENTION_DELETE
            || self.max_passes_per_cycle == 0
        {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(())
    }
}

/// Bounded, non-secret health counters for one journal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JournalHealth {
    pub appended: u64,
    pub append_refused: u64,
    pub query_refused: u64,
    pub retention_passes: u64,
    pub retention_deleted: u64,
    pub cursors_advanced: u64,
    pub buffer_resolutions: u64,
    /// Events dropped because the store refused them. History loss must be visible.
    pub store_unavailable: bool,
}

/// One ingestion attempt's outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestOutcome {
    /// The event was durably accepted and has canonical order.
    Recorded { event: HistoryEventId },
    /// The line is not history-eligible and was intentionally not stored.
    Skipped,
    /// History degraded; delivery semantics are unaffected.
    StoreUnavailable,
}

/// Per-generation durable history journal.
pub struct HistoryJournal {
    network: i2pr_irc_core::NetworkId,
    store: StoreHandle,
    wall: Box<dyn WallClock>,
    casemapping: Casemapping,
    /// Resolved wire targets, so a target is resolved durably once and then cached.
    resolved: BTreeMap<(BufferKind, String), BufferId>,
    backlog: BacklogCap,
    retention: RetentionPolicy,
    health: JournalHealth,
    /// The nickname this bouncer owns, once registration has established one.
    ///
    /// Set by the owner rather than configured, because it is a property of the
    /// registration that just completed rather than of the journal.
    own_nick: Option<String>,
}

impl HistoryJournal {
    pub fn new(
        network: i2pr_irc_core::NetworkId,
        store: StoreHandle,
        wall: Box<dyn WallClock>,
        casemapping: Casemapping,
    ) -> Self {
        Self {
            network,
            store,
            wall,
            casemapping,
            resolved: BTreeMap::new(),
            backlog: BacklogCap::DEFAULT,
            retention: RetentionPolicy::default(),
            health: JournalHealth::default(),
            own_nick: None,
        }
    }

    /// Records which nickname this bouncer owns on its Network.
    ///
    /// This is what makes an upstream echo recognizable. A `PRIVMSG` or `NOTICE` that
    /// arrives *from the server* carrying this Network's own nick is the server echoing
    /// something a local client sent: it is confirmation, not conversation.
    ///
    /// The judgement needs no content matching. A message from our own nick arriving
    /// from upstream, in a channel we are in or in a query we opened, can only be the
    /// server's echo -- nobody else can speak as us, and a message from upstream
    /// claiming to be us is the server confirming what it accepted. Matching on the body
    /// instead would need a bounded pending-message set whose full contents would have to
    /// be compared, and a server that echoes with a `batch` reference or reformats the
    /// text would defeat it. The prefix is both sufficient and unbounded.
    ///
    /// `None` means the Network has not registered a nickname yet, and every event is
    /// then recorded as inbound. That is the conservative direction: it can mislabel one
    /// outgoing message, but it cannot label someone else's as ours.
    pub fn set_own_nick(&mut self, nick: Option<&str>) {
        self.own_nick = nick.map(str::to_owned);
    }

    /// The nickname this journal currently treats as its own, for change detection.
    pub fn own_nick(&self) -> Option<&str> {
        self.own_nick.as_deref()
    }

    pub fn with_limits(mut self, backlog: BacklogCap, retention: RetentionPolicy) -> Self {
        self.backlog = backlog;
        self.retention = retention;
        self
    }

    /// Resolves the durable `ClientId` for a login, creating the lineage once.
    ///
    /// Cursors and read markers are keyed by a durable lineage, so a journal must
    /// never invent one locally: an unpersisted lineage could not own playback state
    /// across a restart.
    pub async fn ensure_client(
        &self,
        login: &str,
    ) -> Result<i2pr_irc_core::ClientId, RuntimeError> {
        self.store
            .create_client(login)
            .await
            .map(|(client, _created)| client)
            .map_err(|error| classify(error.kind()))
    }

    pub fn health(&self) -> JournalHealth {
        self.health
    }
    pub fn backlog_cap(&self) -> BacklogCap {
        self.backlog
    }
    pub fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    /// Resolves one wire target to its stable durable buffer identity.
    ///
    /// The first resolution goes to the store, which is authoritative. Later hits are
    /// served from a bounded cache. If the store would map an existing target to a
    /// buffer whose stored identity is a *different* target, the ambiguity is refused
    /// rather than merged.
    pub async fn resolve_buffer(
        &mut self,
        kind: BufferKind,
        target: &str,
    ) -> Result<BufferId, RuntimeError> {
        let folded: String = self
            .casemapping
            .fold(target.as_bytes())
            .into_iter()
            .map(char::from)
            .collect();
        if let Some(buffer) = self.resolved.get(&(kind, folded.clone())) {
            self.health.buffer_resolutions = self.health.buffer_resolutions.saturating_add(1);
            return Ok(*buffer);
        }
        let record = self
            .store
            .resolve_buffer(self.network, kind, target)
            .await
            .map_err(|error| classify(error.kind()))?;
        // A durable identity may only be adopted when it agrees with the target the
        // caller asked about; otherwise two histories would silently merge.
        if record.kind != kind {
            self.health.store_unavailable = true;
            return Err(RuntimeError::AmbiguousBuffer(target.to_owned()));
        }
        if self.resolved.len() >= i2pr_irc_store::MAX_BUFFERS_PER_NETWORK {
            return Err(RuntimeError::QueueOverloaded);
        }
        self.resolved.insert((kind, folded), record.buffer);
        self.health.buffer_resolutions = self.health.buffer_resolutions.saturating_add(1);
        Ok(record.buffer)
    }

    /// Ingests one validated upstream line.
    ///
    /// Only inbound PRIVMSG/NOTICE are history-eligible in this plan. Everything else
    /// is skipped explicitly rather than being stored under a guess about intent.
    pub async fn ingest(
        &mut self,
        buffer: BufferId,
        message: &Message,
    ) -> Result<IngestOutcome, RuntimeError> {
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        if !matches!(command.as_str(), "PRIVMSG" | "NOTICE") {
            return Ok(IngestOutcome::Skipped);
        }
        // A message that arrived as a response to this bouncer's own command is
        // operational traffic, not conversation, and must not become history.
        if is_numeric(&message.command) {
            return Ok(IngestOutcome::Skipped);
        }
        let event = NewHistoryEvent {
            network: self.network,
            buffer,
            received_at: self.wall.now(),
            // Preserved exactly as the upstream stated it, including millisecond
            // precision and a leap second. `None` means the upstream sent none and
            // it is never invented from local time.
            server_time: message.server_time(),
            msgid: message.msgid().map(str::to_owned),
            direction: self.direction_of(message),
            event_class: command,
            payload: bounded_payload(message)?,
            // Derived here, from the message this function already decoded, rather than
            // left to the store to re-parse a stored line. The store could do it, and
            // would have to carry protocol knowledge to; deriving it here means the
            // search representation is produced exactly once, at the moment the message
            // was understood.
            search: Some(search_fields(message)),
        };
        self.append(vec![event]).await
    }

    /// The direction one ingested message belongs to.
    ///
    /// `Outgoing` means the upstream echoed a message a local client sent: it is the
    /// `echo-message` confirmation event, and the *only* point at which a local message
    /// becomes history. A local socket write is not evidence of upstream delivery -- the
    /// queue accepted the bytes, which says nothing about whether the server received or
    /// accepted them. Recording on the write would put a message in history that may never
    /// have reached anybody.
    ///
    /// The judgement needs no content matching. A message from our own nick arriving *from
    /// the server* can only be the server echoing what it accepted: nobody else can speak as
    /// us, and a server that echoes under our prefix is confirming delivery, not relaying a
    /// third party's words. Matching on the body instead would need a bounded set of pending
    /// message bodies to compare against, which is unbounded work and which a server that
    /// echoes with a `batch` reference or reformats the text would defeat anyway.
    ///
    /// A message the upstream never stamped stays inbound even when it carries our prefix.
    /// Without a `time` there is no ordering evidence that this is an echo rather than a
    /// late conversation line, and guessing wrong would put someone else's words in the
    /// Operator's own history.
    fn direction_of(&self, message: &Message) -> EventDirection {
        let Some(own) = self.own_nick.as_deref() else {
            return EventDirection::Inbound;
        };
        let Some(nick) = message.prefix_nick() else {
            return EventDirection::Inbound;
        };
        // Casemapped, because a nickname is casefolded by the protocol and the upstream
        // may echo a differently-cased spelling than the one registration used.
        let folded = self.casemapping.fold(nick.as_bytes());
        let expected = self.casemapping.fold(own.as_bytes());
        if folded == expected && message.server_time().is_some() {
            EventDirection::Outbound
        } else {
            EventDirection::Inbound
        }
    }

    /// Appends a bounded batch as one transaction, assigning canonical order.
    pub async fn append(
        &mut self,
        events: Vec<NewHistoryEvent>,
    ) -> Result<IngestOutcome, RuntimeError> {
        if events.is_empty() {
            return Ok(IngestOutcome::Skipped);
        }
        if events.len() > MAX_HISTORY_BATCH {
            self.health.append_refused = self.health.append_refused.saturating_add(1);
            return Err(RuntimeError::QueueOverloaded);
        }
        match self.store.append_history(&events).await {
            Ok(result) => {
                self.health.appended = self.health.appended.saturating_add(result.accepted as u64);
                Ok(match result.last {
                    Some(event) => IngestOutcome::Recorded { event },
                    None => IngestOutcome::Skipped,
                })
            }
            Err(error) => {
                // History degrades; it never falsifies network delivery and never
                // blocks control traffic.
                self.health.append_refused = self.health.append_refused.saturating_add(1);
                self.health.store_unavailable = true;
                let _ = error;
                Ok(IngestOutcome::StoreUnavailable)
            }
        }
    }

    /// Reads a bounded backlog for one client, oldest-to-newest by canonical order.
    pub async fn backlog(
        &self,
        client: i2pr_irc_core::ClientId,
        buffer: BufferId,
        cap: BacklogCap,
    ) -> Result<Vec<HistoryEvent>, RuntimeError> {
        cap.validate()?;
        let after = self
            .store
            .get_cursor(client, buffer)
            .await
            .map_err(|error| classify(error.kind()))?;
        // One bounded page is sufficient: the cap never exceeds the store's own query
        // ceiling, so paging here would only add round trips without adding safety.
        let page = self
            .store
            .query_history(&HistoryQuery {
                buffer,
                bound: HistoryQueryBound {
                    after,
                    before: None,
                    limit: cap.events.min(MAX_HISTORY_QUERY_EVENTS),
                },
            })
            .await
            .map_err(|error| classify(error.kind()))?;
        let mut remaining = cap.bytes;
        let mut out = Vec::new();
        for event in page {
            // An event that does not fit in the remaining budget is not delivered at
            // all. Delivering it anyway would overshoot the byte ceiling, and
            // truncating it would send a different message than the one retained.
            if event.payload.len() > remaining {
                break;
            }
            remaining -= event.payload.len();
            out.push(event);
            if out.len() >= cap.events {
                break;
            }
        }
        Ok(out)
    }

    /// Reads one explicit bounded range for one buffer, ignoring any cursor.
    ///
    /// An explicit `CHATHISTORY` request names its own range, so it must not be
    /// silently narrowed to "everything this client has not seen".
    pub async fn backlog_range(
        &self,
        buffer: BufferId,
        after: Option<HistoryEventId>,
        before: Option<HistoryEventId>,
        limit: usize,
    ) -> Result<Vec<HistoryEvent>, RuntimeError> {
        if limit == 0 || limit > MAX_HISTORY_QUERY_EVENTS {
            return Err(RuntimeError::InvalidConfig);
        }
        if let (Some(after), Some(before)) = (after, before)
            && after.0 >= before.0
        {
            // An inverted range would return a misleading "complete" answer.
            return Err(RuntimeError::InvalidConfig);
        }
        self.store
            .query_history(&HistoryQuery {
                buffer,
                bound: HistoryQueryBound {
                    after,
                    before,
                    limit,
                },
            })
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Monotonically advances one client's playback cursor.
    pub async fn advance_cursor(
        &mut self,
        client: i2pr_irc_core::ClientId,
        buffer: BufferId,
        to: HistoryEventId,
    ) -> Result<HistoryEventId, RuntimeError> {
        match self.store.advance_cursor(client, buffer, to).await {
            Ok(event) => {
                self.health.cursors_advanced = self.health.cursors_advanced.saturating_add(1);
                Ok(event)
            }
            Err(error) => {
                // A cursor must never be reported as advanced when it was not. The
                // caller re-reads durable state rather than assuming otherwise.
                Err(classify(error.kind()))
            }
        }
    }

    pub async fn cursor(
        &self,
        client: i2pr_irc_core::ClientId,
        buffer: BufferId,
    ) -> Result<Option<HistoryEventId>, RuntimeError> {
        self.store
            .get_cursor(client, buffer)
            .await
            .map_err(|error| classify(error.kind()))
    }

    pub async fn read_marker(
        &self,
        buffer: BufferId,
    ) -> Result<Option<HistoryEventId>, RuntimeError> {
        self.store
            .get_read_marker(buffer)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Buffers on this Network whose newest retained event falls in a time window.
    ///
    /// Bounded on rows and passed through to a store query that uses the buffer
    /// index, so a `TARGETS` request cannot become a full scan.
    pub async fn recent_targets(
        &self,
        lower: IrcTimestamp,
        upper: IrcTimestamp,
        limit: usize,
    ) -> Result<Vec<i2pr_irc_store::RecentTarget>, RuntimeError> {
        self.store
            .recent_targets(self.network, lower, upper, limit)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Resolves an upstream `msgid=` reference within this Network.
    ///
    /// Returns every match rather than the first, because a duplicate id is a real
    /// condition and picking the lowest `HistoryEventId` would answer a question the
    /// client did not ask while looking authoritative.
    pub async fn resolve_msgid(
        &self,
        network: NetworkId,
        msgid: &str,
    ) -> Result<i2pr_irc_store::MsgidLookup, RuntimeError> {
        self.store
            .resolve_msgid(network, msgid)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Reads one bounded window centred on an anchor event, for `AROUND`.
    ///
    /// Two indexed seeks rather than a scan, so the cost of asking for "the messages
    /// around this one" does not depend on how much history the buffer holds.
    pub async fn history_around(
        &self,
        request: &i2pr_irc_store::HistoryAround,
    ) -> Result<Vec<HistoryEvent>, RuntimeError> {
        self.store
            .history_around(request)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// The events bracketing one canonical protocol timestamp in this buffer.
    ///
    /// An indexed seek rather than a scan. `AROUND` and every timestamp reference go
    /// through here, so the cost of positioning in history does not depend on how much
    /// history the bouncer holds.
    pub async fn nearest_event(
        &self,
        buffer: BufferId,
        reference: IrcTimestamp,
    ) -> Result<i2pr_irc_store::NearestEvent, RuntimeError> {
        self.store
            .nearest_event(buffer, reference)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Runs one bounded search over this Network's retained history.
    pub async fn store_search(
        &self,
        query: &i2pr_irc_store::SearchQuery,
    ) -> Result<Vec<i2pr_irc_store::SearchHit>, RuntimeError> {
        self.store
            .search(query)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// The Network this journal belongs to.
    ///
    /// Exposed so an owner can compile a search against the Network it owns rather than
    /// against one named by the request. The scope is a property of the owner, and a
    /// caller that could supply its own would be a caller that could search another
    /// Network's history.
    pub fn network(&self) -> NetworkId {
        self.network
    }

    /// The canonical protocol timestamp for one retained event.
    ///
    /// Falls back to local receive time when the upstream never stamped the event, so
    /// a marker reply always carries a usable value.
    pub async fn event_timestamp(
        &self,
        buffer: BufferId,
        event: HistoryEventId,
    ) -> Result<Option<i2pr_irc_wire::IrcTimestamp>, RuntimeError> {
        // A one-event window bounded by the identity itself, so this stays a single
        // indexed lookup rather than a scan.
        let after = HistoryEventId(event.0.saturating_sub(1));
        let before = HistoryEventId(event.0.saturating_add(1));
        let events = self
            .backlog_range(buffer, Some(after), Some(before), 1)
            .await?;
        Ok(events.first().map(|row| {
            row.server_time
                .unwrap_or_else(|| crate::chathistory::local_timestamp(row.received_at))
        }))
    }

    pub async fn set_read_marker(
        &mut self,
        buffer: BufferId,
        to: HistoryEventId,
    ) -> Result<HistoryEventId, RuntimeError> {
        self.store
            .advance_read_marker(buffer, to)
            .await
            .map_err(|error| classify(error.kind()))
    }

    /// Runs one bounded retention cycle in bounded chunks.
    ///
    /// Retention never holds the store worker in one arbitrarily large transaction,
    /// and it reports whether more work remains so a caller can schedule the next
    /// cycle rather than looping without limit.
    pub async fn retain(
        &mut self,
        boundary: HistoryEventId,
    ) -> Result<RetentionReport, RuntimeError> {
        self.retention.validate()?;
        let mut total = RetentionReport::default();
        for _ in 0..self.retention.max_passes_per_cycle {
            let report = self
                .store
                .retain(&RetentionRequest {
                    network: self.network,
                    before: boundary,
                    max_delete: self.retention.max_delete_per_pass,
                })
                .await
                .map_err(|error| classify(error.kind()))?;
            self.health.retention_passes = self.health.retention_passes.saturating_add(1);
            self.health.retention_deleted = self
                .health
                .retention_deleted
                .saturating_add(report.deleted as u64);
            total.deleted = total.deleted.saturating_add(report.deleted);
            total.cursors_clamped = total.cursors_clamped.saturating_add(report.cursors_clamped);
            total.markers_clamped = total.markers_clamped.saturating_add(report.markers_clamped);
            total.oldest_retained = report.oldest_retained.or(total.oldest_retained);
            total.last_deleted = report.last_deleted.or(total.last_deleted);
            total.more_pending = report.more_pending;
            if !report.more_pending {
                break;
            }
        }
        Ok(total)
    }

    /// The receive timestamp used for events this journal accepts.
    pub fn now(&self) -> WallTime {
        self.wall.now()
    }
}

/// Derives the bounded search fields from a decoded message.
///
/// Every field is clamped by the store before it is written, so this returns what the
/// message said and lets the single clamping rule live in one place. A prefix the
/// protocol does not give us yields an empty sender rather than a guessed one: a search
/// that attributes a message to a nick nobody sent it from would be worse than one that
/// cannot filter by sender at all.
fn search_fields(message: &Message) -> i2pr_irc_store::SearchFields {
    i2pr_irc_store::SearchFields {
        sender: message.prefix_nick().map(str::to_owned).unwrap_or_default(),
        target: message
            .params
            .first()
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .unwrap_or_default(),
        body: message
            .params
            .get(1)
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .unwrap_or_default(),
    }
}

/// Builds the bounded canonical payload for one event.
///
/// The stored payload is the protocol content *without* its line terminator, so a
/// stored event can never be split into extra lines by whatever writes it later.
fn bounded_payload(message: &Message) -> Result<Vec<u8>, RuntimeError> {
    let encoded = message.encode().map_err(|_| RuntimeError::Protocol)?;
    let trimmed = encoded
        .strip_suffix(b"\r\n")
        .ok_or(RuntimeError::Protocol)?;
    if trimmed.is_empty() || trimmed.len() > i2pr_irc_store::MAX_HISTORY_PAYLOAD_BYTES {
        return Err(RuntimeError::QueueOverloaded);
    }
    Ok(trimmed.to_vec())
}

fn is_numeric(command: &[u8]) -> bool {
    !command.is_empty() && command.iter().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_core::{NetworkId, SystemWallClock};

    fn message(raw: &str) -> Message {
        Message::parse(raw.as_bytes()).expect("message parses")
    }

    #[tokio::test]
    async fn backlog_and_retention_caps_are_explicitly_bounded() {
        let cap = BacklogCap::new(0, 10);
        assert!(cap.validate().is_err());
        assert!(
            BacklogCap::new(MAX_HISTORY_QUERY_EVENTS + 1, 10)
                .validate()
                .is_err()
        );
        assert!(
            BacklogCap::new(10, MAX_HISTORY_QUERY_BYTES + 1)
                .validate()
                .is_err()
        );
        assert!(BacklogCap::DEFAULT.validate().is_ok());
    }

    #[tokio::test]
    async fn retention_policy_never_asks_for_an_unbounded_transaction() {
        let policy = RetentionPolicy::default();
        assert!(policy.validate().is_ok());
        assert!(
            RetentionPolicy {
                max_delete_per_pass: i2pr_irc_store::MAX_RETENTION_DELETE + 1,
                ..RetentionPolicy::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn numeric_replies_are_not_history() {
        assert!(is_numeric(b"353"));
        assert!(!is_numeric(b"PRIVMSG"));
        assert!(!is_numeric(b""));
    }

    #[test]
    fn payloads_are_stored_without_a_line_terminator() {
        let payload =
            bounded_payload(&message(":a!b@c PRIVMSG #room :hi\r\n")).expect("payload fits");
        assert_eq!(payload, b":a!b@c PRIVMSG #room :hi");
        assert!(!payload.ends_with(b"\r\n"));
    }

    #[test]
    fn a_health_projection_never_carries_a_payload_or_endpoint() {
        let health = JournalHealth::default();
        let rendered = format!("{health:?}");
        assert!(!rendered.contains('@'));
        assert!(rendered.contains("append_refused"));
    }

    #[tokio::test]
    async fn a_journal_reports_store_unavailable_instead_of_claiming_history() {
        let store =
            i2pr_irc_store::Store::open(&i2pr_irc_store::StorePath::Memory).expect("store opens");
        let mut journal = HistoryJournal::new(
            NetworkId(1),
            store.handle_clone(),
            Box::new(SystemWallClock),
            Casemapping::Rfc1459,
        );
        // With the store worker gone, ingestion degrades history only.
        store.shutdown().expect("store shuts down");
        let outcome = journal
            .ingest(BufferId(1), &message(":a!b@c PRIVMSG #room :hi\r\n"))
            .await
            .expect("ingestion degrades rather than propagating a storage failure");
        assert_eq!(outcome, IngestOutcome::StoreUnavailable);
        let health = journal.health();
        assert!(health.store_unavailable, "history loss must be visible");
        assert_eq!(health.appended, 0, "nothing may be reported as recorded");
    }
}
