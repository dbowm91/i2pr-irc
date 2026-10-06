# History, cursors, and playback

## The journal is the runtime's adapter over the durable store

`HistoryJournal` owns buffer resolution, a bounded ingestion path, bounded backlog queries, monotonic cursor advance, and bounded retention. It holds no history of its own: the durable store is the journal, and every bound here is one the store also enforces.

## Canonical order is local sequence

`HistoryEventId` is canonical order. `server-time` and `msgid` are metadata.

The consequence is that a skewed, repeated, or missing server timestamp cannot reorder retained history — the locally assigned sequence is what orders it, and it is monotonic and never reused.

## Local outgoing messages are omitted

This plan stores **no** history for a local outgoing PRIVMSG/NOTICE.

A local socket write is not evidence of upstream delivery. A disconnect can leave delivery ambiguous, so storing it would label an unconfirmed write as history. Omission is the only choice that cannot produce a false confirmation claim.

When `echo-message` arrives (M003-D), the upstream echo becomes the canonical confirmed event instead. This is why the journal exposes no path that could mark a local write as delivered.

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