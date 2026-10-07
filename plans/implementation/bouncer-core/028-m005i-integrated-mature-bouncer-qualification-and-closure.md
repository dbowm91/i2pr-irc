# Bouncer Core M005-I / Plan 028 — Integrated Mature-Bouncer Qualification and M005 Closure

Status: closed

Closure: plans/closure/bouncer-core/028-status.md

Blocker:

- None. Plan 027 closure accepted; see plans/closure/bouncer-core/027-status.md.
- Cleared during execution: see the "Carried in from the Plan 027 closure" note below.

Carried in from the Plan 027 closure, to resolve rather than merely re-measure:

- A generation teardown took about 120 s to be noticed. Plan 027 recorded this as a
  pre-existing finding after measuring 120.9 s with no registration actions configured at
  all, against a CONNECT_TIMEOUT of 120 s. **Resolved during this plan's execution:** the
  measurement was sound and the conclusion was not. The bouncer ends a generation on
  end-of-stream immediately; the 120 s was `LIVENESS_DEADLINE` firing in a test whose
  `drop_generation` had silently done nothing. See "Post-closure annotation" in
  plans/closure/bouncer-core/027-status.md and this record's resolution section.

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: invariant qualification + milestone closure

## 1. Objective

Qualify M005 as one integrated mature-bouncer product and close the bouncer-core roadmap before portable router integration begins.

This plan adds only testability/diagnostic corrections needed to prove the integrated behavior. New convenience features discovered during qualification are deferred.

## 2. Entry criteria

Plans 020-027 must each have accepted closure records with no unresolved M005-blocking finding.

Historical M003/M004 evidence remains valid but is not substituted for M005 integration tests where new control/session/storage paths cross those invariants.

## 3. Integrated qualification matrix

### Process ownership and control

Prove:

- one RuntimeController;
- one live NetworkOwner per durable Network;
- no bound SessionTask owned by two actors;
- unbound session has no upstream authority;
- transfer preserves SessionId/ClientId/decoder bytes;
- dynamic add/change/delete leaves durable/live graph converged;
- shutdown owns/joins every controller/admission/network/session task.

### Detached channels

Across restart, reconnect and multiple clients prove:

- upstream membership retained;
- downstream hidden state consistent;
- history retained;
- reattach projection truthful;
- legacy and explicit-history clients do not duplicate backlog.

### Presence and nick policy

Prove:

- active/passive/manual-away aggregation;
- deterministic reconnect behavior;
- bounded 433 fallback;
- MONITOR/fallback reclaim;
- no environment-derived identity.

### Bouncer control

Prove:

- bouncer-networks BIND/list/mutation/notify transcripts;
- unbound and bound control sessions;
- I2P-only address validation;
- BouncerServ typed operations;
- no raw network escape hatch.

### History

Prove:

- search/index migration and retention consistency;
- bounded text/selectors/results;
- CHATHISTORY current advertised surface;
- search under store pressure cannot starve liveness.

### IRCv3

Prove the final downstream CAP matrix for:

- legacy client;
- modern full-cap client;
- several clients with disjoint CAP sets;
- upstream server with minimal offers;
- upstream server with full reviewed offers;
- client churn and reconnect.

Upstream request fingerprint for a fixed offer must remain client-independent.

### Operator ergonomics

Prove:

- diagnostic correctness and redaction;
- config snapshot round trip/failure;
- constrained registration actions and replay semantics;
- no secret payload in generic outputs.

## 4. Adverse integrated campaigns

Run deterministic campaigns combining features, not only isolated unit tests:

- many Networks reconnect while control clients list/subscribe;
- one Network configuration change during router-like path loss;
- store stall while history search and channel-policy mutations occur;
- many detached channels receiving history while clients attach/detach;
- passive/active client churn during reconnect;
- slow control client while network notifications change;
- retained-history pressure plus search plus read-marker updates;
- shutdown while admissions are unbound and others are bound;
- restart with every M005 durable policy populated.

Use ResourceLedger/control gauges to assert steady-state recovery.

## 5. Security/anonymity requalification

Re-run and extend M004 negative evidence:

- static no generic DNS/TCP;
- no DCC/direct path;
- CTCP metadata fingerprint fixed;
- client tag policy fixed;
- poisoned USER/LOGNAME/HOSTNAME/HOME/TMPDIR/PATH sentinels absent;
- no SASL/config/action secret in upstream-inappropriate or downstream diagnostic output;
- bouncer-networks clearnet host/port/tls attempts fail before any network authority;
- BouncerServ cannot execute arbitrary raw IRC or host commands.

## 6. Migration/restart matrix

Test supported predecessor chain through every M005 schema version.

At minimum prove:

- M004 schema opens and migrates through current;
- failure during each migration rolls back cleanly;
- restart reconstructs durable Network/channel/policy/config state;
- no live session, generation, route, presence observation or current nick is persisted accidentally;
- search side indexes rebuild/validate consistently.

If maintaining every intermediate development schema is unnecessary because no release shipped it, record that decision explicitly and test every released/predecessor schema that is actually supported.

## 7. Compatibility

Run ordinary legacy one-Network-per-downstream scenarios with no M005 capability negotiation.

A mature feature set is not closed if ordinary pre-M005 clients regress in registration, JOIN/PART, chat, WHOIS/NAMES/LIST routing or bounded automatic backlog.

## 8. Performance/resource evidence

Measure enough to detect structural regressions:

- task/queue counts for idle many-Network process;
- attach/control snapshot cost under 64 Networks;
- search bounded-work behavior;
- detached high-traffic history ingestion;
- notification fanout to several clients;
- settled ResourceLedger readings after campaigns.

Do not create arbitrary benchmark gates without a measured baseline.

## 9. Work packages

A. requirement-to-evidence inventory for Plans 020-027;
B. integrated ownership/control campaign;
C. detached/presence/nick campaigns;
D. bouncer-control interoperability/security campaign;
E. history/search/IRCv3 campaign;
F. config/diagnostic/action redaction campaign;
G. migration/restart matrix;
H. resource/performance baseline;
I. documentation reconciliation;
J. closure decision.

## 10. Verification

Run and record:

- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets --all-features -- -D warnings
- cargo test --workspace --all-features
- scripts/check-network-boundary.py
- scripts/verify.sh full
- rustup run 1.88.0 sh scripts/verify.sh full

Run any fuzz/property targets and protocol conformance runners owned by the final feature set.

## 11. Closure blockers

Do not close M005 with:

- a second reachable upstream owner;
- unbounded process/session/control/history-search queue;
- clearnet-compatible bouncer-network endpoint path;
- client-dependent upstream CAP fingerprint;
- secret/environment leakage;
- false downstream CAP advertisement;
- history/search index inconsistency;
- ghost owner after durable deletion;
- session bytes lost/duplicated at bind transfer;
- unbounded nick reclaim;
- arbitrary raw command execution;
- a supported predecessor migration failure;
- unresolved high-severity finding.

## 12. Documentation

Reconcile README, architecture overview, bouncer-core roadmap, registry, capability docs, storage schema docs and control-session docs to the final implementation.

Historical closure records are not rewritten except explicit post-closure annotations needed to prevent a present-tense false claim.

## 13. Acceptance criteria

M005 closes only when i2pr-irc core is a durable, multi-Network, multi-client, mature local IRC bouncer with bounded modern control/history/presence/channel features and deterministic adverse-network behavior, still independent of a concrete router and structurally unable to create generic upstream clearnet traffic.

## 14. Router disposition

If M005 closes with no router-blocking finding, update the router-integration roadmap/registry so R001 portable SAM planning or implementation becomes eligible under its own prerequisites.

M005 closure does not authorize R002 managed-i2pr integration or R003 Proposal 170 control work beyond their existing interface/product gates.

## 15. Closure evidence

Create plans/closure/bouncer-core/028-status.md containing:

- implementation commit ranges for Plans 020-027;
- requirement/evidence matrix;
- final CAP matrix;
- control-session ownership matrix;
- bouncer-networks I2P profile matrix;
- migration matrix;
- anonymity/redaction matrix;
- resource/performance observations;
- commands executed and exact results;
- unresolved findings and severity;
- M005 closure decision;
- Router R001 readiness disposition.
