# Bouncer Core Corrective 034 — Restore Rust 1.88 Repository Verification

Status: closed 2026-10-07 — see plans/closure/bouncer-core/034-status.md

Repository baseline:

- `140dac560f356aa9cc8985002a5bcf45c96c163d`

Raised by:

- `plans/closure/router-integration/033-status.md`

Canonical authority:

- workspace `rust-version = "1.88"`
- `plans/002-long-term-roadmap.md` Phase 1 verification floor
- `plans/000-long-term-specification.md` verification requirements
- `plans/003-planning-process.md`

Primary class: verification + maintenance corrective

## 1. Objective

Restore a fully green repository verification floor under the declared Rust 1.88 MSRV after Corrective 033 confirmed that:

~~~sh
rustup run 1.88.0 sh scripts/verify.sh full
~~~

fails in pre-existing `crates/core/src/lib.rs` Clippy diagnostics even though:

- current-toolchain full verification passes;
- the SAM crate itself passes Rust 1.88 Clippy;
- the same failure reproduces on the pre-Corrective-033 baseline;
- no R001/SAM production behavior caused it.

This corrective is intentionally narrow. It does not reopen M005 or R001 and does not change any networking, persistence, protocol, identity, or router semantics.

## 2. Finding

### C034-F1 — declared Rust 1.88 full verification is red

The workspace declares:

~~~toml
rust-version = "1.88"
~~~

The repository closure convention repeatedly treats Rust 1.88 verification as part of the supported floor.

Corrective 033 recorded that the current repository passes:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh full
~~~

but fails:

~~~sh
rustup run 1.88.0 sh scripts/verify.sh full
~~~

in `crates/core/src/lib.rs` on `clippy::uninlined_format_args`.

The failure is pre-existing but still violates the repository's declared verification floor.

Severity: medium for release/maintenance correctness; no known runtime behavior defect.

## 3. Invariants

1. Rust 1.88 remains the declared MSRV.
2. Current stable verification remains green.
3. No production behavior changes are required to satisfy the corrective.
4. Do not globally disable `clippy::uninlined_format_args`.
5. Do not weaken `-D warnings`.
6. Do not remove `--all-targets`, `--all-features`, or `--locked` from verification.
7. Do not special-case `crates/core` out of the MSRV run.
8. Any source rewrite must preserve exact test semantics.
9. No dependency version changes are required unless Rust 1.88 exposes an additional dependency/MSRV failure after the Clippy issue is fixed.
10. R001's live i2pd evidence and all SAM/network-boundary semantics remain unchanged.

## 4. Required investigation

Run the exact failing command first and capture every Rust 1.88 diagnostic, not only the first one.

Classify findings into:

- source-level Clippy/style incompatibility;
- rustfmt difference;
- compiler language/API incompatibility;
- dependency MSRV incompatibility;
- build-script/tooling incompatibility.

The known first finding is `clippy::uninlined_format_args` in `crates/core/src/lib.rs`.

Do not assume it is the only finding until the entire full verification command reaches completion.

## 5. Preferred correction policy

For source-level `uninlined_format_args` findings:

- rewrite formatting to syntax accepted cleanly by Rust 1.88 and current stable;
- prefer named/interpolated format arguments where the value is already bound;
- when the argument is a complex expression and old Clippy requires inlining, bind it to a clearly named local first rather than adding a lint suppression;
- keep assertions/readability at least as clear as the current tests.

Examples of the expected class of rewrite:

~~~rust
let parsed = I2pEndpoint::parse(form).unwrap();
assert_eq!(format!("{parsed:?}"), "I2pEndpoint([redacted])");
~~~

rather than:

~~~rust
assert_eq!(
    format!("{:?}", I2pEndpoint::parse(form).unwrap()),
    "I2pEndpoint([redacted])"
);
~~~

The implementation should use the exact fixes suggested by Rust 1.88 Clippy only after checking they also pass current stable.

## 6. Suppression policy

A lint allowance is not the default fix.

A narrow local `#[allow(clippy::...)]` may be used only if all of the following are demonstrated in the closure record:

- Rust 1.88 Clippy reports a false positive or demands a rewrite that current stable rejects or makes materially less clear/correct;
- there is no equivalent syntax accepted by both toolchains;
- the allowance is attached to the smallest possible item;
- current and MSRV verification both pass afterward.

Workspace/global suppression is forbidden by this corrective.

## 7. Verification script integrity

Do not change `scripts/verify.sh` merely to make the failure disappear.

The script must continue to run:

~~~sh
scripts/check-network-boundary.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/fuzz-smoke.sh   # full mode
~~~

under whichever toolchain invokes it.

A script change is permitted only for a genuine toolchain portability bug that preserves or strengthens all checks, and must be separately justified in closure evidence.

## 8. Work packages

### A. Reproduce and inventory Rust 1.88 failures

Run the full command from a clean tree and record all diagnostics.

### B. Correct cross-toolchain source diagnostics

Apply behavior-neutral rewrites in `crates/core` and any later files exposed by the full run.

### C. Re-run package-level checks

At minimum:

~~~sh
rustup run 1.88.0 cargo clippy -p i2pr-irc-core --all-targets --all-features --locked -- -D warnings
rustup run 1.88.0 cargo test -p i2pr-irc-core --all-features --locked
~~~

Then repeat for any additional affected crate.

### D. Restore repository-wide MSRV verification

Run the exact full Rust 1.88 verification command until green.

### E. Cross-check current stable

Run current-toolchain full verification and confirm no lint/test regression.

### F. Reconcile planning evidence

Close Corrective 034 and remove the pre-existing MSRV warning from the active registry handoff section.

Do not rewrite Corrective 033's historical closure; its statement that the failure was pre-existing remains true.

## 9. Required tests/evidence

Closure evidence must include:

- the exact pre-fix Rust 1.88 diagnostics;
- file/line or test/function disposition for every finding;
- exact source-only corrections;
- package-level Rust 1.88 Clippy/test result;
- full Rust 1.88 verification result;
- current stable full verification result;
- network-boundary result;
- confirmation that no Cargo dependency changed, or an explicit review if one unexpectedly had to change;
- confirmation that no production SAM/runtime behavior changed.

## 10. Acceptance criteria

Corrective 034 closes only when all are true:

1. `rustup run 1.88.0 sh scripts/verify.sh full` passes from a clean tree.
2. Current-toolchain `scripts/verify.sh full` passes.
3. Rust 1.88 remains the workspace `rust-version`.
4. `-D warnings`, `--all-targets`, `--all-features`, and `--locked` remain intact.
5. No global lint suppression is added.
6. No production behavior/API/network authority changes are introduced.
7. R001 Corrective 033 evidence remains unchanged.
8. Registry/roadmap accurately show no remaining MSRV verification defect.

## 11. Stop conditions

Stop and register a broader corrective if the full Rust 1.88 run exposes:

- a dependency whose current locked version no longer supports Rust 1.88;
- language/library usage that cannot be expressed on 1.88 without changing production semantics;
- a requirement to raise the workspace MSRV;
- a verification-script incompatibility affecting multiple toolchains;
- more than localized source/test lint cleanup across unrelated subsystems.

Do not silently raise the MSRV to close this plan.

## 12. Closure evidence

Create:

- `plans/closure/bouncer-core/034-status.md`

Record:

- C034-F1 disposition;
- before/after toolchain diagnostics;
- changed files and why each change is behavior-neutral;
- Rust 1.88 package and full-suite verification;
- current-toolchain full-suite verification;
- dependency/network-boundary review;
- unresolved findings/severity;
- explicit statement that M005 and R001 remain historically closed/conditionally closed exactly as before and were not reopened.
