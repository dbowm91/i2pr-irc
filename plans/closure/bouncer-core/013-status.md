# Corrective 013 — Post-M003 IRCv3 time/history and queue-integrity conformance

Status: closed

Corrects: `plans/implementation/bouncer-core/013-post-m003-ircv3-time-history-and-queue-integrity-corrective.md`

Repository baseline: `bdc9048d306068ae2427ebddd82513982b94e2d3`

This document is the strict current authority for Bouncer Core M004 readiness. It supersedes nothing: `plans/closure/bouncer-core/012-status.md` is preserved exactly as written and was not edited to suggest these defects were known at M003 closure.

## Implementation commits

| Commit | Scope |
|---|---|
| `1f341ee` | WP-A canonical `IrcTimestamp` and server-time conformance; WP-B schema v2 and the v1→v2 migration |
| `dc71abc` | WP-C CHATHISTORY grammar, references, ISUPPORT/BATCH/CAP contract |
| `06cb017` | WP-D live wiring — intents, owner handlers, TARGETS store op |
| `2dfd376` | WP-D remainder (initial + broadcast MARKREAD), WP-E upstream queue integrity, WP-F fanout detach |
| `83a147f` | WP-G queue-boundary qualification |

## Specifications reviewed

Re-checked at handoff start, as the plan's gate requires. The grammar had not moved materially and implementation proceeded.

| Extension | URL |
|---|---|
| server-time | https://ircv3.net/specs/extensions/server-time |
| chathistory | https://github.com/ircv3/ircv3-specifications/pull/503 (draft/chathistory, draft/chathistory-targets) |
| read-marker | https://github.com/ircv3/ircv3-specifications/pull/495 (draft/read-marker) |

Reviewed against the working drafts: canonical `YYYY-MM-DDThh:mm:ss.sssZ` at millisecond precision with `:60` legal in examples; positional CHATHISTORY grammar with the limit as the LAST parameter, `timestamp=`/`msgid=` selectors required, TARGETS taking no target; `MARKREAD <target> [<timestamp>]` where literal `*` is the server's unknown-marker sentinel and the server MUST send MARKREAD after JOIN and before RPL_ENDOFNAMES.

## C004-F1 — server-time was integer seconds

**Disposition: closed.**

`crates/wire/src/timestamp.rs` adds a hand-rolled bounded `IrcTimestamp`. It was not delegated to a date/time library because the wire grammar is not an epoch instant and a leap second is legal on the wire but silently normalised to `:59` by most calendar libraries — which would make replayed history disagree with the server.

- leap second accepted only at `23:59:60`, the sole position UTC permits one, rejecting `12:30:60` without a leap-second table;
- total ordering `:59.999 < :60.000 < :60.999 < next :00.000` via `sort_key() -> (i64, bool, u16)`, the leap flag compared *before* millis so the two leap instants stay distinguishable;
- real Gregorian calendar validation (Howard Hinnant `days_from_civil` / `civil_from_days`);
- `TIMESTAMP_BYTES = 24` byte ceiling, years 1–9999 only, UTC only, exactly three fractional digits.

`Message::time()` was **renamed** to `server_time()` returning `Option<IrcTimestamp>`, deliberately so every call site had to be revisited rather than silently keeping the old integer contract. `synthesize_server_time` was itself emitting the invalid integer tag and now emits canonical text.

39 wire unit tests cover ordinary milliseconds, `.000Z`, leap-second round trip, invalid month/day/hour/offset/precision, and boundary dates.

## C004-F1 — schema v2 and the v1→v2 migration

**Disposition: closed.**

`SCHEMA_VERSION = 2`, `MIN_SUPPORTED_SCHEMA_VERSION = 1`, `OpenDisposition::Migrated { from }`. The schema is composed at runtime from a shared head, the versioned `history_events` body and a shared tail, because `concat!` cannot reference a const and each unchanged table must have exactly one definition.

| Migration property | Evidence |
|---|---|
| v1 opens and migrates atomically | staging table + bounded batches of `MIGRATION_BATCH_ROWS = 256`, one transaction |
| whole seconds become valid canonical timestamps | `1970-01-01T00:00:00.000Z` style conversions, deterministic |
| history order unchanged | canonical order is `HistoryEventId`, never a timestamp |
| cursors and read markers stable | every `event_id` copied explicitly, moving `sqlite_sequence` forward |
| `AUTOINCREMENT` monotonicity preserved | explicit copy rather than implicit rowid reuse |
| failed migration leaves v1 intact | staging table name occupied → version stays 1, rows survive, later open migrates cleanly |
| newer schema still refused | `user_version = SCHEMA_VERSION + 1` → `SchemaTooNew` |
| v1 nulls stay null | covered |
| out-of-window values become absent, not a failure | `-32_503_680_000` (year 940) **is** representable and converts; `-100_000_000_000` becomes `NULL` |

Two deliberate non-inventions, recorded so they are not "fixed" later by mistake:

- **Sub-second precision v1 already discarded is not reconstructed.** Inventing it backwards would fabricate precision the source never had.
- **`received_at` stays local whole seconds.** It is diagnostic metadata and was never a protocol value; upgrading it would invent precision the process does not have. Only `server_time` carries protocol fidelity.

v2 `server_time` is `TEXT` with a GLOB CHECK enforcing the canonical shape, so the integer-epoch defect cannot be reintroduced by a writer that bypasses the Rust type.

## C004-F2 — CHATHISTORY grammar

**Disposition: closed.**

`HistoryQueryRequest` rebuilt as `Latest`, `LatestAfter`, `Before`, `After`, `Around`, `Between`, `Targets`; `Oldest` removed. `parse_chathistory` rewritten with a per-subcommand exact shape:

| Subcommand | Shape |
|---|---|
| `BEFORE` | `<target> <reference> <limit>` |
| `AFTER` | `<target> <reference> <limit>` |
| `LATEST` | `<target> *\|<reference> <limit>` |
| `AROUND` | `<target> <reference> <limit>` |
| `BETWEEN` | `<target> <reference> <reference> <limit>` |
| `TARGETS` | `<reference> <reference> <limit>`, no target |

- the limit is the LAST parameter in every form;
- references require their `timestamp=` / `msgid=` prefix, so `CHATHISTORY BEFORE #c some-junk 50` cannot look like a reference to an identifier the server never issued;
- TARGETS requires both selectors to be timestamps and is answered from a new `RecentTarget` store projection, not by querying history for a buffer the client invented;
- `MessageReference::Timestamp` holds an `IrcTimestamp`; `MsgId` requires a `msgid=` selector;
- `MAX_HISTORY_LIMIT = 50` is the single source of truth for both the advertised `CHATHISTORY=50` ISUPPORT token and the enforced parser ceiling, with a test asserting they cannot drift;
- BATCH response uses batch type `chathistory`, `draft/chathistory-targets` for TARGETS, per-message `batch=`, and a `draft/chathistory-end` terminator; an empty result is an empty successful batch;
- `AROUND` and `TARGETS` are implemented rather than refused, because refusing part of an advertised extension is the same class of lie as not advertising it.

`HistoryRefusal` rebuilt to `UnknownSubcommand`, `MissingParameters`, `TooManyParameters`, `InvalidTimestamp`, `InvalidReference`, `InvalidLimit`, `UnsupportedReferenceType`, `NoSuchBuffer`, `HistoryUnavailable`, mapping to `INVALID_PARAMS` / `INVALID_TARGET` / `MESSAGE_ERROR` / `INVALID_MSGREFTYPE`.

### Behaviour change beyond the plan's letter, flagged deliberately

The downstream CAP mediator previously NAKed everything, which made the advertised `draft/chathistory` / `draft/read-marker` surface untrue by construction. It now ACKs exactly what it serves and refuses a partially supported request as a whole rather than half-acknowledging it. This is wire-visible and was required by "make the advertised contract truthful".

## C004-F3 — MARKREAD semantics

**Disposition: closed.**

`parse_markread` rewritten: one parameter is a get, two are a set. `ParsedMarker::Clear` was removed — under the reviewed draft `MARKREAD *` is not a client operation.

| Requirement | Behaviour |
|---|---|
| client get | replies with the stored marker, or `*` when unknown |
| client set | resolves the named timestamp to a durable retained event, stores it, replies with the value actually stored |
| monotonic | `new = max(old, resolved)`; an older set retains and returns the newer marker |
| initial marker after JOIN, before RPL_ENDOFNAMES | emitted from the `JOIN` in the projection, so it arrives even when membership is incomplete |
| propagation to other negotiated sessions | a set that actually advances the marker is broadcast to every other attached session that negotiated `draft/read-marker` |
| client may not set `*` | refused; accepting it would let one session erase another's read state |
| unknown target | reviewed standard-reply error |
| un-negotiated client | never sent a MARKREAD at all, and an explicit `421` if it asks |

`SessionCapabilities` gained `read_markers` separately from `explicit_history`, because the two drafts are independently negotiable and inferring one from the other would silently withhold or deliver something the client did not ask for.

Resolving a client timestamp to a durable position *before* storing is what makes monotonicity hold: a client cannot name an arbitrary instant to skip ahead.

## C004-F4 — upstream queue integrity

**Disposition: closed.**

`let _ = normal_tx.try_send(...)` on the client-intent path is gone. A refused admission is now reported to the originating `SessionId` on the control queue and counted in `upstream_rejected`. The notice text is fixed and carries no client bytes, so there is nothing to inject and no secret to leak.

- The command is **never retried**. It was rejected before admission, so it definitely was not delivered, and a later generation could not know whether writing it again would duplicate it.
- No response route survives a refusal. Route allocation and send happen in the same owner turn, and nothing is opened for a frame that was not admitted.
- Desired membership is the opposite case, because the database commit happens first. It converges through `DesiredReconcile`: a generation-local set holding only a channel name and the direction of intent, capped at `MAX_DESIRED_RECONCILE = MAX_CHANNELS`, with the latest committed intent winning per channel.
- Reconciliation **never carries chat**. `JOIN`/`PART` are idempotent so replaying them is safe; replaying user traffic across a generation is exactly what the generation stamp exists to prevent.
- If the bounded set cannot hold another entry, the durable intent is still correct in storage, so the generation is deliberately restarted rather than dropping the Operator's intent.

Two defects were found while qualifying this and fixed:

1. The reconciliation drain originally ran only on the 60-second liveness probe, so a deferred JOIN on a quiet Network could sit unwritten for a minute. It now runs at the top of every turn **and** on its own `DESIRED_RECONCILE_INTERVAL` (250 ms) timer, because convergence cannot depend on traffic arriving — a committed JOIN may be the last thing that ever happens on a Network.
2. The PART path recorded a deferred intent but never published the pending depth, so `desired_reconcile_pending` read zero while work was queued. Both paths now go through one `defer_desired` helper.

## C004-F5 — downstream fanout integrity

**Disposition: closed.**

A refused live frame now ends that one `SessionId`: routes dropped, reader aborted, writer shut down, `fanout_detached` incremented, `downstream-overload` disposition recorded. The upstream connection, the Network and every other attachment are untouched.

There is deliberately no "drop chat but keep the `MODE`" refinement. Importance is not knowable from the command alone — a `MODE`, `NICK`, `JOIN`, `PART`, `KICK` or a `BATCH` boundary can all leave a client holding state later frames depend on — so ordered delivery is the contract and breaking it ends that attachment. Durable history keeps its separate best-effort drop policy; it is not part of the live stream's ordering guarantee.

### A second defect this exposed

The owner applied every line in one read without ever handing the scheduler back, so session writer tasks were starved for the duration of a burst and exhausted their own bounded queues without ever being read. The first draft of the detach policy therefore disconnected **healthy** clients for pressure they did not cause.

`UPSTREAM_LINES_PER_TURN = 32` now yields cooperatively mid-chunk — deliberately well under the per-session normal queue, so an attachment that is keeping up can always drain within one window. It is a scheduler handoff, not a spin: an idle owner still sleeps.

## Testing

373 tests pass across 19 test binaries, stable across repeated full runs.

| Area | Evidence |
|---|---|
| timestamp / server-time | 39 wire unit tests |
| migration | v1 fixture → v2, rollback, reopen idempotence, AUTOINCREMENT, NULL handling, future-version refusal, 1000-row batch boundary |
| CHATHISTORY | all six shapes, exact parameter counts, `*` placement, selector grammar, directions, AROUND cap, TARGETS ordering/limits, ISUPPORT, batch types, empty batch, end indication, standard-reply errors |
| MARKREAD | get unknown → `*`, set known, no backward movement, reply to setter, broadcast, initial marker ordering, invalid timestamp, no upstream write, retention clamping |
| upstream queue | refusal reported and counted, no response route survives, committed JOIN and PART both converge, bounded reconciliation at max/max+1, arbitrary chat never replayed |
| fanout | only the overloaded client detaches, MODE/NICK/KICK/JOIN/PART overflow likewise, healthy session receives every frame, routes cleared, reattach gets a truthful projection, counts return to steady state |

Two pre-existing tests encoded the old drop-frame contract and were rewritten, not deleted. The storage-pressure test also had to keep its client reading, because a fixture that stops reading is itself an overloaded client and would have confounded storage pressure with client slowness.

## Dependency, MSRV and network boundary

- No new dependency was added. MSRV stays 1.88; the 1.88 toolchain gate passes.
- `./scripts/check-network-boundary.py` exits 0.
- `./scripts/fuzz-smoke.sh` exits 0.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are clean.
- No environment-derived hostname, username, OS or router version, local path, process ID or machine identifier is inserted into any IRC-visible field. Secrets and SASL payloads remain excluded from `Debug` and diagnostics, and SQLite driver messages remain discarded.

## Unresolved findings

### UF-013-1 — live response routing is not wired (medium, non-blocking)

`ResponseRouter` is constructed per generation and `drop_session` / `expire` are called, but **`route()` is never called from any live path**, and `deliver()` is never called either. Correlation is therefore exercised only by the integration tests that drive `ResponseRouter` directly.

This is a pre-existing M003-D gap, not something Corrective 013 introduced or widened — and it means the plan's requirement that "failed query admission leaves no response route" currently holds *vacuously*, since no route is allocated on the client-intent path. It is recorded here rather than quietly relied upon.

It is **not** M004-blocking: M004 is anonymity and adverse-network qualification, and wiring correlation is an owned-scope decision that should be planned and reviewed on its own. Corrective 013 deliberately did **not** activate route allocation, because allocating routes without a live delivery path would fill the table to `MAX_ROUTES` and then begin refusing queries, which is strictly worse than the current state.

### No other open findings

None of the five findings is partially closed, and no invariant was weakened to achieve closure.

## M004 readiness decision

**M004 is unblocked for decomposition.**

Corrective 013 closes all five findings, satisfies every acceptance criterion in section 13 of the plan, and left no M004-blocking finding. M003's historical closure stands unmodified.

M004 must still be decomposed and registered in `plans/registry.md` before implementation begins. UF-013-1 should be raised as a candidate corrective against whichever milestone takes up response routing, because a bouncer that fans out every upstream line to every attached client is not yet correct for a shared Operator — but it does not block anonymity qualification.