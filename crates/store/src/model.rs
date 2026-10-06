//! Durable record shapes shared by the store worker and runtime callers.
//!
//! Every string, collection, and batch here has an explicit ceiling, because all of
//! these values can ultimately be influenced by a remote peer through history
//! ingestion or client configuration.
use i2pr_irc_core::{
    BufferId, Casemapping, ClientId, HistoryEventId, I2pEndpoint, NetworkId, WallTime,
};
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
/// Maximum replay payload bytes retained for one event.
pub const MAX_HISTORY_PAYLOAD_BYTES: usize = 4096;
/// Maximum total replay payload bytes one bounded query may return.
pub const MAX_HISTORY_QUERY_BYTES: usize = 512 * 1024;
/// Maximum events removed by one retention operation.
pub const MAX_RETENTION_DELETE: usize = 4096;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferKind {
    Channel,
    Query,
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

/// A durable Network's complete configuration: everything needed to rebuild an
/// upstream owner after restart, and nothing that describes live observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkRecord {
    pub network: NetworkId,
    pub endpoint: I2pEndpoint,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub sasl: Option<(String, StoredSecret)>,
    pub desired_channels: Vec<String>,
}

impl NetworkRecord {
    /// Applies the same domain constraints as fresh configuration. Corrupt durable
    /// state is rejected rather than repaired into a plausible-looking default.
    pub fn validate(&self) -> Result<(), &'static str> {
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
        for channel in &self.desired_channels {
            if channel.len() < 2
                || channel.len() > MAX_TARGET_BYTES
                || !channel.starts_with(['#', '&'])
                || channel.bytes().any(|b| {
                    b.is_ascii_whitespace() || matches!(b, b',' | b':' | 0 | b'\r' | b'\n')
                })
            {
                return Err("desired channel shape");
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
    pub server_time: Option<WallTime>,
    pub msgid: Option<String>,
    pub direction: EventDirection,
    /// Stable protocol class retained for truthful replay decisions.
    pub event_class: String,
    pub payload: Vec<u8>,
}

/// One event accepted for durable append. `event` is assigned by the store so the
/// caller cannot choose its own canonical order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewHistoryEvent {
    pub network: NetworkId,
    pub buffer: BufferId,
    pub received_at: WallTime,
    pub server_time: Option<WallTime>,
    pub msgid: Option<String>,
    pub direction: EventDirection,
    pub event_class: String,
    /// Canonical protocol content for one event, *without* its line terminator.
    ///
    /// Rejecting embedded CR, LF, and NUL is what stops a stored payload from being
    /// split into extra lines by whatever downstream later writes it.
    pub payload: Vec<u8>,
}
impl NewHistoryEvent {
    pub fn validate(&self) -> Result<(), &'static str> {
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
            endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            sasl: None,
            desired_channels: vec!["#room".into()],
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
            |r: &mut NetworkRecord| r.desired_channels = vec!["room".to_owned()],
            |r: &mut NetworkRecord| r.desired_channels = vec!["#a,#b".to_owned()],
            |r: &mut NetworkRecord| r.nick = "b".repeat(65),
        ] {
            let mut subject = record();
            mutate(&mut subject);
            assert!(subject.validate().is_err(), "{subject:?}");
        }
    }

    #[test]
    fn desired_channel_and_identity_counts_are_bounded() {
        let mut subject = record();
        subject.desired_channels = (0..=MAX_DESIRED_CHANNELS)
            .map(|index| format!("#room{index}"))
            .collect();
        assert!(subject.validate().is_err());
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
