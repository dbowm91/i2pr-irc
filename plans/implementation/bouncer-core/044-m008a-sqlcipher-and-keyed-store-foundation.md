# Bouncer Core M008-A / Plan 044 — SQLCipher and Keyed-Store Foundation

Status: blocked

Hard dependency:

- plans/closure/bouncer-core/043-status.md

Research authority:

- plans/research/009-m008-m009-privacy-encryption-and-otr.md

Architecture authority:

- plans/adrs/ADR-0006-encryption-layering-store-key-and-otr-endpoint.md
- plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md

Primary class: security infrastructure + persistence

## 1. Objective

Add an optional whole-database encrypted store mode using SQLCipher while preserving the existing single-worker SQLite architecture, FTS5 history search, plaintext compatibility mode, Rust 1.88 floor, and explicit key ownership boundary.

This plan establishes the backend and keyed-open primitive only. Plaintext-to-encrypted migration/key rotation is Plan 045.

## 2. Backend qualification before adoption

The workspace currently uses rusqlite 0.40 with bundled SQLite.

rusqlite 0.40 exposes SQLCipher build features. The preferred first candidate is the self-contained bundled SQLCipher path with vendored crypto provider so an encrypted build does not require the Operator to preinstall SQLCipher/OpenSSL.

Before freezing the workspace feature:

- prove the candidate builds on current supported Linux, macOS and Windows CI/toolchains;
- prove Rust 1.88 compatibility;
- prove FTS5 is enabled in the SQLCipher amalgamation;
- record SQLCipher major/version and crypto-provider provenance;
- record SQLCipher Community Edition and transitive crypto-provider licenses;
- compare binary/build implications against the current bundled SQLite path.

If the candidate cannot satisfy supported-platform/MSRV requirements, stop and register a backend corrective/ADR update. Do not substitute ad-hoc field encryption.

## 3. Runtime policy

One SQLCipher-capable SQLite backend may serve both policies:

- Plaintext;
- Encrypted(StoreKey).

SQLCipher's ordinary no-key behavior must remain SQLite-compatible.

Existing Store::open may remain the compatibility plaintext wrapper.

Add an explicit API such as:

~~~rust
pub struct StoreOpenOptions {
    pub encryption: StoreEncryption,
}

pub enum StoreEncryption {
    Plaintext,
    Encrypted(StoreKey),
}
~~~

Names may differ, but the distinction must be typed rather than inferred from file contents.

## 4. StoreKey

Introduce a dedicated secret type.

Required properties:

- exactly 256 bits of key material for the baseline;
- high-entropy raw key supplied by caller;
- no Display;
- Debug renders [redacted];
- zeroized on drop;
- avoid Clone unless a concrete ownership need is demonstrated;
- never serialized;
- never stored in SQLite;
- never exposed through diagnostics/config snapshots.

The open path should consume the application-owned key value and zeroize the caller-side representation as soon as SQLCipher has accepted it.

Passphrase derivation/user prompting/keyring/HSM access are explicitly outside crates/store.

## 5. Key application

Encrypted open sequence:

1. open SQLite/SQLCipher connection;
2. apply raw key before any application schema read;
3. force a keyed database read;
4. verify SQLCipher is actually active;
5. then run existing application-id/schema/required-table/index validation and migrations;
6. only then start the store worker.

Do not call schema::open_and_migrate before keyed-open validation.

Wrong key must fail closed before any schema initialization/migration.

## 6. Safe key submission

Use the safest API available through the selected safe Rust dependency surface.

Constraints:

- workspace unsafe_code = forbid remains intact;
- no custom raw sqlite3_key FFI in this repository;
- no key interpolation from untrusted text;
- temporary key encodings are short-lived and redacted;
- errors must not include PRAGMA text/key material.

If rusqlite's safe PRAGMA path is the only supported key mechanism, isolate it in one store-private function and document the temporary-memory limitation honestly.

## 7. Error taxonomy

Add typed/redacted errors sufficient to distinguish operational action without leaking details, for example:

- encryption backend unavailable;
- key rejected / encrypted database unreadable;
- plaintext database opened under encrypted-only policy;
- schema corruption after successful keyed open.

Do not pass raw SQLite/SQLCipher error strings through public diagnostics if they may include SQL or values.

## 8. Plaintext compatibility

Plaintext policy must:

- continue to open existing schema 1-8 fixtures/migrations;
- create ordinary plaintext DB when explicitly selected;
- retain all existing store semantics.

Encrypted policy must not silently interpret a plaintext database as a new empty encrypted database.

Likewise plaintext policy against an encrypted DB must fail rather than overwrite/reinitialize it.

## 9. In-memory stores

Tests may continue to use StorePath::Memory.

Encrypted in-memory mode may be supported for API symmetry, but it carries no at-rest security claim.

Do not force all existing memory fixtures to supply keys.

## 10. FTS/search preservation

The SQLCipher build must expose FTS5.

All existing:

- history append;
- history_search maintenance;
- MATCH queries;
- search-index integrity validation;
- retention cleanup

must run unchanged after encrypted open.

No field-level encryption or alternate search index is introduced.

## 11. Dependency/security review

Record:

- exact rusqlite/libsqlite3-sys versions;
- SQLCipher source/version;
- crypto provider;
- licenses;
- native toolchain requirements;
- MSRV effect;
- binary size/build-time impact where readily measurable.

No network dependency is added at runtime.

## 12. Tests

Minimum:

- plaintext store creates/opens normally;
- encrypted store creates and reopens with same key;
- wrong key fails;
- plaintext mode refuses encrypted database;
- encrypted mode against plaintext database fails without destructive mutation;
- key Debug is redacted;
- SQLCipher cipher_version/non-secret backend proof;
- schema migration/integrity checks execute after key validation;
- history FTS works in encrypted mode;
- Store worker shutdown releases its key-bearing connection normally.

## 13. Verification

Run current and Rust 1.88 full verification with the SQLCipher-capable build.

Add/extend CI for supported platform compile/test evidence as appropriate.

## 14. Acceptance criteria

Plan 044 closes only when:

1. the selected SQLCipher backend is buildable on supported targets and Rust 1.88;
2. encrypted Store open/reopen works through the normal worker API;
3. wrong/missing key fails closed before migration;
4. existing plaintext stores remain explicitly supported;
5. FTS5/search remains operational;
6. key material is redacted/zeroized at the application boundary;
7. no unsafe Rust or field-encryption fallback is introduced.

## 15. Stop conditions

Stop for new planning if:

- SQLCipher cannot meet supported-platform/MSRV packaging requirements;
- enabling SQLCipher disables required FTS5 behavior;
- safe key application would require repository-local unsafe FFI;
- one backend cannot support explicit plaintext and encrypted modes safely;
- a key source must be embedded into crates/store.

## 16. Closure evidence

Create plans/closure/bouncer-core/044-status.md with:

- backend/platform/MSRV matrix;
- dependency/license record;
- keyed-open/wrong-key matrix;
- FTS qualification;
- key redaction/zeroization evidence;
- Plan 045 readiness.
