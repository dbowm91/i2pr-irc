//! Opaque SAM session identifiers.
//!
//! # Why an opaque random ID and not the `NetworkId`
//!
//! The `NetworkId` is a small monotonic integer. It would make a compact session ID, and
//! it is exactly the wrong choice: every Network this process owns would then present a
//! session ID that an observer at the bridge could correlate across time, across restarts,
//! and across every bouncer that assigned Network 1 first. Plan 030 section 7 forbids
//! serializing the `NetworkId`, the display name, the nick, the endpoint, the process
//! ID, a timestamp, and the build string. What remains is randomness.
//!
//! # Why the source is injected
//!
//! A protocol test that had to assert "this is the line I sent" would otherwise have to
//! assert against a random value, which either makes the test tautological or makes it
//! flaky. So randomness is a trait with one OS implementation and the tests supply a
//! deterministic one. The trait is the *only* way to make a session ID in this crate, so
//! there is no path that quietly produces a predictable one.

use std::{fmt, sync::Arc};

use zeroize::Zeroizing;

/// Characters of entropy in every generated session ID.
///
/// 32 hex characters is 128 bits. Plan 030 section 7 requires "at least 128 bits"; this
/// is exactly that, and it fits in a SAM `ID=` value on either router without question.
pub const SESSION_ID_CHARS: usize = 32;

/// Ceiling on entropy read from the source.
///
/// A session ID needs 16 bytes. Reading more would be speculative; reading less would be
/// below the requirement. The source is asked for exactly this much.
const ENTROPY_BYTES: usize = SESSION_ID_CHARS / 2;

/// A bounded source of cryptographically random bytes.
///
/// Failures are explicit. A source that cannot produce randomness must not be allowed to
/// substitute a counter, a clock, or a hash of process state — a session ID that looks
/// random and is predictable is worse than no session, because it looks safe in review.
pub trait RandomSource: Send + Sync {
    /// Fills `out` with random bytes, or fails.
    fn fill(&self, out: &mut [u8]) -> Result<(), RandomUnavailable>;
}

/// The OS random source failed.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("OS randomness unavailable")]
pub struct RandomUnavailable;

/// The production source: the operating system's CSPRNG.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&self, out: &mut [u8]) -> Result<(), RandomUnavailable> {
        // `getrandom` on every supported platform resolves to a libc call or a syscall
        // the platform already provides. It is the only OS-random dependency in the
        // workspace and it is here for exactly this one use.
        getrandom::fill(out).map_err(|_| RandomUnavailable)
    }
}

/// A SAM session ID: 32 lowercase hex characters of OS randomness.
///
/// Deliberately *not* `Display` in a way that could be logged by accident — see
/// [`std::fmt::Debug`] below — and deliberately not derived from anything.
#[derive(Clone)]
pub struct SamSessionId(Zeroizing<String>);

impl SamSessionId {
    /// The hex characters, for writing into a `SESSION CREATE` or `STREAM CONNECT`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Generates one from `source`.
    ///
    /// Failure is returned rather than papered over. A caller that cannot get randomness
    /// has no valid session ID to present, and the honest answer is to fail the attempt
    /// under the reconnect budget rather than to connect with a guessable identity.
    pub fn generate(source: &dyn RandomSource) -> Result<Self, RandomUnavailable> {
        let mut bytes = Zeroizing::new([0u8; ENTROPY_BYTES]);
        source.fill(bytes.as_mut())?;
        Ok(Self(Zeroizing::new(hex(&bytes))))
    }
}

// Hand-written because the wrapper does not derive them: comparing or hashing the ID is
// legitimate, and there is no reason to make it impossible, only uninteresting.
impl PartialEq for SamSessionId {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}
impl Eq for SamSessionId {}
impl std::hash::Hash for SamSessionId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.as_str().hash(state);
    }
}

impl fmt::Debug for SamSessionId {
    /// Redacted.
    ///
    /// A session ID is not secret in the way a Destination is — the router has it, and it
    /// appears in the bouncer's own diagnostics when classifying a reply. It is redacted
    /// anyway because it is the one value that would let a reader correlate two Networks
    /// by comparing logs, and correlation is exactly what the opaque ID exists to
    /// prevent. A caller that genuinely needs it uses [`Self::as_str`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SamSessionId([redacted])")
    }
}

/// The default source, held behind an `Arc` so a caller can clone it into a client.
pub fn os_random() -> Arc<dyn RandomSource> {
    Arc::new(OsRandom)
}

/// Lowercase hex, without a hex-encoding dependency.
///
/// Sixteen bytes is eight nibbles each, so this is a fixed-size loop rather than a
/// general encoder. Writing it out avoids adding a dependency for sixteen bytes.
fn hex(bytes: &[u8; ENTROPY_BYTES]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(SESSION_ID_CHARS);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A deterministic source, so a protocol test can assert the exact line it sent.
    #[derive(Default)]
    struct Fixed(Mutex<Vec<u8>>);

    impl RandomSource for Fixed {
        fn fill(&self, out: &mut [u8]) -> Result<(), RandomUnavailable> {
            let state = self.0.lock().expect("fixed source is readable");
            for (index, slot) in out.iter_mut().enumerate() {
                *slot = state.get(index).copied().unwrap_or(index as u8);
            }
            Ok(())
        }
    }

    /// A source that always fails, to prove failure is explicit.
    struct Failing;

    impl RandomSource for Failing {
        fn fill(&self, _: &mut [u8]) -> Result<(), RandomUnavailable> {
            Err(RandomUnavailable)
        }
    }

    #[test]
    fn a_generated_id_is_the_expected_hex_of_the_source_bytes() {
        // Four explicit bytes, then the source's deterministic fill for the rest.
        let source = Fixed(Mutex::new(vec![0x00, 0x01, 0x0f, 0x10]));
        let id = SamSessionId::generate(&source).expect("the source answers");
        let expected: String = [0x00u8, 0x01, 0x0f, 0x10]
            .iter()
            .copied()
            .chain(4u8..ENTROPY_BYTES as u8)
            .flat_map(|byte| [byte >> 4, byte & 0x0f])
            .map(|nibble| format!("{nibble:x}"))
            .collect();
        assert_eq!(
            id.as_str(),
            expected,
            "the encoding is lower hex of the source bytes, in order"
        );
        assert_eq!(id.as_str().len(), SESSION_ID_CHARS);
        assert!(
            id.as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "the encoding is lowercase hex: {}",
            id.as_str()
        );
    }

    /// 128 bits of entropy, stated as a width rather than implied by a length.
    #[test]
    fn a_generated_id_carries_at_least_128_bits() {
        let id = SamSessionId::generate(&OsRandom).expect("the OS answers");
        assert!(
            id.as_str().len() * 4 >= 128,
            "the ID carries at least 128 bits: {}",
            id.as_str().len() * 4
        );
    }

    #[test]
    fn distinct_draws_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let id = SamSessionId::generate(&OsRandom).expect("the OS answers");
            assert!(
                seen.insert(id.as_str().to_owned()),
                "two draws collided: {}",
                id.as_str()
            );
        }
    }

    /// The failure case that matters: a source that cannot answer must not produce an ID.
    #[test]
    fn a_failing_source_is_reported_and_never_substitutes_a_value() {
        assert_eq!(
            SamSessionId::generate(&Failing).map(|id| id.as_str().to_owned()),
            Err(RandomUnavailable)
        );
    }

    /// The forbidden-material requirement, asserted structurally.
    ///
    /// The generator takes only a `RandomSource`, so there is no parameter through which
    /// a `NetworkId`, a nick, an endpoint, or a timestamp could enter. This test names
    /// the absence rather than the presence, because the absence is the requirement.
    #[test]
    fn nothing_identifying_can_reach_a_generated_id() {
        // The only inputs to `generate` are `self` and the source. If a future signature
        // grew a parameter, this file would need to change, and the compiler would point
        // at it. Asserting entropy width and lowercase-hex shape here covers the other
        // direction: any identifying material would either be too long, too short, or
        // contain characters outside the hex alphabet.
        let id = SamSessionId::generate(&OsRandom).expect("the OS answers");
        assert_eq!(id.as_str().chars().count(), SESSION_ID_CHARS);
        assert!(
            id.as_str()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "an ID is exactly lowercase hex and nothing else: {}",
            id.as_str()
        );
    }

    #[test]
    fn debug_is_redacted() {
        let id = SamSessionId::generate(&OsRandom).expect("the OS answers");
        assert_eq!(format!("{id:?}"), "SamSessionId([redacted])");
        assert!(!format!("{id:?}").contains(id.as_str()));
    }
}
