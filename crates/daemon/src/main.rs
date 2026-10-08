use fs2::FileExt;
use i2pr_irc_runtime::RuntimeController;
use i2pr_irc_sam::{SamBridgeEndpoint, SamClientConfig, SamProvider};
use i2pr_irc_store::{Store, StoreOpenOptions, StorePath};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const MAX_CONFIG_BYTES: usize = 16 * 1024;
const SHUTDOWN_BOUND: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Config {
    state_dir: PathBuf,
    store_file: PathBuf,
    sam_bridge: SamBridgeEndpoint,
    listener: Option<String>,
}

impl Config {
    fn parse(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|_| "cannot read configuration".to_owned())?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("configuration exceeds size limit".to_owned());
        }
        let source =
            std::str::from_utf8(&bytes).map_err(|_| "configuration is not UTF-8".to_owned())?;
        let mut values = std::collections::BTreeMap::new();
        for (line_no, raw) in source.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("invalid config line {}", line_no + 1))?;
            let key = key.trim();
            let value = value.trim();
            if value.len() > 4096 || values.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err("duplicate or oversized configuration value".to_owned());
            }
            if !matches!(
                key,
                "version"
                    | "state_dir"
                    | "store_file"
                    | "sam_bridge"
                    | "listener"
                    | "encryption"
                    | "key_file"
            ) {
                return Err("unknown configuration key".to_owned());
            }
        }
        if values.get("version").map(String::as_str) != Some("1") {
            return Err("unsupported configuration version".to_owned());
        }
        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let resolve_config_path = |name: &str, default: &str| {
            let candidate = PathBuf::from(values.get(name).map(String::as_str).unwrap_or(default));
            if candidate.is_absolute() {
                candidate
            } else {
                config_dir.join(candidate)
            }
        };
        let encryption = values
            .get("encryption")
            .map(String::as_str)
            .unwrap_or("plaintext");
        if encryption != "plaintext" && encryption != "sqlcipher" {
            return Err("unsupported encryption mode".to_owned());
        }
        if encryption == "sqlcipher" {
            if !values.contains_key("key_file") {
                return Err("encrypted mode requires a key file reference".to_owned());
            }
            return Err("encrypted key provisioning is not available until secure init".to_owned());
        }
        let listener = values.get("listener").cloned();
        if let Some(address) = listener.as_deref() {
            let parsed = address
                .parse::<std::net::SocketAddr>()
                .map_err(|_| "listener must be a numeric loopback address".to_owned())?;
            if !parsed.ip().is_loopback() || parsed.port() == 0 {
                return Err("listener must be a numeric loopback address".to_owned());
            }
        }
        let bridge = values
            .get("sam_bridge")
            .map(String::as_str)
            .unwrap_or("127.0.0.1:7656");
        let sam_bridge = SamBridgeEndpoint::parse(bridge)
            .map_err(|_| "SAM bridge must be a numeric loopback address".to_owned())?;
        let state_dir = resolve_config_path("state_dir", "state");
        let store_file = values
            .get("store_file")
            .map(|_| resolve_config_path("store_file", "bouncer.sqlite3"))
            .unwrap_or_else(|| state_dir.join("bouncer.sqlite3"));
        Ok(Self {
            state_dir,
            store_file,
            sam_bridge,
            listener,
        })
    }
}

struct StateLease(File);

impl StateLease {
    fn acquire(state_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(state_dir)?;
        let metadata = fs::symlink_metadata(state_dir)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe state directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o002 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "world-writable state directory",
                ));
            }
        }
        let lock_path = state_dir.join(".i2pr-irc.lock");
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe lock file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.mode() & 0o022 != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "writable lock file",
                    ));
                }
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock_path)?;
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for StateLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

async fn run(config_path: &Path) -> Result<(), String> {
    let config = Config::parse(config_path)?;
    let _lease = StateLease::acquire(&config.state_dir)
        .map_err(|_| "cannot obtain exclusive state ownership".to_owned())?;
    if let Some(listener) = config.listener.as_ref() {
        return Err(format!(
            "listener {listener} is configured but not activated in this bootstrap milestone"
        ));
    }
    if !config.store_file.is_absolute() {
        return Err("store path must resolve to an absolute path".to_owned());
    }
    let store_metadata = fs::symlink_metadata(&config.store_file)
        .map_err(|_| "configured store does not exist".to_owned())?;
    if store_metadata.file_type().is_symlink() || !store_metadata.is_file() {
        return Err("configured store path is unsafe".to_owned());
    }
    let store = Store::open_with_options(
        &StorePath::File(config.store_file),
        StoreOpenOptions::plaintext(),
    )
    .map_err(|_| "cannot open configured store".to_owned())?;
    let sam_config = SamClientConfig {
        bridge: config.sam_bridge,
        ..SamClientConfig::production()
    };
    let provider = SamProvider::with_config(sam_config);
    let (mut controller, control) = RuntimeController::new(provider, store.handle_clone());
    let mut serve = tokio::spawn(async move { controller.serve().await });
    tokio::select! {
        signal = tokio::signal::ctrl_c() => { signal.map_err(|_| "cannot receive shutdown signal".to_owned())?; }
        result = wait_terminate() => { result?; }
        result = &mut serve => {
            return match result {
                Ok(Ok(())) => Ok(()),
                _ => Err("runtime stopped unexpectedly".to_owned()),
            };
        }
    }
    control.request_stop();
    match tokio::time::timeout(SHUTDOWN_BOUND, &mut serve).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(_))) => return Err("runtime shutdown failed".to_owned()),
        _ => {
            return Err(
                "runtime shutdown exceeded 30 seconds; process lease retained until exit"
                    .to_owned(),
            );
        }
    }
    store
        .shutdown()
        .map_err(|_| "store shutdown failed".to_owned())?;
    Ok(())
}

#[cfg(unix)]
async fn wait_terminate() -> Result<(), String> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())
        .map_err(|_| "cannot install termination handler".to_owned())?;
    term.recv().await;
    Ok(())
}

#[cfg(not(unix))]
async fn wait_terminate() -> Result<(), String> {
    std::future::pending().await
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.as_slice() == ["--help"] || args.is_empty() {
        println!(
            "i2pr-irc --config <path> run\n       i2pr-irc --version\n\nThis bootstrap has no active listener; local access is added by a later milestone."
        );
        return ExitCode::SUCCESS;
    }
    if args.as_slice() == ["--version"] {
        println!("i2pr-irc {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let path = match args.as_slice() {
        [flag, path, command] if flag == "--config" && command == "run" => PathBuf::from(path),
        [command] if command == "init" || command == "status" => {
            eprintln!("{command} is not implemented in this milestone");
            return ExitCode::from(2);
        }
        _ => {
            eprintln!("invalid command; use --help");
            return ExitCode::from(2);
        }
    };
    match run(&path).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("i2pr-irc: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = env::temp_dir().join(format!(
            "i2pr-irc-daemon-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn config_is_bounded_numeric_and_resolves_default_store_inside_state() {
        let dir = scratch("config");
        let file = dir.join("daemon.conf");
        fs::write(
            &file,
            "version=1\nstate_dir=private\nsam_bridge=127.0.0.1:7656\n",
        )
        .unwrap();
        let config = Config::parse(&file).unwrap();
        assert_eq!(config.state_dir, dir.join("private"));
        assert_eq!(config.store_file, dir.join("private/bouncer.sqlite3"));
        fs::write(&file, "version=1\nsam_bridge=router.i2p:7656\n").unwrap();
        assert!(Config::parse(&file).is_err());
        fs::write(&file, "version=1\nlistener=0.0.0.0:6667\n").unwrap();
        assert!(Config::parse(&file).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn lease_is_exclusive_and_released_on_drop() {
        let dir = scratch("lease");
        let lease = StateLease::acquire(&dir).unwrap();
        assert!(StateLease::acquire(&dir).is_err());
        drop(lease);
        assert!(StateLease::acquire(&dir).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn lease_rejects_symlink_and_world_writable_state_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let parent = scratch("unsafe");
        let target = parent.join("target");
        fs::create_dir(&target).unwrap();
        let link = parent.join("link");
        symlink(&target, &link).unwrap();
        assert!(StateLease::acquire(&link).is_err());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(StateLease::acquire(&target).is_err());
        let _ = fs::remove_dir_all(parent);
    }
}
