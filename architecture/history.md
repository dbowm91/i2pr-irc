# History, cursors, and playback

## The journal is the runtime's adapter over the durable store

`HistoryJournal` owns buffer resolution, a bounded ingestion path, bounded backlog queries, monotonic cursor advance, and bounded retention. It holds no history of its own: the durable store is the journal, and every bound here is one the store also enforces.

## Canonical order is local sequence

`HistoryEventId` is canonical order. `server-time` and `msgid` are metadata.

The consequence is that a skewed, repeated, or missing server timestamp cannot reorder retained history — the locally assigned sequence is what orders it, and it is monotonic and never reused.

## Local outgoing messages are omitted

This plan stores **no** history for a local outgoing PRIVMSG/NOTICE.

A local socket write is not evidence of upstream delivery. A disconnect can leave delivery ambiguous, so storing it would label an unconfirmed write as history. Omission is the only choice that cannot produce a false confirmation claim.

When `echo-message` is negotiated, the upstream echo becomes the canonical confirmed event instead, and `echo-message` is advertised downstream only in that case. This is why the journal exposes no path that could mark a local write as delivered.

## Stored payloads carry no terminator

A stored payload is protocol content **without** CRLF, and CR/LF/NUL are refused on ingest.

Replay refuses a payload containing CR or LF outright rather than stripping it. Stripping would silently "repair" corrupted state into a valid-looking frame; refusing surfaces the problem. This is what guarantees one stored event can never be replayed as two lines.

## Ingestion is bounded and non-blocking

```text
upstream line  --try_send-->  bounded queue (256)  --owner loop-->  store append
                     | full: drop + count                    max 16 per turn
```

A full queue drops the event and increments a counter rather than buffering. An unbounded retry buffer would only trade memory pressure for history that arrives too late to matter, and history work never delays a keepalive answer.

A refused append is never reported as recorded. History loss shows up in bounded counters (`appended`, `append_refused`, `store_unavailable`, `history_dropped`), so it is visible rather than silent. History degradation never affects network delivery semantics.

That last point is a deliberate policy split. Best-effort durable history may be dropped, because losing it cannot desynchronize anything the client is currently receiving. A live IRC frame may not be dropped, because a downstream stream is ordered — see [network-ownership.md](network-ownership.md).

The owner drains at most a batch per turn and never spins to catch up, so under sustained load the bounded queue drains at whatever rate the loop is already taking turns. Every line handed to ingestion is accounted for exactly once — recorded, skipped, or dropped and counted — because a line that simply vanished would leave no trace anywhere.

## A buffer is resolved after the line that creates it

A line is history-eligible only for a channel whose `BufferId` the owner already knows. That buffer is resolved from an *observed self JOIN*, so the check has to run **after** the line has been applied to generation-owned state: the self JOIN is the line that creates the membership.

Checking before applying means the one line that confirms a channel never resolves that channel's buffer, and the channel would record no history until the server happened to send a second, redundant JOIN. Every line of a channel is therefore processed after its buffer is known, including later lines in the same read.

## Playback is bounded in both dimensions

The default automatic backlog is capped at 50 events **and** 64 KiB, per session, across at most 32 buffers.

An event that does not fit the remaining byte budget is not delivered at all:

- delivering it anyway would overshoot the byte ceiling;
- truncating it would send a *different* message than the one retained.

A session queue that refuses a frame ends delivery for that session. The cursor stays where it is, so the next attempt re-delivers from a known point.

## Cursors advance only after the bytes land

```text
queue QueuedFrame::Ack(payload, ack)
      -> writer writes to socket
      -> writer sends ack
      -> only now: cursor advances (monotonic)
```

A crash between the socket write and the cursor commit therefore **duplicates** on restart. Duplication is deliberate: a gap would be silent, while duplication is visible and self-correcting.

A stale acknowledgement never rewinds a cursor, and a cursor belongs to one `(ClientId, BufferId)` pair — never to the buffer alone.

## Retention clamps deterministically

Retention deletes in bounded chunks over a bounded number of passes, and reports `more_pending` so a caller schedules the next cycle instead of looping.

The clamp rule has two halves, and both matter:

| Cursor position relative to the removed range | Result |
|---|---|
| inside the removed range | clamped to the newest surviving event below it, or `0` |
| above the removed range | untouched — its event is still retained |

`0` means "before any retained event", so a future query from it replays the whole remaining buffer rather than skipping it.

## A cursor needs a durable client lineage

Playback state has to survive a restart, so it cannot hang off a locally invented client id. `ensure_client` resolves (or creates) the durable `ClientId` first; the store's foreign keys then guarantee no cursor can reference a lineage that does not exist.
## History is searchable without re-parsing it

Ingestion derives the bounded search fields — sender nick, target, body — from the message
it has already decoded, and writes them alongside the retained event in one transaction.
The store therefore never re-parses a stored line to answer a query, and never carries
protocol knowledge on the hot write path.

That derivation has exactly one exception: the schema 5 → 6 migration backfill, which must
rebuild an index for events the store already holds and cannot ask the runtime about them.
That is the single place the store decodes protocol, and it is bounded and one-off.

Ingestion also stamps `effective_time` — the time an event occupies in history, whether or
not the upstream ever sent a `server-time`. Every timestamp reference reads that column,
so a buffer whose upstream stamps nothing is still positionable. See
[history search and indexed references](history-search.md).

## Ingestion is not gated by what a client may see

History ingestion and downstream presentation are separate decisions, and detaching separates them explicitly.

A detached channel's traffic is withheld from fanout and from projection by default, and it is still applied to network state and still written to durable history. Its explicit local relay policy may pass only matching chat messages while leaving membership and channel state hidden. Filtering at ingestion instead would mean that reattaching a channel destroyed whatever happened while it was hidden — which would make hiding a channel indistinguishable, to the Operator, from losing it, and would make the retained history depend on whether anyone happened to be watching.

The consequence for replay is that a buffer table contains detached channels. Every replay path filters by visibility rather than trusting the buffer list: legacy automatic backlog skips detached buffers, and the read-marker projection only covers visible channels. A client must not be handed messages from a channel it has just been told the bouncer does not show.

Cursors are unaffected. They stay per-`(ClientId, BufferId)` and private, they remain the single delivery boundary, and they still advance only after the session writer confirms the bytes reached the socket. A client that was disconnected for an entire detached interval returns with its cursor where it left it and can query retained history normally. Reattachment projects observed state first, then offers the bounded privacy-eligible backlog through that same client cursor; no second cursor or replay model is created for detached channels.

## Detached channel activity remains local

Each desired channel may persist a detached relay mode (`none`, `mentions`, or `all`), an automatic reattach trigger (`off`, `message`, or `mention`), and a bounded inactivity detach duration. These settings affect only local presentation. Automatic timeout and reattach never issue upstream JOIN or PART commands; automatic reattach requires observed membership and uses the same truthful projection as the explicit command. Activity is evaluated from owner-parsed inbound channel PRIVMSG/NOTICE lines, using the negotiated IRC casemapping for nick boundaries. OTR payloads are opaque and cannot trigger mention behavior. A single generation-owned one-second timer scans at most the bounded desired-channel set and initializes deadlines from fresh monotonic time after reconnect.

Detached relay modes pass only channel chat lines. They do not reveal detached membership, topic, mode, or other control state. The defaults remain `none`/`off`/disabled, and a timer is constrained to 1–86400 seconds.

## Local watch notifications

Watch rules are durable operator configuration; matching results and message bodies are
not retained as notifications. The owner checks at most 128 literal rules once for each
parsed inbound message, skips OTR payloads, emits no more than eight hits for one event,
and coalesces each rule for two seconds. A local NOTICE contains only a process-unique
sequence and rule id. It goes through each attached session's existing bounded output
queue, is dropped without blocking IRC processing when that queue is full, and is never
replayed after reconnect. Diagnostics expose emitted and dropped counts without terms,
destinations, or bodies.
