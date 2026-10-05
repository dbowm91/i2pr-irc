//! Single-network registration owner. Network I/O is injected through the I2P provider.
use i2pr_irc_core::{
    Casemapping, ConnectionGeneration, I2pEndpoint, I2pStreamProvider, LocalAcceptor, ProviderError,
};
use i2pr_irc_wire::{LineDecoder, Message};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    sync::watch,
    task::JoinSet,
    time::{Instant, timeout},
};
use zeroize::{Zeroize, Zeroizing};

pub const NORMAL_QUEUE_CAPACITY: usize = 64;
pub const CONTROL_QUEUE_CAPACITY: usize = 8;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
pub const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(180);
pub const CAP_SASL_TIMEOUT: Duration = Duration::from_secs(90);
pub const MAX_CHANNELS: usize = 128;
pub const MAX_MEMBERS_PER_CHANNEL: usize = 2048;
pub const MAX_TOTAL_MEMBERS: usize = 8192;
pub const MAX_ISUPPORT_TOKENS: usize = 128;
pub const MAX_CHANNEL_NAME_BYTES: usize = 200;
pub const MAX_CREDENTIAL_BYTES: usize = 1024;
pub const MAX_DOWNSTREAM_LINE: usize = 8703;
pub const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("operation timed out")]
    Timeout,
    #[error("protocol failure")]
    Protocol,
    #[error("registration rejected")]
    Registration,
    #[error("invalid network configuration")]
    InvalidConfig,
    #[error("bounded output queue overloaded")]
    QueueOverloaded,
    #[error("connection generation space exhausted")]
    GenerationExhausted,
    #[error("I/O failure")]
    Io(#[from] std::io::Error),
    #[error("stopped")]
    Stopped,
}
#[derive(Clone)]
pub struct Secret(String);
impl Secret {
    pub fn new(v: String) -> Self {
        Self(v)
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize()
    }
}
#[derive(Clone, Debug)]
pub struct UpstreamConfig {
    pub endpoint: I2pEndpoint,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub sasl: Option<(String, Secret)>,
    pub desired_channels: Vec<String>,
}
impl UpstreamConfig {
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
                || !matches!(channel.as_bytes()[0], b'#' | b'&')
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
    pub upstream_events_seen: u64,
    pub last_error: Option<&'static str>,
}
pub struct NetworkSupervisor<P> {
    provider: P,
    config: UpstreamConfig,
    snapshot: watch::Sender<NetworkSnapshot>,
}
impl<P: I2pStreamProvider> NetworkSupervisor<P> {
    pub fn new(provider: P, config: UpstreamConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let (snapshot, _) = watch::channel(NetworkSnapshot::default());
        snapshot.send_modify(|state| state.phase = Some(Phase::Idle));
        Ok(Self {
            provider,
            config,
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

    /// Own one local session and one upstream generation until stop. Each failed
    /// generation is discarded before bounded reconnect backoff; user traffic is
    /// never retained for replay. The actor itself owns all stream halves.
    pub async fn serve<A: LocalAcceptor>(
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
                result = timeout(CONNECT_TIMEOUT, self.provider.connect(&self.config.endpoint)) => {
                    match result { Ok(Ok(stream)) => Ok(stream), Ok(Err(e)) => Err(RuntimeError::Provider(e)), Err(_) => Err(RuntimeError::Timeout) }
                }
            };
            let result = match connection {
                Ok(upstream) => {
                    self.set_phase(Phase::Registering, Some(ConnectionGeneration(generation)));
                    let (_client_id, client) = tokio::select! {
                        _ = stopped(&mut stop) => { self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation))); return Ok(()) },
                        result = acceptor.accept() => result?,
                    };
                    let online_started = Instant::now();
                    let result = self
                        .run_generation(
                            upstream,
                            client,
                            ConnectionGeneration(generation),
                            &mut stop,
                        )
                        .await;
                    if online_started.elapsed() >= Duration::from_secs(300) {
                        backoff.stable_online();
                    }
                    if result.is_ok() {
                        self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation)));
                        return Ok(());
                    }
                    result
                }
                Err(e) => Err(e),
            };
            if matches!(result, Err(RuntimeError::Registration)) {
                self.snapshot.send_modify(|state| {
                    state.phase = Some(Phase::Stopped);
                    state.downstream_attached = false;
                    state.last_error = Some("registration rejected");
                });
                return Err(RuntimeError::Registration);
            }
            let delay = backoff.next_delay(generation.wrapping_mul(0x9e3779b97f4a7c15));
            self.snapshot.send_modify(|state| {
                state.phase = Some(Phase::Backoff);
                state.downstream_attached = false;
                state.reconnect_attempt = backoff.attempt;
                state.current_backoff_ms = Some(delay.as_millis().min(u64::MAX as u128) as u64);
                state.last_error = result.as_ref().err().map(error_class);
            });
            tokio::select! { _ = stopped(&mut stop) => { self.set_phase(Phase::Stopped, Some(ConnectionGeneration(generation))); return Ok(()) }, _ = tokio::time::sleep(delay) => {} }
        }
    }

    async fn run_generation<
        S: i2pr_irc_core::ByteStream + 'static,
        D: i2pr_irc_core::ByteStream + 'static,
    >(
        &self,
        upstream: S,
        downstream: D,
        _generation: ConnectionGeneration,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), RuntimeError> {
        let (mut ur, mut uw) = tokio::io::split(upstream);
        let (mut dr, dw) = tokio::io::split(downstream);
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
            let mut decoder = LineDecoder::default();
            let mut buf = [0u8; 2048];
            let mut welcomed = false;
            let mut cap_finished = false;
            let mut offered = BTreeSet::new();
            let mut sasl_plain_offered = false;
            let mut requested = false;
            let mut sasl_active = false;
            while !(welcomed && cap_finished) {
                let n = if cap_finished {
                    ur.read(&mut buf).await?
                } else {
                    tokio::time::timeout(CAP_SASL_TIMEOUT, ur.read(&mut buf))
                        .await
                        .map_err(|_| RuntimeError::Timeout)?
                        .map_err(RuntimeError::Io)?
                };
                if n == 0 {
                    return Err(RuntimeError::Protocol);
                }
                for line in decoder.push(&buf[..n]) {
                    let msg = Message::parse(&line.map_err(|_| RuntimeError::Protocol)?)
                        .map_err(|_| RuntimeError::Protocol)?;
                    let cmd = String::from_utf8_lossy(&msg.command).to_ascii_uppercase();
                    let params: Vec<String> = msg
                        .params
                        .iter()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .collect();
                    match cmd.as_str() {
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
                        "PING" => {
                            if let Some(token) = params.last() {
                                send(&mut uw, &format!("PONG :{}\r\n", token)).await?;
                            }
                        }
                        "001" => welcomed = true,
                        "ERROR" | "464" | "465" | "451" => return Err(RuntimeError::Registration),
                        _ => {}
                    }
                }
            }
            for channel in self.config.desired_channels.iter().take(MAX_CHANNELS) {
                send(&mut uw, &format!("JOIN {}\r\n", channel)).await?;
            }
            Ok::<(), RuntimeError>(())
        };
        tokio::select! { _ = stopped(stop) => return Err(RuntimeError::Stopped), r = timeout(REGISTRATION_TIMEOUT, registration) => r.unwrap_or(Err(RuntimeError::Timeout)) }?;
        self.snapshot.send_modify(|state| {
            state.phase = Some(Phase::Online);
            state.generation = Some(_generation);
            state.nick = Some(self.config.nick.clone());
            state.channels = self
                .config
                .desired_channels
                .iter()
                .take(MAX_CHANNELS)
                .cloned()
                .collect();
            state.last_error = None;
            state.current_backoff_ms = None;
            state.downstream_attached = true;
        });
        let mut ubuf = [0u8; 2048];
        let mut dbuf = [0u8; 2048];
        let mut udec = LineDecoder::default();
        let mut ddec = LineDecoder::default();
        let mut nick = self.config.nick.clone();
        let mut joined: BTreeSet<String> = BTreeSet::new();
        let mut members: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut topics: BTreeMap<String, String> = BTreeMap::new();
        let mut channel_modes: BTreeMap<String, BTreeSet<char>> = BTreeMap::new();
        let mut isupport: BTreeSet<String> = BTreeSet::new();
        let mut casemapping = Casemapping::Rfc1459;
        let mut client_ready = false;
        let mut client_nick: Option<String> = None;
        let mut client_user = false;
        let mut probe = tokio::time::interval(Duration::from_secs(60));
        probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut awaiting_pong: Option<(Instant, String)> = None;
        let (control_tx, mut control_rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (normal_tx, mut normal_rx) = mpsc::channel::<OutboundIntent>(NORMAL_QUEUE_CAPACITY);
        let mut writer_tasks = JoinSet::new();
        writer_tasks.spawn(async move {
            loop {
                let next = next_intent_frame(&mut control_rx, &mut normal_rx).await;
                match next {
                    Some(Err(bytes)) => write_frame(&mut uw, &bytes).await?,
                    Some(Ok(intent)) if intent.generation == _generation => {
                        write_frame(&mut uw, &intent.wire).await?
                    }
                    Some(Ok(_)) => continue,
                    None => return Ok::<(), std::io::Error>(()),
                }
            }
        });
        let (client_control_tx, mut client_control_rx) =
            mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (client_normal_tx, mut client_normal_rx) =
            mpsc::channel::<Vec<u8>>(NORMAL_QUEUE_CAPACITY);
        writer_tasks.spawn(async move {
            let mut dw = dw;
            loop {
                let next = next_queued_frame(&mut client_control_rx, &mut client_normal_rx).await;
                match next {
                    Some(bytes) => write_frame(&mut dw, &bytes).await?,
                    None => return Ok::<(), std::io::Error>(()),
                }
            }
        });
        loop {
            tokio::select! {
                _ = stopped(stop) => {
                    self.set_phase(Phase::Stopping, Some(_generation));
                    let _ = control_tx.try_send(b"QUIT :Bouncer shutting down\r\n".to_vec());
                    drop(control_tx); drop(normal_tx); drop(client_control_tx); drop(client_normal_tx);
                    while let Some(result) = writer_tasks.join_next().await { result.map_err(|e| RuntimeError::Io(std::io::Error::other(e)))?.map_err(RuntimeError::Io)?; }
                    return Ok(());
                }
                writer = writer_tasks.join_next() => {
                    match writer { Some(Ok(Ok(()))) => return Err(RuntimeError::Protocol), Some(Ok(Err(e))) => return Err(RuntimeError::Io(e)), Some(Err(e)) => return Err(RuntimeError::Io(std::io::Error::other(e))), None => return Err(RuntimeError::Protocol) }
                }
                _ = probe.tick() => {
                    if awaiting_pong.as_ref().is_some_and(|(since, _)| since.elapsed() >= Duration::from_secs(120)) { return Err(RuntimeError::Timeout); }
                    if awaiting_pong.is_none() {
                        let token = format!("bouncer-{}", _generation.0);
                        queue_control(&control_tx, &format!("PING :{}\r\n", token))?;
                        awaiting_pong = Some((Instant::now(), token));
                    }
                }
                n = ur.read(&mut ubuf) => {
                    let n = n.map_err(RuntimeError::Io)?; if n == 0 { return Err(RuntimeError::Protocol); }
                    for line in udec.push(&ubuf[..n]) {
                        let bytes = line.map_err(|_| RuntimeError::Protocol)?;
                        let msg = Message::parse(&bytes).map_err(|_| RuntimeError::Protocol)?;
                        msg.validate_tag_budget(i2pr_irc_wire::TagDirection::ServerOutput).map_err(|_| RuntimeError::Protocol)?;
                        self.snapshot.send_modify(|state| state.upstream_events_seen = state.upstream_events_seen.saturating_add(1));
                        let cmd = String::from_utf8_lossy(&msg.command).to_ascii_uppercase();
                        if cmd == "PONG" && msg.params.last().zip(awaiting_pong.as_ref()).is_some_and(|(p, (_, expected))| p == expected.as_bytes()) { awaiting_pong = None; }
                        if cmd == "PING" { let p = msg.params.last().ok_or(RuntimeError::Protocol)?; queue_control(&control_tx, &format!("PONG :{}\r\n", String::from_utf8_lossy(p)))?; }
                        let source_nick = msg.prefix.as_deref().map(|p| p.split(|b| *b == b'!' || *b == b'@').next().unwrap_or(p)).map(String::from_utf8_lossy).map(|v| v.into_owned());
                        match cmd.as_str() {
                            "005" => for token in msg.params.iter().skip(1).filter_map(|v| std::str::from_utf8(v).ok()).flat_map(str::split_whitespace) {
                                    if let Some(value) = token.strip_prefix("CASEMAPPING=") { casemapping = match value { "ascii" => Casemapping::Ascii, "strict-rfc1459" => Casemapping::StrictRfc1459, _ => Casemapping::Rfc1459 }; }
                                    if token.len() <= 64 && token.bytes().all(|b| b.is_ascii_graphic()) && !matches!(token, "are" | "supported" | "by" | "this" | "server") && isupport.len() < MAX_ISUPPORT_TOKENS { isupport.insert(token.to_owned()); }
                            },
                            "JOIN" if !msg.params.is_empty() => {
                                let channel = String::from_utf8_lossy(&msg.params[0]).into_owned();
                                if let Some(source) = &source_nick {
                                    if same_nick(source, &nick, casemapping) && channel.len() <= MAX_CHANNEL_NAME_BYTES && joined.len() < MAX_CHANNELS { joined.insert(channel.clone()); }
                                    if members.len() < MAX_CHANNELS || members.contains_key(&channel) {
                                        let total = members.values().map(BTreeSet::len).sum::<usize>();
                                        let set = members.entry(channel).or_default();
                                        if set.len() < MAX_MEMBERS_PER_CHANNEL && total < MAX_TOTAL_MEMBERS && !set.iter().any(|m| same_nick(member_nick(m), source, casemapping)) { set.insert(source.clone()); }
                                    }
                                }
                            }
                            "PART" if !msg.params.is_empty() => {
                                let channel = String::from_utf8_lossy(&msg.params[0]).into_owned();
                                if let Some(source) = &source_nick {
                                    if let Some(set) = members.get_mut(&channel) { remove_member(set, source, casemapping); }
                                    if same_nick(source, &nick, casemapping) { joined.remove(&channel); }
                                }
                            }
                            "KICK" if msg.params.len() > 1 => {
                                let channel = String::from_utf8_lossy(&msg.params[0]).into_owned(); let target = String::from_utf8_lossy(&msg.params[1]).into_owned();
                                if let Some(set) = members.get_mut(&channel) { remove_member(set, &target, casemapping); }
                                if same_nick(&target, &nick, casemapping) { joined.remove(&channel); }
                            }
                            "QUIT" => if let Some(source) = &source_nick { for set in members.values_mut() { remove_member(set, source, casemapping); } },
                            "NICK" if !msg.params.is_empty() && valid_client_nick(&msg.params[0]) => {
                                let replacement = String::from_utf8_lossy(&msg.params[0]).into_owned();
                                if let Some(source) = &source_nick { for set in members.values_mut() { rename_member(set, source, &replacement, casemapping); } if same_nick(source, &nick, casemapping) { nick = replacement; } }
                            }
                            "353" if msg.params.len() >= 3 => {
                                let channel = String::from_utf8_lossy(&msg.params[msg.params.len()-2]).into_owned();
                                if members.len() < MAX_CHANNELS || members.contains_key(&channel) {
                                    let names = msg.params.last().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
                                    let total = members.values().map(BTreeSet::len).sum::<usize>(); let set = members.entry(channel).or_default();
                                    for name in names.split_whitespace().take(MAX_MEMBERS_PER_CHANNEL.saturating_sub(set.len()).min(MAX_TOTAL_MEMBERS.saturating_sub(total))) { if !set.iter().any(|m| same_nick(member_nick(m), member_nick(name), casemapping)) { set.insert(name.to_owned()); } }
                                }
                            }
                            "332" if msg.params.len() >= 3 => { let channel = String::from_utf8_lossy(&msg.params[1]).into_owned(); if topics.len() < MAX_CHANNELS || topics.contains_key(&channel) { topics.insert(channel, String::from_utf8_lossy(msg.params.last().unwrap()).into_owned()); } }
                            "TOPIC" if msg.params.len() >= 2 => { let channel = String::from_utf8_lossy(&msg.params[0]).into_owned(); if topics.len() < MAX_CHANNELS || topics.contains_key(&channel) { topics.insert(channel, String::from_utf8_lossy(msg.params.last().unwrap()).into_owned()); } }
                            "MODE" if msg.params.len() >= 2 => {
                                let channel = String::from_utf8_lossy(&msg.params[0]).into_owned();
                                if (channel.starts_with('#') || channel.starts_with('&')) && (channel_modes.len() < MAX_CHANNELS || channel_modes.contains_key(&channel)) {
                                    let set = channel_modes.entry(channel).or_default(); let mut adding = true;
                                    for mode in String::from_utf8_lossy(&msg.params[1]).chars() { match mode { '+' => adding = true, '-' => adding = false, 'o' | 'v' | 'h' | 'a' | 'q' => {}, c if c.is_ascii_alphabetic() => if adding { set.insert(c); } else { set.remove(&c); }, _ => {} } }
                                }
                            }
                            _ => {}
                        }
                        self.snapshot.send_modify(|state| { state.nick = Some(nick.clone()); state.channels = joined.iter().cloned().collect(); });
                        if client_ready {
                            let outgoing = if msg.tags.is_empty() { bytes } else { let mut untagged = msg.clone(); untagged.tags.clear(); untagged.encode().map_err(|_| RuntimeError::Protocol)? };
                            queue_bytes(&client_normal_tx, outgoing)?;
                        }
                    }
                }
                n = dr.read(&mut dbuf) => {
                    let n = n.map_err(RuntimeError::Io)?; if n == 0 { return Ok(()); }
                    for line in ddec.push(&dbuf[..n]) {
                        let bytes = line.map_err(|_| RuntimeError::Protocol)?;
                        let msg = Message::parse(&bytes).map_err(|_| RuntimeError::Protocol)?;
                        let cmd = String::from_utf8_lossy(&msg.command).to_ascii_uppercase();
                        if msg.prefix.is_some() || msg.validate_tag_budget(i2pr_irc_wire::TagDirection::ClientInput).is_err() { return Err(RuntimeError::Protocol); }
                        if !client_ready {
                            match cmd.as_str() {
                                "CAP" => {
                                    let subcommand = msg.params.first().map(|p| String::from_utf8_lossy(p).to_ascii_uppercase()).unwrap_or_default();
                                    match subcommand.as_str() {
                                        "LS" => queue_line(&client_normal_tx, &format!(":bouncer CAP {} LS :\r\n", client_nick.as_deref().unwrap_or("*")))?,
                                        "REQ" => queue_line(&client_normal_tx, &format!(":bouncer CAP {} NAK :Unsupported capabilities\r\n", client_nick.as_deref().unwrap_or("*")))?,
                                        "END" => {},
                                        _ => queue_line(&client_normal_tx, ":bouncer 410 * CAP :Invalid CAP subcommand\r\n")?,
                                    }
                                }
                                "NICK" => { if let Some(value) = msg.params.first() { if valid_client_nick(value) && same_nick(&String::from_utf8_lossy(value), &nick, casemapping) { client_nick = Some(String::from_utf8_lossy(value).into_owned()); } else { queue_line(&client_normal_tx, ":bouncer 433 * * :Nickname unavailable on this network\r\n")?; } } }
                                "USER" => client_user = true,
                                "PING" => if let Some(token) = msg.params.last() { queue_control(&client_control_tx, &format!(":bouncer PONG bouncer :{}\r\n", String::from_utf8_lossy(token)))?; },
                                _ => queue_line(&client_normal_tx, ":bouncer 451 * :Register first\r\n")?,
                            }
                            if !client_ready && client_user && client_nick.is_some() {
                                client_ready = true;
                                let registered_nick = client_nick.as_deref().unwrap_or("*");
                                queue_line(&client_normal_tx, &format!(":bouncer 001 {} :Welcome\r\n", registered_nick))?;
                                for token in &isupport { queue_line(&client_normal_tx, &format!(":bouncer 005 {} {} :are supported by this server\r\n", registered_nick, token))?; }
                                for c in &joined {
                                    queue_line(&client_normal_tx, &format!(":{} JOIN {}\r\n", nick, c))?;
                                    if let Some(topic) = topics.get(c) {
                                        let prefix = format!(":bouncer 332 {} {} :", registered_nick, c);
                                        let budget = i2pr_irc_wire::MAX_LINE_BYTES.saturating_sub(prefix.len() + 2);
                                        let mut end = topic.len().min(budget);
                                        while !topic.is_char_boundary(end) { end -= 1; }
                                        queue_line(&client_normal_tx, &format!("{}{}\r\n", prefix, &topic[..end]))?;
                                    }
                                    if let Some(modes) = channel_modes.get(c) { let modes = modes.iter().collect::<String>(); if !modes.is_empty() { queue_line(&client_normal_tx, &format!(":bouncer 324 {} {} +{}\r\n", registered_nick, c, modes))?; } }
                                    if let Some(names) = members.get(c) {
                                        let list = names.iter().cloned().collect::<Vec<_>>().join(" ");
                                        let prefix = format!(":bouncer 353 {} = {} :", registered_nick, c);
                                        let budget = i2pr_irc_wire::MAX_LINE_BYTES.saturating_sub(prefix.len() + 2).max(1);
                                        let mut start = 0;
                                        while start < list.len() {
                                            let mut end = (start + budget).min(list.len());
                                            while !list.is_char_boundary(end) { end -= 1; }
                                            queue_line(&client_normal_tx, &format!("{}{}\r\n", prefix, &list[start..end]))?;
                                            start = end;
                                        }
                                    }
                                    queue_line(&client_normal_tx, &format!(":bouncer 366 {} {} :End of NAMES list\r\n", registered_nick, c))?;
                                }
                            }
                            continue;
                        }
                        match cmd.as_str() {
                            "CAP" => {
                                let subcommand = msg.params.first().map(|p| String::from_utf8_lossy(p).to_ascii_uppercase()).unwrap_or_default();
                                match subcommand.as_str() {
                                    "LS" => queue_line(&client_normal_tx, &format!(":bouncer CAP {} LS :\r\n", client_nick.as_deref().unwrap_or("*")))?,
                                    "REQ" => queue_line(&client_normal_tx, &format!(":bouncer CAP {} NAK :Unsupported capabilities\r\n", client_nick.as_deref().unwrap_or("*")))?,
                                    "END" => {},
                                    _ => queue_line(&client_normal_tx, ":bouncer 410 * CAP :Invalid CAP subcommand\r\n")?,
                                }
                            }
                            "PING" => { let p = msg.params.last().ok_or(RuntimeError::Protocol)?; queue_control(&client_control_tx, &format!(":bouncer PONG bouncer :{}\r\n", String::from_utf8_lossy(p)))?; }
                            "PRIVMSG" | "NOTICE" | "JOIN" | "PART" | "NICK" | "TOPIC" | "MODE" => normal_tx.try_send(OutboundIntent { generation: _generation, class: IntentClass::NonReplayable, wire: msg.encode().map_err(|_| RuntimeError::Protocol)? }).map_err(|_| RuntimeError::QueueOverloaded)?,
                            "WHOIS" | "WHO" | "NAMES" | "LIST" => normal_tx.try_send(OutboundIntent { generation: _generation, class: IntentClass::GenerationQuery, wire: msg.encode().map_err(|_| RuntimeError::Protocol)? }).map_err(|_| RuntimeError::QueueOverloaded)?,
                            "QUIT" => return Ok(()),
                            _ => queue_line(&client_normal_tx, &format!(":bouncer 421 {} * :Unsupported command\r\n", nick))?,
                        }
                    }
                }
            }
            self.snapshot.send_modify(|state| {
                state.upstream_normal_queue_depth = normal_tx.max_capacity() - normal_tx.capacity();
                state.upstream_control_queue_depth =
                    control_tx.max_capacity() - control_tx.capacity();
                state.downstream_normal_queue_depth =
                    client_normal_tx.max_capacity() - client_normal_tx.capacity();
                state.downstream_control_queue_depth =
                    client_control_tx.max_capacity() - client_control_tx.capacity();
            });
        }
    }
}
async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
fn error_class(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::Provider(_) => "provider",
        RuntimeError::Timeout => "timeout",
        RuntimeError::Protocol => "protocol",
        RuntimeError::Registration => "registration",
        RuntimeError::Io(_) => "io",
        RuntimeError::Stopped => "stopped",
        RuntimeError::InvalidConfig => "configuration",
        RuntimeError::QueueOverloaded => "queue-overload",
        RuntimeError::GenerationExhausted => "generation-exhausted",
    }
}
fn same_nick(left: &str, right: &str, casemapping: Casemapping) -> bool {
    casemapping.fold(left.as_bytes()) == casemapping.fold(right.as_bytes())
}
fn member_nick(member: &str) -> &str {
    member.trim_start_matches(['~', '&', '@', '%', '+'])
}
fn remove_member(members: &mut BTreeSet<String>, nick: &str, casemapping: Casemapping) {
    if let Some(member) = members
        .iter()
        .find(|member| same_nick(member_nick(member), nick, casemapping))
        .cloned()
    {
        members.remove(&member);
    }
}
fn rename_member(members: &mut BTreeSet<String>, old: &str, new: &str, casemapping: Casemapping) {
    if let Some(member) = members
        .iter()
        .find(|member| same_nick(member_nick(member), old, casemapping))
        .cloned()
    {
        let prefix = &member[..member.len() - member_nick(&member).len()];
        members.remove(&member);
        members.insert(format!("{prefix}{new}"));
    }
}
fn valid_client_nick(bytes: &[u8]) -> bool {
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
fn queue_control(sender: &mpsc::Sender<Vec<u8>>, line: &str) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
        return Err(RuntimeError::Protocol);
    }
    sender
        .try_send(line.as_bytes().to_vec())
        .map_err(|_| RuntimeError::QueueOverloaded)
}
fn queue_line(sender: &mpsc::Sender<Vec<u8>>, line: &str) -> Result<(), RuntimeError> {
    if line.len() > i2pr_irc_wire::MAX_LINE_BYTES || !line.ends_with("\r\n") {
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
async fn next_queued_frame(
    control: &mut mpsc::Receiver<Vec<u8>>,
    normal: &mut mpsc::Receiver<Vec<u8>>,
) -> Option<Vec<u8>> {
    tokio::select! { biased; command = control.recv() => command, command = normal.recv() => command }
}
async fn next_intent_frame(
    control: &mut mpsc::Receiver<Vec<u8>>,
    normal: &mut mpsc::Receiver<OutboundIntent>,
) -> Option<Result<OutboundIntent, Vec<u8>>> {
    tokio::select! { biased; command = control.recv() => command.map(Err), command = normal.recv() => command.map(Ok) }
}
async fn send<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, s: &str) -> Result<(), std::io::Error> {
    tokio::time::timeout(STREAM_WRITE_TIMEOUT, async {
        w.write_all(s.as_bytes()).await?;
        w.flush().await
    })
    .await
    .unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "bounded stream write timed out",
        ))
    })
}
async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    tokio::time::timeout(STREAM_WRITE_TIMEOUT, async {
        w.write_all(bytes).await?;
        w.flush().await
    })
    .await
    .unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
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
    use i2pr_irc_core::{ClientId, I2pEndpoint};
    use i2pr_irc_testkit::{FakeI2pStreamProvider, FakeLocalAcceptor, FaultScript};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct SharedProvider(Arc<FakeI2pStreamProvider>);
    #[async_trait::async_trait]
    impl I2pStreamProvider for SharedProvider {
        async fn connect(
            &self,
            endpoint: &I2pEndpoint,
        ) -> Result<Box<dyn i2pr_irc_core::ByteStream>, ProviderError> {
            self.0.connect(endpoint).await
        }
    }
    struct SharedAcceptor(Arc<FakeLocalAcceptor>);
    impl LocalAcceptor for SharedAcceptor {
        type Stream = i2pr_irc_testkit::ScriptedStream;
        async fn accept(&self) -> Result<(ClientId, Self::Stream), ProviderError> {
            self.0.accept().await
        }
    }
    async fn read_until(stream: &mut i2pr_irc_testkit::ScriptedStream, needle: &[u8]) -> Vec<u8> {
        let mut all = Vec::new();
        let mut buf = [0; 256];
        let read = async {
            while !all.windows(needle.len()).any(|w| w == needle) {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                all.extend_from_slice(&buf[..n]);
            }
        };
        if tokio::time::timeout(Duration::from_secs(3), read)
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
    #[test]
    fn secret_debug_is_redacted() {
        assert!(!format!("{:?}", Secret::new("secret".into())).contains("secret"));
    }
    #[test]
    fn backoff_is_bounded() {
        let mut b = Backoff {
            attempt: 0,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(5),
            jitter_percent: 20,
        };
        for _ in 0..20 {
            assert!(b.next_delay(7) <= b.cap)
        }
    }
    #[test]
    fn replay_class_is_explicit() {
        let i = OutboundIntent {
            generation: ConnectionGeneration(1),
            class: IntentClass::NonReplayable,
            wire: vec![],
        };
        assert!(!i.survives_disconnect())
    }
    #[test]
    fn network_configuration_rejects_injection_and_unbounded_channels() {
        let valid = UpstreamConfig {
            endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
            nick: "bot".into(),
            username: "user".into(),
            realname: "bouncer".into(),
            sasl: None,
            desired_channels: vec!["#room".into()],
        };
        assert!(valid.validate().is_ok());
        let mut invalid = valid.clone();
        invalid.nick = "bot\r\nPRIVMSG".into();
        assert!(matches!(
            invalid.validate(),
            Err(RuntimeError::InvalidConfig)
        ));
        let mut invalid = valid;
        invalid.desired_channels = vec!["#ok".into(); MAX_CHANNELS + 1];
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
            queue_line(&normal, "PRIVMSG #c :x\r\n").unwrap();
        }
        assert!(matches!(
            queue_line(&normal, "PRIVMSG #c :overflow\r\n"),
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
            next_queued_frame(&mut control_rx, &mut normal_rx)
                .await
                .unwrap(),
            b"PONG :urgent\r\n"
        );
        assert_eq!(
            next_queued_frame(&mut control_rx, &mut normal_rx)
                .await
                .unwrap(),
            b"PRIVMSG #c :queued\r\n"
        );
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
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec!["#room".into()],
            },
        )
        .unwrap();
        let mut state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let mut downstream = local.take_peer().await;
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
        while state.borrow().phase != Some(Phase::Online) {
            state.changed().await.unwrap();
        }
        assert_eq!(state.borrow().phase, Some(Phase::Online));
        assert_eq!(state.borrow().generation, Some(ConnectionGeneration(1)));
        upstream.write_all(b":bot!u@h JOIN #room\r\n:srv 005 bot CASEMAPPING=rfc1459 CHANTYPES=#& :are supported by this server\r\n:srv 332 bot #room :subject\r\n:srv MODE #room +nt\r\n:srv 353 bot = #room :bot @Alice\r\n:srv 366 bot #room :End\r\n").await.unwrap();
        while state.borrow().upstream_events_seen < 6 {
            state.changed().await.unwrap();
        }
        downstream
            .write_all(b"CAP LS 302\r\nCAP REQ :message-tags\r\nNICK mobile\r\nNICK bot\r\nUSER bot 0 * :phone\r\nCAP END\r\n")
            .await
            .unwrap();
        let welcome = read_until(&mut downstream, b"366 bot #room :End of NAMES list\r\n").await;
        let projection = String::from_utf8_lossy(&welcome);
        assert!(projection.contains("001 bot"));
        assert!(projection.contains("CAP * NAK :Unsupported capabilities"));
        assert!(projection.contains("433 * * :Nickname unavailable on this network"));
        assert!(projection.contains("CASEMAPPING=rfc1459"), "{projection}");
        assert!(projection.contains("332 bot #room :subject"));
        assert!(projection.contains("324 bot #room +nt"));
        assert!(projection.contains("353 bot = #room :@Alice bot"));
        assert!(!projection.contains("421 bot"));
        downstream.write_all(b"WHOIS Alice\r\n").await.unwrap();
        let query = read_until(&mut upstream, b"WHOIS Alice\r\n").await;
        assert!(String::from_utf8_lossy(&query).contains("WHOIS Alice"));
        upstream
            .write_all(b"@time=123 :srv NOTICE mobile :tagged\r\n")
            .await
            .unwrap();
        let forwarded = read_until(&mut downstream, b"NOTICE mobile :tagged\r\n").await;
        assert!(!String::from_utf8_lossy(&forwarded).contains("@time="));
        downstream
            .write_all(b"PRIVMSG #room :hello\r\n")
            .await
            .unwrap();
        assert_eq!(
            Message::parse(b"PRIVMSG #room :hello\r\n")
                .unwrap()
                .encode()
                .unwrap(),
            b"PRIVMSG #room :hello\r\n"
        );
        let chat = tokio::time::timeout(
            Duration::from_secs(3),
            read_until(&mut upstream, b"PRIVMSG #room :hello\r\n"),
        )
        .await
        .unwrap_or_else(|_| panic!("runtime ended: {}", task.is_finished()));
        assert!(String::from_utf8_lossy(&chat).contains("PRIVMSG #room :hello"));
        upstream.write_all(b"PING :alive\r\n").await.unwrap();
        let pong = read_until(&mut upstream, b"PONG :alive\r\n").await;
        assert!(String::from_utf8_lossy(&pong).contains("PONG :alive"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn configured_sasl_plain_completes_without_secret_diagnostics() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(2), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: Some(("alice".into(), Secret::new("swordfish".into()))),
                desired_channels: vec![],
            },
        )
        .unwrap();
        assert!(!format!("{:?}", supervisor.config.sasl).contains("swordfish"));
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
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
        let payload = format!("AUTHENTICATE {}\r\n", encoded);
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
        local
            .queue_outcome(Ok((ClientId(3), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: Some(("alice".into(), Secret::new("swordfish".into()))),
                desired_channels: vec![],
            },
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
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
    async fn missing_matching_pong_reconnects_on_virtual_time() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(10), FaultScript::default())))
            .unwrap();
        local
            .queue_outcome(Ok((ClientId(11), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec![],
            },
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let ping1 = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        assert!(String::from_utf8_lossy(&ping1).contains("PING :bouncer-1"));
        upstream
            .write_all(b":srv PONG bot :bouncer-1\r\n")
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;
        let _ = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        upstream
            .write_all(b":srv PONG bot :bouncer-1\r\n")
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;
        let _ = read_until(&mut upstream, b"PING :bouncer-1\r\n").await;
        tokio::time::advance(Duration::from_secs(120)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let mut second_upstream = provider.take_peer().await;
        let _second_downstream = local.take_peer().await;
        let initial = read_until(&mut second_upstream, b"USER user 0 * :bouncer\r\n").await;
        assert!(String::from_utf8_lossy(&initial).contains("CAP LS 302"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_chat_is_not_replayed_into_replacement_generation() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        for id in [ClientId(20), ClientId(21)] {
            local
                .queue_outcome(Ok((id, FaultScript::default())))
                .unwrap();
        }
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec!["#persistent".into()],
            },
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let mut downstream = local.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        upstream
            .write_all(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
            .await
            .unwrap();
        let first_join = read_until(&mut upstream, b"JOIN #persistent\r\n").await;
        assert!(String::from_utf8_lossy(&first_join).contains("JOIN #persistent"));
        downstream
            .write_all(b"NICK bot\r\nUSER bot 0 * :phone\r\n")
            .await
            .unwrap();
        let _ = read_until(&mut downstream, b"001 bot :Welcome\r\n").await;
        downstream
            .write_all(b"PRIVMSG #room :possibly delivered\r\n")
            .await
            .unwrap();
        let sent = read_until(&mut upstream, b"PRIVMSG #room :possibly delivered\r\n").await;
        assert!(String::from_utf8_lossy(&sent).contains("possibly delivered"));
        upstream.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        let mut replacement = provider.take_peer().await;
        let _replacement_client = local.take_peer().await;
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
            .queue_outcome(Ok((ClientId(30), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec![],
            },
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        tokio::task::yield_now().await;
        for _ in 0..100 {
            tokio::time::advance(Duration::from_secs(301)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(provider.requested_endpoints().len(), 101);
        let _upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stop_cancels_in_progress_registration() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(40), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec![],
            },
        )
        .unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn registration_deadline_is_reported_and_stop_cancels_backoff() {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        provider.queue_outcome(Ok(FaultScript::default())).unwrap();
        let local = Arc::new(FakeLocalAcceptor::default());
        local
            .queue_outcome(Ok((ClientId(50), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec![],
            },
        )
        .unwrap();
        let state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
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
        local
            .queue_outcome(Ok((ClientId(51), FaultScript::default())))
            .unwrap();
        let supervisor = NetworkSupervisor::new(
            SharedProvider(provider.clone()),
            UpstreamConfig {
                endpoint: I2pEndpoint::parse("irc.example.i2p").unwrap(),
                nick: "bot".into(),
                username: "user".into(),
                realname: "bouncer".into(),
                sasl: None,
                desired_channels: vec![],
            },
        )
        .unwrap();
        let state = supervisor.subscribe_snapshot();
        let (stop_tx, stop_rx) = watch::channel(false);
        let acceptor = SharedAcceptor(local.clone());
        let task = tokio::spawn(async move { supervisor.serve(&acceptor, stop_rx).await });
        let mut upstream = provider.take_peer().await;
        let _downstream = local.take_peer().await;
        let _ = read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        tokio::time::advance(CAP_SASL_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(state.borrow().phase, Some(Phase::Backoff));
        assert_eq!(state.borrow().last_error, Some("timeout"));
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
}
