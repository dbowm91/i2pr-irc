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

The downstream advertisement is a function of the upstream negotiation, not a static
list: `message-tags`, `batch`, `labeled-response`, `server-time`, `standard-replies`,
`cap-notify` and `draft/no-implicit-names`, plus the implemented `draft/chathistory`,
`draft/read-marker`, `draft/pre-away`, `soju.im/search` and `soju.im/bouncer-networks`
adapters. `echo-message` and the five member-state capabilities are the conditional members — see
below and [member-state.md](member-state.md).

One cell holds it, shared by the owner and the session reader, so `CAP LS`, `CAP REQ` and
the `005` welcome cannot answer from different sets. The full contract, including the
per-session refusals and the `cap-notify` rules, is in
[downstream-protocol.md](downstream-protocol.md).

`chathistory` and `read-marker` are advertised only to the extent they are actually served, and the advertised surface is enumerated in [chathistory.md](chathistory.md). Deferring a capability is a reviewable decision; omitting it by accident is not, and neither is advertising an extension that is only half implemented.

`CAP REQ` is all-or-nothing. A request naming one unavailable capability is NAKed as a whole, so a client is never left guessing which half of its request took effect.

A `CAP` line from upstream is consumed by the bouncer and never fanned out. Relaying it
would show a local client the upstream's capability negotiation under the upstream's own
prefix, which the client reads as the server addressing it — and it discloses the upstream
connection's shape to every attached Operator.

## Member-state capabilities are conditional, because mediation is real

`extended-join`, `account-notify`, `away-notify`, `multi-prefix` and `setname` are requested
upstream when offered and advertised downstream only once *acknowledged*. A server can offer
a capability and refuse it, so the acknowledgement is the only statement about the connection
in hand.

The condition is the same one `echo-message` has and for the same kind of reason: the
bouncer mediates what the server supplied. A `multi-prefix` bouncer that never negotiated
`multi-prefix` has no complete prefix runs to widen, and advertising it would promise every
client a NAMES list richer than any client could ever receive.

Advertising them is the easy half. What each one asserts -- that a JOIN, an `ACCOUNT`, an
`AWAY`, a `SETNAME` and a prefix run are each delivered at the surface *this* session
negotiated, and not at the surface some other client on the same Network negotiated -- is in
[member-state.md](member-state.md).

## echo-message is conditional, because confirmation is real

The bouncer confirms a message only once the **server** has echoed it. A local socket write is not evidence of upstream delivery.

`echo-message` is requested upstream when offered and advertised downstream only once it
is enabled — without an upstream echo, advertising it would promise confirmation the
bouncer cannot deliver.

M005-F made that promise real. The upstream echo is the confirmation event and the only
point at which a local message enters history, and it is recorded once as `Outbound`. The
initiator receives the echo as an ordinary frame from the server, not as a synthetic local
one. A `time` tag is required for the direction judgement: without one there is no
evidence the frame is an echo rather than a late conversation line, and guessing would put
someone else's words in the Operator's own outbound history.

If a server withdraws `echo-message` with `CAP DEL`, the capability is removed from the
downstream advertisement and every session that negotiated `cap-notify` is told.

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

`server-time` is preserved from upstream exactly, in the canonical `YYYY-MM-DDThh:mm:ss.sssZ` form the extension defines, or synthesized from bouncer receive time when an event had none. It is metadata only: canonical order remains `HistoryEventId` and tag mediation cannot influence it.

Preservation is literal. The wire grammar is a UTC calendar timestamp, not an epoch instant, and a leap second is legal on the wire — most calendar libraries silently normalise `:60` to `:59`, which would make replayed history disagree with the server. The bouncer therefore uses its own bounded `IrcTimestamp`, which accepts a leap second only at the sole position UTC allows one (`23:59:60`) and orders it correctly between `23:59:59.999` and the following `00:00:00.000`.

No integer epoch value is ever emitted as a `time` tag, and an integer epoch arriving *in* a `time` tag is not valid server-time at all.

## BATCH identifiers are ephemeral

A batch exists only inside one live generation on one connection. A persisted batch id would outlive its connection and could collide with an unrelated batch.

Nesting (4), open batches (64), identifier bytes (32), and messages per batch (128) are all explicitly bounded; an over-long batch is force-closed rather than tracked.