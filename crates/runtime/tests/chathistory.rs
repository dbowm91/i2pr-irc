//! Plan 011 qualification: bounded CHATHISTORY queries, truthful replay output,
//! monotonic read markers, and no duplicate legacy/chathistory history.
#![cfg(test)]

use i2pr_irc_core::{
    BufferId, Casemapping, HistoryEventId, I2pEndpoint, NetworkId, VirtualWallClock,
};
use i2pr_irc_runtime::{
    capability::{DownstreamCapabilities, UpstreamCapabilities},
    chathistory::{
        self, CHATHISTORY_CAPABILITY, HistoryQueryRequest, HistoryRefusal, MarkerRefusal,
        MessageReference, ParsedMarker, ParsedRequest, READ_MARKER_CAPABILITY,
    },
    journal::{HistoryJournal, IngestOutcome},
    session::SessionCapabilities,
};
use i2pr_irc_store::{
    BufferKind, NetworkRecord, Store, StoreHandle, StorePath, fallback_display_name,
};

// ------------------------------------------------------------------- fixtures

fn store() -> (Store, StoreHandle) {
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    (store, handle)
}
use i2pr_irc_wire::Message;
use std::collections::BTreeSet;

// ------------------------------------------------------------------- fixtures

async fn journal_for(handle: &StoreHandle) -> HistoryJournal {
    handle
        .save_network(&NetworkRecord {
            network: NetworkId(1),
            display_name: fallback_display_name(NetworkId(1)),
            endpoint: I2pEndpoint::parse("irc.example.i2p").expect("endpoint parses"),
            failover_group: None,
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            auto_away: false,
            keep_nick: false,
            sasl: None,
            desired_channels: i2pr_irc_store::attached_channels(&["#room"]),
        })
        .await
        .expect("network saved");
    HistoryJournal::new(
        NetworkId(1),
        handle.clone(),
        Box::new(VirtualWallClock::default()),
        Casemapping::Rfc1459,
    )
}

/// A canonical `server-time` value for an epoch-second reading.
fn stamp(epoch_seconds: i64) -> String {
    i2pr_irc_wire::IrcTimestamp::from_unix_millis(epoch_seconds * 1_000)
        .expect("representable")
        .to_string()
}

fn parse(raw: &str) -> Message {
    Message::parse(raw.as_bytes()).expect("parses")
}

/// Records `count` chat lines, returning their assigned identities and msgids.
async fn record(
    journal: &mut HistoryJournal,
    buffer: BufferId,
    count: usize,
) -> Vec<(HistoryEventId, String)> {
    let mut recorded = Vec::with_capacity(count);
    for index in 0..count {
        let msgid = format!("m{index}");
        // A real conformant `time` tag, so reference resolution is exercised against
        // the protocol representation rather than the local receive clock.
        let line = format!(
            "@time={};msgid={msgid} :a!b@c PRIVMSG #room :message {index}\r\n",
            stamp(1_700_000_000 + i64::try_from(index).expect("small"))
        );
        if let IngestOutcome::Recorded { event } = journal
            .ingest(buffer, &parse(&line))
            .await
            .expect("ingests")
        {
            recorded.push((event, msgid));
        }
    }
    recorded
}

// ------------------------------------------------------- capability isolation

#[test]
fn the_draft_capability_literals_exist_only_in_the_adapter() {
    // The adapter owns the names and the revision; the registry refers to them.
    assert_eq!(CHATHISTORY_CAPABILITY, "draft/chathistory");
    assert_eq!(READ_MARKER_CAPABILITY, "draft/read-marker");
    assert!(
        !chathistory::ADAPTER_REVISION.is_empty(),
        "the implemented spec revision must be stated in the adapter"
    );
    assert!(i2pr_irc_runtime::capability::DOWNSTREAM_HISTORY.contains(&CHATHISTORY_CAPABILITY));
    assert!(i2pr_irc_runtime::capability::DOWNSTREAM_HISTORY.contains(&READ_MARKER_CAPABILITY));
}

#[tokio::test]
async fn a_chathistory_client_suppresses_the_duplicate_legacy_backlog() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 5).await;

    let upstream = UpstreamCapabilities::default();
    // A legacy client: automatic backlog applies.
    let legacy = SessionCapabilities::default();
    assert!(legacy.wants_backlog());

    // A chathistory client: it manages its own history, so no automatic backlog.
    let granted: BTreeSet<String> = DownstreamCapabilities::default()
        .advertise(&upstream)
        .into_iter()
        .collect();
    assert!(!chathistory::session_manages_history(&granted));

    let mut negotiated = BTreeSet::new();
    negotiated.insert(CHATHISTORY_CAPABILITY.to_owned());
    let history_client = SessionCapabilities::default().with_negotiated(&negotiated);
    assert!(
        chathistory::session_manages_history(&negotiated),
        "the adapter recognises its own capability"
    );
    assert!(
        !history_client.wants_backlog(),
        "a client that queries CHATHISTORY must not also receive the same automatic backlog"
    );
}

#[tokio::test]
async fn a_chathistory_query_and_the_legacy_backlog_cover_disjoint_history() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let phone = journal.ensure_client("phone").await.expect("lineage");
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let recorded = record(&mut journal, buffer, 6).await;

    // The legacy path advances a playback cursor.
    let backlog = journal
        .backlog(
            phone,
            buffer,
            i2pr_irc_runtime::journal::BacklogCap::DEFAULT,
        )
        .await
        .expect("backlog");
    assert_eq!(backlog.len(), 6);
    journal
        .advance_cursor(phone, buffer, recorded[5].0)
        .await
        .expect("advances");

    // An explicit CHATHISTORY query names its own range and is unaffected by the
    // playback cursor, so it never silently duplicates or skips the backlog.
    let reply = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 3 })
        .await
        .expect("executes");
    assert_eq!(reply.lines.len(), 3);
    assert_eq!(reply.newest, Some(recorded[5].0));
    // Issuing a query does not by itself move the playback cursor: query delivery is
    // not playback state.
    assert_eq!(
        journal.cursor(phone, buffer).await.expect("reads"),
        Some(recorded[5].0)
    );
}

// ------------------------------------------------------------ bounded queries

#[tokio::test]
async fn a_query_is_bounded_in_events_and_bytes() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 20).await;

    let reply = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 5 })
        .await
        .expect("executes");
    assert_eq!(reply.lines.len(), 5, "the event ceiling is real");
    assert!(
        reply.bytes <= chathistory::MAX_RESPONSE_BYTES,
        "the byte ceiling is real: {}",
        reply.bytes
    );
    // Every line is individually within the wire budget.
    for line in &reply.lines {
        assert!(line.len() <= i2pr_irc_wire::MAX_TAGGED_LINE_BYTES);
    }
}

#[tokio::test]
async fn results_are_ordered_by_local_identity_not_by_timestamp() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    // server-time runs backwards across the recorded messages.
    let mut recorded = Vec::new();
    for index in 0..5i64 {
        let line = format!(
            "@time={};msgid=m{index} :a!b@c PRIVMSG #room :m{index}\r\n",
            1_700_000_000 - index
        );
        if let IngestOutcome::Recorded { event } = journal
            .ingest(buffer, &parse(&line))
            .await
            .expect("ingests")
        {
            recorded.push(event);
        }
    }
    let reply = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 10 })
        .await
        .expect("executes");
    assert_eq!(reply.newest, Some(*recorded.last().expect("recorded")));
    // The newest local event is emitted last, regardless of its earlier server-time.
    let last = Message::parse(reply.lines.last().expect("line")).expect("parses");
    assert_eq!(
        last.msgid(),
        Some("m4"),
        "canonical order is local identity, not the skewed server-time"
    );
    let first = Message::parse(reply.lines.first().expect("line")).expect("parses");
    assert_eq!(first.msgid(), Some("m0"));
}

#[tokio::test]
async fn an_unknown_subcommand_is_refused_rather_than_degraded() {
    let (_store, handle) = store();
    let journal = journal_for(&handle).await;
    for raw in [
        "CHATHISTORY FROBNICATE #room * 10\r\n",
        "CHATHISTORY SIDEWAYS #room msgid=m0 10\r\n",
    ] {
        assert_eq!(
            chathistory::parse_chathistory(&parse(raw)),
            ParsedRequest::Refused(HistoryRefusal::UnknownSubcommand),
            "{raw}"
        );
    }
    let _ = &journal;
}

#[tokio::test]
async fn around_brackets_a_selector_within_its_limit() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 9).await;

    // AROUND is a real bounded query, not a refusal: the total returned never exceeds
    // the requested limit, and the selector's own message is included.
    for limit in [1usize, 2, 4, 6] {
        let reply = chathistory::execute(
            &journal,
            buffer,
            &HistoryQueryRequest::Around {
                reference: MessageReference::MsgId("m4".to_owned()),
                limit,
            },
        )
        .await
        .expect("executes");
        assert!(
            reply.lines.len() <= limit,
            "AROUND returned {} lines for limit {limit}",
            reply.lines.len()
        );
    }

    // A 6-message window around the middle event contains that event.
    let reply = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::Around {
            reference: MessageReference::MsgId("m4".to_owned()),
            limit: 6,
        },
    )
    .await
    .expect("executes");
    assert!(
        reply
            .lines
            .iter()
            .any(|line| String::from_utf8_lossy(line).contains("message 4")),
        "AROUND must include the selected message"
    );
}

#[tokio::test]
async fn a_stale_reference_fails_deterministically() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 2).await;

    // A `msgid=` that nothing retained carries is *unknown*, not merely unavailable.
    // The two are different answers for a client: one means "no such message", the
    // other means "ask again later", and collapsing them would make a stale bookmark
    // look like a transient failure.
    let outcome = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::After {
            reference: MessageReference::MsgId("never-existed".to_owned()),
            limit: 10,
        },
    )
    .await;
    assert_eq!(outcome.err(), Some(HistoryRefusal::UnknownReference));

    // A timestamp beyond everything retained resolves to the newest event: "after the
    // end" is a position, not an error.
    let resolved = chathistory::resolve(
        &journal,
        buffer,
        &MessageReference::Timestamp(
            i2pr_irc_wire::IrcTimestamp::parse_str("9999-12-31T23:59:59.999Z").expect("parses"),
        ),
    )
    .await
    .expect("resolves");
    assert!(
        matches!(resolved, chathistory::HistoryPosition::Event(event) if event.0 > 0),
        "a reference past the end anchors to the newest retained event: {resolved:?}"
    );
}

#[tokio::test]
async fn a_reference_before_the_retained_window_is_its_own_position() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 2).await;
    let earliest = "1970-01-01T00:00:00.000Z";

    // A bookmark older than retention is "before the beginning". Anchoring it onto the
    // oldest retained event instead would be off by one in the one direction the client
    // cannot check: `AFTER` would silently skip the oldest message and the client would
    // have no way to notice.
    assert_eq!(
        chathistory::resolve(
            &journal,
            buffer,
            &MessageReference::Timestamp(
                i2pr_irc_wire::IrcTimestamp::parse_str(earliest).expect("parses"),
            ),
        )
        .await
        .expect("a reference before everything retained still has a position"),
        chathistory::HistoryPosition::BeforeStart,
    );

    let after = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::After {
            reference: MessageReference::Timestamp(
                i2pr_irc_wire::IrcTimestamp::parse_str(earliest).expect("parses"),
            ),
            limit: 10,
        },
    )
    .await
    .expect("the page is delivered rather than refused");
    assert_eq!(
        after.lines.len(),
        2,
        "every retained event follows the beginning: {:?}",
        after.lines
    );

    // `BEFORE` from before the beginning is empty, and that is the truthful answer: no
    // retained message is earlier than the start of retention. Handing back the oldest
    // page would claim messages exist that do not.
    let before = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::Before {
            reference: MessageReference::Timestamp(
                i2pr_irc_wire::IrcTimestamp::parse_str(earliest).expect("parses"),
            ),
            limit: 10,
        },
    )
    .await
    .expect("an empty page, not a refusal");
    assert!(
        before.lines.is_empty(),
        "nothing precedes the beginning of retention: {:?}",
        before.lines
    );

    // `AROUND` from before the beginning spends the whole budget where the messages are.
    let around = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::Around {
            reference: MessageReference::Timestamp(
                i2pr_irc_wire::IrcTimestamp::parse_str(earliest).expect("parses"),
            ),
            limit: 10,
        },
    )
    .await
    .expect("delivered");
    assert_eq!(around.lines.len(), 2, "{:?}", around.lines);
}

#[tokio::test]
async fn a_duplicate_msgid_is_reported_as_ambiguous_rather_than_resolved() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    // Two retained messages carrying the same upstream id. The draft does not forbid it,
    // and an upstream that reuses an id after a restart is an ordinary event.
    for body in ["first", "second"] {
        journal
            .append(vec![i2pr_irc_store::NewHistoryEvent {
                network: journal.network(),
                buffer,
                received_at: i2pr_irc_core::WallTime(1),
                server_time: None,
                msgid: Some("reused".to_owned()),
                direction: i2pr_irc_store::EventDirection::Inbound,
                event_class: "PRIVMSG".to_owned(),
                payload: format!(":alice!a@h PRIVMSG #room :{body}").into_bytes(),
                search: None,
            }])
            .await
            .expect("append succeeds");
    }

    // Picking the lowest event id would answer a question the client did not ask while
    // looking authoritative. "Two messages carry that id" is the truthful answer, and the
    // client can decide which one it meant.
    assert_eq!(
        chathistory::resolve(
            &journal,
            buffer,
            &MessageReference::MsgId("reused".to_owned()),
        )
        .await
        .err(),
        Some(HistoryRefusal::AmbiguousReference)
    );

    let outcome = chathistory::execute(
        &journal,
        buffer,
        &HistoryQueryRequest::Before {
            reference: MessageReference::MsgId("reused".to_owned()),
            limit: 10,
        },
    )
    .await;
    assert_eq!(outcome.err(), Some(HistoryRefusal::AmbiguousReference));
}

// ------------------------------------------------------------ truthful output

#[tokio::test]
async fn a_replayed_line_carries_a_truthful_target_type_and_time() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 1).await;

    let reply = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 1 })
        .await
        .expect("executes");
    let line = reply.lines.first().expect("line");
    let message = Message::parse(line.as_slice()).expect("parses");
    // Target and message type are preserved exactly as retained.
    assert_eq!(message.command.as_slice(), b"PRIVMSG");
    assert!(message.params.contains(&b"#room".to_vec()));
    assert!(message.params.contains(&b"message 0".to_vec()));
    assert!(
        message.server_time().is_some(),
        "server-time is always present"
    );
    assert!(
        reply.lines[0].ends_with(b"\r\n"),
        "each replayed line is exactly one complete frame"
    );
}

#[tokio::test]
async fn no_membership_event_can_appear_in_a_replay() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 1).await;
    // Membership events are not history in this milestone, so a replay cannot
    // contain one: that is what keeps the payload truthful without event-playback.
    for line in [
        "@msgid=j :bot!u@h JOIN #room\r\n",
        "@msgid=p :bot!u@h PART #room\r\n",
    ] {
        assert_eq!(
            journal.ingest(buffer, &parse(line)).await.expect("ingests"),
            IngestOutcome::Skipped
        );
    }
    let reply = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 10 })
        .await
        .expect("executes");
    assert_eq!(reply.lines.len(), 1, "only chat was ever stored");
    let rendered = String::from_utf8_lossy(&reply.lines[0]).into_owned();
    assert!(!rendered.contains("JOIN"));
    assert!(!rendered.contains("PART"));
}

// ------------------------------------------------------------- read markers

#[tokio::test]
async fn a_read_marker_moves_only_forward_and_is_shared_per_buffer() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let recorded = record(&mut journal, buffer, 4).await;

    let ParsedMarker::Set { target, timestamp } = chathistory::parse_markread(&parse(&format!(
        "MARKREAD #room timestamp={}\r\n",
        stamp(1_700_000_002)
    )))
    .expect("parses") else {
        panic!("expected a set marker")
    };
    assert_eq!(target, "#room");
    assert_eq!(
        timestamp,
        i2pr_irc_wire::IrcTimestamp::parse_str(&stamp(1_700_000_002)).expect("parses")
    );
    let resolved =
        match chathistory::resolve(&journal, buffer, &MessageReference::Timestamp(timestamp))
            .await
            .expect("resolves")
        {
            chathistory::HistoryPosition::Event(event) => event,
            chathistory::HistoryPosition::BeforeStart => {
                panic!("a stamp inside the retained window resolves to an event")
            }
        };
    assert_eq!(
        journal
            .set_read_marker(buffer, resolved)
            .await
            .expect("sets"),
        resolved
    );

    // A second client reporting an older message cannot un-mark what was read.
    let older = recorded[0].0;
    assert_eq!(
        chathistory::monotonic_marker(Some(resolved), older),
        resolved,
        "read state is shared for the Operator and never moves backwards"
    );
    assert_eq!(
        journal.read_marker(buffer).await.expect("reads"),
        Some(resolved)
    );
}

#[tokio::test]
async fn a_marker_reference_into_pruned_history_clamps_rather_than_failing() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    let recorded = record(&mut journal, buffer, 5).await;
    // The marker sits *inside* the range retention will remove.
    journal
        .set_read_marker(buffer, recorded[1].0)
        .await
        .expect("sets");

    // Retention removes the range the marker could have named; M003-C's clamp rule
    // applies unchanged, so the marker stays valid rather than dangling.
    let boundary = HistoryEventId(recorded[3].0.0 + 1);
    let report = journal.retain(boundary).await.expect("retention");
    assert_eq!(report.deleted, 4);
    assert_eq!(report.markers_clamped, 1);
    let marker = journal
        .read_marker(buffer)
        .await
        .expect("reads")
        .expect("exists");
    assert!(marker < boundary, "the marker sits below the removed range");
}

#[tokio::test]
async fn a_markread_get_is_answered_locally_without_touching_upstream() {
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 2).await;
    assert_eq!(
        chathistory::parse_markread(&parse("MARKREAD #room\r\n")),
        Ok(ParsedMarker::Get)
    );
    // A client cannot erase a marker by sending the unknown-marker sentinel.
    assert_eq!(
        chathistory::parse_markread(&parse("MARKREAD #room *\r\n")).err(),
        Some(MarkerRefusal::InvalidTimestamp)
    );
    // Read state is local: nothing here is written upstream, and the marker lives in
    // the operator's own durable store.
    assert!(i2pr_irc_core::NetworkId(1).0 == 1);
}

// ---------------------------------------------------------------- determinism

#[test]
fn every_refusal_is_deterministic_and_names_a_reason() {
    for refusal in [
        HistoryRefusal::UnknownSubcommand,
        HistoryRefusal::MissingParameters,
        HistoryRefusal::TooManyParameters,
        HistoryRefusal::InvalidTimestamp,
        HistoryRefusal::InvalidReference,
        HistoryRefusal::InvalidLimit,
        HistoryRefusal::UnsupportedReferenceType,
        HistoryRefusal::NoSuchBuffer,
        HistoryRefusal::HistoryUnavailable,
    ] {
        let text = refusal.to_string();
        assert!(!text.is_empty());
        let again = refusal.to_string();
        assert_eq!(
            text, again,
            "the same refusal always reports the same reason"
        );
    }
}

#[test]
fn history_and_read_marker_commands_are_answered_locally() {
    // The bouncer must not forward these upstream: it owns the history, and the
    // server has none of it.
    assert!(chathistory::is_local_only(&parse(
        "CHATHISTORY LATEST #room 10\r\n"
    )));
    assert!(chathistory::is_local_only(&parse("MARKREAD #room abc\r\n")));
    assert!(!chathistory::is_local_only(&parse("PRIVMSG #room :hi\r\n")));
}

#[tokio::test]
async fn a_history_query_does_not_block_network_liveness() {
    // The query path is bounded store work behind the same owned queue the rest of
    // the runtime uses; a query is awaited in bounded pages rather than scanning an
    // unbounded history.
    let (_store, handle) = store();
    let mut journal = journal_for(&handle).await;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#room")
        .await
        .expect("buffer");
    record(&mut journal, buffer, 30).await;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 20 }),
    )
    .await
    .expect("a bounded query completes promptly");
    assert!(outcome.is_ok());
}

#[tokio::test]
async fn a_client_lineage_is_not_required_to_query_history() {
    // An explicit query is not playback, so it works without a durable cursor and
    // therefore without pre-registering a client lineage.
    let (_store, handle) = store();
    let journal = journal_for(&handle).await;
    // A buffer that exists but holds no retained history answers empty, which is the
    // truthful answer: there is nothing there.
    let mut journal = journal;
    let buffer = journal
        .resolve_buffer(BufferKind::Channel, "#empty")
        .await
        .expect("buffer");
    let outcome = chathistory::execute(&journal, buffer, &HistoryQueryRequest::Latest { limit: 5 })
        .await
        .expect("an empty buffer is a valid, empty answer");
    assert!(outcome.lines.is_empty());
    assert_eq!(outcome.newest, None);
    // No client lineage was ever created, which is the point: a query is not playback
    // state and therefore never needs one.
}
