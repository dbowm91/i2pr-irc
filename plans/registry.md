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
| Bouncer core | active | plans/subsystems/bouncer-core-roadmap.md | M005-E / Plan 024 ready; M005-A through M005-D closed | M004, Corrective 019, and Plans 020-023 are closed. Research 006 and ADR-0003 freeze M005 control-session/runtime ownership. Plan 020 landed the bounded RuntimeController, pre-bind DownstreamAdmission, one-shot PreparedSession transfer, and schema 3. Plan 021 landed the typed DesiredChannelRecord, schema 4's durable detached flag, and the detach/reattach transitions. Plan 022 landed schema 5's auto_away and keep_nick policy, per-session active/passive classification with draft/pre-away mediation, owner-scoped manual away, a bounded deterministic nick fallback answered inside the registration window, and generation-owned reclaim. Plan 023 landed the soju.im/bouncer-networks draft, the local BouncerServ administration service, controller-allocated netids, a single-sourced I2P attribute profile, and snapshot-derived notification deltas. Plan 024 is the only dependency-ready M005 handoff; Plans 025-028 remain behind sequential closure gates. |
| I2P router integration | proposed / blocked | plans/subsystems/i2p-router-integration-roadmap.md | R001 blocked | Canonical ordering requires bouncer-core M005 before portable SAM implementation. R002 additionally waits on stable public i2pr app I2P-stream/local-listener/lifecycle contracts. R003 requires a concrete product need plus stable scoped control semantics. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Source | Closure/result |
|---|---|---|---|---|
| Bouncer Core M005-E / Plan 024 — Indexed History Search and CHATHISTORY Completion | ready | capability + invariant | plans/subsystems/bouncer-core-roadmap.md | |

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
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

| Plan | Status | Blocker |
|---|---|---|
| Bouncer Core M005-F / Plan 025 — Downstream IRCv3 Protocol Polish | blocked | Plan 024 closure |
| Bouncer Core M005-G / Plan 026 — Richer IRCv3 Member-State Mediation | blocked | Plan 025 closure |
| Bouncer Core M005-H / Plan 027 — Operator Diagnostics, Configuration Snapshots, and Constrained Registration Actions | blocked | Plan 026 closure |
| Bouncer Core M005-I / Plan 028 — Integrated Mature-Bouncer Qualification and M005 Closure | blocked | Plan 027 closure |

Router integration remains blocked on M005 closure; see the subsystem table and plans/subsystems/i2p-router-integration-roadmap.md.

## Unplanned later milestones

M004 is fully closed, Plans 020-023 closed M005-A through M005-D with no open findings, and M005 is fully decomposed into registered Plans 020-028. Only Plan 024 is ready.

The later router milestones remain intentionally outside this handoff:

- Router R001 — portable SAM adapter/cross-router qualification, blocked on M005 closure;
- Router R002 — i2pr managed-app adapter;
- Router R003 — optional scoped Proposal 170/control integration.

Do not skip a registered M005 dependency gate or begin router implementation merely from the long-term roadmap.

## Accepted architecture decisions

| ADR | Status | Decision |
|---|---|---|
| plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md | accepted | Upstream IRC authority is structurally I2P-only through I2pStreamProvider; SAM/i2pr are adapters; Proposal 170 is separate optional control plane. |
| plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md | accepted | M003 uses an owned bounded rusqlite worker; durable DesiredState/history/cursors remain separate from live ObservedState; HistoryEventId is canonical order; SessionId is ephemeral and distinct from durable ClientId. |
| plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md | accepted | M005 adds a bounded process RuntimeController and pre-bind DownstreamAdmission; a selected session transfers exactly once into the existing NetworkOwner, which remains the bound data-path owner. |

## Research authority

Current foundation research:

- plans/research/001-bouncer-and-i2p-foundation.md
- plans/research/004-m003-storage-multiclient-history-research.md
- plans/research/005-m004-anonymity-and-adverse-network-research.md
- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Important retained conclusions:

- ZNC is a feature-envelope reference, not the target extension architecture.
- soju's persistent multi-network/multi-client/history model is the closer conceptual bouncer reference; implementation remains independent.
- IRCv3 labeled-response is foundational to later multi-client request routing.
- draft/chathistory and draft/read-marker remain draft-isolated wire adapters over internal durable history/cursor semantics.
- SAM 3.1 STREAM is the conservative first portable router target and should use long-lived session ownership.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Immediate handoff

Implement only:

- plans/implementation/bouncer-core/020-m005a-runtime-control-and-downstream-admission.md
- plans/implementation/bouncer-core/021-m005b-durable-detached-channel-policy.md

Research 006 and ADR-0003 freeze the control-session/runtime ownership needed by M005. Plan 020 adds process control and pre-bind admission without moving bound-session or upstream ownership out of NetworkOwner. Plan 021 adds a durable presentation policy for channels the bouncer still holds, without moving membership ownership out of NetworkOwner. Plan 022 adds durable presence and preferred-nick policy, per-session presence classification, and generation-owned reclaim, without moving membership ownership out of NetworkOwner. Plan 023 adds a bouncer control plane — an interop draft and a local administration service — that submits every mutation as a typed controller request, without granting any session a store, supervisor, or owner handle.

Plans 020-023 are closed at plans/closure/bouncer-core/{020,021,022,023}-status.md, all with no open findings; between them they landed the bounded RuntimeController, the pre-bind DownstreamAdmission, the one-shot PreparedSession transfer, schema 3, the typed DesiredChannelRecord, schema 4's durable detached flag, the detach/reattach transitions, schema 5's auto_away and keep_nick policy, per-session active/passive classification with draft/pre-away mediation, owner-scoped manual away, a bounded deterministic nick fallback answered inside the registration window, generation-owned reclaim, the soju.im/bouncer-networks draft, the local BouncerServ service, controller-allocated netids, and snapshot-derived notification deltas. Plan 024 is unblocked and dependency-ready. Plans 025-028 remain registered behind their sequential closure gates. Router integration remains blocked behind M005 closure, and no router-specific implementation is authorized by the M004 closure, the M005 planning handoff, or the Plan 020-023 closures.
