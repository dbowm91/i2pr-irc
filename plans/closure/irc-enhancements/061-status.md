# Plan 061 — M012-C Closure Status

Status: closed; operator-attested same-Network failover implemented; deployed endpoint equivalence remains unqualified
Implementation commit: `f028a43` — `feat(runtime): add attested I2P endpoint failover`
Closure commit: pending
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Keep one owner and typed I2P-only authority | `NetworkOwner` rotates sequentially among endpoints stored on one durable `NetworkRecord`, using the same `NetworkId`-scoped `I2pStreamProvider`. There is no generic dialer, DNS, proxy or unrelated-network fallback. `failed_primary_rotates_only_to_the_attested_same_network_alternate` covers provider failure and owner scope. |
| Require explicit equivalence and credential authorization | Durable failover policy requires both operator attestations. BouncerServ requires explicit `equivalent=yes credentials=yes`, supports bounded addition and clear, and refuses raw Destination alternates, duplicates and over-limit groups. `the_local_service_requires_explicit_failover_attestation_and_can_manage_alternates` verifies control behavior. This is a configuration attestation, not independent proof of federation. |
| Preserve endpoint order, bounds and restart compatibility | One primary plus at most seven alternates; alternates are supported I2P name/base32 forms at most 240 bytes. Store schema v15 migrates older records without a failover group and persists an ordered group transactionally. Snapshot v4 explicitly imports enabled/disabled policy; v1-v3 preserve an existing group. `an_attested_failover_group_is_bounded_and_survives_reopen`, `every_supported_predecessor_schema_opens_and_reaches_the_current_version`, and `an_operator_attested_failover_group_round_trips_and_legacy_import_preserves_it` cover persistence and import. |
| Rotate only for retryable transport/provider failures | Provider, timeout, protocol and I/O failures advance to the next alternate under shared reconnect admission/backoff. Registration rejection is terminal and cannot forward credentials to another endpoint. Exhaustion returns to controlled offline/backoff; process restart starts at primary. Rotation is sequential within the existing NetworkOwner. |
| Keep diagnostics and configuration exports secret-safe | Diagnostics report only the selected endpoint index; endpoint values are excluded from debug, diagnostics, and network listings. `selected_failover_index_is_reported_without_an_endpoint_value` and control/export tests cover redaction. |
| Preserve prior migration and runtime invariants | `cargo test --workspace --all-features` passed: 706 passed, 4 ignored. Focused model, snapshot, migration, Store reopen, failover owner and operator-control tests passed. `cargo fmt --all -- --check`, Clippy with all targets/features and warnings denied, and `git diff --check` passed. |
| Stable and MSRV qualification | `sh scripts/verify.sh full` and `rustup run 1.88.0 sh scripts/verify.sh full` completed through their workspace/doc-test and release fuzz-smoke stages. The standard targets report four Eggchaos cases as ignored because they require the separately pinned external qualification target. |

## Security, recovery, and limitations

The implementation does not determine whether two `.i2p` endpoints share an operator, service identity, history, account database, or authorization domain. Operators must explicitly attest trust and credential scope before an alternate is eligible. No live service transcript or endpoint-pair qualification was collected, and no current IRC2P/ILITA federation equivalence is claimed. The raw Destination form remains supported for the primary but is disallowed for alternates due to the bounded snapshot line format. Endpoint selection is memory-only and resets to primary on process restart.

No user chat or non-idempotent command is replayed during endpoint rotation. A terminal registration/authentication rejection is not treated as a transport failure. Shared state and credentials stay bound to the existing durable Network; configuring an incorrect trust group remains an operator error and should be corrected by clearing or editing that group's endpoints.

## Registry and roadmap disposition

Plan 061 is closed with operator-attested same-Network failover. Plan 062 is unblocked and active for integrated recovery qualification. Plan 063 remains proposed pending Plan 062 closure. Plan 054 remains independently active on its controlled live IRC-over-i2pd evidence gate; R002 remains blocked on public i2pr managed-app contracts.
