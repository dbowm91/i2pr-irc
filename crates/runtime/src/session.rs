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
}
impl Default for SessionCapabilities {
    fn default() -> Self {
        Self {
            legacy_backlog: true,
            explicit_history: false,
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

    /// Applies a client's successful `CAP REQ`, recording that it manages history.
    pub fn with_negotiated(&self, enabled: &std::collections::BTreeSet<String>) -> Self {
        Self {
            legacy_backlog: self.legacy_backlog,
            explicit_history: self.explicit_history
                || crate::chathistory::session_manages_history(enabled),
        }
    }
}

/// Handle the owner uses to reach one session.
///
/// The owner holds only bounded routing metadata, never a stream or mutable session
/// state, so the number of attached clients cannot widen the owner's own state.
#[derive(Clone)]
pub struct SessionHandle {
    session: SessionId,
    client: ClientId,
    control_tx: mpsc::Sender<crate::downstream::QueuedFrame>,
    normal_tx: mpsc::Sender<crate::downstream::QueuedFrame>,
    capabilities: SessionCapabilities,
}

impl SessionHandle {
    pub fn session(&self) -> SessionId {
        self.session
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
    /// Queues one normalized upstream line for fanout. A full queue is reported so the
    /// owner can detach *this* client rather than stalling upstream or other clients.
    pub fn fanout(&self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        self.normal_tx
            .try_send(crate::downstream::QueuedFrame::Fire(bytes))
            .map_err(|_| RuntimeError::QueueOverloaded)
    }
    /// This attachment's negotiated capabilities.
    pub fn capabilities(&self) -> SessionCapabilities {
        self.capabilities
    }
    pub fn with_capabilities(mut self, capabilities: SessionCapabilities) -> Self {
        self.capabilities = capabilities;
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
        let (read, write) = tokio::io::split(stream);
        let (control_tx, normal_tx, writer) = crate::downstream::spawn_session_writer(write);
        let handle = SessionHandle {
            session,
            client,
            control_tx,
            normal_tx,
            capabilities: SessionCapabilities::default(),
        };
        let identity = session;
        let owner_handle = handle.clone();
        let reader = tokio::spawn(async move {
            let mut reader = SessionReader::new(identity, read, handle, expected_nick);
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
            reader,
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

/// Per-session decoder and registration state.
///
/// This is deliberately a plain struct owned by the reader task: it is the only
/// mutable per-client state, and no network or durable state lives here.
struct SessionReader<D: ByteStream> {
    session: SessionId,
    read: ReadHalf<D>,
    handle: SessionHandle,
    /// The nickname this network registered, fixed for the session's lifetime.
    expected_nick: String,
    decoder: LineDecoder,
    registered_nick: Option<String>,
    user_received: bool,
    /// A client that issued `CAP LS`/`CAP REQ` before registration is still
    /// negotiating: it must send `CAP END` before any welcome or projection.
    cap_negotiating: bool,
    ready: bool,
}

impl<D: ByteStream> SessionReader<D> {
    fn new(
        session: SessionId,
        read: ReadHalf<D>,
        handle: SessionHandle,
        expected_nick: String,
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
            ready: false,
        }
    }

    /// Reads until the stream ends, translating each client line into a typed intent.
    ///
    /// The only reasons this session ends are client-side. Upstream generation loss is
    /// owned by the network supervisor, which ends every session it holds itself.
    async fn run(&mut self, events: &mpsc::Sender<SessionEvent>) -> DownstreamDisposition {
        let mut buf = [0u8; 2048];
        loop {
            let count = match self.read.read(&mut buf).await {
                Ok(0) => return DownstreamDisposition::Eof,
                Ok(count) => count,
                Err(_) => return DownstreamDisposition::ReadFailure,
            };
            let mut batch: VecDeque<Vec<u8>> = match self
                .decoder
                .push(&buf[..count])
                .into_iter()
                .collect::<Result<VecDeque<_>, _>>()
            {
                Ok(lines) => lines,
                // A framing or size violation is an explicit protocol failure, never a
                // partial parse of the lines that happened to decode.
                Err(_) => return DownstreamDisposition::ProtocolViolation,
            };
            if batch.len() > MAX_LINES_PER_READ {
                // One read yielding an unbounded number of lines is overload, not
                // work: this bounds both memory and the owner's per-read duty.
                return DownstreamDisposition::QueueOverload;
            }
            while let Some(raw) = batch.pop_front() {
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
                    if crate::valid_client_nick(value)
                        && i2pr_irc_core::Casemapping::Rfc1459.fold(requested.as_bytes())
                            == i2pr_irc_core::Casemapping::Rfc1459
                                .fold(self.expected_nick.as_bytes())
                    {
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
            "WHOIS" | "WHO" | "NAMES" | "LIST" => {
                return Ok(Some(SessionIntent::Forward {
                    wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    class: IntentClass::GenerationQuery,
                }));
            }
            _ => {
                let nick = self.expected_nick.clone();
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
                match subcommand.as_str() {
                    "REQ" => self.handle.queue_normal(&format!(
                        ":bouncer CAP {target} NAK :Unsupported capabilities\r\n"
                    )),
                    _ => self
                        .handle
                        .queue_normal(&format!(":bouncer CAP {target} LS :\r\n")),
                }
            }
            "END" => {
                if !self.ready {
                    self.cap_negotiating = false;
                }
                Ok(())
            }
            "LIST" => self
                .handle
                .queue_normal(&format!(":bouncer CAP {target} LIST :\r\n")),
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
