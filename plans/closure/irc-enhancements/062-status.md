# Plan 062 — M012-D Integrated Connectivity/Recovery Qualification

Status: conditionally closed; deterministic product-path qualification passed; pinned Eggchaos and deployed-service qualification remain open evidence conditions
Implementation commit: `f028a43` — `feat(runtime): add attested I2P endpoint failover`
Closure commit: `34d3bb6` — `docs(plans): conditionally close M012 qualification and start M013-A`
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
| Qualify Eggchaos fault campaign | `python3 scripts/qualify-m007-eggchaos.py` reported `NOT RUN`: eggchaos-cli v0.2.0 from the pinned source revision is not installed. No router-blackout claim is made. This is the named remaining qualification condition. |
| Claim deployed IRC2P/ILITA compatibility | No authorized live transcript or endpoint test was performed. No such claim is made; fake IRCd tests establish controlled fixture behavior only. |

## Security, recovery, and limits

The deterministic evidence qualifies the repository's existing product paths, but it does not replace the external pinned fault campaign. Plan 062 is conditionally closed so independent profile and protocol research may proceed without presenting Eggchaos or deployed service behavior as proven. Reopen this qualification when the pinned tool is available and run its four explicitly ignored product-path cases before making an Eggchaos-based recovery claim. Continue to preserve at-most-once behavior for ambiguous user messages.

Plan 060's upstream catch-up implementation remains deferred. Failover trust depends on explicit operator attestations and is not evidence that any real endpoint pair is equivalent. Plan 054 remains independently active for controlled live IRC-over-i2pd product-path evidence. R002 remains blocked on stable public i2pr managed-app contracts.

## Registry and roadmap disposition

Plan 062 is conditionally closed with the external fault-campaign condition above. Plan 063 is ready to proceed with explicit authentication profiles; its fixture evidence must remain distinct from current deployed IRC2P/ILITA behavior. Plan 064 remains proposed pending Plan 063. Plan 065 remains research-blocked pending its own preflight and Plan 064 disposition. Plan 066 remains proposed pending the M013 dispositions.
