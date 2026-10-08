use i2pr_irc_core::{I2pEndpoint, NetworkId, WallTime};
use i2pr_irc_store::{
    BufferKind, EventDirection, HistoryQuery, HistoryQueryBound, MsgidLookup, NetworkRecord,
    NewHistoryEvent, RegistrationActionKind, RegistrationActionPhase, RetentionRequest,
    SearchFields, SearchQuery, SearchTerm, Store, StoreEncryption, StoreKey, StoreOpenOptions,
    StorePath, StoredRegistrationAction, StoredSecret, attached_channels, export_encrypted_copy,
    testing,
};
use i2pr_irc_wire::IrcTimestamp;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

const SENTINELS: &[&[u8]] = &[
    b"sentinel-sasl-user-71a2",
    b"sentinel-sasl-password-4c93",
    b"sentinel-action-prejoin-a841",
    b"sentinel-action-recovery-b726",
    b"sentinel-display-name-3e10",
    b"sentinel-router.i2p",
    b"sentinel-channel-5c21",
    b"sentinel-history-body-92d4",
    b"sentinel-history-sender-f018",
    b"sentinel-history-target-8a33",
    b"sentinelftsonlyterm6b51",
    b"sentinel-msgid-7a18",
    b"2026-10-08T12:34:56.789Z",
];

fn key(byte: u8) -> StoreKey {
    StoreKey::from_bytes([byte; 32])
}

fn synthetic_network() -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(71),
        display_name: "sentinel-display-name-3e10".to_owned(),
        endpoint: I2pEndpoint::parse("sentinel-router.i2p").expect("test endpoint parses"),
        nick: "privacybot".to_owned(),
        username: "sentinel-sasl-user-71a2".to_owned(),
        realname: "synthetic qualification fixture".to_owned(),
        sasl: Some((
            "sentinel-sasl-user-71a2".to_owned(),
            StoredSecret::new("sentinel-sasl-password-4c93".to_owned()),
        )),
        desired_channels: attached_channels(&["#sentinel-channel-5c21".to_owned()]),
        auto_away: false,
        keep_nick: false,
    }
}

fn forbidden_plaintext(paths: &[PathBuf]) -> Vec<u8> {
    let mut contents = Vec::new();
    for path in paths {
        if let Ok(bytes) = fs::read(path) {
            contents.extend(bytes);
        }
    }
    contents
}

fn assert_no_sentinels(paths: &[PathBuf]) {
    let bytes = forbidden_plaintext(paths);
    for sentinel in SENTINELS {
        assert!(
            !bytes.windows(sentinel.len()).any(|part| part == *sentinel),
            "encrypted database content must not contain a synthetic sentinel"
        );
    }
    assert!(!bytes.windows(16).any(|part| part == b"SQLite format 3\0"));
}

fn database_and_sidecars(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        paths.push(PathBuf::from(sidecar));
    }
    paths
}

#[tokio::test]
async fn encrypted_closed_files_hide_sentinels_and_restart_preserves_store_semantics() {
    let dir = testing::temp_dir("encrypted-sentinel-qualification");
    let encrypted = dir.db("encrypted.sqlite3");
    let rotated = dir.db("rotated.sqlite3");
    let store = Store::open_with_options(
        &StorePath::File(encrypted.clone()),
        StoreOpenOptions::encrypted(key(0x71)),
    )
    .expect("encrypted store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&synthetic_network())
        .await
        .expect("network saves");
    let actions = [
        StoredRegistrationAction {
            kind: RegistrationActionKind::Message,
            phase: RegistrationActionPhase::PreJoin,
            target: "NickServ".to_owned(),
            payload: StoredSecret::new("sentinel-action-prejoin-a841".to_owned()),
        },
        StoredRegistrationAction {
            kind: RegistrationActionKind::Message,
            phase: RegistrationActionPhase::FallbackRecovery,
            target: "NickServ".to_owned(),
            payload: StoredSecret::new("sentinel-action-recovery-b726".to_owned()),
        },
    ];
    handle
        .save_registration_actions(NetworkId(71), &actions)
        .await
        .expect("synthetic action payloads save");
    let buffer = handle
        .resolve_buffer(NetworkId(71), BufferKind::Channel, "#sentinel-channel-5c21")
        .await
        .expect("buffer resolves")
        .buffer;
    let timestamp =
        IrcTimestamp::parse(b"2026-10-08T12:34:56.789Z").expect("canonical timestamp parses");
    let appended = handle
        .append_history(&[
            NewHistoryEvent {
                network: NetworkId(71),
                buffer,
                received_at: WallTime(1_791_468_896),
                server_time: Some(timestamp),
                msgid: Some("sentinel-msgid-7a18".to_owned()),
                direction: EventDirection::Inbound,
                event_class: "PRIVMSG".to_owned(),
                payload: b":sentinel-history-sender-f018!u@h PRIVMSG #sentinel-channel-5c21 :sentinel-history-body-92d4".to_vec(),
                search: Some(SearchFields {
                    sender: "sentinel-history-sender-f018".to_owned(),
                    target: "sentinel-history-target-8a33".to_owned(),
                    body: "sentinelftsonlyterm6b51".to_owned(),
                }),
            },
            NewHistoryEvent {
                network: NetworkId(71),
                buffer,
                received_at: WallTime(1_791_468_897),
                server_time: None,
                msgid: None,
                direction: EventDirection::Inbound,
                event_class: "PRIVMSG".to_owned(),
                payload: b":other!u@h PRIVMSG #sentinel-channel-5c21 :opaque two".to_vec(),
                search: Some(SearchFields {
                    sender: "other".to_owned(),
                    target: "#sentinel-channel-5c21".to_owned(),
                    body: "opaque two".to_owned(),
                }),
            },
        ])
        .await
        .expect("synthetic history saves");
    let first = appended.first.expect("first event exists");
    let last = appended.last.expect("last event exists");
    let (client, _) = handle
        .create_client("sentinel-client")
        .await
        .expect("client creates");
    handle
        .advance_cursor(client, buffer, first)
        .await
        .expect("cursor saves");
    handle
        .advance_read_marker(buffer, first)
        .await
        .expect("read marker saves");
    let indexed = handle
        .search(&SearchQuery {
            network: NetworkId(71),
            buffers: vec![buffer],
            sender: None,
            after: None,
            before: None,
            terms: vec![
                SearchTerm::parse("sentinelftsonlyterm6b51").expect("sentinel search term parses"),
            ],
            limit: 10,
        })
        .await
        .expect("FTS query works before shutdown");
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].event, first);
    store.shutdown().expect("encrypted worker shuts down");

    // Plaintext control: the same scanner detects synthetic metadata and secrets
    // in ordinary SQLite, proving the encrypted negative check is discriminating.
    let plaintext = dir.db("plaintext-control.sqlite3");
    let plaintext_store =
        Store::open(&StorePath::File(plaintext.clone())).expect("plaintext control opens");
    plaintext_store
        .handle()
        .save_network(&synthetic_network())
        .await
        .expect("control network saves");
    plaintext_store
        .shutdown()
        .expect("plaintext control shuts down");
    let plain_bytes = forbidden_plaintext(std::slice::from_ref(&plaintext));
    assert!(SENTINELS[..6].iter().any(|sentinel| {
        plain_bytes
            .windows(sentinel.len())
            .any(|part| part == *sentinel)
    }));

    assert_no_sentinels(&database_and_sidecars(&encrypted));
    export_encrypted_copy(
        &encrypted,
        StoreEncryption::Encrypted(key(0x71)),
        &rotated,
        key(0x72),
    )
    .expect("rotation validates destination");
    assert_no_sentinels(&database_and_sidecars(&rotated));

    let restarted = Store::open_with_options(
        &StorePath::File(rotated.clone()),
        StoreOpenOptions::encrypted(key(0x72)),
    )
    .expect("correct key restarts rotated store");
    let handle = restarted.handle_clone();
    let networks = handle.load_networks().await.expect("network loads");
    assert_eq!(networks.len(), 1);
    assert_eq!(
        networks[0].sasl.as_ref().map(|(_, secret)| secret.expose()),
        Some("sentinel-sasl-password-4c93")
    );
    assert_eq!(
        handle
            .load_registration_actions(NetworkId(71))
            .await
            .expect("actions load")
            .iter()
            .map(|action| action.payload.expose())
            .collect::<Vec<_>>(),
        vec![
            "sentinel-action-prejoin-a841",
            "sentinel-action-recovery-b726"
        ]
    );
    assert_eq!(
        handle
            .query_history(&HistoryQuery {
                buffer,
                bound: HistoryQueryBound {
                    after: None,
                    before: None,
                    limit: 10,
                },
            })
            .await
            .expect("CHATHISTORY event query works")
            .len(),
        2
    );
    assert_eq!(
        handle
            .resolve_msgid(NetworkId(71), "sentinel-msgid-7a18")
            .await,
        Ok(MsgidLookup::Unique(first))
    );
    assert_eq!(
        handle
            .get_cursor(client, buffer)
            .await
            .expect("cursor reads"),
        Some(first)
    );
    assert_eq!(
        handle.get_read_marker(buffer).await.expect("marker reads"),
        Some(first)
    );
    assert_eq!(
        handle
            .search(&SearchQuery {
                network: NetworkId(71),
                buffers: vec![buffer],
                sender: None,
                after: None,
                before: None,
                terms: vec![SearchTerm::parse("sentinelftsonlyterm6b51").unwrap()],
                limit: 10,
            })
            .await
            .expect("FTS works after restart")[0]
            .event,
        first
    );
    assert_eq!(
        handle
            .retain(&RetentionRequest {
                network: NetworkId(71),
                before: last,
                max_delete: 10,
            })
            .await
            .expect("bounded retention works")
            .deleted,
        1
    );
    restarted.shutdown().expect("rotated worker shuts down");

    assert_eq!(
        Store::open_with_options(
            &StorePath::File(rotated),
            StoreOpenOptions::encrypted(key(0x71)),
        )
        .err()
        .map(|error| *error.kind()),
        Some(i2pr_irc_store::StoreErrorKind::KeyRejected),
        "old key cannot open rotated destination"
    );
}

#[tokio::test]
async fn bounded_plaintext_and_encrypted_performance_smoke() {
    for encrypted in [false, true] {
        let dir = testing::temp_dir(if encrypted {
            "perf-encrypted"
        } else {
            "perf-plain"
        });
        let path = dir.db("store.sqlite3");
        let open_start = Instant::now();
        let store = if encrypted {
            Store::open_with_options(
                &StorePath::File(path.clone()),
                StoreOpenOptions::encrypted(key(0x73)),
            )
            .expect("encrypted store opens")
        } else {
            Store::open(&StorePath::File(path.clone())).expect("plaintext store opens")
        };
        let open_elapsed = open_start.elapsed();
        let handle = store.handle_clone();
        handle
            .save_network(&synthetic_network())
            .await
            .expect("network saves");
        let buffer = handle
            .resolve_buffer(NetworkId(71), BufferKind::Channel, "#smoke")
            .await
            .expect("buffer resolves");
        let rows = (0..128)
            .map(|index| NewHistoryEvent {
                network: NetworkId(71),
                buffer: buffer.buffer,
                received_at: WallTime(1_791_468_900 + index),
                server_time: None,
                msgid: None,
                direction: EventDirection::Inbound,
                event_class: "PRIVMSG".to_owned(),
                payload: format!(":alice!u@h PRIVMSG #smoke :bounded smoke token {index}")
                    .into_bytes(),
                search: Some(SearchFields {
                    sender: "alice".to_owned(),
                    target: "#smoke".to_owned(),
                    body: format!("bounded smoke token {index}"),
                }),
            })
            .collect::<Vec<_>>();
        let append_start = Instant::now();
        handle
            .append_history(&rows)
            .await
            .expect("bounded append batch completes");
        let append_elapsed = append_start.elapsed();
        let query_start = Instant::now();
        let hits = handle
            .search(&SearchQuery {
                network: NetworkId(71),
                buffers: vec![buffer.buffer],
                sender: None,
                after: None,
                before: None,
                terms: vec![SearchTerm::parse("smoke").unwrap()],
                limit: 10,
            })
            .await
            .expect("representative FTS query completes");
        let query_elapsed = query_start.elapsed();
        assert_eq!(hits.len(), 10);
        eprintln!(
            "store performance smoke encrypted={encrypted}: open={open_elapsed:?}, append_128={append_elapsed:?}, fts_query={query_elapsed:?}"
        );
        store
            .shutdown()
            .expect("performance smoke store shuts down");
    }
}
