# Bouncer Core M005-G / Plan 026 — Richer IRCv3 Member-State Mediation

Status: blocked

Blocker:

- Plan 025 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: capability

## 1. Objective

Extend the reviewed upstream/downstream IRCv3 state surface where doing so requires richer observed user/member metadata.

Candidate target set:

- account-tag;
- account-notify;
- away-notify;
- extended-join;
- chghost;
- invite-notify;
- multi-prefix;
- setname;
- extended-monitor where it is needed by established M005 behavior.

At implementation start re-read current specifications and trim the set rather than advertising partial semantics.

## 2. Invariants

1. Upstream request set remains fixed by build/policy and server offer, never attached-client demand.
2. Observed account/away/userhost/realname state is generation-local unless there is a distinct durable product requirement.
3. Unknown or incomplete state is omitted rather than fabricated.
4. Per-session downstream CAP mediation may reduce what a client sees but cannot alter shared upstream state.
5. Privacy mediation from M004 remains before client exposure and upstream transmission.
6. Every expanded member field has explicit byte/cardinality bounds.

## 3. State-model changes

Extend bounded observed member/network state only for fields required by accepted capabilities.

Potential fields include:

- account;
- away flag;
- username/host changes;
- realname where extended-join supplies it;
- complete multi-prefix membership modes.

Define invalidation rules on nick change, quit, reconnect, incomplete NAMES/WHO state and capability absence.

Do not persist these observations to SQLite.

## 4. Upstream capability policy

Only add a capability to UPSTREAM_FOUNDATIONAL or its reviewed successor when the runtime handles every message form that capability enables.

A capability offered but not supported stays unrequested.

The exact upstream request fingerprint for a fixed server offer must remain identical across:

- no client;
- legacy client;
- clients negotiating disjoint subsets;
- repeated attach/detach churn.

## 5. Downstream projection

Advertise each accepted capability only when the bouncer can mediate it.

Projection/fanout must degrade per client:

- extended-join client may receive extended JOIN fields;
- legacy client receives compatible JOIN;
- multi-prefix client may receive complete prefix information;
- clients without account/away/chghost capabilities do not receive unsupported event forms.

Do not detect client brands.

## 6. Work packages

A. current spec review and accepted subset;
B. bounded observed member-state extensions;
C. upstream CAP request additions;
D. event application/invalidation;
E. per-session downstream mediation;
F. projection and reconnect reconstruction;
G. fingerprint/privacy qualification;
H. docs/closure.

## 7. Tests

For each accepted capability include:

- offer/request/ACK behavior;
- max/max+1 field bounds;
- event application and invalidation;
- legacy-client downgraded view;
- capable-client richer view;
- reconnect clears stale observations;
- several simultaneous clients with different CAP sets;
- fixed upstream fingerprint matrix;
- environment/private-data negative matrix;
- queue/resource bounds under high membership cardinality.

## 8. Deferred from this plan

Unless new research establishes a concrete requirement, keep these out:

- arbitrary metadata-2;
- message reactions;
- message redaction;
- multiline/chathistory event-model expansion;
- client-to-client transport features;
- vendor extensions that require storing new message event classes.

## 9. Verification

Run conformance corpus, state/projection/routing/privacy tests, full workspace verification and MSRV.

## 10. Documentation

Update capability policy, observed state, projection and anonymity docs with exact accepted subset and reasons for deferrals.

## 11. Acceptance criteria

The accepted richer IRCv3 features behave as mediated bouncer semantics across reconnect and heterogeneous clients without making client choice change upstream fingerprint or turning incomplete observation into false state.

## 12. Stop conditions

Stop if a candidate capability requires durable live identity state, generic network authority, client-brand detection or a history schema expansion not planned here. Defer rather than partially advertise.

## 13. Closure evidence

Create plans/closure/bouncer-core/026-status.md with per-capability conformance/projection matrices, fingerprint evidence, bounds and M005-H readiness.
