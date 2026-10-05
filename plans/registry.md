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
| Bouncer core | active | plans/subsystems/bouncer-core-roadmap.md | M001 corrective pass required; M002 blocked | M001 closure found missing fault/time/property evidence. M002 remains blocked on M001 closure. M003-M005 are sequenced by the roadmap and do not yet have handoff plans. |
| I2P router integration | proposed / blocked | plans/subsystems/i2p-router-integration-roadmap.md | R001 blocked | Canonical ordering requires bouncer-core M005 before portable SAM implementation. R002 additionally waits on stable public i2pr app I2P-stream/local-listener/lifecycle contracts. R003 requires a concrete product need plus stable scoped control semantics. |

## Dependency-ready implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
| Bouncer Core M001 — Protocol, Domain, and Deterministic-Fault Foundation | corrective pass required | invariant + infrastructure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/001-status.md |

## Blocked implementation plans

| Plan | Status | Blocker | Handoff |
|---|---|---|---|
| Bouncer Core M002 — Single-Network Operational Bouncer | blocked | M001 is not evidence-closed; see plans/closure/bouncer-core/001-status.md | plans/implementation/bouncer-core/002-single-network-operational-bouncer.md |

## Unplanned later milestones

These have roadmap authority but intentionally do not yet have implementation handoffs:

- Bouncer Core M003 — durable multi-network/multi-client/history;
- Bouncer Core M004 — anonymity and adverse-network qualification;
- Bouncer Core M005 — mature operator feature set;
- Router R001 — portable SAM adapter/cross-router qualification;
- Router R002 — i2pr managed-app adapter;
- Router R003 — optional scoped Proposal 170/control integration.

Do not create implementation code for these merely from their roadmap descriptions. Research may continue, but implementation handoffs must be written/refreshed against the then-current repository state and dependency closures.

## Accepted architecture decisions

| ADR | Status | Decision |
|---|---|---|
| plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md | accepted | Upstream IRC authority is structurally I2P-only through I2pStreamProvider; SAM/i2pr are adapters; Proposal 170 is separate optional control plane. |

## Research authority

Current foundation research:

- plans/research/001-bouncer-and-i2p-foundation.md

Important retained conclusions:

- ZNC is a feature-envelope reference, not the target extension architecture.
- soju's persistent multi-network/multi-client/history model is the closer conceptual bouncer reference; implementation remains independent.
- IRCv3 labeled-response is foundational to later multi-client request routing.
- draft/chathistory and draft/read-marker remain draft-isolated wire adapters over internal durable history/cursor semantics.
- SAM 3.1 STREAM is the conservative first portable router target and should use long-lived session ownership.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Immediate handoff

The M001 attempt did not satisfy closure, and M002 remains blocked. The corrective evidence is recorded at:

plans/closure/bouncer-core/001-status.md

No later implementation plan is currently handoff-ready. Do not skip directly to SAM, SQLite, UI, or i2pr integration.
