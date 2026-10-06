//! Plan 007 storage qualification.
//!
//! These tests are deliberately outside the store crate's unit tests: they exercise
//! the public typed surface exactly as M003-B and later plans will, and they read the
//! raw database through [`i2pr_irc_store::testing`] so migration and restart evidence
//! never depends on the same API that is under test.
use i2pr_irc_core::{I2pEndpoint, WallTime};
use i2pr_irc_store::{
    BufferKind, EventDirection, HistoryEventId, NetworkId, NetworkRecord, NewHistoryEvent,
    RetentionRequest, STORE_QUEUE_CAPACITY, Store, StoreErrorKind, StoreHealth, StorePath,
    StoredSecret,
    testing::{self, EXPECTED_TABLES},
};

fn store_at(path: &std::path::Path) -> Store {
    Store::open(&StorePath::File(path.to_path_buf())).expect("store opens")
}

fn record(network: u64, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "bouncer".into(),
        sasl: None,
        desired_channels: channels.iter().map(|value| (*value).to_owned()).collect(),
    }
}

// ---------------------------------------------------------------- schema/open

#[test]
fn fresh_database_creates_exactly_schema_version_two() {
    let dir = testing::temp_dir("fresh");
    let path = dir.db("fresh.sqlite3");
    let store = store_at(&path);
    assert_eq!(
        testing::tables(&path),
        EXPECTED_TABLES.to_vec(),
        "the table set is unchanged from schema 1"
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
    assert_eq!(testing::identity(&path).1, 2, "open migrates forward");

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
    assert_eq!(testing::identity(&path).1, 2);
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
        vec!["#alpha".to_owned(), "#beta".to_owned(), "#gamma".to_owned()],
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
    assert_eq!(networks[0].desired_channels, vec!["#alpha".to_owned()]);
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
