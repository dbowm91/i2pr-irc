# Corrective 014 — Live Multi-Client Response Routing

Status: closed

Corrects: `plans/implementation/bouncer-core/014-live-multiclient-response-routing-corrective.md`

Repository baseline: `04297bc` ("Link Corrective 014 from bouncer roadmap")

Implementation commit: `da08190` ("Route live multi-client responses by SessionId")

This document is the strict current authority for the routing surface. It supersedes
nothing: `plans/closure/bouncer-core/013-status.md` is preserved exactly as written,
including its UF-013-1 finding, which this plan closes rather than rewrites.

## Specifications reviewed

Re-read at handoff start, as the plan's gate requires. The reviewed labeled-response
specification requires the `label` tag to carry a **value** (`@label=pQraCjj82e`), not to
be a valueless tag. That single detail changed the emitted frame and is recorded below
because it is the kind of thing a plausible-looking implementation gets wrong.

| Extension | URL |
|---|---|
| labeled-response | https://ircv3.net/specs/extensions/labeled-response |
| batch | https://ircv3.net/specs/extensions/batch |

Worked example from the specification that shaped the design: a multipart answer arrives
as `@label=X BATCH +ref labeled-response`, then body lines carrying only `batch=ref`, then
`BATCH -ref` carrying **neither** a label nor a numeric.

## UF-013-1 — live response routing was not wired

**Disposition: closed.**

UF-013-1 recorded that `ResponseRouter` was constructed per generation and had
`drop_session` / `expire` called on it, but that **`route()` and `deliver()` were never
called from any live path**. That finding is now closed: both are on the live path.

The owner allocates a route for each correlated client query and consults the router
before ordinary fanout on every upstream line.

| Live path element | Where |
|---|---|
| route allocation, label translation, admission | `NetworkOwner::forward_routed` |
| reply classification and single-session delivery | `NetworkOwner::apply_upstream_line` |
| frame → router description, label restoration | `incoming_for`, `rebuild_reply` |

`SessionIntent::Forward` with `IntentClass::GenerationQuery` now routes; every other
class is unchanged, so `PRIVMSG`/`NOTICE`/`MODE` and the durable membership intents behave
exactly as before.

## Defects found and fixed while implementing

These were not in the plan. Each was found by writing the live path and testing it.

1. **Labels were emitted valueless.** The existing `render` produced `@i2p0...1 WHOIS
   alice`. Under the reviewed specification the `label` tag carries a required value, so
   the server cannot correlate a valueless tag against anything. Now emitted as
   `label=<opaque>`.

2. **Routing only the terminator would have leaked the answer.** The original design
   matched an unlabeled reply only on its family terminator. For `WHOIS` that means
   `RPL_WHOISUSER` (311), `RPL_WHOISSERVER` (312) and `RPL_WHOISCHAN` (317) would have
   fanned out to every attached client — disclosing one client's lookup to all of them —
   while only the terminating `318` was routed. `RequestClass::numerics()` now covers the
   whole family, and `MAX_MULTIPART_REPLIES` bounds how many replies one route can absorb.
   The families are pairwise disjoint, which is what makes "one outstanding query per
   family" sufficient to attribute a reply without guessing; a test asserts the
   disjointness rather than trusting it.

3. **A batched reply needed reference tracking.** Messages *inside* a
   labeled-response batch carry `batch=<ref>` and no label, so label matching alone
   delivers only the opener. `ResponseRouter` now tracks batch reference → route, and the
   unlabeled `BATCH -<ref>` frame is what completes a batched route. Completing at the
   numeric instead would orphan the closing frame and fan it out to everyone.

4. **`expect_batch` / `finish_batch` were a manual hazard.** They required the caller to
   notice a batch and call them in the right order. Batch state is now derived from the
   frames themselves; both methods are gone.

5. **A refused admission could leave a route behind.** Route allocation and upstream
   admission happen in the same owner turn, so a refused send cancels exactly the route it
   opened (`cancel_labeled` / `cancel_fallback`). Corrective 013's requirement that
   "failed query admission leaves no response route" held *vacuously* before this plan,
   because no route was allocated at all; it now holds by construction.

## Invariants enforced

| Invariant | Mechanism | Evidence |
|---|---|---|
| routes are generation-local | router constructed per generation; no durable route table | `a_generation_replacement_delivers_no_reply_from_the_old_one` |
| a detached client never receives a later reply | `drop_session` on detach, quit, and overload | `a_reply_arriving_after_a_detach_reaches_nobody` |
| no client label reaches the server | translated to a monotonic opaque token | `a_client_label_is_never_forwarded_upstream_...`, live concurrent test |
| no session/client identity in an upstream label | counter-only allocation | `an_upstream_label_never_encodes_a_session_or_client_identity` |
| a stale label matches nothing | `RouteOutcome::Dropped`, counted in `orphaned_replies_dropped` | `a_stale_label_is_dropped_and_never_fanned_out` |
| a routed reply is never also fanned out | routing consulted before fanout; `fans_out` decided once | `a_query_reply_reaches_only_the_client_that_asked` |
| unsolicited upstream traffic still fans out | `RouteOutcome::Fanout` | `an_unsolicited_reply_still_fans_out_to_every_client` |
| every table and queue is bounded | `MAX_ROUTES`, `MAX_ROUTE_BATCHES`, `MAX_MULTIPART_REPLIES`, existing queues | `the_route_ceiling_is_refused_explicitly` |
| state applies exactly once per upstream line | `state.apply_line` before any routing decision | covered by the live tests above |

## Capability reconciliation (plan section 7)

The plan required auditing the split capability surfaces. They genuinely disagreed:

- `capability::DOWNSTREAM_FOUNDATIONAL` advertised `message-tags`, `server-time`,
  `batch`, `labeled-response`;
- the live `SessionReader` advertised only the two history drafts.

One module was claiming support the live reader did not provide — a `CAP LS` a client has
no way to challenge.

**Decision: withhold rather than advertise**, which the plan explicitly permits. Response
routing is now live for the bouncer's own upstream correlation, but *serving a client's
labels* additionally requires downstream message-tag mediation and a truthful
`CLIENTTAGDENY`. Neither exists. Advertising `labeled-response` now would promise tag
semantics this build cannot honour.

- `DOWNSTREAM_FOUNDATIONAL` is empty.
- `DOWNSTREAM_DEFERRED_FOUNDATIONAL` (`message-tags`, `batch`, `labeled-response`) and
  `DOWNSTREAM_DEFERRED_SERVER_TIME` name what is withheld, so withholding is a reviewable
  decision rather than an omission. Promoting them is M004-A work.
- `DownstreamCapabilities::advertisement()` and `downstream::downstream_supported()` give
  both CAP paths one authority, so an ACK can no longer contradict a `CAP LS`.

**Upstream is unaffected and unchanged**: the bouncer still requests the full label
surface, because it genuinely uses it for its own correlation. The two directions are
genuinely different and are documented as such.

### Behaviour change beyond the plan's letter, flagged deliberately

Three existing tests asserted the old downstream claim (`advertised == ["batch",
"labeled-response", "message-tags", "server-time"]`). They were rewritten to assert the
reconciled contract, not deleted: the advertisement is now checked to be a subset of what
the live reader serves, and each withheld capability is checked to be absent. This is
wire-visible and is the same class of change Corrective 013 made to the CAP mediator.

## Testing

390 tests pass across 19 test binaries; `scripts/verify.sh full` exits 0.

Nine new live end-to-end tests in `crates/runtime/tests/multi_network.rs` drive a real
attached client through a real scripted upstream, so routing is proven on the live path
rather than only against `ResponseRouter` in isolation. A `start_labeled` harness variant
negotiates the upstream label surface, because without it the bouncer correctly falls back
to one-outstanding-query-per-family correlation and cannot serve concurrent lookups.

| Plan section 9 requirement | Evidence |
|---|---|
| two clients issue concurrent WHOIS, each receives only its own | `two_clients_query_concurrently_and_each_gets_only_its_own_answer` |
| original downstream labels restored | same test, asserts `@label=same` returns to its owner |
| multi-line WHOIS routing | `a_query_reply_reaches_only_the_client_that_asked` |
| BATCH-associated routed response | `a_batched_multi_line_answer_stays_with_its_client_and_closes` |
| reply not duplicated through fanout | both tests assert the peer's stream lacks the reply |
| unsolicited upstream events still fan out | `an_unsolicited_reply_still_fans_out_to_every_client` |
| detach releases routes; a late reply reaches nobody | `a_reply_arriving_after_a_detach_reaches_nobody` |
| generation replacement delivers no reply from the old one | `a_generation_replacement_delivers_no_reply_from_the_old_one` |
| route table bounded, overflow refused deterministically | `the_route_ceiling_is_refused_explicitly` |
| competing unlabeled request refused locally | `without_labels_only_one_ambiguous_query_per_family_may_be_open` |
| every family numeric stays with the asking client | `every_numeric_of_an_open_family_stays_with_its_asking_client` |
| family numeric sets are disjoint | `the_family_numeric_sets_are_disjoint` |
| refused admission leaves no route | `a_refused_upstream_admission_leaves_no_route_behind` |
| capability advertisement matches actual live semantics | `the_advertisement_never_exceeds_what_the_live_reader_serves` |

## Dependency, MSRV and network boundary

- No new dependency was added. MSRV stays 1.88.
- `./scripts/check-network-boundary.py` exits 0; `./scripts/fuzz-smoke.sh` exits 0.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features
  -- -D warnings` are clean.
- `RoutingRequest` and `UpstreamAdmission` were introduced because clippy's
  `too_many_arguments` fired; they group the request identity and the admission
  authority rather than suppressing the lint. The second one also guarantees a routed
  query is admitted through exactly the same queue and generation fence as an unrouted
  one — there is deliberately no second way in.
- No environment-derived hostname, username, OS or router version, local path, process ID
  or machine identifier is inserted into any IRC-visible field. The generated upstream
  label encodes only a generation-local counter.

## Unresolved findings

### None

No finding is partially closed and no invariant was weakened to achieve closure.

One deliberate, documented limitation is recorded here so it is not "fixed" by mistake:

- **Only one unlabeled query per family may be outstanding at a time.** When upstream
  does not negotiate `labeled-response`, two concurrent WHOIS from two clients cannot be
  disambiguated, so the second is refused with a bounded local busy disposition rather
  than queued or guessed. This is the plan's specified fallback behaviour, not a defect.

### Future work this hands to M004-A

Promoting `labeled-response`, `message-tags`, `batch` and `server-time` downstream
requires the client-tag mediator and truthful `CLIENTTAGDENY` that Plan 015 owns. Until
then the constants above keep the withholding explicit.

## Corrective 014 acceptance

All acceptance criteria in sections 7 through 13 of the plan are met. The plan's
readiness question is answered: **M004 is unblocked for implementation.**

`plans/registry.md` moves Corrective 014 to closed, and Plans 015 through 018 become the
active sequence for Bouncer core M004.