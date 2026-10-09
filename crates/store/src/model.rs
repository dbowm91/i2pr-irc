//! Durable record shapes shared by the store worker and runtime callers.
//!
//! Every string, collection, and batch here has an explicit ceiling, because all of
//! these values can ultimately be influenced by a remote peer through history
//! ingestion or client configuration.
use crate::{StoreError, StoreErrorKind};
use i2pr_irc_core::{
    BufferId, Casemapping, ClientId, HistoryEventId, I2pEndpoint, NetworkId, WallTime,
};
use i2pr_irc_wire::IrcTimestamp;
use zeroize::{Zeroize, Zeroizing};

/// Maximum durable Networks one catalog may hold.
pub const MAX_NETWORKS: usize = 64;
/// Maximum desired channels retained per Network.
pub const MAX_DESIRED_CHANNELS: usize = 128;
/// Maximum clients one operator may register.
pub const MAX_CLIENTS: usize = 64;
/// Maximum buffers per Network.
pub const MAX_BUFFERS_PER_NETWORK: usize = 1024;
/// Maximum events accepted in one append batch.
pub const MAX_HISTORY_BATCH: usize = 512;
/// Maximum events returned by one bounded history query.
pub const MAX_HISTORY_QUERY_EVENTS: usize = 512;
/// Ceiling on buffers one `TARGETS` query may return.
pub const MAX_RECENT_TARGETS: usize = 256;
/// Maximum replay payload bytes retained for one event.
pub const MAX_HISTORY_PAYLOAD_BYTES: usize = 4096;
/// Maximum total replay payload bytes one bounded query may return.
pub const MAX_HISTORY_QUERY_BYTES: usize = 512 * 1024;
/// Maximum events removed by one retention operation.
pub const MAX_RETENTION_DELETE: usize = 4096;
/// Maximum event age permitted for a per-buffer persistent history override.
pub const MAX_BUFFER_RETENTION_AGE_SECS: u64 = 31_536_000;
/// Maximum retained events permitted for a per-buffer persistent history override.
pub const MAX_BUFFER_RETENTION_EVENTS: u32 = 1_000_000;
/// Maximum retained payload bytes permitted for a per-buffer persistent history override.
pub const MAX_BUFFER_RETENTION_BYTES: u64 = 1_073_741_824;
/// Maximum local watch rules one Network may hold.
pub const MAX_WATCH_RULES: usize = 128;
/// Maximum bytes in one literal watch term.
pub const MAX_WATCH_TERM_BYTES: usize = 128;

/// Ceiling on one retained upstream `msgid`.
pub const MAX_HISTORY_MSGID_BYTES: usize = 64;
/// Maximum channel/query/peer identity bytes.
pub const MAX_TARGET_BYTES: usize = 200;
/// Maximum configured identity field bytes.
pub const MAX_IDENTITY_BYTES: usize = 256;
/// Maximum SASL username bytes retained durably.
pub const MAX_SASL_USERNAME_BYTES: usize = 256;
/// Maximum SASL password bytes retained durably.
pub const MAX_SASL_PASSWORD_BYTES: usize = 1024;

/// Where a durable database lives.
///
/// The bouncer never resolves a hostname or opens a generic socket, so this is a
/// filesystem location only. An in-memory database exists purely for deterministic
/// tests and can never be a production store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorePath {
    File(std::path::PathBuf),
    #[doc(hidden)]
    Memory,
}

/// Redacted restart-required credential. It is never rendered and never logged.
#[derive(Clone, Eq, PartialEq)]
pub struct StoredSecret(Zeroizing<String>);
impl StoredSecret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    /// Explicitly reads the secret for the one legitimate consumer: reconnect
    /// authentication. Callers must not place the result in diagnostics.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Debug for StoredSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoredSecret([redacted])")
    }
}
impl Drop for StoredSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Durable kind of a history buffer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BufferKind {
    Channel,
    Query,
}

/// Whether a buffer's events may outlive the current process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryPrivacyPolicy {
    /// Keep history in the encrypted or plaintext Store according to its configured mode.
    Persistent,
    /// Keep a bounded process-local ring; the contents are lost on restart.
    Ephemeral,
    /// Keep no message content or searchable derivation.
    NoHistory,
}

/// Per-buffer durable override. `None` inherits the profile default (persistent).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BufferRetentionPolicy {
    pub policy: Option<HistoryPrivacyPolicy>,
    pub max_age_secs: Option<u64>,
    pub max_events: Option<u32>,
    pub max_payload_bytes: Option<u64>,
}

/// Match shape for a local notification rule. Patterns are literal strings; the
/// runtime does not interpret user input as a regular expression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchMatchKind {
    Keyword,
    Sender,
}

/// Durable local-only rule. A missing target scopes it to all buffers on this
/// Network; otherwise target is a channel or query name interpreted with IRC casemap.
#[derive(Clone, Eq, PartialEq)]
pub struct WatchRule {
    pub id: u32,
    pub network: NetworkId,
    /// Stable privacy/history buffer scope. `None` means all buffers of `kind`.
    pub buffer: Option<BufferId>,
    pub kind: BufferKind,
    pub target: Option<String>,
    pub matcher: WatchMatchKind,
    pub term: String,
}

impl std::fmt::Debug for WatchRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchRule")
            .field("id", &self.id)
            .field("network", &self.network)
            .field("kind", &self.kind)
            .field("buffer", &self.buffer)
            .field("target", &"[redacted]")
            .field("matcher", &self.matcher)
            .field("term", &"[redacted]")
            .finish()
    }
}

impl WatchRule {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id == 0
            || self.id > MAX_WATCH_RULES as u32
            || self.term.is_empty()
            || self.term.len() > MAX_WATCH_TERM_BYTES
            || !self.term.bytes().all(|b| !b.is_ascii_control())
            || self.target.as_ref().is_some_and(|target| {
                target.is_empty()
                    || target.len() > MAX_TARGET_BYTES
                    || target
                        .bytes()
                        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
            })
        {
            return Err("watch rule shape");
        }
        Ok(())
    }
}

impl BufferRetentionPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .max_age_secs
            .is_some_and(|v| v == 0 || v > MAX_BUFFER_RETENTION_AGE_SECS)
            || self
                .max_events
                .is_some_and(|v| v == 0 || v > MAX_BUFFER_RETENTION_EVENTS)
            || self
                .max_payload_bytes
                .is_some_and(|v| v == 0 || v > MAX_BUFFER_RETENTION_BYTES)
        {
            return Err("buffer retention ceiling");
        }
        if self.policy != Some(HistoryPrivacyPolicy::Persistent)
            && (self.max_age_secs.is_some()
                || self.max_events.is_some()
                || self.max_payload_bytes.is_some())
        {
            return Err("retention ceilings require persistent policy");
        }
        Ok(())
    }
}

/// Direction/audience of a retained history event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventDirection {
    /// Observed from the upstream server.
    Inbound,
    /// Written by a local client toward upstream. Never proof of confirmed
    /// delivery, so it is retained with explicit unconfirmed semantics only.
    Outbound,
    /// Delivered by the bouncer itself (for example a synthesized server-time).
    Local,
}

/// One durable desired channel: what the Operator wants this Network to hold, and
/// whether that membership is presented downstream.
///
/// Desired membership and detached presentation are deliberately separate facts. A
/// detached channel is still joined upstream and still collects history; it is only
/// hidden from ordinary downstream live state. Storing the presentation decision here
/// rather than deriving it means it survives a restart and applies identically to
/// every attached session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredChannelRecord {
    pub target: String,
    /// Durable ordering of this channel among the Network's desired channels.
    ///
    /// Positions are strictly increasing across a Network's records, so order is
    /// total and a reloaded list means exactly what the saved one meant. Gaps are
    /// allowed: removing one channel must not renumber the others.
    pub position: usize,
    /// Hidden from ordinary downstream live state. Upstream membership is unaffected.
    pub detached: bool,
    /// Activity and reattachment policy while this channel is detached.
    pub activity: ChannelActivityPolicy,
}

/// What to relay to local sessions while a channel is detached.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RelayDetached {
    /// Suppress channel-scoped traffic, preserving current behavior.
    #[default]
    None,
    /// Relay only human-readable messages that mention the configured nick.
    Mentions,
    /// Relay all channel chat while keeping membership and state lines hidden.
    All,
}

/// Whether inbound channel activity automatically restores visibility.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReattachOn {
    /// Require an explicit Operator attach.
    #[default]
    Off,
    /// Reattach on any eligible inbound message.
    Message,
    /// Reattach on a human-readable nick mention.
    Mention,
}

/// Bounded per-channel activity policy; all defaults preserve existing behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChannelActivityPolicy {
    pub relay_detached: RelayDetached,
    pub reattach_on: ReattachOn,
    /// Inactivity duration in seconds; `None` disables timed detach.
    pub detach_after_secs: Option<u32>,
}

impl ChannelActivityPolicy {
    pub const MAX_DETACH_AFTER_SECS: u32 = 86_400;

    pub fn validate(self) -> Result<(), &'static str> {
        if self
            .detach_after_secs
            .is_some_and(|seconds| !(1..=Self::MAX_DETACH_AFTER_SECS).contains(&seconds))
        {
            return Err("detach-after duration");
        }
        Ok(())
    }
}

impl DesiredChannelRecord {
    /// A record at an explicit durable position.
    pub fn at(target: &str, position: usize, detached: bool) -> Self {
        Self {
            target: target.to_owned(),
            position,
            detached,
            activity: ChannelActivityPolicy::default(),
        }
    }

    /// An attached record placed after every channel already on the Network.
    ///
    /// This is the position the store itself would assign, so an in-memory record and
    /// a freshly reloaded one agree.
    pub fn after(existing: &[Self], target: &str) -> Self {
        let position = existing
            .iter()
            .map(|record| record.position + 1)
            .max()
            .unwrap_or(0);
        Self::at(target, position, false)
    }

    /// The same channel with its presentation decision replaced.
    pub fn with_detached(&self, detached: bool) -> Self {
        Self {
            target: self.target.clone(),
            position: self.position,
            detached,
            activity: self.activity,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.target.len() < 2
            || self.target.len() > MAX_TARGET_BYTES
            || !self.target.starts_with(['#', '&'])
            || self
                .target
                .bytes()
                .any(|b| b.is_ascii_whitespace() || matches!(b, b',' | b':' | 0 | b'\r' | b'\n'))
        {
            return Err("desired channel shape");
        }
        if self.position >= MAX_DESIRED_CHANNELS {
            return Err("desired channel position");
        }
        self.activity.validate()?;
        Ok(())
    }
}

/// Builds an attached desired-channel list with sequential positions.
///
/// This exists so configuration and test fixtures state an ordinary channel list
/// rather than each inventing its own position numbering.
pub fn attached_channels<S: AsRef<str>>(targets: &[S]) -> Vec<DesiredChannelRecord> {
    targets
        .iter()
        .enumerate()
        .map(|(index, target)| DesiredChannelRecord::at(target.as_ref(), index, false))
        .collect()
}

/// A durable Network's complete configuration: everything needed to rebuild an
/// upstream owner after restart, and nothing that describes live observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkRecord {
    pub network: NetworkId,
    /// Operator-chosen label this Network is listed under.
    ///
    /// Display only: it never participates in lookup, routing, or identity, and it is
    /// never derived from the endpoint or from any local path.
    pub display_name: String,
    pub endpoint: I2pEndpoint,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub sasl: Option<(String, StoredSecret)>,
    /// Durable channel intent, in durable order.
    pub desired_channels: Vec<DesiredChannelRecord>,
    /// Whether the bouncer goes away upstream when no active local session remains.
    ///
    /// Disabled by default and disabled by migration, because enabling it would make an
    /// existing Network emit new upstream `AWAY` traffic merely because the binary was
    /// upgraded. Turning this on is a deliberate Operator decision, not a consequence of
    /// running newer code.
    pub auto_away: bool,
    /// Whether the bouncer keeps trying to reclaim its configured nick upstream.
    ///
    /// Disabled by default for the same reason: reclaim writes `NICK` traffic upstream
    /// forever, and that traffic must not begin without being asked for.
    pub keep_nick: bool,
}

/// Longest accepted `display_name`. A name is a single protocol token, so it is
/// bounded like one.
pub const MAX_DISPLAY_NAME_BYTES: usize = 64;

/// The name a Network is listed under when none was ever chosen.
///
/// Derived only from the durable `NetworkId`, so it is stable across restarts and
/// carries no endpoint, nick, path, or machine detail. It is deliberately not the
/// endpoint or the nick: a display name is operator-facing text and must not leak the
/// bouncer's upstream identity into list output.
pub fn fallback_display_name(network: NetworkId) -> String {
    format!("network-{}", network.0)
}

/// Whether a `display_name` is a single bounded IRC token.
///
/// The name is interpolated into operator-facing numeric replies, so it must not be
/// able to carry a space, a parameter separator, or a prefix character that would
/// change how the surrounding reply parses.
pub fn valid_display_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_DISPLAY_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b':' | b',' | 0 | b'\r' | b'\n'))
}

impl NetworkRecord {
    /// Applies the same domain constraints as fresh configuration. Corrupt durable
    /// state is rejected rather than repaired into a plausible-looking default.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_display_name(&self.display_name) {
            return Err("display name");
        }
        if self.nick.is_empty() || self.nick.len() > 64 {
            return Err("nick length");
        }
        if !self
            .nick
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || is_nick_special(b))
            || !self
                .nick
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || is_nick_special(b) || b == b'-')
        {
            return Err("nick charset");
        }
        if self.username.is_empty() || self.username.len() > 64 {
            return Err("username length");
        }
        if !self
            .username
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b':')
        {
            return Err("username charset");
        }
        if self.realname.is_empty()
            || self.realname.len() > MAX_IDENTITY_BYTES
            || self
                .realname
                .bytes()
                .any(|b| b == 0 || b == b'\r' || b == b'\n')
        {
            return Err("realname");
        }
        if self.desired_channels.len() > MAX_DESIRED_CHANNELS {
            return Err("desired channel count");
        }
        // Order is total and casemap-unique: a duplicate would collide on the durable
        // primary key, and a non-increasing position would make the saved order depend
        // on how a list happened to be built rather than on what it meant.
        let mut previous: Option<usize> = None;
        let mut seen = std::collections::BTreeSet::new();
        for channel in &self.desired_channels {
            channel.validate()?;
            if previous.is_some_and(|value| channel.position <= value) {
                return Err("desired channel order");
            }
            previous = Some(channel.position);
            if !seen.insert(Casemapping::Rfc1459.fold(channel.target.as_bytes())) {
                return Err("desired channel duplicate");
            }
        }
        if let Some((user, password)) = &self.sasl {
            if user.is_empty()
                || user.len() > MAX_SASL_USERNAME_BYTES
                || !user.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err("sasl username");
            }
            let secret = password.expose();
            if secret.len() > MAX_SASL_PASSWORD_BYTES || secret.bytes().any(|b| b == 0) {
                return Err("sasl password");
            }
        }
        Ok(())
    }
}
fn is_nick_special(b: u8) -> bool {
    matches!(
        b,
        b'[' | b']' | b'\\' | b'`' | b'_' | b'^' | b'{' | b'|' | b'}'
    )
}

/// Result of creating a Network. The caller learns whether it supplied the durable
/// identity or the store allocated one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedNetwork {
    pub network: NetworkId,
    pub created: bool,
}

/// Compact catalog projection used to rebuild supervisors after restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkSummary {
    pub record: NetworkRecord,
    pub created: bool,
}

/// A durable client lineage. It survives attachment turnover; only the ephemeral
/// `SessionId` changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientRecord {
    pub client: ClientId,
    pub login: String,
}

/// A stable history buffer identity scoped to one Network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferRecord {
    pub buffer: BufferId,
    pub network: NetworkId,
    pub kind: BufferKind,
    /// Casemapped identity used for lookup. The display target is preserved
    /// separately so a later casemapping change cannot silently merge two buffers.
    pub canonical_key: Vec<u8>,
    pub target: String,
}
impl BufferRecord {
    pub fn lookup_key(kind: BufferKind, casemapping: Casemapping, target: &str) -> Vec<u8> {
        let mut key = vec![match kind {
            BufferKind::Channel => 0u8,
            BufferKind::Query => 1u8,
        }];
        key.extend(casemapping.fold(target.as_bytes()));
        key
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.target.is_empty() || self.target.len() > MAX_TARGET_BYTES {
            return Err("buffer target");
        }
        if self.canonical_key.len() > MAX_TARGET_BYTES + 1 {
            return Err("buffer canonical key");
        }
        Ok(())
    }
}

/// One durable history event. Canonical order is `event`, never `received_at` or any
/// server-supplied timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEvent {
    pub event: HistoryEventId,
    pub network: NetworkId,
    pub buffer: BufferId,
    pub received_at: WallTime,
    /// The upstream protocol timestamp exactly as the server stated it.
    ///
    /// `None` means the upstream sent no `server-time`; it is never synthesized
    /// from local time, because a fabricated upstream claim is a lie the client
    /// cannot detect. Ordering never depends on this value.
    pub server_time: Option<IrcTimestamp>,
    pub msgid: Option<String>,
    pub direction: EventDirection,
    /// Stable protocol class retained for truthful replay decisions.
    pub event_class: String,
    pub payload: Vec<u8>,
}

/// One buffer that has retained history, for a `CHATHISTORY TARGETS` reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentTarget {
    pub buffer: BufferId,
    pub target: String,
    /// Canonical protocol timestamp of the newest retained event in this buffer.
    ///
    /// Falls back to local receive time for events the upstream never stamped, so
    /// the value is always a usable resume point rather than absent.
    pub newest: IrcTimestamp,
    pub newest_event: HistoryEventId,
}

/// One event accepted for durable append. `event` is assigned by the store so the
/// caller cannot choose its own canonical order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewHistoryEvent {
    pub network: NetworkId,
    pub buffer: BufferId,
    pub received_at: WallTime,
    /// The upstream protocol timestamp exactly as the server stated it.
    ///
    /// `None` means the upstream sent no `server-time`; it is never synthesized
    /// from local time, because a fabricated upstream claim is a lie the client
    /// cannot detect. Ordering never depends on this value.
    pub server_time: Option<IrcTimestamp>,
    pub msgid: Option<String>,
    pub direction: EventDirection,
    pub event_class: String,
    /// Canonical protocol content for one event, *without* its line terminator.
    ///
    /// Rejecting embedded CR, LF, and NUL is what stops a stored payload from being
    /// split into extra lines by whatever downstream later writes it.
    pub payload: Vec<u8>,
    /// The bounded normalized fields the search side index stores for this event.
    ///
    /// Derived by the caller from the message it had already decoded, and written inside
    /// the append transaction. `None` means the event is not searchable, which is a
    /// statement about the event rather than a failed index write.
    pub search: Option<SearchFields>,
}
impl NewHistoryEvent {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(fields) = &self.search {
            fields.validate()?;
        }
        if self.payload.is_empty() || self.payload.len() > MAX_HISTORY_PAYLOAD_BYTES {
            return Err("history payload size");
        }
        if self.payload.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
            return Err("history payload framing");
        }
        if self.event_class.is_empty() || self.event_class.len() > 32 {
            return Err("history event class");
        }
        if let Some(msgid) = &self.msgid
            && (msgid.is_empty()
                || msgid.len() > 128
                || !msgid.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err("history msgid");
        }
        Ok(())
    }
}

/// Bounded local-relative history range. `None` on either bound means "from the
/// oldest/newest retained event".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryQueryBound {
    pub after: Option<HistoryEventId>,
    pub before: Option<HistoryEventId>,
    pub limit: usize,
}
impl HistoryQueryBound {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.limit == 0 || self.limit > MAX_HISTORY_QUERY_EVENTS {
            return Err("history query limit");
        }
        if let (Some(after), Some(before)) = (self.after, self.before)
            && after.0 >= before.0
        {
            return Err("history query range");
        }
        Ok(())
    }
}

/// One bounded history query against one Buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryQuery {
    pub buffer: BufferId,
    pub bound: HistoryQueryBound,
}

/// One bounded history window centred on an anchor event.
///
/// Two separate budgets rather than one, because the two sides are not
/// interchangeable: the caller knows how many events it wants on each side of the anchor,
/// and a single `limit` would force it to guess a split and then silently lose half of
/// what it asked for.
///
/// The anchor itself is always included when it is retained. A caller that did not get it
/// would have to reconstruct the page from two requests whose join is not guaranteed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryAround {
    pub buffer: BufferId,
    pub anchor: HistoryEventId,
    /// Events wanted strictly before the anchor.
    pub before: usize,
    /// Events wanted strictly after the anchor.
    pub after: usize,
}

impl HistoryAround {
    /// Rejects a window larger than the query ceiling, anchor included.
    ///
    /// The bound is on the *total*, not on each side: two halves each at the ceiling would
    /// be twice the work one `MAX_HISTORY_QUERY_EVENTS` answer is allowed to represent.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.before + self.after + 1 > MAX_HISTORY_QUERY_EVENTS {
            return Err("history around window");
        }
        if self.before > MAX_HISTORY_QUERY_EVENTS || self.after > MAX_HISTORY_QUERY_EVENTS {
            return Err("history around window");
        }
        Ok(())
    }
}

/// Append outcome. The store reports what it durably accepted so history loss is
/// observable rather than silent.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryAppendResult {
    pub first: Option<HistoryEventId>,
    pub last: Option<HistoryEventId>,
    pub accepted: usize,
}

/// Bounded retention request. The delete count is a ceiling, not a promise, so a
/// caller must poll again rather than assume one pass exhausted the backlog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionRequest {
    pub network: NetworkId,
    pub before: HistoryEventId,
    pub max_delete: usize,
}
impl RetentionRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_delete == 0 || self.max_delete > MAX_RETENTION_DELETE {
            return Err("retention delete ceiling");
        }
        Ok(())
    }
}

/// Retention outcome including the deterministic clamping of cursors and read
/// markers whose target event was removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionReport {
    pub deleted: usize,
    /// Newest removed event, or `None` when nothing was removed.
    pub last_deleted: Option<HistoryEventId>,
    /// Oldest event still retained for this Network.
    pub oldest_retained: Option<HistoryEventId>,
    pub cursors_clamped: usize,
    pub markers_clamped: usize,
    /// True when more rows remain eligible, so retention is incomplete.
    pub more_pending: bool,
}

/// Bounded store diagnostics. Never carries payloads, endpoints, or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreHealth {
    Starting,
    Ready,
    Failed,
    Stopped,
}

/// A durable client lineage record plus its assigned identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedClient {
    pub client: ClientId,
    pub created: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> NetworkRecord {
        NetworkRecord {
            network: NetworkId(1),
            display_name: fallback_display_name(NetworkId(1)),
            endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            sasl: None,
            desired_channels: attached_channels(&["#room"]),
            auto_away: false,
            keep_nick: false,
        }
    }

    #[test]
    fn valid_configuration_passes_domain_validation() {
        assert_eq!(record().validate(), Ok(()));
        let mut with_secret = record();
        with_secret.sasl = Some(("bot".into(), StoredSecret::new("hunter2".into())));
        assert_eq!(with_secret.validate(), Ok(()));
    }

    #[test]
    fn corrupt_identity_is_rejected_rather_than_defaulted() {
        for mutate in [
            (|r: &mut NetworkRecord| r.nick = "1bot".to_owned()) as fn(&mut NetworkRecord),
            |r: &mut NetworkRecord| r.nick = String::new(),
            |r: &mut NetworkRecord| r.username = "user name".to_owned(),
            |r: &mut NetworkRecord| r.realname = "bad\rname".to_owned(),
            |r: &mut NetworkRecord| r.realname = String::new(),
            |r: &mut NetworkRecord| {
                r.desired_channels = vec![DesiredChannelRecord::at("room", 0, false)]
            },
            |r: &mut NetworkRecord| {
                r.desired_channels = vec![DesiredChannelRecord::at("#a,#b", 0, false)]
            },
            |r: &mut NetworkRecord| r.nick = "b".repeat(65),
        ] {
            let mut subject = record();
            mutate(&mut subject);
            assert!(subject.validate().is_err(), "{subject:?}");
        }
    }

    #[test]
    fn desired_channel_order_and_identity_are_total() {
        let mut subject = record();
        // Two positions that do not strictly increase would make the stored order
        // depend on how a list happened to be built.
        subject.desired_channels = vec![
            DesiredChannelRecord::at("#a", 3, false),
            DesiredChannelRecord::at("#b", 3, false),
        ];
        assert_eq!(subject.validate(), Err("desired channel order"));
        subject.desired_channels = vec![
            DesiredChannelRecord::at("#a", 3, false),
            DesiredChannelRecord::at("#b", 1, false),
        ];
        assert_eq!(subject.validate(), Err("desired channel order"));
        // Rfc1459 folds `[` and `]`, so these two are the same channel twice.
        subject.desired_channels = vec![
            DesiredChannelRecord::at("#a[b]", 0, false),
            DesiredChannelRecord::at("#a{b}", 1, false),
        ];
        assert_eq!(subject.validate(), Err("desired channel duplicate"));
        subject.desired_channels = vec![
            DesiredChannelRecord::at("#a", 0, false),
            DesiredChannelRecord::at("#b", 7, true),
        ];
        assert_eq!(subject.validate(), Ok(()), "gaps in position are allowed");
    }

    #[test]
    fn a_new_record_is_placed_after_every_existing_one() {
        let existing = vec![DesiredChannelRecord::at("#a", 7, true)];
        assert_eq!(
            DesiredChannelRecord::after(&existing, "#b"),
            DesiredChannelRecord::at("#b", 8, false),
            "a new channel sorts after the last, whatever gaps exist below it"
        );
        assert_eq!(
            DesiredChannelRecord::after(&[], "#b"),
            DesiredChannelRecord::at("#b", 0, false)
        );
        // Detaching is a presentation change only: the durable order never moves.
        let attached = DesiredChannelRecord::at("#a", 7, false);
        assert_eq!(attached.with_detached(true).position, 7);
    }

    #[test]
    fn desired_channel_and_identity_counts_are_bounded() {
        let mut subject = record();
        subject.desired_channels = (0..=MAX_DESIRED_CHANNELS)
            .map(|index| DesiredChannelRecord::at(&format!("#room{index}"), index, false))
            .collect();
        assert_eq!(subject.validate(), Err("desired channel count"));
    }

    #[test]
    fn secrets_are_redacted_in_diagnostics() {
        let secret = StoredSecret::new("hunter2".into());
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        // `expose` is the deliberate, narrow escape hatch for reconnect
        // authentication, so it does return the value.
        assert_eq!(secret.expose(), "hunter2");
        // A durable record's Debug output must not leak the password either.
        let mut subject = record();
        subject.sasl = Some(("bot".into(), secret));
        assert!(!format!("{subject:?}").contains("hunter2"));
    }

    #[test]
    fn history_event_payload_and_class_are_bounded() {
        let mut event = NewHistoryEvent {
            network: NetworkId(1),
            buffer: BufferId(1),
            received_at: WallTime(1),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
            search: None,
        };
        assert_eq!(event.validate(), Ok(()));
        event.payload = vec![b'x'; MAX_HISTORY_PAYLOAD_BYTES + 1];
        assert!(event.validate().is_err());
        // A payload carrying its own terminator would let one stored event become
        // two replayed lines, so framing bytes are refused rather than stripped.
        event.payload = b"PRIVMSG #room :bad\ninjected".to_vec();
        assert!(event.validate().is_err());
        event.payload = b"PRIVMSG #room :ok".to_vec();
        event.event_class = String::new();
        assert!(event.validate().is_err());
    }

    #[test]
    fn bounded_queries_and_retention_reject_unbounded_requests() {
        assert!(
            HistoryQueryBound {
                after: None,
                before: None,
                limit: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            HistoryQueryBound {
                after: None,
                before: None,
                limit: MAX_HISTORY_QUERY_EVENTS + 1
            }
            .validate()
            .is_err()
        );
        // An inverted or empty range would return a misleading "complete" answer.
        assert!(
            HistoryQueryBound {
                after: Some(HistoryEventId(5)),
                before: Some(HistoryEventId(5)),
                limit: 10
            }
            .validate()
            .is_err()
        );
        assert!(
            RetentionRequest {
                network: NetworkId(1),
                before: HistoryEventId(1),
                max_delete: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            RetentionRequest {
                network: NetworkId(1),
                before: HistoryEventId(1),
                max_delete: MAX_RETENTION_DELETE + 1
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn buffer_lookup_keys_are_kind_scoped_and_casemapped() {
        let channel = BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, "#Room");
        let query = BufferRecord::lookup_key(BufferKind::Query, Casemapping::Rfc1459, "#Room");
        assert_ne!(
            channel, query,
            "a channel and a query are never the same buffer"
        );
        // RFC1459 folds `[ ] \ ^` onto `{ } | ~`. A backslash is therefore the same
        // channel as a pipe under RFC1459 and a different one under strict ASCII,
        // which is why the negotiated mapping must travel with a buffer lookup.
        let backslash =
            BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, "#r\\om");
        let pipe = BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, "#r|om");
        assert_eq!(backslash, pipe);
        assert_ne!(
            backslash,
            BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Ascii, "#r\\om")
        );
        assert_ne!(
            pipe,
            BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Ascii, "#r\\om")
        );
        // Case folding is part of identity, so a different case is still one buffer.
        assert_eq!(
            channel,
            BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, "#ROOM")
        );
    }
}

// ------------------------------------------------------------------- search

/// Ceiling on results one search returns.
///
/// A search is an Operator-initiated request against one Network, so this is generous but
/// finite: a caller can allocate against it without reading the database.
pub const MAX_SEARCH_RESULTS: usize = 256;

/// Ceiling on terms in one search.
///
/// Every term is ANDed, so this is also the ceiling on execution work per query: a search
/// cannot express "any of these fifty thousand words".
pub const MAX_SEARCH_TERMS: usize = 8;

/// Ceiling on one search term, in bytes.
pub const MAX_SEARCH_TERM_BYTES: usize = 64;

/// Ceiling on buffers one search may span.
///
/// A search spans one Network's buffers. This bounds how many it may name before the
/// caller is told the request was too broad, which is what stops a search from becoming
/// an unbounded cross-buffer scan.
pub const MAX_SEARCH_BUFFERS: usize = 16;

/// Ceiling on bytes one search term may carry back in a result.
pub const MAX_SEARCH_FIELD_BYTES: usize = 1024;

/// The bounded, normalized fields the side index stores for one searchable event.
///
/// Derived at ingestion from the decoded event, never re-parsed from a stored raw line at
/// query time: a search representation that only existed as opaque bytes would be exactly
/// the thing the plan forbids, because a raw line is protocol, not text.
///
/// Only stored `PRIVMSG`/`NOTICE` events carry these. `None` means the event is not
/// searchable, which is a statement about the event and not a failed index write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFields {
    /// Sender nickname, when the event carried a usable prefix.
    pub sender: String,
    /// The channel or target the event was addressed to.
    pub target: String,
    /// The message text.
    pub body: String,
}

impl SearchFields {
    /// Rejects anything oversized or carrying a NUL.
    ///
    /// CR and LF are rejected too: these fields reach a downstream frame, and a newline in
    /// stored search text would let a stored message become an extra line of protocol.
    fn validate(&self) -> Result<(), &'static str> {
        for field in [&self.sender, &self.target, &self.body] {
            if field.len() > MAX_SEARCH_FIELD_BYTES {
                return Err("search field size");
            }
            if field.bytes().any(|byte| matches!(byte, 0 | b'\r' | b'\n')) {
                return Err("search field content");
            }
        }
        Ok(())
    }
}

/// One validated search term.
///
/// A term is a bounded run of word characters and nothing else. That restriction is the
/// whole of the injection defence: with no quotes, no operator characters, and no
/// punctuation that FTS5 treats as syntax, there is nothing for client text to *be*.
///
/// The alternative — escaping a raw expression — would make every future FTS5 operator a
/// potential escape, because the escaping would have to be re-derived rather than
/// prevented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchTerm(String);

impl SearchTerm {
    /// Accepts a term only if every character is a word character.
    ///
    /// Unicode letters and digits are allowed because the index is tokenized with
    /// `unicode61`; everything else — including `_` and `-`, which FTS5 tokenizes away —
    /// is refused rather than quietly altered into something else.
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.is_empty() || raw.len() > MAX_SEARCH_TERM_BYTES {
            return Err("search term size");
        }
        if !raw.chars().all(char::is_alphanumeric) {
            return Err("search term content");
        }
        Ok(Self(raw.to_owned()))
    }

    /// The term as stored and as matched.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The FTS5 literal form.
    ///
    /// A term is already known to contain no quote and no operator, so the quoting is a
    /// belt-and-braces wrapper rather than the thing making it safe.
    pub fn as_fts_literal(&self) -> String {
        format!("\"{}\"", self.0)
    }
}

/// One bounded search over one Network's retained history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    /// The only Network this search may read.
    pub network: NetworkId,
    /// Buffers to search, empty meaning every retained buffer on the Network.
    pub buffers: Vec<BufferId>,
    /// Restrict to one sender nickname.
    pub sender: Option<String>,
    /// At or after this canonical server-time.
    ///
    /// A timestamp rather than an event id, because that is what a client can express. An
    /// event-id bound would have to be derived from a seek, which makes the boundary mean
    /// "the nearest event" rather than "this instant" — two different questions.
    pub after: Option<IrcTimestamp>,
    /// Strictly before this canonical server-time.
    ///
    /// The window is half-open, `[after, before)`, so a whole-millisecond timestamp can
    /// never fall in both halves or in neither.
    pub before: Option<IrcTimestamp>,
    /// Terms, ANDed.
    pub terms: Vec<SearchTerm>,
    pub limit: usize,
}

impl SearchQuery {
    /// Rejects an over-broad or unbounded request before any database work happens.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.limit == 0 || self.limit > MAX_SEARCH_RESULTS {
            return Err("search limit");
        }
        if self.buffers.len() > MAX_SEARCH_BUFFERS {
            return Err("search buffer count");
        }
        if self.terms.len() > MAX_SEARCH_TERMS {
            return Err("search term count");
        }
        if let (Some(after), Some(before)) = (self.after, self.before)
            && after >= before
        {
            return Err("search range");
        }
        if let Some(sender) = &self.sender
            && (sender.is_empty() || sender.len() > MAX_SEARCH_FIELD_BYTES)
        {
            return Err("search sender");
        }
        Ok(())
    }

    /// The FTS5 `MATCH` expression this query compiles to.
    ///
    /// Every term is quoted and joined with `AND`. Nothing a client typed is ever
    /// concatenated as syntax, so there is no expression to inject into.
    pub fn match_expression(&self) -> Option<String> {
        (!self.terms.is_empty()).then(|| {
            self.terms
                .iter()
                .map(SearchTerm::as_fts_literal)
                .collect::<Vec<_>>()
                .join(" AND ")
        })
    }
}

/// One search result: the event's identity plus its indexed text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub event: HistoryEventId,
    pub buffer: BufferId,
    pub sender: String,
    pub target: String,
    pub body: String,
}

/// How a `msgid=` reference resolved.
///
/// Explicit rather than "first match wins", because a duplicate `msgid` is a real
/// condition — a bouncer can legitimately hold the same upstream id in two buffers — and
/// silently picking one would answer a question the client did not ask.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MsgidLookup {
    /// Exactly one retained event carries this id.
    Unique(HistoryEventId),
    /// More than one does, ordered by `HistoryEventId` ascending.
    Ambiguous(Vec<HistoryEventId>),
    /// None does.
    Missing,
}

impl MsgidLookup {
    /// The resolved event, if there is exactly one.
    pub fn unique(self) -> Option<HistoryEventId> {
        match self {
            Self::Unique(event) => Some(event),
            _ => None,
        }
    }

    /// Whether this lookup found something but could not choose.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }
}

/// Ceiling on how many matches one `msgid=` reference reports when it is ambiguous.
///
/// Bounded so a pathological duplicate cannot make one reference read the whole journal.
/// A result at the ceiling is still reported as ambiguous, so truncation never turns
/// "ambiguous" into a false "unique".
pub const MAX_MSGID_AMBIGUOUS: usize = 8;

/// Where a reference fell relative to the retained window.
///
/// A `timestamp=` reference older than everything retained is not an error: it means
/// "before the beginning", and a client asking for history before that wants the oldest
/// page rather than an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NearestEvent {
    /// The newest event at or before the reference.
    pub before: Option<HistoryEventId>,
    /// The oldest event after the reference.
    pub after: Option<HistoryEventId>,
    /// The reference itself resolved to an event, rather than falling between two.
    pub exact: bool,
}

/// Durable kind of one stored registration action.
///
/// A closed set in the storage layer as well as the runtime: the column is `CHECK`-constrained
/// to these two spellings, so a row written by a future build is refused by SQLite rather
/// than read back here as an unknown kind that something downstream would have to guess at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationActionKind {
    /// `MODE` on the bouncer's own nick. The mode string is the target.
    Mode,
    /// A message to an explicitly configured service target.
    Message,
}

/// When one bounded registration action is replayed in a connection generation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RegistrationActionPhase {
    /// After registration and before desired channel joins.
    PreJoin,
    /// After desired channel joins. This is the migrated legacy behavior.
    #[default]
    PostJoin,
    /// After joins when this generation registered under a generated fallback nick.
    FallbackRecovery,
}

impl RegistrationActionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreJoin => "pre-join",
            Self::PostJoin => "post-join",
            Self::FallbackRecovery => "fallback-recovery",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pre-join" => Ok(Self::PreJoin),
            "post-join" => Ok(Self::PostJoin),
            "fallback-recovery" => Ok(Self::FallbackRecovery),
            _ => Err(StoreError::new(StoreErrorKind::Corrupt(
                "registration action phase",
            ))),
        }
    }
}

impl RegistrationActionKind {
    /// The spelling stored in the column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::Message => "message",
        }
    }

    /// Parses a stored spelling, refusing anything this build does not define.
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "mode" => Ok(Self::Mode),
            "message" => Ok(Self::Message),
            _ => Err(StoreError::new(StoreErrorKind::Corrupt(
                "registration action kind",
            ))),
        }
    }
}

/// One stored registration action, as it comes back out of the store.
///
/// The payload is a [`StoredSecret`] from the first instruction, not converted into a
/// `String` on the way. A stored action's text is the one place this bouncer holds text that
/// is expected to be secret, and a read path that produced an ordinary `String` would put one
/// step between that value and a `format!` somewhere else.
#[derive(Clone, Eq, PartialEq)]
pub struct StoredRegistrationAction {
    pub kind: RegistrationActionKind,
    pub phase: RegistrationActionPhase,
    /// The mode string, or the message target.
    pub target: String,
    /// The action text, always present in storage even for a `MODE`, where it is empty.
    pub payload: StoredSecret,
}

impl std::fmt::Debug for StoredRegistrationAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The kind and the target, because the Operator needs to see which service an
        // action addresses; never the payload, which may be that service's password.
        write!(
            f,
            "StoredRegistrationAction({:?} target={:?} payload=[redacted])",
            self.kind, self.target
        )
    }
}

/// Ceiling on stored registration actions per Network.
///
/// Enforced here as well as in the runtime model, because the table is a durable surface a
/// future build could write to directly. A read path that trusted the table's contents
/// without a ceiling would hand an unbounded list to a generation that then writes all of it
/// upstream on every reconnect.
pub const MAX_STORED_ACTIONS: usize = 8;

/// Ceiling on one stored action's payload.
pub const MAX_STORED_ACTION_PAYLOAD_BYTES: usize = 200;

/// Ceiling on one stored action's target.
pub const MAX_STORED_ACTION_TARGET_BYTES: usize = 64;
