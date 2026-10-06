# Response routing

A multi-client bouncer has a problem a single-client bouncer does not: when the upstream
server answers `WHOIS alice`, that answer is a reply to *one* client's question. Delivering
it to every attached client would disclose one client's lookup to all of them.

`crate::routing` (`ResponseRouter`) keeps the correlation. `crate::owner` drives it.

## Routes are generation-local and SessionId-scoped

A route is opened when a client issues a query whose replies are correlatable, and it holds
the `SessionId` that asked. Routes live inside the connection-generation actor, so a new
connection starts with an empty router: a reply tagged for a previous generation matches no
route and is dropped. This is why correctness falls out of structure rather than from a
timeout — there is no path by which a stale generation's reply can reach a current session.

Batch identifiers are generation-local for the same reason. A batch reference from an
earlier connection is not referable later.

## The label is translated, never relayed

A client may attach `@label=foo` to correlate its own request and receive the reply tagged
the same way. That label is an *end-to-end client concern*, so the bouncer must not let the
server see a value the client chose — it would be an uncontrolled string in upstream
traffic.

So the downstream label is replaced with an opaque generation-local token on the way out,
and translated back on the way in. Two clients using the **same** label concurrently produce
**different** upstream tokens, or their answers would be indistinguishable. The server sees
an opaque token; the client sees its own label.

## Only correlatable families

`RequestClass` is a closed set — `WHOIS`, `WHO`, `NAMES`, `LIST` — and a command outside it
is not routed at all. Each family's terminator is written down explicitly (`318`, `315`,
`366`, `323`), because "when does this reply end" is the part that is easy to get wrong and
expensive to get wrong: a route that never closes is a slow leak, and a route that closes
early drops part of an answer.

Adding a family requires writing and testing its completion rule first.

## What happens to unmatched replies

Three outcomes, deliberately distinct:

- **Routed** — the label matches a live route; the reply is rebuilt with that client's own
  label and delivered to that session only.
- **Unlabeled fanout** — an ordinary server-initiated frame, delivered to everyone. This is
  correct and not a fallback: most upstream traffic is server-initiated.
- **Dropped** — a reply carrying a label that matches no live route. It is counted as
  `orphaned_replies_dropped` precisely because it is never shown to any client. Delivering
  it would hand one client's discarded reply to another.

Unknown batch references also fan out: the server opened a batch nobody here asked for.

## The delivery rule

Numeric replies that belong to a family go to the asking session only. Frame *continuations*
inside a batch follow the batch's route, so a multi-line answer never splits across clients.

A client that cannot keep up is detached rather than served a partial answer: a live IRC
stream is ordered, so a skipped frame is unrecoverable and the client cannot be left
attached claiming to be synchronized with state it never saw.

See `network-ownership.md` for where the router lives in the owner loop and `ircv3.md` for
the label and batch wire format.