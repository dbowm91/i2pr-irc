use fs2::FileExt;
use i2pr_irc_runtime::RuntimeController;
use i2pr_irc_sam::{SamBridgeEndpoint, SamClientConfig, SamProvider};
use i2pr_irc_store::{NetworkId, Store, StoreOpenOptions, StorePath};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

pub mod app;
pub mod bootstrap;
pub mod listener;

const MAX_CONFIG_BYTES: usize = 16 * 1024;
const SHUTDOWN_BOUND: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Config {
    state_dir: PathBuf,
    store_file: PathBuf,
    sam_bridge: SamBridgeEndpoint,
    listener: Option<std::net::SocketAddr>,
    default_network: Option<NetworkId>,
    encryption: String,
    key_file: Option<PathBuf>,
    credential_file: PathBuf,
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
                    | "default_network"
                    | "encryption"
                    | "key_file"
                    | "credential_file"
            ) {
                return Err("unknown configuration key".to_owned());
            }
        }
        if values.get("version").map(String::as_str) != Some("1") {
            return Err("unsupported configuration version".to_owned());
        }
        let config_parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let config_dir = fs::canonicalize(config_parent)
            .map_err(|_| "cannot resolve configuration directory".to_owned())?;
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
            .ok_or_else(|| "encryption mode must be explicit".to_owned())?;
        if encryption != "plaintext" && encryption != "sqlcipher" {
            return Err("unsupported encryption mode".to_owned());
        }
        if encryption == "sqlcipher" && !values.contains_key("key_file") {
            return Err("encrypted mode requires a key file reference".to_owned());
        }
        if !values.contains_key("credential_file") {
            return Err("operator credential verifier is required".to_owned());
        }
        let listener = values
            .get("listener")
            .map(|value| value.parse::<std::net::SocketAddr>())
            .transpose()
            .map_err(|_| "listener must be a numeric loopback address".to_owned())?;
        if listener.is_some_and(|address| listener::validate_loopback(address).is_err()) {
            return Err("listener must be a numeric loopback address".to_owned());
        }
        let default_network = values
            .get("default_network")
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|_| "default network id is invalid".to_owned())?
            .map(NetworkId);
        if default_network.is_some_and(|network| network.0 == 0) {
            return Err("default network id is invalid".to_owned());
        }
        let bridge = values
            .get("sam_bridge")
            .map(String::as_str)
            .unwrap_or("127.0.0.1:7656");
        let sam_bridge = SamBridgeEndpoint::parse(bridge)
            .map_err(|_| "SAM bridge must be a numeric loopback address".to_owned())?;
        let state_dir = resolve_config_path("state_dir", "state");
        if !state_dir.starts_with(&config_dir) {
            return Err("state directory must be inside the configuration directory".to_owned());
        }
        let store_file = values
            .get("store_file")
            .map(|_| resolve_config_path("store_file", "bouncer.sqlite3"))
            .unwrap_or_else(|| state_dir.join("bouncer.sqlite3"));
        let key_file = values
            .get("key_file")
            .map(|_| resolve_config_path("key_file", "store.key"));
        let credential_file = resolve_config_path("credential_file", "operator.verifier");
        if !store_file.starts_with(&state_dir) {
            return Err("store file must be inside the state directory".to_owned());
        }
        if !credential_file.starts_with(&state_dir)
            || key_file
                .as_ref()
                .is_some_and(|key| !key.starts_with(&state_dir))
        {
            return Err("credential and key files must be inside the state directory".to_owned());
        }
        Ok(Self {
            state_dir,
            store_file,
            sam_bridge,
            listener,
            default_network,
            encryption: encryption.to_owned(),
            key_file,
            credential_file,
        })
    }
}

struct StateLease(File);

impl StateLease {
    fn acquire(state_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(state_dir)?;
        bootstrap::validate_private_directory(state_dir).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe state directory permissions or ownership",
            )
        })?;
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
    bootstrap::ensure_supported_platform()?;
    bootstrap::check_private_file(config_path)?;
    bootstrap::validate_private_directory(
        config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let config = Config::parse(config_path)?;
    let _lease = StateLease::acquire(&config.state_dir)
        .map_err(|_| "cannot obtain exclusive state ownership".to_owned())?;
    bootstrap::validate_private_directory(&config.state_dir)?;
    if bootstrap::state_is_incomplete(&config.state_dir) {
        return Err("state initialization is incomplete; preserve files for recovery".to_owned());
    }
    if config.default_network.is_some() && config.listener.is_none() {
        return Err("default network requires an activated listener".to_owned());
    }
    let state_real = fs::canonicalize(&config.state_dir)
        .map_err(|_| "cannot resolve state directory".to_owned())?;
    let store_parent = config
        .store_file
        .parent()
        .ok_or_else(|| "store path has no parent".to_owned())?;
    let store_parent_real = fs::canonicalize(store_parent)
        .map_err(|_| "store parent directory does not exist".to_owned())?;
    if !store_parent_real.starts_with(&state_real) {
        return Err("store file must be inside the leased state directory".to_owned());
    }
    if !config.store_file.is_absolute() {
        return Err("store path must resolve to an absolute path".to_owned());
    }
    let store_metadata = fs::symlink_metadata(&config.store_file)
        .map_err(|_| "configured store does not exist".to_owned())?;
    if store_metadata.file_type().is_symlink() || !store_metadata.is_file() {
        return Err("configured store path is unsafe".to_owned());
    }
    for private_file in config
        .key_file
        .iter()
        .chain(std::iter::once(&config.credential_file))
    {
        let parent = private_file
            .parent()
            .ok_or_else(|| "private file path has no parent".to_owned())?;
        let parent = fs::canonicalize(parent)
            .map_err(|_| "private file parent is unavailable".to_owned())?;
        if !parent.starts_with(&state_real) {
            return Err("private file is outside the leased state directory".to_owned());
        }
    }
    let options = match config.encryption.as_str() {
        "plaintext" => StoreOpenOptions::plaintext(),
        "sqlcipher" => StoreOpenOptions::encrypted(bootstrap::load_store_key(
            config
                .key_file
                .as_deref()
                .ok_or_else(|| "encrypted mode requires a key file".to_owned())?,
        )?),
        _ => return Err("unsupported encryption mode".to_owned()),
    };
    let verifier = std::sync::Arc::new(bootstrap::LocalVerifier::load(&config.credential_file)?);
    let store = Store::open_with_options(&StorePath::File(config.store_file), options)
        .map_err(|_| "cannot open configured store".to_owned())?;
    let sam_config = SamClientConfig {
        bridge: config.sam_bridge,
        ..SamClientConfig::production()
    };
    let provider = SamProvider::with_config(sam_config);
    let (mut controller, control) = RuntimeController::new(provider, store.handle_clone());
    let mut serve = tokio::spawn(async move { controller.serve().await });
    let mut local_access = if let Some(address) = config.listener {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        Some((
            stop_tx,
            tokio::spawn(app::serve_local_access(
                address,
                verifier,
                store.handle_clone(),
                control.clone(),
                config.default_network,
                stop_rx,
            )),
        ))
    } else {
        None
    };
    let (outcome, listener_ended) = tokio::select! {
        signal = tokio::signal::ctrl_c() => (signal.map(|_| ()).map_err(|_| "cannot receive shutdown signal".to_owned()), false),
        result = wait_terminate() => (result, false),
        _result = &mut serve => (Err("runtime stopped unexpectedly".to_owned()), false),
        result = async { match local_access.as_mut() { Some((_, task)) => Some(task.await), None => std::future::pending().await } } => {
            (match result {
                Some(Ok(Ok(()))) => Err("local listener stopped unexpectedly".to_owned()),
                Some(Ok(Err(_))) => Err("local listener failed".to_owned()),
                _ => Err("local listener task failed".to_owned()),
            }, true)
        }
    };
    if let Some((stop_tx, _)) = &local_access {
        let _ = stop_tx.send(true);
    }
    let mut listener_shutdown_failed = false;
    if let Some((_, mut task)) = local_access.take()
        && !listener_ended
    {
        match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
            Ok(Ok(Ok(()))) => {}
            _ => {
                listener_shutdown_failed = true;
                task.abort();
                let _ = task.await;
            }
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
    if listener_shutdown_failed {
        Err("local listener shutdown failed".to_owned())
    } else {
        outcome
    }
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
            "i2pr-irc init --config <path> [--plaintext]\n       i2pr-irc --config <path> run\n       i2pr-irc status --config <path>\n       i2pr-irc --version\n\nInit creates encrypted state by default and displays the local Operator token once."
        );
        return ExitCode::SUCCESS;
    }
    if args.as_slice() == ["--version"] {
        println!("i2pr-irc {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if let [command, flag, path] = args.as_slice()
        && command == "status"
        && flag == "--config"
    {
        match show_status(Path::new(path)) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("i2pr-irc: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    if let [command, flag, path] = args.as_slice()
        && command == "init"
        && flag == "--config"
    {
        return init_command(Path::new(path), false);
    }
    if let [command, flag, path, mode] = args.as_slice()
        && command == "init"
        && flag == "--config"
        && mode == "--plaintext"
    {
        return init_command(Path::new(path), true);
    }
    let path = match args.as_slice() {
        [flag, path, command] if flag == "--config" && command == "run" => PathBuf::from(path),
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

fn init_command(config: &Path, plaintext: bool) -> ExitCode {
    match bootstrap::initialize(config, plaintext) {
        Ok(token) => {
            println!(
                "Initialization complete. Save this Operator token securely; it is shown once:\n{}",
                token.as_str()
            );
            println!("Connect to 127.0.0.1:6667 using PASS default:<token> (or SASL PLAIN).");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("i2pr-irc: {error}");
            ExitCode::FAILURE
        }
    }
}

fn show_status(path: &Path) -> Result<(), String> {
    bootstrap::ensure_supported_platform()?;
    bootstrap::check_private_file(path)?;
    bootstrap::validate_private_directory(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let config = Config::parse(path)?;
    bootstrap::validate_private_directory(&config.state_dir)?;
    if bootstrap::state_is_incomplete(&config.state_dir) {
        return Err("state initialization is incomplete".to_owned());
    }
    bootstrap::LocalVerifier::load(&config.credential_file)?;
    if let Some(key_file) = &config.key_file {
        let _key = bootstrap::load_store_key(key_file)?;
    }
    let store_metadata = fs::symlink_metadata(&config.store_file)
        .map_err(|_| "configured store does not exist".to_owned())?;
    if store_metadata.file_type().is_symlink() || !store_metadata.is_file() {
        return Err("configured store path is unsafe".to_owned());
    }
    println!("configuration: {}", path.display());
    println!("state directory: {}", config.state_dir.display());
    println!("store: {}", config.store_file.display());
    println!("encryption: {}", config.encryption);
    println!(
        "listener: {}",
        config
            .listener
            .map_or_else(|| "disabled".to_owned(), |address| address.to_string())
    );
    println!("operator credential: configured (redacted)");
    Ok(())
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[test]
    fn config_is_bounded_numeric_and_resolves_default_store_inside_state() {
        let dir = scratch("config");
        let file = dir.join("daemon.conf");
        fs::write(
            &file,
            "version=1\nstate_dir=private\nsam_bridge=127.0.0.1:7656\ncredential_file=private/operator.verifier\nencryption=plaintext\n",
        )
        .unwrap();
        let config = Config::parse(&file).unwrap();
        assert_eq!(
            config.state_dir,
            fs::canonicalize(dir.join("private").parent().unwrap())
                .unwrap()
                .join("private")
        );
        assert_eq!(config.store_file, config.state_dir.join("bouncer.sqlite3"));
        fs::write(
            &file,
            "version=1\nstate_dir=private\nstore_file=other.sqlite3\n",
        )
        .unwrap();
        assert!(Config::parse(&file).is_err());
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
