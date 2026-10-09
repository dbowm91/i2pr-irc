//! One-time secure local state provisioning. Secrets are never accepted on argv.
use i2pr_irc_store::{Store, StoreKey, StoreOpenOptions, StorePath};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

pub struct LocalVerifier {
    digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitStage {
    IncompleteMarker,
    OperatorVerifier,
    StoreKey,
    Database,
    Config,
    Finalize,
}

impl LocalVerifier {
    pub fn load(path: &Path) -> Result<Self, String> {
        check_private_file(path)?;
        let bytes = fs::read(path).map_err(|_| "cannot read operator verifier".to_owned())?;
        if bytes.len() > 256 {
            return Err("operator verifier is invalid".to_owned());
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| "operator verifier is invalid".to_owned())?;
        let mut digest = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("sha256=") {
                if digest.replace(decode_hex_digest(value)?).is_some() {
                    return Err("operator verifier is invalid".to_owned());
                }
            } else {
                return Err("operator verifier is invalid".to_owned());
            }
        }
        Ok(Self {
            digest: digest.ok_or_else(|| "operator verifier is invalid".to_owned())?,
        })
    }

    pub fn verify(&self, profile: &str, token: &[u8]) -> bool {
        let candidate = Sha256::digest(token);
        valid_profile(profile) && bool::from(self.digest.ct_eq(candidate.as_slice()))
    }
}

impl crate::listener::CredentialVerifier for LocalVerifier {
    fn verify(&self, profile: &str, token: &[u8]) -> bool {
        LocalVerifier::verify(self, profile, token)
    }
}

/// Initialize encrypted state. The config parent must already exist and be a private,
/// user-owned directory. The state directory itself is created with mode 0700.
pub fn initialize(config_path: &Path, plaintext: bool) -> Result<Zeroizing<String>, String> {
    initialize_with_hook(config_path, plaintext, |_| Ok(()))
}

fn initialize_with_hook(
    config_path: &Path,
    plaintext: bool,
    mut after_stage: impl FnMut(InitStage) -> Result<(), String>,
) -> Result<Zeroizing<String>, String> {
    ensure_supported_platform()?;
    let config_path = absolute_from_cwd(config_path)?;
    if fs::symlink_metadata(&config_path).is_ok() {
        return Err("configuration already exists; init never replaces files".to_owned());
    }
    let config_parent = config_path
        .parent()
        .ok_or_else(|| "configuration path has no parent".to_owned())?;
    validate_private_directory(config_parent)?;
    let state_dir = config_parent.join("state");
    fs::create_dir(&state_dir).map_err(|_| "cannot create private state directory".to_owned())?;
    set_private_dir(&state_dir)?;
    validate_private_directory(&state_dir)?;

    let marker = state_dir.join(".init-incomplete");
    atomic_create(&marker, b"initialization incomplete\n")?;
    after_stage(InitStage::IncompleteMarker)?;
    let key_path = state_dir.join("store.key");
    let verifier_path = state_dir.join("operator.verifier");
    let store_path = state_dir.join("bouncer.sqlite3");

    let mut token_bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *token_bytes)
        .map_err(|_| "secure random source unavailable".to_owned())?;
    let mut token = Zeroizing::new(hex_encode(&token_bytes[..]));
    let token_digest = Sha256::digest(token.as_bytes());
    let verifier_text = format!("sha256={}\n", hex_encode(&token_digest));
    atomic_create(&verifier_path, verifier_text.as_bytes())?;
    after_stage(InitStage::OperatorVerifier)?;

    if !plaintext {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *key).map_err(|_| "secure random source unavailable".to_owned())?;
        atomic_create(&key_path, &*key)?;
        after_stage(InitStage::StoreKey)?;
    }
    let file = create_private_file(&store_path)?;
    drop(file);
    let options = if plaintext {
        StoreOpenOptions::plaintext()
    } else {
        let bytes = read_private_key(&key_path)?;
        StoreOpenOptions::encrypted(StoreKey::from_bytes(bytes))
    };
    let store = Store::open_with_options(&StorePath::File(store_path), options)
        .map_err(|_| "cannot initialize durable store".to_owned())?;
    store
        .shutdown()
        .map_err(|_| "cannot finish durable store initialization".to_owned())?;
    after_stage(InitStage::Database)?;

    let encryption = if plaintext { "plaintext" } else { "sqlcipher" };
    let config = format!(
        "version=1\nstate_dir=state\nstore_file=state/bouncer.sqlite3\nsam_bridge=127.0.0.1:7656\nlistener=127.0.0.1:6667\nencryption={encryption}\ncredential_file=state/operator.verifier\n{}",
        if plaintext {
            String::new()
        } else {
            "key_file=state/store.key\n".to_owned()
        }
    );
    atomic_create(&config_path, config.as_bytes())?;
    after_stage(InitStage::Config)?;
    sync_directory(&state_dir)?;
    sync_directory(config_parent)?;
    after_stage(InitStage::Finalize)?;
    fs::remove_file(&marker).map_err(|_| "initialization marker cleanup failed".to_owned())?;
    if sync_directory(&state_dir).is_err() {
        let _ = atomic_create(&marker, b"initialization incomplete\n");
        return Err("cannot commit secure initialization state".to_owned());
    }
    token_bytes.zeroize();
    Ok(std::mem::take(&mut token))
}

pub fn load_store_key(path: &Path) -> Result<StoreKey, String> {
    let bytes = read_private_key(path)?;
    Ok(StoreKey::from_bytes(bytes))
}

pub fn state_is_incomplete(state_dir: &Path) -> bool {
    fs::symlink_metadata(state_dir.join(".init-incomplete")).is_ok()
}

fn read_private_key(path: &Path) -> Result<[u8; 32], String> {
    check_private_file(path)?;
    let mut bytes = Zeroizing::new(fs::read(path).map_err(|_| "cannot read store key".to_owned())?);
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "store key is invalid".to_owned())?;
    bytes.zeroize();
    Ok(key)
}

pub fn ensure_supported_platform() -> Result<(), String> {
    #[cfg(unix)]
    {
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("secure initialization is unsupported on this platform".to_owned())
    }
}

fn absolute_from_cwd(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|_| "cannot resolve configuration path".to_owned())
    }
}

pub fn validate_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "unsafe private directory".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("unsafe private directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe_uid();
        if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err("directory ownership or permissions are unsafe".to_owned());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unsafe_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

fn set_private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot secure state directory".to_owned())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("secure initialization is unsupported on this platform".to_owned())
    }
}

fn create_private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "cannot create private file".to_owned())
}

fn atomic_create(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "invalid private path".to_owned())?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| "secure random source unavailable".to_owned())?;
    let temp = parent.join(format!(".init-{}.tmp", hex_encode(&random)));
    random.zeroize();
    let mut file = create_private_file(&temp)?;
    let write_result = file.write_all(content).and_then(|()| file.sync_all());
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err("cannot write private initialization file".to_owned());
    }
    drop(file);
    let result = fs::hard_link(&temp, path);
    let _ = fs::remove_file(&temp);
    result
        .map_err(|_| "initialization destination already exists or cannot be secured".to_owned())?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync private directory".to_owned())
}

fn valid_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 32
        && profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex_digest(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("operator verifier is invalid".to_owned());
    }
    let mut output = [0u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        let pair = &value[index * 2..index * 2 + 2];
        *byte =
            u8::from_str_radix(pair, 16).map_err(|_| "operator verifier is invalid".to_owned())?;
    }
    Ok(output)
}

pub fn check_private_file(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "private file is unavailable".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("private file path is unsafe".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe_uid() || metadata.mode() & 0o077 != 0 {
            return Err("private file ownership or permissions are unsafe".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listener::authenticate;
    use std::sync::Arc;
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
    };

    #[test]
    fn verifier_is_profile_scoped_and_rejects_bad_secret() {
        let dir = std::env::temp_dir().join(format!("i2pr-init-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        set_private_dir(&dir).unwrap();
        let config = dir.join("daemon.conf");
        let token = initialize(&config, false).unwrap();
        let parsed = crate::Config::parse(&config).unwrap();
        assert_eq!(parsed.encryption, "sqlcipher");
        assert!(
            parsed
                .key_file
                .as_ref()
                .unwrap()
                .ends_with("state/store.key")
        );
        assert_eq!(
            parsed.listener.unwrap().ip(),
            "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
        );
        let verifier = LocalVerifier::load(&dir.join("state/operator.verifier")).unwrap();
        assert!(verifier.verify("default", token.as_bytes()));
        assert!(verifier.verify("workstation-2", token.as_bytes()));
        assert!(!verifier.verify("invalid profile", token.as_bytes()));
        assert!(!verifier.verify("default", b"wrong"));
        let config_bytes = fs::read(&config).unwrap();
        assert!(
            !config_bytes
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        let verifier_bytes = fs::read(dir.join("state/operator.verifier")).unwrap();
        assert!(
            !verifier_bytes
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(dir.join("state")).unwrap().mode() & 0o777,
                0o700
            );
            for path in [
                &config,
                &dir.join("state/store.key"),
                &dir.join("state/operator.verifier"),
            ] {
                assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
            }
        }
        let store = Store::open_with_options(
            &StorePath::File(dir.join("state/bouncer.sqlite3")),
            StoreOpenOptions::encrypted(load_store_key(&dir.join("state/store.key")).unwrap()),
        )
        .unwrap();
        store.shutdown().unwrap();
        assert!(initialize(&config, false).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn plaintext_requires_explicit_selection_and_has_no_key_file() {
        let dir = std::env::temp_dir().join(format!("i2pr-init-plain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        set_private_dir(&dir).unwrap();
        let config = dir.join("daemon.conf");
        initialize(&config, true).unwrap();
        let contents = fs::read_to_string(&config).unwrap();
        assert!(contents.contains("encryption=plaintext"));
        assert!(!dir.join("state/store.key").exists());
        assert!(!state_is_incomplete(&dir.join("state")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn interrupted_atomic_stages_always_leave_a_recovery_marker() {
        for stage in [
            InitStage::IncompleteMarker,
            InitStage::OperatorVerifier,
            InitStage::StoreKey,
            InitStage::Database,
            InitStage::Config,
            InitStage::Finalize,
        ] {
            let dir = std::env::temp_dir()
                .join(format!("i2pr-init-crash-{}-{stage:?}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir(&dir).unwrap();
            set_private_dir(&dir).unwrap();
            let config = dir.join("daemon.conf");
            let result = initialize_with_hook(&config, false, |current| {
                if current == stage {
                    Err("simulated interruption".to_owned())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert!(state_is_incomplete(&dir.join("state")), "stage: {stage:?}");
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn initialized_token_authenticates_a_named_profile_over_real_loopback() {
        let dir = std::env::temp_dir().join(format!("i2pr-init-wire-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        set_private_dir(&dir).unwrap();
        let token = initialize(&dir.join("daemon.conf"), false).unwrap();
        let verifier = Arc::new(LocalVerifier::load(&dir.join("state/operator.verifier")).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            assert!(peer.ip().is_loopback());
            authenticate(stream, verifier).await.unwrap()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        let transcript = format!(
            "PASS laptop:{}\r\nNICK test\r\nUSER test 0 * :fixture\r\nCAP END\r\n",
            token.as_str()
        );
        client.write_all(transcript.as_bytes()).await.unwrap();
        let checkpoint = service.await.unwrap();
        let (_, state) = checkpoint.into_parts();
        assert_eq!(state.profile, "laptop");
        assert!(String::from_utf8_lossy(&state.unread).contains("NICK test\r\n"));
        assert!(!String::from_utf8_lossy(&state.unread).contains(token.as_str()));
        drop(client);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_file_permissions_and_symlinks_fail_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!("i2pr-init-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        set_private_dir(&dir).unwrap();
        let target = dir.join("target");
        fs::write(&target, [0u8; 32]).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        symlink(&target, &link).unwrap();
        assert!(read_private_key(&link).is_err());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_key(&target).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
