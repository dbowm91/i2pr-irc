# Bouncer Core M005-E / Plan 024 — Indexed History Search and CHATHISTORY Completion

Status: closed

Closure: plans/closure/bouncer-core/024-status.md

Blocker:

- Plan 023 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: capability + polish

## 1. Objective

Add efficient bounded server-side history search and close the known indexed-reference gap in the existing CHATHISTORY adapter without replacing HistoryEventId as canonical order.

Deliver:

- store indexes needed for bounded timestamp/msgid reference lookup;
- CHATHISTORY AROUND if the reviewed current draft still defines it at implementation time;
- soju.im/search adapter;
- bounded FTS5-backed text search;
- transactional search-index maintenance with history retention.

## 2. Invariants

1. HistoryEventId remains canonical durable order.
2. Remote server-time and msgid remain metadata, not unique ordering authority.
3. Every search has count, byte, selector and execution-work bounds.
4. Client text never becomes arbitrary SQL, FTS expression or regex.
5. Search indexes never outlive retained history rows.
6. Storage work remains behind the one bounded StoreHandle worker.
7. Draft/search syntax remains adapter-level and does not become the generic store API.

## 3. Storage indexes

Add typed lookup operations for:

- nearest event around a canonical IRC timestamp within one Buffer;
- msgid reference lookup with explicit ambiguity disposition;
- bounded multi-buffer/network search selectors.

Add relational indexes needed for these operations.

Use the bundled SQLite FTS5 support already present through rusqlite's bundled libsqlite3 build for text search if validation at implementation time confirms the feature is enabled in the pinned dependency.

The FTS representation is a side index. HistoryEvent rows remain source of truth.

## 4. Search representation

Do not index opaque raw IRC lines as the only searchable representation.

At ingestion, derive bounded normalized fields sufficient for the supported search surface:

- event id;
- network/buffer;
- sender nick where known;
- target;
- message text;
- server/receive time metadata.

Only stored PRIVMSG/NOTICE events are searchable in the initial surface.

A selector parser converts the protocol request into a typed SearchQuery.

Supported selectors should match the reviewed soju.im/search draft:

- in;
- from;
- after;
- before;
- text;
- limit.

Unknown/invalid selectors fail explicitly.

Text is treated as bounded literal/token search. Do not expose raw MATCH syntax or regular expressions.

## 5. FTS and retention consistency

Append history and its search index entry in one store-side transactional operation where possible.

Retention must remove corresponding FTS/index rows transactionally.

Migration/rebuild from the predecessor schema must be bounded in batches and atomic at schema-transition level. A failed migration leaves the old schema usable.

A corruption/missing-index condition must be detectable; do not silently return incomplete search as complete.

## 6. CHATHISTORY completion

At implementation start re-read the current IRCv3 draft and record the exact revision.

If AROUND remains part of that revision, implement it over typed nearest-reference queries and advertise it only after tests.

Review MSGREFTYPES/ISUPPORT claims against the actually supported timestamp/msgid references.

Do not widen CHATHISTORY merely because an index exists.

## 7. soju.im/search adapter

Advertise soju.im/search only when the adapter, bounded store query and reply batching are all complete.

Replies:

- contain only supported retained PRIVMSG/NOTICE events;
- preserve canonical server-time/msgid when present;
- return deterministic order, preferably HistoryEventId ascending;
- use bounded BATCH framing when negotiated;
- return an explicit empty batch for no matches where the draft expects one;
- do not leak events from another Network or unauthorized Buffer scope.

## 8. Work packages

A. current draft/spec review;
B. typed timestamp/msgid lookup indexes;
C. schema migration and FTS5 side index;
D. safe bounded SearchQuery parser;
E. search store operation and work ceilings;
F. CHATHISTORY AROUND/reference completion;
G. soju.im/search adapter/CAP;
H. retention/migration/corruption qualification;
I. docs/closure.

## 9. Tests

Include:

- predecessor migration and rollback;
- FTS feature availability;
- append/search/retain transactional consistency;
- no retained FTS row after history deletion;
- literal search cannot inject FTS operators/SQL;
- selector bytes/term counts/limit max+1;
- query result byte/count bounds;
- same timestamp with several HistoryEventIds has deterministic order;
- duplicate/unknown msgid has explicit disposition;
- AROUND before/at/after edges;
- search across several buffers but never another Network;
- store pressure cannot starve upstream PING/PONG;
- empty/matching batch protocol transcripts;
- restart yields identical search results for retained history.

## 10. Verification

Run store migration/history suites, chathistory conformance, full workspace verification, adverse store-pressure tests and MSRV verification.

## 11. Documentation

Update storage, history and chathistory architecture; add a search adapter document that records the exact draft revision and selector/matching policy.

## 12. Acceptance criteria

Clients can perform useful bounded history search and indexed history positioning without full-log scans, arbitrary expressions, cross-Network leakage or any change to canonical HistoryEventId ordering.

## 13. Stop conditions

Stop if implementation needs an external search service, regex evaluation from client input, a second database connection pool, unbounded scan, or schema semantics tied directly to a draft command string.

## 14. Closure evidence

Create plans/closure/bouncer-core/024-status.md with migration/index consistency, bound measurements, protocol transcript matrix and M005-F readiness.
