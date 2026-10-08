# Bouncer Core M007-C / Plan 041 — Eggchaos Multi-Client Adverse Qualification and M007 Closure

Status: active

Hard dependency:

- plans/closure/bouncer-core/040-status.md

Source milestone:

- M007 — Identity and Connectivity Resilience

Primary class: external qualification + milestone closure

## 1. Objective

Qualify M007 under realistic process/socket-boundary degradation while several downstream clients remain attached.

Use eggchaos as an external deterministic fault tool in addition to the repository's in-process testkit.

Eggchaos is not a production dependency.

## 2. Pinning and dependency boundary

Use a pinned eggchaos release/revision, preferably the released CLI surface if it exposes every required fault.

Initial preferred pin:

- eggchaos-cli 0.2.0

Record the exact binary version/hash used in closure.

Do not:

- add eggchaos to Cargo.toml;
- raise i2pr-irc MSRV because eggchaos currently requires Rust 1.89+;
- embed eggchaos into production code;
- make the ordinary unit/integration test suite require eggchaos installation.

The external qualification script detects availability and reports NOT RUN outside the explicit qualification target.

## 3. Qualification topologies

### Upstream/SAM path

~~~
i2pr-irc
   |
SamProvider TCP
   |
eggchaos stream proxy
   |
SAM bridge / deterministic fake bridge
~~~

This exercises:

- SAM control framing before raw transition;
- raw IRC stream after STREAM CONNECT;
- process-level socket scheduling/close behavior.

### Downstream client path

Where a standalone local TCP listener exists, place eggchaos there.

If no production listener exists yet, M007 closure uses the existing accepted-stream test boundary and records downstream external-proxy qualification as deferred to standalone-daemon work.

Do not create a production listener solely to satisfy this plan.

## 4. Required eggchaos faults

Exercise deterministic combinations of:

- latency + jitter;
- bandwidth cap;
- blackhole;
- slow close;
- slicing;
- disconnect;
- stream-loss.

Do not use UDP/datagram faults for IRC.

## 5. Scenario tranche

Create versioned scenario documents under a qualification/test directory.

Minimum scenarios:

### A. High-latency stable

- 20 s healthy;
- 90 s elevated latency/jitter;
- return healthy.

Assert no false reconnect if liveness budgets tolerate the configured delay.

### B. Blackhole then reconnect

- healthy registration;
- 30-60 s blackhole;
- disconnect;
- recovery.

Assert bounded generation replacement, no user-message replay, desired channel recovery.

### C. Fragmentation/bandwidth starvation

- severe slicing;
- low bandwidth;
- delayed writes.

Assert parser/framing correctness and control/PING progress.

### D. Repeated churn

At least 100 deterministic disconnect/recovery cycles under a fast qualification profile.

Assert no task/session/route/resource growth.

### E. Identity collision after reconnect

After transport loss, the fake/server profile makes preferred nick unavailable, then later free.

Assert fallback, service phases, corrected MONITOR/ISON evidence, reclaim cooldown, eventual preferred nick.

## 6. Simultaneous downstream clients

For selected scenarios keep at least:

- one modern active client;
- one legacy client;
- one passive/history client.

Inject:

- one client disconnect/reconnect while upstream is degraded;
- one slow/desynchronized client;
- concurrent routed query;
- preferred nick transition.

Assert:

- healthy clients remain synchronized;
- overloaded client alone detaches;
- response routes do not cross SessionId;
- all healthy clients converge on one current nick;
- upstream capability fingerprint remains client-independent.

## 7. No-SASL service workflow qualification

Run one profile with:

- no SASL capability;
- PreJoin IDENTIFY-like synthetic service action;
- fallback-recovery action;
- desired channel joins;
- preferred nick collision and later availability.

The synthetic service fixture records frames but does not claim real NickServ authentication.

Assert exact phase ordering and no duplicate FallbackRecovery within one generation.

## 8. Resource evidence

Capture baseline/peak/settled values for:

- Network tasks;
- session tasks;
- provider scopes;
- reconnect waiters/in-flight attempts;
- upstream queues;
- session queues;
- response routes;
- desired reconcile entries;
- history/store queue depth;
- reclaim/service-action state where observable.

After each scenario settles, owned resource counts return to expected baseline.

## 9. Privacy/security review

Fault tooling must not expose:

- SASL or service credentials;
- I2P Destinations;
- private SAM material;
- message payloads in committed qualification evidence.

Eggchaos itself can relay payload bytes by design, but i2pr-irc closure artifacts should record only counters/scenario outcomes, not traffic captures.

No non-loopback upstream path is added.

## 10. Deterministic in-process counterpart

Every product correctness claim must still have an in-process deterministic regression where practical.

Eggchaos evidence is additive for socket/process behavior, not the only test preventing regression.

If an eggchaos scenario exposes a production bug:

- fix it here if bounded and directly within M007 semantics;
- add an in-process regression;
- otherwise register a successor corrective before closing M007.

## 11. Verification

Run full current + Rust 1.88 repository verification after external campaigns.

Record eggchaos command/version and scenario fingerprints.

No extra Java/i2pd/i2pr SAM matrix is required by this plan.

## 12. Acceptance criteria

M007 closes only when:

- no-SASL service sequencing is durable and bounded;
- transient nickname occupation no longer permanently terminals the Network;
- preferred nick is eventually regained under bounded evidence/cooldown rules;
- simultaneous clients remain consistent through identity changes;
- long adverse socket campaigns do not leak resources or replay ambiguous traffic;
- no high-severity finding remains.

## 13. Documentation reconciliation

Update:

- architecture/presence-and-nick.md;
- reconnect/liveness docs;
- service-action docs;
- M006/M007 roadmap state;
- registry;
- README/operator docs where behavior is user-visible.

## 14. Closure evidence

Create plans/closure/bouncer-core/041-status.md with:

- eggchaos provenance/version;
- scenario definitions/fingerprints;
- server/client profile matrix;
- nick/service transition matrix;
- resource baseline/peak/settled table;
- privacy evidence;
- current + Rust 1.88 verification;
- unresolved findings;
- explicit M007 closure.
