# Capabilities and response routing

## Two directions, two policies

The failure this design prevents is conflating what the bouncer *asks for upstream* with what it *advertises downstream*.

| | Upstream | Downstream |
|---|---|---|
| When | once per generation | per client attachment |
| Rule | request the reviewed foundational set | advertise only what the bouncer can guarantee end to end |
| Depends on attached clients? | **never** | n/a |
| Driven by | what the server offered | what this build implements |

A client attaching must never change what was negotiated upstream. `UpstreamCapabilities::request_set()` is a pure function of the server's offer, so a generation's behavior cannot depend on transient client state.

Requesting everything a server offers would be the same error in the other direction: the bouncer would claim to support `extended-join` and `away-notify` without implementing either.

## Advertise only what you implement

Downstream advertisement covers `message-tags`, `server-time`, `batch`, and `labeled-response`, plus `echo-message` **only** when upstream negotiated it.

`chathistory` and `read-marker` are declared as an explicit constant that is deliberately not advertised yet. Deferring a capability is a reviewable decision; omitting it by accident is not.

`CAP REQ` is all-or-nothing. A request naming one unavailable capability is NAKed as a whole, so a client is never left guessing which half of its request took effect.

## echo-message is conditional, because confirmation is real

The bouncer confirms a message only once the **server** has echoed it. A local socket write is not evidence of upstream delivery.

So `echo-message` is advertised only when upstream negotiated it. Without an upstream echo, advertising it would promise confirmation the bouncer cannot deliver — and would retroactively make Plan 009's omission of local outgoing history look like a bug when it is the correct behavior.

## Labels are translated, never forwarded

A downstream label is chosen by one client and is meaningful only to that client.

```text
downstream label + SessionId
   -> generation-local opaque upstream label  (i2p<16 hex>)
   -> route { SessionId, ClientId, original label, class, deadline }
```

Forwarding the label would let two clients collide, and would leak one client's request identity to the server. A `ClientId` is never encoded into an upstream label; the generated token is a monotonic per-generation counter.

The original label is restored on reply, so the client sees its own request come back.

## Routes are generation-local and never durable

A route exists to answer a request made on a *live* generation. If it survived a restart it would name a connection that no longer exists, and the only possible outcome for a late reply would be misdelivery to whoever occupies the slot.

So the router is created per generation and discarded wholesale when the generation is replaced.

| Trigger | Effect |
|---|---|
| Session detach | routes for that `SessionId` dropped |
| Generation replacement | entire router discarded |
| Timeout (20 s) | slot released deterministically |
| Unknown/stale label | dropped, delivered to nobody |

A client that detaches and reattaches gets a fresh `SessionId` **and** a fresh upstream label, so a reply to the old request can never reach its replacement.

## No generic FIFO

An unlabeled reply arriving with no matching route belongs to no known request. Guessing it onto the oldest outstanding query would deliver one client's answer to another — worse than refusing.

Unlabeled correlation therefore exists only for the four families whose completion semantics are specified:

| Family | Terminator |
|---|---|
| WHOIS | 318 |
| WHO | 315 |
| NAMES | 366 |
| LIST | 323 |

Concurrency is one outstanding ambiguous query per family. A competing request gets a deterministic `Busy` disposition rather than an unbounded queue.

Adding a family requires writing and testing its completion rule first.

## Client tags are default-deny

Server-originated tags the bouncer understands are preserved. Client-only tags are stripped.

A bouncer that echoed arbitrary client tags upstream would let one client forge another client's `msgid`. Widening this is M004's decision, under explicit review.

`server-time` may be preserved from upstream or synthesized from bouncer receive time, but it is metadata only: canonical order remains `HistoryEventId` and tag mediation cannot influence it.

## BATCH identifiers are ephemeral

A batch exists only inside one live generation on one connection. A persisted batch id would outlive its connection and could collide with an unrelated batch.

Nesting (4), open batches (64), identifier bytes (32), and messages per batch (128) are all explicitly bounded; an over-long batch is force-closed rather than tracked.