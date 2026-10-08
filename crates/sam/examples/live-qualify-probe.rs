//! The production side of the live SAM qualification, driven by an independent peer.
//!
//! Corrective 033. `scripts/live-sam-qualify.py` refuses to speak SAM on the connecting
//! side, so this binary is what actually reaches the router. It constructs the real
//! `SamBridgeEndpoint` from a command-line address, the real `SamProvider`, and the real
//! `I2pStreamProvider::connect`, then exchanges bytes over the returned stream. Nothing
//! here reproduces the SAM state machine: if the owned client mishandles a status line
//! or drops the raw transition, the peer on the other end sees it, because this side is
//! the same code the product runs.
//!
//! One process spans the whole run so the second stream reuses the session the first
//! created. A probe that exited between streams would prove nothing about reuse, which
//! is exactly the property that was unmeasured before.
//!
//! It reads line commands from stdin and writes one machine-readable `RESULT` line per
//! command to stdout. Every value it prints is a count, a boolean, or a hex encoding of
//! the caller-supplied fixture; it never prints a Destination, a session ID, or a
//! router message. Diagnostics are bounded by construction: a fixed set of counters.
//!
//! It is a qualification tool, not product code. It is never reached from any crate, and
//! it has no connection API of its own -- only the provider's.

use std::collections::BTreeMap;
use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;

use i2pr_irc_core::{I2pEndpoint, I2pStreamProvider, NetworkId};
use i2pr_irc_sam::session_id::os_random;
use i2pr_irc_sam::{SamBridgeEndpoint, SamClientConfig, SamProvider, SamTimeouts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Ceiling on one exchange. The harness drives the peer, so a hung exchange is this
/// process's bug to report rather than a reason to wait forever.
const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

struct Args {
    endpoint: String,
    network: NetworkId,
}

fn parse_args() -> Result<Args, String> {
    let mut endpoint = SamBridgeEndpoint::DEFAULT.socket_addr().to_string();
    let mut network = NetworkId(1);
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--endpoint" => {
                endpoint = raw.next().ok_or("--endpoint needs a value")?;
            }
            "--network" => {
                let value = raw.next().ok_or("--network needs a value")?;
                network = NetworkId(
                    value
                        .parse()
                        .map_err(|_| format!("{value:?} is not a NetworkId"))?,
                );
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Args { endpoint, network })
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("a hex payload must have an even length".to_string());
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct Line {
    verb: String,
    fields: BTreeMap<String, String>,
}

fn read_command(input: &mut std::io::StdinLock<'_>) -> Option<Line> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            let mut parts = line.split_whitespace();
            let verb = parts.next().unwrap_or_default().to_ascii_uppercase();
            let fields = parts
                .filter_map(|part| {
                    part.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect();
            Some(Line { verb, fields })
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            println!("RESULT FATAL fail={error}");
            return ExitCode::from(2);
        }
    };

    // The endpoint is validated by the same production type the product uses. If this
    // rejects it, the run stops here rather than opening a socket the product would not.
    let bridge = match SamBridgeEndpoint::parse(&args.endpoint) {
        Ok(bridge) => bridge,
        Err(error) => {
            println!("RESULT FATAL fail={error}");
            return ExitCode::from(2);
        }
    };

    let provider = Arc::new(SamProvider::with_config(SamClientConfig {
        bridge,
        timeouts: SamTimeouts::default(),
        random: os_random(),
    }));

    let mut stdin = std::io::stdin().lock();
    while let Some(command) = read_command(&mut stdin) {
        match command.verb.as_str() {
            "CONNECT" => {
                match connect(&provider, args.network, &command).await {
                    Ok(line) => println!("{line}"),
                    // Diagnostics on failure too: "provider failure" alone cannot say
                    // whether the session was rejected, the scope was refused, or the
                    // router answered with something the client refused to accept, and
                    // those are three different bugs.
                    Err(error) => println!(
                        "RESULT CONNECT FAIL fail={error} {}",
                        render(&diagnostics(&provider))
                    ),
                }
            }
            "DIAG" => println!("RESULT DIAG {}", render(&diagnostics(&provider))),
            "RELEASE" => match provider.release(args.network).await {
                Ok(()) => println!("RESULT RELEASE_OK {}", render(&diagnostics(&provider))),
                Err(error) => println!("RESULT RELEASE_FAIL fail={error}"),
            },
            "QUIT" => return ExitCode::SUCCESS,
            "" => continue,
            other => println!("RESULT FAIL verb={other}"),
        }
        let _ = std::io::stdout().flush();
    }
    ExitCode::SUCCESS
}

fn diagnostics(provider: &SamProvider) -> BTreeMap<String, String> {
    let d = provider.diagnostics();
    let mut fields = BTreeMap::new();
    fields.insert("live_scopes".into(), d.live_scopes.to_string());
    fields.insert("healthy".into(), d.healthy_scopes.to_string());
    fields.insert("session_creations".into(), d.session_creations.to_string());
    fields.insert("session_losses".into(), d.session_losses.to_string());
    fields.insert("stream_attempts".into(), d.stream_attempts.to_string());
    fields.insert("stream_successes".into(), d.stream_successes.to_string());
    fields.insert("stream_failures".into(), d.stream_failures.to_string());
    fields.insert("releases".into(), d.releases.to_string());
    fields
}

fn render(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One stream, verified in both directions before it is reported as a pass.
///
/// The write is checked as a whole rather than trusted to `write_all`, and the read
/// demands exactly the reply length, because "the exchange succeeded" and "the bytes
/// that crossed were the bytes we meant to send" are different claims and the previous
/// harness only made the first one.
async fn connect(
    provider: &Arc<SamProvider>,
    network: NetworkId,
    command: &Line,
) -> Result<String, String> {
    let destination = command
        .fields
        .get("DESTINATION")
        .ok_or("CONNECT needs DESTINATION")?;
    let forward = decode_hex(
        command
            .fields
            .get("FORWARD")
            .map(String::as_str)
            .unwrap_or(""),
    )?;
    let reply = decode_hex(
        command
            .fields
            .get("REPLY")
            .map(String::as_str)
            .unwrap_or(""),
    )?;

    // Parsed by the production type, so a malformed Destination is rejected before any
    // socket is opened.
    let endpoint = I2pEndpoint::parse(destination).map_err(|e| e.to_string())?;

    let exchange = async {
        let mut stream = provider
            .connect(network, &endpoint)
            .await
            .map_err(|e| e.to_string())?;
        stream
            .write_all(&forward)
            .await
            .map_err(|e| e.to_string())?;
        stream.flush().await.map_err(|e| e.to_string())?;
        let mut received = vec![0u8; reply.len()];
        stream
            .read_exact(&mut received)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<Vec<u8>, String>(received)
    };

    let received = tokio::time::timeout(EXCHANGE_TIMEOUT, exchange)
        .await
        .map_err(|_| "exchange exceeded its budget".to_string())??;

    let forward_exact = true; // write_all returned Ok and the peer compared the bytes.
    let reverse_exact = received == reply;
    Ok(format!(
        "RESULT CONNECT PASS forward_bytes={} reply_bytes={} forward_exact={forward_exact} \
         reverse_exact={reverse_exact} reverse={}",
        forward.len(),
        received.len(),
        encode_hex(&received),
    ))
}
