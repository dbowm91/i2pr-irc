//! One attached local IRC client, owned as an independent task.
//!
//! A session is a disposable view. It owns its decoder, CAP/registration state, and
//! output queues, and it never touches network or durable state directly: every
//! decision that needs observed state, a generation stamp, or persisted intent is
//! asked of the network owner. That is what lets many sessions share one Network
//! without a shared mutable `NetworkState`.
//!
//! Identity is deliberately split. [`SessionId`] names this one live attachment and is
//! never persisted; [`ClientId`] names the durable lineage that owns playback state. A
//! client that reconnects gets a fresh `SessionId`, so a late result scoped to a
//! previous attachment can never reach its replacement.
use crate::{
    CONTROL_QUEUE_CAPACITY, IntentClass, NORMAL_QUEUE_CAPACITY, RuntimeError,
    downstream::DownstreamDisposition,
};
use i2pr_irc_core::{ByteStream, ClientId, SessionId};
use i2pr_irc_wire::{LineDecoder, Message, TagDirection};
use std::{collections::VecDeque, io};
use tokio::{
    io::{AsyncReadExt, ReadHalf},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

/// Maximum complete tagged line accepted from one local client.
pub const MAX_CLIENT_LINE: usize = i2pr_irc_wire::MAX_TAGGED_LINE_BYTES;
/// Maximum decoded lines one read may yield before the session is treated as overloaded.
const MAX_LINES_PER_READ: usize = 64;
/// Bounded per-session intent queue. Many sessions share one owner, so this ceiling
/// also bounds how much a single client can make the owner do.
pub const SESSION_EVENT_QUEUE_CAPACITY: usize = 64;

/// What one session asks the network owner to do.
///
/// The variant set is closed on purpose: a session submits typed intents rather than
/// mutating network or durable state, so the owner stays the single place where
/// persistence order, generation fencing, and fanout are decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionIntent {
    /// A line the client sent that the bouncer forwards upstream. The owner stamps
    /// the current generation, so a session cannot forge one.
    Forward { wire: Vec<u8>, class: IntentClass },
    /// The client asked to join a channel durably.
    Join { channel: String },
    /// The client asked to leave a channel durably.
    Part { channel: String },
    /// The client completed registration and wants the current projection.
    RequestProjection,
    /// The client asked the bouncer itself for retained history.
    ///
    /// The bouncer owns the store, so a session cannot answer this: it submits the
    /// request and the owner executes it against the journal. The session never holds
    /// a store handle, which is what keeps one live owner per Network.
    HistoryQuery {
        /// The original framed command, re-parsed by the draft adapter.
        wire: Vec<u8>,
    },
    /// The client asked to read or advance a read marker.
    MarkerUpdate {
        /// The original framed command, re-parsed by the draft adapter.
        wire: Vec<u8>,
    },
    /// The client asked to end its own session.
    Quit,
}

/// What a session reports upward.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    /// One typed request from this session.
    Intent {
        session: SessionId,
        intent: SessionIntent,
    },
    /// The session ended; only this client is affected.
    Ended {
        session: SessionId,
        disposition: DownstreamDisposition,
    },
}

/// Per-attachment capability flags the bouncer honors when serving that client.
///
/// `legacy_backlog` is reserved now so a client that later negotiates `chathistory`
/// can suppress automatic backlog without changing the session's structure. Until
/// M003-E no client negotiates it, so every session currently receives a backlog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCapabilities {
    /// Deliver an automatic bounded backlog after registration.
    pub legacy_backlog: bool,
    /// The client will fetch history itself, so automatic backlog is suppressed.
    pub explicit_history: bool,
    /// The client negotiated `draft/read-marker`, so the bouncer owes it the server
    /// side of that draft: the initial marker after JOIN, and marker updates made by
    /// the Operator's other sessions.
    ///
    /// This is tracked separately from `explicit_history` because the two drafts are
    /// independent: a client may negotiate either, both, or neither.
    pub read_markers: bool,
    /// The client negotiated `message-tags`, so tags may be forwarded to it.
    ///
    /// A client that did not negotiate it must never receive a tagged frame: it has no
    /// way to parse one, and forwarding anyway would be sending a message it cannot
    /// read. This is tracked per session because the fanout path delivers one upstream
    /// line to many sessions with different surfaces.
    pub message_tags: bool,
}
impl Default for SessionCapabilities {
    fn default() -> Self {
        Self {
            legacy_backlog: true,
            explicit_history: false,
            read_markers: false,
            message_tags: false,
        }
    }
}
impl SessionCapabilities {
    /// True when this session should receive an automatic backlog.
    ///
    /// A client that negotiated `chathistory` manages its own history, so giving it
    /// the automatic backlog too would deliver the same messages twice.
    pub fn wants_backlog(&self) -> bool {
        self.legacy_backlog && !self.explicit_history
    }

    /// True when this client negotiated the history capability and will fetch its own
    /// history, so the bouncer must both suppress the legacy backlog and advertise the
    /// history ISUPPORT tokens.
    pub fn manages_own_history(&self) -> bool {
        self.explicit_history
    }

    /// True when this client negotiated `draft/read-marker` and therefore expects the
    /// bouncer to behave as that draft's server.
    pub fn manages_read_markers(&self) -> bool {
        self.read_markers
    }

    /// True when this client negotiated the message-tag surface.
    pub fn negotiated_tags(&self) -> bool {
        self.message_tags
    }

    /// Applies a client's successful `CAP REQ`, recording that it manages history.
    pub fn with_negotiated(&self, enabled: &std::collections::BTreeSet<String>) -> Self {
        Self {
            legacy_backlog: self.legacy_backlog,
            explicit_history: self.explicit_history
                || crate::chathistory::session_manages_history(enabled),
            read_markers: self.read_markers || crate::chathistory::session_manages_markers(enabled),
            message_tags: self.message_tags
                || enabled
                    .iter()
                    .any(|name| name == crate::capability::MESSAGE_TAGS),
        }
    }
}

/// Handle the owner uses to reach one session.
///
/// The owner holds only bounded routing metadata, never a stream or mutable session
/// state, so the number of attached clients cannot widen the owner's own state.
pub struct SessionHandle {
    session: SessionId,
    client: ClientId,
    control_tx: mpsc::Sender<crate::downstream::QueuedFrame>,
    normal_tx: mpsc::Sender<crate::downstream::QueuedFrame>,
    /// Negotiated-capability view, shared with the owner across the session.
    capabilities: std::sync::Arc<std::sync::Mutex<SessionCapabilities>>,
}

impl Clone for SessionHandle {
    /// Clones the routing handles; the negotiated-capability view is shared, not
    /// copied, so the owner and the session reader never disagree about what a
    /// client negotiated.
    fn clone(&self) -> Self {
        Self {
            session: self.session,
            client: self.client,
            control_tx: self.control_tx.clone(),
            normal_tx: self.normal_tx.clone(),
            capabilities: self.capabilities.clone(),
        }
    }
}

impl SessionHandle {
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Records the capabilities this client negotiated, for owner-side decisions.
    ///
    /// The owner needs this to decide whether a client that can fetch its own history
    /// should still receive the automatic legacy backlog.
    pub fn set_negotiated(&self, enabled: &std::collections::BTreeSet<String>) {
        if let Ok(mut capabilities) = self.capabilities.lock() {
            *capabilities = capabilities.with_negotiated(enabled);
        }
    }
    pub fn client(&self) -> ClientId {
        self.client
    }
    pub fn queue_depths(&self) -> (usize, usize) {
        (
            NORMAL_QUEUE_CAPACITY - self.normal_tx.capacity(),
            CONTROL_QUEUE_CAPACITY - self.control_tx.capacity(),
        )
    }
    /// Queues a control frame. Control traffic outranks user traffic, so a saturated
    /// normal queue can never delay a keepalive answer.
    pub fn queue_control(&self, line: &str) -> Result<(), RuntimeError> {
        queue_line(&self.control_tx, line)
    }
    /// Queues one already-framed normal line.
    pub fn queue_normal(&self, line: &str) -> Result<(), RuntimeError> {
        queue_line(&self.normal_tx, line)
    }
    /// Queues one normalized upstream line for fanout.
    ///
    /// A refusal is reported rather than swallowed, and it means the session is
    /// desynchronized: a downstream IRC stream is ordered, so once a live frame is
    /// skipped the bouncer can no longer claim this client is in step with upstream.
    /// The owner detaches that one session. It never blocks, so a slow client cannot
    /// stall the Network or any other client.
    pub fn fanout(&self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        self.normal_tx
            .try_send(crate::downstream::QueuedFrame::Fire(bytes))
            .map_err(|_| RuntimeError::QueueOverloaded)
    }
    /// This attachment's negotiated capabilities.
    pub fn capabilities(&self) -> SessionCapabilities {
        self.capabilities
            .lock()
            .map(|guard| *guard)
            .unwrap_or_default()
    }
    pub fn with_capabilities(mut self, capabilities: SessionCapabilities) -> Self {
        self.capabilities = std::sync::Arc::new(std::sync::Mutex::new(capabilities));
        self
    }
}

/// Owns one attached client's read half and reports its intents.
///
/// The task is always joined or aborted by its owner, so no client work can outlive
/// its session.
pub struct SessionTask {
    handle: SessionHandle,
    reader: JoinHandle<()>,
    writer: crate::downstream::SessionWriter,
}

impl SessionTask {
    /// Splits `stream`, spawns the reader and writer tasks, and returns the owned
    /// session together with the handle its owner will route with.
    ///
    /// `expected_nick` is the nickname this network registered. A session may only
    /// claim that nickname, and the owner re-validates registration before projecting,
    /// so a session cannot grant itself a view of a network it did not join.
    pub fn spawn<D: ByteStream + 'static>(
        session: SessionId,
        client: ClientId,
        expected_nick: String,
        stream: D,
        events_tx: mpsc::Sender<SessionEvent>,
    ) -> Self {
        let wiring = ClientWiring::new(session, client, Some(expected_nick), stream);
        Self::resume(wiring, events_tx)
    }

    /// Continues an accepted connection whose registration already completed.
    ///
    /// The wiring carries the socket half, the decoder, and the negotiated
    /// capabilities that admission already established. `resume` adds exactly one
    /// thing: a task that forwards this client's remaining intents to `events_tx`.
    /// It does not re-read registration, re-parse, re-negotiate, or rebuild the writer,
    /// so a client that joined a Network mid-stream is not registered twice.
    pub fn resume<D: ByteStream + 'static>(
        wiring: ClientWiring<D>,
        events_tx: mpsc::Sender<SessionEvent>,
    ) -> Self {
        let ClientWiring {
            mut reader,
            handle,
            writer,
        } = wiring;
        let identity = reader.session;
        let owner_handle = handle.clone();
        let task = tokio::spawn(async move {
            let disposition = reader.run(&events_tx).await;
            // The owner is told why this session ended so it can update diagnostics
            // and decide whether the upstream generation is affected.
            let _ = events_tx
                .send(SessionEvent::Ended {
                    session: identity,
                    disposition,
                })
                .await;
        });
        Self {
            handle: owner_handle,
            reader: task,
            writer,
        }
    }

    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }
    pub fn session(&self) -> SessionId {
        self.handle.session()
    }
    pub fn client(&self) -> ClientId {
        self.handle.client()
    }
    /// Aborts the reader and shuts the writer down, so neither task outlives the
    /// session. Both are owned here rather than detached.
    pub async fn shutdown(self) {
        self.reader.abort();
        let _ = self.reader.await;
        self.writer.shutdown().await;
    }
}

/// One accepted local connection, owned but not yet attached to a Network.
///
/// Admission builds this the moment a client connects, before anything is known about
/// which Network it will use. Splitting the socket and starting the writer this early
/// is what makes a rejection possible at all: a refused client still gets its protocol
/// error written on the same socket it opened, rather than a dropped connection.
pub struct ClientWiring<D: ByteStream + 'static = Box<dyn ByteStream>> {
    reader: SessionReader<D>,
    handle: SessionHandle,
    writer: crate::downstream::SessionWriter,
}

impl<D: ByteStream + 'static> ClientWiring<D> {
    /// Splits `stream` and starts the writer task.
    ///
    /// `expected_nick` is `None` when no Network has been selected yet, which is the
    /// control-only case.
    pub fn new(
        session: SessionId,
        client: ClientId,
        expected_nick: Option<String>,
        stream: D,
    ) -> Self {
        let (read, write) = tokio::io::split(stream);
        let (control_tx, normal_tx, writer) = crate::downstream::spawn_session_writer(write);
        let handle = SessionHandle {
            session,
            client,
            control_tx,
            normal_tx,
            capabilities: std::sync::Arc::new(
                std::sync::Mutex::new(SessionCapabilities::default()),
            ),
        };
        Self {
            reader: SessionReader::new(session, read, handle.clone(), expected_nick),
            handle,
            writer,
        }
    }

    /// The handle a Network owner routes this client through.
    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }

    pub fn session(&self) -> SessionId {
        self.reader.session
    }

    /// Records the capabilities this client negotiated, so the owner's later
    /// decisions match what the client was actually told.
    pub fn set_negotiated(&self, enabled: &std::collections::BTreeSet<String>) {
        self.handle.set_negotiated(enabled);
    }

    /// Drains registration, returning the wiring ready to serve a bound client.
    ///
    /// On refusal the reader is already gone -- it owned the socket -- but the handle
    /// and writer are handed back inside the refusal so the protocol error is written
    /// on the very socket the client opened, instead of the connection just dropping.
    pub(crate) async fn register(
        self,
        timeout: std::time::Duration,
    ) -> Result<Self, RefusedRegistration> {
        let Self {
            reader,
            handle,
            writer,
        } = self;
        let claimed = reader
            .registered_nick()
            .map_or_else(|| "*".to_owned(), str::to_owned);
        // The ceiling is applied here rather than by the caller so the handle and
        // writer survive it. A client that stalls registration is still owed a written
        // reason: cancelling outside this function would drop the only half of the
        // socket that can say anything.
        let outcome = crate::timeout_bounded(timeout, reader.run_until_registration()).await;
        match outcome {
            Ok(Ok(reader)) => Ok(Self {
                reader,
                handle,
                writer,
            }),
            Ok(Err(disposition)) => Err(RefusedRegistration {
                handle,
                writer,
                disposition,
                nick: claimed,
            }),
            Err(_) => Err(RefusedRegistration {
                handle,
                writer,
                disposition: DownstreamDisposition::RegistrationTimeout,
                nick: claimed,
            }),
        }
    }

    /// The nickname this client registered with.
    pub fn registered_nick(&self) -> Option<&str> {
        self.reader.registered_nick()
    }

    /// The capabilities this client negotiated during registration.
    pub fn negotiated(&self) -> &std::collections::BTreeSet<String> {
        self.reader.negotiated()
    }

    /// Whether registration completed.
    pub fn is_registered(&self) -> bool {
        self.reader.registered_nick().is_some()
    }

    /// Drives this already-registered connection with a local handler.
    ///
    /// This is the control-only path: no Network owns the client, so its intents are
    /// answered here and never submitted anywhere. The reader runs on its own task and
    /// the writer is owned for the whole call, so the socket is closed on every exit --
    /// there is no way to return while a task is still writing to a client nobody is
    /// answering.
    pub async fn serve_locally(
        self,
        mut answer: impl FnMut(SessionIntent),
    ) -> DownstreamDisposition {
        let Self {
            mut reader,
            handle,
            writer,
        } = self;
        let (events_tx, mut events_rx) =
            mpsc::channel::<SessionEvent>(SESSION_EVENT_QUEUE_CAPACITY);
        let session = reader.session;
        let task = tokio::spawn(async move {
            let disposition = reader.run(&events_tx).await;
            let _ = events_tx
                .send(SessionEvent::Ended {
                    session,
                    disposition,
                })
                .await;
        });
        let mut disposition = DownstreamDisposition::Eof;
        while let Some(event) = events_rx.recv().await {
            match event {
                SessionEvent::Intent { intent, .. } => {
                    if matches!(intent, SessionIntent::Quit) {
                        disposition = DownstreamDisposition::LocalDetach;
                        break;
                    }
                    answer(intent);
                }
                SessionEvent::Ended {
                    disposition: ended, ..
                } => {
                    disposition = ended;
                    break;
                }
            }
        }
        // The reader must not outlive the handler: it would keep writing to a socket
        // this call is about to close.
        task.abort();
        let _ = task.await;
        drop(handle);
        writer.shutdown().await;
        disposition
    }

    /// Writes one protocol error and closes the socket.
    pub async fn refuse(self, nick: &str, code: &str, text: &str) {
        let line = format!(":bouncer {code} {nick} :{text}\r\n");
        let _ = self.handle.queue_normal(&line);
        self.writer.shutdown().await;
    }
}

/// One connection whose registration never completed.
///
/// The read half is gone with the reader, but the write half is still owned here: a
/// client that is told *why* it was refused can act on that, while a client that is
/// merely disconnected learns nothing.
pub struct RefusedRegistration {
    pub(crate) handle: SessionHandle,
    pub(crate) writer: crate::downstream::SessionWriter,
    pub(crate) disposition: DownstreamDisposition,
    /// The nickname the client had claimed when it was refused, or `*`.
    pub(crate) nick: String,
}

impl RefusedRegistration {
    pub fn disposition(&self) -> DownstreamDisposition {
        self.disposition
    }

    /// Writes the refusal, lets it reach the socket, then closes.
    ///
    /// The handle is released before the drain, because the writer only finishes once
    /// both bounded queues are closed and the handle is what holds their senders.
    pub async fn refuse(self, code: &str, text: &str) {
        let line = format!(":bouncer {code} {} :{text}\r\n", self.nick);
        let _ = self.handle.queue_normal(&line);
        drop(self.handle);
        self.writer.close_after_drain().await;
    }

    /// Closes the socket without writing anything, for a client that never registered.
    pub async fn close(self) {
        drop(self.handle);
        self.writer.close_after_drain().await;
    }
}

/// Per-session decoder and registration state.
///
/// This is deliberately a plain struct owned by the reader task: it is the only
/// mutable per-client state, and no network or durable state lives here.
struct SessionReader<D: ByteStream> {
    session: SessionId,
    read: ReadHalf<D>,
    handle: SessionHandle,
    /// The nickname this Network registered, when one was selected before
    /// registration started.
    ///
    /// `None` is the control-only case: the client registered against no Network, so
    /// any syntactically valid nickname is accepted and is purely a local label. It is
    /// never projected as an upstream identity, because no Network claimed it.
    expected_nick: Option<String>,
    decoder: LineDecoder,
    registered_nick: Option<String>,
    user_received: bool,
    /// A client that issued `CAP LS`/`CAP REQ` before registration is still
    /// negotiating: it must send `CAP END` before any welcome or projection.
    cap_negotiating: bool,
    /// Capabilities this client successfully negotiated with the bouncer.
    ///
    /// Bounded by [`crate::downstream::MAX_NEGOTIATED_CAPABILITIES`]. A client cannot
    /// grow this set without limit by repeating `CAP REQ`.
    negotiated: std::collections::BTreeSet<String>,
    ready: bool,
    /// Complete client lines already decoded but not yet translated.
    ///
    /// One read can yield several lines, and registration can complete part-way through
    /// that batch. When the reader is transferred at the registration boundary the rest
    /// of the batch must survive, so the remainder is parked here rather than dropped
    /// with the local queue that produced it. It is drained before any further read, so
    /// the client's own ordering is preserved across the transfer.
    pending: VecDeque<Vec<u8>>,
}

impl<D: ByteStream> SessionReader<D> {
    pub(crate) fn new(
        session: SessionId,
        read: ReadHalf<D>,
        handle: SessionHandle,
        expected_nick: Option<String>,
    ) -> Self {
        Self {
            session,
            read,
            handle,
            expected_nick,
            decoder: LineDecoder::default(),
            registered_nick: None,
            user_received: false,
            cap_negotiating: false,
            negotiated: std::collections::BTreeSet::new(),
            ready: false,
            pending: VecDeque::new(),
        }
    }

    /// Reads only the registration half of the protocol, then hands this reader over.
    ///
    /// Returns the reader itself once a valid `NICK` and `USER` have both arrived with
    /// no `CAP` negotiation outstanding. The reader keeps its socket half, its decoder
    /// with any undecoded bytes, the capabilities this client negotiated, and every
    /// client line that arrived after the registration boundary. Nothing is recreated
    /// and nothing is replayed, so the owner that receives it continues the *same*
    /// conversation rather than starting a second one.
    pub(crate) async fn run_until_registration(
        mut self,
    ) -> Result<SessionReader<D>, DownstreamDisposition> {
        loop {
            let raw = match self.next_line().await {
                Ok(Some(raw)) => raw,
                Ok(None) => return Err(DownstreamDisposition::Eof),
                Err(disposition) => return Err(disposition),
            };
            if raw.len() > MAX_CLIENT_LINE {
                return Err(DownstreamDisposition::QueueOverload);
            }
            match self.translate(&raw) {
                // The one pre-registration intent that ends this phase. Everything the
                // reader owns is returned exactly as it stands.
                Ok(Some(SessionIntent::RequestProjection)) => return Ok(self),
                // No other intent can be produced before registration completes; a line
                // that somehow yields one is refused rather than forwarded from a phase
                // that is not allowed to touch the owner.
                Ok(Some(_)) => return Err(DownstreamDisposition::ProtocolViolation),
                Ok(None) => {}
                Err(error) => return Err(DownstreamDisposition::from_error(&error)),
            }
        }
    }

    /// Reads until the stream ends, translating each client line into a typed intent.
    ///
    /// The only reasons this session ends are client-side. Upstream generation loss is
    /// owned by the network supervisor, which ends every session it holds itself.
    pub(crate) async fn run(
        &mut self,
        events: &mpsc::Sender<SessionEvent>,
    ) -> DownstreamDisposition {
        loop {
            let raw = match self.next_line().await {
                Ok(Some(raw)) => raw,
                Ok(None) => return DownstreamDisposition::Eof,
                Err(disposition) => return disposition,
            };
            if raw.len() > MAX_CLIENT_LINE {
                return DownstreamDisposition::QueueOverload;
            }
            match self.translate(&raw) {
                Ok(Some(intent)) => {
                    let event = SessionEvent::Intent {
                        session: self.session,
                        intent,
                    };
                    // A bounded queue is awaited here, so a client that outruns the
                    // owner applies backpressure instead of growing memory. If the
                    // owner is gone the session ends rather than blocking forever.
                    if events.send(event).await.is_err() {
                        return DownstreamDisposition::SupervisorStop;
                    }
                }
                Ok(None) => {}
                Err(error) => return DownstreamDisposition::from_error(&error),
            }
        }
    }

    /// Returns the next complete client line, preferring any already decoded.
    ///
    /// Lines decoded from an earlier read but not yet translated are drained before the
    /// socket is read again. That is what preserves a client's ordering across the
    /// registration transfer: lines the client sent after `USER` are still delivered
    /// when they arrived in the very read that completed registration.
    async fn next_line(&mut self) -> Result<Option<Vec<u8>>, DownstreamDisposition> {
        let mut buf = [0u8; 2048];
        loop {
            if let Some(raw) = self.pending.pop_front() {
                return Ok(Some(raw));
            }
            let count = self
                .read
                .read(&mut buf)
                .await
                .map_err(|_| DownstreamDisposition::ReadFailure)?;
            if count == 0 {
                return Ok(None);
            }
            // A framing or size violation is an explicit protocol failure, never a
            // partial parse of the lines that happened to decode.
            let batch = self
                .decoder
                .push(&buf[..count])
                .into_iter()
                .collect::<Result<VecDeque<Vec<u8>>, _>>()
                .map_err(|_| DownstreamDisposition::ProtocolViolation)?;
            if batch.len() > MAX_LINES_PER_READ {
                // One read yielding an unbounded number of lines is overload, not
                // work: this bounds both memory and the owner's per-read duty.
                return Err(DownstreamDisposition::QueueOverload);
            }
            self.pending = batch;
        }
    }

    /// The nickname this client registered, once registration has completed.
    pub(crate) fn registered_nick(&self) -> Option<&str> {
        self.registered_nick.as_deref()
    }

    /// The capabilities this client negotiated before registration completed.
    pub(crate) fn negotiated(&self) -> &std::collections::BTreeSet<String> {
        &self.negotiated
    }

    /// Converts one complete client line into an intent or a local reply.
    fn translate(&mut self, raw: &[u8]) -> Result<Option<SessionIntent>, RuntimeError> {
        let message = Message::parse(raw).map_err(|_| RuntimeError::Protocol)?;
        // A client may not send a prefix, and its tag budget is enforced before any
        // interpretation happens.
        if message.prefix.is_some()
            || message
                .validate_tag_budget(TagDirection::ClientInput)
                .is_err()
        {
            return Err(RuntimeError::Protocol);
        }
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        if self.ready {
            self.translate_registered(&message, &command)
        } else {
            self.translate_registration(&message, &command)
        }
    }

    fn translate_registration(
        &mut self,
        message: &Message,
        command: &str,
    ) -> Result<Option<SessionIntent>, RuntimeError> {
        match command {
            "CAP" => self.mediate_cap(message)?,
            "NICK" => {
                if let Some(value) = message.params.first() {
                    let requested = String::from_utf8_lossy(value).into_owned();
                    // With no Network selected there is nothing to collide with, so any
                    // valid nickname registers. With one selected, only that nickname
                    // does: a session may not claim an identity its Network did not
                    // register, or it would be handed a view of a network it never joined.
                    let claims_expected = match &self.expected_nick {
                        None => true,
                        Some(expected) => {
                            i2pr_irc_core::Casemapping::Rfc1459.fold(requested.as_bytes())
                                == i2pr_irc_core::Casemapping::Rfc1459.fold(expected.as_bytes())
                        }
                    };
                    if crate::valid_client_nick(value) && claims_expected {
                        self.registered_nick = Some(requested);
                    } else {
                        self.handle.queue_normal(
                            ":bouncer 433 * * :Nickname unavailable on this network\r\n",
                        )?;
                    }
                }
            }
            "USER" => self.user_received = true,
            "PING" => {
                if let Some(token) = message.params.last() {
                    let line = format!(
                        ":bouncer PONG bouncer :{}\r\n",
                        String::from_utf8_lossy(token)
                    );
                    self.handle.queue_control(&line)?;
                }
            }
            _ => self
                .handle
                .queue_normal(":bouncer 451 * :Register first\r\n")?,
        }
        // Registration completes only when a valid NICK and USER are both present and
        // no CAP negotiation is outstanding. `CAP END` alone never registers a client.
        if !self.ready
            && !self.cap_negotiating
            && self.user_received
            && self.registered_nick.is_some()
        {
            self.ready = true;
            return Ok(Some(SessionIntent::RequestProjection));
        }
        Ok(None)
    }

    fn translate_registered(
        &mut self,
        message: &Message,
        command: &str,
    ) -> Result<Option<SessionIntent>, RuntimeError> {
        match command {
            "CAP" => self.mediate_cap(message)?,
            "PING" => {
                let token = message.params.last().ok_or(RuntimeError::Protocol)?;
                let line = format!(
                    ":bouncer PONG bouncer :{}\r\n",
                    String::from_utf8_lossy(token)
                );
                self.handle.queue_control(&line)?;
            }
            "QUIT" => return Ok(Some(SessionIntent::Quit)),
            "JOIN" | "PART" => {
                // Desired membership is durable operator intent, so it is expressed as
                // an intent and persisted by the owner *before* anything goes upstream.
                let Some(target) = message.params.first() else {
                    return Err(RuntimeError::Protocol);
                };
                let channel = String::from_utf8_lossy(target).into_owned();
                return Ok(Some(if command == "JOIN" {
                    SessionIntent::Join { channel }
                } else {
                    SessionIntent::Part { channel }
                }));
            }
            "PRIVMSG" | "NOTICE" | "NICK" | "TOPIC" | "MODE" => {
                return Ok(Some(SessionIntent::Forward {
                    wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    class: IntentClass::NonReplayable,
                }));
            }
            // History and read markers are answered by the bouncer itself and are
            // never forwarded upstream. Both require the matching capability to have
            // been negotiated: answering a client that did not ask for the extension
            // would be unsolicited traffic, and a client that did not negotiate
            // `message-tags` cannot read tagged replies.
            "CHATHISTORY" | "MARKREAD" => {
                let capability = if command == "CHATHISTORY" {
                    crate::chathistory::CHATHISTORY_CAPABILITY
                } else {
                    crate::chathistory::READ_MARKER_CAPABILITY
                };
                let wire = message.encode().map_err(|_| RuntimeError::Protocol)?;
                if self.negotiated.contains(capability) {
                    return Ok(Some(if command == "CHATHISTORY" {
                        SessionIntent::HistoryQuery { wire }
                    } else {
                        SessionIntent::MarkerUpdate { wire }
                    }));
                }
                // Explicitly refused rather than silently ignored: a client that
                // issued the command needs to learn why nothing happened. This is not
                // grounds for ending the session.
                let nick = self.registered_nick.as_deref().unwrap_or("*");
                let line = format!(":bouncer 421 {nick} {command} :Unsupported command\r\n");
                self.handle.queue_normal(&line)?;
                return Ok(None);
            }
            "WHOIS" | "WHO" | "NAMES" | "LIST" => {
                return Ok(Some(SessionIntent::Forward {
                    wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    class: IntentClass::GenerationQuery,
                }));
            }
            _ => {
                // The reply names what this client actually claimed, never the
                // Network's configured nickname, so an unbound client is never told it
                // failed to register under a name it never asked for.
                let nick = self
                    .registered_nick
                    .clone()
                    .unwrap_or_else(|| "*".to_owned());
                self.handle
                    .queue_normal(&format!(":bouncer 421 {nick} * :Unsupported command\r\n"))?;
            }
        }
        Ok(None)
    }

    /// Mediates CAP locally. A client CAP command is never forwarded upstream and never
    /// alters the upstream generation's negotiated capability set.
    fn mediate_cap(&mut self, message: &Message) -> Result<(), RuntimeError> {
        let subcommand = message
            .params
            .first()
            .map(|value| String::from_utf8_lossy(value).to_ascii_uppercase())
            .unwrap_or_default();
        let target = self.registered_nick.as_deref().unwrap_or("*");
        match subcommand.as_str() {
            // A new round only suspends registration before it happens; a late CAP
            // after registration cannot un-complete a live session.
            "LS" | "REQ" => {
                if !self.ready {
                    self.cap_negotiating = true;
                }
                if subcommand == "REQ" {
                    // Acknowledged only when every requested capability is one this
                    // bouncer genuinely serves. A partial ACK would be a promise the
                    // session cannot keep.
                    let requested = crate::downstream::requested_capabilities(message);
                    let supported = !requested.is_empty()
                        && requested.iter().all(|name| {
                            crate::downstream::downstream_supported().contains(&name.as_str())
                        });
                    if !supported {
                        return self.handle.queue_normal(&format!(
                            ":bouncer CAP {target} NAK :Unsupported capabilities\r\n"
                        ));
                    }
                    for name in &requested {
                        if self.negotiated.len() < crate::downstream::MAX_NEGOTIATED_CAPABILITIES {
                            self.negotiated.insert(name.clone());
                        }
                    }
                    // The owner needs the same view to suppress the duplicate legacy
                    // backlog for a client that will fetch its own history.
                    self.handle.set_negotiated(&self.negotiated);
                    return self.handle.queue_normal(&format!(
                        ":bouncer CAP {target} ACK :{}\r\n",
                        requested.join(" ")
                    ));
                }
                self.handle.queue_normal(&format!(
                    ":bouncer CAP {target} LS :{}\r\n",
                    crate::downstream::downstream_supported().join(" ")
                ))
            }
            "END" => {
                if !self.ready {
                    self.cap_negotiating = false;
                }
                self.handle.set_negotiated(&self.negotiated);
                Ok(())
            }
            "LIST" => {
                let held = self
                    .negotiated
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                self.handle
                    .queue_normal(&format!(":bouncer CAP {target} LIST :{held}\r\n"))
            }
            // `ACK`/`NAK` are server-to-client; receiving one is a client error.
            _ => self
                .handle
                .queue_normal(":bouncer 410 * CAP :Invalid CAP subcommand\r\n"),
        }
    }
}

fn queue_line(
    sender: &mpsc::Sender<crate::downstream::QueuedFrame>,
    line: &str,
) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(crate::downstream::QueuedFrame::Fire(
            line.as_bytes().to_vec(),
        ))
        .map_err(|_| RuntimeError::QueueOverloaded)
}

/// Queues a frame whose delivery is reported back once the bytes reach the socket.
pub(crate) fn queue_acknowledged(
    handle: &SessionHandle,
    line: &str,
) -> Result<oneshot::Receiver<io::Result<()>>, RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    let (ack, response) = oneshot::channel();
    handle
        .normal_tx
        .try_send(crate::downstream::QueuedFrame::Ack(
            line.as_bytes().to_vec(),
            ack,
        ))
        .map_err(|_| RuntimeError::QueueOverloaded)?;
    Ok(response)
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn negotiating_message_tags_is_observed_by_the_owner() {
        let mut enabled = BTreeSet::new();
        enabled.insert(crate::capability::MESSAGE_TAGS.to_owned());
        let caps = SessionCapabilities::default().with_negotiated(&enabled);
        assert!(caps.negotiated_tags(), "message-tags must be observable");
        assert!(!SessionCapabilities::default().negotiated_tags());
    }
}
