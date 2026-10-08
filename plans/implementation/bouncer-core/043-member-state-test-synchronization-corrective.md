# Bouncer Core Corrective 043 — Deterministic Member-State Test Synchronization

Status: ready for handoff

Repository baseline:

- e66c79a19174d3184edf16a656d4398aeff42715

Raised by:

- plans/closure/bouncer-core/042-status.md §7

Primary class: verification/test-harness corrective

## 1. Objective

Remove the known scheduler-dependent flake from:

- crates/runtime/tests/m005g_member_state.rs
- a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed

without changing production member-state behavior.

Corrective 043 is test synchronization only. M005, M006, M007, and Corrective 042 remain historical product closures.

## 2. Finding

The test intends to prove this sequence:

1. Alice joins after the channel's original NAMES run.
2. MODE +v Alice is processed.
3. only then does a multi-prefix downstream client attach.
4. the fresh projection contains +Alice but must not invent a complete @%+ run that was never observed.

The fixture currently writes JOIN and MODE, then registers the downstream client, then invokes two downstream-visible barriers.

Those barriers prove ordering only after the client already exists. They cannot prove that JOIN and MODE were processed before attachment, which is the premise being tested.

Under scheduler contention the client may attach before one or both upstream mutations are applied. Corrective 042 observed exactly this as a one-run Rust 1.88 failure followed by an identical successful rerun.

Severity: verification correctness. No product defect is established.

## 3. Required synchronization model

Use a pre-attachment upstream protocol barrier.

After writing the JOIN and MODE frames, the test fixture should send a unique server-originated PING and wait until the bouncer writes the matching PONG upstream.

Conceptual transcript:

~~~
server -> :Alice!u@h JOIN #room
server -> :Op!u@h MODE #room +v Alice
server -> PING :m005g-member-barrier

bouncer -> PONG :m005g-member-barrier

only now:
client -> register/attach
~~~

Because the NetworkOwner processes upstream frames in stream order, observing the matching PONG proves that every preceding frame on that same generation reached the owner before attachment begins.

The barrier must be generation-local and must not rely on sleeps, scheduler yields, wall-clock timing, or downstream fanout.

## 4. Testkit/helper shape

Preferred implementation:

- add a narrow test-only helper in m005g_member_state.rs or the existing test fixture;
- helper accepts the runtime and peer index;
- helper emits a bounded unique PING token;
- helper reads until the exact matching PONG on that upstream scripted peer;
- helper has the existing test ceiling/timeout;
- helper is used before calling register(...) in the flaky test.

Do not add a production API solely to synchronize a test.

If an existing reusable fake-provider barrier already provides equivalent upstream-order proof, use it instead, but document why it establishes ordering before attachment.

## 5. Regression requirements

The corrected test must:

- run JOIN -> MODE -> upstream barrier -> client attach;
- remove the post-attachment double barrier used as the false premise proof;
- still assert +Alice is retained;
- still assert @%+Alice is not fabricated;
- fail if the JOIN/MODE processing is deliberately moved after client attachment;
- not depend on sleep durations.

Add or adapt a helper-level test if useful to prove the PING/PONG barrier is stream ordered.

## 6. Stress qualification

The point of this corrective is the absence of a race, not one green run.

Run the affected test repeatedly under both supported toolchains.

Minimum:

~~~sh
for i in $(seq 1 100); do
  cargo test -p i2pr-irc-runtime --test m005g_member_state     a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed -- --exact
done

for i in $(seq 1 100); do
  rustup run 1.88.0 cargo test -p i2pr-irc-runtime --test m005g_member_state     a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed -- --exact
done
~~~

Equivalent deterministic repetition is acceptable.

No retry-on-failure wrapper is acceptable evidence.

## 7. Repository verification

Run:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Both full verification runs must pass on the first invocation used for closure evidence.

## 8. Scope

In scope:

- test-only synchronization helper;
- flaky m005g member-state test;
- comments explaining the ordering proof;
- closure/registry reconciliation.

Out of scope:

- production member-state code;
- member projection semantics;
- upstream/downstream protocol behavior;
- timeout/retry policy changes;
- privacy/encryption work.

## 9. Acceptance criteria

Corrective 043 closes only when:

1. JOIN and MODE are deterministically processed before the client attaches.
2. The test no longer uses a downstream-after-attachment barrier to prove pre-attachment ordering.
3. The targeted test passes 100 consecutive runs on current stable.
4. The targeted test passes 100 consecutive runs on Rust 1.88.
5. Current and Rust 1.88 full verification pass without retry.
6. No production source behavior changes.
7. No new flake is hidden with retry logic or ignored status.

## 10. Closure evidence

Create:

- plans/closure/bouncer-core/043-status.md

Record:

- exact old race;
- new ordering mechanism;
- changed files;
- repeated-test counts/results;
- current/Rust 1.88 full-suite results;
- explicit statement that this was a verification defect, not a product behavior defect;
- readiness disposition for the privacy/encryption implementation line.
