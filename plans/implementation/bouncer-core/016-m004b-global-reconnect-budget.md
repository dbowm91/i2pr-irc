# Bouncer Core M004-B — Global Reconnect Budget and Fair Scheduling

Status: blocked

Blocker:

- `plans/closure/bouncer-core/014-status.md` accepted

Research authority:

- `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Primary class: infrastructure + invariant

## 1. Objective

Prevent process-wide reconnect herds while preserving independent long-lived Network supervision.

Add one bounded process-level reconnect scheduler that gates every initial connect and retry without replacing per-Network exponential backoff.

## 2. Invariants

1. Every production `I2pStreamProvider::connect()` attempt is admitted through the global scheduler.
2. Initial startup and reconnect use the same budget.
3. At most one pending reconnect waiter exists per Network.
4. Waiter count is bounded by the supervised-Network ceiling.
5. In-flight connects never exceed the configured hard limit.
6. Attempt-start rate/burst never exceeds the configured budget.
7. Waiting is starvation-free.
8. stop/cancellation removes the waiter and releases permits.
9. terminal configuration/auth failures do not retry forever.
10. per-Network backoff remains independent.
11. scheduler state is process-local and never persisted.
12. no scheduler path busy-spins.

## 3. Architecture

Add a `ReconnectScheduler` owned by/alongside `NetworkCatalog` and passed to Network owners as a narrow permit interface.

Conceptually:

~~~text
NetworkOwner
   |
   | after local backoff delay
   v
ReconnectScheduler::acquire(NetworkId, attempt metadata)
   |
   +-- bounded waiter set
   +-- fair order
   +-- start-rate tokens
   +-- concurrent-connect permits
   |
   v
ConnectPermit
   |
I2pStreamProvider::connect()
~~~

Permit drop/completion releases in-flight capacity.

Do not give the scheduler access to IRC state, credentials or message content.

## 4. Production policy configuration

Freeze explicit constants/configuration for:

- maximum concurrent connect attempts;
- maximum burst;
- token/refill interval or equivalent attempt-start rate;
- maximum pending waiter count;
- fairness discipline.

Defaults should be conservative and documented, but the implementation must support injected test configuration so virtual-time tests do not depend on wall-clock production values.

Do not make these limits unbounded/configured as zero-means-unlimited.

## 5. Initial startup

Starting 32/64 stored Networks must not call connect simultaneously.

Network owners may enter a distinct waiting/backoff diagnostic phase while awaiting a global permit.

The scheduler should allow a small bounded burst but enforce the same long-run rate as reconnect.

## 6. Per-Network jitter

Keep the existing exponential backoff.

Replace correlated entropy with a deterministic mix including:

- NetworkId;
- connection generation / retry attempt;
- injected process/test entropy seed.

Two Networks with the same attempt count must not receive the same jitter sequence solely because their generations match.

No cryptographic randomness is required; the purpose is herd decorrelation, not secrecy.

## 7. Failure classification

Create an explicit reconnect disposition.

Retryable examples:

- ProviderError::Unavailable/Failed where classified transient;
- connect timeout;
- EOF/reset;
- transport/generation loss;
- transient registration timeout.

Terminal-until-config-change examples:

- invalid durable config;
- SASL/auth rejection;
- deterministic registration rejection that unchanged credentials/config cannot fix.

Do not continuously consume global attempts for terminal failures.

A manual/store-backed configuration reconciliation may re-arm a terminal Network.

## 8. Fairness

Use FIFO or another deterministic starvation-free policy.

Requirements:

- a repeatedly failing Network cannot monopolize the scheduler;
- later Networks eventually receive admission;
- canceled/stopped Network is removed;
- duplicate acquire from the same Network coalesces/refuses rather than adding another waiter.

## 9. Observability

Expose bounded non-secret scheduler diagnostics:

- pending waiters;
- current in-flight attempts;
- total admitted;
- total delayed;
- peak in-flight;
- terminally suppressed count;
- per-Network current scheduler disposition only if bounded.

No endpoint/config secret payloads.

## 10. Testkit support

Extend fake provider/scheduler fixtures to record:

- connect start;
- connect completion;
- current/peak concurrent connects;
- start timestamps under virtual time;
- NetworkId;
- cancellation.

Tests must assert actual concurrency rather than infer it from backoff values.

## 11. Required tests

- 64-network simultaneous startup never exceeds in-flight ceiling;
- attempt starts obey rate/burst;
- simultaneous outage/retry obeys both gates;
- every retryable Network eventually gets a permit;
- one repeatedly failing Network does not starve another;
- stop while waiting removes waiter;
- cancellation during connect releases permit;
- terminal auth/config failure consumes no further permits until reconciled;
- reconfiguration re-arms terminal Network;
- independent jitter sequences for equal attempt counts;
- deterministic seed gives reproducible sequence;
- no duplicate waiter per Network;
- scheduler max/max+1 waiter handling;
- scheduler idle path has no spin.

## 12. Acceptance criteria

M004-B closes when connect concurrency and start-rate are process-wide bounded under startup, outage and recovery while every healthy/retryable Network remains fairly serviceable.

## 13. Stop conditions

Stop if scheduler integration requires replacing one-owner-per-Network supervision, persisting scheduler state, or sharing provider streams across Networks.

## 14. Closure evidence

Create `plans/closure/bouncer-core/016-status.md` with budget constants, fairness proof/tests, failure classification matrix, jitter matrix, 64-Network startup/outage evidence, cancellation evidence and M004-C readiness contribution.
