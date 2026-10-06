# CHATHISTORY and read markers

## One adapter, one revision

Every `draft/...` literal, subcommand keyword, and reference grammar lives in `crates/runtime/src/chathistory.rs`. The store and the rest of the runtime speak generic query keys and durable identities; they never see draft syntax.

That is what lets a future wire adapter update ship without a schema migration — unless the semantic data requirements actually changed. The capability registry refers to the adapter's constants rather than repeating the literals.

`ADAPTER_REVISION` states the reviewed surface: `LATEST`, `BEFORE`, `AFTER`, `BETWEEN`, `TARGETS`, plus `draft/chathistory` and `draft/read-marker`.

`AROUND` needs a bounded time index this milestone does not build, so it is **refused explicitly**. Answering it approximately would not be.

## Queries are bounded twice

| Bound | Value |
|---|---|
| Events per query | ≤ 50 |
| Bytes per response | ≤ 256 KiB |
| `LATEST` window scanned | ≤ 512 events |
| Reference candidates | ≤ 512 events |

`LATEST` needs the *newest* page, so it takes the tail of a bounded retained window rather than the head. An early version returned the head, which would have silently given a client the beginning of a buffer when it asked for the end.

An event that does not fit the remaining byte budget is skipped rather than truncated: truncating would send a *different* message than the one retained.

## Replay is truthful or it does not happen

Every emitted line carries:

- the exact retained target and message type;
- a `server-time` — the upstream value when present, otherwise the stored local receive time;
- a `msgid` **only** when one was genuinely preserved upstream.

A durable `HistoryEventId` is never exposed as a msgid. It would give an internal identity a meaning upstream clients interpret differently.

Results are ordered by `HistoryEventId`. A skewed `server-time` is metadata and cannot reorder anything.

### Why no event playback

Only PRIVMSG/NOTICE is stored, so no JOIN/PART/NICK can appear in a replay. That is deliberate: it is what keeps a history payload truthful without implementing event-playback semantics. The adapter physically cannot offer what the store does not hold.

## References resolve, or they are refused

| Form | Rule | Tie-break |
|---|---|---|
| `msgid` | exact match in the buffer | — |
| `timestamp=N` | newest event at or before N | largest `HistoryEventId` |

A reference to history that was never retained is refused with `StaleReference`. Approximating with an adjacent event would misrepresent what the client asked for — worse than saying no.

Ties always resolve through local identity, never a timestamp comparison.

## One client, one synchronization mode

| Client | Initial synchronization |
|---|---|
| legacy | bounded automatic backlog, gated on the playback cursor |
| `chathistory` | query-driven history, **no** automatic backlog |

`SessionCapabilities::with_negotiated` records that a session manages its own history, and `wants_backlog` then returns false. Giving a `chathistory` client the automatic backlog too would deliver the same messages twice.

Issuing a `CHATHISTORY` query does **not** advance the playback cursor: query delivery is not playback state. Backlog delivery happens exactly once, at projection time, so changing CAP state after registration never replays initial history.

## Read state is local and only moves forward

A marker is shared per buffer for the single Operator, and is distinct from any client cursor.

```text
new_marker = max(old_marker, resolved_event)
```

A second client reporting an older message cannot un-mark what the operator has already read. Read state is never sent upstream.

When retention removes the range a marker named, the cursor clamp rule applies unchanged to the marker: it moves to the newest surviving event below the removed range, or `0` when none exists. It stays valid rather than dangling.

## Refusals are typed

`UnsupportedSubcommand`, `InvalidLimit`, `InvalidReference`, `TooManyParameters`, `StaleReference`, `NoSuchBuffer` — each renders a stable reason and never a payload.

`CHATHISTORY` and `MARKREAD` are answered locally and never forwarded upstream: the server has none of this history.