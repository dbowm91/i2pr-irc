# Standalone M010-A / Plan 050 — Daemon Runtime and Process Bootstrap

Status: closed — see plans/closure/standalone/050-status.md
Repository baseline: f325e7d5e495e36b1fc7b168c30e4b725756d22c
Primary class: infrastructure + process-lifecycle invariant
Roadmap: plans/subsystems/standalone-daemon-roadmap.md
Research: plans/research/010-m010-standalone-daemon-local-access-and-bootstrap.md
Decision: plans/adrs/ADR-0007-local-authentication-and-standalone-process-boundary.md

## 1. Objective

Establish an executable that owns precisely one durable local bouncer process: parse an explicit bounded configuration, obtain exclusive state-directory ownership, open an existing store, construct SamProvider and RuntimeController, restore durable Networks, accept termination, and stop everything predictably. Do not open any listener or claim a working standalone bouncer yet.

## 2. Readiness and dependencies

Ready: M001-M009, C049, R001 closed. Current public APIs: Store::open_with_options, StoreHandle, RuntimeController::new/serve, RuntimeControlHandle::request_stop, SamProvider::with_config/with_limits. Explicit file locking on Rust 1.88 and signal handling require dependency/MSRV review. No dependency on R002, i2pr-sam migration or OTR crypto.

## 3. Invariants and scope

- Preserve one-owner-per-Network, I2pStreamProvider-only upstream, one process-wide reconnect budget and distinct durable/live state.
- A process owns a state directory exclusively before it opens SQLite or starts a Network owner; the lock lives until store shutdown.
- Starting a controller construction has no side effects; serve() is the restore authority.
- No listener, generic DNS, non-I2P upstream, exec hooks, daemonization/forking or background service install.
- Configuration/diagnostics cannot leak credentials, private destinations, raw frames or machine identifiers to upstream IRC.

## 4. Required production changes

Create crates/daemon (package/binary i2pr-irc) with a minimal application library/test surface if useful, and register it in root workspace. Enable the needed Tokio net/signal features only where used; in 050 listener net must not be used.

Implement explicit bounded config version, data/store paths, state-dir ownership, numeric SAM loopback bridge host/port, local listener configuration parsed but not activated, store encryption mode/key-file reference without reading key material until 053, resource ceilings and shutdown diagnostics. Existing database with explicit plaintext mode may be used for initial bootstrap tests; 050 must not pretend encrypted-key provisioning exists.

Define CLI shape with distinct init/run/status/help/version entrypoints or a smaller reviewed subset; unimplemented commands fail explicitly rather than silently succeeding. Do not store auth secrets in CLI args. Resolve relative state-dir paths against explicit config location, not environment-derived IRC-visible identity.

Acquire nonblocking per-state-dir file lock using a 1.88-compatible, reviewed crate or a small platform-specific module with unsafe boundary review (workspace forbids unsafe). Refuse symlink/unsafe directory paths where feasible; prevent lock acquisition via a world-writable shared path. Stale PID files are diagnostic only, not exclusive ownership. Lock cleanup must not delete another process's state.

Construct Store before RuntimeController; spawn serve as the unique supervised task; distinguish initial restore errors, transient SAM unavailable, and fatal store errors. A local router being temporarily unavailable must not be converted to a clearnet fallback. On SIGINT/SIGTERM or internal stop, cancel outer tasks, request_stop(), await controller join with documented bound, flush/close store, release lock. Document exactly which shutdown phase can time out and what evidence is retained.

## 5. Ordered work packages

A. Review MSRV/licensing and explicitly choose config parse, locking and signal crates (record results in architecture/dependency-review.md).
B. Introduce executable + config validation and redacted structured startup errors; implement --help/version/read-only config inspection as appropriate.
C. Implement secure path and directory lease boundary; reject second concurrent process before store access.
D. Compose Store, SamProvider, RuntimeController with explicit startup/ready/stop state transitions and durable restore.
E. Add failure injection and doc examples for a deliberately pre-provisioned plaintext test fixture only.

## 6. Failure, restart and contention semantics

Invalid config, duplicate lock, wrong store path, or unreadable store are startup failure (nonzero exit); never recreate state silently. Existing Network owners may start while SAM is down and recover through existing scheduler; do not invent a second retry loop. Stop during startup must release any acquired file/store resources. Concurrent control queue saturation must not block request_stop. No orphan tasks after a completed normal shutdown.

## 7. Compatibility and tests

- Config parse rejects unknown unsafe modes, overlong values and nonloopback SAM endpoints.
- Two binaries/processes targeting one state dir: first stays owner, second promptly fails; first can restart after exit/crash.
- Store restore starts one owner per valid Network; an invalid durable row behavior remains consistent with controller.
- Stop before restore, during store open, during unavailable router, and during controller activity.
- Test secret-free error strings and deterministic diagnostics; no endpoint data in externally observable IRC fields.
- Verify using temporary OS directories and injected stop signals; tests must not need a live router.

## 8. Verification and docs

Targeted crate integration/child-process tests; cargo fmt, clippy --workspace --all-targets --all-features --locked -D warnings; cargo test --workspace --all-features --locked; sh scripts/verify.sh full; Rust 1.88 equivalent. Update README with an explicit *not-yet-listening* bootstrap status, deployment/locking documentation and Cargo.lock. Do not claim operational product readiness from compilation.

## 9. Acceptance / stop conditions

Accept only if a real binary starts against an existing valid store, restores controller ownership, excludes duplicate processes and exits with all Store/Network owners joined and lock released, with deterministic negative tests and compatible MSRV. Stop if exclusive ownership cannot be enforced, store access is ambiguous, shutdown leaves orphan work, or a generic networking primitive is required.

## 10. Closure evidence / handoff

Record exact commits, requirement/evidence matrix, commands run and outcomes, security/resource review, limitations and next readiness at plans/closure/standalone/050-status.md. On closure mark 050 closed and promote 051 ready only after confirming its interface needs. Do not begin 051 implicitly.
