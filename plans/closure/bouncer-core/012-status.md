# Bouncer Core M003-F — Integrated Qualification and M003 Closure

Status: closed

Plan: `plans/implementation/bouncer-core/012-m003f-integrated-qualification-and-closure.md`

Source milestone: Bouncer Core M003

Closure scope: M003 (Plans 007–012) as one milestone.

## 1. Decision

M003 is closed. The repository now describes the bouncer core as a durable
multi-Network, multi-client IRC bouncer with bounded history and modern IRCv3
synchronization semantics, operating entirely through injected I2P stream providers with
no router dependency and no generic upstream path.

Router integration stays out of scope and remains blocked behind M005.

This record supersedes nothing. It aggregates the five predecessor records:

| Plan | Closure record | Subject |
|---|---|---|
| M003-A / 007 | `plans/closure/bouncer-core/007-status.md` | bounded owned SQLite store; SessionId/ClientId split |
| M003-B / 008 | `plans/closure/bouncer-core/008-status.md` | one owner per Network; sessions as tasks; persistence-first DesiredState |
| M003-C / 009 | `plans/closure/bouncer-core/009-status.md` | durable history journal; cursors; acknowledged legacy playback |
| M003-D / 010 | `plans/closure/bouncer-core/010-status.md` | SessionId-scoped response routing; truthful capability negotiation |
| M003-E / 011 | `plans/closure/bouncer-core/011-status.md` | bounded chathistory and read-marker adapters |

## 2. Implementation commit range

Planning and closure commits for the M003 sequence:

```
e820f6d Register M003 implementation sequence
0b44442 Link M003 architecture authority from roadmap
7f4d6c4 Decompose M003 into sequenced implementation handoffs
6f81cd3 Add the bounded owned SQLite store and durable identity foundation   (M003-A / 007)
646937e Own many networks independently and attach many clients per network  (M003-B / 008)
cb75659 Add a bounded durable history journal with acknowledged legacy playback (M003-C / 009)
763ba33 Add truthful capability negotiation and SessionId-scoped response routing (M003-D / 010)
9febdb3 Add bounded CHATHISTORY and read-marker adapters                     (M003-E / 011)
<this commit>                                                                  (M003-F / 012)
```

## 3. Final architecture

```text
                 NetworkCatalog (bounded: MAX_SUPERVISED_NETWORKS=64)
                    |  allocates SessionId (never reuses, never wraps)
                    v
        +-------------------------------------------+
        |  NetworkOwner  (one live owner per Network)|
        |    generation loop                        |
        |      upstream read --> apply once -->      |
        |        fanout --> each SessionHandle      |
        |        history --> bounded ingest queue   |
        |      upstream control/normal queues       |
        |      ResponseRouter (generation-local)    |
        +-------------------------------------------+
             |                    |                    |
             v                    v                    v
     SessionTask x N        HistoryJournal        StoreHandle (bounded 256)
     (own read half,        (buffer resolution,    |
      control 8 /            cursors, retention,   |  one owned worker thread
      normal 64 queues)      backlog)             |  one rusqlite connection
             |                    |                v
             v                    +----------> i2pr-irc-store
     I2pStreamProvider                      (SQLite schema v1, WAL,
      (injected; fake in every test)         synchronous=FULL, fk=ON)
```

Durable/live split, which is the load-bearing property:

| Durable (SQLite) | Live (never persisted) |
|---|---|
| network config, credentials, desired channels | membership, topics, modes, ISUPPORT |
| client lineage (`ClientId`) | `SessionId`, connection generation |
| channel buffers (`BufferId`) | open routes, batch state, labels |
| history events (`HistoryEventId`) | pending joins, rejected joins |
| per-client cursors, read markers | fanout queues, snapshot gauges |

No table and no API stores a session, a generation, a route, or a label. A restart
rebuilds fresh supervisors and fresh ObservedState; only intent and history return.

## 4. Schema version and migration matrix

`SCHEMA_VERSION = 1`, `APPLICATION_ID = 0x69327072`, all eight tables `STRICT`.

| Stored `user_version` | Disposition | Behaviour |
|---|---|---|
| `0` | fresh | schema created, opened |
| `1` | current | opened, served |
| `> 1` (`SchemaTooNew`) | **startup failure** | refused; never worked around at runtime |

`history_events` uses `AUTOINCREMENT`. This is load-bearing rather than incidental: a
plain rowid allocator would reuse a deleted maximum, and a retained cursor would then
silently alias a different event. Evidence: `crates/runtime/tests/integrated.rs`
(`an_incompatible_schema_is_a_startup_failure_not_a_runtime_state`,
`a_clean_restart_rebuilds_durable_intent_and_no_live_state`).

Driver: `rusqlite 0.40`, `default-features = false, features = ["bundled"]`. Verified
no system SQLite linkage in the built test binary (`otool -L`).

## 5. Resource-bound matrix

Every externally controlled quantity has an explicit ceiling.

| Bound | Value | Location |
|---|---|---|
| Store ingress queue | 256 | `store::STORE_QUEUE_CAPACITY` |
| Store busy timeout | 5 s | `store::STORE_BUSY_TIMEOUT_MS` |
| Store worker park | 25 ms | `store::worker::WORKER_PARK` |
| Supervised Networks | 64 | `catalog::MAX_SUPERVISED_NETWORKS` |
| Sessions per Network | 64 | `owner::MAX_SESSIONS_PER_NETWORK` |
| Total sessions | 1024 | `catalog::MAX_TOTAL_SESSIONS` |
| Session event queue | 64 | `session::SESSION_EVENT_QUEUE_CAPACITY` |
| Client control / normal queue | 8 / 64 | `CONTROL_QUEUE_CAPACITY` / `NORMAL_QUEUE_CAPACITY` |
| Upstream intent queues | 64 / 8 | same constants, upstream side |
| History ingest queue / batch per turn | 256 / 16 | `INGEST_QUEUE_CAPACITY` / `INGEST_BATCH_PER_TURN` |
| Automatic backlog | 50 events **and** 64 KiB, ≤32 buffers | `BacklogCap::DEFAULT`, `MAX_BACKLOG_BUFFERS` |
| Open response routes | 128, 20 s, ≤64 multipart replies | `routing::MAX_ROUTES`, `ROUTE_TIMEOUT`, `MAX_MULTIPART_REPLIES` |
| Label bytes (upstream / downstream) | 64 / 64 | `MAX_LABEL_BYTES`, `MAX_DOWNSTREAM_LABEL_BYTES` |
| Open batches | 64, depth 4, id 32 B, 128 messages | `ircv3::MAX_OPEN_BATCHES` … |
| CHATHISTORY targets / latest window / response | 64 / 512 / 256 KiB | `chathistory::MAX_TARGETS`, `MAX_LATEST_WINDOW`, `MAX_RESPONSE_BYTES` |
| Retention delete per pass / per cycle | 512 / 4 passes | `RetentionPolicy` |
| History payload / query events / query bytes | 4096 B / 512 / 512 KiB | `store::MAX_HISTORY_*` |
| SessionId space | 2^32, reports exhaustion, never wraps | `core::MAX_SESSION_IDS` |
| Wire line / tagged line / tags / params | 512 B / 8703 B / 128 / 15 | `wire::MAX_*` |

Evidence for max and max+1 behaviour: `crates/store/tests/qualification.rs`,
`crates/runtime/tests/multi_network.rs`
(`the_per_network_session_ceiling_refuses_the_attach_it_cannot_serve`),
`crates/runtime/tests/ircv3_routing.rs`, `crates/runtime/tests/chathistory.rs`.

## 6. Multi-Network and multi-client matrix

| Scenario | Evidence | Result |
|---|---|---|
| Many Networks supervise independently | `multi_network::many_networks_supervise_independently` | each owns its generation; no shared state |
| One Network's history/churn leaves others untouched | `integrated::one_networks_failure_history_and_client_churn_leaves_the_others_untouched` | byte-for-byte unchanged generation, history, fanout, and session counters |
| Many idle Networks × several clients | `integrated::many_idle_networks_and_clients_stay_within_deterministic_ceilings` | 6 Networks × 3 clients; returns to zero attached after churn |
| Session ceiling refuses rather than evicts | `integrated::the_per_network_session_ceiling_refuses_the_attach_it_cannot_serve` | 65th attach → `QueueOverloaded`; existing clients untouched |
| One client's queue pressure starves nobody | `integrated::one_clients_queue_pressure_never_starves_the_others` | 2 000 lines against a 2 KiB socket; healthy client still receives; loss counted |
| Same ClientId replacement inherits nothing | `integrated::a_same_client_id_replacement_inherits_nothing_from_the_first_attachment` | distinct SessionId; only the replacement attached |
| Stale session/generation events are inert | `multi_network` (existing) | unknown session dropped; earlier-generation intents dropped |

Note on the saturation response: a full fanout queue loses that one frame for that one
client and increments `fanout_dropped`. It does **not** detach. The queue is the
bouncer's own, so dropping a frame is the bounded response; detaching a client over an
internal queue would turn a momentary hiccup into a disconnect. The previous code and its
docs both claimed detachment that never happened; both are now truthful.

## 7. History, cursor, and retention matrix

| Property | Evidence |
|---|---|
| Canonical order is `HistoryEventId`, never a timestamp | `integrated::deterministic_ordering_holds_across_identical_and_skewed_timestamps` — reversed server times, identical receive times, one event with no msgid; order still ascending by local identity |
| msgid optional; `HistoryEventId` never exposed as a msgid | same test; `history::` suite |
| Durable payload carries no terminator; CR/LF refused | `playback::a_payload_carrying_its_own_newline_is_never_split_into_two_frames` |
| Cursor advances only after bytes reach the socket | `playback` ack path; `history::` suite |
| Cursor and read marker clamp identically and only forward | `integrated::read_marker_and_cursor_survive_retention_and_clamp_monotonically` |
| Retention is bounded per pass and reports `more_pending` | same; `store::RetentionReport` |
| Stale reference refused deterministically | same — `StaleReference` |
| Legacy playback bounded in events **and** bytes | `integrated::legacy_playback_and_chathistory_never_duplicate_initial_history` |
| Negotiated `chathistory` suppresses the automatic backlog | same — `wants_backlog()` false |
| Every line handed to ingestion is accounted for once | `integrated::store_pressure_degrades_storage_only_and_creates_no_side_queue` — recorded + skipped + dropped == flood size exactly |

**Defect found and fixed by this qualification.** The owner resolved a channel's
`BufferId` *before* applying the line to generation-owned state. The self JOIN is the
line that creates membership, so that line never resolved its own channel's buffer: a
channel recorded **no history at all** for the life of a generation unless the server
sent a second, redundant JOIN. The check now runs after `apply_line`. Regression
evidence: `one_networks_failure_history_and_client_churn_leaves_the_others_untouched`
asserts `history_recorded > 0` and fails against the old ordering.

## 8. Response-routing and capability matrix

| Property | Evidence |
|---|---|
| Identical downstream labels become distinct upstream labels | `integrated::concurrent_labeled_queries_from_several_sessions_route_correctly` |
| Out-of-order replies reach the asking session | same — replies delivered 3, 1, 2; each closed on its terminator |
| No generic FIFO; only WHOIS/WHO/NAMES/LIST correlate | `routing::RequestClass`; `ircv3_routing` suite (26 tests) |
| Generation replacement discards every route | `integrated::a_generation_replacement_discards_every_route` |
| Route ceiling, timeout, multipart bound | `ircv3_routing` suite |
| Upstream request set is client-independent | `capability::upstream_is_client_independent` |
| Downstream advertises only implemented semantics | `integrated::capability_advertisement_stays_truthful_after_the_history_adapter_landed` |
| All `draft/...` literals confined to one module | `chathistory::CHATHISTORY_CAPABILITY`, `READ_MARKER_CAPABILITY` only |
| Client tags default-deny | `ircv3::mediate_client_tags` |

Routes are generation-local and `SessionId`-scoped. Labels are translated, never
forwarded, and the generated upstream form (`i2p<16 hex>`) never encodes a `ClientId`.

## 9. Store-pressure and liveness evidence

Test-only fixtures: `Store::open_stalled` and `StoreHandle::set_stall`, both
`#[doc(hidden)]`, neither reachable from a production path. They exist so storage
pressure can be qualified deterministically instead of asserted.

| Scenario | Evidence | Result |
|---|---|---|
| Stalled store never starves control traffic | `integrated::a_stalled_store_never_starves_control_traffic` | 5 client PING/PONG round trips answered promptly at 150 ms/request; Network stays Online |
| Slow storage degrades storage only | `integrated::store_pressure_degrades_storage_only_and_creates_no_side_queue` | client still receives live traffic; upstream intent queues drain to zero; no retry or side queue |
| Shutdown with pending work completes | `integrated::shutdown_with_pending_work_completes_and_does_not_lose_the_queue` | 20 queued mutations drained and committed; reopen confirms durable intent; shutdown joins rather than hangs |

`shutdown` is a blocking join, so the test runs it on a blocking thread while the flood
keeps feeding the bounded queue — otherwise the assertion would be vacuous.

**Defect found and fixed by this qualification.** `attached_sessions`,
`upstream_normal_queue_depth`, `upstream_control_queue_depth`, and `response_routes`
were published only when the generation loop *exited*. For the entire working life of a
Network the snapshot therefore reported zero sessions, an empty queue, and no routes —
the untruthful-diagnostic case the roadmap forbids. These gauges are now published after
every turn, conditionally so an unchanged generation does not wake subscribers per line.

## 10. Security and dependency review

| Concern | Result |
|---|---|
| DNS / generic TCP / HTTP / proxy egress | none. `scripts/check-network-boundary.py` covers every first-party crate that could own network access, with a verified positive control per crate |
| Raw secret diagnostics | none. SASL payloads are `Zeroizing`; SQLite driver messages are discarded because they can echo bound values |
| Persisted `SessionId` / generation | none. `integrated::no_durable_row_records_a_session_id_or_generation` asserts the schema has no such table |
| `ClientId` disclosure through labels | none. Upstream labels are generation-local and derived, never encoded from durable identity |
| Unreviewed client-only tag forwarding | none. Default-deny unless the client negotiated `message-tags` |
| Arbitrary SQL or closure execution from network input | none. All statements are prepared and parameterised; `SQLITE_VERSION` is reported through `rusqlite::version()` because the pragma returns no rows through the driver |
| Raw protocol logging by default | none. Snapshots and health carry counters and identifiers only; `integrated::qualification_diagnostics_carry_no_secret_or_endpoint_material` |
| No system SQLite linkage | verified with `otool -L` |
| Dependency drift | `cargo tree --locked -e all` resolves; clippy and tests pass with `--locked` |

Full CTCP/DCC/anonymity qualification remains M004 and is not claimed here.

## 11. Verification

All commands run from the repository root at the closing commit.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clean |
| `cargo test --workspace --all-features --locked` | 312 passed, 0 failed |
| `cargo tree --locked -e all` | resolves |
| `scripts/check-network-boundary.py` | clean |
| `scripts/fuzz-smoke.sh` | clean |
| `rustup run 1.88.0 sh scripts/verify.sh full` | exit 0 |

MSRV remains Rust 1.88 (edition 2024). It was not raised.

Suite breakdown:

| Suite | Tests |
|---|---|
| `runtime/conformance` (M002/M003-B evidence) | 3 |
| `runtime/multi_network` | 16 |
| `runtime/history` | 18 |
| `runtime/ircv3_routing` | 26 |
| `runtime/chathistory` | 16 |
| `runtime/integrated` (this plan) | 18 |
| `store/qualification` | 23 |
| crate unit tests and doc-tests | remainder to 312 |

## 12. M003 exit conditions

| # | Condition | Evidence | Status |
|---|---|---|---|
| 1 | many supervisors fail/reconnect independently | `multi_network` suite; per-Network `Phase`/`Backoff` ownership | met |
| 2 | many clients attach without shared-state races | `integrated` multi-client matrix; sessions are tasks, owner holds only `SessionHandle` | met |
| 3 | desired channels survive restart and reconcile after registration | `integrated::a_clean_restart_rebuilds_durable_intent_and_no_live_state`; `reconcile` re-reads stored intent only | met |
| 4 | SQLite migration/restart behaviour proven | `store/qualification` (23) + schema disposition above | met |
| 5 | history order is deterministic | `integrated::deterministic_ordering_…` | met |
| 6 | slow storage cannot starve control traffic | `integrated::a_stalled_store_never_starves_control_traffic` | met |
| 7 | labeled-response routes concurrent replies correctly | `integrated::concurrent_labeled_queries_…` | met |
| 8 | bounded command-specific fallback without labels | `routing::RequestClass` with one outstanding query per family; no generic FIFO | met |
| 9 | per-client cursors are private and monotonic | `integrated::read_marker_and_cursor_survive_retention_and_clamp_monotonically` | met |
| 10 | server-time/batch/message-tags/echo-message policy proven | `ircv3_routing` (26) + `chathistory` (16) | met |
| 11 | draft chathistory/read-marker isolated from schema syntax | all `draft/…` literals confined to `chathistory.rs`; schema stores events, not draft names | met |
| 12 | legacy playback bounded and never duplicates negotiated chathistory | `integrated::legacy_playback_and_chathistory_never_duplicate_initial_history` | met |
| 13 | dependency/network/secret boundaries green | §10 | met |
| 14 | Rust 1.88 passes | `rustup run 1.88.0 sh scripts/verify.sh full` | met |

## 13. Defects found by this qualification

| # | Severity | Defect | Resolution |
|---|---|---|---|
| 1 | **high** | A channel resolved its `BufferId` only after a *second* self JOIN, so a channel recorded no history for the life of a generation. | Buffer resolution moved after `apply_line`. Regression test added. |
| 2 | **medium** | Live gauges (`attached_sessions`, both upstream queue depths, `response_routes`) were published only at generation teardown, so a healthy Network reported zero sessions and an empty queue throughout its life. | Published after every turn, conditionally. |
| 3 | **medium** | A full fanout queue was dropped silently, and the owner module doc, `SessionHandle::fanout` doc, and `downstream-session` row all claimed a detachment that never occurred. | Loss counted in `fanout_dropped`; all three doc sites corrected to describe bounded local loss. |
| 4 | low | Store worker shutdown could deadlock when the wakeup was only observed after a `false`-returning request. (Found by Plan 007's own tests.) | Dedicated capacity-1 wakeup channel plus a 25 ms park. |

No defect required a schema change, a new durable decision, a capability expansion, or a
security-boundary change, so no corrective or ADR was registered. Defects 1–3 were fixed
in this work line with regression evidence, as §6 permits.

## 14. Known limitations carried forward

These are deliberate and documented, not defects:

- **Local outgoing PRIVMSG/NOTICE are omitted from history** until upstream
  `echo-message` confirms them (Plan 009 policy). `echo-message` is advertised
  downstream only when upstream negotiated it.
- **`AROUND` is refused**, not approximated.
- **Draft capability syntax can change** without a schema migration, because the schema
  stores canonical events and never a draft name.
- **These gauges are sampled by the owner loop**, not atomic with the queues they
  describe. A reader wanting a settled value should compare a later turn.
- **Ingestion drains at the loop's turn rate**, never by spinning. Under sustained load
  the bounded queue may hold items until the next turn; every line is still accounted
  for.
- **No live router evidence exists.** Every test runs against injected fake I2P stream
  providers. That is the point, and it is also why M004 and M005 remain open.

## 15. M004 readiness decision

**M004 is unblocked and ready to decompose.** Its only stated dependency was M003 closure.

The next planning action is writing M004's implementation handoffs (anonymity and
adverse-network qualification) against the then-current repository state. It should
treat this record's §14 as its inherited assumption set, and §13 defect 1 — channels
recording no history — as the class of bug that only appears under a real network, where
timing is not fixture-controlled.

M005 remains sequenced behind M004. Router integration (R001–R003) remains blocked
behind M005. No router integration work is authorised by this closure.