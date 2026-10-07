//! Typed durable operations executed by the owned store worker.
//!
//! Every function here runs on the worker thread against the one owned connection.
//! Runtime callers never see a `Connection`, a SQL string, or a closure.
use crate::{
    error::{CommitState, StoreError, StoreErrorKind},
    model::*,
};
use i2pr_irc_core::{BufferId, Casemapping, ClientId, HistoryEventId, I2pEndpoint, NetworkId};
use i2pr_irc_wire::IrcTimestamp;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// SQLite stores 64-bit integers; a value that cannot round-trip as one is corrupt
/// rather than something to coerce.
fn to_sql_id<T: Into<u64>>(id: T) -> Result<i64, StoreError> {
    i64::try_from(id.into())
        .map_err(|_| StoreError::new(StoreErrorKind::Corrupt("identifier range")))
}
fn from_sql_id(id: i64) -> Result<u64, StoreError> {
    u64::try_from(id).map_err(|_| StoreError::new(StoreErrorKind::Corrupt("identifier range")))
}

/// Wraps a SQLite failure.
///
/// The driver message is deliberately discarded: it can echo bound values, which
/// would put a payload or a credential into an error string. The typed kind plus the
/// explicit commit state carry everything a caller needs.
fn sql(_error: rusqlite::Error, commit: CommitState) -> StoreError {
    StoreError::mutating(StoreErrorKind::Sqlite, commit)
}

/// Reads one constrained 0/1 policy column.
fn policy_flag(value: i64, reason: &'static str) -> Result<bool, StoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StoreError::new(StoreErrorKind::Corrupt(reason))),
    }
}

pub(crate) fn load_networks(connection: &Connection) -> Result<Vec<NetworkRecord>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT n.network_id, n.endpoint, n.nick, n.username, n.realname, n.display_name,
                    n.auto_away, n.keep_nick, s.sasl_username, s.sasl_password
             FROM networks n
             LEFT JOIN network_secrets s ON s.network_id = n.network_id
             ORDER BY n.network_id",
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let rows = statement
        .query_map([], |row| {
            let network: i64 = row.get(0)?;
            let endpoint: String = row.get(1)?;
            let nick: String = row.get(2)?;
            let username: String = row.get(3)?;
            let realname: String = row.get(4)?;
            let display_name: String = row.get(5)?;
            // A constrained 0/1 column read back as anything else means the row was
            // written by something that does not honour this build's schema. It is
            // reported as corrupt rather than coerced: guessing which presence policy an
            // unrecognized value meant would start or stop upstream AWAY traffic on the
            // Operator's behalf.
            let auto_away: i64 = row.get(6)?;
            let keep_nick: i64 = row.get(7)?;
            let sasl_username: Option<String> = row.get(8)?;
            let sasl_password: Option<Vec<u8>> = row.get(9)?;
            Ok((
                network,
                endpoint,
                nick,
                username,
                realname,
                display_name,
                auto_away,
                keep_nick,
                sasl_username,
                sasl_password,
            ))
        })
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut records = Vec::new();
    for row in rows {
        let (
            network,
            endpoint,
            nick,
            username,
            realname,
            display_name,
            auto_away,
            keep_nick,
            sasl_username,
            sasl_password,
        ) = row.map_err(|error| sql(error, CommitState::RolledBack))?;
        let auto_away = policy_flag(auto_away, "auto away flag")?;
        let keep_nick = policy_flag(keep_nick, "keep nick flag")?;
        if records.len() >= MAX_NETWORKS {
            return Err(StoreError::new(StoreErrorKind::Corrupt(
                "network count exceeds ceiling",
            )));
        }
        let endpoint = I2pEndpoint::parse(&endpoint)
            .map_err(|_| StoreError::new(StoreErrorKind::Corrupt("endpoint")))?;
        let desired_channels =
            load_desired_channels(connection, from_sql_id(network).map(NetworkId)?)?;
        let sasl = match (sasl_username, sasl_password) {
            (Some(user), Some(password)) => Some((
                user,
                StoredSecret::new(String::from_utf8(password).map_err(|_| {
                    StoreError::new(StoreErrorKind::Corrupt("sasl password encoding"))
                })?),
            )),
            // A half-written secret row would mean silently authenticating as
            // nobody, so refuse rather than dropping the credential.
            (None, None) => None,
            _ => {
                return Err(StoreError::new(StoreErrorKind::Corrupt(
                    "partial secret row",
                )));
            }
        };
        let record = NetworkRecord {
            network: NetworkId(from_sql_id(network)?),
            endpoint,
            nick,
            username,
            realname,
            display_name,
            auto_away,
            keep_nick,
            sasl,
            desired_channels,
        };
        record
            .validate()
            .map_err(|reason| StoreError::new(StoreErrorKind::Corrupt(reason)))?;
        records.push(record);
    }
    Ok(records)
}

/// Loads one Network's durable channel intent, in durable order.
///
/// `detached` is stored as a constrained 0/1 integer, so any other value means the row
/// was written by something that does not honour this build's schema. It is reported
/// as corrupt rather than being coerced, because guessing which policy an unrecognized
/// value meant would silently show or hide a channel on the Operator's behalf.
fn load_desired_channels(
    connection: &Connection,
    network: NetworkId,
) -> Result<Vec<DesiredChannelRecord>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT target, position, detached FROM desired_channels
             WHERE network_id=?1 ORDER BY position, target",
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let rows = statement
        .query_map([to_sql_id(network.0)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut channels = Vec::new();
    for row in rows {
        let (target, position, detached) =
            row.map_err(|error| sql(error, CommitState::RolledBack))?;
        if channels.len() >= MAX_DESIRED_CHANNELS {
            return Err(StoreError::new(StoreErrorKind::Corrupt(
                "desired channel count exceeds ceiling",
            )));
        }
        let detached = match detached {
            0 => false,
            1 => true,
            _ => {
                return Err(StoreError::new(StoreErrorKind::Corrupt(
                    "desired channel detached flag",
                )));
            }
        };
        let position = usize::try_from(position)
            .map_err(|_| StoreError::new(StoreErrorKind::Corrupt("desired channel position")))?;
        let channel = DesiredChannelRecord {
            target,
            position,
            detached,
        };
        channel
            .validate()
            .map_err(|reason| StoreError::new(StoreErrorKind::Corrupt(reason)))?;
        channels.push(channel);
    }
    Ok(channels)
}

/// Creates or replaces a Network's durable configuration in one transaction.
pub(crate) fn save_network(
    connection: &mut Connection,
    record: &NetworkRecord,
) -> Result<SavedNetwork, StoreError> {
    record
        .validate()
        .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    let network = to_sql_id(record.network.0)?;
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let created: Option<i64> = transaction
        .query_row(
            "SELECT network_id FROM networks WHERE network_id=?1",
            [network],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if created.is_none() {
        let total: i64 = transaction
            .query_row("SELECT count(*) FROM networks", [], |row| row.get(0))
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        if total as usize >= MAX_NETWORKS {
            return Err(StoreError::new(StoreErrorKind::LimitExceeded(
                "network count",
            )));
        }
    }
    transaction
        .execute(
            "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username,
                     realname, display_name, auto_away, keep_nick)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(network_id) DO UPDATE SET
                endpoint=excluded.endpoint,
                endpoint_kind=excluded.endpoint_kind,
                nick=excluded.nick,
                username=excluded.username,
                realname=excluded.realname,
                display_name=excluded.display_name,
                auto_away=excluded.auto_away,
                keep_nick=excluded.keep_nick",
            params![
                network,
                record.endpoint.as_str(),
                kind_code(record.endpoint.kind()),
                record.nick,
                record.username,
                record.realname,
                record.display_name,
                i64::from(record.auto_away),
                i64::from(record.keep_nick),
            ],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    // Secrets are replaced wholesale so removing a credential cannot leave a stale
    // password row behind that a later load would silently adopt.
    transaction
        .execute("DELETE FROM network_secrets WHERE network_id=?1", [network])
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if let Some((user, password)) = &record.sasl {
        transaction
            .execute(
                "INSERT INTO network_secrets (network_id, sasl_username, sasl_password)
                 VALUES (?1, ?2, ?3)",
                params![network, user, password.expose().as_bytes()],
            )
            .map_err(|error| sql(error, CommitState::RolledBack))?;
    }
    transaction
        .execute(
            "DELETE FROM desired_channels WHERE network_id=?1",
            [network],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    for channel in &record.desired_channels {
        let key =
            BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, &channel.target);
        transaction
            .execute(
                "INSERT INTO desired_channels (network_id, casemap_key, target, position, detached)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    network,
                    key,
                    channel.target,
                    channel.position as i64,
                    i64::from(channel.detached),
                ],
            )
            .map_err(|error| sql(error, CommitState::RolledBack))?;
    }
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(SavedNetwork {
        network: record.network,
        created: created.is_none(),
    })
}

fn kind_code(kind: i2pr_irc_core::I2pEndpointKind) -> i64 {
    match kind {
        i2pr_irc_core::I2pEndpointKind::Hostname => 0,
        i2pr_irc_core::I2pEndpointKind::StandardBase32 => 1,
        i2pr_irc_core::I2pEndpointKind::ExtendedBase32 => 2,
        i2pr_irc_core::I2pEndpointKind::Destination => 3,
    }
}

pub(crate) fn remove_network(
    connection: &mut Connection,
    network: NetworkId,
) -> Result<bool, StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let removed = transaction
        .execute(
            "DELETE FROM networks WHERE network_id=?1",
            [to_sql_id(network.0)?],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    // Buffers, history, cursors, and markers cascade with the Network so a removed
    // Network cannot leave orphaned durable rows that later reattach to a reused id.
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(removed > 0)
}

/// Adds one durable desired channel. Returns false when it was already desired, so
/// the caller can distinguish a no-op from a fresh durable mutation.
pub(crate) fn add_desired_channel(
    connection: &mut Connection,
    network: NetworkId,
    channel: &str,
) -> Result<bool, StoreError> {
    validate_channel(channel)?;
    let key = BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, channel);
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let total: i64 = transaction
        .query_row(
            "SELECT count(*) FROM desired_channels WHERE network_id=?1",
            [to_sql_id(network.0)?],
            |row| row.get(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if total as usize >= MAX_DESIRED_CHANNELS {
        return Err(StoreError::new(StoreErrorKind::LimitExceeded(
            "desired channel count",
        )));
    }
    let position: i64 = transaction
        .query_row(
            "SELECT coalesce(max(position), -1) + 1 FROM desired_channels WHERE network_id=?1",
            [to_sql_id(network.0)?],
            |row| row.get(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let changed = transaction
        .execute(
            "INSERT OR IGNORE INTO desired_channels (network_id, casemap_key, target, position)
             VALUES (?1, ?2, ?3, ?4)",
            params![to_sql_id(network.0)?, key, channel, position],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(changed > 0)
}

pub(crate) fn remove_desired_channel(
    connection: &mut Connection,
    network: NetworkId,
    channel: &str,
) -> Result<bool, StoreError> {
    validate_channel(channel)?;
    let key = BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, channel);
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let changed = transaction
        .execute(
            "DELETE FROM desired_channels WHERE network_id=?1 AND casemap_key=?2",
            params![to_sql_id(network.0)?, key],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(changed > 0)
}

/// Records or clears a channel's detached presentation for one Network.
///
/// This changes only the presentation flag: the channel stays desired and stays joined
/// upstream, because detaching is a statement about what a local session is shown, not
/// about whether the bouncer belongs in the room. Returns false when the channel is not
/// one of this Network's desired channels, so the caller can distinguish "the policy is
/// now this" from "there was nothing to change" without reading the record back.
///
/// A single `UPDATE` is used rather than delete-then-insert so the durable position is
/// untouched: detaching and reattaching must never reorder a Network's channels.
pub(crate) fn set_desired_channel_detached(
    connection: &mut Connection,
    network: NetworkId,
    channel: &str,
    detached: bool,
) -> Result<bool, StoreError> {
    validate_channel(channel)?;
    let key = BufferRecord::lookup_key(BufferKind::Channel, Casemapping::Rfc1459, channel);
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let changed = transaction
        .execute(
            "UPDATE desired_channels SET detached=?3
             WHERE network_id=?1 AND casemap_key=?2",
            params![to_sql_id(network.0)?, key, i64::from(detached)],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(changed > 0)
}

fn validate_channel(channel: &str) -> Result<(), StoreError> {
    if channel.len() < 2
        || channel.len() > MAX_TARGET_BYTES
        || !channel.starts_with(['#', '&'])
        || channel
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, b',' | b':' | 0 | b'\r' | b'\n'))
    {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "desired channel shape",
        )));
    }
    Ok(())
}

/// Creates a durable client lineage or returns the existing one for `login`.
pub(crate) fn create_client(
    connection: &mut Connection,
    login: &str,
) -> Result<(ClientId, bool), StoreError> {
    if login.is_empty() || login.len() > 64 || login.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "client login",
        )));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if let Some(existing) = transaction
        .query_row(
            "SELECT client_id FROM clients WHERE login=?1",
            [login],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?
    {
        transaction
            .commit()
            .map_err(|error| sql(error, CommitState::Unknown))?;
        return Ok((ClientId(from_sql_id(existing)?), false));
    }
    let total: i64 = transaction
        .query_row("SELECT count(*) FROM clients", [], |row| row.get(0))
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if total as usize >= MAX_CLIENTS {
        return Err(StoreError::new(StoreErrorKind::LimitExceeded(
            "client count",
        )));
    }
    // Allocating the max existing id plus one keeps the identity monotonic while
    // staying inside the signed SQLite integer range.
    let next: i64 = transaction
        .query_row(
            "SELECT coalesce(max(client_id), 0) + 1 FROM clients",
            [],
            |row| row.get(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if next > i64::MAX / 2 {
        return Err(StoreError::new(StoreErrorKind::LimitExceeded(
            "client identity space",
        )));
    }
    transaction
        .execute(
            "INSERT INTO clients (client_id, login) VALUES (?1, ?2)",
            params![next, login],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok((ClientId(from_sql_id(next)?), true))
}

/// Resolves or creates a stable Buffer for one Network target.
pub(crate) fn resolve_buffer(
    connection: &mut Connection,
    network: NetworkId,
    kind: BufferKind,
    target: &str,
) -> Result<BufferRecord, StoreError> {
    if target.is_empty() || target.len() > MAX_TARGET_BYTES {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "buffer target",
        )));
    }
    // Durable identity uses the conservative pre-connection casemapping. A live
    // session re-resolves with its negotiated casemapping and fails closed rather
    // than merging two ambiguous histories.
    let key = BufferRecord::lookup_key(kind, Casemapping::Rfc1459, target);
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if let Some(existing) = transaction
        .query_row(
            "SELECT buffer_id FROM buffers WHERE network_id=?1 AND canonical_key=?2",
            params![to_sql_id(network.0)?, key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?
    {
        transaction
            .commit()
            .map_err(|error| sql(error, CommitState::Unknown))?;
        return Ok(BufferRecord {
            buffer: BufferId(from_sql_id(existing)?),
            network,
            kind,
            canonical_key: key,
            target: target.to_owned(),
        });
    }
    let total: i64 = transaction
        .query_row(
            "SELECT count(*) FROM buffers WHERE network_id=?1",
            [to_sql_id(network.0)?],
            |row| row.get(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    if total as usize >= MAX_BUFFERS_PER_NETWORK {
        return Err(StoreError::new(StoreErrorKind::LimitExceeded(
            "buffer count",
        )));
    }
    transaction
        .execute(
            "INSERT INTO buffers (network_id, kind, canonical_key, target) VALUES (?1, ?2, ?3, ?4)",
            params![to_sql_id(network.0)?, direction_code(kind), key, target],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let buffer: i64 = transaction.last_insert_rowid();
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(BufferRecord {
        buffer: BufferId(from_sql_id(buffer)?),
        network,
        kind,
        canonical_key: key,
        target: target.to_owned(),
    })
}

fn direction_code(kind: BufferKind) -> i64 {
    match kind {
        BufferKind::Channel => 0,
        BufferKind::Query => 1,
    }
}

fn event_direction_code(direction: EventDirection) -> i64 {
    match direction {
        EventDirection::Inbound => 0,
        EventDirection::Outbound => 1,
        EventDirection::Local => 2,
    }
}

fn event_direction_from(code: i64) -> Result<EventDirection, StoreError> {
    match code {
        0 => Ok(EventDirection::Inbound),
        1 => Ok(EventDirection::Outbound),
        2 => Ok(EventDirection::Local),
        _ => Err(StoreError::new(StoreErrorKind::Corrupt(
            "history direction code",
        ))),
    }
}

/// Appends one bounded batch in a single transaction, assigning canonical order.
pub(crate) fn append_history(
    connection: &mut Connection,
    events: &[NewHistoryEvent],
) -> Result<HistoryAppendResult, StoreError> {
    if events.is_empty() || events.len() > MAX_HISTORY_BATCH {
        return Err(StoreError::new(StoreErrorKind::LimitExceeded(
            "history batch size",
        )));
    }
    for event in events {
        event
            .validate()
            .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    }
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut result = HistoryAppendResult::default();
    for event in events {
        transaction
            .execute(
                "INSERT INTO history_events
                    (network_id, buffer_id, received_at, server_time, effective_time, msgid, direction, event_class, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    to_sql_id(event.network.0)?,
                    to_sql_id(event.buffer.0)?,
                    event.received_at.unix_seconds(),
                    event.server_time.map(|time| ServerTimeText(time.to_string())),
                    // Derived once, here, by the same rule the migration backfill and the
                    // replay path use. Computing it anywhere else would let the index and
                    // the rendered `time=` tag disagree about where an event sits.
                    crate::search::effective_time(event.server_time, event.received_at),
                    event.msgid,
                    event_direction_code(event.direction),
                    event.event_class,
                    event.payload,
                ],
            )
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        let assigned = from_sql_id(transaction.last_insert_rowid())?;
        // The index row is written inside the append transaction, so a retained event and
        // its search text are committed or absent together. There is no state in which a
        // message is retained but unsearchable, or searchable but not retained.
        if let Some(fields) = &event.search {
            crate::search::insert_search_row(
                &transaction,
                assigned as i64,
                fields.sender.clone(),
                fields.target.clone(),
                fields.body.clone(),
            )?;
        }
        if result.first.is_none() {
            result.first = Some(HistoryEventId(assigned));
        }
        result.last = Some(HistoryEventId(assigned));
        result.accepted += 1;
    }
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(result)
}

/// Canonical `server-time` text on its way into the v2 column.
struct ServerTimeText(String);
impl rusqlite::ToSql for ServerTimeText {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.0.as_str()))
    }
}

/// Reads one bounded history range for a Buffer in canonical local order.
/// Buffers on one Network whose newest retained event falls in a time window.
///
/// Bounded on both rows and time: a `TARGETS` request must not become an unbounded
/// scan of every buffer this Network has ever seen.
pub(crate) fn recent_targets(
    connection: &Connection,
    network: NetworkId,
    lower: &IrcTimestamp,
    upper: &IrcTimestamp,
    limit: usize,
) -> Result<Vec<RecentTarget>, StoreError> {
    if limit == 0 || limit > MAX_RECENT_TARGETS {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "recent-targets limit",
        )));
    }
    // Canonical protocol text, because that is what the column holds — see
    // [`canonical_time`]. The ordering check is therefore also a string comparison, which
    // is correct only because that text is fixed-width UTC.
    if upper < lower {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "recent-targets window",
        )));
    }
    let lower = canonical_time(lower);
    let upper = canonical_time(upper);
    let network = to_sql_id(network.0)?;
    // The newest event per buffer is found with the index, not a scan, and the
    // window is applied to that newest value so a client sees only buffers that
    // actually advanced inside it.
    let mut statement = connection
        .prepare(
            "SELECT b.buffer_id, b.target, h.event_id, h.server_time, h.received_at
             FROM buffers b
             JOIN history_events h ON h.event_id = (
                 SELECT MAX(e.event_id) FROM history_events e WHERE e.buffer_id = b.buffer_id
             )
             WHERE b.network_id = ?1
               AND h.effective_time >= ?2
               AND h.effective_time <= ?3
             ORDER BY h.event_id DESC
             LIMIT ?4",
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                network,
                lower,
                upper,
                to_sql_id(u64::try_from(limit).map_err(|_| StoreError::new(
                    StoreErrorKind::InvalidRequest("recent-targets limit")
                ))?)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    let mut found = Vec::new();
    for row in rows {
        let (buffer, target, event, server_time, received_at) =
            row.map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        // `server_time` is canonical text; the fallback keeps a reply usable for an
        // event the upstream never stamped.
        let newest = match server_time {
            Some(text) => IrcTimestamp::parse_str(&text).ok(),
            None => None,
        }
        .or_else(|| IrcTimestamp::from_unix_millis(received_at.saturating_mul(1_000)))
        .unwrap_or(IrcTimestamp::EPOCH);
        found.push(RecentTarget {
            buffer: BufferId(from_sql_id(buffer)?),
            target,
            newest,
            newest_event: HistoryEventId(from_sql_id(event)?),
        });
    }
    Ok(found)
}

pub(crate) fn query_history(
    connection: &Connection,
    query: &HistoryQuery,
) -> Result<Vec<HistoryEvent>, StoreError> {
    query
        .bound
        .validate()
        .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    let buffer = to_sql_id(query.buffer.0)?;
    // A fixed placeholder layout keeps one prepared shape per bound combination while
    // every value stays a bound parameter, so no peer-influenced text reaches SQL.
    let (clause, placeholders) = match (query.bound.after, query.bound.before) {
        (Some(_), Some(_)) => (" AND event_id > ?2 AND event_id < ?3", 4),
        (Some(_), None) => (" AND event_id > ?2", 3),
        (None, Some(_)) => (" AND event_id < ?2", 3),
        (None, None) => ("", 2),
    };
    let statement_text = format!(
        "SELECT event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload \
         FROM history_events WHERE buffer_id=?1{clause} ORDER BY event_id LIMIT ?{placeholders}"
    );
    let mut statement = connection
        .prepare(&statement_text)
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mapper = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Vec<u8>>(8)?,
        ))
    };
    let bound_limit = to_sql_id(query.bound.limit as u64)?;
    let rows = match (query.bound.after, query.bound.before) {
        (Some(after), Some(before)) => statement
            .query_map(
                params![
                    buffer,
                    to_sql_id(after.0)?,
                    to_sql_id(before.0)?,
                    bound_limit
                ],
                mapper,
            )
            .map_err(|error| sql(error, CommitState::RolledBack))?,
        (Some(after), None) => statement
            .query_map(params![buffer, to_sql_id(after.0)?, bound_limit], mapper)
            .map_err(|error| sql(error, CommitState::RolledBack))?,
        (None, Some(before)) => statement
            .query_map(params![buffer, to_sql_id(before.0)?, bound_limit], mapper)
            .map_err(|error| sql(error, CommitState::RolledBack))?,
        (None, None) => statement
            .query_map(params![buffer, bound_limit], mapper)
            .map_err(|error| sql(error, CommitState::RolledBack))?,
    };
    let mut events = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        let (event, network, buffer, received_at, server_time, msgid, direction, class, payload) =
            row.map_err(|error| sql(error, CommitState::RolledBack))?;
        bytes += payload.len();
        if bytes > MAX_HISTORY_QUERY_BYTES || events.len() >= MAX_HISTORY_QUERY_EVENTS {
            // Stop at the byte ceiling rather than returning a truncated page that
            // a caller could mistake for complete history.
            break;
        }
        events.push(history_event(
            event,
            network,
            buffer,
            received_at,
            server_time,
            msgid,
            direction,
            class,
            payload,
        )?);
    }
    Ok(events)
}

/// Decodes one `history_events` row into its typed event.
///
/// Shared by every read of that table so the column order and the corruption rules have
/// one definition. Two decoders would be two places for the timestamp policy to drift,
/// and a drift here is invisible until a replayed message carries the wrong time.
#[allow(clippy::too_many_arguments)]
fn history_event(
    event: i64,
    network: i64,
    buffer: i64,
    received_at: i64,
    server_time: Option<String>,
    msgid: Option<String>,
    direction: i64,
    event_class: String,
    payload: Vec<u8>,
) -> Result<HistoryEvent, StoreError> {
    let received_at = i2pr_irc_core::WallTime::from_unix_seconds(received_at)
        .ok_or_else(|| StoreError::new(StoreErrorKind::Corrupt("history receive timestamp")))?;
    // The v2 column carries canonical text. Anything that does not parse is
    // durable corruption, not a value to guess at: replaying a rewritten
    // timestamp would make the bouncer disagree with the upstream silently.
    let server_time = server_time
        .map(|value| {
            IrcTimestamp::parse_str(&value)
                .map_err(|_| StoreError::new(StoreErrorKind::Corrupt("history server timestamp")))
        })
        .transpose()?;
    Ok(HistoryEvent {
        event: HistoryEventId(from_sql_id(event)?),
        network: NetworkId(from_sql_id(network)?),
        buffer: BufferId(from_sql_id(buffer)?),
        received_at,
        server_time,
        msgid,
        direction: event_direction_from(direction)?,
        event_class,
        payload,
    })
}

/// Reads one bounded history window centred on an anchor event.
///
/// Two indexed seeks on the primary key, not a scan. The "before" side walks backwards
/// from the anchor and the "after" side walks forwards, each with its own budget; the
/// before-side is reversed afterwards so the result is one ascending run with the anchor
/// in the middle.
///
/// Reading a window of the newest events and filtering it in memory cannot produce this.
/// The anchor may be older than any such window, so the window would have to be grown
/// until it happens to contain the anchor — which is the unbounded scan this operation
/// exists to remove.
///
/// A missing anchor is not an error: the two sides are still returned, because a caller
/// paging either direction from a position it has not retained is exactly the case this
/// operation has to survive. The empty result therefore means "nothing at all on either
/// side", never "the anchor was pruned".
pub(crate) fn history_around(
    connection: &Connection,
    request: &HistoryAround,
) -> Result<Vec<HistoryEvent>, StoreError> {
    request
        .validate()
        .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    let buffer = to_sql_id(request.buffer.0)?;
    let anchor = to_sql_id(request.anchor.0)?;

    let before = history_window(
        connection,
        "SELECT event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload \
         FROM history_events WHERE buffer_id=?1 AND event_id < ?2 ORDER BY event_id DESC LIMIT ?3",
        buffer,
        anchor,
        request.before,
    )?;
    let mut events = Vec::with_capacity(before.len() + request.after + 1);
    events.extend(before.into_iter().rev());

    events.extend(history_window(
        connection,
        "SELECT event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload \
         FROM history_events WHERE buffer_id=?1 AND event_id > ?2 ORDER BY event_id ASC LIMIT ?3",
        buffer,
        anchor,
        request.after,
    )?);

    // The anchor is read separately rather than inferred from the two sides: it is the
    // one event the caller is guaranteed, and a page that silently omitted it would leave
    // the client unable to tell where in the conversation it is standing.
    if let Some(centre) = history_window_by_anchor(connection, buffer, anchor)? {
        let position = events
            .iter()
            .position(|event| event.event >= centre.event)
            .unwrap_or(events.len());
        events.insert(position, centre);
    }
    Ok(events)
}

/// Reads the anchor event itself.
///
/// Separate from [`history_window`] because its statement binds two placeholders rather
/// than three. One bound too many is a driver-level error, so the two shapes are two
/// functions rather than one with a parameter that is sometimes ignored.
fn history_window_by_anchor(
    connection: &Connection,
    buffer: i64,
    anchor: i64,
) -> Result<Option<HistoryEvent>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload \
             FROM history_events WHERE buffer_id=?1 AND event_id = ?2",
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut rows = statement
        .query_map(params![buffer, anchor], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Vec<u8>>(8)?,
            ))
        })
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    match rows.next() {
        Some(row) => {
            let (
                event,
                network,
                buffer_id,
                received_at,
                server_time,
                msgid,
                direction,
                class,
                payload,
            ) = row.map_err(|error| sql(error, CommitState::RolledBack))?;
            history_event(
                event,
                network,
                buffer_id,
                received_at,
                server_time,
                msgid,
                direction,
                class,
                payload,
            )
            .map(Some)
        }
        None => Ok(None),
    }
}

/// Runs one bounded directional read of `history_events`.
///
/// The order is the statement's, not this function's: the caller reverses what it needs.
/// Deciding order here would mean a second statement shape for the same read, and the
/// order is exactly what differs between the two sides of an anchor.
fn history_window(
    connection: &Connection,
    text: &str,
    buffer: i64,
    bound: i64,
    limit: usize,
) -> Result<Vec<HistoryEvent>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = limit.min(MAX_HISTORY_QUERY_EVENTS);
    let mut statement = connection
        .prepare(text)
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let rows = statement
        .query_map(params![buffer, bound, to_sql_id(limit as u64)?], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Vec<u8>>(8)?,
            ))
        })
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut events = Vec::with_capacity(limit);
    for row in rows {
        let (event, network, buffer_id, received_at, server_time, msgid, direction, class, payload) =
            row.map_err(|error| sql(error, CommitState::RolledBack))?;
        events.push(history_event(
            event,
            network,
            buffer_id,
            received_at,
            server_time,
            msgid,
            direction,
            class,
            payload,
        )?);
    }
    Ok(events)
}

/// Reads a client's playback cursor for one Buffer.
pub(crate) fn get_cursor(
    connection: &Connection,
    client: ClientId,
    buffer: BufferId,
) -> Result<Option<HistoryEventId>, StoreError> {
    connection
        .query_row(
            "SELECT event_id FROM client_cursors WHERE client_id=?1 AND buffer_id=?2",
            params![to_sql_id(client.0)?, to_sql_id(buffer.0)?],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?
        .map(|value| from_sql_id(value).map(HistoryEventId))
        .transpose()
}

/// Moves a playback cursor forward only. A repeated or stale advance is a no-op, so
/// a late acknowledgement cannot rewind an already-advanced cursor.
pub(crate) fn advance_cursor(
    connection: &mut Connection,
    client: ClientId,
    buffer: BufferId,
    to: HistoryEventId,
) -> Result<HistoryEventId, StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT event_id FROM client_cursors WHERE client_id=?1 AND buffer_id=?2",
            params![to_sql_id(client.0)?, to_sql_id(buffer.0)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let current = existing.map(from_sql_id).transpose()?;
    let next = match current {
        Some(current) if current >= to.0 => HistoryEventId(current),
        _ => HistoryEventId(to.0),
    };
    transaction
        .execute(
            "INSERT INTO client_cursors (client_id, buffer_id, event_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(client_id, buffer_id) DO UPDATE SET event_id=excluded.event_id",
            params![
                to_sql_id(client.0)?,
                to_sql_id(buffer.0)?,
                to_sql_id(next.0)?
            ],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(next)
}

/// Reads the operator read marker for one Buffer.
pub(crate) fn get_read_marker(
    connection: &Connection,
    buffer: BufferId,
) -> Result<Option<HistoryEventId>, StoreError> {
    connection
        .query_row(
            "SELECT event_id FROM read_markers WHERE buffer_id=?1",
            [to_sql_id(buffer.0)?],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?
        .map(|value| from_sql_id(value).map(HistoryEventId))
        .transpose()
}

/// Moves a read marker forward only, matching the cursor's monotonic rule.
pub(crate) fn advance_read_marker(
    connection: &mut Connection,
    buffer: BufferId,
    to: HistoryEventId,
) -> Result<HistoryEventId, StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT event_id FROM read_markers WHERE buffer_id=?1",
            [to_sql_id(buffer.0)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let current = existing.map(from_sql_id).transpose()?;
    let next = match current {
        Some(current) if current >= to.0 => HistoryEventId(current),
        _ => HistoryEventId(to.0),
    };
    transaction
        .execute(
            "INSERT INTO read_markers (buffer_id, event_id) VALUES (?1, ?2)
             ON CONFLICT(buffer_id) DO UPDATE SET event_id=excluded.event_id",
            params![to_sql_id(buffer.0)?, to_sql_id(next.0)?],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(next)
}

/// Bounded retention. Deleted rows are clamped monotonically so a retained cursor
/// never ends up past the oldest surviving event.
pub(crate) fn retain(
    connection: &mut Connection,
    request: &RetentionRequest,
) -> Result<RetentionReport, StoreError> {
    request
        .validate()
        .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    let network = to_sql_id(request.network.0)?;
    let before = to_sql_id(request.before.0)?;
    let limit = to_sql_id(request.max_delete as u64)?;
    let transaction = connection
        .transaction()
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let rows: Vec<i64> = {
        let mut statement = transaction
            .prepare(
                "SELECT event_id FROM history_events
                 WHERE network_id=?1 AND event_id < ?2 ORDER BY event_id LIMIT ?3",
            )
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        let mapped = statement
            .query_map(params![network, before, limit], |row| row.get::<_, i64>(0))
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        let mut collected = Vec::new();
        for row in mapped {
            collected.push(row.map_err(|error| sql(error, CommitState::RolledBack))?);
        }
        collected
    };
    if rows.is_empty() {
        let oldest = oldest_retained(&transaction, network)?
            .map(from_sql_id)
            .transpose()?
            .map(HistoryEventId);
        transaction
            .commit()
            .map_err(|error| sql(error, CommitState::Unknown))?;
        return Ok(RetentionReport {
            deleted: 0,
            last_deleted: None,
            oldest_retained: oldest,
            ..RetentionReport::default()
        });
    }
    let last = *rows.last().expect("non-empty retention selection");
    let clamp = clamp_and_delete(&transaction, &rows, network)?;
    let oldest = oldest_retained(&transaction, network)?
        .map(from_sql_id)
        .transpose()?
        .map(HistoryEventId);
    let more_pending: i64 = transaction
        .query_row(
            "SELECT count(*) FROM history_events WHERE network_id=?1 AND event_id < ?2",
            params![network, before],
            |row| row.get(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    transaction
        .commit()
        .map_err(|error| sql(error, CommitState::Unknown))?;
    Ok(RetentionReport {
        deleted: clamp.deleted,
        last_deleted: Some(HistoryEventId(from_sql_id(last)?)),
        oldest_retained: oldest,
        cursors_clamped: clamp.cursors_clamped,
        markers_clamped: clamp.markers_clamped,
        more_pending: more_pending > 0,
    })
}

/// Deletes the selected rows and clamps any cursor/marker that pointed into the
/// removed range.
///
/// Clamp rule: a position inside the removed range is pulled down to the newest
/// surviving event for that buffer strictly below the range, or to `0` when the
/// buffer has no earlier event left. `0` therefore means "before any retained
/// event", which replays the whole buffer rather than skipping it. A position above
/// the range is untouched because its event is still retained.
fn clamp_and_delete(
    transaction: &Transaction<'_>,
    rows: &[i64],
    network: i64,
) -> Result<ClampOutcome, StoreError> {
    let newest_removed = *rows.last().expect("non-empty retention selection");
    let oldest_removed = rows[0];
    // Clamping runs before the delete so the surviving maximum is still visible.
    let clamp = |table: &'static str, column: &'static str| {
        let statement_text = format!(
            "UPDATE {table} SET {column} = coalesce(
                 (SELECT max(h.event_id) FROM history_events h
                   WHERE h.buffer_id = {table}.buffer_id AND h.event_id < ?1), 0)
             WHERE {column} >= ?1 AND {column} <= ?2
               AND buffer_id IN (SELECT buffer_id FROM buffers WHERE network_id=?3)"
        );
        transaction
            .execute(
                &statement_text,
                params![oldest_removed, newest_removed, network],
            )
            .map_err(|error| sql(error, CommitState::RolledBack))
    };
    let cursors_clamped = clamp("client_cursors", "event_id")?;
    let markers_clamped = clamp("read_markers", "event_id")?;
    let deleted = transaction
        .execute(
            "DELETE FROM history_events
             WHERE event_id >= ?1 AND event_id <= ?2 AND network_id=?3",
            params![oldest_removed, newest_removed, network],
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    // The search side index is deleted in the same transaction, by exact event id. A
    // retained index row for a message that no longer exists would answer a search with
    // content the Operator has already had deleted, which is the one thing retention
    // exists to prevent. The ids are already in hand and already bounded by
    // `MAX_RETENTION_DELETE`, so this costs one statement per row and cannot itself become
    // an unbounded operation.
    for event_id in rows {
        crate::search::delete_search_row(transaction, *event_id).map_err(|error| {
            sql(
                rusqlite::Error::ToSqlConversionFailure(Box::new(error)),
                CommitState::RolledBack,
            )
        })?;
    }
    Ok(ClampOutcome {
        deleted,
        cursors_clamped,
        markers_clamped,
    })
}

struct ClampOutcome {
    deleted: usize,
    cursors_clamped: usize,
    markers_clamped: usize,
}

fn oldest_retained(transaction: &Transaction<'_>, network: i64) -> Result<Option<i64>, StoreError> {
    transaction
        .query_row(
            "SELECT min(event_id) FROM history_events WHERE network_id=?1",
            [network],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| sql(error, CommitState::RolledBack))
}

// ------------------------------------------------------------------- search

/// The form every history-time bound is compared in.
///
/// `history_events.effective_time` is a TEXT column holding canonical fixed-width UTC,
/// so this is the *only* representation a bound may use. Binding integer milliseconds
/// instead would compare TEXT against INTEGER, and SQLite orders every TEXT value after
/// every INTEGER value regardless of the numbers involved — so the predicate would match
/// either everything or nothing while still looking like a time comparison.
fn canonical_time(value: &IrcTimestamp) -> String {
    value.to_string()
}

/// Resolves a `msgid=` reference within one Network.
///
/// Returns every match rather than the first one. A duplicate upstream id is a real
/// condition, and picking the lowest `HistoryEventId` would answer a question the client
/// did not ask while looking authoritative.
pub(crate) fn resolve_msgid(
    connection: &Connection,
    network: NetworkId,
    msgid: &str,
) -> Result<MsgidLookup, StoreError> {
    if msgid.is_empty() || msgid.len() > crate::model::MAX_HISTORY_MSGID_BYTES {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "msgid reference",
        )));
    }
    let network = to_sql_id(network.0)?;
    let mut statement = connection
        .prepare(
            "SELECT event_id FROM history_events
             WHERE network_id=?1 AND msgid=?2 ORDER BY event_id LIMIT ?3",
        )
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let ceiling = to_sql_id(crate::model::MAX_MSGID_AMBIGUOUS as u64)?;
    let rows = statement
        .query_map(params![network, msgid, ceiling], |row| row.get::<_, i64>(0))
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let mut found = Vec::new();
    for row in rows {
        found.push(row.map_err(|error| sql(error, CommitState::RolledBack))?);
    }
    Ok(match found.len() {
        0 => MsgidLookup::Missing,
        1 => MsgidLookup::Unique(HistoryEventId(from_sql_id(found[0])?)),
        _ => {
            let mut events = Vec::with_capacity(found.len());
            for id in found {
                events.push(HistoryEventId(from_sql_id(id)?));
            }
            MsgidLookup::Ambiguous(events)
        }
    })
}

/// Finds the events bracketing one canonical protocol timestamp in one buffer.
///
/// Two indexed seeks rather than a scan, which is what makes a `timestamp=` reference
/// cost the same whether the buffer holds ten events or ten million. Both edges are
/// reported because `AROUND` needs to know whether the reference landed *on* an event.
///
/// Both edges absent means the buffer holds no positioned event at all. That is a
/// different answer from "the reference falls outside everything retained" -- the
/// latter has an edge on at least one side -- and a caller that anchors to the nearest
/// retained event needs to be able to tell them apart.
pub(crate) fn nearest_event(
    connection: &Connection,
    buffer: BufferId,
    reference: &IrcTimestamp,
) -> Result<NearestEvent, StoreError> {
    // `effective_time` is canonical protocol *text*, and the canonical form is
    // fixed-width UTC, so lexicographic order is chronological order and the index on
    // `(buffer_id, effective_time, event_id)` is usable. Comparing against integer
    // milliseconds would compare a TEXT column against an INTEGER parameter, which
    // SQLite orders by type: every text value sorts after every integer, so the
    // comparison silently matches everything or nothing. That is not a subtlety this
    // code is allowed to have.
    //
    // `effective_time` rather than `server_time`, so an upstream that stamps nothing
    // still has a buffer that can be positioned in.
    let reference_time = canonical_time(reference);
    let buffer = to_sql_id(buffer.0)?;
    let before: Option<i64> = connection
        .query_row(
            "SELECT event_id FROM history_events
             WHERE buffer_id=?1 AND effective_time <= ?2
             ORDER BY effective_time DESC, event_id DESC LIMIT 1",
            params![buffer, reference_time],
            |row| row.get(0),
        )
        .ok();
    let after: Option<i64> = connection
        .query_row(
            "SELECT event_id FROM history_events
             WHERE buffer_id=?1 AND effective_time > ?2
             ORDER BY effective_time ASC, event_id ASC LIMIT 1",
            params![buffer, reference_time],
            |row| row.get(0),
        )
        .ok();
    // "Exact" means an event carries the reference's own millisecond. Asking the row is
    // cheaper than re-deriving it, and it keeps the definition in one place: a reference
    // is exact when a retained event *has* that timestamp, not when it happens to be the
    // nearest one.
    let exact = match before {
        Some(before) => {
            connection
                .query_row(
                    "SELECT effective_time = ?2 FROM history_events WHERE event_id = ?1",
                    params![before, reference_time],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                == 1
        }
        None => false,
    };
    Ok(NearestEvent {
        before: before.map(from_sql_id).transpose()?.map(HistoryEventId),
        after: after.map(from_sql_id).transpose()?.map(HistoryEventId),
        exact,
    })
}

/// Runs one bounded search over one Network's retained history.
///
/// The `MATCH` expression is compiled from validated terms by
/// [`crate::model::SearchQuery::match_expression`]; nothing a client typed reaches SQL as
/// syntax. Buffer scoping is applied in the same statement as the match, so a Network
/// filter cannot be forgotten by a later edit to the query text.
pub(crate) fn search(
    connection: &Connection,
    query: &SearchQuery,
) -> Result<Vec<SearchHit>, StoreError> {
    query
        .validate()
        .map_err(|reason| StoreError::new(StoreErrorKind::InvalidRequest(reason)))?;
    let network = to_sql_id(query.network.0)?;
    let limit = to_sql_id(query.limit as u64)?;
    let sender = query.sender.as_deref();

    let mut hits = Vec::new();
    if let Some(expression) = query.match_expression() {
        // Buffer scoping is a fixed placeholder layout per shape, so the statement text is
        // a small closed set and every client-influenced value is a bound parameter.
        let mut text = String::from(
            "SELECT h.event_id, h.buffer_id, s.sender, s.target, s.body
             FROM history_search s
             JOIN history_events h ON h.event_id = s.rowid
             WHERE history_search MATCH ?1 AND h.network_id = ?2",
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(expression), Box::new(network)];
        if !query.buffers.is_empty() {
            let placeholders: Vec<String> = (0..query.buffers.len())
                .map(|index| format!("?{}", values.len() + index + 1))
                .collect();
            text.push_str(&format!(
                " AND h.buffer_id IN ({})",
                placeholders.join(", ")
            ));
            for buffer in &query.buffers {
                values.push(Box::new(to_sql_id(buffer.0)?));
            }
        }
        if let Some(nick) = sender {
            text.push_str(&format!(" AND s.sender = ?{}", values.len() + 1));
            values.push(Box::new(nick.to_owned()));
        }
        if let Some(after) = &query.after {
            text.push_str(&format!(" AND h.effective_time >= ?{}", values.len() + 1));
            values.push(Box::new(canonical_time(after)));
        }
        if let Some(before) = &query.before {
            text.push_str(&format!(" AND h.effective_time < ?{}", values.len() + 1));
            values.push(Box::new(canonical_time(before)));
        }
        text.push_str(&format!(" ORDER BY h.event_id LIMIT ?{}", values.len() + 1));
        values.push(Box::new(limit));

        let reference: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|value| value.as_ref()).collect();
        let mut statement = connection
            .prepare(&text)
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        let rows = statement
            .query_map(reference.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|error| sql(error, CommitState::RolledBack))?;
        for row in rows {
            let (event, buffer, sender, target, body) =
                row.map_err(|error| sql(error, CommitState::RolledBack))?;
            hits.push(SearchHit {
                event: HistoryEventId(from_sql_id(event)?),
                buffer: BufferId(from_sql_id(buffer)?),
                sender,
                target,
                body,
            });
        }
        return Ok(hits);
    }

    // No terms: this is a bounded listing within the caller's scope, which is what a
    // `from=`-only or time-bounded request means. It is still bounded by the Network, the
    // buffer scope, the range, and the limit -- never by the size of the journal.
    let mut text = String::from(
        "SELECT h.event_id, h.buffer_id, COALESCE(s.sender,''), COALESCE(s.target,''), COALESCE(s.body,'')
         FROM history_events h
         LEFT JOIN history_search s ON s.rowid = h.event_id
         WHERE h.network_id = ?1",
    );
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(network)];
    if !query.buffers.is_empty() {
        let placeholders: Vec<String> = (0..query.buffers.len())
            .map(|index| format!("?{}", values.len() + index + 1))
            .collect();
        text.push_str(&format!(
            " AND h.buffer_id IN ({})",
            placeholders.join(", ")
        ));
        for buffer in &query.buffers {
            values.push(Box::new(to_sql_id(buffer.0)?));
        }
    }
    if let Some(nick) = sender {
        text.push_str(&format!(
            " AND COALESCE(s.sender,'') = ?{}",
            values.len() + 1
        ));
        values.push(Box::new(nick.to_owned()));
    }
    if let Some(after) = &query.after {
        text.push_str(&format!(" AND h.effective_time >= ?{}", values.len() + 1));
        values.push(Box::new(canonical_time(after)));
    }
    if let Some(before) = &query.before {
        text.push_str(&format!(" AND h.effective_time < ?{}", values.len() + 1));
        values.push(Box::new(canonical_time(before)));
    }
    text.push_str(&format!(" ORDER BY h.event_id LIMIT ?{}", values.len() + 1));
    values.push(Box::new(limit));

    let reference: Vec<&dyn rusqlite::ToSql> = values.iter().map(|value| value.as_ref()).collect();
    let mut statement = connection
        .prepare(&text)
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    let rows = statement
        .query_map(reference.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| sql(error, CommitState::RolledBack))?;
    for row in rows {
        let (event, buffer, sender, target, body) =
            row.map_err(|error| sql(error, CommitState::RolledBack))?;
        hits.push(SearchHit {
            event: HistoryEventId(from_sql_id(event)?),
            buffer: BufferId(from_sql_id(buffer)?),
            sender,
            target,
            body,
        });
    }
    Ok(hits)
}
