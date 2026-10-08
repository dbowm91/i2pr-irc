# Bouncer Core Corrective 043 — Closure Status

Status: closed

Implementation commit:

- `8d3033162d12f5a1fe17494e683a4645e4684d2e` — add a generation-local upstream PING/PONG barrier before downstream registration.

## Finding and disposition

Corrective 042 identified a scheduler-dependent flaw in the premise of `a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed`. The JOIN and MODE frames were sent before attachment, but the former downstream barriers were issued only after attachment and therefore could not prove the intended ordering. This was a verification defect; no production member-state behavior defect was established.

## Requirement-to-evidence matrix

| Requirement | Evidence |
|---|---|
| JOIN and MODE are processed before client attachment | Test sends both upstream, then sends a unique server-originated PING and waits for its matching PONG on the same scripted upstream peer before `register()`. Stream ordering makes the barrier generation-local and proves all preceding frames reached the owner. |
| Remove false post-attachment premise proof | The two downstream `sync` calls were removed from the target test. |
| Preserve observed delta and avoid fabricated completeness | Target test still asserts `+Alice` and rejects `@%+Alice`. |
| No sleeps, scheduler yields, retries, or production API | Barrier uses the existing bounded stream reader; only test code changed. |
| Current stable repeated qualification | 100 consecutive exact targeted invocations passed. |
| Rust 1.88 repeated qualification | 100 consecutive exact targeted invocations passed. |
| Full current stable repository verification | `scripts/verify.sh full` passed, including network-boundary check, formatting, clippy with warnings denied, workspace tests, and fuzz smoke. |
| Full Rust 1.88 repository verification | `rustup run 1.88.0 sh scripts/verify.sh full` passed with the same stages. |

## Security and recovery review

No production code, network authority, protocol behavior, secrets, or runtime resource policy changed. The barrier token is synthetic and test-only. No retry-on-failure wrapper or ignored status was used. The upstream peer read has the existing 10-second test ceiling.

## Findings

No new findings. The original issue was limited to the test's ordering proof.

## Roadmap disposition

Corrective 043 is closed. Plan 044 is unblocked and ready: its hard dependency is this closure, and no M008/M009 requirement depends on an external router contract. Plans 045–048 remain sequentially gated by their named predecessor closures.
