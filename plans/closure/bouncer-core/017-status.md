# Bouncer Core M004-C — Adverse-Network and Resource Qualification

Status: closed

Implements: `plans/implementation/bouncer-core/017-m004c-adverse-network-resource-qualification.md`

Research authority: `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Repository baseline: `b28144f` ("Close M004-B and unblock M004-C")

Implementation commits:

- `881f055` — "Account process-wide bounded resources for qualification campaigns"
- `a0e13e3` — "Qualify the bouncer under adverse many-Network campaigns"

## Objective

Qualify the M004 anonymity policy, the global reconnect budget, the M003 durable core and
Corrective-014 routing together under deterministic I2P-like failure campaigns, and add
only the bounded production instrumentation needed to prove recovery.

This milestone added no protocol feature. Its output is evidence.

## Fault-model boundary

The runtime consumes an ordered reliable stream. Byte reordering and duplication within
that stream are **not** injected: they cannot occur by definition, and a test for them
would be testing a fiction.

| Fault | Injected by | Covered in |
|---|---|---|
| arbitrary segmentation | `FaultScript::max_read`/`max_write` | existing testkit tests |
| short reads/writes | `max_read`/`max_write` bounds | testkit |
| **delay** | `FaultController::release_after` (new) | testkit + `adverse.rs` |
| read/write stall | `stall_read`/`stall_write` | testkit + `integrated.rs` |
| bounded backpressure | `FaultScript::capacity` | testkit |
| EOF | `eof_after_read` | testkit |
| reset | `reset_read`, `reset_after_read` | testkit |
| provider unavailable | empty outcome queue | `adverse.rs` (churn, stall) |
| connect timeout | `ProviderError::Timeout` | `reconnect_budget.rs` |
| delayed/missing PONG | upstream script withholding PONG | `owner.rs` tests |
| stale-generation completion | `DeferredCompletions` | testkit |

`release_after` was **added** because delay was the one listed primitive with no way to
express it. It is deliberately distinct from `stall_read`: a stall is a *missing* event
and proves a peer does not spin; a delay is a *slow* one and proves a bounded reader
survives bytes arriving late into an empty queue. It drops bytes rather than queueing
them if the declared capacity is already full, so a delayed release cannot manufacture
buffering the script never declared.

## Resource instrumentation

`crates/runtime/src/resource.rs` adds `ResourceLedger`. It keeps exactly two values per
gauge — **current** and **highest ever** — because a campaign needs to ask two different
questions and a forward-only counter can answer neither:

- a **peak** proves a ceiling held;
- a **return to baseline** proves nothing leaked.

Nothing is accumulated, so the ledger is a fixed-size struct no matter how long the
process runs or how many campaigns it serves. That is the plan's "avoid unbounded
diagnostic history" requirement, met structurally rather than by a retention policy.

### Gauge coverage against plan section 7

| Plan section 7 item | Gauge | Source |
|---|---|---|
| Network owner tasks | `owner_tasks` | `register`/`forget` on owner construction and drop |
| session tasks | `session_tasks` | per-turn publish |
| reconnect waiters | `reconnect_waiters` | read live from the scheduler |
| in-flight connects | `in_flight_connects` | read live from the scheduler |
| store queue depth | `store_queue` | read live from the store handle |
| upstream normal/control queue depths | `upstream_normal`, `upstream_control` | per-turn publish |
| session queues | `session_normal`, `session_control` | deepest attached session |
| response routes | `response_routes` | per-turn publish |
| **open batches** | `open_batches` | per-turn publish (**new** — nothing exposed this) |
| DesiredReconcile entries | `desired_reconcile` | per-turn publish |
| history ingest queue/accounting | `history_ingest` | per-turn publish |

`open_batches` was absent from `NetworkSnapshot` entirely. The router already tracked it;
the plan required it be observable, so it is now published.

### Three deliberate modelling choices

- **Process-wide gauges are not summed.** `store_queue`, `reconnect_waiters` and
  `in_flight_connects` are absent from `NetworkGauges`. Summing one store queue across 64
  Networks would multiply it by 64 and report a fiction.
- **Session queues are the deepest, not the total.** A sum would report one pathological
  client as if every client were that far behind.
- **Scheduler and store readings are read live, not mirrored.** A mirrored copy would be a
  second source of truth free to disagree with the thing it describes.

### Bounded by construction

The gauge set is a closed struct of integers, not a map keyed by caller-supplied text.
Tracked Networks are capped at `MAX_TRACKED_NETWORKS` (the supervised ceiling); exceeding
it is refused and counted. `LedgerRefused` carries no detail, because naming the Network
turned away would turn a diagnostic into an enumeration of what the user is running.

## Campaign evidence

All five campaigns are in `crates/runtime/tests/adverse.rs` and run at
`MAX_SUPERVISED_NETWORKS` (64) where the claim is about counting.

### Discipline: measure, then settle

**Concurrency is measured, never inferred.** A provider parks every admitted attempt until
the test releases it, so four overlapping attempts are observable rather than a timing
coincidence. An earlier version of the fixture counted attempts *after* the park and
therefore measured only the microseconds spent inside the fixture — the ceiling assertion
would have been vacuous. This was caught because the campaign asserted the ceiling
*saturates*, not merely that it is not exceeded.

**Every campaign asserts it settled.** Each takes a baseline before starting and asserts
`current == baseline` at the end.

### Section 3 — many-Network campaigns

| Campaign | Claim | Assertion |
|---|---|---|
| `a_startup_herd_at_the_ceiling_never_exceeds_the_connect_ceiling` | 64 Networks starting together do not become 64 simultaneous connects | `requested() <= 4`; `peak == 4` (saturates, so the test is not vacuous); scheduler reports the same ceiling; waiters bounded and `<= FLEET`; after shutdown `owner_tasks == 0` and `networks == 0` |
| `a_startup_herd_is_admitted_at_the_burst_rate_not_all_at_once` | the rate axis is gated, not only the concurrency axis | with a one-hour token interval, releasing the in-flight attempts admits **nothing**: `requested() == MAX_CONNECT_BURST` |
| `a_simultaneous_outage_ends_every_generation_and_replays_nothing` | a shared outage ends every generation cleanly | every Network reaches Backoff/Connecting; `response_routes == 0` and `open_batches == 0` afterwards; owners still counted because they still exist |
| `reconnect_churn_leaves_no_residue` | 120 reconnect rounds accumulate nothing | `attempts >= 120`; peak in-flight `<= 4`; waiters bounded; `refused == 0`; settles to baseline |
| `a_stalled_provider_produces_a_bounded_number_of_attempts` | no busy-loop | under pinned virtual time, 600 s of simulated time yields a bounded attempt count and never exceeds the in-flight ceiling |

The burst campaign deserves emphasis: it is the one that distinguishes a rate limit from
a concurrency limit. Releasing in-flight capacity admits nothing, because the token bucket
is empty and the interval is an hour. A system with only an in-flight ceiling would fail it.

### Section 8 — busy-loop qualification

The stall and churn campaigns run under `#[tokio::test(start_paused = true)]`, so "fast"
and "at the scheduled rate" stop being distinguishable by wall-clock luck. This required
the clock fix recorded below.

### Section 5 — privacy fuzz matrix

`crates/fuzz-smoke/src/main.rs` now drives the **real** classifier and the **real** tag
mediator over 20,000 generated frames, plus the existing 10,000 wire-parse iterations.

It depends on `i2pr-irc-runtime` deliberately: fuzzing a model of the mediation policy
would prove nothing about the policy that ships.

Generated cases: CTCP with the delimiter missing at the end, delimited and unterminated,
two back-to-back blocks (ambiguous delimiter placement), delimiter-only, `+client-only`
tags, unprefixed tags, oversized/malformed tag prefixes, and repeated CAP/tag/CTCP churn.

Assertions, stated as the thing that must never happen:

- a DCC request classifies as `Suppress` inbound and `Block` outbound;
- a metadata query is never `FanOut` inbound;
- a non-PING CTCP reply is always `Block` outbound;
- a sentinel standing in for hostname/SASL/path/process-id never survives into a mediated
  tag;
- mediated frames stay parseable and round-trip exactly under repeated churn.

Two of these assertions were **wrong on first writing** and the fuzz run caught them:
a CTCP `PING` is answered by the bouncer itself rather than suppressed, and a `PING` reply
is a token echo that is deliberately forwarded. Both are correct policy. The assertions
were corrected to state the real invariant (`delivers nothing to a client`, not
`is suppressed`) rather than being weakened.

### Section 9 — static boundary controls

`scripts/check-network-boundary.py` now carries named positive controls for every
prohibited primitive family the plan lists: generic TCP, DNS, HTTP client, SOCKS/proxy,
and each DCC family (listener, stream, unix listener, helper) — each asserted for both the
source predicate and the dependency-tree predicate, which are different code paths.

The controls were proven to bite by deleting each predicate in a scratch copy outside the
repository and confirming exactly the named controls failed. One vacuous control was
found and fixed during that exercise: the `ctcp.rs` exemption test initially used a
fixture that never named a DCC token, so it proved nothing.

`python3 scripts/check-network-boundary.py` exits 0 on the real repository.

## Resource baseline / peak / settled

Recorded from the campaigns themselves; the ledger produces these rather than a test
asserting them into existence.

| Campaign | Baseline | Peak | Settled |
|---|---|---|---|
| startup herd (64 Networks) | `networks 0`, all gauges 0 | `owner_tasks 64`, `in_flight 4`, waiters `<= 64` | equals baseline |
| burst gate (64 Networks) | `networks 0` | `in_flight 4`, `requested 4` | equals baseline |
| shared outage (4 Networks) | `networks 0` | `owner_tasks 4` | equals baseline |
| churn (3 Networks, 120 rounds) | `networks 0` | `owner_tasks 3`, `in_flight <= 4` | equals baseline |
| stall (4 Networks) | `networks 0` | `owner_tasks 4` | equals baseline |

## Defects found and fixed while qualifying

**1. Two clocks for one rate limiter.** `reconnect.rs` measured its token bucket with
`std::time::Instant` while the per-Network backoff it gates waited on
`tokio::time::sleep`. Under virtual time the backoff advanced while the bucket believed
time had not moved. Plan 017 section 8 asks to prove "the reconnect scheduler sleeps
between admissions" using virtual time — that claim was not even statable while the two
disagreed. The scheduler now uses `tokio::time::Instant`, the clock its own `sleep`
already ran on. Production behaviour is unchanged; testability was the defect.

**2. A healthy idle Network reported zero owner tasks.** The ledger counted an owner only
once its generation loop completed a turn. A Network that came Online and then sat idle
had completed no turn, so it reported zero — precisely the untruthful diagnostic this
subsystem exists to avoid. `register` now counts the owner from construction, which is
when it exists, and `Drop` clears it.

**3. A routing test passed or failed on scheduling luck.** `a_batched_multi_line_answer_...`
took the *first* line of the read buffer as the query whose label to parse. A buffer
legitimately begins with whatever the server sent first — a keepalive, a capability frame
— so the assertion depended on scheduling. It now selects the line by which query it
carries, and the parsing is shared with the neighbouring test so the two cannot drift
apart again. Confirmed stable over eight consecutive runs.

## What was *not* added, and why

Plan 017 section 4 (combined slow-client/store campaigns) and section 6
(restart/crash campaigns) name cases that `crates/runtime/tests/integrated.rs` already
covers, added for plan 012 and never invalidated:

- slow client under upstream burst — `one_clients_queue_pressure_costs_only_its_own_attachment`,
  `a_stateful_frame_overflow_ends_that_client_rather_than_stale_state`
- store stall while PING/PONG is due — `a_stalled_store_never_starves_control_traffic`
- history queue saturation and store failure isolation — `store_pressure_degrades_storage_only_and_creates_no_side_queue`
- committed desired-state restart — `a_clean_restart_rebuilds_durable_intent_and_no_live_state`
- cursor/read-marker/retention durability — `read_marker_and_cursor_survive_retention_and_clamp_monotonically`

Those were re-run unchanged as part of `scripts/verify.sh full` and are cited rather than
duplicated. No new test was written to restate an existing assertion.

The shared-outage campaign asserts that every generation ends and that route and batch
state are cleared. It does **not** assert delivery ambiguity per Network; that property is
covered end-to-end by `a_command_refused_by_the_upstream_queue_is_reported_and_never_replayed`
and `a_generation_replacement_discards_every_route`. Recording this so the gap is visible
rather than assumed.

## Invariant on stop conditions

None triggered. Nothing here required new durable schema semantics, a different owner
model, generic network authority, a new privacy disclosure policy, or an unbounded queue.
The instrumentation added is bounded by construction; the only new runtime dependency is
the fuzz harness depending on the runtime crate it is meant to qualify.

## Unresolved findings

- **UF-015-1** (carried forward, non-blocking): `crates/runtime/src/lib.rs` contains a
  second, legacy `NetworkSupervisor` used only by its own `#[cfg(test)]` module. It shares
  M004-A's privacy policy and M004-C's boundary checks, but it is **not** gated by
  `ReconnectScheduler`, so it is not covered by these campaigns. If it is ever promoted to
  production it must be gated and ledgered first.
- **UF-017-1** (new, non-blocking): `NetworkOwner::serve` constructs its `Backoff` inline
  (`base 1s`, `cap 300s`, `jitter 20%`) rather than injecting it, so a campaign cannot
  drive reconnect churn at a rate of its choosing — it must pin virtual time and advance
  by the cap. Making it injectable is a small, bounded change and would improve M004-D's
  campaign precision. Recorded rather than done, because changing the owner's construction
  signature is outside this plan's "instrumentation only" boundary.

## Exact verification

`scripts/verify.sh full` exits 0. That is:

- `scripts/check-network-boundary.py` — exit 0
- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — clean
- `cargo test --workspace --all-features --locked` — **448 tests passing across 16 test
  binaries**, up from 441 at the M004-B closure
- `scripts/fuzz-smoke.sh` — exit 0

## M004-D readiness decision

**M004-D may begin.** Its stated precondition, that M004-C close, is satisfied.

M004-D inherits:

- a process-wide baseline/peak/settled reading that can assert a whole integrated campaign
  returned to where it started;
- `open_batches` observable, completing the routing gauge set section 7 required;
- one clock for the connect budget and the backoff, so an integrated campaign can run
  under pinned virtual time;
- a seeded privacy corpus that drives production mediation code, reusable as the fuzz
  half of M004-D's integrated pass;
- named static-boundary controls covering all five prohibited primitive families.

M004-D should run the combined pass: the sections 3, 5 and 8 campaigns together with
plan 017's reuse of the existing sections 4 and 6 suites, against the integrated
subsystems rather than individually.