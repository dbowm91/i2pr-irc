//! Bounded private local authentication substrate. The returned checkpoint remains
//! private to the daemon until Plan 052 transfers it into the canonical session reader.
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_HANDSHAKE_LINE: usize = 512;
pub const MAX_HANDSHAKE_BUFFER: usize = 4096;
pub const MAX_HANDSHAKE_LINES: usize = 32;
pub const MAX_PARALLEL_HANDSHAKES: usize = 64;
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);

pub trait CredentialVerifier: Send + Sync + 'static {
    /// Verifies a generated high-entropy local bearer token. Implementations must not
    /// expose the credential in errors or formatting.
    fn verify(&self, profile: &str, token: &[u8]) -> bool;
}

/// One authenticated stream and all bytes consumed beyond the authentication boundary.
/// This type is intentionally not public: only the daemon may hold it before Plan 052.
pub(crate) struct AuthenticatedCheckpoint {
    pub(crate) stream: TcpStream,
    pub(crate) profile: String,
    pub(crate) nick: String,
    pub(crate) user: String,
    pub(crate) unread: Vec<u8>,
    pub(crate) cap_sasl_acked: bool,
}

pub fn validate_loopback(address: SocketAddr) -> Result<(), &'static str> {
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("listener must use a numeric loopback address and nonzero port");
    }
    Ok(())
}

pub async fn serve(
    address: SocketAddr,
    verifier: Arc<dyn CredentialVerifier>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    validate_loopback(address).map_err(str::to_owned)?;
    let listener = TcpListener::bind(address)
        .await
        .map_err(|_| "cannot bind local listener".to_owned())?;
    let permits = Arc::new(Semaphore::new(MAX_PARALLEL_HANDSHAKES));
    let mut tasks = JoinSet::new();
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(|_| "local accept failed".to_owned())?;
                if !peer.ip().is_loopback() { drop(stream); continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(stream); continue; };
                let verifier = Arc::clone(&verifier);
                let mut child_stop = stop.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::select! {
                        biased;
                        _ = wait_stop(&mut child_stop) => Ok(()),
                        result = authenticate(stream, verifier) => result.map(|checkpoint| {
                            let _moved_checkpoint = (checkpoint.profile, checkpoint.nick, checkpoint.user, checkpoint.unread, checkpoint.cap_sasl_acked, checkpoint.stream);
                        }),
                    };
                });
                while tasks.len() > MAX_PARALLEL_HANDSHAKES {
                    let _ = tasks.join_next().await;
                }
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _ = stop.changed().await;
}

async fn authenticate(
    mut stream: TcpStream,
    verifier: Arc<dyn CredentialVerifier>,
) -> Result<AuthenticatedCheckpoint, ()> {
    tokio::time::timeout(HANDSHAKE_DEADLINE, async move {
        let mut bytes = Zeroizing::new(Vec::with_capacity(1024));
        let mut scratch = [0u8; 1024];
        let mut lines = 0usize;
        let mut auth: Option<(String, AuthKind)> = None;
        let mut cap_open = false;
        let mut sasl_acked = false;
        let mut nick = None;
        let mut user = None;
        let mut authenticated: Option<String> = None;
        let mut sasl_chunks = 0usize;
        let mut sasl_payload = Zeroizing::new(String::new());
        loop {
            let count = stream.read(&mut scratch).await.map_err(|_| ())?;
            if count == 0 {
                return Err(());
            }
            if bytes.len().saturating_add(count) > MAX_HANDSHAKE_BUFFER {
                scratch[..count].zeroize();
                return Err(());
            }
            bytes.extend_from_slice(&scratch[..count]);
            scratch[..count].zeroize();
            while let Some(end) = bytes.windows(2).position(|part| part == b"\r\n") {
                if end + 2 > MAX_HANDSHAKE_LINE {
                    return Err(());
                }
                let line = Zeroizing::new(bytes.drain(..end + 2).collect::<Vec<u8>>());
                lines += 1;
                if lines > MAX_HANDSHAKE_LINES {
                    return Err(());
                }
                let line = std::str::from_utf8(&line[..line.len() - 2]).map_err(|_| ())?;
                let mut fields = line.splitn(3, ' ');
                let command = fields.next().ok_or(())?.to_ascii_uppercase();
                let arg = fields.next().unwrap_or("");
                let trailing = fields.next().unwrap_or("").strip_prefix(':').unwrap_or("");
                if authenticated.is_some() && matches!(command.as_str(), "PASS" | "AUTHENTICATE") {
                    return Err(());
                }
                match command.as_str() {
                    "CAP" => match arg.to_ascii_uppercase().as_str() {
                        "LS" => {
                            cap_open = true;
                            stream
                                .write_all(b":i2pr-irc CAP * LS :sasl=PLAIN\r\n")
                                .await
                                .map_err(|_| ())?;
                        }
                        "REQ" => {
                            if trailing
                                .split_ascii_whitespace()
                                .any(|name| name == "sasl" || name == "sasl=PLAIN")
                            {
                                sasl_acked = true;
                                stream
                                    .write_all(b":i2pr-irc CAP * ACK :sasl\r\n")
                                    .await
                                    .map_err(|_| ())?;
                            } else {
                                stream
                                    .write_all(b":i2pr-irc CAP * NAK :unsupported\r\n")
                                    .await
                                    .map_err(|_| ())?;
                            }
                        }
                        "END" => cap_open = false,
                        _ => return Err(()),
                    },
                    "PASS" => {
                        if auth.is_some() || authenticated.is_some() {
                            return Err(());
                        }
                        let value = line.strip_prefix("PASS ").ok_or(())?;
                        let (profile, token) = value.split_once(':').ok_or(())?;
                        validate_profile(profile)?;
                        let token = token.as_bytes();
                        if token.len() > 512 {
                            return Err(());
                        }
                        if !verifier.verify(profile, token) {
                            stream
                                .write_all(b":i2pr-irc 464 * :Authentication failed\r\n")
                                .await
                                .map_err(|_| ())?;
                            return Err(());
                        }
                        authenticated = Some(profile.to_owned());
                        auth = Some((profile.to_owned(), AuthKind::Pass));
                    }
                    "AUTHENTICATE" => {
                        if !sasl_acked || authenticated.is_some() {
                            return Err(());
                        }
                        if arg.eq_ignore_ascii_case("PLAIN") {
                            if auth.is_some() {
                                return Err(());
                            }
                            auth = Some((String::new(), AuthKind::Sasl));
                            sasl_chunks = 0;
                            sasl_payload.clear();
                            stream
                                .write_all(b"AUTHENTICATE +\r\n")
                                .await
                                .map_err(|_| ())?;
                        } else if let Some((_, AuthKind::Sasl)) = auth {
                            sasl_chunks += 1;
                            if sasl_chunks > 3 || arg.len() > 400 {
                                return Err(());
                            }
                            if arg == "+" && sasl_payload.len() < 400 {
                                return Err(());
                            }
                            if arg != "+" {
                                sasl_payload.push_str(arg);
                            }
                            if sasl_payload.len() == 400 {
                                continue;
                            }
                            if sasl_payload.is_empty() {
                                return Err(());
                            }
                            let decoded =
                                Zeroizing::new(STANDARD.decode(&*sasl_payload).map_err(|_| ())?);
                            if decoded.len() > 1024 {
                                return Err(());
                            }
                            let mut pieces = decoded.split(|byte| *byte == 0);
                            let authzid = pieces.next().ok_or(())?;
                            let authcid = pieces.next().ok_or(())?;
                            let password = pieces.next().ok_or(())?;
                            if pieces.next().is_some() {
                                return Err(());
                            }
                            let profile = std::str::from_utf8(authcid).map_err(|_| ())?;
                            validate_profile(profile)?;
                            if !authzid.is_empty() && authzid != authcid {
                                return Err(());
                            }
                            if !verifier.verify(profile, password) {
                                stream
                                    .write_all(b":i2pr-irc 904 * :SASL authentication failed\r\n")
                                    .await
                                    .map_err(|_| ())?;
                                return Err(());
                            }
                            authenticated = Some(profile.to_owned());
                            auth = Some((profile.to_owned(), AuthKind::Sasl));
                            sasl_payload.zeroize();
                            stream
                                .write_all(b":i2pr-irc 903 * :SASL authentication successful\r\n")
                                .await
                                .map_err(|_| ())?;
                        } else {
                            return Err(());
                        }
                    }
                    "NICK" => nick = Some(arg.to_owned()),
                    "USER" => user = Some(arg.to_owned()),
                    _ => return Err(()),
                }
                if let (Some(profile), Some(nick), Some(user)) =
                    (authenticated.as_ref(), nick.as_ref(), user.as_ref())
                    && !cap_open
                {
                    return Ok(AuthenticatedCheckpoint {
                        stream,
                        profile: profile.clone(),
                        nick: nick.clone(),
                        user: user.clone(),
                        unread: std::mem::take(&mut *bytes),
                        cap_sasl_acked: sasl_acked,
                    });
                }
            }
            if bytes.len() > MAX_HANDSHAKE_LINE {
                return Err(());
            }
        }
    })
    .await
    .map_err(|_| ())?
}

#[derive(Clone, Copy)]
enum AuthKind {
    Pass,
    Sasl,
}

fn validate_profile(profile: &str) -> Result<(), ()> {
    if profile.is_empty()
        || profile.len() > 32
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture;
    impl CredentialVerifier for Fixture {
        fn verify(&self, profile: &str, token: &[u8]) -> bool {
            profile == "test"
                && (constant_eq(token, b"known-high-entropy-fixture-token")
                    || (token.len() == 306 && token.iter().all(|byte| *byte == b'A')))
        }
    }

    fn constant_eq(a: &[u8], b: &[u8]) -> bool {
        let mut diff = a.len() ^ b.len();
        let max = a.len().max(b.len());
        for i in 0..max {
            diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
        }
        diff == 0
    }

    #[test]
    fn listener_address_is_loopback_only() {
        assert!(validate_loopback("127.0.0.1:6667".parse().unwrap()).is_ok());
        assert!(validate_loopback("[::1]:6667".parse().unwrap()).is_ok());
        assert!(validate_loopback("0.0.0.0:6667".parse().unwrap()).is_err());
        assert!(validate_loopback("192.0.2.1:6667".parse().unwrap()).is_err());
    }

    #[test]
    fn profile_and_constant_comparison_are_bounded() {
        assert!(validate_profile("default-1").is_ok());
        assert!(validate_profile("../other").is_err());
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(!constant_eq(b"abc", b"ab"));
        let _fixture: Arc<dyn CredentialVerifier> = Arc::new(Fixture);
    }

    async fn authenticated_fixture(transcript: &[u8]) -> AuthenticatedCheckpoint {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            authenticate(stream, Arc::new(Fixture)).await.unwrap()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(transcript).await.unwrap();
        server.await.unwrap()
    }

    #[tokio::test]
    async fn pass_checkpoint_preserves_coalesced_post_auth_bytes() {
        let checkpoint = authenticated_fixture(
            b"PASS test:known-high-entropy-fixture-token\r\nNICK nick\r\nUSER user 0 * :fixture\r\nPRIVMSG #x :not consumed\r\n",
        )
        .await;
        assert_eq!(checkpoint.profile, "test");
        assert_eq!(checkpoint.nick, "nick");
        assert_eq!(checkpoint.user, "user");
        assert_eq!(checkpoint.unread, b"PRIVMSG #x :not consumed\r\n");
    }

    #[tokio::test]
    async fn cap_sasl_plain_authenticates_without_consuming_registration_twice() {
        let encoded = STANDARD.encode(b"\0test\0known-high-entropy-fixture-token");
        let transcript = format!(
            "CAP LS 302\r\nCAP REQ :sasl\r\nAUTHENTICATE PLAIN\r\nAUTHENTICATE {encoded}\r\nNICK nick\r\nUSER user 0 * :fixture\r\nCAP END\r\n"
        );
        let checkpoint = authenticated_fixture(transcript.as_bytes()).await;
        assert_eq!(checkpoint.profile, "test");
        assert!(checkpoint.cap_sasl_acked);
    }

    #[tokio::test]
    async fn pass_first_then_cap_negotiation_and_split_frames_are_preserved() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            authenticate(stream, Arc::new(Fixture)).await.unwrap()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        let transcript = b"PASS test:known-high-entropy-fixture-token\r\nCAP LS 302\r\nNICK n\r\nUSER u 0 * :f\r\nCAP END\r\n";
        for byte in transcript {
            client.write_all(&[*byte]).await.unwrap();
        }
        let checkpoint = server.await.unwrap();
        assert_eq!(checkpoint.nick, "n");
        assert!(checkpoint.unread.is_empty());
    }

    #[tokio::test]
    async fn sasl_plain_accepts_bounded_fragmented_payload() {
        let mut plain = b"\0test\0".to_vec();
        plain.extend(std::iter::repeat_n(b'A', 306));
        let encoded = STANDARD.encode(plain);
        assert_eq!(encoded.len(), 416);
        let transcript = format!(
            "CAP LS 302\r\nCAP REQ :sasl\r\nAUTHENTICATE PLAIN\r\nAUTHENTICATE {}\r\nAUTHENTICATE {}\r\nNICK n\r\nUSER u 0 * :f\r\nCAP END\r\n",
            &encoded[..400],
            &encoded[400..]
        );
        let checkpoint = authenticated_fixture(transcript.as_bytes()).await;
        assert_eq!(checkpoint.profile, "test");
    }

    #[tokio::test]
    async fn rejected_pass_uses_fixed_secret_free_numeric() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            assert!(authenticate(stream, Arc::new(Fixture)).await.is_err());
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"PASS test:wrong\r\n").await.unwrap();
        let mut response = [0u8; 128];
        let read = client.read(&mut response).await.unwrap();
        assert_eq!(
            &response[..read],
            b":i2pr-irc 464 * :Authentication failed\r\n"
        );
        server.await.unwrap();
    }
}
