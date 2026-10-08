# Bouncer Core M008-C / Plan 046 Closure — Encrypted Durable-State Qualification

Status: closed

Qualification implementation commit: `d2f9f65159dded686c722f83da443a0603fba7a4` — `test(store): qualify encrypted durable state privacy`

## Sentinel and closed-file evidence

The qualification uses only synthetic SASL, registration-action, endpoint/display/channel, message, sender/target, FTS-only, msgid, and timestamp sentinels. Data is written through normal Store APIs. After clean shutdown the scanner reads the encrypted main database and any `-wal`, `-shm`, and rollback-journal files that exist. It finds none of the sentinels and no ordinary SQLite file signature. A plaintext control is written through the same public Store API and the same scanner finds its metadata/credential sentinels, demonstrating the negative result is discriminating.

The qualification also rotates the encrypted fixture from key A to key B, scans the resulting destination and sidecars, and confirms the closed destination contains no corpus values. No temporary migration file is used by the production copy path; failed reserved destinations and known sidecars are covered by Plan 045's injected cleanup tests.

| File set | Check | Result |
|---|---|---|
| Encrypted main database | Every synthetic sentinel and SQLite plaintext signature | Absent |
| Encrypted `-wal`, `-shm`, `-journal` when present | Every synthetic sentinel and SQLite plaintext signature | Absent |
| Rotated encrypted destination and sidecars | Every synthetic sentinel and SQLite plaintext signature | Absent |
| Plaintext control database | Same scanner detects stored sentinel values | Detected |

This is a raw-substring at-rest qualification, not a cryptanalytic proof.

## Functional parity and migration/rotation

| Behavior | Evidence | Result |
|---|---|---|
| Correct-key restart | Reopen rotated database through normal worker; load SASL secret and phased actions | Pass |
| Wrong-key fail-closed | Old key rejected on rotated destination before data is served | Pass |
| History and CHATHISTORY references | Bounded `query_history`, unique `msgid`, event IDs and timestamps | Pass |
| FTS and FTS-only term | Query after restart returns the same event identity | Pass |
| Client cursor and read marker | Both read back after restart | Pass |
| Retention/compaction | Bounded retention removes the old event and updates its index transactionally | Pass |
| Plaintext-to-encrypted migration and rotation | Plan 045 current and v7 cases; key A to key B; source remains | Pass |
| Export/verification failure recovery | Injected failure after export and before ordinary reopen; destination removed and source reopens | Pass |
| Configuration snapshot secret omission | Existing runtime test `a_configuration_export_carries_no_credential` passes in both full verification runs | Pass |

Plan 045's parity test additionally checks Network identity/configuration and secrets, desired/detached channels, registration action ordering/phase/payload, client identity, cursor/read marker, history event IDs, FTS results, and a 512-row bounded append batch.

## Key leakage review

Source search for `StoreKey` and formatting, serialization, logging, or tracing found no production key-rendering path. The only key formatting occurrence is the unit assertion that Debug output equals `StoreKey([redacted])`. `StoreError` stores only typed error kinds and discards SQLite messages. Migration returns `Result<(), StoreError>` and does not generate a report containing key-derived data. Existing diagnostics and configuration snapshot tests assert that endpoints and credential values are omitted. No key fingerprint is recorded here.

The temporary raw-key hex representation and app-owned key bytes use `Zeroizing`; SQLCipher/rusqlite internal copies and live-process memory are outside this crate's control. Encryption protects the durable database while the key is kept separately. It does not protect against a compromised live process, a key stolen alongside the database, endpoint compromise, plaintext IRC conversations, or metadata exposed during ordinary operation. SQLCipher is not end-to-end encryption.

## Platform and dependency matrix

| Target/toolchain | Evidence | Result |
|---|---|---|
| macOS x86_64, current stable | Full `scripts/verify.sh` including encrypted runtime tests | Pass |
| macOS aarch64, Rust 1.88 | Full `scripts/verify.sh` including encrypted runtime tests | Pass |
| Linux x86_64, Rust 1.88 | SQLCipher Store cross `cargo check` using Zig | Compile pass; runtime not executed |
| Linux aarch64, Rust 1.88 | SQLCipher Store cross `cargo check` using Zig | Compile pass; runtime not executed |
| Windows x86_64 GNU, Rust 1.88 | SQLCipher Store cross `cargo check` using MinGW | Compile pass; runtime not executed |

The native qualification host was macOS; Linux and Windows are recorded as compile checks only, with no runtime claim. The Rust 1.88 floor and the SQLCipher vendored OpenSSL path compile for the tested OS/architectures. `architecture/dependency-review.md` records rusqlite 0.40.2, libsqlite3-sys 0.38.2, SQLCipher CE 4.14.0 / SQLite 3.51.3 / FTS5, OpenSSL 3.6.3, and their licenses. Runtime does not fetch dependencies or crypto-provider source.

## Performance sanity

A single bounded smoke run on the macOS qualification host compared an empty-store open, one 128-event history append, and a representative FTS query:

| Mode | Open | Append 128 rows | FTS query |
|---|---:|---:|---:|
| Plaintext | 26.5 ms | 34.3 ms | 28.2 ms |
| SQLCipher | 7.5 ms | 29.5 ms | 30.5 ms |

These are one small smoke sample, not a benchmark or performance guarantee. They show no catastrophic regression in this bounded workload; open-time variation was higher than the encryption delta.

## Verification

Passed on the qualification tree:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p i2pr-irc-store --all-features --locked` (98 tests)
- `scripts/check-network-boundary.py`
- `scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`
- Rust 1.88 Store SQLCipher cross checks: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, and `x86_64-pc-windows-gnu`
- focused encrypted qualification and performance smoke tests

Both full verification runs include formatting, clippy, all workspace tests, network-boundary static checks, and fuzz smoke. No external router or network was required.

## M008 disposition and handoff

All M008 acceptance criteria are met: encrypted file scanning is negative against a positive plaintext control; correct-key restart, wrong-key rejection, FTS/history/cursor/marker/retention behavior, source-preserving migration and rotation pass; key leakage review is clean; threat-model language is bounded; and the Rust 1.88/backend target matrix is recorded without claiming unavailable runtime executions.

M008 is closed. Plan 047's OTRv3 opaque-carriage and multi-client invariants can proceed and is marked ready. Plan 048 remains blocked until Plan 047 closes.
