//! Offline source-preserving encrypted database copy and key rotation.
use crate::{
    Store, StoreEncryption, StoreError, StoreErrorKind, StoreKey, StoreOpenOptions, StorePath,
    encryption::key_hex_literal, schema, worker::apply_encryption_key,
};
use rusqlite::Connection;
use std::{fs, fs::OpenOptions, path::Path};

/// Exports a quiesced store into a new encrypted database and verifies it through
/// the normal worker open path. The source is never removed or overwritten.
///
/// Callers must stop and join every Store worker using `source` before invoking
/// this offline operation. `destination` must be a new sibling file; migration
/// never performs installation/replacement of the original path.
pub fn export_encrypted_copy(
    source: &Path,
    source_encryption: StoreEncryption,
    destination: &Path,
    destination_key: StoreKey,
) -> Result<(), StoreError> {
    export_and_verify(
        source,
        source_encryption,
        destination,
        destination_key,
        ExportFailpoint::None,
    )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ExportFailpoint {
    None,
    AfterExport,
    BeforeVerification,
}

fn export_and_verify(
    source: &Path,
    source_encryption: StoreEncryption,
    destination: &Path,
    destination_key: StoreKey,
    failpoint: ExportFailpoint,
) -> Result<(), StoreError> {
    if same_file_or_path(source, destination) || destination.exists() {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "migration destination must be a new distinct file",
        )));
    }
    let parent = usable_parent(destination);
    if !parent.is_dir() {
        return Err(StoreError::new(StoreErrorKind::Open));
    }
    let source_parent = usable_parent(source);
    if fs::canonicalize(source_parent).ok() != fs::canonicalize(parent).ok() {
        return Err(StoreError::new(StoreErrorKind::InvalidRequest(
            "migration destination must be a sibling of the source",
        )));
    }

    let verification_key = destination_key.duplicate_for_verification();
    let source_connection =
        Connection::open(source).map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    match source_encryption {
        StoreEncryption::Plaintext => {}
        StoreEncryption::Encrypted(key) => apply_encryption_key(&source_connection, key)?,
    }
    // Upgrade any supported predecessor using precisely the ordinary startup path.
    schema::open_and_migrate(&source_connection, crate::STORE_BUSY_TIMEOUT_MS)?;

    // Authenticate and validate the source before touching the destination.
    let reserved = private_create_new(destination)?;
    drop(reserved);

    let result = (|| {
        let destination_key = key_hex_literal(destination_key);
        source_connection
            .execute(
                "ATTACH DATABASE ?1 AS encrypted KEY ?2",
                rusqlite::params![
                    destination.to_string_lossy().as_ref(),
                    destination_key.as_str()
                ],
            )
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        source_connection
            .query_row("SELECT sqlcipher_export('encrypted')", [], |row| {
                row.get::<_, Option<i64>>(0).map(|_| ())
            })
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        if failpoint == ExportFailpoint::AfterExport {
            return Err(StoreError::new(StoreErrorKind::Open));
        }
        source_connection
            .pragma_update(Some("encrypted"), "application_id", schema::APPLICATION_ID)
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        source_connection
            .pragma_update(Some("encrypted"), "user_version", schema::SCHEMA_VERSION)
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        source_connection
            .execute("DETACH DATABASE encrypted", [])
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        drop(destination_key);

        sync_file_and_parent(destination)?;
        if failpoint == ExportFailpoint::BeforeVerification {
            return Err(StoreError::new(StoreErrorKind::Open));
        }

        // The ordinary keyed open validates application identity, schema, required
        // indexes, FTS row counts, and encrypted page readability before success.
        let verified = Store::open_with_options(
            &StorePath::File(destination.to_path_buf()),
            StoreOpenOptions::encrypted(verification_key),
        )?;
        verified.shutdown()?;
        Ok(())
    })();
    if result.is_err() {
        drop(source_connection);
        remove_database_files(destination);
    }
    result
}

fn private_create_new(path: &Path) -> Result<fs::File, StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))
}

fn same_file_or_path(source: &Path, destination: &Path) -> bool {
    if source == destination {
        return true;
    }
    match (fs::canonicalize(source), canonicalize_new_path(destination)) {
        (Ok(source), Some(destination)) => source == destination,
        _ => false,
    }
}

fn canonicalize_new_path(path: &Path) -> Option<std::path::PathBuf> {
    let parent = usable_parent(path);
    Some(fs::canonicalize(parent).ok()?.join(path.file_name()?))
}

fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_file_and_parent(path: &Path) -> Result<(), StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    file.sync_all()
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    if let Some(parent) = path.parent()
        && let Ok(directory) = OpenOptions::new().read(true).open(parent)
    {
        directory
            .sync_all()
            .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
    }
    Ok(())
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(std::path::PathBuf::from(sidecar));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> StoreKey {
        StoreKey::from_bytes([byte; 32])
    }

    #[test]
    fn injected_export_and_verification_failures_keep_source_reopenable() {
        for (label, failpoint) in [
            ("during-export", ExportFailpoint::AfterExport),
            ("before-verification", ExportFailpoint::BeforeVerification),
        ] {
            let dir = crate::testing::temp_dir(label);
            let source = dir.db("source.sqlite3");
            let destination = dir.db("destination.sqlite3");
            Store::open(&StorePath::File(source.clone()))
                .expect("source opens")
                .shutdown()
                .expect("source quiesces");

            assert!(
                export_and_verify(
                    &source,
                    StoreEncryption::Plaintext,
                    &destination,
                    key(0x61),
                    failpoint,
                )
                .is_err()
            );
            assert!(!destination.exists(), "failed destination is removed");
            Store::open(&StorePath::File(source))
                .expect("source remains readable after injected failure")
                .shutdown()
                .expect("source shuts down");
        }
    }
}
