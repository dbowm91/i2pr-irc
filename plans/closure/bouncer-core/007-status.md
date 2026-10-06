# Bouncer Core M003-A Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/007-m003a-durable-storage-and-identity-foundation.md`

Authority: ADR-0002, `plans/research/004-m003-storage-multiclient-history-research.md`

Prior closure: `plans/closure/bouncer-core/006-status.md` (Corrective 006)

Repository planning baseline reviewed: `0b44442de3b7285d6be2ef08ba82f27936eef988`

Primary class: infrastructure

Implementation commit: recorded in the M003-F closure commit range; this record is updated with the exact hash at closure.

## What was delivered

The `i2pr-irc-store` crate, the `SessionId`/wall-clock additions to `i2pr-irc-core`, and the extension of the static network-boundary guard to cover the store crate.

This is infrastructure, not a user-visible capability. Per the planning process, infrastructure alone does not close a capability, so M003 remains open behind Plans 008-012.

## Queue topology and capacity

```text
StoreHandle  ->  tokio::mpsc::Sender<Request>  (capacity 256, explicit)
             ->  store worker thread (one Connection, blocking receive)
```

| Property | Value | Evidence |
|---|---|---|
| Ingress ceiling | `STORE_QUEUE_CAPACITY = 256` | `the_ingress_queue_is_bounded_and_overload_is_typed`, `handle.queue_capacity()` |
| Enqueue policy | `try_send`, never awaited for capacity | same test observes both acceptance and typed refusal |
| Overload disposition | `StoreErrorKind::QueueOverloaded` | same test asserts the kind on every refusal |
| SQLite busy timeout | 5000 ms, bounded | `STORE_BUSY_TIMEOUT_MS` |
| Bounded history batch | 512 events | `MAX_HISTORY_BATCH` |
| Bounded history query | 512 events / 512 KiB | `MAX_HISTORY_QUERY_EVENTS`, `MAX_HISTORY_QUERY_BYTES` |
| Bounded retention pass | 4096 events | `MAX_RETENTION_DELETE` |
| Bounded networks / clients / buffers | 64 / 64 / 1024 per network | `MAX_NETWORKS`, `MAX_CLIENTS`, `MAX_BUFFERS_PER_NETWORK` |

No secondary unbounded queue exists anywhere on the store path. The rejected `tokio-rusqlite` worker was never adopted, so the unbounded crossbeam channel identified in ADR-0002 is absent from the tree.

## Dependency and SQLite feature review

`cargo tree --locked -e normal,build -p i2pr-irc-store`:

```text
rusqlite v0.40.2 (default-features = false, features = ["bundled"])
  bitflags, fallible-iterator, smallvec
  libsqlite3-sys v0.38.2  [build] cc, pkg-config, vcpkg, find-msvc-tools, shlex
i2pr-irc-core, i2pr-irc-wire (path)
```

`libsqlite3-sys` build scripts execute locally only (compile and link SQLite) and perform no network access. `scripts/check-network-boundary.py` scans this dependency tree and finds no forbidden egress package.

Bundled was verified rather than assumed: the linked test binary shows **no system SQLite linkage**, so supported binary behavior does not depend on a host library. The `tokio-rusqlite` and connection-pool alternatives were rejected in ADR-0002 and are absent.

## Schema version 1

| Table | Ownership enforced |
|---|---|
| `networks` | durable NetworkId + configuration |
| `network_secrets` | separate typed secret table, cascades with its Network |
| `desired_channels` | ordered durable intent, unique per (network, casemapped key) |
| `clients` | durable client lineage, unique login |
| `buffers` | stable per-Network identity, unique per (network, canonical key) |
| `history_events` | `event_id INTEGER PRIMARY KEY AUTOINCREMENT` |
| `client_cursors` | unique per (client, buffer) |
| `read_markers` | one per buffer |

All tables are `STRICT`.

`AUTOINCREMENT` is the load-bearing choice: SQLite's rowid allocator would reuse a deleted maximum rowid, which would let a retained cursor silently alias a *different* event after retention. `AUTOINCREMENT` keeps the high-water mark in `sqlite_sequence`, so identity is never reused. Evidence: `history_event_identity_is_never_reused_after_deletion`.

## Migration and restart matrix

| Scenario | Disposition | Evidence |
|---|---|---|
| Fresh file | schema 1 created in one transaction | `fresh_database_creates_exactly_schema_version_one` |
| Reopen current database | reused, not re-created | `reopening_a_current_database_reuses_it` |
| Another application's file | `ForeignDatabase` at startup | `an_unrecognized_database_is_refused_at_startup` |
| Newer `user_version` | `SchemaTooNew` at startup, never downgraded | `a_newer_schema_version_is_refused_at_startup` |
| Our version, table missing | `Corrupt` at startup | `a_database_claiming_our_version_but_missing_tables_is_refused` |
| Wrong storage class | rejected by `STRICT` | `strict_tables_reject_a_wrong_storage_class` |
| Orphaned child row | rejected by foreign keys | `foreign_keys_reject_an_orphaned_child_row` |

A database this build cannot serve is a **startup failure**, not a runtime condition the bouncer works around, and a partially migrated database is never accepted because creation runs in one transaction.

## Durable versus live state matrix

| State | Durable | Restored on restart | Evidence |
|---|---|---|---|
| Network configuration | yes | yes | `durable_identities_survive_reopen` |
| Desired channels | yes | yes | `desired_state_survives_restart_and_no_observed_state_does`, `removing_desired_state_is_durable` |
| SASL material | yes, redacted | yes | `secrets_never_appear_in_diagnostics` |
| Client lineage | yes | yes | `durable_identities_survive_reopen`, `distinct_clients_get_distinct_lineages` |
| Buffer identity | yes | yes | `two_clients_keep_independent_cursors_on_one_buffer` |
| History order | yes | yes | `canonical_order_is_local_identity_not_wall_time` |
| Playback cursor | yes | yes | `cursors_and_read_markers_move_only_forward` |
| Read marker | yes | yes | same |
| `SessionId` | **no** | no | `SessionId` is not in the schema and has no persistence path |
| `ConnectionGeneration` | **no** | no | absent from schema |
| Observed membership, topics, modes | **no** | no | asserted absent by name in `desired_state_survives_restart_and_no_observed_state_does` |
| Pending/rejected JOIN attempts | **no** | no | absent from schema |
| Response routes | **no** | no | absent from schema |

The absence proof is structural rather than incidental: the test asserts that no table named `sessions`, `generations`, `members`, `topics`, `modes`, `join_attempts`, `routes`, or `self_channels` exists, so there is nothing for a restart to restore.

## Identity allocation evidence

- `NetworkId`, `ClientId`, `BufferId`, and `HistoryEventId` are opaque local integers, not hashes of display strings.
- `HistoryEventId` is monotonic and non-reusing, proven across a delete-and-reinsert cycle.
- `SessionId` is allocated from a bounded local allocator, never wraps, reports exhaustion, and reissues nothing: `session_identity_is_ephemeral_bounded_and_never_reissued`, `session_identity_exhaustion_is_reported_not_wrapped`.
- `ClientId` and `SessionId` are distinct types with distinct meanings; a durable client reconnecting receives a fresh session identity.
- Cursors and read markers advance monotonically: `cursors_and_read_markers_move_only_forward`.
- Retention clamping is deterministic — a position inside the removed range is pulled to the newest surviving event below it, or to `0`: `retention_clamps_positions_that_point_into_the_removed_range`.

## Secret review

| Property | Result | Evidence |
|---|---|---|
| `StoredSecret` Debug redaction | pass | `secrets_never_appear_in_diagnostics` |
| `NetworkRecord` Debug contains no password | pass | same |
| Secret survives restart for reconnect | pass | same |
| SQLite error text discarded | pass | `sql()` drops the driver message, which can echo bound values |
| Startup failures carry a kind, not free text | pass | `open_and_migrate` returns typed `StoreErrorKind` |
| No payload/endpoint in store diagnostics | pass | `StoreHealth` is a four-variant enum |

## Store failure and overload disposition

- Overload, worker stop, SQLite failure, and incompatible schema are **distinct** typed errors.
- Every mutation reports an explicit `CommitState` (`Committed` / `RolledBack` / `Unknown`), so a canceled caller is never told, or able to assume, that a transaction rolled back. Evidence: `commit_state_is_explicit_and_never_implied_by_cancellation`.
- A canceled request that drops its reply does not fail the store: the worker answers nothing and stays `Ready`.
- Shutdown drains accepted work and joins the worker; a handle left over afterwards refuses new work with `Stopped`. Evidence: `shutdown_answers_in_flight_work_and_joins_the_worker`, `a_handle_to_a_stopped_store_refuses_new_work`.
- `flush()` is a FIFO barrier, making bounded-load tests deterministic without sleeping.

## Correctness defects found and fixed during implementation

These were found by this plan's own tests and are fixed with regression evidence:

| Defect | Consequence if shipped | Fix | Evidence |
|---|---|---|---|
| `WallTime::from_unix_seconds` used `i64::abs()` | panics in debug on a corrupt durable row carrying `i64::MIN` | unsigned-magnitude comparison | `wall_time_is_bounded_and_separate_from_monotonic_time` |
| `SystemWallClock` used `as i64` | lossy cast for extreme elapsed times | `i64::try_from` | same |
| Worker checked the shutdown signal only after a request that returned a stop | the store could never be joined; `join()` deadlocked | explicit capacity-1 wakeup channel plus a bounded park | `shutdown_answers_in_flight_work_and_joins_the_worker` |
| `PRAGMA SQLITE_VERSION` read through a quoted pragma name | returned no rows, so the STRICT-support check always failed and startup refused a valid database | use `rusqlite::version()`, the linked library version | `fresh_database_creates_exactly_schema_version_one` |
| Retention clamp targeted an impossible range | a cursor pointing into a removed range was left dangling | clamp to the newest surviving event below the range | `retention_clamps_positions_that_point_into_the_removed_range` |

## Network boundary review

The static boundary is unchanged. `scripts/check-network-boundary.py` now also scans the `store` crate — source, manifest, and full dependency tree — because it introduces third-party native code. A dedicated positive control proves the store's source and manifest scope independently, so this is verified coverage rather than an assumption.

The store opens a **filesystem path only**. It resolves no hostname, opens no socket, and offers no HTTP, proxy, or resolver surface.

## Verification actually executed

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
./scripts/check-network-boundary.py
./scripts/fuzz-smoke.sh
rustup run 1.88.0 sh scripts/verify.sh full
cargo tree --locked -e all -p i2pr-irc-store
```

All passed. Rust 1.88 compiles the bundled SQLite build script and runs the full store suite, confirming the dependency does not raise the MSRV.

## Unresolved findings

None blocking. Two items are explicitly deferred to later M003 plans rather than left implicit:

- Store-pressure *liveness* evidence (PING/PONG remaining schedulable while the store stalls) requires a store fixture the runtime does not own yet; it is Plan 012 qualification work.
- Automatic operator-configurable retention windows are M003-C; Plan 007 freezes the bounded primitive and its clamping rule only.

## M003-B readiness decision

**M003-B is unblocked.** The preconditions it names — a stable `StoreHandle` API, schema version 1 with `NetworkId`/`ClientId`, `SessionId`, durable desired network/channel load and mutation, and the restart contract — are all delivered and evidenced here. No Plan 008 requirement assumes storage behavior that does not exist.