# Plan 058 — M011-D Closure Status

Status: closed
Implementation commit: `2bfdd13` — `test(runtime): integrate M011 privacy qualification`
Closure commit: pending
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Persistent, ephemeral, and no-history buffer modes compose consistently | `m011_three_buffer_privacy_matrix_survives_store_queries_without_crossing_profiles` exercises a persistent channel, ephemeral query, and no-history channel in one Journal/Store scenario. Persistent content survives restart and reaches FTS; ephemeral content appears only in the live process search/backlog and disappears with its cursor on restart; no-history content is neither durable nor searchable. |
| Stable client state does not cross profiles | The matrix creates two durable client identities, advances one ephemeral cursor, and verifies the other remains unchanged. Existing live integration tests also cover distinct authenticated client IDs, same-buffer independent cursors, and restart behavior. |
| Detached history, local watches, delay, and multiple clients integrate | `local_watch_emits_redacted_bounded_hit_metadata_and_skips_otr` detaches a channel for two sessions, delivers chat after a deterministic 150 ms fake upstream delay, waits for the owner's PONG fence, and verifies both clients receive only redacted watch metadata. The message is retained under persistent policy; live chat remains hidden while detached. No upstream JOIN/PART is generated. OTR content does not trigger a watch. |
| OTR stays opaque through storage, fanout, search, and restart | `persistent_otr_payload_is_retained_opaque_without_fts_derivation`, `otr_queries_fragments_and_whitespace_remain_opaque_across_tags_fanout_and_history`, and `encrypted_store_otr_transcript_fanout_history_and_restart_stay_opaque` verify ciphertext treatment, no FTS derivation, exact carriage, and restart behavior. |
| Upgrade chain and watch-rule defaults are safe | Store qualification includes `every_supported_predecessor_schema_opens_and_reaches_the_current_version`, the privacy/activity migration fixtures, and `watch_rules_round_trip_and_v12_migration_starts_empty`. New watch configuration is empty after migration; existing privacy defaults remain compatible. |
| Owner state and upstream capability behavior remain independent of local clients | `the_upstream_capability_fingerprint_is_downstream_client_independent`, `the_upstream_registration_is_identical_for_every_client_mix`, and stable generation/reconnect integration tests establish that attachment mixes do not alter upstream registration. The production API remains the typed I2P provider. |
| Failure, pressure, bounds, and egress controls | Full verification runs the queue-pressure, stalled-Store, reconnect churn, provider stall/release, partial I/O, delayed-release capacity, bounded watch burst/fairness, and static privacy/network-boundary guards. Store failures degrade history without creating a side queue; notification output uses existing bounded per-session queues and drop counters. |

## Invalid combinations and limits

The following claims are intentionally excluded: notification bodies or previews, durable notification backlogs or acknowledgements, replaying watch hits from stored history, interpreting OTR ciphertext, external notification delivery, live IRC2P/ILITA capability claims, and live router behavior. Watch configuration persists, while event matching state and ephemeral content are process-local. Notifications are redacted, immediate, and best effort. Persistent data may remain in SQLite WAL files, backups, or storage snapshots after logical deletion; no physical erasure is claimed. No-history is prospective for policy changes and prevents subsequent payload retention; existing purge behavior is bounded and represented by the Store's pending purge state.

The multi-client integration uses deterministic local/fake streams and a delayed byte release. It does not reproduce real I2P latency distributions or prove a deployed IRCd's capabilities. The independently active Plan 054 live product-path condition remains separate from M011 closure.

## Verification

Passed on implementation tree `2bfdd13`:

- `rtk cargo fmt --all -- --check`
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk sh scripts/verify.sh full`
- `rtk rustup run 1.88.0 sh scripts/verify.sh full`
- Focused history privacy matrix, detached/watch delayed-path integration, Store migration/retention qualification
- `rtk git diff --check`

Both full verification runs exited successfully on the available macOS environment. They include all-feature workspace tests, static privacy/network-boundary checks, and release fuzz smoke. Four pre-existing pinned EggChaos tests are ignored by their annotations; the explicit pinned qualification target was not run. Linux, a real i2pd router, and deployed IRC2P/ILITA servers were not tested.

## Registry and next-plan disposition

Plan 058 closes M011 after committed evidence for Plans 055–057 and the integrated matrix above. Plan 059 is promoted to ready after fresh source/dependency review: M011 is its only named predecessor; the single NetworkOwner, generation fencing, typed upstream queues, reconnect scheduler, bounded registration actions, current diagnostics, and I2P provider exist. The missing shared post-registration command pacing and explicit connection-gap ledger are the work Plan 059 is meant to implement, not blockers to readiness. No R002 or Plan 054 live-product evidence is required for that core-only work. Plan 060 remains proposed behind Plan 059 and its CHATHISTORY specification/server-support research gate; Plans 061–066 remain at their separately recorded gates. Plan 054 remains independently active for controlled live IRC-over-i2pd evidence.
