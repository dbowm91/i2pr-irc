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
use i2pr_irc_core::{ByteStream, ClientId, NetworkId, SessionId};
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

/// Whether one session counts as the Operator being at the keyboard.
///
/// This is a fact about the session, not a count of sockets. A history sync running in
/// the background and a foreground window are both "attached", and treating them the
/// same is why bouncers end up permanently online: the socket is there, the Operator is
/// not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPresence {
    /// A session that represents the Operator being present.
    Active,
    /// A background or passive session: it receives state, it does not make the bouncer
    /// look present.
    Passive,
}
impl SessionPresence {
    /// The state a session starts in.
    ///
    /// Active, deliberately. A client that never says `PASSIVE` is assumed to be a
    /// foreground client, because assuming otherwise would make auto-away fire for every
    /// client that does not know the draft — a bouncer that is away while somebody is
    /// using it is worse than one that is present when nobody is.
    pub const DEFAULT: Self = Self::Active;
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Trailing parameter that turns `PART` into a detach request.
///
/// Exact match only. A shorter or longer token, or any third parameter, is an ordinary
/// part: guessing at near-misses would let a client that meant to leave a channel with a
/// message instead silently change the bouncer's whole presentation policy.
pub const DETACH_SHORTHAND: &str = "detach";
/// Trailing parameter that turns `PART` into a reattach request. See
/// [`DETACH_SHORTHAND`] for why the match is exact.
pub const ATTACH_SHORTHAND: &str = "attach";

/// Explicit ceiling on an Operator-supplied away message.
///
/// An away message is a line that reaches every member of every channel the bouncer
/// holds. The ceiling sits well below the line ceiling because the content is a single
/// phrase; anything longer is not an away message, and forwarding it would put an
/// unbounded Operator string into a field that has no framing guard left.
pub const MAX_AWAY_TEXT_BYTES: usize = 200;

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
    /// The client asked to stop presenting a channel it still holds.
    ///
    /// Detaching is not leaving: membership upstream and durable history are untouched,
    /// and only the local presentation changes. It is a Network-wide policy, so one
    /// client's request applies to every attached session.
    Detach { channel: String },
    /// The client asked to resume presenting a detached channel.
    Reattach { channel: String },
    /// The client declared itself passive: it still receives state, but it no longer
    /// makes the bouncer look present upstream.
    Passive,
    /// The client declared itself an active foreground session again.
    Active,
    /// The client set or cleared its own away state explicitly.
    ///
    /// `text` is `None` for a bare `AWAY`, which clears. A manual away is Operator intent
    /// and outlives the session that set it, so an unrelated client attaching later cannot
    /// silently drop it.
    Away { text: Option<String> },
    /// The client asked the bouncer's own control surface to do something.
    ///
    /// The original framed command, re-parsed by the adapter that owns it. A session
    /// cannot answer this itself: it submits the command and whoever holds the runtime's
    /// control handle executes it against the controller. The session never holds a
    /// store handle, a supervisor handle, or the controller itself.
    Control {
        /// The original framed command, re-parsed by the bouncer-networks or
        /// BouncerServ adapter.
        wire: Vec<u8>,
    },
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

/// Why a `BOUNCER` request was refused.
///
/// A fixed enum rather than a formatted string, so nothing a client sent can become
/// the reason text of its own refusal.
pub(crate) enum BouncerRefusal {
    NoSuchNetwork(NetworkId),
    AlreadyBound,
    AfterRegistration,
    /// A registered session asked to bind. See the plan: an unbound session stays
    /// unbound for the rest of its life.
    BindTooLate,
}

impl BouncerRefusal {
    fn subcommand(&self) -> &'static str {
        match self {
            Self::NoSuchNetwork(_) | Self::AlreadyBound | Self::BindTooLate => "BIND",
            Self::AfterRegistration => "",
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::NoSuchNetwork(network) => format!(
                "no network with id {}",
                crate::bouncer_networks::render_netid(*network)
            ),
            Self::AlreadyBound => "already bound to a network".to_owned(),
            Self::AfterRegistration => "not valid during registration".to_owned(),
            Self::BindTooLate => "a registered session cannot bind to a network".to_owned(),
        }
    }
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
    /// The client negotiated `draft/pre-away`, so `PASSIVE` and `ACTIVE` are part of the
    /// contract it expects the bouncer to honour.
    ///
    /// Tracked per session rather than globally: the pre-away declaration is about this
    /// connection, and one client knowing the capability must not let another client's
    /// `PASSIVE` be accepted on its behalf.
    pub pre_away: bool,
    /// This session negotiated the bouncer control plane, so it may send `BOUNCER`.
    ///
    /// Per session for the same reason as every other flag: whether a client is allowed
    /// to change process state is a fact about that client, not a property of the bouncer.
    pub bouncer_networks: bool,
    /// This session negotiated change notifications for the control plane.
    ///
    /// Both halves of the notification capability must be live before a client is told it
    /// exists; a client that negotiated the initial batch but never receives an update
    /// has no way to distinguish an idle bouncer from a broken one.
    pub bouncer_networks_notify: bool,
}
impl Default for SessionCapabilities {
    fn default() -> Self {
        Self {
            legacy_backlog: true,
            explicit_history: false,
            read_markers: false,
            message_tags: false,
            pre_away: false,
            bouncer_networks: false,
            bouncer_networks_notify: false,
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

    /// True when this client negotiated the pre-away draft and may therefore declare
    /// itself passive or active.
    pub fn negotiated_pre_away(&self) -> bool {
        self.pre_away
    }

    /// True when this session may send `BOUNCER` control commands.
    pub fn negotiated_bouncer_networks(&self) -> bool {
        self.bouncer_networks
    }

    /// True when this session asked for control-plane change notifications.
    pub fn negotiated_bouncer_networks_notify(&self) -> bool {
        self.bouncer_networks_notify
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
            pre_away: self.pre_away
                || enabled
                    .iter()
                    .any(|name| name == crate::presence::PRE_AWAY_CAPABILITY),
            bouncer_networks: self.bouncer_networks
                || enabled
                    .iter()
                    .any(|name| name == crate::bouncer_networks::BOUNCER_NETWORKS),
            bouncer_networks_notify: self.bouncer_networks_notify
                || enabled
                    .iter()
                    .any(|name| name == crate::bouncer_networks::BOUNCER_NETWORKS_NOTIFY),
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
        // A session attached to a Network it was already selected for has nothing to
        // bind: the decision was made before this socket existed, and accepting a
        // second one here would let a client re-home itself onto a different Network
        // mid-conversation.
        let wiring = ClientWiring::new(
            session,
            client,
            Some(expected_nick),
            std::collections::BTreeSet::new(),
            stream,
        );
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
        bindable: std::collections::BTreeSet<NetworkId>,
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
            reader: SessionReader::new(session, read, handle.clone(), expected_nick, bindable),
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

    /// The Network a pre-registration `BOUNCER BIND` claimed, if one was accepted.
    pub fn bind_request(&self) -> Option<NetworkId> {
        self.reader.bind_request()
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
    ///
    /// The handler is asynchronous because answering a control request means submitting a
    /// typed request to the process controller and waiting for it. Awaiting here is safe:
    /// the reader is already on its own task, so a slow controller cannot stop this loop
    /// from noticing that the client has gone.
    pub async fn serve_locally<Fut>(
        self,
        mut answer: impl FnMut(SessionIntent) -> Fut,
    ) -> DownstreamDisposition
    where
        Fut: std::future::Future<Output = ()>,
    {
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
                    answer(intent).await;
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
    /// A presence declaration sent before registration completed.
    ///
    /// The pre-away draft's whole point is that a background client can say it is
    /// passive *during* registration. Holding the declaration until the session is
    /// registered is what stops the bouncer counting a background client as an Operator
    /// for one turn and then taking it back: the network would see an away-and-back
    /// flap for every such client that connects.
    pre_away_declaration: Option<SessionPresence>,
    /// Whether the pre-registration declaration has already been reported.
    pre_away_reported: bool,
    /// Whether the registration projection has been requested.
    projection_reported: bool,
    /// Networks this client may still claim through a pre-registration `BOUNCER BIND`.
    ///
    /// A bounded set copied from the controller's own snapshot when the socket was
    /// accepted. Membership is decided here rather than by a round trip to the controller
    /// because a `FAIL BOUNCER BIND` must reach the client *before* registration
    /// completes; a client that binds late gets a different answer entirely, so deciding
    /// it later would make the same request mean two different things depending on
    /// timing.
    bindable: std::collections::BTreeSet<NetworkId>,
    /// The Network a pre-registration `BOUNCER BIND` claimed, once one was accepted.
    bind_request: Option<NetworkId>,
}

impl<D: ByteStream> SessionReader<D> {
    pub(crate) fn new(
        session: SessionId,
        read: ReadHalf<D>,
        handle: SessionHandle,
        expected_nick: Option<String>,
        bindable: std::collections::BTreeSet<NetworkId>,
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
            pre_away_declaration: None,
            pre_away_reported: false,
            projection_reported: false,
            bindable,
            bind_request: None,
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
                Ok(Some(SessionIntent::Passive)) | Ok(Some(SessionIntent::Active)) => {
                    // A pre-away declaration is allowed before registration completes,
                    // because declaring itself passive *is* the pre-away draft's point.
                    // It is held by the reader and reported once the session exists.
                    continue;
                }
                // No other intent can be produced before registration completes; a line
                // that somehow yields one is refused rather than forwarded from a phase
                // that is not allowed to touch the owner.
                Ok(Some(_)) => return Err(DownstreamDisposition::ProtocolViolation),
                Ok(None) => {
                    // Admission needs a registered session and nothing else, so a
                    // pre-away declaration is held by the reader rather than forwarded.
                    if self.registration_intent() == Some(SessionIntent::RequestProjection) {
                        return Ok(self);
                    }
                }
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
            // Registration completion is checked after every line, not only after one
            // that translated to nothing: a client that declared itself passive during
            // registration gets that declaration reported first, and the projection that
            // follows it is a separate turn.
            let translated = match self.translate(&raw) {
                Ok(Some(intent)) => Some(intent),
                Ok(None) => None,
                Err(error) => return DownstreamDisposition::from_error(&error),
            };
            // Completing a registration can owe the owner more than one intent: a
            // pre-away declaration first, then the projection. Both are emitted in the
            // same turn, because waiting for another line to arrive would leave a
            // registered session waiting for a projection it can never trigger.
            let mut next = translated.or_else(|| self.registration_intent());
            while let Some(intent) = next {
                let event = SessionEvent::Intent {
                    session: self.session,
                    intent,
                };
                // A bounded queue is awaited here, so a client that outruns the owner
                // applies backpressure instead of growing memory. If the owner is gone
                // the session ends rather than blocking forever.
                if events.send(event).await.is_err() {
                    return DownstreamDisposition::SupervisorStop;
                }
                next = self.registration_intent();
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

    /// The Network a pre-registration `BOUNCER BIND` claimed, if one was accepted.
    pub(crate) fn bind_request(&self) -> Option<NetworkId> {
        self.bind_request
    }

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
            self.translate_registered(&message, &command, raw)
        } else if matches!(command.as_str(), "PASSIVE" | "ACTIVE") {
            // Accepted before registration completes, which is what the draft means by
            // pre-away: a background client declares itself while it is still arriving.
            // It is recorded rather than forwarded, because there is nothing to forward
            // it *to* until the session exists.
            if !self.handle.capabilities().negotiated_pre_away() {
                return Err(RuntimeError::Protocol);
            }
            self.pre_away_declaration = Some(if command == "PASSIVE" {
                SessionPresence::Passive
            } else {
                SessionPresence::Active
            });
            Ok(None)
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
            // The one control verb the draft allows during registration. Everything else
            // in the `BOUNCER` namespace needs a Network, and a Network is not chosen yet.
            "BOUNCER" => self.mediate_bouncer_registration(message)?,
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
        Ok(None)
    }

    /// Mediates the draft's one registration-time `BOUNCER` verb.
    ///
    /// `BOUNCER BIND` claims this connection for a Network. It is accepted only here, while
    /// registration is still open, and only for a Network that existed when this socket was
    /// accepted. A second `BIND` is refused rather than silently replacing the first: two
    /// claims for one connection would leave the client believing it is on a Network the
    /// bouncer never put it on.
    ///
    /// A refusal is written immediately, on the socket the client opened, and the session
    /// stays unbound. Killing the registration would be a harsher answer than the draft
    /// asks for, and an unbound session remains a useful control connection: the client can
    /// correct the netid and bind again before registration completes.
    ///
    /// Sending `BOUNCER` at all without having negotiated the capability *is* a protocol
    /// violation, and ends the session. A client must not be able to reach the control plane
    /// with a capability it never asked for.
    fn mediate_bouncer_registration(&mut self, message: &Message) -> Result<(), RuntimeError> {
        let params: Vec<&str> = message
            .params
            .iter()
            .map(|param| std::str::from_utf8(param).unwrap_or(""))
            .collect();
        let subcommand = params.first().copied().unwrap_or("");
        let rest = if params.is_empty() {
            &[][..]
        } else {
            &params[1..]
        };
        match crate::bouncer_networks::decode_command(subcommand, rest) {
            // A decode failure is reported as itself. Reporting it as "not valid during
            // registration" would tell a client that a malformed netid was a timing
            // problem, which sends it looking in entirely the wrong place.
            Err(error) => {
                let line = crate::bouncer_networks::render_failure(subcommand, &error);
                let _ = self.handle.queue_control(&line);
                Ok(())
            }
            Ok(crate::bouncer_networks::BouncerCommand::Bind { network }) => {
                if !self.handle.capabilities().negotiated_bouncer_networks() {
                    return Err(RuntimeError::Protocol);
                }
                if self.bind_request.is_some() {
                    self.refuse_bouncer("BIND", &BouncerRefusal::AlreadyBound);
                } else if !self.bindable.contains(&network) {
                    self.refuse_bouncer("BIND", &BouncerRefusal::NoSuchNetwork(network));
                } else {
                    self.bind_request = Some(network);
                }
                Ok(())
            }
            // `LISTNETWORKS` and friends are answered by the control session this
            // connection becomes, not during registration. Answering them here would mean
            // answering from a session that does not exist yet.
            _ => {
                self.refuse_bouncer(subcommand, &BouncerRefusal::AfterRegistration);
                Ok(())
            }
        }
    }

    /// Routes a registered client's control request to whichever surface owns it.
    ///
    /// Two shapes reach the bouncer's own administration, and they are deliberately
    /// different in what they may do:
    ///
    /// * `BOUNCER …` is the bouncer-networks draft's command vocabulary, gated on that
    ///   capability.
    /// * `PRIVMSG BouncerServ :…` is the local administration service. It needs no
    ///   capability, because it is ordinary IRC: any client can type it, and every client
    ///   that connects here is a local Operator who was admitted by the access boundary
    ///   with a trusted `ClientId`. Advertising it would be a claim about interoperability
    ///   that no other client understands.
    ///
    /// Everything else — a `PRIVMSG` to a person, a `PRIVMSG` to a channel — is left alone so
    /// the owner forwards it upstream. This method returns `None` for anything that is not a
    /// control request, which is what keeps an ordinary message from being mistaken for one.
    fn mediate_control(
        &mut self,
        message: &Message,
        command: &str,
        raw: &[u8],
    ) -> Result<Option<SessionIntent>, RuntimeError> {
        if command == "BOUNCER" {
            if !self.handle.capabilities().negotiated_bouncer_networks() {
                return Err(RuntimeError::Protocol);
            }
            // `BIND` is the one verb the draft allows only during registration. A client
            // that sends it here has not read the rule, and quietly doing nothing would
            // leave it believing it had joined a Network.
            let subcommand = message
                .params
                .first()
                .map(|param| String::from_utf8_lossy(param).to_ascii_uppercase())
                .unwrap_or_default();
            if subcommand == "BIND" {
                self.refuse_bouncer("BIND", &BouncerRefusal::BindTooLate);
                return Ok(None);
            }
            return Ok(Some(SessionIntent::Control { wire: raw.to_vec() }));
        }
        // The target must fold-match the service identity, because IRC casemapping
        // applies to a target name and a client may legitimately type `bouncerserv`.
        let target = message
            .params
            .first()
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .unwrap_or_default();
        if i2pr_irc_core::Casemapping::Rfc1459.fold(target.as_bytes())
            != i2pr_irc_core::Casemapping::Rfc1459
                .fold(crate::bouncer_networks::SERVICE_NICK.as_bytes())
        {
            return Ok(None);
        }
        Ok(Some(SessionIntent::Control { wire: raw.to_vec() }))
    }

    fn refuse_bouncer(&self, _subcommand: &str, refusal: &BouncerRefusal) {
        let _ = self.handle.queue_control(&format!(
            ":{} FAIL BOUNCER {} :{}\r\n",
            crate::bouncer_networks::SERVICE_NICK,
            refusal.subcommand(),
            refusal.reason(),
        ));
    }

    /// The intent registration completing produces, once every line has been consumed.
    ///
    /// Completion is evaluated here rather than at the end of `translate` so it waits
    /// for the reader's decoded buffer to drain. A client that sends `NICK`, `USER`,
    /// `CAP REQ`, `CAP END`, and `PASSIVE` in one write has all of those decoded by the
    /// time `USER` is translated; completing on `USER` would make its own `CAP REQ` a
    /// registered command and its `PASSIVE` a declaration the bouncer has already
    /// announced it did not make. Draining first is what makes a pre-registration
    /// command mean what the draft says it means.
    fn registration_intent(&mut self) -> Option<SessionIntent> {
        if self.cap_negotiating || !self.user_received || self.registered_nick.is_none() {
            return None;
        }
        // The declaration is reported before the projection so the owner classifies the
        // session before it projects anything on its behalf.
        if let Some(declaration) = self.pre_away_declaration
            && !self.pre_away_reported
        {
            self.pre_away_reported = true;
            // `ready` is deliberately *not* set here. Registration is not complete until
            // CAP negotiation has finished, and claiming otherwise makes the session's
            // own `CAP END` arrive as a registered command that the reader ignores.
            return Some(match declaration {
                SessionPresence::Active => SessionIntent::Active,
                SessionPresence::Passive => SessionIntent::Passive,
            });
        }
        if self.ready || self.projection_reported || !self.pending.is_empty() {
            return None;
        }
        self.ready = true;
        self.projection_reported = true;
        Some(SessionIntent::RequestProjection)
    }

    fn translate_registered(
        &mut self,
        message: &Message,
        command: &str,
        raw: &[u8],
    ) -> Result<Option<SessionIntent>, RuntimeError> {
        match command {
            "CAP" => self.mediate_cap(message)?,
            // The bouncer-networks control plane.
            //
            // Submitted rather than answered here: this session has no store and no
            // controller, only whoever owns it does.
            "BOUNCER" => {
                if let Some(intent) = self.mediate_control(message, command, raw)? {
                    return Ok(Some(intent));
                }
            }
            // A message to the local administration service is a control request; a
            // message to anybody else is ordinary traffic and belongs on its ordinary
            // path. Falling through to the forward arm below is not an optimisation: a
            // `PRIVMSG` silently dropped here is a client's message that never reaches
            // the channel it was addressed to.
            "PRIVMSG" => {
                if let Some(intent) = self.mediate_control(message, command, raw)? {
                    return Ok(Some(intent));
                }
                return Ok(Some(SessionIntent::Forward {
                    wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    class: IntentClass::NonReplayable,
                }));
            }
            "PING" => {
                let token = message.params.last().ok_or(RuntimeError::Protocol)?;
                let line = format!(
                    ":bouncer PONG bouncer :{}\r\n",
                    String::from_utf8_lossy(token)
                );
                self.handle.queue_control(&line)?;
            }
            "QUIT" => return Ok(Some(SessionIntent::Quit)),
            // The pre-away declarations are only honoured from a client that negotiated
            // the capability. Accepting them unconditionally would let a client that never
            // asked for the semantics silence the Operator's presence with a command the
            // client itself does not understand the consequences of.
            "PASSIVE" | "ACTIVE" => {
                if !self.handle.capabilities().negotiated_pre_away() {
                    return Err(RuntimeError::Protocol);
                }
                return Ok(Some(if command == "PASSIVE" {
                    SessionIntent::Passive
                } else {
                    SessionIntent::Active
                }));
            }
            // An away message is the Operator's own words, so it is bounded and checked
            // here rather than at the owner: an unbounded frame must never reach a field
            // that goes to every member of every channel the bouncer holds.
            "AWAY" => {
                let text = match message.params.first() {
                    None => None,
                    Some(raw) => {
                        let text = String::from_utf8_lossy(raw).into_owned();
                        if text.is_empty() || text.len() > MAX_AWAY_TEXT_BYTES {
                            return Err(RuntimeError::Protocol);
                        }
                        if text
                            .bytes()
                            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
                        {
                            return Err(RuntimeError::Protocol);
                        }
                        Some(text)
                    }
                };
                return Ok(Some(SessionIntent::Away { text }));
            }
            "JOIN" | "PART" => {
                // Desired membership is durable operator intent, so it is expressed as
                // an intent and persisted by the owner *before* anything goes upstream.
                let Some(target) = message.params.first() else {
                    return Err(RuntimeError::Protocol);
                };
                let channel = String::from_utf8_lossy(target).into_owned();
                if command == "JOIN" {
                    return Ok(Some(SessionIntent::Join { channel }));
                }
                // `PART <channel> :detach` and `PART <channel> :attach` are the
                // compatibility shorthand for the two presentation decisions. Both
                // require exactly two parameters, so a real `PART` with a trailing part
                // message, and any longer form, stay ordinary parts. Nothing else about
                // the command is borrowed for this: an unmatched second parameter is
                // simply not a policy request.
                if message.params.len() == 2 {
                    let second = String::from_utf8_lossy(&message.params[1]).into_owned();
                    match second.as_str() {
                        DETACH_SHORTHAND => {
                            return Ok(Some(SessionIntent::Detach { channel }));
                        }
                        ATTACH_SHORTHAND => {
                            return Ok(Some(SessionIntent::Reattach { channel }));
                        }
                        _ => {}
                    }
                }
                return Ok(Some(SessionIntent::Part { channel }));
            }
            "NOTICE" | "NICK" | "TOPIC" | "MODE" => {
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
