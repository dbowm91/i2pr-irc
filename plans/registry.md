# i2pr-irc Active Planning Registry

This file is the compact control surface for active planning. Detailed requirements and completed evidence live in the canonical documents, subsystem roadmaps, implementation plans, closure records, research records, and Git history.

Canonical direction:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md

## Status vocabulary

- proposed — roadmap or plan exists but is not approved/dependency-ready for execution.
- ready — dependencies/interfaces are satisfied and the plan may be handed off.
- active — implementation or closure work is in progress.
- blocked — a named hard/interface dependency prevents implementation.
- research-blocked — no implementation is justified until a named research/product condition exists.
- closing — implementation landed and closure evidence is being gathered.
- closed — closure record accepted.
- conditionally closed — production work landed but a named evidence condition remains.
- superseded — replaced by another planning artifact.
- archived — retained only for traceability.

## Active subsystem roadmaps

| Subsystem | Status | Roadmap | Current milestone | Dependencies or blockers |
|---|---|---|---|---|
| Bouncer core | active M006-A | plans/subsystems/bouncer-core-roadmap.md | Plan 036 ready | Corrective 035 closed the reversed IRCv3 MONITOR 730/731 semantics. Plans 036-038 close M006, followed by Plans 039-041 for M007. M005 and Corrective 034 remain closed. |
| I2P router integration | R001 closed | plans/subsystems/i2p-router-integration-roadmap.md | R001 complete for this repository; R002 blocked upstream | Corrective 033 proved exact application-byte transport and SAM session reuse through the production SamProvider against i2pd 2.61.0. Research 008 delegates broad Java/i2pd/i2pr and mixed-router SAM conformance to the dedicated SAM library project. R002 waits only on stable public i2pr managed-app I2P-stream/local-listener/lifecycle contracts and its own managed-app prerequisites; R003 remains research-blocked. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Source | Closure/result |
|---|---|---|---|---|
| Bouncer Core Corrective 035 — MONITOR Numeric Conformance | closed | protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/035-status.md |
| Bouncer Core M006-A / Plan 036 — Registration Downgrade and Legacy-Server Baseline | ready | protocol compatibility + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/035-status.md |

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
| Bouncer Core Corrective 034 — Restore Rust 1.88 Repository Verification | closed | verification + maintenance corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/034-status.md |
| Router Corrective 033 — Repair Live SAM STREAM Qualification | closed | qualification corrective | plans/subsystems/i2p-router-integration-roadmap.md | plans/closure/router-integration/033-status.md |
| Router R001-D / Plan 032 — SAM Cross-Router Qualification and R001 Closure | historical closure, superseded on Finding 1 by Corrective 033 | capability qualification | plans/subsystems/i2p-router-integration-roadmap.md | plans/closure/router-integration/032-status.md |
| Router R001-C / Plan 031 — Per-Network SAM Provider Integration | closed | invariant + capability | plans/subsystems/i2p-router-integration-roadmap.md | plans/closure/router-integration/031-status.md |
| Router R001-B / Plan 030 — Owned SAM 3.1 Wire/Client Foundation | closed | infrastructure + invariant | plans/subsystems/i2p-router-integration-roadmap.md | plans/closure/router-integration/030-status.md |
| Router R001-A / Plan 029 — Provider Scope, Lifecycle, and Endpoint Foundation | closed | invariant + infrastructure | plans/subsystems/i2p-router-integration-roadmap.md | plans/closure/router-integration/029-status.md |
| Bouncer Core M005-I / Plan 028 — Integrated Mature-Bouncer Qualification and M005 Closure | closed | invariant + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/028-status.md |
| Bouncer Core M005-H / Plan 027 — Operator Diagnostics, Configuration Snapshots, and Constrained Registration Actions | closed | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/027-status.md |
| Bouncer Core M005-G / Plan 026 — Richer IRCv3 Member-State Mediation | closed | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/026-status.md |
| Bouncer Core M005-F / Plan 025 — Downstream IRCv3 Protocol Polish | closed | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/025-status.md |
| Bouncer Core M005-E / Plan 024 — Indexed History Search and CHATHISTORY Completion | closed | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/024-status.md |
| Bouncer Core M005-D / Plan 023 — Bouncer Networks and Local IRC Administration | closed | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/023-status.md |
| Bouncer Core M005-C / Plan 022 — Presence and Preferred-Nick Policy | closed | invariant + capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/022-status.md |
| Bouncer Core M005-B / Plan 021 — Durable Detached-Channel Policy | closed | invariant + infrastructure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/021-status.md |
| Bouncer Core M005-A / Plan 020 — Runtime Control and Downstream Admission Foundation | closed | infrastructure + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/020-status.md |
| Bouncer Core Corrective 019 — Close M004 Findings and Restore Sole-Owner Evidence | closed | invariant + testability corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/019-status.md |
| Bouncer Core M004-D / Plan 018 — Integrated Anonymity Qualification and M004 Closure | closed | invariant qualification + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/018-status.md |
| Bouncer Core M004-C / Plan 017 — Adverse-Network and Resource Qualification | closed | invariant + resilience | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/017-status.md |
| Bouncer Core M004-B / Plan 016 — Global Reconnect Budget | closed | invariant + resilience | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/016-status.md |
| Bouncer Core M004-A / Plan 015 — Anonymity Protocol Mediation | closed | invariant + capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/015-status.md |
| Bouncer Core Corrective 014 — Live Multi-Client Response Routing | closed | invariant + protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/014-status.md |
| Bouncer Core Corrective 013 — Post-M003 IRCv3 Time/History and Queue-Integrity Conformance | closed | invariant + protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/013-status.md |
| Bouncer Core M003-F — Integrated Qualification and M003 Closure | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/012-status.md |
| Bouncer Core M003-E — IRCv3 Chathistory and Read-Marker Adapters | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/011-status.md |
| Bouncer Core M003-D — Response Routing and Foundational IRCv3 Mediation | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/010-status.md |
| Bouncer Core M003-C — History Journal, Cursors, and Legacy Playback | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/009-status.md |
| Bouncer Core M003-B — Multi-Network and Multi-Client Ownership | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/008-status.md |
| Bouncer Core M003-A — Durable Storage and Identity Foundation | closed | infrastructure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/007-status.md |
| Bouncer Core Corrective 006 — Framing Recovery and Casemapping Token Conformance | closed | invariant + protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/006-status.md |
| Research 002 — Rust IRC Crate Conformance and Reuse Decision | closed | research/decision gate | plans/research/002-rust-irc-crate-conformance-plan.md | plans/research/003-rust-irc-crate-conformance-results.md |
| Bouncer Core Corrective 005 — Pre-M003 Observed Membership and Downstream CAP Correctness | closed | invariant + protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/005-status.md |
| Bouncer Core Corrective 004 — M002 Persistent Upstream Lifecycle and State Fidelity | closed | invariant + capability corrective | plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md | plans/closure/bouncer-core/004-status.md |
| Bouncer Core M002 — Single-Network Operational Bouncer | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/002-status.md |
| Bouncer Core Corrective 003 — M001 Wire, Time, and Fault Qualification | closed | invariant + infrastructure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/003-status.md |

## Blocked implementation plans

| Plan | Status | Blocker | Handoff |
|---|---|---|---|
| Bouncer Core M006-B / Plan 037 — Account-Tag and Invite-Notify Mediation | blocked | Plan 036 closure | plans/implementation/bouncer-core/037-m006b-account-tag-and-invite-notify-mediation.md |
| Bouncer Core M006-C / Plan 038 — Integrated IRC Interoperability Qualification and M006 Closure | blocked | Plan 037 closure | plans/implementation/bouncer-core/038-m006c-integrated-irc-interoperability-qualification-and-closure.md |
| Bouncer Core M007-A / Plan 039 — Phased Service Actions for Non-SASL Authentication and Recovery | blocked | Plan 038 / M006 closure | plans/implementation/bouncer-core/039-m007a-phased-service-actions-for-nonsasl-authentication.md |
| Bouncer Core M007-B / Plan 040 — Preferred-Nick, Reconnect, and Multi-Client Identity Resilience | blocked | Plan 039 closure | plans/implementation/bouncer-core/040-m007b-preferred-nick-reconnect-and-multiclient-identity-resilience.md |
| Bouncer Core M007-C / Plan 041 — Eggchaos Multi-Client Adverse Qualification and M007 Closure | blocked | Plan 040 closure | plans/implementation/bouncer-core/041-m007c-eggchaos-multiclient-adverse-qualification-and-closure.md |
| Router R002 — i2pr Managed-App Adapter | blocked | stable public i2pr managed-app I2P-stream/local-listener/lifecycle contracts + its own managed-app prerequisites | no implementation handoff yet |

## Unplanned later milestones

M006 and M007 are fully planned and registered. Later product lines remain intentionally unplanned:

- privacy/encryption at rest: credential-vault and encrypted SQLite/history design;
- encrypted conversation research: OTR/E2EE endpoint placement, multi-client semantics, and history behavior;
- standalone daemon/listener/packaging work;
- Router R002 — i2pr managed-app adapter, blocked on stable public app stream/listener/lifecycle contracts;
- Router R003 — optional scoped Proposal 170/control integration, research-blocked until a concrete product need exists.

Broad SAM portability matrices are not an open milestone in this repository; the dedicated SAM library project owns that conformance work.

## Accepted architecture decisions

| ADR | Status | Decision |
|---|---|---|
| plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md | accepted | Upstream IRC authority is structurally I2P-only through I2pStreamProvider; SAM/i2pr are adapters; Proposal 170 is separate optional control plane. |
| plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md | accepted | M003 uses an owned bounded rusqlite worker; durable DesiredState/history/cursors remain separate from live ObservedState; HistoryEventId is canonical order; SessionId is ephemeral and distinct from durable ClientId. |
| plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md | accepted | M005 adds a bounded process RuntimeController and pre-bind DownstreamAdmission; a selected session transfers exactly once into the existing NetworkOwner, which remains the bound data-path owner. |
| plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md | accepted | R001 uses NetworkId-scoped provider semantics and one long-lived transient owned SAM 3.1 STREAM session per active Network; unrelated Networks do not share one I2P Destination by default. |
| plans/adrs/ADR-0005-explicit-i2p-provider-scope-release.md | accepted | I2pStreamProvider gains explicit idempotent NetworkId scope release so long-lived router sessions survive IRC reconnects but are torn down on durable Network deletion/process shutdown. |

## Research authority

Current foundation research:

- plans/research/001-bouncer-and-i2p-foundation.md
- plans/research/004-m003-storage-multiclient-history-research.md
- plans/research/005-m004-anonymity-and-adverse-network-research.md
- plans/research/006-m005-mature-bouncer-and-control-session-research.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md
- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Important retained conclusions:

- ZNC is a feature-envelope reference, not the target extension architecture.
- soju's persistent multi-network/multi-client/history model is the closer conceptual bouncer reference; implementation remains independent.
- IRCv3 labeled-response is foundational to later multi-client request routing.
- draft/chathistory and draft/read-marker remain draft-isolated wire adapters over internal durable history/cursor semantics.
- SAM 3.1 STREAM is the conservative first portable router target.
- R001 uses a small owned SAM 3.1 client while the standalone SAM library matures; third-party SAM crates are conformance/test references, not production dependencies.
- Corrective 033's real i2pd application-byte pass is sufficient R001 evidence for this repository; broad SAM portability belongs to the dedicated SAM library project.
- M006 treats no-CAP/no-SASL/plain IRC-over-I2P as first-class compatibility modes while keeping configured SASL fail-closed.
- M006 promotes account-tag and invite-notify without fabricating live state; chghost and extended-monitor remain explicitly deferred.
- M007 builds service authentication/recovery from constrained phased actions rather than NickServ prose parsing, and uses Eggchaos only as an external qualification substrate.
- Provider scope is per durable Network by default so unrelated IRC Networks do not silently share one I2P Destination; transient SAM identity survives IRC reconnects but not provider/router-session recreation or process restart.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Immediate handoff

Implement only:

- plans/implementation/bouncer-core/036-m006a-registration-downgrade-and-legacy-server-baseline.md

Corrective 035 is closed: 730 is online, 731 is offline/free, and 303 ISON remains the fallback probe. See `plans/closure/bouncer-core/035-status.md`.

Execute the remaining registered sequence strictly:

1. Plan 036 — legacy/no-CAP/no-SASL registration baseline.
2. Plan 037 — account-tag and invite-notify mediation.
3. Plan 038 — integrated M006 qualification/closure.
4. Plan 039 — phased service actions for non-SASL authentication/recovery.
5. Plan 040 — preferred-nick/reconnect/multi-client identity resilience.
6. Plan 041 — Eggchaos process/socket adverse qualification and M007 closure.

Do not parallelize Plans 036-040; they modify the same registration/generation/identity state machine.

R001 is closed for this repository on the existing real i2pd product-path evidence. R002 remains independently blocked on upstream i2pr managed-app contracts. Privacy/encryption work is intentionally a later separate line.
