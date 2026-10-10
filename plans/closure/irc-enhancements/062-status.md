# Plan 062 — M012-D Integrated Connectivity/Recovery Qualification

Status: closed; deterministic and pinned Eggchaos product-path qualification passed; deployed-service evidence remains separate
Implementation commit: `f028a43` — `feat(runtime): add attested I2P endpoint failover`
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Pace post-registration recovery while preserving liveness | Plan 059 closed with the shared bounded command pacer; full runtime/integration suites and the stable/Rust 1.88 verification runs passed. The recovery scheduler accepts only bounded setup frames, while PING/PONG and online control paths bypass it. |
| Record truthful upstream gaps and recover by generation | Plan 059's v14 bounded Store ledger records open/reconnected/interrupted states and monotonic duration only within one process. Existing reconnect, interrupted restart, stale-generation and Store-pressure tests are included in full verification. Ambiguous chat is never replayed across generations. |
| Qualify failover together with pacing and gap handling | Plan 061's `failed_primary_rotates_only_to_the_attested_same_network_alternate` passed. Endpoint policy remains attached to one durable NetworkId, one NetworkOwner and one typed I2P provider scope. Registration/auth rejection is terminal. No alternate endpoint is inferred from its name. |
| Keep upstream CHATHISTORY truthful | Plan 060 was formally closed with the production feature deferred because the official draft warns against production use. No upstream history request or catch-up replay exists. Therefore no-history/ephemeral semantics during upstream catch-up are not applicable; local history privacy/cursor behavior remains covered by the workspace history/privacy suite. |
| Preserve bounds, privacy, restart behavior and cross-network isolation | `cargo test --workspace --all-features`: 706 passed, 4 ignored. The passing suites include adverse reconnect, partial protocol handling, bounded owner/provider attempts, schema predecessor migration, encrypted/ephemeral history, no-history matrices, Store pressure, diagnostics redaction, and multi-Network isolation. All four ignored cases are annotated for the explicit pinned Eggchaos target. |
| Verify static network boundary and release build | `sh scripts/verify.sh full` and `rustup run 1.88.0 sh scripts/verify.sh full` completed through workspace/doc tests and release fuzz smoke on the available macOS environment. The full script includes formatting, Clippy with warnings denied, all-feature tests, and positive/negative static network-boundary controls. |
| Qualify Eggchaos fault campaign | `EGGCHAOS_BIN=/tmp/i2pr-irc-eggchaos/target/release/eggchaos python3 scripts/qualify-m007-eggchaos.py` passed. The CLI reported version 0.2.0, built from pinned source commit `b6a277d5ad4267bd602bc15a4333b14322057b90`; executable SHA-256: `b1733d5f9caadb7cd9590e57c1022f19758f1d562caadc74e172bafacf87df73`. Generic loopback smoke passed all eight phases. Product-path shaping passed with one stream; blackhole recovery ended within the bounded liveness deadline and recovered at generation 2; hard disconnect recovered at generation 2; disruptive recovery replayed no ambiguous user frame. All four ignored product-path cases passed. |
| Build provenance and command | Cloned `https://github.com/eggstack/eggchaos`, checked out exact commit `b6a277d5ad4267bd602bc15a4333b14322057b90`, and ran `cargo build --release -p eggchaos-cli`. The resulting `eggchaos version` reported API `v1`, version `0.2.0`; its SHA-256 is recorded above. |
| Qualification command | `EGGCHAOS_BIN=/tmp/i2pr-irc-eggchaos/target/release/eggchaos python3 scripts/qualify-m007-eggchaos.py` |
| Claim deployed IRC2P/ILITA compatibility | No authorized live transcript or endpoint test was performed. No such claim is made; fake IRCd tests establish controlled fixture behavior only. |

## Security, recovery, and limits

The deterministic and pinned external evidence qualifies the repository's existing product paths across a process/socket boundary. The Eggchaos fixtures use loopback and a fake SAM bridge; they do not establish live IRC2P/ILITA compatibility, actual router-blackout behavior, or packet-loss behavior. Continue to preserve at-most-once behavior for ambiguous user messages.

Plan 060's upstream catch-up implementation remains deferred. Failover trust depends on explicit operator attestations and is not evidence that any real endpoint pair is equivalent. Plan 054 remains independently active for controlled live IRC-over-i2pd product-path evidence. R002 remains blocked on stable public i2pr managed-app contracts.

## Registry and roadmap disposition

Plan 062 is closed. Plans 063-066 are also closed; no successor is unblocked by this closure. Plan 054 remains independently active for controlled live IRC-over-i2pd evidence, and R002 remains blocked on public i2pr managed-app contracts.
