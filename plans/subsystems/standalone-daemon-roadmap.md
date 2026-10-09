# Standalone Daemon and Local Access Roadmap

Status: M010 in progress; M010-A/Plan 050 through M010-D/Plan 053 closed; M010-E/Plan 054 active with controlled live-service evidence blocker
Canonical direction: plans/000-long-term-specification.md; plans/002-long-term-roadmap.md; plans/003-planning-process.md
Research: plans/research/010-m010-standalone-daemon-local-access-and-bootstrap.md
Decision authority: plans/adrs/ADR-0007-local-authentication-and-standalone-process-boundary.md, ADR-0001, ADR-0003, ADR-0006
Predecessors: M001-M009, Corrective 049 and Router R001 closed on main at f325e7d5e495e36b1fc7b168c30e4b725756d22c

## 1. Purpose and ownership

Turn the existing library-first I2P-only IRC bouncer into a real local service usable by an ordinary IRC client. The new daemon owns process and local-service boundaries; it must not duplicate IRC NetworkOwner semantics, durable store implementation, IRC parser or SAM internals.

Boundaries:
- daemon: config, CLI, process lease, local listeners, Operator authentication, client-profile resolution, startup and coordinated shutdown;
- existing runtime: typed IRC admission after trust, network lifecycle, owner, administration and IRC/IRCv3 mediation;
- existing store: durable Network/client/history state with explicit injected optional SQLCipher key;
- existing SAM adapter: loopback-only router-side I2P upstream connections;
- future managed i2pr adapter: later and independently blocked R002, not in M010.

## 2. Non-goals

No built-in GUI/IRC chat client; no web API, generic HTTP client/server, remote listener, multi-user roles/hosting, clearnet IRC, SOCKS, DCC, Proposal 170 control, i2pr private internals, package publication, system service installer or broad SAM portability work. No mandatory OTR implementation in the bouncer; OTR remains client endpoint crypto.

## 3. Current evidence and gaps

RuntimeController::serve restores durable Networks and supervision, with request_stop() separate from its bounded request queue. LocalAcceptor and DownstreamAdmission require already-trusted ClientId. An in-repo SAM 3.1 provider is product-qualified against i2pd 2.61.0. Store::open_with_options takes an explicit encryption key. There is no production binary, socket listener, Operator authentication, state-directory lock, key-source UX or real-client product test. A static network guard currently forbids generic socket APIs in all production crates.

## 4. Cross-cutting invariants

- I2P-only upstream, no generic DNS/TCP/HTTP/proxy/DCC escape.
- Numeric loopback or explicitly secured Unix local listener only; a listening socket does not confer trust.
- No operator control, ClientId issuance, or BouncerServ before Operator authentication.
- CAP/PASS/SASL/NICK/USER and unread bytes cross registration/auth handoff without dropped or duplicate reply.
- NetworkId (durable) and SessionId (ephemeral) remain distinct; stable ClientId maps to authenticated client profile.
- All pre-auth sockets/tasks/lines/attempts and post-auth sessions are bounded.
- Data-store key, Operator secret, and upstream SASL credentials are separate; no silent plaintext fallback or replacement key.
- Exclusive process ownership of a data directory and graceful shutdown with no new client admits after stop.
- Live router evidence only for the product path; do not re-verify generic router pair connectivity.

## 5. M010 dependency graph

M009/C049 and R001 [closed]
             |
             v
Plan 050 / M010-A [closed] daemon scaffold, configuration, process lease/lifecycle
             |
             v
Plan 051 / M010-B [closed] auth-aware local acceptance + guard
             |
             v
Plan 052 / M010-C [closed] IRC registration checkpoint + stable profiles
             |
             v
Plan 053 / M010-D [closed] secure initialization, key/credential UX
             |
             v
Plan 054 / M010-E [active; controlled live IRC endpoint unavailable] production integration and closure

Acceptance of ADR-0007 and Research 010 satisfies the design gate. Subsequent plans become ready only after the preceding plan's closure, with concrete failure evidence reconciled. No future plan is allowed to claim active concurrently merely because it is documented.

## 6. Milestone breakdown

**M010-A / 050 — process foundation (infrastructure)**: dedicated executable and CLI contract, bounded config parse, numeric local-only address config, exclusive lifetime lock, wired Store/Controller/SamProvider, deterministic stop and fault tests. No listener and no standalone product claim yet.

**M010-B / 051 — authenticated listener substrate (security invariant)**: bounded TCP and optionally Unix acceptors, anonymous pre-auth CAP/PASS/SASL protocol limits, untrusted-to-trusted boundary, strict local-only static policy. Prototype handoff is private until Plan 052 completes no-replay admission.

**M010-C / 052 — real downstream binding (capability)**: typed registration checkpoint consumed once by existing downstream session machinery, stable authenticated client profile / ClientId mapping, chosen Network or unbound mode, no duplicate CAP or registration, conventional client compatibility.

**M010-D / 053 — secure state bootstrap (capability/security)**: init, operator-secret/key provisioning, encrypted default or explicit mode, protected storage, idempotent refusal to overwrite, recovery/rotation/backup guidance without introducing new key format.

**M010-E / 054 — integrated qualification (qualification/closure)**: full daemon -> auth -> runtime -> SAM -> real router path; restart persistence, failure bounds, privacy, MSRV, CLI docs and M010 closure evidence. OTR client interoperability recorded honestly when a suitable client is available.

## 7. Verification and evidence

Before each closure: focused regression tests; sh scripts/verify.sh full; rustup run 1.88.0 sh scripts/verify.sh full (or explicit environment blocker); platform-specific Unix/Windows path results; requirement-to-evidence table; no secret or host fingerprint leak; audited Cargo.lock and manifests for allowed sockets, crypto and file locking.

Admission tests include idle flood, simultaneous CAP-first clients, partial-read buffers, invalid/missing PASS/SASL, wrong socket family, oversized frames, stop under full queues, active-state replay prevention, and multi-client reconnect lineage. Live SAM tests target one real independent router such as i2pd, not Java/i2pr/mixed router matrix. A product qualification may use an independent IRC server endpoint over I2P to isolate IRC behavior; document exactly what it proves.

## 8. Risks / stop and corrective triggers

Registration auth complexity; partial CAP negotiations before Operator identity; transport-specific socket permission races; relative/symlink state paths; entropy/key loss; non-atomic init; MSRV incompatible file lock; process resource exhaustion; double daemon; inability to run platform tests.

If tests show incorrect auth state transfer or permission ambiguity, do not mark green and proceed. Register a numbered corrective, reference evidence, and reconcile status. Do not weaken static guard or scope to meet schedule.

## 9. Milestone table

| Milestone | Status | Plan | Closure expected |
|---|---|---|---|
| M010-A | closed | plans/implementation/standalone/050-m010a-daemon-runtime-bootstrap.md | plans/closure/standalone/050-status.md |
| M010-B | closed | plans/implementation/standalone/051-m010b-local-listener-and-authentication.md | plans/closure/standalone/051-status.md |
| M010-C | closed | plans/implementation/standalone/052-m010c-registration-handoff-client-profiles.md | plans/closure/standalone/052-status.md |
| M010-D | closed | plans/implementation/standalone/053-m010d-secure-init-and-key-provisioning.md | plans/closure/standalone/053-status.md |
| M010-E | active; operational evidence blocker | plans/implementation/standalone/054-m010e-product-integration-and-closure.md | plans/closure/standalone/054-status.md |

## 10. Completion definition

An authenticated conventional local IRC client can connect to the production executable, select or inherit an I2P IRC Network, send/receive IRC over a real local SAM router, disconnect and reconnect, and find the correct durable client identity/history after restart. A second process cannot take ownership of the same state directory, wrong credentials/keys fail safely, admission remains bounded under abuse, all production upstream behavior remains I2P-only, and the exact commands/evidence exist in Plan 054 closure.

M010 closes the **standalone product functionality** gate, not distribution/release packaging. Installation, system services, signed binaries and the i2pr managed-app adapter get separately planned future milestones.
