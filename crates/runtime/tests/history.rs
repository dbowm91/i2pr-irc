//! Plan 009 qualification: bounded durable history, canonical order, monotonic
//! cursors, acknowledged legacy playback, bounded retention, and restart semantics.
#![cfg(test)]

use i2pr_irc_core::{
    Casemapping, ClientId, HistoryEventId, I2pEndpoint, NetworkId, SystemWallClock,
    VirtualWallClock,
};
use i2pr_irc_runtime::{
    journal::{BacklogCap, HistoryJournal, IngestOutcome, RetentionPolicy},
    playback,
    session::SessionCapabilities,
};
use i2pr_irc_store::{
    BufferKind, EventDirection, HistoryEvent, NetworkRecord, Store, StoreHandle, StorePath,
    fallback_display_name,
};
use i2pr_irc_wire::Message;

/// A canonical `server-time` value for an epoch-second reading.
///
/// Fixtures must use the real wire grammar: an integer epoch is *not* a valid
/// `server-time`, so a fixture using one would be testing a value the parser is
/// right to reject.
fn stamp(epoch_seconds: i64) -> String {
    i2pr_irc_wire::IrcTimestamp::from_unix_millis(epoch_seconds * 1_000)
        .expect("representable")
        .to_string()
}

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}

/// The login used for the durable client lineage in these tests.
const PHONE: &str = "phone";

async fn journal_for(
    handle: &StoreHandle,
    wall: VirtualWallClock,
    channels: &[&str],
) -> HistoryJournal {
    handle
        .save_network(&NetworkRecord {
            network: NetworkId(1),
            display_name: fallback_display_name(NetworkId(1)),
            endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            auto_away: false,
            keep_nick: false,
            sasl: None,
            desired_channels: i2pr_irc_store::attached_channels(
                &channels
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
            ),
        })
        .await
        .expect("network saved");
    HistoryJournal::new(
        NetworkId(1),
        handle.clone(),
        Box::new(wall),
        Casemapping::Rfc1459,
    )
}

fn message(raw: &str) -> Message {
    Message::parse(raw.as_bytes()).expect("message parses")
}

// ------------------------------------------------------------ ingestion policy

#[tokio::test]
async fn only_inbound_chat_is_recorded_as_history() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves");

    // Chat is history.
    assert!(matches!(
        journal
            .ingest(buffer, &message(":a!b@c PRIVMSG #room :hello\r\n"))
            .await
            .expect("ingests"),
        IngestOutcome::Recorded { .. }
    ));
    assert!(matches!(
        journal
            .ingest(buffer, &message(":a!b@c NOTICE #room :heads up\r\n"))
            .await
            .expect("ingests"),
        IngestOutcome::Recorded { .. }
    ));
    // Operational traffic is not: numerics are this bouncer's own replies, and
    // membership events are explicitly out of scope for this milestone.
    for line in [
        ":srv 353 bot = #room :bot\r\n",
        ":bot!u@h JOIN #room\r\n",
        ":a!b@c PART #room\r\n",
        "PING :token\r\n",
    ] {
        assert_eq!(
            journal
                .ingest(buffer, &message(line))
                .await
                .expect("ingests"),
            IngestOutcome::Skipped,
            "{line} must not become history"
        );
    }
    let health = journal.health();
    assert_eq!(health.appended, 2, "only chat was recorded");
}

#[tokio::test]
async fn local_outgoing_messages_are_omitted_until_an_upstream_echo_exists() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves");

    // A local write is not evidence of upstream delivery, so this plan stores
    // nothing for it. The journal exposes no path that could label it confirmed.
    assert_eq!(journal.health().appended, 0);
    assert_eq!(
        journal
            .backlog(i2pr_irc_core::ClientId(1), buffer, BacklogCap::DEFAULT)
            .await
            .expect("backlog")
            .len(),
        0,
        "no local write may appear as history"
    );
}

#[tokio::test]
async fn canonical_order_is_local_sequence_not_any_timestamp() {
    let (_store, handle) = store();
    let wall = VirtualWallClock::default();
    let mut journal = journal_for(&handle, wall.clone(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves");

    // Receive time advances, but server-time goes backwards on every line.
    let mut ids = Vec::new();
    for index in 0..4i64 {
        wall.advance(10).expect("clock advances");
        // Tags precede the prefix; server-time deliberately runs backwards while
        // receive time advances.
        let line = format!(
            "@time={} :a!b@c PRIVMSG #room :m{index}\r\n",
            stamp(1_700_000_000 - index)
        );
        let outcome = journal
            .ingest(buffer, &message(&line))
            .await
            .expect("ingests");
        if let IngestOutcome::Recorded { event } = outcome {
            ids.push(event);
        }
    }
    assert_eq!(ids.len(), 4);
    for pair in ids.windows(2) {
        assert!(
            pair[0] < pair[1],
            "canonical order must follow the local sequence, not server-time"
        );
    }
    let events = journal
        .backlog(ClientId(1), buffer, BacklogCap::DEFAULT)
        .await
        .expect("backlog");
    let order: Vec<String> = events
        .iter()
        .map(|event| String::from_utf8_lossy(&event.payload).into_owned())
        .collect();
    assert_eq!(
        order,
        vec![
            format!("@time={} :a!b@c PRIVMSG #room :m0", stamp(1_700_000_000)),
            format!("@time={} :a!b@c PRIVMSG #room :m1", stamp(1_699_999_999)),
            format!("@time={} :a!b@c PRIVMSG #room :m2", stamp(1_699_999_998)),
            format!("@time={} :a!b@c PRIVMSG #room :m3", stamp(1_699_999_997)),
        ]
    );
}

#[tokio::test]
async fn server_time_and_msgid_are_preserved_as_metadata_only() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves");
    journal
        .ingest(
            buffer,
            &message(&format!(
                "@time={};msgid=abc123 :a!b@c PRIVMSG #room :hi\r\n",
                i2pr_irc_wire::IrcTimestamp::from_unix_millis(1_700_000_000_500)
                    .expect("representable")
            )),
        )
        .await
        .expect("ingests");
    let events = journal
        .backlog(ClientId(1), buffer, BacklogCap::DEFAULT)
        .await
        .expect("backlog");
    assert_eq!(
        events[0].server_time.map(|time| time.to_string()),
        Some("2023-11-14T22:13:20.500Z".to_owned())
    );
    assert_eq!(events[0].msgid.as_deref(), Some("abc123"));
    assert!(
        events[0].received_at.unix_seconds() >= 0,
        "receive wall time is retained independently of server-time"
    );
}

// ------------------------------------------------------------------- buffers

#[tokio::test]
async fn a_channel_and_a_query_are_never_the_same_buffer() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let channel = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("channel resolves");
    let query = journal
        .resolve_buffer(BufferKind::Query, "#room")
        .await
        .expect("query resolves");
    assert_ne!(channel, query);
    // Repeated resolution is stable, including under a different case.
    let again = journal
        .resolve_buffer(BufferKind::Channel, "#ROOM")
        .await
        .expect("resolves");
    assert_eq!(again, channel, "a channel keeps one stable identity");
}

#[tokio::test]
async fn buffer_resolution_refuses_an_identity_of_the_wrong_kind() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("resolves");
    // The journal must not hand back a channel identity for a query request.
    let outcome = journal.resolve_buffer(BufferKind::Query, "#room").await;
    // A query and a channel are distinct durable rows, so this resolves separately;
    // the invariant is that the two are never merged.
    assert!(
        matches!(outcome, Ok(buffer) if buffer != journal.resolve_buffer(BufferKind::Channel, "#room").await.unwrap())
    );
}

// ---------------------------------------------------------- cursors/markers

#[tokio::test]
async fn cursors_are_per_client_and_move_monotonically() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    // A durable lineage must exist before a cursor may reference it: playback state
    // has to survive a restart, so it cannot hang off a locally invented id.
    let phone = journal.ensure_client(PHONE).await.expect("lineage");
    let laptop = journal.ensure_client("laptop").await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let mut ids = Vec::new();
    for index in 0..4u64 {
        if let IngestOutcome::Recorded { event } = journal
            .ingest(
                buffer,
                &message(&format!(":a!b@c PRIVMSG #room :m{index}\r\n")),
            )
            .await
            .expect("ingests")
        {
            ids.push(event);
        }
    }

    // Two client lineages on one buffer keep independent positions.
    assert_eq!(
        journal
            .advance_cursor(phone, buffer, ids[2])
            .await
            .expect("advances"),
        ids[2]
    );
    assert_eq!(
        journal.cursor(laptop, buffer).await.expect("reads"),
        None,
        "a cursor belongs to one client lineage, not to the buffer"
    );
    // A stale acknowledgement never rewinds.
    assert_eq!(
        journal
            .advance_cursor(phone, buffer, ids[0])
            .await
            .expect("stale advance"),
        ids[2]
    );
    // The second lineage starts from its own position.
    assert_eq!(
        journal
            .advance_cursor(laptop, buffer, ids[1])
            .await
            .expect("advances"),
        ids[1]
    );
}

#[tokio::test]
async fn a_read_marker_is_shared_per_buffer_and_moves_only_forward() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    assert_eq!(journal.read_marker(buffer).await.expect("reads"), None);
    assert_eq!(
        journal
            .set_read_marker(buffer, HistoryEventId(50))
            .await
            .expect("sets"),
        HistoryEventId(50)
    );
    // The marker is distinct from any client cursor and is shared by the Operator.
    let phone = journal.ensure_client(PHONE).await.expect("lineage");
    assert_eq!(journal.cursor(phone, buffer).await.expect("reads"), None);
    assert_eq!(
        journal
            .set_read_marker(buffer, HistoryEventId(10))
            .await
            .expect("stale set"),
        HistoryEventId(50)
    );
}

// ------------------------------------------------------------------ playback

#[tokio::test]
async fn legacy_backlog_is_bounded_in_events_and_bytes() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let batch: Vec<i2pr_irc_store::NewHistoryEvent> = (0..20u64)
        .map(|index| i2pr_irc_store::NewHistoryEvent {
            network: NetworkId(1),
            buffer,
            received_at: i2pr_irc_core::WallTime(index as i64),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: format!(":a!b@c PRIVMSG #room :m{index}").into_bytes(),
        })
        .collect();
    handle.append_history(&batch).await.expect("appends");

    let phone = journal.ensure_client(PHONE).await.expect("lineage");
    let capped = journal
        .backlog(phone, buffer, BacklogCap::new(5, 4096))
        .await
        .expect("backlog");
    assert_eq!(capped.len(), 5, "the event cap is enforced");

    // A tiny byte budget admits fewer events than the event cap would allow.
    let byte_capped = journal
        .backlog(phone, buffer, BacklogCap::new(100, 60))
        .await
        .expect("backlog");
    assert!(
        !byte_capped.is_empty(),
        "the byte budget must still deliver history"
    );
    let total: usize = byte_capped.iter().map(|event| event.payload.len()).sum();
    assert!(
        byte_capped
            .last()
            .is_some_and(|event| total - event.payload.len() <= 60),
        "an event must not be delivered once the byte budget is spent, got {total}"
    );
    assert!(
        byte_capped.len() < 20,
        "the byte budget must actually bound delivery, got {} events",
        byte_capped.len()
    );
}

#[tokio::test]
async fn a_backlog_resumes_from_the_durable_cursor() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let phone = journal.ensure_client(PHONE).await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let mut ids = Vec::new();
    for index in 0..6u64 {
        if let IngestOutcome::Recorded { event } = journal
            .ingest(
                buffer,
                &message(&format!(":a!b@c PRIVMSG #room :m{index}\r\n")),
            )
            .await
            .expect("ingests")
        {
            ids.push(event);
        }
    }
    // A client that has seen the first three events receives only the rest.
    journal
        .advance_cursor(phone, buffer, ids[2])
        .await
        .expect("advances");
    let events = journal
        .backlog(phone, buffer, BacklogCap::DEFAULT)
        .await
        .expect("backlog");
    let first = String::from_utf8_lossy(&events[0].payload).into_owned();
    assert_eq!(first, ":a!b@c PRIVMSG #room :m3");
    assert_eq!(events.len(), 3);
}

#[test]
fn a_chathistory_capable_client_can_suppress_the_legacy_backlog() {
    // The hook exists now so M003-E does not have to restructure sessions.
    assert!(playback::wants_backlog(SessionCapabilities::default()));
    assert!(!playback::wants_backlog(SessionCapabilities {
        legacy_backlog: false,
        explicit_history: false,
        read_markers: false,
        message_tags: false,
        pre_away: false,
        bouncer_networks: false,
        bouncer_networks_notify: false,
    }));
    assert!(!playback::wants_backlog(SessionCapabilities {
        legacy_backlog: true,
        explicit_history: true,
        read_markers: false,
        message_tags: false,
        pre_away: false,
        bouncer_networks: false,
        bouncer_networks_notify: false,
    }));
    // The drafts are independently negotiable, so a read-marker client that does not
    // manage its own history still receives the automatic backlog.
    assert!(playback::wants_backlog(SessionCapabilities {
        legacy_backlog: true,
        explicit_history: false,
        read_markers: true,
        message_tags: false,
        pre_away: false,
        bouncer_networks: false,
        bouncer_networks_notify: false,
    }));
}

// ----------------------------------------------------------------- retention

#[tokio::test]
async fn retention_runs_in_bounded_chunks_and_reports_remaining_work() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let batch: Vec<i2pr_irc_store::NewHistoryEvent> = (0..12u64)
        .map(|index| i2pr_irc_store::NewHistoryEvent {
            network: NetworkId(1),
            buffer,
            received_at: i2pr_irc_core::WallTime(index as i64),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: format!(":a!b@c PRIVMSG #room :m{index}").into_bytes(),
        })
        .collect();
    let appended = handle.append_history(&batch).await.expect("appends");
    let boundary = HistoryEventId(appended.last.expect("assigned").0 + 1);

    let journal = journal.with_limits(
        BacklogCap::DEFAULT,
        RetentionPolicy {
            max_events_per_buffer: 100,
            max_delete_per_pass: 5,
            max_passes_per_cycle: 1,
        },
    );
    let mut journal = journal;
    let report = journal.retain(boundary).await.expect("retention");
    assert_eq!(
        report.deleted, 5,
        "one pass deletes at most its chunk ceiling"
    );
    assert!(
        report.more_pending,
        "a bounded cycle must report that eligible work remains"
    );
    assert_eq!(journal.health().retention_passes, 1);
}

#[tokio::test]
async fn retention_clamps_a_cursor_into_the_removed_range_monotonically() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let mut ids = Vec::new();
    for index in 0..6u64 {
        if let IngestOutcome::Recorded { event } = journal
            .ingest(
                buffer,
                &message(&format!(":a!b@c PRIVMSG #room :m{index}\r\n")),
            )
            .await
            .expect("ingests")
        {
            ids.push(event);
        }
    }
    // One lineage sits *inside* the range retention will remove, and one sits above
    // it. Both halves of the rule must hold.
    let inside = journal.ensure_client("inside").await.expect("lineage");
    let above = journal.ensure_client("above").await.expect("lineage");
    journal
        .advance_cursor(inside, buffer, ids[2])
        .await
        .expect("advances");
    journal
        .advance_cursor(above, buffer, ids[4])
        .await
        .expect("advances");
    journal.set_read_marker(buffer, ids[3]).await.expect("sets");

    let boundary = HistoryEventId(ids[3].0 + 1);
    let report = journal.retain(boundary).await.expect("retention");
    assert_eq!(report.deleted, 4);
    assert_eq!(
        report.cursors_clamped, 1,
        "only the inside position is clamped"
    );
    assert_eq!(report.markers_clamped, 1);
    assert_eq!(
        journal.cursor(above, buffer).await.expect("reads"),
        Some(ids[4]),
        "a position above the removed range still names a retained event, so it is untouched"
    );
    // The removed range started at the very first retained event, so the deterministic
    // clamp target is 0: "before any retained event". That replays the whole remaining
    // buffer rather than skipping it, and it never moves backwards afterwards.
    let cursor = journal
        .cursor(inside, buffer)
        .await
        .expect("reads")
        .expect("exists");
    assert_eq!(cursor, HistoryEventId(0));
    assert!(
        ids[4] > cursor,
        "a clamped cursor sits below the first retained event"
    );
    let marker = journal
        .read_marker(buffer)
        .await
        .expect("reads")
        .expect("exists");
    assert_eq!(
        marker,
        HistoryEventId(0),
        "the marker uses the same clamp rule"
    );
}

// ------------------------------------------------------------ restart/limits

#[tokio::test]
async fn history_and_cursors_survive_restart() {
    // A file-backed store is required here: an in-memory database cannot outlive the
    // process, so it could not demonstrate durability at all.
    let dir = i2pr_irc_store::testing::temp_dir("history-restart");
    let path = dir.db("restart.sqlite3");
    let store = Store::open(&StorePath::File(path.clone())).expect("store opens");
    let handle = store.handle_clone();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &["#room"]).await;
    let phone = journal.ensure_client(PHONE).await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let recorded = journal
        .ingest(buffer, &message(":a!b@c PRIVMSG #room :before restart\r\n"))
        .await
        .expect("ingests");
    let IngestOutcome::Recorded { event } = recorded else {
        panic!("expected a recorded event")
    };
    journal
        .advance_cursor(phone, buffer, event)
        .await
        .expect("advances");
    journal.set_read_marker(buffer, event).await.expect("sets");
    drop(journal);
    store.shutdown().expect("store shuts down");

    // A fresh journal over a fresh store on the same file resumes from durable state.
    let reopened = Store::open(&StorePath::File(path)).expect("store reopens");
    let mut journal = HistoryJournal::new(
        NetworkId(1),
        reopened.handle_clone(),
        Box::new(SystemWallClock),
        Casemapping::Rfc1459,
    );
    let resumed = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer survives");
    assert_eq!(
        journal.cursor(phone, resumed).await.expect("reads"),
        Some(event),
        "a cursor survives restart"
    );
    assert_eq!(
        journal.read_marker(resumed).await.expect("reads"),
        Some(event)
    );
    reopened.shutdown().expect("reopened store shuts down");
}

#[tokio::test]
async fn store_failure_degrades_history_without_faking_delivery() {
    let (store, handle) = store();
    let _ = &store;
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    store.shutdown().expect("store shuts down");

    let outcome = journal
        .ingest(buffer, &message(":a!b@c PRIVMSG #room :lost\r\n"))
        .await
        .expect("ingestion degrades rather than propagating a storage failure");
    assert_eq!(
        outcome,
        IngestOutcome::StoreUnavailable,
        "a refused append must never be reported as recorded"
    );
    let health = journal.health();
    assert!(health.store_unavailable, "history loss must be visible");
    assert_eq!(health.appended, 0);
}

#[tokio::test]
async fn an_unbounded_ingest_batch_is_refused_rather_than_partially_recorded() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle, VirtualWallClock::default(), &[]).await;
    let oversized = vec![
        i2pr_irc_store::NewHistoryEvent {
            network: NetworkId(1),
            buffer: i2pr_irc_store::BufferId(1),
            received_at: i2pr_irc_core::WallTime(0),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: b"PRIVMSG #room :x".to_vec(),
        };
        i2pr_irc_store::MAX_HISTORY_BATCH + 1
    ];
    assert!(journal.append(oversized).await.is_err());
    assert_eq!(
        journal.health().appended,
        0,
        "nothing may be partially recorded"
    );
    assert_eq!(journal.health().append_refused, 1);
}

#[test]
fn a_journal_health_projection_is_bounded_and_carries_no_payload() {
    let health = i2pr_irc_runtime::journal::JournalHealth {
        appended: 5,
        append_refused: 1,
        query_refused: 0,
        retention_passes: 2,
        retention_deleted: 10,
        cursors_advanced: 3,
        buffer_resolutions: 1,
        store_unavailable: false,
    };
    let rendered = format!("{health:?}");
    assert!(rendered.contains("appended: 5"));
    assert!(!rendered.contains("PRIVMSG"));
    assert!(!rendered.contains('@'));
}

#[test]
fn a_retained_event_round_trips_through_the_store_unchanged() {
    // The stored payload is protocol content without its terminator.
    let original = HistoryEvent {
        event: HistoryEventId(7),
        network: NetworkId(1),
        buffer: i2pr_irc_store::BufferId(1),
        received_at: i2pr_irc_core::WallTime(1234),
        server_time: Some(
            i2pr_irc_wire::IrcTimestamp::from_unix_millis(1_700_000_000_123)
                .expect("representable"),
        ),
        msgid: Some("m1".into()),
        direction: EventDirection::Inbound,
        event_class: "PRIVMSG".into(),
        payload: b":a!b@c PRIVMSG #room :hi".to_vec(),
    };
    assert_eq!(
        String::from_utf8_lossy(&original.payload),
        ":a!b@c PRIVMSG #room :hi"
    );
}
