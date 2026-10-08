//! The authenticated handoff from the local authority to existing runtime admission.
use i2pr_irc_core::SessionIdAllocator;
use i2pr_irc_runtime::{
    AdmissionOutcome, DownstreamAdmission, PreAuthenticatedRegistration, RuntimeControlHandle,
    admission::NetworkSelection,
};
use i2pr_irc_store::{ClientId, NetworkId, StoreHandle};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{Semaphore, watch},
    task::JoinSet,
};

use crate::listener::{AuthenticatedCheckpoint, CredentialVerifier};

/// Runs the production local-accept handoff with explicit credential authority. The
/// caller owns process signals, Store, and RuntimeController shutdown ordering.
pub async fn serve_local_access(
    address: SocketAddr,
    verifier: Arc<dyn CredentialVerifier>,
    store: StoreHandle,
    control: RuntimeControlHandle,
    default_network: Option<NetworkId>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let (handoffs_tx, mut handoffs_rx) =
        tokio::sync::mpsc::channel(crate::listener::MAX_AUTHENTICATED_HANDOFFS);
    let listener_stop = stop.clone();
    let mut listener_task = tokio::spawn(async move {
        crate::listener::serve(address, verifier, handoffs_tx, listener_stop).await
    });
    let permits = Arc::new(Semaphore::new(crate::listener::MAX_PARALLEL_HANDSHAKES));
    let sessions = SessionIdAllocator::new();
    let mut admitted = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break Ok(()); }
            }
            result = &mut listener_task => {
                break match result {
                    Ok(result) => result,
                    Err(_) => Err("local listener task failed".to_owned()),
                };
            }
            checkpoint = handoffs_rx.recv() => {
                let Some(checkpoint) = checkpoint else { break Ok(()); };
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(checkpoint); continue; };
                let store = store.clone();
                let control = control.clone();
                let sessions = sessions.clone();
                admitted.spawn(async move {
                    let _permit = permit;
                    let _ = admit_authenticated(checkpoint, default_network, &store, control, &sessions).await;
                });
                while admitted.len() > crate::listener::MAX_PARALLEL_HANDSHAKES {
                    let _ = admitted.join_next().await;
                }
            }
            _ = admitted.join_next(), if !admitted.is_empty() => {}
        }
    };
    if !listener_task.is_finished() {
        // The same stop source reaches the accept loop; sending has already happened at
        // the process coordinator. This bound identifies a broken cancellation path.
        if tokio::time::timeout(Duration::from_secs(10), &mut listener_task)
            .await
            .is_err()
        {
            listener_task.abort();
            let _ = listener_task.await;
        }
    }
    admitted.abort_all();
    while admitted.join_next().await.is_some() {}
    result
}

/// Resolves the durable cursor identity only after the listener has authenticated the
/// Operator. Store creation is transactional and repeated/concurrent profile login
/// converges through the existing unique client-login index.
pub async fn admit_authenticated(
    checkpoint: AuthenticatedCheckpoint,
    default_network: Option<NetworkId>,
    store: &StoreHandle,
    control: RuntimeControlHandle,
    sessions: &SessionIdAllocator,
) -> Result<(ClientId, AdmissionOutcome), String> {
    let (stream, state) = checkpoint.into_parts();
    let session = sessions
        .allocate()
        .ok_or_else(|| "session capacity exhausted".to_owned())?;
    let selected = match default_network {
        Some(network) => {
            let record = control
                .network_record(network)
                .await
                .map_err(|_| "configured default network is unavailable".to_owned())?
                .ok_or_else(|| "configured default network does not exist".to_owned())?;
            Some(NetworkSelection {
                network,
                expected_nick: record.nick,
            })
        }
        None => None,
    };
    let canonical_profile = state.profile.to_ascii_lowercase();
    let (client, _) = store
        .create_client(&canonical_profile)
        .await
        .map_err(|_| "cannot resolve authenticated client profile".to_owned())?;
    let registration = PreAuthenticatedRegistration {
        negotiated_capabilities: state.negotiated_capabilities.into_iter().collect(),
        unread: state.unread,
    };
    let outcome = DownstreamAdmission::new(selected, control, session, client)
        .run_pre_authenticated(Box::new(stream), registration)
        .await;
    Ok((client, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listener::{CredentialVerifier, authenticate};
    use i2pr_irc_runtime::RuntimeController;
    use i2pr_irc_sam::SamProvider;
    use i2pr_irc_store::{Store, StoreOpenOptions, StorePath};
    use std::{fs, net::SocketAddr, sync::Arc};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::watch,
    };

    struct Fixture;
    impl CredentialVerifier for Fixture {
        fn verify(&self, profile: &str, token: &[u8]) -> bool {
            matches!(profile, "Mobile" | "Other") && token == b"fixture-token-32-bytes-long-enough"
        }
    }

    async fn authenticated_socket(address: SocketAddr, profile: &str) -> AuthenticatedCheckpoint {
        let listener = TcpListener::bind(address).await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            authenticate(stream, Arc::new(Fixture)).await.unwrap()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        let transcript = format!(
            "CAP LS 302\r\nPASS {profile}:fixture-token-32-bytes-long-enough\r\nNICK m\r\nUSER u 0 * :test\r\nCAP END\r\n"
        );
        client.write_all(transcript.as_bytes()).await.unwrap();
        drop(client);
        task.await.unwrap()
    }

    #[tokio::test]
    async fn authenticated_profiles_have_stable_distinct_client_ids() {
        let dir = std::env::temp_dir().join(format!("i2pr-profile-lineage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open_with_options(
            &StorePath::File(dir.join("store.sqlite3")),
            StoreOpenOptions::plaintext(),
        )
        .unwrap();
        let (mut controller, control) =
            RuntimeController::new(SamProvider::new(), store.handle_clone());
        let runtime = tokio::spawn(async move { controller.serve().await });
        let sessions = SessionIdAllocator::new();
        let first = authenticated_socket("127.0.0.1:0".parse().unwrap(), "Mobile").await;
        let second = authenticated_socket("127.0.0.1:0".parse().unwrap(), "Mobile").await;
        let (first_result, second_result) = tokio::join!(
            admit_authenticated(first, None, store.handle(), control.clone(), &sessions),
            admit_authenticated(second, None, store.handle(), control.clone(), &sessions),
        );
        let (first_client, first_outcome) = first_result.unwrap();
        let (second_client, second_outcome) = second_result.unwrap();
        assert!(matches!(first_outcome, AdmissionOutcome::Unbound { .. }));
        assert!(matches!(second_outcome, AdmissionOutcome::Unbound { .. }));
        assert_eq!(first_client, second_client);
        let other = authenticated_socket("127.0.0.1:0".parse().unwrap(), "Other").await;
        let (other_client, _) =
            admit_authenticated(other, None, store.handle(), control.clone(), &sessions)
                .await
                .unwrap();
        assert_ne!(first_client, other_client);
        control.request_stop();
        runtime.await.unwrap().unwrap();
        store.shutdown().unwrap();
        let reopened = Store::open_with_options(
            &StorePath::File(dir.join("store.sqlite3")),
            StoreOpenOptions::plaintext(),
        )
        .unwrap();
        let (restored_client, created) = reopened.handle().create_client("mobile").await.unwrap();
        assert!(!created);
        assert_eq!(restored_client, first_client);
        reopened.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn production_local_pipeline_authenticates_then_admits_and_stops() {
        let dir =
            std::env::temp_dir().join(format!("i2pr-listener-pipeline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open_with_options(
            &StorePath::File(dir.join("store.sqlite3")),
            StoreOpenOptions::plaintext(),
        )
        .unwrap();
        let (mut controller, control) =
            RuntimeController::new(SamProvider::new(), store.handle_clone());
        let runtime = tokio::spawn(async move { controller.serve().await });
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let (stop_tx, stop_rx) = watch::channel(false);
        let local = tokio::spawn(serve_local_access(
            address,
            Arc::new(Fixture),
            store.handle_clone(),
            control.clone(),
            None,
            stop_rx,
        ));
        let mut client = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match TcpStream::connect(address).await {
                    Ok(client) => break client,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .unwrap();
        client.write_all(b"CAP LS 302\r\nCAP REQ :soju.im/bouncer-networks\r\nPASS Mobile:fixture-token-32-bytes-long-enough\r\nNICK m\r\nUSER u 0 * :test\r\nCAP END\r\n").await.unwrap();
        let mut received = Vec::new();
        let mut buffer = [0u8; 1024];
        while !received.windows(4).any(|part| part == b"001 ") {
            let count = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert_ne!(count, 0, "authenticated session closed before welcome");
            received.extend_from_slice(&buffer[..count]);
        }
        assert!(
            String::from_utf8_lossy(&received).contains("Welcome to the bouncer control session")
        );
        stop_tx.send(true).unwrap();
        local.await.unwrap().unwrap();
        control.request_stop();
        runtime.await.unwrap().unwrap();
        let (client_id, created) = store.handle().create_client("mobile").await.unwrap();
        assert!(!created);
        drop(client);
        store.shutdown().unwrap();
        let _ = client_id;
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn missing_default_network_fails_before_profile_creation() {
        let dir = std::env::temp_dir().join(format!("i2pr-missing-default-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open_with_options(
            &StorePath::File(dir.join("store.sqlite3")),
            StoreOpenOptions::plaintext(),
        )
        .unwrap();
        let (mut controller, control) =
            RuntimeController::new(SamProvider::new(), store.handle_clone());
        let runtime = tokio::spawn(async move { controller.serve().await });
        let checkpoint = authenticated_socket("127.0.0.1:0".parse().unwrap(), "Mobile").await;
        assert!(
            admit_authenticated(
                checkpoint,
                Some(NetworkId(99)),
                store.handle(),
                control.clone(),
                &SessionIdAllocator::new(),
            )
            .await
            .is_err()
        );
        let (_, created) = store.handle().create_client("mobile").await.unwrap();
        assert!(
            created,
            "missing default must not persist the authenticated profile"
        );
        control.request_stop();
        runtime.await.unwrap().unwrap();
        store.shutdown().unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
