//! Schema versions 1 through 9 and their transactional migration harness.
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
/// a Network is listed under; see [`NETWORKS_V2`] and [`migrate_2_to_3`]. Version 4
/// adds the bouncer-owned `detached` presentation flag on a desired channel; see
/// [`DESIRED_CHANNELS_V3`] and [`migrate_3_to_4`]. Version 5 adds the two Operator
/// presence policies, `auto_away` and `keep_nick`; see [`NETWORKS_V5`] and
/// [`migrate_4_to_5`]. Version 6 adds the search side index and the two relational
/// indexes history reference lookup needs; see [`HISTORY_SEARCH_V6`],
/// [`HISTORY_REFERENCE_INDEXES_V6`], and [`migrate_5_to_6`]. Version 7 adds the bounded
/// registration-action table; version 8 adds action phases and migrates prior rows to
/// `post-join`.
pub const SCHEMA_VERSION: i64 = 9;
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

const NETWORKS_V5: &str = r#"
CREATE TABLE networks (
    network_id      INTEGER PRIMARY KEY,
    endpoint        TEXT NOT NULL,
    endpoint_kind   INTEGER NOT NULL,
    nick            TEXT NOT NULL,
    username        TEXT NOT NULL,
    realname        TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    auto_away       INTEGER NOT NULL DEFAULT 0
                    CHECK (auto_away IN (0, 1)),
    keep_nick       INTEGER NOT NULL DEFAULT 0
                    CHECK (keep_nick IN (0, 1))
) STRICT;
"#;

/// The `networks` table as schema 5 creates it.
///
/// `auto_away` and `keep_nick` are Operator presence policy, constrained to 0 or 1 for
/// the same reason `detached` is: an unrecognized value would be read back as an unknown
/// policy, and neither guess is the Operator's decision.
///
/// Both default to 0. An existing Network that gains these columns therefore keeps
/// emitting exactly the upstream traffic it emitted before, which is the point: a
/// presence policy that switched itself on because the binary was upgraded would look,
/// from upstream, indistinguishable from the bouncer misbehaving.
///
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

/// The `desired_channels` table as schema 1, 2, and 3 created it.
///
/// Retained verbatim so the v3 -> v4 migration rebuilds exactly the representation it
/// is replacing rather than one reconstructed from a newer declaration.
const DESIRED_CHANNELS_V3: &str = r#"
CREATE TABLE desired_channels (
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    casemap_key     BLOB NOT NULL,
    target          TEXT NOT NULL,
    position        INTEGER NOT NULL,
    PRIMARY KEY (network_id, casemap_key)
) STRICT;
"#;

/// The `desired_channels` table as schema 4 creates it.
///
/// `detached` is a bouncer-owned *presentation* decision about a channel that is still
/// desired and still joined upstream. It is constrained to 0 or 1 at the storage layer
/// because a value outside that range would be read back as an unknown policy, and the
/// store refuses to guess which end of an unrecognized value was meant.
const DESIRED_CHANNELS_V4: &str = r#"
CREATE TABLE desired_channels (
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    casemap_key     BLOB NOT NULL,
    target          TEXT NOT NULL,
    position        INTEGER NOT NULL,
    detached        INTEGER NOT NULL DEFAULT 0
                    CHECK (detached IN (0, 1)),
    PRIMARY KEY (network_id, casemap_key)
) STRICT;
"#;

/// Tables that are identical in every schema version, after `desired_channels` and
/// before `history_events`.
const SCHEMA_TABLES: &str = r#"
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

/// Tables that are identical in every schema version, before `desired_channels`.
const SCHEMA_HEAD: &str = r#"
CREATE TABLE network_secrets (
    network_id      INTEGER PRIMARY KEY
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    sasl_username   TEXT NOT NULL,
    sasl_password   BLOB NOT NULL
) STRICT;
"#;

/// Composes a complete schema from its shared parts.
///
/// `concat!` cannot reference a const, so the pieces are joined at runtime instead.
/// That keeps one definition of every unchanged table rather than duplicating all
/// eight tables per version.
fn compose(desired_channels: &str, networks: &str, history: &str, tail: &str) -> String {
    let mut sql = String::with_capacity(
        SCHEMA_HEAD.len()
            + desired_channels.len()
            + networks.len()
            + history.len()
            + tail.len()
            + HISTORY_EVENTS_INDEX.len(),
    );
    sql.push_str(SCHEMA_HEAD);
    sql.push_str(desired_channels);
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
    compose(
        DESIRED_CHANNELS_V3,
        NETWORKS_V2,
        HISTORY_EVENTS_V1,
        SCHEMA_TAIL,
    )
}

/// Schema 2, used to build the fixture the schema 3 migration must handle.
///
/// It is a real declaration rather than "v3 minus a column" so the fixture cannot
/// silently drift into describing a shape this build never actually wrote.
pub(crate) fn schema_v2() -> String {
    compose(
        DESIRED_CHANNELS_V3,
        NETWORKS_V2,
        HISTORY_EVENTS_V2,
        SCHEMA_TAIL,
    )
}

/// Schema 3, used to build the fixture the schema 4 migration must handle.
pub(crate) fn schema_v3() -> String {
    compose(
        DESIRED_CHANNELS_V3,
        NETWORKS_V3,
        HISTORY_EVENTS_V2,
        SCHEMA_TAIL,
    )
}

/// Schema 4, used to build the fixture the schema 5 migration must handle.
pub(crate) fn schema_v4() -> String {
    compose(
        DESIRED_CHANNELS_V4,
        NETWORKS_V3,
        HISTORY_EVENTS_V2,
        SCHEMA_TAIL,
    )
}

/// Schema 5, used to build the fixture the schema 6 migration must handle.
///
/// Retained verbatim so the migration rebuilds exactly the representation it is
/// replacing, rather than one reconstructed from a newer declaration.
pub(crate) fn schema_v5() -> String {
    compose(
        DESIRED_CHANNELS_V4,
        NETWORKS_V5,
        HISTORY_EVENTS_V2,
        SCHEMA_TAIL,
    )
}

/// The effective-time column schema 6 adds to `history_events`.
///
/// `server_time` alone cannot answer a timestamp reference: an upstream that never sends
/// `server-time` leaves it NULL for every event, so an index over it would be empty for
/// the whole buffer and every `timestamp=` reference would fail against a buffer full of
/// history.
///
/// `effective_time` is the time the event *occupies* in history — the upstream's stamp
/// when it sent one, and otherwise the local receive time converted to the same canonical
/// text. It is derived once at append time and never recomputed, so the index and the
/// replay path cannot disagree about where an event sits.
///
/// It is deliberately a separate column rather than a write into `server_time`: that
/// column records what the upstream actually said, and inventing a value there would turn
/// "the upstream sent no timestamp" into a false claim.
///
/// Added with a default rather than as a rebuild, so event ids, cursors and read markers
/// keep pointing at exactly the rows they did before.
const HISTORY_EFFECTIVE_TIME_V6: &str = r#"
ALTER TABLE history_events ADD COLUMN effective_time TEXT NOT NULL DEFAULT '';
"#;

/// The relational indexes schema 6 adds.
///
/// Both exist to stop a reference lookup from being a scan. `effective_time` is indexed
/// rather than `server_time` because it is the value every reference is compared against.
/// `msgid` is indexed with `network_id` because an upstream id is only meaningful within
/// the Network that issued it.
const HISTORY_REFERENCE_INDEXES_V6: &str = r#"
CREATE INDEX history_by_time ON history_events (buffer_id, effective_time, event_id);
CREATE INDEX history_by_msgid ON history_events (network_id, msgid);
"#;

/// The FTS5 side index schema 6 adds.
///
/// This is an *index*, not a source of truth: `history_events` remains the only place a
/// retained event lives, and this table is rebuilt from it whenever it is missing.
///
/// The rowid is the `HistoryEventId`, which is what makes retention exact. Deleting a
/// retained row and deleting its index entry become one statement each rather than a join
/// that could half-succeed, and an index row can never name an event that does not exist.
const HISTORY_SEARCH_V6: &str = r#"
CREATE VIRTUAL TABLE history_search USING fts5(
    sender,
    target,
    body,
    tokenize = 'unicode61 remove_diacritics 2'
);
"#;

/// Schema 6, used to build the fixture the schema 7 migration must handle.
///
/// Declared as its own shape rather than `schema_v7()` minus a table, for the same reason
/// every earlier version is: a migration fixture has to describe a database this build
/// actually wrote. A v6 database is "v5 plus the 6 additions" and *not* "v7 less the
/// registration-action table", and the difference matters the moment a future migration
/// changes the tail.
pub(crate) fn schema_v6() -> String {
    format!(
        "{}{}{}{}",
        compose(
            DESIRED_CHANNELS_V4,
            NETWORKS_V5,
            HISTORY_EVENTS_V2,
            SCHEMA_TAIL,
        ),
        HISTORY_EFFECTIVE_TIME_V6,
        HISTORY_REFERENCE_INDEXES_V6,
        HISTORY_SEARCH_V6,
    )
}

/// The current schema, created directly when no database exists yet.
pub(crate) fn schema_v7() -> String {
    format!("{}{}", schema_v6(), REGISTRATION_ACTIONS_V7)
}

/// The current schema, created directly when no database exists yet.
pub(crate) fn schema_v8() -> String {
    format!("{}{}", schema_v7(), REGISTRATION_ACTIONS_V8)
}

const BUFFER_PRIVACY_V9: &str = r#"
CREATE TABLE buffer_privacy (
    buffer_id INTEGER PRIMARY KEY REFERENCES buffers(buffer_id) ON DELETE CASCADE,
    policy TEXT NOT NULL CHECK (policy IN ('persistent','ephemeral','no-history')),
    max_age_secs INTEGER CHECK (max_age_secs IS NULL OR max_age_secs BETWEEN 1 AND 31536000),
    max_events INTEGER CHECK (max_events IS NULL OR max_events BETWEEN 1 AND 1000000),
    max_bytes INTEGER CHECK (max_bytes IS NULL OR max_bytes BETWEEN 1 AND 1073741824),
    purge_pending INTEGER NOT NULL DEFAULT 0 CHECK (purge_pending IN (0,1)),
    CHECK (policy = 'persistent' OR (max_age_secs IS NULL AND max_events IS NULL AND max_bytes IS NULL))
) STRICT;
"#;

pub(crate) fn schema_v9() -> String {
    format!("{}{}", schema_v8(), BUFFER_PRIVACY_V9)
}

/// Fills in the derived state migration 6 added, for a schema 6 fixture.
///
/// A genuine v6 database has its `effective_time` populated and its FTS side index built,
/// because migration 6 is what did that to every row already retained. A fixture that
/// declared the columns and then left them empty would still pass the 6 -> 7 migration --
/// which is additive and reads neither -- while proving nothing about the state an
/// actually-migrated database is in when a later reader opens it.
///
/// It calls the same two backfills the migration calls, rather than hand-written SQL that
/// would agree with them today and drift from them the first time either changed.
pub(crate) fn seed_schema_v6(connection: &mut Connection) {
    let tx = connection
        .transaction()
        .expect("fixture transaction begins");
    backfill_search_index(&tx).expect("the fixture backfills its search index");
    backfill_effective_time(&tx).expect("the fixture backfills effective time");
    tx.commit().expect("fixture transaction commits");
}

/// Every FTS5 feature this schema needs must be present in the linked SQLite.
///
/// Checked at migration and open time rather than assumed from the dependency list. A
/// build without FTS5 would otherwise create a database that advertises a searchable
/// history it cannot search, which is precisely the "silently return incomplete search as
/// complete" failure the plan forbids.
pub(crate) fn verify_search_support(connection: &Connection) -> Result<(), StoreError> {
    let enabled: i64 = connection
        .query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    if enabled != 1 {
        return Err(StoreError::new(StoreErrorKind::Open));
    }
    Ok(())
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
            verify_promised_columns(connection)?;
            verify_search_support(connection)?;
            verify_search_index(connection)?;
            Ok(OpenDisposition::Current)
        }
        OpenDisposition::Migrated { from } => {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            migrate_forward(&transaction, from)?;
            verify_promised_tables(&transaction)?;
            verify_promised_columns(&transaction)?;
            verify_search_support(&transaction)?;
            verify_search_index(&transaction)?;
            transaction
                .commit()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            Ok(OpenDisposition::Migrated { from })
        }
        OpenDisposition::Fresh => {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            verify_search_support(&transaction)?;
            transaction
                .execute_batch(&schema_v9())
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
///
/// An FTS5 virtual table is backed by several shadow tables (`…_data`, `…_idx`,
/// `…_docsize`, `…_content`, `…_config`). They are SQLite's own storage for the index,
/// not tables this schema promises, so they are excluded -- listing them would make the
/// promised table set depend on an implementation detail of the SQLite build in use.
pub(crate) fn table_names(connection: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master
         WHERE type='table'
           AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE 'history_search_%'
         ORDER BY name",
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
///
/// This is deliberately the *whole* promised set rather than the tables the M001-M004
/// migration path happened to touch. A database missing `registration_actions` or
/// `network_secrets` declares the current version perfectly well -- `user_version` is a
/// header, not a proof -- and would open successfully, only to fail later: every stored
/// credential unreadable, every action replay refused. Refusing at open turns a runtime
/// failure into a refusal the Operator sees at startup, which is the only place they can
/// act on it.
const REQUIRED_TABLES: &[&str] = &[
    "networks",
    "desired_channels",
    "history_events",
    "client_cursors",
    "buffers",
    "read_markers",
    "clients",
    "network_secrets",
    // The FTS5 side index. It is a virtual table, so it appears in `sqlite_master`
    // alongside ordinary tables and can be checked the same way.
    "history_search",
    // Schema 7's registration actions. Purely additive when it was introduced, and holding
    // payloads that may be service credentials, so its absence must be an open failure
    // rather than a per-request one.
    "registration_actions",
    "buffer_privacy",
];

/// Indexes this build promises, beyond the presence of their table.
///
/// A missing search index is not a degraded feature; it is a database that would answer
/// searches from nothing and report no matches. Detecting it at open time is what turns
/// that into a refusal instead of a silent lie.
const REQUIRED_INDEXES: &[&str] = &["history_by_time", "history_by_msgid"];

/// Confirms the search index is present and consistent with retained history.
///
/// Presence alone is not enough: an index that exists but has lost rows would still
/// answer "no matches". Comparing row counts is the cheapest check that distinguishes
/// "searchable" from "silently empty", and a mismatch refuses the open rather than
/// serving an incomplete result as complete.
fn verify_search_index(connection: &Connection) -> Result<(), StoreError> {
    for index in REQUIRED_INDEXES {
        let present: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [index],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        if present != 1 {
            return Err(StoreError::new(StoreErrorKind::Corrupt(
                "missing search index",
            )));
        }
    }
    let indexed: i64 = connection
        .query_row("SELECT count(*) FROM history_search", [], |row| row.get(0))
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    let searchable: i64 = connection
        .query_row(
            "SELECT count(*) FROM history_events WHERE event_class IN ('PRIVMSG','NOTICE')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    if indexed != searchable {
        return Err(StoreError::new(StoreErrorKind::Corrupt(
            "search index disagrees with retained history",
        )));
    }
    Ok(())
}

/// Columns this build promises, beyond the mere presence of their table.
///
/// A table that survived a migration without one of its promised columns would be
/// served as though the policy it carries were absent. That is the failure mode schema
/// version 4 exists to prevent, so the column is checked rather than assumed from the
/// table being there.
const REQUIRED_COLUMNS: &[(&str, &str)] = &[
    ("desired_channels", "detached"),
    ("networks", "auto_away"),
    ("networks", "keep_nick"),
    // Without this column every timestamp reference against a buffer whose upstream
    // sends no `server-time` resolves against an empty index.
    ("history_events", "effective_time"),
    ("registration_actions", "phase"),
];

/// Confirms every promised column is present on its table.
fn verify_promised_columns(connection: &Connection) -> Result<(), StoreError> {
    for (table, column) in REQUIRED_COLUMNS {
        let present: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
                (table, column),
                |row| row.get(0),
            )
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        if present != 1 {
            return Err(StoreError::new(StoreErrorKind::Corrupt(
                "missing schema column",
            )));
        }
    }
    Ok(())
}

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
            3 => migrate_3_to_4(transaction)?,
            4 => migrate_4_to_5(transaction)?,
            5 => migrate_5_to_6(transaction)?,
            6 => migrate_6_to_7(transaction)?,
            7 => migrate_7_to_8(transaction)?,
            8 => migrate_8_to_9(transaction)?,
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

/// Adds the bouncer-owned `desired_channels.detached` column.
///
/// Existing rows are marked attached. That is the only safe default: a channel that
/// was joined before this build existed has been presented downstream this whole time,
/// and silently marking it detached would remove a channel from a client's view
/// without anyone having asked for it.
///
/// `ALTER TABLE ... ADD COLUMN` cannot rebuild the table the way a full rebuild would,
/// so this deliberately keeps the existing column order and appends one NOT NULL column
/// with a constant default. SQLite permits NOT NULL on an added column exactly when the
/// default is not NULL, which is what makes the one-statement migration sound: every
/// row is visible to a reader as attached the instant the statement succeeds, and the
/// surrounding transaction means a reader never sees a half-added column.
fn migrate_3_to_4(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    transaction
        .execute_batch(
            "ALTER TABLE desired_channels
                 ADD COLUMN detached INTEGER NOT NULL DEFAULT 0
                 CHECK (detached IN (0, 1));",
        )
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// Adds the two Operator presence policies to `networks`.
///
/// Both columns default to disabled, which is the only safe migration: a Network that
/// begins emitting `AWAY` or reclaim `NICK` traffic after a binary upgrade has changed
/// its upstream behaviour without the Operator asking, and the change would be
/// indistinguishable from the bouncer misbehaving. Enabling either policy is an explicit
/// configuration change, recorded like any other.
///
/// The step is two `ADD COLUMN` statements in the one migration transaction, for the same
/// reason the v3 -> v4 step is a single statement: SQLite re-checks the whole row, and a
/// half-applied schema is worse than no migration at all.
fn migrate_4_to_5(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "ALTER TABLE networks ADD COLUMN auto_away INTEGER NOT NULL DEFAULT 0
             CHECK (auto_away IN (0, 1));
         ALTER TABLE networks ADD COLUMN keep_nick INTEGER NOT NULL DEFAULT 0
             CHECK (keep_nick IN (0, 1));",
    )
    .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// The schema 5 -> 6 step: effective history time, search indexes, and a bounded
/// backfill of existing history.
///
/// Four steps in one transaction, in dependency order: the column first, because the
/// index below is defined over it; then the indexes and the virtual table; then the
/// backfill. The backfill copies at most `MIGRATION_BATCH_ROWS` rows per step so a large
/// retained journal is never materialized at once, and the whole step is still one
/// transaction: a partially backfilled index is worse than none, because it would answer
/// searches with results that silently stop partway through.
///
/// Only `PRIVMSG` and `NOTICE` are indexed. That is the searchable surface this build
/// offers, and indexing anything else would be work whose results could never be shown.
fn migrate_5_to_6(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    verify_search_support(tx)?;
    tx.execute_batch(HISTORY_EFFECTIVE_TIME_V6)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.execute_batch(HISTORY_REFERENCE_INDEXES_V6)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.execute_batch(HISTORY_SEARCH_V6)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    backfill_search_index(tx)?;
    backfill_effective_time(tx)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// The registration-action table schema 7 adds.
///
/// The payload is a `TEXT` blob that may hold a service password, which is why it is read
/// straight into a [`crate::StoredSecret`] and never into an ordinary `String` on the way
/// out. Storage cannot enforce that; the read path can, and does.
///
/// `position` is part of the primary key because replay order is part of the meaning: an
/// Operator who configures two actions wants them in the order they wrote them, and a row
/// store that returned them in an unspecified order would replay them in an arbitrary one.
/// The `CHECK` on `kind` keeps the table to the allowlist's two entries, so a row written by
/// a future build cannot be read back as an unknown kind and guessed at.
const REGISTRATION_ACTIONS_V7: &str = r#"
CREATE TABLE registration_actions (
    network_id      INTEGER NOT NULL
                    REFERENCES networks(network_id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('mode', 'message')),
    target          TEXT NOT NULL,
    payload         TEXT NOT NULL,
    PRIMARY KEY (network_id, position)
) STRICT;
"#;

/// Migrates schema 6 to schema 7 by adding the registration-action table.
///
/// Purely additive: no existing table is read, written, or rebuilt. That is why this
/// migration can be trivially correct -- there is no data to misinterpret -- and it is also
/// why an older bouncer binary pointed at a migrated database still sees exactly the
/// configuration it had before, just without the new table's rows being replayed.
fn migrate_6_to_7(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(REGISTRATION_ACTIONS_V7)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// Schema 8 adds an explicit execution phase. Existing durable actions were replayed after
/// desired JOINs, so the default preserves their established behavior exactly.
const REGISTRATION_ACTIONS_V8: &str = r#"
ALTER TABLE registration_actions
    ADD COLUMN phase TEXT NOT NULL DEFAULT 'post-join'
    CHECK (phase IN ('pre-join', 'post-join', 'fallback-recovery'));
"#;

fn migrate_7_to_8(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(REGISTRATION_ACTIONS_V8)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

fn migrate_8_to_9(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(BUFFER_PRIVACY_V9)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    Ok(())
}

/// Fills `effective_time` for every retained row, in bounded batches.
///
/// Runs after the column is added and before the database is served, because a row left
/// at the empty default would sort *before* every real timestamp and become the oldest
/// message in its buffer — a wrong answer that looks like a correct one.
fn backfill_effective_time(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    let mut cursor: i64 = 0;
    loop {
        let rows: Vec<(i64, Option<String>, i64)> = {
            let mut statement = tx
                .prepare(
                    "SELECT event_id, server_time, received_at FROM history_events
                     WHERE event_id > ?1
                     ORDER BY event_id LIMIT ?2",
                )
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            let mapped = statement
                .query_map(rusqlite::params![cursor, MIGRATION_BATCH_ROWS], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            let mut collected = Vec::new();
            for row in mapped {
                collected.push(row.map_err(|_| StoreError::new(StoreErrorKind::Open))?);
            }
            collected
        };
        if rows.is_empty() {
            return Ok(());
        }
        for (event_id, server_time, received_at) in &rows {
            let effective = crate::search::stored_effective_time(
                server_time.as_deref(),
                i2pr_irc_core::WallTime(*received_at),
            )
            .ok_or_else(|| StoreError::new(StoreErrorKind::Corrupt("history timestamp")))?;
            tx.execute(
                "UPDATE history_events SET effective_time = ?1 WHERE event_id = ?2",
                rusqlite::params![effective, *event_id],
            )
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        }
        cursor = rows
            .last()
            .map(|(event_id, ..)| *event_id)
            .ok_or_else(|| StoreError::new(StoreErrorKind::Open))?;
    }
}

/// Copies retained searchable events into the side index, in bounded batches.
///
/// Rows are selected in ascending `HistoryEventId` and the cursor advances past whatever
/// was copied, so each batch is a range rather than a rescan. The search text is derived
/// from the stored payload by the same bounded decoder ingestion uses; a payload that
/// cannot yield text is indexed as empty rather than dropped, because dropping it would
/// leave the index count disagreeing with the retained rows.
fn backfill_search_index(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    let mut cursor: i64 = 0;
    loop {
        let rows: Vec<(i64, Vec<u8>)> = {
            let mut statement = tx
                .prepare(
                    "SELECT event_id, payload FROM history_events
                     WHERE event_id > ?1 AND event_class IN ('PRIVMSG','NOTICE')
                     ORDER BY event_id LIMIT ?2",
                )
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            let mapped = statement
                .query_map(rusqlite::params![cursor, MIGRATION_BATCH_ROWS], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
            let mut collected = Vec::new();
            for row in mapped {
                collected.push(row.map_err(|_| StoreError::new(StoreErrorKind::Open))?);
            }
            collected
        };
        if rows.is_empty() {
            return Ok(());
        }
        for (event_id, payload) in &rows {
            let (sender, target, body) = crate::search::derive_fields(payload)?;
            crate::search::insert_search_row(tx, *event_id, sender, target, body)?;
        }
        cursor = rows
            .last()
            .map(|(event_id, ..)| *event_id)
            .ok_or_else(|| StoreError::new(StoreErrorKind::Open))?;
    }
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
