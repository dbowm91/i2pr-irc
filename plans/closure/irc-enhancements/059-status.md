# Plan 059 — M012-A Closure Status

Status: closed
Implementation commit: `16b08e0` — `feat(runtime): pace recovery and record upstream gaps`
Closure commit: `8f69467` — `docs(plans): close M012-A plan 059`
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Post-registration recovery traffic is paced without starving liveness | `CommandPacer` is shared by the production catalog and uses a FIFO async mutex at a fixed ten recovery commands per second. Registration action phases and desired JOIN restore use the gate; PING/PONG and online control paths bypass it. `recovery_slots_are_shared_by_clones_and_spaced` verifies shared slots and FIFO service. Existing `a_diagnostics_line_is_tagged_and_bounded` and `a_diagnostics_list_that_is_truncated_says_how_many_are_missing` restore 64 desired channels and pass within their bounded startup window. |
| Recovery order and no-chat-replay invariants remain intact | Existing action-phase integration keeps pre-join actions before JOINs, post-join actions afterward, and fallback recovery last. The pacer accepts only the setup frames at those existing call sites. `ambiguous_chat_is_not_replayed_into_replacement_generation`, reconnect integration, and network-boundary suites pass; normal client PRIVMSG/NOTICE is not queued into this scheduler. |
| Cross-Network fairness and bounds | The catalog injects one process-wide pacer. Tokio mutex waiter ordering gives FIFO slots across owners, with at most the bounded live Network count waiting (64); a cancellation drops its lock waiter. No background pacing task or retained user payload is created. The runtime recovery rate is a fixed safe default; no operator knob was introduced without a registered configuration contract. |
| Durable, bounded gap evidence | Schema v14 adds `connection_gaps` with a monotonically increasing per-Network sequence, optional monotonic duration, and `open`/`reconnected`/`interrupted` disposition. The Store prunes to 64 rows per Network. No wall-clock assertion, endpoint, or message payload is written. The v13 migration fixture and `every_supported_predecessor_schema_opens_and_reaches_the_current_version` cover the full migration chain. |
| Gap closure and restart uncertainty are truthful | The live disconnect-to-registration interval is measured by `Instant`; bounded owner-scoped tasks submit Store work without blocking reconnect progress. Registration success closes the row. On owner startup, an already queued Store request marks stale open rows interrupted with unknown duration before a later gap request can enter the worker queue. `an_upstream_that_hangs_up_is_replaced_far_inside_the_keepalive_interval` verifies durable closure after the next registration. |
| Store loss is distinct from history ingestion loss and exposed safely | Diagnostics report retained gap count, disposition, duration, and ledger failures on the bounded diagnostics list line. The separate failure counter does not modify `history_dropped`. The whole-process, truncation, stalled-Store, and environment/credential redaction suites pass. Store data remains inside the existing encrypted Store and typed worker boundary. |

## Security, recovery, and limits

Recovery setup frames are bounded by persisted channel/action ceilings and a process-wide fixed rate. Liveness/control writes remain direct. Generation cancellation releases a pacing wait; no non-idempotent chat is stored or replayed. Each owner has at most one active gap-writer task plus one startup projection task, both held by a `JoinSet` and canceled with the owner. The global maximum is therefore bounded by the existing supervised-Network ceiling.

Gap durations are process-local monotonic elapsed measurements. A process/owner restart cannot recover the original monotonic instant, so an open row becomes `interrupted` and its duration remains absent. A failed Store enqueue/commit is counted as a ledger failure and never mislabeled as history ingestion loss. The fixed pacing rate is not operator configurable in this plan; adding a setting requires a separately reviewed config contract and bounds.

## Verification

Passed on implementation tree `16b08e0`:

- `rtk cargo fmt --all -- --check`
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk sh scripts/verify.sh full`
- `rtk rustup run 1.88.0 sh scripts/verify.sh full`
- `rtk cargo test -p i2pr-irc-runtime --lib recovery_slots_are_shared_by_clones_and_spaced`
- `rtk cargo test -p i2pr-irc-runtime --test m005i_integration an_upstream_that_hangs_up_is_replaced_far_inside_the_keepalive_interval -- --exact`
- `rtk cargo test -p i2pr-irc-runtime --test m005h_diagnostics`
- `rtk cargo test -p i2pr-irc-store --test qualification connection_gap_ledger_is_ordered_bounded_and_uses_monotonic_durations`
- `rtk cargo test -p i2pr-irc-store --test qualification every_supported_predecessor_schema_opens_and_reaches_the_current_version -- --exact`
- `rtk git diff --check`

Both full verification scripts passed on the available macOS toolchains. Four pre-existing pinned EggChaos tests remained ignored by their annotations; the explicit pinned target was not run. No live i2pd router or deployed IRC2P/ILITA service was used. The independent M010 Plan 054 qualification remains outside this closure.

## Registry and next-plan disposition

Plan 059 closes M012-A. Plan 060 is promoted to `ready` after the Plan 059 dependency closed and the IRCv3 CHATHISTORY/BATCH specification review was recorded in Research 011: any production request must depend on the active server's negotiated `draft/chathistory` capability; the specification is still work in progress; full functionality depends on `batch`, `server-time`, and `message-tags`; server authorization and history retention remain endpoint-specific. This is protocol-design readiness only and does not assert that IRC2P or ILITA currently supports upstream catch-up. Plan 061 remains proposed behind 060 and its same-network equivalence research; Plan 062 remains gated on the M012 dispositions. Plan 054 remains independently active for controlled live IRC-over-i2pd evidence.
