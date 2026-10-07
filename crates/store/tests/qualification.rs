//! Plan 007 storage qualification.
//!
//! These tests are deliberately outside the store crate's unit tests: they exercise
//! the public typed surface exactly as M003-B and later plans will, and they read the
//! raw database through [`i2pr_irc_store::testing`] so migration and restart evidence
//! never depends on the same API that is under test.
use i2pr_irc_core::{BufferId, I2pEndpoint, WallTime};
use i2pr_irc_store::{
    BufferKind, EventDirection, HistoryAround, HistoryEventId, MAX_DISPLAY_NAME_BYTES,
    MAX_HISTORY_QUERY_EVENTS, MAX_SEARCH_BUFFERS, MAX_SEARCH_RESULTS, MAX_SEARCH_TERMS,
    MsgidLookup, NetworkId, NetworkRecord, NewHistoryEvent, RegistrationActionKind,
    RetentionRequest, SCHEMA_VERSION, STORE_QUEUE_CAPACITY, SearchFields, SearchQuery, SearchTerm,
    Store, StoreErrorKind, StoreHandle, StoreHealth, StorePath, StoredRegistrationAction,
    StoredSecret, attached_channels, fallback_display_name,
    testing::{self, EXPECTED_TABLES},
};
use i2pr_irc_wire::IrcTimestamp;

fn store_at(path: &std::path::Path) -> Store {
    Store::open(&StorePath::File(path.to_path_buf())).expect("store opens")
}

fn record(network: u64, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: fallback_display_name(NetworkId(network)),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "bouncer".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: attached_channels(
            &channels
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        ),
    }
}

// ---------------------------------------------------------------- schema/open

#[test]
fn fresh_database_creates_exactly_the_current_schema() {
    let dir = testing::temp_dir("fresh");
    let path = dir.db("fresh.sqlite3");
    let store = store_at(&path);
    assert_eq!(
        testing::tables(&path),
        EXPECTED_TABLES.to_vec(),
        "the table list is the schema's own promise, not an incidental detail"
    );
    assert_eq!(
        testing::identity(&path),
        (
            i2pr_irc_store::APPLICATION_ID,
            i2pr_irc_store::SCHEMA_VERSION
        )
    );
    assert_eq!(
        testing::column_type(&path, "history_events", "server_time"),
        "TEXT",
        "schema 2 stores the protocol timestamp as canonical text"
    );
    assert!(
        testing::foreign_keys_enabled(&path),
        "foreign keys must be on before any request is served"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn reopening_a_current_database_reuses_it() {
    let dir = testing::temp_dir("reopen");
    let path = dir.db("reopen.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    let before = testing::tables(&path);
    let store = store_at(&path);
    assert_eq!(
        testing::tables(&path),
        before,
        "a reopen does not re-create schema"
    );
    store.shutdown().expect("reopened store shuts down");
}

#[test]
fn an_unrecognized_database_is_refused_at_startup() {
    let dir = testing::temp_dir("foreign");
    let path = dir.db("foreign.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    // Another application owns this file.
    testing::stamp(&path, 0x1234_5678, 1);
    assert_eq!(
        Store::open(&StorePath::File(path.clone()))
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::ForeignDatabase)
    );
}

#[test]
fn a_newer_schema_version_is_refused_at_startup() {
    let dir = testing::temp_dir("toonew");
    let path = dir.db("toonew.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    testing::stamp(
        &path,
        i2pr_irc_store::APPLICATION_ID,
        i2pr_irc_store::SCHEMA_VERSION + 1,
    );
    assert_eq!(
        Store::open(&StorePath::File(path.clone()))
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::SchemaTooNew),
        "a newer database must be refused, never downgraded or reinterpreted"
    );
}

#[test]
fn a_database_claiming_our_version_but_missing_tables_is_refused() {
    let dir = testing::temp_dir("corrupt");
    let path = dir.db("corrupt.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    // A tampered database: right identity, wrong shape.
    testing::execute(&path, "DROP TABLE history_events");
    assert_eq!(
        Store::open(&StorePath::File(path.clone()))
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::Corrupt("missing schema table"))
    );
}

#[test]
fn strict_tables_reject_a_wrong_storage_class() {
    let dir = testing::temp_dir("strict");
    let path = dir.db("strict.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    // STRICT tables must refuse a TEXT value in an INTEGER column rather than
    // coercing it into a row that later means something different.
    let error = testing::expect_rejected(
        &path,
        "INSERT INTO buffers (network_id, kind, canonical_key, target)
         VALUES ('not-an-integer', 0, x'00', '#room')",
    );
    assert!(
        error
            .to_string()
            .contains("cannot store TEXT value in INTEGER column"),
        "expected a STRICT storage-class rejection, got: {error}"
    );
}

// ------------------------------------------------------- schema 1 -> 2 migration

/// Builds a v1 database holding `events` history rows.
///
/// Each entry is `(event_id, received_at, server_time)`; `server_time` is the v1
/// whole-second representation and is inserted verbatim.
fn v1_fixture(path: &std::path::Path, events: &[(i64, i64, Option<i64>)]) {
    let connection = testing::create_v1_database(path);
    connection
        .execute_batch(
            "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username, realname)
                  VALUES (1, 'irc.example.i2p', 0, 'bot', 'user', 'bouncer')",
        )
        .expect("network fixture applies");
    connection
        .execute_batch(
            "INSERT INTO buffers (buffer_id, network_id, kind, canonical_key, target)
                  VALUES (1, 1, 0, x'23', '#room')",
        )
        .expect("buffer fixture applies");
    for (event_id, received_at, server_time) in events {
        connection
            .execute(
                "INSERT INTO history_events
                    (event_id, network_id, buffer_id, received_at, server_time, msgid, direction, event_class, payload)
                 VALUES (?1, 1, 1, ?2, ?3, ?4, 0, 'PRIVMSG', ?5)",
                rusqlite::params![
                    event_id,
                    received_at,
                    server_time,
                    format!("msg-{event_id}"),
                    format!(":a!b@c PRIVMSG #room :m{event_id}").into_bytes(),
                ],
            )
            .expect("history fixture applies");
    }
}

#[test]
fn a_schema_one_database_is_migrated_to_two_on_open() {
    let dir = testing::temp_dir("migrate");
    let path = dir.db("migrate.sqlite3");
    // 1546612406 seconds is 2019-01-04T14:33:26Z, the timestamp used in the
    // IRCv3 chathistory specification examples.
    v1_fixture(&path, &[(1, 1_546_612_400, Some(1_546_612_406))]);
    assert_eq!(testing::identity(&path).1, 1, "fixture is at schema 1");

    let store = store_at(&path);
    assert_eq!(
        testing::identity(&path).1,
        SCHEMA_VERSION,
        "open migrates forward to the current schema"
    );

    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1",
            &[],
        )
        .as_deref(),
        Some("2019-01-04T14:33:26.000Z"),
        "a v1 whole-second value becomes a canonical millisecond timestamp"
    );
    assert_eq!(
        testing::count(&path, "SELECT count(*) FROM history_events"),
        1,
        "migration must not lose rows"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn migration_preserves_event_ids_cursors_and_read_markers() {
    let dir = testing::temp_dir("identity");
    let path = dir.db("identity.sqlite3");
    v1_fixture(
        &path,
        &[(7, 100, Some(100)), (8, 200, Some(200)), (9, 300, None)],
    );
    {
        let connection = testing::raw(&path);
        connection
            .execute_batch("INSERT INTO clients (client_id, login) VALUES (1, 'operator')")
            .expect("client fixture applies");
        connection
            .execute_batch(
                "INSERT INTO client_cursors (client_id, buffer_id, event_id)
                      VALUES (1, 1, 8)",
            )
            .expect("cursor fixture applies");
        connection
            .execute_batch("INSERT INTO read_markers (buffer_id, event_id) VALUES (1, 7)")
            .expect("marker fixture applies");
    }

    let store = store_at(&path);
    assert_eq!(
        testing::optional_i64(
            &path,
            "SELECT event_id FROM client_cursors WHERE client_id = 1",
            &[],
        ),
        Some(8),
        "a durable cursor must still point at the same event"
    );
    assert_eq!(
        testing::optional_i64(&path, "SELECT event_id FROM read_markers", &[]),
        Some(7),
        "a durable read marker must still point at the same event"
    );
    assert_eq!(
        testing::optional_i64(
            &path,
            "SELECT event_id FROM history_events WHERE msgid = 'msg-8'",
            &[],
        ),
        Some(8),
        "event identity is stable across migration"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn migration_keeps_autoincrement_monotonic_so_a_cursor_cannot_alias() {
    let dir = testing::temp_dir("monotonic");
    let path = dir.db("monotonic.sqlite3");
    v1_fixture(&path, &[(4, 1, None), (11, 2, None), (29, 3, None)]);

    let store = store_at(&path);
    assert_eq!(
        testing::autoincrement_sequence(&path, "history_events"),
        Some(29),
        "the sequence must carry the highest migrated id forward"
    );

    // A new event must be allocated strictly above every previously issued id,
    // otherwise a retained cursor could later alias a different event.
    let buffer = store
        .handle()
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves")
        .buffer;
    let appended = store
        .handle()
        .append_history(&[NewHistoryEvent {
            network: NetworkId(1),
            buffer,
            received_at: WallTime(9),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b":a!b@c PRIVMSG #room :after-migration".to_vec(),
            search: None,
        }])
        .await
        .expect("append succeeds")
        .last
        .expect("identity assigned");
    assert!(
        appended.0 > 29,
        "event {appended:?} must exceed every pre-migration id so no retained cursor aliases it"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn migration_moves_every_row_across_the_batched_boundary() {
    let dir = testing::temp_dir("batched");
    let path = dir.db("batched.sqlite3");
    // More rows than the migration's internal batch size, so the copy genuinely
    // iterates instead of succeeding in one step.
    let rows: Vec<(i64, i64, Option<i64>)> = (1..=1_000)
        .map(|id| (id, 1_000 + id, Some(1_546_612_406)))
        .collect();
    v1_fixture(&path, &rows);

    let store = store_at(&path);
    assert_eq!(
        testing::count(&path, "SELECT count(*) FROM history_events"),
        1_000
    );
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1000",
            &[],
        )
        .as_deref(),
        Some("2019-01-04T14:33:26.000Z"),
        "the final row of the last batch is converted too"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn a_null_server_time_stays_absent_rather_than_becoming_invented() {
    let dir = testing::temp_dir("nulltime");
    let path = dir.db("nulltime.sqlite3");
    v1_fixture(&path, &[(1, 500, None)]);
    let store = store_at(&path);
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1",
            &[],
        ),
        None,
        "an absent upstream timestamp must not be fabricated from local time"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn a_v1_server_time_outside_the_protocol_window_migrates_as_absent() {
    let dir = testing::temp_dir("outofrange");
    let path = dir.db("outofrange.sqlite3");
    // v1 accepted any integer seconds within +/-32.5e9. The negative end of that
    // window still lands inside the four-digit year range (it reaches year 940),
    // so the interesting case is a value the protocol window cannot express at
    // all -- well before year 1, which no conformant server-time could denote.
    v1_fixture(
        &path,
        &[
            (1, 500, Some(-100_000_000_000)),
            (2, 501, Some(-32_503_680_000)),
            (3, 502, None),
        ],
    );
    let store = store_at(&path);
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1",
            &[],
        ),
        None,
        "an unrepresentable v1 timestamp becomes absent"
    );
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 2",
            &[],
        )
        .as_deref(),
        Some("0940-01-01T00:00:00.000Z"),
        "a v1 value that is still representable is converted, not dropped"
    );
    assert_eq!(
        testing::count(&path, "SELECT count(*) FROM history_events"),
        3,
        "every row is retained; only an inexpressible timestamp is dropped"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn the_v2_timestamp_column_rejects_a_non_conformant_shape() {
    let dir = testing::temp_dir("shape");
    let path = dir.db("shape.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    let error = testing::expect_rejected(
        &path,
        "INSERT INTO history_events
            (network_id, buffer_id, received_at, server_time, direction, event_class, payload)
         VALUES (1, 1, 0, '1700000000', 0, 'PRIVMSG', x'00')",
    );
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "expected the storage-level shape check to reject an integer epoch, got: {error}"
    );
}

#[test]
fn a_migration_that_fails_leaves_the_version_one_database_intact() {
    let dir = testing::temp_dir("rollback");
    let path = dir.db("rollback.sqlite3");
    v1_fixture(&path, &[(1, 1_000, Some(1_546_612_406)), (2, 2_000, None)]);

    // Occupy the name the migration needs for its staging table, so the migration
    // fails partway. The store must refuse rather than serve a half-migrated file.
    testing::execute(&path, "CREATE TABLE history_events_v2 (occupied INTEGER)");

    assert!(
        Store::open(&StorePath::File(path.clone())).is_err(),
        "a migration that cannot complete must not succeed"
    );
    assert_eq!(
        testing::identity(&path).1,
        1,
        "a failed migration must leave the version one database untouched"
    );
    assert_eq!(
        testing::count(
            &path,
            "SELECT count(*) FROM history_events WHERE msgid = 'msg-1'"
        ),
        1,
        "the original v1 rows are still present and readable"
    );
    assert_eq!(
        testing::optional_i64(&path, "SELECT occupied FROM history_events_v2", &[]),
        None,
        "the obstructing table is not consumed or partially populated"
    );

    // Once the obstruction is cleared the same file migrates cleanly, proving the
    // failure rolled back rather than leaving a permanently poisoned database.
    testing::execute(&path, "DROP TABLE history_events_v2");
    let store = store_at(&path);
    assert_eq!(testing::identity(&path).1, SCHEMA_VERSION);
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1",
            &[],
        )
        .as_deref(),
        Some("2019-01-04T14:33:26.000Z")
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn reopening_an_already_migrated_database_does_not_migrate_again() {
    let dir = testing::temp_dir("idem");
    let path = dir.db("idem.sqlite3");
    v1_fixture(&path, &[(1, 1_000, Some(1_546_612_406))]);
    store_at(&path).shutdown().expect("first open migrates");
    let after_first = testing::tables(&path);
    store_at(&path)
        .shutdown()
        .expect("second open is a plain reopen");
    assert_eq!(
        testing::tables(&path),
        after_first,
        "migration is not repeated"
    );
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT server_time FROM history_events WHERE event_id = 1",
            &[],
        )
        .as_deref(),
        Some("2019-01-04T14:33:26.000Z"),
        "a second open must not re-truncate an already canonical timestamp"
    );
}

#[test]
fn foreign_keys_reject_an_orphaned_child_row() {
    let dir = testing::temp_dir("fk");
    let path = dir.db("fk.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    // A buffer for a Network that does not exist must be refused by the schema.
    let error = testing::expect_rejected(
        &path,
        "INSERT INTO buffers (network_id, kind, canonical_key, target)
         VALUES (4242, 0, x'00', '#room')",
    );
    assert!(error.to_string().contains("FOREIGN KEY"), "got: {error}");
}

// ------------------------------------------------------- schema 2 -> 3 migration

/// Builds a v2 database holding `networks` rows whose endpoints are identifying.
///
/// The endpoint is deliberately an obviously recognisable string so the test can
/// prove it never reaches the migrated display name.
fn v2_fixture(path: &std::path::Path, networks: &[(i64, &str, &str)]) {
    let connection = testing::create_v2_database(path);
    for (network, nick, endpoint) in networks {
        connection
            .execute(
                "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username, realname)
                 VALUES (?1, ?2, 0, ?3, 'user', 'bouncer')",
                rusqlite::params![network, endpoint, nick],
            )
            .expect("network fixture applies");
    }
}

#[test]
fn a_schema_two_database_is_migrated_to_three_on_open() {
    let dir = testing::temp_dir("m23");
    let path = dir.db("m23.sqlite3");
    v2_fixture(
        &path,
        &[
            (1, "bot", "identifying-endpoint-one.i2p"),
            (7, "other", "identifying-endpoint-seven.i2p"),
        ],
    );
    assert_eq!(testing::identity(&path).1, 2, "fixture is at schema 2");

    let store = store_at(&path);
    assert_eq!(
        testing::identity(&path).1,
        SCHEMA_VERSION,
        "open migrates forward to the current schema"
    );

    let names = testing::texts(
        &path,
        "SELECT display_name FROM networks ORDER BY network_id",
    );
    assert_eq!(
        names,
        vec![
            fallback_display_name(NetworkId(1)),
            fallback_display_name(NetworkId(7)),
        ],
        "every migrated row receives the deterministic NetworkId-derived fallback"
    );
    assert_eq!(
        names,
        vec!["network-1".to_owned(), "network-7".to_owned()],
        "the fallback is derived only from the durable id"
    );
    for endpoint in [
        "identifying-endpoint-one.i2p",
        "identifying-endpoint-seven.i2p",
    ] {
        assert!(
            !names.iter().any(|name| name.contains(endpoint)),
            "the upstream endpoint must never appear in an operator-facing name: {names:?}"
        );
    }
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn the_migrated_display_name_is_readable_through_the_public_api() {
    let dir = testing::temp_dir("m23api");
    let path = dir.db("m23api.sqlite3");
    v2_fixture(&path, &[(1, "bot", "irc.example.i2p")]);

    let store = store_at(&path);
    let records = store.handle().load_networks().await.expect("networks load");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].display_name,
        fallback_display_name(NetworkId(1)),
        "the migrated row loads with its durable fallback name"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn an_operator_chosen_display_name_round_trips_and_survives_reopen() {
    let dir = testing::temp_dir("m23name");
    let path = dir.db("m23name.sqlite3");
    let mut record = record(1, &[]);
    record.display_name = "hidden-service".into();
    store_at(&path)
        .handle()
        .save_network(&record)
        .await
        .expect("record saves");

    let store = store_at(&path);
    let loaded = store.handle().load_networks().await.expect("networks load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded[0].display_name, "hidden-service",
        "an operator-chosen name is durable and is not overwritten by the fallback"
    );
    store.shutdown().expect("store shuts down");
}

#[test]
fn a_display_name_that_could_alter_reply_parsing_is_refused() {
    let mut record = record(1, &[]);
    for rejected in [
        "",
        "has space",
        "has:colon",
        "has,comma",
        "has\nnewline",
        &"x".repeat(MAX_DISPLAY_NAME_BYTES + 1),
    ] {
        record.display_name = rejected.to_owned();
        assert!(
            record.validate().is_err(),
            "a display name that could break operator-facing reply parsing must be refused: {rejected:?}"
        );
    }
    record.display_name = "x".repeat(MAX_DISPLAY_NAME_BYTES);
    assert!(
        record.validate().is_ok(),
        "a name exactly at the ceiling is accepted"
    );
}

#[test]
fn a_failed_v2_to_3_migration_leaves_the_version_two_database_intact() {
    let dir = testing::temp_dir("m23rollback");
    let path = dir.db("m23rollback.sqlite3");
    v2_fixture(&path, &[(1, "bot", "irc.example.i2p")]);

    // A `networks` row that cannot satisfy the column's own shape check does not exist
    // (the column has no CHECK), so the migration is instead broken by occupying the
    // schema with an incompatible `networks` replacement. `ALTER TABLE ADD COLUMN`
    // fails when the column already exists.
    testing::execute(
        &path,
        "ALTER TABLE networks ADD COLUMN display_name TEXT NOT NULL DEFAULT 'x'",
    );
    assert!(
        Store::open(&StorePath::File(path.clone())).is_err(),
        "a migration that cannot complete must not succeed"
    );
    assert_eq!(
        testing::identity(&path).1,
        2,
        "a failed migration must leave the version two database untouched"
    );
    assert_eq!(
        testing::optional_text(
            &path,
            "SELECT display_name FROM networks WHERE network_id = 1",
            &[]
        )
        .as_deref(),
        Some("x"),
        "the pre-existing column is left exactly as it was rather than rewritten"
    );
}

// ------------------------------------------------------------------- identity

#[tokio::test]
async fn durable_identities_survive_reopen() {
    let dir = testing::temp_dir("identity");
    let path = dir.db("identity.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(7, &["#room"]))
        .await
        .expect("network saved");
    let (client, created) = store
        .handle()
        .create_client("phone")
        .await
        .expect("client created");
    assert!(created);
    store.shutdown().expect("store shuts down");

    let reopened = store_at(&path);
    let networks = reopened
        .handle()
        .load_networks()
        .await
        .expect("catalog loads");
    assert_eq!(networks[0].network, NetworkId(7), "NetworkId is durable");
    let (again, created_again) = reopened
        .handle()
        .create_client("phone")
        .await
        .expect("client resolved");
    assert_eq!(again, client, "ClientId is durable across restart");
    assert!(!created_again, "an existing lineage is not re-created");
    reopened.shutdown().expect("reopened store shuts down");
}

#[tokio::test]
async fn distinct_clients_get_distinct_lineages() {
    let dir = testing::temp_dir("clients");
    let path = dir.db("clients.sqlite3");
    let store = store_at(&path);
    let (first, _) = store
        .handle()
        .create_client("phone")
        .await
        .expect("first client");
    let (second, created) = store
        .handle()
        .create_client("laptop")
        .await
        .expect("second client");
    assert!(created);
    assert_ne!(
        first, second,
        "a durable lineage is per client, not per process"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn history_event_identity_is_never_reused_after_deletion() {
    let dir = testing::temp_dir("reuse");
    let path = dir.db("reuse.sqlite3");
    let store = store_at(&path);
    let network = NetworkId(1);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(network, BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves")
        .buffer;
    let event = NewHistoryEvent {
        network,
        buffer,
        received_at: WallTime(1),
        server_time: None,
        msgid: None,
        direction: EventDirection::Inbound,
        event_class: "PRIVMSG".into(),
        payload: b":a!b@c PRIVMSG #room :one".to_vec(),
        search: None,
    };
    let first = store
        .handle()
        .append_history(std::slice::from_ref(&event))
        .await
        .expect("append")
        .last
        .expect("identity assigned");
    let report = store
        .handle()
        .retain(&RetentionRequest {
            network,
            before: HistoryEventId(first.0 + 1),
            max_delete: 16,
        })
        .await
        .expect("retention");
    assert_eq!(report.deleted, 1, "the only event was deleted");
    let second = store
        .handle()
        .append_history(&[event])
        .await
        .expect("second append")
        .last
        .expect("identity assigned");
    assert!(
        second.0 > first.0,
        "reusing an event id would let a retained cursor alias a different event"
    );
    store.shutdown().expect("store shuts down");
    let _ = path;
}

#[tokio::test]
async fn canonical_order_is_local_identity_not_wall_time() {
    let dir = testing::temp_dir("order");
    let path = dir.db("order.sqlite3");
    let store = store_at(&path);
    let network = NetworkId(1);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(network, BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    // Identical receive times and a skewed server time must not reorder history.
    let batch: Vec<NewHistoryEvent> = (0..3)
        .map(|index| NewHistoryEvent {
            network,
            buffer,
            received_at: WallTime(1000),
            // Descending server time, so timestamp order and local order disagree.
            server_time: Some(
                i2pr_irc_wire::IrcTimestamp::from_unix_millis((999 - index) * 1_000)
                    .expect("representable"),
            ),
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: format!(":a!b@c PRIVMSG #room :m{index}").into_bytes(),
            search: None,
        })
        .collect();
    store.handle().append_history(&batch).await.expect("append");
    let events = store
        .handle()
        .query_history(&i2pr_irc_store::HistoryQuery {
            buffer,
            bound: i2pr_irc_store::HistoryQueryBound {
                after: None,
                before: None,
                limit: 10,
            },
        })
        .await
        .expect("query");
    assert_eq!(events.len(), 3);
    let payloads: Vec<String> = events
        .iter()
        .map(|event| String::from_utf8_lossy(&event.payload).into_owned())
        .collect();
    assert_eq!(
        payloads,
        vec![
            ":a!b@c PRIVMSG #room :m0",
            ":a!b@c PRIVMSG #room :m1",
            ":a!b@c PRIVMSG #room :m2"
        ],
        "canonical order is the assigned local sequence, never a timestamp"
    );
    for pair in events.windows(2) {
        assert!(pair[0].event < pair[1].event);
    }
    store.shutdown().expect("store shuts down");
}

// ---------------------------------------------------------------- restart/obs

#[tokio::test]
async fn desired_state_survives_restart_and_no_observed_state_does() {
    let dir = testing::temp_dir("restart");
    let path = dir.db("restart.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &["#alpha", "#beta"]))
        .await
        .expect("network saved");
    store
        .handle()
        .add_desired_channel(NetworkId(1), "#gamma")
        .await
        .expect("channel added");
    store.shutdown().expect("store shuts down");

    let reopened = store_at(&path);
    let networks = reopened
        .handle()
        .load_networks()
        .await
        .expect("catalog loads");
    assert_eq!(
        networks[0].desired_channels,
        attached_channels(&["#alpha", "#beta", "#gamma"]),
        "durable intent survives restart in insertion order"
    );
    // The structural proof that nothing live is stored: schema 1 contains no table,
    // column, or API describing observed membership, generations, or sessions, so
    // there is nothing for a restart to restore.
    for forbidden in [
        "sessions",
        "session",
        "generations",
        "generation",
        "members",
        "topics",
        "modes",
        "join_attempts",
        "routes",
        "self_channels",
        "clients_sessions",
    ] {
        assert!(
            !testing::tables(&path).iter().any(|name| name == forbidden),
            "schema 1 must not carry {forbidden}"
        );
    }
    reopened.shutdown().expect("reopened store shuts down");
}

#[tokio::test]
async fn removing_desired_state_is_durable() {
    let dir = testing::temp_dir("desired-remove");
    let path = dir.db("desired.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &["#alpha", "#beta"]))
        .await
        .expect("network saved");
    assert!(
        store
            .handle()
            .remove_desired_channel(NetworkId(1), "#beta")
            .await
            .expect("removed")
    );
    assert!(
        !store
            .handle()
            .add_desired_channel(NetworkId(1), "#alpha")
            .await
            .expect("re-add is a no-op"),
        "an already-desired channel is not a fresh durable mutation"
    );
    store.shutdown().expect("store shuts down");
    let reopened = store_at(&path);
    let networks = reopened
        .handle()
        .load_networks()
        .await
        .expect("catalog loads");
    assert_eq!(networks[0].desired_channels, attached_channels(&["#alpha"]));
    reopened.shutdown().expect("reopened store shuts down");
}

#[tokio::test]
async fn removing_a_network_cascades_its_durable_children() {
    let dir = testing::temp_dir("cascade");
    let path = dir.db("cascade.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    store
        .handle()
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    assert_eq!(testing::count(&path, "SELECT count(*) FROM buffers"), 1);
    assert!(
        store
            .handle()
            .remove_network(NetworkId(1))
            .await
            .expect("network removed"),
        "a removed Network must not leave orphaned durable rows behind"
    );
    assert_eq!(testing::count(&path, "SELECT count(*) FROM buffers"), 0);
    store.shutdown().expect("store shuts down");
}

// ------------------------------------------------------------------- secrets

#[tokio::test]
async fn secrets_never_appear_in_diagnostics() {
    let dir = testing::temp_dir("secret");
    let path = dir.db("secret.sqlite3");
    let store = store_at(&path);
    let mut with_secret = record(1, &[]);
    with_secret.sasl = Some(("bot".into(), StoredSecret::new("s3cr3t-value".into())));
    store
        .handle()
        .save_network(&with_secret)
        .await
        .expect("network with secret saved");
    let loaded = &store.handle().load_networks().await.expect("catalog loads")[0];
    assert!(
        !format!("{loaded:?}").contains("s3cr3t-value"),
        "a durable record's Debug must never leak the password"
    );
    store.shutdown().expect("store shuts down");

    // The secret survives restart for reconnect, and is still redacted.
    let reopened = store_at(&path);
    let reloaded = &reopened
        .handle()
        .load_networks()
        .await
        .expect("catalog loads")[0];
    assert_eq!(
        reloaded
            .sasl
            .as_ref()
            .expect("secret survived restart")
            .1
            .expose(),
        "s3cr3t-value"
    );
    assert!(!format!("{:?}", reloaded.sasl).contains("s3cr3t-value"));
    reopened.shutdown().expect("reopened store shuts down");
}

// ------------------------------------------------------- bounded ingress/queue

#[tokio::test]
async fn the_ingress_queue_is_bounded_and_overload_is_typed() {
    let dir = testing::temp_dir("queue");
    let path = dir.db("queue.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    assert_eq!(
        handle.queue_capacity(),
        STORE_QUEUE_CAPACITY,
        "the ceiling is explicit, not implicit"
    );

    // Fill the queue from many concurrent callers without awaiting each response.
    // Some are answered, and the rest must be refused with the typed overload
    // error rather than growing an unbounded backlog.
    let mut refusals = 0usize;
    let mut accepted = 0usize;
    let mut tasks = Vec::new();
    for _ in 0..(STORE_QUEUE_CAPACITY * 4) {
        let handle = handle.clone();
        tasks.push(tokio::spawn(async move {
            match handle.health().await {
                Ok(_) => Ok(()),
                Err(error) => Err(*error.kind()),
            }
        }));
    }
    for task in tasks {
        match task.await.expect("task joins") {
            Ok(()) => accepted += 1,
            Err(kind) => {
                assert_eq!(kind, StoreErrorKind::QueueOverloaded);
                refusals += 1;
            }
        }
    }
    assert!(
        accepted > 0 && refusals > 0,
        "expected both acceptance and typed refusal, got {accepted} accepted and {refusals} refused"
    );
    // A flush proves the queue is a real bounded buffer that still drains.
    handle
        .flush()
        .await
        .expect("flush drains every queued request");
    assert_eq!(handle.health().await.expect("ready"), StoreHealth::Ready);
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn shutdown_answers_in_flight_work_and_joins_the_worker() {
    let dir = testing::temp_dir("drain");
    let path = dir.db("drain.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &["#room"]))
        .await
        .expect("network saved");
    // A request issued just before shutdown must resolve explicitly rather than
    // hang: either it committed, or it reports that the store is gone.
    let handle = store.handle_clone();
    let in_flight = tokio::spawn(async move { handle.load_networks().await });
    store.shutdown().expect("store shuts down");
    let _ = in_flight.await.expect("in-flight task joins");
    assert!(
        testing::tables(&path).contains(&"networks".to_owned()),
        "durable work survives shutdown"
    );
}

#[tokio::test]
async fn a_handle_to_a_stopped_store_refuses_new_work() {
    let dir = testing::temp_dir("health");
    let path = dir.db("health.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    assert_eq!(handle.health().await.expect("ready"), StoreHealth::Ready);
    store.shutdown().expect("store shuts down");
    assert_eq!(
        handle.health().await.err().map(|error| *error.kind()),
        Some(StoreErrorKind::Stopped)
    );
}

// ---------------------------------------------------------- cursors/retention

#[tokio::test]
async fn cursors_and_read_markers_move_only_forward() {
    let dir = testing::temp_dir("monotonic");
    let path = dir.db("monotonic.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    let (client, _) = store.handle().create_client("phone").await.expect("client");
    assert_eq!(
        store
            .handle()
            .advance_cursor(client, buffer, HistoryEventId(10))
            .await
            .expect("advance"),
        HistoryEventId(10)
    );
    assert_eq!(
        store
            .handle()
            .advance_cursor(client, buffer, HistoryEventId(5))
            .await
            .expect("stale advance"),
        HistoryEventId(10),
        "a stale acknowledgement must not rewind a cursor"
    );
    assert_eq!(
        store
            .handle()
            .advance_read_marker(buffer, HistoryEventId(20))
            .await
            .expect("marker"),
        HistoryEventId(20)
    );
    assert_eq!(
        store
            .handle()
            .advance_read_marker(buffer, HistoryEventId(1))
            .await
            .expect("stale marker"),
        HistoryEventId(20),
        "a read marker never moves backwards"
    );
    store.shutdown().expect("store shuts down");
    let _ = path;
}

#[tokio::test]
async fn two_clients_keep_independent_cursors_on_one_buffer() {
    let dir = testing::temp_dir("independence");
    let path = dir.db("independence.sqlite3");
    let store = store_at(&path);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    let (phone, _) = store.handle().create_client("phone").await.expect("phone");
    let (laptop, _) = store
        .handle()
        .create_client("laptop")
        .await
        .expect("laptop");
    store
        .handle()
        .advance_cursor(phone, buffer, HistoryEventId(30))
        .await
        .expect("phone cursor");
    assert_eq!(
        store
            .handle()
            .get_cursor(laptop, buffer)
            .await
            .expect("laptop cursor read"),
        None,
        "playback position is per client lineage, not per buffer"
    );
    store.shutdown().expect("store shuts down");
    let _ = path;
}

#[tokio::test]
async fn retention_clamps_positions_that_point_into_the_removed_range() {
    let dir = testing::temp_dir("clamp");
    let path = dir.db("clamp.sqlite3");
    let store = store_at(&path);
    let network = NetworkId(1);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(network, BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    let (client, _) = store.handle().create_client("phone").await.expect("client");
    let mut ids = Vec::new();
    for index in 0..5u64 {
        let appended = store
            .handle()
            .append_history(&[NewHistoryEvent {
                network,
                buffer,
                received_at: WallTime(index as i64),
                server_time: None,
                msgid: None,
                direction: EventDirection::Inbound,
                event_class: "PRIVMSG".into(),
                payload: format!(":a!b@c PRIVMSG #room :m{index}").into_bytes(),
                search: None,
            }])
            .await
            .expect("append")
            .last
            .expect("identity assigned");
        ids.push(appended);
    }
    store
        .handle()
        .advance_cursor(client, buffer, ids[2])
        .await
        .expect("cursor");
    store
        .handle()
        .advance_read_marker(buffer, ids[3])
        .await
        .expect("marker");
    let report = store
        .handle()
        .retain(&RetentionRequest {
            network,
            before: HistoryEventId(ids[3].0 + 1),
            max_delete: 16,
        })
        .await
        .expect("retention");
    assert_eq!(report.deleted, 4);
    assert_eq!(
        report.cursors_clamped, 1,
        "a cursor inside the removed range must be clamped"
    );
    assert_eq!(
        report.markers_clamped, 1,
        "a read marker inside the removed range must be clamped"
    );
    assert_eq!(report.oldest_retained, Some(ids[4]));
    // A clamped position now sits below the oldest retained event, so a future
    // query from it replays everything still held instead of silently skipping it.
    assert_eq!(
        store
            .handle()
            .get_cursor(client, buffer)
            .await
            .expect("cursor read")
            .expect("cursor exists"),
        HistoryEventId(0)
    );
    store.shutdown().expect("store shuts down");
    let _ = path;
}

#[tokio::test]
async fn retention_is_bounded_per_pass_and_reports_pending_work() {
    let dir = testing::temp_dir("retention-bound");
    let path = dir.db("retention.sqlite3");
    let store = store_at(&path);
    let network = NetworkId(1);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(network, BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    let batch: Vec<NewHistoryEvent> = (0..8)
        .map(|index| NewHistoryEvent {
            network,
            buffer,
            received_at: WallTime(index as i64),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: format!(":a!b@c PRIVMSG #room :m{index}").into_bytes(),
            search: None,
        })
        .collect();
    let appended = store.handle().append_history(&batch).await.expect("append");
    let boundary = HistoryEventId(appended.last.expect("assigned").0 + 1);
    let first = store
        .handle()
        .retain(&RetentionRequest {
            network,
            before: boundary,
            max_delete: 3,
        })
        .await
        .expect("retention");
    assert_eq!(
        first.deleted, 3,
        "one pass deletes at most the requested ceiling"
    );
    assert!(
        first.more_pending,
        "a bounded pass must report that work remains"
    );
    let second = store
        .handle()
        .retain(&RetentionRequest {
            network,
            before: boundary,
            max_delete: 100,
        })
        .await
        .expect("retention");
    assert_eq!(second.deleted, 5);
    assert!(!second.more_pending);
    store.shutdown().expect("store shuts down");
    let _ = path;
}

#[tokio::test]
async fn unbounded_retention_and_query_requests_are_refused() {
    let dir = testing::temp_dir("refusals");
    let path = dir.db("refusals.sqlite3");
    let store = store_at(&path);
    let network = NetworkId(1);
    store
        .handle()
        .save_network(&record(1, &[]))
        .await
        .expect("network saved");
    let buffer = store
        .handle()
        .resolve_buffer(network, BufferKind::Channel, "#room")
        .await
        .expect("buffer")
        .buffer;
    assert_eq!(
        store
            .handle()
            .retain(&RetentionRequest {
                network,
                before: HistoryEventId(100),
                max_delete: usize::MAX,
            })
            .await
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::InvalidRequest("retention delete ceiling"))
    );
    assert_eq!(
        store
            .handle()
            .query_history(&i2pr_irc_store::HistoryQuery {
                buffer,
                bound: i2pr_irc_store::HistoryQueryBound {
                    after: None,
                    before: None,
                    limit: usize::MAX,
                },
            })
            .await
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::InvalidRequest("history query limit"))
    );
    store.shutdown().expect("store shuts down");
    let _ = path;
}

// ------------------------------------------------- schema 3 -> 4 and detached policy

/// Builds a v3 database holding one Network with attached desired channels.
///
/// The channels exist at every position so the migration has to leave ordering alone,
/// not merely add a column to a single row.
fn v3_fixture(path: &std::path::Path, network: i64, channels: &[(&str, i64)]) {
    let connection = testing::create_v3_database(path);
    connection
        .execute(
            "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username, realname, display_name)
             VALUES (?1, 'irc.example.i2p', 0, 'bot', 'user', 'bouncer', 'network-' || ?1)",
            [network],
        )
        .expect("network fixture applies");
    for (target, position) in channels {
        connection
            .execute(
                "INSERT INTO desired_channels (network_id, casemap_key, target, position)
                 VALUES (?1, CAST(lower(?2) AS BLOB), ?2, ?3)",
                rusqlite::params![network, target, position],
            )
            .expect("desired channel fixture applies");
    }
}

#[test]
fn a_schema_three_database_is_migrated_to_four_on_open() {
    let dir = testing::temp_dir("m34");
    let path = dir.db("m34.sqlite3");
    v3_fixture(&path, 1, &[("#alpha", 0), ("#beta", 2), ("#gamma", 7)]);
    assert_eq!(testing::identity(&path).1, 3, "fixture is at schema 3");

    let store = store_at(&path);
    assert_eq!(
        testing::identity(&path).1,
        SCHEMA_VERSION,
        "open migrates forward to the current schema"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_migrated_desired_channel_is_attached_rather_than_hidden() {
    let dir = testing::temp_dir("m34attached");
    let path = dir.db("m34attached.sqlite3");
    v3_fixture(&path, 1, &[("#alpha", 0), ("#beta", 2)]);

    let store = store_at(&path);
    let records = store.handle().load_networks().await.expect("networks load");
    assert_eq!(
        records[0].desired_channels,
        vec![
            i2pr_irc_store::DesiredChannelRecord::at("#alpha", 0, false),
            i2pr_irc_store::DesiredChannelRecord::at("#beta", 2, false),
        ],
        "every channel that predates this build is attached, because it has been \
         presented to clients this whole time and hiding it would remove a channel \
         nobody asked to remove"
    );
    assert_eq!(
        testing::texts(
            &path,
            "SELECT target FROM desired_channels WHERE network_id = 1 ORDER BY position"
        ),
        vec!["#alpha".to_owned(), "#beta".to_owned()],
        "durable order and position survive the migration unchanged"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn the_detached_flag_is_constrained_at_the_storage_layer() {
    let dir = testing::temp_dir("m34check");
    let path = dir.db("m34check.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");

    let (not_null, default) = testing::column_constraints(&path, "desired_channels", "detached");
    assert!(
        not_null,
        "the flag may never be NULL: no policy is not a third policy"
    );
    assert_eq!(
        default,
        Some(0),
        "a row inserted without the flag is attached"
    );

    testing::execute(
        &path,
        "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username, realname, display_name)
         VALUES (1, 'irc.example.i2p', 0, 'bot', 'user', 'bouncer', 'one')",
    );
    let error = testing::expect_rejected(
        &path,
        "INSERT INTO desired_channels (network_id, casemap_key, target, position, detached)
         VALUES (1, CAST('#room' AS BLOB), '#room', 0, 2)",
    );
    assert!(
        error.to_string().contains("CHECK"),
        "a value outside 0/1 must be refused by the database itself, not only in Rust: {error}"
    );
}

#[tokio::test]
async fn a_database_missing_the_promised_column_is_refused_rather_than_served() {
    let dir = testing::temp_dir("m34missing");
    let path = dir.db("m34missing.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    // Rebuild the table without the column this build promises, then stamp it as
    // current. This is the exact shape of a database that claims a version it does not
    // have, and it must not be served with the flag silently read as absent.
    testing::execute(
        &path,
        "DROP TABLE desired_channels;
         CREATE TABLE desired_channels (
             network_id  INTEGER NOT NULL REFERENCES networks(network_id) ON DELETE CASCADE,
             casemap_key BLOB NOT NULL,
             target      TEXT NOT NULL,
             position    INTEGER NOT NULL,
             PRIMARY KEY (network_id, casemap_key)
         ) STRICT;
         PRAGMA user_version = 4;",
    );
    assert!(
        Store::open(&StorePath::File(path.clone())).is_err(),
        "a current-version database without the promised column is corrupt, not usable"
    );
    assert_eq!(
        testing::identity(&path).1,
        4,
        "refusing to serve must not rewrite the file"
    );
}

#[tokio::test]
async fn an_unrecognizable_detached_value_is_reported_as_corrupt() {
    let dir = testing::temp_dir("m34corrupt");
    let path = dir.db("m34corrupt.sqlite3");
    store_at(&path).shutdown().expect("store shuts down");
    testing::execute(
        &path,
        "INSERT INTO networks (network_id, endpoint, endpoint_kind, nick, username, realname, display_name)
         VALUES (1, 'irc.example.i2p', 0, 'bot', 'user', 'bouncer', 'one')",
    );
    // The CHECK constraint stops this build's own writer. `ignore_check_constraints` is
    // how a value gets in anyway -- a future writer, a repaired dump, a hand-edited file.
    // Coercing it would mean showing or hiding a channel the Operator never chose, so the
    // read path refuses the database instead.
    testing::execute(
        &path,
        "PRAGMA ignore_check_constraints = ON;
         INSERT INTO desired_channels (network_id, casemap_key, target, position, detached)
         VALUES (1, CAST('#room' AS BLOB), '#room', 0, 2);
         PRAGMA ignore_check_constraints = OFF;",
    );
    assert_eq!(
        store_at(&path)
            .handle()
            .load_networks()
            .await
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::Corrupt("desired channel detached flag")),
        "an unrecognizable policy is refused, never guessed at"
    );
    assert!(
        store_at(&path).handle().load_networks().await.is_err(),
        "a write that touched this row did not make it readable again, so nothing about \
         the damaged policy was silently adopted as a result of ordinary traffic"
    );
}

#[tokio::test]
async fn detaching_is_durable_survives_reopen_and_does_not_reorder_channels() {
    let dir = testing::temp_dir("detach");
    let path = dir.db("detach.sqlite3");
    let mut subject = record(1, &["#alpha", "#beta", "#gamma"]);
    store_at(&path)
        .handle()
        .save_network(&subject)
        .await
        .expect("record saves");

    assert!(
        store_at(&path)
            .handle()
            .set_desired_channel_detached(NetworkId(1), "#beta", true)
            .await
            .expect("detach commits"),
        "the channel was desired, so the flag is recorded"
    );
    subject = store_at(&path)
        .handle()
        .load_networks()
        .await
        .expect("networks load")
        .remove(0);
    assert_eq!(
        subject.desired_channels,
        vec![
            i2pr_irc_store::DesiredChannelRecord::at("#alpha", 0, false),
            i2pr_irc_store::DesiredChannelRecord::at("#beta", 1, true),
            i2pr_irc_store::DesiredChannelRecord::at("#gamma", 2, false),
        ],
        "detaching changes only the flag: the channel is still desired and still in \
         place, which is what makes it a presentation decision"
    );

    store_at(&path)
        .handle()
        .set_desired_channel_detached(NetworkId(1), "#beta", false)
        .await
        .expect("reattach commits");
    assert_eq!(
        store_at(&path)
            .handle()
            .load_networks()
            .await
            .expect("networks load")[0]
            .desired_channels,
        subject
            .desired_channels
            .iter()
            .map(|entry| entry.with_detached(false))
            .collect::<Vec<_>>(),
        "clearing the flag restores exactly the original list"
    );
}

#[tokio::test]
async fn detaching_an_unknown_channel_is_reported_rather_than_invented() {
    let dir = testing::temp_dir("detachmiss");
    let path = dir.db("detachmiss.sqlite3");
    store_at(&path)
        .handle()
        .save_network(&record(1, &["#alpha"]))
        .await
        .expect("record saves");

    assert!(
        !store_at(&path)
            .handle()
            .set_desired_channel_detached(NetworkId(1), "#ghost", true)
            .await
            .expect("the request itself is well formed"),
        "nothing was changed, so the caller can say so instead of assuming success"
    );
    assert_eq!(
        store_at(&path)
            .handle()
            .load_networks()
            .await
            .expect("networks load")[0]
            .desired_channels,
        attached_channels(&["#alpha"]),
        "a request for a channel this Network does not hold creates no row"
    );
}

#[tokio::test]
async fn a_detach_matching_uses_the_rfc1459_fold() {
    let dir = testing::temp_dir("detachfold");
    let path = dir.db("detachfold.sqlite3");
    store_at(&path)
        .handle()
        .save_network(&record(1, &["#Brackets[ok]"]))
        .await
        .expect("record saves");

    assert!(
        store_at(&path)
            .handle()
            .set_desired_channel_detached(NetworkId(1), "#BRACKETS{Ok}", true)
            .await
            .expect("detach commits"),
        "Rfc1459 folds square brackets to braces and uppercases, so this names the same channel"
    );
    assert_eq!(
        store_at(&path)
            .handle()
            .load_networks()
            .await
            .expect("networks load")[0]
            .desired_channels[0]
            .target,
        "#Brackets[ok]",
        "the stored spelling is the Operator's, not the request's"
    );
}

#[tokio::test]
async fn a_malformed_detach_target_is_refused_before_anything_is_written() {
    let dir = testing::temp_dir("detachbad");
    let path = dir.db("detachbad.sqlite3");
    store_at(&path)
        .handle()
        .save_network(&record(1, &["#alpha"]))
        .await
        .expect("record saves");

    for target in ["", "room", "#a,b", "#a:b", "#a b"] {
        assert_eq!(
            store_at(&path)
                .handle()
                .set_desired_channel_detached(NetworkId(1), target, true)
                .await
                .err()
                .map(|error| *error.kind()),
            Some(StoreErrorKind::InvalidRequest("desired channel shape")),
            "{target:?} is not a channel name"
        );
    }
    assert_eq!(
        store_at(&path)
            .handle()
            .load_networks()
            .await
            .expect("networks load")[0]
            .desired_channels,
        attached_channels(&["#alpha"])
    );
}

// ------------------------------------------------------- search side index

/// Builds a Network with one Buffer and returns the open store plus its handle.
///
/// `BufferId(1)` is the Channel buffer the Network's own desired channel resolves to, so
/// these tests search the same buffer a client would.
async fn searchable_store(tag: &str) -> (Store, StoreHandle, BufferId) {
    let dir = testing::temp_dir(tag);
    let path = dir.db("search.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, &["#room"]))
        .await
        .expect("network saved");
    let buffer = handle
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves")
        .buffer;
    (store, handle, buffer)
}

/// Parses a canonical protocol timestamp for use as a search bound.
fn stamp(text: &str) -> IrcTimestamp {
    IrcTimestamp::parse(text.as_bytes()).expect("fixture timestamp parses")
}

/// Renders one canonical protocol timestamp from unix seconds.
fn stamp_text(seconds: i64) -> String {
    IrcTimestamp::from_unix_millis(seconds.saturating_mul(1_000))
        .expect("fixture timestamp is representable")
        .to_string()
}

/// Appends one searchable message and returns its event id.
async fn say(
    handle: &StoreHandle,
    buffer: BufferId,
    sender: &str,
    body: &str,
    server_time: &str,
) -> HistoryEventId {
    let payload = format!(":{sender}!u@h PRIVMSG #room :{body}");
    let result = handle
        .append_history(&[NewHistoryEvent {
            network: NetworkId(1),
            buffer,
            received_at: WallTime(1),
            server_time: Some(
                IrcTimestamp::parse(server_time.as_bytes()).expect("timestamp parses"),
            ),
            // A msgid is a single protocol token: the body carries spaces, and a
            // msgid that carried one would be refused for the right reason at the
            // wrong moment.
            msgid: Some(format!("m{}", body.replace(' ', "-"))),
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".to_owned(),
            payload: payload.into_bytes(),
            search: Some(SearchFields {
                sender: sender.to_owned(),
                target: "#room".to_owned(),
                body: body.to_owned(),
            }),
        }])
        .await
        .expect("append succeeds");
    result.last.expect("an event was appended")
}

#[tokio::test]
async fn a_search_finds_only_messages_that_were_retained_and_indexed() {
    let (store, handle, buffer) = searchable_store("search-hit").await;
    say(
        &handle,
        buffer,
        "alice",
        "hello there",
        "2026-01-01T00:00:00.000Z",
    )
    .await;
    say(
        &handle,
        buffer,
        "bob",
        "goodbye now",
        "2026-01-01T00:00:01.000Z",
    )
    .await;

    let hits = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: None,
            before: None,
            terms: vec![SearchTerm::parse("hello").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");

    assert_eq!(
        hits.len(),
        1,
        "only the matching message is returned: {hits:?}"
    );
    assert_eq!(hits[0].body, "hello there");
    assert_eq!(hits[0].sender, "alice");
    assert_eq!(hits[0].target, "#room");
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_time_window_is_a_half_open_interval_over_canonical_timestamps() {
    let (store, handle, buffer) = searchable_store("search-window").await;
    let first = say(&handle, buffer, "alice", "word", "2026-01-01T00:00:00.000Z").await;
    let middle = say(&handle, buffer, "alice", "word", "2026-01-01T00:00:05.000Z").await;
    let last = say(&handle, buffer, "alice", "word", "2026-01-01T00:00:10.000Z").await;

    // This is the property that a TEXT server_time column actually breaks when the bound
    // is bound as a number: SQLite orders every TEXT after every INTEGER, so an integer
    // bound matches either all of history or none of it. What this proves is that the
    // window is half-open -- `after` keeps the event sitting exactly on it, `before`
    // drops the event sitting exactly on it -- and that both endpoints are really
    // timestamps rather than numbers that happen to look like them.
    let inside = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: Some(stamp("2026-01-01T00:00:00.000Z")),
            before: Some(stamp("2026-01-01T00:00:10.000Z")),
            terms: vec![SearchTerm::parse("word").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert_eq!(
        inside.iter().map(|hit| hit.event).collect::<Vec<_>>(),
        vec![first, middle],
        "the lower endpoint is inside the window and the upper endpoint is not"
    );
    let _ = last;

    // Both endpoints reachable from outside the window: a whole day, and a single
    // millisecond that no event carries.
    let whole_day = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: Some(stamp("2026-01-01T00:00:00.000Z")),
            before: Some(stamp("2026-01-02T00:00:00.000Z")),
            terms: vec![SearchTerm::parse("word").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert_eq!(
        whole_day.iter().map(|hit| hit.event).collect::<Vec<_>>(),
        vec![first, middle, last],
        "a window wider than the data returns all of it"
    );

    let narrow = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: Some(stamp("2026-01-01T00:00:01.000Z")),
            before: Some(stamp("2026-01-01T00:00:02.000Z")),
            terms: vec![SearchTerm::parse("word").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert!(
        narrow.is_empty(),
        "a window between two events returns nothing rather than everything: {narrow:?}"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn search_terms_cannot_inject_fts_operators_or_sql() {
    let (store, handle, buffer) = searchable_store("search-injection").await;
    say(
        &handle,
        buffer,
        "alice",
        "hello there",
        "2026-01-01T00:00:00.000Z",
    )
    .await;

    // Every one of these is either refused as a term or, if it parsed, could not have
    // changed the shape of the statement. The store's compiled expression is the only
    // SQL text that reaches SQLite, and it is built only from validated terms.
    for hostile in [
        "hello\" OR 1=1 --",
        "NEAR(a b)",
        "hello*",
        "col:val",
        "-hello",
        "\"",
        "a AND b",
    ] {
        assert!(
            SearchTerm::parse(hostile).is_err(),
            "{hostile:?} must never become a search term"
        );
    }
    let expression = SearchQuery {
        network: NetworkId(1),
        buffers: Vec::new(),
        sender: None,
        after: None,
        before: None,
        terms: vec![SearchTerm::parse("hello").expect("term parses")],
        limit: 16,
    }
    .match_expression()
    .expect("a term list compiles");
    assert_eq!(
        expression, "\"hello\"",
        "terms are quoted literals, not syntax"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_search_never_reads_another_networks_history() {
    let (store, handle, buffer) = searchable_store("search-scope").await;
    say(
        &handle,
        buffer,
        "alice",
        "shared word",
        "2026-01-01T00:00:00.000Z",
    )
    .await;
    handle
        .save_network(&record(2, &["#other"]))
        .await
        .expect("second network saved");

    let hits = handle
        .search(&SearchQuery {
            network: NetworkId(2),
            buffers: Vec::new(),
            sender: None,
            after: None,
            before: None,
            terms: vec![SearchTerm::parse("shared").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert!(
        hits.is_empty(),
        "a Network that does not hold the message cannot see it: {hits:?}"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn retention_leaves_no_index_row_behind() {
    let (store, handle, buffer) = searchable_store("search-retain").await;
    let first = say(
        &handle,
        buffer,
        "alice",
        "forgettable words",
        "2026-01-01T00:00:00.000Z",
    )
    .await;
    say(
        &handle,
        buffer,
        "bob",
        "kept words",
        "2026-01-01T00:00:01.000Z",
    )
    .await;

    handle
        .retain(&RetentionRequest {
            network: NetworkId(1),
            before: HistoryEventId(first.0 + 1),
            max_delete: 16,
        })
        .await
        .expect("retention succeeds");

    let hits = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: None,
            before: None,
            terms: vec![SearchTerm::parse("forgettable").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert!(
        hits.is_empty(),
        "a deleted message cannot still be found by search: {hits:?}"
    );

    let remaining = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: None,
            before: None,
            terms: vec![SearchTerm::parse("kept").expect("term parses")],
            limit: 16,
        })
        .await
        .expect("search succeeds");
    assert_eq!(
        remaining.len(),
        1,
        "the retained message is still searchable"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_timestamp_reference_resolves_through_the_index_without_a_scan() {
    let (store, handle, buffer) = searchable_store("search-reference").await;
    let first = say(&handle, buffer, "alice", "one", "2026-01-01T00:00:00.000Z").await;
    let middle = say(&handle, buffer, "alice", "two", "2026-01-01T00:00:05.000Z").await;
    let last = say(
        &handle,
        buffer,
        "alice",
        "three",
        "2026-01-01T00:00:10.000Z",
    )
    .await;

    let at = handle
        .nearest_event(
            buffer,
            IrcTimestamp::parse(b"2026-01-01T00:00:05.000Z").expect("timestamp parses"),
        )
        .await
        .expect("lookup succeeds");
    assert_eq!(at.before, Some(middle), "the nearest event is found");
    assert!(at.exact, "a reference that lands on an event is exact");
    assert_eq!(at.after, Some(last));

    let between = handle
        .nearest_event(
            buffer,
            IrcTimestamp::parse(b"2026-01-01T00:00:07.000Z").expect("timestamp parses"),
        )
        .await
        .expect("lookup succeeds");
    assert_eq!(between.before, Some(middle));
    assert_eq!(between.after, Some(last));
    assert!(
        !between.exact,
        "a reference between two events is not exact"
    );

    let before_all = handle
        .nearest_event(
            buffer,
            IrcTimestamp::from_unix_millis(0).expect("timestamp in range"),
        )
        .await
        .expect("lookup succeeds");
    assert_eq!(before_all.before, None);
    assert_eq!(before_all.after, Some(first));
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn several_events_sharing_a_timestamp_resolve_deterministically() {
    let (store, handle, buffer) = searchable_store("search-ties").await;
    // Three events, one millisecond. Local order is the only thing that can break the tie,
    // and it must break it the same way every time.
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(
            say(
                &handle,
                buffer,
                "alice",
                &format!("tie{index}"),
                "2026-01-01T00:00:00.000Z",
            )
            .await,
        );
    }
    let resolved = handle
        .nearest_event(
            buffer,
            IrcTimestamp::parse(b"2026-01-01T00:00:00.000Z").expect("timestamp parses"),
        )
        .await
        .expect("lookup succeeds");
    assert_eq!(
        resolved.before,
        Some(*ids.last().expect("ids were appended")),
        "a tie resolves to the newest local id, deterministically"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_duplicate_msgid_is_reported_as_ambiguous_rather_than_resolved() {
    let (store, handle, buffer) = searchable_store("search-msgid").await;
    let payload = |sender: &str, body: &str| NewHistoryEvent {
        network: NetworkId(1),
        buffer,
        received_at: WallTime(1),
        server_time: None,
        msgid: Some("duplicate-id".to_owned()),
        direction: EventDirection::Inbound,
        event_class: "PRIVMSG".to_owned(),
        payload: format!(":{sender}!u@h PRIVMSG #room :{body}").into_bytes(),
        search: Some(SearchFields {
            sender: sender.to_owned(),
            target: "#room".to_owned(),
            body: body.to_owned(),
        }),
    };
    let result = handle
        .append_history(&[payload("alice", "first"), payload("bob", "second")])
        .await
        .expect("append succeeds");

    let lookup = handle
        .resolve_msgid(NetworkId(1), "duplicate-id")
        .await
        .expect("lookup succeeds");
    assert!(
        lookup.is_ambiguous(),
        "a duplicate upstream id is never silently resolved to one of them: {lookup:?}"
    );
    assert_eq!(
        lookup,
        MsgidLookup::Ambiguous(vec![
            result.first.expect("first"),
            result.last.expect("last")
        ]),
        "ambiguity is reported in canonical order"
    );

    assert_eq!(
        handle
            .resolve_msgid(NetworkId(1), "no-such-id")
            .await
            .expect("lookup succeeds"),
        MsgidLookup::Missing,
        "an unknown id is missing, not ambiguous"
    );
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn search_bounds_are_enforced_before_any_database_work() {
    let (store, handle, buffer) = searchable_store("search-bounds").await;
    say(&handle, buffer, "alice", "word", "2026-01-01T00:00:00.000Z").await;
    let base = SearchQuery {
        network: NetworkId(1),
        buffers: Vec::new(),
        sender: None,
        after: None,
        before: None,
        terms: vec![SearchTerm::parse("word").expect("term parses")],
        limit: 16,
    };

    let over_limit = SearchQuery {
        limit: MAX_SEARCH_RESULTS + 1,
        ..base.clone()
    };
    assert!(over_limit.validate().is_err());
    assert!(
        handle.search(&over_limit).await.is_err(),
        "an over-limit search is refused, not truncated"
    );

    let too_many_terms = SearchQuery {
        terms: (0..MAX_SEARCH_TERMS + 1)
            .map(|index| SearchTerm::parse(&format!("w{index}")).expect("term parses"))
            .collect(),
        ..base.clone()
    };
    assert!(too_many_terms.validate().is_err());

    let too_many_buffers = SearchQuery {
        buffers: (0..MAX_SEARCH_BUFFERS + 1)
            .map(|index| BufferId(u64::try_from(index).expect("fits")))
            .collect(),
        ..base.clone()
    };
    assert!(too_many_buffers.validate().is_err());

    let backwards = SearchQuery {
        after: Some(stamp("2026-01-02T00:00:00.000Z")),
        before: Some(stamp("2026-01-01T00:00:00.000Z")),
        ..base.clone()
    };
    assert!(
        backwards.validate().is_err(),
        "a backwards range is refused rather than silently swapped"
    );

    // The window is half-open, so a zero-width range is refused too rather than
    // answering a whole day of history for `after == before`.
    let empty_window = SearchQuery {
        after: Some(stamp("2026-01-01T00:00:00.000Z")),
        before: Some(stamp("2026-01-01T00:00:00.000Z")),
        ..base.clone()
    };
    assert!(empty_window.validate().is_err());
    store.shutdown().expect("store shuts down");
}

#[test]
fn the_schema_five_migration_backfills_the_index_for_retained_history() {
    let dir = testing::temp_dir("migrate-v5-v6");
    let path = dir.db("v5.sqlite3");
    let old = testing::create_v5_database(&path);
    drop(old);

    let store = store_at(&path);
    let handle = store.handle_clone();
    let hits = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
        .block_on(async {
            handle
                .search(&SearchQuery {
                    network: NetworkId(1),
                    buffers: Vec::new(),
                    sender: None,
                    after: None,
                    before: None,
                    terms: vec![SearchTerm::parse("hello").expect("term parses")],
                    limit: 16,
                })
                .await
        })
        .expect("search succeeds after migration");

    assert_eq!(
        hits.len(),
        1,
        "history retained before the migration is searchable after it: {hits:?}"
    );
    assert_eq!(hits[0].body, "hello there");
    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_search_index_that_disagrees_with_history_refuses_the_open() {
    let dir = testing::temp_dir("search-corrupt");
    let path = dir.db("corrupt.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, &["#room"]))
        .await
        .expect("network saved");
    let buffer = handle
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves")
        .buffer;
    say(
        &handle,
        buffer,
        "alice",
        "findable words",
        "2026-01-01T00:00:00.000Z",
    )
    .await;
    store.shutdown().expect("store shuts down");

    // Drop an index row behind the store's back. The database is still structurally
    // valid and every table is present, which is exactly why a presence check alone
    // would not catch it -- and why a search would answer "no matches" and be believed.
    let connection = testing::raw(&path);
    connection
        .execute("DELETE FROM history_search", [])
        .expect("index row is removable");

    assert!(
        Store::open(&StorePath::File(path.clone())).is_err(),
        "a search index that lost rows refuses the open rather than answering 'no matches'"
    );
}

#[tokio::test]
async fn a_window_around_an_anchor_is_bounded_and_centred_on_it() {
    let (_store, handle, buffer) = searchable_store("around-centre").await;
    let mut ids = Vec::new();
    for index in 0..9 {
        ids.push(
            say(
                &handle,
                buffer,
                "alice",
                &format!("m{index}"),
                &stamp_text(1_700_000_000 + i64::from(index)),
            )
            .await,
        );
    }
    let anchor = ids[4];

    let events = handle
        .history_around(&HistoryAround {
            buffer,
            anchor,
            before: 2,
            after: 2,
        })
        .await
        .expect("the window is produced");

    assert_eq!(
        events.iter().map(|event| event.event).collect::<Vec<_>>(),
        vec![ids[2], ids[3], ids[4], ids[5], ids[6]],
        "the anchor is included and both budgets are honoured, in ascending local order"
    );

    // An asymmetric budget is honoured as asked. A single shared `limit` would force the
    // caller to guess a split and then silently lose half of what it wanted.
    let lopsided = handle
        .history_around(&HistoryAround {
            buffer,
            anchor,
            before: 0,
            after: 3,
        })
        .await
        .expect("the window is produced");
    assert_eq!(
        lopsided.iter().map(|event| event.event).collect::<Vec<_>>(),
        vec![ids[4], ids[5], ids[6], ids[7]]
    );
}

#[tokio::test]
async fn a_window_around_a_removed_anchor_answers_on_both_sides() {
    let (_store, handle, buffer) = searchable_store("around-pruned").await;
    let mut ids = Vec::new();
    for index in 0..5 {
        ids.push(
            say(
                &handle,
                buffer,
                "alice",
                &format!("m{index}"),
                &stamp_text(1_700_000_000 + i64::from(index)),
            )
            .await,
        );
    }
    let anchor = ids[2];
    handle
        .retain(&RetentionRequest {
            network: NetworkId(1),
            before: ids[3],
            max_delete: 16,
        })
        .await
        .expect("retention runs");

    // Retention removed everything older than the anchor, so only the "after" side has
    // anything left. The point is that the call still succeeds: answering with an error
    // would strand a client holding a bookmark to a message that retention has since
    // removed, which is exactly the bookmark a returning client has.
    let events = handle
        .history_around(&HistoryAround {
            buffer,
            anchor,
            before: 4,
            after: 4,
        })
        .await
        .expect("a removed anchor is still a position to page from");
    assert_eq!(
        events.iter().map(|event| event.event).collect::<Vec<_>>(),
        vec![ids[3], ids[4]],
        "the retained side comes back and the pruned side is simply empty"
    );
    assert!(
        events.iter().all(|event| event.event != anchor),
        "the anchor itself is absent, because it was removed"
    );
}

#[tokio::test]
async fn a_window_larger_than_the_query_ceiling_is_refused_not_truncated() {
    let (_store, handle, buffer) = searchable_store("around-bounds").await;
    let anchor = say(&handle, buffer, "alice", "one", "2026-01-01T00:00:00.000Z").await;
    let over = HistoryAround {
        buffer,
        anchor,
        before: MAX_HISTORY_QUERY_EVENTS,
        after: MAX_HISTORY_QUERY_EVENTS,
    };
    assert!(over.validate().is_err());
    assert!(
        handle.history_around(&over).await.is_err(),
        "an over-wide window is refused rather than served partially, so a caller cannot \
         mistake the ceiling for the whole conversation"
    );
}

// ------------------------------------------------- registration actions

fn stored_action(
    kind: RegistrationActionKind,
    target: &str,
    payload: &str,
) -> StoredRegistrationAction {
    StoredRegistrationAction {
        kind,
        target: target.into(),
        payload: StoredSecret::new(payload.into()),
    }
}

#[tokio::test]
async fn registration_actions_survive_a_restart_in_the_order_they_were_written() {
    let dir = testing::temp_dir("actions-round-trip");
    let path = dir.db("actions.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle
        .save_network(&record(1, &[]))
        .await
        .expect("the network is durable");

    let wanted = vec![
        stored_action(RegistrationActionKind::Mode, "+B", ""),
        stored_action(
            RegistrationActionKind::Message,
            "NickServ",
            "IDENTIFY hunter2",
        ),
        stored_action(RegistrationActionKind::Mode, "+i", ""),
    ];
    assert_eq!(
        handle
            .save_registration_actions(NetworkId(1), &wanted)
            .await
            .expect("actions are durable"),
        3
    );

    let read = handle
        .load_registration_actions(NetworkId(1))
        .await
        .expect("actions are readable");
    assert_eq!(read.len(), 3, "replay order is part of the meaning");
    assert_eq!(read[0].target, "+B");
    assert_eq!(read[1].target, "NickServ");
    assert_eq!(
        read[1].payload.expose(),
        "IDENTIFY hunter2",
        "the whole message round trips; only its *rendering* is forbidden"
    );
    assert_eq!(read[2].target, "+i");

    // A reopen proves the rows are durable rather than cached in the worker.
    drop(store);
    drop(handle);
    let reopened = store_at(&path);
    let after = reopened
        .handle_clone()
        .load_registration_actions(NetworkId(1))
        .await
        .expect("actions survive a restart");
    assert_eq!(after.len(), 3);
    assert_eq!(after[1].payload.expose(), "IDENTIFY hunter2");
    reopened.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn a_stored_action_is_never_rendered_by_its_debug() {
    // The one thing a stored action holds that must not escape into a log or an error.
    let action = stored_action(
        RegistrationActionKind::Message,
        "NickServ",
        "IDENTIFY hunter2",
    );
    let rendered = format!("{action:?}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(
        rendered.contains("NickServ"),
        "the target is operational: {rendered}"
    );
    assert!(rendered.contains("[redacted]"), "{rendered}");
}

#[tokio::test]
async fn registration_actions_are_bounded_on_the_write_path() {
    let dir = testing::temp_dir("actions-bounds");
    let path = dir.db("bounds.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle.save_network(&record(1, &[])).await.expect("durable");

    let too_many: Vec<_> = (0..=i2pr_irc_store::MAX_STORED_ACTIONS)
        .map(|_| stored_action(RegistrationActionKind::Mode, "+B", ""))
        .collect();
    assert!(
        matches!(
            handle
                .save_registration_actions(NetworkId(1), &too_many)
                .await
                .expect_err("the count ceiling is enforced on write")
                .kind(),
            StoreErrorKind::InvalidRequest(_)
        ),
        "the count ceiling is enforced on write"
    );
    assert!(
        handle
            .load_registration_actions(NetworkId(1))
            .await
            .expect("readable")
            .is_empty(),
        "a refused write leaves nothing behind"
    );

    let long_target = "x".repeat(i2pr_irc_store::MAX_STORED_ACTION_TARGET_BYTES + 1);
    assert!(
        matches!(
            handle
                .save_registration_actions(
                    NetworkId(1),
                    &[stored_action(
                        RegistrationActionKind::Mode,
                        &long_target,
                        ""
                    )],
                )
                .await
                .expect_err("the target ceiling is enforced on write")
                .kind(),
            StoreErrorKind::InvalidRequest(_)
        ),
        "the target ceiling is enforced on write"
    );

    let long_payload = "x".repeat(i2pr_irc_store::MAX_STORED_ACTION_PAYLOAD_BYTES + 1);
    assert!(
        matches!(
            handle
                .save_registration_actions(
                    NetworkId(1),
                    &[stored_action(
                        RegistrationActionKind::Message,
                        "NickServ",
                        &long_payload,
                    )],
                )
                .await
                .expect_err("the payload ceiling is enforced on write")
                .kind(),
            StoreErrorKind::InvalidRequest(_)
        ),
        "the payload ceiling is enforced on write"
    );

    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn setting_actions_replaces_the_whole_list() {
    // The Operator's intent is "this is now the list". An append-only store would make
    // removing an action a second operation with its own semantics.
    let dir = testing::temp_dir("actions-replace");
    let path = dir.db("replace.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle.save_network(&record(1, &[])).await.expect("durable");

    handle
        .save_registration_actions(
            NetworkId(1),
            &[
                stored_action(RegistrationActionKind::Mode, "+B", ""),
                stored_action(RegistrationActionKind::Mode, "+i", ""),
            ],
        )
        .await
        .expect("durable");
    handle
        .save_registration_actions(
            NetworkId(1),
            &[stored_action(RegistrationActionKind::Mode, "+w", "")],
        )
        .await
        .expect("durable");

    let read = handle
        .load_registration_actions(NetworkId(1))
        .await
        .expect("readable");
    assert_eq!(
        read.len(),
        1,
        "the second write replaced rather than appended"
    );
    assert_eq!(read[0].target, "+w");

    handle
        .save_registration_actions(NetworkId(1), &[])
        .await
        .expect("an empty set clears");
    assert!(
        handle
            .load_registration_actions(NetworkId(1))
            .await
            .expect("readable")
            .is_empty()
    );

    store.shutdown().expect("store shuts down");
}

#[tokio::test]
async fn deleting_a_network_deletes_its_actions() {
    let dir = testing::temp_dir("actions-cascade");
    let path = dir.db("cascade.sqlite3");
    let store = store_at(&path);
    let handle = store.handle_clone();
    handle.save_network(&record(1, &[])).await.expect("durable");
    handle
        .save_registration_actions(
            NetworkId(1),
            &[stored_action(RegistrationActionKind::Mode, "+B", "")],
        )
        .await
        .expect("durable");
    assert!(handle.remove_network(NetworkId(1)).await.expect("removed"));
    assert!(
        handle
            .load_registration_actions(NetworkId(1))
            .await
            .expect("readable")
            .is_empty(),
        "a forgotten Network must not leave a credential-bearing row behind"
    );

    store.shutdown().expect("store shuts down");
}
