# Bouncer Core M003-D — Response Routing and Foundational IRCv3 Mediation

Status: ready for handoff

Blocker cleared:

- `plans/closure/bouncer-core/009-status.md` accepted

Authority:

- Research 003 conformance results/corpus
- Research 004
- canonical capability-mediation requirements

Primary class: capability

## 1. Objective

Make simultaneous downstream request/response behavior correct and introduce the stable IRCv3 foundations required by M003 history adapters.

Deliver:

- stable upstream capability policy;
- downstream message-tags/server-time/batch foundation;
- labeled-response translation/correlation;
- bounded command-specific fallback routing when upstream lacks labels;
- echo-message/history policy;
- multi-session concurrent query correctness.

## 2. Invariants

1. Upstream requested capabilities do not depend on which downstream client is attached.
2. Downstream capabilities are advertised only for semantics the bouncer itself can guarantee.
3. Downstream labels are never forwarded transparently upstream.
4. Response routes are generation-local and SessionId-scoped.
5. ClientId is never encoded into upstream labels.
6. Route/correlation counts and lifetimes are bounded.
7. Session detach/generation replacement cancels its routes.
8. No generic FIFO routes arbitrary unlabeled responses.
9. Client-only tags remain default-deny except explicitly generated/reviewed tags; M004 may widen policy.
10. Existing wire/tag budgets remain authoritative.

## 3. Upstream capability policy

When advertised, request the reviewed foundational set independent of downstream clients:

- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message;
- existing account/state capabilities already supported by the bouncer.

Keep SASL behavior stable.

Record requested/negotiated capabilities in generation-owned state.

Do not blindly request every server capability.

## 4. Downstream capability policy

Add a capability registry owned by bouncer semantics.

M003-D should make these available where their semantics are fully implemented:

- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message only under the chosen truthful policy.

Draft history capabilities remain M003-E.

Downstream CAP LS/REQ/ACK/NAK/END must obey the already-correct registration gate.

## 5. Tags/server-time

Preserve reviewed server tags subject to the anonymity policy.

For server-time:

- forward valid upstream server-time where present;
- otherwise synthesize bouncer receive time for stored/replayed events where downstream semantics require it;
- never use it as durable order.

Unknown client-only tags are rejected/stripped by the conservative policy until M004 explicitly reviews them.

## 6. BATCH

Implement bounded batch tracking/emission sufficient for:

- labeled multi-message responses;
- later chathistory batches.

Batch IDs are ephemeral/session-local downstream identifiers and are never durable.

Bound nesting/count/ID length according to reviewed IRCv3 behavior and local ceilings.

## 7. Labeled-response translation

For each accepted downstream labeled command:

~~~text
downstream label + SessionId
       ->
generation-local opaque upstream label
       ->
route { SessionId, original label, request class, deadline }
~~~

Upstream label length remains within protocol bounds.

Replies restore the original client label.

Multi-message responses associated with upstream BATCH follow the route until the terminating batch.

Single-response/ACK completion removes the route.

Timeout, session detach and generation replacement remove it.

## 8. Unlabeled fallback

When upstream lacks labeled-response, use explicit command-family correlation for the current supported query allowlist:

- WHOIS -> terminating 318;
- WHO -> 315;
- NAMES -> 366;
- LIST -> 323.

Initial safe concurrency may be one outstanding ambiguous fallback query per Network or per family.

A competing request receives a local bounded busy/error disposition rather than entering an unbounded queue or being guessed onto the wrong client.

Do not add fallback routing for a command until its reply/terminator semantics are specified and tested.

## 9. Echo-message

Choose and document one truthful downstream policy.

Preferred:

- when upstream negotiated echo-message, use the upstream echo for downstream echo/history confirmation;
- when unavailable, do not advertise downstream echo-message unless the bouncer implements equivalent semantics without falsely claiming confirmed delivery.

Never convert local stream write success into confirmed remote delivery.

## 10. Work packages

A. capability registry/upstream policy;
B. downstream CAP negotiation for stable foundational capabilities;
C. tag/server-time mediation;
D. BATCH;
E. label allocator/correlation;
F. unlabeled fallback router;
G. echo-message/history integration;
H. concurrent multi-session qualification.

## 11. Failure/restart/cancellation

Correlations are never persisted.

Upstream generation loss clears every route and ends/invalidates outstanding replies.

Session detach clears routes owned by that SessionId.

Late replies with unknown/stale labels are not delivered to another client.

Fallback router timeout releases its slot deterministically.

## 12. Tests

- upstream CAP request set identical with different attached clients;
- downstream CAP LS/REQ advertises only implemented caps;
- two sessions issue concurrent labeled WHOIS and receive only their responses;
- same downstream labels from two sessions do not collide upstream;
- old SessionId label cannot route to replacement SessionId;
- route ceiling/max+1;
- timeout cleanup;
- BATCH multi-response routing;
- upstream without labeled-response routes WHOIS/WHO/NAMES/LIST correctly;
- competing fallback request gets deterministic local busy/error;
- server-time preserve/synthesize behavior;
- client-only tag conservative rejection;
- echo-message policy under supported/unsupported upstream;
- history order unchanged by tags/timestamps.

## 13. Verification/docs

Full conformance corpus, fuzz smoke, boundary guard, Rust 1.88. Add capability/response-routing architecture and `plans/closure/bouncer-core/010-status.md`.

## 14. Acceptance criteria

Concurrent response routing is correct by SessionId, all correlation is bounded/generation-local, stable IRCv3 foundations are truthfully advertised, and upstream capability fingerprint is downstream-client independent.

## 15. Stop conditions

Stop if correct labeled-response support requires a wire change that fails the conformance corpus, if fallback semantics are ambiguous for a claimed supported command, or if echo-message cannot be advertised truthfully under the chosen history model.

## 16. Closure evidence

Record capability matrices, label namespace/correlation bounds, fallback completion matrix, concurrent session fixtures, echo-message disposition, exact verification, and M003-E readiness in `plans/closure/bouncer-core/010-status.md`.
