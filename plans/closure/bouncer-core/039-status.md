# Bouncer Core M007-A / Plan 039 Closure — Phased Service Actions

Status: closed

Plan: `plans/implementation/bouncer-core/039-m007a-phased-service-actions-for-nonsasl-authentication.md`

## Implementation commit

- `b352a5c2c025acc561a2569fdbb9d3fe79a27380` — `feat: add phased registration service actions`

## Migration matrix

| Starting database | Migration result | Evidence |
|---|---|---|
| Schema 7 with an existing action | Schema 8; row and order survive with `post-join` phase | `schema_seven_actions_migrate_to_post_join`; seeded schema 7 fixture in `every_supported_predecessor_schema_opens_and_reaches_the_current_version` |
| Schema 6 with no action table | Schema 8; action list remains empty | `a_schema_six_database_reaches_schema_seven_with_an_empty_action_set` |
| Schemas 1–5 | Sequential migrations reach schema 8 | `every_supported_predecessor_schema_opens_and_reaches_the_current_version` |
| Fresh database | Schema 8 includes the constrained phase column | `fresh_database_creates_exactly_the_current_schema`; required-column open check |

Schema 8 restricts phase values to `pre-join`, `post-join`, and `fallback-recovery`. Existing
schema 7 rows receive the historical `post-join` behavior without rebuilding the action table.

## Phase execution and operator updates

`action_phases_bracket_join_and_recovery_follows_a_fallback_nick` observes this transcript:

```text
PRIVMSG NickServ :IDENTIFY pre-secret
JOIN #room
MODE bot_1 +B
PRIVMSG NickServ :RECOVER bot recovery-secret
```

Fallback recovery is skipped unless registration used a generated fallback nick and
`keep_nick` is enabled; `fallback_recovery_is_skipped_without_both_fallback_and_keep_nick`
covers preferred-nick registration and fallback registration with the policy disabled.
`a_reconnect_replays_the_action_sequence_intentionally` verifies setup actions replay on a
new generation. Phase-specific `ACTION SET` replaces only its named phase, and a single typed
controller request serializes load/merge/validation/persist so simultaneous updates from two
sessions do not overwrite each other (`phase_scoped_action_set_replaces_only_that_phase`,
`simultaneous_phase_updates_from_two_sessions_do_not_overwrite_each_other`).

## Security and recovery review

- The allowlist remains limited to service-targeted `PRIVMSG`/`NOTICE` and permitted self
  `MODE` frames. Raw commands and `NICK`, `JOIN`, `PART`, `QUIT`, `CAP`, `AUTHENTICATE`, and
  `OPER` are structurally unavailable; `no_forbidden_command_can_be_reached_through_the_action_surface`
  and the parser action matrix cover refusals.
- Stored text remains secret-classified. Debug redacts it; `ACTION STATUS` reports only total
  and per-phase counts; diagnostics and config snapshots contain counts only. Config snapshot
  v2 round-trips per-phase counts and never exports action payloads.
- Action errors do not echo text. `an_action_status_reports_a_count_and_never_a_payload`,
  `an_action_refusal_names_the_problem_without_echoing_the_text`, and
  `config_snapshot_round_trips_phase_counts_without_action_payloads` provide evidence.
- A generation interrupted during setup is not resumed at an action index. The next
  successful generation starts the applicable setup phases again; downstream user traffic
  remains outside this replay path.
- No service-prose parsing, general perform strings, arbitrary network access, or new egress
  authority was added.

## Verification

- `cargo fmt --all -- --check`: passed on the final tree.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: passed on
  the final tree.
- `scripts/verify.sh full`: passed on the final tree, including workspace tests and fuzz smoke.
- Rust 1.88 full workspace format, Clippy, and test checks passed. After the final serialized
  phase-update addition, Rust 1.88 format and Clippy passed, and the complete `m005h_diagnostics`
  and store `qualification` integration suites passed again (26 and 73 tests).
- `git diff --check`: passed before the implementation commit.

The first workspace test attempt encountered the existing timing-sensitive
`retention_removes_what_a_search_can_find` failure. Its isolated rerun passed, and both the
final current-toolchain full verification and Rust 1.88 full workspace run passed.

## Findings and disposition

No unresolved Plan 039 finding remains. M007-A is closed. Plan 040's hard dependency is
discharged; Plan 040 is ready to proceed and is now active. Plan 041 remains blocked on Plan
040 closure.
