# Bouncer Core Corrective 005 Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/005-pre-m003-observed-membership-and-downstream-cap-corrective.md`

Prior closure: `plans/closure/bouncer-core/004-status.md` (Corrective 004 / C001)

Repository baseline reviewed: `4d6f9c6` (registry state before this corrective)

Implementation commits:

- `a2d93ae` — Separate desired, attempted, and observed channel membership

## Finding disposition

| Finding | Before | After | Evidence |
|---|---|---|---|
| C002-F1 configured JOIN promoted to observed membership | `mark_desired_joined()` copied `desired_channels` into `self_channels` the moment JOIN bytes were written, so the runtime reported channels it had not joined | writing a JOIN records a bounded generation-local `JoinAttempt::Pending` and nothing else; only a server-confirmed self JOIN adds observed membership | `a_written_desired_join_is_not_observed_membership`, `upstream_reaches_online_with_no_local_client`, `a_desired_channel_without_confirmation_is_never_projected` |
| C002-F2 join failure indistinguishable from success | no join-failure classification existed, so a refused join was silently indistinguishable from a successful one | `403`, `405`, `471`, `473`, `474`, `475`, `476` classify the attempt as `Rejected` after reply-shape validation, never create membership, and never mutate operator intent | `every_standard_join_failure_leaves_membership_absent`, `join_failure_accepts_the_bare_and_nick_prefixed_reply_shapes`, `malformed_and_unrelated_rejections_are_not_recorded`, `a_rejected_desired_join_is_never_projected_and_retried_by_a_new_generation` |
| C002-F3 downstream CAP negotiation did not gate registration | `CAP END` cleared no state, so `CAP LS` + NICK + USER emitted 001/005/366 with an unresolved negotiation round | registration is gated on NICK and USER *and* no outstanding negotiation; `CAP LS`/`REQ` start or retain it, `CAP END` ends it, late `CAP` cannot undo registration | `cap_ls_suspends_registration_until_cap_end`, `cap_end_before_registration_waits_for_both_nick_and_user`, `user_before_nick_registers_once_after_cap_end`, `cap_req_is_refused_locally_and_still_awaits_cap_end`, `downstream_cap_negotiation_holds_the_welcome_until_cap_end` |
| C002-F4 `353` visibility gate rejected `@` | only `=` and `*` were accepted, so a legal `353 … @ #secret` reply was silently dropped and secret-channel membership was lost | visibility accepts `=`, `*`, and `@`; it stays a reply-grammar field and is never consumed as a member PREFIX symbol | `names_visibility_accepts_equals_star_and_at`, `names_visibility_is_never_a_membership_prefix`, `unknown_names_visibility_is_ignored_without_corrupting_membership`, `secret_channel_names_with_at_visibility_reach_the_projection` |

`NetworkState::mark_desired_joined()` was removed rather than renamed, and `join_attempts` is private with accessor methods, so M003 code cannot reach a desired-to-observed promotion through the state API. `joined_channels()` remains observed-only.

## Required-behavior matrix

| # | Required behavior | Evidence | Result |
|---|---|---|---|
| 1 | a written JOIN is not observed membership | `a_written_desired_join_is_not_observed_membership` | pass |
| 2 | observed membership appears only from a self JOIN | `a_written_desired_join_is_not_observed_membership`, `self_kick_removes_observed_membership` | pass |
| 3 | each supported join-failure numeric leaves the channel unjoined and keeps desired intent | `every_standard_join_failure_leaves_membership_absent` | pass |
| 4 | a rejected join remains desired configuration, not an error to retry | `every_standard_join_failure_leaves_membership_absent`, `a_rejected_desired_join_is_never_projected_and_retried_by_a_new_generation` | pass |
| 5 | no downstream client receives a synthetic JOIN for an unconfirmed channel | `a_desired_channel_without_confirmation_is_never_projected`, `a_rejected_desired_join_is_never_projected_and_retried_by_a_new_generation` | pass |
| 6 | pending attempts and rejections are bounded, non-secret, and per generation | `join_attempts_are_bounded_and_reject_unrepresentable_targets`, `a_confirmed_join_clears_a_previous_rejection_and_casemaps_the_key` | pass |
| 7 | a fresh generation re-attempts desired channels after its own registration | `a_rejected_desired_join_is_never_projected_and_retried_by_a_new_generation`, `ambiguous_chat_is_not_replayed_into_replacement_generation` | pass |
| 8 | `CAP LS` + NICK/USER produces no welcome until `CAP END` | `cap_ls_suspends_registration_until_cap_end`, `downstream_cap_negotiation_holds_the_welcome_until_cap_end` | pass |
| 9 | `CAP REQ` gets a local NAK and still waits for `CAP END` | `cap_req_is_refused_locally_and_still_awaits_cap_end` | pass |
| 10 | no-CAP registration is unaffected | `registration_without_cap_negotiation_is_unaffected`, `single_client_vertical_registers_routes_and_answers_ping` | pass |
| 11 | repeated/late CAP messages are deterministic and bounded | `repeated_and_invalid_cap_commands_are_deterministic`, `late_cap_after_registration_cannot_unregister_the_client` | pass |
| 12 | downstream detach during CAP leaves upstream online and uninterrupted | `downstream_detach_during_cap_negotiation_leaves_upstream_online` | pass |
| 13 | post-registration CAP cannot alter upstream CAP state | `late_cap_after_registration_cannot_unregister_the_client` (no upstream `CAP` write is asserted in the unit test) | pass |
| 14 | advertised downstream capability set remains empty | `cap_ls_suspends_registration_until_cap_end`, `registration_projects_retained_state_truthfully` | pass |
| 15 | `353 … @ #channel` names are incorporated, projection is truthful | `names_visibility_accepts_equals_star_and_at`, `secret_channel_names_with_at_visibility_reach_the_projection` | pass |
| 16 | unknown visibility never corrupts membership | `unknown_names_visibility_is_ignored_without_corrupting_membership`, `names_visibility_is_never_a_membership_prefix` | pass |

## Join lifecycle

| phase | channel name (example) | membership | projection | attempt record |
| --- | --- | --- | --- | --- |
| configured | `#room` in operator config | absent | absent | none |
| JOIN written upstream | `#room` | absent | absent | `Pending` |
| server `JOIN #room` for our nick | `#room` | present | `001`, `005`, `JOIN`, topic/modes/names when observed | cleared |
| `403`/`471`/`…` for `#room` | `#room` | absent | absent | `Rejected(<numeric>)` |
| self `PART`/`KICK` | `#room` | absent | absent | cleared |
| generation replaced | `#room` | reset with the generation | reset | reset; `Pending` again after the next registration |

Nothing in this table is persisted. A `Pending` or `Rejected` record is a statement about one connection generation, which is why M003 may store operator intent durably but must never restore these records as membership.

## Registration state machine

| client sends | negotiating | registered | effect |
| --- | --- | --- | --- |
| `CAP LS 302` before NICK/USER | yes | no | local `CAP * LS :` |
| `CAP REQ :…` before NICK/USER | yes | no | local `CAP * NAK :…`; nothing forwarded upstream |
| `CAP END` before NICK/USER | no | no | waits for the remaining registration facts |
| NICK/USER with no CAP | no | on the second fact | exactly one `001` projection |
| NICK/USER while negotiating | yes | no | no `001`, no `005`, no channel projection |
| `CAP END` after NICK/USER | no | yes | one projection using current observed state |
| `CAP LS`/`LIST` after registration | no | unchanged | answered locally; registration is never undone |
| `CAP ACK`/`NAK` from a client | unchanged | unchanged | `410 … Invalid CAP subcommand` |

`CAP LIST` answers with the empty advertised set rather than `410 Invalid CAP subcommand`, because `LIST` is a defined subcommand and the bouncer's advertised set is empty; this is a registration-edge correction in the class of C002-F3, not a new capability.

## Join-failure classification

| reply shape | result |
| --- | --- |
| `:srv 403 #room :No such channel` | channel taken from the only parameter |
| `:srv 403 bot #room :No such channel` | channel taken from the last non-trailing parameter |
| `:srv 403 bot :No such channel` | not recorded; no channel-shaped target |
| `:srv 403 bot #room,other :…` | not recorded; not a channel token |
| `:srv 437 bot #room :…` | not recorded; network-specific numeric stays an ordinary event |
| reply naming a channel we never attempted | recorded as a bounded diagnostic only; still creates no membership |

Attempt keys use the negotiated casemapping, so a differently cased confirmation or rejection addresses the same record. The record ceiling is `MAX_CHANNELS`, shared with observed channel state.

## Verification commands and results

Executed on the final tree of this corrective (macOS, `rustc 1.89.0 (29483883e 2025-08-04)`, `cargo 1.89.0`):

| Command | Result |
|---|---|
| `./scripts/check-network-boundary.py` | pass (exit 0) |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass, no warnings |
| `cargo test --workspace --all-features --locked` | pass: 110 tests (core 6, wire 15, runtime 77, testkit 12), 0 failed |
| `./scripts/verify.sh full` (guard + fmt + clippy + tests + `scripts/fuzz-smoke.sh`) | pass (exit 0) |
| `rustup run 1.88.0 sh scripts/verify.sh full` | pass (exit 0) on the declared floor `rustc 1.88.0` |

Behavior changes required updating two existing regression tests, both because they encoded the old defect rather than an invariant: `upstream_reaches_online_with_no_local_client` now asserts membership is absent after the JOIN write and present only after the server's self JOIN, and the runtime test helper `register_client` now sends `CAP END` because a client that starts CAP negotiation must end it.

## Unresolved findings and severity

No C002 finding remains open. Known accepted limitations:

| item | severity | disposition |
| --- | --- | --- |
| `CAP LIST` answers an empty set while `CAP REQ` is NAKed; no downstream capability is implemented | low | intentional; the bouncer advertises nothing it cannot honor for downstream clients |
| a join failure is not retried within a generation | low | no infinite retry queue exists by design; the next generation re-attempts after its own registration |
| join-failure classification covers only the seven standard channel-failure numerics | low | network-specific numerics are deliberately left as ordinary events because nothing specified here can classify them safely |
| one client at a time and no downstream survival across reconnect | medium | unchanged from Corrective 004; M003 scope |
| no durable storage, production listener, concrete router adapter, or live-network qualification | medium | outside this corrective; M003/M004/M005 and R001 |

## M003 readiness decision

Corrective 005 is closed, so C002-F1 through C002-F4 no longer block persistence work. `plans/registry.md` moves Corrective 005 to closed and keeps M003 gated only on the Research 002 disposition.

The handoff boundary for M003, unchanged in kind and extended here: persistence may store the generation-owned state model and durable operator intent, but it must not restore pending or rejected join attempts as membership, must not reintroduce coupled upstream/downstream lifetime, must not assume mode state the runtime marked incomplete, and must not use persistence to paper over current-state ownership.