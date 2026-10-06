//! Schema version 1 and its transactional migration harness.
//!
//! The schema is written in SQL rather than as a serialized Rust value graph: draft
//! IRCv3 syntax and internal Rust representation must both be free to change without
//! a storage migration.
use crate::{StoreError, StoreErrorKind};
use rusqlite::Connection;

/// The one schema version this build creates and understands.
pub const SCHEMA_VERSION: i64 = 1;
/// Application identity stored in SQLite's `application_id` header. A database
/// without this exact value is refused rather than adopted.
pub const APPLICATION_ID: i64 = 0x6932_7072;
/// Oldest bundled SQLite that supports the STRICT tables below (3.37.0).
pub const MIN_SQLITE_VERSION: (u32, u32, u32) = (3, 37, 0);
/// SQLite's documented ceiling on `application_id`.
const MAX_APPLICATION_ID: i64 = 0x7fff_ffff;
/// SQLite's ceiling on `user_version`.
const MAX_USER_VERSION: i64 = 1_000_000_000;

/// Result of validating an existing database before any request is served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDisposition {
    /// Fresh database; schema 1 must be created.
    Fresh,
    /// This application's database at schema 1.
    Current,
}

/// Schema 1. Every table is STRICT, so a value of the wrong storage class is
/// rejected by SQLite instead of being coerced into an ambiguous row.
pub(crate) const SCHEMA_V1: &str = r#"
CREATE TABLE networks (
    network_id      INTEGER PRIMARY KEY,
    endpoint        TEXT NOT NULL,
    endpoint_kind   INTEGER NOT NULL,
    nick            TEXT NOT NULL,
    username        TEXT NOT NULL,
    realname        TEXT NOT NULL
) STRICT;

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

CREATE INDEX history_by_buffer ON history_events (buffer_id, event_id);

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
    // Schema 1 is the only version this build knows. Anything else, including a
    // version an older build wrote, is refused rather than guessed at: a wrong guess
    // would reinterpret durable meaning.
    if user_version == SCHEMA_VERSION {
        Ok(OpenDisposition::Current)
    } else {
        Err(TOO_NEW)
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

/// Applies the open policy: connection pragmas, identity, and schema 1 creation.
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
    let disposition =
        classify(connection).map_err(|error| StoreError::new(open_error_kind(&error)))?;
    if disposition == OpenDisposition::Current {
        // Prove the promised schema is actually present before serving requests. A
        // database that claims our version but lost a promised table is corrupt, and
        // serving it would reinterpret durable meaning silently.
        let present = table_names(connection).map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        if !["networks", "history_events", "client_cursors", "buffers"]
            .iter()
            .all(|name| present.iter().any(|table| table == name))
        {
            return Err(StoreError::new(StoreErrorKind::Corrupt(
                "missing schema table",
            )));
        }
        return Ok(disposition);
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    transaction
        .execute_batch(SCHEMA_V1)
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
