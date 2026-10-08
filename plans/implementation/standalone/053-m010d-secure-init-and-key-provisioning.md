# Standalone M010-D / Plan 053 — Secure Initialization and Key Provisioning

Status: active — Plan 052 closed; Plan 054 remains sequentially gated
Repository baseline for planning: f325e7d5e495e36b1fc7b168c30e4b725756d22c
Primary class: security capability + operational polish
Authority: plans/subsystems/standalone-daemon-roadmap.md; ADR-0006; ADR-0007; Research 010

## 1. Objective

Make an uninitialized standalone installation safely operable without hand-editing a database or supplying secrets in command-line arguments. Build init/provisioning for config, state directory, Operator credential verifier/source, independent SQLCipher store key, and repeatable operator setup for a local IRC client. Add honest failure and recovery guidance.

## 2. Readiness and existing evidence

052 must be closed with stable ClientId/profile and authentication policy. Store::open_with_options supports explicit SQLCipher encryption; StoreKey::from_bytes consumes 32 bytes. Migration, encrypted export and rotation exist in store already (M008). This plan owns only key source/UX and secure local configuration, not a second crypto implementation. No hardware keyring/HSM requirement.

## 3. Invariants

- Default newly initialized state uses encrypted storage; plaintext must be an explicit opt-in policy, never fallback for a key failure.
- SQLCipher key and Operator bearer credential are independent uniformly generated values; no derivation from nickname, client password, host ID or environment identity.
- No secret in process argv, normal stdout/stderr logs, diagnostic snapshots, config export, IRC wire without operator intent, or git-tracked example config.
- Init does not replace existing state/key/credential silently; on a partial write it leaves recognizable recoverable state, not an apparently successful empty database.
- Security of private paths is checked on startup, not just on init; Unix file/directory permissions + symlink/ownership checks; Windows ACL/equivalent policy requires target-specific qualification or explicit unsupported error.
- A rotated local login credential does not rekey the database or change ClientId mappings; a rotated database key does not change login identities.

## 4. Required production changes

Implement i2pr-irc init (or equivalent reviewed CLI): choose a secure explicit application data dir, create config and private subdirectories, generate OS-random 256-bit Operator token, provision a protected verifier credential with stable client-profile grammar, generate independent 256-bit SQLCipher key to a protected file if enabled, create and verify new Store via Store::open_with_options, and emit one intentional securely scoped credential-display/provisioning mechanism. Do not print bearer token on routine daemon startup. Clear temporary bytes after use where supported, and do not preserve secret in panic formatting.

Use write-to-exclusive-temp + sync + atomic rename for new secret/config files, refusing destination overwrite and unsafe parent or symlink paths. Make init idempotent in refusal semantics: re-invocation without an explicit recovery command must not alter existing state. Review failure ordering and cleanup with power-loss/crash injection around each rename.

Authentication token format: fixed sufficient entropy and canonical encoding; if storing a verifier, use constant-time cryptographic digest verification for these generated random tokens. Low-entropy user-entered passwords are out of scope absent an explicitly reviewed slow-password KDF and memory limits.

Add CLI informational tools to show redacted config/state (without secrets) and documented instructions for attaching local IRC clients. Credential rotation/recovery may be delegated to a future separate plan if not safely implementable, but explicit safe failure/reinitialization semantics, backups and lost-key warning are required now.

## 5. Ordered work

A. Audit state/key/credential file threat model and target OS semantics; choose private-path and atomic-writing libraries under Rust 1.88.
B. Implement init and exact encrypted/plaintext config decision and idempotent refusal.
C. Wire protected auth verifier and optional SQLCipher key file into daemon startup; fail closed on mismatch.
D. Add permission/symlink/ownership/partial-init tests and documented operator setup; remove any secrets from routine diagnostics.
E. Verify old M008 databases remain supported without implicit migration or key changes; qualify key mismatch and missing-key handling.

## 6. Failure/restart/concurrency

Missing key, wrong key, unreadable credentials, unsafe perms, corrupt config, ambiguous mode or partial init: reject startup without wiping or altering durable state. Prevent simultaneous init/run against same directory through the existing lease. If secret generation succeeds but later init fails, never publish a success message; preserve a reversible/recoverable marker without exposing secrets. Do not install a default predictable password. Do not permit the daemon to autogenerate keys merely because it cannot find them.

## 7. Verification

Fresh init encrypted -> start -> stop -> reopen; explicit plaintext; wrong key/no key/key for another database; missing/malformed Operator verifier; symlinked key/config; world-readable paths on Unix; dangerous shared parent; repeated init and conflicting run; crash after each atomic stage; rotated login credentials if implemented; data/history unchanged across restart; Microsoft/Unix target gating and Rust 1.88. Test redaction against known sentinel secrets in all logs/errors/config snapshots.

Run targeted store/daemon tests plus full workspace verification and boundary positive controls. No multi-router SAM matrix needed.

## 8. Docs / acceptance / stop

Document supported OS paths, backup and key-loss behavior, how to configure ordinary IRC client PASS/SASL and stable profile, encryption vs OTR, and manual recovery limits. Accept only when normal installation can initialize encrypted state without editing source, reread it across restart, and cannot silently destroy it through missing/wrong credentials. Stop if target platform cannot provide verified private file protection, secret source is exposed in argv/logs, or encryption is opportunistically downgraded.

## 9. Closure

Record exact init commands on test-only directories, key/failure matrix, permission checks and relevant commits in plans/closure/standalone/053-status.md. Promote 054 only after 053 closes.
