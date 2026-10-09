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
| Bouncer core | post-M009 complete, no active plan | plans/subsystems/bouncer-core-roadmap.md | No registered successor plan | M001-M009 remain historical product closures; Corrective 049 closed the post-M009 SAM framing defect and the registry/README/roadmap reconciliation. |
| Standalone daemon/local access | M010 in progress; 050-053 closed; 054 active with live-service evidence blocker | plans/subsystems/standalone-daemon-roadmap.md | M010-E / Plan 054 | M010 closure requires controlled live i2pd-to-IRC product-path evidence; no R002 dependency. |
| IRC privacy, resilience and authentication | M011 in progress; 055-056 closed, 057 ready | plans/subsystems/irc-privacy-resilience-roadmap.md | M011-C / Plan 057 | Plans 055-056 closure evidence is recorded; core work does not depend on managed-app R002. Optional TLS/EXTERNAL Plan 065 remains research-blocked; no clearnet override. |
| I2P router integration | R001 closed | plans/subsystems/i2p-router-integration-roadmap.md | R001 complete for this repository; R002 blocked upstream | Corrective 033 proved exact application-byte transport and SAM session reuse through the production SamProvider against i2pd 2.61.0. Broad SAM portability belongs to the dedicated SAM library project. R002 waits on stable public i2pr managed-app I2P-stream/local-listener/lifecycle contracts and its own prerequisites; R003 remains research-blocked. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Roadmap | Handoff |
|---|---|---|---|---|
| Standalone M010-E / Plan 054 — Standalone Product Integration and M010 Closure | active | qualification + milestone closure | plans/subsystems/standalone-daemon-roadmap.md | plans/implementation/standalone/054-m010e-product-integration-and-closure.md |
| IRC M011-C / Plan 057 — Local Watch and Highlight Notifications | active | capability + security invariant | plans/subsystems/irc-privacy-resilience-roadmap.md | plans/implementation/irc-enhancements/057-m011c-local-watch-and-highlight-notifications.md |

Plan 054 is active after evidence-based closure of Plan 053; final live controlled IRC over i2pd qualification remains operationally blocked. See its named blocker and do not claim M010 closure without product-path evidence.

Plans 050-053 are closed with evidence at `plans/closure/standalone/`. The daemon listener is enabled by the provisioned credentials and key; Plan 054 is active and gated on live product-path qualification.

## Registered IRC feature handoffs and milestone status

| Milestone | Plan | Status | Handoff |
|---|---|---|---|
| M011-A | 055 — per-buffer privacy, bounded ephemeral/no-history | closed | plans/implementation/irc-enhancements/055-m011a-buffer-retention-modes-and-schema.md |
| M011-B | 056 — activity-based detached policy | closed | plans/implementation/irc-enhancements/056-m011b-detached-channel-activity-policy.md |
| M011-C | 057 — local watch/highlight notifications | ready | plans/implementation/irc-enhancements/057-m011c-local-watch-and-highlight-notifications.md |
| M011-D | 058 — integrated privacy closure | proposed after 057 | plans/implementation/irc-enhancements/058-m011d-privacy-feature-integration-and-closure.md |
| M012-A | 059 — IRC command pacing and upstream outage gaps | proposed after M011 | plans/implementation/irc-enhancements/059-m012a-command-pacing-and-upstream-gap-ledger.md |
| M012-B | 060 — optional upstream CHATHISTORY recovery | proposed; live server support research-gated | plans/implementation/irc-enhancements/060-m012b-conditional-upstream-chathistory-reconciliation.md |
| M012-C | 061 — verified same-network I2P endpoint failover | proposed; equivalence research-gated | plans/implementation/irc-enhancements/061-m012c-verified-network-equivalent-i2p-failover.md |
| M012-D | 062 — integrated resilience closure | proposed after 059-061 | plans/implementation/irc-enhancements/062-m012d-resilience-integration-closure.md |
| M013-A | 063 — IRC2P/ILITA auth profiles, no TLS default | proposed after M012 | plans/implementation/irc-enhancements/063-m013a-authentication-profiles.md |
| M013-B | 064 — CHGHOST/playback/redaction review | proposed after 063 | plans/implementation/irc-enhancements/064-m013b-ircv3-feature-review.md |
| M013-C | 065 — optional inner TLS + SASL EXTERNAL | research-blocked until preflight | plans/implementation/irc-enhancements/065-m013c-i2p-tls-sasl-external.md |
| M013-D | 066 — compatibility and authentication closure | proposed after 063-065 disposition | plans/implementation/irc-enhancements/066-m013d-compatibility-closure.md |

Plans 055-056 are closed with evidence at `plans/closure/irc-enhancements/055-status.md` and `056-status.md`; Plan 057 is ready after fresh source/dependency review. Plan 058 remains gated on 057. Plan 054 is independently active and remains blocked on external controlled live IRC evidence. The default profiles are plain IRC over typed I2P streams: IRC2P typically NickServ without SASL; ILITA operator-configured required SASL PLAIN. Current deployed mechanisms must be verified by authorized live CAP evidence. Optional EXTERNAL needs explicit authenticated TLS-over-I2P and client certificate; no clearnet connector or user override is authorized.

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
| IRC M011-A / Plan 055 — Per-Buffer Retention and No-History | closed | privacy invariant + capability | plans/subsystems/irc-privacy-resilience-roadmap.md | plans/closure/irc-enhancements/055-status.md |
| Bouncer Core Corrective 049 — Post-M009 Verification and Documentation Reconciliation | closed | verification reliability + planning/documentation reconciliation | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/049-status.md |
| Bouncer Core M009-B / Plan 048 — Integrated OTR Privacy Qualification and M009 Closure | closed | privacy qualification + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/048-status.md |
| Bouncer Core Corrective 042 — Post-M007 MONITOR and Adverse-Qualification Corrective | closed | protocol correctness + qualification corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/042-status.md |
| Bouncer Core M008-C / Plan 046 — Encrypted Durable-State Qualification and M008 Closure | closed | security qualification + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/046-status.md |
| Bouncer Core M009-A / Plan 047 — OTRv3 Opaque-Carriage and Multi-Client Invariants | closed | privacy invariant + protocol compatibility | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/047-status.md |
| Bouncer Core M007-C / Plan 041 — Eggchaos Multi-Client Adverse Qualification and M007 Closure | closed | external qualification + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/041-status.md |
| Bouncer Core M007-B / Plan 040 — Preferred-Nick, Reconnect, and Multi-Client Identity Resilience | closed | identity state machine + resilience | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/040-status.md |
| Bouncer Core M006-B / Plan 037 — Account-Tag and Invite-Notify Mediation | closed | IRCv3 capability + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/037-status.md |
| Bouncer Core M006-A / Plan 036 — Registration Downgrade and Legacy-Server Baseline | closed | protocol compatibility + invariant | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/036-status.md |
| Bouncer Core Corrective 035 — MONITOR Numeric Conformance | historical closure | protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/035-status.md |

| Bouncer Core M007-A / Plan 039 — Phased Service Actions for Non-SASL Authentication and Recovery | closed | durable policy + security | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/039-status.md |
| Bouncer Core Corrective 034 — Restore Rust 1.88 Repository Verification | closed | verification + maintenance corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/034-status.md |
| Bouncer Core M006-C / Plan 038 — Integrated IRC Interoperability Qualification and M006 Closure | closed | qualification + milestone closure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/038-status.md |
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
| Router R002 — i2pr Managed-App Adapter | blocked | stable public i2pr managed-app I2P-stream/local-listener/lifecycle contracts + its own managed-app prerequisites | no implementation handoff yet |

## Unplanned later milestones

M008 and M009 are closed. No Bouncer Core implementation plan is currently active or dependency-ready.

M010 standalone daemon/local listener/bootstrap and basic secure key provisioning are registered (Plans 050-054). Plans 050-053 are closed; Plan 054 remains active pending its controlled live product-path evidence. Full release packaging, service installers, keyring/HSM integration and optional real-client OTR qualification beyond recorded available evidence remain later decisions.

Later product lines intentionally remain unplanned:

- installer/service/package publication and distribution hardening;
- OS keyring/HSM or other advanced store-key provisioning;
- optional further real-client OTR interoperability through the production listener;
- later refinement of M011 per-buffer history and privacy controls after registered Plans 055-058;
- built-in IRC client cryptographic endpoint support, including any OTRv4 evaluation;
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
| plans/adrs/ADR-0006-encryption-layering-store-key-and-otr-endpoint.md | accepted | Durable privacy uses optional whole-database SQLCipher with an injected process-level key; OTR remains endpoint-to-endpoint client crypto and the bouncer carries ciphertext opaquely without keys/session state. |
| plans/adrs/ADR-0007-local-authentication-and-standalone-process-boundary.md | accepted | The standalone daemon is an explicit local-only socket authority; Operator auth precedes trusted ClientId/admission; CAP/PASS/SASL registration state passes once; process/key ownership is independent of core/router state. |
| plans/adrs/ADR-0008-per-buffer-privacy-and-local-alert-policy.md | accepted for planning | Per-buffer privacy/retention and local-only bounded notifications; no persistence of no-history payloads. |
| plans/adrs/ADR-0009-i2p-irc-transport-authentication-profiles.md | accepted for planning | Plain IRC-over-I2P by default; explicit IRC2P NickServ and ILITA SASL PLAIN profiles; optional I2P-only TLS/EXTERNAL; no clearnet override. |

## Research authority

Current foundation research:

- plans/research/001-bouncer-and-i2p-foundation.md
- plans/research/004-m003-storage-multiclient-history-research.md
- plans/research/005-m004-anonymity-and-adverse-network-research.md
- plans/research/006-m005-mature-bouncer-and-control-session-research.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md
- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md
- plans/research/009-m008-m009-privacy-encryption-and-otr.md
- plans/research/010-m010-standalone-daemon-local-access-and-bootstrap.md
- plans/research/011-irc-privacy-resilience-authentication.md

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
- M008 selects optional whole-database SQLCipher so credentials, history and FTS terms are protected together; the Store consumes an injected key and does not own key-source policy.
- M009 keeps OTR truly endpoint-to-endpoint: i2pr-irc never holds OTR private keys/fingerprints/session state or plaintext, and only qualifies transparent OTRv3 carriage/non-replay/history behavior.
- OTRv4 is deferred to a future client/endpoint line; no native OTR library/unsafe FFI enters the bouncer.
- Provider scope is per durable Network by default so unrelated IRC Networks do not silently share one I2P Destination; transient SAM identity survives IRC reconnects but not provider/router-session recreation or process restart.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Latest closure and handoff

Corrective 049 is closed; see `plans/closure/bouncer-core/049-status.md`. It fixed a real SAM client framing defect (a reply terminator split across TCP reads was dropped, stalling the phase to its deadline) and reconciled the registry, README, bouncer roadmap, and long-term roadmap with the actual closed state through M009 and R001.

M010 is registered as a standalone-productization workstream after Research 010 and accepted ADR-0007. Plans 050-053 are closed; Plan 054 is the **sole active** handoff and awaits controlled live i2pd-to-IRC product-path qualification. M001-M009, C049 and R001 remain closed. R002 remains independently blocked on upstream i2pr managed-app contracts.


## Post-M010 IRC enhancement handoff

Research 011, ADR-0008, ADR-0009, and Plans 055-066 are committed on work/plans-055-066-irc-privacy-resilience. Plan 055 has an active partial implementation in `88cc10f`; ephemeral runtime retention and runtime/operator controls remain open, so no closure or successor readiness is claimed. Preserve original I2P-only network boundary and the independently active M010-E/054 status.
