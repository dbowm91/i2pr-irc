//! Single-network registration owner. Network I/O is injected through the I2P provider.
use i2pr_irc_core::{ConnectionGeneration, I2pEndpoint, I2pStreamProvider, ProviderError};
use i2pr_irc_wire::{LineDecoder, Message};
use std::{collections::BTreeSet, fmt, time::Duration};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
    time::timeout,
};
use zeroize::Zeroize;

pub const UPSTREAM_QUEUE_CAPACITY: usize = 64;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
pub const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(180);
pub const MAX_CHANNELS: usize = 128;
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
#[derive(Clone, Debug, Default)]
pub struct NetworkSnapshot {
    pub phase: Option<Phase>,
    pub generation: Option<ConnectionGeneration>,
    pub nick: Option<String>,
    pub channels: Vec<String>,
    pub reconnect_attempt: u32,
    pub last_error: Option<&'static str>,
}
pub struct NetworkSupervisor<P> {
    provider: P,
    config: UpstreamConfig,
}
impl<P: I2pStreamProvider> NetworkSupervisor<P> {
    pub fn new(provider: P, config: UpstreamConfig) -> Self {
        Self { provider, config }
    }
    pub async fn register_once(
        &self,
        generation: ConnectionGeneration,
        mut stop: watch::Receiver<bool>,
    ) -> Result<NetworkSnapshot, RuntimeError> {
        let stream = tokio::select! {_ = stopped(&mut stop)=>return Err(RuntimeError::Stopped),r=timeout(CONNECT_TIMEOUT,self.provider.connect(&self.config.endpoint))=>r.map_err(|_|RuntimeError::Timeout)??};
        let (mut reader, mut writer) = tokio::io::split(stream);
        send(
            &mut writer,
            &format!(
                "CAP LS 302\r\nNICK {}\r\nUSER {} 0 * :{}\r\n",
                self.config.nick, self.config.username, self.config.realname
            ),
        )
        .await?;
        let mut decoder = LineDecoder::default();
        let mut buf = [0u8; 2048];
        let mut offered = BTreeSet::new();
        let mut requested = false;
        let mut sasl_started = false;
        let registration = async {
            loop {
                let n = reader.read(&mut buf).await?;
                if n == 0 {
                    return Err(RuntimeError::Protocol);
                }
                for line in decoder.push(&buf[..n]) {
                    let line = line.map_err(|_| RuntimeError::Protocol)?;
                    let msg = Message::parse(&line).map_err(|_| RuntimeError::Protocol)?;
                    let cmd = String::from_utf8_lossy(&msg.command).to_ascii_uppercase();
                    let p: Vec<String> = msg
                        .params
                        .iter()
                        .map(|x| String::from_utf8_lossy(x).into_owned())
                        .collect();
                    match cmd.as_str() {
                        "CAP" => {
                            let text = p.join(" ");
                            if text.contains(" LS ") || text.starts_with("LS ") {
                                let caps = text
                                    .split_whitespace()
                                    .filter(|x| !matches!(*x, "LS" | "302" | "*" | "=LS"))
                                    .map(|x| {
                                        x.trim_start_matches(':')
                                            .split('=')
                                            .next()
                                            .unwrap_or("")
                                            .to_owned()
                                    })
                                    .filter(|x| !x.is_empty());
                                offered.extend(caps);
                                if !requested {
                                    let mut wanted = vec![
                                        "server-time",
                                        "message-tags",
                                        "batch",
                                        "echo-message",
                                    ];
                                    if self.config.sasl.is_some() {
                                        wanted.push("sasl")
                                    }
                                    let selected: Vec<_> = wanted
                                        .into_iter()
                                        .filter(|c| offered.contains(*c))
                                        .collect();
                                    if selected.is_empty() {
                                        send(&mut writer, "CAP END\r\n").await?
                                    } else {
                                        send(
                                            &mut writer,
                                            &format!("CAP REQ :{}\r\n", selected.join(" ")),
                                        )
                                        .await?
                                    }
                                    requested = true;
                                }
                            }
                            if (text.contains(" ACK ") || text.starts_with("ACK "))
                                && self.config.sasl.is_some()
                                && offered.contains("sasl")
                                && !sasl_started
                            {
                                send(&mut writer, "AUTHENTICATE PLAIN\r\n").await?;
                                sasl_started = true;
                            }
                            if text.contains(" NAK ") && self.config.sasl.is_some() {
                                return Err(RuntimeError::Registration);
                            }
                        }
                        "AUTHENTICATE" if p.first().is_some_and(|v| v == "+") => {
                            let (user, password) = self
                                .config
                                .sasl
                                .as_ref()
                                .ok_or(RuntimeError::Registration)?;
                            let raw = format!("\0{}\0{}", user, password.0);
                            let encoded = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                raw.as_bytes(),
                            );
                            for chunk in encoded.as_bytes().chunks(400) {
                                send(
                                    &mut writer,
                                    &format!("AUTHENTICATE {}\r\n", String::from_utf8_lossy(chunk)),
                                )
                                .await?;
                            }
                            if encoded.len() % 400 == 0 {
                                send(&mut writer, "AUTHENTICATE +\r\n").await?;
                            }
                        }
                        "903" => send(&mut writer, "CAP END\r\n").await?,
                        "904" | "905" | "906" | "907" | "465" => {
                            return Err(RuntimeError::Registration);
                        }
                        "PING" => {
                            let token = p.last().ok_or(RuntimeError::Protocol)?;
                            send(&mut writer, &format!("PONG :{}\r\n", token)).await?
                        }
                        "001" => {
                            send(&mut writer, "CAP END\r\n").await?;
                            for c in self.config.desired_channels.iter().take(MAX_CHANNELS) {
                                send(&mut writer, &format!("JOIN {}\r\n", c)).await?;
                            }
                            return Ok(());
                        }
                        "ERROR" => return Err(RuntimeError::Registration),
                        _ => {}
                    }
                }
            }
        };
        let result = tokio::select! {_ = stopped(&mut stop)=>Err(RuntimeError::Stopped),r=timeout(REGISTRATION_TIMEOUT,registration)=>r.map_err(|_|RuntimeError::Timeout)?};
        result?;
        Ok(NetworkSnapshot {
            phase: Some(Phase::Online),
            generation: Some(generation),
            nick: Some(self.config.nick.clone()),
            channels: self
                .config
                .desired_channels
                .iter()
                .take(MAX_CHANNELS)
                .cloned()
                .collect(),
            ..Default::default()
        })
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
async fn send<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, s: &str) -> Result<(), std::io::Error> {
    w.write_all(s.as_bytes()).await?;
    w.flush().await
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
}
