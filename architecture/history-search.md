# History search and indexed references

## OTR and other endpoint-encrypted conversations

When an IRC endpoint supplies an OTRv3-encrypted message body, retained history and
CHATHISTORY replay preserve that opaque body. History may therefore replay old
ciphertext that the receiving endpoint can no longer decrypt. FTS indexes only the
bytes the bouncer receives; it cannot provide plaintext-content search for an
end-to-end encrypted conversation whose plaintext never entered the bouncer.
SQLCipher protects retained database pages at rest, but it does not turn ciphertext
search into semantic search and its store key is independent of endpoint OTR keys.

Plan 024 / M005-E. This document records the two things Plan 024 delivered: a bounded
server-side search over retained history, and the indexed reference lookups CHATHISTORY
needed but did not have.

## Draft revision implemented

`crates/runtime/src/search.rs` exports `ADAPTER_REVISION`:

```text
soju.im/search selectors in,from,after,before,text,limit (M005-E reviewed surface)
```

Every `soju.im/search` literal and every selector keyword lives in that one module. The
store speaks `SearchQuery` and never sees draft syntax, which is the same separation
`draft/chathistory` keeps. A test asserts the revision string names the draft and never
contains raw `MATCH`, so a future draft that adds an expression selector cannot land
without this string changing.

### Every parameter is a selector

`SEARCH` names no target of its own — the target is `in=` — so there is **no leading
parameter to skip**.

This is worth recording because getting it wrong was silent and serious. An earlier
version of `parse_search` skipped the first parameter on the assumption that it was a
target, so `SEARCH in=#room text=quick` parsed as `text=quick` and the `in=` scope was
discarded. Every search still returned plausible results; they were just not scoped to
the channel the client named. Unit tests missed it because they called `parse_selectors`
directly and never went through the parameter handling. The integration test in
`crates/runtime/tests/m005e_search_history.rs` caught it, and the regression test
`every_parameter_of_a_search_command_is_a_selector` now goes through `parse_search` so the
gap cannot be tested around again.

## Text is a literal, never an expression

`SearchTerm` accepts a bounded run of Unicode alphanumerics and nothing else — no
quotes, no operator characters, no punctuation FTS5 treats as syntax. `match_expression()`
quotes each term and joins them with `AND`, which is a literal this code chose rather than
one the client typed.

The alternative, escaping a raw expression, makes every future FTS5 operator a potential
escape: the escaping has to be re-derived rather than prevented. Here there is nothing for
client text to *be*.

## What is searchable, and what is deliberately not

Stored inbound `PRIVMSG`/`NOTICE`, and nothing else. Every additional event class widens
what a client can find without any corresponding statement that it should. A `JOIN` is
therefore never searchable, and the integration suite asserts it.

The searchable representation is **derived at ingestion** by the runtime, from the message
it had already decoded — sender nick, target, body. The store carries protocol knowledge
in exactly one place: the migration backfill, which has to rebuild an index for events the
store already holds and cannot ask the runtime about them.

A payload that will not decode is still indexed, with empty fields. Skipping it would make
the index row count disagree with the retained row count, and `verify_search_index` refuses
the open on disagreement. A message that can never match a term costs one row; a message
that made every later open fail costs the Operator their history.

## The FTS table is an index, not a source of truth

`history_search` is an FTS5 virtual table whose **rowid is the `HistoryEventId`**.

That single decision is what makes retention exact. Deleting a retained row and deleting
its index entry are one statement each, in one transaction, by exact id — not a join that
could half-succeed. An index row can never name an event that does not exist, and a
retained event can never be missing its index row, because they are written together in
the append transaction.

`verify_search_support` checks `ENABLE_FTS5` at open and at migration rather than trusting
the dependency list. A build without FTS5 would otherwise create a database that advertises
a searchable history it cannot search.

## Times are canonical text, everywhere

`history_events` gains `effective_time` in schema 6.

`server_time` alone cannot answer a timestamp reference. An upstream that never sends
`server-time` leaves every `server_time` NULL, so an index over that column is empty for
the whole buffer and every `timestamp=` reference fails against a buffer full of history.
`effective_time` is the time an event *occupies* in history: the upstream stamp when there
was one, otherwise the local receive time converted to the same canonical text.

It is a separate column rather than a write into `server_time`. That column records what
the upstream actually said, and inventing a value there would turn "the upstream sent no
timestamp" into a false claim.

The rule has **one definition**, `i2pr_irc_store::effective_time`, shared by the append
path, the migration backfill, and the runtime's `time=` tag rendering. A second copy is
how a replayed message and a timestamp reference would come to disagree about where a
message sits.

Every comparison against it binds canonical text, never integer milliseconds:

```text
history_events.effective_time is TEXT holding fixed-width UTC,
so lexicographic order is chronological order.
```

Binding an `INTEGER` parameter instead compares TEXT against INTEGER, and SQLite orders
every TEXT value after every INTEGER value regardless of the numbers involved — so the
predicate matches either everything or nothing while still looking like a time comparison.
`recent_targets` had exactly this bug and shipped it silently; `canonical_time` is now the
only way a bound reaches SQL.

## A search window is half-open

`after=` is inclusive, `before=` is exclusive: `[after, before)`.

A whole-millisecond timestamp then cannot fall in both halves or in neither, and
`after == before` is a genuinely empty window rather than a single instant that happened to
match. `SearchQuery::validate` refuses `after >= before` rather than swapping the pair,
because a swapped range is a different question from the one that was asked.

Results are ordered by `HistoryEventId` ascending, not by time. That keeps a search
consistent with CHATHISTORY paging: a client that searches, gets event ids, and then asks
for the range between two of them sees a contiguous page.

## A reference is a position, and positions can be outside the window

CHATHISTORY reference resolution is now two indexed seeks rather than a bounded window
read and an in-memory filter:

| Form | Lookup |
|---|---|
| `msgid=` | `history_by_msgid` on `(network_id, msgid)` |
| `timestamp=` | `history_by_time` on `(buffer_id, effective_time, event_id)` |

`HistoryPosition` has two values, and the second one is the interesting one:

- `Event(id)` — at or inside the retained window.
- `BeforeStart` — earlier than every retained event.

An older implementation anchored a too-old reference onto the oldest retained event.
That is off by one in the one direction a client cannot check: `AFTER <old bookmark>`
silently skips the oldest message, and the client has no way to notice. The out-of-window
case therefore gets its own value rather than being forced through the in-window one.

The direction table, which is the whole of `execute`:

| | `Event(e)` | `BeforeStart` |
|---|---|---|
| `BEFORE` | events before `e` | **empty** |
| `AFTER` | events after `e` | everything retained |
| `AROUND` | centred on `e` | the oldest page |
| `LATEST *` | newest page after `e` | the newest page |

`BEFORE` from before the beginning is empty and that is the truthful answer: no retained
message is earlier than the start of retention. Handing back the oldest page would claim
messages exist that do not. The two directions are deliberately not symmetric, and
`HistoryPosition::lower_bound` and `is_before_start` are separate for that reason.

## `msgid` ambiguity is an answer, not a bug

`MsgidLookup` is `Unique` / `Ambiguous` / `Missing`, and all three reach the client:

| Condition | `HistoryRefusal` | Reported as |
|---|---|---|
| exactly one retained event carries it | — | the position |
| more than one does | `AmbiguousReference` | "More than one retained message carries that msgid" |
| none does | `UnknownReference` | "No retained message carries that reference" |

A duplicate upstream id is a real condition — an upstream that reuses one after a restart
is ordinary — and picking the lowest `HistoryEventId` would answer a question the client
did not ask while looking authoritative.

`UnknownReference` is distinct from `HistoryUnavailable` on purpose. After retention ran
successfully, the only true statement about a pruned id is that nothing carries it; a
client told "unavailable" would retry something that will never succeed.

## `AROUND` is two seeks, not a window

`history_around` reads the anchor plus `before` events walking backwards and `after`
events walking forwards, each with its own budget, then reverses the before-side into one
ascending run.

Reading a window of the newest events and filtering it in memory cannot produce this: the
anchor may be older than any such window, so the window would have to grow until it
happened to contain the anchor — which is the unbounded scan the operation exists to
remove. Before this change, `AROUND` with an anchor older than `LATEST` window simply
failed.

A missing anchor is **not** an error. Both sides are still returned, because a client
paging from a bookmark retention has since removed is exactly the case that has to survive.
An empty result therefore means "nothing on either side", never "the anchor was pruned".

## Bounds

| Bound | Value | Where |
|---|---|---|
| `SEARCH` line | ≤ `MAX_LINE_BYTES` | wire decoder |
| `SEARCH` parameters | 16 | `MAX_SEARCH_PARAMS` |
| Channels per search | 16 | `MAX_SEARCH_BUFFERS` |
| Terms per search | 8 | `MAX_SEARCH_TERMS` |
| Bytes per term | 64 | `MAX_SEARCH_TERM_BYTES` |
| Bytes per field | 1024 | `MAX_SEARCH_FIELD_BYTES` |
| Results per search | 256, default 50 | `MAX_SEARCH_RESULTS` |
| Bytes per reply | 256 KiB | `MAX_REPLY_BYTES` |
| Ambiguous `msgid` matches | 8 | `MAX_MSGID_AMBIGUOUS` |
| `AROUND` total events | ≤ `MAX_HISTORY_QUERY_EVENTS` | `HistoryAround::validate` |

Every one is enforced at the parser *and* at the store. A parser that trusted the store
would let a future caller skip the parser entirely; a store that trusted the parser would
be one SQL string away from an unbounded scan.

`HistoryAround::validate` bounds the **total**, not each side. Two halves each at the
ceiling would be twice the work one `MAX_HISTORY_QUERY_EVENTS` answer is allowed to
represent.

## Reply framing

Search replies are framed in one BATCH of type `soju.im/search`:

```text
:bouncer BATCH +<id> soju.im/search
:BouncerServ SOV SEARCH buffer=buffer-<id> msgid=hit<event> sender=<nick> target=<target> text=<body>
:bouncer BATCH -<id>
```

Batch identifiers are **generation-local**, on the same footing as response routes: an id
from an earlier connection must not be referencable by a client that reconnects and asks
the same question again. They wrap at 1 rather than 0 or saturating.

An empty result is still an opened-and-closed batch. A client that asked and heard nothing
has to guess whether to wait, and "finished, and there was nothing" is the only answer that
ends the question.

A refusal is a fixed `FAIL SEARCH INVALID_SEARCH :<reason>` line followed by the same
complete empty batch. The reason is a constant — never the offending selector or its
value — because a refusal that echoed the request would turn every bound that is not met
into a way to put arbitrary client text into a frame the Operator sees.

`msgid` on a result is the bouncer's own `hit<HistoryEventId>`. An upstream `msgid` is
never claimed as a reference this bouncer can resolve, and the shape is visibly distinct
from one an upstream issued.

## One Network, chosen by the owner

`compile` takes the `NetworkId` from the journal the owner already holds, never from the
request. That is the only thing standing between a typo and a cross-Network disclosure,
so it is chosen where the ownership is.

The store applies the scope in the same statement as the match, so a Network filter
cannot be forgotten by a later edit to the query text. `in=` resolves through the owner's
own casemapped target map, so `#Room` and `#room` are one channel and a name that does
not resolve is **refused** rather than searched as an empty scope — answering a typo with
"no matches" reads as a true statement about the journal and is not.

Replies go only to the session that asked. History is the bouncer's own state; a search is
a lookup a client makes against itself, and it never crosses the upstream connection or
another client's socket.

## What was removed, and why

`HistoryJournal::reference_candidates` is gone.

It read a bounded window of a buffer and filtered it in memory to find one message. Every
reference resolution went through it. That was the gap this plan set out to close, and
leaving it in place would have meant the store had two reference paths with different
answers — one indexed and one approximate — and no test would have said which one ran.
