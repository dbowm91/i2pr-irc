# Research 004 — M003 storage, multi-client, history, and IRCv3 design

Status: complete for implementation planning

Research date: 2026-10-06

Repository baseline:

- `0cb3354d5e4139a1f3b37a8d0afc10854c6c7686`

Related accepted decisions:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`
- `plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md`

Related prior evidence:

- `plans/closure/bouncer-core/004-status.md`
- `plans/closure/bouncer-core/005-status.md`
- `plans/closure/bouncer-core/006-status.md`
- `plans/research/003-rust-irc-crate-conformance-results.md`

## 1. Purpose

Resolve the remaining architectural questions required to turn Bouncer Core M003 from a roadmap milestone into bounded implementation handoffs.

The research covers:

- SQLite dependency and async ownership;
- durable versus generation-local state;
- durable IDs and live session identity;
- many-Network and many-client ownership;
- history ordering/retention;
- per-client playback cursors and operator read markers;
- labeled-response and unlabeled fallback routing;
- foundational downstream IRCv3 capability mediation;
- draft chathistory/read-marker isolation;
- closure sequencing.

## 2. Storage dependency findings

### rusqlite

Reviewed current upstream `rusqlite` 0.40.1:

- actively maintained through September 2026;
- MIT licensed;
- edition 2024;
- declared Rust 1.88 MSRV;
- suitable direct embedded SQLite binding;
- broad optional feature set can be kept disabled except features intentionally selected by the store crate.

Conclusion: appropriate production SQLite dependency for M003.

### tokio-rusqlite

Reviewed current upstream `tokio-rusqlite` 0.8.0:

- current and maintained;
- MIT licensed;
- uses `rusqlite` 0.40.1;
- creates one background thread per opened connection;
- dispatches requests through `crossbeam_channel::unbounded()`.

The unbounded request channel directly conflicts with the project invariant that every externally influenced queue has an explicit ceiling.

Conclusion: do not use it as the production M003 storage boundary.

### pool/general SQL abstractions

A pool-oriented SQLite abstraction or broader SQL framework can be useful for applications that need parallel query workloads or backend portability.

M003 instead needs:

- one embedded database;
- deterministic total history order;
- transactionally serialized durable mutations;
- minimal dependency surface;
- explicit bounded request pressure.

A connection pool does not improve those requirements enough to justify the added ownership and dependency complexity.

Conclusion: start with one owned SQLite worker. Revisit only after measured evidence.

## 3. Store actor design

The preferred topology is:

~~~text
Tokio runtime tasks
       |
       | typed bounded requests
       v
StoreHandle
       |
       v
owned storage worker thread
       |
       v
rusqlite::Connection
~~~

The store boundary should expose typed operations rather than arbitrary SQL closures.

Candidate operation families:

- schema/open/health;
- load Networks;
- create/update/remove Network desired configuration;
- add/remove desired channel;
- create/lookup durable ClientId;
- create/lookup Buffer;
- append one or a bounded batch of HistoryEvents;
- query history by Buffer and sequence/time reference;
- advance ClientId playback cursor monotonically;
- advance Buffer read marker monotonically;
- bounded retention/compaction;
- flush/shutdown.

The queue must have a documented capacity and overload behavior.

History ingestion may be batched inside the worker, but batching cannot create an unbounded staging buffer.

## 4. SQLite policy

Recommended first schema policy:

- `PRAGMA foreign_keys=ON`;
- WAL journaling;
- initial `synchronous=FULL`;
- STRICT tables where the selected bundled SQLite supports them;
- explicit application identity;
- explicit schema version;
- transactional migrations;
- bounded query limits;
- bounded retention deletes.

The binary-oriented application may use bundled SQLite to avoid depending on unknown system SQLite feature levels across macOS, Windows, Raspberry Pi-class Linux, and other supported targets.

A switch to `synchronous=NORMAL` is performance tuning, not a transparent default change, because it weakens durability after OS/power failure.

## 5. Durable state boundary

Persist:

- NetworkId;
- network endpoint/configuration required for restart;
- durable desired channels;
- configured IRC identity values;
- restart-required authentication material under secret-handling rules;
- ClientId lineages;
- BufferId;
- HistoryEvent;
- per-client playback cursors;
- per-buffer operator read marker.

Do not persist as restart authority:

- ConnectionGeneration;
- current registration phase;
- current nick observations beyond durable configured intent where applicable;
- self_channels;
- live member/topic/mode state;
- join_attempts;
- PING/liveness state;
- reconnect timer state;
- live response correlations;
- SessionId.

Restart creates fresh live owners from DesiredState.

## 6. Identity model

Current `ClientId` is canonically durable for history/read-state purposes, but the current runtime also uses it as one accepted stream identity.

That conflation must end in M003.

Add:

- `SessionId`: ephemeral ID for one attached local stream.

Use:

- `ClientId`: durable lineage/device/profile identity used for playback cursors;
- `SessionId`: live CAP/registration/output queue/response-route ownership.

This prevents a late reply from a disconnected session being delivered to a later connection that happens to use the same durable ClientId.

NetworkId remains durable. ConnectionGeneration remains ephemeral.

BufferId is durable and always scoped by NetworkId.

HistoryEventId is durable and provides canonical local order.

## 7. History event model

Minimum logical event record:

~~~text
HistoryEvent
- HistoryEventId / local sequence
- NetworkId
- BufferId
- local receive wall time
- optional upstream server-time
- optional upstream msgid
- direction/audience
- event class
- bounded canonical IRC payload/fields needed for replay
~~~

Local HistoryEventId is the authoritative total order.

Server time and msgid are preserved source metadata.

Do not key ordering solely by:

- server timestamp;
- msgid;
- downstream receipt timestamp.

Two messages may have equal timestamps, a server may omit time/msgid, and remote clock order is not a transaction order.

IDs used by durable cursors must not be reused after retention deletion.

## 8. Receive time

The current core has an injected monotonic clock for reconnect/liveness.

History also needs an injectable wall-clock source for:

- local receive timestamp;
- synthesized downstream server-time where upstream omitted it;
- deterministic history tests.

Keep wall time separate from monotonic time.

Store an unambiguous normalized UTC representation or integer epoch representation selected by the implementation plan.

Preserve the upstream `time` tag separately rather than using it as local canonical order.

## 9. Multi-Network ownership

Introduce a process-level owner, conceptually:

~~~text
BouncerRuntime
+-- StoreHandle
+-- NetworkCatalog
|   +-- NetworkSupervisor(NetworkId A)
|   +-- NetworkSupervisor(NetworkId B)
|   +-- ...
+-- downstream attachment routing
~~~

Each NetworkSupervisor continues to own exactly one Network's mutable live state.

Do not merge all networks into one shared mutex.

Network failures/reconnects remain independent.

The catalog owns start/stop/reconfiguration of supervisors and maps durable NetworkId to the current owner handle.

Global reconnect-budget work remains primarily M004 unless a minimal process-level primitive is required to avoid an obviously incorrect M003 many-network implementation.

## 10. Multi-client ownership

Replace the current zero-or-one attached client slot with separately owned downstream session tasks.

Each session owns:

- SessionId;
- associated durable ClientId;
- stream halves;
- decoder;
- CAP/registration state;
- bounded outbound control/normal queues;
- session-local label namespace;
- shutdown path.

Sessions submit typed intents to NetworkSupervisor.

NetworkSupervisor fans normalized upstream events to applicable sessions.

A slow/failed client is detached independently.

No session directly mutates NetworkState.

Fanout queues remain bounded.

## 11. Durable desired mutations

Durable DesiredState changes need crash-consistent ordering relative to upstream commands.

For a persistent JOIN requested by an operator/client:

1. validate;
2. commit desired-state mutation;
3. after persistence success, issue upstream JOIN;
4. observed membership still waits for server self JOIN.

For removal/PART:

1. commit removal from desired state;
2. after persistence success, issue upstream PART;
3. observed membership waits for server PART/KICK or generation loss.

If persistence fails, do not claim the durable mutation succeeded.

This prevents a successful upstream state change followed by process crash from silently reverting operator intent.

## 12. History ingestion versus network liveness

History writes must never be awaited in a way that blocks upstream read/control processing.

The runtime submits bounded history work to the store path.

If the history queue is full or the store fails:

- mark history/store health degraded;
- record bounded diagnostics/counters;
- continue upstream control/liveness;
- do not allocate an unbounded retry queue;
- do not falsely advance downstream history cursors.

Whether a particular message is omitted from durable history under pressure must be explicit and measurable.

M004 later stress-qualifies prolonged store pressure.

## 13. Outbound message history

Transport delivery ambiguity remains intact.

Preferred behavior:

- when upstream `echo-message` is available, the upstream echo becomes the canonical confirmed history event for the bouncer's own outgoing message;
- without upstream echo-message, local accepted outgoing traffic may be represented with explicit direction/delivery-confidence semantics if M003 chooses to store it;
- it must never be transformed into evidence of confirmed remote delivery merely because the local stream write completed;
- no history representation authorizes automatic retransmission after generation loss.

The M003 implementation plan should choose a minimal first policy and test it.

## 14. Playback cursor versus read marker

Two distinct durable concepts are needed.

### Per-client playback cursor

~~~text
(ClientId, BufferId) -> HistoryEventId
~~~

Meaning: the last history position successfully delivered through that durable client lineage's automatic legacy playback.

### Operator read marker

~~~text
BufferId -> HistoryEventId
~~~

Meaning: the latest message the operator has declared read, synchronized across their clients.

Read marker is shared because the first product has one Operator.

The two must never be conflated.

A legacy playback cursor advances only after downstream delivery completes according to the session-writer acknowledgment contract.

If a crash causes replay duplication, prefer duplicate delivery over a silent gap.

Retention needs an explicit rule for cursors pointing at deleted history, likely monotonic clamping to the oldest retained/next valid boundary.

## 15. Legacy playback

Legacy clients that do not negotiate chathistory may receive bounded automatic backlog after normal registration/current-state projection.

Playback requirements:

- explicit maximum event count/bytes;
- deterministic order by local HistoryEventId;
- no unbounded enqueue into a slow client;
- cursor advances only after successful session delivery;
- detached/failed session cannot advance cursor after cancellation.

A client negotiating the supported chathistory capability should not receive duplicate automatic legacy playback.

## 16. Labeled-response routing

When upstream supports labeled-response:

- downstream labels are never forwarded transparently;
- allocate a bounded generation-scoped upstream label;
- map it to SessionId, original downstream label, request class, and deadline;
- restore the original label downstream;
- remove mapping on completion/timeout/generation replacement/session detach;
- multi-message responses track associated BATCH until completion as required.

Do not encode durable ClientId in the upstream label.

Bound label/correlation count.

## 17. Fallback response routing

Without upstream labeled-response, do not use one generic FIFO for arbitrary commands.

For initially supported request families, use command-specific completion rules such as:

- WHOIS ending at 318;
- WHO ending at 315;
- NAMES ending at 366;
- LIST ending at 323.

The safest initial policy is one ambiguous unlabeled request at a time per command family or per Network, with explicit local busy/error behavior for a competing request.

Later evidence may widen safe concurrency.

All fallback correlation is generation-local and SessionId-scoped.

## 18. IRCv3 capability mediation for M003

Upstream policy should request foundational capabilities when advertised, independently of attached clients:

- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message;
- existing account/state capabilities already selected by the project where useful.

Downstream advertisement is based on semantics the bouncer itself can provide.

Likely M003 downstream foundation:

- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message where semantics are truthful;
- draft/chathistory;
- draft/read-marker.

Do not advertise a capability merely because upstream advertises it.

Upstream capability fingerprint remains independent of downstream client identity.

## 19. Chathistory adapter

Do not design the database around CHATHISTORY wire syntax.

Store API should expose generic bounded queries resembling:

- latest;
- before;
- after;
- between;
- around;
- targets.

The current draft adapter maps wire references to these generic queries.

The result order remains local HistoryEventId order with source metadata preserved.

Draft capability names/status remain isolated so protocol evolution does not force schema migration.

## 20. Read-marker adapter

The read-marker adapter resolves the current draft wire reference to local HistoryEventId and advances the durable Buffer read marker only monotonically.

Read state is private to the Operator's clients.

No upstream disclosure occurs unless a future explicitly reviewed protocol requires it.

## 21. Retention

M003 needs bounded retention from its first durable schema.

Required policy dimensions:

- configurable target;
- hard safety ceiling;
- bounded delete batch size;
- no FTS/search requirement;
- cursor/read-marker clamping behavior after old events are removed;
- bounded checkpoint/maintenance behavior.

Indexes should be selected for bounded Buffer-local sequence/range queries and msgid/time lookup required by the wire adapters.

## 22. Recommended implementation decomposition

### M003-A — Durable storage and identity foundation

Freeze:

- rusqlite dependency/features;
- store actor;
- SessionId;
- schema version 1;
- migrations;
- IDs;
- DesiredState load/mutation;
- wall clock;
- retention primitives.

### M003-B — Multi-network and multi-client ownership

Add:

- BouncerRuntime/NetworkCatalog;
- many supervisors;
- many simultaneous sessions;
- fanout;
- durable ClientId association;
- persisted desired JOIN/PART ordering.

### M003-C — History journal and client synchronization

Add:

- Buffer/HistoryEvent ingestion;
- retention;
- restart queries;
- playback cursor;
- read-marker storage primitive;
- bounded legacy playback.

### M003-D — Response routing and foundational IRCv3 mediation

Add:

- upstream/downstream message-tags/server-time/batch foundations;
- label translation;
- command-specific fallback routing;
- echo-message policy;
- multi-session concurrent request tests.

### M003-E — IRCv3 history/read-state adapters

Add:

- draft/chathistory adapter;
- draft/read-marker adapter;
- history query references;
- no-duplicate legacy/chathistory playback behavior;
- draft isolation.

### M003-F — Integrated qualification and M003 closure

Run:

- migration/restart/crash tests;
- multi-network independence;
- multi-client fanout/routing;
- store pressure;
- retention/cursor edge cases;
- IRCv3 conformance regression;
- Rust 1.88;
- static network-boundary verification.

## 23. Implementation readiness

There is no remaining external research blocker.

M003-A is dependency-ready now.

M003-B through M003-F should be planned now but remain blocked/ordered behind their direct predecessor so implementation cannot jump around the durable identity/storage contracts.

If M003-A discovers that the selected SQLite topology cannot satisfy bounded backpressure, migration atomicity, or the Rust 1.88 floor, stop and correct the storage decision before beginning M003-B.
