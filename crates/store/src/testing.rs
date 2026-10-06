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
    StoreError,
    schema::{self, OpenDisposition},
};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Every table the frozen schema version 1 contains, in SQLite's name order.
pub const EXPECTED_TABLES: [&str; 8] = [
    "buffers",
    "client_cursors",
    "clients",
    "desired_channels",
    "history_events",
    "network_secrets",
    "networks",
    "read_markers",
];

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
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
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

/// Foreign-key enforcement is on, proven against the real schema.
pub fn foreign_keys_enabled(path: &Path) -> bool {
    raw(path)
        .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
        .unwrap_or(0)
        != 0
}
