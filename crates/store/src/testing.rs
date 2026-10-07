//! Schema and database inspection helpers for storage qualification tests.
//!
//! Migration, rollback, restart, and secret evidence must read the raw database
//! rather than trust the same API that is under test. These helpers deliberately
//! bypass the owned worker, so they are kept out of the production request surface:
//! nothing in [`crate::StoreHandle`] can reach them.
//!
//! This module is `#[doc(hidden)]` because it is a test affordance, not a runtime
//! capability. It exposes no network authority and no production code path.
#![doc(hidden)]

use crate::{
    StoreError, StoreErrorKind,
    error::CommitState,
    schema::{self, OpenDisposition},
};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Every table the schema contains, in SQLite's name order.
///
/// The table *set* has been identical across every schema version so far; only the
/// representation of `history_events.server_time` and `networks.display_name` changed,
/// and only `desired_channels` gained a column.
pub const EXPECTED_TABLES: [&str; 9] = [
    "buffers",
    "client_cursors",
    "clients",
    "desired_channels",
    "history_events",
    // The FTS5 search side index. Its shadow tables are SQLite's own storage and are
    // excluded by `table_names`, so listing them here would tie the promised schema to a
    // SQLite build detail.
    "history_search",
    "network_secrets",
    "networks",
    "read_markers",
];

/// Creates a database at schema version 1 and returns a raw connection to it.
///
/// Used to build migration fixtures: a v1 database is exactly what the v1 -> v2
/// migration must handle, and it cannot be produced by this build's own `open`
/// because that always migrates.
pub fn create_v1_database(path: &Path) -> Connection {
    let connection = Connection::open(path).expect("database file is creatable");
    connection
        .execute_batch(&schema::schema_v1())
        .expect("schema 1 applies");
    connection
        .pragma_update(None, "application_id", crate::APPLICATION_ID)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", 1)
        .expect("user_version is writable");
    connection
}

/// Creates a database at schema version 2 and returns a raw connection to it.
///
/// Used to build the fixture the v2 -> v3 migration must handle. Like
/// [`create_v1_database`] it cannot be produced by this build's own `open`, because
/// that always migrates forward.
pub fn create_v2_database(path: &Path) -> Connection {
    let connection = Connection::open(path).expect("database file is creatable");
    connection
        .execute_batch(&schema::schema_v2())
        .expect("schema 2 applies");
    connection
        .pragma_update(None, "application_id", crate::APPLICATION_ID)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", 2)
        .expect("user_version is writable");
    connection
}

/// Creates a database at schema version 3 and returns a raw connection to it.
///
/// Used to build the fixture the v3 -> v4 migration must handle.
pub fn create_v3_database(path: &Path) -> Connection {
    let connection = Connection::open(path).expect("database file is creatable");
    connection
        .execute_batch(&schema::schema_v3())
        .expect("schema 3 applies");
    connection
        .pragma_update(None, "application_id", crate::APPLICATION_ID)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", 3)
        .expect("user_version is writable");
    connection
}

/// Creates a database at schema version 4 and returns a raw connection to it.
///
/// Used to build the fixture the v4 -> v5 migration must handle.
pub fn create_v4_database(path: &Path) -> Connection {
    let connection = Connection::open(path).expect("database file is creatable");
    connection
        .execute_batch(&schema::schema_v4())
        .expect("schema 4 applies");
    connection
        .pragma_update(None, "application_id", crate::APPLICATION_ID)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", 4)
        .expect("user_version is writable");
    connection
}

/// Creates a database at schema version 5 and returns a raw connection to it.
///
/// Used to build the fixture the v5 -> v6 migration must handle. Search events are seeded
/// so the migration's bounded backfill has something to copy: a backfill that is only ever
/// exercised against an empty journal proves nothing about the batch loop.
pub fn create_v5_database(path: &Path) -> Connection {
    let connection = Connection::open(path).expect("database file is creatable");
    connection
        .execute_batch(&schema::schema_v5())
        .expect("schema 5 applies");
    seed_history_for_backfill(&connection);
    connection
        .pragma_update(None, "application_id", crate::APPLICATION_ID)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", 5)
        .expect("user_version is writable");
    connection
}

/// Seeds one Network, one Buffer, and three retained messages.
fn seed_history_for_backfill(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO networks
                (network_id, endpoint, endpoint_kind, nick, username, realname, display_name)
             VALUES (1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.b32.i2p', 1,
                     'bot', 'user', 'bouncer', 'lab');
             INSERT INTO buffers (buffer_id, network_id, kind, target, canonical_key)
             VALUES (1, 1, 0, '#room', x'23726f6f6d');
             INSERT INTO history_events
                (event_id, network_id, buffer_id, received_at, server_time, msgid,
                 direction, event_class, payload)
             VALUES
                (1, 1, 1, 1000, '2026-01-01T00:00:00.000Z', 'm1', 0, 'PRIVMSG',
                 CAST(':alice!a@h PRIVMSG #room :hello there' AS BLOB)),
                (2, 1, 1, 1001, '2026-01-01T00:00:01.000Z', 'm2', 0, 'PRIVMSG',
                 CAST(':bob!b@h PRIVMSG #room :goodbye now' AS BLOB)),
                (3, 1, 1, 1002, '2026-01-01T00:00:02.000Z', 'm3', 0, 'JOIN',
                 CAST(':carol!c@h JOIN #room' AS BLOB));",
        )
        .expect("history fixture is insertable");
}

/// A temporary directory owned by one test, removed when the guard is dropped.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
    /// A database path inside this directory.
    pub fn db(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a uniquely named temporary directory. Every store test gets its own
/// database, so tests never share durable state.
pub fn temp_dir(label: &str) -> TempDir {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "i2pr-irc-store-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("temporary directory is creatable");
    TempDir(path)
}

/// Opens a raw connection without migrating it.
pub fn raw(path: &Path) -> Connection {
    Connection::open(path).expect("database file is openable")
}

/// Recorded application identity and schema version.
pub fn identity(path: &Path) -> (i64, i64) {
    let connection = raw(path);
    let application_id = connection
        .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
        .expect("application_id is readable");
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .expect("user_version is readable");
    (application_id, user_version)
}

/// Forces a header to look like a database this build must refuse.
pub fn stamp(path: &Path, application_id: i64, user_version: i64) {
    let connection = raw(path);
    connection
        .pragma_update(None, "application_id", application_id)
        .expect("application_id is writable");
    connection
        .pragma_update(None, "user_version", user_version)
        .expect("user_version is writable");
}

/// Classifies a database without migrating it.
pub fn classify(path: &Path) -> Result<OpenDisposition, StoreError> {
    let connection = raw(path);
    schema::classify(&connection).map_err(|error| StoreError::new(schema::open_error_kind(&error)))
}

/// Every table present, so a test can assert the exact schema shape.
pub fn tables(path: &Path) -> Vec<String> {
    raw(path)
        .prepare(
            // FTS5 shadow tables are excluded for the same reason `schema::table_names`
            // excludes them: they are SQLite's own storage, not promised schema.
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'history_search_%'
             ORDER BY name",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("schema is readable")
}

/// Runs arbitrary SQL as a test fixture. Production code has no equivalent path.
pub fn execute(path: &Path, statement: &str) {
    raw(path)
        .execute_batch(statement)
        .expect("fixture statement applies");
}

/// Runs a statement expected to be rejected, proving a real constraint exists.
pub fn expect_rejected(path: &Path, statement: &str) -> rusqlite::Error {
    raw(path)
        .execute_batch(statement)
        .expect_err("statement must be rejected by the database")
}

/// Counts rows matching a predicate.
pub fn count(path: &Path, sql: &str) -> i64 {
    raw(path)
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .expect("count query is valid")
}

/// Reads one optional integer column.
pub fn optional_i64(path: &Path, sql: &str, args: &[i64]) -> Option<i64> {
    let connection = raw(path);
    let mut statement = connection.prepare(sql).expect("query is valid");
    let mut rows = statement
        .query(rusqlite::params_from_iter(args.iter()))
        .expect("query executes");
    rows.next()
        .expect("at most one row")
        .and_then(|row| row.get::<_, i64>(0).ok())
}

/// A `StoreError` whose durable commit state is `Unknown`.
///
/// A real SQLite commit failure cannot be provoked from a test without corrupting a
/// database, but the *decision* that follows one is exactly what has to be tested: a
/// caller must re-read durable state rather than assume the mutation landed or did not.
pub fn unknown_commit() -> StoreError {
    StoreError::mutating(StoreErrorKind::Sqlite, CommitState::Unknown)
}

/// Reads every value of one text column, in the order the query produced them.
pub fn texts(path: &Path, sql: &str) -> Vec<String> {
    let connection = raw(path);
    let mut statement = connection.prepare(sql).expect("query is valid");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query executes");
    rows.map(|row| row.expect("text column is readable"))
        .collect()
}

/// Foreign-key enforcement is on, proven against the real schema.
pub fn foreign_keys_enabled(path: &Path) -> bool {
    raw(path)
        .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
        .unwrap_or(0)
        != 0
}

/// The declared constraints SQLite reports for one column.
///
/// Used to prove the `detached` flag is constrained at the storage layer rather than
/// only in Rust, so a value outside 0/1 cannot be smuggled in by another writer.
pub fn column_constraints(path: &Path, table: &str, column: &str) -> (bool, Option<i64>) {
    let connection = raw(path);
    let mut statement = connection
        .prepare("SELECT \"notnull\", dflt_value FROM pragma_table_info(?1) WHERE name = ?2")
        .expect("query is valid");
    let mut rows = statement
        .query(rusqlite::params![table, column])
        .expect("query executes");
    rows.next()
        .expect("the column exists")
        .map(|row| {
            (
                row.get::<_, i64>(0).unwrap_or(0) != 0,
                row.get::<_, Option<String>>(1)
                    .ok()
                    .flatten()
                    .and_then(|value| value.parse::<i64>().ok()),
            )
        })
        .unwrap_or((false, None))
}

/// Reads one optional text column.
pub fn optional_text(path: &Path, sql: &str, args: &[i64]) -> Option<String> {
    let connection = raw(path);
    let mut statement = connection.prepare(sql).expect("query is valid");
    let mut rows = statement
        .query(rusqlite::params_from_iter(args.iter()))
        .expect("query executes");
    rows.next()
        .expect("at most one row")
        .and_then(|row| row.get::<_, String>(0).ok())
}

/// The next rowid `AUTOINCREMENT` would hand out for a table, which is how a test
/// proves that migration did not reset event-id monotonicity.
pub fn autoincrement_sequence(path: &Path, table: &str) -> Option<i64> {
    raw(path)
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .ok()
}

/// Declared storage type and constraints for one column, so a test can assert the
/// schema shape rather than inferring it from behaviour.
pub fn column_type(path: &Path, table: &str, column: &str) -> String {
    raw(path)
        .query_row(
            "SELECT type FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default()
}
