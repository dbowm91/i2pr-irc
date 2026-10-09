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

The worker uses a blocking receive, so no SQLite call ever runs on a Tokio task. Callers cannot reach the connection: `StoreHandle` exposes typed operations, and the `Request` enum is closed, so neither SQL text nor a closure can be submitted from runtime or network input. rusqlite is built against bundled SQLCipher with vendored OpenSSL, which serves both ordinary plaintext SQLite and explicitly keyed encrypted databases.

## Load and failure disposition

Submitting is `try_send`, never an await for capacity. A full queue returns `StoreErrorKind::QueueOverloaded` immediately, which is what keeps storage pressure from stalling PING/PONG or any other control traffic. `Stop` in an enqueue, and a dropped response path, are distinct from a typed SQLite failure.

Every mutation reports an explicit [`CommitState`]. A caller that loses its response path cannot infer rollback from cancellation: SQLite may already have committed. `CommitState::Unknown` names that case so durable state is re-read rather than assumed.

`StoreHandle::flush()` is a FIFO barrier — answering it proves every earlier request has been answered — which makes bounded-load tests deterministic without sleeping.

Shutdown sets a closing flag, wakes the worker through a dedicated capacity-1 channel (so a stop can never wait on a full request queue), drains work already accepted, and joins the thread. The wakeup is necessary because the request channel stays connected while other `StoreHandle` clones exist.

## Schema version 8

The schema is defined in `schema.rs` as SQL, not as a serialized Rust value graph, so neither draft IRCv3 syntax nor internal Rust representation can dictate a migration. It is composed at runtime from the versioned `networks` body, the shared unchanged tables, the versioned `history_events` body, and a shared tail, because `concat!` cannot reference a const and each unchanged table must have exactly one definition.

`SCHEMA_VERSION` is 8. `MIN_SUPPORTED_SCHEMA_VERSION` is still 1, so every database written since M002 migrates forward in place rather than being refused.

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
| `registration_actions` | ordered phased registration-action list, one row per Network position |
| `history_search` | FTS5 **side index** over searchable history; `history_events` remains the source of truth |

`history_search` is a virtual table, so its `…_data`, `…_idx`, `…_docsize`, `…_content`
and `…_config` shadow tables are excluded from the promised set: they are SQLite's own
storage for the index, and listing them would make the promise depend on an implementation
detail of the SQLite build in use.

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

### What version 5 added, and why

Version 5 adds `networks.auto_away` and `networks.keep_nick`: the two durable halves of [presence and preferred-nick policy](presence-and-nick.md).

They are policy, not observation, which is exactly why they are durable while the away state and the live nick are not. `auto_away` is the Operator's standing instruction about when to be away; `keep_nick` is the standing instruction about whether to fight for a nickname. Both `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT 0`, so both migrate **disabled**.

Migrating them on would be the worst possible default: every existing Network would start emitting upstream `AWAY` and `NICK` traffic the Operator never asked for, purely because the binary changed. New upstream behaviour has to be something an Operator turns on, not something an upgrade does to them.

The automatic away text is deliberately *not* a column. It is a bouncer-owned constant, because an away message reaches an IRC-visible field on paths including upstream re-application after a reconnect, and a configurable string there is a control-character delivery problem waiting to happen. See [presence and preferred-nick policy](presence-and-nick.md).

A detached channel is still a desired channel. `detach` is a single `UPDATE`, not a delete-and-reinsert, so the durable position never moves; detaching and reattaching cannot reorder a Network's channel list.

`NetworkRecord::validate` requires positions to be strictly increasing. Order is therefore total and a reloaded list means exactly what the saved one meant, without depending on how a list happened to be built. Gaps are allowed, because removing one channel must not renumber the others. Targets are casemap-unique under Rfc1459, matching the durable primary key, so a duplicate is reported before SQLite sees it.

### What version 6 added, and why

Version 6 adds three things, all of them for [history search and indexed
references](history-search.md): the `history_search` FTS5 side index, the two relational
indexes reference lookup needs, and `history_events.effective_time`.

**The FTS rowid is the `HistoryEventId`.** That one decision makes retention exact —
deleting a retained row and deleting its index entry are one statement each, in one
transaction, by exact id, rather than a join that could half-succeed. An index row can
never name an event that does not exist. The index is written inside the append
transaction, so a message is retained-and-searchable or absent, never one without the
other.

**Two indexes replace scans.** `history_by_time` on
`(buffer_id, effective_time, event_id)` positions a `timestamp=` reference with two seeks,
and `history_by_msgid` on `(network_id, msgid)` resolves a `msgid=` without reading a
buffer. `network_id` leads the msgid index because an upstream identifier is only
meaningful within the Network that issued it.

**`effective_time` is the time an event occupies in history**, not the time the upstream
stamped it. An upstream that never sends `server-time` leaves every `server_time` NULL,
so an index over that column alone is empty for the whole buffer and every timestamp
reference fails against a buffer full of history. `effective_time` is the upstream stamp
when there was one and the local receive time converted to the same canonical text
otherwise.

It is a separate column rather than a write into `server_time`. That column records what
the upstream actually said, and inventing a value there would turn "the upstream sent no
timestamp" into a false claim. The rule has one definition,
`i2pr_irc_store::effective_time`, shared by the append path, the migration backfill, and
the runtime's `time=` tag rendering — a second copy is how a replayed message and a
timestamp reference would come to disagree about where a message sits.

**Every comparison against it binds canonical text.** `effective_time` is `TEXT` holding
fixed-width UTC, so lexicographic order is chronological order. Binding integer
milliseconds instead compares TEXT against INTEGER, and SQLite orders every TEXT value
after every INTEGER value regardless of the numbers involved — so the predicate matches
either everything or nothing while still looking like a time comparison. `recent_targets`
had exactly this bug and shipped it silently; `canonical_time` is now the only path by
which a time bound reaches SQL.

### What version 7 added, and why

Version 7 adds `registration_actions` alone, for [constrained post-registration
actions](operator-surfaces.md). Nothing existing is read, written, or rebuilt.

The table is keyed `(network_id, position)` and that key is the design decision. Replay order
is part of the meaning — an Operator who configured two actions wants them in the order they
wrote them — and a store returning rows in an unspecified order would replay them in an
arbitrary one. Making position part of the primary key settles the order at the storage layer
instead of trusting a `SELECT` to happen to preserve insertion order.

`kind` is `CHECK`-constrained to `('mode', 'message')`, the two shapes the runtime allowlist
constructs. The constraint is the point: a row written by a future build is refused by SQLite
rather than read back as an unknown kind that something downstream would have to guess at.

The `payload` column is a `TEXT` blob that may hold a service password, so the read path wraps
it in a `StoredSecret` before returning: no caller is handed an ordinary `String` that could be
printed. Storage cannot enforce redaction on the way out; the read path can, and does.

A read is `ORDER BY position` and bounded by `MAX_STORED_ACTIONS`, so a table holding more rows
than the runtime's ceiling is truncated rather than replayed — the ceiling is a property of the
model, and storage enforces the same one rather than assuming it.

Cascading on `networks` means a deleted Network takes its actions with it, for the same reason
it takes its buffers and history: a reused `NetworkId` must not inherit a list of commands it
never configured.

### What version 8 added, and why

Version 8 adds a constrained `phase` column to `registration_actions`. The values are
`pre-join`, `post-join`, and `fallback-recovery`; the migration defaults every existing row to
`post-join`, preserving the previous execution order and behavior. The schema check constraint
keeps unknown phase values out of storage, and the existing `(network_id, position)` key
continues to preserve list order. The migration from version 7 is additive and transactional.

### What versions 9 through 11 add

Version 9 adds `buffer_privacy`, keyed by stable `BufferId`, for explicit persistent,
ephemeral, or no-history overrides and bounded persistent age/event/byte ceilings. No
row means the legacy-compatible persistent default. Stricter modes set `purge_pending`
before deleting up to 4096 event and FTS rows in one transaction; history queries and
appends refuse the buffer while deletion is pending. The store worker resumes deletion in
bounded transactions, and the transition removes per-buffer cursor and marker references.
Version 10 adds `retention_pending`, which hides a buffer while newly tightened persistent
age/event/byte ceilings prune older rows in bounded batches. Append applies those ceilings
and keeps FTS rows, cursors, and markers consistent. This is logical deletion and does not
claim physical erasure from WAL files, backups, or snapshots.

Version 11 adds `history_events.search_indexed`. It records whether an event has derived FTS
fields, allowing opaque OTR ciphertext to remain in history without a searchable plaintext
row. Startup verifies that every marked event has exactly one FTS row and that no FTS row
points at an event marked unindexed.

Version 13 adds `watch_rules`, keyed by Network and bounded rule id. Target-specific rules
reference a stable `BufferId`; a rule without a buffer applies to every buffer of its kind.
Rules hold only the operator-selected literal matcher, kind, and scope; existing stores
migrate with no rules. They are not history events, notification text, or message-derived
search terms. Store validation caps each Network at 128 rules and each term at 128 bytes.
Network and buffer deletion cascade to the associated rules.

The runtime keeps ephemeral history in a process-local ring capped at 512 events, 1 MiB,
and 128 events per buffer. Eviction zeroizes retained payload and derived search fields;
process exit loses the ring. No-history avoids payload construction and durable writes.
Persistent OTR ciphertext may be retained as opaque payload but is excluded from derived
search fields. Per-buffer policy is administered locally through `BouncerServ` commands
`history status` and `history set`; `inherit` restores the persistent default.

### Migrating version 6

The v6 → v7 step is a single `CREATE TABLE`, inside the same migration transaction as every
other step. There is no backfill because there is nothing to reinterpret, which is why an older
bouncer binary pointed at a migrated database still sees exactly the configuration it had.

### Migrating version 5

The v5 → v6 step is four statements in one transaction, in dependency order: add
`effective_time`, create the two indexes over it, create the FTS table, then backfill.
The order matters because the index is defined over the column.

Both backfills are bounded at `MIGRATION_BATCH_ROWS` per step so a large retained journal
is never materialized at once, and both run inside the single migration transaction. A
partially backfilled index is worse than none, because it would answer searches with
results that silently stop partway through.

The search backfill decodes stored payloads with the same bounded decoder ingestion uses,
which is the one place the store carries protocol knowledge. Refusing to decode would mean
an upgraded database whose retained history was silently unsearchable. A payload that
yields no text is still indexed, empty — dropping it would make the index row count
disagree with the retained row count.

The effective-time backfill is not optional bookkeeping. A row left at the empty default
sorts *before* every real timestamp and becomes the oldest message in its buffer: a wrong
answer that looks like a correct one.

Open additionally refuses a database whose promised indexes are missing, and one whose
index row count disagrees with its retained searchable rows. An index that exists but has
lost rows would otherwise answer "no matches" — a degraded feature presented as a
complete one.

### Migrating version 3

The v3 → v4 step is a single `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT 0 CHECK (detached IN (0, 1))`, inside the same migration transaction as every other step. `ADD COLUMN` cannot rebuild the table, so the column is appended rather than inserted; SQLite permits `NOT NULL` on an added column exactly when the default is not `NULL`, which is what makes the one-statement migration sound.

Existing rows become **attached**. That is the only safe default: a channel that was joined before this build existed has been presented to clients this whole time, and marking it detached would remove a channel from every client's view without anyone having asked.

The `CHECK` reads an out-of-range value as corruption rather than as a policy, so a row that reached the table through a future writer, a repaired dump, or a hand-edited file is refused instead of being interpreted.

### Migrating version 2

The v2 → v3 step is an `ALTER TABLE ... ADD COLUMN` plus a fill, in the same transaction as the rest of the migration chain. The column is added with an empty default because SQLite forbids a non-constant column default, and every row is then set to `network-<id>`.

Because the fill happens inside the migration transaction, a reader can never observe a mixture of migrated and unmigrated names: either the whole migration commits, or the database stays at version 2 with no column at all. A step that cannot complete leaves the version 2 database untouched and still openable.

Migration steps are applied in order, one version at a time, so a database several versions behind walks the same path it would have taken on each intervening release rather than jumping.

## Open policy

Open validates before serving anything: application identity (`application_id`), schema version (`user_version`), that the promised tables and columns actually exist, that the bundled SQLCipher/SQLite supports `STRICT` (3.37.0+) **and** FTS5, that the reference indexes are present, and that the search index agrees with retained history. A database this build cannot serve is a startup failure — `ForeignDatabase`, `SchemaTooNew`, `Corrupt`, or a redacted key/backend error — never a condition the bouncer works around. Schema creation and migration each run in one transaction, so a partially migrated database is never accepted.

A version newer than `SCHEMA_VERSION` is refused at startup. A version between `MIN_SUPPORTED_SCHEMA_VERSION` and the current one is migrated forward; an older one is refused rather than guessed at.

`PRAGMA` settings at open: `foreign_keys=ON`, `journal_mode=WAL`, `synchronous=FULL`, and a bounded `busy_timeout` of 5s so lock contention fails explicitly instead of parking a thread. Relaxing `synchronous` needs measured evidence and a separate reviewed decision.

Corrupt durable rows are rejected with the same domain validation applied to fresh input. The store never synthesizes a plausible default, because silently repairing an identity is worse than refusing to start.

## Desired versus observed state

Storage owns Network configuration, durable desired channels, client lineage, buffer identity, history, cursors, and read markers.

Storage is **not** authority for `ConnectionGeneration`, registration phase, joined membership, members/topics/modes, pending or rejected JOIN attempts, response correlations, `SessionId`, or backoff/liveness timers. No table in the schema describes them, so there is nothing for a restart to restore: restart rebuilds fresh supervisors and fresh ObservedState, then reconciles stored intent.

## Secrets

`StoredSecret` renders as `StoredSecret([redacted])`, zeroes on drop, and exposes its value only through an explicitly named `expose()` used by reconnect authentication. `NetworkRecord`'s `Debug` therefore never contains a password. SQLite error messages are discarded and replaced with a typed kind, because a driver message can echo a bound value.

## Explicit encrypted-open policy

`Store::open` remains the explicit plaintext compatibility path. `Store::open_with_options` accepts either `StoreOpenOptions::plaintext()` or an encrypted option carrying a consumed `StoreKey` of exactly 32 caller-supplied random bytes. The store does not generate, derive, load, log, serialize, or retain the key in its schema. Its `Debug` output is redacted and the application-owned bytes and temporary hexadecimal encoding are zeroized after SQLCipher accepts the key. The SQLCipher connection retains the active cipher context only while the owned Store worker is alive.

Encrypted open applies the key before schema access, checks the non-secret `cipher_version` pragma, and reads `sqlite_master` to force decryption/authentication before schema validation or migration. A rejected key fails closed with a redacted `KeyRejected` classification. An encrypted database opened through the plaintext path fails as a startup error; encryption mode is never guessed, and no schema is created over unreadable bytes. Encryption-at-rest protects a closed database only while the key is kept separately from it; it does not protect a compromised live process or hide IRC metadata from the bouncer.

## Offline encrypted copy and key rotation

`export_encrypted_copy` is an offline operation: the caller must stop and join the
Store worker before calling it. It accepts the source's declared policy and a new
destination key, opens/migrates the source using the normal schema path, and uses
SQLCipher's `sqlcipher_export` to copy the complete database without materializing
history in application memory. The destination must be a distinct new sibling file;
it is created exclusively with mode `0600` on Unix. The exporter sets and checks the
application ID and schema version, syncs the destination and parent directory where
available, then reopens it through the ordinary keyed Store API so schema, indexes,
and FTS consistency are checked before success.

The source is never deleted or replaced. The same copy-and-verify path supports
plaintext-to-encrypted migration and old-key-to-new-key rotation. A wrong source key is
rejected before destination creation; failures after reservation remove the incomplete
destination and sidecars best-effort, while the source remains available under its
original policy. Schema migration of an older source occurs through ordinary startup
before export, so the source may be upgraded transactionally even when a later copy
fails. This API does not claim secure deletion or perform installation swaps.
