# Bouncer Core M004-C — Adverse-Network and Resource Qualification

Status: blocked

Blockers:

- `plans/closure/bouncer-core/015-status.md` accepted
- `plans/closure/bouncer-core/016-status.md` accepted

Research authority:

- `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Primary class: invariant qualification

## 1. Objective

Qualify the M004 anonymity policy, global reconnect budget, M003 durable core and Corrective-014 routing together under deterministic I2P-like failure campaigns.

This plan should add testkit instrumentation and only bounded production diagnostics required to prove recovery. It is not a feature milestone.

## 2. Fault-model boundary

The runtime consumes an ordered reliable stream.

Test:

- arbitrary segmentation;
- short reads/writes;
- delay;
- read/write stall;
- bounded backpressure;
- EOF;
- reset;
- provider unavailable;
- connect timeout;
- delayed/missing PONG;
- stale-generation completion.

Do not inject byte reordering/duplication within the ordered stream.

## 3. Many-Network campaigns

At the configured supervised-Network ceiling where practical:

### Startup herd

All Networks start together.

Prove:

- connect concurrency <= scheduler ceiling;
- start rate <= budget;
- fairness/no starvation;
- bounded waiter set;
- no task leak.

### Simultaneous outage

All live Networks lose transport.

Prove:

- each generation ends cleanly;
- no user traffic is replayed;
- reconnects obey local backoff + global scheduler;
- route/batch/session generation state is cleared;
- DesiredState remains durable.

### Simultaneous recovery

Provider becomes healthy for all Networks.

Prove:

- no second herd;
- eventually admitted Networks recover fairly;
- one failing/terminal Network cannot block others.

## 4. Combined client/store campaigns

Run combinations rather than isolated unit cases:

- one slow client + one healthy client + upstream burst;
- several slow clients across several Networks;
- SQLite stall while PING/PONG is due;
- history queue saturation during live messages;
- retention work during upstream burst;
- durable JOIN/PART store failure while other Networks stay online;
- reconnect recovery while a history query is active;
- routed WHOIS/WHO/NAMES/LIST while another session is overloaded.

Expected policies remain:

- slow live client => detach only that session;
- history pressure => bounded drop/count;
- upstream control remains schedulable;
- DesiredState reconciliation remains bounded;
- route state cannot leak to replacement sessions.

## 5. Privacy-bearing protocol campaigns

Fuzz/qualify:

- CTCP delimiter placement;
- missing trailing delimiter;
- ACTION at size bounds;
- DCC host/port payloads;
- VERSION/TIME/etc. metadata queries and replies;
- unknown CTCP;
- `+` client-only tags;
- unprefixed tags;
- malformed/oversized tag prefixes;
- repeated CAP/tag/CTCP churn.

Assert no environment/secret sentinel escapes and no blocked DCC reaches a network API.

## 6. Restart/crash campaigns

Use deterministic reopen/restart fixtures around:

- committed desired JOIN/PART;
- history append;
- cursor advance;
- read-marker advance;
- retention;
- schema v2 reopen.

Prove:

- durable DesiredState/history/cursors survive according to commit state;
- no stale ObservedState or SessionId returns;
- ambiguous non-idempotent user commands are not replayed;
- HistoryEventId remains canonical and non-reused.

## 7. Resource instrumentation

Expose/test bounded live counts for:

- Network owner tasks;
- session tasks;
- reconnect waiters;
- in-flight connects;
- store queue depth;
- upstream normal/control queue depths;
- session queues;
- response routes;
- open batches;
- DesiredReconcile entries;
- history ingest queue/accounting.

Avoid unbounded diagnostic history.

For each campaign record:

- baseline;
- peak bounded count;
- settled count.

Settled state must equal expected live baseline after churn.

## 8. Busy-loop qualification

Use virtual time / poll counters where possible to prove:

- stalled provider does not spin;
- reconnect scheduler sleeps between admissions;
- empty DesiredReconcile does not spin;
- store stall does not create a retry loop;
- malformed peer cannot cause immediate reconnect hot-loop beyond backoff/budget.

## 9. Static boundary qualification

Extend `scripts/check-network-boundary.py` positive controls for prohibited production primitives:

- generic TCP;
- DNS;
- HTTP client;
- SOCKS/proxy;
- DCC dial/listen.

Retain production source/dependency tree scan.

## 10. Required campaign scale

Minimum evidence should include:

- 64 Networks or current MAX_SUPERVISED_NETWORKS;
- several clients per selected Network;
- normal/control/session/store/history/route max+1 tests;
- at least 100 reconnect churn iterations under virtual time;
- repeated campaign runs showing deterministic bounded steady state.

If runtime cost makes one exact count impractical in CI, keep a full deterministic local qualification target and a smaller CI smoke target, but closure must record the full evidence.

## 11. Corrective handling

Bounded bugs found by campaigns may be fixed here with regressions.

If a finding requires:

- new durable schema semantics;
- replacement of owner model;
- generic network authority;
- new privacy disclosure policy;
- unbounded queue/workaround;

stop and register a separate corrective/ADR.

## 12. Acceptance criteria

M004-C closes when the combined system remains bounded, schedulable, privacy-preserving and restart-consistent under the defined campaigns, with resource counts returning to expected steady state.

## 13. Closure evidence

Create `plans/closure/bouncer-core/017-status.md` with:

- fault matrix;
- many-Network scheduler evidence;
- combined slow-client/store evidence;
- privacy fuzz matrix;
- restart/crash matrix;
- baseline/peak/settled resource table;
- static boundary positive controls;
- exact verification;
- M004-D readiness decision.
