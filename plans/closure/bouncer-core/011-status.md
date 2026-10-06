# Bouncer Core M003-E Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/011-m003e-chathistory-and-read-marker-adapters.md`

Authority: Research 003 conformance results, Research 004, canonical capability-mediation requirements

Prior closure: `plans/closure/bouncer-core/010-status.md`

Repository planning baseline reviewed: `763ba338e755ec25c01edf648370df7c102c30bc`

Primary class: capability

## What was delivered

A versioned IRCv3 draft adapter for `CHATHISTORY` and read markers, with bounded queries in both event count and bytes, truthful replay output, monotonic read markers, no duplicate legacy/chathistory history, and every `draft/...` literal isolated in one module.

## Adapter isolation

All draft syntax — capability names, subcommand keywords, reference grammar, and the implemented spec revision — lives in `crates/runtime/src/chathistory.rs` and nowhere else.

The capability registry refers to the adapter's constants (`DOWNSTREAM_HISTORY`) rather than repeating the literals. The store and the rest of the runtime speak generic query keys and durable identities; they never see draft syntax. A future adapter update therefore ships without a schema migration unless the semantic data requirements actually changed.

`ADAPTER_REVISION` states the reviewed surface: `LATEST`, `BEFORE`, `AFTER`, `BETWEEN`, `TARGETS`, plus `draft/chathistory` and `draft/read-marker`.

Evidence: `the_draft_capability_literals_exist_only_in_the_adapter`.

## Implemented subcommand surface

| Subcommand | Status | Evidence |
|---|---|---|
| `LATEST` | implemented (newest page) | `results_are_ordered_by_local_identity_not_by_timestamp` |
| `BEFORE` | implemented | `a_stale_reference_fails_deterministically` |
| `AFTER` | implemented | same |
| `BETWEEN` | implemented | parser unit tests |
| `TARGETS` | implemented (oldest page) | parser unit tests |
| `AROUND` | **refused explicitly** | `an_unsupported_subcommand_is_refused_rather_than_degraded` |
| anything else | **refused explicitly** | same |

`AROUND` requires a bounded time index this milestone does not build. Refusing it is honest; answering it approximately would not be.

## Bounded queries

| Bound | Value |
|---|---|
| Events per query | ≤ `BacklogCap::DEFAULT.events` (50) |
| Bytes per response | ≤ 256 KiB |
| `LATEST` window scanned | ≤ 512 events |
| Reference candidates | ≤ 512 events |

Evidence: `a_query_is_bounded_in_events_and_bytes`, `a_history_query_does_not_block_network_liveness`.

An event that does not fit the remaining byte budget is skipped rather than truncated, and the reply reports `more_pending` so a client knows more exists.

## Canonical order and truthful output

Results are ordered by local `HistoryEventId`. `server-time` and `msgid` are preserved as metadata.

| Property | Evidence |
|---|---|
| Skewed `server-time` does not reorder results | `results_are_ordered_by_local_identity_not_by_timestamp` |
| Target and message type are preserved exactly | `a_replayed_line_carries_a_truthful_target_type_and_time` |
| `server-time` is always present (upstream or local receive) | same |
| A msgid is emitted only when genuinely preserved | `a_msgid_is_never_invented_from_a_durable_identity` |
| No membership event can appear in a replay | `no_membership_event_can_appear_in_a_replay` |

That last property is why no event-playback semantics were needed: only PRIVMSG/NOTICE is stored, so the adapter physically cannot offer what the store does not hold.

## Reference resolution

| Reference form | Resolution rule | Tie-break |
|---|---|---|
| `msgid` | exact match within the buffer | — |
| `timestamp=N` | newest event at or before N | largest `HistoryEventId` |

Ties resolve through local identity, never through a timestamp comparison. A reference to history that was never retained is refused with `StaleReference` rather than approximated with an adjacent event, because an adjacent answer would misrepresent what the client asked for.

Evidence: `a_stale_reference_fails_deterministically`, plus parser unit tests for malformed and over-long references.

A `HistoryEventId` is **never** exposed as a msgid: the durable identity would then mean something upstream clients interpret differently.

## Legacy duplication prevention

| Client | Initial synchronization | Evidence |
|---|---|---|
| legacy | bounded automatic backlog using the playback cursor | `a_chathistory_client_suppresses_the_duplicate_legacy_backlog` |
| `chathistory` | query-driven history, no automatic backlog | same |

`SessionCapabilities::with_negotiated` records that a session manages its own history, and `wants_backlog` then returns false. This is the hook M003-D reserved; Plan 010 explicitly did not advertise the capability, and Plan 011 is where it becomes real.

Cursor behavior is documented and tested: issuing a `CHATHISTORY` query does **not** by itself advance the playback cursor, because query delivery is not playback state. Evidence: `a_chathistory_query_and_the_legacy_backlog_cover_disjoint_history`.

Changing CAP state after registration does not replay initial history twice, because backlog delivery happens exactly once, at projection time.

## Read markers

| Property | Evidence |
|---|---|
| Marker moves only forward | `a_read_marker_moves_only_forward_and_is_shared_per_buffer` |
| Marker is shared per buffer for the Operator | same |
| Marker referencing pruned history clamps | `a_marker_reference_into_pruned_history_clamps_rather_than_failing` |
| `MARKREAD *` clears without touching upstream | `a_markread_clear_is_accepted_without_touching_upstream` |
| Refusals are deterministic and name a reason | `every_refusal_is_deterministic_and_names_a_reason` |

The clamp reuses M003-C's retention rule unchanged, so the marker stays valid rather than dangling. Read state is local to this Operator's clients and is never sent upstream.

## Determinism

Every refusal is an explicit enum variant, not a string or a silent empty answer: `UnsupportedSubcommand`, `InvalidLimit`, `InvalidReference`, `TooManyParameters`, `StaleReference`, `NoSuchBuffer`. Each renders a stable reason and never a payload.

`CHATHISTORY` and `MARKREAD` are answered locally and never forwarded upstream — the server has none of this history. Evidence: `history_and_read_marker_commands_are_answered_locally`.

## Network boundary review

Unchanged. History queries are durable local reads delivered to already-attached local clients. `scripts/check-network-boundary.py` passes unchanged.

## Correctness defects found and fixed during implementation

| Defect | Consequence if shipped | Fix | Evidence |
|---|---|---|---|
| `LATEST` returned the **oldest** events | a client asking for the latest history would silently receive the beginning of a buffer | `LATEST` now takes the tail of a bounded retained window | `results_are_ordered_by_local_identity_not_by_timestamp` |
| `render_one` re-parsed a stored payload that has no line terminator | every replay would have failed to render | reconstruct the frame (payload + CRLF) before re-parsing, and clear inherited tags first | `a_rendered_event_always_carries_a_truthful_server_time` |
| A vacuous `assert!(x.is_err() \|\| true)` | would have passed regardless of behavior | replaced with a real parse-time refusal assertion | `an_unsupported_subcommand_is_refused_rather_than_degraded` |

The `render_one` bug is the most important one: it would have made the entire replay path non-functional, and it was caught only because the test rendered a real stored payload rather than asserting on the parse result alone.

## Verification actually executed

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
./scripts/check-network-boundary.py
rustup run 1.88.0 sh scripts/verify.sh full
```

All passed. 239 workspace tests pass across 18 suites, including every pre-existing test and 16 new chathistory tests plus 8 new adapter unit tests.

## Unresolved findings

None blocking. Two items are explicitly deferred:

- `AROUND` needs a bounded time index. It is refused explicitly, not approximated, and would require a separate reviewed index design.
- The wire conformance corpus was not extended with draft chathistory cases. The adapter behavior is covered by unit and integration tests; extending the shared corpus is desirable but belongs to a separate reviewed change rather than being authored inside this plan.

## M003-F readiness decision

**M003-F is unblocked.** It is the integrated qualification and closure milestone, and every subsystem it must qualify now exists: the owned bounded store (M003-A), independent Network and session ownership (M003-B), durable history with acknowledged legacy playback (M003-C), truthful capability negotiation and SessionId-scoped routing (M003-D), and the bounded history/read-marker adapters (M003-E).

Two items M003-F explicitly owns are the remaining open questions across this milestone: the **store-pressure liveness** fixture (a stalled store while PING/PONG stays schedulable) and a **clean end-to-end multi-network restart** demonstration through the catalog. Both need the integrated harness this milestone provides, and neither has an unresolved design question blocking it.