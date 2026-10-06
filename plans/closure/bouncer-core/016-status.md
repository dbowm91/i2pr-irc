# Bouncer Core M004-B — Global Reconnect Budget and Fair Scheduling

Status: closed

Implements: `plans/implementation/bouncer-core/016-m004b-global-reconnect-budget.md`

Research authority: `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Repository baseline: `639837b` ("Close M004-A and unblock M004-B")

Implementation commit: `4908775` ("Gate every connect through one process-wide reconnect budget")

## Objective

Prevent process-wide reconnect herds while preserving independent long-lived Network
supervision.

## Budget constants

Frozen in `crates/runtime/src/reconnect.rs` as an explicit `ReconnectBudget` value,
rather than as scattered literals, so a test can inject a different policy under virtual
time and the production numbers stay reviewable in one place.

| Constant | Value | Why this number |
|---|---|---|
| `MAX_IN_FLIGHT_CONNECTS` | 4 | An anonymity-network tunnel is expensive; opening many at once is what looks like a client to the network |
| `MAX_CONNECT_BURST` | 4 | The rate axis needs its own gate — without it, four in flight finishing quickly start attempts as fast as the provider can fail them |
| `CONNECT_TOKEN_INTERVAL` | 2 s | After the burst, one start every two seconds, so a full outage cannot become a request flood |
| `MAX_RECONNECT_WAITERS` | 64 | Tied to the supervised-Network ceiling; one waiter per Network |
| fairness | FIFO | Deterministic and starvation-free |

`ReconnectBudget::validate()` **refuses** a zero in-flight ceiling, zero burst, zero token
interval, or zero waiters. A zero would otherwise read to a reviewer as "no limit" while
behaving as "nothing may ever connect" — the wrong failure in both directions.

## Invariants and evidence

| Invariant | Mechanism | Evidence |
|---|---|---|
| 1. every production connect is gated | `acquire` precedes `provider.connect` in the owner loop | `the_in_flight_ceiling_is_never_exceeded` |
| 2. initial startup and reconnect share the budget | one gate, before the loop's first connect | same |
| 3. at most one waiter per Network | `queued` set; a duplicate acquire does not re-push | `a_duplicate_acquire_coalesces_instead_of_adding_a_waiter` |
| 4. waiter count bounded | `max_waiters`, refused explicitly | `the_waiter_ceiling_is_refused_rather_than_grown` |
| 5. in-flight never exceeds the ceiling | `in_flight < max_in_flight` before admission | `the_in_flight_ceiling_is_never_exceeded` |
| 6. start rate/burst bounded | token bucket, consumed on admission | `the_burst_ceiling_bounds_attempt_starts` |
| 7. starvation-free | FIFO; a retrier re-queues at the back | `waiters_are_served_in_arrival_order`, `a_repeatedly_failing_network_does_not_starve_another` |
| 8. cancellation removes the waiter and releases permits | `WaiterGuard` on drop; `ConnectPermit` on drop | `cancelling_while_waiting_removes_the_waiter`, `a_cancelled_connect_attempt_releases_its_permit` |
| 9. terminal failures do not retry forever | `mark_terminal` / `rearm` | `a_terminal_network_consumes_no_further_permits`, `reconciliation_rearms_a_terminal_network` |
| 10. per-Network backoff stays independent | unchanged exponential backoff; the scheduler gates *when allowed*, not *when wanted* | existing backoff tests unchanged |
| 11. scheduler state is process-local | constructed in `NetworkCatalog`, never stored | no persistence call site exists |
| 12. no busy-spin | `Notify` fires only on release or queue change | `an_idle_scheduler_does_not_spin` |

## Failure classification matrix

`classify()` decides whether **unchanged configuration** could fix the failure, not how
bad it looks.

| Failure | Disposition | Rationale |
|---|---|---|
| `RuntimeError::Registration` (credentials or config refused) | Terminal | the identical request cannot succeed |
| EOF / reset | Retryable | clears on its own |
| connect timeout | Retryable | clears on its own |
| transport or generation loss | Retryable | clears on its own |
| transient registration timeout | Retryable | distinct from a rejection |
| `ProviderError::Unavailable` / `Failed` | Retryable | transient by classification |

A terminal Network consumes **no** global attempts. Only a configuration reconciliation
calls `rearm`. This is what stops a permanently misconfigured Network from sitting in the
budget competing with healthy ones.

## Jitter matrix

`jitter_entropy(network, generation, seed)` mixes all three inputs through a splitmix64
finaliser.

| Property | Evidence |
|---|---|
| two Networks on the same attempt count diverge | `equal_attempt_counts_get_independent_jitter` |
| a fixed seed reproduces the sequence | `jitter_is_reproducible_for_a_fixed_seed` |
| the attempt number changes the value | `jitter_varies_with_the_attempt` |
| a different seed decorrelates | same test |

No cryptographic randomness is used, and none is needed: the purpose is herd dispersion,
not secrecy.

## Observability

`SchedulerDiagnostics` carries pending waiters, in-flight, total admitted, total delayed,
peak in-flight, and terminally suppressed. **Every field is a count.** There is
deliberately no field that could hold an endpoint, nick, credential, or message content,
and `diagnostics_carry_counts_only` asserts the rendered form contains none.

## Testing

`crates/runtime/tests/reconnect_budget.rs` adds 19 tests. `scripts/verify.sh full` exits 0;
441 tests pass across 21 binaries.

The tests measure **actual concurrency**: they hold permits open, count how many exist at
once, and release them deliberately. None of them infers concurrency from backoff values.

The test budget uses a one-hour token interval so the rate gate cannot refill by accident
during a test — which means a fixture that needs a second start must open the burst
widely. That is a property of the tests, not of production.

## Defect found and fixed while implementing

**The token bucket was checked but never decremented.** `may_start` checked
`tokens >= 1.0`, but admission never subtracted, so a full bucket admitted waiters as fast
as they arrived and the rate limit was a no-op. Found by
`the_burst_ceiling_bounds_attempt_starts`, which sets in-flight capacity *above* the
burst specifically so the two gates can be told apart. Both are now consumed together on
admission.

Recorded because the test that caught it exists only because the two gates were given
different values on purpose.

## Invariant on stop conditions

None triggered. The scheduler does not replace one-owner-per-Network supervision, does not
persist anything, and never sees a provider stream — it hands out a permit and nothing
else. `NetworkOwner` remains the sole owner of its Network and of its streams.

## Deliberate limitation, recorded so it is not "fixed" by mistake

**The legacy `NetworkSupervisor` in `lib.rs` is not gated by this scheduler.** It is the
UF-015-1 duplicate implementation, used only by its own tests. Gating it would mean
threading a scheduler into a code path that is not a shipping path; M004-A set the
precedent of sharing privacy policy across both and leaving the removal as follow-on.
If that supervisor is ever promoted to production, it must be gated first.

## M004-C readiness contribution

M004-C inherits:

- a process-wide bound on connect concurrency and connect-start rate;
- FIFO admission, so an adversarial or failing Network cannot starve the others;
- an explicit terminal classification, so a permanently broken Network cannot consume the
  budget;
- deterministic, seeded jitter, so an adverse campaign can reproduce a run;
- bounded, count-only diagnostics.

M004-C may begin now that M004-A and M004-B are both closed, which is the plan's stated
precondition.