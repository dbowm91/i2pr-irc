# Bouncer Core Roadmap

Status: active

Long-term references:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/research/001-bouncer-and-i2p-foundation.md

Related ADRs:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md

Post-closure corrective authority:

- plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md

## 1. Purpose and ownership boundary

This workstream owns the IRC bouncer independent of any concrete I2P router protocol.

It owns:

- strict bounded IRC/IRCv3 wire representation;
- bouncer domain identities and state machines;
- one-owner-per-network supervision;
- downstream client protocol behavior;
- capability mediation;
- reconnect/liveness/backpressure policy;
- durable desired state, history, and client cursors;
- anonymity-oriented IRC filtering;
- deterministic fault simulation used to qualify network behavior;
- operator-visible bouncer diagnostics.

It consumes I2P byte streams through I2pStreamProvider and local client streams through LocalAcceptor.

It must not own:

- generic TCP/DNS upstream networking;
- SAM syntax or router lifecycle;
- Proposal 170/I2PControl;
- private i2pr router internals;
- arbitrary extension/plugin execution;
- web/HTTP side channels;
- DCC transport;
- non-loopback hosted-service exposure.

## 2. Work classification

### Invariants

- upstream network authority is I2P-only;
- each Network has one live NetworkSupervisor owner;
- stale ConnectionGeneration events cannot mutate the current generation;
- non-idempotent outbound operations are not blindly replayed across ambiguous disconnects;
- every externally influenced queue/buffer/collection/timer has an explicit ceiling;
- downstream capabilities are advertised from bouncer semantics, not blindly mirrored;
- upstream capability negotiation does not fingerprint the currently attached downstream client;
- anonymity filtering cannot be bypassed by an unreviewed tag/CTCP/DCC path;
- history ordering is not based solely on remote wall-clock timestamps.

### Infrastructure

- wire codec;
- core IDs/domain values;
- injected time;
- I2pStreamProvider and LocalAcceptor interfaces;
- deterministic fault stream/provider;
- storage schema and bounded store workers;
- supervisor/event channels;
- static network-boundary guards.

### Capabilities

- persistent upstream IRC session;
- local downstream IRC server behavior;
- SASL/CAP registration;
- multi-network/multi-client attachment;
- durable history/replay;
- labeled-response routing;
- channel persistence/reconstruction;
- IRCv3 history/read-state;
- detached/persistent channel ergonomics;
- operator control and diagnostics.

### Polish

- richer diagnostics;
- import/export;
- bounded history search;
- configuration ergonomics;
- performance tuning after measured evidence.

## 3. Non-goals

This roadmap does not implement a router adapter.

It does not add clearnet IRC, remote hosted bouncer access, general multi-user hosting, ZNC-style modules, URL previews, DCC, ident, webhooks, or arbitrary command execution.

It does not require Proposal 170.

## 4. Current state

M001 protocol/domain/fault foundations are evidence-closed. M002 has a historical evidence-based closure, but post-closure review found that its runtime couples upstream Network lifetime to downstream client lifetime and has state-fidelity/static-boundary gaps that must be corrected before persistence or multi-client fanout. Those findings are owned by the active M002 post-closure corrective addendum and Corrective 004. M003 is blocked on Corrective 004 closure.

Canonical product/security direction and terminology are frozen. ADR-0001 establishes I2P-only upstream authority through I2pStreamProvider.

Research has identified ZNC as a mature feature-envelope reference and soju as the closer conceptual reference for persistent multi-network/multi-client/history behavior. Current IRCv3 specifications establish the need for explicit capability mediation, labeled-response routing, message-tag bounds, and draft-isolated history/read-marker behavior.

The runtime now contains the M002 single-network operational vertical, including CAP/SASL registration, liveness, bounded queues, state projection, reconnect/backoff, and deterministic fault evidence. Its current owner shape is not yet acceptable as the persistence foundation because a downstream detach terminates the upstream generation/supervisor and the channel/member mode representation is still lossy. Corrective 004 owns those bounded defects.

## 5. Target architecture

~~~
                         +-------------------------+
local client stream --->| DownstreamSession       |
                         +------------+------------+
                                      |
                                      | typed intents / routed events
                                      v
+------------------+      +-----------+-----------+      +-------------------+
| Configuration /  |----->| NetworkSupervisor     |----->| I2pStreamProvider |
| desired state    |      | one per Network       |      | abstract in core  |
+------------------+      +-----------+-----------+      +-------------------+
                                      |
                       normalized IRC events/state
                         /            |             \
                        v             v              v
               +-------------+ +-------------+ +-------------+
               | HistoryStore| | client fanout| | diagnostics |
               +-------------+ +-------------+ +-------------+

all protocol boundaries
        |
        v
 strict bounded IRC/IRCv3 wire layer
~~~

The supervisor is an actor-like owner, not a shared mutable bag. The implementation may use Tokio channels/tasks, but queue capacities and shutdown ownership are part of the contract.

The wire representation remains independent from network state. It must represent unknown forward-compatible commands/tags without converting them into misleading known values.

Storage is below bouncer semantics. The state machine remains testable with an in-memory/fake store and a fake I2P stream provider.

## 6. Dependency graph

~~~
M001 protocol/domain/fault foundation
  |
  v
M002 single-network operational bouncer
  |
  v
C001 / Corrective 004 persistent-upstream lifecycle + state fidelity
  |
  v
M003 multi-network/multi-client + durable history
  |
  v
M004 anonymity + adverse-network qualification
  |
  v
M005 mature operator feature set
  |
  +--> router-integration M001 portable SAM
~~~

Dependency classes:

- M002 has a hard dependency on M001.
- M003 has hard dependencies on historical M002 completion and the active C001 / Corrective 004 post-closure repair.
- M004 has a hard dependency on M003.
- M005 has a hard dependency on M004.
- Router integration has a hard dependency on M005 under the canonical phase ordering.
- External router interoperability fixtures are operational dependencies for router claims, not core M001-M005.

## 7. Milestones

### M001 — Protocol, domain, and deterministic-fault foundation

Class: invariant + infrastructure

Objective:

Establish the Rust workspace, reviewed dependency floor, strict bounded IRC/IRCv3 wire substrate, canonical domain IDs/endpoint types, I2pStreamProvider/LocalAcceptor contracts, injected time, deterministic reliable-stream fault harness, and static no-clearnet boundary guards.

Dependencies:

- ADR-0001 accepted.
- canonical planning foundation complete.

Deliverable boundary:

No functioning bouncer is claimed. The milestone provides safe/testable primitives on which one can be built.

User/operator value:

Indirect. It prevents protocol ambiguity and test-hostile networking from becoming foundational debt.

Exit conditions:

- exact wire bounds and malformed-input behavior are tested;
- partial read/write framing is deterministic;
- unknown commands/tags survive safe parse/encode round trips as specified;
- endpoint types cannot represent a generic clearnet endpoint;
- fake I2P provider and fault stream can drive deterministic connection-generation tests;
- time is injectable in domain/reconnect primitives;
- static no-clearnet guard has a positive control;
- dependency review records why each production dependency exists;
- fuzz/property targets exist for hostile wire input;
- full repository verification floor is green.

Deferred:

- Tokio production supervisor;
- SAM;
- downstream listener sockets;
- SQLite;
- complete bouncer operation.

### M002 — Single-network operational bouncer

Class: capability

Objective:

Provide one persistent upstream Network through a fake/test I2P provider and one local downstream client path, with complete IRC registration, capability negotiation, optional SASL, state tracking, phase-specific liveness, reconnect/backoff, stale-generation fencing, and clean shutdown.

Dependencies:

- M001 closed.

Deliverable boundary:

One Network and one attached downstream client. Persistence may remain minimal/in-memory except configuration needed to run deterministic scenarios.

User/operator value:

First true bouncer vertical, though not yet the durable/multi-client product.

Exit conditions:

- downstream client can register against the bouncer and observe synthesized state;
- upstream registration handles CAP 302 and configured SASL;
- PING/PONG remains live under bounded data pressure;
- disconnects at each registration phase recover or fail with typed state;
- reconnect uses bounded exponential backoff/jitter;
- stale generation completions are ignored;
- ambiguous outbound user messages are not replayed;
- shutdown drains/cancels owners without detached tasks;
- repeated deterministic disconnect/reconnect scenarios pass.

Deferred:

- SQLite/history;
- multiple networks/clients;
- chathistory/read-marker;
- full anonymity qualification;
- router adapter.

### M003 — Durable multi-network, multi-client, and history model

Class: capability

Objective:

Scale the correct M002 owner model to many Networks and local clients with transactional durable configuration/history, channel/current-state reconstruction, per-client cursors, response routing, and the foundational IRCv3 history set.

Dependencies:

- M002 closed.
- storage dependency/ownership decision reviewed before code.

Deliverable boundary:

A durable general bouncer core still operating through fake/test I2P stream providers.

User/operator value:

Persistent multi-network bouncer behavior suitable for ordinary local IRC clients.

Exit conditions:

- many NetworkSupervisors fail/reconnect independently;
- many downstream clients attach without shared-state races;
- desired channels survive restart and reconcile after fresh registration;
- SQLite migrations/restart behavior is proven;
- message history ordering is deterministic;
- slow storage cannot starve control traffic;
- labeled-response routes concurrent request replies to the correct client;
- command-specific bounded fallback routing exists when labels are absent;
- per-client cursors remain private/monotonic;
- server-time/batch/message-tags/echo-message semantics are covered;
- draft chathistory/read-marker adapters are isolated from durable schema syntax;
- legacy playback is bounded and does not duplicate negotiated chathistory.

Deferred:

- adverse-network scale qualification;
- full CTCP/tag privacy hardening;
- mature convenience features;
- router adapter.

### M004 — Anonymity and adverse-network qualification

Class: invariant + capability qualification

Objective:

Prove the bouncer behaves safely under anonymity-sensitive protocol inputs, high latency, stalls, path loss, reconnect storms, slow clients, store pressure, and restart/crash scenarios.

Dependencies:

- M003 closed.

Deliverable boundary:

The first bouncer-core state that may be described as suitable for anonymity-network operation, still independent of a concrete router.

User/operator value:

Confidence that unstable I2P-like conditions and hostile/privacy-bearing IRC inputs do not create leakage or systemic failure.

Exit conditions:

- DCC cannot invoke network behavior;
- reviewed CTCP policy is complete and tested;
- client-only tag allow/deny behavior is tested;
- no environment-derived IRC identity fields;
- secret/log redaction tests pass;
- upstream capability policy is stable across differing downstream clients;
- global reconnect attempt budget prevents herd behavior;
- many-network fault campaigns remain bounded;
- slow downstream clients have explicit bounded disposition;
- history pressure/failure cannot starve liveness;
- restart/crash leaves durable history/config consistent;
- static network guard proves no generic upstream DNS/TCP path in production;
- memory/task/queue counts return to bounded steady state after fault campaigns.

Deferred:

- richer operator feature set;
- router interoperability.

### M005 — Mature operator feature set

Class: capability + polish

Objective:

Add the most valuable mature-bouncer conveniences without widening the anonymity/network authority model.

Dependencies:

- M004 closed.

Candidate deliverables:

- detached/persistent channels;
- automatic away policy;
- keep-nick and nick reclaim;
- constrained IRC-only perform/connect commands;
- local IRC service administration;
- soju.im/bouncer-networks;
- richer reviewed IRCv3 capabilities;
- bounded history search;
- configuration import/export;
- operator diagnostics.

Exit conditions:

Each accepted feature has bounded semantics, restart behavior, multi-client behavior, anonymity review, and focused tests. The milestone must not become a catch-all plugin platform.

Deferred:

- arbitrary native/script plugins;
- generic external integrations;
- router-specific code.

## 8. Cross-cutting requirements

### Storage and migration

- durable IDs are not derived solely from display strings;
- schema migrations are ordered, transactional, and tested from each supported predecessor;
- restart uses desired state, not stale live observations;
- history retention/compaction is bounded;
- database work does not block latency-sensitive network tasks.

### Protocol and compatibility

- IRC/IRCv3 draft features are isolated/versioned;
- unknown forward-compatible wire values remain representable where safe;
- compatibility behavior is explicit rather than client-name detection;
- no downstream client can cause a materially different upstream capability fingerprint by merely attaching.

### Security and authorization

- initial process has one Operator;
- local TCP clients authenticate;
- local IPC trust requires an explicit accepted policy before replacing app authentication;
- secrets are redacted;
- anonymity filters are applied before upstream transmission/downstream environment exposure;
- no feature introduces alternate network egress.

### Concurrency, cancellation, and recovery

- one supervisor owns each Network;
- every spawned task has an owner and shutdown path;
- generation tokens fence stale async completion;
- bounded channels define overload behavior;
- cancellation cannot convert a failed send into automatic replay;
- global reconnect scheduling prevents herd behavior.

### Observability

- diagnostics are structured/bounded;
- disconnect reasons and backoff are visible;
- no raw credentials/private destination material;
- fault counters distinguish provider, registration, auth, protocol, store, and client failures.

### Performance and resource use

- optimize for many mostly idle IRC networks;
- avoid per-network heavyweight thread/runtime ownership;
- queue/memory ceilings take priority over lossless buffering of an arbitrarily slow local client;
- history writes are batched where correctness permits;
- performance gates are added only after a measured baseline.

### Documentation and operations

- every config value has security/recovery semantics documented;
- default timeout values are operational defaults, not protocol guarantees;
- any relaxation of local-only downstream access requires a new architecture decision.

## 9. Verification strategy

Use deterministic fake streams/providers before live-router tests.

Core qualification includes:

- golden parser/encoder corpus;
- arbitrary-byte fuzzing;
- property tests for parse/encode preservation;
- max/max+1 bound tests;
- partial read/write matrices;
- phase-boundary disconnect injection;
- stale-generation completion tests;
- virtual-time reconnect/liveness tests;
- many-network reconnect-storm tests;
- slow downstream/store pressure tests;
- multi-client concurrent query routing;
- migration/restart/crash tests;
- redaction/privacy negative tests;
- network-boundary static guard with positive control.

Live router evidence belongs to the router integration roadmap and must not be substituted for deterministic core evidence.

## 10. Risks and decision points

Key risks:

- adopting an IRC library whose high-level client assumptions prevent strict bouncer/server behavior;
- overfitting internal storage to draft IRCv3 syntax;
- shared mutable state causing stale-generation races;
- replaying ambiguous user traffic after reconnect;
- making retries independently per network and causing storms;
- unbounded replay/history/client queues;
- accepting convenient modules/integrations that silently restore external egress;
- treating compilation as proof of protocol correctness.

Architecture decisions requiring an ADR if encountered:

- multi-operator/multi-user hosting;
- remote downstream listening;
- externally executable plugin system;
- clearnet upstream support;
- changing durable history ordering identity;
- replacing one-owner-per-network supervision.

## 11. Completion definition

This roadmap is complete when M001-M005 are evidence-closed and the core is a durable, multi-network, multi-client, modern IRC/IRCv3 bouncer qualified under deterministic adverse-network/anonymity tests without any router-specific dependency or generic upstream clearnet path.

## 12. Milestone status

| Milestone | Status | Implementation plan | Closure record | Blockers |
|---|---|---|---|---|
| M001 | closed | plans/implementation/bouncer-core/001-protocol-domain-and-fault-harness-foundation.md | plans/closure/bouncer-core/003-status.md | — |
| M002 | historical closure; corrective active | plans/implementation/bouncer-core/002-single-network-operational-bouncer.md | plans/closure/bouncer-core/002-status.md | Strict current authority is C001 / Corrective 004 |
| C001 / Corrective 004 | ready | plans/implementation/bouncer-core/004-m002-persistent-upstream-lifecycle-and-state-fidelity-corrective.md | future plans/closure/bouncer-core/004-status.md | none |
| M003 | blocked | future | future | C001 / Corrective 004 closure |
| M004 | not started | future | future | M003 |
| M005 | not started | future | future | M004 |
