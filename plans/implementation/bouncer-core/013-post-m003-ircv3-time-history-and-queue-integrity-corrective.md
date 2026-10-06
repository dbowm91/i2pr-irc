# Bouncer Core Corrective 013 — Post-M003 IRCv3 Time/History and Queue-Integrity Conformance

Status: closed — see `plans/closure/bouncer-core/013-status.md`

Repository baseline: `bdc9048d306068ae2427ebddd82513982b94e2d3`

Corrects the historically closed M003 implementation before M004.

Historical closure retained:

- `plans/closure/bouncer-core/012-status.md`

Source roadmap:

- `plans/subsystems/bouncer-core-roadmap.md`

Applicable architecture:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`
- `plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md`

Primary class: invariant + protocol correctness corrective

External protocol baseline reviewed 2026-10-06:

- IRCv3 server-time: https://ircv3.net/specs/extensions/server-time
- IRCv3 draft/chathistory: https://ircv3.net/specs/extensions/chathistory
- IRCv3 draft/read-marker: https://ircv3.net/specs/extensions/read-marker

Because chathistory/read-marker are work-in-progress specifications, implementation MUST re-check those pages at handoff start. If their grammar changed materially after this plan was registered, stop and refresh this corrective rather than implementing stale syntax.

## 1. Objective

Repair post-M003 defects that make the current durable/multi-client architecture stronger than its protocol and queue-disposition behavior.

The corrective has five implementation goals:

1. implement spec-correct IRCv3 `server-time` parsing, preservation, storage, and rendering;
2. make the advertised `draft/chathistory` and `draft/read-marker` surfaces conform to the current IRCv3 grammar and server behavior;
3. eliminate silent loss at downstream-to-upstream queue boundaries, including the durable DesiredState case;
4. restore ordered-stream semantics for live downstream fanout by never keeping a client attached after silently dropping an arbitrary live IRC frame;
5. reconcile planning/closure authority so M004 cannot start against an overstated M003 baseline.

This corrective does not replace the M003 architecture. The owned bounded SQLite worker, one-owner-per-Network model, ClientId/SessionId split, HistoryEventId ordering, bounded history, and response-router architecture remain the intended substrate.

## 2. Why the corrective is required

A source review after M003 closure found protocol and queue-integrity defects not exercised by the closure suite.

### C004-F1 — `server-time` wire semantics are non-conformant

Current owned wire behavior:

- `Message::time()` attempts to parse the `time` tag as an integer number of seconds, truncating at a literal dot;
- a valid IRCv3 tag such as `time=2011-10-19T16:40:51.620Z` therefore does not parse;
- history ingestion consequently drops conformant upstream server-time metadata;
- chathistory rendering emits an integer epoch value as the `time` tag.

Current IRCv3 `server-time` requires:

~~~text
YYYY-MM-DDThh:mm:ss.sssZ
~~~

in UTC with millisecond precision. The specification also explicitly permits a leap-second second field of `60`.

Consequences:

- the bouncer advertises `server-time` semantics it does not actually speak;
- preserved history loses the upstream timestamp;
- replay emits an invalid `time` tag;
- CHATHISTORY and MARKREAD timestamp references cannot interoperate correctly with compliant clients.

Severity: high.

### C004-F2 — CHATHISTORY command grammar is positional and semantic mismatch

Current `parse_chathistory` extracts:

- target from parameter 1;
- limit from parameter 2;
- first reference from parameter 3;
- second reference from parameter 4.

That does not match the reviewed current draft.

Current syntax includes:

~~~text
CHATHISTORY BEFORE  <target> <timestamp=... | msgid=...> <limit>
CHATHISTORY AFTER   <target> <timestamp=... | msgid=...> <limit>
CHATHISTORY LATEST  <target> <* | timestamp=... | msgid=...> <limit>
CHATHISTORY AROUND  <target> <timestamp=... | msgid=...> <limit>
CHATHISTORY BETWEEN <target> <reference> <reference> <limit>
CHATHISTORY TARGETS <timestamp=...> <timestamp=...> <limit>
~~~

The current code also:

- accepts bare msgids instead of requiring the `msgid=` selector form;
- accepts integer `timestamp=` values rather than the server-time timestamp format;
- models TARGETS as an ordinary target/history query even though TARGETS has no target argument and returns target/timestamp records;
- does not expose the reviewed `CHATHISTORY=<max>` / `MSGREFTYPES` ISUPPORT contract;
- does not currently model the full reviewed batch/end/error behavior required for a truthful server-side implementation;
- explicitly refuses AROUND while still advertising the extension without a clear partial-capability contract.

Severity: high because compliant clients can issue syntactically valid requests the bouncer misparses or misinterprets.

### C004-F3 — MARKREAD behavior does not match the current draft

Current `parse_markread` models:

- `MARKREAD *` as a clear operation;
- a target plus the same integer timestamp reference parser as CHATHISTORY.

Current reviewed draft behavior is:

~~~text
MARKREAD <target>
MARKREAD <target> <timestamp=YYYY-MM-DDThh:mm:ss.sssZ>
~~~

where:

- target-only is a client get operation;
- target + timestamp is a client set operation;
- the set timestamp must correspond to a previous message time tag;
- the server replies with its stored marker;
- an older/equal attempted marker does not move the marker backward and returns the stored newer value;
- server output uses literal `*` only to indicate that no marker is known;
- negotiated clients receive channel markers after server JOIN and before RPL_ENDOFNAMES;
- marker changes should be propagated to the Operator's other negotiated clients;
- markers are private and never sent upstream.

Severity: medium/high.

### C004-F4 — bounded upstream queues can silently lose client commands

Current owner code contains ignored non-blocking send results such as:

~~~text
let _ = normal_tx.try_send(...)
~~~

for ordinary forwarded client commands.

For desired JOIN/PART the durable mutation is committed first, but the later `queue_upstream(...)` result is also discarded.

Consequences:

- a normal client command can be definitely not queued while the client receives no failure;
- a query can leave an allocated response route with no upstream request;
- durable DesiredState can commit while the current generation silently fails to receive its JOIN/PART reconciliation command;
- queue bounds remain finite but the semantic disposition is not explicit.

Severity: high.

### C004-F5 — live downstream fanout can silently desynchronize a client

Current M003-F behavior retains a client after `SessionHandle::fanout` reports its bounded queue is full and drops the individual frame.

An IRC client stream is ordered stateful protocol traffic. An arbitrary dropped frame may be:

- JOIN/PART/KICK;
- NICK;
- MODE/TOPIC;
- NAMES/state reply;
- labeled response;
- ordinary message.

Keeping the stream attached after arbitrary live frame loss means the client can continue from state it never actually received.

This also diverges from the accepted M003-B plan, which required a full session queue to detach the affected client.

Severity: medium/high.

## 3. Corrective invariants

The corrective MUST establish and prove:

1. Negotiated `server-time` accepts and emits the current fixed UTC IRCv3 format, including millisecond precision.
2. A valid upstream leap-second timestamp is preserved without silently rewriting it into a different wire timestamp.
3. Canonical history order remains HistoryEventId; timestamp correction does not replace local ordering.
4. Existing schema-v1 history can be migrated transactionally to the new timestamp representation.
5. CHATHISTORY parameter order/reference grammar matches the reviewed current draft exactly for every advertised/implemented subcommand.
6. A `msgid` reference requires the `msgid=` selector syntax.
7. A timestamp reference uses the same canonical timestamp grammar as server-time.
8. TARGETS is either implemented with its actual semantics or the bouncer does not claim support for behavior it cannot provide.
9. AROUND is either implemented correctly or capability advertisement is adjusted so the bouncer does not falsely claim a complete extension surface.
10. CHATHISTORY limits, ISUPPORT, batch types, end indication, and standard-reply errors are truthful and bounded.
11. MARKREAD get/set/server behavior follows the reviewed draft and never moves the durable read position backward.
12. Read markers remain Operator-private and are never sent upstream.
13. Every client command rejected by an upstream queue receives an explicit bounded disposition; it is never silently discarded.
14. A failed queue submission cannot leave a stale response route.
15. Durable DesiredState remains authoritative even if its immediate upstream reconciliation enqueue fails.
16. No arbitrary live upstream frame may be dropped for a client that remains logically synchronized and attached.
17. A session whose live fanout queue cannot accept the next ordered frame is detached/resynchronized explicitly.
18. History/store best-effort drop semantics remain separate from live downstream stream semantics.
19. All queues remain bounded; no retry path adds an unbounded side queue.
20. Rust 1.88, I2P-only authority, secret redaction, and all previously closed M003 architectural invariants remain intact.

## 4. Scope

### In scope

- canonical IRC timestamp parser/formatter/representation;
- server-time tag handling;
- timestamp persistence/migration;
- chathistory parser, reference resolution, output and capability/ISUPPORT contract;
- read-marker parser/state/output/broadcast contract;
- queue-overload disposition for client intents;
- desired-state reconciliation after committed JOIN/PART when immediate upstream enqueue is unavailable;
- response-route rollback/cancellation on failed request enqueue;
- downstream fanout overload behavior;
- tests/conformance corpus additions;
- planning/architecture documentation reconciliation.

### Explicitly out of scope

- changing HistoryEventId as canonical history order;
- replacing SQLite/rusqlite or the store actor;
- FTS/history search;
- new multi-user authorization;
- M004 CTCP/DCC/privacy campaign;
- global reconnect budget;
- M005 convenience features;
- router adapters;
- Proposal 170;
- widening generic network authority.

## 5. Required production changes

### A. Introduce one canonical IRC timestamp representation

Add a bounded type for IRCv3 server-time/history references rather than passing integer seconds through the wire layer.

It must:

- parse exactly the reviewed UTC syntax;
- validate calendar fields;
- preserve millisecond precision;
- preserve a syntactically valid leap second `:60` without rewriting the original timestamp to `:59` or the following minute;
- support deterministic comparison/reference resolution;
- format canonical output;
- have a strict byte ceiling;
- reject non-UTC offsets and malformed fractional precision for this protocol surface.

The owner of this type may be the wire crate or a narrowly shared protocol-domain module, but there must be one parser/formatter used by server-time, CHATHISTORY and MARKREAD.

Do not add a date/time dependency that raises MSRV or silently normalizes leap seconds. If a dependency is proposed, review its exact parsing/formatting and leap-second semantics first.

### B. Preserve upstream server-time exactly

For a valid incoming `time` tag:

- preserve the canonical wire value;
- store sufficient data to replay the same millisecond timestamp;
- never use it as local history identity/order.

For a history event with no upstream time:

- synthesize a valid UTC millisecond server-time from the local wall clock when replay semantics require one.

The existing whole-second WallTime may remain useful for non-wire diagnostics if desired, but it cannot remain the sole durable representation for protocol server-time.

### C. Schema v2 migration

The current v1 `history_events.received_at` and `server_time` integer-second representation is insufficient to preserve current server-time semantics.

Introduce a schema v2 migration, transactionally tested from v1.

The exact column design is an implementation choice, but closure must prove:

- v1 opens and migrates to v2 atomically;
- current whole-second values become valid millisecond/canonical timestamps without changing history order;
- any sub-second precision already lost by v1 is explicitly acknowledged as unrecoverable rather than invented;
- future valid upstream millisecond/leap-second values are preserved;
- failed migration leaves the v1 database intact;
- a schema newer than v2 remains rejected;
- HistoryEventId/cursors/read markers remain stable.

A reasonable design is a canonical validated server-time text field plus millisecond local receive time, but the implementation may use an equivalent representation if all wire and ordering requirements are proven.

### D. Correct server-time rendering

Every emitted `time` tag must be syntactically valid under the ratified server-time extension.

No integer epoch value may be emitted as a `time` tag.

Add conformance fixtures including:

- ordinary milliseconds;
- `.000Z`;
- leap second `:60.xxxZ`;
- invalid month/day/hour/offset/precision;
- boundary dates supported by the durable representation.

### E. Rebuild CHATHISTORY parsing around exact subcommand shapes

Do not use one shared positional extraction scheme for all subcommands.

Required parser shapes:

- BEFORE: target, reference, limit;
- AFTER: target, reference, limit;
- LATEST: target, `*` or reference, limit;
- AROUND: target, reference, limit;
- BETWEEN: target, first reference, second reference, limit;
- TARGETS: first timestamp, second timestamp, limit, with no target argument.

References:

- `timestamp=<server-time-format>`;
- `msgid=<opaque-id>`;
- only the subcommands/spec positions that permit each reference type.

Malformed, missing or excess arguments receive the reviewed standard-reply error behavior.

### F. Make the advertised chathistory contract truthful

Before advertising `draft/chathistory`, provide the current reviewed server-side contract, including:

- `CHATHISTORY=<max>` ISUPPORT;
- `MSGREFTYPES` matching reference types actually supported;
- required BATCH response behavior when batch was negotiated;
- `chathistory` batch type for message history;
- `draft/chathistory-targets` batch type for TARGETS;
- correct canonical target parameter;
- empty successful batch when no content exists where required;
- `draft/chathistory-end` behavior if the implementation claims it;
- bounded standard-reply FAIL/error behavior;
- PRIVMSG/NOTICE-only replay unless event-playback is separately negotiated/implemented.

Implement AROUND and TARGETS correctly if the capability remains advertised as the current extension. If implementation evidence shows one cannot be provided within existing bounded store APIs, stop and explicitly narrow/withhold the capability rather than approximating the result.

### G. Correct reference resolution

Reference resolution must remain bounded.

For timestamp references:

- compare the canonical protocol timestamp representation;
- deterministic ties resolve through HistoryEventId;
- no adjacent-message guessing for stale/pruned references.

For msgid:

- preserve opaque upstream values;
- scope lookup to the correct Network/Buffer semantics;
- never expose HistoryEventId as a msgid.

### H. Implement full reviewed MARKREAD server semantics

Support:

1. client get: `MARKREAD <target>`;
2. client set: `MARKREAD <target> timestamp=...`;
3. server reply with current marker or `*` if unknown;
4. monotonic no-backward updates;
5. exact matching/resolution to retained message history;
6. initial channel marker after JOIN and before RPL_ENDOFNAMES for negotiated clients;
7. update propagation to other negotiated local sessions of the same Operator;
8. user/query target get behavior;
9. standard-reply errors for invalid/missing/internal-failure cases.

Do not treat `MARKREAD *` as a client clear operation under this reviewed draft.

### I. Make ordinary upstream queue rejection explicit

Audit every ignored non-blocking queue send on the client-intent path.

For an ordinary non-durable forwarded command:

- if it cannot enter the bounded upstream queue, it definitely was not accepted for upstream delivery;
- cancel any route/correlation allocated for it;
- send a bounded local error/FAIL/NOTICE to the originating SessionId;
- increment a non-secret overload counter;
- do not invent retry/replay.

Queries must not leave stale routing entries after queue rejection.

No `let _ = try_send(...)` may remain where failure changes client-visible command semantics.

### J. Reconcile committed DesiredState when immediate enqueue fails

JOIN/PART differ from ordinary forwarded commands because the database commit is the durable authority.

After a successful desired-state commit:

- failure to enqueue the immediate wire command must not roll back or forget the durable intent silently;
- record an explicit bounded reconciliation-needed condition;
- ensure the current generation eventually converges to durable desired state through a bounded mechanism.

Allowed designs include:

- a bounded per-Network desired-state reconciliation set keyed by channel, capped by existing channel ceilings and drained when upstream capacity becomes available; or
- an explicit controlled generation restart that reconstructs durable desired state.

Do not:

- place the command in an unbounded retry queue;
- replay arbitrary non-idempotent chat along with it;
- claim success without diagnostics while live state remains indefinitely divergent.

The plan prefers a bounded desired-state reconciliation set because it avoids unnecessary reconnects, but implementation may choose the restart strategy if it is simpler and fully evidenced.

### K. Restore ordered live fanout semantics

A downstream IRC stream is ordered. Once one live frame is dropped, the bouncer cannot know that the client remains synchronized.

Therefore:

- if a SessionHandle live normal queue rejects the next upstream frame, mark that session overloaded/desynchronized;
- stop further fanout to it;
- remove its response routes;
- detach/shutdown that SessionId deterministically;
- increment an explicit overload-detach counter/disposition;
- leave all other sessions and the upstream Network online.

Do not apply this rule to the separate durable-history ingest queue; best-effort history drop remains a different bounded policy.

Do not try to classify only MODE/JOIN/NICK as "important" and silently drop chat: ordered IRC delivery itself is the contract, and later labels/batches can also make apparently ordinary frames stateful.

### L. Planning/documentation reconciliation

Preserve `plans/closure/bouncer-core/012-status.md` as historical evidence.

Do not rewrite it to pretend the defects were known at closure.

Add closure `plans/closure/bouncer-core/013-status.md` and make it the strict current authority for M004 readiness.

Update:

- IRC wire/server-time docs;
- history/chathistory/read-marker docs;
- network-owner/downstream-session overload docs;
- schema/migration docs;
- bouncer-core roadmap;
- active registry.

## 6. Ordered work packages

### Work package A — timestamp type and server-time conformance

Implement canonical timestamp parse/format/preservation and wire conformance tests.

### Work package B — schema v2 timestamp migration

Migrate durable history, preserve IDs/cursors, add v1->v2 rollback/reopen evidence.

### Work package C — CHATHISTORY grammar and response conformance

Correct all subcommand shapes/references, capability/ISUPPORT/error/BATCH behavior, and bounded query adapters.

### Work package D — MARKREAD conformance

Implement get/set/reply/initial/broadcast/monotonic behavior over durable marker state.

### Work package E — upstream queue integrity

Remove silent client-intent send loss, cancel failed routes, and implement bounded DesiredState convergence after enqueue failure.

### Work package F — downstream fanout integrity

Detach one overloaded/desynchronized session rather than dropping one arbitrary live frame and keeping it attached.

### Work package G — integrated regression/closure

Re-run M003 qualification with spec-real wire fixtures, queue saturation, migrations, multi-session behavior, conformance corpus, Rust 1.88 and planning reconciliation.

## 7. Required protocol behavior matrix

### server-time

| input/operation | required result |
|---|---|
| `@time=2011-10-19T16:40:51.620Z` | parsed and preserved exactly |
| `@time=2012-06-30T23:59:60.419Z` | valid leap second preserved |
| integer `@time=1700000000` | not treated as valid server-time |
| timezone offset `+01:00` | rejected for this IRCv3 tag |
| history without upstream time | local valid UTC millisecond timestamp when replay requires time |
| replay of preserved timestamp | same canonical timestamp, not epoch integer |

### CHATHISTORY

| request | required interpretation |
|---|---|
| `LATEST #c * 50` | newest <=50 |
| `LATEST #c timestamp=... 50` | newest after selector |
| `BEFORE #c msgid=x 50` | before selector |
| `AFTER #c timestamp=... 50` | after selector |
| `AROUND #c msgid=x 50` | bounded around selector |
| `BETWEEN #c msgid=a msgid=b 50` | bounded between, direction handled correctly |
| `TARGETS timestamp=a timestamp=b 50` | target list, not history for a fake target |
| bare `msgid` | invalid selector |
| integer `timestamp=1700` | invalid timestamp selector |
| excess/missing args | standard-reply invalid-params behavior |

### MARKREAD

| request/event | required result |
|---|---|
| `MARKREAD #c` | return current marker or `*` |
| `MARKREAD #c timestamp=...` newer | advance + reply + broadcast |
| older/equal set | retain and return existing newer marker |
| unknown target | reviewed error |
| negotiated channel JOIN | marker sent before 366 |
| `MARKREAD *` client command | not interpreted as "clear all" |

## 8. Queue-overload behavior matrix

| boundary | full condition |
|---|---|
| ordinary client -> upstream normal queue | command not accepted; route canceled; originating session gets local failure |
| query -> upstream queue | query not sent; no route remains |
| desired JOIN/PART after DB commit | durable intent retained; bounded reconcile/restart scheduled; explicit diagnostic |
| session live fanout queue | affected session detached/desynchronized; other sessions continue |
| history ingest queue | event may be dropped and counted; Network/client live stream remains unaffected |
| store ingress | existing typed overload behavior remains |

## 9. Migration and compatibility

Schema v1 is already evidence-closed and may exist on disk.

Corrective 013 is the first required predecessor migration.

Closure must include:

- fresh v2 creation;
- v1 -> v2 migration;
- v2 reopen;
- failed migration rollback;
- future schema rejection;
- v1 rows with null/non-null server_time;
- cursor/read-marker ID stability;
- acknowledgment that v1 already discarded sub-second upstream precision and cannot reconstruct it.

Do not reset or silently recreate an existing database to avoid migration work.

## 10. Failure, cancellation, restart, contention

### Protocol query

Malformed CHATHISTORY/MARKREAD affects only the requesting session; upstream Network remains online.

### Route allocation/send race

If route allocation succeeds but upstream queue admission fails, route cleanup is part of the same owner turn before reporting failure.

### Desired mutation

Once SQLite commits desired intent, cancellation of the originating session does not undo that operator intent. Reconciliation is Network-owned.

### Fanout overload

Only the overloaded SessionId is detached. The Network and other sessions remain live.

### Restart

Schema migration completes before supervisors start. Fresh observed state is rebuilt exactly as under ADR-0002.

## 11. Required tests

### Timestamp/server-time

- exact normal timestamp roundtrip;
- milliseconds retained;
- leap-second roundtrip;
- calendar validation;
- malformed format rejection;
- real IRCv3 server-time fixture enters history;
- replay emits valid time tag;
- local synthesized timestamp valid;
- identical timestamps still ordered by HistoryEventId.

### Migration

- fresh schema v2;
- fixture schema v1 -> v2;
- rollback on induced migration failure;
- history IDs/cursors/markers unchanged;
- old seconds map deterministically to `.000Z`/equivalent;
- future schema refused.

### CHATHISTORY

- all six current subcommand shapes;
- exact parameter count;
- `*` only where valid;
- timestamp/msgid prefix grammar;
- BEFORE/AFTER/BETWEEN directions;
- AROUND cap;
- TARGETS ordering/limits;
- CHATHISTORY and MSGREFTYPES ISUPPORT;
- correct batch types;
- empty result batch;
- end indication;
- standard-reply errors;
- event-playback absent -> only PRIVMSG/NOTICE;
- event and byte ceilings.

### MARKREAD

- get unknown -> `*`;
- set known timestamp;
- older/equal cannot move backward;
- reply to setter;
- broadcast to another negotiated SessionId;
- initial channel marker ordered before 366;
- user/query target get;
- invalid timestamp;
- marker private/no upstream write;
- restart/retention clamping.

### Upstream queue

- ordinary Forward max/max+1 returns explicit failure;
- failed send leaves no response route;
- no client command disappears without counter/disposition;
- committed JOIN with full queue eventually reconciles or deliberately restarts;
- committed PART likewise;
- bounded reconciliation max/max+1;
- arbitrary chat is never replayed through desired-state reconciliation.

### Fanout

- queue max/max+1 detaches only overloaded client;
- MODE overflow cannot leave client attached with stale mode state;
- NICK/KICK/JOIN/PART overflow likewise;
- healthy second session receives every frame;
- routes for detached session cleared;
- reattach obtains truthful current projection/history;
- task/queue counts return to steady state.

## 12. Verification

Expected closure commands:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo tree --locked -e all
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Also run a dedicated current-IRCv3 conformance fixture suite using literal examples derived independently from the reviewed official specifications.

## 13. Acceptance criteria

1. Real current server-time values parse, persist and replay correctly.
2. No invalid integer epoch `time` tags are emitted.
3. Schema v1 migrates transactionally to v2 without changing durable event/cursor identity.
4. Every advertised CHATHISTORY subcommand follows the reviewed current grammar and output semantics.
5. MARKREAD get/set/server/update behavior follows the reviewed current draft.
6. Draft capability advertisement is withheld for any semantics the bouncer still cannot provide.
7. No ordinary client command is silently lost at an upstream queue boundary.
8. Failed query admission cannot leave an open response route.
9. Durable JOIN/PART intent converges after immediate enqueue failure using a bounded mechanism.
10. A live downstream frame is either delivered in order or the affected session is detached; no attached client silently skips a frame.
11. History/store best-effort dropping remains bounded and does not leak into live IRC stream semantics.
12. Existing M003 architecture, network boundary, secret handling and Rust 1.88 remain green.
13. M004 stays blocked until `plans/closure/bouncer-core/013-status.md` accepts the corrective.

## 14. Stop conditions

Stop and register a successor architectural decision if:

- exact server-time preservation requires abandoning HistoryEventId ordering;
- schema v2 cannot migrate v1 transactionally;
- a proposed date/time dependency raises MSRV or cannot preserve leap seconds;
- correct chathistory behavior requires unbounded history scans;
- MARKREAD semantics require multi-user authorization beyond the current single-Operator model;
- desired-state reconciliation requires replaying arbitrary client traffic;
- fanout correctness appears to require blocking the Network owner on a slow client;
- the current IRCv3 draft changes materially during implementation.

## 15. Closure evidence required

Create `plans/closure/bouncer-core/013-status.md` containing:

- implementation commits;
- exact IRCv3 spec revision/date/URLs reviewed;
- C004-F1 through C004-F5 disposition;
- timestamp grammar fixture matrix;
- schema v1->v2 migration matrix;
- CHATHISTORY subcommand/capability/ISUPPORT/error matrix;
- MARKREAD get/set/initial/broadcast matrix;
- upstream queue-overload matrix;
- desired-state reconciliation evidence;
- downstream fanout-overload/detach matrix;
- full M003 regression results;
- dependency/MSRV/network-boundary review;
- unresolved findings/severity;
- explicit M004 readiness decision.

## 16. Handoff notes

Do not "fix" server-time by merely formatting the existing integer seconds as a decimal tag. The wire format is a UTC calendar timestamp.

Do not "fix" fanout by increasing the queue until tests stop failing. The semantic issue is what happens at the finite bound.

Do not "fix" ordinary upstream queue pressure by retrying user chat across owner turns without an explicit delivery contract. A command rejected before queue admission is a local failure, not a replay candidate.

Do not rewrite the historical M003 closure. Corrective 013 is the evidence-preserving mechanism for post-closure findings.
