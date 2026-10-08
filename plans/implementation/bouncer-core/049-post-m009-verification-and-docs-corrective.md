# Bouncer Core Corrective 049 — Post-M009 Verification and Documentation Reconciliation

Status: ready for handoff

Repository baseline:

- 2081e9be6792109f2a042f1643576de8f69cd723

Raised by:

- plans/closure/bouncer-core/048-status.md
- post-M009 planning/source review

Historical authority:

- plans/closure/bouncer-core/043-status.md
- plans/closure/bouncer-core/046-status.md
- plans/closure/bouncer-core/048-status.md
- plans/closure/router-integration/033-status.md

Primary class: verification reliability + planning/documentation reconciliation

## 1. Objective

Restore a clean post-M009 readiness baseline before opening another feature milestone.

Corrective 049 has two bounded responsibilities:

1. investigate and eliminate, or rigorously disposition, the transient Rust 1.88 SAM fragmented-reply timeout observed during Plan 048 full verification;
2. reconcile the public/canonical planning surfaces so they describe the repository that actually exists through M009 rather than the older M005/R001-conditional state.

This corrective does not add product features, expand SAM portability testing, or begin the standalone daemon/listener milestone.

## 2. Finding C049-F1 — transient SAM fragmented-reply timeout

Plan 048's closure records that the first:

~~~sh
rustup run 1.88.0 sh scripts/verify.sh full
~~~

encountered one transient SessionCreate timeout in an unrelated SAM fragmented-reply conformance test.

The same test passed in isolation and a complete identical Rust 1.88 verification rerun passed.

This is insufficient evidence of a production SAM defect, but "rerun until green" is not an acceptable long-term verification policy.

Likely affected test:

- crates/sam/tests/sam31_conformance.rs
- a_fragmented_reply_produces_the_same_result

The test intentionally makes the loopback fake bridge write each SAM reply one byte at a time. The production client uses production-scale phase deadlines, while the fake itself uses asynchronous per-byte writes.

Do not assume the root cause is the client, the fake, Rust 1.88, or host contention until reproduced/instrumented.

## 3. Investigation sequence

### A. Recover the exact failure

Use the retained Plan 048/full-verification output if available to confirm:

- exact test name;
- exact timeout phase;
- elapsed time;
- whether the test process was otherwise progressing;
- whether the failure came from sam31_conformance or another fragmented-reply test.

If retained logs are insufficient, state that explicitly and treat the closure wording as the starting hypothesis rather than established test identity.

### B. Targeted repetition

Run the exact affected test repeatedly under:

- current stable;
- Rust 1.88.

Minimum if the test remains fast:

~~~sh
for i in $(seq 1 100); do
  cargo test -p i2pr-irc-sam --test sam31_conformance     a_fragmented_reply_produces_the_same_result -- --exact
done

for i in $(seq 1 100); do
  rustup run 1.88.0 cargo test -p i2pr-irc-sam --test sam31_conformance     a_fragmented_reply_produces_the_same_result -- --exact
done
~~~

If the exact affected test differs, substitute it and record the actual command.

### C. Contention reproduction

Exercise the affected test while representative CPU/build/test contention exists, or repeat the SAM conformance suite with normal test parallelism.

The objective is to distinguish:

- a fake-bridge scheduling race;
- a client framing/state-machine defect;
- a deadline/test-profile defect;
- generic host starvation.

Do not weaken production deadlines merely to make a test pass.

## 4. Fake-bridge review

Review the test-only fragmented write path in crates/sam/src/fake.rs.

Current shape:

~~~text
for each reply byte:
    socket.write_all([byte]).await
~~~

Questions to answer:

- can one scripted reply be consumed or interleaved incorrectly across concurrent connections;
- can the fake stop replying after a partial verb transition;
- can a task be starved while holding state needed by the same test;
- is byte-at-a-time write sufficient to prove fragmented reads without depending on excessive scheduler wakeups;
- does the fake need a deterministic chunking model instead of one async syscall per byte.

A preferable fixture, if a test defect is found, may fragment into a deterministic sequence of small chunks or use a test-only scripted chunk boundary rather than hundreds/thousands of one-byte async writes.

Any change must continue to prove that a SAM line split across arbitrary TCP read boundaries is reassembled correctly.

## 5. Client review

Do not modify production client code unless evidence demonstrates a real defect.

Specifically verify:

- pending/ready/inflight line accounting cannot lose bytes across fragmented reads;
- SessionCreate phase state advances correctly only after a complete classified reply;
- nested per-read and whole-phase timeout behavior cannot accidentally shorten the configured phase;
- two replies in one segment and raw-transition tests remain green.

If a production defect is discovered, keep it in Corrective 049 only if bounded to the framing/deadline behavior already under investigation. Otherwise register a successor corrective before closure.

## 6. Resolution requirements for C049-F1

Corrective 049 may close C049-F1 in one of three ways:

### Test-fixture defect fixed

Preferred if reproducible.

Requirements:

- deterministic test-only fix;
- original fragmentation property retained;
- targeted stress green;
- full verification green first-pass.

### Production defect fixed

Acceptable only with direct evidence.

Requirements:

- smallest bounded production correction;
- deterministic regression;
- no SAM scope/profile expansion;
- full provider/runtime regressions.

### Non-reproducible environmental event

Allowed only with stronger evidence than one rerun.

Minimum:

- exact affected test 100x current + 100x Rust 1.88;
- SAM conformance suite repeated under normal parallel load;
- at least 3 consecutive full Rust 1.88 verification runs pass without retry;
- no suspicious race or fixture defect found in source review.

If those conditions are not met, leave the finding open rather than declaring it environmental.

## 7. Finding C049-F2 — active registry is stale

Current plans/registry.md says the Bouncer Core is M009 closed, but its "Active and dependency-ready implementation plans" table still contains closed Corrective 043 and Plans 044-045.

It also says under "Unplanned later milestones":

- M008 and M009 are fully researched and registered behind Corrective 043.

That is historical and false: 043-048 are all closed.

Corrective 049 registration makes 049 the sole active plan.

At closure:

- Active and dependency-ready table becomes empty unless a successor plan is actually registered;
- 043-048 remain only in closed/history sections;
- M008/M009 are described as closed;
- standalone daemon/listener/key-provisioning remains a future researched/planned line only if separately registered;
- R002 remains independently blocked upstream.

## 8. Finding C049-F3 — README implementation state is materially stale

README currently describes i2pr-irc as a "planned Rust IRC bouncer" and its implementation-state narrative is centered on M001-M005.

It also states R001 is conditionally closed because Java I2P/i2pr portability evidence is missing.

Canonical planning has since established:

- M006 and M007 closed;
- Correctives 042/043 closed;
- M008 encrypted durable state closed;
- M009 OTRv3-transparent carriage closed;
- R001 closed for this repository on the real i2pd product-path evidence;
- broad multi-router SAM conformance delegated to the dedicated SAM library;
- R002 still blocked on public i2pr managed-app contracts;
- no production standalone daemon/listener exists yet.

Update README to reflect those facts concisely.

Do not turn README into a duplicate roadmap.

## 9. Roadmap reconciliation

Update plans/subsystems/bouncer-core-roadmap.md where its prose still describes M008/M009 as the "next" privacy track rather than a completed track.

Expected current state after closure:

- product/core behavior closed through M009;
- Corrective 049 closed;
- no registered successor bouncer-core plan;
- standalone/bootstrap/listener is the obvious future productization line but remains unregistered until researched/planned;
- external real-client OTR qualification waits on that production listener;
- R002 remains separately blocked.

Update plans/002-long-term-roadmap.md only where needed to mark M008/M009 complete and avoid implying they are pending.

Historical closure records remain untouched.

## 10. Documentation accuracy requirements

README must accurately distinguish:

### Implemented core/runtime

- durable multi-Network/multi-client bouncer engine;
- IRC/IRCv3 compatibility and degraded-server behavior;
- reconnect/keep-nick/services resilience;
- SAM provider for standalone-router integration;
- SQLCipher optional encrypted Store;
- OTRv3-transparent opaque transport semantics.

### Not yet a finished standalone product

- no production executable crate;
- no production local TCP/Unix listener/bootstrap;
- no production store-key provisioning UX;
- no packaging/service/install layer;
- no real-client OTR interoperability qualification through a product listener.

Avoid "complete bouncer application" language until those pieces exist.

## 11. Scope

In scope:

- SAM fragmented-reply flake investigation;
- test-fixture or bounded production fix if evidence requires it;
- targeted/full verification stress;
- registry cleanup;
- bouncer/long-term roadmap current-state cleanup;
- README implementation-state reconciliation.

Out of scope:

- new SAM features;
- Java/i2pd/i2pr router matrices;
- migration to the dedicated SAM library;
- standalone daemon/listener implementation;
- keyring/passphrase/HSM UX;
- additional IRCv3 capabilities;
- OTRv4 or new E2EE protocols;
- R002 implementation.

## 12. Verification

Minimum after any code/test change:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Additionally satisfy the C049-F1 stress/disposition matrix in §6.

Documentation-only edits after the final code commit do not require repeating 100x targeted stress, but closure must run at least one final current and Rust 1.88 full verification on the exact closure tree.

## 13. Acceptance criteria

Corrective 049 closes only when:

1. the Plan 048 transient SAM timeout is either deterministically fixed or rigorously classified under §6;
2. no retry-on-failure mechanism, ignored test, or relaxed production timeout is introduced to hide it;
3. the SAM fragmented-reply property remains covered;
4. current and Rust 1.88 full verification pass on the closure tree;
5. registry has no closed plan in the active table;
6. registry no longer describes M008/M009 as waiting behind Corrective 043;
7. README reflects implementation through M009 and R001's current scope;
8. README still states clearly that no finished standalone daemon/listener exists;
9. bouncer/long-term roadmaps no longer present M008/M009 as future work;
10. historical closure records remain unchanged.

## 14. Stop conditions

Stop and register a successor plan/corrective if:

- the SAM failure proves to be a production lifecycle defect beyond framing/test-fixture scope;
- reliable verification requires changing the production SAM profile;
- cleanup reveals a broader CI/test scheduler problem affecting unrelated suites;
- documentation reconciliation requires deciding the standalone architecture rather than merely describing its absence.

## 15. Closure evidence

Create:

- plans/closure/bouncer-core/049-status.md

Record:

- exact transient failure identity from Plan 048;
- reproduction/stress commands and counts;
- root cause or non-reproducible disposition;
- changed test/production files;
- fragmentation property evidence;
- current/Rust 1.88 verification;
- README before/after implementation-state summary;
- registry/roadmap reconciliation;
- explicit statement whether any successor implementation plan is registered.
