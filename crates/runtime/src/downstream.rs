//! One attached local IRC client over a live upstream generation.
//!
//! A session is a disposable view: it holds its own bounded queues, writer task,
//! and registration state, and it borrows a point-in-time projection of the
//! generation-owned [`NetworkState`]. Session completion is data the network owner
//! handles, never upstream generation completion.
use crate::{
    CONTROL_QUEUE_CAPACITY, IntentClass, NORMAL_QUEUE_CAPACITY, OutboundIntent, RuntimeError,
    chathistory::{CHATHISTORY_CAPABILITY, READ_MARKER_CAPABILITY},
    state::NetworkState,
};
use i2pr_irc_core::{ByteStream, ClientId, ConnectionGeneration};
use i2pr_irc_wire::{LineDecoder, MAX_LINE_BYTES, Message, TagDirection};
use std::io;
use tokio::{
    io::{AsyncReadExt, ReadHalf, WriteHalf},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

/// Capabilities this bouncer can genuinely serve for a local client.
///
/// Deliberately short. Advertising a capability the session does not implement would
/// make `CAP LS` a lie the client has no way to detect.
///
/// This is the live authority. The reviewable rationale for each entry, and the list of
/// capabilities deliberately withheld, live in [`crate::capability`].
pub const DOWNSTREAM_ADVERTISED: &[&str] = &[
    CHATHISTORY_CAPABILITY,
    READ_MARKER_CAPABILITY,
    crate::capability::MESSAGE_TAGS,
    crate::capability::BATCH,
    crate::capability::LABELED_RESPONSE,
    crate::presence::PRE_AWAY_CAPABILITY,
    // The control plane is advertised because both halves are live: the initial
    // `LISTNETWORKS` batch and the revision-derived change notifications. Advertising
    // only the first would leave a client unable to tell an idle bouncer from a broken
    // one, so both are gated on the same complete implementation.
    crate::search::SEARCH_CAPABILITY,
    crate::bouncer_networks::BOUNCER_NETWORKS,
    crate::bouncer_networks::BOUNCER_NETWORKS_NOTIFY,
];

/// The exact `CAP LS` and `CAP REQ` support set for this generation.
///
/// Both are derived from the same constant so a capability cannot be advertised by one
/// path and refused by the other: an ACK for a capability absent from `CAP LS` is the
/// kind of contradiction a client cannot detect until it depends on it.
pub fn downstream_supported() -> &'static [&'static str] {
    DOWNSTREAM_ADVERTISED
}

/// Ceiling on capabilities one client may hold negotiated.
pub const MAX_NEGOTIATED_CAPABILITIES: usize = 8;

/// The capabilities a `CAP REQ` line asks for, bounded and upper-cased.
pub(crate) fn requested_capabilities(message: &Message) -> Vec<String> {
    message
        .params
        .iter()
        .skip(1)
        .filter_map(|param| {
            // The lossy string is a temporary, so each piece is owned before the
            // next parameter is visited.
            let text = String::from_utf8_lossy(param).into_owned();
            (!text.is_empty()).then_some(text)
        })
        .flat_map(|text| {
            text.split(' ')
                .filter(|name| !name.is_empty())
                .map(|name| name.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .take(MAX_NEGOTIATED_CAPABILITIES * 2)
        .collect()
}

/// Maximum complete tagged line accepted from one local client.
pub const MAX_CLIENT_LINE: usize = i2pr_irc_wire::MAX_TAGGED_LINE_BYTES;

/// Why an attached client ended. Only [`DownstreamDisposition::SupervisorStop`] and
/// upstream generation loss may coincide with upstream shutdown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownstreamDisposition {
    /// The session remains the attached client.
    Attached,
    /// The client's own `QUIT` requested a graceful local detach.
    LocalDetach,
    /// The local client closed its stream.
    Eof,
    /// Malformed framing, an illegal prefix, an unsupported command shape, or an
    /// oversized tag budget.
    ProtocolViolation,
    /// A bounded client queue refused more traffic.
    QueueOverload,
    /// The local client stream failed.
    ReadFailure,
    /// The owned client writer task ended.
    WriterFailure,
    /// The client did not complete registration inside the admission ceiling.
    ///
    /// Distinct from [`DownstreamDisposition::Eof`] because the two call for different
    /// operator responses: a client that disconnected is normal, and a client that
    /// connected and then said nothing is a client the ceiling had to end.
    RegistrationTimeout,
    /// Explicit supervisor stop; upstream shutdown is owned by the network owner.
    SupervisorStop,
    /// The upstream generation ended and the client was terminated with it.
    UpstreamGenerationLost,
}
impl DownstreamDisposition {
    pub fn class(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::LocalDetach => "local-detach",
            Self::Eof => "downstream-eof",
            Self::ProtocolViolation => "downstream-protocol",
            Self::QueueOverload => "downstream-overload",
            Self::ReadFailure => "downstream-read",
            Self::WriterFailure => "downstream-writer",
            Self::RegistrationTimeout => "downstream-registration-timeout",
            Self::SupervisorStop => "supervisor-stop",
            Self::UpstreamGenerationLost => "upstream-generation-lost",
        }
    }
    /// True when only this client ends and the upstream generation continues.
    pub fn is_local_only(self) -> bool {
        matches!(
            self,
            Self::LocalDetach
                | Self::Eof
                | Self::ProtocolViolation
                | Self::QueueOverload
                | Self::ReadFailure
                | Self::WriterFailure
        )
    }
    /// Maps a session failure onto its disposition.
    pub fn from_error(error: &RuntimeError) -> Self {
        match error {
            RuntimeError::QueueOverloaded => Self::QueueOverload,
            RuntimeError::Protocol => Self::ProtocolViolation,
            RuntimeError::Io(_) => Self::ReadFailure,
            _ => Self::ProtocolViolation,
        }
    }
}

/// One frame queued for a session's writer task.
///
/// A frame is either fire-and-forget or *acknowledged*. History playback must use the
/// acknowledged form: a playback cursor may only advance after the writer reports the
/// bytes actually reached the socket, so a crash between write and commit duplicates
/// on restart rather than leaving a silent gap.
#[derive(Debug)]
pub enum QueuedFrame {
    /// Written without confirmation.
    Fire(Vec<u8>),
    /// Written, then the result is reported to `ack`.
    Ack(Vec<u8>, oneshot::Sender<io::Result<()>>),
}
impl QueuedFrame {
    fn into_parts(self) -> (Vec<u8>, Option<oneshot::Sender<io::Result<()>>>) {
        match self {
            Self::Fire(bytes) => (bytes, None),
            Self::Ack(bytes, ack) => (bytes, Some(ack)),
        }
    }
}

/// Ownership handle for one client's writer task. It is never detached.
/// How long a closing writer is given to write what it already holds.
const WRITER_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

pub struct SessionWriter {
    exit: mpsc::Receiver<io::Result<()>>,
    handle: JoinHandle<()>,
}
impl SessionWriter {
    /// Completes when the owned writer task ends.
    pub async fn wait(&mut self) -> io::Result<()> {
        self.exit.recv().await.unwrap_or(Ok(()))
    }
    /// Aborts and joins the owned writer task so no client work outlives its session.
    pub async fn shutdown(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }

    /// Lets queued frames reach the socket, then closes.
    ///
    /// Aborting is the right way to stop a writer and the wrong way to end a
    /// conversation: a refusal queued microseconds earlier would be discarded along
    /// with the task, and the client that was owed an explanation would get a closed
    /// connection instead. So the writer is given a bounded window to write what it
    /// already holds, and only a writer that overruns that window is aborted.
    ///
    /// The caller must have released the session handle first: the writer only finishes
    /// once both bounded queues are closed, and the handle is what holds their senders.
    pub async fn close_after_drain(self) {
        let Self { mut exit, handle } = self;
        if crate::timeout_bounded(WRITER_DRAIN_DEADLINE, exit.recv())
            .await
            .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }
    }
}

/// Spawns the owned writer task plus its separate bounded control/normal queues.
///
/// This is the M003-B/M003-C session writer: frames carry an optional acknowledgment
/// so history playback can advance a cursor only after bytes reach the socket.
pub fn spawn_session_writer<D: ByteStream + 'static>(
    stream: WriteHalf<D>,
) -> (
    mpsc::Sender<QueuedFrame>,
    mpsc::Sender<QueuedFrame>,
    SessionWriter,
) {
    let (control_tx, mut control_rx) = mpsc::channel::<QueuedFrame>(CONTROL_QUEUE_CAPACITY);
    let (normal_tx, mut normal_rx) = mpsc::channel::<QueuedFrame>(NORMAL_QUEUE_CAPACITY);
    let (exit_tx, exit_rx) = mpsc::channel::<io::Result<()>>(1);
    let handle = tokio::spawn(async move {
        let mut stream = stream;
        let result = loop {
            match crate::next_queued_frame(&mut control_rx, &mut normal_rx).await {
                Some(frame) => {
                    let (bytes, ack) = frame.into_parts();
                    match crate::write_frame(&mut stream, &bytes).await {
                        Ok(()) => {
                            // The acknowledgment is only sent after the bytes are on
                            // the socket, so a cursor advance is never premature.
                            if let Some(ack) = ack {
                                let _ = ack.send(Ok(()));
                            }
                        }
                        Err(error) => {
                            let reported = std::io::Error::new(error.kind(), error.to_string());
                            if let Some(ack) = ack {
                                let _ = ack.send(Err(std::io::Error::new(
                                    reported.kind(),
                                    reported.to_string(),
                                )));
                            }
                            break Err(error);
                        }
                    }
                }
                None => break Ok(()),
            }
        };
        let _ = exit_tx.send(result).await;
    });
    (
        control_tx,
        normal_tx,
        SessionWriter {
            exit: exit_rx,
            handle,
        },
    )
}

/// Spawns a writer task over raw, already-framed upstream bytes.
///
/// The legacy single-session path predates per-frame acknowledgment and only ever
/// forwards already-formed bytes, so it needs no acknowledgment channel. Keeping
/// this separate avoids widening the acknowledged session writer with a mode it
/// never uses.
pub fn spawn_raw_writer<D: ByteStream + 'static>(
    stream: WriteHalf<D>,
) -> (mpsc::Sender<Vec<u8>>, mpsc::Sender<Vec<u8>>, SessionWriter) {
    let (control_tx, mut control_rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
    let (normal_tx, mut normal_rx) = mpsc::channel::<Vec<u8>>(NORMAL_QUEUE_CAPACITY);
    let (exit_tx, exit_rx) = mpsc::channel::<io::Result<()>>(1);
    let handle = tokio::spawn(async move {
        let mut stream = stream;
        let result = loop {
            match crate::next_upstream_frame(&mut control_rx, &mut normal_rx).await {
                Some(frame) => {
                    if let Err(error) = crate::write_frame(&mut stream, &frame).await {
                        break Err(error);
                    }
                }
                None => break Ok(()),
            }
        };
        let _ = exit_tx.send(result).await;
    });
    (
        control_tx,
        normal_tx,
        SessionWriter {
            exit: exit_rx,
            handle,
        },
    )
}

/// Everything a client line may legitimately depend on.
pub struct DownstreamContext<'a> {
    pub generation: ConnectionGeneration,
    pub state: &'a NetworkState,
    pub upstream_control: &'a mpsc::Sender<Vec<u8>>,
    pub upstream_normal: &'a mpsc::Sender<OutboundIntent>,
    /// Capabilities this client successfully negotiated with the bouncer.
    ///
    /// An owned snapshot rather than a borrow: the session owns the live set and is
    /// borrowed mutably for the duration of the call. It is bounded by
    /// [`MAX_NEGOTIATED_CAPABILITIES`], so cloning it is not a scaling concern.
    pub negotiated: std::collections::BTreeSet<String>,
}

/// One attached local IRC client.
pub struct DownstreamSession<D: ByteStream> {
    client: ClientId,
    read: ReadHalf<D>,
    decoder: LineDecoder,
    control_tx: mpsc::Sender<Vec<u8>>,
    normal_tx: mpsc::Sender<Vec<u8>>,
    registered_nick: Option<String>,
    /// Capabilities this client successfully negotiated with the bouncer.
    ///
    /// Bounded by [`MAX_NEGOTIATED_CAPABILITIES`]; a client cannot make this set
    /// grow without limit by sending repeated `CAP REQ` lines.
    negotiated: std::collections::BTreeSet<String>,
    user_received: bool,
    /// A client that issued `CAP LS`/`CAP REQ` before registration is still
    /// negotiating: it must send `CAP END` before any welcome or state projection.
    cap_negotiating: bool,
    ready: bool,
}
impl<D: ByteStream> DownstreamSession<D> {
    pub fn new(
        client: ClientId,
        read: ReadHalf<D>,
        control_tx: mpsc::Sender<Vec<u8>>,
        normal_tx: mpsc::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            client,
            read,
            decoder: LineDecoder::default(),
            control_tx,
            normal_tx,
            registered_nick: None,
            negotiated: std::collections::BTreeSet::new(),
            user_received: false,
            cap_negotiating: false,
            ready: false,
        }
    }
    /// Capabilities this client successfully negotiated, for projection decisions.
    pub fn negotiated(&self) -> std::collections::BTreeSet<String> {
        self.negotiated.clone()
    }

    pub fn client(&self) -> ClientId {
        self.client
    }
    pub fn is_ready(&self) -> bool {
        self.ready
    }
    /// True while a pre-registration client still owes the bouncer a `CAP END`.
    pub fn is_cap_negotiating(&self) -> bool {
        self.cap_negotiating
    }
    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf).await
    }
    pub fn queue_depths(&self) -> (usize, usize) {
        (
            NORMAL_QUEUE_CAPACITY - self.normal_tx.capacity(),
            CONTROL_QUEUE_CAPACITY - self.control_tx.capacity(),
        )
    }
    /// Frames every complete client line in `bytes`. A framing or size violation is
    /// an explicit protocol failure, never a partial parse.
    pub fn ingest(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, RuntimeError> {
        self.decoder
            .push(bytes)
            .into_iter()
            .map(|line| line.map_err(|_| RuntimeError::Protocol))
            .collect()
    }
    /// Forwards one normalized upstream line to a registered client.
    pub fn forward(&self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        queue_bytes(&self.normal_tx, bytes)
    }
    /// Handles one complete client line and reports whether the session continues.
    pub fn handle_line(
        &mut self,
        raw: &[u8],
        ctx: &DownstreamContext<'_>,
    ) -> Result<DownstreamDisposition, RuntimeError> {
        let message = Message::parse(raw).map_err(|_| RuntimeError::Protocol)?;
        if message.prefix.is_some()
            || message
                .validate_tag_budget(TagDirection::ClientInput)
                .is_err()
        {
            return Err(RuntimeError::Protocol);
        }
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        if self.ready {
            self.handle_registered_line(&message, &command, ctx)
        } else {
            self.handle_registration(&message, &command, ctx)
        }
    }

    /// Mediates CAP locally. This never forwards a client CAP command upstream and
    /// never alters the upstream generation's negotiated capability set. The bouncer
    /// advertises no downstream capability of its own, so `REQ` is always refused
    /// while `LIST` reports the empty advertised set.
    fn mediate_cap(&mut self, message: &Message) -> Result<(), RuntimeError> {
        let subcommand = message
            .params
            .first()
            .map(|value| String::from_utf8_lossy(value).to_ascii_uppercase())
            .unwrap_or_default();
        let target = self.registered_nick.as_deref().unwrap_or("*");
        match subcommand.as_str() {
            // A new negotiation round only suspends registration before it happens;
            // a late CAP after registration cannot un-complete a live session.
            "LS" | "REQ" => {
                if !self.ready {
                    self.cap_negotiating = true;
                }
            }
            "END" => {
                if !self.ready {
                    self.cap_negotiating = false;
                }
                return Ok(());
            }
            "LIST" => {
                return queue_line(
                    &self.normal_tx,
                    &format!(
                        ":bouncer CAP {target} LIST :{}\r\n",
                        self.joined_capabilities()
                    ),
                );
            }
            // `ACK`/`NAK` are server-to-client; receiving one is a client protocol
            // error, not a negotiation step.
            _ => {
                return queue_line(
                    &self.normal_tx,
                    ":bouncer 410 * CAP :Invalid CAP subcommand\r\n",
                );
            }
        }

        if subcommand == "REQ" {
            // A request is acknowledged only when *every* capability in it is one
            // this bouncer genuinely implements downstream. A partial ACK would be a
            // promise the session cannot keep.
            let requested = requested_capabilities(message);
            let supported = !requested.is_empty()
                && requested
                    .iter()
                    .all(|name| downstream_supported().contains(&name.as_str()));
            if !supported {
                return queue_line(
                    &self.normal_tx,
                    &format!(":bouncer CAP {target} NAK :Unsupported capabilities\r\n"),
                );
            }
            for name in &requested {
                if self.negotiated.len() < MAX_NEGOTIATED_CAPABILITIES {
                    self.negotiated.insert(name.clone());
                }
            }
            return queue_line(
                &self.normal_tx,
                &format!(":bouncer CAP {target} ACK :{}\r\n", requested.join(" ")),
            );
        }

        queue_line(
            &self.normal_tx,
            &format!(
                ":bouncer CAP {target} LS :{}\r\n",
                downstream_supported().join(" ")
            ),
        )
    }

    /// The negotiated capability set, as the owner's view of this session.
    pub fn capabilities(&self) -> crate::session::SessionCapabilities {
        crate::session::SessionCapabilities::default().with_negotiated(&self.negotiated)
    }

    /// The capabilities this client currently holds negotiated.
    fn joined_capabilities(&self) -> String {
        self.negotiated
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn handle_registration(
        &mut self,
        message: &Message,
        command: &str,
        ctx: &DownstreamContext<'_>,
    ) -> Result<DownstreamDisposition, RuntimeError> {
        match command {
            "CAP" => self.mediate_cap(message)?,
            "NICK" => {
                if let Some(value) = message.params.first() {
                    let requested = String::from_utf8_lossy(value).into_owned();
                    if crate::valid_client_nick(value)
                        && ctx.state.same_nick(&requested, &ctx.state.nick)
                    {
                        self.registered_nick = Some(requested);
                    } else {
                        queue_line(
                            &self.normal_tx,
                            ":bouncer 433 * * :Nickname unavailable on this network\r\n",
                        )?;
                    }
                }
            }
            "USER" => self.user_received = true,
            "PING" => {
                if let Some(token) = message.params.last() {
                    queue_control(
                        &self.control_tx,
                        &format!(
                            ":bouncer PONG bouncer :{}\r\n",
                            String::from_utf8_lossy(token)
                        ),
                    )?;
                }
            }
            _ => queue_line(&self.normal_tx, ":bouncer 451 * :Register first\r\n")?,
        }
        // Registration completes only when a valid NICK and USER are both present
        // and no CAP negotiation is outstanding. A client that never used CAP is
        // unaffected, and `CAP END` alone never registers a client.
        if !self.ready
            && !self.cap_negotiating
            && self.user_received
            && self.registered_nick.is_some()
        {
            self.ready = true;
            self.project_current_state(ctx)?;
        }
        Ok(DownstreamDisposition::Attached)
    }

    fn handle_registered_line(
        &mut self,
        message: &Message,
        command: &str,
        ctx: &DownstreamContext<'_>,
    ) -> Result<DownstreamDisposition, RuntimeError> {
        match command {
            "CAP" => self.mediate_cap(message)?,
            "PING" => {
                let token = message.params.last().ok_or(RuntimeError::Protocol)?;
                queue_control(
                    &self.control_tx,
                    &format!(
                        ":bouncer PONG bouncer :{}\r\n",
                        String::from_utf8_lossy(token)
                    ),
                )?;
            }
            "PRIVMSG" | "NOTICE" | "JOIN" | "PART" | "NICK" | "TOPIC" | "MODE" => {
                ctx.upstream_normal
                    .try_send(OutboundIntent {
                        generation: ctx.generation,
                        class: IntentClass::NonReplayable,
                        wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    })
                    .map_err(|_| RuntimeError::QueueOverloaded)?;
            }
            "WHOIS" | "WHO" | "NAMES" | "LIST" => {
                ctx.upstream_normal
                    .try_send(OutboundIntent {
                        generation: ctx.generation,
                        class: IntentClass::GenerationQuery,
                        wire: message.encode().map_err(|_| RuntimeError::Protocol)?,
                    })
                    .map_err(|_| RuntimeError::QueueOverloaded)?;
            }
            "QUIT" => return Ok(DownstreamDisposition::LocalDetach),
            _ => queue_line(
                &self.normal_tx,
                &format!(":bouncer 421 {} * :Unsupported command\r\n", ctx.state.nick),
            )?,
        }
        Ok(DownstreamDisposition::Attached)
    }

    /// Sends the point-in-time bounded projection of retained upstream state.
    fn project_current_state(&self, ctx: &DownstreamContext<'_>) -> Result<(), RuntimeError> {
        let target = self.registered_nick.as_deref().unwrap_or("*");
        queue_line(
            &self.normal_tx,
            &format!(":bouncer 001 {target} :Welcome\r\n"),
        )?;
        for token in &ctx.state.isupport {
            queue_line(
                &self.normal_tx,
                &format!(":bouncer 005 {target} {token} :are supported by this server\r\n"),
            )?;
        }
        // The bouncer's own history surface is advertised only to clients that
        // actually negotiated it, and only for the subcommands this build really
        // implements. Advertising `CHATHISTORY` to a client that never negotiated the
        // capability would invite a request the client has no batch support to read.
        if ctx.negotiated.contains(CHATHISTORY_CAPABILITY) {
            for token in crate::chathistory::isupport_tokens() {
                queue_line(
                    &self.normal_tx,
                    &format!(":bouncer 005 {target} {token} :are supported by this server\r\n"),
                )?;
            }
        }
        for channel in ctx.state.joined_channels() {
            queue_line(
                &self.normal_tx,
                &format!(":{} JOIN {channel}\r\n", ctx.state.nick),
            )?;
            let Some(state) = ctx.state.channels.get(&channel) else {
                continue;
            };
            if let Some(topic) = &state.topic {
                let prefix = format!(":bouncer 332 {target} {channel} :");
                let budget = MAX_LINE_BYTES.saturating_sub(prefix.len() + 2).max(1);
                let mut end = topic.len().min(budget);
                while end > 0 && !topic.is_char_boundary(end) {
                    end -= 1;
                }
                queue_line(&self.normal_tx, &format!("{prefix}{}\r\n", &topic[..end]))?;
            }
            // An incomplete mode snapshot is omitted rather than projected falsely.
            if let Some(modes) = state.modes.render()
                && !modes.is_empty()
            {
                let line = format!(":bouncer 324 {target} {channel} +{modes}\r\n");
                if line.len() <= MAX_LINE_BYTES {
                    queue_line(&self.normal_tx, &line)?;
                }
            }
            // Membership is projected only when it is known to be complete.
            if state.names_seen && state.members_complete {
                let mut names: Vec<String> = state
                    .members
                    .iter()
                    .map(|member| member.display())
                    .collect();
                names.sort();
                let list = names.join(" ");
                let prefix = format!(":bouncer 353 {target} = {channel} :");
                let budget = MAX_LINE_BYTES.saturating_sub(prefix.len() + 2).max(1);
                let mut start = 0;
                while start < list.len() {
                    let mut end = (start + budget).min(list.len());
                    while end > start && !list.is_char_boundary(end) {
                        end -= 1;
                    }
                    queue_line(
                        &self.normal_tx,
                        &format!("{prefix}{}\r\n", &list[start..end]),
                    )?;
                    start = end;
                }
                if list.is_empty() {
                    queue_line(&self.normal_tx, &format!("{prefix}\r\n"))?;
                }
                queue_line(
                    &self.normal_tx,
                    &format!(":bouncer 366 {target} {channel} :End of NAMES list\r\n"),
                )?;
            }
        }
        Ok(())
    }
}

fn queue_control(sender: &mpsc::Sender<Vec<u8>>, line: &str) -> Result<(), RuntimeError> {
    queue_line(sender, line)
}

fn queue_line(sender: &mpsc::Sender<Vec<u8>>, line: &str) -> Result<(), RuntimeError> {
    if line.len() > MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(line.as_bytes().to_vec())
        .map_err(|_| RuntimeError::QueueOverloaded)
}

fn queue_bytes(sender: &mpsc::Sender<Vec<u8>>, bytes: Vec<u8>) -> Result<(), RuntimeError> {
    sender
        .try_send(bytes)
        .map_err(|_| RuntimeError::QueueOverloaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DesiredChannelPolicy, NetworkState};

    /// A session plus the queues a test observes directly.
    struct Harness {
        session: DownstreamSession<tokio::io::DuplexStream>,
        normal_rx: mpsc::Receiver<Vec<u8>>,
        control_rx: mpsc::Receiver<Vec<u8>>,
        upstream_normal_rx: mpsc::Receiver<OutboundIntent>,
        state: NetworkState,
        upstream_control: mpsc::Sender<Vec<u8>>,
        upstream_normal: mpsc::Sender<OutboundIntent>,
    }
    impl Harness {
        fn new(state: NetworkState) -> Self {
            let (client_side, _peer) = tokio::io::duplex(4096);
            let (read, _write) = tokio::io::split(client_side);
            let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
            let (normal_tx, normal_rx) = mpsc::channel(NORMAL_QUEUE_CAPACITY);
            let (upstream_control, _upstream_control_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
            let (upstream_normal, upstream_normal_rx) = mpsc::channel(NORMAL_QUEUE_CAPACITY);
            Self {
                session: DownstreamSession::new(ClientId(1), read, control_tx, normal_tx),
                normal_rx,
                control_rx,
                upstream_normal_rx,
                state,
                upstream_control,
                upstream_normal,
            }
        }
        /// Borrows disjoint fields so the session can be used mutably.
        fn send(&mut self, line: &[u8]) -> Result<DownstreamDisposition, RuntimeError> {
            let context = DownstreamContext {
                negotiated: self.session.negotiated.clone(),
                generation: ConnectionGeneration(7),
                state: &self.state,
                upstream_control: &self.upstream_control,
                upstream_normal: &self.upstream_normal,
            };
            self.session.handle_line(line, &context)
        }
        fn register(&mut self) {
            self.send(b"NICK bot\r\n").expect("nick accepted");
            self.send(b"USER bot 0 * :phone\r\n")
                .expect("user accepted");
        }
        fn drain_normal(&mut self) -> String {
            let mut out = String::new();
            while let Ok(frame) = self.normal_rx.try_recv() {
                out.push_str(&String::from_utf8_lossy(&frame));
            }
            out
        }
    }

    #[tokio::test]
    async fn registration_projects_retained_state_truthfully() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        // Membership is only ever established by the server's own JOIN.
        state.begin_desired_join("#room");
        state.apply_line(&Message::parse(b":bot!u@h JOIN #room\r\n").unwrap());
        state.apply_line(
            &Message::parse(b":srv 005 bot PREFIX=(ov)@+ CHANMODES=beI,k,l,imnpst\r\n").unwrap(),
        );
        state.apply_line(&Message::parse(b":srv 332 bot #room :subject\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 324 bot #room +kl key 42\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 353 bot = #room :bot @Alice\r\n").unwrap());
        let mut harness = Harness::new(state);
        harness.register();
        let projection = harness.drain_normal();
        assert!(projection.contains("001 bot"), "{projection}");
        assert!(projection.contains("005 bot PREFIX=(ov)@+"), "{projection}");
        assert!(projection.contains("JOIN #room"), "{projection}");
        assert!(
            projection.contains("332 bot #room :subject"),
            "{projection}"
        );
        assert!(
            projection.contains("324 bot #room +kl key 42"),
            "{projection}"
        );
        assert!(
            projection.contains("353 bot = #room :@Alice bot"),
            "{projection}"
        );
        assert!(
            projection.contains("366 bot #room :End of NAMES list"),
            "{projection}"
        );
    }

    #[tokio::test]
    async fn incomplete_mode_state_omits_the_mode_projection() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        state.apply_line(&Message::parse(b":bot!u@h JOIN #room\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv MODE #room +nq\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 353 bot = #room :bot\r\n").unwrap());
        let mut harness = Harness::new(state);
        harness.register();
        let projection = harness.drain_normal();
        assert!(projection.contains("JOIN #room"), "{projection}");
        assert!(!projection.contains("324"), "{projection}");
        assert!(projection.contains("353 bot = #room :bot"), "{projection}");
    }

    #[tokio::test]
    async fn incomplete_membership_omits_the_names_projection() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        state.apply_line(&Message::parse(b":bot!u@h JOIN #room\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 353 bot = #room :bot\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv MODE #room +v Stranger\r\n").unwrap());
        let mut harness = Harness::new(state);
        harness.register();
        let projection = harness.drain_normal();
        assert!(!projection.contains("353"), "{projection}");
        assert!(!projection.contains("366"), "{projection}");
    }

    #[tokio::test]
    async fn duplicate_nick_is_refused_and_registration_is_not_completed() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"NICK mobile\r\n").expect("handled");
        let refused = harness.drain_normal();
        assert!(
            refused.contains("433 * * :Nickname unavailable"),
            "{refused}"
        );
        harness
            .send(b"USER mobile 0 * :phone\r\n")
            .expect("handled");
        assert!(!harness.session.is_ready());
    }

    #[tokio::test]
    async fn user_traffic_is_generation_scoped_and_query_classed() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.register();
        harness.send(b"PRIVMSG #room :hello\r\n").expect("sent");
        harness.send(b"NAMES #room\r\n").expect("sent");
        let chat = harness.upstream_normal_rx.try_recv().expect("chat queued");
        let query = harness.upstream_normal_rx.try_recv().expect("query queued");
        assert_eq!(chat.class, IntentClass::NonReplayable);
        assert_eq!(chat.generation, ConnectionGeneration(7));
        assert_eq!(query.class, IntentClass::GenerationQuery);
        assert!(!chat.survives_disconnect());
        assert_eq!(chat.wire, b"PRIVMSG #room :hello\r\n");
    }

    #[tokio::test]
    async fn client_prefix_and_local_quit_are_explicit_dispositions() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        assert!(matches!(
            harness.send(b":spoof PRIVMSG #a :x\r\n"),
            Err(RuntimeError::Protocol)
        ));
        harness.register();
        assert_eq!(
            harness.send(b"QUIT :bye\r\n").unwrap(),
            DownstreamDisposition::LocalDetach
        );
    }

    #[tokio::test]
    async fn unsupported_command_is_reported_locally() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.register();
        harness.drain_normal();
        harness.send(b"OPER admin secret\r\n").expect("handled");
        let local = harness.drain_normal();
        assert!(local.contains("421 bot * :Unsupported command"), "{local}");
    }

    #[tokio::test]
    async fn client_ping_is_answered_on_the_control_queue() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.register();
        harness.drain_normal();
        harness.send(b"PING :local\r\n").expect("handled");
        let pong = harness.control_rx.try_recv().expect("pong queued");
        assert_eq!(pong, b":bouncer PONG bouncer :local\r\n");
    }

    #[tokio::test]
    async fn bounded_client_queues_report_overload_explicitly() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.register();
        // The registration projection already occupies part of the queue.
        let mut queued = harness.session.queue_depths().0;
        while harness
            .session
            .forward(b":srv PRIVMSG #room :flood\r\n".to_vec())
            .is_ok()
        {
            queued += 1;
            assert!(queued <= NORMAL_QUEUE_CAPACITY, "queue stayed bounded");
        }
        assert!(matches!(
            harness
                .session
                .forward(b":srv PRIVMSG #room :flood\r\n".to_vec()),
            Err(RuntimeError::QueueOverloaded)
        ));
        assert_eq!(harness.session.queue_depths(), (NORMAL_QUEUE_CAPACITY, 0));
    }

    #[tokio::test]
    async fn a_desired_channel_without_confirmation_is_never_projected() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        state.begin_desired_join("#room");
        state.apply_line(&Message::parse(b":srv 332 bot #room :subject\r\n").unwrap());
        state.apply_line(&Message::parse(b":srv 353 bot = #room :bot\r\n").unwrap());
        let mut harness = Harness::new(state);
        harness.register();
        let projection = harness.drain_normal();
        assert!(projection.contains("001 bot"), "{projection}");
        assert!(!projection.contains("JOIN #room"), "{projection}");
        assert!(!projection.contains("366"), "{projection}");
    }

    #[tokio::test]
    async fn registration_without_cap_negotiation_is_unaffected() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"NICK bot\r\n").expect("nick accepted");
        harness
            .send(b"USER bot 0 * :phone\r\n")
            .expect("user accepted");
        assert!(harness.session.is_ready());
        assert!(!harness.session.is_cap_negotiating());
        assert!(harness.drain_normal().contains("001 bot"));
    }

    #[tokio::test]
    async fn cap_ls_suspends_registration_until_cap_end() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"CAP LS 302\r\n").expect("cap ls accepted");
        assert!(harness.session.is_cap_negotiating());
        harness.send(b"NICK bot\r\n").expect("nick accepted");
        harness
            .send(b"USER bot 0 * :phone\r\n")
            .expect("user accepted");
        assert!(!harness.session.is_ready());
        let held = harness.drain_normal();
        assert!(held.contains("CAP * LS :"), "{held}");
        assert!(!held.contains("001"), "{held}");
        harness.send(b"CAP END\r\n").expect("cap end accepted");
        assert!(!harness.session.is_cap_negotiating());
        assert!(harness.session.is_ready());
        let projection = harness.drain_normal();
        assert!(projection.contains("001 bot"), "{projection}");
        // The projection is emitted exactly once.
        harness.send(b"CAP LIST\r\n").expect("cap list accepted");
        let after = harness.drain_normal();
        assert!(!after.contains("001"), "{after}");
    }

    #[tokio::test]
    async fn cap_end_before_registration_waits_for_both_nick_and_user() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&Message::parse(b":bot!u@h JOIN #room\r\n").unwrap());
        let mut harness = Harness::new(state);
        harness.send(b"CAP LS 302\r\n").expect("cap ls accepted");
        harness.send(b"NICK bot\r\n").expect("nick accepted");
        harness.send(b"CAP END\r\n").expect("cap end accepted");
        assert!(!harness.session.is_ready());
        harness
            .send(b"USER bot 0 * :phone\r\n")
            .expect("user accepted");
        assert!(harness.session.is_ready());
        assert!(harness.drain_normal().contains("JOIN #room"));
    }

    #[tokio::test]
    async fn user_before_nick_registers_once_after_cap_end() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"CAP LS 302\r\n").expect("cap ls accepted");
        harness
            .send(b"USER bot 0 * :phone\r\n")
            .expect("user accepted");
        assert!(!harness.session.is_ready());
        harness.send(b"NICK bot\r\n").expect("nick accepted");
        assert!(!harness.session.is_ready());
        harness.send(b"CAP END\r\n").expect("cap end accepted");
        assert!(harness.session.is_ready());
        let projection = harness.drain_normal();
        assert_eq!(projection.matches("001 bot").count(), 1, "{projection}");
    }

    #[tokio::test]
    async fn a_cap_req_naming_one_unavailable_capability_is_refused_as_a_whole() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"CAP LS 302\r\n").expect("cap ls accepted");
        // `echo-message` is requested upstream but is not served downstream, so a
        // request naming it must be refused rather than half-acknowledged.
        harness
            .send(b"CAP REQ :message-tags echo-message\r\n")
            .expect("cap req accepted");
        harness.send(b"NICK bot\r\n").expect("nick accepted");
        harness
            .send(b"USER bot 0 * :phone\r\n")
            .expect("user accepted");
        let local = harness.drain_normal();
        assert!(
            local.contains("CAP * NAK :Unsupported capabilities"),
            "{local}"
        );
        assert!(!local.contains("001"), "{local}");
        assert!(!harness.session.is_ready());
        // No client CAP traffic is forwarded upstream.
        assert!(harness.upstream_normal_rx.try_recv().is_err());
        harness.send(b"CAP END\r\n").expect("cap end accepted");
        assert!(harness.session.is_ready());
    }

    #[tokio::test]
    async fn repeated_and_invalid_cap_commands_are_deterministic() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"CAP LS 302\r\n").expect("first ls accepted");
        harness.send(b"CAP LS 302\r\n").expect("second ls accepted");
        harness.send(b"CAP LS\r\n").expect("third ls accepted");
        let listing = harness.drain_normal();
        assert_eq!(listing.matches("CAP * LS :").count(), 3, "{listing}");
        assert!(harness.session.is_cap_negotiating());
        harness.send(b"CAP END\r\n").expect("cap end accepted");
        harness
            .send(b"CAP END\r\n")
            .expect("repeat cap end accepted");
        assert!(!harness.session.is_cap_negotiating());
        // A client-sent ACK is not a negotiation step.
        harness
            .send(b"CAP ACK :message-tags\r\n")
            .expect("ack refused");
        let refused = harness.drain_normal();
        assert!(
            refused.contains("410 * CAP :Invalid CAP subcommand"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn late_cap_after_registration_cannot_unregister_the_client() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.register();
        harness.drain_normal();
        harness.send(b"CAP LS 302\r\n").expect("late ls accepted");
        assert!(harness.session.is_ready());
        assert!(!harness.session.is_cap_negotiating());
        let local = harness.drain_normal();
        assert!(local.contains("CAP bot LS :"), "{local}");
        assert!(!local.contains("001"), "{local}");
    }

    #[tokio::test]
    async fn client_ping_is_answered_while_cap_negotiation_is_pending() {
        let mut harness = Harness::new(NetworkState::new("bot", &[]));
        harness.send(b"CAP LS 302\r\n").expect("cap ls accepted");
        harness.drain_normal();
        harness.send(b"PING :local\r\n").expect("ping answered");
        assert_eq!(
            harness.control_rx.try_recv().expect("pong queued"),
            b":bouncer PONG bouncer :local\r\n"
        );
        assert!(!harness.session.is_ready());
    }

    #[test]
    fn cap_acknowledges_only_capabilities_the_session_really_implements() {
        let mut harness = Harness::new(NetworkState::new(
            "bot",
            &[DesiredChannelPolicy::attached("#room")],
        ));
        harness.register();

        // A supported capability is acknowledged and retained.
        harness
            .send(b"CAP REQ :draft/chathistory\r\n")
            .expect("accepted");
        let out = harness.drain_normal();
        assert!(
            out.contains("ACK :draft/chathistory"),
            "a supported capability must be acknowledged: {out}"
        );
        assert!(
            harness
                .session
                .negotiated()
                .contains(crate::chathistory::CHATHISTORY_CAPABILITY),
            "the acknowledged capability must be retained for projection decisions"
        );

        // An unsupported one is refused, and a mixed request is refused whole rather
        // than partially acknowledged.
        harness.send(b"CAP REQ :sasl/PLAIN\r\n").expect("accepted");
        assert!(harness.drain_normal().contains("NAK"));
        harness
            .send(b"CAP REQ :draft/chathistory sasl/PLAIN\r\n")
            .expect("accepted");
        assert!(
            harness.drain_normal().contains("NAK"),
            "a partially-supported request must be refused, not half-acknowledged"
        );
    }

    #[test]
    fn chathistory_isupport_is_advertised_only_to_a_negotiated_client() {
        // A client that never negotiated the capability must not be told it exists.
        let mut harness = Harness::new(NetworkState::new(
            "bot",
            &[DesiredChannelPolicy::attached("#room")],
        ));
        harness.register();
        harness.send(b"JOIN #room\r\n").expect("accepted");
        let out = harness.drain_normal();
        assert!(
            !out.contains("CHATHISTORY="),
            "an un-negotiated client must not receive the history ISUPPORT: {out}"
        );

        let mut harness = Harness::new(NetworkState::new(
            "bot",
            &[DesiredChannelPolicy::attached("#room")],
        ));
        harness
            .send(b"CAP REQ :draft/chathistory\r\n")
            .expect("accepted");
        // Registration only completes after the negotiation round is closed.
        harness.send(b"CAP END\r\n").expect("accepted");
        harness.drain_normal();
        harness.register();
        harness.send(b"JOIN #room\r\n").expect("accepted");
        let out = harness.drain_normal();
        assert!(
            out.contains("CHATHISTORY=50") && out.contains("MSGREFTYPES="),
            "a negotiated client must receive the truthful history ISUPPORT: {out}"
        );
    }

    #[test]
    fn dispositions_separate_local_teardown_from_upstream_teardown() {
        assert!(DownstreamDisposition::LocalDetach.is_local_only());
        assert!(DownstreamDisposition::Eof.is_local_only());
        assert!(DownstreamDisposition::QueueOverload.is_local_only());
        assert!(DownstreamDisposition::ProtocolViolation.is_local_only());
        assert!(DownstreamDisposition::ReadFailure.is_local_only());
        assert!(DownstreamDisposition::WriterFailure.is_local_only());
        assert!(!DownstreamDisposition::SupervisorStop.is_local_only());
        assert!(!DownstreamDisposition::UpstreamGenerationLost.is_local_only());
        assert_eq!(
            DownstreamDisposition::from_error(&RuntimeError::QueueOverloaded),
            DownstreamDisposition::QueueOverload
        );
        assert_eq!(
            DownstreamDisposition::from_error(&RuntimeError::Protocol),
            DownstreamDisposition::ProtocolViolation
        );
    }
}
