# Plan 024 — M005-E Indexed History Search and CHATHISTORY Completion — Closure

Closed 2026-10-08. Outcome: **closed, no open findings**.

## Outcome

A client can search retained history with bounded text and bounded scope, and can position
itself anywhere in that history — including before anything it still holds — without a
single full-log scan. Two things changed together because they are the same feature:

- `soju.im/search` is a real capability: parsed to a typed `SearchQuery`, answered from an
  FTS5 side index, framed in one bounded batch, scoped to one Network by the owner.
- CHATHISTORY references resolve through indexed lookups instead of a bounded window read,
  so `AROUND` and every `msgid=`/`timestamp=` selector work anywhere in the retained
  history rather than only inside the last 512 events.

`history_events` remains the source of truth. `history_search` is an index, and the index
cannot outlive a retained row because they are written and deleted in one transaction
against an exact id.

## What was landed

### Work package A — draft review, recorded

`crates/runtime/src/search.rs` exports `ADAPTER_REVISION`:

```text
soju.im/search selectors in,from,after,before,text,limit (M005-E reviewed surface)
```

`ADAPTER_REVISION` is asserted by a test to name the draft and never to contain raw
`MATCH`, so a future draft that adds an expression selector cannot land without the string
changing and someone noticing.

Every `soju.im/search` literal and every selector keyword lives in that one module. The
store speaks `SearchQuery` and never sees draft syntax — the same separation
`draft/chathistory` keeps.

### Work package B — typed timestamp and msgid lookups

Schema 6 adds two indexes:

| Index | Columns | Replaces |
|---|---|---|
| `history_by_time` | `(buffer_id, effective_time, event_id)` | a window read plus an in-memory filter |
| `history_by_msgid` | `(network_id, msgid)` | ditto, with an explicit ambiguity disposition |

`nearest_event` returns both bracketing edges plus an `exact` flag, so `AROUND` can tell
whether a reference landed *on* an event rather than between two.

`resolve_msgid` returns `MsgidLookup::{Unique, Ambiguous, Missing}` rather than first-match.
A duplicate upstream id is a real condition — an upstream reusing one after a restart is
ordinary — and picking the lowest `HistoryEventId` would answer a question the client did
not ask while looking authoritative. `MAX_MSGID_AMBIGUOUS` caps the report at 8, and a
result *at* the cap is still reported ambiguous, so truncation never turns "ambiguous"
into a false "unique".

### Work package C — schema migration and the FTS5 side index

`migrate_5_to_6` is four statements in one transaction, in dependency order: add
`effective_time`, create the two indexes over it, create the FTS table, backfill.

Both backfills are bounded at `MIGRATION_BATCH_ROWS`. Both run inside the single
migration transaction, because a partially backfilled index is worse than none — it would
answer searches with results that silently stop partway through.

**The FTS rowid is the `HistoryEventId`.** That one decision is what makes retention
exact: deleting a retained row and deleting its index entry are one statement each by
exact id, not a join that could half-succeed.

`verify_search_support` checks `ENABLE_FTS5` at open and at migration rather than trusting
the dependency list. A build without FTS5 would otherwise create a database that
advertises a searchable history it cannot search.

`verify_search_index` checks the promised indexes exist **and** that the index row count
agrees with the retained searchable rows. An index that exists but has lost rows would
otherwise answer "no matches" — a degraded feature presented as a complete one.

`history_search_%` shadow tables are excluded from the promised table set: they are
SQLite's own storage, and listing them would make the promise depend on the SQLite build
in use.

### Work package D — the safe bounded selector parser

`SearchTerm` accepts a bounded run of Unicode alphanumerics and nothing else — no quotes,
no operator characters, no punctuation FTS5 treats as syntax. `match_expression()` quotes
each term and joins with `AND`, a literal this code chose rather than one the client typed.

The alternative, escaping a raw expression, makes every future FTS5 operator a potential
escape: the escaping has to be re-derived rather than prevented. Here there is nothing for
client text to *be*.

`SEARCH` names no target of its own — the target is `in=` — so there is no leading
parameter to skip. See defect 1.

### Work package E — the store operation and its ceilings

The compiled `MATCH` is the only SQL text that reaches SQLite; every client-influenced value
is a bound parameter. Network scoping is applied **in the same statement as the match**, so
a Network filter cannot be forgotten by a later edit to the query text.

Every bound is enforced at the parser *and* at the store. A parser that trusted the store
would let a future caller skip the parser; a store that trusted the parser would be one
SQL string away from an unbounded scan.

The time window is half-open, `[after, before)`, so a whole-millisecond timestamp cannot
fall in both halves or in neither. `after >= before` is refused rather than swapped — a
swapped range is a different question from the one that was asked.

### Work package F — CHATHISTORY reference and AROUND completion

`resolve` is now two indexed seeks. `HistoryPosition` has two values, and the second is
the interesting one:

| | `Event(e)` | `BeforeStart` |
|---|---|---|
| `BEFORE` | events before `e` | **empty** |
| `AFTER` | events after `e` | everything retained |
| `AROUND` | centred on `e` | the oldest page |
| `LATEST *` | newest page after `e` | the newest page |

An earlier implementation anchored a too-old reference onto the oldest retained event.
That is off by one in the one direction a client cannot check: `AFTER <old bookmark>`
silently skipped the oldest message. The out-of-window case now has its own value rather
than being forced through the in-window one.

`history_around` is a new store operation: two indexed seeks walking backwards and
forwards from the anchor with independent budgets, then reversed into one ascending run.
Reading a window of the newest events and filtering it cannot produce this, because the
anchor may be older than any such window — so the window would have to grow until it
happened to contain the anchor, which is the unbounded scan the operation exists to
remove. Before this change, `AROUND` with an anchor older than `LATEST` window failed.

A missing anchor is **not** an error: both sides are still returned, because a client
paging from a bookmark retention has since removed is exactly the case that has to
survive. An empty result means "nothing on either side", never "the anchor was pruned".

`UnknownReference` and `AmbiguousReference` are new, distinct from `HistoryUnavailable`.
After retention ran successfully, the only true statement about a pruned id is that
nothing carries it; a client told "unavailable" would retry something that will never
succeed.

`HistoryJournal::reference_candidates` is **removed**. It read a bounded window and
filtered it in memory, and every reference resolution went through it. Leaving it in place
would have meant the store had two reference paths with different answers — one indexed,
one approximate — with no test saying which one ran.

The `MSGREFTYPES=timestamp,msgid` claim was reviewed against what is served, as the plan
requires. Both reference kinds resolve for every subcommand including `AROUND`, at every
window edge, and both new refusal dispositions are reachable over the wire — all asserted
in `m005e_search_history`. The token is truthful as written and is unchanged.

### Work package G — the search adapter and CAP

`soju.im/search` is advertised only because the adapter, the bounded store query, and the
reply batching are all complete.

Replies:

- contain only retained `PRIVMSG`/`NOTICE`;
- are scoped to one Network, chosen by the owner from the journal it already holds, and to
  the buffers `in=` actually resolved to;
- arrive in ascending `HistoryEventId`, so a search and a CHATHISTORY page agree about
  order;
- are framed in one batch of type `soju.im/search`, whose id is generation-local;
- return a complete opened-and-closed empty batch when nothing matches, because a client
  that heard nothing has to guess whether to wait;
- go only to the session that asked — a search never crosses the upstream connection or
  another client's socket.

A refusal is a fixed `FAIL SEARCH INVALID_SEARCH :<reason>` plus the same complete empty
batch. The reason is a constant, never the offending selector or its value: a refusal that
echoed the request would turn every bound that is not met into a way to put arbitrary
client text into a frame the Operator sees.

`msgid` on a result is `hit<HistoryEventId>`. An upstream `msgid` is never claimed as a
reference this bouncer can resolve, and the shape is visibly distinct from one an upstream
issued.

### Work package H — retention, migration and corruption qualification

Append and index are one transaction. Retention deletes index rows by exact event id in
the same transaction as the retained rows. Migration is bounded, batched, and atomic at
schema-transition level. A missing index or a row-count disagreement refuses the open.

### Work package I — docs

`architecture/history-search.md` is new and records the draft revision, the selector and
matching policy, the `effective_time` rule, the position table, the bounds, the reply
shape, and the removed scan path. `architecture/chathistory.md`,
`architecture/storage.md`, `architecture/history.md`, `architecture/overview.md`, and
`architecture/testing.md` are updated.

## Protocol isolation

The plan's invariant 7 — draft/search syntax stays adapter-level and does not become the
generic store API — holds structurally. The store's vocabulary is `SearchQuery`,
`SearchTerm`, `SearchHit`, `MsgidLookup`, `NearestEvent`, `HistoryAround`. None of them
names a selector, a capability, or a subcommand. `soju.im/search` appears nowhere in
`crates/store`.

The one place the store touches protocol is the migration backfill, which has to rebuild
an index for events it already holds and cannot ask the runtime about them. It uses the
same bounded decoder ingestion uses, in bounded batches, at migration time only. Refusing
to decode would leave an upgraded database whose retained history was silently
unsearchable — the failure the plan names as unacceptable.

## Design decisions and deviations

1. **`effective_time` is a new column, not a write into `server_time`.** `server_time`
   records what the upstream actually said. Writing a synthesized value there would turn
   "the upstream sent no timestamp" into a false claim about the upstream.
2. **The rule has exactly one definition.** `i2pr_irc_store::effective_time` is shared by
   the append path, the migration backfill, and the runtime's `time=` tag rendering. The
   runtime's `local_timestamp` delegates to it. Two copies would be how a replayed message
   and a timestamp reference came to disagree about where a message sits.
3. **Results are ordered by `HistoryEventId`, not by time.** That keeps a search consistent
   with CHATHISTORY paging: a client that searches, gets event ids, then asks for the range
   between two of them sees a contiguous page.
4. **An undecodable payload is indexed, with empty fields.** Skipping it would make the
   index count disagree with the retained count, and that disagreement refuses the next
   open. A message that can never match costs one row; a message that made every later
   open fail costs the Operator their history.
5. **`AROUND` takes two budgets, not one `limit`.** A single limit forces the caller to
   guess a split and then silently lose half of what it wanted. The total is still bounded
   by `MAX_HISTORY_QUERY_EVENTS`.
6. **A `BeforeStart` `MARKREAD` is answered, not written.** A read mark earlier than every
   retained message is the truthful state of a client that has read nothing, so the stored
   marker is returned unchanged rather than a position invented and broadcast to every
   other session.
7. **The `in=` selector uses the owner's own casemapped map.** A channel that does not
   resolve is refused rather than searched as an empty scope: answering a typo with "no
   matches" reads as a true statement about the journal and is not.

## Limits recorded

1. **Only stored `PRIVMSG`/`NOTICE` is searchable.** A `JOIN` is not. Every additional
   event class widens what a client can find without any statement that it should.
2. **The search backfill decodes protocol.** It is the one exception to "the store speaks
   no protocol", it is bounded, and it is one-off. Recorded above.
3. **`HistoryEventId` is a linear scan in disguise for the "no terms" case.** A search with
   `from=` or a time window but no `text=` falls back to an indexed range read bounded by
   Network, buffer scope, range, and limit — never by journal size. It is not a full-text
   search and is not advertised as one.
4. **FTS5 must be compiled in.** It is, through the bundled SQLite, and the open path
   verifies it rather than assuming. A build without it refuses the database rather than
   serving a history that reports no matches.
5. **A search reply names its result with a bouncer-local `hit<id>`,** not the upstream
   `msgid`. Feeding a `hit<id>` back into `CHATHISTORY msgid=` does not resolve, and that
   is deliberate: it is not an upstream identifier and claiming otherwise would be a lie
   about what the server can resolve.
6. **Search is per Network.** There is no cross-Network search, and none is planned: the
   Network scope is applied in the same statement as the match, so it is not a policy that
   a future edit could forget.
7. **`MAX_REPLY_BYTES` truncates a page by dropping later results**, not by failing. A
   client sees fewer results than its `limit=` asked for. `limit=` is the only count the
   client may rely on, and it is always honored; the byte ceiling is a second, lower bound
   that can only make the answer smaller.

## Defects found and fixed during implementation

Six, five of them found by the qualification suite rather than by reading the code:

1. **`parse_search` silently discarded the `in=` scope.** `SEARCH` names no target of its
   own, but the parser skipped the first parameter as though it did — so
   `SEARCH in=#room text=quick` parsed as `text=quick`. Every search still returned
   plausible results, scoped to the wrong set, and a client narrowing to one channel would
   have received matches from every buffer on the Network. Unit tests missed it because
   they called `parse_selectors` directly and started past the bug; only the end-to-end
   suite exercised the parameter handling. Fixed, with
   `every_parameter_of_a_search_command_is_a_selector` going through `parse_search` so the
   gap cannot be tested around again.
2. **`recent_targets` compared a TEXT column against INTEGER milliseconds.** `server_time`
   is TEXT and the bound was an `i64`, and SQLite orders every TEXT value after every
   INTEGER value regardless of the numbers involved — so `CHATHISTORY TARGETS` time
   filtering never matched and looked correct. This was a **pre-existing** bug in shipped
   code, not something this plan introduced; it is fixed here because the new `effective_time`
   rule made the correct comparison unavoidable to state. `canonical_time` is now the only
   path by which a time bound reaches SQL.
3. **An upstream that sends no `server-time` produced an unpositionable buffer.** The first
   implementation indexed `server_time`, which is NULL for every event in that case, so
   every timestamp reference failed against a buffer full of history. Fixed with
   `effective_time` and its single definition.
4. **`AROUND` failed for any anchor older than `LATEST` window.** The budget was spent on
   reading the newest page and filtering it in memory. Fixed with `history_around`'s two
   indexed seeks.
5. **`history_around`'s anchor read bound three parameters to a two-placeholder
   statement**, and its directional statements bound a limit they never declared. Both
   were driver-level errors surfacing as `HistoryUnavailable` — an indistinguishable
   failure for a real one. Caught by the first integration run and split into three
   statements rather than one with a sometimes-ignored parameter.
6. **An out-of-window reference was anchored onto the oldest retained event**, which made
   `AFTER <old bookmark>` silently skip that oldest message — off by one in the one
   direction a client cannot check. Fixed with `HistoryPosition::BeforeStart` and the
   asymmetric direction table.

Three latent test-harness defects are also recorded, because the same class would hide a
real defect later:

- assertions against the whole receive buffer, where the client was still being fanned the
  very traffic the test had just written upstream — so negative assertions passed
  vacuously and positive ones passed for the wrong reason;
- `batch_lines` counted batch *openings* rather than results, asserting one more than it
  meant to;
- a restart test re-created a durable Network, which is a configuration error rather than a
  no-op, so it was testing the controller instead of the index.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-store` — pass (14 unit + 60 qualification, 14 new)
- `cargo test -p i2pr-irc-runtime --lib` — pass (223 tests, 10 new in `search`)
- `cargo test -p i2pr-irc-runtime --test chathistory` — pass (19 tests, 2 new)
- `cargo test -p i2pr-irc-runtime --test m005e_search_history` — pass (15 tests, all new)
- `cargo test --workspace --all-features` — pass (29 suites)
- `./scripts/verify.sh quick` — pass
- `./scripts/verify.sh full` — pass

## Protocol transcript matrix

Every row below is asserted in `crates/runtime/tests/m005e_search_history.rs`.

| Request | Reply |
|---|---|
| `SEARCH in=#room text=quick` (one match) | `BATCH +n soju.im/search`, one `SOV SEARCH` line, `BATCH -n` |
| `SEARCH in=#room text=absent` | opened-and-closed empty batch |
| `SEARCH colour=red` | `FAIL SEARCH INVALID_SEARCH :unknown search selector` + empty batch |
| `SEARCH text` | `FAIL … :malformed search selector` + empty batch |
| `SEARCH after=yesterday` | `FAIL … :malformed timestamp selector` + empty batch |
| `SEARCH limit=0` | `FAIL … :invalid search limit` + empty batch |
| `SEARCH limit=99999` | `FAIL … :invalid search limit` + empty batch |
| `SEARCH in=#nowhere` | `FAIL … :malformed search selector` + empty batch |
| `SEARCH text=hello*` | `FAIL … :search text must be word characters only` + empty batch |
| `SEARCH after=<later> before=<earlier>` | `FAIL … :malformed search selector` + empty batch |
| `SEARCH` (no selector) | `FAIL … :no search selector given` + empty batch |
| `SEARCH …` without the capability | `421 <nick> SEARCH :Unsupported command`, no batch, no results |
| `SEARCH text=only` from Network 2 | empty batch; Network 1's text is not present |
| `SEARCH …` answered while another client is attached | nothing on the other client's socket |
| `SEARCH in=#room text=<deleted>` after retention | empty batch |
| `SEARCH in=#room text=retained` after a restart | byte-identical results, modulo the batch id |
| `SEARCH in=#room text=haystack limit=5` over 32 retained | exactly 5 results |
| `SEARCH in=#room text=haystack` over 32 retained | ≤ `DEFAULT_SEARCH_LIMIT` (50) results |
| `CHATHISTORY AROUND #room timestamp=<on an event> 5` | the event plus its neighbours, nothing outside |
| `CHATHISTORY AROUND #room timestamp=<past the end> 3` | the newest page |
| `CHATHISTORY AROUND #room timestamp=<before the start> 3` | the oldest page |
| `CHATHISTORY AFTER #room msgid=id1 2` | the two events after `id1`, not `id1` |
| `CHATHISTORY AFTER #room msgid=never-issued 2` | `FAIL CHATHISTORY MESSAGE_ERROR … :No retained message carries that reference` |
| `CHATHISTORY AFTER #room msgid=<duplicate> 2` | `FAIL CHATHISTORY MESSAGE_ERROR … :More than one retained message carries that msgid` |

## Findings

None open. Plan 024 introduced no architectural conflict and no new network boundary. The
one pre-existing defect it found — defect 2, the TEXT-versus-INTEGER comparison in
`recent_targets` — is fixed here rather than deferred, because the new time rule makes the
correct comparison unavoidable to state and a second rule in the same file is how it
would drift again.

## M005-F readiness

Plan 025 (M005-F, downstream IRCv3 protocol polish) is **unblocked and
dependency-ready**. Everything it needs is landed:

- **`standard-replies` can now be advertised honestly.** Plan 023 deferred the
  advertisement while emitting only the draft's `FAIL` form. Search refusals and the new
  `UnknownReference`/`AmbiguousReference` dispositions extend the same standard-reply
  surface across `CHATHISTORY`, `MARKREAD`, `BOUNCER`, and `SEARCH`, so the semantics
  behind the capability are now real rather than borrowed for a failure format;
- **`AROUND` is complete at every edge**, so M005-F can extend `CHATHISTORY` surface
  without finding a subcommand that only works inside a bounded window;
- **the search reply shape is settled** — batch type, `SOV` result line, bouncer-local
  `hit<id>` reference, empty-batch rule — which is the pattern M005-F's reply polish
  applies consistently;
- **`FAIL SEARCH` and the new history refusals give standard-replies real,
  already-tested instances** to generalise from.

Plans 026-028 remain gated on their sequential predecessors.