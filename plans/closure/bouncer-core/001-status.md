# Bouncer Core M001 Closure Record

Status: corrective pass required

Source plan: `plans/implementation/bouncer-core/001-protocol-domain-and-fault-harness-foundation.md`

Reviewed baseline: `7276d2f8f6ec62c3c8a9023b027bbdaf90528f02`

Implementation commit: `89f7e30a3c32a5828ff1643c58d52f27e20bd1b2`.

## Finding

The workspace, owned bounded wire parser, endpoint/provider contracts, virtual monotonic counter, test stream, boundary check, and deterministic parser smoke executable exist. However the fault harness does not yet cover controllable readiness stalls, reset boundaries, bounded duplex backpressure, and stale-generation pending work. The parser suite lacks the required max/max+1 matrix and arbitrary-input property assertions. The dependency/build-script tree review is not recorded with command output. Therefore M001 cannot be evidence-closed, and M002 remains blocked.

## Requirement-to-evidence matrix

| Requirement | Evidence | Result |
|---|---|---|
| Workspace/MSRV/edition/lints | Root Cargo manifest; Rust 1.88, edition 2024 | Present |
| Owned bounded IRC codec and unknown command preservation | `crates/wire/src/lib.rs`, parser tests | Partial; broad boundary matrices missing |
| I2P-only endpoint/provider and local acceptor contract | `crates/core/src/lib.rs`; invalid endpoint test | Partial; endpoint validation needs specification review |
| Injected time | `VirtualClock` in core | Partial; timer/sleeper semantics not implemented |
| Deterministic stream/provider harness | `crates/testkit/src/lib.rs` | Partial; several required fault controls absent |
| Static no-clearnet guard with positive control | `scripts/check-network-boundary.py` | Present for core/wire source and manifest scan |
| Hostile-input smoke | `crates/fuzz-smoke`, `scripts/fuzz-smoke.sh` | Present as deterministic smoke, not coverage-guided fuzz |
| Architecture/dependency docs | `architecture/*.md` | Present, incomplete fault model |

## Commands executed

| Command | Outcome |
|---|---|
| `cargo fmt --all` | Passed |
| `cargo test --workspace` | Passed, 9 tests |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed after removing the fixture's unused-mut binding |
| `cargo test --workspace --all-features` | Passed, 9 tests across workspace crates |
| `scripts/verify.sh quick` | Passed; boundary, format, clippy, and test checks |
| `scripts/verify.sh full` | Passed; quick checks plus release fuzz-smoke executable |
| `cargo tree --workspace` | Reviewed; no resolver/HTTP/socket client in the tree, no dependency in wire |
| `git diff --check` | Passed before commit |

## Invariant, recovery, and security review

No generic network connector is introduced in core/wire. Endpoint Debug output is redacted. SASL secret Debug output is redacted and Drop zeroizes its owned string. The current endpoint validator is syntactic and does not perform resolution. No production live-network/router behavior is claimed. A monotonic counter exists, but a virtual timer/sleeper abstraction and deterministic timer cancellation are absent.

## Unresolved findings

- **High:** M001 required fault and injected-timer semantics are incomplete; M002 readiness is not justified.
- **Medium:** closure verification floor and dependency tree evidence are incomplete.
- **Medium:** wire bound and malformed-input coverage is narrower than plan acceptance.

## Roadmap disposition

M001 status is corrective pass required. M002 remains blocked on M001 closure. No later implementation plan is dependency-ready: M003 waits on M002; M004 and M005 follow in order; router work depends on M005 and external public contracts. No future plan is unblocked by this result.

Registry and roadmap were updated to reflect this disposition.
