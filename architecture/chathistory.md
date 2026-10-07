# CHATHISTORY and read markers

## One adapter, one revision

Every `draft/...` literal, subcommand keyword, and reference grammar lives in `crates/runtime/src/chathistory.rs`. The store and the rest of the runtime speak generic query keys and durable identities; they never see draft syntax.

That is what lets a future wire adapter update ship without a schema migration — unless the semantic data requirements actually changed. The capability registry refers to the adapter's constants rather than repeating the literals.

## The grammar is positional per subcommand, and the limit is last

There is no shared positional extraction scheme, because there is no shared shape. Each subcommand has an exact parameter count and the limit is **always** the last parameter.

| Subcommand | Shape |
|---|---|
| `BEFORE` | `<target> <reference> <limit>` |
| `AFTER` | `<target> <reference> <limit>` |
| `LATEST` | `<target> *\|<reference> <limit>` |
| `AROUND` | `<target> <reference> <limit>` |
| `BETWEEN` | `<target> <reference> <reference> <limit>` |
| `TARGETS` | `<reference> <reference> <limit>` — no target |

`TARGETS` names buffers rather than messages, so it has no target argument at all and both of its selectors must be timestamps. Treating it as an ordinary query against a named buffer is how the first implementation answered it, which meant it answered a question nobody asked.

A reference must carry its selector prefix: `timestamp=<server-time-format>` or `msgid=<opaque-id>`. A bare token is a syntax error, not a msgid — so `CHATHISTORY BEFORE #c some-junk 50` cannot look like a reference to an identifier the server never issued.

`MAX_HISTORY_LIMIT` is the single source of truth for both the parser ceiling and the advertised `CHATHISTORY=50` ISUPPORT token, with a test asserting they cannot drift apart.

## Advertised means served

Advertising `draft/chathistory` promises a specific server-side contract, and an extension that is half-implemented makes the advertisement a lie:

- `CHATHISTORY=<max>` and `MSGREFTYPES=timestamp,msgid` are emitted as ISUPPORT;
- results arrive in a BATCH with batch type `chathistory`, `TARGETS` with `draft/chathistory-targets`, and each message carries `batch=`;
- an empty result is an empty successful batch, not silence;
- a bounded standard-reply `FAIL` is returned for a malformed request;
- only PRIVMSG/NOTICE is replayed, because event-playback is not negotiated and is not offered.

`AROUND` is implemented rather than refused. Refusing part of an advertised extension is the same class of lie as not advertising it at all: its budget splits as `before = (limit - 1) / 2`, `after = limit - 1 - before`, with the selector itself counting as one slot. Since Plan 024 that bracket is two indexed seeks rather than a window read, so it works for a selector anywhere in the retained history rather than only inside `LATEST` window.

Replay is stamped only for a session that negotiated `server-time`; one that negotiated
`draft/chathistory` without it receives the message with no tags at all. See
[downstream IRCv3 protocol polish](downstream-protocol.md).

The `MSGREFTYPES=timestamp,msgid` claim is reviewed against what is actually served in
`crates/runtime/tests/m005e_search_history.rs`: both reference kinds resolve for every
subcommand including `AROUND`, at every window edge, and both refusal dispositions are
reachable over the wire. The token is truthful as written.

Downstream CAP negotiation ACKs exactly the capabilities actually served — `draft/chathistory` and `draft/read-marker` — and refuses a partially supported request as a whole rather than half-acknowledging it.

## Queries are bounded twice

| Bound | Value |
|---|---|
| Events per query | ≤ 50 (`MAX_HISTORY_LIMIT`) |
| Bytes per response | ≤ 256 KiB |
| `LATEST` window scanned | ≤ 512 events |
| `AROUND` total events | ≤ `MAX_HISTORY_QUERY_EVENTS` (512) |
| Ambiguous `msgid` matches reported | ≤ 8 |

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

## References resolve through the index, or they are refused

| Form | Lookup | Tie-break |
|---|---|---|
| `msgid=` | `history_by_msgid` on `(network_id, msgid)` | — |
| `timestamp=N` | `history_by_time` on `(buffer_id, effective_time, event_id)` | largest `HistoryEventId` |

Neither lookup scans. Before Plan 024 both resolved by reading a bounded window of the
buffer and filtering it in memory, so a reference older than that window simply failed —
which is exactly the case bookmarks exist for. See `architecture/history-search.md`.

Resolution is bounded and never guesses, but the bounds are now seeks rather than a
window, so a reference that lands outside the retained window is not an error:

| Condition | Answer |
|---|---|
| resolves to a retained event | that event |
| predates everything retained | `HistoryPosition::BeforeStart` |
| `msgid=` carried by exactly one retained event | that event |
| `msgid=` carried by more than one | `AmbiguousReference` |
| `msgid=` carried by none | `UnknownReference` |

`BeforeStart` is a position rather than a failure, and the directions are deliberately
not symmetric: `AFTER` from before the beginning returns everything retained, while
`BEFORE` returns nothing — no retained message is earlier than the start of retention.
An earlier implementation anchored a too-old reference onto the oldest event, which
silently dropped that oldest message from `AFTER`. Full table in `history-search.md`.

`UnknownReference` is distinct from `HistoryUnavailable` on purpose: after retention ran
successfully, the only true statement about a pruned id is that nothing carries it.

Ties always resolve through local identity, never a timestamp comparison. An `AROUND`
selector resolves the same way, so a bracketed window is centred on a real retained event
— and it is centred on one even when that event is older than any bounded window.

## One client, one synchronization mode

| Client | Initial synchronization |
|---|---|
| legacy | bounded automatic backlog, gated on the playback cursor |
| `chathistory` | query-driven history, **no** automatic backlog |

`SessionCapabilities::with_negotiated` records that a session manages its own history, and `wants_backlog` then returns false. Giving a `chathistory` client the automatic backlog too would deliver the same messages twice.

`read_markers` is tracked separately from `explicit_history` because the two drafts are independently negotiable. Inferring one from the other would silently withhold or deliver something the client did not ask for.

Issuing a `CHATHISTORY` query does **not** advance the playback cursor: query delivery is not playback state. Backlog delivery happens exactly once, at projection time, so changing CAP state after registration never replays initial history.

## Read state is local and only moves forward

A marker is shared per buffer for the single Operator, and is distinct from any client cursor.

```text
new_marker = max(old_marker, resolved_event)
```

A second client reporting an older message cannot un-mark what the operator has already read. Read state is never sent upstream.

A client **set** resolves the named timestamp to a durable retained event before storing it, which is what makes monotonicity hold: a client cannot name an arbitrary instant to skip ahead. The server answers with the value it actually stored, which may be older than requested.

`MARKREAD *` is **not** a client operation. `*` is the server's own unknown-marker reply, and accepting it from a client would let one session erase another's read state, so a client that sends it is refused.

When retention removes the range a marker named, the cursor clamp rule applies unchanged to the marker: it moves to the newest surviving event below the removed range, or `0` when none exists. It stays valid rather than dangling.

### What the Operator's other sessions are told

Read state belongs to the Operator, not to one attachment, so a set that actually **advances** the marker is propagated to every other attached session that negotiated `draft/read-marker`. A set that moves nothing is not propagated: it is not an update.

A client that never negotiated the draft is never sent a `MARKREAD` at all, in a propagation or in the initial projection. An unnegotiated command is a protocol violation for a strict client, and both the initial marker and the broadcast are things the client asked for.

## Refusals are typed

`UnknownSubcommand`, `MissingParameters`, `TooManyParameters`, `InvalidTimestamp`, `InvalidReference`, `InvalidLimit`, `UnsupportedReferenceType`, `NoSuchBuffer`, `UnknownReference`, `AmbiguousReference`, `HistoryUnavailable`, and the MARKREAD set of `MissingParameters`, `TooManyParameters`, `InvalidTarget`, `InvalidTimestamp`, `NoSuchBuffer`, `Internal` — each renders a stable standard-reply code and never a payload.

`CHATHISTORY` and `MARKREAD` are answered locally and never forwarded upstream: the server has none of this history. A refusal is still a reply — a client learns why nothing arrived instead of waiting forever — and refusing a command is never grounds for ending the session.

## Commands reach the bouncer, not just its unit tests

`CHATHISTORY` and `MARKREAD` arrive as typed `SessionIntent` values and are executed by the owning generation. Sessions hold no store handle; a session submits a bounded typed intent instead, which is what keeps one live owner per Network.

The dispatch is conditional on the negotiated capability. A client that never negotiated the extension is refused with an explicit `421` rather than having the command silently fall through to an unknown-command path.