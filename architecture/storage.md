# Durable storage

The bouncer's durable substrate is the `i2pr-irc-store` crate. One owned worker thread holds the only SQLite connection for the lifetime of a store, and every request crosses one explicitly bounded queue.

## Topology

```text
NetworkSupervisor / BouncerRuntime        (Tokio tasks)
        |
        | typed StoreHandle calls only
        v
StoreHandle  -> bounded mpsc (256)  ->  store worker thread
                                              |
                                              v
                                     rusqlite::Connection  (file, WAL)
```

The worker uses a blocking receive, so no SQLite call ever runs on a Tokio task. Callers cannot reach the connection: `StoreHandle` exposes typed operations, and the `Request` enum is closed, so neither SQL text nor a closure can be submitted from runtime or network input.

## Load and failure disposition

Submitting is `try_send`, never an await for capacity. A full queue returns `StoreErrorKind::QueueOverloaded` immediately, which is what keeps storage pressure from stalling PING/PONG or any other control traffic. `Stop` in an enqueue, and a dropped response path, are distinct from a typed SQLite failure.

Every mutation reports an explicit [`CommitState`]. A caller that loses its response path cannot infer rollback from cancellation: SQLite may already have committed. `CommitState::Unknown` names that case so durable state is re-read rather than assumed.

`StoreHandle::flush()` is a FIFO barrier — answering it proves every earlier request has been answered — which makes bounded-load tests deterministic without sleeping.

Shutdown sets a closing flag, wakes the worker through a dedicated capacity-1 channel (so a stop can never wait on a full request queue), drains work already accepted, and joins the thread. The wakeup is necessary because the request channel stays connected while other `StoreHandle` clones exist.

## Schema version 1

Schema 1 is defined in `schema.rs` as SQL, not as a serialized Rust value graph, so neither draft IRCv3 syntax nor internal Rust representation can dictate a migration.

| Table | Purpose |
|---|---|
| `networks` | durable Network configuration |
| `network_secrets` | restart-required SASL material, typed separately |
| `desired_channels` | durable operator intent, ordered |
| `clients` | durable client lineage |
| `buffers` | stable per-Network buffer identity |
| `history_events` | bounded durable history |
| `client_cursors` | per-`(ClientId, BufferId)` playback position |
| `read_markers` | per-`BufferId` operator read state |

Every table is `STRICT`, so a value of the wrong storage class is rejected by SQLite rather than coerced into a row that later means something different. Foreign keys are enabled at open, and a removed Network cascades to its buffers, history, cursors, and markers so a reused identity cannot inherit orphaned rows.

`history_events.event_id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, which is monotonic and never reuses a deleted rowid. That is what makes a retained cursor unable to alias a different event after retention deletes older rows.

## Open policy

Open validates before serving anything: application identity (`application_id`), schema version (`user_version`), that the promised tables actually exist, and that the bundled SQLite supports `STRICT` (3.37.0+). A database this build cannot serve is a startup failure — `ForeignDatabase`, `SchemaTooNew`, or `Corrupt` — never a condition the bouncer works around. Schema creation runs in one transaction, so a partially migrated database is never accepted.

`PRAGMA` settings at open: `foreign_keys=ON`, `journal_mode=WAL`, `synchronous=FULL`, and a bounded `busy_timeout` of 5s so lock contention fails explicitly instead of parking a thread. Relaxing `synchronous` needs measured evidence and a separate reviewed decision.

Corrupt durable rows are rejected with the same domain validation applied to fresh input. The store never synthesizes a plausible default, because silently repairing an identity is worse than refusing to start.

## Desired versus observed state

Storage owns Network configuration, durable desired channels, client lineage, buffer identity, history, cursors, and read markers.

Storage is **not** authority for `ConnectionGeneration`, registration phase, joined membership, members/topics/modes, pending or rejected JOIN attempts, response correlations, `SessionId`, or backoff/liveness timers. No table in schema 1 describes them, so there is nothing for a restart to restore: restart rebuilds fresh supervisors and fresh ObservedState, then reconciles stored intent.

## Secrets

`StoredSecret` renders as `StoredSecret([redacted])`, zeroes on drop, and exposes its value only through an explicitly named `expose()` used by reconnect authentication. `NetworkRecord`'s `Debug` therefore never contains a password. SQLite error messages are discarded and replaced with a typed kind, because a driver message can echo a bound value.