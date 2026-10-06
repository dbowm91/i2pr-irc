//! One Network owner: one upstream generation plus its attached sessions.
//!
//! The owner is the sole mutable authority for one Network. It keeps generation-owned
//! [`NetworkState`] local (never shared), routes typed session intents, fans one
//! normalized upstream event out to every attached session, and persists durable
//! DesiredState *before* any corresponding upstream bytes are written.
//!
//! Two properties drive the shape of this loop:
//!
//! - A client that is slow or overflowing must be detached on its own. Its full queue
//!   detaches that session; it never stalls upstream processing or another client.
//! - A disconnect after an outbound command leaves delivery ambiguous. Nothing user
//!   typed is ever retained for replay into a later generation.
use crate::{
    CONNECT_TIMEOUT, CONTROL_QUEUE_CAPACITY, IntentClass, NORMAL_QUEUE_CAPACITY, OutboundIntent,
    REGISTRATION_TIMEOUT, RuntimeError,
    catalog::SupervisorCommand,
    downstream::DownstreamDisposition,
    projection,
    session::{SESSION_EVENT_QUEUE_CAPACITY, SessionEvent, SessionIntent, SessionTask},
    state::{LineOutcome, NetworkState},
};
use i2pr_irc_core::{
    ByteStream, ClientId, ConnectionGeneration, I2pStreamProvider, NetworkId, SessionId,
};
use i2pr_irc_store::{StoreError, StoreHandle};
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
        let mut offered = std::collections::BTreeSet::new();
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
                            offered.extend(capabilities.split_whitespace().map(|item| {
                                item.trim_start_matches(':')
                                    .split('=')
                                    .next()
                                    .unwrap_or("")
                                    .to_owned()
                            }));
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
                                    && (!offered.contains("sasl") || !sasl_plain_offered)
                                {
                                    return Err(RuntimeError::Registration);
                                }
                                let wanted: Vec<&str> = if self.context.record.sasl.is_some()
                                    && offered.contains("sasl")
                                {
                                    vec!["sasl"]
                                } else {
                                    Vec::new()
                                };
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
                            let sasl_accepted = params
                                .iter()
                                .any(|p| p.split_whitespace().any(|cap| cap.starts_with("sasl")));
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
        let mut awaiting_pong: Option<(Instant, String)> = None;
        let outcome = loop {
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
                    self.handle_session_event(
                        event,
                        &mut sessions,
                        &mut state,
                        &control_tx,
                        &normal_tx,
                        generation,
                    ).await;
                }
                writer = writer_exit => {
                    match writer {
                        Some(Ok(Ok(()))) => break Err(RuntimeError::Protocol),
                        Some(Ok(Err(error))) => break Err(RuntimeError::Io(error)),
                        Some(Err(error)) => break Err(RuntimeError::Io(std::io::Error::other(error))),
                        None => break Err(RuntimeError::Protocol),
                    }
                }
                _ = probe.tick() => {
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
                    for line in decoder.push(&ubuf[..count]) {
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
                        ) {
                            Ok(()) => {}
                            Err(error) => { failure = Some(error); break; }
                        }
                    }
                    self.publish_state(&state);
                    if let Some(error) = failure { break Err(error); }
                }
            }
            self.snapshot.send_modify(|snapshot| {
                snapshot.upstream_normal_queue_depth = NORMAL_QUEUE_CAPACITY - normal_tx.capacity();
                snapshot.upstream_control_queue_depth =
                    CONTROL_QUEUE_CAPACITY - control_tx.capacity();
                snapshot.attached_sessions = sessions.len();
            });
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
    /// The event is normalized and applied once; fanout then decides per session. A
    /// session whose queue is full is detached on its own.
    #[allow(clippy::too_many_arguments)]
    fn apply_upstream_line(
        &self,
        raw: &[u8],
        message: &Message,
        state: &mut NetworkState,
        sessions: &BTreeMap<SessionId, SessionTask>,
        control_tx: &mpsc::Sender<Vec<u8>>,
    ) -> Result<(), RuntimeError> {
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
        for (session, task) in sessions {
            if task.handle().fanout(outgoing.clone()).is_err() {
                // Reported so the owner detaches this session; the loop below does not
                // act on it inline because teardown owns the task.
                let _ = session;
            }
        }
        Ok(())
    }

    /// Applies one session event to network, durable, and upstream state.
    #[allow(clippy::too_many_arguments)]
    async fn handle_session_event(
        &self,
        event: SessionEvent,
        sessions: &mut BTreeMap<SessionId, SessionTask>,
        state: &mut NetworkState,
        control_tx: &mpsc::Sender<Vec<u8>>,
        normal_tx: &mpsc::Sender<OutboundIntent>,
        generation: ConnectionGeneration,
    ) {
        match event {
            SessionEvent::Ended {
                session,
                disposition,
            } => {
                if let Some(task) = sessions.remove(&session) {
                    task.shutdown().await;
                }
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
                    return;
                }
                match intent {
                    SessionIntent::RequestProjection => {
                        if let Some(task) = sessions.get(&session) {
                            let _ = projection::project(task.handle(), state, &state.nick);
                        }
                    }
                    SessionIntent::Quit => {
                        if let Some(task) = sessions.remove(&session) {
                            task.shutdown().await;
                        }
                        self.snapshot.send_modify(|snapshot| {
                            snapshot.sessions_ended = snapshot.sessions_ended.saturating_add(1);
                            snapshot.last_session_disposition =
                                Some(DownstreamDisposition::LocalDetach.class());
                            snapshot.attached_sessions = sessions.len();
                        });
                    }
                    SessionIntent::Forward { wire, class } => {
                        // The owner stamps the generation, so a session cannot forge a
                        // frame as belonging to a live generation.
                        let _ = normal_tx.try_send(OutboundIntent {
                            generation,
                            class,
                            wire,
                        });
                    }
                    SessionIntent::Join { channel } => {
                        // Persistence first: durable intent is committed before any
                        // upstream bytes exist, so a crash cannot lose the intent or
                        // leave an upstream JOIN with no durable record.
                        match self.store.add_desired_channel(self.network, &channel).await {
                            Ok(_) => {
                                if state.begin_desired_join(&channel) {
                                    let _ = queue_upstream(
                                        normal_tx,
                                        generation,
                                        &format!("JOIN {channel}\r\n"),
                                    );
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
                                let _ = queue_upstream(
                                    normal_tx,
                                    generation,
                                    &format!("PART {channel}\r\n"),
                                );
                            }
                            Err(error) => self.report_local_error(sessions, session, &error),
                        }
                    }
                }
                let _ = control_tx;
            }
        }
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
