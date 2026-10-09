# Standalone M010-D / Plan 053 Closure

Status: closed
Implementation commit: `8efa7999c5ccbec853c8252d8240e5c0db025069`
Corrective runtime evidence: `42c3338a8228bf38448897872d50b8a02ee48b0f`
Predecessor: `plans/closure/standalone/052-status.md`

## Requirement-to-evidence

| Requirement | Evidence | Result |
|---|---|---|
| Secure init with encrypted storage by default | `i2pr-irc init` creates independent 256-bit random operator token and SQLCipher key; explicit `--plaintext` is the only plaintext path | Pass |
| Refuse replacement and preserve interruption evidence | Private state directory, exclusive no-overwrite installation, durable `.init-incomplete` marker and injected interruption points across verifier/key/database/config/finalize stages | Pass |
| Protect local paths and fail closed | Unix effective-user ownership, private mode, symlink and parent checks; startup repeats checks; non-Unix reports unsupported rather than assuming ACL equivalence | Pass |
| Authenticate local listener from provisioned verifier | Daemon loads protected verifier and store key before enabling loopback service; loopback PASS token login covered through the production listener; profile grammar is bounded | Pass |
| Keep secrets out of routine output | Token displayed once after successful init; `status` is redacted; verifier stores a one-way digest and compares in constant time; no secret is in process arguments | Pass |
| Encrypted reopen and key failure | Store startup uses the independent key file and existing encrypted Store checks; absent/invalid key and wrong-key paths fail without plaintext fallback | Pass |
| Operator setup and recovery limits | README documents initialization, local client PASS/profile use, backup scope, lost-key irrecoverability and incomplete-init handling | Pass |
| Preserve I2P-only upstream authority | `scripts/check-network-boundary.py` passed, including positive controls; only configured loopback local listener authority is permitted | Pass |

## Verification

- `rtk cargo fmt --all` — passed
- `rtk cargo test -p i2pr-irc-daemon --locked` — 18 passed
- `rtk cargo clippy -p i2pr-irc-daemon --all-targets --all-features --locked -- -D warnings` — passed
- `rtk python3 scripts/check-network-boundary.py` — passed with positive controls
- `rtk sh scripts/verify.sh full` — passed on the default Rust 1.89 toolchain
- `rtk env CARGO_INCREMENTAL=0 rustup run 1.88.0 sh scripts/verify.sh full` — passed, including workspace tests and release fuzz-smoke

Plan 052's runtime response-drain corrective was separately regression-tested and included in the Rust 1.88 full run. An additional manual `cargo run` CLI smoke attempt stalled waiting on Cargo's build lock and was interrupted; it is not counted as passing evidence. The CLI paths are exercised through daemon tests and the full workspace suite.

## Security and recovery review

Initialization is encrypted by default, with no opportunistic downgrade when key access or SQLCipher open fails. The generated login token and database key are independent. A random-token digest is appropriate for this high-entropy credential and uses constant-time equality. File installation uses same-filesystem exclusive hard-link publication of synced private temporary files, providing atomic no-overwrite visibility; `.init-incomplete` prevents daemon startup after partial initialization. Existing state is not overwritten. Unix-only permission enforcement is explicit; other platforms fail closed pending platform-specific policy. Lost database keys are unrecoverable, and the operator is told to back up the entire private state directory securely.

## Handoff

Plan 054 is promoted to active. Its deterministic/static checks can proceed, but M010 closure remains gated on an authorized controlled IRC service reachable over a live i2pd SAM product path. The installed i2pd 2.61.0 binary was not running, and no controlled service/destination was available in this environment. Prior R001 transport evidence is reusable substrate evidence only; it does not prove the complete daemon product path. See the active blocker in Plan 054. M010 is not closed.
