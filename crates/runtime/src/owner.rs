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
    projection,
    routing::ResponseRouter,
    session::{
        SESSION_EVENT_QUEUE_CAPACITY, SessionEvent, SessionHandle, SessionIntent, SessionTask,
    },
    state::{LineOutcome, NetworkState},
};
use i2pr_irc_core::{
    ByteStream, ClientId, ConnectionGeneration, I2pStreamProvider, NetworkId, SessionId,
};
use i2pr_irc_store::{BufferId, BufferKind, StoreError, StoreHandle};
use i2pr_irc_wire::{LineDecoder, Message, TagDirection};
use std::{collections::BTreeMap, time::Duration};
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

/// Delivers the bounded legacy backlog for every resolved buffer this session can see.
///
/// The whole set is capped in total events and bytes, so a client attached to many
/// channels receives a bounded amount rather than one full backlog per buffer.
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
    let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
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
            let _ = handle.queue_normal(&frame(crate::chathistory::render_failure(
                refusal,
                &command,
                first_param.as_deref(),
            )));
            return;
        }
    };

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
        let _ = handle.queue_normal(&frame(crate::chathistory::render_failure(
            crate::chathistory::HistoryRefusal::NoSuchBuffer,
            &command,
            Some(&target),
        )));
        return;
    };
    let buffer = *buffer;

    let reply = match crate::chathistory::execute(journal, buffer, &request).await {
        Ok(reply) => reply,
        Err(refusal) => {
            let _ = handle.queue_normal(&frame(crate::chathistory::render_failure(
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
            let _ = handle.queue_normal(&frame(crate::chathistory::render_failure(
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
        .recent_targets(older.unix_millis(), newer.unix_millis(), limit)
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
                Ok(event) => {
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
) -> PlaybackOutcome {
    let cap = crate::journal::BacklogCap::DEFAULT;
    let mut total = PlaybackOutcome::default();
    for buffer in buffers.values().copied().take(MAX_BACKLOG_BUFFERS) {
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
fn casemapped(target: &str) -> String {
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
    /// Observed membership only.
    pub channels: Vec<String>,
    pub reconnect_attempt: u32,
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

/// Owns one Network across connection generations and any number of local sessions.
pub struct NetworkOwner<P> {
    provider: P,
    network: NetworkId,
    context: crate::catalog::SupervisorContext,
    store: StoreHandle,
    snapshot: watch::Sender<NetworkSnapshot>,
}

impl<P: I2pStreamProvider> NetworkOwner<P> {
    pub fn new(
        provider: P,
        context: crate::catalog::SupervisorContext,
        store: StoreHandle,
    ) -> Result<Self, RuntimeError> {
        let (snapshot, _) = watch::channel(NetworkSnapshot::default());
        snapshot.send_modify(|state| {
            state.network = Some(context.network);
            state.phase = Some(Phase::Idle);
            state.nick = Some(context.record.nick.clone());
        });
        Ok(Self {
            provider,
            network: context.network,
            context,
            store,
            snapshot,
        })
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
        sessions: usize,
        router: &ResponseRouter,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        control_tx: &mpsc::Sender<Vec<u8>>,
    ) {
        let normal_depth = NORMAL_QUEUE_CAPACITY - normal_tx.capacity();
        let control_depth = CONTROL_QUEUE_CAPACITY - control_tx.capacity();
        let routes = router.open_routes();
        self.snapshot.send_if_modified(|snapshot| {
            if snapshot.attached_sessions == sessions
                && snapshot.upstream_normal_queue_depth == normal_depth
                && snapshot.upstream_control_queue_depth == control_depth
                && snapshot.response_routes == routes
            {
                return false;
            }
            snapshot.attached_sessions = sessions;
            snapshot.upstream_normal_queue_depth = normal_depth;
            snapshot.upstream_control_queue_depth = control_depth;
            snapshot.response_routes = routes;
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
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            let delay = backoff.next_delay(generation.wrapping_mul(0x9e3779b97f4a7c15));
            self.snapshot.send_modify(|state| {
                state.phase = Some(Phase::Backoff);
                state.attached_sessions = 0;
                state.reconnect_attempt = backoff.attempt;
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
        let mut state = NetworkState::new(
            &self.context.record.nick,
            &self.context.record.desired_channels,
        );
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
                    self.context.record.nick,
                    self.context.record.username,
                    self.context.record.realname
                ),
            )
            .await?;
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
                        "001" => welcomed = true,
                        "ERROR" | "464" | "465" | "451" => return Err(RuntimeError::Registration),
                        _ => match state.apply_line(&message) {
                            LineOutcome::Quiet => {}
                            LineOutcome::ReplyPong(token) => {
                                send(&mut uw, &format!("PONG :{token}\r\n")).await?;
                            }
                            LineOutcome::Malformed => return Err(RuntimeError::Protocol),
                        },
                    }
                }
            }
            // Desired state is re-sent only after a fresh registration, never carried
            // across a generation boundary. Writing a JOIN proves nothing about
            // membership: each attempt is recorded as outstanding and only an
            // authoritative self JOIN closes it as confirmed.
            for channel in self
                .context
                .record
                .desired_channels
                .iter()
                .take(crate::state::MAX_CHANNELS)
            {
                state.begin_desired_join(channel);
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
            );
        }

        let mut probe = tokio::time::interval(crate::LIVENESS_INTERVAL);
        probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                            &state,
                            &control_tx,
                            &normal_tx,
                            &session_tx,
                            &mut sessions,
                        ).await;
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
                        &mut reconcile,
                    )
                    .await
                    {
                        // The only error this path reports is durable DesiredState that
                        // this generation provably cannot converge to in place. A
                        // controlled reconnect rebuilds membership from storage, which
                        // is the honest way to honour intent the bounded set cannot hold.
                        break Err(error);
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
                        match self.apply_upstream_line(
                            &raw,
                            &message,
                            &mut state,
                            &sessions,
                            &control_tx,
                            &buffers,
                            &ingest_tx,
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
                    }
                    self.publish_state(&state);
                    if let Some(error) = failure { break Err(error); }
                }
            }
            // Published after the turn so the snapshot reflects settled state. The
            // upstream writer drains on its own task, so publishing before the turn
            // would always report the queue exactly as the turn found it.
            self.publish_gauges(sessions.len(), &router, &normal_tx, &control_tx);
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
        state: &NetworkState,
        control_tx: &mpsc::Sender<Vec<u8>>,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        session_tx: &mpsc::Sender<SessionEvent>,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
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
                    );
                    Ok(())
                };
                let _ = reply.send(accepted);
            }
            SupervisorCommand::Session { session, event } => {
                // A command naming an unknown session is stale and is dropped: it must
                // never be applied to whatever session now holds that identity slot.
                if sessions.contains_key(&session) {
                    let _ = session_tx.try_send(event);
                }
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
    ) -> Result<FanoutReport, RuntimeError> {
        let mut desynchronized = Vec::new();
        match state.apply_line(message) {
            LineOutcome::Quiet => {}
            LineOutcome::ReplyPong(token) => {
                queue_control(control_tx, &format!("PONG :{token}\r\n"))?;
            }
            LineOutcome::Malformed => return Err(RuntimeError::Protocol),
        }
        // Message-tag semantics are not advertised downstream yet, so tags are removed
        // rather than forwarded under a capability the client did not negotiate.
        let outgoing = if message.tags.is_empty() {
            raw.to_vec()
        } else {
            let mut untagged = message.clone();
            untagged.tags.clear();
            untagged.encode().map_err(|_| RuntimeError::Protocol)?
        };
        for (id, task) in sessions {
            if task.handle().fanout(outgoing.clone()).is_err() {
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
        reconcile: &mut DesiredReconcile,
    ) -> Result<(), RuntimeError> {
        match event {
            SessionEvent::Ended {
                session,
                disposition,
            } => {
                if let Some(task) = sessions.remove(&session) {
                    task.shutdown().await;
                }
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
                            )
                            .await;
                        }
                    }
                    SessionIntent::Quit => {
                        if let Some(task) = sessions.remove(&session) {
                            task.shutdown().await;
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
                    SessionIntent::Forward { wire, class } => {
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
                        let class_label = class.as_str();
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
                                self.report_local_error(sessions, session, &error);
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
                            Err(error) => self.report_local_error(sessions, session, &error),
                        }
                    }
                }
                let _ = control_tx;
            }
        }
        Ok(())
    }

    /// Tells one client its durable operation failed, without touching the Network.
    fn report_local_error(
        &self,
        sessions: &BTreeMap<SessionId, SessionTask>,
        session: SessionId,
        error: &StoreError,
    ) {
        let nick = self.snapshot.borrow().nick.clone().unwrap_or_default();
        let line = format!(
            ":bouncer NOTICE {nick} :Bouncer could not persist that request ({})\r\n",
            error.commit_state().as_str()
        );
        if let Some(task) = sessions.get(&session) {
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
    for channel in state.joined_channels() {
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

/// True when this line names a channel rather than a direct-message peer.
fn is_channel_target(target: &str) -> bool {
    target.starts_with(['#', '&'])
}

/// Spawns one session task and records it under its ephemeral identity.
fn attach_session<D: ByteStream + 'static>(
    session: SessionId,
    client: ClientId,
    nick: &str,
    stream: D,
    session_tx: &mpsc::Sender<SessionEvent>,
    sessions: &mut BTreeMap<SessionId, SessionTask>,
    snapshot: &watch::Sender<NetworkSnapshot>,
) {
    let task = SessionTask::spawn(session, client, nick.to_owned(), stream, session_tx.clone());
    sessions.insert(session, task);
    snapshot.send_modify(|state| {
        state.sessions_accepted = state.sessions_accepted.saturating_add(1);
        state.attached_sessions = sessions.len();
    });
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
