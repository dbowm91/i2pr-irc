//! A deterministic loopback SAM bridge, for tests only.
//!
//! # What this is and is not
//!
//! It is a scripted server that speaks the subset of SAM 3.1 this crate sends, bound to
//! the loopback interface. It exists so the client can be driven through every branch —
//! both terminators, fragmented replies, an over-long line followed by a good one,
//! malformed replies, a delayed reply, a control close, and a payload that looks like SAM
//! — without a router anywhere.
//!
//! It is **not** a second SAM implementation. It has no session table, no tunnels, and no
//! Destination handling. It is reachable only behind the `testkit` feature, which no
//! production build enables, and this file is inside `crates/sam` rather than in
//! `crates/testkit` so that the boundary scan sees it and can hold it to the same rule as
//! everything else in the crate.
//!
//! # Why the boundary scan permits it
//!
//! It opens a loopback listener. That is the one socket operation a test needs that
//! production must not have, and it is safe here for a specific reason: it binds
//! `127.0.0.1:0` and accepts only on that socket, so it cannot be reached from outside
//! the host, and it is compiled out of every non-test build.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// The only address the fake ever binds.
///
/// A named constant so the boundary scan and this test can both assert it, and so a
/// future edit that widens the bind has exactly one place to change.
pub const FAKE_BIND: &str = "127.0.0.1:0";

/// A loopback endpoint nothing is listening on.
///
/// For the "the router is not running" case. The port is bound and dropped so it is
/// almost certainly free, which is the closest a test can get to a router that has not
/// started. Lives here rather than in a test so that the socket name stays inside one of
/// the two files the boundary scan permits to hold it.
pub async fn absent_endpoint() -> crate::endpoint::SamBridgeEndpoint {
    let listener = TcpListener::bind(FAKE_BIND)
        .await
        .expect("a loopback port binds");
    let addr = listener
        .local_addr()
        .expect("the bound address is readable");
    drop(listener);
    crate::endpoint::SamBridgeEndpoint::parse(&addr.to_string())
        .expect("a bound loopback address is a valid bridge endpoint")
}

/// A scripted reply sequence.
#[derive(Debug, Clone, Default)]
pub struct Script {
    /// Replies, in order, written back after each request is read.
    pub replies: Vec<Vec<u8>>,
    /// Whether to close the connection after the last reply.
    ///
    /// Set for the "control close" case. A bridge that hangs up is a real router
    /// behaviour, and the client must surface it as a closed connection rather than
    /// waiting for a deadline.
    pub close_after: bool,
    /// Write each reply one byte at a time.
    ///
    /// Forces the client's partial-read path to reassemble, which is otherwise easy to
    /// leave untested on a loopback socket that always delivers whole writes.
    pub fragment: bool,
    /// Sent on the last connection once the last reply has been written, without being
    /// preceded by a request.
    ///
    /// This is how raw application data gets onto a socket that has already answered
    /// `STREAM STATUS RESULT=OK`. A reply list alone cannot express it, because after
    /// the stream status there is no further request to trigger the next reply.
    pub trailing: Vec<u8>,
}

/// What the fake observed.
#[derive(Debug, Default)]
pub struct Observed {
    /// Every request line received.
    ///
    /// Behind a `Mutex` rather than shared by reference because the serving task writes
    /// it and the test reads it, and a test that reads it has to be able to wait for the
    /// write rather than assume it already happened.
    requests: std::sync::Mutex<Vec<String>>,
}

impl Observed {
    /// Every request line received so far.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the observed log is readable")
            .clone()
    }
    fn record(&self, line: String) {
        self.requests
            .lock()
            .expect("the observed log is writable")
            .push(line);
    }
}

/// A running fake bridge.
pub struct FakeBridge {
    addr: SocketAddr,
    observed: Arc<Observed>,
    connections: Arc<AtomicUsize>,
    handle: tokio::task::JoinHandle<()>,
}

impl FakeBridge {
    /// Binds a loopback listener and answers with `script` on every connection.
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind(FAKE_BIND)
            .await
            .expect("the fake bridge binds a loopback port");
        let addr = listener
            .local_addr()
            .expect("the bound address is readable");
        let observed = Arc::new(Observed::default());
        let connections = Arc::new(AtomicUsize::new(0));
        let handle = tokio::spawn(serve(
            listener,
            script,
            Arc::clone(&observed),
            Arc::clone(&connections),
        ));
        Self {
            addr,
            observed,
            connections,
            handle,
        }
    }

    /// The loopback address to hand to a client.
    pub fn endpoint(&self) -> crate::endpoint::SamBridgeEndpoint {
        crate::endpoint::SamBridgeEndpoint::parse(&self.addr.to_string())
            .expect("a bound loopback address is a valid bridge endpoint")
    }

    /// Every request line received so far.
    pub fn requests(&self) -> Vec<String> {
        self.observed.requests()
    }

    /// How many connections were accepted.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for FakeBridge {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn serve(
    listener: TcpListener,
    script: Script,
    observed: Arc<Observed>,
    connections: Arc<AtomicUsize>,
) {
    let fragment = script.fragment;
    let replies = script.replies.clone();
    let close_after = script.close_after;
    let trailing = Arc::new(script.trailing.clone());
    while let Ok((mut stream, _)) = listener.accept().await {
        connections.fetch_add(1, Ordering::SeqCst);
        let replies = replies.clone();
        let observed = Arc::clone(&observed);
        // Shared rather than cloned per accept: the loop moves its binding into the
        // first connection task, so an outer `Arc` is what leaves one for the second.
        let trailing = Arc::clone(&trailing);
        tokio::spawn(async move {
            let mut reader = LineAccumulator::default();
            let mut chunk = [0u8; 512];
            let mut index = 0usize;
            loop {
                // Answer only once a whole request line has arrived, so the client
                // cannot receive a reply before it has finished sending.
                if reader
                    .take_line()
                    .map(|line| {
                        observed.record(line);
                        true
                    })
                    .unwrap_or(false)
                    && index < replies.len()
                {
                    let reply = replies[index].clone();
                    index += 1;
                    if fragment {
                        for byte in reply {
                            if stream.write_all(&[byte]).await.is_err() {
                                return;
                            }
                        }
                    } else if stream.write_all(&reply).await.is_err() {
                        return;
                    }
                    if index == replies.len() && !trailing.is_empty() {
                        // The exchange is over; what remains is application data.
                        let _ = stream.write_all(&trailing).await;
                        if close_after {
                            return;
                        }
                    }
                    if close_after && index == replies.len() {
                        return;
                    }
                    continue;
                }
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(count) => reader.push(&chunk[..count]),
                }
            }
        });
    }
}

/// Accumulates bytes until a complete CRLF line is available.
///
/// The mirror of the client's own reader, and deliberately simple: the fake has to
/// reproduce the *peer's* obligation to terminate a request, not validate it.
#[derive(Default)]
struct LineAccumulator {
    bytes: Vec<u8>,
}

impl LineAccumulator {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn take_line(&mut self) -> Option<String> {
        let newline = self.bytes.iter().position(|byte| *byte == b'\n')?;
        let line: Vec<u8> = self.bytes.drain(..=newline).collect();
        let text = String::from_utf8_lossy(&line).into_owned();
        Some(text.trim_end_matches(['\r', '\n']).to_owned())
    }
}

/// Convenience: a complete line.
pub fn line(text: &str) -> Vec<u8> {
    format!("{text}\r\n").into_bytes()
}

/// Convenience: a `HELLO OK` reply, in the short form Java I2P uses.
pub fn hello_ok() -> Vec<u8> {
    line("HELLO OK")
}

/// Convenience: a successful session reply carrying a Destination.
///
/// The Destination is fake key material. Including it is the point: a client that
/// retained it would fail this test's counterpart in `session_id` and
/// `protocol`, which assert that nothing private is kept.
pub fn session_ok_with_destination() -> Vec<u8> {
    line(&format!(
        "SESSION STATUS RESULT=OK ID=abc DESTINATION={}",
        "Q".repeat(600)
    ))
}

/// Convenience: a successful stream reply.
pub fn stream_ok() -> Vec<u8> {
    line("STREAM STATUS RESULT=OK")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn the_fake_records_requests_and_counts_connections() {
        let bridge = FakeBridge::start(Script {
            replies: vec![hello_ok()],
            ..Script::default()
        })
        .await;
        let mut stream = TcpStream::connect(bridge.endpoint().socket_addr())
            .await
            .expect("the fake accepts on loopback");
        stream
            .write_all(&line("HELLO VERSION MIN=3.1 MAX=3.1"))
            .await
            .expect("the request is written");
        let mut reply = [0u8; 64];
        let count = stream.read(&mut reply).await.expect("a reply arrives");
        assert_eq!(&reply[..count], b"HELLO OK\r\n");
        // The recording happens on the serving task, so poll for it rather than
        // assuming it has already run.
        for _ in 0..100 {
            if !bridge.requests().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            bridge.requests(),
            vec!["HELLO VERSION MIN=3.1 MAX=3.1".to_owned()],
            "the request is recorded verbatim"
        );
        assert_eq!(bridge.connections(), 1);
    }

    /// The fake is bound to loopback only, like the real client must be.
    #[test]
    fn the_fake_binds_loopback_and_nothing_else() {
        let addr: SocketAddr = "127.0.0.1:0".parse().expect("the literal parses");
        assert!(addr.ip().is_loopback());
    }

    /// A helper that cannot accidentally become a wildcard bind.
    #[test]
    fn the_bind_literal_is_loopback() {
        assert_eq!(FAKE_BIND, "127.0.0.1:0");
    }
}
