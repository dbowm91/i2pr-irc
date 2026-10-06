//! Schema versions 1 through 3 and their transactional migration harness.
//!
//! The schema is written in SQL rather than as a serialized Rust value graph: draft
//! IRCv3 syntax and internal Rust representation must both be free to change without
//! a storage migration.
use crate::{StoreError, StoreErrorKind};
use i2pr_irc_wire::IrcTimestamp;
use rusqlite::Connection;

/// The newest schema version this build creates and understands.
///
/// Version 2 changes only how a history event's protocol timestamp is stored; see
/// [`HISTORY_EVENTS_V2`] for why. Version 3 adds the operator-chosen `display_name`
/// a Network is listed under; see [`NETWORKS_V2`] and [`migrate_2_to_3`].
pub const SCHEMA_VERSION: i64 = 3;
/// Oldest schema version this build can migrate forward from.
pub const MIN_SUPPORTED_SCHEMA_VERSION: i64 = 1;
/// Application identity stored in SQLite's `application_id` header. A database
/// without this exact value is refused rather than adopted.
pub const APPLICATION_ID: i64 = 0x6932_7072;
/// Oldest bundled SQLite that supports the STRICT tables below (3.37.0).
pub const MIN_SQLITE_VERSION: (u32, u32, u32) = (3, 37, 0);
/// SQLite's documented ceiling on `application_id`.
const MAX_APPLICATION_ID: i64 = 0x7fff_ffff;
/// SQLite's ceiling on `user_version`.
const MAX_USER_VERSION: i64 = 1_000_000_000;
/// Rows copied per step while migrating, so a large table never has to be
/// materialized in memory at once. The migration still runs in one transaction.
const MIGRATION_BATCH_ROWS: i64 = 256;

/// Result of validating an existing database before any request is served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDisposition {
    /// Fresh database; the current schema must be created.
    Fresh,
    /// This application's database at an older schema, migrated forward by
    /// [`open_and_migrate`] before any request is served.
    Migrated { from: i64 },
    /// This application's database already at the current schema.
    Current,
}

/// The `networks` table as schema 3 creates it.
///
/// `display_name` is the operator-chosen label a Network is listed under in
/// operator-facing output. It is a display value only: it never participates in
/// lookup, identity, or routing, and it is not derived from the endpoint.
const NETWORKS_V3: &str = r#"
CREATE TABLE networks (
    network_id      INTEGER PRIMARY KEY,
    endpoint        TEXT NOT NULL,
    endpoint_kind   INTEGER NOT NULL,
    nick            TEXT NOT NULL,
    username        TEXT NOT NULL,
    realname        TEXT NOT NULL,
    display_name    TEXT NOT NULL
) STRICT;
"#;

/// The `networks` table as schema 1 and 2 created it.
///
/// Retained verbatim so the v2 -> v3 migration rebuilds exactly the representation it
/// is replacing, rather than one reconstructed from a newer declaration.
const NETWORKS_V2: &str = r#"
CREATE TABLE networks (
    network_id      INTEGER PRIMARY KEY,
    endpoint        TEXT NOT NULL,
    endpoint_kind   INTEGER NOT NULL,
    nick            TEXT NOT NULL,
    username        TEXT NOT NULL,
    realname        TEXT NOT NULL
) STRICT;
"#;

/// Tables that are identical in every schema version, after `networks` and before
/// `history_events`.
const SCHEMA_TABLES: &str = r#"
CREATE TABLE network_secrets (
    network_id      INTEGER PRIMARY KEY
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    sasl_username   TEXT NOT NULL,
    sasl_password   BLOB NOT NULL
) STRICT;

CREATE TABLE desired_channels (
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    casemap_key     BLOB NOT NULL,
    target          TEXT NOT NULL,
    position        INTEGER NOT NULL,
    PRIMARY KEY (network_id, casemap_key)
) STRICT;

CREATE TABLE clients (
    client_id       INTEGER PRIMARY KEY,
    login           TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE buffers (
    buffer_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    kind            INTEGER NOT NULL,
    canonical_key   BLOB NOT NULL,
    target          TEXT NOT NULL,
    UNIQUE (network_id, canonical_key)
) STRICT;
"#;

/// Tables that are identical in every schema version, after `history_events`.
const SCHEMA_TAIL: &str = r#"
CREATE TABLE client_cursors (
    client_id       INTEGER NOT NULL
                    REFERENCES clients(client_id) ON DELETE CASCADE,
    buffer_id       INTEGER NOT NULL
                    REFERENCES buffers(buffer_id) ON DELETE CASCADE,
    event_id        INTEGER NOT NULL,
    PRIMARY KEY (client_id, buffer_id)
) STRICT;

CREATE TABLE read_markers (
    buffer_id       INTEGER PRIMARY KEY
                    REFERENCES buffers(buffer_id) ON DELETE CASCADE,
    event_id        INTEGER NOT NULL
) STRICT;
"#;

/// Schema 1 `history_events`: protocol timestamp held as whole epoch seconds.
///
/// Retained verbatim because a v1 database is exactly what schema 2 must migrate,
/// and because it documents what the migration is correcting.
pub(crate) const HISTORY_EVENTS_V1: &str = r#"
CREATE TABLE history_events (
    -- AUTOINCREMENT is monotonic and never reuses a deleted rowid, so a retained
    -- cursor can never later alias a different event after retention.
    event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    buffer_id       INTEGER NOT NULL
                    REFERENCES buffers(buffer_id) ON DELETE CASCADE,
    received_at     INTEGER NOT NULL,
    server_time     INTEGER,
    msgid           TEXT,
    direction       INTEGER NOT NULL,
    event_class     TEXT NOT NULL,
    payload         BLOB NOT NULL
) STRICT;
"#;

/// Index over the history table, identical in every schema version.
pub(crate) const HISTORY_EVENTS_INDEX: &str =
    "CREATE INDEX history_by_buffer ON history_events (buffer_id, event_id);";

/// Schema 2 `history_events`: protocol timestamp held as canonical validated text.
///
/// The change from v1 is `server_time` only. v1 stored whole epoch seconds, which
/// cannot represent the millisecond precision that the IRCv3 `server-time`
/// extension requires and cannot represent a leap second at all, so a conformant
/// upstream timestamp was truncated on the way in and an invalid one could be
/// replayed on the way out. v2 stores the canonical `YYYY-MM-DDThh:mm:ss.sssZ`
/// string, which round-trips exactly.
///
/// `received_at` is deliberately left as local whole seconds. It is diagnostic
/// metadata and has never been a protocol value; upgrading it would invent
/// precision the process does not actually have.
pub(crate) const HISTORY_EVENTS_V2: &str = r#"
CREATE TABLE history_events (
    event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    buffer_id       INTEGER NOT NULL
                    REFERENCES buffers(buffer_id) ON DELETE CASCADE,
    received_at     INTEGER NOT NULL,
    -- Canonical server-time text, or NULL when the upstream sent none. The GLOB
    -- constrains the wire *shape* at the storage layer; calendar validity is
    -- enforced by the Rust parser on the way in and out.
    server_time     TEXT
                    CHECK (server_time IS NULL OR server_time GLOB
                        '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    msgid           TEXT,
    direction       INTEGER NOT NULL,
    event_class     TEXT NOT NULL,
    payload         BLOB NOT NULL
) STRICT;
"#;

/// Composes a complete schema from its shared parts.
///
/// `concat!` cannot reference a const, so the pieces are joined at runtime instead.
/// That keeps one definition of every unchanged table rather than duplicating all
/// eight tables per version.
fn compose(networks: &str, history: &str, tail: &str) -> String {
    let mut sql = String::with_capacity(
        networks.len() + history.len() + tail.len() + HISTORY_EVENTS_INDEX.len(),
    );
    sql.push_str(networks);
    sql.push_str(SCHEMA_TABLES);
    sql.push_str(history);
    sql.push_str(HISTORY_EVENTS_INDEX);
    sql.push_str(tail);
    sql
}

/// Schema 1, used to build migration fixtures. A v1 database is exactly what the
/// schema 2 migration must handle.
pub(crate) fn schema_v1() -> String {
    compose(NETWORKS_V2, HISTORY_EVENTS_V1, SCHEMA_TAIL)
}

/// Schema 2, used to build the fixture the schema 3 migration must handle.
///
/// It is a real declaration rather than "v3 minus a column" so the fixture cannot
/// silently drift into describing a shape this build never actually wrote.
pub(crate) fn schema_v2() -> String {
    compose(NETWORKS_V2, HISTORY_EVENTS_V2, SCHEMA_TAIL)
}

/// The current schema, created directly when no database exists yet.
pub(crate) fn schema_v3() -> String {
    compose(NETWORKS_V3, HISTORY_EVENTS_V2, SCHEMA_TAIL)
}

/// Refuses a database this build must not serve before any migration runs.
pub(crate) fn classify(connection: &Connection) -> Result<OpenDisposition, ClassifyError> {
    const FOREIGN: ClassifyError = ClassifyError::Unsupported("database is not an i2pr-irc store");
    const TOO_NEW: ClassifyError =
        ClassifyError::Unsupported("database schema is newer than this build supports");
    if sqlite_version() < MIN_SQLITE_VERSION {
        return Err(ClassifyError::Unsupported(
            "sqlite runtime lacks STRICT table support",
        ));
    }
    let application_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|_| ClassifyError::Query)?;
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| ClassifyError::Query)?;
    if !(0..=MAX_APPLICATION_ID).contains(&application_id)
        || !(0..=MAX_USER_VERSION).contains(&user_version)
    {
        return Err(ClassifyError::Unsupported("database header is corrupt"));
    }
    if application_id == 0 {
        // A file with no application identity is only acceptable when it holds no
        // schema of its own; adopting someone else's tables would silently change
        // this application's durable meaning.
        if user_version != 0 {
            return Err(FOREIGN);
        }
        let existing: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type IN ('table','view','trigger') AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| ClassifyError::Query)?;
        return if existing == 0 {
            Ok(OpenDisposition::Fresh)
        } else {
            Err(FOREIGN)
        };
    }
    if application_id != APPLICATION_ID {
        return Err(FOREIGN);
    }
    // A version this build can still migrate forward from is upgraded; anything else,
    // including a version a *newer* build wrote, is refused rather than guessed at,
    // because a wrong guess would reinterpret durable meaning.
    match user_version {
        version if version == SCHEMA_VERSION => Ok(OpenDisposition::Current),
        version if (MIN_SUPPORTED_SCHEMA_VERSION..SCHEMA_VERSION).contains(&version) => {
            Ok(OpenDisposition::Migrated { from: version })
        }
        _ => Err(TOO_NEW),
    }
}

/// Why a database cannot be served, kept separate from an SQLite query failure.
#[derive(Debug)]
pub enum ClassifyError {
    /// The database, or the bundled SQLite, cannot support this build's schema.
    Unsupported(&'static str),
    /// Reading the database header itself failed.
    Query,
}

/// Maps a classification failure onto the public typed store error kind.
pub(crate) fn open_error_kind(error: &ClassifyError) -> StoreErrorKind {
    match error {
        ClassifyError::Query => StoreErrorKind::Open,
        ClassifyError::Unsupported(reason) => match *reason {
            "database is not an i2pr-irc store" => StoreErrorKind::ForeignDatabase,
            "sqlite runtime lacks STRICT table support" => StoreErrorKind::SqliteTooOld,
            _ => StoreErrorKind::SchemaTooNew,
        },
    }
}

/// The bundled SQLite version this build is linked against.
///
/// `rusqlite::version()` reports the library actually compiled in, which is what the
/// STRICT-table feature check must be based on: querying `sqlite_version()` through a
/// pragma is unreliable because a quoted pragma name is read as a table reference.
fn sqlite_version() -> (u32, u32, u32) {
    let mut parts = rusqlite::version()
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Applies the open policy: connection pragmas, identity, schema creation, and any
/// forward migration.
///
/// The whole operation runs in one transaction. A failure leaves the database at its
/// previous version rather than half migrated, and a partially created schema is
/// never accepted.
pub(crate) fn open_and_migrate(
    connection: &Connection,
    busy_timeout_ms: u32,
) -> Result<OpenDisposition, StoreError> {
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(u64::from(busy_timeout_ms)))
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    // FULL durability initially; relaxing it needs measured evidence and a separate
    // reviewed decision.
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    match classify(connection).map_err(|error| StoreError::new(open_error_kind(&error)))? {
        OpenDisposition::Current => {
            // Prove the promised schema is actually present before serving requests. A
            // database that claims our version but lost a promised table is corrupt, and
            // serving it would reinterpret durable meaning silently.
            verify_promised_tables(connection)?;
            Ok(OpenDisposition::Current)
        }
        OpenDisposition::Migrated { from } => {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            migrate_forward(&transaction, from)?;
            verify_promised_tables(&transaction)?;
            transaction
                .commit()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            Ok(OpenDisposition::Migrated { from })
        }
        OpenDisposition::Fresh => {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            transaction
                .execute_batch(&schema_v3())
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            transaction
                .pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            transaction
                .commit()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            Ok(OpenDisposition::Fresh)
        }
    }
}

/// Every user table present, used to verify the promised schema before serving.
pub(crate) fn table_names(connection: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row?);
    }
    Ok(names)
}

/// Confirms the tables this build promises are present inside the migration
/// transaction, so a migration that "succeeds" structurally but drops a table is
/// refused before it can commit.
fn verify_promised_tables(connection: &Connection) -> Result<(), StoreError> {
    let present = table_names(connection).map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    if REQUIRED_TABLES
        .iter()
        .all(|name| present.iter().any(|table| table == name))
    {
        Ok(())
    } else {
        Err(StoreError::new(StoreErrorKind::Corrupt(
            "missing schema table",
        )))
    }
}

/// Tables that must exist before this build serves any request.
const REQUIRED_TABLES: &[&str] = &[
    "networks",
    "history_events",
    "client_cursors",
    "buffers",
    "read_markers",
];

/// Migrates an already-open transaction forward to [`SCHEMA_VERSION`].
///
/// Steps are applied in order, one version at a time, so a database several versions
/// behind walks the same path it would have taken on each intervening release rather
/// than jumping. Each step rebuilds just the table whose representation changed;
/// nothing else is touched, so event ids, cursors, and read markers keep pointing at
/// the same rows.
fn migrate_forward(transaction: &rusqlite::Transaction<'_>, from: i64) -> Result<(), StoreError> {
    let mut version = from;
    while version < SCHEMA_VERSION {
        match version {
            1 => migrate_1_to_2(transaction)?,
            2 => migrate_2_to_3(transaction)?,
            _ => return Err(StoreError::new(StoreErrorKind::SchemaTooNew)),
        }
        version += 1;
    }
    Ok(())
}

/// One v1 history row, as read by the batched migration scan.
type V1Row = (
    i64,            // event_id
    i64,            // network_id
    i64,            // buffer_id
    i64,            // received_at
    Option<i64>,    // server_time
    Option<String>, // msgid
    i64,            // direction
    String,         // event_class
    Vec<u8>,        // payload
);

/// Rebuilds `history_events` with canonical text `server_time`.
///
/// `AUTOINCREMENT` semantics survive because every `event_id` is copied explicitly:
/// re-inserting the maximum rowid moves `sqlite_sequence` forward, so the next
/// allocated id is still strictly greater than any id ever issued. A retained
/// cursor therefore cannot later alias a different event.
fn migrate_1_to_2(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    // Build the v2 table under a temporary name, then swap, so the original v1 table
    // is only dropped once every row has been copied successfully. The index is
    // deliberately *not* created yet: its name still belongs to the v1 table, and it
    // is rebuilt after the swap under its real name.
    let staging = HISTORY_EVENTS_V2.replacen(
        "CREATE TABLE history_events (",
        "CREATE TABLE history_events_v2 (",
        1,
    );
    transaction
        .execute_batch(&staging)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;

    // Copy in bounded batches so an arbitrarily large history table never has to be
    // materialized at once. The surrounding transaction keeps this all-or-nothing.
    let mut last_event_id: i64 = 0;
    loop {
        let batch = {
            let mut statement = transaction
                .prepare(
                    "SELECT event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload
                     FROM history_events WHERE event_id > ?1 ORDER BY event_id LIMIT ?2",
                )
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            let rows = statement
                .query_map(
                    rusqlite::params![last_event_id, MIGRATION_BATCH_ROWS],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, Vec<u8>>(8)?,
                        ))
                    },
                )
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            rows.collect::<Result<Vec<V1Row>, _>>()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?
        };
        if batch.is_empty() {
            break;
        }
        for row in &batch {
            insert_migrated_event(transaction, row)?;
            last_event_id = row.0;
        }
    }

    transaction
        .execute_batch(
            "DROP TABLE history_events;
             ALTER TABLE history_events_v2 RENAME TO history_events;",
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    transaction
        .execute_batch(HISTORY_EVENTS_INDEX)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// Adds the operator-facing `networks.display_name` column.
///
/// Existing rows receive `network-<id>`: a deterministic label derived only from the
/// `NetworkId` the row already carries. It is deliberately *not* derived from the
/// endpoint, the nick, or any local path, because those are either identifying or
/// machine-specific and a display name is operator-facing text. An operator who wants
/// something else sets it explicitly.
///
/// The column is added with an empty default because SQLite forbids a non-constant
/// column default, then every row is filled in the same transaction. A reader can
/// therefore never observe a mixture of migrated and unmigrated names: either the
/// whole migration commits or the database stays at version 2 with no column at all.
fn migrate_2_to_3(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    transaction
        .execute_batch(
            "ALTER TABLE networks ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
             UPDATE networks SET display_name = 'network-' || network_id;",
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// Re-inserts one v1 row into the v2 staging table, converting its protocol
/// timestamp.
///
/// A v1 `server_time` that cannot be expressed as a canonical protocol timestamp is
/// migrated as NULL rather than failing the whole database. v1 accepted any integer
/// seconds within a +/-32.5e9 window, which reaches back before year 1 and so
/// contains values no conformant `server-time` could ever denote. Under the protocol
/// an unrepresentable timestamp is equivalent to no timestamp, and the local
/// `received_at` that carries retention meaning is preserved regardless, so nulling
/// costs no canonical order.
fn insert_migrated_event(
    transaction: &rusqlite::Transaction<'_>,
    row: &V1Row,
) -> Result<(), StoreError> {
    let canonical = row.4.and_then(|seconds| {
        // v1 truncated to whole seconds. That precision loss is unrecoverable and is
        // deliberately not invented back here.
        seconds
            .checked_mul(1_000)
            .and_then(IrcTimestamp::from_unix_millis)
            .map(|value| value.to_string())
    });
    transaction
        .execute(
            "INSERT INTO history_events_v2
                (event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![row.0, row.1, row.2, row.3, canonical, row.5, row.6, row.7, row.8],
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}
