# Bouncer Core M003-A — Durable Storage and Identity Foundation

Status: ready for handoff

Repository planning baseline: `0cb3354d5e4139a1f3b37a8d0afc10854c6c7686`

Research/decision authority:

- `plans/research/004-m003-storage-multiclient-history-research.md`
- `plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md`
- `plans/research/003-rust-irc-crate-conformance-results.md`

Source roadmap:

- `plans/subsystems/bouncer-core-roadmap.md#M003--durable-multi-network-multi-client-and-history-model`

Primary class: infrastructure

## 1. Objective

Create the durable substrate M003 will build on without yet adding simultaneous clients or history playback.

The delivered foundation must provide:

- an owned bounded SQLite worker using `rusqlite`;
- schema version 1 and transactional migration/open validation;
- durable NetworkId, ClientId, BufferId, and HistoryEventId representation;
- ephemeral SessionId distinct from durable ClientId;
- durable DesiredState load/mutation;
- explicit wall-clock abstraction for history receive time;
- bounded retention primitives and store health;
- restart evidence proving live ObservedState is never restored from disk.

M003-B and later plans must depend on this closure rather than inventing their own storage or identity semantics.

## 2. Readiness

All pre-M003 protocol gates are closed:

- Corrective 004;
- Corrective 005;
- Corrective 006;
- Research 003 conformance disposition.

ADR-0002 freezes the storage/identity decisions required by this plan.

No external router or listener capability is required.

## 3. Current evidence

The current workspace has no durable store crate and no database dependency.

Current core IDs are local `u64` wrappers for NetworkId and ClientId. ConnectionGeneration is explicitly ephemeral.

Current runtime stores desired channels in `NetworkState` configuration input but does not persist them.

The current injected `Clock` is monotonic and appropriate for reconnect/liveness; it is not a wall clock suitable for durable receive timestamps.

`tokio-rusqlite` is intentionally not selected because its current worker uses an unbounded crossbeam request channel.

## 4. Invariants

1. SQLite blocking calls never execute on NetworkSupervisor or downstream Tokio tasks.
2. Store ingress is bounded.
3. Runtime callers submit typed store operations, not arbitrary SQL closures.
4. No storage API returns raw `rusqlite::Connection` ownership to runtime code.
5. DesiredState is durable; ObservedState remains generation-local.
6. ConnectionGeneration and SessionId are never persisted as restart authority.
7. Durable IDs are opaque and are not hashes of display strings.
8. History/cursor IDs cannot silently alias a different object after deletion/restart.
9. Schema migration/open failure prevents normal runtime startup.
10. Store diagnostics never expose SASL password/private destination material.
11. Store dependency choices preserve Rust 1.88.
12. The static no-clearnet boundary remains unchanged.

## 5. Scope

### In scope

- add a dedicated store crate/module with `rusqlite 0.40.x`, `default-features = false`, and the reviewed SQLite feature set;
- prefer bundled SQLite for deterministic supported binary behavior unless implementation evidence demonstrates a packaging blocker;
- owned worker thread;
- bounded request channel and typed responses;
- schema/open configuration;
- schema version 1;
- durable IDs;
- SessionId in core/runtime;
- durable network configuration and desired channels;
- restart-required SASL material in a secret-specific representation;
- Buffer/history/cursor tables sufficient to freeze IDs/schema even if later plans fill them;
- wall-clock abstraction and deterministic fake wall clock;
- store health and shutdown;
- bounded retention operation primitive;
- focused migration/restart/security tests.

### Out of scope

- simultaneous downstream sessions;
- NetworkCatalog/BouncerRuntime orchestration;
- history ingestion/playback;
- labeled-response;
- downstream IRCv3 history capabilities;
- real listener/authentication;
- router adapters;
- FTS/search;
- database encryption policy.

## 6. Required production changes

### A. Store crate and dependency

Add an owned store component, preferably `crates/store`.

Use `rusqlite` directly. Do not adopt `tokio-rusqlite`, a connection pool, SQLx, or arbitrary closure dispatch in this plan.

Document the exact selected `rusqlite` version/features and transitive/build-script review.

### B. Bounded worker

Create a single owned worker thread per database.

A suitable shape is:

~~~text
StoreHandle
  -> bounded typed request sender
       -> storage thread
            -> rusqlite::Connection
~~~

A bounded Tokio MPSC receiver may be consumed with a synchronous/blocking receive API from the worker thread if that avoids another channel dependency.

Every request that needs a result carries a bounded one-shot response path.

The worker owns connection open, pragmas, migrations, transaction execution, maintenance, and close.

Queue full, worker stopped, SQLite failure, and incompatible schema are distinct typed errors.

### C. SQLite open policy

At open:

- verify/set application identity;
- enable foreign keys;
- select WAL;
- use FULL synchronous durability initially;
- set an explicit busy timeout bounded by project policy;
- verify supported SQLite version for STRICT schema;
- run migrations transactionally before serving normal requests.

No partially migrated database is accepted.

### D. Schema version 1

At minimum create typed STRICT tables representing:

- networks;
- network_secrets or an equivalent separately typed secret table;
- desired_channels;
- clients;
- buffers;
- history_events;
- client_cursors;
- read_markers;
- schema/application metadata where not covered by SQLite pragmas.

Foreign keys and unique constraints must encode ownership boundaries.

Use stable integer identities. HistoryEventId allocation must prove non-reuse under retention deletion; using `INTEGER PRIMARY KEY AUTOINCREMENT` is acceptable and preferred unless the implementation proves an equivalent monotonic allocator.

Do not serialize Rust structs/enums wholesale as the durable schema.

### E. Durable network configuration

Define a store-facing durable Network configuration sufficient to reconstruct `UpstreamConfig` after restart without restoring live observations.

At minimum preserve:

- NetworkId;
- I2pEndpoint canonical value;
- configured nick/user/realname;
- optional SASL username/password in secret-safe types;
- desired channels.

Validation on load must apply the same domain constraints as fresh configuration.

Malformed/corrupt durable rows fail explicitly; do not silently synthesize defaults that could alter identity.

### F. SessionId

Add an ephemeral SessionId domain type.

SessionId:

- is allocated locally at attachment time;
- is never used as durable cursor identity;
- is never restored after restart;
- is distinct from ClientId in APIs and diagnostics.

Do not change ClientId's canonical durable meaning.

### G. Wall clock

Add an injectable wall-clock interface separate from monotonic Clock.

Provide:

- production system UTC implementation;
- deterministic test implementation;
- bounded integer/typed receive timestamp representation.

Do not derive canonical history ordering from wall time.

### H. Store interface

Expose typed operations sufficient for later plans, including:

- load configuration catalog;
- create/update/remove network;
- add/remove desired channel;
- create/lookup client;
- create/lookup buffer;
- append bounded history batch;
- query bounded history range;
- get/advance client cursor;
- get/advance read marker;
- bounded retention;
- health/flush/shutdown.

Later plans may implement currently unused operations, but their semantic contracts should be frozen now.

### I. Restart boundary

Provide a restart reconstruction test/helper that loads durable DesiredState into fresh runtime configuration structures.

Prove that the store cannot restore:

- self_channels;
- members/topics/modes;
- join attempts;
- generation;
- pending response routes;
- SessionId.

## 7. Ordered work packages

### A — dependency/store skeleton

Add crate, reviewed dependency, typed errors, bounded request channel, worker ownership and shutdown.

### B — schema/open/migration

Implement schema v1, pragmas, application/schema validation, transactional migration harness and corruption/incompatible-version behavior.

### C — durable identities/configuration

Implement NetworkId/ClientId/BufferId/HistoryEventId conversion and CRUD for network desired configuration/secrets/channels.

### D — SessionId/wall time

Add ephemeral session identity and injectable wall clock with deterministic tests.

### E — store contract/retention primitives

Freeze typed request surface needed by B-F, bounded queries and bounded maintenance.

### F — restart/security qualification

Prove DesiredState reconstruction, secret redaction, no stale ObservedState, bounded queue overload, clean shutdown and Rust 1.88.

## 8. Failure, restart, cancellation, contention

Store queue full returns an explicit overload error; callers do not spin or allocate a secondary unbounded queue.

Worker failure transitions store health to failed and rejects new work.

A canceled request may still have committed if SQLite completed its transaction; mutation APIs therefore return explicit committed/failed results and callers must not infer rollback solely from task cancellation.

Process restart reopens/migrates the DB before starting network owners.

Only the worker owns the SQLite connection, so M003-A has no inter-connection write contention.

## 9. Compatibility

This defines schema version 1. There is no predecessor production schema.

After closure, schema v1 becomes migration authority for later plans.

Do not change repository licensing or Rust 1.88.

## 10. Required tests

- queue max and max+1;
- worker stop with queued/in-flight work;
- open/create v1 DB;
- reopen v1 DB;
- incompatible future schema rejected;
- migration transaction rollback fixture;
- foreign-key enforcement;
- STRICT type rejection;
- durable IDs stable across reopen;
- HistoryEventId/non-reuse allocation property;
- secret values absent from Debug/errors;
- valid configuration roundtrip;
- corrupt/invalid configuration rejected;
- desired channels survive restart;
- no ObservedState field restored;
- SessionId never stored;
- fake wall clock deterministic;
- retention primitive bounded;
- static network boundary and conformance corpus remain green.

## 11. Verification

Expected minimum:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh full
scripts/fuzz-smoke.sh
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Also record `cargo tree --locked -e all` and the SQLite/rusqlite feature/dependency review.

## 12. Documentation

Add/update:

- storage architecture;
- schema/migration contract;
- identity model including SessionId;
- desired-vs-observed restart behavior;
- dependency review;
- roadmap/registry;
- closure record `plans/closure/bouncer-core/007-status.md`.

## 13. Acceptance criteria

1. A bounded typed store worker owns all SQLite work.
2. No selected async wrapper contains an unbounded hidden request queue.
3. Schema v1 opens/migrates transactionally and rejects incompatible state.
4. Network/desired-channel configuration survives restart.
5. Live ObservedState does not.
6. durable IDs and SessionId semantics are explicit and tested.
7. history cursor identity cannot be reused after retention deletion.
8. blocking store work cannot run on network owner tasks.
9. store overload/failure is typed and bounded.
10. Rust 1.88/full verification passes.
11. M003-B remains blocked until this closure is accepted.

## 14. Stop conditions

Stop and register a corrective/ADR review if:

- bundled SQLite cannot meet supported platform/build constraints;
- rusqlite raises MSRV above 1.88;
- a single worker cannot expose bounded async semantics without an unbounded hidden queue;
- schema design requires persisting live ObservedState;
- secrets require a product-level at-rest encryption decision before safe persistence;
- a proposed ID strategy can reuse cursor/history identity.

## 15. Closure evidence

`plans/closure/bouncer-core/007-status.md` records:

- implementation commits;
- exact rusqlite/SQLite feature tree;
- queue topology/capacity;
- schema v1;
- migration/restart matrices;
- durable/live state matrix;
- identity allocation evidence;
- secret review;
- exact verification including Rust 1.88;
- explicit M003-B readiness decision.
