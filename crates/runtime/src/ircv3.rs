//! Bounded BATCH tracking and tag mediation.
//!
//! # BATCH
//!
//! Batch identifiers here are **ephemeral and session-local**. They are never
//! durable: a batch exists only inside one live generation on one connection, and a
//! persisted batch id would outlive the connection that owns it and could later
//! collide with an unrelated batch.
//!
//! # Tags
//!
//! Two tag directions are treated differently, on purpose:
//!
//! - **Server-originated** tags that the bouncer understands are preserved, so
//!   `server-time` and `msgid` reach clients that negotiated `message-tags`.
//! - **Client-only** tags are rejected or stripped by default. A bouncer that echoed
//!   arbitrary client tags upstream would let one client forge another client's
//!   metadata. Widening this is M004's decision, under explicit review.

use crate::{RuntimeError, routing::LABEL_TAG};
use i2pr_irc_wire::{IrcTimestamp, MAX_TAG_PREFIX_BYTES, Message, TIME_TAG};

/// Ceiling on simultaneous open batches for one generation.
pub const MAX_OPEN_BATCHES: usize = 64;
/// Ceiling on nesting depth. Deeper nesting is refused rather than tracked.
pub const MAX_BATCH_DEPTH: usize = 4;
/// Ceiling on batch identifier bytes.
pub const MAX_BATCH_ID_BYTES: usize = 32;
/// Ceiling on batch type bytes.
pub const MAX_BATCH_TYPE_BYTES: usize = 32;
/// Ceiling on how many messages one batch may carry before it is force-closed.
pub const MAX_BATCH_MESSAGES: usize = 128;

/// Why one batch parameter was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchError {
    TooManyOpen,
    TooDeep,
    IdTooLong,
    TypeTooLong,
    UnknownReference,
    TooManyMessages,
}

/// One tracked open batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Batch {
    pub id: String,
    pub kind: String,
    pub parent: Option<String>,
    pub depth: u8,
    pub messages: u16,
}

/// Bounded batch tracking for one generation.
#[derive(Default)]
pub struct BatchTracker {
    open: Vec<Batch>,
    next_id: u64,
    issued: u64,
}

impl BatchTracker {
    pub fn open_count(&self) -> usize {
        self.open.len()
    }
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
    pub fn contains(&self, id: &str) -> bool {
        self.open.iter().any(|batch| batch.id == id)
    }
    /// The chain of enclosing batch ids, outermost first, for one open batch.
    pub fn ancestry(&self, id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut cursor = self.open.iter().find(|batch| batch.id == id);
        while let Some(batch) = cursor {
            chain.push(batch.id.clone());
            cursor = batch
                .parent
                .as_deref()
                .and_then(|parent| self.open.iter().find(|b| b.id == parent));
        }
        chain.reverse();
        chain
    }

    /// Opens a batch, assigning a generation-local opaque identifier.
    pub fn open(
        &mut self,
        kind: &str,
        parent: Option<&str>,
        now: std::time::Instant,
    ) -> Result<Batch, BatchError> {
        let _ = now;
        if self.open.len() >= MAX_OPEN_BATCHES {
            return Err(BatchError::TooManyOpen);
        }
        if kind.len() > MAX_BATCH_TYPE_BYTES {
            return Err(BatchError::TypeTooLong);
        }
        let depth = match parent {
            None => 0,
            Some(parent) => {
                let batch = self
                    .open
                    .iter()
                    .find(|batch| batch.id == parent)
                    .ok_or(BatchError::UnknownReference)?;
                batch.depth.saturating_add(1)
            }
        };
        if depth as usize >= MAX_BATCH_DEPTH {
            return Err(BatchError::TooDeep);
        }
        let id = self.allocate_id();
        if id.len() > MAX_BATCH_ID_BYTES {
            return Err(BatchError::IdTooLong);
        }
        let batch = Batch {
            id: id.clone(),
            kind: kind.to_owned(),
            parent: parent.map(str::to_owned),
            depth,
            messages: 0,
        };
        self.open.push(batch.clone());
        self.issued = self.issued.saturating_add(1);
        Ok(batch)
    }

    /// Records one message belonging to an open batch.
    pub fn note_message(&mut self, id: &str) -> Result<(), BatchError> {
        let batch = self
            .open
            .iter_mut()
            .find(|batch| batch.id == id)
            .ok_or(BatchError::UnknownReference)?;
        if batch.messages as usize >= MAX_BATCH_MESSAGES {
            // Force the batch closed rather than tracking an unbounded message count.
            self.open.retain(|batch| batch.id != id);
            return Err(BatchError::TooManyMessages);
        }
        batch.messages = batch.messages.saturating_add(1);
        Ok(())
    }

    /// Closes a batch, returning its ancestors that this also completes.
    pub fn close(&mut self, id: &str) -> Result<Vec<String>, BatchError> {
        let batch = self
            .open
            .iter()
            .position(|batch| batch.id == id)
            .ok_or(BatchError::UnknownReference)?;
        self.open.remove(batch);
        // Closing a nested batch does not close its parent: only a parent's own
        // terminator does.
        Ok(Vec::new())
    }

    fn allocate_id(&mut self) -> String {
        let value = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        format!("b{value:x}")
    }
}

/// How a client-originated tag was treated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagDisposition {
    Forwarded,
    Stripped,
    Rejected,
}

/// The conservative client-tag policy: deny by default.
///
/// A client tag is untrusted input that the upstream server will believe. A forged
/// `msgid` lets one client claim another's history position; a client-supplied `time`
/// lets one client misorder what everyone else sees; and an arbitrary vendor or
/// client-only tag is a channel the bouncer cannot reason about. So the default is to
/// remove everything the client sent unless a reviewed extension defines what that tag
/// means and who may set it.
///
/// Exactly one tag survives, and it is not really the client's: the response label is
/// this bouncer's own correlation mechanism. It is consumed by the response router,
/// translated to an opaque generation-local token, and restored only to the client that
/// sent it. It is never forwarded to the server in the client's spelling, so two clients
/// choosing the same label cannot collide.
///
/// The client-only allowlist is intentionally empty. Adding to it requires a separate
/// privacy and timing review, because a client-only tag is relayed verbatim to everyone
/// else on the network.
pub fn mediate_client_tags(message: &Message, negotiated: bool) -> (Message, TagDisposition) {
    let _ = negotiated;
    let mut mediated = message.clone();
    let mut stripped_any = false;
    for name in message.tags.keys().collect::<Vec<_>>() {
        if name.as_slice() == LABEL_TAG.as_bytes() {
            // Retained for the router; see the note above.
            continue;
        }
        mediated.tags.remove(name);
        stripped_any = true;
    }
    let disposition = if stripped_any {
        TagDisposition::Stripped
    } else {
        TagDisposition::Forwarded
    };
    (mediated, disposition)
}

/// Re-emits a retained or synthetic `server-time` for a downstream client that
/// negotiated `message-tags`.
///
/// This never changes durable order: `server-time` is metadata, and the canonical
/// order remains `HistoryEventId`.
///
/// The synthesized value is always the canonical `YYYY-MM-DDThh:mm:ss.sssZ` form.
/// An integer epoch is *not* a `server-time` value, so emitting one would be a tag
/// the recipient cannot parse.
pub fn synthesize_server_time(message: &mut Message, receive: i2pr_irc_core::WallTime) {
    if message.tags.contains_key(TIME_TAG) {
        // A real upstream value is preserved rather than overwritten.
        return;
    }
    // The local clock has whole-second resolution, so the milliseconds are `.000`
    // rather than invented precision.
    let Some(timestamp) = receive
        .unix_seconds()
        .checked_mul(1_000)
        .and_then(IrcTimestamp::from_unix_millis)
    else {
        // An unrepresentable wall clock leaves the tag absent, which is honest.
        return;
    };
    message
        .tags
        .insert(TIME_TAG.to_vec(), Some(timestamp.to_string().into_bytes()));
}

/// Removes every tag from a message for a client that did not negotiate
/// `message-tags`.
pub fn strip_all_tags(message: &Message) -> Message {
    let mut bare = message.clone();
    bare.tags.clear();
    bare
}

/// Refuses a re-emission whose tag prefix would exceed the wire budget.
pub fn validate_tag_budget(message: &Message) -> Result<(), RuntimeError> {
    let rendered = message.encode().map_err(|_| RuntimeError::Protocol)?;
    let prefix = rendered.iter().take_while(|byte| **byte != b' ').count();
    if prefix > MAX_TAG_PREFIX_BYTES {
        return Err(RuntimeError::QueueOverloaded);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_wire::MSGID_TAG;

    fn parse(raw: &str) -> Message {
        Message::parse(raw.as_bytes()).expect("parses")
    }

    #[test]
    fn client_only_and_forged_tags_are_stripped_rather_than_forwarded() {
        // A forged msgid or an unknown client tag must never reach upstream: that is
        // how one client could impersonate another client's metadata. M004-A tightened
        // this from "forward a well-formed msgid" to deny by default.
        let forged = parse("@msgid=spoofed;+custom=1 :a!b@c PRIVMSG #room :hi\r\n");
        let (mediated, disposition) = mediate_client_tags(&forged, true);
        assert_eq!(disposition, TagDisposition::Stripped);
        assert!(
            !mediated.tags.contains_key(MSGID_TAG),
            "even a well-formed client msgid must not reach upstream"
        );
        assert!(
            !mediated.tags.contains_key(b"+custom".as_slice()),
            "an unknown client tag must be stripped"
        );
    }

    #[test]
    fn the_response_label_is_the_only_tag_a_client_may_supply() {
        // The label is the bouncer's own correlation mechanism, not client metadata:
        // the router translates it to an opaque upstream token and restores it only to
        // the client that sent it.
        let labeled = parse("@label=mine WHOIS alice\r\n");
        let (mediated, disposition) = mediate_client_tags(&labeled, true);
        assert_eq!(disposition, TagDisposition::Forwarded);
        assert_eq!(
            mediated.tags.get(LABEL_TAG.as_bytes()),
            Some(&Some(b"mine".to_vec()))
        );
    }

    #[test]
    fn a_client_only_allowlist_is_empty() {
        // Every `+` tag is denied, including ones a client might consider its own.
        for name in ["+typing", "+draft/reply", "+example/vendor"] {
            let raw = format!("@{name}=1 :a!b@c PRIVMSG #room :hi\r\n");
            let (mediated, _) = mediate_client_tags(&parse(&raw), true);
            assert!(mediated.tags.is_empty(), "{name} must be denied by default");
        }
    }

    #[test]
    fn a_malformed_client_tag_is_stripped_rather_than_trusted() {
        for raw in [
            "@time=notanumber :a!b@c PRIVMSG #room :hi\r\n",
            "@time=1700000000 :a!b@c PRIVMSG #room :hi\r\n",
            "@time=2019-01-04T14:33:26.123 :a!b@c PRIVMSG #room :hi\r\n",
            "@msgid= :a!b@c PRIVMSG #room :hi\r\n",
        ] {
            let message = parse(raw);
            let (mediated, _) = mediate_client_tags(&message, true);
            assert!(
                mediated.tags.is_empty(),
                "a malformed tag must not be forwarded: {raw}"
            );
        }
    }

    #[test]
    fn a_client_supplied_time_is_removed_even_though_it_parses() {
        // Parsing is not authorisation. A client that could set its own timestamp could
        // make its message look older or newer than it is to every other participant.
        let message = parse("@time=2023-11-14T22:13:20.000Z :a!b@c PRIVMSG #room :hi\r\n");
        let (mediated, disposition) = mediate_client_tags(&message, true);
        assert_eq!(disposition, TagDisposition::Stripped);
        assert!(
            mediated.tags.is_empty(),
            "a client must not be able to assert its own message time"
        );
    }

    #[test]
    fn tags_are_dropped_entirely_when_the_client_did_not_negotiate_them() {
        let message = parse("@time=2023-11-14T22:13:20.000Z :a!b@c PRIVMSG #room :hi\r\n");
        let (mediated, _) = mediate_client_tags(&message, false);
        assert!(mediated.tags.is_empty());
        let bare = strip_all_tags(&message);
        assert!(bare.tags.is_empty());
    }

    #[test]
    fn synthesis_never_overwrites_a_real_upstream_time() {
        let mut preserved = parse("@time=2020-09-13T12:26:40.123Z :a!b@c PRIVMSG #room :hi\r\n");
        synthesize_server_time(&mut preserved, i2pr_irc_core::WallTime(1_700_000_000));
        assert_eq!(
            preserved.server_time().map(|time| time.to_string()),
            Some("2020-09-13T12:26:40.123Z".to_owned()),
            "milliseconds survive synthesis being skipped"
        );

        let mut synthesized = parse(":a!b@c PRIVMSG #room :hi\r\n");
        synthesize_server_time(&mut synthesized, i2pr_irc_core::WallTime(1_700_000_000));
        assert_eq!(
            synthesized.server_time().map(|time| time.to_string()),
            Some("2023-11-14T22:13:20.000Z".to_owned()),
            "a synthesized timestamp is canonical text, never an integer epoch"
        );
    }

    #[test]
    fn synthesis_preserves_a_leap_second_instead_of_normalising_it() {
        let mut preserved = parse("@time=2012-06-30T23:59:60.419Z :a!b@c PRIVMSG #room :hi\r\n");
        synthesize_server_time(&mut preserved, i2pr_irc_core::WallTime(1_700_000_000));
        assert_eq!(
            preserved.server_time().map(|time| time.to_string()),
            Some("2012-06-30T23:59:60.419Z".to_owned()),
            "a leap second must not be rewritten to :59 or rolled forward"
        );
    }

    #[test]
    fn batch_identifiers_are_ephemeral_opaque_and_bounded() {
        let mut tracker = BatchTracker::default();
        let now = std::time::Instant::now();
        let first = tracker.open("chathistory", None, now).expect("opens");
        let second = tracker
            .open("netjoin", Some(&first.id), now)
            .expect("nests");
        assert_ne!(first.id, second.id);
        assert!(first.id.len() <= MAX_BATCH_ID_BYTES);
        assert_eq!(
            tracker.ancestry(&second.id),
            vec![first.id.clone(), second.id.clone()],
            "ancestry must be outermost first"
        );
    }

    #[test]
    fn batch_nesting_count_and_message_counts_are_bounded() {
        let mut tracker = BatchTracker::default();
        let now = std::time::Instant::now();
        let mut depth = 1u8;
        let mut current = tracker.open("chathistory", None, now).expect("opens");
        loop {
            match tracker.open("netjoin", Some(&current.id), now) {
                Ok(batch) => {
                    depth = depth.saturating_add(1);
                    if depth as usize > MAX_BATCH_DEPTH {
                        panic!("nesting must be refused at the ceiling");
                    }
                    current = batch;
                }
                Err(BatchError::TooDeep) => break,
                Err(other) => panic!("unexpected: {other:?}"),
            }
        }
        assert_eq!(depth as usize, MAX_BATCH_DEPTH);

        let mut tracker = BatchTracker::default();
        let batch = tracker.open("chathistory", None, now).expect("opens");
        for _ in 0..MAX_BATCH_MESSAGES {
            tracker.note_message(&batch.id).expect("counts");
        }
        assert_eq!(
            tracker.note_message(&batch.id),
            Err(BatchError::TooManyMessages),
            "an unbounded message count must force the batch closed, not track it"
        );
        assert!(!tracker.contains(&batch.id));
    }

    #[test]
    fn the_open_batch_ceiling_is_refused() {
        let mut tracker = BatchTracker::default();
        let now = std::time::Instant::now();
        for _ in 0..MAX_OPEN_BATCHES {
            tracker.open("chathistory", None, now).expect("opens");
        }
        assert_eq!(tracker.open_count(), MAX_OPEN_BATCHES);
        assert_eq!(
            tracker.open("chathistory", None, now),
            Err(BatchError::TooManyOpen)
        );
        assert_eq!(tracker.open_count(), MAX_OPEN_BATCHES);
    }

    #[test]
    fn an_unknown_batch_reference_is_refused() {
        let mut tracker = BatchTracker::default();
        let now = std::time::Instant::now();
        assert_eq!(
            tracker.open("netjoin", Some("nope"), now),
            Err(BatchError::UnknownReference)
        );
        assert_eq!(
            tracker.note_message("nope"),
            Err(BatchError::UnknownReference)
        );
        assert_eq!(tracker.close("nope"), Err(BatchError::UnknownReference));
        assert!(tracker.is_empty());
    }

    #[test]
    fn closing_a_nested_batch_does_not_close_its_parent() {
        let mut tracker = BatchTracker::default();
        let now = std::time::Instant::now();
        let parent = tracker.open("chathistory", None, now).expect("opens");
        let child = tracker
            .open("netjoin", Some(&parent.id), now)
            .expect("nests");
        tracker.close(&child.id).expect("closes");
        assert!(tracker.contains(&parent.id));
        tracker.close(&parent.id).expect("closes");
        assert!(tracker.is_empty());
    }
}
