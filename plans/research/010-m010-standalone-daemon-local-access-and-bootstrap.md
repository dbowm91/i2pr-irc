# Research 010 — M010 Standalone Daemon, Local Access, and Bootstrap

Status: completed research; informs ADR-0007 and Plans 050-054
Research date: 2026-10-08
Source baseline: main at f325e7d5e495e36b1fc7b168c30e4b725756d22c

## Question and scope

Determine how to turn the evidence-closed bouncer engine (M001-M009) and SAM router adapter (R001) into a headless, independently deployable I2P-only IRC bouncer. Research only executable ownership, local clients, authentication, stable client identity, storage/key bootstrap, shutdown, resource bounds, and product-path qualification. Do not reopen core protocol milestones or require blocked router R002.

## Repository evidence

1. Cargo.toml defines library crates core, wire, runtime, sam, store, and testkit, plus test-only fuzz-smoke; there is no production executable or listener.
2. crates/runtime/src/controller.rs provides RuntimeController::new(provider, StoreHandle), serve() with durable restore, a bounded RuntimeControlHandle, and request_stop() on a separate watch channel. Controller startup does not bind sockets.
3. crates/runtime/src/admission.rs receives a *trusted* ClientId, uses a 60-second registration limit, supports unbound control sessions and one-time PreparedSession transfer. That limit is NOT a pre-authentication bound.
4. crates/core/src/lib.rs LocalAcceptor::accept returns (ClientId, Stream). This is a trust contract, not a socket-accept or authentication implementation.
5. crates/runtime/src/session.rs handles CAP/NICK/USER/BOUNCER BIND before registration; it does not authenticate downstream PASS or SASL. Its decoder and pending lines survive registration transfer. Pre-auth CAP LS followed by a waiting client creates a deadlock if an adapter merely waits for PASS.
6. crates/store/src/worker.rs exposes Store::open_with_options(path, StoreOpenOptions) and StoreHandle::create_client(login). crates/store/src/encryption.rs consumes 32-byte StoreKey; the store does not provision or locate keys.
7. crates/sam/src/provider.rs supplies production SamProvider. Router Corrective 033 proved bidirectional application bytes with real i2pd 2.61.0 and scoped SAM session reuse; no second cross-router matrix is justified here.
8. scripts/check-network-boundary.py scans enumerated crates and refuses generic socket, DNS and DCC primitives. New local-only listener source requires a narrow allowlist plus negative controls, not disabling this guard.
9. Workspace MSRV is 1.88; std::fs::File lock/try_lock stabilized only in 1.89. Instance locking requires a reviewed 1.88-compatible primitive.

## External primary references

- IRCv3 capability negotiation: https://ircv3.net/specs/extensions/capability-negotiation.html
- IRCv3 SASL 3.2: https://ircv3.net/specs/extensions/sasl-3.2
- RFC 2812 registration/PASS ordering: https://www.rfc-editor.org/rfc/rfc2812
- Tokio graceful shutdown: https://tokio.rs/tokio/topics/shutdown
- Tokio TCP and Unix listener docs: https://docs.rs/tokio/latest/tokio/net/index.html
- Rust file locking stabilization (1.89): https://doc.rust-lang.org/std/fs/struct.File.html
- Existing repo evidence: plans/closure/bouncer-core/049-status.md; plans/closure/router-integration/033-status.md; architecture/control-session.md; architecture/storage.md; architecture/security-anonymity.md.

External sources are protocol and API references, not code donors. Independently implement all application interfaces; do not copy third-party implementation or assume crate license/MSRV compatibility without a dependency review.

## Findings and decisions

### R010-F1 — The local accept boundary is privileged

Every accepted ClientId is treated as the one local Operator, including BouncerServ and configuration authority. Loopback does not authenticate peers. Production TCP MUST authenticate before creating/resolving ClientId or constructing a trusted admission object. Unix transport is not automatically a bypass; same credential requirement initially, with OS permissions as defense in depth. This is single-operator multi-client, not multi-user hosting.

### R010-F2 — CAP ordering and authentication cannot be bolted on by re-reading bytes

An IRC client may send CAP LS before PASS; SASL clients commonly send NICK and USER before AUTHENTICATE. A listener waiting for PASS as its first line deadlocks some clients. An adapter that answers CAP and later forwards those bytes through existing SessionReader produces duplicate protocol replies; a naive replay also changes timing, ownership and negotiated capabilities. Design a single registration state machine or a lossless typed handoff containing negotiated CAP state, registration facts, unread bytes and writer ownership. Only one component may answer any individual command.

### R010-F3 — Credentials, client lineage and store keys are different identities

Auth establishes the Operator; named client profiles determine durable ClientId/cursors; SessionId remains ephemeral. StoreKey is process-level database encryption and is not derived from or rotated with IRC login credentials. First release may support OS-generated high-entropy bearer secrets rather than user-selected low-entropy passwords; constant-time comparison and private credential storage still apply. Human passwords, if introduced, need a proper slow password verifier and explicit memory/concurrency accounting.

### R010-F4 — Actual daemon ownership is missing

Controller.startup restore, Store shutdown, and SamProvider resource release exist at library level. A daemon must own boot ordering, state directory locking, signals, listener cancellation, authentication task joins, controller stop/join, store flush/shutdown, and exit classification. A duplicate daemon against the same state directory must fail before starting supervisors; SQLite locking alone is insufficient.

### R010-F5 — Local-only firewall of the codebase must survive

SAM's TCP authority is allowlisted for numeric loopback connects; its tests use a test-only loopback listener. A new executable can bind a numeric loopback address and optional Unix socket, but must not gain outgoing TCP, DNS, HTTP, proxy, DCC or non-loopback bind authority. Add explicit checks on bound addresses and guard-fixture positive controls.

### R010-F6 — Evidence scope must be product-specific

Deterministic tests are primary. M010 qualification should prove one real IRC client through the production local listener, auth, admission, RuntimeController, SamProvider, and a real i2pd router to an I2P IRC service or controlled independent stream endpoint where feasible. A mock IRC server behind the I2P stream can test bouncer protocol behavior; it is not proof of cross-router SAM portability. Do not duplicate R001 or the dedicated i2pr-sam project's matrix. Real-client OTRv3 qualification is desirable only where an appropriate independently configured client is available and must be reported separately if not run.

## Alternatives rejected

- In-process test acceptor as production adapter: no authentication, service boot, or bounded pre-auth accounting.
- Bind all interfaces and rely on a firewall: violates the local-only contract.
- Generic reverse proxy/HTTP control surface: unnecessarily broadens egress and privilege exposure.
- Waiting for PASS as the first frame: breaks CAP-first clients.
- Terminating downstream SASL in the existing upstream SASL logic: conflates different trust and credential scopes.
- Deriving SQLCipher key from login password: violates ADR-0006 separation.
- Repeating Java/i2pd/i2pr SAM matrices: verifies a temporary client rather than the M010 daemon.
- Depending on i2pr managed-app R002: incorrectly blocks a standalone product that should work independently.

## Planned result and open implementation checks

ADR-0007 freezes the auth/registration boundary; standalone subsystem roadmap separates executable, local listener, session integration, provisioning and end-to-end qualification (050-054).

Pre-implementation implementers must specifically inspect whether the current SessionReader registration handling can be factored into a shared state object without letting unauthenticated traffic enter operator control. Do not claim that PASS/SASL are currently supported downstream. The exact low-level encoding of a registration checkpoint and the precise CLI format are implementation details, but any alternate mechanism must prove no double reply, no dropped byte and no privilege crossover.

No tests were run as part of this planning-only research. The source findings are an audit of the referenced baseline; no production implementation or release claim is made.
