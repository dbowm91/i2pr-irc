# Bouncer Core M003-B Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/008-m003b-multinetwork-multiclient-ownership.md`

Authority: ADR-0002, `plans/research/004-m003-storage-multiclient-history-research.md`

Prior closure: `plans/closure/bouncer-core/007-status.md`

Repository planning baseline reviewed: `6f81cd31291d3f71e6907e74c30f23c719585278`

Primary class: capability

## What was delivered

A catalog that owns many Networks, a per-Network owner that runs one upstream generation with any number of attached local sessions, per-session tasks with their own decoders and queues, SessionId/ClientId separation in live routing, bounded fanout with per-client detachment, and persistence-first DesiredState for client JOIN/PART.

M002 evidence is preserved: every pre-existing conformance test still passes unmodified.

## Ownership topology

```text
NetworkCatalog                      process-level; owns which Networks exist
   |-- SupervisorHandle  (network 1) bounded control channel (64)
   |      v
   |   NetworkOwner<P>             sole owner of network 1 observed state
   |      |-- upstream read/write for exactly one ConnectionGeneration
   |      |-- control queue (32) | normal intent queue (64)
   |      |-- session event queue (256)
   |      |-- SessionTask(A)  decoder + control/normal queues + writer task
   |      `-- SessionTask(B)  decoder + control/normal queues + writer task
   `-- SupervisorHandle  (network 2) ... fully independent
```

There is **no** process-wide `Arc<Mutex<NetworkState>>`. Each owner keeps its `NetworkState` local; the catalog routes typed commands and holds only bounded handles.

## Bounded state

| Bound | Value | Evidence |
|---|---|---|
| Networks per catalog | 64 | `the_catalog_supervises_a_bounded_number_of_networks` |
| Sessions per Network | 64 | `session_ceiling_is_refused_explicitly` |
| Session event queue per Network | 256 | `SESSION_QUEUE_CAPACITY` |
| Supervisor control channel | 64 | harness bound |
| Upstream control / normal queues | 32 / 64 | `CONTROL_QUEUE_CAPACITY`, `NORMAL_QUEUE_CAPACITY` |
| Decoded lines per client read | 64 | `MAX_LINES_PER_READ` |
| Lines yielded per client queue send | bounded channel (64) | `SESSION_EVENT_QUEUE_CAPACITY` |
| Attach/detach cycles to steady state | 25 | `repeated_attach_detach_cycles_return_to_a_bounded_steady_state` |

Overload is always typed and local. A full session queue or a full session map refuses *that client*; it never stalls upstream processing or another client.

## Multi-network isolation

| Property | Evidence |
|---|---|
| Eight Networks each reach Online on their own generation | `many_networks_supervise_independently` |
| One Network's reconnect cycle does not advance another's generation | `one_network_failing_leaves_the_others_online` |
| One Network's backoff does not change another's `reconnect_attempt` | same |
| Catalog ceiling is refused, not silently exceeded | `the_catalog_supervises_a_bounded_number_of_networks` |
| Restart restores durable intent and no live supervisor or session | `restart_rebuilds_networks_from_durable_state_without_sessions` |

## Multi-client behavior

| Property | Evidence |
|---|---|
| Several sessions attach concurrently and share one generation | `several_sessions_attach_concurrently_and_share_one_generation` |
| One upstream event fans out to every attached session | `one_upstream_event_fans_out_to_every_attached_client` |
| One client detaching leaves the others *and* the generation | `one_client_detaching_leaves_the_others_and_the_generation` |
| Session ceiling refused explicitly | `session_ceiling_is_refused_explicitly` |
| Repeated attach/detach returns to a bounded steady state | `repeated_attach_detach_cycles_return_to_a_bounded_steady_state` |
| The upstream generation is **not** online-on-first-client: it comes online with zero clients | `many_networks_supervise_independently` reaches Online before any attach |

## Identity semantics

| Property | Evidence |
|---|---|
| SessionId is per attachment; a reused ClientId gets a new session | `a_reused_client_id_gets_a_fresh_session_identity` |
| An event naming an unknown session cannot detach a live one | `a_stale_session_event_cannot_reach_a_replacement_session` |
| A session cannot forge an upstream generation | `a_session_cannot_forge_an_upstream_generation` |

The owner stamps the live `ConnectionGeneration` on every forwarded intent. A session supplies only bytes and an intent class, so it cannot attribute a frame to a generation it does not own.

## DesiredState is persistence-first

| Property | Evidence |
|---|---|
| A JOIN commits durably before any upstream JOIN is observable | `a_join_commits_durably_before_upstream_bytes_exist` |
| A failed commit writes no upstream JOIN at all | `a_failed_join_commit_writes_no_upstream_join` |
| Durable intent survives restart | `desired_state_persists_through_a_restart` |
| A storage failure does not end the upstream Network | same |

`a_failed_join_commit_writes_no_upstream_join` drains the upstream stream after the store worker is stopped and asserts it contains no `JOIN #` at all, while the Network stays `Online`. That is the strong form of persistence-first: the invariant is "no upstream JOIN without a prior durable commit", not "a JOIN eventually arrives".

## No replay across generations

Nothing user typed is retained for replay into a later generation. The generation writer drops any intent stamped by an earlier generation rather than writing it, because a disconnect after an outbound command leaves delivery ambiguous. `a_session_cannot_forge_an_upstream_generation` covers the stamping side; the drop rule is enforced by the generation-fence check in the writer task.

No idempotency key, attempt record, or delivery-receipt table exists in this plan, which is correct: Plan 009 introduces replay/history explicitly rather than the bouncer quietly replaying user chat across a reconnect.

## Secret review

A stored SASL credential is never rendered in any diagnostic: `a_stored_secret_never_reaches_a_diagnostic`. The Network snapshot type carries nick, phase, generation, observed channels, and counters — no payload, endpoint, or credential.

## Correctness defects found and fixed during implementation

| Defect | Consequence if shipped | Fix | Evidence |
|---|---|---|---|
| Upstream QUIT was queued *after* its own sender was dropped in generation teardown | the deliberate shutdown QUIT could never be written | queue before dropping senders, then close | `a_session_cannot_forge_an_upstream_generation` (teardown path exercised in every generation test) |
| A `sleep(0)` branch was added to the owner select loop | a busy-yield that violates "never spin", masking stalls | removed; the loop is driven only by real events | all owner tests |
| `std::mem::take` on a local map did not compile as written, silently risk passing the wrong borrow | teardown could leave session tasks alive past their generation | explicit `std::mem::take(&mut sessions)` with a comment on ownership | `one_client_detaching_leaves_the_others_and_the_generation` |

## Network boundary review

Unchanged. The catalog opens no listener and resolves no hostname. All tests use `FakeI2pStreamProvider` and in-process duplex streams; no test requires a real listener or router. `scripts/check-network-boundary.py` passes unchanged, including its positive controls.

## Verification actually executed

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
./scripts/check-network-boundary.py
./scripts/fuzz-smoke.sh
rustup run 1.88.0 sh scripts/verify.sh full
```

All passed. 179 workspace tests pass, including every pre-existing M002 conformance test and 16 new multi-network/multi-client tests. Rust 1.88 clippy additionally required inlined format arguments in one new test; the explicit form is committed.

## Unresolved findings

None blocking. One item is explicitly deferred rather than left implicit:

- Store-pressure *liveness* while a Network is Online (PING/PONG remaining schedulable when the store stalls) is M003-F integrated qualification, because it requires the store-stall fixture shared across plans.

## M003-C readiness decision

**M003-C is unblocked.** The preconditions it names — a bounded history journal attached to each upstream generation, per-`BufferId` durable buffers, monotonic per-client cursors, read markers, bounded retention with deterministic clamping, and legacy playback — are all available on the durable substrate delivered by M003-A and owned per Network by this plan. `BufferId`, `HistoryEventId`, cursors, markers, and `retain` already exist with frozen semantics, and the owner now supplies the per-buffer upstream event stream that Plan 009 turns into retained history.