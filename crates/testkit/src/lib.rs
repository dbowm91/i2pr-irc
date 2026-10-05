//! Deterministic bounded byte-stream fixtures shared by core integration tests.
use async_trait::async_trait;
use i2pr_irc_core::{ByteStream, I2pEndpoint, I2pStreamProvider, ProviderError};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Clone, Debug)]
pub struct FaultScript {
    pub max_read: usize,
    pub max_write: usize,
    pub capacity: usize,
    pub eof_after: Option<usize>,
}
impl Default for FaultScript {
    fn default() -> Self {
        Self {
            max_read: usize::MAX,
            max_write: usize::MAX,
            capacity: 64 * 1024,
            eof_after: None,
        }
    }
}
struct Shared {
    incoming: VecDeque<u8>,
    written: Vec<u8>,
    script: FaultScript,
    read_count: usize,
}
pub struct ScriptedStream {
    shared: Arc<Mutex<Shared>>,
}
impl ScriptedStream {
    pub fn pair(input: impl Into<Vec<u8>>, script: FaultScript) -> (Self, Arc<Mutex<Vec<u8>>>) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(Mutex::new(Shared {
            incoming: input.into().into(),
            written: Vec::new(),
            script,
            read_count: 0,
        }));
        (Self { shared }, writes)
    }
    pub fn bytes_written(&self) -> Vec<u8> {
        self.shared.lock().unwrap().written.clone()
    }
}
impl AsyncRead for ScriptedStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let mut s = self.shared.lock().unwrap();
        if s.script.eof_after.is_some_and(|n| s.read_count >= n) || s.incoming.is_empty() {
            return std::task::Poll::Ready(Ok(()));
        }
        let n = buf.remaining().min(s.script.max_read).min(s.incoming.len());
        for _ in 0..n {
            if let Some(b) = s.incoming.pop_front() {
                buf.put_slice(&[b]);
                s.read_count += 1
            }
        }
        std::task::Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for ScriptedStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        b: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let mut s = self.shared.lock().unwrap();
        if s.written.len() >= s.script.capacity {
            return std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "bounded fixture full",
            )));
        }
        let n = b
            .len()
            .min(s.script.max_write)
            .min(s.script.capacity - s.written.len());
        s.written.extend_from_slice(&b[..n]);
        std::task::Poll::Ready(Ok(n))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
#[derive(Default)]
pub struct FakeI2pStreamProvider {
    pub requested: Mutex<Vec<I2pEndpoint>>,
    pub outcomes: Mutex<VecDeque<Result<Vec<u8>, ProviderError>>>,
    pub scripts: Mutex<VecDeque<FaultScript>>,
}
#[async_trait]
impl I2pStreamProvider for FakeI2pStreamProvider {
    async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError> {
        self.requested.lock().unwrap().push(endpoint.clone());
        let result = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(ProviderError::Unavailable));
        let input = result?;
        let script = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::new(ScriptedStream::pair(input, script).0))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    #[tokio::test]
    async fn short_writes_are_ordered() {
        let (mut s, _) = ScriptedStream::pair(
            Vec::<u8>::new(),
            FaultScript {
                max_write: 2,
                ..Default::default()
            },
        );
        s.write_all(b"abcdef").await.unwrap();
        assert_eq!(s.bytes_written(), b"abcdef");
    }
}
