use i2pr_irc_core::{I2pEndpoint, NetworkId, WallTime};
use i2pr_irc_store::{
    BufferKind, EventDirection, NetworkRecord, NewHistoryEvent, SearchFields, SearchQuery,
    SearchTerm, Store, StoreErrorKind, StoreKey, StoreOpenOptions, StorePath, StoredSecret,
    testing,
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
