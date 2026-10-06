# Bouncer Core Plan 020 — Runtime Control and Downstream Admission Closure

Plan: `plans/implementation/bouncer-core/020-m005a-runtime-control-and-downstream-admission.md`

Baseline: `ac1670a` ("Decompose and register M005 milestone")

Implementation: `c79acef` ("Land the M005-A runtime controller and downstream admission")

Class: infrastructure + invariant

## 1. Outcome

The process-level ownership M005 was decomposed to establish exists, and bound-session and
upstream ownership stayed where the architecture already put them.

`RuntimeController` is now one task that owns every Network owner, the durable Network
records, and the single bounded queue through which all of it is mutated.
`DownstreamAdmission` owns a client socket from accept time until a Network claims it, and
the claim is a one-shot transfer of the reader itself rather than a second connection.

No new protocol capability is advertised, no upstream owner was added, and no client task,
session, or owner can reach storage or a supervisor directly.

## 2. Ownership and lifetime matrix

| Component | Owns | Must not | Evidence |
|---|---|---|---|
| `RuntimeController` | every `LiveOwner`, durable records, revision, one bounded queue | be driven by a client task | `every_control_clone_reads_the_same_snapshot` |
| `LiveOwner` | `SupervisorHandle` + stop signal + gauge receiver + `JoinHandle`, as one value | let a handle outlive its task silently | `change_stops_the_old_owner_and_starts_exactly_one_replacement` |
| `RuntimeControlHandle` | a bounded sender, a stop signal, a status receiver | reach storage, a catalog, or a supervisor | `the_control_handle_carries_no_store_and_no_catalog`, `the_runtime_controller_exposes_no_dial_or_listen_operation` |
| `DurableNetworks` | every durable Network mutation | be obtainable by a session or owner | `change_with_an_unknown_commit_starts_exactly_what_is_durable` |
| `NetworkOwner` | `NetworkState`, upstream generation, bound sessions | gain a second upstream owner | unchanged; `create_is_durable_before_it_is_activated` |
| `DownstreamAdmission` | one accepted socket through registration | touch storage or upstream | `an_unreachable_selection_opens_no_upstream_connection` |
| `PreparedSession` | the reader, exactly once | be cloned or bound twice | `a_prepared_session_cannot_be_cloned_or_transferred_twice` |
| `SessionTask::resume` | one task over an existing reader | re-run registration | `a_bound_client_keeps_one_identity_across_the_transfer` |

A caller may hold `RuntimeControlHandle`. It is not a `NetworkCatalog`, not a `StoreHandle`,
and not a `SupervisorHandle`.

## 3. Transfer byte-preservation evidence

`a_command_sent_in_the_same_read_as_registration_is_not_lost` writes

```text
NICK bot\r\nUSER user 0 * :client\r\nPING :keepalive\r\n
```

in a single write, so the `PING` is decoded from the same read that completes registration.
The decoder's decoded-but-untranslated batch is parked in the reader and drained before the
socket is read again, so the command is answered on the transferred socket. The test also
asserts the client received its `001` projection, so a pass cannot come from a session that
was simply dropped.

`a_bound_client_keeps_one_identity_across_the_transfer` asserts the outcome carries the
`SessionId` admission allocated (`SessionId(606)`), not a new one.

`the_legacy_direct_attach_path_projects_the_same_welcome_burst` drives two clients through
the admission path against one Network and asserts both see `001` and
`005 ... CLIENTTAGDENY=*`, so the new path and the existing direct path cannot drift apart
in what a client sees.

## 4. Dynamic create/change/delete failure matrix

| Operation | Durable outcome | Owner outcome | Result |
|---|---|---|---|
| create, commit confirmed | persisted | started | `Ok(NetworkId)` |
| create, commit unknown, re-read proves it landed | persisted | started | `Ok(NetworkId)` |
| create, commit unknown, re-read proves it did not | unchanged | not started | `Err` |
| create past `MAX_SUPERVISED_NETWORKS` | unchanged | not started | `Err(QueueOverloaded)` |
| create, candidate fails validation | unchanged | not started | `Err(InvalidConfig)`, proven by `an_invalid_candidate_is_refused_before_anything_is_written` |
| change, commit confirmed | replaced | old stopped **and awaited**, one replacement started | `Ok(())` |
| change, commit unknown | re-read; whatever is durable is started | old stopped and awaited, exactly one started | `Err` to the caller |
| delete, commit confirmed | removed | owner stopped and awaited | `Ok(true)` |
| delete of an absent Network | unchanged | none | `Ok(false)` |
| delete, commit unknown | unknown | owner stopped and awaited, no owner remains | `Err` to the caller, proven by `delete_with_an_unknown_commit_leaves_no_live_owner` |

Two ordering properties are load-bearing and are now explicit in the code:

- **Change stops and awaits the old owner before committing.** Returning before the join
  would let the replacement race the owner it replaces.
- **Delete stops and awaits the owner before forgetting the row.** The reverse order leaves
  a durable row whose owner is gone and whose sessions have silently ended.

The stop signal is on a side channel rather than in the bounded command queue. Shutdown
queued behind work the owner has not reached is a deadlock: the queue is full precisely
because the owner is busy, so it will not drain until it stops, and it will not stop until
the queue drains.

## 5. Schema 2 to 3 migration evidence

`SCHEMA_VERSION` is 3. The v2 → v3 step adds `networks.display_name` and fills every row
with `network-<id>`.

Store qualification (`crates/store/tests/qualification.rs`):

- `a_schema_two_database_is_migrated_to_three_on_open` builds a v2 fixture through
  `testing::create_v2_database`, opens it, and asserts every row received
  `fallback_display_name(NetworkId)`, that the value equals the literal `network-<id>`,
  and that **no endpoint text appears in any name**. The fixture endpoints are deliberately
  recognisable strings so this is a real assertion.
- `the_migrated_display_name_is_readable_through_the_public_api` reads the migrated row
  back through the public typed API, so the evidence does not depend on the same path that
  is under test.
- `an_operator_chosen_display_name_round_trips_and_survives_reopen` proves an operator-set
  name is durable and is not overwritten by the fallback.
- `a_display_name_that_could_alter_reply_parsing_is_refused` rejects empty, spaced,
  `:`-bearing, `,`-bearing, newline-bearing, and over-ceiling values, and accepts exactly
  `MAX_DISPLAY_NAME_BYTES`.
- `a_failed_v2_to_3_migration_leaves_the_version_two_database_intact` obstructs the step,
  asserts the open fails, asserts the version stays 2, and asserts the pre-existing column
  is left exactly as it was.

Migration steps now apply in order, one version at a time, so a database several versions
behind walks the same path it would have taken on each intervening release.

## 6. Queue and resource bounds

| Bound | Value | Where |
|---|---|---|
| Control requests | `CONTROL_REQUEST_CAPACITY = 64` | `controller.rs` |
| Snapshot entries | `MAX_CONTROL_SNAPSHOT_NETWORKS = MAX_SUPERVISED_NETWORKS` | `controller.rs` |
| Registration time | `ADMISSION_REGISTRATION_TIMEOUT = 60s`, injectable | `admission.rs` |
| Lines per read | `MAX_LINES_PER_READ` | `session.rs` |
| Writer drain on close | `WRITER_DRAIN_DEADLINE = 5s` | `downstream.rs` |
| Per-session intent queue | `SESSION_EVENT_QUEUE_CAPACITY` | `session.rs` |

`a_saturated_control_queue_is_refused_as_overload_while_shutdown_stays_reachable` stalls
the store, fills the queue, asserts the next request is refused as
`RuntimeError::QueueOverloaded`, then asserts `request_stop` still works while the queue is
full and that the controller ends with requests outstanding.

`the_snapshot_is_bounded_and_carries_nothing_identifying` renders the whole snapshot and
asserts it contains neither the endpoint, the nickname, the username, nor the realname.

`the_snapshot_revision_advances_on_every_state_change` proves a reader can prove it saw a
specific change.

## 7. Two latent defects found and fixed

These were not in the plan. Both were found by making M005-A's own claims testable, and
both are recorded because each was a way for a claim to be silently false.

**A closed control queue ended the writer instead of letting the normal queue drain.**
`next_queued_frame` returned on the first `None`, and a biased `select!` returns `None` from
a closed queue immediately. A refusal queued on the normal queue microseconds earlier was
therefore discarded with the task: a client that was owed an explanation got a closed
connection instead. This could not be observed before M005-A because the session handle held
both queues and released them together, at which point there was nothing left to lose.
`next_queued_frame` and `next_upstream_frame` now track each producer's liveness separately
and end only when both are done.

**A failed mutation left the published snapshot claiming a live owner that had stopped.**
`delete` and `change` stopped the owner and then touched storage, so a storage failure
returned before the snapshot was republished. An Operator reading that snapshot would wait
for an owner that no longer existed. Both now republish immediately after the owner stops
and before storage is touched. `delete_with_an_unknown_commit_leaves_no_live_owner` fails
without this.

## 8. Legacy compatibility evidence

- `SupervisorHandle::attach` and `SupervisorCommand::Attach` are unchanged. The new
  `AttachPrepared` is a separate variant, not a second form of `Attach`.
- `NetworkCatalog::attach` and the whole existing qualification suite are untouched and
  pass.
- `a_legacy_direct_attach_path_projects_the_same_welcome_burst` covers the pre-bound path
  through the admission entry point and asserts both paths project identically.
- `the_legacy_network_owner_is_gated_out_of_the_production_build` and
  `no_other_first_party_module_references_the_legacy_owner` (Corrective 019) still pass: the
  new modules do not reference the gated legacy owner.

## 9. Full verification results

| Check | Result |
|---|---|
| `./scripts/check-network-boundary.py` | pass |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace --all-features` | pass, 25 suites |
| `cargo test -p i2pr-irc-store` | pass, 9 unit + 37 qualification |
| `cargo test -p i2pr-irc-runtime --test m005a_controller_admission` | pass, 27 |
| `./scripts/verify.sh quick` | pass |
| `./scripts/verify.sh full` | pass |

### Structural guards added

- `the_runtime_controller_exposes_no_dial_or_listen_operation` reads `controller.rs` and
  asserts no socket namespace and no dial/listen/accept/connect shape. `verify.sh full`
  runs the tree-wide version of this guard; this one names the capability and is scoped to
  the controller, so it survives a reader who never runs the script.
- `a_prepared_session_cannot_be_cloned_or_transferred_twice` asserts `PreparedSession`
  carries no `derive(Clone)` and that `bind` takes it by value. A test asserting this in
  prose would not stop anyone adding `#[derive(Clone)]`.

## 10. Deviations from the plan

1. **`DurableNetworks` is a trait, not a bare `StoreHandle`.** The plan did not anticipate
   it. It exists because the `CommitState::Unknown` branch is the single most consequential
   decision in the controller and a real SQLite commit failure cannot be provoked from a
   test. Production still passes the store handle; `with_durable` exists for the test.

2. **`SessionReader::expected_nick` became `Option<String>`.** Admission runs registration
   before a Network may be known, and an unbound client must be able to register under any
   valid nickname. With a Network selected, the original strict comparison is unchanged.

3. **The registration ceiling moved inside `ClientWiring::register`.** Applying it in the
   caller cancelled the future and destroyed the read half with it, leaving nothing able to
   tell the client why. Applied inside, the handle and writer survive it.

4. **`SessionWriter::close_after_drain` was added.** Aborting a writer is the right way to
   stop one and the wrong way to end a conversation.

5. **`Phase::as_str` was added** so a rename of a phase variant cannot silently change an
   operator-facing string.

6. **The plan's "wait for `376`" framing does not exist.** The registration projection ends
   with `RPL_ENDOFNAMES` per channel and emits no `376`; the control-only welcome does.
   Tests name the frame they actually wait for. Asserting on a frame neither path emits is
   how a test proves nothing while looking thorough.

## 11. Findings

No open findings against M005-A.

Three limits of what was landed are recorded so M005-D and M005-I do not mistake them for
completed work:

- **Network selection is supplied by the caller, not resolved by the runtime.** M005-A
  admits a client with a `NetworkSelection` the caller chose. Deciding *which* Network a
  client belongs to — from its login, its existing `ClientId`, or an explicit command — is
  M005-D's job. The refusal path for a stale selection already works and is tested.
- **`ControlSnapshot` carries counts, names, and classifications only.** It has no
  per-network gauges beyond phase and attached sessions. Richer diagnostics are M005-I.
- **The owner still services supervisor commands only inside a live generation.** A
  Network that cannot reach its upstream cannot adopt a client. That is pre-existing
  behaviour and is not wrong, but it does mean "a client with no upstream gets no view" is
  still true after M005-A. M005-D should decide whether that stays.

## 12. M005-B readiness decision

**Plan 021 is unblocked and dependency-ready.**

Everything Plan 021 needs is landed and evidenced:

- a durable Network owner for detached-channel policy — `RuntimeController::change` /
  `delete` and the durable `desired_channels` table, both proven;
- a bounded, revisioned control snapshot to expose channel policy through — present, with
  the redaction and bound tests above;
- a typed mutation path that never hands storage to a caller — `ControlRequest` variants
  over `DurableNetworks`.

No M005-A finding blocks it. The two items above that touch M005-B's subject matter are
recorded as limits, not defects: channel policy is expressible through the landed
`ControlRequest` surface, and `desired_channels` is already the durable store for it.

Plans 022-028 remain gated behind their sequential predecessors, which is unchanged.