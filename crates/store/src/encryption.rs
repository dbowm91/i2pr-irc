//! Explicit whole-database encryption policy and injected SQLCipher key.
use zeroize::Zeroizing;

/// Raw high-entropy SQLCipher key material. This type is consumed at store open,
/// never formatted, cloned, serialized, or stored in SQLite.
pub struct StoreKey(Zeroizing<[u8; 32]>);

impl StoreKey {
    /// Accepts exactly 256 bits of caller-provided random key material.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub(crate) fn into_bytes(self) -> Zeroizing<[u8; 32]> {
        self.0
    }

    pub(crate) fn duplicate_for_verification(&self) -> Self {
        Self::from_bytes(*self.0)
    }
}

pub(crate) fn key_hex_literal(key: StoreKey) -> Zeroizing<String> {
    let bytes = key.into_bytes();
    let mut encoded = Zeroizing::new(String::with_capacity(67));
    encoded.push_str("x'");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes.iter() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded.push('\'');
    drop(bytes);
    encoded
}

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey([redacted])")
    }
}

/// Whether a store is explicitly opened as ordinary plaintext SQLite or encrypted
/// SQLCipher. The policy is never inferred from the file contents.
pub enum StoreEncryption {
    Plaintext,
    Encrypted(StoreKey),
}

impl std::fmt::Debug for StoreEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plaintext => f.write_str("Plaintext"),
            Self::Encrypted(_) => f.write_str("Encrypted([redacted])"),
        }
    }
}

/// Store startup options. Encryption is explicit and the key is consumed by open.
pub struct StoreOpenOptions {
    pub encryption: StoreEncryption,
}

impl StoreOpenOptions {
    pub const fn plaintext() -> Self {
        Self {
            encryption: StoreEncryption::Plaintext,
        }
    }

    pub fn encrypted(key: StoreKey) -> Self {
        Self {
            encryption: StoreEncryption::Encrypted(key),
        }
    }
}

impl std::fmt::Debug for StoreOpenOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreOpenOptions")
            .field("encryption", &self.encryption)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_options_debug_are_redacted() {
        let key = StoreKey::from_bytes([0xA5; 32]);
        assert_eq!(format!("{key:?}"), "StoreKey([redacted])");
        let options = StoreOpenOptions::encrypted(key);
        let rendered = format!("{options:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("A5"));
    }
}
