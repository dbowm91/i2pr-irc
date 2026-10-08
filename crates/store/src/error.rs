//! Typed store failures with an explicit durable commit disposition.
use thiserror::Error;

/// Whether a mutating operation's transaction is known to have committed.
///
/// A caller that loses its response path cannot infer rollback from task
/// cancellation: SQLite may have completed the transaction before the reply was
/// delivered. Every mutation therefore reports one of these three states instead of
/// leaving the caller to guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitState {
    /// SQLite committed the transaction.
    Committed,
    /// SQLite rolled the transaction back; no durable change happened.
    RolledBack,
    /// The caller abandoned the request before a result was known, so durable state
    /// must be re-read before the mutation is retried or treated as applied.
    Unknown,
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("store failure: {kind}")]
pub struct StoreError {
    kind: StoreErrorKind,
    commit: CommitState,
}

impl StoreError {
    /// Builds a non-mutating failure, which by definition committed nothing.
    pub(crate) fn new(kind: StoreErrorKind) -> Self {
        Self {
            kind,
            commit: CommitState::RolledBack,
        }
    }
    pub(crate) fn mutating(kind: StoreErrorKind, commit: CommitState) -> Self {
        Self { kind, commit }
    }
    pub fn kind(&self) -> &StoreErrorKind {
        &self.kind
    }
    pub fn commit_state(&self) -> CommitState {
        self.commit
    }
    /// True when durable state may already reflect this mutation.
    pub fn may_have_committed(&self) -> bool {
        !matches!(self.commit, CommitState::RolledBack)
    }
}

impl CommitState {
    /// Stable, non-secret classification for a local error reply.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::RolledBack => "rolled-back",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum StoreErrorKind {
    /// The bounded ingress queue is full. Callers degrade explicitly; they never
    /// spin, allocate a secondary unbounded queue, or block network control work.
    #[error("store queue overloaded")]
    QueueOverloaded,
    /// The worker thread is gone or was never started.
    #[error("store worker stopped")]
    Stopped,
    /// SQLite itself failed.
    #[error("sqlite failure")]
    Sqlite,
    /// The database was written by a newer, incompatible schema.
    #[error("database schema is newer than this build supports")]
    SchemaTooNew,
    /// The database does not carry this application's identity.
    #[error("database is not an i2pr-irc store")]
    ForeignDatabase,
    /// The bundled SQLite is too old for the STRICT schema this build requires.
    #[error("sqlite runtime lacks required STRICT table support")]
    SqliteTooOld,
    /// A durable row failed the same domain validation applied to fresh input, or
    /// is otherwise corrupt. The store never silently synthesizes a default that
    /// could silently alter identity.
    #[error("durable state is invalid: {0}")]
    Corrupt(&'static str),
    /// A caller-supplied value failed validation before any SQL ran.
    #[error("invalid store request: {0}")]
    InvalidRequest(&'static str),
    /// A request or response exceeded its explicit ceiling.
    #[error("bounded store limit exceeded: {0}")]
    LimitExceeded(&'static str),
    /// Startup failed, so the process must not begin normal operation.
    #[error("store open failed")]
    Open,
    /// The configured encrypted-store policy could not authenticate/decrypt the file.
    #[error("encrypted database key rejected or database is not encrypted")]
    KeyRejected,
    /// This build did not expose a working SQLCipher backend.
    #[error("SQLCipher backend unavailable")]
    EncryptionUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_state_is_explicit_and_never_implied_by_cancellation() {
        let rolled = StoreError::mutating(StoreErrorKind::Sqlite, CommitState::RolledBack);
        assert!(!rolled.may_have_committed());
        let committed = StoreError::mutating(StoreErrorKind::Sqlite, CommitState::Committed);
        assert!(committed.may_have_committed());
        // A canceled caller must never be able to conclude rollback from the mere
        // fact that its future was dropped; `Unknown` exists to name that case.
        let unknown = StoreError::mutating(StoreErrorKind::Sqlite, CommitState::Unknown);
        assert!(unknown.may_have_committed());
    }

    #[test]
    fn store_errors_never_echo_payloads() {
        let error = StoreError::new(StoreErrorKind::Corrupt("network row"));
        assert_eq!(
            error.to_string(),
            "store failure: durable state is invalid: network row"
        );
    }
}
