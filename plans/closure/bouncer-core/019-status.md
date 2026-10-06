# Bouncer Core Corrective 019 — M004 Findings Closure

Plan: `plans/implementation/bouncer-core/019-m004-findings-corrective.md`

Baseline: `f708011` ("Correct M004 closure record after independent audit")

Source findings: `plans/closure/bouncer-core/018-status.md` — UF-015-1, UF-017-1, UF-018-1

Class: invariant + testability corrective

## 1. Outcome

All three findings are closed. M004 has no open findings.

The corrective added no protocol feature, no dependency, and no egress path. What it changed
is the difference between two claims that had been treated as equivalent and are not:

- "no production code calls the legacy Network owner" became "the legacy Network owner does
  not exist in the production build";
- "the stalled-provider campaign bounds attempts" became "the campaign's ceiling is below
  what a simulated spin produces, which was verified by mutation rather than asserted".

## 2. Gated items and before/after reachability

Every row is in `crates/runtime/src/lib.rs`. The attribute sits on the line above each
declaration.

| Item | Line | Before | After |
|---|---|---|---|
| `impl UpstreamConfig` | 157 | reachable from the shipped rlib | test build only |
| `pub struct NetworkSnapshot` (legacy) | 211 | reachable; shadowed `owner::NetworkSnapshot` | test build only |
| `type AcceptFuture<'a, A>` | 240 | reachable | test build only |
| `pub struct NetworkSupervisor<P>` | 257 | `pub` at the crate root; in the public API | test build only |
| `impl<P: I2pStreamProvider> NetworkSupervisor<P>` | 263 | reachable | test build only |
| `fn apply_upstream_line` | 826 | reachable | test build only |
| `async fn next_accept` | 899 | reachable | test build only |
| `async fn read_client` | 909 | reachable | test build only |
| `async fn client_writer_exit` | 920 | reachable | test build only |
| `pub(crate) async fn stopped` | 953 | reachable | test build only |
| `pub(crate) fn queue_control` | 1002 | reachable | test build only |
| `async fn next_intent_frame` | 1031 | reachable | test build only |
| `async fn send` | 1038 | reachable | test build only |
| `pub struct UpstreamConfig` | 148 | reachable | test build only |

The ungated connect the finding named, `lib.rs:326`
(`self.provider.connect(&self.config.endpoint)`), is inside the gated `serve` and is
therefore absent from the production build.

### Two corrections to the plan's own instructions

**The private helpers were gated, not removed.** The plan (§4, §11) said to delete
`apply_upstream_line`, `next_accept`, `read_client`, `client_writer_exit`,
`next_intent_frame`, and `send` once the supervisor was gated, and named "proving any of
them is load-bearing" as a stop condition. Deletion was attempted and they are all
load-bearing — every one is called from `NetworkSupervisor::serve` itself. Deleting them
breaks the legacy suite that §9 requires to keep passing. They are gated `#[cfg(test)]`
instead, which achieves the plan's actual goal — they leave the production build, and
`clippy -D warnings` is clean — without breaking a live suite. The stop condition was
respected in substance: the deletion was not forced through.

**`queue_control` and `stopped` were gated too, though the plan did not list them.** Both
lost their last production reader when the supervisor was gated: `owner.rs:2458` and
`downstream.rs:652` each define their own private `queue_control`, and `owner.rs:2424`
defines its own `stopped`. Leaving either ungated makes `dead_code` fire, so
`clippy -D warnings` could not be satisfied. Gating keeps
`control_queue_is_separate_and_normal_overflow_is_explicit` working, which is what the plan
required of it.

Consolidating the three `queue_control` copies and the two `stopped` copies is deliberately
**not** in this corrective. It is a de-duplication, not the closure of a finding, and it
would touch shipping code on no evidence that the copies disagree.

### What is proven and what is not

`crates/runtime/tests/corrective_019.rs` asserts the gating two ways, and neither is a
link-time proof:

- `the_legacy_network_owner_is_gated_out_of_the_production_build` reads `lib.rs` and
  asserts each declaration is immediately preceded by `#[cfg(test)]`. Rust cannot reference
  a correctly-absent type, so a test naming `NetworkSupervisor` would fail to compile for the
  wrong reason; the source form is the honest check.
- `no_other_first_party_module_references_the_legacy_owner` scans every `crates/runtime/src`
  file except `lib.rs` for the three exclusive names.

Independently observed: after `cargo doc -p i2pr-irc-runtime --no-deps`, the generated
public API contains no `NetworkSupervisor` item. The only occurrence of the string in the
rendered index is prose in the crate-level doc that names it as gated.

## 3. Ported production-path coverage

`crates/runtime/tests/corrective_019.rs`, 10 tests. All drive `owner::NetworkOwner::serve`
through `SupervisorContext` and `SupervisorCommand`, the same entry points the catalog uses.

### SASL PLAIN — 4 tests

| Test | Asserts |
|---|---|
| `the_production_path_completes_a_sasl_plain_handshake` | `CAP REQ :sasl message-tags` is sent; `AUTHENTICATE PLAIN` opens the exchange; `AUTHENTICATE <base64>` carries `\0alice\0swordfish`; the password never appears unencoded |
| `a_sasl_credential_reaches_no_diagnostic_and_no_downstream_byte` | the password, the username, and the base64 payload are absent from the structured snapshot and from downstream bytes |
| `a_refused_sasl_credential_is_a_terminal_registration_error` | `904` ends the owner with `Err(Registration)` rather than retrying |
| `sasl_configured_but_not_offered_is_a_terminal_registration_error` | a Network configured for SASL refuses to register against a server that never offered it |

The credential payload is computed from the constants rather than pasted in, so a change to
either cannot leave a literal asserting the wrong value.

### Upstream `QUIT` fence — 4 tests

| Test | Asserts |
|---|---|
| `an_explicit_stop_writes_exactly_one_upstream_quit` | exactly one `QUIT :Bouncer shutting down`, and it is the final frame |
| `no_client_traffic_reaches_the_network_after_the_quit_fence` | nothing follows the `QUIT`; no `QUIT` appears before the explicit stop |
| `a_failed_generation_is_aborted_and_sends_no_quit` | a generation that lost its upstream emits no `QUIT` and never says "Bouncer shutting down" |
| `a_terminally_rejected_generation_sends_no_quit` | a registration rejection ends the owner with no `QUIT` |

Both capture-based tests read `FaultController::bytes_written(0)` for the specific
connection, so a later generation's traffic cannot be mistaken for the one under test.

### Production absence — 2 tests

Described in §2.

### Mutation evidence

Both behaviours were checked by mutating production code and confirming the new tests fail.

| Mutation | Result |
|---|---|
| Remove the `QUIT` `try_send` at `owner.rs:1453` | `an_explicit_stop_writes_exactly_one_upstream_quit` and `no_client_traffic_reaches_the_network_after_the_quit_fence` fail; 6 pass |
| Remove the `AUTHENTICATE PLAIN` send at `owner.rs:1042` | all three handshake tests fail; 5 pass |
| Set `Backoff` `base` and `cap` to zero at `owner.rs:815-820` | `a_stalled_provider_produces_a_bounded_number_of_attempts` fails at 100 attempts against a ceiling of 64 |

All mutations were reverted; `git diff` contains none of them.

## 4. Stalled-provider campaign: measured before and after

`a_stalled_provider_produces_a_bounded_number_of_attempts`, `crates/runtime/tests/adverse.rs`.

**Before.** One `advance(600s)` and `NETWORKS <= attempts <= NETWORKS * 32`. Measured **4
attempts for 4 Networks** — exactly the lower bound, 32x under the upper bound. A spin and
correct backoff were indistinguishable.

**Cause.** `tokio::time::advance` performs a single poll. A timer re-armed during that poll
never fires, so `connect -> backoff -> retry` advances one link per call.

**After.** The clock is stepped in a loop and the ceiling is derived from measurement.

Attempts scale with both virtual time and poll count:

| steps x step | virtual time | attempts |
|---|---|---|
| 1 x 600s (before) | 600s | 4 |
| 1000 x 100ms | 100s | 4 |
| 500 x 600ms | 300s | 12 |
| 1000 x 300ms | 300s | 12 |
| 1000 x 600ms | 600s | 20 |
| 2000 x 600ms | 1200s | 32 |
| **5000 x 600ms (chosen)** | **3000s** | **52** |
| 20000 x 600ms | 12000s | 141 |

Deterministic: `fleet_budget()` fixes the jitter seed and `jitter_entropy` is a pure function
of Network, generation, and seed. 52 reproduced exactly across repeated runs.

The lower bound is now `attempts > NETWORKS` — strictly above, which is what the old
`>=` could not express — and the ceiling is `NETWORKS * 16` = 64.

### The ceiling is mutation-verified, and the first attempt at one was not

An earlier iteration chose `NETWORKS * 6` = 24 at 1000 steps. That bound **passed** under the
backoff-removed mutation, because at 1000 steps both configurations measure 20. The two
curves start together:

| steps | with backoff | backoff removed |
|---|---|---|
| 1000 | 20 | 20 |
| 2000 | 32 | 40 |
| 5000 | 52 | 100 |
| 20000 | 141 | 400 |

A short campaign cannot separate them at all. 5000 steps is where the gap supports a real
ceiling: 52 is comfortably under 64, and 100 is comfortably over it. The test comment records
this table so a future tightening does not re-pick a length that cannot discriminate.

The mutation was `base` and `cap` set to zero at `owner.rs:815-820` — the minimal change that
removes backoff and nothing else. Note that this is a bound on *backoff being honoured*, not
on arbitrary CPU spin: under a paused runtime the attempt rate is structurally capped by
poll count, so no attempt-count ceiling in this harness can detect a busy-loop that never
calls `connect`.

## 5. `MAX_TOTAL_SESSIONS`

Deleted from `crates/runtime/src/catalog.rs`. No source file references it; the only remaining
occurrences are the plan and closure narratives that discuss its removal.

`git log -S` shows it was never read since introduction at `646937e`.

The real bound, all three terms enforced:

| Factor | Value | Enforcement |
|---|---|---|
| Supervised Networks | 64 | `catalog.rs:31`, checked at `catalog.rs:212` |
| Sessions per Network | 64 | `owner.rs:51`, checked at `owner.rs:1492` and re-applied at `owner.rs:1195` |
| Session queue | 256 | `catalog.rs:36` (`SESSION_QUEUE_CAPACITY`) |

Product: **4096** concurrent sessions. `plans/closure/bouncer-core/012-status.md:116` was
amended from "1024 / `MAX_TOTAL_SESSIONS`" to the derived 4096 with an explanatory note.

1024 was **not** enforced. It has no product requirement behind it, and refusing legitimate
attaches at an arbitrary number would be a behaviour change with no basis.

## 6. Documentation corrections

- `timeout_bounded` (`lib.rs`): its doc comment claimed a virtual clock must not be able to
  expire the deadline without an explicit `advance()`. It is `tokio::time::timeout` over a
  pausable clock, and auto-advance fires it — a parked 120s deadline expires in microseconds
  of real time. The comment now states the real behaviour and connects it to the campaign
  defect above.
- `apply_upstream_line`: the comment claiming the legacy supervisor "shares this policy with
  the production owner rather than having its own" now says it is a *duplicate* of
  `owner.rs`'s enforcement site and that no shared guard exists.
- `architecture/network-supervisor.md`: the doc opened by describing `NetworkSupervisor::serve`
  as the owner. It now names `owner::NetworkOwner` as the shipping implementation, records
  that the legacy one is `#[cfg(test)]`-only, and drops the `LocalAcceptor` diagram in favour
  of the multi-session shape the live owner actually has.
- `architecture/storage.md`: topology label updated from `NetworkSupervisor / BouncerRuntime`
  to `NetworkOwner (catalog-supervised)`.
- `plans/closure/bouncer-core/018-status.md`: findings section amended with a disposition
  table. The record's matrices were not rewritten. Two inline annotations were added where
  the text would otherwise read as a present-tense claim about a constant that no longer
  exists.
- `plans/registry.md` and `plans/subsystems/bouncer-core-roadmap.md`: 019 moved from active to
  closed; the M005 handoff now reads "plan or research only".

## 7. Test-count delta

| | Baseline `f708011` | After | Delta |
|---|---|---|---|
| Tests | 464 | 474 | +10 |
| Test binaries | 17 | 18 | +1 |

The delta is entirely `crates/runtime/tests/corrective_019.rs`. The legacy in-`lib.rs` suite
still runs at 192 tests, and `cargo test -p i2pr-irc-runtime --lib` is green, so gating cost
no coverage.

`Cargo.lock` is unchanged. No third-party dependency was added; MSRV remains 1.88.

## 8. Verification

All commands run at the closure commit and all exited 0.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | exit 0 |
| `cargo test --workspace --all-features` | 474 passed, 0 failed |
| `scripts/check-network-boundary.py` | exit 0 |
| `scripts/verify.sh full` | exit 0, 474 tests |
| `rustup run 1.88.0 sh scripts/verify.sh full` | exit 0, 474 tests |

Toolchain: `1.88.0-aarch64-apple-darwin` for the MSRV run; host `darwin 25.6.0 x64`.

One pre-existing rustdoc warning remains and is untouched: `chathistory.rs:746` links to the
private item `parse_limit`.

## 9. Disposition of each finding

| Finding | Original severity | Disposition |
|---|---|---|
| UF-015-1 | medium | **Closed.** Gated, not deleted. The legacy owner, `UpstreamConfig`, the legacy `NetworkSnapshot`, `AcceptFuture` and the legacy-only helpers are all absent from the production build. The two behaviours that made deletion unsafe now have production-path coverage, verified by mutation. Severity revised to low in the 018 amendment. |
| UF-017-1 | low (raised to medium in the 018 amendment) | **Closed.** The campaign is repaired and its ceiling is mutation-verified against a backoff-removed spin. The `timeout_bounded` doc comment is corrected. |
| UF-018-1 | low | **Closed.** The constant is deleted and the 012 ceiling row is corrected to the derived 4096. |

## 10. Effect on M005

**Neither unblocked nor changed.** M005 was never blocked by this corrective; it was
planning/research eligible throughout, and the roadmap recorded that from the moment 019 was
registered. Closing it changes no M005 dependency, scope, or acceptance criterion.

## 11. Recorded, not fixed

Two items were examined and deliberately left alone, so a later reader does not mistake them
for oversights:

- **Three copies of `queue_control`, two of `stopped`.** `lib.rs:1002` (now test-only),
  `owner.rs:2458`, `downstream.rs:652`; `lib.rs:953` (now test-only) and `owner.rs:2424`.
  Consolidation would touch shipping code with no evidence that the copies disagree. This is
  the natural follow-up to this corrective, not part of it.
- **`Backoff` injection into `NetworkOwner::serve`.** Still not done, still for the reason in
  plan §6: it changes a shipping constructor's signature for a benefit no current test needs.
  If M005's operator diagnostics work needs the retry schedule pinned, this is where to
  revisit it — the mutation table in §4 is the evidence that makes such a test possible.
- **The 25 gated legacy tests.** They remain a maintenance surface. Corrective 019 made them
  redundant rather than load-bearing, but did not schedule their removal. Deleting them, and
  with them the legacy owner, is the natural next step and needs no plan change.