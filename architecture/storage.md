# Durable storage

The bouncer's durable substrate is the `i2pr-irc-store` crate. One owned worker thread holds the only SQLite connection for the lifetime of a store, and every request crosses one explicitly bounded queue.

## Topology

```text
NetworkOwner (catalog-supervised)     (Tokio tasks)
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

## Schema version 4

The schema is defined in `schema.rs` as SQL, not as a serialized Rust value graph, so neither draft IRCv3 syntax nor internal Rust representation can dictate a migration. It is composed at runtime from the versioned `networks` body, the shared unchanged tables, the versioned `history_events` body, and a shared tail, because `concat!` cannot reference a const and each unchanged table must have exactly one definition.

| Table | Purpose |
|---|---|
| `networks` | durable Network configuration, including its operator-facing display name |
| `network_secrets` | restart-required SASL material, typed separately |
| `desired_channels` | durable operator intent, ordered, with its bouncer-owned presentation flag |
| `clients` | durable client lineage |
| `buffers` | stable per-Network buffer identity |
| `history_events` | bounded durable history |
| `client_cursors` | per-`(ClientId, BufferId)` playback position |
| `read_markers` | per-`BufferId` operator read state |

Every table is `STRICT`, so a value of the wrong storage class is rejected by SQLite rather than coerced into a row that later means something different. Foreign keys are enabled at open, and a removed Network cascades to its buffers, history, cursors, and markers so a reused identity cannot inherit orphaned rows.

`history_events.event_id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, which is monotonic and never reuses a deleted rowid. That is what makes a retained cursor unable to alias a different event after retention deletes older rows.

### What version 2 changed, and why

Version 1 stored `server_time` as integer epoch **seconds**. That cannot represent the wire protocol: a server-time is a UTC calendar timestamp with millisecond precision, so a conformant timestamp never parsed and a replayed one emitted an integer where the grammar requires `YYYY-MM-DDThh:mm:ss.sssZ`.

Version 2 makes `server_time` canonical validated text with a GLOB check that rejects the old integer shape at the storage layer, so a regression cannot be reintroduced by a writer that bypasses the Rust type.

`received_at` deliberately stays local whole seconds. It is diagnostic metadata and was never a protocol value; upgrading it would invent precision the process does not have. Only `server_time` carries protocol fidelity.

### Migrating version 1

A version 1 database on disk is real and evidence-closed, so the migration runs on open inside one transaction: build the new table, copy in bounded batches of `MIGRATION_BATCH_ROWS`, swap, and commit. A failure rolls the whole thing back, leaving the version 1 database untouched and still openable — tested by occupying the staging table name and asserting the version stays 1, the rows survive, and a later open after removing the obstruction migrates cleanly.

`AUTOINCREMENT` survives because every `event_id` is copied explicitly, which moves `sqlite_sequence` forward and keeps a retained cursor from later aliasing a different event.

A version 1 `server_time` outside the four-digit year window migrates as `NULL` rather than failing the migration. Version 1 accepted any integer within ±32.5e9 seconds, reaching back before year 1; under the protocol an inexpressible timestamp means "no timestamp", so the row and its canonical `HistoryEventId` order survive without one. Sub-second precision that version 1 already discarded is explicitly **not** invented backwards.

### What version 3 added, and why

Version 3 adds `networks.display_name`: the operator-chosen label a Network is listed under in operator-facing output.

It is display only. It never participates in lookup, routing, or identity, and it is deliberately *not* derived from the endpoint, the nickname, or any local path — a display name is operator-facing text and must not leak the bouncer's upstream identity into list output.

Existing rows receive `network-<id>`, derived only from the durable `NetworkId` the row already carries. That is deterministic, stable across restarts, and carries no endpoint, nick, path, or machine detail. `fallback_display_name` is the single definition, so a writer cannot invent a second spelling.

The value is validated as a single bounded IRC token (ASCII graphic, no space, no parameter separator, at most `MAX_DISPLAY_NAME_BYTES`) because it is interpolated into operator-facing numeric replies and must not be able to change how the surrounding reply parses.

### What version 4 added, and why

Version 4 adds `desired_channels.detached`: whether a channel the bouncer still holds is presented to attached sessions.

Desired membership and detached presentation are separate durable facts, and storing the second one is what makes it survive a restart. A derived rule — "hidden because something is happening right now" — would restore a different policy after a restart than the one the Operator set, and would let a reconnect disagree with what every client was just shown.

The flag is constrained to `0` or `1` at the storage layer. A value outside that range is refused on read as `Corrupt` rather than coerced, because guessing which end of an unrecognised value was meant would show or hide a channel nobody chose. The store also refuses to open a database that claims the current version but has lost a promised column: a table that survived a migration without `detached` would be served as though every channel were attached.

A detached channel is still a desired channel. `detach` is a single `UPDATE`, not a delete-and-reinsert, so the durable position never moves; detaching and reattaching cannot reorder a Network's channel list.

`NetworkRecord::validate` requires positions to be strictly increasing. Order is therefore total and a reloaded list means exactly what the saved one meant, without depending on how a list happened to be built. Gaps are allowed, because removing one channel must not renumber the others. Targets are casemap-unique under Rfc1459, matching the durable primary key, so a duplicate is reported before SQLite sees it.

### Migrating version 3

The v3 → v4 step is a single `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT 0 CHECK (detached IN (0, 1))`, inside the same migration transaction as every other step. `ADD COLUMN` cannot rebuild the table, so the column is appended rather than inserted; SQLite permits `NOT NULL` on an added column exactly when the default is not `NULL`, which is what makes the one-statement migration sound.

Existing rows become **attached**. That is the only safe default: a channel that was joined before this build existed has been presented to clients this whole time, and marking it detached would remove a channel from every client's view without anyone having asked.

The `CHECK` reads an out-of-range value as corruption rather than as a policy, so a row that reached the table through a future writer, a repaired dump, or a hand-edited file is refused instead of being interpreted.

### Migrating version 2

The v2 → v3 step is an `ALTER TABLE ... ADD COLUMN` plus a fill, in the same transaction as the rest of the migration chain. The column is added with an empty default because SQLite forbids a non-constant column default, and every row is then set to `network-<id>`.

Because the fill happens inside the migration transaction, a reader can never observe a mixture of migrated and unmigrated names: either the whole migration commits, or the database stays at version 2 with no column at all. A step that cannot complete leaves the version 2 database untouched and still openable.

Migration steps are applied in order, one version at a time, so a database several versions behind walks the same path it would have taken on each intervening release rather than jumping.

## Open policy

Open validates before serving anything: application identity (`application_id`), schema version (`user_version`), that the promised tables actually exist, and that the bundled SQLite supports `STRICT` (3.37.0+). A database this build cannot serve is a startup failure — `ForeignDatabase`, `SchemaTooNew`, or `Corrupt` — never a condition the bouncer works around. Schema creation and migration each run in one transaction, so a partially migrated database is never accepted.

A version newer than `SCHEMA_VERSION` is refused at startup. A version between `MIN_SUPPORTED_SCHEMA_VERSION` and the current one is migrated forward; an older one is refused rather than guessed at.

`PRAGMA` settings at open: `foreign_keys=ON`, `journal_mode=WAL`, `synchronous=FULL`, and a bounded `busy_timeout` of 5s so lock contention fails explicitly instead of parking a thread. Relaxing `synchronous` needs measured evidence and a separate reviewed decision.

Corrupt durable rows are rejected with the same domain validation applied to fresh input. The store never synthesizes a plausible default, because silently repairing an identity is worse than refusing to start.

## Desired versus observed state

Storage owns Network configuration, durable desired channels, client lineage, buffer identity, history, cursors, and read markers.

Storage is **not** authority for `ConnectionGeneration`, registration phase, joined membership, members/topics/modes, pending or rejected JOIN attempts, response correlations, `SessionId`, or backoff/liveness timers. No table in the schema describes them, so there is nothing for a restart to restore: restart rebuilds fresh supervisors and fresh ObservedState, then reconciles stored intent.

## Secrets

`StoredSecret` renders as `StoredSecret([redacted])`, zeroes on drop, and exposes its value only through an explicitly named `expose()` used by reconnect authentication. `NetworkRecord`'s `Debug` therefore never contains a password. SQLite error messages are discarded and replaced with a typed kind, because a driver message can echo a bound value.