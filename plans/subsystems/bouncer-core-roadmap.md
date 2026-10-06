# Bouncer Core Roadmap

Status: active

Long-term references:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/research/001-bouncer-and-i2p-foundation.md
- plans/research/005-m004-anonymity-and-adverse-network-research.md

Related ADRs:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md
- plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md

Post-closure corrective authority:

- plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md
- plans/implementation/bouncer-core/013-post-m003-ircv3-time-history-and-queue-integrity-corrective.md
- plans/implementation/bouncer-core/014-live-multiclient-response-routing-corrective.md

Pre-M003 gates:

- plans/implementation/bouncer-core/005-pre-m003-observed-membership-and-downstream-cap-corrective.md
- plans/research/002-rust-irc-crate-conformance-plan.md

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

M001 and M002 are evidence-closed, with Correctives 004-006 preserving the post-closure findings that had to be resolved before persistence and multi-client work.

M003 was implemented through Plans 007-012 and historically closed in `plans/closure/bouncer-core/012-status.md`. That line delivered the owned bounded SQLite store, durable/live identity split, many independent Network owners and downstream sessions, bounded durable history/cursors, SessionId-scoped response routing, and draft history/read-marker adapters. ADR-0002 remains the durable storage/identity authority.

A post-closure source/specification review against the current IRCv3 documents found that the M003 closure overstated protocol and queue-integrity readiness:

- the owned `server-time` path parses/renders integer epoch seconds instead of the ratified UTC millisecond timestamp grammar;
- the advertised CHATHISTORY parser/adapter does not match the current draft parameter/reference grammar and TARGETS semantics;
- MARKREAD does not implement the current get/set/server semantics;
- several downstream-to-upstream bounded queue failures are ignored, permitting silent command loss and possible DesiredState/live divergence;
- a saturated live downstream fanout queue currently drops one arbitrary frame while leaving the session attached, which can leave an IRC client desynchronized.

Corrective 013 owned these findings and has now closed them; see `plans/closure/bouncer-core/013-status.md`. Historical M003 evidence remains useful and was not rewritten.

Research 005 decomposes M004 and promotes the unresolved response-routing finding UF-013-1 into Corrective 014. M004 now has registered handoffs: Corrective 014 first, M004-A and M004-B after it, M004-C after both, and M004-D for integrated closure. No M004 implementation may skip that sequence.

Canonical product/security direction remains unchanged: I2P-only upstream authority through I2pStreamProvider, one live owner per Network, bounded asynchronous behavior, durable DesiredState separate from fresh ObservedState, and no blind replay across ambiguous delivery.

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
C002 / Corrective 005 observed-membership + downstream-CAP correctness
  |
  v
Research 002 Rust IRC crate/spec conformance
  |            (found two defects, below)
  v
C003 / Corrective 006 framing recovery + casemapping conformance
  |
  v
M003-A / 007 storage + durable identity
  |
  v
M003-B / 008 multi-network + multi-client ownership
  |
  v
M003-C / 009 history + cursors + legacy playback
  |
  v
M003-D / 010 response routing + IRCv3 foundation
  |
  v
M003-E / 011 chathistory + read-marker adapters
  |
  v
M003-F / 012 integrated qualification + historical M003 closure
  |
  v
C004 / Corrective 013 IRCv3 time/history + queue integrity
  |
  v
C005 / Corrective 014 live response routing
  |
  +-------------------------+
  |                         |
  v                         v
M004-A / 015            M004-B / 016
anonymity mediation     global reconnect budget
  |                         |
  +------------+------------+
               |
               v
       M004-C / 017
adverse-network/resource qualification
               |
               v
       M004-D / 018
integrated anonymity qualification + M004 closure
               |
               v
M005 mature operator feature set
  |
  +--> router-integration M001 portable SAM
~~~

Dependency classes:

- M002 has a hard dependency on M001.
- M003 depends on historical M002 completion and Corrective 004, plus three pre-M003 gates that are all closed: Corrective 005, Corrective 006 (raised by the Research 002 corpus), and the Research 002 conformance/decision dependency with no unresolved M003-affecting correctness defect.
- M004 has a hard dependency on M003 plus the post-M003 correctness gates. Corrective 014 was the direct prerequisite and is closed; M004-A and M004-B then proceeded independently and are both closed, M004-C requires both and is now ready, and M004-D closes the milestone.
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

- M002, Corrective 004, Corrective 005, and Corrective 006 closed.
- Research 002 completed with a recorded production/dev-only/reference/excluded disposition for the candidate IRC crates and no unresolved correctness defect affecting M003.
- storage/identity research completed in `plans/research/004-m003-storage-multiclient-history-research.md`.
- persistence consistency and identity ownership accepted in `plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md`.

Implementation decomposition:

1. M003-A / Plan 007 — durable storage and identity foundation. **Closed**; see `plans/closure/bouncer-core/007-status.md`.
2. M003-B / Plan 008 — multi-Network and multi-client ownership. **Closed**; see `plans/closure/bouncer-core/008-status.md`.
3. M003-C / Plan 009 — history journal, cursors, and legacy playback. **Closed**; see `plans/closure/bouncer-core/009-status.md`.
4. M003-D / Plan 010 — response routing and foundational IRCv3 mediation. **Closed**; see `plans/closure/bouncer-core/010-status.md`.
5. M003-E / Plan 011 — chathistory/read-marker adapters. **Closed**; see `plans/closure/bouncer-core/011-status.md`.
6. M003-F / Plan 012 — integrated qualification and M003 closure. **Closed**; see `plans/closure/bouncer-core/012-status.md`.

Only the earliest dependency-ready plan is executable at a time. All six M003 plans are closed; M003 is evidence-closed as a whole in `plans/closure/bouncer-core/012-status.md`.

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

- historical M003 closure accepted;
- Corrective 013 closed with no unresolved M004-blocking finding. (Satisfied; see `plans/closure/bouncer-core/013-status.md`.)
- Research 005 completed: `plans/research/005-m004-anonymity-and-adverse-network-research.md`.
- Corrective 014 closes UF-013-1 before M004-A/B implementation.

Implementation decomposition:

1. Corrective 014 — live multi-client response routing. **Closed**; see `plans/closure/bouncer-core/014-status.md`. UF-013-1 is closed and downstream `labeled-response` is withheld until M004-A's client-tag mediator exists.
2. M004-A / Plan 015 — anonymity protocol mediation. **Closed**; see `plans/closure/bouncer-core/015-status.md`.
3. M004-B / Plan 016 — global reconnect budget and fair scheduling. **Closed**; see `plans/closure/bouncer-core/016-status.md`.
4. M004-C / Plan 017 — adverse-network/resource qualification. **Ready**.
5. M004-D / Plan 018 — integrated anonymity qualification and M004 closure. **Blocked on 017**.

Corrective 014, M004-A, and M004-B are closed, so M004-C is unblocked. Corrective 014
deliberately withheld downstream `labeled-response`, which M004-A then promoted alongside the
client-tag mediator and `CLIENTTAGDENY`; M004-B added the process-wide connect budget M004-C
builds its adverse-network qualification on. Both decisions are recorded in their own closure
records rather than left implicit.

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
| M002 | closed | plans/implementation/bouncer-core/002-single-network-operational-bouncer.md | plans/closure/bouncer-core/002-status.md | none |
| C001 / Corrective 004 | closed | plans/implementation/bouncer-core/004-m002-persistent-upstream-lifecycle-and-state-fidelity-corrective.md | plans/closure/bouncer-core/004-status.md | none |
| C002 / Corrective 005 | closed | plans/implementation/bouncer-core/005-pre-m003-observed-membership-and-downstream-cap-corrective.md | plans/closure/bouncer-core/005-status.md | none |
| Research 002 | closed | plans/research/002-rust-irc-crate-conformance-plan.md | plans/research/003-rust-irc-crate-conformance-results.md | none |
| C003 / Corrective 006 | closed | plans/implementation/bouncer-core/006-framing-recovery-corrective.md | plans/closure/bouncer-core/006-status.md | none |
| M003 | historical closure retained | plans 007-012 | plans/closure/bouncer-core/012-status.md | Strict current readiness authority is Corrective 013 |
| M003-A / Plan 007 | closed | plans/implementation/bouncer-core/007-m003a-durable-storage-and-identity-foundation.md | plans/closure/bouncer-core/007-status.md | none |
| M003-B / Plan 008 | closed | plans/implementation/bouncer-core/008-m003b-multinetwork-multiclient-ownership.md | plans/closure/bouncer-core/008-status.md | none |
| M003-C / Plan 009 | closed | plans/implementation/bouncer-core/009-m003c-history-journal-cursors-and-legacy-playback.md | plans/closure/bouncer-core/009-status.md | none |
| M003-D / Plan 010 | closed | plans/implementation/bouncer-core/010-m003d-response-routing-and-ircv3-foundation.md | plans/closure/bouncer-core/010-status.md | none |
| M003-E / Plan 011 | closed | plans/implementation/bouncer-core/011-m003e-chathistory-and-read-marker-adapters.md | plans/closure/bouncer-core/011-status.md | none |
| M003-F / Plan 012 | closed | plans/implementation/bouncer-core/012-m003f-integrated-qualification-and-closure.md | plans/closure/bouncer-core/012-status.md | none |
| C004 / Corrective 013 | closed | plans/implementation/bouncer-core/013-post-m003-ircv3-time-history-and-queue-integrity-corrective.md | plans/closure/bouncer-core/013-status.md | UF-013-1 promoted to Corrective 014 |
| C005 / Corrective 014 | closed | plans/implementation/bouncer-core/014-live-multiclient-response-routing-corrective.md | plans/closure/bouncer-core/014-status.md | none |
| M004 | active | plans 016-018 | future plans/closure/bouncer-core/018-status.md | Corrective 014, M004-A and M004-B closed |
| M004-A / Plan 015 | closed | plans/implementation/bouncer-core/015-m004a-anonymity-protocol-mediation.md | plans/closure/bouncer-core/015-status.md | Corrective 014 closed |
| M004-B / Plan 016 | closed | plans/implementation/bouncer-core/016-m004b-global-reconnect-budget.md | plans/closure/bouncer-core/016-status.md | Corrective 014 closed |
| M004-C / Plan 017 | ready | plans/implementation/bouncer-core/017-m004c-adverse-network-resource-qualification.md | future plans/closure/bouncer-core/017-status.md | Plans 015 + 016 closed |
| M004-D / Plan 018 | blocked | plans/implementation/bouncer-core/018-m004d-integrated-anonymity-qualification-and-closure.md | future plans/closure/bouncer-core/018-status.md | Plan 017 closure |
| M005 | not started | future | future | M004 closure |
