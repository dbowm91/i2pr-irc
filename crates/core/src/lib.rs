//! Domain identities and explicit stream/time capability contracts.
use async_trait::async_trait;
use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub u64);
    };
}
id!(NetworkId);
id!(ClientId);
id!(ConnectionGeneration);

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct I2pEndpoint(String);
impl fmt::Debug for I2pEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("I2pEndpoint([redacted])")
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("invalid I2P endpoint")]
    Invalid,
}
impl I2pEndpoint {
    pub fn parse(value: &str) -> Result<Self, EndpointError> {
        let v = value.trim_end_matches('.').to_ascii_lowercase();
        let valid_b32 = v.ends_with(".b32.i2p")
            && v.strip_suffix(".b32.i2p").is_some_and(|s| {
                s.len() == 52
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            });
        let valid_name = v.ends_with(".i2p")
            && v.len() <= 255
            && v.split('.').all(|p| {
                !p.is_empty()
                    && p.len() <= 63
                    && p.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        let valid_dest = !v.contains('.')
            && v.len() >= 300
            && v.len() <= 4096
            && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'~');
        if v.len() > 4096
            || v.bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b'/' || b == b':')
            || !(valid_b32 || valid_name || valid_dest)
        {
            return Err(EndpointError::Invalid);
        }
        Ok(Self(v))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    #[error("destination unavailable")]
    Unavailable,
    #[error("connection timed out")]
    Timeout,
    #[error("provider stopped")]
    Cancelled,
    #[error("provider failure")]
    Failed,
}
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ByteStream for T {}
#[async_trait]
pub trait I2pStreamProvider: Send + Sync {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError>;
}
pub trait LocalAcceptor: Send + Sync {
    type Stream: ByteStream;
    fn accept(
        &self,
    ) -> impl std::future::Future<Output = Result<(ClientId, Self::Stream), ProviderError>> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct MonoTime(pub u64);
pub trait Clock: Send + Sync {
    fn now(&self) -> MonoTime;
}
#[derive(Default)]
pub struct VirtualClock(AtomicU64);
impl VirtualClock {
    pub fn advance(&self, d: Duration) {
        self.0
            .fetch_add(d.as_millis().min(u64::MAX as u128) as u64, Ordering::SeqCst);
    }
}
impl Clock for VirtualClock {
    fn now(&self) -> MonoTime {
        MonoTime(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Casemapping {
    Ascii,
    StrictRfc1459,
    Rfc1459,
}
impl Casemapping {
    pub fn fold(self, input: &[u8]) -> Vec<u8> {
        input
            .iter()
            .map(|b| {
                let b = b.to_ascii_lowercase();
                match (self, b) {
                    (Self::Rfc1459, b'[') => b'{',
                    (Self::Rfc1459, b']') => b'}',
                    (Self::Rfc1459, b'\\') => b'|',
                    (Self::Rfc1459, b'^') => b'~',
                    (Self::StrictRfc1459, b'[') => b'{',
                    (Self::StrictRfc1459, b']') => b'}',
                    (Self::StrictRfc1459, b'\\') => b'|',
                    _ => b,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_rejects_generic_network_forms() {
        for s in [
            "irc.example.com",
            "1.2.3.4:6667",
            "https://x.i2p",
            "x.i2p:80",
            " foo.i2p",
        ] {
            assert!(I2pEndpoint::parse(s).is_err(), "{s}")
        }
        assert!(I2pEndpoint::parse("irc.example.i2p").is_ok());
    }
    #[test]
    fn casemap() {
        assert_eq!(Casemapping::Rfc1459.fold(b"Nick[\\^"), b"nick{|~");
    }
}
