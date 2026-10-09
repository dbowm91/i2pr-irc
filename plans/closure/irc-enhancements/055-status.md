# Plan 055 — M011-A Closure Status

Status: closed
Implementation commit: `eada883` — `feat(runtime): implement per-buffer history privacy`
Closure commit: `a604e51` — `docs(plans): close IRC privacy retention plan 055`
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Stable per-buffer typed policy, legacy persistent default, and schema migration | Store policy model and schema v9-v11 migrations; `every_supported_predecessor_schema_opens_and_reaches_the_current_version`, `fresh_database_creates_exactly_the_current_schema`, and `a_schema_four_database_reaches_the_current_schema_with_policies_disabled`. Old profiles remain persistent when no override row exists. |
| No-history policy gate before durable payload and FTS writes | Owner suppresses enqueue for known no-history buffers, and journal/store recheck policy before durable writes; `no_history_policy_prevents_durable_event_and_search_index_writes` and `the_local_service_administers_presence_and_channel_policy`. |
| Bounded ephemeral retention, process-local cursors/search, restart loss, and zeroization | Journal ring is capped at 512 events, 1 MiB, and 128 events per buffer; eviction and drop zeroize retained payload/search fields. `ephemeral_history_is_bounded_memory_only_and_lost_on_journal_restart` and the integrated history suite pass. |
| Persistent age/event/byte ceilings and consistent FTS/reference cleanup | Append and worker cleanup enforce ceilings; `persistent_event_ceiling_prunes_events_and_search_rows_together`, `persistent_age_ceiling_excludes_expired_events_and_search_rows`, and `retention_leaves_no_index_row_behind`. |
| Bounded stricter-mode purge that cannot be relaxed before completion | Purge advances in 4096-row transactions and resumes through store-worker idle/request progress; `pending_privacy_purge_cannot_be_cleared_by_policy_relaxation` proves a partially completed purge remains hidden and cannot be relaxed. |
| Query, cursor, marker, CHATHISTORY, SEARCH, and detached-channel consistency | Runtime/store history, CHATHISTORY, SEARCH, retention, cursor, read-marker, and detached-policy suites pass. `a_search_spans_several_buffers_of_one_network`, `a_chathistory_query_and_the_legacy_backlog_cover_disjoint_history`, `read_marker_and_cursor_survive_retention_and_clamp_monotonically`, and `a_detached_channel_stays_joined_upstream_and_keeps_collecting_history` cover key paths. |
| OTR remains opaque and excluded from derived search fields | Schema v11 adds `history_events.search_indexed` and verifies marker/FTS agreement; `persistent_otr_payload_is_retained_opaque_without_fts_derivation` and `encrypted_store_otr_transcript_fanout_history_and_restart_stay_opaque` pass. |
| Local Operator policy controls and correct channel/query identity | `history status` and `history set` route through typed controller/owner messages; `the_local_service_administers_presence_and_channel_policy` covers the live local control path. `direct_messages_resolve_to_the_peer_buffer` covers direct-message peer identity. |
| No new network authority, secret exposure, or false live-service claim | Existing static network-boundary and environment-identifier qualifications pass. This plan changes local storage and policy mediation only; no I2P service compatibility claim is made. |

## Restart, privacy, and residual limits

Persistent remains the inherited policy. Ephemeral content is process-local and is lost on process exit. No-history avoids durable payload/index writes. Logical purge removes rows, FTS entries, cursors, and markers in bounded transactions; it cannot promise physical erasure from SQLite WAL files, backups, snapshots, or storage-device remanence. OTR ciphertext may be retained and replayed as opaque payload, but its body is not indexed. A storage-policy read/write failure does not queue message content elsewhere or change live IRC delivery.

The current qualification uses deterministic fake upstreams and local Store instances. It does not establish deployed IRC2P/ILITA server behavior or a live i2pd path; those claims remain outside Plan 055. The independent M010 Plan 054 live-service blocker is unchanged.

## Verification

Passed on the final implementation tree at `eada883`:

- `cargo fmt --all -- --check` (through the full verification script)
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` (through the full verification script)
- `sh scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`
- `cargo test -p i2pr-irc-store --test qualification every_supported_predecessor_schema_opens_and_reaches_the_current_version`
- `cargo test -p i2pr-irc-store --test qualification schema_seven_actions_migrate_to_post_join -- --exact`
- `cargo test -p i2pr-irc-runtime --test m005f_protocol_polish encrypted_store_otr_transcript_fanout_history_and_restart_stay_opaque -- --exact`
- Focused ephemeral-history, local BouncerServ policy, direct-message identity, and multi-buffer search tests

Both full runs completed successfully, including workspace tests, static privacy/network-boundary qualification, and release fuzz smoke. Four existing pinned EggChaos tests remained ignored as documented by their test annotations; they require the explicit pinned qualification target and were not claimed as run.

## Registry and next-plan disposition

Plan 055 is closed. Plan 056 is promoted to `ready`: Plan 055 is closed, and its named prerequisite M005 detached-channel owner/projection behavior remains covered by the existing `m005b_detached_policy` suite. Plans 057 and 058 remain dependency-gated on 056 and 057 respectively. Plan 054 remains independently active and blocked on controlled live IRC-over-i2pd product-path evidence; that does not block this core-only M011 successor.
