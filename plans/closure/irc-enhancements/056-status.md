# Plan 056 — M011-B Closure Status

Status: closing
Implementation commit: `2ebf576` — `feat(runtime): implement detached channel activity policy`
Closure commit: pending
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Typed, bounded activity policy with safe migration defaults | `ChannelActivityPolicy` limits `detach_after_secs` to 1–86400; relay and reattach choices are enums. Schema v12 migrates v11 with all activity controls disabled. `channel_activity_policy_round_trips_and_unknown_channels_are_not_created` and `schema_eleven_migration_defaults_activity_controls_off` pass. |
| Durable updates through the owner and local Operator interface | Updates use typed controller/owner requests and commit before live application; ambiguous commits reconcile from Store. BouncerServ requires all three bounded options and exposes them in `CHANNEL STATUS`. `the_local_service_administers_presence_and_channel_policy` exercises set/status and persistence. |
| Monotonic inactivity timing bounded by desired channels | One owner interval drives deadlines held in a map keyed by desired channel; no per-channel or per-message tasks are spawned. Timeout invokes local detach without PART. `inactivity_detach_is_bounded_local_policy_and_never_parts_upstream` passes. |
| Casemap-correct human-readable matching and OTR exclusion | Matching uses negotiated casemapping and nick token boundaries. OTR fragments cannot trigger mention reattach or mention relay. `detached_channel_activity_indexes_follow_negotiated_casemapping`, `detached_mentions_use_irc_casemapping_and_nick_token_boundaries`, `a_detached_channel_auto_reattaches_on_one_human_readable_mention`, and `an_otr_fragment_cannot_trigger_automatic_mention_reattach` pass. |
| Detached relay and automatic reattach preserve upstream membership | `none`, `mentions`, and `all` apply only to eligible channel chat; automated reattach requires observed upstream membership and issues no JOIN/PART. `detached_relay_modes_only_release_eligible_channel_messages` and the timer/mention integration tests pass. |
| History/privacy correctness on reattach | Reattach projects JOIN/state and then delivers only privacy-eligible bounded backlog using each stable ClientId cursor. `a_detached_channel_stays_joined_upstream_and_keeps_collecting_history` proves missed retained events replay once and are not replayed again after cursor acknowledgment. No-history buffers return no backlog. |
| No new egress or live-service claim | Change is local policy over existing owner, Store, and BouncerServ paths. No upstream capability or deployed IRC2P/ILITA claim is made. The existing static boundary tests run in full verification. |

## Restart, privacy, and residual limits

Policy survives restart in the durable desired-channel record. Monotonic inactivity deadlines are rebuilt from process-local observations and therefore cannot claim that time elapsed while the process was stopped. Relay and reattach default off. Detached channels remain upstream-joined; timeout and mention automation do not issue PART/JOIN. OTR content remains opaque and cannot be used as human-readable mention text. Backlog replay follows the existing per-buffer history policy and per-ClientId cursor; no-history payloads are not retained for this purpose. Existing SQLite/WAL/backups physical-erasure limits remain as described in Plan 055.

The tests use deterministic local/fake upstreams. They do not establish deployed IRC2P/ILITA behavior or a live i2pd product path. Plan 054's independent live-service blocker is unchanged.

## Verification

Passed on implementation tree `2ebf576`:

- `rtk cargo fmt --all -- --check`
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk sh scripts/verify.sh full`
- `rtk rustup run 1.88.0 sh scripts/verify.sh full`
- Focused Plan 056 Store migration/roundtrip, BouncerServ parser and live policy, casemapping, timer detach, mention reattach, OTR exclusion, and detached-backlog cursor tests
- `rtk git diff --check`

Both full verification runs exited successfully, including all-feature workspace tests, repository privacy/network-boundary checks, and release fuzz smoke. Four pre-existing pinned EggChaos tests are ignored by their annotations; the explicit pinned qualification target was not run and is not claimed here. No real upstream server or live router qualification was performed.

## Registry and next-plan disposition

Plan 056 is closed. Plan 057 is promoted to ready: Plan 055 privacy gates, owner-observed parsed message events, casemapping, OTR detection, stable ClientId cursors, and authenticated bounded local control paths are present, and fresh source review found no M010/054 dependency. Plan 058 remains proposed pending 057. Plan 054 remains independently active pending controlled live IRC-over-i2pd product-path evidence.
