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
fn fresh_database_creates_exactly_schema_version_one() {
    let dir = testing::temp_dir("fresh");
    let path = dir.db("fresh.sqlite3");
    let store = store_at(&path);
    assert_eq!(
        testing::tables(&path),
        EXPECTED_TABLES.to_vec(),
        "schema 1 is exactly the frozen table set"
    );
    assert_eq!(
        testing::identity(&path),
        (
            i2pr_irc_store::APPLICATION_ID,
            i2pr_irc_store::SCHEMA_VERSION
        )
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
    testing::stamp(&path, i2pr_irc_store::APPLICATION_ID, 2);
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
            server_time: Some(WallTime(999 - index)),
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
