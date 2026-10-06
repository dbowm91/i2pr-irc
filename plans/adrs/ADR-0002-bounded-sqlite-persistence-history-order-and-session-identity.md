# ADR-0002: Bounded SQLite persistence, durable history order, and client/session identity

Status: accepted

Date: 2026-10-06

Decision owners: project maintainers

Related specification sections:

- plans/000-long-term-specification.md sections 4.4, 4.6, 5, 8, 12, and 14
- plans/001-terminology-and-domain-model.md

Affected roadmap:

- plans/subsystems/bouncer-core-roadmap.md

Related research:

- plans/research/004-m003-storage-multiclient-history-research.md
- plans/research/003-rust-irc-crate-conformance-results.md

## Context

M003 introduces durable configuration/history, many upstream Networks, simultaneous downstream clients, per-client replay position, response routing, and IRCv3 history adapters.

The project therefore needs to freeze several decisions before code creates a persistence ABI by accident:

- SQLite ownership and async isolation;
- which state is durable versus generation-local;
- durable identity allocation;
- canonical history ordering;
- the distinction between a durable client lineage and one live downstream attachment;
- how draft IRCv3 history/read-marker syntax relates to storage.

The current runtime intentionally keeps live ObservedState generation-owned. Correctives 004 and 005 proved that command intent, pending work, and observed network state must not be collapsed into one representation.

## Decision drivers

- preserve the existing bounded-queue invariant;
- keep blocking SQLite work off latency-sensitive Tokio tasks;
- make crash/restart semantics explicit;
- prevent draft IRCv3 syntax from becoming the durable schema;
- keep history order deterministic when server timestamps collide or are absent;
- support several downstream attachments without confusing durable read/playback state with socket lifetime;
- minimize production dependencies and generic authority;
- preserve Rust 1.88 compatibility.

## Considered options

### Option A — tokio-rusqlite as the storage boundary

Convenient async API, but the current 0.8 implementation internally dispatches work through an unbounded crossbeam channel. This conflicts with the project requirement that externally influenced queues have explicit ceilings.

Rejected as the production ownership boundary.

### Option B — pooled SQLite/SQL abstraction

A pool or broader SQL framework supports concurrency and other databases, but M003 has one embedded store and intentionally serialized durable write ordering. Pooling introduces additional connection, locking, scheduling, and dependency surface without solving a current requirement.

Rejected for M003. A later migration requires a new decision.

### Option C — rusqlite behind an owned bounded worker

One project-owned storage worker owns the SQLite connection and accepts typed requests through a bounded queue. The API exposes storage operations rather than arbitrary closures.

Selected.

## Decision

### Storage ownership

M003 uses `rusqlite` directly behind a project-owned bounded storage actor/worker.

The production store API exposes typed operations. Runtime callers do not receive a raw `rusqlite::Connection` and do not submit arbitrary database closures.

The initial SQLite topology is one owned connection/worker. Blocking SQLite calls never run on latency-sensitive Tokio network tasks.

The store queue has an explicit capacity and overload disposition. History pressure must not create an unbounded in-memory queue or block IRC control traffic indefinitely.

### SQLite policy

The initial store uses:

- foreign keys explicitly enabled;
- transactional ordered migrations;
- STRICT tables where supported by the selected bundled SQLite;
- WAL journal mode;
- a documented synchronous durability setting, initially FULL unless measured evidence justifies a separately reviewed relaxation;
- bounded query sizes and bounded retention/compaction work.

The application records and validates its schema version and application identity before normal runtime startup.

### Durable versus live state

Durable storage owns:

- Network configuration and durable NetworkId;
- durable desired channels and other DesiredState;
- persistent ClientId lineage;
- Buffer identity;
- HistoryEvent records;
- per-client playback cursors;
- operator read marker state;
- required restart configuration/secrets under the project's secret-handling policy.

Storage does not become authority for:

- ConnectionGeneration;
- current upstream registration phase;
- current joined membership;
- member/topic/mode ObservedState;
- pending/rejected JOIN attempts;
- live response correlations;
- live downstream SessionId;
- liveness/backoff timers.

Restart creates fresh supervisors and fresh ObservedState, then reconciles stored DesiredState.

### Durable identities

NetworkId, ClientId, BufferId, and HistoryEventId are durable opaque local identities and are not derived solely from display strings.

M003 adds SessionId as an ephemeral identity for one live downstream attachment.

ClientId answers "which durable client lineage owns playback/read state?" SessionId answers "which currently attached connection owns this CAP state, queue, and response route?"

Late responses from an old SessionId must not be delivered to a new attachment merely because both attachments belong to the same ClientId.

### History order

Every HistoryEvent has a bouncer-local durable sequence/HistoryEventId that is the canonical history order.

Server-time, receive wall time, and upstream msgid are source metadata. They do not replace the local order.

History identifiers used as cursor positions must not be reused after retention deletes older rows. The SQLite schema must choose an allocation strategy that proves this property.

### History representation

The store persists bounded protocol data sufficient to reconstruct downstream history while preserving opaque supported IRC fields/tags. It does not persist Rust enum serialization as the schema contract.

Draft IRCv3 syntax is not stored as durable identity.

### Cursors and read state

Per-client playback cursor:

~~~text
(ClientId, BufferId) -> HistoryEventId
~~~

Operator read marker:

~~~text
BufferId -> HistoryEventId
~~~

These are distinct concepts.

A playback cursor advances only after the relevant downstream delivery is acknowledged by the local session writer according to the M003 protocol.

Read-marker wire syntax is an adapter over the durable local sequence and moves only forward.

### IRCv3 adapter boundary

`draft/chathistory` and `draft/read-marker` remain versioned wire adapters over generic history/cursor queries.

Changing a draft capability spelling or command syntax must not require a database schema migration unless the underlying product semantics change.

## Consequences

Positive:

- bounded storage pressure is enforceable;
- SQLite blocking work is isolated;
- history order survives timestamp collisions and restart;
- draft protocol churn does not dictate schema;
- multi-client response routing can key live work by SessionId without exposing durable ClientId upstream;
- storage remains small and auditable.

Negative:

- the project owns a small worker/queue abstraction rather than using tokio-rusqlite directly;
- one worker limits raw database parallelism;
- durable mutations requiring confirmation may add an explicit async state transition;
- schema/migration tests become a first-class maintenance obligation.

Deferred:

- multiple SQLite reader connections;
- connection pooling;
- PostgreSQL or another store;
- encrypted-at-rest database policy beyond secret handling already required by the product;
- FTS/history search;
- multi-user/tenant identity.

## Compatibility and migration

No previous durable schema exists, so M003 may define schema version 1.

Once schema version 1 closes, future schema changes require ordered migrations and predecessor-version tests.

Internal numeric ID representation may change before M003 closure but becomes a durable compatibility concern once the schema is evidence-closed.

## Security and reliability implications

The storage worker must not expose SQL strings or arbitrary closures across the runtime boundary.

Secret-bearing fields are typed/redacted in diagnostics and never included in generic history/log dumps.

Database failure cannot silently convert DesiredState mutation into a successful upstream mutation. Durable intent-changing commands require an explicit persistence-first or otherwise crash-consistent disposition.

History ingestion failure is observable and bounded; it must not starve upstream PING/PONG.

Retention never deletes events still required to satisfy a retained cursor without an explicit cursor-clamping policy documented and tested by the implementing plan.

## Verification

M003 must prove:

- bounded store queue and overload behavior;
- no blocking SQLite work on network-owner Tokio tasks;
- migration atomicity and failure rollback;
- restart reconstructs DesiredState but not stale ObservedState;
- IDs remain stable and history cursor identity is never reused;
- many concurrent network/session actors cannot race durable mutations into inconsistent state;
- store pressure/failure does not starve network control traffic;
- draft history/read-marker adapters can change independently of schema semantics.

## Supersession

None.
