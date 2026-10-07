# Bouncer Core Corrective 014 — Live Multi-Client Response Routing

Status: closed

Repository baseline: `11e49d286fc2a442ffbf06ca8d0517338a155b3a`

Owns unresolved finding:

- UF-013-1 from `plans/closure/bouncer-core/013-status.md`

Research authority:

- `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Historical implementation authority:

- `plans/implementation/bouncer-core/010-m003d-response-routing-and-ircv3-foundation.md`
- `plans/closure/bouncer-core/010-status.md`

Primary class: invariant + protocol correctness corrective

## 1. Objective

Make the already-existing response-routing machinery live end to end before M004 qualification begins.

The current runtime constructs, expires and drops `ResponseRouter` state but never allocates a route on the live client-intent path. WHOIS/WHO/NAMES/LIST replies therefore still travel through ordinary fanout.

Corrective 014 must ensure one client's generation query is answered only to the requesting SessionId, with bounded labeled-response translation when available and bounded command-specific fallback otherwise.

## 2. Required invariants

1. Every live response route is generation-local.
2. Every route is owned by one SessionId, never only by durable ClientId.
3. A downstream label is never forwarded transparently upstream.
4. A stale SessionId cannot receive a later reply.
5. Generation replacement clears every route.
6. Failed upstream queue admission leaves no route.
7. Route count is explicitly bounded.
8. A competing unlabeled fallback request is refused locally rather than guessed onto another client.
9. Ordinary unsolicited upstream events still fan out normally.
10. No routing change widens the I2P-only network boundary.

## 3. Scope

### In scope

- connect `SessionIntent::Forward { class: GenerationQuery }` to `ResponseRouter`;
- inspect current negotiated upstream labeled-response state;
- allocate opaque generation-local upstream labels;
- rewrite outgoing query labels;
- consume upstream labeled replies before ordinary fanout;
- route associated response BATCHes to the owning SessionId;
- implement/activate bounded WHOIS/WHO/NAMES/LIST fallback when labels are unavailable;
- clean routes on send refusal, timeout, detach and generation loss;
- reconcile downstream capability advertisement so labeled-response is advertised only when the live path can honor it;
- integrated multi-client tests.

### Out of scope

- CTCP/DCC anonymity policy;
- client-tag policy;
- reconnect scheduler;
- new query command families;
- router integration.

## 4. Labeled path

When upstream labeled-response is enabled:

1. classify the outgoing GenerationQuery;
2. preserve the downstream client's original label if present;
3. allocate a bounded opaque upstream label not containing ClientId, SessionId, network name, nick or other identifying material;
4. register route metadata containing SessionId, original downstream label, request class, generation and deadline;
5. only after route allocation succeeds, attempt bounded upstream queue admission;
6. if admission fails, remove the route before reporting local failure;
7. match incoming labeled replies and any associated BATCH only to the route owner;
8. restore the original downstream label only for that client;
9. close route on the command-specific completion condition / batch completion / timeout.

Do not send routed reply frames through ordinary fanout.

## 5. Unlabeled fallback

When upstream lacks labeled-response, activate the already-planned conservative command-family routing.

Initial families:

- WHOIS, terminator 318;
- WHO, terminator 315;
- NAMES, terminator 366;
- LIST, terminator 323.

Initial safe concurrency:

- at most one outstanding ambiguous request per family per Network, or a stricter one-outstanding-query-per-Network policy if that keeps the implementation simpler and provably correct.

A competing request receives an explicit local busy/failure response.

No generic numeric FIFO is allowed.

## 6. Upstream reply interception

Incoming messages must be offered to the router before ordinary downstream fanout.

The router may classify a message as:

- consumed for one SessionId;
- part of an open routed batch;
- route completion;
- unrelated/unsolicited => ordinary fanout.

State-mutating upstream events must still be applied to NetworkState exactly once even when their wire frame is routed to only one client.

Do not duplicate a routed reply through both route delivery and fanout.

## 7. Capability reconciliation

Audit the current split capability surfaces.

If `labeled-response` is advertised downstream, live semantics must be real.

If foundational `message-tags` / `batch` are prerequisites for the chosen downstream labeled-response behavior, advertise/mediate them consistently or withhold labeled-response until the complete combination is live.

Do not leave one module claiming support while the live SessionReader advertises a different set.

## 8. Failure/cancellation

- route allocation full => request refused locally;
- upstream normal queue full after route allocation => route removed then local failure;
- session detach => drop all routes for SessionId;
- generation replacement => drop all routes;
- timeout => route removed and local timeout/failure delivered if session still exists;
- malformed routed response => fail closed without misrouting to another session.

## 9. Required tests

- two sessions issue concurrent labeled WHOIS with same client label and receive only their own replies;
- route labels differ upstream;
- original downstream labels restored;
- multi-line WHOIS routing;
- BATCH-associated routed response;
- queue max/max+1;
- queue admission failure removes allocated route;
- detach before reply cannot route to replacement session using same ClientId;
- generation replacement clears routes;
- upstream without labels routes WHOIS/WHO/NAMES/LIST by terminator;
- competing fallback request gets local refusal;
- unrelated PRIVMSG still fans out;
- routed reply not duplicated through fanout;
- upstream state is still applied once;
- capability advertisement matches actual live semantics;
- Rust 1.88/full verification green.

## 10. Acceptance criteria

Corrective 014 closes only when a real attached client query creates a route and a real upstream response reaches only the requesting SessionId in both labeled and fallback modes.

## 11. Stop conditions

Stop and register a new design if:

- reply isolation requires shared mutable NetworkState outside the owner;
- correct routing requires unbounded correlation;
- a claimed command family has ambiguous completion semantics that cannot be bounded safely;
- capability mediation requires widening generic networking.

## 12. Closure evidence

Create `plans/closure/bouncer-core/014-status.md` including:

- route lifecycle matrix;
- labeled and unlabeled concurrency fixtures;
- route-bound max/max+1;
- session/generation cancellation evidence;
- capability reconciliation;
- exact verification;
- explicit readiness for M004-A and M004-B.
