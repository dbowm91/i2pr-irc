# Bouncer Core M005-H / Plan 027 — Operator Diagnostics, Configuration Snapshots, and Constrained Registration Actions

Status: blocked

Blocker:

- Plan 026 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: polish + capability

## 1. Objective

Finish the mature local-operator ergonomics needed before M005 qualification:

- stable bounded diagnostics;
- deterministic retry/backoff visibility;
- versioned configuration snapshot export/import;
- constrained opt-in post-registration actions replacing a generic perform/raw-quote facility.

## 2. Diagnostics

Build on NetworkSnapshot, CatalogStatus and ResourceLedger rather than introducing another metrics owner.

Expose non-secret fields needed to operate the bouncer:

- NetworkId/display name;
- phase/generation;
- current and preferred nick;
- automatic/manual away state class;
- detached/visible desired-channel counts;
- attached active/passive session counts;
- reconnect attempt and next retry deadline/delay where meaningful;
- terminal registration/reconcile state;
- queue depths and rejection counters;
- history recorded/skipped/dropped;
- response-route/batch counts;
- store health;
- reconnect-budget wait/in-flight counts;
- resource current/peak gauges;
- controller revision.

Never expose raw endpoint/Destination, SASL secret, registration-action payload or local filesystem path in generic diagnostics.

Revisit Backoff injection into NetworkOwner only if needed to make next-retry diagnostics exact and deterministically testable. Corrective 019 recorded the evidence for this seam.

## 3. Configuration snapshot

Define a versioned typed local configuration format independent of IRC draft syntax.

Initial scope:

- Networks and stable NetworkId;
- display name;
- typed I2P endpoint;
- non-secret IRC identity;
- Network policies;
- desired channel policies;
- constrained registration-action metadata.

Secrets are excluded by default. If an explicit secret-bearing export mode is ever added, it needs separate review and secure destination semantics; it is not required to close this plan.

Import:

- parses and validates the entire snapshot before mutation;
- is bounded by the same production ceilings;
- refuses schema/version it cannot understand;
- uses RuntimeController typed mutations;
- cannot leave a partially applied live/durable graph after a failed validation;
- preserves stable IDs only when restoring to an empty/compatible local store; conflicting identities fail explicitly rather than being remapped silently.

If fully transactional multi-Network import cannot be implemented over the current one-worker API without unsafe partial live state, limit the first surface to validate/plan plus explicit per-Network application and document that boundary instead of pretending atomicity.

## 4. Constrained registration actions

Do not implement arbitrary network quote.

Provide a bounded list of opt-in commands intentionally replayed after every successful upstream registration generation.

The action parser uses the normal IRC Message parser and privacy/tag/CTCP mediation.

Initial allowlist should be minimal and reviewed, for example:

- MODE targeting the current self nick;
- PRIVMSG/NOTICE only to explicitly configured service targets when the Operator knowingly stores a restart action.

Forbidden include at least:

- NICK;
- JOIN/PART;
- QUIT;
- CAP;
- AUTHENTICATE;
- BOUNCER;
- raw prefixed messages;
- DCC;
- commands with client tags not allowed by policy.

Cap action count and bytes. Treat stored action payload as secret/redacted because it may contain service credentials.

Actions are intentional per-generation setup, not ambiguous-user-message retry. Their repeated-on-reconnect semantics must be documented.

## 5. Failure and restart semantics

- diagnostics read live authoritative counters without blocking network liveness;
- a failed config import does not silently claim success;
- unknown durable mutation outcomes use controller re-read semantics;
- registration-action enqueue failure is observable and bounded;
- generation loss cancels unfinished action emission; next successful generation starts the configured sequence from its defined beginning;
- action payload never appears in diagnostics or error strings.

## 6. Work packages

A. diagnostic DTO/projection;
B. exact retry schedule observability/testability;
C. config snapshot schema/export;
D. bounded import validation/application;
E. secret/redaction classification;
F. constrained registration-action model/store migration;
G. post-registration action runner through existing mediation;
H. operator service/control integration;
I. docs/closure.

## 7. Tests

Include:

- diagnostics exactness under connect/backoff/terminal/reconcile states;
- no endpoint/secret/action payload in diagnostics;
- next retry deterministic under virtual time if exposed;
- config export/import round trip without secrets;
- invalid version/count/endpoint/policy rejects before mutation;
- stable ID collision behavior;
- import failure does not leave ghost owner;
- registration-action count/byte bounds;
- allowlist/denylist command matrix;
- DCC/CTCP/tag privacy cannot be bypassed through stored action;
- reconnect intentionally repeats action sequence;
- failure halfway does not replay arbitrary downstream traffic;
- secret-bearing action redaction in Debug/errors/closure fixtures.

## 8. Verification

Run full verification, configuration/migration tests, privacy/redaction campaign, reconnect virtual-time tests and MSRV.

## 9. Documentation

Document diagnostics stability expectations, config snapshot versioning, secret omission, import failure semantics and registration-action replay semantics.

## 10. Acceptance criteria

An Operator can inspect the bouncer's bounded state, move non-secret configuration through a versioned local format, and configure a small audited post-registration action set without gaining an arbitrary raw-command execution path or leaking credentials.

## 11. Stop conditions

Stop if desired ergonomics require shell execution, arbitrary network quote, exporting secrets by default, generic file/network side effects, or an unbounded diagnostic/event stream.

## 12. Closure evidence

Create plans/closure/bouncer-core/027-status.md with diagnostic/redaction matrix, config round-trip/failure matrix, registration-action security matrix and M005-I readiness.
