# Bouncer Core M003-C Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/009-m003c-history-journal-cursors-and-legacy-playback.md`

Authority: ADR-0002, `plans/research/004-m003-storage-multiclient-history-research.md`

Prior closure: `plans/closure/bouncer-core/008-status.md`

Repository planning baseline reviewed: `646937e0ae063b189547e8733270553519325c55`

Primary class: capability

## What was delivered

A per-generation durable history journal (`HistoryJournal`), monotonic per-`(ClientId, BufferId)` playback cursors, a distinct per-`BufferId` operator read marker, bounded legacy automatic backlog with writer-acknowledged cursor advance, bounded retention in bounded chunks, and a documented failure/restart contract.

## Chosen policy: local outgoing messages are omitted

Plan §6 offered two minimal policies for local outgoing PRIVMSG/NOTICE before `echo-message` arrives in M003-D. This plan chooses **omission**:

> Local outgoing PRIVMSG/NOTICE are omitted until an upstream echo confirms them.

The reason is that a local socket write is not evidence of upstream delivery. A disconnect can leave delivery ambiguous, so recording a local write would label an unconfirmed write as history. Omitting is the only choice that cannot produce a false confirmation claim. When M003-D adds `echo-message`, the upstream echo becomes the canonical confirmed event instead, and the local omission simply stops mattering.

Evidence: `local_outgoing_messages_are_omitted_until_an_upstream_echo_exists`.

## Ingestion policy

| Line class | Outcome | Rationale |
|---|---|---|
| inbound PRIVMSG | recorded | conversation |
| inbound NOTICE | recorded | conversation |
| numeric (e.g. `353`) | skipped | this bouncer's own reply, not conversation |
| JOIN / PART / QUIT | skipped | event playback is explicitly out of scope for M003-C |
| PING / PONG | skipped | transport |

Evidence: `only_inbound_chat_is_recorded_as_history`.

## Canonical order and metadata

`HistoryEventId`/local sequence is canonical order. `server-time` and `msgid` are stored as metadata and never participate in ordering.

Evidence: `canonical_order_is_local_sequence_not_any_timestamp` records four events with receive time advancing while `server-time` runs backwards, and asserts local order plus exact payload order. `server_time_and_msgid_are_preserved_as_metadata_only` proves both survive durable round-trip.

`Message::time` / `Message::msgid` were added to the wire crate for this. They are deliberately metadata-only accessors: an unparsable `time` is absent rather than zero, and a value outside the representable window is refused rather than trusted.

## Durable schema, not an opaque value graph

Events are stored as individual bounded columns with a text protocol payload. No opaque Rust enum serialization exists in the schema, so neither draft IRCv3 syntax nor internal representation can force a migration.

The stored payload is protocol content **without** its line terminator, and CR/LF/NUL are rejected on ingest. Replay refuses a payload containing CR or LF rather than stripping it, so a stored payload can never be split into extra lines downstream.

Evidence: `a_retained_event_round_trips_through_the_store_unchanged`, plus the store-level `history_event_payload_and_class_are_bounded` and playback unit tests.

## Buffer identity

| Property | Evidence |
|---|---|
| A channel and a query never share a buffer | `a_channel_and_a_query_are_never_the_same_buffer` |
| Repeated resolution is stable across case | same |
| A lineage must exist before a cursor may reference it | `cursors_are_per_client_and_move_monotonically` |

Durable resolution uses the live generation's casemapping. `RuntimeError::AmbiguousBuffer` exists for the case §5 requires — where a re-resolution would merge two existing durable identities — and the journal fails closed rather than merging.

## Cursors and read markers

| Property | Evidence |
|---|---|
| Cursors are per `(ClientId, BufferId)`, not per buffer | `cursors_are_per_client_and_move_monotonically` |
| A cursor never rewinds on a stale acknowledgement | same |
| Read marker is distinct from any cursor and shared per buffer | `a_read_marker_is_shared_per_buffer_and_moves_only_forward` |
| A backlog resumes from the durable cursor | `a_backlog_resumes_from_the_durable_cursor` |

## Legacy playback

Bounded in **both** event count and total bytes. An event that does not fit the remaining byte budget is not delivered at all — delivering it would overshoot the ceiling, and truncating it would send a different message than the one retained.

Evidence: `legacy_backlog_is_bounded_in_events_and_bytes` asserts both the event cap and that the byte budget genuinely bounds delivery.

A cursor advances **only** after the session writer reports the bytes reached the socket. `QueuedFrame::Ack` carries a one-shot acknowledgement that the writer sends after the write succeeds. A crash between write and commit therefore duplicates on restart, which is preferable to a silent gap.

A session capability hook (`SessionCapabilities`) is reserved now so a client that later negotiates `chathistory` can suppress automatic backlog without restructuring sessions. Evidence: `a_chathistory_capable_client_can_suppress_the_legacy_backlog`.

## Retention

Bounded chunks (`max_delete_per_pass`), a bounded number of passes per cycle (`max_passes_per_cycle`), and a hard safety ceiling validated against the store's own `MAX_RETENTION_DELETE`. A cycle reports `more_pending` so a caller schedules the next cycle rather than looping.

Evidence: `retention_runs_in_bounded_chunks_and_reports_remaining_work`.

The deterministic clamp rule, and both halves of it:

| Cursor position relative to removed range | Result |
|---|---|
| inside the removed range | clamped to the newest surviving event below it, or `0` when none exists |
| above the removed range | untouched — its event is still retained |

Evidence: `retention_clamps_a_cursor_into_the_removed_range_monotonically` asserts both, that the clamp target is `0` when the removed range began at the first retained event, and that the read marker uses the same rule.

## Store pressure

| Property | Evidence |
|---|---|
| A refused append is never reported as recorded | `store_failure_degrades_history_without_faking_delivery` |
| History loss is visible in bounded counters | same (`store_unavailable`, `append_refused`) |
| An unbounded batch is refused, not partially recorded | `an_unbounded_ingest_batch_is_refused_rather_than_partially_recorded` |
| Ingestion is bounded and non-blocking in the owner loop | `INGEST_QUEUE_CAPACITY`, `INGEST_BATCH_PER_TURN` |

Ingestion runs through a bounded queue drained at most `INGEST_BATCH_PER_TURN` items per owner loop turn. A full queue drops the event and increments `history_dropped` rather than buffering — an unbounded retry buffer would trade memory pressure for history that arrives too late to matter. Control traffic is never delayed by history work.

## Restart

`history_and_cursors_survive_restart` uses a **file-backed** store rather than the in-memory fixture: an in-memory database cannot outlive the process, so it could not demonstrate durability at all. It proves `BufferId`, cursor, and read marker all resume after reopening the same file.

## Network boundary review

Unchanged. History is durable local storage plus delivery to already-attached local clients; playback never writes upstream and never implies that history is proof of delivery. `scripts/check-network-boundary.py` passes unchanged.

## Correctness defects found and fixed during implementation

| Defect | Consequence if shipped | Fix | Evidence |
|---|---|---|---|
| `backlog` subtracted the payload size *after* pushing, so one event could overshoot the byte budget | the byte ceiling was not actually a ceiling | refuse any event that does not fit the remaining budget | `legacy_backlog_is_bounded_in_events_and_bytes` |
| `replay_frame` called `trim_end_matches` before checking for embedded newlines | a corrupted stored payload would be silently "repaired" into a valid frame, hiding corruption | refuse any payload containing CR/LF outright | `a_payload_carrying_its_own_newline_is_never_split_into_two_frames` |
| A cursor could be advanced for a client lineage that did not exist | foreign-key violation surfaced as a misleading protocol error | `ensure_client` resolves a durable lineage first; `Kind::Sqlite` maps to `InvalidConfig`, not `Protocol` | `cursors_are_per_client_and_move_monotonically` |
| `Message::time` was written against a `WallTime` the wire crate does not depend on | wire would have needed a dependency it must not have, for a value it does not own | wire returns bounded raw seconds; the runtime validates into `WallTime` | `server_time_and_msgid_are_preserved_as_metadata_only` |

Two test-authoring mistakes are also worth recording because they revealed the invariants were correct: tags placed after the command are literal parameters, not tags (so they were never parsed), and a cursor above a removed range must **not** be clamped.

## Verification actually executed

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
./scripts/check-network-boundary.py
rustup run 1.88.0 sh scripts/verify.sh full
```

All passed. 197 workspace tests pass, including every pre-existing M002/M003-B test and 18 new history tests.

## Unresolved findings

None blocking. Two items are explicitly deferred:

- Event playback for JOIN/PART/NICK and other non-chat classes is out of scope for this plan by design and is not scheduled here; it would need its own milestone decision because replaying membership events can create false history.
- Automatic outgoing echo history waits on `echo-message`, which M003-D introduces.

## M003-D readiness decision

**M003-D is unblocked.** The preconditions it names — a session writer that can report delivery, per-client capability negotiation, bounded queued frames on both control and normal paths, `echo-message` becoming the canonical confirmation of an outgoing message, and a bounded batch/label registry — are all available on the base this plan leaves behind. In particular the acknowledged `QueuedFrame` path and the reserved capability hook are exactly the substrate M003-D needs, and the journal's documented omission of local outgoing messages becomes correct the moment `echo-message` lands.