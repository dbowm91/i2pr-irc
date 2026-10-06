# Bouncer Core M003-B — Multi-Network and Multi-Client Ownership

Status: blocked

Blocker:

- `plans/closure/bouncer-core/007-status.md` accepted

Source milestone:

- Bouncer Core M003

Authority:

- `plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md`
- `plans/research/004-m003-storage-multiclient-history-research.md`

Primary class: capability

## 1. Objective

Scale the corrected single-Network owner model to many independently supervised Networks and many simultaneous local downstream sessions without introducing shared-state races.

The plan establishes:

- process-level BouncerRuntime/NetworkCatalog ownership;
- one NetworkSupervisor per durable NetworkId;
- simultaneous downstream sessions identified by SessionId and associated with durable ClientId;
- bounded event fanout;
- persistence-first desired JOIN/PART mutation;
- independent network failure/reconnect and client failure semantics.

History playback and IRCv3 response routing remain later plans.

## 2. Preconditions

M003-A must close with stable:

- StoreHandle API;
- schema/NetworkId/ClientId;
- SessionId;
- durable desired network/channel load/mutation;
- restart contract.

Do not implement against an assumed future storage API.

## 3. Invariants

1. One NetworkSupervisor remains the sole mutable live owner of one Network.
2. No process-wide Arc<Mutex<NetworkState>> is introduced.
3. Network failures do not terminate unrelated Networks.
4. Client failure/slow-client overload detaches only that SessionId.
5. ClientId persists across attachments; SessionId never does.
6. A stale session cannot emit intents after detach.
7. A late reply/event scoped to an old SessionId cannot be delivered to a replacement session solely because ClientId matches.
8. Desired JOIN/PART changes are crash-consistent with durable intent.
9. All attachment, fanout, and intent queues are bounded.
10. Upstream I2P-only/network boundary is unchanged.

## 4. Scope

### In scope

- BouncerRuntime;
- NetworkCatalog;
- start/stop/reconcile many supervisors from store;
- supervisor control handles;
- attach multiple downstream sessions to one Network;
- one session task/writer per attachment;
- bounded fanout;
- SessionId allocation;
- durable ClientId association;
- typed downstream intents;
- persistence-first desired JOIN/PART;
- many-network/many-client deterministic tests.

### Out of scope

- real local listener/auth scheme;
- legacy history playback;
- history ingestion;
- labeled-response;
- chathistory/read-marker;
- global reconnect storm qualification beyond basic bounded ownership;
- router adapters.

## 5. Target topology

~~~text
BouncerRuntime
+-- StoreHandle
+-- NetworkCatalog
|   +-- NetworkId A -> NetworkSupervisor A
|   +-- NetworkId B -> NetworkSupervisor B
|   +-- ...
+-- attachment/control ingress

NetworkSupervisor A
+-- one upstream generation
+-- generation-owned NetworkState
+-- SessionId 1 -> DownstreamSession task
+-- SessionId 2 -> DownstreamSession task
+-- ...
~~~

The catalog owns supervisor lifetime. Supervisors own session membership for their Network.

## 6. Required production changes

### A. BouncerRuntime and catalog

Load enabled Networks from StoreHandle and create one supervisor per NetworkId.

Expose typed start/stop/reconfigure/attach operations.

Bound process-level control queues.

### B. Supervisor handles

Refactor NetworkSupervisor so it can receive:

- attach(SessionId, ClientId, ByteStream);
- detach/session-exit;
- typed client intents;
- explicit stop/reconfigure.

The upstream owner loop remains independent of downstream attachment count.

### C. Session tasks

Each attached stream owns independent:

- decoder;
- CAP/registration state;
- output queues;
- writer task;
- SessionId.

The supervisor stores only bounded handles/metadata required for routing/fanout.

A detached session's task and writer are joined/aborted under one owner.

### D. Fanout

Normalize one upstream event once, mutate NetworkState once, then fan out according to each session's readiness/capabilities.

Do not clone attacker-sized unbounded structures. Existing line/tag ceilings apply before fanout.

A full session queue detaches that session; it does not stall upstream processing or other clients.

### E. Typed intents

Move downstream command handling toward typed intents so the supervisor decides:

- durable desired mutation;
- replay class;
- response-routing class;
- current generation.

Raw wire remains payload where appropriate, but sessions do not directly mutate network/store state.

### F. Persistent JOIN/PART

For a client request that changes durable desired membership:

JOIN:
1. validate under current rules;
2. persist desired channel through StoreHandle;
3. only on commit success emit upstream JOIN;
4. observed membership still requires self JOIN.

PART/removal:
1. persist removal;
2. only on commit success emit upstream PART;
3. observed membership changes only from authoritative network event/generation loss.

If persistence fails, return a local error and leave durable intent/upstream action unchanged.

### G. Reconfiguration

Store-backed process restart is required. Live dynamic Network add/remove may be added if it fits the frozen catalog API, but is not required to claim M003-B if restart coverage is complete.

## 7. Work packages

A. catalog/runtime owner;
B. supervisor control/attachment API;
C. multi-session task ownership;
D. bounded fanout;
E. typed intent conversion;
F. durable desired JOIN/PART;
G. multi-network/multi-client qualification.

## 8. Failure/restart/contention

Network A reconnect/failure must not alter Network B phase/generation.

One slow client is detached independently.

Store failure during desired mutation fails that client operation but does not kill the upstream Network solely because persistence is unavailable.

Simultaneous conflicting JOIN/PART requests are serialized by NetworkSupervisor and StoreHandle so the last committed desired state is deterministic.

On process restart, catalog rebuilds supervisors from durable Networks and desired channels; no prior SessionId/session is restored.

## 9. Tests

- 2, 8, and bounded-many independent NetworkSupervisors;
- one Network repeatedly fails while another remains online;
- 2+ simultaneous sessions on one Network receive state/event fanout;
- slow/overflow client detached while other client continues;
- session EOF/QUIT independent;
- reused ClientId gets a new SessionId;
- stale SessionId intent rejected;
- JOIN persistence failure emits no upstream JOIN;
- successful JOIN commits before upstream bytes;
- PART persistence failure emits no upstream PART;
- conflicting JOIN/PART serialized deterministically;
- restart recreates many Networks with no sessions;
- task/queue counts return to bounded steady state.

## 10. Verification

Run full workspace, conformance corpus, boundary guard, fuzz smoke and Rust 1.88 verification.

## 11. Documentation

Update runtime/catalog/supervisor/session architecture and create `plans/closure/bouncer-core/008-status.md`.

## 12. Acceptance criteria

- many Networks operate/fail independently;
- many sessions attach concurrently without shared mutable NetworkState;
- SessionId/ClientId semantics are enforced;
- fanout is bounded;
- desired JOIN/PART is persistence-first and crash-consistent;
- stale sessions cannot affect a current generation;
- no generic network authority appears;
- M003-C is unblocked only after closure.

## 13. Stop conditions

Stop if multi-client support requires weakening one-owner-per-Network, if store mutations cannot be serialized without blocking network control traffic, or if local listener/auth design becomes necessary to prove this internal capability.

## 14. Closure evidence

`plans/closure/bouncer-core/008-status.md` records topology, queue bounds, multi-network independence matrix, session/fanout matrix, durable mutation ordering, task cleanup, verification, and M003-C readiness.
