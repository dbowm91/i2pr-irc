# ADR-0003: Process runtime control and pre-bind downstream admission

Status: accepted

Date: 2026-10-06

Decision owners: project maintainers

Related specification sections:

- plans/000-long-term-specification.md sections 4.3, 4.4, 4.6, 5, 6, 10, 13 and 14
- plans/001-terminology-and-domain-model.md

Affected roadmap:

- plans/subsystems/bouncer-core-roadmap.md

Related research:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

## Context

M003 deliberately made each NetworkOwner the sole owner of one upstream Network and of the bound downstream SessionTask objects attached to the current generation. That ownership is evidence-closed.

M005 needs two behaviors that do not belong to one Network:

- a local IRC connection that can exist before selecting a Network and can remain an unbound bouncer-control connection;
- process-level creation, update, deletion, listing and status of many Networks.

NetworkCatalog currently stores handles but does not own I2pStreamProvider or live supervisor tasks, so it cannot serialize durable configuration mutation with supervisor lifecycle.

Putting these behaviors in an arbitrary NetworkOwner would give one Network authority over its siblings. Moving every bound SessionTask to a global owner would rewrite the M003 lifetime model.

## Decision drivers

- preserve one live upstream owner per Network;
- preserve SessionId-scoped response routing and generation fencing;
- support soju.im/bouncer-networks BIND semantics honestly;
- retain legacy pre-bound attachment;
- serialize durable Network mutation with live owner lifecycle;
- keep all queues and notification state bounded;
- keep generic network authority structurally absent;
- keep StoreHandle a persistence boundary rather than a runtime orchestrator.

## Considered options

### Permanent process ownership of all downstream sessions

Rejected for M005. It would move already-qualified bound-session lifetime out of NetworkOwner and require a new cross-owner termination and fanout model.

### Pre-bind admission with one-time transfer

Selected. A process-level admission object owns the local stream only until registration resolves to either an unbound control session or a selected Network. A bound session is then transferred exactly once into the selected NetworkOwner with its buffered decoder state and bounded writer intact.

### Fake a default Network before BIND

Rejected. It cannot represent a real unbound bouncer connection and would make network selection state misleading.

## Decision

### RuntimeController

i2pr-irc adds one process RuntimeController and a bounded RuntimeControlHandle.

The controller owns process-level orchestration only:

- provider handle used when constructing Network owners;
- NetworkCatalog;
- live supervisor task/stop registry;
- dynamic Network create/change/delete/reconcile;
- process control snapshots and revision;
- accepted pre-bind session orchestration.

It does not own upstream IRC state and does not interpret ordinary channel/user commands.

### Network ownership

NetworkOwner remains the exclusive owner of:

- upstream transport generations;
- registration and liveness;
- observed channel/user state;
- outbound upstream queues;
- response routing;
- bound session fanout and bound SessionTask lifetime.

A process control operation may ask an owner to stop or reconcile but cannot mutate its live observed state directly.

### Admission

A DownstreamAdmission object is allowed to exist without NetworkId.

It receives an already established ClientId from LocalAcceptor, allocates SessionId, owns bounded local read/write state, performs local registration/capability mediation, and may record a pre-registration Network selection.

When a selected Network is ready to receive the client, the admission object transfers a PreparedSession exactly once. Buffered bytes and negotiated state must survive the handoff.

An unbound session may complete registration and remain local-control-only.

### Authentication boundary

ClientId at the core LocalAcceptor boundary means the local client identity has already been authenticated or otherwise trusted by the concrete local adapter.

Standalone loopback TCP remains required by the canonical specification to authenticate before admission. M005 does not weaken that requirement and does not make an unauthenticated socket an Operator.

### Process control requests

Downstream bouncer-control syntax is translated into typed RuntimeControlHandle requests.

A downstream task never receives StoreHandle, I2pStreamProvider, a raw SQLite connection, a generic socket API, or mutable catalog internals.

### Dynamic configuration

Durable desired configuration remains authoritative across restart. Runtime mutation is explicitly reconciled with live owner lifecycle and represents unknown commit state by re-reading the store.

A delete cannot leave a live ghost owner. An update cannot keep an old owner transmitting while durable identity is ambiguous.

### Notifications

Process-level network state is published through bounded snapshot/watch semantics with a monotonic revision. Protocol-specific delta rendering is downstream adapter work and is not the durable catalog representation.

### Draft protocol isolation

soju.im/bouncer-networks is a work-in-progress protocol adapter. Its attribute names do not become storage column names or core domain vocabulary except where they map to an existing explicit product concept.

## Consequences

Positive:

- bouncer-networks can support true unbound and bound connections;
- live Network mutation gains one serialization point;
- the M003 NetworkOwner model remains intact;
- bound sessions still die with the generation that owns them;
- no generic network authority is added;
- bouncer control can be reused by IRC service, future CLI and future managed-app surfaces without sharing wire syntax.

Negative:

- M005 introduces one additional process actor/control queue;
- transfer-on-bind requires preserving decoder bytes and writer ownership explicitly;
- dynamic add/change/delete needs restart and unknown-commit tests;
- local listener adapters must continue to establish ClientId trust before core admission.

## Compatibility

Legacy one-Network-per-downstream attachment remains supported.

No current durable identity changes. NetworkId remains stable and is the only canonical network selector.

The first M005 implementation plan may add a durable display name, but it cannot replace NetworkId or derive network identity from that name.

## Security and reliability implications

The RuntimeController is local process authority and must not be reachable through an unauthenticated non-loopback listener.

Every control request and result is bounded.

Control notifications cannot contain credentials. Generic diagnostics remain redacted and do not expose private I2P destination material.

Any bouncer-network address accepted from IRC syntax must pass I2pEndpoint validation before durable mutation.

## Verification

M005-A must prove:

- one accepted session can remain unbound without creating an upstream connection;
- one session can select and transfer to exactly one Network;
- buffered post-registration bytes survive transfer exactly once;
- a late BIND is refused;
- one session cannot bind to two Networks;
- controller queue pressure is explicit;
- add/change/delete converge live owners to durable state under store failure and unknown commit state;
- a deleted Network has no surviving owner;
- legacy bound attachment remains behaviorally compatible;
- static no-clearnet guards remain green.

## Supersession

None.
