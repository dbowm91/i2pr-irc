//! Deriving and maintaining the bounded search side index.
//!
//! This module owns the *representation* the index stores, and nothing else. It performs
//! no queries and holds no state: the store's operations call it to derive fields and to
//! write rows, and everything about what a search means lives in
//! [`crate::model::SearchQuery`].
//!
//! # Why the store decodes at all
//!
//! New events carry their search fields in, derived by the runtime from the message it
//! had already decoded. That is the normal path, and it keeps protocol knowledge out of
//! the hot write path.
//!
//! The migration backfill has no such luxury: it is rebuilding an index for events the
//! store already holds, in a database that exists precisely so history outlives the
//! process. Refusing to decode those payloads would mean an upgraded database whose
//! retained history was silently unsearchable — which is the exact failure the plan names
//! as unacceptable. So the store can decode, once, in bounded batches, at migration time.
//!
//! # Why a row that yields no text is still indexed
//!
//! A stored payload that cannot be decoded into searchable fields is indexed with empty
//! text rather than skipped. Skipping it would leave the index row count disagreeing with
//! the retained row count, and that disagreement is what [`crate::schema`] refuses to open
//! a database over. A message that will never match a term costs one row; a message that
//! made every later open fail costs the Operator their history.

use crate::model::SearchFields;
use crate::{StoreError, StoreErrorKind};
use rusqlite::params;

/// The event classes this build makes searchable.
///
/// Only messages carry searchable text. Indexing joins and modes would grow the index
/// with rows no client could ever ask for, and would make the searchable set depend on
/// which events happened to arrive.
pub const SEARCHABLE_CLASSES: [&str; 2] = ["PRIVMSG", "NOTICE"];

/// Whether this event class is searchable.
pub fn is_searchable(event_class: &str) -> bool {
    SEARCHABLE_CLASSES.contains(&event_class)
}

/// Derives the bounded normalized fields for one stored payload.
///
/// Returns empty fields for anything that is not a decodable message. An empty row is
/// still written; see the module note for why.
pub fn derive_fields(payload: &[u8]) -> Result<(String, String, String), StoreError> {
    // `Message::parse` requires the terminator a wire line carries and a stored payload
    // does not: retained payloads are canonical content *without* their line terminator,
    // which is exactly what makes them safe to write into a downstream frame later. So
    // the terminator is restored for decoding and never stored.
    let mut line = payload.to_vec();
    line.extend_from_slice(b"\r\n");
    let Ok(message) = i2pr_irc_wire::Message::parse(&line) else {
        return Ok((String::new(), String::new(), String::new()));
    };
    if !SEARCHABLE_CLASSES
        .iter()
        .any(|class| class.as_bytes() == message.command.as_slice())
    {
        return Ok((String::new(), String::new(), String::new()));
    }
    let sender = message
        .prefix
        .as_deref()
        .map(lossy_nick)
        .unwrap_or_default();
    let target = message
        .params
        .first()
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .unwrap_or_default();
    let body = message
        .params
        .get(1)
        .map(|param| String::from_utf8_lossy(param).into_owned())
        .unwrap_or_default();
    let fields = SearchFields {
        sender,
        target,
        body,
    };
    // A payload that decoded but produced an oversized field is still indexed, with the
    // field truncated to the ceiling. Truncating search text changes what a term can
    // match; refusing to index changes whether anything can be searched at all. The
    // second is a larger lie.
    let clamped = SearchFields {
        sender: clamp(&fields.sender),
        target: clamp(&fields.target),
        body: clamp(&fields.body),
    };
    Ok((clamped.sender, clamped.target, clamped.body))
}

/// The canonical time one retained event occupies in history.
///
/// `server_time` is the upstream's stamp when it sent one; `received_at` is the local
/// receive time, used only as a fallback.
///
/// The fallback is the whole reason this is one function rather than a column read: an
/// upstream that sends no `server-time` leaves every stored `server_time` NULL, so a
/// reference lookup keyed on that column alone would find nothing in a buffer full of
/// history. The rule therefore has exactly one definition, shared by the index written at
/// append time, the migration backfill, and the replay path that renders a `time=` tag.
pub fn effective_time(
    server_time: Option<i2pr_irc_wire::IrcTimestamp>,
    received_at: i2pr_irc_core::WallTime,
) -> String {
    // `WallTime` is bounded to a range inside the four-digit-year window the wire grammar
    // uses, so this cannot fail for a valid stored row; the epoch fallback exists so a
    // future bound change degrades to an obviously-wrong-but-well-formed value rather
    // than panicking on the history read path.
    server_time
        .unwrap_or_else(|| {
            i2pr_irc_wire::IrcTimestamp::from_unix_millis(
                received_at.unix_seconds().saturating_mul(1_000),
            )
            .unwrap_or(i2pr_irc_wire::IrcTimestamp::EPOCH)
        })
        .to_string()
}

/// The same rule, for a row read back out of storage.
///
/// `None` reports corruption rather than falling back: the column is shape-checked by
/// the schema and calendar-checked on the way in, so text that will not parse is a
/// damaged row. Falling back would substitute a plausible timestamp for the damaged one
/// and make the damage invisible at exactly the point it matters — a reference lookup
/// that silently answers against a wrong position.
pub fn stored_effective_time(
    stored_server_time: Option<&str>,
    received_at: i2pr_irc_core::WallTime,
) -> Option<String> {
    match stored_server_time {
        Some(raw) => Some(
            i2pr_irc_wire::IrcTimestamp::parse(raw.as_bytes())
                .ok()?
                .to_string(),
        ),
        None => Some(effective_time(None, received_at)),
    }
}

/// Writes one index row whose rowid is the event's `HistoryEventId`.
///
/// Using the event id as the rowid is what makes retention exact: deleting a retained row
/// and deleting its index entry are one statement each, so neither can succeed alone.
pub fn insert_search_row(
    tx: &rusqlite::Transaction<'_>,
    event: i64,
    sender: String,
    target: String,
    body: String,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO history_search (rowid, sender, target, body) VALUES (?1, ?2, ?3, ?4)",
        params![event, sender, target, body],
    )
    // The detail is deliberately dropped. `StoreErrorKind::Sqlite` carries no free text
    // because a SQLite message can quote the values in the failing statement, and this
    // statement writes client-supplied search text.
    .map_err(|_| StoreError::new(StoreErrorKind::Sqlite))?;
    Ok(())
}

/// Deletes one event's index row, if it has one.
///
/// A row that is already absent is not an error: retention is idempotent, and a second
/// pass over the same range must not fail because the first pass removed the index entry.
pub fn delete_search_row(tx: &rusqlite::Transaction<'_>, event: i64) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM history_search WHERE rowid = ?1",
        params![event],
    )
    .map_err(|_| StoreError::new(StoreErrorKind::Sqlite))?;
    Ok(())
}

/// The nick portion of a message prefix, without the user and host.
/// The nick a stored prefix names, or an empty string when it names none.
///
/// Delegates to the wire decoder so the migration backfill and the ingestion path read a
/// prefix identically. Two copies of "the nick is the part before the first separator"
/// would be two answers to the same question about the same frame.
fn lossy_nick(prefix: &[u8]) -> String {
    i2pr_irc_wire::prefix_nick(prefix)
        .unwrap_or_default()
        .to_owned()
}

/// Clamps one derived field to the searchable ceiling on a character boundary.
fn clamp(value: &str) -> String {
    if value.len() <= crate::model::MAX_SEARCH_FIELD_BYTES {
        return value.to_owned();
    }
    let mut end = crate::model::MAX_SEARCH_FIELD_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_messages_derive_searchable_text() {
        let (sender, target, body) =
            derive_fields(b":alice!a@h PRIVMSG #room :hello there").expect("decodes");
        assert_eq!(sender, "alice");
        assert_eq!(target, "#room");
        assert_eq!(body, "hello there");

        let (sender, target, body) = derive_fields(b":alice!a@h JOIN #room").expect("decodes");
        assert_eq!(
            (sender, target, body),
            (String::new(), String::new(), String::new())
        );

        let (sender, target, body) = derive_fields(b"not an irc line at all \x01\x02")
            .expect("never panics on arbitrary bytes");
        assert_eq!(
            (sender, target, body),
            (String::new(), String::new(), String::new())
        );
    }

    #[test]
    fn a_payload_the_wire_would_refuse_is_indexed_empty_rather_than_dropped() {
        // An embedded terminator and an over-long line are both things the wire decoder
        // refuses, and a retained payload can hold either. The index must still get a
        // row: skipping it would make the index row count disagree with the retained row
        // count, and that disagreement refuses the next database open.
        for payload in [
            &b":alice!a@h PRIVMSG #room :one\r\ntwo"[..],
            format!(":alice!a@h PRIVMSG #room :{}", "y".repeat(2_000)).as_bytes(),
        ] {
            let (sender, target, body) = derive_fields(payload).expect("never errors");
            assert_eq!(
                (sender, target, body),
                (String::new(), String::new(), String::new()),
                "an undecodable payload is indexed, not dropped"
            );
        }
    }

    #[test]
    fn clamping_is_a_character_boundary_not_a_byte_slice() {
        // Defence in depth rather than a reachable path: a real IRC line is bounded by the
        // wire decoder well below this field ceiling, so nothing stored should need it.
        // It exists so a future representation change cannot turn a multi-byte character
        // into a panic, and it is tested directly because no input reaches it.
        let value = "\u{00e9}".repeat(crate::model::MAX_SEARCH_FIELD_BYTES);
        let clamped = clamp(&value);
        assert!(clamped.len() <= crate::model::MAX_SEARCH_FIELD_BYTES);
        assert!(value.starts_with(&clamped));
    }
}
