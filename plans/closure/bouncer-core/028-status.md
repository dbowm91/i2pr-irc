# Plan 028 — M005-I Integrated Mature-Bouncer Qualification and M005 Closure

Closed 2026-10-07. Outcome: **M005 closed. Three production defects found and fixed; the
finding Plan 027 carried in is withdrawn as a fixture defect; one new design defect found in
the connect rate limiter. No unresolved high-severity finding remains.**

## 1. Why this plan found things eight per-subsystem suites had not

Every M005 plan qualified its own subsystem in isolation, and every one of them closed with
its own invariants satisfied. The residual risk at milestone closure is not "is a plan's
invariant true" — it is "are the subsystems consistent with each other". Three defects of
exactly that kind were live in the tree when this plan started:

| Defect | Why per-subsystem testing could not see it |
|---|---|
| The connect rate limiter could hang | `ReconnectScheduler` was tested against in-flight ceilings and burst arithmetic, never against "what wakes a waiter". No test ever had a waiter blocked on the *token* gate with in-flight capacity free. |
| `ControlSnapshot` answered from memory | The controller's tests all issued a control-plane mutation before reading, so `commit()` always refreshed the snapshot first and the staleness was invisible. |
| A promised table was not required at open | The open-path test dropped `history_events` — a table the M001-M002 migration path touches. Nothing dropped a table only schema 7 introduced. |

Each is a case where the evidence existed and pointed at the wrong thing. That is the
argument for a qualification plan that runs the whole product, and it is why this plan's
work was not a formality.

## 2. Implementation commit ranges for Plans 020-027

| Plan | Implementation | Closure |
|---|---|---|
| 020 — M005-A runtime control and admission | `c79acef` | `a722a22` |
| 021 — M005-B durable detached-channel policy | `dcf608d` | `2436f96` |
| 022 — M005-C presence and preferred-nick policy | `5087280` | `aad9198` |
| 023 — M005-D bouncer-networks and administration | `de477d5` | `c8e9a1a` |
| 024 — M005-E indexed search and CHATHISTORY | `41bd2a2` | `a1d4c86` |
| 025 — M005-F downstream IRCv3 polish | `3fce25a` | `63b10db` |
| 026 — M005-G member-state mediation | `1d45231` | `f9ad905` |
| 027 — M005-H diagnostics, config, actions | `d3d604f`, `83e9f01`, `7b1c9f9`, `dc39c96` | `d604953` |
| 028 — this plan | `30b4230`, `6c9168d` | this record |

Research authority: `44c65a7`. Milestone decomposition: `ac1670a`.

## 3. The carried-in finding, resolved rather than re-measured

Plan 027 recorded that "a generation teardown takes about two minutes to be noticed",
measured at 120.9 s against a `CONNECT_TIMEOUT` of 120 s, and carried it into this plan as
the largest single operational gap of Plans 020-027.

**The finding is withdrawn.** The measurement was sound and the conclusion drawn from it was
not a property of the bouncer at all.

`run_generation` breaks on `count == 0` from the upstream read, so a server that hangs up
ends the generation immediately. Nothing about the 120 s was the owner waiting. It was
`LIVENESS_DEADLINE` (120 s) firing in a test whose generation was never actually ended:
`Runtime::plain_peer` discarded the `FaultController` and `drive_registration` registered
every generation as `upstreams_closable.push(None)`, so `drop_generation` was an
`if let Some(..)` that matched nothing. The test then blocked in `next_peer()` until the
keepalive deadline ended the generation for it, and asserted its real subject — replay on
reconnect — against that accidental reconnect.

A second defect sat underneath it, in the fixture itself:
`FaultController::close_write` woke `read_wakers[side]` and no writer waker, where the peer
observing end-of-file is side `1 - side` and the closing side's own parked writer is also
owed a wake. Even a working `drop_generation` would have stalled until an unrelated event
touched the connection. `close_write` and `poll_shutdown` now share `close_write_half`.

| | Before | After |
|---|---|---|
| `m005h_diagnostics` suite wall clock | 121.4 s | 6.2 s |
| What the test drops | nothing | the generation it means to |
| What the 120.9 s measured | the keepalive timer | — |

The three testkit tests pin the wake contract in both directions. `drop_generation` now
closes the *peer's* write half ("the server hung up") and panics rather than silently
no-opping. `an_upstream_that_hangs_up_is_replaced_far_inside_the_keepalive_interval` in
`m005i_integration.rs` is the standing qualification: it ends a generation the way a server
does and requires a replacement connection well inside `LIVENESS_INTERVAL`.

The `adverse.rs` startup-herd flake carried alongside it was a separate race: the ledger was
read in the same instant the last owner task joined, which races the store worker draining
one queued request. Settle assertions now wait a bounded 5 s for their own premise. 0
failures in 25 consecutive runs, against roughly 1 in 8 before.

## 4. Defects found and fixed by this plan

### 4.1 The rate limiter could hang (high severity)

`ReconnectScheduler::acquire` has two gates. In-flight capacity frees when a permit is
dropped, and `release()` calls `notify_waiters`. The token gate frees on a clock, and
**nothing notifies for a clock** — the bucket is refilled lazily, when some waiter re-checks
it. A waiter blocked only on tokens therefore parked on a notification that only an unrelated
event would ever send.

On a cold start of more Networks than `MAX_CONNECT_BURST` (4), that is every Network past
the burst, and nothing else will ever touch the queue. The mechanism built to stop a startup
herd from becoming a simultaneous connect would instead leave those Networks permanently
unconnected — a silent failure, because each of them is waiting politely rather than
reporting an error.

`token_due_in` gives that waiter its own deadline, derived from the same rate the lazy refill
uses rather than a second independent notion of how long a token takes. Evidence:
`a_waiter_blocked_only_on_the_token_gate_is_admitted_when_its_token_is_due`, confirmed to
**fail** against the unfixed scheduler, and
`the_token_gate_holds_the_start_rate_even_though_waiters_wake_on_time`, which fails if the
fix degenerates into ignoring the rate.

### 4.2 `ControlSnapshot` answered from memory (high severity)

`publish()` ran only from `commit()`, which fires on control-plane mutations. Every
owner-owned field in a `ControlNetwork` — phase, attached sessions, advertisement — belongs
to a task this controller does not drive, and none of them changes because a control-plane
edit happened.

Measured directly: a Network with two live sessions and phase `online` reported
`attached=0 phase=idle` indefinitely, and answered `2` / `online` only after a `create` on
an unrelated Network. Every `BOUNCER NET` and `bouncer-networks LIST` therefore told an
Operator that nobody was connected.

Plans 025 and 026 had already established the principle — the published snapshot is not
authoritative for owner state, which is why `advertisement()` and `diagnostics()` read the
live owner — and had routed one field around it. The fix applies it to the snapshot itself:
`Status` recomputes before replying, and the controller republishes after every dispatched
request so `subscribe_status` watchers also see owner-side movement.

### 4.3 A promised table was not required at open (medium severity)

`registration_actions`, `clients` and `network_secrets` were absent from `REQUIRED_TABLES`.
A database declaring the current version without them opened successfully and failed later —
every stored credential unreadable, every action replay refused. `user_version` is a header,
not a proof. `every_promised_table_is_required_at_open` now walks the whole promised set.

## 5. Requirement-to-evidence inventory, Plans 020-027

Evidence is a named test that exists in the tree at closure. The per-plan closure records
carry the full matrices; this is the milestone-level index.

### Process ownership and control (020)

| Requirement | Evidence |
|---|---|
| one RuntimeController; one live owner per Network | `m005i: every_owner_is_released_when_the_process_stops`, `several_networks_reconnect_together_and_keep_one_owner_each` |
| transfer preserves SessionId/ClientId/decoder bytes | `m005a_controller_admission`: `a_command_sent_in_the_same_read_as_registration_is_not_lost`, `a_bound_client_keeps_one_identity_across_the_transfer` |
| dynamic add/change/delete leaves the graph converged | `m005i: deleting_a_network_stops_its_owner_and_leaves_no_ghost`, `a_configuration_change_replaces_the_owner_and_converges` |
| shutdown owns and joins every task | `m005i: every_owner_is_released_when_the_process_stops`; `adverse: a_startup_herd_at_the_ceiling_never_exceeds_the_connect_ceiling` |

### Detached channels (021)

| Requirement | Evidence |
|---|---|
| upstream membership retained, no upstream PART either way | `m005b: a_detached_channel_stays_joined_upstream_and_keeps_collecting_history`, `reattaching_never_sends_an_upstream_part_or_leaves_durable_intent`, `an_ordinary_part_still_leaves_upstream_and_forgets_desired_intent` |
| reattach projection truthful, backlog not duplicated | `m005b: reattaching_projects_truthful_state_before_any_history` |
| decision survives restart | `m005i: a_restart_reconstructs_durable_policy_and_nothing_else`; `store: a_restart_persists_desired_state_and_no_live_state` |

### Presence and nick policy (022)

| Requirement | Evidence |
|---|---|
| aggregation over sessions, not sockets | `m005c: a_passive_session_does_not_clear_auto_away`, `an_explicit_manual_away_outranks_every_session_count` |
| bounded deterministic 433 fallback | `m005c: the_fallback_sequence_is_bounded_deterministic_and_distinct` |
| no environment-derived identity | `m005c: no_host_or_environment_value_can_reach_a_nick_or_an_away_message`; `privacy.rs` |
| reclaim bounded and generation-fenced | `m005c: reclaim_writes_are_capped_per_generation`, `a_replaced_generations_reclaim_state_cannot_act_on_its_replacement` |

### Bouncer control (023)

| Requirement | Evidence |
|---|---|
| BIND/list/mutation/notify transcripts | `m005d_bouncer_networks.rs` (28 tests) |
| BouncerServ typed operations | `m005h_diagnostics.rs` (21); `runtime --lib` action-matrix module |
| no raw network escape hatch | `m005d` attribute-disposition tests; `m005h: no_forbidden_command_can_be_reached_through_the_action_surface` |

### History and search (024)

| Requirement | Evidence |
|---|---|
| indexed search, bounded text/scope/results | `m005e_search_history.rs` (15); `store: search_bounds_are_enforced_before_any_database_work` |
| reference lookups resolve outside the window | `m005e`; `store: several_events_sharing_a_timestamp_resolve_deterministically` |
| index never outlives a retained row | `store: retention_leaves_no_index_row_behind` |

### IRCv3 (025, 026)

| Requirement | Evidence |
|---|---|
| per-session mediation, no cross-client effect | `m005i: clients_with_disjoint_capabilities_do_not_affect_each_other`; `m005f: a_time_tag_reaches_only_the_session_that_negotiated_server_time` |
| conditional on upstream acknowledgement | `m005g_member_state.rs` (14) |
| deferred set is reviewable, not omitted | `m005g` deferred-capability test |
| upstream request fingerprint is client-independent | `m005c: the_upstream_capability_fingerprint_does_not_depend_on_attached_clients` |

### Operator ergonomics (027)

| Requirement | Evidence |
|---|---|
| diagnostic correctness, structural redaction | `m005h` 11 unit + integration; `m005i: no_operator_surface_can_be_made_to_print_a_stored_credential` |
| config round trip and failure | `m005h: an_import_that_names_a_different_network_under_one_identity_stops_without_writing_it`, `an_import_does_not_erase_a_stored_credential` |
| registration actions and replay semantics | `m005h: a_stored_action_is_replayed_after_a_successful_registration`, `a_reconnect_replays_the_action_sequence_intentionally` |

## 6. Final CAP matrix

`downstream_supported()` is the advertised set; the conditional column is what the upstream
actually acknowledged.

| Capability | Advertised | Conditional on upstream | Mediated per session |
|---|---|---|---|
| `draft/chathistory`, `draft/read-marker` | yes | no | yes |
| `soju.im/search` | yes | no | yes |
| `soju.im/bouncer-networks`, `-notify` | yes | no | yes |
| `message-tags`, `batch`, `labeled-response` | yes | no | yes |
| `server-time`, `standard-replies`, `cap-notify` | yes | no | yes |
| `draft/no-implicit-names` | yes | no | yes |
| `draft/pre-away` | yes | no | yes |
| `echo-message` | yes | **yes** | yes |
| `extended-join`, `account-notify`, `away-notify`, `multi-prefix`, `setname` | yes | **yes** | yes |
| `account-tag`, `chghost`, `invite-notify`, `extended-monitor` | **never** | — | deferred, with a stated reason each |

`CAP REQ` is all-or-nothing: a request naming one unimplemented capability is NAKed as a
whole, so a session is never left guessing which half took effect.

## 7. Control-session ownership matrix

| Actor | Owns | Must not | Evidence |
|---|---|---|---|
| `RuntimeController` | every owner, durable records, revision, one bounded request queue | be driven by a client task | `every_owner_is_released_when_the_process_stops` |
| `RuntimeControlHandle` | a bounded sender, a stop signal, a status receiver | reach storage, a catalog, or a supervisor | `m005a: the_runtime_controller_exposes_no_dial_or_listen_operation`, `every_control_clone_reads_the_same_snapshot` |
| `NetworkOwner` | generation, bound sessions, gauges, diagnostics source | gain a second upstream owner | `several_networks_reconnect_together_and_keep_one_owner_each` |
| `DownstreamAdmission` | one accepted socket through registration | touch storage or upstream | `m005a: an_unreachable_selection_opens_no_upstream_connection` |
| `BouncerServ` | typed local administration | execute arbitrary raw IRC or host commands | `no_forbidden_command_can_be_reached_through_the_action_surface` |

## 8. Bouncer-networks I2P profile matrix

`host` is a typed `I2pEndpoint` or it is nothing. No URL parsing, no port extraction, no TLS
material, no resolver path.

| Attribute | Disposition |
|---|---|
| `host` | typed `I2pEndpoint`; `irc.example.org:6697` and `https://irc.example.org` are refused on shape alone |
| `port`, `tls`, `pass` | recognised and refused as "not supported" |
| `state`, `error` | refused as read-only |
| anything else | "unknown attribute" — nothing is silently dropped |
| clearnet destination | refused before any network authority is acquired |

## 9. Migration and restart matrix

Every predecessor version a supported release could have left on disk opens and reaches
schema 7 in one `open`, arriving at the exact promised table set
(`every_supported_predecessor_schema_opens_and_reaches_the_current_version`).

| From → to | Fixture | Preserves | Rollback proven |
|---|---|---|---|
| 1 → 2 | `create_v1_database` | event ids, cursors, read markers | yes |
| 2 → 3 | `create_v2_database` | display name, reply-safety | yes |
| 3 → 4 | `create_v3_database` | desired membership, attached | added by `a_schema_three…` family |
| 4 → 5 | `create_v4_database` (seeded) | Network, nick, channels; **both policies disabled** | added by this plan |
| 5 → 6 | `create_v5_database` (seeded) | history, FTS backfill, effective time | added by this plan |
| 6 → 7 | `create_v6_database` (seeded, backfilled) | history untouched; **no actions invented** | added by this plan |

`schema_v6()` and `create_v6_database` are real declarations rather than a newer version
subtracted. The v6 fixture runs the *same* backfills migration 6 runs, so the matrix is not
testing a shape that migration never produces
(`the_schema_six_fixture_carries_the_state_its_migration_derived`).

The "policies disabled" row is the one that matters most. An upgraded binary that defaulted
either presence policy on would start sending `AWAY` and reclaiming nicks upstream that the
Operator never asked for, and nothing in the data would look wrong: the flags are valid, the
rows are intact, the bouncer connects. Only the default is evidence, so the test asserts it
and then proves the column is genuinely settable afterwards.

Restart state is asserted as an **exact column set**, not a scan for values
(`a_restart_persists_desired_state_and_no_live_state`). The failure being guarded against is
not "a stale nick was stored" but "a column exists that could hold live state at all" — a
column holding nothing today is a persistence surface a later change can fill.

Intermediate development schemas 3-6 are all retained and tested. No release shipped between
M004 and M005, but the fixtures cost nothing to keep and the chain test would otherwise only
prove the two released endpoints.

## 10. Anonymity and redaction matrix

| Surface | Endpoint | SASL value | SASL name | Action payload | Path |
|---|---|---|---|---|---|
| `DIAG` process half | absent | absent | absent | absent | absent |
| `DIAG NETWORK` | absent | absent | absent | absent | absent |
| `CONFIG EXPORT` | **present** | absent | absent | absent | absent |
| `ACTION STATUS` | absent | absent | absent | absent | absent |

The endpoint row is a decision, not an omission. `BOUNCER NET` withholds an I2P destination
because it renders into an IRC-visible frame every session that negotiated the capability can
read; `CONFIG EXPORT` includes it because an export without the identity of the Network being
moved is not an export. The difference is who holds the output.

The guarantee is **structural**: no report type has a field a secret could be read out of,
which is stronger than a redaction pass.
`no_operator_surface_can_be_made_to_print_a_stored_credential` asks all four surfaces at once
against one credentialed Network — a per-surface test proves each is safe alone, and this
proves that asking all of them of the same running bouncer surfaces nothing through an
interaction between them.

Requalified from M004, unchanged: static no-generic-DNS/TCP boundary
(`scripts/check-network-boundary.py`, clean); no DCC or direct path; CTCP metadata fingerprint
fixed; client tag policy fixed; poisoned `USER`/`LOGNAME`/`HOSTNAME`/`HOME`/`TMPDIR`/`PATH`
sentinels absent (`privacy.rs`, 15 tests).

## 11. Resource and performance observations

Recorded, not gated. The plan explicitly forbids inventing benchmark thresholds without a
measured baseline, so these are readings that would move if something structural regressed.

| Observation | Reading |
|---|---|
| `m005i_integration` full suite | 9 campaigns, 8.1 s |
| six live Networks through teardown | owners released, no Network outlives its owner |
| twelve clients on one Network | all served, all released to zero on drop |
| four Networks losing upstream together | four replacement generations, one owner each |
| admission ceilings | `MAX_IN_FLIGHT_CONNECTS` 4, `MAX_CONNECT_BURST` 4, `CONNECT_TOKEN_INTERVAL` 2 s |
| session ceiling | `MAX_SESSIONS_PER_NETWORK` 64 |
| control-queue / diagnostic bound | `MAX_DIAGNOSTIC_LINES` = `CONTROL_QUEUE_CAPACITY` (8) |
| store tables | 10, each now required at open |
| process test suites | 33 suites, all passing |

The adverse-scale campaigns remain in `adverse.rs` at the supervised ceiling with the
`ResourceLedger` reading peak and returning to baseline; this plan did not restate them at a
smaller size, because a smaller fleet proves less and the ledger assertions are the ones that
matter.

## 12. Commands executed and results

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace --all-features` | pass — 33 suites, 0 failures |
| `scripts/check-network-boundary.py` | pass (exit 0) |
| `rustup run 1.88.0 cargo fmt --all -- --check` | pass (MSRV) |
| `cargo test -p i2pr-irc-store` | pass — 14 unit + 72 qualification (9 new) |
| `cargo test -p i2pr-irc-runtime --test m005i_integration` | pass — 9 campaigns, 8.1 s |
| `cargo test -p i2pr-irc-runtime --test m005h_diagnostics` | pass — 21, down from 121.4 s |
| `cargo test -p i2pr-irc-runtime --test reconnect_budget` | pass — 19 (2 new) |
| `adverse` startup-herd, 25 consecutive runs | 0 failures |
| token-gate test against the unfixed scheduler | **fails**, confirming it is a real regression test |
| `scripts/verify.sh quick` | pass (exit 0) |
| `scripts/verify.sh full` | pass (exit 0) |
| `rustup run 1.88.0 sh scripts/verify.sh full` | pass (exit 0) |

## 13. Unresolved findings

| Finding | Severity | Disposition |
|---|---|---|
| Connect rate limiter could hang a rate-limited waiter | high | **fixed** (§4.1) |
| `ControlSnapshot` stale for all owner-owned fields | high | **fixed** (§4.2) |
| Promised tables not required at open | medium | **fixed** (§4.3) |
| Plan 027's ~120 s teardown finding | — | **withdrawn** — fixture defect, not bouncer behaviour (§3) |
| `adverse.rs` startup-herd flake | low | **fixed** — settle assertions now wait a bounded 5 s |
| Whole-process `DIAG` reports at most two Networks | low | **stated boundary**, unchanged from Plan 027; a consequence of the control-queue bound, and `DIAG NETWORK` always delivers one Network in full |
| Configuration import is not transactional across Networks | low | **stated boundary**, permitted by the plan's own fallback clause; nothing is written until the whole snapshot validates, which is a property of the type |

### Two evidence citations in earlier records do not resolve

Work package A checked every test name this record and the Plans 020-027 records cite. Two do
not exist under the names given:

| Cited as | Cited in | Actually evidenced by |
|---|---|---|
| `the_control_handle_carries_no_store_and_no_catalog` | Plan 020 closure | `the_runtime_controller_exposes_no_dial_or_listen_operation`, `every_control_clone_reads_the_same_snapshot`; and structurally by `RuntimeControlHandle` holding only `requests`, `status` and `stop` |
| `neither_policy_direction_writes_an_upstream_part` | Plan 021 closure | `a_detached_channel_stays_joined_upstream_and_keeps_collecting_history` (detach), `reattaching_never_sends_an_upstream_part_or_leaves_durable_intent` (reattach), `an_ordinary_part_still_leaves_upstream_and_forgets_desired_intent` |

Both claims are true and fully evidenced — the *names* are wrong, not the coverage. Neither
is annotated in its own record, because the records are historical and the plan permits
annotation only to prevent a present-tense false claim, which a wrong test name very nearly
is: a reader checking the citation finds nothing and has no way to tell whether the property
went unevidenced or was renamed. This record carries the correction; the earlier records are
left as written.

None of the eleven closure blockers in the plan's §11 is present: there is no second
reachable upstream owner, no unbounded queue, no clearnet-compatible bouncer-network path, no
client-dependent upstream CAP fingerprint, no secret or environment leakage, no false
downstream CAP advertisement, no history/search index inconsistency, no ghost owner after
durable deletion, no session bytes lost or duplicated at bind transfer, no unbounded nick
reclaim, and no arbitrary raw command execution.

## 14. M005 closure decision

**M005 is closed.**

i2pr-irc core is now a durable, multi-Network, multi-client, mature local IRC bouncer: one
runtime controller owning one upstream owner per Network, durable detached-channel and
presence policy that survives restart, indexed history search with bounded work, per-session
IRCv3 mediation across sixteen advertised capabilities, an Operator surface that is bounded
and cannot print a secret, and deterministic adverse-network behaviour with every queue and
timer explicitly ceiled.

It remains independent of a concrete router and structurally unable to create generic upstream
clearnet traffic.

The milestone closed on evidence rather than on assertion only because the integrated pass
found three defects that eight per-subsystem suites had each, correctly, passed over. That is
the finding worth carrying into R001: a milestone closure that only re-runs its parts
certifies the parts.

## 15. Router R001 readiness disposition

R001 (portable SAM router adapter) becomes **eligible under its own prerequisites**. M005 was
its hard dependency and that dependency is now discharged, with no router-blocking finding
outstanding.

M005 closure does **not** authorise R002 managed-i2pr integration or R003 Proposal 170
control work beyond their existing interface and product gates. ADR-0001's boundary stands
unchanged: the SAM bridge is a router adapter, not permission for arbitrary network access,
and future i2pr integration must consume public managed-app capabilities rather than private
router internals.