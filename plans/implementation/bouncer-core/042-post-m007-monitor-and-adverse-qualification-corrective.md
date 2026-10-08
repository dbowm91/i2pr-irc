# Bouncer Core Corrective 042 — Post-M007 MONITOR and Adverse-Qualification Corrective

Status: ready for handoff

Repository baseline:

- 0852bf74fdcaabf008df8e23c087533bf948972c

Raised by post-M007 source/evidence review.

Corrects:

- crates/runtime/src/owner.rs preferred-nick reclaim evidence handling;
- crates/runtime/src/state.rs MONITOR ISUPPORT interpretation;
- plans/closure/bouncer-core/035-status.md interpretation where necessary;
- plans/closure/bouncer-core/041-status.md adverse-network evidence scope;
- plans/registry.md control-surface drift.

External protocol authority:

- IRCv3 MONITOR 3.2: https://ircv3.net/specs/core/monitor-3.2.html

Primary class: protocol correctness + qualification corrective

## 1. Objective

Close the remaining post-M007 correctness/evidence gaps without reopening the M006/M007 feature architecture.

Corrective 042 must:

1. stop treating absence from a 730 RPL_MONONLINE event as evidence that the preferred nick is free;
2. recognize standards-compliant bare MONITOR and every positive MONITOR=<limit> as usable for the bouncer's one-target preferred-nick watch;
3. add the missing negative MONITOR vectors that would have caught the defect;
4. extend Eggchaos process/socket evidence through the actual SamProvider path for disruptive faults rather than relying only on generic echo smoke plus in-process reconnect tests;
5. reconcile the registry/roadmap so no closed plan appears active and no stale handoff remains.

No new IRC feature is added.

## 2. Finding C042-F1 — 730 is an event, not a negative snapshot

Current note_reclaim_evidence groups 730 and 303 into the same "online set" logic:

- preferred listed => not free;
- preferred absent => free.

That inference is valid only for 303 RPL_ISON because 303 is the response to a query asking which requested nicks are online.

730 RPL_MONONLINE is a notification containing the monitored targets that became online. It says nothing about monitored targets absent from that specific event.

Example:

~~~
:srv 730 bot :alice!user@host
~~~

means alice is online. It does not prove bot is offline.

Current behavior can therefore set reclaim evidence and issue NICK bot after an unrelated MONITOR online event.

Severity: high for preferred-nick correctness on servers emitting multi-target MONITOR events.

## 3. Correct evidence semantics

Implement command-specific logic rather than one shared absence/presence rule.

### 731 RPL_MONOFFLINE

Positive reclaim evidence only when the listed offline target set contains the preferred nick under current casemapping.

Optional nick!user@host forms, if observed/accepted for compatibility, compare only the nick.

Absence of the preferred nick means no evidence.

### 730 RPL_MONONLINE

Never creates free/reclaim evidence.

If the listed targets contain the preferred nick, clear any stale immediate free evidence for that nick.

If the event is about another nick, leave reclaim evidence unchanged except that it must not be created by this frame.

Do not infer anything from absence.

### 303 RPL_ISON

303 remains snapshot/query evidence.

If the preferred nick is present in the returned online set: not free.

If the preferred nick is absent from the returned online set: free evidence.

This logic applies only to an outstanding/current generation reclaim probe; an unsolicited or stale-generation 303 cannot wake another generation.

## 4. Finding C042-F2 — MONITOR ISUPPORT parsing is too restrictive

Current NetworkState::monitor_limit only recognizes MONITOR=<integer>, and reclaim_strategy uses MONITOR only when the parsed value is within a local small ceiling.

IRCv3 MONITOR allows:

- MONITOR with no value, meaning no explicit target limit;
- MONITOR=<positive integer>, meaning the server permits that many targets;
- MONITOR=0, meaning disabled.

The bouncer watches exactly one preferred nick.

Required interpretation:

- bare MONITOR => usable/unlimited for this one target;
- MONITOR=1 or greater => usable;
- MONITOR=0 => disabled, use bounded ISON probing;
- malformed/non-numeric MONITOR value => do not assume support; use ISON fallback;
- missing MONITOR => use ISON fallback.

Do not reject a server advertising MONITOR=100 merely because the local implementation would never monitor 100 targets.

## 5. Suggested representation

Replace the ambiguous Option<usize> contract with an explicit bounded enum/value if that improves correctness, for example:

~~~
enum MonitorSupport {
    Absent,
    Disabled,
    Unlimited,
    Limited(usize),
}
~~~

or an equivalent representation.

The preferred-nick reclaim strategy only needs to answer whether at least one MONITOR slot is available.

Do not expand this into a downstream MONITOR broker or extended-monitor implementation.

## 6. Regression matrix

Add tests that specifically distinguish event semantics from snapshot semantics.

Required cases:

- 730 preferred nick listed => no NICK reclaim;
- 730 unrelated nick listed => no NICK reclaim;
- 730 several unrelated nicks => no reclaim;
- 730 preferred nick with !user@host => no reclaim and stale free evidence cleared;
- 731 preferred nick listed => reclaim evidence;
- 731 unrelated nick only => no reclaim;
- 731 comma-separated list containing preferred => reclaim;
- 303 preferred present => no reclaim;
- 303 preferred absent => reclaim;
- casemapping variants for each relevant command;
- stale generation 730/731/303 cannot affect replacement generation;
- reclaim refusal/cooldown from Plan 040 remains green.

ISUPPORT:

- bare MONITOR => Monitor strategy;
- MONITOR=1 => Monitor;
- MONITOR=4 => Monitor;
- MONITOR=100 => Monitor;
- MONITOR=0 => Probe;
- malformed MONITOR=abc => Probe;
- absent MONITOR => Probe.

## 7. Finding C042-F3 — external adverse evidence is narrower than the M007 product claim

Plan 041's strongest external product-path scenario runs:

~~~
SamProvider -> Eggchaos -> deterministic fake SAM bridge
~~~

under latency/jitter, bandwidth restriction, and slicing.

Blackhole, disconnect, and stream-loss are externally verified by fault-smoke.py against a generic echo server, while actual i2pr-irc reconnect/recovery semantics for those faults are covered in-process.

That is useful layered evidence, but it is weaker than process/socket-boundary recovery through the production provider.

This corrective should strengthen the claim rather than merely reword it if the existing Eggchaos surface can support a bounded deterministic run.

## 8. Product-path Eggchaos qualification

Extend the explicit ignored qualification test/script so the actual production SamProvider path experiences at least:

### A. Blackhole followed by forced recovery

1. establish Network through SamProvider -> Eggchaos -> fake SAM bridge;
2. reach IRC online state;
3. activate upstream blackhole for a bounded interval long enough to exercise liveness;
4. force/observe the connection ending under a deterministic ceiling if blackhole alone does not produce an EOF;
5. restore healthy transport;
6. verify a new generation registers;
7. desired channel state reconciles;
8. no ambiguous user chat is replayed;
9. provider scope/session lifecycle remains bounded.

### B. Hard disconnect

1. establish online generation through the same product path;
2. apply Eggchaos disconnect to the active path;
3. observe generation loss;
4. restore connectivity;
5. verify bounded reconnect and registration;
6. verify same durable Network identity/policy and no task/route residue.

### C. Stream-loss disposition

If Eggchaos stream-loss can be made deterministic at the byte-stream proxy layer:

- exercise it through the product path;
- assert bounded timeout/disconnect/recovery and no silent claim of message delivery.

If stream-loss cannot produce a stable product assertion because it intentionally drops arbitrary application bytes without TCP semantics, document it as a fault-tool smoke capability only and do not overclaim it as equivalent to real packet loss.

Do not weaken correctness to make the scenario pass.

## 9. Eggchaos boundaries

Eggchaos remains:

- external test infrastructure;
- pinned by immutable source revision/version;
- absent from Cargo.toml;
- non-required for ordinary unit/workspace verification;
- loopback-only in this repository;
- independent of the Rust 1.88 MSRV.

The deterministic in-process harness remains mandatory regression coverage.

A missing Eggchaos executable reports NOT RUN for the explicit external qualification and cannot be recorded as PASS.

## 10. Resource and multi-client assertions

For disruptive product-path scenarios record/assert:

- generation count/replacement;
- reconnect scheduler waiters/in-flight attempts;
- provider live/healthy scopes;
- session creations where relevant;
- attached sessions before/after recovery;
- response-route count;
- upstream/session queue depths;
- desired channel reconcile state.

Where the accepted-stream multi-client boundary is practical in the same test, keep at least two sessions attached and prove healthy sessions converge after upstream recovery.

Do not create a standalone listener solely for this corrective.

## 11. Planning reconciliation

The current registry contains closed Plans 035-041 under "Active and dependency-ready implementation plans" and its latest-handoff section still says "Implement only Plan 037" despite M006/M007 closure.

Corrective 042 registration must:

- make Corrective 042 the sole active/ready handoff;
- move/remove closed 035-041 from the active table;
- retain them under recently closed/history;
- update the bouncer-core roadmap current state to say M006/M007 are closed but Corrective 042 is the strict current post-M007 readiness authority;
- preserve historical closure records rather than rewriting them;
- on Corrective 042 closure, return the active table to empty unless another plan is explicitly opened.

## 12. Scope

In scope:

- MONITOR 730/731/303 reclaim evidence;
- MONITOR ISUPPORT support detection;
- missing conformance tests;
- product-path Eggchaos blackhole/disconnect qualification;
- accurate stream-loss evidence disposition;
- planning/registry cleanup;
- documentation updates.

Out of scope:

- extended-monitor;
- downstream MONITOR commands;
- chghost;
- new IRCv3 features;
- OTR/E2EE;
- encrypted history/database;
- standalone listener/daemon;
- router/SAM portability matrices;
- R002 managed-app integration.

## 13. Verification

Minimum repository verification:

~~~
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Explicit external qualification:

~~~
EGGCHAOS_BIN=/path/to/pinned/eggchaos scripts/qualify-m007-eggchaos.py
~~~

Update the script/scenario document so its output distinguishes:

- generic Eggchaos fault smoke;
- product-path latency/bandwidth/slicing;
- product-path blackhole recovery;
- product-path disconnect recovery;
- stream-loss disposition.

## 14. Acceptance criteria

Corrective 042 closes only when:

1. an unrelated 730 cannot create reclaim evidence;
2. 730 never uses absence semantics;
3. 731 creates evidence only when the preferred nick is explicitly listed;
4. 303 remains the only absence-as-free snapshot path;
5. bare MONITOR and every positive MONITOR=<n> are usable for the one-target watch;
6. MONITOR=0/malformed/absent safely fall back to ISON;
7. product-path Eggchaos blackhole/disconnect recovery is qualified or a concrete tool limitation is recorded without overclaiming;
8. no ambiguous user message is replayed during disruptive recovery;
9. current and Rust 1.88 full verification pass;
10. registry/roadmap contain no stale active/ready closed plans.

## 15. Stop conditions

Stop and register a successor corrective/ADR if:

- correct MONITOR behavior requires a downstream monitor broker;
- product-path blackhole/disconnect exposes an unrelated production defect too large for this corrective;
- Eggchaos must become a production or Cargo dependency;
- a fix changes durable identity semantics established in M007;
- recovery would require replaying ambiguous user traffic.

## 16. Closure evidence

Create:

- plans/closure/bouncer-core/042-status.md

Record:

- C042-F1/F2/F3 dispositions;
- command-specific 730/731/303 matrix;
- ISUPPORT MONITOR matrix;
- changed source files;
- Eggchaos version/source commit/binary hash;
- product-path disruptive scenario outcomes;
- generic-smoke versus product-path evidence distinction;
- resource baseline/peak/settled values;
- current + Rust 1.88 verification;
- registry/roadmap reconciliation;
- explicit statement that M006/M007 historical closures remain retained but Corrective 042 supersedes their affected readiness claims.
