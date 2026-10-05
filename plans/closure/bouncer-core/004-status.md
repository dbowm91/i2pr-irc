# Bouncer Core Corrective 004 Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/004-m002-persistent-upstream-lifecycle-and-state-fidelity-corrective.md`

Corrective authority: `plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md`

Prior closure: `plans/closure/bouncer-core/002-status.md` (M002)

Repository baseline reviewed: `2d5683b` (registry state before this corrective)

Implementation commits:

- `0d30769` — Own upstream lifetime independently of local client attach
- `1dff97f` — Document the corrected network owner and downstream attachment

## Finding disposition

| Finding | Before | After | Evidence |
|---|---|---|---|
| C001-F1 upstream lifetime coupled to downstream attachment | `serve` connected upstream, then waited on `accept()` before registration, so no client meant no registered session | registration runs inside the generation with no client; `serve` never awaits an acceptor | `upstream_reaches_online_with_no_local_client` |
| C001-F2 downstream EOF/QUIT terminates the supervisor | generation returned `Ok(())` on EOF or client `QUIT`, which `serve` treated as terminal success | detach is a `DownstreamDisposition` handled as data inside the generation; only explicit stop or a generation failure exits it | `downstream_eof_detaches_only_and_keeps_the_generation`, `downstream_quit_never_becomes_upstream_quit`, `second_client_reattaches_the_same_generation_and_sees_detached_state` |
| C001-F3 lossy state synthesis | modes stored as bare letters, arguments discarded, membership prefixes stripped with a hard-coded `~&@%+`, only `CASEMAPPING` interpreted | bounded ISUPPORT model: `CHANTYPES` classifies targets, `PREFIX` replaces the mapping when advertised, `CHANMODES` classes decide argument consumption; incomplete state is marked and omitted | `advertised_prefix_replaces_the_hard_coded_assumption`, `symbols_outside_the_advertised_mapping_are_nicks`, `advertised_chantypes_change_live_classification`, `chanmode_arity_follows_advertised_classes`, `parameterised_modes_round_trip_truthfully` |
| C001-F4 static boundary proof omits the runtime | guard scanned only `core` and `wire` | guard scans `core`, `wire`, `runtime`, and `testkit` sources, manifests, build scripts, and dependency trees, with crate-scope positive controls | `scripts/check-network-boundary.py`; probe run reproduced below |
| C001-F5 stale planning text | roadmap described the runtime as a registration slice and treated M003 as eligible | roadmap, addendum, registry, and this record reconciled; M003 readiness is decided explicitly below | `plans/subsystems/bouncer-core-roadmap.md`, `plans/registry.md` |

## Acceptance criteria

| # | Criterion | Evidence | Result |
|---|---|---|---|
| 1 | Online and connected with zero local clients | `upstream_reaches_online_with_no_local_client` | pass |
| 2 | attachment not required for upstream registration | `upstream_reaches_online_with_no_local_client`; `LocalAcceptor` is only polled after registration | pass |
| 3 | EOF/QUIT does not terminate the generation or supervisor | `downstream_eof_detaches_only_and_keeps_the_generation`, `downstream_quit_never_becomes_upstream_quit` | pass |
| 4 | a later client attaches to the same generation with current bounded state | `second_client_reattaches_the_same_generation_and_sees_detached_state`, `isupport_values_reach_state_and_the_next_projection` | pass |
| 5 | liveness and state processing continue while detached | `liveness_deadline_reconnects_with_no_client_attached`, `upstream_reaches_online_with_no_local_client` | pass |
| 6 | only explicit stop deliberately emits upstream QUIT | `explicit_stop_is_the_only_path_that_sends_upstream_quit`, `downstream_quit_never_becomes_upstream_quit`, `stale_detached_session_cannot_affect_the_next_client` | pass |
| 7 | upstream-loss reconnect/no-replay still passes | `ambiguous_chat_is_not_replayed_into_replacement_generation`, `upstream_failure_while_detached_replaces_the_generation`, `upstream_failure_while_attached_terminates_that_client` | pass |
| 8 | PREFIX/CHANTYPES drive interpretation after advertisement | `advertised_prefix_replaces_the_hard_coded_assumption`, `symbols_outside_the_advertised_mapping_are_nicks`, `advertised_chantypes_change_live_classification` | pass |
| 9 | parameterized modes preserved truthfully or explicitly incomplete; no false 324 | `parameterised_modes_round_trip_truthfully`, `unknown_mode_letter_makes_the_snapshot_incomplete`, `missing_parameter_makes_the_snapshot_incomplete`, `incomplete_mode_state_omits_the_mode_projection` | pass |
| 10 | boundary guard covers runtime and its dependency tree with positive controls | `scripts/check-network-boundary.py`; live probe reported below | pass |
| 11 | queue/task counts bounded across repeated attach/detach | `repeated_attach_detach_cycles_stay_bounded` (100 cycles, generation 1, depths 0) | pass |
| 12 | full verification on the Rust 1.88 floor | `rustup run 1.88.0 sh scripts/verify.sh full` | pass |
| 13 | planning/docs no longer describe M003 as eligible before closure | `plans/registry.md`, `plans/subsystems/bouncer-core-roadmap.md` §4/§6/§12 | pass at closure |

No stop condition in the plan's §14 was reached: multi-client fanout, downstream survival across upstream reconnect, generic listeners, router integration, and persistence were all left out of scope, and the I2P-only invariant was never weakened.

## Owner and task lifecycle

```text
NetworkSupervisor::serve                        owns generations, backoff, cancellation
└── run_generation(upstream, generation, acceptor, stop)
    ├── upstream read half        owner loop    registration, liveness, observed state
    ├── upstream write half       owner loop    CAP/SASL + welcome (registration only)
    └── upstream writer task      JoinSet       bounded control(8) + normal(64) queues

    LocalAcceptor (zero or one at any instant)
    ├── accepted   -> DownstreamSession { read half, decoder, bounded queues }
    │                                   SessionWriter { exit signal, JoinHandle }
    └── detached   -> session dropped, writer task aborted and joined,
                     same generation continues serving upstream
```

Task ownership rules the implementation satisfies:

- Registration, liveness, observed state, and the upstream writer task are created by the generation and exist for the whole generation.
- Each downstream session owns its own read half, decoder, bounded queues, and writer task; the writer task is aborted and joined on every detach path, so a canceled session cannot outlive itself and never becomes a detached task.
- Generation exit drops the session, terminates its writer task, then either joins the upstream writer after one bounded final `QUIT` (explicit stop) or aborts it immediately (generation failure).
- The accept future is only created while detached and is not recreated while a retry delay is pending, so an acceptor with no waiting client cannot spin the owner.

## Attach, detach, and generation matrix

| event | downstream session | upstream generation | `ConnectionGeneration` | upstream bytes |
| --- | --- | --- | --- | --- |
| client accepted | attached | unaffected | unchanged | none |
| client registration completes | ready, state projected | unaffected | unchanged | none |
| client EOF | detached | continues | unchanged | none |
| client `QUIT` | detached | continues | unchanged | none |
| client prefix/framing/tag violation | detached | continues | unchanged | none |
| client queue overload | detached | continues | unchanged | none |
| client writer failure | detached | continues | unchanged | none |
| local accept failure | not attached | continues | unchanged | none |
| upstream failure | terminated with generation | discarded | incremented on next attempt | none |
| explicit stop | terminated | stopped | final | one bounded `QUIT` |

Upstream generation count is asserted unchanged across every local-only detach: `downstream_eof_detaches_only_and_keeps_the_generation`, `downstream_quit_never_becomes_upstream_quit`, `downstream_protocol_violation_detaches_only`, `second_client_reattaches_the_same_generation_and_sees_detached_state`, `client_backlog_never_starves_upstream_control_traffic`, and `repeated_attach_detach_cycles_stay_bounded` each assert generation 1 across their scenario. `explicit_stop_is_the_only_path_that_sends_upstream_quit` captures upstream writes through the provider fault controller and asserts exactly one `QUIT`, only on explicit stop; `downstream_quit_never_becomes_upstream_quit` and `stale_detached_session_cannot_affect_the_next_client` assert no upstream `QUIT` at all.

Explicit stop also raises a shutdown fence before its `QUIT`, so queued user traffic cannot reach the network after the fence (`explicit_stop_is_the_only_path_that_sends_upstream_quit`).

## No-client upstream evidence

| Property | Test |
|---|---|
| generation reaches Online with zero clients ever queued | `upstream_reaches_online_with_no_local_client` |
| server PING answered with no client attached | `upstream_reaches_online_with_no_local_client` |
| observed state accumulates while detached and reaches the next client | `upstream_reaches_online_with_no_local_client`, `second_client_reattaches_the_same_generation_and_sees_detached_state`, `isupport_values_reach_state_and_the_next_projection` |
| upstream failure replaces the generation with no client attached | `upstream_failure_while_detached_replaces_the_generation` |
| liveness probe sent, answered, and enforced with no client attached | `liveness_deadline_reconnects_with_no_client_attached` |
| 100 provider failures stay bounded and recover without a client | `one_hundred_provider_failures_remain_bounded_and_recover` |
| registration and CAP/SASL deadlines hold without a client | `registration_deadline_is_reported_and_stop_cancels_backoff`, `cap_sasl_phase_has_its_own_bounded_deadline`, `stop_cancels_in_progress_registration` |

## ISUPPORT fixture matrix

| advertised value | interpretation exercised | test | result |
| --- | --- | --- | --- |
| none (`PREFIX=(ov)@+`, `CHANMODES=beI,k,l,imnpst`, `CHANTYPES=#&` defaults) | documented conservative defaults used only until advertised | `default_prefix_is_bounded_and_validated`, `chanmode_arity_follows_advertised_classes`, `chantypes_are_bounded_and_classify_targets` | defaults bounded and validated |
| `PREFIX=(ov)@+` | only `@` and `+` are membership symbols | `symbols_outside_the_advertised_mapping_are_nicks` | `~Someone` stays part of the nick |
| `PREFIX=(qaohv)~&@%+` | full advertised mapping replaces the hard-coded set | `advertised_prefix_replaces_the_hard_coded_assumption`, `isupport_values_reach_state_and_the_next_projection` | all six symbols parsed and projected |
| malformed `PREFIX=(ov)@`, `()@`, oversized mapping | value rejected, previous mapping authoritative | `default_prefix_is_bounded_and_validated` | rejected |
| `CHANTYPES=#&` | `#`/`&` classify channel targets; `+`/`%` do not | `chantypes_are_bounded_and_classify_targets` | nicks and user-mode targets stay out of channel state |
| `CHANTYPES=#&+!` | live classification follows the advertised set | `advertised_chantypes_change_live_classification` | `+lobby` tracked, not rejected |
| `CHANMODES=beI,k,l,imnpst` | classes decide argument consumption | `parameterised_modes_round_trip_truthfully`, `incremental_parameterised_modes_merge_without_loss`, `missing_parameter_makes_the_snapshot_incomplete` | `+kl key 42` round-trips with arguments |
| custom `CHANMODES=b,k,l,imnpstR` | extra declared letter accepted | `chanmode_arity_follows_advertised_classes` | accepted |
| malformed `CHANMODES` (3 groups, duplicate letter) | value rejected, previous mapping authoritative | `chanmode_arity_follows_advertised_classes` | rejected |
| `CASEMAPPING=rfc1459` | folded comparison retained | `nick_part_quit_topic_and_desired_state_are_tracked` | renames match |

## Truthful and incomplete projection evidence

| situation | projection behavior | test |
| --- | --- | --- |
| complete retained state | welcome, ISUPPORT, JOIN, 332, 324 with arguments, 353 with advertised prefixes, 366 | `registration_projects_retained_state_truthfully` |
| undeclared mode letter (`+nq` against advertised `CHANMODES`) | snapshot marked incomplete, `324` omitted | `unknown_mode_letter_makes_the_snapshot_incomplete`, `incomplete_mode_state_omits_the_mode_projection` |
| required argument missing (`+kl key`) | snapshot incomplete, no `324` | `missing_parameter_makes_the_snapshot_incomplete` |
| authoritative `324` after an incomplete delta | completeness restored with the authoritative set | `authoritative_snapshot_restores_completeness` |
| membership change for an unknown member | member list marked incomplete, no `353`/`366` | `membership_mode_changes_update_or_invalidate_membership`, `incomplete_membership_omits_the_names_projection` |
| member ceiling exceeded | member list marked incomplete at the ceiling, never unbounded | `member_ceilings_invalidate_membership_truthfully` |
| topic larger than the local ceiling | topic omitted, not truncated in stored state | `oversized_topic_is_omitted_not_truncated` |
| membership mode applied to a known member | highest advertised symbol retained for projection | `membership_mode_changes_update_or_invalidate_membership` |

## Static runtime-boundary evidence

`scripts/check-network-boundary.py` now scans `core`, `wire`, `runtime`, and `testkit` sources, `Cargo.toml`, and `build.rs`, and checks each crate's normal, build, and dev dependency tree with `cargo tree --locked --target all -e all`. Positive controls run on every invocation and use the same predicates and the same crate scoping as the real scan, over a synthetic fixture tree:

- source predicate control (`use std::net::TcpStream;` must be detected);
- manifest predicate control (`reqwest = { version = "1" }` must be detected);
- dependency predicate control (`hyper v1.0.0` must be detected);
- runtime source-scope control (a fixture `crates/runtime/src/lib.rs` containing a forbidden token must be reported);
- runtime manifest-scope control (a fixture `crates/runtime/Cargo.toml` with a forbidden dependency must be reported);
- over-report control (a clean `crates/wire` fixture must produce no finding).

A live probe was also run against this repository: adding `use std::net::TcpStream;` to `crates/runtime/src/state.rs` produced `forbidden network ownership in .../crates/runtime/src/state.rs: std::net::Tcp` and exit status 1; restoring the file returned exit status 0. No generic resolver, generic upstream socket, proxy, HTTP client, SAM implementation, or router administration path was introduced.

## Existing M002 regression results

| pre-existing property | test | result |
| --- | --- | --- |
| single-network vertical: CAP 302, NAK, 433, ISUPPORT, 332, 324, 353, routing, tag stripping, chat upstream, PING/PONG | `single_client_vertical_registers_routes_and_answers_ping` | pass |
| configured SASL PLAIN completes with no secret in diagnostics | `configured_sasl_plain_completes_without_secret_diagnostics` | pass |
| configured SASL unavailable is terminal registration rejection | `configured_sasl_unavailable_is_a_terminal_registration_error` | pass |
| ambiguous chat is not replayed into the replacement generation; desired channels are re-joined | `ambiguous_chat_is_not_replayed_into_replacement_generation` | pass |
| stale-generation fencing of provider work | testkit stale-generation tests | pass |
| bounded queues and explicit overload | `control_queue_is_separate_and_normal_overflow_is_explicit`, `bounded_client_queues_report_overload_explicitly` | pass |
| control priority over a normal backlog | `ready_control_frame_precedes_normal_backlog`, `client_backlog_never_starves_upstream_control_traffic` | pass |
| bounded backoff, reconnect, and recovery | `backoff_is_bounded`, `one_hundred_provider_failures_remain_bounded_and_recover` | pass |
| configuration validation rejects injection and unbounded channel lists | `network_configuration_rejects_injection_and_unbounded_channels` | pass |
| secret redaction and replay classification | `secret_debug_is_redacted`, `replay_class_is_explicit` | pass |
| M001 wire/core/testkit suites | unchanged | pass |

## Repeated attach/detach resource bounds

`repeated_attach_detach_cycles_stay_bounded` performs 100 attach/detach cycles on a single generation with no client ever queuing upstream traffic. It asserts after every cycle that the generation is still 1, and at the end that cumulative attached sessions and cumulative detaches are both 100, that upstream normal queue depth is 0, that both downstream queue depths are 0, that the provider was asked for exactly one connection, and that upstream traffic still flows after the last cycle. A session's writer task is aborted and joined on every detach, so cycles accumulate no task, queue, or decoder state.

## Verification commands and results

Executed on the final tree of this corrective (macOS, `rustc 1.89.0 (29483883e 2025-08-04)`, `cargo 1.89.0`):

| Command | Result |
|---|---|
| `./scripts/check-network-boundary.py` | pass (exit 0) |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass, no warnings |
| `cargo test --workspace --all-features --locked` | pass: 87 tests (core 6, wire 15, runtime 54, testkit 12), 0 failed |
| `./scripts/verify.sh full` (guard + fmt + clippy + tests + `scripts/fuzz-smoke.sh`) | pass (exit 0) |
| `rustup run 1.88.0 sh scripts/verify.sh full` | pass (exit 0) on the declared floor `rustc 1.88.0 (6b00bc388 2025-06-23)` |

Note on tooling: the `rtk` binary on this machine is an unrelated FFI type-generation tool, so commands are recorded exactly as executed rather than with the `rtk` prefixes used in the 002 and 003 closure records.

## Unresolved findings and severity

No C001 finding remains open. The following are known, accepted limitations with severity recorded:

| item | severity | disposition |
| --- | --- | --- |
| one client at a time; a second concurrent client is not queued and multi-client fanout does not exist | medium | outside this corrective by design; M003 scope |
| a downstream session is not preserved across upstream reconnect; it is terminated with its generation | medium | explicitly allowed by the corrective; M003 scope |
| channel mode arguments are consumed positionally in advertised letter order, so a server that sends letters and arguments out of canonical order can yield an incomplete snapshot | low | fails closed (no `324`) rather than projecting a false mode string |
| `CHANMODES` before advertisement uses the documented community default classes, so a server with non-standard mode arity that never advertises `CHANMODES` yields an incomplete snapshot | low | fails closed; no false projection |
| a projection larger than the bounded client queue ends that client with an overload disposition instead of unbounded buffering | low | bounded and explicit; retained state stays with the generation |
| no durable storage, production listener, concrete router adapter, or live-network qualification | medium | declared outside the M002/corrective boundary; M003/M004/M005 and R001 |
| local accept errors are retried with a bounded delay and never escalate to a terminal supervisor error | low | keeps a local failure from destroying a live upstream session; no production acceptor exists yet |

## M003 readiness decision

Corrective 004 is closed, so C001-F1 through C001-F5 no longer block persistence work. The in-memory owner model is now correct: one generation owns upstream lifetime and observed state, downstream attachment is data, and the state representation is bounded, ISUPPORT-driven, and truthful or explicitly incomplete.

`plans/registry.md` therefore moves M003 from planning-blocked to planning-eligible and records that it remains unplanned — there is still no M003 implementation handoff, and M003 must not be treated as implementation-ready. M004 and M005 remain sequenced behind M003 and M004. Router R001 remains blocked by the canonical M005 dependency; R002 and R003 retain their separate public-capability and product conditions.

The handoff boundary for M003 is explicit: persistence may store the generation-owned state model, but it must not reintroduce coupled upstream/downstream lifetime, must not assume mode state that the runtime marked incomplete, and must not use persistence to paper over current-state ownership.
