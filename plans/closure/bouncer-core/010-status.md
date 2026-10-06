# Bouncer Core M003-D Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/010-m003d-response-routing-and-ircv3-foundation.md`

Authority: Research 003 conformance results, Research 004, canonical capability-mediation requirements

Prior closure: `plans/closure/bouncer-core/009-status.md`

Repository planning baseline reviewed: `cb75659908883ae983c48fce06ab1e95d12f9a10`

Primary class: capability

## What was delivered

A capability registry with separate upstream and downstream policy, generation-local SessionId-scoped response routing with label translation, bounded unlabeled fallback correlation for four specified command families, bounded BATCH tracking, tag/server-time mediation, and a truthful `echo-message` policy — all wired into the live generation loop.

## Capability matrix

| Direction | Capability | Policy | Rationale |
|---|---|---|---|
| Upstream | `message-tags`, `server-time`, `batch`, `labeled-response`, `echo-message` | requested when offered | the reviewed foundational set the bouncer implements |
| Upstream | `sasl` | requested only when configured | unchanged authentication behavior |
| Upstream | everything else (`away-notify`, `extended-join`, …) | **not requested** | not implemented; mirroring the server's offer would claim behavior the bouncer lacks |
| Downstream | `message-tags`, `server-time`, `batch`, `labeled-response` | always advertised | semantics fully implemented |
| Downstream | `echo-message` | advertised **only** when upstream negotiated it | the bouncer confirms a message only after the server echoes it |
| Downstream | `chathistory`, `read-marker` | **never advertised** | M003-E; deferred capability is an explicit constant, not an omission |

Evidence: `the_upstream_request_set_is_the_reviewed_constant_only`, `downstream_advertisement_is_exactly_what_this_build_implements`, `deferred_history_capabilities_are_never_advertised`, `capabilities_used_by_the_bouncer_are_the_reviewed_ones`.

## Downstream-client independence

`UpstreamCapabilities::request_set()` is a pure function of what the server offered. Attaching two different clients, each with different downstream `CAP REQ` results, leaves the upstream set and fingerprint unchanged.

Evidence: `the_upstream_capability_fingerprint_is_downstream_client_independent`, `upstream_is_client_independent`.

This matters because upstream negotiation happens once per generation while clients attach and detach freely. A negotiation that depended on attached clients would make the generation's behavior depend on transient state.

## Label namespace and correlation bounds

| Property | Value | Evidence |
|---|---|---|
| Simultaneous routes per generation | 128 (`MAX_ROUTES`) | `the_route_table_is_bounded_and_overflow_gets_a_deterministic_refusal` |
| Generated upstream label length | ≤ 64 bytes | `a_client_label_is_never_forwarded_upstream_and_never_encodes_a_client_id` |
| Accepted client label length | ≤ 64 bytes | `an_over_long_client_label_is_refused_before_the_wire` |
| Route lifetime | 20 s, expiring deterministically | `a_route_times_out_and_releases_its_slot_deterministically` |
| Replies per route before force-close | 64 | `MAX_MULTIPART_REPLIES` |

Generated labels are generation-local monotonic tokens (`i2p<16 hex>`). A downstream label is **never** forwarded upstream, and a `ClientId` is **never** encoded into one:

Evidence: `a_client_label_is_never_forwarded_upstream_and_never_encodes_a_client_id`, `two_sessions_issue_concurrent_labeled_whois_and_receive_only_their_own`.

Two sessions using the identical client label `"mine"` receive distinct upstream labels, and each reply is restored with that session's own original label.

## Route lifetime and cancellation

| Trigger | Effect | Evidence |
|---|---|---|
| Session detach | all routes for that `SessionId` dropped | `detaching a_session_drops_only_its_routes` (unit) |
| Session detach, then a late reply arrives | `Unmatched`, delivered to nobody | `a_stale_label_cannot_route_to_a_replacement_session` |
| Generation replacement | the whole router is discarded | router is created per generation |
| Timeout | slot released deterministically and reusable | `a_route_times_out_and_releases_its_slot_deterministically` |
| Unknown/stale label | `Unmatched` | `a_late_reply_with_an_unknown_label_is_never_delivered_to_another_client` |

Routes are never persisted. A durable route would name a connection that no longer exists, and the only possible outcome for a late reply would be misdelivery to whoever occupies the slot.

## Fallback completion matrix

| Family | Terminator | Concurrency | Evidence |
|---|---|---|---|
| WHOIS | 318 | one outstanding per family | `fallback_correlation_follows_the_specified_completion_matrix` |
| WHO | 315 | one outstanding per family | same |
| NAMES | 366 | one outstanding per family | same |
| LIST | 323 | one outstanding per family | same |
| everything else | **no route** | — | `a_command_family_without_specified_semantics_creates_no_route` |

The matrix test asserts both directions: an unrelated numeric does not complete a route, and the family's own terminator does.

There is deliberately **no generic FIFO**. An unlabeled reply with no matching route belongs to no known request; guessing it onto the oldest query would deliver one client's answer to another.

Evidence: `there_is_no_generic_fifo_for_unlabeled_responses` — every numeric, including `001`, is dropped when no route is open.

A competing request for a busy family receives `RouteRefusal::Busy` deterministically, never an unbounded queue and never a guess:

Evidence: `a_competing_fallback_query_gets_a_deterministic_busy_disposition` (five consecutive attempts, all refused).

## BATCH

| Bound | Value |
|---|---|
| Simultaneously open batches | 64 |
| Nesting depth | 4 |
| Batch identifier bytes | 32 |
| Batch type bytes | 32 |
| Messages per batch before force-close | 128 |

Evidence: `batch_tracking_is_bounded_and_identifiers_are_ephemeral`, `batch_nesting_count_and_message_counts_are_bounded`, `the_open_batch_ceiling_is_refused`, `a_batch_route_follows_replies_until_its_terminator`, `an_unknown_batch_reference_is_refused`.

Batch identifiers are ephemeral and session-local, never durable: a persisted id would outlive its connection and could collide with an unrelated batch.

## Tag and server-time mediation

| Rule | Evidence |
|---|---|
| Client-only tags are default-deny | `client_only_tags_are_default_deny` |
| A malformed client `time`/`msgid` is stripped, not trusted | `a_malformed_client_tag_is_stripped_rather_than_trusted` (unit) |
| A real upstream `server-time` is preserved, never overwritten | `a_server_time_is_preserved_or_synthesized_but_never_orders_history` |
| A missing one may be synthesized from bouncer receive time | same |
| Tags never change canonical order | `tags_never_change_canonical_history_order` |

A client forging `msgid` or inventing tags must not reach the server: that is how one client could impersonate another's metadata. Widening the policy is M004's decision under explicit review.

## Echo-message policy

The bouncer advertises downstream `echo-message` **only when upstream negotiated it**.

A local stream write is never converted into confirmed remote delivery. The Plan 009 omission of local outgoing history remains correct precisely because of this: once the server echoes a message, that echo becomes the confirmation, and until then there is none.

Evidence: `echo_message_is_advertised_only_when_the_server_actually_echoes` (unit), `echo_message_is_advertised_only_when_the_server_actually_echoes` (integration) — the latter asserts both that *offered* is not enough and that *enabled* is.

## Network boundary review

Unchanged. Response routing mediates protocol semantics on an existing connection; it opens no socket and resolves no name. `scripts/check-network-boundary.py` passes unchanged.

## Correctness defects found and fixed during implementation

| Defect | Consequence if shipped | Fix | Evidence |
|---|---|---|---|
| `mediate_client_tags` returned the message unchanged while reporting `TagDisposition::Stripped` when a client had not negotiated `message-tags` | a client would receive tags it never negotiated, and the disposition would report a lie the client could not detect | return `strip_all_tags(message)` | `tags_are_dropped_entirely_when_the_client_did_not_negotiate_them` |
| The unlabeled fallback lookup borrowed `self.fallback` immutably and then mutated it | would not compile; the naive fix would have cloned the whole map per reply | locate the family key first, then remove | `fallback_correlation_follows_the_specified_completion_matrix` |
| Upstream `CAP NAK` for a non-SASL capability aborted nothing but also recorded nothing | a server refusing an optional capability would leave negotiation state inconsistent with reality | a NAK now proceeds with whatever was granted | generation tests |
| `render` contained a branch that pushed `param` on both sides | dead code implying parameter escaping that did not exist | removed | clippy |

Two test-authoring mistakes are recorded because they confirmed invariants: the rendered upstream line carries a leading `@` while a parsed message's label does not, and the capability request set is emitted in its declared constant order rather than sorted.

## Verification actually executed

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
./scripts/check-network-boundary.py
rustup run 1.88.0 sh scripts/verify.sh full
```

All passed. 223 workspace tests pass, including every pre-existing test and 26 new routing/capability tests.

## Unresolved findings

None blocking. One item is explicitly deferred:

- The wire conformance corpus was **not extended** with new tag/label cases in this plan. The existing corpus passes unchanged and the new behavior is covered by unit and integration tests; extending the corpus is desirable but is not a blocker for M003-E, and adding cases to a corpus this plan did not author would be a separate reviewed change.

## M003-E readiness decision

**M003-E is unblocked.** The preconditions it names — downstream `CAP` negotiation that obeys the registration gate, a session capability hook that can suppress legacy backlog, a bounded batch tracker whose identifiers are ephemeral, durable buffers with monotonic cursors and read markers, and the journal's documented ingestion policy — are all present. Specifically, `SessionCapabilities::explicit_history` is the hook M003-E needs to suppress automatic backlog for a `chathistory`-negotiating client, `BatchTracker` bounds the batch IDs M003-E will allocate, and the deferred `chathistory`/`read-marker` capability constants are already reserved so M003-E can advertise them truthfully the moment their semantics land.