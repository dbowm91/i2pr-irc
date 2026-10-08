//! Persistent single-network upstream owner with zero-or-one attached local client.
//!
//! [`owner::NetworkOwner`] owns upstream registration, liveness, and observed IRC state
//! across connection generations, and it is the only Network owner in the production
//! build. A client attachment is never a precondition for the upstream session, and
//! client detach never ends the upstream generation.
//!
//! The superseded first-generation owner, `NetworkSupervisor`, is retained here under
//! `#[cfg(test)]` only. Corrective 019 gates it out of the shipped API rather than
//! deleting it, because at the time it was gated its qualification suite still covered
//! behaviour the production path did not: the SASL PLAIN handshake and the upstream
//! `QUIT` fence. Both are now covered against `owner::NetworkOwner` in
//! `crates/runtime/tests/corrective_019.rs`, so the legacy suite is redundant rather
//! than load-bearing and the legacy owner and its helpers can be deleted outright.
pub mod action;
pub mod admission;
pub mod bouncer_networks;
pub mod bouncerserv;
pub mod capability;
pub mod catalog;
pub mod chathistory;
pub mod config_snapshot;
pub mod control_session;
pub mod controller;
pub mod ctcp;
pub mod diagnostics;
pub mod downstream;
pub mod ircv3;
pub mod journal;
pub mod member;
pub mod owner;
pub mod playback;
pub mod presence;
pub mod projection;
pub mod reconnect;
pub mod resource;
pub mod routing;
pub mod search;
pub mod session;
pub mod state;

pub use admission::{AdmissionOutcome, DownstreamAdmission, PreparedSession};
pub use controller::{
    CONTROL_REQUEST_CAPACITY, ControlNetwork, ControlRequest, ControlSnapshot, DurableNetworks,
    RuntimeControlHandle, RuntimeController,
};
pub use owner::{ChannelPolicy, StoreChannelPolicy};
pub use reconnect::ReconnectScheduler;

use i2pr_irc_core::{ConnectionGeneration, ProviderError};
use std::{fmt, io, time::Duration};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, sync::mpsc, time::timeout};
use zeroize::Zeroize;

// Corrective 019: every import below is reachable only from the gated legacy
// supervisor, so they are gated too. Leaving them ungated would make the production
// build claim a dependency on modules it no longer contains.
#[cfg(test)]
use crate::{
    downstream::{DownstreamContext, DownstreamDisposition, DownstreamSession, SessionWriter},
    state::{LineOutcome, NetworkState},
};
#[cfg(test)]
use i2pr_irc_core::{
    ByteStream, ClientId, I2pEndpoint, I2pStreamProvider, LocalAcceptor, NetworkId,
};
#[cfg(test)]
use i2pr_irc_wire::{LineDecoder, Message, TagDirection};
#[cfg(test)]
use std::{
    collections::BTreeSet,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[cfg(test)]
use tokio::{
    io::AsyncReadExt,
    sync::watch,
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};
#[cfg(test)]
use zeroize::Zeroizing;

pub use crate::state::{
    JOIN_FAILURE_NUMERICS, JoinAttempt, MAX_CHANNEL_NAME_BYTES, MAX_CHANNELS, MAX_ISUPPORT_TOKENS,
    MAX_MEMBERS_PER_CHANNEL, MAX_TOTAL_MEMBERS,
};

pub const NORMAL_QUEUE_CAPACITY: usize = 64;
pub const CONTROL_QUEUE_CAPACITY: usize = 8;
/// Ceiling on acquiring one upstream stream from [`I2pStreamProvider`].
///
/// Renamed from `CONNECT_TIMEOUT` because "connect" stopped meaning generic TCP when the
/// SAM adapter landed: the wait now covers a router-side cold path that can include
/// building a lease set and a tunnel pool. 120 s was shorter than that path, so a
/// working router could be reported as failing.
pub const PROVIDER_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(300);
pub const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(180);
pub const CAP_SASL_TIMEOUT: Duration = Duration::from_secs(90);
pub const MAX_CREDENTIAL_BYTES: usize = 1024;
/// Ceiling on releasing one Network's provider scope.
///
/// Deliberately far below [`PROVIDER_ACQUIRE_TIMEOUT`]: release runs on the deletion and
/// shutdown paths, where the caller is already waiting for the Network to go away.
/// Reusing the connect budget would let a wedged router adapter hold a delete open for
/// two minutes per Network, and shutdown has no timeout of its own at all.
pub const PROVIDER_RELEASE_TIMEOUT: Duration = Duration::from_secs(15);
pub const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
pub const LIVENESS_INTERVAL: Duration = Duration::from_secs(60);
pub const LIVENESS_DEADLINE: Duration = Duration::from_secs(120);
/// Delay before retrying a failed local attach. A local accept failure never
/// affects the upstream generation, so it must not spin.
pub const LOCAL_ATTACH_RETRY: Duration = Duration::from_millis(50);

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
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("provider failure: {0}")]
    Provider(#[from] ProviderError),
    /// The process-wide resource ledger already tracks the supervised-Network ceiling, so
    /// this owner cannot be accounted for. Reported as an overload because that is what
    /// it is: the process is supervising as much as it is permitted to.
    #[error("bounded resource ledger overloaded")]
    LedgerRefused(#[from] crate::resource::LedgerRefused),
    #[error("operation timed out")]
    Timeout,
    #[error("protocol failure")]
    Protocol,
    #[error("registration rejected")]
    Registration,
    /// Every bounded fallback nick was refused, or the sequence produced something that
    /// is not a legal nick.
    ///
    /// Retryable after a dedicated collision cooldown. The bounded sequence is retried
    /// only after the occupancy may have changed.
    #[error("preferred nick exhausted every bounded fallback")]
    NickExhausted,
    #[error("invalid network configuration")]
    InvalidConfig,
    /// No durable or live Network carries the requested identity.
    ///
    /// Distinct from [`RuntimeError::InvalidConfig`], which reports a Network that exists
    /// and whose configuration was rejected. Administration has to tell those apart: the
    /// first is a typo and the second is a broken record, and answering both with the same
    /// message would teach an Operator that the bouncer's answer is not trustworthy.
    #[error("no such network")]
    UnknownNetwork,
    #[error("bounded output queue overloaded")]
    QueueOverloaded,
    #[error("connection generation space exhausted")]
    GenerationExhausted,
    #[error("I/O failure")]
    Io(#[from] std::io::Error),
    #[error("stopped")]
    Stopped,
    /// Two durable buffer identities would merge under the requested casemapping.
    /// The journal fails closed rather than silently merging two histories.
    #[error("ambiguous durable buffer target: {0}")]
    AmbiguousBuffer(String),
}
#[derive(Clone)]
pub struct Secret(String);
impl Secret {
    pub fn new(v: String) -> Self {
        Self(v)
    }

    /// Hands the value to the durable secret type.
    ///
    /// The only way out of this type, and deliberately not `expose`-shaped: the value
    /// leaves as a `StoredSecret`, which already redacts and zeroes, so no caller ends
    /// up holding the credential in an ordinary `String` it might format later.
    pub fn into_stored(mut self) -> i2pr_irc_store::StoredSecret {
        // `Secret` has a `Drop`, so the field cannot simply be moved out. Taking it
        // leaves this instance empty -- its `Drop` then zeroes an empty string -- and
        // hands ownership to `StoredSecret`, which zeroes on its own drop. Exactly one
        // live copy exists at a time, and both owners zero what they hold.
        let value = std::mem::take(&mut self.0);
        i2pr_irc_store::StoredSecret::new(value)
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
impl Eq for Secret {}
impl PartialEq for Secret {
    /// Compares values, not renderings.
    ///
    /// This exists so a type holding a credential can still derive `PartialEq`, which
    /// keeps test assertions and structural comparisons possible. Equality reveals
    /// *whether* two secrets match and nothing about either of them, and the derived
    /// `Debug` above is what guarantees no diagnostic can do better than that.
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize()
    }
}
/// Upstream configuration for the gated legacy supervisor.
///
/// [`owner::NetworkOwner`] takes its configuration from the durable
/// [`i2pr_irc_store::NetworkRecord`] instead, so this type has no production reader.
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct UpstreamConfig {
    pub endpoint: I2pEndpoint,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub sasl: Option<(String, Secret)>,
    pub desired_channels: Vec<String>,
}
#[cfg(test)]
impl UpstreamConfig {
    /// Conservative pre-connection validation. Live channel-type interpretation
    /// uses the server-advertised `CHANTYPES` set instead.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let token = |value: &str, max: usize| {
            !value.is_empty()
                && value.len() <= max
                && value.bytes().all(|b| b.is_ascii_graphic() && b != b':')
        };
        if !valid_client_nick(self.nick.as_bytes())
            || !token(&self.username, 64)
            || self.realname.is_empty()
            || self.realname.len() > 256
            || self
                .realname
                .bytes()
                .any(|b| b == 0 || b == b'\r' || b == b'\n')
            || self.desired_channels.len() > MAX_CHANNELS
        {
            return Err(RuntimeError::InvalidConfig);
        }
        for channel in &self.desired_channels {
            if channel.len() > MAX_CHANNEL_NAME_BYTES
                || channel.len() < 2
                || !state::ChanTypes::default().is_channel(channel)
                || channel.bytes().any(|b| {
                    b.is_ascii_whitespace() || matches!(b, b',' | b':' | 0 | b'\r' | b'\n')
                })
            {
                return Err(RuntimeError::InvalidConfig);
            }
        }
        if let Some((user, password)) = &self.sasl
            && (user.is_empty()
                || user.len() > 256
                || user.bytes().any(|b| !b.is_ascii_graphic())
                || password.0.len() > MAX_CREDENTIAL_BYTES
                || password.0.bytes().any(|b| b == 0))
        {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(())
    }
}

/// Diagnostic projection of the legacy network owner. It never carries message
/// payloads, endpoints, or credentials.
///
/// This is not the projection the running bouncer publishes;
/// [`owner::NetworkSnapshot`] is. Two same-named projections were a hazard for exactly
/// the reason Corrective 019 gates this one: a reader could not tell which owner a
/// diagnostic belonged to.
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub struct NetworkSnapshot {
    pub phase: Option<Phase>,
    pub generation: Option<ConnectionGeneration>,
    pub nick: Option<String>,
    pub channels: Vec<String>,
    pub reconnect_attempt: u32,
    pub current_backoff_ms: Option<u64>,
    pub upstream_normal_queue_depth: usize,
    pub upstream_control_queue_depth: usize,
    pub downstream_normal_queue_depth: usize,
    pub downstream_control_queue_depth: usize,
    pub downstream_attached: bool,
    /// Cumulative attached clients observed by this supervisor.
    pub downstream_session_total: u64,
    /// Cumulative finished downstream sessions, including every detach reason.
    pub downstream_detach_total: u64,
    /// Disposition class of the most recent downstream session end.
    pub downstream_last_disposition: Option<&'static str>,
    pub upstream_events_seen: u64,
    pub last_error: Option<&'static str>,
    /// Desired channels this generation has written a JOIN for but the server has
    /// neither confirmed nor rejected. Never membership.
    pub pending_joins: Vec<String>,
    /// Desired channels this generation failed to join, with the numeric that
    /// refused them. Operator intent is unchanged by any entry here.
    pub rejected_joins: Vec<(String, &'static str)>,
}

#[cfg(test)]
type AcceptFuture<'a, A> = Pin<
    Box<
        dyn Future<Output = Result<(ClientId, <A as LocalAcceptor>::Stream), ProviderError>>
            + Send
            + 'a,
    >,
>;

/// The superseded first-generation Network owner, retained for its qualification suite.
///
/// Corrective 019 gates it out of the production build rather than deleting it. Its
/// `serve` calls [`I2pStreamProvider::connect`] directly, outside both
/// [`reconnect::ReconnectScheduler`] admission and [`resource::ResourceLedger`]
/// accounting, so shipping a second such owner in the public API is precisely what
/// `ADR-0001` forbids. Gating makes "no production code calls the legacy supervisor" the
/// stronger and simpler claim: in the production build, it does not exist at all.
#[cfg(test)]
pub struct NetworkSupervisor<P> {
    provider: P,
    config: UpstreamConfig,
    /// The only Network this legacy supervisor drives.
    ///
    /// [`NetworkOwner`] is the real supervisor and holds a genuine Network. This type
    /// exists for the in-process generation and registration tests that predate the
    /// catalog, and it has no catalog entry to name. A fixed scope keeps those tests
    /// compiling against the same scoped provider contract rather than inventing an
    /// unowned or shared scope for them.
    network: NetworkId,
    snapshot: watch::Sender<NetworkSnapshot>,
}
#[cfg(test)]
impl<P: I2pStreamProvider> NetworkSupervisor<P> {
    pub fn new(provider: P, config: UpstreamConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let (snapshot, _) = watch::channel(NetworkSnapshot::default());
        snapshot.send_modify(|state| state.phase = Some(Phase::Idle));
        Ok(Self {
            provider,
            config,
            network: NetworkId(1),
            snapshot,
        })
    }
    pub fn subscribe_snapshot(&self) -> watch::Receiver<NetworkSnapshot> {
        self.snapshot.subscribe()
    }
    fn set_phase(&self, phase: Phase, generation: Option<ConnectionGeneration>) {
        self.snapshot.send_modify(|state| {
            state.phase = Some(phase);
            state.generation = generation;
            if matches!(phase, Phase::Idle | Phase::Backoff | Phase::Stopped) {
                state.downstream_attached = false;
            }
        });
    }
    fn publish_state(&self, state: &NetworkState) {
        self.snapshot.send_modify(|snapshot| {
            snapshot.nick = Some(state.nick.clone());
            // Observed membership only, so a diagnostics reader cannot mistake an
            // outstanding or rejected attempt for a live channel.
            snapshot.channels = state.joined_channels();
            snapshot.pending_joins = state.pending_joins();
            snapshot.rejected_joins = state.rejected_joins();
        });
    }

    /// Owns one upstream Network across client attachment changes. Each failed
    /// generation is discarded before bounded reconnect backoff; user traffic is
    /// never retained for replay. Local client detach never ends a generation.
    pub async fn serve<A: LocalAcceptor + 'static>(
        &self,
        acceptor: &A,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), RuntimeError>
    where
        A::Stream: 'static,
    {
        let mut backoff = Backoff {
            attempt: 0,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(300),
            jitter_percent: 20,
        };
        let mut generation = 0u64;
        loop {
            if *stop.borrow() {
                self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                return Ok(());
            }
            generation = generation
                .checked_add(1)
                .ok_or(RuntimeError::GenerationExhausted)?;
            self.set_phase(Phase::Connecting, Some(ConnectionGeneration(generation)));
            let connection = tokio::select! {
                _ = stopped(&mut stop) => { self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation))); return Ok(()) },
                result = timeout(PROVIDER_ACQUIRE_TIMEOUT, self.provider.connect(self.network, &self.config.endpoint)) => {
                    match result { Ok(Ok(stream)) => Ok(stream), Ok(Err(e)) => Err(RuntimeError::Provider(e)), Err(_) => Err(RuntimeError::Timeout) }
                }
            };
            let result = match connection {
                Ok(upstream) => {
                    self.set_phase(Phase::Registering, Some(ConnectionGeneration(generation)));
                    let online_started = Instant::now();
                    let result = self
                        .run_generation(
                            upstream,
                            ConnectionGeneration(generation),
                            acceptor,
                            &mut stop,
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
                                state.downstream_attached = false;
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
                state.downstream_attached = false;
                state.reconnect_attempt = backoff.attempt;
                state.current_backoff_ms = Some(delay.as_millis().min(u64::MAX as u128) as u64);
                state.last_error = Some(error_class(&result));
            });
            tokio::select! { _ = stopped(&mut stop) => { self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation))); return Ok(()) }, _ = tokio::time::sleep(delay) => {} }
        }
    }

    /// Runs one upstream generation. Registration, liveness, observed state, and
    /// the single upstream writer task are owned here for the whole generation.
    /// The downstream attachment owner is zero-or-one and is data, not control.
    async fn run_generation<'a, S, A>(
        &self,
        upstream: S,
        generation: ConnectionGeneration,
        acceptor: &'a A,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), RuntimeError>
    where
        S: ByteStream + 'static,
        A: LocalAcceptor + 'a,
        A::Stream: 'static,
    {
        let (mut ur, mut uw) = tokio::io::split(upstream);
        let desired: Vec<crate::state::DesiredChannelPolicy> = self
            .config
            .desired_channels
            .iter()
            .map(|target| crate::state::DesiredChannelPolicy::attached(target.clone()))
            .collect();
        let mut state = NetworkState::new(&self.config.nick, &desired);
        // One decoder spans registration and the online phase so a line that
        // arrives in the same read as `001` is not lost.
        let mut udec = LineDecoder::default();
        let mut ubuf = [0u8; 2048];
        let mut welcomed = false;
        let mut cap_finished = false;
        let mut offered = BTreeSet::new();
        let mut sasl_plain_offered = false;
        let mut requested = false;
        let mut sasl_active = false;
        let registration = async {
            send(&mut uw, "CAP LS 302\r\n").await?;
            send(
                &mut uw,
                &format!(
                    "NICK {}\r\nUSER {} 0 * :{}\r\n",
                    self.config.nick, self.config.username, self.config.realname
                ),
            )
            .await?;
            while !(welcomed && cap_finished) {
                let n = if cap_finished {
                    ur.read(&mut ubuf).await?
                } else {
                    timeout(CAP_SASL_TIMEOUT, ur.read(&mut ubuf))
                        .await
                        .map_err(|_| RuntimeError::Timeout)?
                        .map_err(RuntimeError::Io)?
                };
                if n == 0 {
                    return Err(RuntimeError::Protocol);
                }
                for line in udec.push(&ubuf[..n]) {
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
                                if self.config.sasl.is_some()
                                    && (!offered.contains("sasl") || !sasl_plain_offered)
                                {
                                    return Err(RuntimeError::Registration);
                                }
                                let wanted: Vec<&str> =
                                    if self.config.sasl.is_some() && offered.contains("sasl") {
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
                            if self.config.sasl.is_some() && !sasl_accepted {
                                return Err(RuntimeError::Registration);
                            }
                            if self.config.sasl.is_some() {
                                send(&mut uw, "AUTHENTICATE PLAIN\r\n").await?;
                                sasl_active = true;
                            } else {
                                send(&mut uw, "CAP END\r\n").await?;
                                cap_finished = true;
                            }
                        }
                        "CAP"
                            if params.iter().any(|p| p == "NAK") && self.config.sasl.is_some() =>
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
                                .config
                                .sasl
                                .as_ref()
                                .ok_or(RuntimeError::Registration)?;
                            let raw = Zeroizing::new(format!("\0{}\0{}", user, password.0));
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
            // Desired state is re-sent only after a fresh registration, never
            // carried across a generation boundary. Writing a JOIN proves nothing
            // about membership: each attempt is recorded as outstanding and only an
            // authoritative self JOIN closes it as confirmed.
            for channel in self.config.desired_channels.iter().take(MAX_CHANNELS) {
                state.begin_desired_join(channel);
                send(&mut uw, &format!("JOIN {channel}\r\n")).await?;
            }
            Ok::<(), RuntimeError>(())
        };
        tokio::select! {
            _ = stopped(stop) => return Err(RuntimeError::Stopped),
            result = timeout(REGISTRATION_TIMEOUT, registration) => result.unwrap_or(Err(RuntimeError::Timeout))?,
        };
        // The generation is online on its own: no local client is required.
        self.snapshot.send_modify(|snapshot| {
            snapshot.phase = Some(Phase::Online);
            snapshot.generation = Some(generation);
            snapshot.downstream_attached = false;
            snapshot.last_error = None;
            snapshot.current_backoff_ms = None;
        });
        self.publish_state(&state);

        let (control_tx, mut control_rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (normal_tx, mut normal_rx) = mpsc::channel::<OutboundIntent>(NORMAL_QUEUE_CAPACITY);
        // After a shutdown fence is raised, no user traffic reaches the stream.
        let fence = Arc::new(AtomicBool::new(false));
        let mut upstream_writer = JoinSet::new();
        {
            let fence = fence.clone();
            upstream_writer.spawn(async move {
                loop {
                    let next = next_intent_frame(&mut control_rx, &mut normal_rx).await;
                    match next {
                        Some(Err(bytes)) => write_frame(&mut uw, &bytes).await?,
                        Some(Ok(intent))
                            if intent.generation == generation
                                && !fence.load(Ordering::Acquire) =>
                        {
                            write_frame(&mut uw, &intent.wire).await?
                        }
                        Some(Ok(_)) => continue,
                        None => return Ok::<(), std::io::Error>(()),
                    }
                }
            });
        }

        let mut session: Option<DownstreamSession<A::Stream>> = None;
        let mut session_writer: Option<SessionWriter> = None;
        let mut accept_fut: Option<AcceptFuture<'_, A>> = None;
        let mut accept_retry_at: Option<Instant> = None;
        let mut probe = tokio::time::interval(LIVENESS_INTERVAL);
        probe.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut awaiting_pong: Option<(Instant, String)> = None;
        let mut dbuf = [0u8; 2048];
        let outcome = loop {
            // A pending retry delay keeps the acceptor unpolled, so a local accept
            // failure cannot spin the owner.
            if session.is_none() && accept_fut.is_none() && accept_retry_at.is_none() {
                accept_fut = Some(Box::pin(acceptor.accept()));
            }
            let accept = next_accept::<A>(&mut accept_fut);
            let retry = async {
                match accept_retry_at {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            };
            let client_read = read_client(&mut session, &mut dbuf);
            let client_exit = client_writer_exit(&mut session_writer);
            tokio::select! {
                _ = stopped(stop) => break Ok(()),
                _ = retry => { accept_retry_at = None; }
                accepted = accept => {
                    accept_fut = None;
                    match accepted {
                        Some(Ok((client, stream))) => {
                            let (read, write) = tokio::io::split(stream);
                            let (client_control, client_normal, writer) = downstream::spawn_raw_writer(write);
                            session = Some(DownstreamSession::new(client, read, client_control, client_normal));
                            session_writer = Some(writer);
                            self.snapshot.send_modify(|snapshot| {
                                snapshot.downstream_attached = true;
                                snapshot.downstream_session_total = snapshot.downstream_session_total.saturating_add(1);
                                snapshot.downstream_normal_queue_depth = 0;
                                snapshot.downstream_control_queue_depth = 0;
                            });
                        }
                        // A local accept failure never affects the upstream generation.
                        Some(Err(_)) => {
                            accept_retry_at = Some(Instant::now() + LOCAL_ATTACH_RETRY);
                            self.snapshot.send_modify(|snapshot| snapshot.last_error = Some("accept"));
                        }
                        None => {}
                    }
                }
                read = client_read => {
                    let Some(result) = read else { continue };
                    match result {
                        Err(_) => self.detach_session(&mut session, &mut session_writer, DownstreamDisposition::ReadFailure).await,
                        Ok(0) => self.detach_session(&mut session, &mut session_writer, DownstreamDisposition::Eof).await,
                        Ok(count) => {
                            let ingested = session.as_mut().map(|session| session.ingest(&dbuf[..count]));
                            let lines = match ingested {
                                Some(Ok(lines)) => lines,
                                Some(Err(error)) => {
                                    let disposition = DownstreamDisposition::from_error(&error);
                                    self.detach_session(&mut session, &mut session_writer, disposition).await;
                                    continue;
                                }
                                None => continue,
                            };
                            let context = DownstreamContext {
                                negotiated: session
                                    .as_ref()
                                    .map(|session| session.negotiated())
                                    .unwrap_or_default(),
                                generation,
                                state: &state,
                                upstream_control: &control_tx,
                                upstream_normal: &normal_tx,
                            };
                            let mut disposition = DownstreamDisposition::Attached;
                            for line in lines {
                                let outcome = session.as_mut().map(|session| session.handle_line(&line, &context));
                                match outcome {
                                    Some(Ok(DownstreamDisposition::Attached)) => {}
                                    Some(Ok(other)) => { disposition = other; break; }
                                    Some(Err(error)) => { disposition = DownstreamDisposition::from_error(&error); break; }
                                    None => break,
                                }
                            }
                            if disposition != DownstreamDisposition::Attached {
                                self.detach_session(&mut session, &mut session_writer, disposition).await;
                            }
                        }
                    }
                }
                exited = client_exit => {
                    let result = match exited { Some(result) => result, None => continue };
                    let disposition = match result {
                        Ok(()) => DownstreamDisposition::WriterFailure,
                        Err(_) => DownstreamDisposition::WriterFailure,
                    };
                    self.detach_session(&mut session, &mut session_writer, disposition).await;
                }
                writer = upstream_writer.join_next() => {
                    match writer {
                        Some(Ok(Ok(()))) => break Err(RuntimeError::Protocol),
                        Some(Ok(Err(error))) => break Err(RuntimeError::Io(error)),
                        Some(Err(error)) => break Err(RuntimeError::Io(io::Error::other(error))),
                        None => break Err(RuntimeError::Protocol),
                    }
                }
                _ = probe.tick() => {
                    if awaiting_pong.as_ref().is_some_and(|(since, _)| since.elapsed() >= LIVENESS_DEADLINE) {
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
                count = ur.read(&mut ubuf) => {
                    let count = match count { Ok(count) => count, Err(error) => break Err(RuntimeError::Io(error)) };
                    if count == 0 { break Err(RuntimeError::Protocol); }
                    let mut failure = None;
                    for line in udec.push(&ubuf[..count]) {
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
                                Some(expected)
                                    if message
                                        .params
                                        .last()
                                        .is_some_and(|token| *token == expected.as_bytes()) =>
                                {
                                    awaiting_pong = None;
                                }
                                _ => {
                                    failure = Some(RuntimeError::Protocol);
                                    break;
                                }
                            }
                        }
                        match apply_upstream_line(&mut state, session.as_ref(), &control_tx, raw, &message) {
                            Ok(Some(disposition)) => {
                                self.detach_session(&mut session, &mut session_writer, disposition).await;
                            }
                            Ok(None) => {}
                            Err(error) => { failure = Some(error); break; }
                        }
                    }
                    self.publish_state(&state);
                    if let Some(error) = failure { break Err(error); }
                }
            }
            let (downstream_normal, downstream_control) = session
                .as_ref()
                .map_or((0, 0), DownstreamSession::queue_depths);
            self.snapshot.send_modify(|snapshot| {
                snapshot.upstream_normal_queue_depth = NORMAL_QUEUE_CAPACITY - normal_tx.capacity();
                snapshot.upstream_control_queue_depth =
                    CONTROL_QUEUE_CAPACITY - control_tx.capacity();
                snapshot.downstream_normal_queue_depth = downstream_normal;
                snapshot.downstream_control_queue_depth = downstream_control;
            });
        };

        // Deterministic teardown: every spawned task is owned here and either
        // joined after the final upstream QUIT or deliberately aborted.
        drop(session.take());
        if let Some(writer) = session_writer.take() {
            writer.shutdown().await;
        }
        match outcome {
            Ok(()) => {
                // Explicit stop is the only local action that deliberately sends
                // upstream QUIT, and it is sent at most once.
                fence.store(true, Ordering::Release);
                let _ = control_tx.try_send(b"QUIT :Bouncer shutting down\r\n".to_vec());
                drop(control_tx);
                drop(normal_tx);
                while let Some(result) = upstream_writer.join_next().await {
                    // Shutdown is best effort; the QUIT is already queued first.
                    let _ = result;
                }
                Ok(())
            }
            Err(error) => {
                drop(control_tx);
                drop(normal_tx);
                upstream_writer.abort_all();
                while upstream_writer.join_next().await.is_some() {}
                Err(error)
            }
        }
    }

    /// Ends one downstream session without touching the upstream generation.
    async fn detach_session<D: ByteStream>(
        &self,
        session: &mut Option<DownstreamSession<D>>,
        writer: &mut Option<SessionWriter>,
        disposition: DownstreamDisposition,
    ) {
        let finished = session.take();
        let owned_writer = writer.take();
        drop(finished);
        if let Some(owned_writer) = owned_writer {
            owned_writer.shutdown().await;
        }
        self.snapshot.send_modify(|snapshot| {
            snapshot.downstream_attached = false;
            snapshot.downstream_detach_total = snapshot.downstream_detach_total.saturating_add(1);
            snapshot.downstream_last_disposition = Some(disposition.class());
            snapshot.downstream_normal_queue_depth = 0;
            snapshot.downstream_control_queue_depth = 0;
        });
    }
}

// Corrective 019: the helpers below are reachable only from the gated legacy
// supervisor, so they are gated too. They are gated rather than deleted because the
// legacy supervisor is gated rather than deleted and its qualification suite still
// calls them: deleting them would break a live suite in order to remove code the
// production build no longer contains anyway.
/// Applies one upstream line to observed state and, when a client is registered,
/// forwards it. Returns a disposition when only the client must detach.
#[cfg(test)]
fn apply_upstream_line<D: ByteStream>(
    state: &mut NetworkState,
    session: Option<&DownstreamSession<D>>,
    control_tx: &mpsc::Sender<Vec<u8>>,
    raw: Vec<u8>,
    message: &Message,
) -> Result<Option<DownstreamDisposition>, RuntimeError> {
    match state.apply_line(message) {
        LineOutcome::Quiet => {}
        LineOutcome::ReplyPong(token) => {
            queue_control(control_tx, &format!("PONG :{token}\r\n"))?;
        }
        LineOutcome::Malformed => return Err(RuntimeError::Protocol),
    }
    // CTCP privacy policy is applied before any client sees the frame, so a metadata
    // probe never reaches a client that would answer it with its own hostname and
    // software. This block is a *duplicate* of the enforcement the production owner
    // performs at `owner.rs:1563`; it shares the `ctcp` module with it, not the
    // enforcement site. Corrective 019 corrected this comment, which claimed a shared
    // guard where two separate copies exist.
    let direction = match &message.command[..] {
        [b'N', b'O', b'T', b'I', b'C', b'E'] => crate::ctcp::CtcpDirection::Reply,
        _ => crate::ctcp::CtcpDirection::Query,
    };
    match crate::ctcp::inbound_action(&crate::ctcp::classify(message, direction)) {
        crate::ctcp::InboundAction::FanOut => {}
        crate::ctcp::InboundAction::Suppress => return Ok(None),
        crate::ctcp::InboundAction::AnswerPing(_) => {
            let ctcp = crate::ctcp::classify(message, direction);
            let token = crate::ctcp::ping_reply_text(&ctcp).unwrap_or_default();
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
                .unwrap_or_else(|| state.nick.clone());
            queue_control(
                control_tx,
                &format!(":bouncer NOTICE {target} :\u{1}PING {token}\u{1}\r\n"),
            )?;
            return Ok(None);
        }
    }
    let Some(session) = session.filter(|session| session.is_ready()) else {
        return Ok(None);
    };
    // Message-tag semantics are per session: a client that negotiated `message-tags`
    // receives tags, and a client that did not never sees one.
    let wants_tags = session.capabilities().negotiated_tags();
    let outgoing = if message.tags.is_empty() || !wants_tags {
        if message.tags.is_empty() {
            raw
        } else {
            let mut untagged = message.clone();
            untagged.tags.clear();
            untagged.encode().map_err(|_| RuntimeError::Protocol)?
        }
    } else {
        message.encode().map_err(|_| RuntimeError::Protocol)?
    };
    match session.forward(outgoing) {
        Ok(()) => Ok(None),
        Err(error) => Ok(Some(DownstreamDisposition::from_error(&error))),
    }
}

#[cfg(test)]
async fn next_accept<'a, A: LocalAcceptor>(
    slot: &mut Option<AcceptFuture<'a, A>>,
) -> Option<Result<(ClientId, A::Stream), ProviderError>> {
    match slot {
        Some(future) => Some(future.as_mut().await),
        None => std::future::pending().await,
    }
}

#[cfg(test)]
async fn read_client<D: ByteStream>(
    session: &mut Option<DownstreamSession<D>>,
    buf: &mut [u8],
) -> Option<io::Result<usize>> {
    match session {
        Some(session) => Some(session.read(buf).await),
        None => std::future::pending().await,
    }
}

#[cfg(test)]
async fn client_writer_exit(session_writer: &mut Option<SessionWriter>) -> Option<io::Result<()>> {
    match session_writer {
        Some(writer) => Some(writer.wait().await),
        None => std::future::pending().await,
    }
}

/// Applies a bounded deadline to a future that would otherwise park indefinitely.
///
/// This is `tokio::time::timeout` over the runtime clock, and the runtime clock is
/// pausable. Under `#[tokio::test(start_paused = true)]` the runtime auto-advances
/// whenever every task is parked, so this deadline *does* expire without the test ever
/// calling `advance()`. Corrective 019 measured a parked 120s deadline firing in eight
/// microseconds of real time.
///
/// That is the intended behaviour for a real deadline — auto-advance standing in for
/// elapsed time is exactly what a pausable runtime is for — but it is not what a test
/// that intends to exercise *backoff* wants. `owner.rs` therefore reaches backoff through
/// `Backoff::next_delay` and the scheduler's own timer rather than through a timeout. A
/// test that asserts a retry schedule must advance time in a loop: a single
/// `advance(600s)` fires one re-armed timer, not sixty thousand of them.
pub(crate) async fn timeout_bounded<T>(
    duration: Duration,
    future: impl std::future::Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(duration, future).await
}

/// Parks until the stop flag is set or the sender is dropped.
///
/// Gated with the legacy supervisor for the same reason as `queue_control`: `owner.rs`
/// has its own private copy, so this one no longer has a production reader.
#[cfg(test)]
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
pub(crate) fn error_class(error: &Result<(), RuntimeError>) -> &'static str {
    match error {
        Err(RuntimeError::Provider(_)) => "provider",
        Err(RuntimeError::Timeout) => "timeout",
        Err(RuntimeError::Protocol) => "protocol",
        Err(RuntimeError::Registration) => "registration",
        // A collision that exhausted every bounded fallback is reported as its own
        // class, not folded into "registration": the credentials were fine and the
        // Operator's nick is what the server refused.
        Err(RuntimeError::NickExhausted) => "nick-collision-retry",
        Err(RuntimeError::Io(_)) => "io",
        Err(RuntimeError::Stopped) => "stopped",
        Err(RuntimeError::InvalidConfig) => "configuration",
        Err(RuntimeError::UnknownNetwork) => "unknown-network",
        Err(RuntimeError::QueueOverloaded) => "queue-overload",
        Err(RuntimeError::GenerationExhausted) => "generation-exhausted",
        Err(RuntimeError::AmbiguousBuffer(_)) => "ambiguous-buffer",
        Err(RuntimeError::LedgerRefused(_)) => "ledger-overload",
        Ok(()) => "stopped",
    }
}
pub(crate) fn valid_client_nick(bytes: &[u8]) -> bool {
    let special = |b: u8| {
        matches!(
            b,
            b'[' | b']' | b'\\' | b'`' | b'_' | b'^' | b'{' | b'|' | b'}'
        )
    };
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_alphabetic() || special(bytes[0]))
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || special(*b) || *b == b'-')
}
/// Queues one bounded control frame, refusing anything that is not a complete line.
///
/// Corrective 019 gates this behind `#[cfg(test)]`: after the legacy supervisor was
/// gated, `owner.rs` and `downstream.rs` turned out to each hold their own private copy,
/// so this one had no production reader left. It is kept rather than deleted because the
/// control-vs-normal overflow test still exercises it directly. Consolidating the three
/// copies is deliberately not part of 019; it is a de-duplication, not a closure of a
/// finding, and it would touch shipping code on no evidence that the copies disagree.
#[cfg(test)]
pub(crate) fn queue_control(
    sender: &mpsc::Sender<Vec<u8>>,
    line: &str,
) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(line.as_bytes().to_vec())
        .map_err(|_| RuntimeError::QueueOverloaded)
}
/// Next frame for a session writer. Control outranks normal so a saturated user
/// queue can never delay a keepalive answer.
pub(crate) async fn next_queued_frame(
    control: &mut mpsc::Receiver<downstream::QueuedFrame>,
    normal: &mut mpsc::Receiver<downstream::QueuedFrame>,
) -> Option<downstream::QueuedFrame> {
    // A closed queue means "no more frames from this producer", not "the writer is
    // finished". Conflating the two discards whatever the *other* queue still holds --
    // and the last frame on the other queue is very often the one that explains why the
    // connection is closing. The writer ends only when both producers are done.
    let mut control_open = true;
    let mut normal_open = true;
    loop {
        if !control_open && !normal_open {
            return None;
        }
        tokio::select! {
            biased;
            frame = control.recv(), if control_open => match frame {
                Some(frame) => return Some(frame),
                None => control_open = false,
            },
            frame = normal.recv(), if normal_open => match frame {
                Some(frame) => return Some(frame),
                None => normal_open = false,
            },
        }
    }
}

/// Next raw upstream frame. Upstream traffic is already framed bytes, so it carries
/// no per-frame acknowledgment state.
pub(crate) async fn next_upstream_frame(
    control: &mut mpsc::Receiver<Vec<u8>>,
    normal: &mut mpsc::Receiver<Vec<u8>>,
) -> Option<Vec<u8>> {
    // Same rule as `next_queued_frame`: one closed producer does not end the writer.
    let mut control_open = true;
    let mut normal_open = true;
    loop {
        if !control_open && !normal_open {
            return None;
        }
        tokio::select! {
            biased;
            frame = control.recv(), if control_open => match frame {
                Some(frame) => return Some(frame),
                None => control_open = false,
            },
            frame = normal.recv(), if normal_open => match frame {
                Some(frame) => return Some(frame),
                None => normal_open = false,
            },
        }
    }
}
#[cfg(test)]
async fn next_intent_frame(
    control: &mut mpsc::Receiver<Vec<u8>>,
    normal: &mut mpsc::Receiver<OutboundIntent>,
) -> Option<Result<OutboundIntent, Vec<u8>>> {
    tokio::select! { biased; command = control.recv() => command.map(Err), command = normal.recv() => command.map(Ok) }
}
#[cfg(test)]
async fn send<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, s: &str) -> Result<(), io::Error> {
    write_frame(w, s.as_bytes()).await
}
pub(crate) async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
) -> Result<(), io::Error> {
    timeout(STREAM_WRITE_TIMEOUT, async {
        w.write_all(bytes).await?;
        w.flush().await
    })
    .await
    .unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "bounded stream write timed out",
        ))
    })
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntentClass {
    Control,
    DesiredState,
    NonReplayable,
    GenerationQuery,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundIntent {
    pub generation: ConnectionGeneration,
    pub class: IntentClass,
    pub wire: Vec<u8>,
}
impl OutboundIntent {
    pub fn survives_disconnect(&self) -> bool {
        self.class == IntentClass::DesiredState
    }
}
impl IntentClass {
    /// A stable, non-secret label for this class.
    ///
    /// It appears in a local NOTICE when the bounded upstream queue refuses a client
    /// command, so the client learns which kind of request was dropped without the
    /// bouncer echoing the command itself. These names are fixed strings rather than
    /// derived from anything the client sent.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::DesiredState => "desired-state",
            Self::NonReplayable => "client-command",
            Self::GenerationQuery => "query",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Backoff {
    pub attempt: u32,
    pub base: Duration,
    pub cap: Duration,
    pub jitter_percent: u8,
}
impl Backoff {
    pub fn next_delay(&mut self, entropy: u64) -> Duration {
        let raw = self
            .base
            .saturating_mul(1u32 << self.attempt.min(31))
            .min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        let span = (raw.as_millis() * self.jitter_percent.min(100) as u128 / 100) as u64;
        let offset = if span == 0 {
            0
        } else {
            (entropy % (2 * span + 1)) as i128 - span as i128
        };
        Duration::from_millis((raw.as_millis() as i128 + offset).max(0) as u64).min(self.cap)
    }
    pub fn stable_online(&mut self) {
        self.attempt = 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_testkit::{FakeI2pStreamProvider, FakeLocalAcceptor, FaultScript, ScriptedStream};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;

    struct SharedProvider(Arc<FakeI2pStreamProvider>);
    #[async_trait::async_trait]
    impl I2pStreamProvider for SharedProvider {
        async fn connect(
            &self,
            network: NetworkId,
            endpoint: &I2pEndpoint,
        ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, ProviderError> {
            self.0.connect(network, endpoint).await
        }
        async fn release(&self, network: NetworkId) -> Result<(), ProviderError> {
            self.0.release(network).await
        }
    }
    struct SharedAcceptor(Arc<FakeLocalAcceptor>);
    impl LocalAcceptor for SharedAcceptor {
        type Stream = ScriptedStream;
        async fn accept(&self) -> Result<(ClientId, Self::Stream), ProviderError> {
            self.0.accept().await
        }
    }

    fn config(desired: Vec<String>) -> UpstreamConfig {
        UpstreamConfig {
            endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            sasl: None,
            desired_channels: desired,
        }
    }

    async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> Vec<u8> {
        let mut all = Vec::new();
        let mut buf = [0; 256];
        let read = async {
            while !all.windows(needle.len()).any(|window| window == needle) {
                let count = stream.read(&mut buf).await.unwrap();
                assert!(count > 0);
                all.extend_from_slice(&buf[..count]);
            }
        };
        if tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .is_err()
        {
            panic!(
                "expected IRC frame {}; received {}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&all)
            );
        }
        all
    }

    /// Drives a supervisor through `001` so it reaches Online on generation one.
    async fn online_upstream(
        provider: &Arc<FakeI2pStreamProvider>,
        supervisor_joined: &tokio::task::JoinHandle<Result<(), RuntimeError>>,
    ) -> ScriptedStream {
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let _ = supervisor_joined;
        upstream
    }

    /// Lets the owner and its writer task run before a bounded reader arms its own
    /// timeout, so virtual time cannot outrun pending work.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    async fn wait_online(state: &mut watch::Receiver<NetworkSnapshot>) {
        while state.borrow().phase != Some(Phase::Online) {
            state.changed().await.unwrap();
        }
    }

    /// Waits for one downstream session end observed after `since` detaches.
    async fn wait_detach(
        state: &mut watch::Receiver<NetworkSnapshot>,
        since: u64,
        class: &str,
    ) -> NetworkSnapshot {
        loop {
            let current = state.borrow().clone();
            if current.downstream_detach_total > since
                && current.downstream_last_disposition == Some(class)
                && current.phase == Some(Phase::Online)
            {
                return current;
            }
            state.changed().await.unwrap();
        }
    }

    /// Waits until `sessions` clients have been attached in this generation.
    async fn wait_attached(
        state: &mut watch::Receiver<NetworkSnapshot>,
        sessions: u64,
    ) -> NetworkSnapshot {
        loop {
            let current = state.borrow().clone();
            if current.downstream_attached
                && current.downstream_session_total == sessions
                && current.phase == Some(Phase::Online)
            {
                return current;
            }
            state.changed().await.unwrap();
        }
    }

    /// Registers one client and returns every byte it received, waiting for the
    /// last frame of its expected projection. `CAP END` is part of the flow because
    /// a client that started CAP negotiation must close it before registration.
    async fn register_client(client: &mut ScriptedStream, last_frame: &[u8]) -> String {
        client
            .write_all(b"CAP LS 302\r\nNICK bot\r\nUSER bot 0 * :phone\r\nCAP END\r\n")
            .await
            .unwrap();
        let projection = read_until(client, last_frame).await;
        String::from_utf8_lossy(&projection).into_owned()
    }

    const WELCOME: &[u8] = b"001 bot :Welcome\r\n";
    const END_OF_NAMES: &[u8] = b"366 bot #room :End of NAMES list\r\n";
    const SECRET_END_OF_NAMES: &[u8] = b"366 bot #secret :End of NAMES list\r\n";

    #[test]
    fn secret_debug_is_redacted() {
        assert!(!format!("{:?}", Secret::new("secret".into())).contains("secret"));
    }

    #[test]
    fn backoff_is_bounded() {
        let mut backoff = Backoff {
            attempt: 0,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(5),
            jitter_percent: 20,
        };
        for _ in 0..20 {
            assert!(backoff.next_delay(7) <= backoff.cap);
        }
    }

    #[test]
    fn replay_class_is_explicit() {
        let intent = OutboundIntent {
            generation: ConnectionGeneration(1),
            class: IntentClass::NonReplayable,
            wire: vec![],
        };
        assert!(!intent.survives_disconnect());
    }

    #[test]
    fn network_configuration_rejects_injection_and_unbounded_channels() {
        let valid = config(vec!["#room".into()]);
        assert!(valid.validate().is_ok());
        let mut invalid = valid.clone();
        invalid.nick = "bot\r\nPRIVMSG".into();
        assert!(matches!(
            invalid.validate(),
            Err(RuntimeError::InvalidConfig)
        ));
        let mut invalid = valid.clone();
        invalid.desired_channels = vec!["#ok".into(); MAX_CHANNELS + 1];
        assert!(matches!(
            invalid.validate(),
            Err(RuntimeError::InvalidConfig)
        ));
        let mut invalid = valid;
        invalid.desired_channels = vec!["notachannel".into()];
        assert!(matches!(
            invalid.validate(),
            Err(RuntimeError::InvalidConfig)
        ));
    }

    #[tokio::test]
    async fn control_queue_is_separate_and_normal_overflow_is_explicit() {
        let (normal, _normal_rx) = mpsc::channel(NORMAL_QUEUE_CAPACITY);
        let (control, _control_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        for _ in 0..NORMAL_QUEUE_CAPACITY {
            queue_control(&normal, "PRIVMSG #c :x\r\n").unwrap();
        }
        assert!(matches!(
            queue_control(&normal, "PRIVMSG #c :overflow\r\n"),
            Err(RuntimeError::QueueOverloaded)
        ));
        queue_control(&control, "PONG :urgent\r\n").unwrap();
    }

    #[tokio::test]
    async fn ready_control_frame_precedes_normal_backlog() {
        let (control_tx, mut control_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        let (normal_tx, mut normal_rx) = mpsc::channel(NORMAL_QUEUE_CAPACITY);
        normal_tx
            .try_send(b"PRIVMSG #c :queued\r\n".to_vec())
            .unwrap();
        control_tx.try_send(b"PONG :urgent\r\n".to_vec()).unwrap();
        assert_eq!(
            next_upstream_frame(&mut control_rx, &mut normal_rx)
                .await
                .unwrap(),
            b"PONG :urgent\r\n"
        );
        assert_eq!(
            next_upstream_frame(&mut control_rx, &mut normal_rx)
                .await
                .unwrap(),
            b"PRIVMSG #c :queued\r\n"
        );
    }

    /// Core corrective evidence: upstream registration never waits for a client.
    #[tokio::test]
    async fn upstream_reaches_online_with_no_local_client() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        // No local client is ever queued for this scenario.
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        // Registration completes with no local client attached.
        let registration = read_until(&mut upstream, b"JOIN #room\r\n").await;
        assert!(String::from_utf8_lossy(&registration).contains("CAP END"));
        wait_online(&mut state).await;
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        assert!(!state.borrow().downstream_attached);
        // The JOIN was written, but nothing is observed until the server confirms.
        assert!(state.borrow().channels.is_empty());
        assert_eq!(state.borrow().pending_joins, vec!["#room".to_owned()]);
        // Upstream liveness and state processing continue with zero clients.
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :alive\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :alive"));
        upstream
            .write_all(b":bot!u@h JOIN #room\r\n:srv 332 bot #room :subject\r\n:srv 353 bot = #room :bot @Alice\r\n")
            .await
            .unwrap();
        while state.borrow().upstream_events_seen < 3 {
            state.changed().await.unwrap();
        }
        // The authoritative self JOIN is what creates observed membership.
        assert_eq!(state.borrow().channels, vec!["#room".to_owned()]);
        assert!(state.borrow().pending_joins.is_empty());
        assert!(state.borrow().rejected_joins.is_empty());
        assert!(!state.borrow().downstream_attached);
        // A local client attaches later to the same generation.
        local
            .queue_outcome(Ok((ClientId(1), FaultScript::default())))
            .unwrap();
        let mut client = local.take_peer().await;
        let projection = register_client(&mut client, END_OF_NAMES).await;
        assert!(
            projection.contains("332 bot #room :subject"),
            "{projection}"
        );
        assert!(
            projection.contains("353 bot = #room :@Alice bot"),
            "{projection}"
        );
        while !state.borrow().downstream_attached {
            state.changed().await.unwrap();
        }
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        assert_eq!(state.borrow().downstream_session_total, 1);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn downstream_eof_detaches_only_and_keeps_the_generation() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(10), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        let since = state.borrow().downstream_detach_total;
        client.shutdown().await.unwrap();
        let detached = wait_detach(&mut state, since, "downstream-eof").await;
        assert_eq!(detached.generation, Some(ConnectionGeneration(1)));
        assert_eq!(detached.downstream_session_total, 1);
        assert_eq!(detached.downstream_normal_queue_depth, 0);
        // The same generation still serves upstream traffic.
        upstream.write_all(b"PING :still-alive\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :still-alive\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :still-alive"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn downstream_quit_never_becomes_upstream_quit() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider
            .queue_outcome(Ok(FaultScript {
                capture_writes: true,
                ..FaultScript::default()
            }))
            .unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(11), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let controller = provider.take_controller().await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        let since = state.borrow().downstream_detach_total;
        client.write_all(b"QUIT :bye\r\n").await.unwrap();
        let detached = wait_detach(&mut state, since, "local-detach").await;
        assert_eq!(detached.generation, Some(ConnectionGeneration(1)));
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        read_until(&mut upstream, b"PONG :alive\r\n").await;
        assert!(
            !String::from_utf8_lossy(&controller.bytes_written(0)).contains("QUIT"),
            "downstream QUIT must not reach upstream"
        );
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn second_client_reattaches_the_same_generation_and_sees_detached_state() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for client in [ClientId(20), ClientId(21)] {
            local
                .queue_outcome(Ok((client, FaultScript::default())))
                .unwrap();
        }
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #room\r\n").await;
        let mut first = local.take_peer().await;
        register_client(&mut first, WELCOME).await;
        // State accumulates while no client is attached.
        upstream
            .write_all(b":bot!u@h JOIN #room\r\n:srv 005 bot PREFIX=(ov)@+ CHANMODES=beI,k,l,imnpst\r\n:srv 324 bot #room +kl key 42\r\n:srv 353 bot = #room :bot @Alice\r\n")
            .await
            .unwrap();
        while state.borrow().upstream_events_seen < 4 {
            state.changed().await.unwrap();
        }
        let since = state.borrow().downstream_detach_total;
        first.shutdown().await.unwrap();
        wait_detach(&mut state, since, "downstream-eof").await;
        let mut second = local.take_peer().await;
        let projection = register_client(&mut second, END_OF_NAMES).await;
        assert!(projection.contains("005 bot PREFIX=(ov)@+"), "{projection}");
        assert!(
            projection.contains("324 bot #room +kl key 42"),
            "{projection}"
        );
        assert!(
            projection.contains("353 bot = #room :@Alice bot"),
            "{projection}"
        );
        let attached = wait_attached(&mut state, 2).await;
        assert_eq!(attached.generation, Some(ConnectionGeneration(1)));
        assert_eq!(attached.downstream_session_total, 2);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn repeated_attach_detach_cycles_stay_bounded() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for cycle in 0..100 {
            local
                .queue_outcome(Ok((ClientId(1_000 + cycle), FaultScript::default())))
                .unwrap();
        }
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        for _cycle in 0..100u64 {
            let since = state.borrow().downstream_detach_total;
            let mut client = local.take_peer().await;
            client
                .write_all(b"NICK bot\r\nUSER bot 0 * :phone\r\n")
                .await
                .unwrap();
            let _ = read_until(&mut client, b"001 bot :Welcome\r\n").await;
            client.shutdown().await.unwrap();
            let settled = wait_detach(&mut state, since, "downstream-eof").await;
            assert_eq!(settled.generation, Some(ConnectionGeneration(1)));
        }
        let settled = state.borrow().clone();
        assert_eq!(settled.generation, Some(ConnectionGeneration(1)));
        assert_eq!(settled.downstream_session_total, 100);
        assert_eq!(settled.downstream_detach_total, 100);
        assert_eq!(settled.upstream_normal_queue_depth, 0);
        assert_eq!(settled.downstream_normal_queue_depth, 0);
        assert_eq!(settled.downstream_control_queue_depth, 0);
        assert_eq!(provider.requested_endpoints().len(), 1);
        upstream.write_all(b"PING :bounded\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :bounded\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :bounded"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn downstream_protocol_violation_detaches_only() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(30), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        // Client-supplied prefixes are rejected before any upstream routing.
        let since = state.borrow().downstream_detach_total;
        client.write_all(b":spoof PRIVMSG #a :x\r\n").await.unwrap();
        let detached = wait_detach(&mut state, since, "downstream-protocol").await;
        assert_eq!(detached.generation, Some(ConnectionGeneration(1)));
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        read_until(&mut upstream, b"PONG :alive\r\n").await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_detached_session_cannot_affect_the_next_client() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for client in [ClientId(40), ClientId(41)] {
            local
                .queue_outcome(Ok((client, FaultScript::default())))
                .unwrap();
        }
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let controller = provider.take_controller().await;
        let mut first = local.take_peer().await;
        register_client(&mut first, WELCOME).await;
        first
            .write_all(b"PRIVMSG #room :first session\r\n")
            .await
            .unwrap();
        let sent = read_until(&mut upstream, b"PRIVMSG #room :first session\r\n").await;
        assert!(String::from_utf8_lossy(&sent).contains("first session"));
        let since = state.borrow().downstream_detach_total;
        first.shutdown().await.unwrap();
        wait_detach(&mut state, since, "downstream-eof").await;
        let mut second = local.take_peer().await;
        let projection = register_client(&mut second, WELCOME).await;
        assert!(!projection.contains("first session"), "{projection}");
        assert!(
            !String::from_utf8_lossy(&controller.bytes_written(0)).contains("QUIT"),
            "local EOF must not emit upstream QUIT"
        );
        let attached = wait_attached(&mut state, 2).await;
        assert_eq!(attached.generation, Some(ConnectionGeneration(1)));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_failure_while_detached_replaces_the_generation() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #room\r\n").await;
        upstream.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let mut replacement = provider.take_peer().await;
        let _ = read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
        replacement
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let replayed = read_until(&mut replacement, b"JOIN #room\r\n").await;
        assert!(String::from_utf8_lossy(&replayed).contains("JOIN #room"));
        wait_online(&mut state).await;
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(2)));
        assert!(!state.borrow().downstream_attached);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_failure_while_attached_terminates_that_client() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(50), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        upstream.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let mut replacement = provider.take_peer().await;
        let _ = read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
        // The attached client was terminated with its generation.
        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
        drop(client);
        replacement
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        wait_online(&mut state).await;
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(2)));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_chat_is_not_replayed_into_replacement_generation() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for client in [ClientId(60), ClientId(61)] {
            local
                .queue_outcome(Ok((client, FaultScript::default())))
                .unwrap();
        }
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#persistent".into()]),
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #persistent\r\n").await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        client
            .write_all(b"PRIVMSG #room :possibly delivered\r\n")
            .await
            .unwrap();
        let sent = read_until(&mut upstream, b"PRIVMSG #room :possibly delivered\r\n").await;
        assert!(String::from_utf8_lossy(&sent).contains("possibly delivered"));
        upstream.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let mut replacement = provider.take_peer().await;
        let registered = read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
        assert!(!String::from_utf8_lossy(&registered).contains("possibly delivered"));
        replacement
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let rejoin = read_until(&mut replacement, b"JOIN #persistent\r\n").await;
        assert!(String::from_utf8_lossy(&rejoin).contains("JOIN #persistent"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn liveness_deadline_reconnects_with_no_client_attached() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        settle().await;
        let first = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        assert!(String::from_utf8_lossy(&first).contains("PING :bouncer-1"));
        upstream
            .write_all(b":srv PONG bot :bouncer-1\r\n")
            .await
            .unwrap();
        settle().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        let _ = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        upstream
            .write_all(b":srv PONG bot :bouncer-1\r\n")
            .await
            .unwrap();
        settle().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        let _ = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        // The unanswered probe reaches its deadline and ends the generation.
        tokio::time::advance(LIVENESS_DEADLINE + Duration::from_secs(2)).await;
        settle().await;
        let mut replacement = provider.take_peer().await;
        let registered = read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
        assert!(String::from_utf8_lossy(&registered).contains("CAP LS 302"));
        while state.borrow().generation != Some(ConnectionGeneration(2)) {
            state.changed().await.unwrap();
        }
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn client_backlog_never_starves_upstream_control_traffic() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider
            .queue_outcome(Ok(FaultScript {
                // A small socket buffer stalls the upstream writer so the bounded
                // normal queue actually fills, without deadlocking the handshake.
                capacity: 256,
                ..FaultScript::default()
            }))
            .unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(70), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        let since = state.borrow().downstream_detach_total;
        for index in 0..NORMAL_QUEUE_CAPACITY * 2 {
            client
                .write_all(format!("PRIVMSG #room :flood {index}\r\n").as_bytes())
                .await
                .unwrap();
        }
        upstream.write_all(b"PING :under-load\r\n").await.unwrap();
        // The owner keeps serving control traffic while normal traffic is saturated.
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !seen
            .windows(b"PONG :under-load\r\n".len())
            .any(|window| window == b"PONG :under-load\r\n")
        {
            let mut buf = [0u8; 256];
            let read = tokio::time::timeout_at(deadline, upstream.read(&mut buf)).await;
            let Ok(Ok(count)) = read else {
                break;
            };
            assert!(count > 0, "upstream stayed responsive");
            seen.extend_from_slice(&buf[..count]);
        }
        assert!(
            seen.windows(b"PONG :under-load\r\n".len())
                .any(|window| window == b"PONG :under-load\r\n"),
            "server PING must be answered while the normal queue is saturated"
        );
        assert_eq!(state.borrow().phase, Some(Phase::Online));
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        // The saturated backlog fails the client explicitly, not the generation.
        wait_detach(&mut state, since, "downstream-overload").await;
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        // Drain the bounded upstream backlog so shutdown never waits on a stalled
        // write; draining ends after a bounded idle window.
        let idle = Duration::from_millis(200);
        for _ in 0..512 {
            let mut buf = [0u8; 256];
            if !matches!(
                tokio::time::timeout(idle, upstream.read(&mut buf)).await,
                Ok(Ok(count)) if count > 0
            ) {
                break;
            }
        }
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn explicit_stop_is_the_only_path_that_sends_upstream_quit() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider
            .queue_outcome(Ok(FaultScript {
                capture_writes: true,
                ..FaultScript::default()
            }))
            .unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(80), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let _upstream = online_upstream(&provider, &task).await;
        let controller = provider.take_controller().await;
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        let written = String::from_utf8_lossy(&controller.bytes_written(0)).into_owned();
        assert_eq!(written.matches("QUIT").count(), 1, "{written}");
        // The attached client is terminated by the supervisor stop.
        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn single_client_vertical_registers_routes_and_answers_ping() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(1), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let initial = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        assert!(String::from_utf8_lossy(&initial).contains("CAP LS 302\r\n"));
        upstream
            .write_all(b":srv CAP * LS :server-time message-tags\r\n")
            .await
            .unwrap();
        let end = read_until(&mut upstream, b"CAP END\r\n").await;
        assert!(!String::from_utf8_lossy(&end).contains("CAP REQ"));
        upstream
            .write_all(b":srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let join = read_until(&mut upstream, b"JOIN #room\r\n").await;
        assert!(String::from_utf8_lossy(&join).contains("JOIN #room"));
        upstream.write_all(b":bot!u@h JOIN #room\r\n:srv 005 bot CASEMAPPING=rfc1459 CHANTYPES=#& :are supported by this server\r\n:srv 332 bot #room :subject\r\n:srv MODE #room +nt\r\n:srv 353 bot = #room :bot @Alice\r\n:srv 366 bot #room :End\r\n").await.unwrap();
        while state.borrow().upstream_events_seen < 6 {
            state.changed().await.unwrap();
        }
        // The local client is accepted only after the generation is online.
        let mut client = local.take_peer().await;
        client
            .write_all(b"CAP LS 302\r\nCAP REQ :message-tags\r\nNICK mobile\r\nNICK bot\r\nUSER bot 0 * :phone\r\nCAP END\r\n")
            .await
            .unwrap();
        let welcome = read_until(&mut client, b"366 bot #room :End of NAMES list\r\n").await;
        let projection = String::from_utf8_lossy(&welcome);
        assert!(projection.contains("001 bot"));
        assert!(projection.contains("CAP * ACK :message-tags"));
        assert!(projection.contains("433 * * :Nickname unavailable on this network"));
        assert!(projection.contains("CASEMAPPING=rfc1459"), "{projection}");
        assert!(projection.contains("332 bot #room :subject"));
        assert!(projection.contains("324 bot #room +nt"));
        assert!(projection.contains("353 bot = #room :@Alice bot"));
        assert!(!projection.contains("421 bot"));
        client.write_all(b"WHOIS Alice\r\n").await.unwrap();
        let query = read_until(&mut upstream, b"WHOIS Alice\r\n").await;
        assert!(String::from_utf8_lossy(&query).contains("WHOIS Alice"));
        upstream
            .write_all(b"@time=123 :srv NOTICE mobile :tagged\r\n")
            .await
            .unwrap();
        let forwarded = read_until(&mut client, b"NOTICE mobile :tagged\r\n").await;
        assert!(
            String::from_utf8_lossy(&forwarded).contains("@time="),
            "a client that negotiated message-tags receives tags: {}",
            String::from_utf8_lossy(&forwarded)
        );
        client.write_all(b"PRIVMSG #room :hello\r\n").await.unwrap();
        assert_eq!(
            Message::parse(b"PRIVMSG #room :hello\r\n")
                .unwrap()
                .encode()
                .unwrap(),
            b"PRIVMSG #room :hello\r\n"
        );
        let chat = read_until(&mut upstream, b"PRIVMSG #room :hello\r\n").await;
        assert!(String::from_utf8_lossy(&chat).contains("PRIVMSG #room :hello"));
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :alive\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :alive"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn isupport_values_reach_state_and_the_next_projection() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for client in [ClientId(90), ClientId(91)] {
            local
                .queue_outcome(Ok((client, FaultScript::default())))
                .unwrap();
        }
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #room\r\n").await;
        // `005` may arrive in the same read as the welcome and must not be lost.
        upstream
            .write_all(b":bot!u@h JOIN #room\r\n:srv 005 bot CASEMAPPING=rfc1459 PREFIX=(qaohv)~&@%+ CHANTYPES=#& CHANMODES=beI,k,l,imnpst :are supported by this server\r\n:srv 332 bot #room :subject\r\n:srv 324 bot #room +kl key 42\r\n:srv 353 bot = #room :bot @Alice ~Quiet\r\n")
            .await
            .unwrap();
        while state.borrow().upstream_events_seen < 5 {
            state.changed().await.unwrap();
        }
        let mut first = local.take_peer().await;
        register_client(&mut first, END_OF_NAMES).await;
        let since = state.borrow().downstream_detach_total;
        first.shutdown().await.unwrap();
        wait_detach(&mut state, since, "downstream-eof").await;
        let mut second = local.take_peer().await;
        let projection = register_client(&mut second, END_OF_NAMES).await;
        assert!(projection.contains("PREFIX=(qaohv)~&@%+"), "{projection}");
        assert!(
            projection.contains("324 bot #room +kl key 42"),
            "{projection}"
        );
        assert!(
            projection.contains("353 bot = #room :@Alice bot ~Quiet"),
            "{projection}"
        );
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_desired_join_is_never_projected_and_retried_by_a_new_generation() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(70), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#locked".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #locked\r\n").await;
        assert_eq!(state.borrow().pending_joins, vec!["#locked".to_owned()]);
        assert!(state.borrow().channels.is_empty());
        // The server refuses the join.
        upstream
            .write_all(b":srv 473 bot #locked :Cannot join channel (+i)\r\n")
            .await
            .unwrap();
        while state.borrow().rejected_joins.is_empty() {
            state.changed().await.unwrap();
        }
        assert_eq!(
            state.borrow().rejected_joins,
            vec![("#locked".to_owned(), "473")]
        );
        assert!(state.borrow().pending_joins.is_empty());
        assert!(state.borrow().channels.is_empty());
        // A client attaching after the failure receives no synthetic JOIN, and the
        // generation is still healthy.
        let mut client = local.take_peer().await;
        let projection = register_client(&mut client, WELCOME).await;
        assert!(projection.contains("001 bot"), "{projection}");
        assert!(!projection.contains("JOIN #locked"), "{projection}");
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        read_until(&mut upstream, b"PONG :alive\r\n").await;
        // A fresh generation re-attempts the same desired configuration.
        client.shutdown().await.unwrap();
        upstream.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let mut replacement = provider.take_peer().await;
        let _ = read_until(&mut replacement, b"USER user 0 * :bouncer\r\n").await;
        replacement
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let retried = read_until(&mut replacement, b"JOIN #locked\r\n").await;
        assert!(String::from_utf8_lossy(&retried).contains("JOIN #locked"));
        wait_online(&mut state).await;
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(2)));
        assert!(state.borrow().rejected_joins.is_empty());
        assert_eq!(state.borrow().pending_joins, vec!["#locked".to_owned()]);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn downstream_cap_negotiation_holds_the_welcome_until_cap_end() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(71), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let mut client = local.take_peer().await;
        client
            .write_all(
                b"CAP LS 302\r\nCAP REQ :echo-message\r\nNICK bot\r\nUSER bot 0 * :phone\r\n",
            )
            .await
            .unwrap();
        let local_reply = read_until(&mut client, b"CAP * NAK :Unsupported capabilities\r\n").await;
        let local_text = String::from_utf8_lossy(&local_reply).into_owned();
        assert!(local_text.contains("CAP * NAK"), "{local_text}");
        // No welcome, ISUPPORT, or channel projection before CAP END.
        let mut buf = [0u8; 512];
        assert!(
            tokio::time::timeout(Duration::from_millis(250), client.read(&mut buf))
                .await
                .is_err(),
            "no frame may be queued before CAP END"
        );
        assert!(state.borrow().downstream_attached);
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        client.write_all(b"CAP END\r\n").await.unwrap();
        let welcome = read_until(&mut client, WELCOME).await;
        let projection = String::from_utf8_lossy(&welcome).into_owned();
        assert_eq!(projection.matches("001 bot").count(), 1, "{projection}");
        // The client is now registered and its traffic is routed upstream.
        client.write_all(b"NAMES #room\r\n").await.unwrap();
        let query = read_until(&mut upstream, b"NAMES #room\r\n").await;
        assert!(String::from_utf8_lossy(&query).contains("NAMES #room"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn downstream_detach_during_cap_negotiation_leaves_upstream_online() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(72), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#room".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #room\r\n").await;
        let mut client = local.take_peer().await;
        client
            .write_all(b"CAP LS 302\r\nNICK bot\r\nUSER bot 0 * :phone\r\n")
            .await
            .unwrap();
        let listing = read_until(&mut client, b"CAP * LS :draft/chathistory").await;
        assert!(String::from_utf8_lossy(&listing).contains("CAP * LS :"));
        let since = state.borrow().downstream_detach_total;
        client.shutdown().await.unwrap();
        let detached = wait_detach(&mut state, since, "downstream-eof").await;
        assert_eq!(detached.generation, Some(ConnectionGeneration(1)));
        assert_eq!(detached.phase, Some(Phase::Online));
        upstream.write_all(b"PING :still-alive\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :still-alive\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :still-alive"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn secret_channel_names_with_at_visibility_reach_the_projection() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(73), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            config(vec!["#secret".into()]),
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = online_upstream(&provider, &task).await;
        let _ = read_until(&mut upstream, b"JOIN #secret\r\n").await;
        upstream
            .write_all(b":bot!u@h JOIN #secret\r\n:srv 353 bot @ #secret :bot @Alice\r\n:srv 366 bot #secret :End of NAMES list\r\n")
            .await
            .unwrap();
        while state.borrow().upstream_events_seen < 3 {
            state.changed().await.unwrap();
        }
        let mut client = local.take_peer().await;
        let projection = register_client(&mut client, SECRET_END_OF_NAMES).await;
        assert!(
            projection.contains("353 bot = #secret :@Alice bot"),
            "{projection}"
        );
        assert!(
            projection.contains("366 bot #secret :End of NAMES list"),
            "{projection}"
        );
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn configured_sasl_plain_completes_without_secret_diagnostics() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let mut with_sasl = config(vec![]);
        with_sasl.sasl = Some(("alice".into(), Secret::new("swordfish".into())));
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), with_sasl).unwrap();
        assert!(!format!("{:?}", supervisor.config.sasl).contains("swordfish"));
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS * :message-tags\r\n:srv CAP * LS :sasl=PLAIN\r\n")
            .await
            .unwrap();
        let req = read_until(&mut upstream, b"CAP REQ :sasl\r\n").await;
        assert!(String::from_utf8_lossy(&req).contains("CAP REQ :sasl"));
        upstream
            .write_all(b":srv CAP * ACK :sasl=PLAIN\r\n")
            .await
            .unwrap();
        let auth = read_until(&mut upstream, b"AUTHENTICATE PLAIN\r\n").await;
        assert!(String::from_utf8_lossy(&auth).contains("AUTHENTICATE PLAIN"));
        upstream.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
        let raw = "\0alice\0swordfish";
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
        let payload = format!("AUTHENTICATE {encoded}\r\n");
        let response = read_until(&mut upstream, payload.as_bytes()).await;
        assert!(String::from_utf8_lossy(&response).contains(&payload));
        upstream
            .write_all(b":srv 903 bot :SASL success\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let cap_end = read_until(&mut upstream, b"CAP END\r\n").await;
        assert!(String::from_utf8_lossy(&cap_end).contains("CAP END"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn configured_sasl_unavailable_is_a_terminal_registration_error() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let mut with_sasl = config(vec![]);
        with_sasl.sasl = Some(("alice".into(), Secret::new("swordfish".into())));
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), with_sasl).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :message-tags\r\n")
            .await
            .unwrap();
        assert!(matches!(
            task.await.unwrap(),
            Err(RuntimeError::Registration)
        ));
        drop(stop_tx);
    }

    #[tokio::test(start_paused = true)]
    async fn one_hundred_provider_failures_remain_bounded_and_recover() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..100 {
            provider
                .queue_outcome(Err(ProviderError::Unavailable))
                .unwrap();
        }
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(100), FaultScript::default())))
            .unwrap();
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        tokio::task::yield_now().await;
        for _ in 0..100 {
            tokio::time::advance(Duration::from_secs(301)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(provider.requested_endpoints().len(), 101);
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        // A local client can still attach after the failure storm.
        let mut client = local.take_peer().await;
        register_client(&mut client, WELCOME).await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stop_cancels_in_progress_registration() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn registration_deadline_is_reported_and_stop_cancels_backoff() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        tokio::time::advance(REGISTRATION_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(state.borrow().phase, Some(Phase::Backoff));
        assert_eq!(state.borrow().last_error, Some("timeout"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cap_sasl_phase_has_its_own_bounded_deadline() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        let supervisor =
            NetworkSupervisor::new(SharedProvider(provider.clone()), config(vec![])).unwrap();
        let state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        tokio::time::advance(CAP_SASL_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(state.borrow().phase, Some(Phase::Backoff));
        assert_eq!(state.borrow().last_error, Some("timeout"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
}
