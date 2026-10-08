# Bouncer Core M008-A / Plan 044 Closure — SQLCipher and Keyed-Store Foundation

Status: closed

Implementation commit: `0600338e1f666939ee9974d9cd784ffb89931771` — `feat(store): add SQLCipher keyed open foundation`

## Outcome

The Store now supports explicit plaintext and SQLCipher-encrypted policies through its ordinary single-worker API. Encrypted open applies a caller-supplied 256-bit `StoreKey`, checks that SQLCipher is active, forces a keyed read before schema validation/migration, and only then starts the worker. The existing plaintext `Store::open` compatibility path remains. `StoreKey` has redacted Debug output and zeroizes its application-owned byte array on drop. The safe rusqlite PRAGMA path is isolated; its temporary hexadecimal encoding is zeroizing, while rusqlite/SQLCipher internal copies cannot be controlled by this crate.

## Backend, dependency, and platform evidence

- `rusqlite 0.40.2` and `libsqlite3-sys 0.38.2`, with `bundled-sqlcipher-vendored-openssl`.
- SQLCipher Community Edition 4.14.0 (SQLite 3.51.3) with FTS5; OpenSSL 3.6.3 vendored at build time. No runtime network dependency.
- License review recorded in `architecture/dependency-review.md`: SQLCipher BSD-3-Clause, OpenSSL Apache-2.0, rusqlite/libsqlite3-sys MIT/Apache-2.0.
- Rust 1.88 compile checks passed for macOS arm64 host, Linux x86_64 and aarch64, and Windows x86_64 GNU. Full native workspace verification passed on macOS x86_64 current stable and macOS arm64 Rust 1.88. Windows/Linux checks establish compilation only, not runtime execution.
- Existing repository supported-target build scripts and vendored native sources were used; no backend corrective/ADR change was needed.

## Acceptance evidence

| Requirement | Evidence |
|---|---|
| Explicit plaintext/encrypted mode and ordinary worker API | `StoreOpenOptions`, `StoreEncryption`, `Store::open_with_options`; existing plaintext wrapper retained |
| Key length, redaction, zeroization boundary | `StoreKey` accepts exactly 32 bytes, redacted Debug test, `Zeroizing<[u8; 32]>` drop semantics |
| Wrong key and mode mismatch fail closed | Encrypted integration tests check wrong-key rejection, encrypted file under plaintext policy, plaintext file under encrypted policy, and source-byte preservation |
| Keyed read before schema migration | Private key setup checks cipher version and reads `sqlite_master` before `schema::open_and_migrate` |
| SQLCipher and FTS5 enabled | Non-empty `cipher_version`; encrypted history append/search test; FTS5 virtual-table test |
| Existing plaintext behavior | Full workspace store/runtime qualification passes; explicit plaintext database open remains covered |
| No unsafe or field-encryption fallback | Workspace unsafe prohibition unchanged; whole-database SQLCipher only |

## Verification

All commands passed on the committed implementation tree:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `scripts/check-network-boundary.py`
- `scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`
- Rust 1.88 `cargo check -p i2pr-irc-store --all-features --locked --target x86_64-unknown-linux-gnu`
- Rust 1.88 equivalent store checks for `aarch64-unknown-linux-gnu` and `x86_64-pc-windows-gnu`

Both full verification runs include workspace formatting, clippy, tests, static network-boundary checks, and fuzz smoke checks. The focused encrypted tests cover create/reopen, FTS search, wrong key, policy mismatches, redacted errors, and unchanged source bytes.

## Review and handoff

No unresolved implementation finding remains within Plan 044. The SQLCipher build path satisfies the declared MSRV and tested target compilation matrix. This closure does not claim runtime testing on Linux or Windows, nor Windows MSVC ABI support. Plan 045's source-preserving migration and key-rotation requirements are implementable on this explicit keyed-open foundation, so Plan 045 is unblocked. Plan 046 remains blocked on Plan 045; Plans 047-048 remain blocked on M008 closure and then Plan 047 respectively.
