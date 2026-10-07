//! Legacy automatic backlog delivery, with cursor advance gated on writer completion.
//!
//! After registration and the current-state projection, a session that wants a backlog
//! receives retained history it has not seen yet. Two rules make this safe:
//!
//! - **Bounded.** The delivery is capped in both event count and total bytes, and a
//!   session queue that refuses a frame ends delivery rather than growing without
//!   limit. A client that cannot keep up gets less history, never a stalled Network.
//! - **Acknowledged.** A cursor advances only after the writer reports that the bytes
//!   actually reached the socket. A crash between write and commit therefore
//!   duplicates on restart, which is preferable to a silent gap.
//!
//! Playback is history delivery to one already-attached client. It never writes
//! anything upstream, and it never implies that history is proof of delivery.
use crate::{
    journal::{BacklogCap, HistoryJournal},
    session::{SessionCapabilities, SessionHandle, queue_acknowledged},
};
use i2pr_irc_core::{BufferId, ClientId, SessionId};
use i2pr_irc_store::HistoryEvent;
use tokio::sync::oneshot;

/// Ceiling on how long playback waits for one writer acknowledgment.
///
/// The bound keeps a stalled client from pinning a playback task forever; on expiry
/// the cursor simply does not advance, so the next attempt re-delivers rather than
/// skipping.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What one playback attempt delivered.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlaybackOutcome {
    /// Events confirmed written to the socket.
    pub delivered: usize,
    /// Bytes confirmed written to the socket.
    pub bytes: usize,
    /// Newest event confirmed delivered; the cursor now sits here.
    pub cursor: Option<i2pr_irc_core::HistoryEventId>,
    /// True when more retained history remains beyond the cap.
    pub more_pending: bool,
    /// Delivery stopped because the session queue refused a frame.
    pub overflowed: bool,
    /// Delivery stopped because the session ended mid-playback.
    pub session_ended: bool,
}

/// Delivers the bounded backlog for one buffer and advances the cursor monotonically.
///
/// `journal` is taken by value because cursor advance is journal-owned state; the
/// caller re-borrows it afterwards for the next buffer.
pub async fn deliver_buffer(
    journal: &mut HistoryJournal,
    handle: &SessionHandle,
    client: ClientId,
    session: SessionId,
    buffer: BufferId,
    cap: BacklogCap,
) -> PlaybackOutcome {
    let events = match journal.backlog(client, buffer, cap).await {
        Ok(events) => events,
        // A storage failure degrades history only. The client simply receives none.
        Err(_) => return PlaybackOutcome::default(),
    };
    if events.is_empty() {
        return PlaybackOutcome::default();
    }
    let requested = events.len();
    deliver_events(
        journal, handle, client, session, buffer, events, requested, cap.events,
    )
    .await
}

/// Shared delivery loop, so both the per-buffer path and a multi-buffer batch use one
/// rule set.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deliver_events(
    journal: &mut HistoryJournal,
    handle: &SessionHandle,
    client: ClientId,
    _session: SessionId,
    buffer: BufferId,
    events: Vec<HistoryEvent>,
    requested: usize,
    cap_events: usize,
) -> PlaybackOutcome {
    // More history remains whenever the store returned exactly a full page, which is
    // the only signal available without an unbounded count query.
    let mut outcome = PlaybackOutcome {
        more_pending: requested >= cap_events.min(i2pr_irc_store::MAX_HISTORY_QUERY_EVENTS),
        ..PlaybackOutcome::default()
    };
    for event in events {
        let line = match replay_frame(&event) {
            Ok(line) => line,
            // A retained payload that no longer fits the wire limit is skipped rather
            // than truncated into a different message.
            Err(_) => continue,
        };
        let ack = match queue_acknowledged(handle, &line) {
            Ok(ack) => ack,
            // A refused queue ends this delivery. The cursor stays where it is, so the
            // next attempt re-delivers from a known point.
            Err(_) => {
                outcome.overflowed = true;
                break;
            }
        };
        match await_ack(ack).await {
            AckOutcome::Written => {
                outcome.delivered += 1;
                outcome.bytes += event.payload.len();
                // The cursor advances only now, after the bytes are on the socket.
                // A failed commit leaves the cursor where it was, so the event is
                // delivered again rather than being skipped.
                if let Ok(position) = journal.advance_cursor(client, buffer, event.event).await {
                    outcome.cursor = Some(position);
                }
            }
            AckOutcome::TimedOut => {
                outcome.session_ended = true;
                break;
            }
            AckOutcome::Failed => {
                outcome.session_ended = true;
                break;
            }
        }
    }
    outcome
}

/// Re-renders one retained event as a wire line.
///
/// A retained payload is protocol content without its terminator, so the frame is the
/// payload plus CRLF. Anything longer than the wire ceiling is refused rather than
/// truncated, because a truncated replay would be a different message than the one
/// that was retained.
fn replay_frame(event: &HistoryEvent) -> Result<String, crate::RuntimeError> {
    let text = std::str::from_utf8(&event.payload).map_err(|_| crate::RuntimeError::Protocol)?;
    // A retained payload is protocol content with no terminator at all. Stripping
    // trailing newlines here would *repair* a payload that should never exist, so a
    // stored payload containing any CR or LF is refused instead of cleaned.
    if text.is_empty() || text.contains(['\r', '\n']) {
        return Err(crate::RuntimeError::Protocol);
    }
    let line = format!("{text}\r\n");
    if line.len() > i2pr_irc_wire::MAX_TAGGED_LINE_BYTES {
        return Err(crate::RuntimeError::QueueOverloaded);
    }
    Ok(line)
}

enum AckOutcome {
    Written,
    TimedOut,
    Failed,
}

async fn await_ack(ack: oneshot::Receiver<std::io::Result<()>>) -> AckOutcome {
    match tokio::time::timeout(ACK_TIMEOUT, ack).await {
        Err(_) => AckOutcome::TimedOut,
        // A dropped reply means the writer task ended without confirming. Treating that
        // as undelivered is the safe direction: it re-delivers instead of skipping.
        Ok(Err(_)) => AckOutcome::Failed,
        Ok(Ok(Err(_))) => AckOutcome::Failed,
        Ok(Ok(Ok(()))) => AckOutcome::Written,
    }
}

/// True when this session should receive an automatic backlog at all.
pub fn wants_backlog(capabilities: SessionCapabilities) -> bool {
    capabilities.wants_backlog()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_bytes(payload: &[u8]) -> HistoryEvent {
        HistoryEvent {
            event: i2pr_irc_core::HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(0),
            server_time: None,
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: payload.to_vec(),
        }
    }

    fn event(payload: &str) -> HistoryEvent {
        HistoryEvent {
            event: i2pr_irc_core::HistoryEventId(1),
            network: i2pr_irc_core::NetworkId(1),
            buffer: BufferId(1),
            received_at: i2pr_irc_core::WallTime(0),
            server_time: None,
            msgid: None,
            direction: i2pr_irc_store::EventDirection::Inbound,
            event_class: "PRIVMSG".into(),
            payload: payload.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_retained_event_replays_as_one_framed_line() {
        let line = replay_frame(&event(":a!b@c PRIVMSG #room :hi")).expect("replays");
        assert_eq!(line, ":a!b@c PRIVMSG #room :hi\r\n");
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn a_payload_carrying_its_own_newline_is_never_split_into_two_frames() {
        // This is exactly what the store's framing rejection prevents upstream; the
        // replay side refuses it too rather than emitting a second line.
        assert!(replay_frame(&event("PRIVMSG #room :one\nPRIVMSG #room :two")).is_err());
        assert!(replay_frame(&event("PRIVMSG #room :\r")).is_err());
    }

    #[test]
    fn an_unencodable_or_oversized_payload_is_skipped_rather_than_truncated() {
        // A payload that is not valid UTF-8 cannot be re-framed safely, so it is
        // refused rather than written through as lossy bytes.
        assert!(replay_frame(&event_bytes(&[0xff, 0xfe, b'x'])).is_err());
        let oversized = vec![b'x'; i2pr_irc_wire::MAX_TAGGED_LINE_BYTES + 1];
        assert!(replay_frame(&event_bytes(&oversized)).is_err());
        assert!(replay_frame(&event_bytes(b"")).is_err());
    }

    #[test]
    fn the_capability_hook_can_suppress_legacy_backlog() {
        let legacy = SessionCapabilities::default();
        assert!(wants_backlog(legacy));
        let explicit = SessionCapabilities {
            message_tags: false,
            legacy_backlog: true,
            explicit_history: true,
            read_markers: false,
            pre_away: false,
            bouncer_networks: false,
            bouncer_networks_notify: false,
            search: false,
            server_time: false,
            standard_replies: false,
            cap_notify: false,
            no_implicit_names: false,
            echo_message: false,
        };
        assert!(
            !wants_backlog(explicit),
            "a client that fetches history itself must not also receive an automatic backlog"
        );
        // The two drafts are independent: managing history says nothing about whether
        // the bouncer owes this client a read marker, so suppressing the backlog must
        // not be inferred from read-marker negotiation either.
        let markers_only = SessionCapabilities {
            message_tags: false,
            legacy_backlog: true,
            explicit_history: false,
            read_markers: true,
            pre_away: false,
            bouncer_networks: false,
            bouncer_networks_notify: false,
            search: false,
            server_time: false,
            standard_replies: false,
            cap_notify: false,
            no_implicit_names: false,
            echo_message: false,
        };
        assert!(
            wants_backlog(markers_only),
            "negotiating only read-marker must not suppress the legacy backlog"
        );
    }
}
