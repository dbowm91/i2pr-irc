use i2pr_irc_core::{I2pEndpoint, NetworkId, WallTime};
use i2pr_irc_store::{
    BufferKind, EventDirection, NetworkRecord, NewHistoryEvent, RegistrationActionKind,
    RegistrationActionPhase, SearchFields, SearchQuery, SearchTerm, Store, StoreErrorKind,
    StoreKey, StoreOpenOptions, StorePath, StoredRegistrationAction, StoredSecret,
    export_encrypted_copy, testing,
};

fn key(byte: u8) -> StoreKey {
    StoreKey::from_bytes([byte; 32])
}

fn network() -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(1),
        display_name: "net-1".to_owned(),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("I2P endpoint parses"),
        nick: "bot".to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        sasl: Some((
            "sasl-user".to_owned(),
            StoredSecret::new("sasl-pass".to_owned()),
        )),
        desired_channels: Vec::new(),
        auto_away: false,
        keep_nick: false,
    }
}

#[tokio::test]
async fn encrypted_open_reopens_with_key_and_keeps_fts_available() {
    let dir = testing::temp_dir("encrypted");
    let path = dir.db("encrypted.sqlite3");
    let options = StoreOpenOptions::encrypted(key(0x4a));
    let store = Store::open_with_options(&StorePath::File(path.clone()), options)
        .expect("SQLCipher store opens");
    let handle = store.handle_clone();
    handle
        .save_network(&network())
        .await
        .expect("network saves");
    let buffer = handle
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("history buffer resolves");
    handle
        .append_history(&[NewHistoryEvent {
            network: NetworkId(1),
            buffer: buffer.buffer,
            received_at: WallTime(1),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".to_owned(),
            payload: b":alice!u@h PRIVMSG #room :cipherterm".to_vec(),
            search: Some(SearchFields {
                sender: "alice".to_owned(),
                target: "#room".to_owned(),
                body: "cipherterm".to_owned(),
            }),
        }])
        .await
        .expect("history appends");
    store.shutdown().expect("encrypted store stops");

    let bytes = std::fs::read(&path).expect("encrypted file reads");
    assert!(!bytes.windows(16).any(|part| part == b"SQLite format 3\0"));
    assert!(!bytes.windows(10).any(|part| part == b"cipherterm"));

    let reopened = Store::open_with_options(
        &StorePath::File(path.clone()),
        StoreOpenOptions::encrypted(key(0x4a)),
    )
    .expect("correct key reopens store");
    let handle = reopened.handle_clone();
    assert_eq!(
        handle.load_networks().await.expect("networks load").len(),
        1
    );
    let found = handle
        .search(&SearchQuery {
            network: NetworkId(1),
            buffers: Vec::new(),
            sender: None,
            after: None,
            before: None,
            terms: vec![SearchTerm::parse("cipherterm").expect("term parses")],
            limit: 10,
        })
        .await
        .expect("FTS search runs in encrypted store");
    assert_eq!(found.len(), 1);
    reopened.shutdown().expect("reopened store stops");
}

#[test]
fn wrong_key_and_encryption_policy_mismatches_fail_closed() {
    let dir = testing::temp_dir("encrypted-key");
    let path = dir.db("encrypted.sqlite3");
    Store::open_with_options(
        &StorePath::File(path.clone()),
        StoreOpenOptions::encrypted(key(0x4a)),
    )
    .expect("encrypted store opens")
    .shutdown()
    .expect("store stops");
    let original = std::fs::read(&path).expect("encrypted file reads");

    assert_eq!(
        Store::open_with_options(
            &StorePath::File(path.clone()),
            StoreOpenOptions::encrypted(key(0x55)),
        )
        .err()
        .map(|error| *error.kind()),
        Some(StoreErrorKind::KeyRejected),
    );
    assert_eq!(
        Store::open(&StorePath::File(path.clone()))
            .err()
            .map(|error| *error.kind()),
        Some(StoreErrorKind::Open),
        "plaintext mode must not create or overwrite a schema over encrypted bytes"
    );
    assert_eq!(
        std::fs::read(&path).expect("ciphertext remains readable as bytes"),
        original,
        "a failed plaintext open leaves encrypted source bytes unchanged"
    );
}

#[test]
fn encrypted_open_refuses_plaintext_without_mutating_its_contents() {
    let dir = testing::temp_dir("plain-to-encrypted-refused");
    let path = dir.db("plain.sqlite3");
    Store::open(&StorePath::File(path.clone()))
        .expect("plaintext store opens")
        .shutdown()
        .expect("plaintext store stops");
    let before = std::fs::read(&path).expect("source reads");

    assert_eq!(
        Store::open_with_options(
            &StorePath::File(path.clone()),
            StoreOpenOptions::encrypted(key(0x4a)),
        )
        .err()
        .map(|error| *error.kind()),
        Some(StoreErrorKind::KeyRejected),
        "an encrypted-only open must not reinterpret plaintext bytes"
    );
    assert_eq!(std::fs::read(&path).expect("source still reads"), before);
}

#[tokio::test]
async fn plaintext_copy_exports_and_verifies_encrypted_destination() {
    let dir = testing::temp_dir("encrypted-copy");
    let source = dir.db("source.sqlite3");
    let destination = dir.db("encrypted.sqlite3");
    let store = Store::open(&StorePath::File(source.clone())).expect("source opens");
    store
        .handle()
        .save_network(&network())
        .await
        .expect("network saves");
    store
        .handle()
        .add_desired_channel(NetworkId(1), "#kept")
        .await
        .expect("desired channel saves");
    store
        .handle()
        .set_desired_channel_detached(NetworkId(1), "#kept", true)
        .await
        .expect("detached state saves");
    let action = StoredRegistrationAction {
        kind: RegistrationActionKind::Message,
        phase: RegistrationActionPhase::FallbackRecovery,
        target: "NickServ".to_owned(),
        payload: StoredSecret::new("copy-action-secret".to_owned()),
    };
    store
        .handle()
        .save_registration_actions(NetworkId(1), &[action])
        .await
        .expect("registration action saves");
    let buffer = store
        .handle()
        .resolve_buffer(NetworkId(1), BufferKind::Channel, "#room")
        .await
        .expect("buffer resolves");
    store
        .handle()
        .append_history(&[NewHistoryEvent {
            network: NetworkId(1),
            buffer: buffer.buffer,
            received_at: WallTime(9),
            server_time: None,
            msgid: Some("copy-event".to_owned()),
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".to_owned(),
            payload: b":alice!u@h PRIVMSG #room :copied marker".to_vec(),
            search: Some(SearchFields {
                sender: "alice".to_owned(),
                target: "#room".to_owned(),
                body: "copied marker".to_owned(),
            }),
        }])
        .await
        .expect("history saves");
    let large_batch = (0..512)
        .map(|index| NewHistoryEvent {
            network: NetworkId(1),
            buffer: buffer.buffer,
            received_at: WallTime(10 + index),
            server_time: None,
            msgid: None,
            direction: EventDirection::Inbound,
            event_class: "PRIVMSG".to_owned(),
            payload: format!(":alice!u@h PRIVMSG #room :copied marker row {index}").into_bytes(),
            search: Some(SearchFields {
                sender: "alice".to_owned(),
                target: "#room".to_owned(),
                body: format!("copied marker row {index}"),
            }),
        })
        .collect::<Vec<_>>();
    let appended = store
        .handle()
        .append_history(&large_batch)
        .await
        .expect("maximum bounded batch appends");
    let (client, _) = store
        .handle()
        .create_client("copy-client")
        .await
        .expect("client creates");
    let last = appended.last.expect("history has a last event");
    store
        .handle()
        .advance_cursor(client, buffer.buffer, last)
        .await
        .expect("cursor saves");
    store
        .handle()
        .advance_read_marker(buffer.buffer, last)
        .await
        .expect("read marker saves");
    let query = SearchQuery {
        network: NetworkId(1),
        buffers: Vec::new(),
        sender: None,
        after: None,
        before: None,
        terms: vec![SearchTerm::parse("copied").expect("term parses")],
        limit: 10,
    };
    let source_hits = store
        .handle()
        .search(&query)
        .await
        .expect("source FTS query succeeds");
    let source_networks = store
        .handle()
        .load_networks()
        .await
        .expect("source networks load");
    let source_actions = store
        .handle()
        .load_registration_actions(NetworkId(1))
        .await
        .expect("source actions load");
    store.shutdown().expect("source is quiesced");
    let source_before = std::fs::read(&source).expect("source reads");

    export_encrypted_copy(
        &source,
        i2pr_irc_store::StoreEncryption::Plaintext,
        &destination,
        key(0x4a),
    )
    .expect("copy exports and passes ordinary encrypted open checks");
    assert_eq!(
        std::fs::read(&source).expect("source remains"),
        source_before
    );
    let encrypted_bytes = std::fs::read(&destination).expect("destination reads");
    assert!(
        !encrypted_bytes
            .windows(16)
            .any(|part| part == b"SQLite format 3\0")
    );

    let copied = Store::open_with_options(
        &StorePath::File(destination.clone()),
        StoreOpenOptions::encrypted(key(0x4a)),
    )
    .expect("new key opens exported copy");
    assert_eq!(
        copied.handle().load_networks().await.expect("loads").len(),
        1
    );
    let copied_network = copied
        .handle()
        .load_networks()
        .await
        .expect("copied networks load");
    assert_eq!(copied_network, source_networks);
    assert_eq!(
        copied_network[0]
            .sasl
            .as_ref()
            .map(|(_, secret)| secret.expose()),
        Some("sasl-pass")
    );
    assert_eq!(
        copied
            .handle()
            .load_registration_actions(NetworkId(1))
            .await
            .expect("copied actions load"),
        source_actions
    );
    assert_eq!(
        copied
            .handle()
            .get_cursor(client, buffer.buffer)
            .await
            .expect("copied cursor loads"),
        Some(last)
    );
    assert_eq!(
        copied
            .handle()
            .get_read_marker(buffer.buffer)
            .await
            .expect("copied marker loads"),
        Some(last)
    );
    assert_eq!(
        copied
            .handle()
            .search(&query)
            .await
            .expect("copy FTS query succeeds"),
        source_hits,
        "history identity and search results are preserved"
    );
    copied.shutdown().expect("copy stops");

    assert_eq!(
        export_encrypted_copy(
            &source,
            i2pr_irc_store::StoreEncryption::Plaintext,
            &destination,
            key(0x4b),
        )
        .err()
        .map(|error| *error.kind()),
        Some(StoreErrorKind::InvalidRequest(
            "migration destination must be a new distinct file"
        )),
        "existing destination is never overwritten"
    );
}

#[test]
fn encrypted_rotation_and_wrong_old_key_preserve_source() {
    let dir = testing::temp_dir("encrypted-rotation");
    let source = dir.db("old-key.sqlite3");
    let destination = dir.db("new-key.sqlite3");
    Store::open_with_options(
        &StorePath::File(source.clone()),
        StoreOpenOptions::encrypted(key(0x4a)),
    )
    .expect("source encrypted store opens")
    .shutdown()
    .expect("source is quiesced");
    let original = std::fs::read(&source).expect("source bytes read");

    assert_eq!(
        export_encrypted_copy(
            &source,
            i2pr_irc_store::StoreEncryption::Encrypted(key(0x55)),
            &destination,
            key(0x4b),
        )
        .err()
        .map(|error| *error.kind()),
        Some(StoreErrorKind::KeyRejected),
        "wrong old key is rejected before destination creation"
    );
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read(&source).expect("source remains unchanged"),
        original
    );

    export_encrypted_copy(
        &source,
        i2pr_irc_store::StoreEncryption::Encrypted(key(0x4a)),
        &destination,
        key(0x4b),
    )
    .expect("rotation exports and verifies with the new key");
    assert_eq!(
        std::fs::read(&source).expect("old-key source remains"),
        original
    );
    assert_eq!(
        Store::open_with_options(
            &StorePath::File(destination.clone()),
            StoreOpenOptions::encrypted(key(0x4a)),
        )
        .err()
        .map(|error| *error.kind()),
        Some(StoreErrorKind::KeyRejected)
    );
    Store::open_with_options(
        &StorePath::File(destination),
        StoreOpenOptions::encrypted(key(0x4b)),
    )
    .expect("new key opens destination")
    .shutdown()
    .expect("destination stops");
}

#[test]
fn older_plaintext_schema_is_migrated_before_encrypted_export() {
    let dir = testing::temp_dir("encrypted-old-schema");
    let source = dir.db("schema-seven.sqlite3");
    let destination = dir.db("schema-seven-encrypted.sqlite3");
    drop(testing::create_v7_database(&source));

    export_encrypted_copy(
        &source,
        i2pr_irc_store::StoreEncryption::Plaintext,
        &destination,
        key(0x4a),
    )
    .expect("supported predecessor migrates and exports");
    Store::open_with_options(
        &StorePath::File(destination),
        StoreOpenOptions::encrypted(key(0x4a)),
    )
    .expect("export has current schema and metadata")
    .shutdown()
    .expect("verified predecessor copy stops");
}
