# Bouncer Core M005-F / Plan 025 — Downstream IRCv3 Protocol Polish

Status: closed

Closure: plans/closure/bouncer-core/025-status.md

Blocker:

- Plan 024 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: capability + protocol correctness

## 1. Objective

Promote the low-risk IRCv3 semantics the bouncer can provide itself without expanding the durable member model.

Target:

- downstream server-time;
- downstream echo-message with real upstream confirmation;
- cap-notify;
- draft/no-implicit-names at the current reviewed revision;
- standard-replies if Plan 023 did not already promote it;
- exact CAP availability/advertisement cleanup.

## 2. Invariants

1. Downstream capabilities are advertised only when end-to-end semantics are live.
2. An attached client's CAP choices never alter the upstream requested capability set.
3. server-time never invents an upstream timestamp claim.
4. echo-message means server-confirmed echo, not successful local write.
5. Per-session CAP filtering cannot make one client corrupt another's view.
6. No-implicit-names affects only implicit projection; explicit NAMES remains available.
7. Draft capability spellings/revisions remain isolated.

## 3. server-time

Promote downstream server-time only after fanout, history replay and bouncer-originated frames have a coherent policy.

For an upstream event carrying a valid time tag, preserve it exactly.

For retained history without upstream server-time, follow the already documented bouncer-history synthesis policy only where the history adapter explicitly represents local receive time. Do not attach a fabricated upstream time tag to an ordinary live frame.

A client that did not negotiate the capability must not receive time tags it cannot interpret.

## 4. echo-message

The bouncer already requests echo-message upstream when offered but withholds it downstream.

Promote downstream echo-message only when:

- current upstream generation negotiated it;
- a downstream-originated PRIVMSG/NOTICE is not recorded/confirmed merely because it entered the upstream queue;
- the actual upstream echo is the confirmation event;
- that echo is fanned out once to every appropriate attached session, including the initiator according to negotiated semantics;
- history receives one canonical outgoing event, not a local-write copy plus an upstream-echo duplicate.

When upstream lacks echo-message, do not advertise the capability.

## 5. cap-notify

Implement truthful downstream CAP NEW/DEL behavior for capabilities whose availability may change during a session, particularly generation-dependent echo-message.

The set remains bounded.

Do not mirror arbitrary upstream CAP changes. Downstream availability is still the bouncer's semantic surface.

## 6. no-implicit-names

Implement the currently reviewed draft/no-implicit-names adapter.

A negotiating client suppresses implicit NAMES during projection/JOIN reconstruction but explicit NAMES remains a routed/synthesized supported command.

This reduces potentially large attachment bursts without changing retained member state.

## 7. standard replies

If not already advertised by Plan 023, implement the capability and migrate appropriate local bouncer failures to FAIL/WARN/NOTE while preserving required legacy numerics where compatibility needs them.

Do not replace every standard IRC numeric merely to use the extension.

## 8. Work packages

A. current spec revision review;
B. server-time downstream filtering;
C. confirmed echo-message/history integration;
D. cap-notify registry and dynamic availability;
E. no-implicit-names adapter/projection;
F. standard replies promotion;
G. capability consistency matrix;
H. docs/closure.

## 9. Tests

Include:

- CAP LS/REQ/LIST exact advertised set;
- client churn does not change upstream CAP request bytes;
- server-time negotiated/non-negotiated fanout;
- leap-second timestamp remains exact;
- invalid upstream time is not rewritten as valid;
- outgoing chat not confirmed on queue admission;
- upstream echo produces one downstream/history event;
- no echo-message advertisement when upstream unavailable;
- cap-notify NEW/DEL on generation capability change;
- no-implicit-names suppresses only implicit 353/366;
- explicit NAMES still works;
- batch/tag bounds and slow-client behavior remain unchanged.

## 10. Verification

Run current IRC conformance corpus, history/routing/integrated suites, full verification, privacy matrix and MSRV.

## 11. Documentation

Reconcile architecture/ircv3.md and architecture/downstream-session.md against exact production advertisement. No stale aspirational capability statements may remain at closure.

## 12. Acceptance criteria

The bouncer's low-risk downstream IRCv3 surface is truthful, useful and deterministic across clients and upstream generations, with no locally fabricated delivery confirmation or client-dependent upstream fingerprint.

## 13. Stop conditions

Stop if a target capability requires adding durable member/account/host state; defer that work to Plan 026 rather than partially implementing it here.

## 14. Closure evidence

Create plans/closure/bouncer-core/025-status.md with capability matrix, echo confirmation/history evidence, tag/time matrix and M005-G readiness.
