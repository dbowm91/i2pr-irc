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
| Bouncer core | active | plans/subsystems/bouncer-core-roadmap.md | M005 planning eligible | Corrective 014 and M004-A/B/C/D are closed, so M004 is complete. UF-015-1 (a legacy duplicate network implementation) and UF-017-1 (an uninjectable reconnect backoff) are recorded and non-blocking. No implementation plan is registered yet; M005 requires planning/research before a handoff may be written. |
| I2P router integration | proposed / blocked | plans/subsystems/i2p-router-integration-roadmap.md | R001 blocked | Canonical ordering requires bouncer-core M005 before portable SAM implementation. R002 additionally waits on stable public i2pr app I2P-stream/local-listener/lifecycle contracts. R003 requires a concrete product need plus stable scoped control semantics. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Source | Closure/result |
|---|---|---|---|---|
| _None_ | — | — | — | Every registered implementation plan is closed or awaiting planning. |


## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
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

None. Every registered implementation plan is either closed or dependency-ready. Router
integration remains blocked, but on bouncer-core M005 rather than on any plan in this
table; see the subsystem table above and `plans/subsystems/i2p-router-integration-roadmap.md`.

## Unplanned later milestones

M004 is fully decomposed, registered and closed. Only later roadmap milestones remain
intentionally unplanned.

- Bouncer Core M005 — mature operator feature set;
- Router R001 — portable SAM adapter/cross-router qualification;
- Router R002 — i2pr managed-app adapter;
- Router R003 — optional scoped Proposal 170/control integration.

Do not create implementation code for these merely from their roadmap descriptions. Research may continue, but implementation handoffs must be written/refreshed against the then-current repository state and dependency closures.

## Accepted architecture decisions

| ADR | Status | Decision |
|---|---|---|
| plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md | accepted | Upstream IRC authority is structurally I2P-only through I2pStreamProvider; SAM/i2pr are adapters; Proposal 170 is separate optional control plane. |
| plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md | accepted | M003 uses an owned bounded rusqlite worker; durable DesiredState/history/cursors remain separate from live ObservedState; HistoryEventId is canonical order; SessionId is ephemeral and distinct from durable ClientId. |

## Research authority

Current foundation research:

- plans/research/001-bouncer-and-i2p-foundation.md
- plans/research/004-m003-storage-multiclient-history-research.md
- plans/research/005-m004-anonymity-and-adverse-network-research.md

Important retained conclusions:

- ZNC is a feature-envelope reference, not the target extension architecture.
- soju's persistent multi-network/multi-client/history model is the closer conceptual bouncer reference; implementation remains independent.
- IRCv3 labeled-response is foundational to later multi-client request routing.
- draft/chathistory and draft/read-marker remain draft-isolated wire adapters over internal durable history/cursor semantics.
- SAM 3.1 STREAM is the conservative first portable router target and should use long-lived session ownership.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Immediate handoff

No implementation plan is currently authorized. M004 is closed at
`plans/closure/bouncer-core/018-status.md`.

The next eligible work is M005 planning and research. A plan cannot be written until the
milestone is decomposed against the repository state that M004 closure produced; when one
exists it must be registered here before implementation begins.

Router integration remains blocked behind M005, and no router-specific implementation is
authorized by M004 closure alone.
