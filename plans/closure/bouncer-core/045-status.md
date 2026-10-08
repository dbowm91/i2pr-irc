# Bouncer Core M008-B / Plan 045 Closure — Encrypted Store Migration and Key Rotation

Status: closed

Implementation commit: `49d3a8fe24700cf43cfe2044de6528dd15a4898b` — `feat(store): export encrypted copies and rotate keys`

## Outcome

`export_encrypted_copy` is an offline, source-preserving API. The caller declares the source policy and supplies a distinct destination key. It validates and normally migrates the source, reserves a new sibling destination exclusively, uses SQLCipher's `sqlcipher_export` without materializing the database in application memory, explicitly sets `application_id` and `user_version`, closes the attachment, syncs the result and parent directory where available, and reopens it with the ordinary encrypted Store API before returning success. The source is never removed or replaced. The same path handles plaintext-to-encrypted conversion and encrypted old-key-to-new-key rotation.

The caller contract requires all Store workers for the source to be stopped and joined first. The API refuses existing or non-sibling destinations. Wrong source keys fail before destination creation. After reservation, failures remove the incomplete destination and known SQLite sidecars best-effort; the source remains readable under its declared policy. Unix destinations are created with mode `0600`. No secure-deletion or install-swap claim is made.

## Migration and rotation matrix

| Case | Evidence | Result |
|---|---|---|
| Current schema-8 plaintext to encrypted | Integration test creates ordinary store, copies, reopens with new key | Pass |
| Supported older plaintext schema (v7 fixture) to encrypted | Test exercises normal v7-to-v8 source migration followed by export and keyed reopen | Pass |
| Encrypted key A to key B | Rotation test verifies key B succeeds and key A is rejected on destination | Pass |
| Wrong old key | Test verifies `KeyRejected`, destination was never created, and source bytes remain identical | Pass |
| Existing destination | Test verifies typed refusal and no overwrite | Pass |
| Failure after export / before verification | Internal injected failpoints prove destination cleanup and source reopenability | Pass |

## Durable-state and FTS parity

The current-schema migration test copies a bounded maximum history batch (512 rows) plus a sentinel event. It checks equality of the loaded Network record including SASL secret and desired-channel state, registration action phase/order and secret payload, client identity, cursor, read marker, event identity/search hit ordering, and FTS search results. SQLCipher export copies the full SQLite database through its supported export mechanism; it does not create a second in-memory history representation. The v7 fixture establishes migration-before-export behavior for a supported predecessor. The full M008-C qualification remains responsible for the broader sentinel, parity, platform/runtime, and performance matrix.

## Verification and review

Passed:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `scripts/check-network-boundary.py`
- `scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`

Both full verification runs include formatting, clippy, workspace tests, static network-boundary checks, and fuzz smoke. Store tests include the migration/rotation matrix and injected failures. Review confirmed parameterized ATTACH path/key inputs, redacted migration errors, source-key validation before destination mutation, explicit destination metadata, ordinary keyed reopen verification, and no production test hook for failure injection.

## Handoff

No unresolved Plan 045 implementation finding remains. Plan 046's durable-state qualification, sentinel scan, platform/runtime evidence, performance smoke, and threat-model language can proceed and is marked ready. Plans 047-048 remain blocked until M008 closes, then Plan 047 closes before Plan 048 can start.
