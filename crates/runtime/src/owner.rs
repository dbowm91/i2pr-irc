//! One Network owner: one upstream generation plus its attached sessions.
//!
//! The owner is the sole mutable authority for one Network. It keeps generation-owned
//! [`NetworkState`] local (never shared), routes typed session intents, fans one
//! normalized upstream event out to every attached session, and persists durable
//! DesiredState *before* any corresponding upstream bytes are written.
//!
//! Two properties drive the shape of this loop:
//!
//! - A client that is slow or overflowing affects only itself. Its queue is bounded
//!   and nothing waits on it, so its pressure never reaches upstream processing or
//!   another client; the frames it loses are counted rather than buffered.
//! - A disconnect after an outbound command leaves delivery ambiguous. Nothing user
//!   typed is ever retained for replay into a later generation.
use crate::{
    CONNECT_TIMEOUT, CONTROL_QUEUE_CAPACITY, IntentClass, NORMAL_QUEUE_CAPACITY, OutboundIntent,
    REGISTRATION_TIMEOUT, RuntimeError,
    capability::UpstreamCapabilities,
    catalog::SupervisorCommand,
    downstream::DownstreamDisposition,
    journal::IngestOutcome,
    playback::PlaybackOutcome,
    presence::{AwayOrigin, PresencePolicy, PresenceState, ReclaimAttempt, SessionPresence},
    projection,
    reconnect::{ReconnectScheduler, jitter_entropy},
    resource::{NetworkGauges, ResourceLedger},
    routing::{
        BatchRole, Incoming, LABEL_TAG, RequestClass, ResponseRouter, RouteOutcome, RouteRefusal,
        Routed, RoutingRequest,
    },
    session::{
        SESSION_EVENT_QUEUE_CAPACITY, SessionEvent, SessionHandle, SessionIntent, SessionTask,
        TagSurface,
    },
    state::{LineOutcome, NetworkState},
};
use i2pr_irc_core::{
    ByteStream, ClientId, ConnectionGeneration, I2pStreamProvider, NetworkId, SessionId,
};
use i2pr_irc_store::{BufferId, BufferKind, CommitState, NetworkRecord, StoreError, StoreHandle};
use i2pr_irc_wire::{LineDecoder, Message, TagDirection};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use zeroize::Zeroizing;

/// Ceiling on simultaneous sessions for one Network. A full map refuses new
/// attachments explicitly instead of growing the owner's state without limit.
pub const MAX_SESSIONS_PER_NETWORK: usize = 64;
/// Ceiling on buffered upstream intents awaiting persistence confirmation.
pub const PENDING_INTENT_CAPACITY: usize = 64;
/// Ceiling on lines queued for durable history ingestion.
///
/// History work is bounded and strictly best effort: when this is full, the event is
/// dropped and counted rather than buffered, because an unbounded retry buffer would
/// trade memory pressure for history that arrives too late to matter.
pub const INGEST_QUEUE_CAPACITY: usize = 256;
/// How many queued ingestion items one loop turn may drain, so history work cannot
/// monopolize the owner and delay PING/PONG.
pub const INGEST_BATCH_PER_TURN: usize = 16;
/// How many upstream lines one read may apply before yielding to the scheduler.
///
/// A single read can carry hundreds of lines. Applying all of them before returning to
/// the scheduler starves every session writer task, and under ordered delivery that is
/// not cosmetic: healthy clients would exhaust their own bounded queues without ever
/// being read and would be detached for pressure they did not cause. Yielding keeps a
/// burst arriving as several slices, so unrelated tasks always get to run.
///
/// The bound sits well under the per-session normal queue on purpose: an attachment
/// that is keeping up must be able to drain within one window, so a burst can never
/// desynchronize a client that never stopped reading.
///
/// This is a cooperative handoff, not a spin: the turn returns control and waits for
/// the executor, so an idle owner still sleeps rather than burning a core.
pub const UPSTREAM_LINES_PER_TURN: usize = 32;
/// Deferred desired membership is relieved on its own short timer.
///
/// Reconciliation cannot wait for traffic. A committed JOIN or PART may be the last
/// thing that ever happens on this Network, in which case no upstream read, session
/// event or keepalive tick ever arrives and intent the bouncer already promised to
/// store would sit unwritten indefinitely. The interval is a bounded timer, not a
/// spin, and it only does work when the reconciliation set is non-empty.
pub const DESIRED_RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
/// Ceiling on buffers whose backlog one session may receive in one pass, so a client
/// attached to many channels still receives a bounded total amount of history.
pub const MAX_BACKLOG_BUFFERS: usize = 32;
/// Ceiling on deferred desired-membership intents awaiting upstream queue capacity.
///
/// This matches the observed-membership ceiling on purpose. The set can only ever hold
/// channels the Operator already asked the bouncer to track, so a distinct ceiling
/// would either be larger than the state it reconciles or needlessly force a restart.
pub const MAX_DESIRED_RECONCILE: usize = crate::state::MAX_CHANNELS;

/// Answers one `SEARCH` request from the owning generation.
///
/// Like every other history answer, the reply goes only to the session that asked and
/// never crosses the upstream connection: retained history is the bouncer's own state,
/// and a search is a lookup a client makes against itself.
///
/// The Network scope comes from the journal this owner already owns rather than from the
/// request. That is the only thing standing between a typo and a cross-Network
/// disclosure, so it is chosen where the ownership is.
async fn answer_history_search(
    journal: &crate::journal::HistoryJournal,
    handle: &SessionHandle,
    wire: &[u8],
    buffers: &BTreeMap<String, BufferId>,
    batch: &mut u64,
) {
    let Some(message) = i2pr_irc_wire::Message::parse(wire).ok() else {
        return;
    };
    let request = match crate::search::parse_search(&message) {
        crate::search::ParsedSearch::Accepted(request) => request,
        crate::search::ParsedSearch::Refused(refusal) => {
            return refuse_search(handle, refusal.reason(), batch);
        }
    };

    // `in=` names this Network's buffers. An unresolvable name is refused rather than
    // searched as an empty scope, because "no matches" for a channel that does not exist
    // here is indistinguishable from a true statement about the journal.
    let scope = match crate::search::resolve_buffer(&request.channels, buffers) {
        Ok(scope) => scope,
        Err(refusal) => return refuse_search(handle, refusal.reason(), batch),
    };

    let query = crate::search::compile(&request, journal.network(), scope);
    let hits = match journal.store_search(&query).await {
        Ok(hits) => hits,
        Err(_) => {
            return refuse_search(
                handle,
                crate::search::SearchRefusal::Unavailable.reason(),
                batch,
            );
        }
    };

    for line in crate::search::render_batch(&hits, *batch) {
        if handle.queue_normal(&frame(&line)).is_err() {
            return;
        }
    }
    *batch = next_batch_id(batch);
}

/// Refuses one search request: an explicit reason, then a complete empty batch.
///
/// Both halves are load-bearing. The reason is a fixed string, never the offending
/// selector or its value, so a refusal cannot become a channel for echoing client text
/// back into a frame. The empty batch is there because a client that asked and heard
/// nothing has to guess whether to wait, and "finished, and there was nothing" is the
/// only answer that ends the question.
fn refuse_search(handle: &SessionHandle, reason: &'static str, batch: &mut u64) {
    let failure = crate::search::render_refusal_for(&handle.capabilities(), reason);
    let empty = crate::search::render_batch(&[], *batch);
    if handle.queue_normal(&frame(&failure)).is_err() {
        return;
    }
    for line in empty {
        if handle.queue_normal(&frame(&line)).is_err() {
            return;
        }
    }
    *batch = next_batch_id(batch);
}

/// The next generation-local search batch identifier.
///
/// Wrapping back to 1 rather than 0 or saturating: zero is not a batch identifier, and
/// reusing one would let a client correlate two unrelated result sets as if they were
/// the same page.
fn next_batch_id(batch: &mut u64) -> u64 {
    *batch = batch.wrapping_add(1).max(1);
    *batch
}

/// Answers one `CHATHISTORY` request from the owning generation.
///
/// The reply goes only to the session that asked. History is the bouncer's own state,
/// so a request never crosses the upstream connection and never reaches another
/// client.
async fn answer_history_query(
    journal: &mut crate::journal::HistoryJournal,
    handle: &SessionHandle,
    wire: &[u8],
    buffers: &BTreeMap<String, BufferId>,
    batches: &mut crate::ircv3::BatchTracker,
) {
    let Some(message) = i2pr_irc_wire::Message::parse(wire).ok() else {
        return;
    };
    // The draft's `FAIL` form is `FAIL CHATHISTORY <code> <subcommand> <params>`, so the
    // field echoed after the code is the *subcommand* the client typed -- `BEFORE`, not
    // `CHATHISTORY`. Echoing the command name instead told the client its own command
    // back twice and never told it which of the six subcommands failed.
    let subcommand = message
        .params
        .first()
        .map(|param| String::from_utf8_lossy(param).to_ascii_uppercase())
        .unwrap_or_default();
    let first_param = message
        .params
        .get(1)
        .and_then(|param| String::from_utf8(param.to_vec()).ok());
    let parsed = crate::chathistory::parse_chathistory(&message);
    let request = match parsed {
        crate::chathistory::ParsedRequest::Accepted(request) => request,
        crate::chathistory::ParsedRequest::Refused(refusal) => {
            // A refusal is still a reply: the client needs to learn why nothing
            // arrived, or it will simply wait.
            let _ = handle.queue_normal(&frame(crate::chathistory::render_refusal_for(
                &handle.capabilities(),
                refusal,
                &subcommand,
                first_param.as_deref(),
            )));
            return;
        }
    };
    let command = subcommand;

    // `TARGETS` names buffers rather than messages, so it has no single buffer to
    // page. It is answered with its own batch type.
    if let crate::chathistory::HistoryQueryRequest::Targets {
        older,
        newer,
        limit,
    } = &request
    {
        let _ = answer_targets(journal, handle, older, newer, *limit, batches).await;
        return;
    }

    let Some(target) = message
        .params
        .get(1)
        .and_then(|p| String::from_utf8(p.to_vec()).ok())
    else {
        return;
    };
    let Some(buffer) = buffers.get(&target) else {
        let _ = handle.queue_normal(&frame(crate::chathistory::render_refusal_for(
            &handle.capabilities(),
            crate::chathistory::HistoryRefusal::NoSuchBuffer,
            &command,
            Some(&target),
        )));
        return;
    };
    let buffer = *buffer;

    let wants_time = handle.capabilities().negotiated_server_time();
    let reply = match crate::chathistory::execute_for(journal, buffer, &request, wants_time).await {
        Ok(reply) => reply,
        Err(refusal) => {
            let _ = handle.queue_normal(&frame(crate::chathistory::render_refusal_for(
                &handle.capabilities(),
                refusal,
                &command,
                Some(&target),
            )));
            return;
        }
    };
    // A client that negotiated `batch` gets its history inside one; `wrap_in_batch`
    // closes the batch even when there is nothing to send.
    match crate::chathistory::wrap_in_batch(&reply, &target, batches) {
        Ok(lines) => {
            for line in lines {
                if handle.queue_normal(&frame(&line)).is_err() {
                    return;
                }
            }
        }
        Err(refusal) => {
            let _ = handle.queue_normal(&frame(crate::chathistory::render_refusal_for(
                &handle.capabilities(),
                refusal,
                &command,
                Some(&target),
            )));
        }
    }
}

/// Answers one `TARGETS` request: buffers with retained history in a time window.
async fn answer_targets(
    journal: &crate::journal::HistoryJournal,
    handle: &SessionHandle,
    older: &i2pr_irc_wire::IrcTimestamp,
    newer: &i2pr_irc_wire::IrcTimestamp,
    limit: usize,
    batches: &mut crate::ircv3::BatchTracker,
) -> bool {
    let batch = match batches.open(
        crate::chathistory::TARGETS_BATCH_TYPE,
        None,
        std::time::Instant::now(),
    ) {
        Ok(batch) => batch,
        Err(_) => return false,
    };
    // The list is bounded and the window is inclusive of both endpoints, matching the
    // draft's "newer than / older than" wording.
    let targets = journal
        .recent_targets(*older, *newer, limit)
        .await
        .unwrap_or_default();
    let _ = handle.queue_normal(&frame(format!(
        ":bouncer BATCH +{} {} *\r\n",
        batch.id,
        crate::chathistory::TARGETS_BATCH_TYPE
    )));
    for target in targets.into_iter().take(limit) {
        // Each target names the timestamp of the newest message in it, so a client
        // can resume from exactly that point.
        let _ = handle.queue_normal(&frame(format!(
            ":bouncer BATCH +{} chathistory {} timestamp={}\r\n",
            batch.id, target.target, target.newest
        )));
    }
    let _ = batches.close(&batch.id);
    let _ = handle.queue_normal(&frame(format!(":bouncer BATCH -{}\r\n", batch.id)));
    true
}

/// Answers one `MARKREAD` request: a client get or a client set.
///
/// Read state belongs to the Operator rather than to one attachment, so a set that
/// actually advances the marker is propagated to every *other* attached session that
/// negotiated `draft/read-marker`. Propagation stays inside this Network: markers are
/// the bouncer's own state and are never written upstream.
async fn answer_marker_update(
    journal: &mut crate::journal::HistoryJournal,
    sessions: &BTreeMap<SessionId, SessionTask>,
    session: SessionId,
    wire: &[u8],
    buffers: &BTreeMap<String, BufferId>,
) {
    let Some(message) = i2pr_irc_wire::Message::parse(wire).ok() else {
        return;
    };
    let Some(handle) = sessions.get(&session).map(SessionTask::handle) else {
        return;
    };
    let parsed = crate::chathistory::parse_markread(&message);
    let target = message
        .params
        .first()
        .and_then(|p| String::from_utf8(p.to_vec()).ok())
        .unwrap_or_default();
    match parsed {
        Ok(crate::chathistory::ParsedMarker::Get) => {
            let Some(buffer) = buffers.get(&target).copied() else {
                let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_reply(
                    &target, None,
                )));
                return;
            };
            let stored = journal.read_marker(buffer).await.ok().flatten();
            // The reply carries the stored marker, or `*` when none is known. The
            // durable marker is an event id, so it is translated back into the
            // protocol timestamp the client set.
            let stamp = match stored {
                Some(event) => journal.event_timestamp(buffer, event).await.ok().flatten(),
                None => None,
            };
            let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_reply(
                &target, stamp,
            )));
        }
        Ok(crate::chathistory::ParsedMarker::Set { target, timestamp }) => {
            let Some(buffer) = buffers.get(&target).copied() else {
                let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_failure(
                    crate::chathistory::MarkerRefusal::NoSuchBuffer,
                    Some(&target),
                )));
                return;
            };
            // Read before writing so an advance can be distinguished from a retained
            // marker. A set that changes nothing must not look like an update to the
            // Operator's other sessions.
            let previous = journal.read_marker(buffer).await.ok().flatten();
            // Resolving the client timestamp to a durable position keeps the marker
            // monotonic: a client cannot name an arbitrary instant to skip ahead.
            let reference = crate::chathistory::MessageReference::Timestamp(timestamp);
            match crate::chathistory::resolve(journal, buffer, &reference).await {
                Ok(crate::chathistory::HistoryPosition::Event(event)) => {
                    match journal.set_read_marker(buffer, event).await {
                        Ok(applied) => {
                            // The draft requires the server to answer with the value
                            // it stored, which may be older than requested.
                            let stored = journal
                                .event_timestamp(buffer, applied)
                                .await
                                .ok()
                                .flatten();
                            let _ = handle.queue_normal(&frame(
                                crate::chathistory::render_marker_reply(&target, stored),
                            ));
                            if previous != Some(applied) {
                                let line =
                                    frame(crate::chathistory::render_marker_reply(&target, stored));
                                broadcast_marker(sessions, session, &line);
                            }
                        }
                        Err(_) => {
                            let _ = handle.queue_normal(&frame(
                                crate::chathistory::render_marker_failure(
                                    crate::chathistory::MarkerRefusal::Internal,
                                    Some(&target),
                                ),
                            ));
                        }
                    }
                }
                // A read mark earlier than every retained message is the truthful state
                // of a client that has read nothing in this buffer. It is answered with
                // the marker that is already stored rather than being written as a
                // position, because writing one would invent a point in history that does
                // not exist and then broadcast it to every other session.
                Ok(crate::chathistory::HistoryPosition::BeforeStart) => {
                    let stored = match previous {
                        Some(marker) => {
                            journal.event_timestamp(buffer, marker).await.ok().flatten()
                        }
                        None => None,
                    };
                    let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_reply(
                        &target, stored,
                    )));
                }
                Err(_) => {
                    let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_failure(
                        crate::chathistory::MarkerRefusal::InvalidTimestamp,
                        Some(&target),
                    )));
                }
            }
        }
        Err(refusal) => {
            let _ = handle.queue_normal(&frame(crate::chathistory::render_marker_failure(
                refusal,
                Some(&target),
            )));
        }
    }
}

/// Propagates one marker update to the Operator's other read-marker sessions.
///
/// A session that did not negotiate the draft is skipped rather than sent the frame:
/// an unnegotiated command is a protocol violation for a strict client, and the
/// broadcast is an optimization the requesting session already has directly.
fn broadcast_marker(sessions: &BTreeMap<SessionId, SessionTask>, origin: SessionId, line: &str) {
    for (id, task) in sessions {
        if *id == origin {
            continue;
        }
        let handle = task.handle();
        if handle.capabilities().manages_read_markers() {
            let _ = handle.queue_normal(line);
        }
    }
}

/// Frames an already-rendered line for the session queue.
///
/// Accepts either raw bytes or an already-formatted string; both paths are ASCII or
/// lossy-converted, and the session writer is the component that decides whether a
/// frame is well formed enough to send.
pub(crate) fn frame(line: impl AsRef<[u8]>) -> String {
    String::from_utf8_lossy(line.as_ref()).into_owned()
}

async fn deliver_legacy_backlog(
    journal: &mut crate::journal::HistoryJournal,
    handle: &SessionHandle,
    client: ClientId,
    session: SessionId,
    buffers: &BTreeMap<String, BufferId>,
    snapshot: &watch::Sender<NetworkSnapshot>,
    state: &NetworkState,
) -> PlaybackOutcome {
    let cap = crate::journal::BacklogCap::DEFAULT;
    let mut total = PlaybackOutcome::default();
    // Buffer tables are keyed by casemapped channel identity and include detached
    // channels, because a detached channel keeps collecting history exactly as an
    // attached one does. Replaying one here would hand a client messages from a channel
    // it has just been told the bouncer no longer shows, which is both a privacy failure
    // and a stream the client cannot make sense of. Reattaching is what makes the backlog
    // available again, through the client's own cursor.
    for (channel, buffer) in buffers
        .iter()
        .filter(|(channel, _)| !state.is_detached_key(channel))
        .map(|(channel, buffer)| (channel.clone(), *buffer))
        .take(MAX_BACKLOG_BUFFERS)
    {
        let _ = &channel;
        if total.delivered >= cap.events || total.bytes >= cap.bytes {
            total.more_pending = true;
            break;
        }
        let remaining = crate::journal::BacklogCap::new(
            cap.events.saturating_sub(total.delivered),
            cap.bytes.saturating_sub(total.bytes),
        );
        let outcome =
            crate::playback::deliver_buffer(journal, handle, client, session, buffer, remaining)
                .await;
        total.delivered += outcome.delivered;
        total.bytes += outcome.bytes;
        total.overflowed |= outcome.overflowed;
        total.session_ended |= outcome.session_ended;
        total.more_pending |= outcome.more_pending;
        if outcome.overflowed || outcome.session_ended {
            break;
        }
    }
    snapshot.send_modify(|state| {
        state.backlog_delivered = state
            .backlog_delivered
            .saturating_add(total.delivered as u64);
        state.backlog_truncated = state.backlog_truncated || total.more_pending;
        state.last_error = if total.overflowed {
            Some("backlog-overflow")
        } else {
            state.last_error
        };
    });
    total
}

/// The channel this line authoritatively confirmed membership of, if any.
///
/// Only a self JOIN from the server confirms membership. A `PART` removes it, and any
/// other JOIN is someone else's membership and proves nothing about this bouncer.
fn confirmed_self_channel(state: &NetworkState, message: &Message) -> Option<String> {
    let command = &message.command;
    let target = std::str::from_utf8(message.params.first()?).ok()?;
    if !is_channel_target(target) {
        return None;
    }
    let prefix = message.prefix.as_ref()?;
    // A prefix is `nick!user@host`; the nick is the part before the first `!`.
    let nick = prefix.split(|byte| *byte == b'!').next()?;
    if !state.same_nick(std::str::from_utf8(nick).ok()?, &state.nick) {
        return None;
    }
    if command.eq_ignore_ascii_case(b"PART") {
        return None;
    }
    if !command.eq_ignore_ascii_case(b"JOIN") {
        return None;
    }
    state
        .joined_channels()
        .into_iter()
        .find(|channel| state.same_nick(channel, target))
}

/// Casemapped lookup key for a wire target.
///
/// Crate-visible because target resolution is not this module's invention: the search
/// adapter has to fold a client's `in=` selector exactly the way the owner folded it when
/// it created the buffer, or a channel could exist under two spellings.
pub(crate) fn casemapped(target: &str) -> String {
    i2pr_irc_core::Casemapping::Rfc1459
        .fold(target.as_bytes())
        .into_iter()
        .map(char::from)
        .collect()
}

/// One upstream line queued for durable history ingestion.
struct IngestItem {
    buffer: BufferId,
    message: Message,
}

/// Which attached sessions lost a live upstream frame during one line.
struct FanoutReport {
    /// Sessions whose own normal queue refused this frame.
    ///
    /// The caller detaches exactly these. There is deliberately no "drop the chat and
    /// keep the MODE" refinement: an IRC line's importance is not knowable from its
    /// command alone -- a MODE, a NICK, a JOIN or a BATCH boundary can all leave the
    /// client holding state that later frames depend on -- so ordered delivery is the
    /// contract, and breaking it ends that one attachment.
    desynchronized: Vec<SessionId>,
}

/// Direction of a durable membership intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesiredIntent {
    Join,
    Part,
}

/// Desired membership this generation could not hand to the upstream queue.
///
/// A JOIN or PART is committed to SQLite *before* any wire bytes exist, so a refused
/// enqueue leaves durable operator intent and live state divergent. The bouncer owes
/// the Operator a convergent live state, but it may not build an unbounded retry queue
/// and it may never carry client chat along with the retry. So this set holds only a
/// channel name and the direction of intent, is capped by the same ceiling as observed
/// membership, and is drained as soon as the upstream queue has capacity again.
///
/// It is generation-local. A new generation rebuilds desired membership from storage
/// anyway, so nothing here needs to survive a reconnect.
#[derive(Default)]
struct DesiredReconcile {
    pending: BTreeMap<String, DesiredIntent>,
}

impl DesiredReconcile {
    /// Records one deferred intent, replacing any earlier intent for the same channel.
    ///
    /// The latest intent wins because the database already committed that one: a
    /// deferred JOIN for a channel whose PART was later committed would otherwise
    /// re-join it, contradicting the Operator.
    ///
    /// Returns false when the set is already at its ceiling. The durable intent is
    /// still correct -- it lives in SQLite -- so the caller reports that this
    /// generation cannot converge in place rather than dropping the operator's intent.
    fn defer(&mut self, channel: String, intent: DesiredIntent) -> bool {
        if self.pending.len() >= MAX_DESIRED_RECONCILE && !self.pending.contains_key(&channel) {
            return false;
        }
        self.pending.insert(channel, intent);
        true
    }

    /// Hands deferred intents to the upstream queue while it has room.
    ///
    /// Stops at the first refusal so the remaining entries keep their relative order.
    /// Only channel names and directions are ever replayed: an ordinary client command
    /// that was refused is never retried, because a later generation could not know
    /// whether writing it again would duplicate it.
    fn drain(
        &mut self,
        sender: &mpsc::Sender<OutboundIntent>,
        generation: ConnectionGeneration,
    ) -> u64 {
        let mut drained: u64 = 0;
        let mut blocked = Vec::new();
        for (channel, intent) in std::mem::take(&mut self.pending) {
            let line = match intent {
                DesiredIntent::Join => format!("JOIN {channel}\r\n"),
                DesiredIntent::Part => format!("PART {channel}\r\n"),
            };
            if queue_upstream(sender, generation, &line).is_ok() {
                drained = drained.saturating_add(1);
            } else {
                blocked.push((channel, intent));
            }
        }
        self.pending = blocked.into_iter().collect();
        drained
    }
}

/// Bounded, non-secret diagnostic projection of one Network owner.
#[derive(Clone, Debug, Default)]
pub struct NetworkSnapshot {
    pub network: Option<NetworkId>,
    pub phase: Option<Phase>,
    pub generation: Option<ConnectionGeneration>,
    pub nick: Option<String>,
    /// What this bouncer currently advertises to an attached client.
    ///
    /// Derived from the upstream negotiation, so it is a function of what the server
    /// agreed rather than a static list. Published here because it is also the answer a
    /// diagnostic reader needs: "why did my client not get `echo-message`" is answered by
    /// this field, not by a guess.
    pub advertisement: Vec<String>,
    /// Observed membership only.
    pub channels: Vec<String>,
    /// Observed membership the bouncer holds but does not present downstream.
    ///
    /// This is a policy, not a fault: these channels are joined and still collect
    /// history. It is reported separately from `channels` so an operator reading
    /// diagnostics can tell "the bouncer is not in this room" apart from "the Operator
    /// asked for this room to be hidden".
    pub detached_channels: Vec<String>,
    pub reconnect_attempt: u32,
    /// How long this owner will wait before its next reconnect attempt.
    ///
    /// `None` means nothing is scheduled, which is different from a zero delay: an
    /// Operator watching a flapping Network needs to tell "waiting four seconds" from
    /// "already retrying", and only the owner knows which, because the backoff schedule
    /// lives here rather than in the process-wide connect scheduler.
    pub next_retry_delay: Option<std::time::Duration>,
    /// The away state upstream is currently being told, if any.
    ///
    /// This is the Operator's presence as the rest of the network sees it, which makes it
    /// a diagnostic worth having: a bouncer that is away while the Operator is at the
    /// keyboard and a bouncer that is present while they are not are both failures that
    /// are otherwise invisible from outside.
    pub away: Option<String>,
    /// Why the away state above is being held, when one is.
    ///
    /// Published beside the text rather than derived from it, because the text is free form:
    /// classifying it at read time would mean pattern-matching a value the Operator may
    /// change at will. Diagnostics reports a *class*; the text itself is not exported.
    pub away_origin: Option<AwayOrigin>,
    /// Sessions currently counted as the Operator being present.
    ///
    /// Reported next to `detached_channels` for the same reason: a reader must be able to
    /// tell "no active client" from "several active clients that all declared themselves
    /// passive", which are very different situations behind the same socket count.
    pub active_sessions: usize,
    /// Durable detach and reattach decisions this owner has applied.
    pub channels_detached: u64,
    pub channels_reattached: u64,
    /// Sessions currently attached.
    pub attached_sessions: usize,
    pub sessions_accepted: u64,
    pub sessions_ended: u64,
    pub last_session_disposition: Option<&'static str>,
    pub upstream_normal_queue_depth: usize,
    pub upstream_control_queue_depth: usize,
    pub upstream_events_seen: u64,
    pub pending_joins: Vec<String>,
    pub rejected_joins: Vec<(String, &'static str)>,
    pub last_error: Option<&'static str>,
    /// Bounded history counters. Never carries a payload.
    pub history_recorded: u64,
    pub history_skipped: u64,
    /// Lines dropped because the ingestion queue was full.
    pub history_dropped: u64,
    /// Live upstream frames refused by one attached client's own bounded normal queue.
    ///
    /// Each refusal means that client lost a frame and was therefore desynchronized, so
    /// this counter is always paired with `fanout_detached`. It is counted per refused
    /// frame; the detach is counted per session.
    pub fanout_dropped: u64,
    /// Sessions detached because they could not keep up with live upstream.
    ///
    /// A live IRC stream is ordered, so a skipped frame is unrecoverable: the client
    /// cannot be left attached claiming to be synchronized with state it never saw.
    pub fanout_detached: u64,
    /// Client commands the bounded upstream queue refused before admission.
    ///
    /// A refused command was definitely not accepted for delivery and is never
    /// retried, so this counter is the only record that it happened.
    pub upstream_rejected: u64,
    /// Desired channel intents deferred because their immediate upstream enqueue was
    /// refused. Bounded by the observed-membership ceiling.
    pub desired_reconcile_pending: usize,
    /// Desired channel intents handed to the upstream queue by reconciliation.
    pub desired_reconcile_drained: u64,
    /// Times the bounded reconciliation set could not hold another deferred intent.
    ///
    /// Non-zero means this generation cannot converge in place, and the Network was
    /// deliberately restarted instead so durable DesiredState is rebuilt from storage.
    pub desired_reconcile_overflowed: u64,
    /// Events confirmed delivered to clients by automatic backlog.
    /// Negotiated upstream capabilities, as a bounded fingerprint. Never a payload.
    pub upstream_capabilities: String,
    /// Routes currently open for this generation. Never durable.
    pub response_routes: usize,
    /// Batch references currently attributed to one of those routes.
    ///
    /// Tracked only so a labeled reply's continuation frames can reach the session that
    /// asked the question. A batch whose owning route is gone is pruned rather than kept,
    /// so this must fall back to zero when routing settles.
    pub open_batches: usize,
    /// Correlated replies dropped because no live route claimed them.
    ///
    /// A reply that carries a response label but matches no route is orphaned: its
    /// request is stale or was never the bouncer's. It is counted here precisely
    /// because it is never shown to any client -- delivering it would hand one client's
    /// discarded reply to another.
    pub orphaned_replies_dropped: u64,
    /// Inbound CTCP frames suppressed by the privacy policy.
    ///
    /// A metadata probe, a DCC request or an unknown command reaching an attached
    /// client would either prompt it to auto-reveal its client software or offer it a
    /// direct-connect request, so those frames never leave this process.
    pub ctcp_suppressed: u64,
    /// Client frames withheld by the outbound privacy policy.
    ///
    /// A blocked frame is never transmitted upstream and never fanned out, so this is
    /// the only record that it happened.
    pub client_frames_blocked: u64,
    pub backlog_delivered: u64,
    /// True when more retained history exists beyond what the cap delivered.
    pub backlog_truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Idle,
    Connecting,
    Registering,
    Online,
    Backoff,
    Stopping,
    Stopped,
}

impl Phase {
    /// A fixed, non-secret name for diagnostics.
    ///
    /// A `Debug` render would also work, but naming the set here means a variant
    /// cannot be renamed without also changing an operator-facing string, which is
    /// exactly the sort of silent contract change worth making loud.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Registering => "registering",
            Self::Online => "online",
            Self::Backoff => "backoff",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

/// Durable channel policy for one Network: what to hold, and what to show.
///
/// This is a trait for the same reason Plan 020's `DurableNetworks` is one. The decision
/// that has to be qualified here is what an owner does when a detach commit's outcome
/// cannot be determined, and a bare store handle cannot be made to answer ambiguously
/// without corrupting a real database. Production passes the store handle; a test passes
/// a double that answers one commit ambiguously and delegates everything else.
#[async_trait::async_trait]
pub trait ChannelPolicy: Send + Sync {
    /// Records or clears one desired channel's detached flag. Returns false when the
    /// channel is not one of this Network's desired channels.
    async fn set_detached(
        &self,
        network: NetworkId,
        channel: &str,
        detached: bool,
    ) -> Result<bool, StoreError>;
    /// Re-reads this Network's durable desired channels.
    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError>;
}

/// The production policy: the store worker.
#[derive(Clone)]
pub struct StoreChannelPolicy(StoreHandle);
#[async_trait::async_trait]
impl ChannelPolicy for StoreChannelPolicy {
    async fn set_detached(
        &self,
        network: NetworkId,
        channel: &str,
        detached: bool,
    ) -> Result<bool, StoreError> {
        self.0
            .set_desired_channel_detached(network, channel, detached)
            .await
    }
    async fn load(&self) -> Result<Vec<NetworkRecord>, StoreError> {
        self.0.load_networks().await
    }
}

/// Owns one Network across connection generations and any number of local sessions.
pub struct NetworkOwner<P> {
    provider: P,
    network: NetworkId,
    context: crate::catalog::SupervisorContext,
    store: StoreHandle,
    /// Durable channel policy, held behind its own trait so the ambiguous-commit
    /// branch is reachable from a test without a corrupt database.
    policy: Arc<dyn ChannelPolicy>,
    /// Operator presence, owned here rather than per generation.
    ///
    /// Manual-away belongs to the Operator, not to a connection: a reconnect must
    /// re-apply it, not drop it, and only an owner that outlives generations can carry
    /// that. The generation-scoped half — what upstream currently believes — is derived
    /// from this on every registration and dies with the generation.
    presence: std::sync::Mutex<PresenceState>,
    /// The process control plane, for bound sessions' administration requests.
    ///
    /// Held by handle and only by handle. It is a bounded request sender, so a session
    /// asking to create or delete a Network submits a typed request to the one task that
    /// owns every live owner -- the owner cannot mutate the catalog from under the
    /// controller, and a client task still cannot reach a store or a supervisor.
    control: Option<crate::controller::RuntimeControlHandle>,
    snapshot: watch::Sender<NetworkSnapshot>,
    /// Process-wide connect admission.
    ///
    /// Held by handle, never owned: one Network's retry timing must not be able to
    /// affect another's, and every attempt in this process shares one budget.
    reconnect: ReconnectScheduler,
    /// Process-wide resource accounting.
    ///
    /// Held by handle for the same reason as the scheduler: an owner that owned it could
    /// keep reporting after it stopped, and one owner dropping it would stop the others
    /// being measurable.
    resources: ResourceLedger,
}

impl<P: I2pStreamProvider> NetworkOwner<P> {
    pub fn new(
        provider: P,
        context: crate::catalog::SupervisorContext,
        store: StoreHandle,
        reconnect: ReconnectScheduler,
    ) -> Result<Self, RuntimeError> {
        let (snapshot, _) = watch::channel(NetworkSnapshot::default());
        Self::with_snapshot_channel(provider, context, store, reconnect, snapshot)
    }

    /// Builds an owner whose durable channel policy is a caller-supplied double.
    ///
    /// Everything else is identical to [`NetworkOwner::new`], including the store the
    /// owner uses for history. This exists for the ambiguous-commit qualification and
    /// changes no production path.
    pub fn with_channel_policy(
        provider: P,
        context: crate::catalog::SupervisorContext,
        store: StoreHandle,
        reconnect: ReconnectScheduler,
        policy: Arc<dyn ChannelPolicy>,
    ) -> Result<Self, RuntimeError> {
        let (snapshot, _) = watch::channel(NetworkSnapshot::default());
        Self::with_snapshot_channel_and_policy(
            provider, context, store, reconnect, snapshot, policy,
        )
    }

    /// Builds an owner over a snapshot channel the caller created.
    ///
    /// The controller owns the receiver so it can report per-Network gauges without
    /// owning the owner. Holding a receiver rather than the owner is the point: the
    /// controller must never be able to drive an owner it has a view of.
    pub fn with_snapshot_channel(
        provider: P,
        context: crate::catalog::SupervisorContext,
        store: StoreHandle,
        reconnect: ReconnectScheduler,
        snapshot: watch::Sender<NetworkSnapshot>,
    ) -> Result<Self, RuntimeError> {
        // Production policy is the store worker itself; nothing else is wrapped around
        // it, so the ordinary path is one hop from the owner to the bounded worker.
        let policy = Arc::new(StoreChannelPolicy(store.clone())) as Arc<dyn ChannelPolicy>;
        Self::with_snapshot_channel_and_policy(
            provider, context, store, reconnect, snapshot, policy,
        )
    }

    /// Gives this owner the process control plane.
    ///
    /// This is what lets a session *bound* to this Network administrate the bouncer,
    /// rather than making administration a privilege of being unbound. The owner does not
    /// gain authority: it holds a bounded request sender, so every change still lands in
    /// the controller's one serialized queue. A supervisor built without a controller --
    /// a test harness, or the pre-M005 legacy path -- simply has none, and a bound
    /// session there gets an explicit refusal instead of a silent one.
    pub fn with_control(mut self, control: crate::controller::RuntimeControlHandle) -> Self {
        self.control = Some(control);
        self
    }

    /// [`NetworkOwner::with_snapshot_channel`] with the durable channel policy supplied
    /// by the caller.
    ///
    /// There is deliberately no third way to build an owner: the policy is the only
    /// injectable seam, and it is the only one whose ambiguous-commit branch matters.
    pub fn with_snapshot_channel_and_policy(
        provider: P,
        context: crate::catalog::SupervisorContext,
        store: StoreHandle,
        reconnect: ReconnectScheduler,
        snapshot: watch::Sender<NetworkSnapshot>,
        policy: Arc<dyn ChannelPolicy>,
    ) -> Result<Self, RuntimeError> {
        snapshot.send_modify(|state| {
            state.network = Some(context.network);
            state.phase = Some(Phase::Idle);
            state.nick = Some(context.record.nick.clone());
        });
        // An owner task exists from construction, so it is counted from construction. The
        // `Drop` below is what makes the count return to baseline.
        context.resources.register(context.network)?;
        let resources = context.resources.clone();
        // Read before `context` moves into the owner: the durable policy is the seed,
        // and a separate borrow of the moved value would not be available here.
        let presence = PresenceState::new(PresencePolicy::from_record(&context.record));
        Ok(Self {
            provider,
            network: context.network,
            context,
            store,
            policy,
            presence: std::sync::Mutex::new(presence),
            control: None,
            snapshot,
            reconnect,
            resources,
        })
    }

    /// Process-wide resource accounting this owner publishes into.
    pub fn resources(&self) -> &ResourceLedger {
        &self.resources
    }

    /// Publishes this owner's resting gauges, used when a generation ends.
    ///
    /// A Network that is between generations still exists and still holds its owner task
    /// and its attached sessions, so only the per-generation gauges fall to zero. An
    /// owner that reported itself entirely empty here would make a restarting Network
    /// look like a leak-free one.
    fn publish_resting_gauges(&self) {
        let _ = self.resources.observe(
            self.network,
            NetworkGauges {
                owner_tasks: 1,
                ..NetworkGauges::ZERO
            },
        );
    }

    /// The process-wide connect budget this Network is gated by.
    pub fn reconnect_scheduler(&self) -> &ReconnectScheduler {
        &self.reconnect
    }

    pub fn network(&self) -> NetworkId {
        self.network
    }

    pub fn subscribe_snapshot(&self) -> watch::Receiver<NetworkSnapshot> {
        self.snapshot.subscribe()
    }

    fn set_phase(&self, phase: Phase, generation: Option<ConnectionGeneration>) {
        self.snapshot.send_modify(|state| {
            state.phase = Some(phase);
            state.generation = generation;
            if matches!(phase, Phase::Idle | Phase::Backoff | Phase::Stopped) {
                state.attached_sessions = 0;
            }
        });
    }

    fn publish_state(&self, state: &NetworkState) {
        self.snapshot.send_modify(|snapshot| {
            snapshot.nick = Some(state.nick.clone());
            // Observed membership only, so a reader cannot mistake an outstanding or
            // rejected attempt for a live channel.
            snapshot.channels = state.joined_channels();
            snapshot.detached_channels = state.detached_channels();
            snapshot.pending_joins = state.pending_joins();
            snapshot.rejected_joins = state.rejected_joins();
        });
    }

    /// Publishes the gauges that describe the *live* generation.
    ///
    /// These change on every turn of the generation loop, so they are written on every
    /// turn rather than only at teardown. A snapshot refreshed only when the
    /// generation ends would report a healthy Network as having no sessions, an empty
    /// upstream queue, and no open routes, which is precisely the kind of untruthful
    /// diagnostic this subsystem exists to avoid.
    ///
    /// The write is conditional so a busy but unchanged generation does not wake
    /// every subscriber once per line.
    fn publish_gauges(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        router: &ResponseRouter,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        control_tx: &mpsc::Sender<Vec<u8>>,
        reconcile_pending: usize,
        history_queued: usize,
    ) {
        let normal_depth = NORMAL_QUEUE_CAPACITY - normal_tx.capacity();
        let control_depth = CONTROL_QUEUE_CAPACITY - control_tx.capacity();
        let routes = router.open_routes();
        let batches = router.open_batches();
        let attached = sessions.len();
        // The deepest queue across attached sessions, not the sum. A sum would report one
        // pathological client as if every client were that far behind, which is the
        // opposite of what the number is for.
        let (mut session_normal, mut session_control) = (0, 0);
        for task in sessions.values() {
            let (normal, control) = task.handle().queue_depths();
            session_normal = session_normal.max(normal);
            session_control = session_control.max(control);
        }
        let owner_owned = NetworkGauges {
            owner_tasks: 1,
            session_tasks: attached,
            session_normal,
            session_control,
            upstream_normal: normal_depth,
            upstream_control: control_depth,
            response_routes: routes,
            open_batches: batches,
            desired_reconcile: reconcile_pending,
            history_ingest: history_queued,
        };
        // The ledger is the process-wide accounting surface, so a refusal here is counted
        // there and never propagated: refusing to report a gauge must not stop the
        // Network, because the Network is real and the diagnostic is not.
        let _ = self.resources.observe(self.network, owner_owned);
        self.snapshot.send_if_modified(|snapshot| {
            if snapshot.attached_sessions == attached
                && snapshot.upstream_normal_queue_depth == normal_depth
                && snapshot.upstream_control_queue_depth == control_depth
                && snapshot.response_routes == routes
                && snapshot.open_batches == batches
            {
                return false;
            }
            snapshot.attached_sessions = attached;
            snapshot.upstream_normal_queue_depth = normal_depth;
            snapshot.upstream_control_queue_depth = control_depth;
            snapshot.response_routes = routes;
            snapshot.open_batches = batches;
            true
        });
    }

    /// Runs the Network owner until stopped.
    ///
    /// Session attachment and upstream ownership are independent: sessions attach and
    /// detach at any time, and losing every client never ends the upstream session.
    pub async fn serve(
        &self,
        mut commands: mpsc::Receiver<SupervisorCommand>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), RuntimeError> {
        let mut backoff = crate::Backoff {
            attempt: 0,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(300),
            jitter_percent: 20,
        };
        let mut generation = 0u64;
        // Attachments that arrived before the owner loop started. They are bounded by
        // the control queue and are re-issued into the generation when it comes up.
        let mut pending_attach: Vec<(SessionId, ClientId, Box<dyn ByteStream>)> = Vec::new();
        loop {
            if stopped_now(&stop) {
                self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                return Ok(());
            }
            generation = generation
                .checked_add(1)
                .ok_or(RuntimeError::GenerationExhausted)?;
            // Process-wide admission happens before any connect call. Every attempt in
            // this process -- first connect and every retry alike -- passes this gate, so
            // a cold start of many stored Networks cannot stampede the router.
            let permit = tokio::select! {
                _ = stopped(&mut stop) => {
                    self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                    return Ok(());
                }
                admitted = self.reconnect.acquire(self.network) => match admitted {
                    Ok(permit) => permit,
                    Err(_) => {
                        // Terminal, or the bounded waiter set is full. Either way there
                        // is nothing to retry into.
                        self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                        return Err(RuntimeError::QueueOverloaded);
                    }
                },
            };
            self.set_phase(Phase::Connecting, Some(ConnectionGeneration(generation)));
            let connection = tokio::select! {
                _ = stopped(&mut stop) => {
                    self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                    return Ok(());
                }
                result = timeout_connection(self.provider.connect(&self.context.record.endpoint)) => {
                    match result {
                        Ok(Ok(stream)) => Ok(stream),
                        Ok(Err(error)) => Err(RuntimeError::Provider(error)),
                        Err(_) => Err(RuntimeError::Timeout),
                    }
                }
            };
            // The permit is held for exactly the connect attempt. Dropping it releases
            // in-flight capacity whether the attempt succeeded, failed, or was cancelled.
            drop(permit);
            let outcome = match connection {
                Ok(upstream) => {
                    self.set_phase(Phase::Registering, Some(ConnectionGeneration(generation)));
                    let online_started = Instant::now();
                    let result = self
                        .run_generation(
                            upstream,
                            ConnectionGeneration(generation),
                            &mut commands,
                            &mut stop,
                            &mut pending_attach,
                        )
                        .await;
                    if online_started.elapsed() >= Duration::from_secs(300) {
                        backoff.stable_online();
                    }
                    match result {
                        Ok(()) | Err(RuntimeError::Stopped) => {
                            self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                            return Ok(());
                        }
                        Err(RuntimeError::Registration) => {
                            self.snapshot.send_modify(|state| {
                                state.phase = Some(Phase::Stopped);
                                state.attached_sessions = 0;
                                state.last_error = Some("registration rejected");
                            });
                            return Err(RuntimeError::Registration);
                        }
                        // Terminal for the same reason: a refused sequence will be
                        // refused identically on a retry, and each retry would spend a
                        // process-wide connect permit to produce identical upstream
                        // traffic. Configuration or a reconcile is the only thing that
                        // can change the answer.
                        Err(RuntimeError::NickExhausted) => {
                            self.reconnect.mark_terminal(self.network);
                            self.snapshot.send_modify(|state| {
                                state.phase = Some(Phase::Stopped);
                                state.attached_sessions = 0;
                                state.last_error = Some("nick exhausted");
                            });
                            return Err(RuntimeError::NickExhausted);
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            // The generation's per-generation state is gone: routes, batches, upstream
            // queues and the ingest queue all died with it. The owner task itself did
            // not, so only the generation-scoped gauges fall to zero here.
            self.publish_resting_gauges();
            // A registration rejection means the credentials or configuration were
            // refused. Retrying the identical request cannot succeed, and each attempt
            // would spend a permit the whole process shares, so the Network is marked
            // terminal and stops competing until it is reconciled.
            if matches!(&outcome, Err(RuntimeError::Registration)) {
                self.reconnect.mark_terminal(self.network);
            }
            let entropy = jitter_entropy(self.network, generation, self.reconnect.entropy_seed());
            let delay = backoff.next_delay(entropy);
            self.snapshot.send_modify(|state| {
                state.phase = Some(Phase::Backoff);
                state.attached_sessions = 0;
                state.reconnect_attempt = backoff.attempt;
                state.next_retry_delay = Some(delay);
                state.last_error = Some(crate::error_class(&outcome));
            });
            tokio::select! {
                _ = stopped(&mut stop) => {
                    self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                    return Ok(());
                }
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    /// Owns one upstream generation together with every session attached to it.
    #[allow(clippy::too_many_arguments)]
    async fn run_generation<D: ByteStream + 'static>(
        &self,
        upstream: D,
        generation: ConnectionGeneration,
        commands: &mut mpsc::Receiver<SupervisorCommand>,
        stop: &mut watch::Receiver<bool>,
        pending_attach: &mut Vec<(SessionId, ClientId, Box<dyn ByteStream>)>,
    ) -> Result<(), RuntimeError> {
        let (mut ur, mut uw) = tokio::io::split(upstream);
        // Desired intent is durable and restored here; observed state is always fresh.
        //
        // It is re-read from storage at each generation rather than reused from the
        // record this owner was built with. A channel joined or detached while an
        // earlier generation was live is durable intent just like any other, and taking
        // the birth record instead would silently drop it on the next reconnect.
        let desired = self.durable_desired_policy().await;
        let mut state = NetworkState::new(&self.context.record.nick, &desired);
        // Presence policy is durable; the away state upstream currently holds is not.
        // A fresh generation starts with no upstream away state, so the current policy is
        // re-applied after registration rather than a stale observation being restored.
        // The Operator's manual-away carries across the generation boundary; everything
        // this generation observed does not.
        let mut presence = self.owner_presence().for_generation();
        // Per-session classification. An empty entry set with a non-empty session map is
        // impossible: every attached session is classified at attach time.
        let mut presence_of: BTreeMap<SessionId, SessionPresence> = BTreeMap::new();
        // Bounded fallback sequence for a preferred nick the server already holds.
        let mut fallback = crate::presence::NickFallback::new(&self.context.record.nick, None);
        let mut decoder = LineDecoder::default();
        let mut ubuf = [0u8; 2048];
        let mut welcomed = false;
        let mut cap_finished = false;
        // Upstream capability negotiation is generation-owned and downstream-client
        // independent: it is a pure function of what the server offered.
        let mut upstream_caps = UpstreamCapabilities::default();
        let mut sasl_plain_offered = false;
        let mut requested = false;
        let mut sasl_active = false;
        let registration = async {
            send(&mut uw, "CAP LS 302\r\n").await?;
            send(
                &mut uw,
                &format!(
                    "NICK {}\r\nUSER {} 0 * :{}\r\n",
                    state.nick, self.context.record.username, self.context.record.realname
                ),
            )
            .await?;
            // The preferred nick has now been offered. The fallback sequence continues
            // after it rather than starting from it, so a refusal is answered with a
            // different name rather than with the same one twice.
            fallback.prime();
            while !(welcomed && cap_finished) {
                let n = if cap_finished {
                    ur.read(&mut ubuf).await?
                } else {
                    crate::timeout_bounded(crate::CAP_SASL_TIMEOUT, ur.read(&mut ubuf))
                        .await
                        .map_err(|_| RuntimeError::Timeout)?
                        .map_err(RuntimeError::Io)?
                };
                if n == 0 {
                    return Err(RuntimeError::Protocol);
                }
                for line in decoder.push(&ubuf[..n]) {
                    let bytes = line.map_err(|_| RuntimeError::Protocol)?;
                    let message = Message::parse(&bytes).map_err(|_| RuntimeError::Protocol)?;
                    message
                        .validate_tag_budget(TagDirection::ServerOutput)
                        .map_err(|_| RuntimeError::Protocol)?;
                    let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
                    let params: Vec<String> = message
                        .params
                        .iter()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .collect();
                    match command.as_str() {
                        "CAP" if params.iter().any(|p| p == "LS") => {
                            let capabilities = params.last().map(String::as_str).unwrap_or("");
                            for token in capabilities.split_whitespace() {
                                upstream_caps.note_offer(token.trim_start_matches(':'));
                            }
                            sasl_plain_offered |= capabilities.split_whitespace().any(|item| {
                                item.strip_prefix("sasl=").is_some_and(|mechanisms| {
                                    mechanisms
                                        .split(',')
                                        .any(|mechanism| mechanism.eq_ignore_ascii_case("PLAIN"))
                                })
                            });
                            let continuation = params.get(2).is_some_and(|p| p == "*");
                            if !requested && !continuation {
                                if self.context.record.sasl.is_some()
                                    && (!upstream_caps.was_offered("sasl") || !sasl_plain_offered)
                                {
                                    return Err(RuntimeError::Registration);
                                }
                                // The requested set is the reviewed foundational set
                                // plus SASL when configured. It never depends on an
                                // attached client, because upstream negotiation happens
                                // once per generation while clients attach freely.
                                let mut wanted = upstream_caps.request_set();
                                if self.context.record.sasl.is_some()
                                    && upstream_caps.was_offered("sasl")
                                {
                                    wanted.insert(0, "sasl".to_owned());
                                }
                                if wanted.is_empty() {
                                    send(&mut uw, "CAP END\r\n").await?;
                                    cap_finished = true;
                                } else {
                                    send(&mut uw, &format!("CAP REQ :{}\r\n", wanted.join(" ")))
                                        .await?;
                                }
                                requested = true;
                            }
                        }
                        "CAP" if params.iter().any(|p| p == "ACK") => {
                            for token in params.iter().flat_map(|p| p.split_whitespace()) {
                                upstream_caps.note_enabled(token);
                            }
                            // Observed state needs to know whether a membership prefix run
                            // is the member's complete set or only its highest symbol, and
                            // only the upstream negotiation can settle that. Recording it
                            // here, once, is what keeps a later projection from guessing.
                            state.set_upstream_multi_prefix(
                                upstream_caps.is_enabled(crate::capability::MEMBER_MULTI_PREFIX),
                            );
                            let sasl_accepted = upstream_caps.is_enabled("sasl");
                            if self.context.record.sasl.is_some() && !sasl_accepted {
                                return Err(RuntimeError::Registration);
                            }
                            if self.context.record.sasl.is_some() {
                                send(&mut uw, "AUTHENTICATE PLAIN\r\n").await?;
                                sasl_active = true;
                            } else {
                                send(&mut uw, "CAP END\r\n").await?;
                                cap_finished = true;
                            }
                        }
                        "CAP"
                            if params.iter().any(|p| p == "NAK")
                                && self.context.record.sasl.is_some() =>
                        {
                            return Err(RuntimeError::Registration);
                        }
                        "CAP" if params.iter().any(|p| p == "NAK") => {
                            // A NAK means the server refused something we asked for. The
                            // non-SASL capabilities are optional, so negotiation simply
                            // proceeds with whatever was granted.
                            send(&mut uw, "CAP END\r\n").await?;
                            cap_finished = true;
                        }
                        "AUTHENTICATE"
                            if sasl_active && params.first().is_some_and(|p| p == "+") =>
                        {
                            let (user, password) = self
                                .context
                                .record
                                .sasl
                                .as_ref()
                                .ok_or(RuntimeError::Registration)?;
                            let raw = Zeroizing::new(format!("\0{}\0{}", user, password.expose()));
                            let encoded = Zeroizing::new(base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                raw.as_bytes(),
                            ));
                            for chunk in encoded.as_bytes().chunks(400) {
                                let frame = Zeroizing::new(format!(
                                    "AUTHENTICATE {}\r\n",
                                    String::from_utf8_lossy(chunk)
                                ));
                                send(&mut uw, &frame).await?;
                            }
                            if encoded.len() % 400 == 0 {
                                send(&mut uw, "AUTHENTICATE +\r\n").await?;
                            }
                        }
                        "903" if sasl_active => {
                            send(&mut uw, "CAP END\r\n").await?;
                            cap_finished = true;
                        }
                        "904" | "905" | "906" | "907" if sasl_active => {
                            return Err(RuntimeError::Registration);
                        }
                        // A collision is answered, not waited out. Sitting until the
                        // registration ceiling would turn a two-second collision into a
                        // thirty-second stall and look indistinguishable from a dead
                        // network.
                        "433" | "436" => {
                            let Some(next) = fallback.next_candidate() else {
                                return Err(RuntimeError::NickExhausted);
                            };
                            if !crate::presence::valid_nick(&next) {
                                return Err(RuntimeError::NickExhausted);
                            }
                            state.nick = next.clone();
                            send(&mut uw, &format!("NICK {next}\r\n")).await?;
                        }
                        "001" => welcomed = true,
                        "ERROR" | "464" | "465" | "451" => return Err(RuntimeError::Registration),
                        _ => match state.apply_line(&message) {
                            LineOutcome::Quiet => {}
                            LineOutcome::ReplyPong(token) => {
                                send(&mut uw, &format!("PONG :{token}\r\n")).await?;
                            }
                            LineOutcome::Malformed => {
                                return Err(RuntimeError::Protocol);
                            }
                        },
                    }
                }
            }
            // Desired state is re-sent only after a fresh registration, never carried
            // across a generation boundary. Writing a JOIN proves nothing about
            // membership: each attempt is recorded as outstanding and only an
            // authoritative self JOIN closes it as confirmed.
            //
            // A detached channel is joined here exactly like an attached one. Detaching
            // is a statement about downstream presentation, so upstream membership and
            // history collection continue unchanged.
            for channel in desired
                .iter()
                .take(crate::state::MAX_CHANNELS)
                .map(|entry| entry.target.clone())
            {
                state.begin_desired_join(&channel);
                send(&mut uw, &format!("JOIN {channel}\r\n")).await?;
            }
            Ok::<(), RuntimeError>(())
        };
        tokio::select! {
            _ = stopped(stop) => return Err(RuntimeError::Stopped),
            result = tokio::time::timeout(REGISTRATION_TIMEOUT, registration) =>
                result.unwrap_or(Err(RuntimeError::Timeout))?,
        }
        // The generation is online on its own: no local client is required.
        self.snapshot.send_modify(|snapshot| {
            snapshot.phase = Some(Phase::Online);
            snapshot.generation = Some(generation);
            snapshot.last_error = None;
            snapshot.reconnect_attempt = 0;
            snapshot.upstream_capabilities = upstream_caps.fingerprint();
            // Routes are generation-local, so a fresh generation starts with none.
            snapshot.response_routes = 0;
        });
        self.publish_state(&state);

        // The generation's presence policy is applied now, against the sessions that
        // are already attached. An upstream that has just registered us knows nothing
        // about our away state, so writing it here is what makes "re-applied, not
        // restored" true in the only sense that matters on the wire.
        let (control_tx, mut control_rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (normal_tx, mut normal_rx) = mpsc::channel::<OutboundIntent>(NORMAL_QUEUE_CAPACITY);
        let (session_tx, mut session_rx) =
            mpsc::channel::<SessionEvent>(SESSION_EVENT_QUEUE_CAPACITY);
        let mut upstream_writer = JoinSet::new();
        {
            let generation_fence = generation;
            upstream_writer.spawn(async move {
                loop {
                    let next = next_intent_frame(&mut control_rx, &mut normal_rx).await;
                    match next {
                        Some(Err(bytes)) => write_frame(&mut uw, &bytes).await?,
                        Some(Ok(intent)) if intent.generation == generation_fence => {
                            write_frame(&mut uw, &intent.wire).await?
                        }
                        // An intent stamped by an earlier generation is dropped rather
                        // than written: replaying user traffic across a reconnect would
                        // risk duplicating a message whose delivery is unknown.
                        Some(Ok(_)) => continue,
                        None => return Ok::<(), std::io::Error>(()),
                    }
                }
            });
        }

        // Every attached session, keyed by its ephemeral identity.
        let mut sessions: BTreeMap<SessionId, SessionTask> = BTreeMap::new();
        // The per-generation history journal and its bounded ingestion queue. They are
        // generation-scoped: a new generation starts with fresh observed buffers and
        // never inherits the previous generation's queue.
        let mut journal = crate::journal::HistoryJournal::new(
            self.context.network,
            self.store.clone(),
            Box::new(i2pr_irc_core::SystemWallClock),
            i2pr_irc_core::Casemapping::Rfc1459,
        );
        // Resolved conversation targets, populated from authoritative membership and
        // from client intents. A target is only resolved durably once.
        let mut buffers: BTreeMap<String, BufferId> = BTreeMap::new();
        for channel in state.joined_channels() {
            if let Ok(buffer) = journal.resolve_buffer(BufferKind::Channel, &channel).await {
                buffers.insert(casemapped(&channel), buffer);
            }
        }
        let (ingest_tx, mut ingest_rx) = mpsc::channel::<IngestItem>(INGEST_QUEUE_CAPACITY);
        // Response routing is generation-local and SessionId-scoped. It is created per
        // generation, so nothing can survive into a different connection.
        let mut router = ResponseRouter::default();
        // Batch identifiers are generation-local, exactly like response routes: a
        // batch id from an earlier connection must never be referencable later.
        let mut batches = crate::ircv3::BatchTracker::default();
        // What this generation currently advertises downstream, so a capability change
        // can be reported as a difference rather than as a bare announcement. Seeded
        // from the registration result: `echo-message` was decided there.
        let mut advertised_downstream: BTreeSet<String> =
            crate::capability::DownstreamCapabilities::advertisement(&upstream_caps)
                .into_iter()
                .collect();
        let listing: Vec<String> = advertised_downstream.iter().cloned().collect();
        self.snapshot
            .send_modify(|snapshot| snapshot.advertisement = listing.clone());
        // Search replies are framed in their own batch type, so they need their own
        // identifier space. It is generation-local for the same reason `batches` is: an
        // id from an earlier connection must not be referencable by a client that
        // reconnects and asks the same question again.
        let mut search_batch: u64 = 1;
        // Desired membership this generation could not hand to the upstream queue.
        // Generation-local like every other piece of live bookkeeping: a new
        // generation rebuilds desired membership from durable storage anyway.
        let mut reconcile = DesiredReconcile::default();
        // Attachments that arrived while this generation was starting.
        for (session, client, stream) in pending_attach.drain(..).take(MAX_SESSIONS_PER_NETWORK) {
            attach_session(
                session,
                client,
                &state.nick,
                stream,
                &session_tx,
                &mut sessions,
                &self.snapshot,
                listing.clone(),
            );
            presence_of.insert(session, SessionPresence::DEFAULT);
        }

        let mut probe = tokio::time::interval(crate::LIVENESS_INTERVAL);
        probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Keep-nick reclaim runs on its own generation-owned clock. It is deliberately
        // not wired to client activity: a client attaching must never make the bouncer
        // poll upstream faster, or the bouncer's upstream behaviour would depend on
        // which local sessions happen to exist.
        let mut reclaim = tokio::time::interval(crate::presence::RECLAIM_INTERVAL);
        reclaim.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The tick fires immediately, so the clock starts now and the first pass is
        // compared against a zero elapsed time: a fresh generation asks nothing until the
        // schedule is actually due.
        let reclaim_started = std::time::Instant::now();
        // Upstream evidence that the preferred nick is free wakes this rather than the
        // schedule, so a claim the server has already blessed is not delayed by the
        // interval. Generation-local, like the clock it accelerates.
        let reclaim_wake = tokio::sync::Notify::new();
        // Generation-local reclaim state. It exists only when the policy is on *and* the
        // server did not give us the nick we asked for, so a Network that holds its
        // configured nick allocates nothing.
        let mut reclaim_attempt = (presence.policy().keep_nick
            && !state.nick.eq_ignore_ascii_case(&self.context.record.nick))
        .then(|| ReclaimAttempt::new(&self.context.record.nick, &state.nick));
        if reclaim_attempt.is_some() {
            self.begin_reclaim(&mut reclaim_attempt, &state, &control_tx);
        }
        // Presence is evaluated *after* the waiting attachments are applied, so a
        // generation that came up with a client already waiting does not declare itself
        // away and then immediately back. A bouncer that flaps its away state on every
        // reconnect is a bouncer the network learns to ignore.
        if let Some(frame) = self.apply_presence(&mut presence, &presence_of) {
            let _ = queue_control(&control_tx, &frame);
        }
        let mut reconcile_tick = tokio::time::interval(DESIRED_RECONCILE_INTERVAL);
        reconcile_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut awaiting_pong: Option<(Instant, String)> = None;
        let outcome = loop {
            // Deferred desired membership is relieved at the top of every turn, not only
            // on the keepalive tick. A committed JOIN or PART is operator intent the
            // bouncer has already promised to store, so it converges as soon as the
            // queue has room -- bounded by MAX_DESIRED_RECONCILE entries and by one
            // non-blocking attempt per turn, and never by waiting on anything.
            let reconciled = reconcile.drain(&normal_tx, generation);
            if reconciled > 0 {
                let pending = reconcile.pending.len();
                self.snapshot.send_modify(|snapshot| {
                    snapshot.desired_reconcile_drained = snapshot
                        .desired_reconcile_drained
                        .saturating_add(reconciled);
                    snapshot.desired_reconcile_pending = pending;
                });
            }
            let attach = next_attach(commands);
            let upstream_read = ur.read(&mut ubuf);
            let session_event = session_rx.recv();
            let writer_exit = upstream_writer.join_next();
            tokio::select! {
                _ = stopped(stop) => break Ok(()),
                command = attach => {
                    if let Some(command) = command {
                        self.handle_command(
                            command,
                            generation,
                            &control_tx,
                            &normal_tx,
                            &session_tx,
                            &mut sessions,
                            &mut presence_of,
                            &mut state,
                            &mut journal,
                            &buffers,
                            &mut reconcile,
                            advertised_downstream.iter().cloned().collect(),
                        )
                        .await;
                    }
                }
                event = session_event => {
                    // A closed queue means the owner is being torn down; the loop's
                    // own stop path ends the generation.
                    let Some(event) = event else { break Ok(()) };
                    if let Err(error) = self.handle_session_event(
                        event,
                        &mut sessions,
                        &mut state,
                        &control_tx,
                        &normal_tx,
                        generation,
                        &mut journal,
                        &buffers,
                        &mut router,
                        &mut batches,
                        &mut search_batch,
                        &mut reconcile,
                        &upstream_caps,
                        &mut presence,
                        &mut presence_of,
                    )
                    .await
                    {
                        // The only error this path reports is durable DesiredState that
                        // this generation provably cannot converge to in place. A
                        // controlled reconnect rebuilds membership from storage, which
                        // is the honest way to honour intent the bounded set cannot hold.
                        break Err(error);
                    }
                    // Presence is re-evaluated after every session event, because every
                    // session event can change the answer: an attach, a detach, a
                    // `PASSIVE`, a manual `AWAY`. An event that changes nothing produces
                    // no frame, because `apply_presence` returns one only on a
                    // transition.
                    if let Some(frame) = self.apply_presence(&mut presence, &presence_of)
                        && queue_control(&control_tx, &frame).is_err()
                    {
                        self.snapshot.send_modify(|snapshot| {
                            snapshot.last_error = Some("upstream-queue-refused");
                        });
                    }
                }
                writer = writer_exit => {
                    match writer {
                        Some(Ok(Ok(()))) => break Err(RuntimeError::Protocol),
                        Some(Ok(Err(error))) => break Err(RuntimeError::Io(error)),
                        Some(Err(error)) => break Err(RuntimeError::Io(std::io::Error::other(error))),
                        None => break Err(RuntimeError::Protocol),
                    }
                }
                item = ingest_rx.recv() => {
                    // At most a bounded batch per turn, so history work cannot
                    // monopolize the owner and delay a keepalive answer.
                    if let Some(item) = item {
                        let mut batch = vec![item];
                        for _ in 1..INGEST_BATCH_PER_TURN {
                            match ingest_rx.try_recv() {
                                Ok(next) => batch.push(next),
                                Err(_) => break,
                            }
                        }
                        for item in batch {
                            // The journal learns this Network's identity so it can
                            // tell an upstream echo from somebody else's message.
                            // Assigned each turn rather than once so a nick change
                            // mid-generation is picked up; it is two bounded strings.
                            let live_nick = Some(state.nick.as_str());
                            if journal.own_nick() != live_nick {
                                journal.set_own_nick(live_nick);
                            }
                            match journal.ingest(item.buffer, &item.message).await {
                                Ok(IngestOutcome::Recorded { .. }) => self.snapshot
                                    .send_modify(|snapshot| {
                                        snapshot.history_recorded =
                                            snapshot.history_recorded.saturating_add(1)
                                    }),
                                Ok(IngestOutcome::Skipped) => self.snapshot.send_modify(
                                    |snapshot| {
                                        snapshot.history_skipped =
                                            snapshot.history_skipped.saturating_add(1)
                                    },
                                ),
                                Ok(IngestOutcome::StoreUnavailable) => self.snapshot
                                    .send_modify(|snapshot| {
                                        snapshot.history_dropped =
                                            snapshot.history_dropped.saturating_add(1)
                                    }),
                                Err(_) => self.snapshot.send_modify(|snapshot| {
                                    snapshot.history_dropped =
                                        snapshot.history_dropped.saturating_add(1)
                                }),
                            }
                        }
                    }
                }
                _ = reconcile_tick.tick() => {
                    // An idle Network still converges committed operator intent.
                    let drained = reconcile.drain(&normal_tx, generation);
                    if drained > 0 {
                        let pending = reconcile.pending.len();
                        self.snapshot.send_modify(|snapshot| {
                            snapshot.desired_reconcile_drained =
                                snapshot.desired_reconcile_drained.saturating_add(drained);
                            snapshot.desired_reconcile_pending = pending;
                        });
                    }
                }
                _ = reclaim.tick() => {
                    // Reclaim writes upstream only when the policy is on, the observed
                    // nick differs from the configured one, and the schedule or
                    // `MONITOR` evidence says a write is due. The whole `reclaim` state
                    // is generation-local, so a probe scheduled by a connection that has
                    // since died cannot act on the connection that replaced it.
                    if presence.policy().keep_nick {
                        let elapsed = reclaim_started.elapsed();
                        let frame = reclaim_attempt
                            .as_mut()
                            .and_then(|attempt| self.reclaim_tick(attempt, &state, elapsed));
                        if let Some(frame) = frame
                            && queue_control(&control_tx, &frame).is_err()
                        {
                            self.snapshot.send_modify(|snapshot| {
                                snapshot.last_error = Some("upstream-queue-refused");
                            });
                        }
                    }
                }
                _ = reclaim_wake.notified() => {
                    // Evidence, not the clock. Same policy, same ceiling, and the same
                    // generated local attempt state: this only moves the *timing* of a
                    // write the scheduled pass would have been allowed to make anyway.
                    if presence.policy().keep_nick {
                        let elapsed = reclaim_started.elapsed();
                        let frame = reclaim_attempt
                            .as_mut()
                            .and_then(|attempt| self.reclaim_tick(attempt, &state, elapsed));
                        if let Some(frame) = frame
                            && queue_control(&control_tx, &frame).is_err()
                        {
                            self.snapshot.send_modify(|snapshot| {
                                snapshot.last_error = Some("upstream-queue-refused");
                            });
                        }
                    }
                }
                _ = probe.tick() => {
                    // Expired routes release their slots deterministically, so a slow
                    // server cannot wedge the router.
                    let expired = router.expire(std::time::Instant::now());
                    if expired > 0 {
                        self.snapshot
                            .send_modify(|snapshot| snapshot.response_routes = router.open_routes());
                    }
                    if awaiting_pong.as_ref().is_some_and(|(since, _)| since.elapsed() >= crate::LIVENESS_DEADLINE) {
                        break Err(RuntimeError::Timeout);
                    }
                    if awaiting_pong.is_none() {
                        let token = format!("bouncer-{}", generation.0);
                        match queue_control(&control_tx, &format!("PING :{token}\r\n")) {
                            Ok(()) => awaiting_pong = Some((Instant::now(), token)),
                            Err(error) => break Err(error),
                        }
                    }
                }
                count = upstream_read => {
                    let count = match count { Ok(count) => count, Err(error) => break Err(RuntimeError::Io(error)) };
                    if count == 0 { break Err(RuntimeError::Protocol); }
                    let mut failure = None;
                    // Lines applied since this chunk last handed the scheduler back.
                    let mut lines_since_yield = 0usize;
                    for line in decoder.push(&ubuf[..count]) {
                        lines_since_yield += 1;
                        // Hand the scheduler back periodically so session writers get to
                        // drain during a burst. Decoding is already complete for this
                        // chunk, so yielding here loses nothing.
                        if lines_since_yield >= UPSTREAM_LINES_PER_TURN {
                            lines_since_yield = 0;
                            tokio::task::yield_now().await;
                        }
                        let raw = match line { Ok(raw) => raw, Err(_) => { failure = Some(RuntimeError::Protocol); break } };
                        let message = match Message::parse(&raw) { Ok(message) => message, Err(_) => { failure = Some(RuntimeError::Protocol); break } };
                        if message.validate_tag_budget(TagDirection::ServerOutput).is_err() {
                            failure = Some(RuntimeError::Protocol);
                            break;
                        }
                        self.snapshot.send_modify(|snapshot| snapshot.upstream_events_seen = snapshot.upstream_events_seen.saturating_add(1));
                        // Only a PONG that answers an outstanding probe satisfies
                        // liveness; anything else is a protocol failure.
                        if message.command.eq_ignore_ascii_case(b"PONG") {
                            match awaiting_pong.as_ref().map(|(_, token)| token.clone()) {
                                Some(expected) if message.params.last().is_some_and(|token| *token == expected.as_bytes()) => {
                                    awaiting_pong = None;
                                }
                                _ => { failure = Some(RuntimeError::Protocol); break; }
                            }
                        }
                        // A server may add or withdraw a capability at any time. The
                        // announcement is recorded before the line reaches ordinary
                        // handling, because the downstream advertisement is a function of
                        // the upstream set and a client that asked for change
                        // notifications must be told before the next frame it depends on.
                        let announced =
                            crate::capability::UpstreamCapabilities::note_change(&message);
                        let acknowledged = message.command.eq_ignore_ascii_case(b"CAP")
                            && message
                                .params
                                .iter()
                                .any(|param| param.eq_ignore_ascii_case(b"ACK"));
                        if let Some((change, names)) = announced.filter(|_| {
                            !acknowledged
                        }) {
                            for name in &names {
                                match change {
                                    crate::capability::CapChange::New => {
                                        upstream_caps.note_new(name)
                                    }
                                    crate::capability::CapChange::Del => {
                                        upstream_caps.note_deleted(name)
                                    }
                                }
                            }
                            // A withdrawn capability stops being something the bouncer may
                            // rely on, and a `multi-prefix` run the server had promised
                            // must stop being reported as complete.
                            if matches!(change, crate::capability::CapChange::Del) {
                                state.set_upstream_multi_prefix(upstream_caps.is_enabled(
                                    crate::capability::MEMBER_MULTI_PREFIX,
                                ));
                            }
                            if matches!(change, crate::capability::CapChange::New) {
                                // The server now offers a capability. If the bouncer
                                // serves it downstream and has not enabled it, the only
                                // way it can ever be served is to ask. The request is
                                // bounded by this one announcement's names and skips
                                // anything already enabled, so a server repeating `NEW`
                                // cannot make this grow.
                                let wanted: Vec<String> = names
                                    .iter()
                                    .filter(|name| !upstream_caps.is_enabled(name))
                                    .filter(|name| {
                                        crate::capability::UPSTREAM_FOUNDATIONAL
                                            .contains(&name.as_str())
                                    })
                                    .cloned()
                                    .collect();
                                if !wanted.is_empty() {
                                    queue_control(
                                        &control_tx,
                                        &format!("CAP REQ :{}\r\n", wanted.join(" ")),
                                    )?;
                                }
                            }
                            let _ = change;
                            let now: BTreeSet<String> =
                                crate::capability::DownstreamCapabilities::advertisement(
                                    &upstream_caps,
                                )
                                .into_iter()
                                .collect();
                            if now != advertised_downstream {
                                let listing: Vec<String> = now.iter().cloned().collect();
                                for task in sessions.values() {
                                    task.handle().set_advertised(listing.clone());
                                }
                                self.snapshot
                                    .send_modify(|snapshot| snapshot.advertisement = listing.clone());
                                self.publish_capability_change(
                                    &advertised_downstream,
                                    &upstream_caps,
                                    &sessions,
                                );
                                advertised_downstream = now;
                            }
                        } else if acknowledged {
                            // An `ACK` is what actually *enables* a capability. Without
                            // this the request sent above would be answered and then
                            // forgotten, and the capability could never become serviceable.
                            for param in message.params.iter().skip(1) {
                                for name in String::from_utf8_lossy(param)
                                    .split_whitespace()
                                    .map(str::to_owned)
                                    .collect::<Vec<_>>()
                                {
                                    upstream_caps.note_enabled(&name);
                                }
                            }
                            // A capability that arrives mid-generation changes what the
                            // bouncer can mediate just as a startup ACK does, so observed
                            // state is told here too rather than only at registration.
                            state.set_upstream_multi_prefix(upstream_caps.is_enabled(
                                crate::capability::MEMBER_MULTI_PREFIX,
                            ));
                            let now: BTreeSet<String> =
                                crate::capability::DownstreamCapabilities::advertisement(
                                    &upstream_caps,
                                )
                                .into_iter()
                                .collect();
                            if now != advertised_downstream {
                                let listing: Vec<String> = now.iter().cloned().collect();
                                for task in sessions.values() {
                                    task.handle().set_advertised(listing.clone());
                                }
                                self.snapshot
                                    .send_modify(|snapshot| snapshot.advertisement = listing.clone());
                                self.publish_capability_change(
                                    &advertised_downstream,
                                    &upstream_caps,
                                    &sessions,
                                );
                                advertised_downstream = now;
                            }
                        }
                        match self.apply_upstream_line(
                            &raw,
                            &message,
                            &mut state,
                            &sessions,
                            &control_tx,
                            &buffers,
                            &ingest_tx,
                            &mut router,
                        ) {
                            Ok(report) => {
                                // A session that lost a live frame is detached before
                                // the next line is applied, so it cannot be handed a
                                // later frame and resume as if nothing was missed. Only
                                // the sessions named here are removed.
                                for id in report.desynchronized {
                                    self.detach_overloaded(&mut sessions, &mut router, id).await;
                                }
                            }
                            Err(error) => { failure = Some(error); break; }
                        }
                        // Reclaim evidence.
                        //
                        // `730` (MONITOR OFFLINE) names the nicks that became free, and
                        // a `303` whose list omits the preferred nick says the same thing
                        // for the `ISON` path. Neither is authoritative: the `NICK` that
                        // follows is a request, and only the server's own frame confirms
                        // it. Treating either as confirmation would let the bouncer
                        // believe it holds a nick it is still queued to claim.
                        self.note_reclaim_evidence(
                            &state,
                            &message,
                            &mut reclaim_attempt,
                            &reclaim_wake,
                        );
                        // Resolve a channel to its durable buffer *after* this line has
                        // been applied, because the line that creates membership is the
                        // self JOIN itself. Checking before applying would mean the very
                        // line that confirms the channel never resolves its buffer, and
                        // the channel would record no history until a server sent a
                        // second, redundant JOIN.
            if confirmed_self_channel(&state, &message).is_some_and(|channel| {
                            !buffers.contains_key(&casemapped(&channel))
                        }) && let Some(channel) = confirmed_self_channel(&state, &message) {
                            match journal.resolve_buffer(BufferKind::Channel, &channel).await {
                                Ok(buffer) => {
                                    buffers.insert(casemapped(&channel), buffer);
                                }
                                Err(_) => {
                                    self.snapshot.send_modify(|snapshot| {
                                        snapshot.history_dropped =
                                            snapshot.history_dropped.saturating_add(1)
                                        });
                                }
                            }
                        }
                        // A reattach whose membership was still absent is projected here,
                        // when the authoritative self JOIN finally confirms it, and not
                        // when it was requested. Projecting at request time would tell a
                        // client it had joined a channel the bouncer had not; projecting
                        // never would leave it told that it had joined a channel it never
                        // saw. Exactly once: the flag is cleared as it is consumed.
                        if let Some(channel) = confirmed_self_channel(&state, &message)
                            && state.is_reveal_pending(&channel)
                        {
                            state.clear_reveal(&channel);
                            self.reveal_channel(
                                &sessions,
                                &state,
                                &channel,
                                &mut journal,
                                &buffers,
                            )
                            .await;
                            self.snapshot.send_modify(|snapshot| {
                                snapshot.channels_reattached =
                                    snapshot.channels_reattached.saturating_add(1)
                            });
                        }
                    }
                    self.publish_state(&state);
                    if let Some(error) = failure { break Err(error); }
                }
            }
            // Published after the turn so the snapshot reflects settled state. The
            // upstream writer drains on its own task, so publishing before the turn
            // would always report the queue exactly as the turn found it.
            self.publish_gauges(
                &sessions,
                &router,
                &normal_tx,
                &control_tx,
                reconcile.pending.len(),
                INGEST_QUEUE_CAPACITY - ingest_tx.capacity(),
            );
        };

        // Deterministic teardown: every session and the writer are owned here, so no
        // client task can outlive the generation that created it.
        let ending = std::mem::take(&mut sessions);
        for (_, session) in ending {
            session.shutdown().await;
        }
        // The QUIT is queued before the senders are dropped, so the writer observes
        // it in order ahead of the channel-close that ends it.
        if matches!(outcome, Ok(())) {
            // Explicit stop is the only local action that deliberately sends an
            // upstream QUIT, and it is sent at most once.
            let _ = control_tx.try_send(b"QUIT :Bouncer shutting down\r\n".to_vec());
        }
        drop(control_tx);
        drop(normal_tx);
        match outcome {
            Ok(()) => {
                while let Some(result) = upstream_writer.join_next().await {
                    // Shutdown is best effort; the QUIT is already queued first.
                    let _ = result;
                }
                Ok(())
            }
            Err(error) => {
                upstream_writer.abort_all();
                while upstream_writer.join_next().await.is_some() {}
                Err(error)
            }
        }
    }

    /// Handles one owner command.
    #[allow(clippy::too_many_arguments)]
    async fn handle_command(
        &self,
        command: SupervisorCommand,
        generation: ConnectionGeneration,
        control_tx: &mpsc::Sender<Vec<u8>>,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        session_tx: &mpsc::Sender<SessionEvent>,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
        presence_of: &mut BTreeMap<SessionId, SessionPresence>,
        // A channel presentation change mutates observed presentation, so the state is
        // borrowed mutably for this command rather than being the read-only view the
        // rest of the command surface gets.
        state: &mut NetworkState,
        // The reattach projection needs the journal and the buffer map, and a refused
        // upstream enqueue needs the reconciliation set. An administrative command that
        // could not reach them would have to reimplement the client's own path, and two
        // implementations of "reattach a channel" is how they start disagreeing.
        journal: &mut crate::journal::HistoryJournal,
        buffers: &BTreeMap<String, BufferId>,
        reconcile: &mut DesiredReconcile,
        // What this generation advertises, for a session attaching right now. Passed in
        // rather than read from the snapshot: `watch` shares one lock between reads and
        // writes, and attaching writes to that same snapshot, so a borrow held across the
        // attach would deadlock on itself.
        advertised: Vec<String>,
    ) {
        match command {
            SupervisorCommand::Attach {
                session,
                client,
                stream,
                reply,
            } => {
                let accepted = if sessions.len() >= MAX_SESSIONS_PER_NETWORK {
                    Err(RuntimeError::QueueOverloaded)
                } else {
                    attach_session(
                        session,
                        client,
                        &state.nick,
                        stream,
                        session_tx,
                        sessions,
                        &self.snapshot,
                        // Whatever this generation advertises right now, read from the
                        // snapshot the owner already publishes rather than recomputed, so
                        // there is one answer rather than two that can differ.
                        //
                        // Bound and dropped first. `watch` shares one lock between reads
                        // and writes, and `attach_session` writes to the snapshot, so a
                        // borrow held across the call would deadlock on itself.
                        advertised.clone(),
                    );
                    // A newly attached session starts active. Classification is a fact
                    // about the session, not a counter, so the entry is added here and
                    // removed where the session goes away.
                    presence_of.insert(session, SessionPresence::DEFAULT);
                    // Presence is deliberately *not* re-evaluated on attach. A session
                    // that negotiated the pre-away draft may declare itself passive
                    // during registration, before the bouncer has heard from it at all;
                    // evaluating now would count it as an Operator and then take that
                    // back, which is a flap the rest of the network sees on every
                    // background client that connects. The first evaluation happens when
                    // the session's own first intent arrives.
                    Ok(())
                };
                let _ = reply.send(accepted);
            }
            SupervisorCommand::AttachPrepared { session, reply } => {
                // Captured before the value is consumed: adoption takes the session by
                // value, and presence classification is keyed by its id.
                let session_id = session.session_id();
                let accepted = adopt_prepared_session(
                    session,
                    &state.nick,
                    session_tx,
                    sessions,
                    &self.snapshot,
                );
                if accepted.is_ok() {
                    // An adopted session registered during admission and may already have
                    // declared itself passive; its intent arrived through the ordinary
                    // queue and the `Passive` arm classifies it. Until then it counts as
                    // active, which is the safe default.
                    presence_of.insert(session_id, SessionPresence::DEFAULT);
                }
                let _ = reply.send(accepted);
            }
            SupervisorCommand::Session { session, event } => {
                // A command naming an unknown session is stale and is dropped: it must
                // never be applied to whatever session now holds that identity slot.
                if sessions.contains_key(&session) {
                    let _ = session_tx.try_send(event);
                }
            }
            SupervisorCommand::ChannelPolicy {
                channel,
                detached,
                reply,
            } => {
                // No requesting session: the change came from the administration
                // surface, which reports the outcome through its own reply rather than
                // through a NOTICE. The ordering — commit first, present second — is
                // the same code either way, so an administrative change cannot drift
                // from a client's own `PART :detach`.
                let outcome = if detached {
                    self.apply_detach(sessions, state, None, &channel).await;
                    Ok(())
                } else {
                    self.apply_reattach(
                        sessions, state, None, &channel, journal, buffers, normal_tx, generation,
                        reconcile,
                    )
                    .await;
                    Ok(())
                };
                let _ = reply.send(outcome);
            }
            SupervisorCommand::Reconcile { reply } => {
                let _ = reply.send(self.reconcile(state).await);
            }
            SupervisorCommand::Stop { reply } => {
                let _ = reply.send(());
            }
        }
        let _ = (control_tx, normal_tx, generation);
    }

    /// Re-reads durable configuration and reconciles it against observed state.
    ///
    /// Observed membership is never overwritten from storage: only DesiredState is
    /// restored, and an authoritative network event remains the only way membership
    /// changes.
    /// Recomputes the downstream advertisement and tells listening sessions what moved.
    ///
    /// `cap-notify` exists because the downstream set is conditional on what upstream
    /// negotiated, and because the upstream may change that mid-generation. A client that
    /// negotiated the capability and is told nothing is in exactly the position
    /// `cap-notify` was supposed to fix.
    ///
    /// What is reported is the change in what *this bouncer* can serve, not what the
    /// server announced. Those differ: a server may newly offer a capability the bouncer
    /// does not implement, and telling a client about it would advertise something no
    /// client could ever get. Diffing the advertisement also makes the report idempotent:
    /// re-announcing the same thing changes nothing, and so says nothing.
    ///
    /// Both directions are emitted because both can happen. `echo-message` disappears
    /// when a `CAP DEL` withdraws it upstream, and appears once the bouncer holds it.
    ///
    /// Only sessions that negotiated `cap-notify` are addressed. Sending `CAP NEW` to a
    /// client that never asked would be unsolicited, and a client that does not
    /// understand `cap-notify` is entitled to treat the line as an unknown command.
    fn publish_capability_change(
        &self,
        previous: &BTreeSet<String>,
        upstream: &UpstreamCapabilities,
        sessions: &BTreeMap<SessionId, SessionTask>,
    ) {
        let now: BTreeSet<String> =
            crate::capability::DownstreamCapabilities::advertisement(upstream)
                .into_iter()
                .collect();
        if now == *previous {
            return;
        }
        let added: Vec<String> = now.difference(previous).cloned().collect();
        let removed: Vec<String> = previous.difference(&now).cloned().collect();
        for (subcommand, names) in [("NEW", added), ("DEL", removed)] {
            if names.is_empty() {
                continue;
            }
            let line = format!(":bouncer CAP * {subcommand} :{}\r\n", names.join(" "));
            for task in sessions.values() {
                if !task.handle().capabilities().negotiated_cap_notify() {
                    continue;
                }
                if task.handle().queue_control(&line).is_err() {
                    // A session whose queue refuses the notification has already fallen
                    // behind; the ordinary detach path decides what happens to it. This
                    // owner never awaits a client, so one full queue cannot reach the
                    // Network or any other attachment.
                    self.snapshot.send_modify(|snapshot| {
                        snapshot.fanout_dropped = snapshot.fanout_dropped.saturating_add(1)
                    });
                }
            }
        }
    }

    async fn reconcile(&self, state: &NetworkState) -> Result<(), RuntimeError> {
        let records = self
            .store
            .load_networks()
            .await
            .map_err(|error| crate::catalog::classify(error.kind()))?;
        // Reconciliation reads stored DesiredState only. Observed membership is left
        // exactly as the current generation observed it, because an authoritative
        // network event remains the only thing that may change it.
        let present = records.iter().any(|record| record.network == self.network);
        if !present {
            return Ok(());
        }
        self.publish_state(state);
        Ok(())
    }

    /// Applies one upstream line, then fans it out to every attached session.
    ///
    /// The event is normalized and applied once; fanout then decides per session.
    ///
    /// A session whose queue refuses the frame is reported as desynchronized rather
    /// than quietly skipped: a downstream IRC stream is ordered, so the bouncer cannot
    /// claim such a client is still in step with upstream, and there is no way to tell
    /// it which frames it missed. The caller detaches exactly those sessions. The
    /// owner never blocks on a client, so pressure on one attachment cannot reach the
    /// Network or any other attachment.
    ///
    /// A history-eligible line is also queued for durable ingestion. That queue is
    /// bounded and non-blocking: a full queue drops the event and counts it, so
    /// history pressure can never delay control traffic. That is deliberately a
    /// different policy -- durable history is best effort, while a live frame is not.
    #[allow(clippy::too_many_arguments)]
    fn apply_upstream_line(
        &self,
        raw: &[u8],
        message: &Message,
        state: &mut NetworkState,
        sessions: &BTreeMap<SessionId, SessionTask>,
        control_tx: &mpsc::Sender<Vec<u8>>,
        buffers: &BTreeMap<String, BufferId>,
        ingest_tx: &mpsc::Sender<IngestItem>,
        router: &mut ResponseRouter,
    ) -> Result<FanoutReport, RuntimeError> {
        let mut desynchronized = Vec::new();
        // Network state is applied exactly once per upstream line, before any routing
        // decision, so a reply delivered to a single client still updates the shared
        // view exactly as an ordinary fanout would.
        match state.apply_line(message) {
            LineOutcome::Quiet => {}
            LineOutcome::ReplyPong(token) => {
                queue_control(control_tx, &format!("PONG :{token}\r\n"))?;
            }
            LineOutcome::Malformed => return Err(RuntimeError::Protocol),
        }
        // Routing is consulted before ordinary fanout. A reply belonging to one client's
        // open route is delivered only to that client; it must never also fan out, or
        // the whole point of routing would be defeated.
        let outcome = router.deliver(incoming_for(message), |route| {
            // The client's own label is restored and the server's opaque label is
            // removed. No other client ever sees either.
            //
            // The reply is also degraded for the client that asked: `multi-prefix` puts
            // complete prefix runs into WHO and WHOIS replies, and the reply goes only
            // to a session that may not read them. Reading that session's own
            // capabilities here, rather than somewhere that could answer for a
            // different one, is what keeps two clients on one Network from being
            // answered the same question identically.
            let multi_prefix = sessions
                .get(&route.session)
                .map(|task| task.handle().capabilities().negotiated_multi_prefix())
                .unwrap_or(false);
            let reply =
                match crate::member::degrade_routed_reply(message, &state.prefix, multi_prefix) {
                    crate::member::Mediated::Rewritten(reduced) => reduced,
                    crate::member::Mediated::Pass | crate::member::Mediated::Withhold => {
                        message.clone()
                    }
                };
            rebuild_reply(&reply, route.downstream_label.as_deref())
        });
        let mut fans_out = matches!(outcome, RouteOutcome::Fanout);
        // A detached channel's live traffic is withheld from attached sessions while
        // still being applied to state and still ingested into durable history below.
        // The two are deliberately different decisions: hiding a channel from a local
        // client's view must not silently destroy what the bouncer recorded.
        let detached = if fans_out {
            detached_fanout(state, message)
        } else {
            DetachedFanout::Deliver
        };
        if matches!(detached, DetachedFanout::Suppress) {
            fans_out = false;
        }
        // Only used to address a CTCP reply when the sender carried no usable prefix.
        let nick_hint = self.snapshot.borrow().nick.clone().unwrap_or_default();
        match outcome {
            RouteOutcome::Fanout => {}
            RouteOutcome::Dropped => {
                // A correlated-looking reply that belongs to no live route. Delivering
                // it to anyone would hand one client's orphaned reply to another.
                self.snapshot.send_modify(|snapshot| {
                    snapshot.orphaned_replies_dropped =
                        snapshot.orphaned_replies_dropped.saturating_add(1)
                });
                return Ok(FanoutReport { desynchronized });
            }
            RouteOutcome::Continued(delivered) | RouteOutcome::Completed(delivered) => {
                // A session is removed from `sessions` on detachment, so a missing entry
                // means the client is already gone and its routes were dropped with it.
                if let Some(task) = sessions.get(&delivered.session)
                    && task.handle().fanout(delivered.line).is_err()
                {
                    desynchronized.push(delivered.session);
                    self.snapshot.send_modify(|snapshot| {
                        snapshot.fanout_dropped = snapshot.fanout_dropped.saturating_add(1)
                    });
                }
                self.snapshot
                    .send_modify(|snapshot| snapshot.response_routes = router.open_routes());
            }
        }
        // A `CAP` line is the bouncer's negotiation with the *server*, never traffic. It
        // is consumed here and never fanned out: relaying it would show a local client the
        // upstream's capability negotiation under the upstream's own prefix, which the
        // client would read as the server addressing it -- and which discloses the
        // upstream connection's shape to every attached Operator.
        //
        // The line still reached `state.apply_line` above, so membership and network state
        // are unaffected; a `CAP` line carries neither.
        if message.command.eq_ignore_ascii_case(b"CAP") {
            fans_out = false;
        }
        if fans_out {
            // CTCP is classified before it reaches any client. Only an ACTION is chat;
            // a PING query is answered by the bouncer itself rather than handed to a
            // client that would answer it with its own hostname and software; and every
            // metadata probe, DCC request and unknown command is suppressed entirely.
            match self.apply_inbound_ctcp(message, sessions, control_tx, &nick_hint) {
                InboundCtcp::Deliver => {}
                // The frame is consumed: answered privately, or suppressed. State has
                // already been applied and durable history still keeps the message it
                // actually carried, because this is a privacy decision about what a
                // client may observe, not about whether the event happened.
                InboundCtcp::Answered | InboundCtcp::Suppressed => {
                    return Ok(FanoutReport { desynchronized });
                }
            }
            // Tags are delivered per session, because the tag surface is negotiated per
            // session. A client that negotiated `message-tags` can parse them; a client
            // that did not must never receive one, since it has no way to read a frame
            // whose first bytes are a tag. A third form exists for `server-time`: a
            // session that negotiated the tag prefix but not that particular tag gets
            // every other tag and loses only `time`, because the others are exactly what
            // it asked for.
            let tag_forms = TagForms::build(message, raw);
            // A rewritten frame already has every detached channel removed from it, so it
            // replaces all three forms. Dropping its tags is sound: a tag is optional in
            // every direction, and the alternative would reintroduce a detached channel
            // name carried by a server-chosen tag value.
            let rewritten = match &detached {
                DetachedFanout::Rewritten(line) => Some(line.clone()),
                _ => None,
            };
            for (id, task) in sessions {
                let capabilities = task.handle().capabilities();
                // Member-state mediation runs per session and before the tag surface is
                // chosen, because it can withhold the frame or hand back a *different*
                // frame. Choosing the surface first would let a session be handed reduced
                // bytes carrying the tag set of the frame it no longer has.
                let reduced = match crate::member::mediate(state, message, &capabilities) {
                    crate::member::Mediated::Withhold => continue,
                    crate::member::Mediated::Rewritten(reduced) => Some(reduced),
                    crate::member::Mediated::Pass => None,
                };
                let surface = capabilities.tag_surface();
                let line = match &rewritten {
                    Some(line) => line.clone(),
                    // The unreduced frame already has all three forms built.
                    None if reduced.is_none() => match &tag_forms {
                        Some(forms) => forms.render(surface),
                        None => raw.to_vec(),
                    },
                    // A reduced frame carries its own tags, so its forms are built from it.
                    None => match TagForms::build(
                        reduced.as_ref().expect("reduced is Some in this arm"),
                        raw,
                    ) {
                        Some(forms) => forms.render(surface),
                        None => raw.to_vec(),
                    },
                };
                if task.handle().fanout(line).is_err() {
                    // Bounded fanout: the owner never awaits the session, so a stalled
                    // client cannot delay the Network or any other client. What it cannot
                    // do is keep the client: it has now skipped a live frame, so it is
                    // detached by the caller and counted here.
                    desynchronized.push(*id);
                    self.snapshot.send_modify(|snapshot| {
                        snapshot.fanout_dropped = snapshot.fanout_dropped.saturating_add(1)
                    });
                }
            }
        }
        // Only a message with a known buffer target is history-eligible. Eligibility
        // itself is decided by the journal, not here.
        let target = history_target(message);
        if let Some(buffer) = target.and_then(|target| buffers.get(target))
            && ingest_tx
                .try_send(IngestItem {
                    buffer: *buffer,
                    message: message.clone(),
                })
                .is_err()
        {
            // Bounded ingestion: a refused item is dropped and counted rather than
            // buffered. Retrying into an unbounded queue would be worse. This loss is
            // never reported to a client: durable history is not part of the live
            // stream's ordering guarantee.
            self.snapshot.send_modify(|snapshot| {
                snapshot.history_dropped = snapshot.history_dropped.saturating_add(1)
            });
        }
        Ok(FanoutReport { desynchronized })
    }

    /// Detaches one session that could not keep up with live upstream, and only that one.
    ///
    /// Its response routes go with it, so a reply that arrives after the gap can never
    /// be delivered into the middle of a stream the client believes is complete. The
    /// upstream connection, the Network and every other attachment continue untouched.
    async fn detach_overloaded(
        &self,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
        router: &mut ResponseRouter,
        session: SessionId,
    ) {
        if let Some(task) = sessions.remove(&session) {
            task.shutdown().await;
        }
        router.drop_session(session);
        let attached = sessions.len();
        let open_routes = router.open_routes();
        self.snapshot.send_modify(|snapshot| {
            snapshot.fanout_detached = snapshot.fanout_detached.saturating_add(1);
            snapshot.sessions_ended = snapshot.sessions_ended.saturating_add(1);
            snapshot.last_session_disposition = Some(DownstreamDisposition::QueueOverload.class());
            snapshot.attached_sessions = attached;
            snapshot.response_routes = open_routes;
        });
    }

    /// Applies one session event to network, durable, and upstream state.
    ///
    /// Returns `Err` only when this generation provably cannot converge to durable
    /// DesiredState in place, which the caller turns into a controlled restart. Every
    /// other failure is reported to the originating session and never to the Network.
    #[allow(clippy::too_many_arguments)]
    async fn handle_session_event(
        &self,
        event: SessionEvent,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
        state: &mut NetworkState,
        control_tx: &mpsc::Sender<Vec<u8>>,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        generation: ConnectionGeneration,
        journal: &mut crate::journal::HistoryJournal,
        buffers: &BTreeMap<String, BufferId>,
        router: &mut ResponseRouter,
        batches: &mut crate::ircv3::BatchTracker,
        search_batch: &mut u64,
        reconcile: &mut DesiredReconcile,
        upstream_caps: &UpstreamCapabilities,
        presence: &mut PresenceState,
        presence_of: &mut BTreeMap<SessionId, SessionPresence>,
    ) -> Result<(), RuntimeError> {
        match event {
            SessionEvent::Ended {
                session,
                disposition,
            } => {
                if let Some(task) = sessions.remove(&session) {
                    task.shutdown().await;
                }
                // A departing session stops counting towards active presence. Leaving
                // its classification behind would let a session that no longer exists
                // keep the Operator looking online.
                presence_of.remove(&session);
                // A detached client must never receive a reply that arrives later, even
                // if the server is slow. Dropping its routes is what guarantees that.
                router.drop_session(session);
                self.snapshot.send_modify(|snapshot| {
                    snapshot.sessions_ended = snapshot.sessions_ended.saturating_add(1);
                    snapshot.last_session_disposition = Some(disposition.class());
                    snapshot.attached_sessions = sessions.len();
                });
            }
            SessionEvent::Intent { session, intent } => {
                // An event naming an unknown or already-detached session is stale. It
                // must not create intents upstream, because the client it belonged to
                // no longer exists.
                if !sessions.contains_key(&session) {
                    return Ok(());
                }
                match intent {
                    // A bound session is still a local Operator's connection, so it can
                    // administrate the bouncer exactly as an unbound one can. The request
                    // goes to the controller's bounded queue; the owner performs it
                    // nowhere and owns nothing of it.
                    SessionIntent::Control { wire } => {
                        let Some(control) = self.control.clone() else {
                            self.notice_session(
                                sessions,
                                Some(session),
                                BOUNCER_PREFIX,
                                "this bouncer has no process control plane",
                            );
                            return Ok(());
                        };
                        let handle = match sessions.get(&session) {
                            Some(task) => task.handle().clone(),
                            // The session ended between the intent and here. Dropping the
                            // request is correct: there is nobody left to answer.
                            None => return Ok(()),
                        };
                        let nick = state.nick.clone();
                        let mut surface =
                            crate::control_session::ControlSurface::new(control, handle, nick);
                        surface.send_initial_batch().await;
                        surface.dispatch(&wire).await;
                        surface.publish_changes().await;
                    }
                    SessionIntent::RequestProjection => {
                        if let Some(task) = sessions.get(&session) {
                            let handle = task.handle();
                            // A client that negotiated the read-marker draft is owed
                            // its current marker per channel, in the projection, before
                            // RPL_ENDOFNAMES. A client that did not negotiate it is
                            // never sent a MARKREAD it did not ask for.
                            let read_markers = if handle.capabilities().manages_read_markers() {
                                Some(initial_read_markers(journal, buffers, state).await)
                            } else {
                                None
                            };
                            let _ = projection::project(
                                handle,
                                state,
                                &state.nick,
                                read_markers.as_ref(),
                            );
                        }
                        // Legacy automatic backlog runs only after the projection, so a
                        // client sees current state before retained history.
                        if let Some((handle, client)) = sessions
                            .get(&session)
                            .map(|task| (task.handle().clone(), task.client()))
                            && crate::playback::wants_backlog(handle.capabilities())
                        {
                            deliver_legacy_backlog(
                                journal,
                                &handle,
                                client,
                                session,
                                buffers,
                                &self.snapshot,
                                state,
                            )
                            .await;
                        }
                    }
                    SessionIntent::Quit => {
                        if let Some(task) = sessions.remove(&session) {
                            task.shutdown().await;
                            presence_of.remove(&session);
                        }
                        router.drop_session(session);
                        self.snapshot.send_modify(|snapshot| {
                            snapshot.sessions_ended = snapshot.sessions_ended.saturating_add(1);
                            snapshot.last_session_disposition =
                                Some(DownstreamDisposition::LocalDetach.class());
                            snapshot.attached_sessions = sessions.len();
                        });
                    }
                    SessionIntent::HistoryQuery { wire } => {
                        if let Some(task) = sessions.get(&session) {
                            answer_history_query(journal, task.handle(), &wire, buffers, batches)
                                .await;
                        }
                    }
                    SessionIntent::MarkerUpdate { wire } => {
                        answer_marker_update(journal, sessions, session, &wire, buffers).await;
                    }
                    SessionIntent::HistorySearch { wire } => {
                        if let Some(task) = sessions.get(&session) {
                            answer_history_search(
                                journal,
                                task.handle(),
                                &wire,
                                buffers,
                                search_batch,
                            )
                            .await;
                        }
                    }
                    SessionIntent::Forward { wire, class } => {
                        let class_label = class.as_str();
                        // Privacy mediation runs before anything is queued upstream and
                        // before any durable or diagnostic side effect, so a blocked
                        // frame is never transmitted, fanned out, or recorded as sent.
                        let wire = match self.mediate_client_frame(wire) {
                            Some(wire) => wire,
                            None => return Ok(()),
                        };
                        // A query whose reply the client expects to correlate is routed,
                        // not blindly forwarded: without a route its numeric replies
                        // would fan out to every attached client, disclosing one client's
                        // lookup to all of them.
                        if class == IntentClass::GenerationQuery {
                            self.forward_routed(
                                router,
                                upstream_caps,
                                &UpstreamAdmission {
                                    normal_tx,
                                    generation,
                                    sessions,
                                },
                                session,
                                wire,
                            );
                            return Ok(());
                        }
                        // The owner stamps the generation, so a session cannot forge a
                        // frame as belonging to a live generation.
                        //
                        // A refusal is a definite local failure, not a hiccup: the frame
                        // was rejected before admission, so it definitely was not
                        // accepted for upstream delivery and is never retried or
                        // replayed into a later generation. No response route survives a
                        // refusal because this path allocates none -- route allocation
                        // and send happen in the same owner turn, and nothing is opened
                        // for a frame that was not admitted.
                        if normal_tx
                            .try_send(OutboundIntent {
                                generation,
                                class,
                                wire,
                            })
                            .is_err()
                        {
                            self.report_upstream_overload(sessions, session, class_label);
                        }
                    }
                    SessionIntent::Join { channel } => {
                        // Persistence first: durable intent is committed before any
                        // upstream bytes exist, so a crash cannot lose the intent or
                        // leave an upstream JOIN with no durable record.
                        match self.store.add_desired_channel(self.network, &channel).await {
                            Ok(_) => {
                                if state.begin_desired_join(&channel)
                                    && queue_upstream(
                                        normal_tx,
                                        generation,
                                        &format!("JOIN {channel}\r\n"),
                                    )
                                    .is_err()
                                {
                                    // The commit stands and the join attempt stays
                                    // recorded; only the wire write was refused. The
                                    // bouncer now owes the Operator a live state that
                                    // matches what it already promised to store.
                                    if !reconcile.defer(channel.clone(), DesiredIntent::Join) {
                                        self.snapshot.send_modify(|snapshot| {
                                            snapshot.desired_reconcile_overflowed = snapshot
                                                .desired_reconcile_overflowed
                                                .saturating_add(1)
                                        });
                                        return Err(RuntimeError::QueueOverloaded);
                                    }
                                    self.snapshot.send_modify(|snapshot| {
                                        snapshot.desired_reconcile_pending = reconcile.pending.len()
                                    });
                                }
                            }
                            Err(error) => {
                                // Durable intent is unchanged and no upstream JOIN is
                                // written; the client is told the operation failed.
                                self.report_local_error(sessions, Some(session), &error);
                            }
                        }
                    }
                    SessionIntent::Part { channel } => {
                        match self
                            .store
                            .remove_desired_channel(self.network, &channel)
                            .await
                        {
                            Ok(_) => {
                                // Observed membership still changes only from an
                                // authoritative network event or generation loss.
                                if queue_upstream(
                                    normal_tx,
                                    generation,
                                    &format!("PART {channel}\r\n"),
                                )
                                .is_err()
                                    && !self.defer_desired(
                                        reconcile,
                                        channel.clone(),
                                        DesiredIntent::Part,
                                    )
                                {
                                    return Err(RuntimeError::QueueOverloaded);
                                }
                            }
                            Err(error) => self.report_local_error(sessions, Some(session), &error),
                        }
                    }
                    SessionIntent::Detach { channel } => {
                        self.apply_detach(sessions, state, Some(session), &channel)
                            .await;
                    }
                    SessionIntent::Passive => {
                        presence_of.insert(session, SessionPresence::Passive);
                    }
                    SessionIntent::Active => {
                        presence_of.insert(session, SessionPresence::Active);
                    }
                    SessionIntent::Away { text } => {
                        self.apply_manual_away(presence, text.as_deref());
                    }
                    SessionIntent::Reattach { channel } => {
                        self.apply_reattach(
                            sessions,
                            state,
                            Some(session),
                            &channel,
                            journal,
                            buffers,
                            normal_tx,
                            generation,
                            reconcile,
                        )
                        .await;
                    }
                }
                let _ = control_tx;
            }
        }
        Ok(())
    }

    /// Hides one desired channel from every attached session, durably.
    ///
    /// Order is not negotiable: the durable policy is committed first, and only a commit
    /// that certainly landed changes what clients are shown. That is what makes the
    /// policy survive a restart -- a restart reads the flag, not the live view.
    ///
    /// The membership itself is untouched. No upstream `PART` is written and no
    /// authoritative self event can be fabricated, so the bouncer stays in the room and
    /// keeps collecting history exactly as before.
    async fn apply_detach(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        state: &mut NetworkState,
        session: Option<SessionId>,
        channel: &str,
    ) {
        let outcome = self.policy.set_detached(self.network, channel, true).await;
        match outcome {
            Ok(true) => {
                if state.detach(channel) {
                    // One client detaching changes the Network's policy, so every
                    // attached session is told -- not just the one that asked. A client
                    // that stayed silent would otherwise keep a channel it can no longer
                    // see and no explanation for it disappearing.
                    self.announce_detach(sessions, channel);
                }
            }
            Ok(false) => {
                self.notice_session(sessions, session, BOUNCER_PREFIX, DETACH_NOT_DURABLE);
            }
            Err(error) if error.commit_state() == CommitState::Unknown => {
                // The durable effect is unknown. The flag is re-read rather than assumed
                // either way, and presentation follows durable state. Guessing here would
                // make what a client sees depend on which of two outcomes this process
                // happened to see first, and the other outcome is what a reconnect sees.
                if self.durable_detached(channel).await == Some(true) && state.detach(channel) {
                    self.announce_detach(sessions, channel);
                    self.notice_session(sessions, session, BOUNCER_PREFIX, DETACH_RECONCILED);
                } else {
                    self.notice_session(sessions, session, BOUNCER_PREFIX, DETACH_UNKNOWN);
                }
            }
            // A store that definitely refused has no new policy, so there is nothing to
            // present. Reporting it as a transition would show every client a channel
            // that is still going to be there on reconnect.
            Err(error) => self.report_local_error(sessions, session, &error),
        }
    }

    /// Restores downstream presentation of one desired channel, durably.
    ///
    /// When membership is already observed the session is given the full bounded
    /// projection, because a client cannot be shown a channel as joined without the
    /// topic, modes, and names that make that claim coherent. When membership is absent,
    /// the channel is joined upstream instead and projected once the server confirms it,
    /// so nothing is projected for a channel this bouncer does not actually hold.
    #[allow(clippy::too_many_arguments)]
    async fn apply_reattach(
        &self,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
        state: &mut NetworkState,
        session: Option<SessionId>,
        channel: &str,
        journal: &mut crate::journal::HistoryJournal,
        buffers: &BTreeMap<String, BufferId>,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        generation: ConnectionGeneration,
        reconcile: &mut DesiredReconcile,
    ) {
        let outcome = self.policy.set_detached(self.network, channel, false).await;
        // `true` means a row changed, which for this mutation means the flag was
        // cleared. Reading that as "still detached" would leave a successfully
        // reattached channel invisible, which is the opposite of what was asked for.
        let still_detached = match outcome {
            Ok(true) => false,
            Ok(false) => {
                self.notice_session(sessions, session, BOUNCER_PREFIX, REATTACH_NOT_DURABLE);
                return;
            }
            // Presentation follows the durable flag, not the outcome this process saw.
            // An unknown commit that landed means the channel is visible; one that did
            // not means it stays hidden.
            Err(error) if error.commit_state() == CommitState::Unknown => {
                let Some(detached) = self.durable_detached(channel).await else {
                    self.notice_session(sessions, session, BOUNCER_PREFIX, REATTACH_UNKNOWN);
                    return;
                };
                detached
            }

            Err(error) => {
                self.report_local_error(sessions, session, &error);
                return;
            }
        };
        if still_detached {
            // Durable state still says detached: nothing changed and nothing is shown.
            self.notice_session(sessions, session, BOUNCER_PREFIX, REATTACH_RECONCILED);
            return;
        }
        if !state.reattach(channel) {
            return;
        }
        if !Self::is_observed_member(state, channel) {
            // The server does not currently hold this channel for the bouncer. A JOIN is
            // written and the reveal is deferred to the authoritative self JOIN, so a
            // client is never told it is in a channel the bouncer has not joined.
            state.begin_desired_join(channel);
            if queue_upstream(normal_tx, generation, &format!("JOIN {channel}\r\n")).is_err()
                && !self.defer_desired(reconcile, channel.to_owned(), DesiredIntent::Join)
            {
                return;
            }
            return;
        }
        // Membership is already observed, so the reveal can be projected immediately.
        self.reveal_channel(sessions, state, channel, journal, buffers)
            .await;
        self.snapshot.send_modify(|snapshot| {
            snapshot.channels_reattached = snapshot.channels_reattached.saturating_add(1)
        });
    }

    /// Projects a newly reattached channel to every attached session.
    async fn reveal_channel(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        state: &NetworkState,
        channel: &str,
        journal: &mut crate::journal::HistoryJournal,
        buffers: &BTreeMap<String, BufferId>,
    ) {
        for task in sessions.values() {
            let handle = task.handle();
            // A synthetic JOIN precedes the projection so a client sees the same ordered
            // shape it saw when the channel was attached originally.
            if handle
                .queue_control(&projection::reattach_join_line(channel))
                .is_err()
            {
                continue;
            }
            let read_markers = if handle.capabilities().manages_read_markers() {
                Some(initial_read_markers(journal, buffers, state).await)
            } else {
                None
            };
            let _ = projection::project_channel(
                handle,
                state,
                &state.nick,
                channel,
                read_markers.as_ref(),
            );
        }
    }

    /// True when the server currently holds `channel` for this bouncer.
    fn is_observed_member(state: &NetworkState, channel: &str) -> bool {
        state
            .joined_channels()
            .iter()
            .any(|held| state.same_nick(held, channel))
    }

    /// Evaluates presence and returns the upstream `AWAY` frame a transition requires.
    ///
    /// Returns `None` when nothing changed, which is the common case: a session
    /// attaching, a projection running, or the same away state being re-derived must
    /// not produce upstream traffic. Only a genuine transition writes a frame.
    fn apply_presence(
        &self,
        presence: &mut PresenceState,
        presence_of: &BTreeMap<SessionId, SessionPresence>,
    ) -> Option<String> {
        let active = presence_of
            .values()
            .filter(|presence| presence.is_active())
            .count();
        let decided = presence.away_state_with_origin(active);
        let desired = decided.as_ref().map(|(text, _)| text.clone());
        self.snapshot.send_modify(|snapshot| {
            snapshot.active_sessions = active;
            snapshot.away = desired.clone();
            snapshot.away_origin = decided.map(|(_, origin)| origin);
        });
        presence.note_upstream_away(desired).map(|text| match text {
            // Returning to present is the protocol's bare `AWAY`, which carries no
            // parameter at all. `AWAY :` would put an empty message in front of every
            // member of every channel the bouncer holds, which is both ugly and a
            // different statement from "I am back".
            None => "AWAY\r\n".to_owned(),
            Some(text) if text.is_empty() => "AWAY\r\n".to_owned(),
            Some(text) => format!("AWAY :{text}\r\n"),
        })
    }

    /// Opens this generation's reclaim: `MONITOR` when the server offered a usable
    /// limit, bounded `ISON` probing when it did not.
    ///
    /// The strategy is chosen once per generation from what the server advertised rather
    /// than per pass, so a server that drops `MONITOR` mid-connection cannot make the
    /// bouncer alternate between two mechanisms on consecutive ticks.
    fn begin_reclaim(
        &self,
        attempt: &mut Option<ReclaimAttempt>,
        state: &NetworkState,
        control_tx: &mpsc::Sender<Vec<u8>>,
    ) {
        let Some(attempt) = attempt.as_mut() else {
            return;
        };
        let _ = state;
        match crate::presence::reclaim_strategy(state.monitor_limit()) {
            crate::presence::ReclaimStrategy::Monitor => {
                let _ = queue_control(control_tx, &format!("MONITOR + {}\r\n", attempt.preferred));
            }
            crate::presence::ReclaimStrategy::Probe => {
                let _ = queue_control(control_tx, &format!("ISON {}\r\n", attempt.preferred));
            }
        }
    }

    /// Reads one upstream line for reclaim evidence.
    ///
    /// The two replies mean opposite things, and reading them the same way is how a
    /// bouncer ends up convinced a free nick is taken:
    ///
    /// * `303` (RPL_ISON) lists the nicks that are **online**, so the preferred one being
    ///   *absent* from the answer is the evidence that it is free.
    /// * `730` (RPL_MONITOROFFLINE) names the nick that just went **offline**, so the
    ///   preferred one being *named* is the evidence that it is free.
    ///
    /// Only those two commands are evidence at all. Every other line -- including a
    /// `731` reporting the preferred nick on-line -- is recorded as nothing rather than
    /// as negative evidence that would suppress a future write.
    ///
    /// Accepted evidence wakes the reclaim clock rather than waiting out the interval. A
    /// server that says the preferred nick just came free has answered the question the
    /// bouncer is asking, and deferring the write to the next scheduled pass would lose a
    /// nick the bouncer already knows is available.
    fn note_reclaim_evidence(
        &self,
        state: &NetworkState,
        message: &Message,
        attempt: &mut Option<ReclaimAttempt>,
        wake: &tokio::sync::Notify,
    ) {
        let Some(attempt) = attempt.as_mut() else {
            return;
        };
        // Both replies are addressed to the bouncer, so the nick list starts after the
        // recipient parameter.
        let offline = if message.command.eq_ignore_ascii_case(b"730") {
            Some(true)
        } else if message.command.eq_ignore_ascii_case(b"303") {
            Some(false)
        } else {
            None
        };
        let Some(offline) = offline else {
            return;
        };
        let listed: Vec<String> = message
            .params
            .iter()
            .skip(1)
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .collect();
        if listed.is_empty() {
            return;
        }
        let preferred_listed = listed
            .iter()
            .any(|nick| state.same_nick(nick, &attempt.preferred));
        let free = if offline {
            preferred_listed
        } else {
            !preferred_listed
        };
        if free {
            attempt.evidence = true;
            wake.notify_one();
        }
    }

    /// One keep-nick reclaim pass, returning the upstream frame a write requires.
    ///
    /// The strategy is chosen once per generation from what the server advertised, not
    /// per pass, so a server that drops `MONITOR` mid-connection cannot make the
    /// bouncer alternate between two mechanisms on consecutive ticks.
    fn reclaim_tick(
        &self,
        attempt: &mut ReclaimAttempt,
        state: &NetworkState,
        elapsed: std::time::Duration,
    ) -> Option<String> {
        if state.nick.eq_ignore_ascii_case(&self.context.record.nick) {
            return None;
        }
        if !attempt.should_write(elapsed, crate::presence::RECLAIM_INTERVAL) {
            return None;
        }
        if !crate::presence::note_reclaim_write(
            attempt,
            crate::presence::MAX_RECLAIM_WRITES_PER_GENERATION,
        ) {
            self.snapshot.send_modify(|snapshot| {
                snapshot.last_error = Some("reclaim-write-ceiling");
            });
            return None;
        }
        Some(format!("NICK {}\r\n", attempt.preferred))
    }

    /// The owner-scoped presence state.
    ///
    /// A poisoned lock is recovered rather than propagated: presence is not authority,
    /// and losing the ability to answer "is the Operator away" would stop the bouncer
    /// doing anything at all over a panic in an unrelated generation.
    fn owner_presence(&self) -> std::sync::MutexGuard<'_, PresenceState> {
        self.presence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records the Operator's explicit away state on both the owner and the generation.
    ///
    /// Both copies exist deliberately: the owner copy survives a reconnect, the
    /// generation copy drives this connection's transitions.
    fn apply_manual_away(&self, presence: &mut PresenceState, text: Option<&str>) {
        let mut owner = self.owner_presence();
        match text {
            Some(text) => {
                owner.set_manual_away(text);
                presence.set_manual_away(text);
            }
            None => {
                owner.clear_manual_away();
                presence.clear_manual_away();
            }
        }
    }

    /// Emits the bouncer-owned `PART` that tells every session a channel was detached.
    ///
    /// The prefix is the bouncer's own reserved name, never the client's nick and never a
    /// person who is still in the room: nothing happened to anybody upstream, and a frame
    /// attributed to a real participant would say otherwise.
    fn announce_detach(&self, sessions: &BTreeMap<SessionId, SessionTask>, channel: &str) {
        let line = projection::detach_line(channel);
        for task in sessions.values() {
            let _ = task.handle().queue_control(&line);
        }
        self.snapshot.send_modify(|snapshot| {
            snapshot.channels_detached = snapshot.channels_detached.saturating_add(1)
        });
    }

    /// Re-reads one channel's durable presentation flag.
    ///
    /// Used only after a commit whose outcome the store could not determine. It reports
    /// `None` when the read itself failed, which the caller must treat as "still unknown"
    /// rather than as a policy.
    async fn durable_detached(&self, channel: &str) -> Option<bool> {
        let records = self.policy.load().await.ok()?;
        let record = records
            .iter()
            .find(|record| record.network == self.network)?;
        record
            .desired_channels
            .iter()
            .find(|entry| {
                i2pr_irc_core::Casemapping::Rfc1459.fold(entry.target.as_bytes())
                    == i2pr_irc_core::Casemapping::Rfc1459.fold(channel.as_bytes())
            })
            .map(|entry| entry.detached)
    }

    /// Reads this Network's durable channel policy for a fresh generation.
    ///
    /// A store that cannot answer falls back to the record this owner was built from.
    /// That is the last known durable intent, which is strictly better than joining
    /// nothing at all, and the generation still reports its own failure through the
    /// snapshot rather than silently pretending the read succeeded.
    async fn durable_desired_policy(&self) -> Vec<crate::state::DesiredChannelPolicy> {
        match self.policy.load().await {
            Ok(records) => records
                .iter()
                .find(|record| record.network == self.network)
                .map(|record| {
                    record
                        .desired_channels
                        .iter()
                        .map(|entry| crate::state::DesiredChannelPolicy {
                            target: entry.target.clone(),
                            detached: entry.detached,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => {
                self.snapshot
                    .send_modify(|snapshot| snapshot.last_error = Some(UNREADABLE_POLICY));
                self.context
                    .record
                    .desired_channels
                    .iter()
                    .map(|entry| crate::state::DesiredChannelPolicy {
                        target: entry.target.clone(),
                        detached: entry.detached,
                    })
                    .collect()
            }
        }
    }

    /// Writes one operator-notice line to a single session, when it is still attached.
    fn notice_session(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        session: Option<SessionId>,
        prefix: &str,
        text: &str,
    ) {
        if let Some(task) = session.and_then(|id| sessions.get(&id)) {
            let nick = self.snapshot.borrow().nick.clone().unwrap_or_default();
            let _ = task
                .handle()
                .queue_control(&format!(":{prefix} NOTICE {nick} :{text}\r\n"));
        }
    }

    /// Routes one client query upstream, translating its label, then admits the frame.
    ///
    /// Route allocation and admission happen in the same owner turn on purpose. If the
    /// upstream queue refuses the frame, the route that was just opened is cancelled in
    /// the same turn, so a frame that definitely never reached the server can never leave
    /// a route behind to capture a later reply that belongs to no one.
    fn forward_routed(
        &self,
        router: &mut ResponseRouter,
        upstream_caps: &UpstreamCapabilities,
        admission: &UpstreamAdmission<'_>,
        session: SessionId,
        wire: Vec<u8>,
    ) {
        let UpstreamAdmission {
            normal_tx,
            generation,
            sessions,
        } = admission;
        let message = match Message::parse(&wire) {
            Ok(message) => message,
            // The session reader already validated this frame, so an unparsable one
            // cannot reach here. Refusing it is still correct: nothing is opened.
            Err(_) => return,
        };
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        // The client's own label is extracted and then dropped: it is replaced by an
        // opaque generation-local token, and it is never forwarded to the server.
        let downstream_label = message
            .tags
            .get(LABEL_TAG.as_bytes())
            .and_then(|value| value.as_ref())
            .map(|value| String::from_utf8_lossy(value).to_ascii_lowercase());
        let params: Vec<String> = message
            .params
            .iter()
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .collect();
        let client = sessions
            .get(&session)
            .map(SessionTask::client)
            .unwrap_or(ClientId(session.0));
        let routed = router.route(
            RoutingRequest {
                session,
                client,
                command: &command,
                params: &params,
                downstream_label: downstream_label.as_deref(),
                labeled_upstream: upstream_caps.labels_available(),
            },
            std::time::Instant::now(),
        );
        let (line, upstream_label) = match routed {
            // Not a correlated family: the bouncer makes no claim about this reply, so
            // forwarding it untouched is correct and ordinary fanout applies later.
            Routed::Unlabeled => (wire, None),
            Routed::Frame {
                line,
                upstream_label,
            } => (line.into_bytes(), upstream_label),
            Routed::Refused(refusal) => {
                // Refused locally: nothing was opened and nothing was sent upstream.
                self.report_route_refusal(sessions, session, refusal);
                return;
            }
        };
        if normal_tx
            .try_send(OutboundIntent {
                generation: *generation,
                class: IntentClass::GenerationQuery,
                wire: line,
            })
            .is_err()
        {
            // The frame was not admitted, so the query definitively did not reach the
            // server. The route must not survive to claim someone else's later reply.
            match upstream_label {
                Some(label) => {
                    router.cancel_labeled(&label);
                }
                None => {
                    if let Some(class) = RequestClass::parse(&command) {
                        router.cancel_fallback(class);
                    }
                }
            }
            self.report_upstream_overload(sessions, session, "query");
        }
    }

    /// Tells one client a correlated query was refused before it reached the server.
    ///
    /// The text is fixed and says nothing about the request itself, so a refusal cannot
    /// disclose what the client asked or which other client is holding capacity.
    fn report_route_refusal(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        session: SessionId,
        refusal: RouteRefusal,
    ) {
        let nick = self.snapshot.borrow().nick.clone().unwrap_or_default();
        let reason = match refusal {
            RouteRefusal::Busy => "Bouncer is already tracking a similar request",
            RouteRefusal::Unsupported => "Bouncer cannot correlate that request",
        };
        if let Some(task) = sessions.get(&session) {
            let _ = task
                .handle()
                .queue_normal(&format!(":bouncer NOTICE {nick} :{reason}\r\n"));
        }
        self.snapshot
            .send_modify(|snapshot| snapshot.last_error = Some("route-refused"));
    }

    /// Applies the upstream-to-downstream CTCP policy to one frame.
    ///
    /// This is the boundary that keeps an anonymity network's anonymity from being undone by
    /// the client software sitting behind it. An upstream `CLIENTINFO` query, if fanned out,
    /// would cause every attached client to answer with its own hostname and version — and
    /// that answer is what identifies this Operator's client on a network where the address is
    /// supposed to be unlinkable.
    ///
    /// The reply to a `PING` is addressed to the sender's own nick so it returns privately
    /// rather than being observed by the rest of the channel.
    fn apply_inbound_ctcp(
        &self,
        message: &Message,
        sessions: &BTreeMap<SessionId, SessionTask>,
        control_tx: &mpsc::Sender<Vec<u8>>,
        nick: &str,
    ) -> InboundCtcp {
        let direction = match &message.command[..] {
            [b'N', b'O', b'T', b'I', b'C', b'E'] => crate::ctcp::CtcpDirection::Reply,
            _ => crate::ctcp::CtcpDirection::Query,
        };
        let ctcp = crate::ctcp::classify(message, direction);
        match crate::ctcp::inbound_action(&ctcp) {
            crate::ctcp::InboundAction::FanOut => InboundCtcp::Deliver,
            crate::ctcp::InboundAction::Suppress => {
                self.snapshot.send_modify(|snapshot| {
                    snapshot.ctcp_suppressed = snapshot.ctcp_suppressed.saturating_add(1)
                });
                InboundCtcp::Suppressed
            }
            crate::ctcp::InboundAction::AnswerPing(_) => {
                let Some(token) = crate::ctcp::ping_reply_text(&ctcp) else {
                    return InboundCtcp::Suppressed;
                };
                let target = message
                    .prefix
                    .as_deref()
                    .map(|prefix| {
                        let split = prefix
                            .iter()
                            .position(|byte| *byte == b'!')
                            .unwrap_or(prefix.len());
                        String::from_utf8_lossy(&prefix[..split]).into_owned()
                    })
                    .filter(|sender| !sender.is_empty())
                    .unwrap_or_else(|| nick.to_owned());
                let line = format!(":bouncer NOTICE {target} :\u{1}PING {token}\u{1}\r\n");
                // Queued to upstream, not to clients: the answer reveals the bouncer's own
                // fixed token and nothing about who is attached.
                if queue_control(control_tx, &line).is_err() {
                    self.snapshot.send_modify(|snapshot| {
                        snapshot.ctcp_suppressed = snapshot.ctcp_suppressed.saturating_add(1)
                    });
                    return InboundCtcp::Suppressed;
                }
                let _ = sessions;
                InboundCtcp::Answered
            }
        }
    }

    /// Applies the client-to-upstream privacy policy to one frame.
    ///
    /// Returns the bytes to forward, or `None` when the frame is blocked.
    ///
    /// Two independent policies apply here, and both run *before* the frame is queued,
    /// so a blocked frame is never transmitted upstream, never fanned out, and never
    /// recorded as sent:
    ///
    /// - **CTCP**: a metadata *reply* from a local client is how that client's software
    ///   and hostname would reach the upstream server and become this Operator's
    ///   fingerprint. `DCC` in any form is blocked outright. `ACTION` and `PING` are the
    ///   allowlist; everything else is denied by default.
    /// - **Tags**: client-supplied tags are denied by default. The one exception is the
    ///   response label, which is this bouncer's own correlation mechanism: it is
    ///   consumed by the router, translated to an opaque upstream token, and restored
    ///   only to the client that sent it. It never reaches the server.
    fn mediate_client_frame(&self, wire: Vec<u8>) -> Option<Vec<u8>> {
        let message = Message::parse(&wire).ok()?;
        let direction = match &message.command[..] {
            [b'N', b'O', b'T', b'I', b'C', b'E'] => crate::ctcp::CtcpDirection::Reply,
            _ => crate::ctcp::CtcpDirection::Query,
        };
        let ctcp = crate::ctcp::classify(&message, direction);
        if crate::ctcp::outbound_action(&ctcp) == crate::ctcp::OutboundAction::Block {
            self.snapshot.send_modify(|snapshot| {
                snapshot.client_frames_blocked = snapshot.client_frames_blocked.saturating_add(1)
            });
            return None;
        }
        // Tag mediation applies to every forwarded frame, not only chat, so a client
        // cannot smuggle a forged `msgid` onto a MODE or a NICK. It does not depend on
        // whether this client negotiated the tag surface: what a client asked for says
        // nothing about whether the tags it sent may be trusted.
        let (mediated, _) = crate::ircv3::mediate_client_tags(&message);
        mediated.encode().ok()
    }

    /// Tells one client its durable operation failed, without touching the Network.
    fn report_local_error(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        session: Option<SessionId>,
        error: &StoreError,
    ) {
        let nick = self.snapshot.borrow().nick.clone().unwrap_or_default();
        let line = format!(
            ":bouncer NOTICE {nick} :Bouncer could not persist that request ({})\r\n",
            error.commit_state().as_str()
        );
        if let Some(task) = session.and_then(|id| sessions.get(&id)) {
            let _ = task.handle().queue_normal(&line);
        }
        self.snapshot
            .send_modify(|snapshot| snapshot.last_error = Some("store-refused"));
    }

    /// Records one deferred desired-membership intent and publishes the pending depth.
    ///
    /// Returns false when the bounded set could not hold another entry. The durable
    /// intent is still correct in storage, so that is reported rather than dropped: the
    /// caller ends the generation, and the next one rebuilds membership from DesiredState.
    fn defer_desired(
        &self,
        reconcile: &mut DesiredReconcile,
        channel: String,
        intent: DesiredIntent,
    ) -> bool {
        if !reconcile.defer(channel, intent) {
            self.snapshot.send_modify(|snapshot| {
                snapshot.desired_reconcile_overflowed =
                    snapshot.desired_reconcile_overflowed.saturating_add(1);
            });
            return false;
        }
        let pending = reconcile.pending.len();
        self.snapshot
            .send_modify(|snapshot| snapshot.desired_reconcile_pending = pending);
        true
    }

    /// Tells one client its command was refused by the bounded upstream queue.
    ///
    /// The text is fixed and carries no client bytes and no secret: echoing the
    /// rejected command back would only invite injection, and the intent class is the
    /// whole of what the client needs to know. It goes on the control queue, because the
    /// normal queue is the thing that just demonstrated it is under pressure.
    fn report_upstream_overload(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        session: SessionId,
        class: &'static str,
    ) {
        self.snapshot.send_modify(|snapshot| {
            snapshot.upstream_rejected = snapshot.upstream_rejected.saturating_add(1);
            snapshot.last_error = Some("upstream-queue-refused");
        });
        let Some(task) = sessions.get(&session) else {
            return;
        };
        let nick = self.snapshot.borrow().nick.clone().unwrap_or_default();
        let _ = task.handle().queue_control(&format!(
            ":bouncer NOTICE {nick} :Bouncer could not accept that command for upstream delivery ({class} refused)\r\n"
        ));
    }
}

/// Stopping an owner removes it from process-wide accounting.
///
/// This is the only path that clears the ledger entry, so a campaign that starts and
/// stops Networks can assert the process returned to its baseline. It runs on drop rather
/// than at the end of `serve` because an owner abandoned before it ever served -- a
/// dropped task handle, an aborted supervisor -- must still stop being counted.
impl<P> Drop for NetworkOwner<P> {
    fn drop(&mut self) {
        self.resources.forget(self.network);
    }
}

/// What the bouncer did with one inbound CTCP frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InboundCtcp {
    /// Ordinary chat or an action: deliver as usual.
    Deliver,
    /// The bouncer answered a `PING` itself and delivered nothing to clients.
    Answered,
    /// Suppressed: a metadata probe, a DCC request, or an unknown command.
    Suppressed,
}

/// Current read marker per channel, for a client's initial MARKREAD set.
///
/// Only channels with a *known* marker appear, so an absent entry renders as the
/// draft's `*` sentinel rather than as a fabricated instant. The set is bounded by
/// observed membership, so this cannot grow with retained history.
async fn initial_read_markers(
    journal: &mut crate::journal::HistoryJournal,
    buffers: &BTreeMap<String, BufferId>,
    state: &NetworkState,
) -> BTreeMap<String, i2pr_irc_wire::IrcTimestamp> {
    let mut markers = BTreeMap::new();
    for channel in state.visible_channels() {
        let Some(buffer) = buffers.get(&casemapped(&channel)).copied() else {
            continue;
        };
        let Ok(Some(event)) = journal.read_marker(buffer).await else {
            continue;
        };
        // The durable marker is an event id. If that event can no longer be read back,
        // the marker is reported as unknown rather than as a guessed timestamp.
        if let Ok(Some(stamp)) = journal.event_timestamp(buffer, event).await {
            markers.insert(channel, stamp);
        }
    }
    markers
}

/// The bouncer's own reserved prefix for lines that describe bouncer policy rather than
/// an event that happened on the network.
const BOUNCER_PREFIX: &str = "bouncer";

/// Recorded when a generation could not re-read durable channel policy.
///
/// The generation falls back to the intent this owner was built from, so the marker says
/// which record was used rather than implying a channel was lost.
const UNREADABLE_POLICY: &str = "durable-channel-policy-unreadable";

/// Reported when a detach or reattach names a channel this Network does not hold.
///
/// The alternative is silence, and silence would leave a client believing its request had
/// been applied when nothing was stored at all.
const DETACH_NOT_DURABLE: &str =
    "Bouncer does not hold that channel as a desired channel on this network";
const REATTACH_NOT_DURABLE: &str =
    "Bouncer does not hold that channel as a desired channel on this network";
/// Reported when a commit's outcome is unknown and durable state could not be re-read.
const DETACH_UNKNOWN: &str = "Bouncer could not confirm the detach; channel state may follow";
const REATTACH_UNKNOWN: &str = "Bouncer could not confirm the reattach; channel state may follow";
/// Reported when an ambiguous commit was resolved by re-reading durable state and the
/// request had in fact taken effect.
const DETACH_RECONCILED: &str = "Bouncer re-read durable state; the channel is detached";
const REATTACH_RECONCILED: &str = "Bouncer re-read durable state; the channel was still detached";

/// What this generation's upstream write path accepts right now.
///
/// Grouped so a routed query is admitted through exactly the same queue and generation
/// fence an unrouted one uses; there is deliberately no second way in.
struct UpstreamAdmission<'a> {
    normal_tx: &'a mpsc::Sender<OutboundIntent>,
    generation: ConnectionGeneration,
    sessions: &'a BTreeMap<SessionId, SessionTask>,
}

/// Describes one upstream frame to the router, without re-parsing wire text.
///
/// A `BATCH` command carries its reference as a parameter rather than as a `batch` tag,
/// and the closing frame is unlabeled, so both forms are normalized here. That is what
/// lets the messages *inside* a labeled-response batch -- which carry only
/// `batch=<reference>` -- still reach the client that asked the question.
fn incoming_for(message: &Message) -> Incoming<'_> {
    let tag_text = |key: &str| {
        message
            .tags
            .get(key.as_bytes())
            .and_then(|value| value.as_deref())
            .and_then(|value| std::str::from_utf8(value).ok())
    };
    let command = message.command.as_slice();
    let numeric = (command.len() == 3 && command.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(command).ok())
        .flatten();
    if command.eq_ignore_ascii_case(b"BATCH") {
        let (reference, role) = match message.params.first().map(Vec::as_slice) {
            // `BATCH +<reference> [type]` opens; `BATCH -<reference>` closes.
            Some(reference) if reference.first() == Some(&b'+') => {
                (Some(&reference[1..]), BatchRole::Open)
            }
            Some(reference) if reference.first() == Some(&b'-') => {
                (Some(&reference[1..]), BatchRole::Close)
            }
            _ => (None, BatchRole::None),
        };
        return Incoming {
            label: tag_text(LABEL_TAG),
            batch: reference.and_then(|value| std::str::from_utf8(value).ok()),
            numeric: None,
            batch_role: role,
        };
    }
    Incoming {
        label: tag_text(LABEL_TAG),
        batch: tag_text("batch"),
        numeric,
        batch_role: BatchRole::None,
    }
}

/// Rebuilds one reply for the client that asked, restoring its original label.
///
/// The server's opaque label is always removed: it is generation-local bookkeeping that
/// means nothing downstream and would let one client observe another's routing state.
/// Every other tag is preserved, because a client that negotiated `labeled-response` has
/// already negotiated the message-tag surface those tags travel on.
/// The delivery forms of one frame, one per negotiated tag surface.
///
/// Built once per frame rather than once per session, because encoding a frame is the
/// expensive part and the forms depend only on the frame's tags.
#[derive(Clone, Debug)]
struct TagForms {
    tagged: Vec<u8>,
    untagged: Vec<u8>,
    /// `None` when the frame carried no `time` tag to withhold, which is the same
    /// situation as the `untagged` form.
    timed: Option<Vec<u8>>,
}
impl TagForms {
    /// Builds every form for one frame.
    ///
    /// Returns `None` when any form cannot be encoded, so a caller can fall back to the
    /// bytes it already has. Encoding is total rather than panicking: a frame that will
    /// not encode is a real condition, and taking the generation down over it would
    /// disconnect an Operator for something the bouncer can simply relay as received.
    fn build(message: &Message, raw: &[u8]) -> Option<Self> {
        if message.tags.is_empty() {
            // The untagged form of an untagged message *is* that message, re-encoded.
            // `raw` would be wrong here: a mediated frame carries different bytes from
            // the line it replaced, and handing the original back would undo the
            // mediation exactly when the mediation happened on a frame with no tags.
            let encoded = message.encode().ok().unwrap_or_else(|| raw.to_vec());
            return Some(Self {
                tagged: encoded.clone(),
                untagged: encoded,
                timed: None,
            });
        }
        let mut bare = message.clone();
        bare.tags.clear();
        let untagged = bare.encode().ok()?;
        let mut without_time = message.clone();
        without_time.tags.remove(i2pr_irc_wire::TIME_TAG);
        let timed = if message.tags.contains_key(i2pr_irc_wire::TIME_TAG)
            && !without_time.tags.is_empty()
        {
            Some(without_time.encode().ok()?)
        } else {
            None
        };
        Some(Self {
            tagged: message.encode().ok()?,
            untagged,
            timed,
        })
    }
    /// The form this session's negotiated surface calls for.
    fn render(&self, surface: TagSurface) -> Vec<u8> {
        match surface {
            TagSurface::All => self.tagged.clone(),
            // Withholding `time` only matters when a `time` tag was present to withhold.
            TagSurface::WithoutTime => self.timed.clone().unwrap_or_else(|| self.untagged.clone()),
            TagSurface::None => self.untagged.clone(),
        }
    }
}

fn rebuild_reply(message: &Message, downstream_label: Option<&str>) -> Vec<u8> {
    let mut rebuilt = message.clone();
    rebuilt.tags.remove(LABEL_TAG.as_bytes());
    if let Some(label) = downstream_label {
        rebuilt.tags.insert(
            LABEL_TAG.as_bytes().to_vec(),
            Some(label.as_bytes().to_vec()),
        );
    }
    // A label that cannot be re-encoded must never be dropped silently in favour of an
    // untagged line: that would look like a fresh reply to the client. The encoder only
    // fails on structurally invalid input, which parsing already rejected, so this is a
    // defensive fallback rather than an expected path.
    rebuilt
        .encode()
        .unwrap_or_else(|_| message.clone().encode().unwrap_or_default())
}

/// The conversation target a history-eligible line belongs to.
///
/// Only PRIVMSG and NOTICE carry history in this milestone. A status prefix (server or
/// nick!user@host) identifies a channel target; a bare nick is a direct-message peer.
fn history_target(message: &Message) -> Option<&str> {
    let command = &message.command;
    if !command.eq_ignore_ascii_case(b"PRIVMSG") && !command.eq_ignore_ascii_case(b"NOTICE") {
        return None;
    }
    let target = std::str::from_utf8(message.params.first()?).ok()?;
    let prefix = message
        .prefix
        .as_ref()
        .and_then(|prefix| std::str::from_utf8(prefix).ok())
        .map(|prefix| prefix.rsplit_once('!').map_or(prefix, |(_, rest)| rest))
        .unwrap_or(target);
    Some(if target.starts_with(['#', '&']) {
        target
    } else {
        prefix
    })
}

/// What to do with one upstream line under the current detached-channel policy.
#[derive(Debug, Eq, PartialEq)]
enum DetachedFanout {
    /// Deliver the line unchanged.
    Deliver,
    /// Deliver a frame from which every detached channel has been removed.
    Rewritten(Vec<u8>),
    /// Deliver nothing.
    Suppress,
}

/// Decides whether one upstream line may be presented to attached sessions.
///
/// The rule is that a line *about* a detached channel is withheld, and a line that
/// merely mentions one alongside visible channels is redacted rather than dropped. That
/// asymmetry matters: a `QUIT` listing several channels still concerns the visible ones,
/// so suppressing it whole would silently desynchronize them, while delivering it
/// unchanged would leak the detached channel's name. Redaction is the only answer that
/// keeps both promises.
///
/// A line about several detached and visible channels at once is withheld whole.
/// Over-suppressing is the safe direction: a client must never be shown a frame whose
/// meaning depends on a channel it is not allowed to see.
fn detached_fanout(state: &NetworkState, message: &Message) -> DetachedFanout {
    let command = &message.command;
    if command.eq_ignore_ascii_case(b"JOIN")
        || command.eq_ignore_ascii_case(b"PART")
        || command.eq_ignore_ascii_case(b"KICK")
        || command.eq_ignore_ascii_case(b"MODE")
        || command.eq_ignore_ascii_case(b"TOPIC")
        || command.eq_ignore_ascii_case(b"INVITE")
    {
        let Some(first) = message.params.first() else {
            return DetachedFanout::Deliver;
        };
        let Ok(first) = std::str::from_utf8(first) else {
            return DetachedFanout::Deliver;
        };
        // A target that is not a channel at all cannot be detached.
        if !is_channel_target(first) {
            return DetachedFanout::Deliver;
        }
        // Several channels at once: withhold the frame rather than part-rewrite a JOIN.
        let named: Vec<&str> = first.split(',').collect();
        let detached_any = named.iter().any(|name| state.is_detached(name));
        if detached_any {
            return DetachedFanout::Suppress;
        }
        if command.eq_ignore_ascii_case(b"JOIN") {
            // `JOIN` names channels in one comma-separated parameter; every name has to
            // be visible or the frame is withheld, which the check above already decided.
            return DetachedFanout::Deliver;
        }
        return DetachedFanout::Deliver;
    }
    if command.eq_ignore_ascii_case(b"PRIVMSG") || command.eq_ignore_ascii_case(b"NOTICE") {
        return match history_target(message) {
            Some(target) if is_channel_target(target) && state.is_detached(target) => {
                DetachedFanout::Suppress
            }
            _ => DetachedFanout::Deliver,
        };
    }
    if command.eq_ignore_ascii_case(b"QUIT") {
        return redact_quit_channels(state, message);
    }
    DetachedFanout::Deliver
}

/// Rebuilds a `QUIT` without naming any detached channel.
///
/// A `QUIT` whose trailing parameter listed only detached channels carries nothing left
/// to say to a client, so it is withheld. Tags are dropped because a server-chosen tag
/// value may itself name a channel; a client that negotiated `message-tags` treats tags
/// as optional and a frame without them is still well formed.
fn redact_quit_channels(state: &NetworkState, message: &Message) -> DetachedFanout {
    let Some(list) = message.params.last() else {
        return DetachedFanout::Deliver;
    };
    let Ok(list) = std::str::from_utf8(list) else {
        return DetachedFanout::Deliver;
    };
    let named: Vec<&str> = list.split(',').collect();
    let mut visible = Vec::with_capacity(named.len());
    let mut withheld = 0usize;
    for name in named {
        if is_channel_target(name) && state.is_detached(name) {
            withheld += 1;
            continue;
        }
        visible.push(name);
    }
    if withheld == 0 {
        return DetachedFanout::Deliver;
    }
    if visible.is_empty() {
        return DetachedFanout::Suppress;
    }
    let mut rebuilt = message.clone();
    rebuilt.tags.clear();
    if let Some(last) = rebuilt.params.last_mut() {
        *last = visible.join(",").into_bytes();
    }
    match rebuilt.encode() {
        Ok(line) => DetachedFanout::Rewritten(line),
        // A frame that cannot be re-encoded is not delivered in some other shape: the
        // original would leak the channel and a partial rewrite would lie about it.
        Err(_) => DetachedFanout::Suppress,
    }
}

/// True when this line names a channel rather than a direct-message peer.
fn is_channel_target(target: &str) -> bool {
    target.starts_with(['#', '&'])
}

/// Spawns one session task and records it under its ephemeral identity.
#[allow(clippy::too_many_arguments)]
fn attach_session<D: ByteStream + 'static>(
    session: SessionId,
    client: ClientId,
    nick: &str,
    stream: D,
    session_tx: &mpsc::Sender<SessionEvent>,
    sessions: &mut BTreeMap<SessionId, SessionTask>,
    snapshot: &watch::Sender<NetworkSnapshot>,
    advertisement: Vec<String>,
) {
    let task = SessionTask::spawn(session, client, nick.to_owned(), stream, session_tx.clone());
    // Set before the client can ask anything, so its very first `CAP LS` and the `005`
    // welcome come from one set rather than from the static default.
    task.handle().set_advertised(advertisement.clone());
    sessions.insert(session, task);
    snapshot.send_modify(|state| {
        state.sessions_accepted = state.sessions_accepted.saturating_add(1);
        state.attached_sessions = sessions.len();
    });
}

/// Adopts one client that admission already registered on this Network.
///
/// The transferred session keeps its socket, its writer task, its decoder, and its
/// negotiated capabilities: nothing is recreated, so the client never sees a second
/// connection or a second registration.
///
/// Two refusals are possible and both are explicit:
///
/// * the Network is already at its session ceiling, so accepting would exceed a bound;
/// * the claimed nickname does not fold-match the nickname this Network registered.
///   The binding that selected this Network may predate a configuration change, and a
///   session must never be projected under an identity this Network does not hold.
///
/// On acceptance the registration projection is requested through the ordinary intent
/// path rather than performed here. That is deliberate: there is exactly one
/// implementation of the projection, so an adopted client and an attached one cannot
/// diverge, and neither can be projected twice.
fn adopt_prepared_session(
    prepared: Box<crate::admission::PreparedSession>,
    expected_nick: &str,
    session_tx: &mpsc::Sender<SessionEvent>,
    sessions: &mut BTreeMap<SessionId, SessionTask>,
    snapshot: &watch::Sender<NetworkSnapshot>,
) -> Result<(), RuntimeError> {
    if sessions.len() >= MAX_SESSIONS_PER_NETWORK {
        return Err(RuntimeError::QueueOverloaded);
    }
    let claimed = match prepared.registered_nick() {
        Some(nick) => nick.to_owned(),
        // A transferred session has always completed registration. One that did not is
        // not a session, and admitting it would project a network to nobody.
        None => return Err(RuntimeError::Protocol),
    };
    if i2pr_irc_core::Casemapping::Rfc1459.fold(claimed.as_bytes())
        != i2pr_irc_core::Casemapping::Rfc1459.fold(expected_nick.as_bytes())
    {
        // The client is told, on its own socket, that its nickname is not available
        // here. Silently closing would be indistinguishable from a network fault, and
        // would leave a client that reconnected on the same stale selection with no way
        // to tell that its configuration is what changed.
        let _ = prepared.handle().queue_control(&format!(
            ":bouncer 433 {claimed} :Nickname unavailable on this network\r\n"
        ));
        return Err(RuntimeError::InvalidConfig);
    }
    let session = prepared.session_id();
    if sessions.contains_key(&session) {
        // Session identities are allocated by admission and never reused, so this can
        // only mean the same conversation was offered twice.
        return Err(RuntimeError::Protocol);
    }
    prepared.publish_negotiated();
    let task = SessionTask::resume(prepared.into_wiring(), session_tx.clone());
    // The transferred reader answers `CAP LS` and `REQ` from the cell it was seeded with
    // during admission, which is the unconditional surface: at admit time this Network's
    // upstream negotiation was not known. Replacing it here is what lets the very first
    // `CAP LS` a bound client sends name `echo-message`.
    //
    // Bound and dropped before anything else writes to the snapshot. `watch` shares one
    // lock between reads and writes, and this function writes below, so a borrow held
    // into that write would deadlock on itself.
    let advertised = snapshot.borrow().advertisement.clone();
    task.handle().set_advertised(advertised);
    sessions.insert(session, task);
    // The projection runs on the ordinary intent path. See the note above.
    let _ = session_tx.try_send(SessionEvent::Intent {
        session,
        intent: SessionIntent::RequestProjection,
    });
    snapshot.send_modify(|state| {
        state.sessions_accepted = state.sessions_accepted.saturating_add(1);
        state.attached_sessions = sessions.len();
    });
    Ok(())
}

fn queue_upstream(
    sender: &mpsc::Sender<OutboundIntent>,
    generation: ConnectionGeneration,
    line: &str,
) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(OutboundIntent {
            generation,
            class: IntentClass::DesiredState,
            wire: line.as_bytes().to_vec(),
        })
        .map_err(|_| RuntimeError::QueueOverloaded)
}

/// Waits for one owner command without spinning when none is available.
async fn next_attach(
    commands: &mut mpsc::Receiver<SupervisorCommand>,
) -> Option<SupervisorCommand> {
    commands.recv().await
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if stopped_now(stop) {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

fn stopped_now(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow()
}

async fn next_intent_frame(
    control: &mut mpsc::Receiver<Vec<u8>>,
    normal: &mut mpsc::Receiver<OutboundIntent>,
) -> Option<Result<OutboundIntent, Vec<u8>>> {
    tokio::select! { biased; command = control.recv() => command.map(Err), command = normal.recv() => command.map(Ok) }
}

async fn timeout_connection<T>(
    future: impl std::future::Future<Output = T>,
) -> Result<T, RuntimeError> {
    crate::timeout_bounded(CONNECT_TIMEOUT, future)
        .await
        .map_err(|_| RuntimeError::Timeout)
}

async fn send<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, s: &str) -> Result<(), std::io::Error> {
    crate::write_frame(w, s.as_bytes()).await
}

fn queue_control(sender: &mpsc::Sender<Vec<u8>>, line: &str) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(line.as_bytes().to_vec())
        .map_err(|_| RuntimeError::QueueOverloaded)
}

/// Writes one frame with a bounded deadline.
pub(crate) async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    crate::write_frame(w, bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation() -> ConnectionGeneration {
        ConnectionGeneration(1)
    }

    #[tokio::test]
    async fn a_deferred_intent_converges_once_the_queue_has_room_again() {
        let (tx, mut rx) = mpsc::channel::<OutboundIntent>(1);
        // Fill the queue so the first attempt is refused the way a real overload is.
        tx.try_send(OutboundIntent {
            generation: generation(),
            class: IntentClass::Control,
            wire: b"PING :busy\r\n".to_vec(),
        })
        .expect("fills the bounded queue");

        let mut reconcile = DesiredReconcile::default();
        assert!(
            reconcile.defer("#room".to_owned(), DesiredIntent::Join),
            "the first deferred intent is always recorded"
        );
        assert_eq!(
            reconcile.drain(&tx, generation()),
            0,
            "a full queue drains nothing"
        );
        assert_eq!(
            reconcile.pending.len(),
            1,
            "a refused intent is retained, never dropped"
        );

        rx.recv().await.expect("the blocking frame is consumed");
        assert_eq!(reconcile.drain(&tx, generation()), 1);
        assert!(reconcile.pending.is_empty());
        let intent = rx.recv().await.expect("the reconciled intent arrives");
        assert_eq!(intent.wire, b"JOIN #room\r\n");
        assert_eq!(intent.class, IntentClass::DesiredState);
    }

    #[tokio::test]
    async fn the_latest_committed_intent_for_a_channel_wins() {
        let (tx, mut rx) = mpsc::channel::<OutboundIntent>(8);
        let mut reconcile = DesiredReconcile::default();
        // The Operator joined, then parted, both committed. Replaying the JOIN would
        // contradict the commit that replaced it.
        assert!(reconcile.defer("#room".to_owned(), DesiredIntent::Join));
        assert!(reconcile.defer("#room".to_owned(), DesiredIntent::Part));
        assert_eq!(reconcile.pending.len(), 1);
        assert_eq!(reconcile.drain(&tx, generation()), 1);
        let intent = rx.recv().await.expect("intent arrives");
        assert_eq!(intent.wire, b"PART #room\r\n");

        // And the same in the other direction.
        let mut reconcile = DesiredReconcile::default();
        assert!(reconcile.defer("#other".to_owned(), DesiredIntent::Part));
        assert!(reconcile.defer("#other".to_owned(), DesiredIntent::Join));
        assert_eq!(reconcile.drain(&tx, generation()), 1);
        let intent = rx.recv().await.expect("intent arrives");
        assert_eq!(intent.wire, b"JOIN #other\r\n");
    }

    #[tokio::test]
    async fn the_reconciliation_set_is_bounded_at_its_ceiling() {
        let (_tx, _rx) = mpsc::channel::<OutboundIntent>(1);
        let mut reconcile = DesiredReconcile::default();
        for index in 0..MAX_DESIRED_RECONCILE {
            assert!(
                reconcile.defer(format!("#room{index}"), DesiredIntent::Join),
                "intent {index} is within the ceiling"
            );
        }
        assert!(
            !reconcile.defer("#overflow".to_owned(), DesiredIntent::Join),
            "a set past its ceiling refuses another intent rather than growing"
        );
        assert_eq!(reconcile.pending.len(), MAX_DESIRED_RECONCILE);
        // Re-deferring a channel already tracked is always allowed: it replaces its own
        // entry rather than consuming another slot.
        assert!(reconcile.defer("#room0".to_owned(), DesiredIntent::Part));
        assert_eq!(reconcile.pending.len(), MAX_DESIRED_RECONCILE);
    }

    #[tokio::test]
    async fn a_partly_refused_drain_keeps_remaining_entries_in_order() {
        let (tx, mut rx) = mpsc::channel::<OutboundIntent>(2);
        let mut reconcile = DesiredReconcile::default();
        for channel in ["#a", "#b", "#c"] {
            assert!(reconcile.defer(channel.to_owned(), DesiredIntent::Join));
        }
        // Room for two of three, so the drain stops partway rather than losing the tail.
        assert_eq!(reconcile.drain(&tx, generation()), 2);
        assert_eq!(rx.recv().await.expect("a").wire, b"JOIN #a\r\n");
        assert_eq!(rx.recv().await.expect("b").wire, b"JOIN #b\r\n");
        assert_eq!(
            reconcile.pending.keys().collect::<Vec<_>>(),
            ["#c"],
            "the refused entry is retained alone, in order"
        );
        assert_eq!(reconcile.drain(&tx, generation()), 1);
        assert_eq!(rx.recv().await.expect("c").wire, b"JOIN #c\r\n");
        assert!(reconcile.pending.is_empty());
    }

    #[tokio::test]
    async fn reconciliation_replays_channel_names_and_nothing_else() {
        let (tx, mut rx) = mpsc::channel::<OutboundIntent>(8);
        let mut reconcile = DesiredReconcile::default();
        assert!(reconcile.defer("#room".to_owned(), DesiredIntent::Join));
        assert!(reconcile.defer("#other".to_owned(), DesiredIntent::Part));
        assert!(reconcile.defer("#room".to_owned(), DesiredIntent::Part));
        assert_eq!(reconcile.drain(&tx, generation()), 2);
        assert!(reconcile.pending.is_empty());

        // Every frame reconciliation produced is a membership command. Nothing else can
        // reach this queue: a refused client command is reported to its own session and
        // never retained, precisely because it may not be idempotent.
        for index in 0..2 {
            let intent = rx.recv().await.expect("a reconciled intent arrives");
            let line = String::from_utf8_lossy(&intent.wire).into_owned();
            assert!(
                matches!(line.as_str(), "PART #room\r\n" | "PART #other\r\n"),
                "reconciliation may only carry membership commands, wrote {line:?}"
            );
            assert!(
                !line.contains("PRIVMSG") && !line.contains("NOTICE"),
                "user traffic must never be replayed through reconciliation: {line}"
            );
            assert_eq!(
                intent.class,
                IntentClass::DesiredState,
                "a reconciled intent is still classified as durable desired state"
            );
            let _ = index;
        }
        assert!(
            rx.try_recv().is_err(),
            "reconciliation emits nothing beyond the deferred membership intents"
        );
    }
}
