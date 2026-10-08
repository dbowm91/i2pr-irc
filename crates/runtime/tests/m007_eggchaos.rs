//! Explicit external qualification of the production SAM provider through eggchaos.
//!
//! This suite is ignored in ordinary runs. `scripts/qualify-m007-eggchaos.py` supplies a
//! pinned eggchaos binary and runs it when the external qualification target is requested.
#![cfg(test)]

use std::{
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use i2pr_irc_core::{I2pEndpoint, NetworkId};
use i2pr_irc_runtime::RuntimeController;
use i2pr_irc_sam::{
    SamClientConfig, SamProvider, SamTimeouts,
    fake::{FakeBridge, Script},
};
use i2pr_irc_store::{NetworkRecord, Store, StorePath};
use tokio::time::sleep;

const CEILING: Duration = Duration::from_secs(30);

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_admin(binary: &std::ffi::OsStr, admin: SocketAddr, child: &mut Child) {
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if child
            .try_wait()
            .expect("eggchaos status is readable")
            .is_some()
        {
            panic!("eggchaos exited before binding its admin endpoint");
        }
        let endpoint = format!("http://{admin}");
        if Command::new(binary)
            .args(["--admin", &endpoint, "health"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "eggchaos admin endpoint did not start"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn record() -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(41),
        display_name: "eggchaos-qualification".into(),
        endpoint: I2pEndpoint::parse("irc.example.i2p").expect("I2P endpoint parses"),
        nick: "bot".into(),
        username: "user".into(),
        realname: "qualification".into(),
        auto_away: false,
        keep_nick: false,
        sasl: None,
        desired_channels: vec![i2pr_irc_store::DesiredChannelRecord::at("#room", 0, false)],
    }
}

#[tokio::test]
#[ignore = "run only through the explicit pinned eggchaos qualification target"]
async fn production_sam_provider_registers_through_jitter_bandwidth_and_slicing() {
    let binary = std::env::var_os("EGGCHAOS_BIN").expect("qualification script sets EGGCHAOS_BIN");
    let bridge = FakeBridge::start(Script::healthy(1)).await;
    let proxy: SocketAddr = std::env::var("EGGCHAOS_PROXY_ADDR")
        .expect("qualification script reserves proxy address")
        .parse()
        .expect("proxy address parses");
    let admin: SocketAddr = std::env::var("EGGCHAOS_ADMIN_ADDR")
        .expect("qualification script reserves admin address")
        .parse()
        .expect("admin address parses");
    let config_path =
        std::env::temp_dir().join(format!("i2pr-irc-m007-{}.toml", std::process::id()));
    let config = format!(
        r#"
version = 1
seed = 410041
[admin]
bind = "{admin}"
[[proxy]]
name = "sam-loopback"
listen = "{proxy}"
upstream = "{}"
seed = 410041
max_connections = 8
[[proxy.fault]]
id = "latency-jitter"
direction = "upstream"
type = "latency"
delay = "10ms"
jitter = "5ms"
[[proxy.fault]]
id = "bandwidth-cap"
direction = "upstream"
type = "bandwidth"
bytes_per_second = 262144
burst_bytes = 65536
[[proxy.fault]]
id = "slicing"
direction = "downstream"
type = "slice"
average_size = 7
variation = 3
"#,
        bridge.endpoint().socket_addr()
    );
    std::fs::write(&config_path, config).expect("temporary eggchaos config writes");
    let mut child = ChildGuard(
        Command::new(&binary)
            .arg("serve")
            .arg("--config")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("pinned eggchaos starts"),
    );
    wait_admin(&binary, admin, &mut child.0).await;

    let bridge_endpoint = i2pr_irc_sam::SamBridgeEndpoint::parse(&proxy.to_string())
        .expect("loopback proxy address is a SAM bridge endpoint");
    let provider = Arc::new(SamProvider::with_config(SamClientConfig {
        bridge: bridge_endpoint,
        timeouts: SamTimeouts::default(),
        random: Arc::new(i2pr_irc_sam::session_id::OsRandom),
    }));
    let store = Store::open(&StorePath::Memory).expect("store opens");
    let handle = store.handle_clone();
    let (mut controller, control) = RuntimeController::new(Arc::clone(&provider), handle);
    let task = tokio::spawn(async move { controller.serve().await });
    let mut status = control.subscribe_status();
    tokio::time::timeout(CEILING, status.changed())
        .await
        .expect("controller starts")
        .expect("status watch remains open");
    control.create(record()).await.expect("Network creates");

    let peer = bridge.peer();
    assert!(
        peer.wait_for(b"USER user", CEILING).await,
        "SAM stream carries IRC registration"
    );
    peer.send(b":srv CAP * LS :\r\n:srv 001 bot :welcome\r\n")
        .await;
    assert!(
        peer.wait_for(b"JOIN #room", CEILING).await,
        "registration and desired join survive the proxy"
    );
    let initial_stream_attempts = provider.diagnostics().stream_attempts;
    sleep(Duration::from_secs(20)).await;
    let snapshot = control
        .status()
        .await
        .expect("diagnostics answer after stable baseline");
    assert!(
        snapshot
            .networks
            .iter()
            .any(|network| network.network == NetworkId(41) && network.live)
    );
    assert_eq!(
        provider.diagnostics().stream_attempts,
        initial_stream_attempts,
        "the stable baseline does not replace the upstream generation"
    );
    let admin_url = format!("http://{admin}");
    let high_latency = Command::new(&binary)
        .args([
            "--admin",
            &admin_url,
            "fault",
            "set",
            "sam-loopback",
            "latency-jitter",
            "--kind",
            "latency",
            "--delay-ms",
            "50",
            "--jitter-ms",
            "20",
        ])
        .status()
        .expect("eggchaos applies the elevated latency profile");
    assert!(
        high_latency.success(),
        "eggchaos accepts the elevated profile"
    );
    sleep(Duration::from_secs(90)).await;
    let snapshot = control
        .status()
        .await
        .expect("diagnostics answer after elevated latency");
    assert!(
        snapshot
            .networks
            .iter()
            .any(|network| network.network == NetworkId(41) && network.live),
        "bounded latency and jitter do not cause a false reconnect"
    );
    assert_eq!(
        provider.diagnostics().stream_attempts,
        initial_stream_attempts,
        "elevated latency does not replace the upstream generation"
    );
    let normal_latency = Command::new(&binary)
        .args([
            "--admin",
            &admin_url,
            "fault",
            "set",
            "sam-loopback",
            "latency-jitter",
            "--kind",
            "latency",
            "--delay-ms",
            "10",
            "--jitter-ms",
            "5",
        ])
        .status()
        .expect("eggchaos restores baseline latency");
    assert!(
        normal_latency.success(),
        "eggchaos restores baseline profile"
    );
    assert_eq!(peer.refused(), 0);
    let peak_provider = provider.diagnostics();
    assert_eq!(peak_provider.live_scopes, 1);
    assert_eq!(peak_provider.healthy_scopes, 1);
    assert_eq!(peak_provider.session_creations, 1);

    control
        .delete(NetworkId(41))
        .await
        .expect("Network deletes");
    let release_deadline = tokio::time::Instant::now() + CEILING;
    while provider.diagnostics().live_scopes != 0 {
        assert!(
            tokio::time::Instant::now() < release_deadline,
            "SAM scope returns to baseline after deletion"
        );
        sleep(Duration::from_millis(20)).await;
    }
    control.request_stop();
    tokio::time::timeout(CEILING, task)
        .await
        .expect("controller stops")
        .expect("controller joins")
        .expect("controller stops cleanly");
    store.shutdown().expect("store shuts down");
    let _ = std::fs::remove_file(config_path);
}
