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
| Bouncer core | active pre-M003 gate | plans/subsystems/bouncer-core-roadmap.md | Research 002 ready | M001, M002, Corrective 004, and Corrective 005 are closed. Corrective 005 closed the desired-vs-observed JOIN and downstream CAP registration defects. Research 002 must record the Rust IRC crate/spec conformance disposition; M003 is blocked on it. |
| I2P router integration | proposed / blocked | plans/subsystems/i2p-router-integration-roadmap.md | R001 blocked | Canonical ordering requires bouncer-core M005 before portable SAM implementation. R002 additionally waits on stable public i2pr app I2P-stream/local-listener/lifecycle contracts. R003 requires a concrete product need plus stable scoped control semantics. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Source | Closure/result |
|---|---|---|---|---|
| Research 002 — Rust IRC Crate Conformance and Reuse Decision | ready for research | research/decision gate | plans/research/002-rust-irc-crate-conformance-plan.md | future plans/research/003-rust-irc-crate-conformance-results.md |

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
| Bouncer Core Corrective 005 — Pre-M003 Observed Membership and Downstream CAP Correctness | closed | invariant + protocol correctness corrective | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/005-status.md |
| Bouncer Core Corrective 004 — M002 Persistent Upstream Lifecycle and State Fidelity | closed | invariant + capability corrective | plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md | plans/closure/bouncer-core/004-status.md |
| Bouncer Core M002 — Single-Network Operational Bouncer | closed | capability | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/002-status.md |
| Bouncer Core Corrective 003 — M001 Wire, Time, and Fault Qualification | closed | invariant + infrastructure | plans/subsystems/bouncer-core-roadmap.md | plans/closure/bouncer-core/003-status.md |

## Blocked implementation plans

| Plan | Status | Blocker | Handoff |
|---|---|---|---|
| Bouncer Core M003 — durable multi-network/multi-client/history | blocked | Research 002 disposition | Corrective 005 is closed; do not write/activate the M003 implementation handoff until the Research 002 gate completes without an unresolved M003-affecting correctness defect |

## Unplanned later milestones

These have roadmap authority but intentionally do not yet have implementation handoffs. M003 is blocked only by the Research 002 conformance/dependency gate; its implementation handoff must be written against the Corrective-005 closure in `plans/closure/bouncer-core/005-status.md` and the Research-003 result baseline.

- Bouncer Core M003 — durable multi-network/multi-client/history (blocked on Research 002);
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

Corrective 005 is closed in `plans/closure/bouncer-core/005-status.md`: server-confirmed observed channel membership, bounded join-failure disposition, downstream CAP registration gating, and complete RPL_NAMREPLY visibility handling are implemented with regression evidence.

One pre-M003 gate remains ready:

1. `plans/research/002-rust-irc-crate-conformance-plan.md`

Research 002 must compare the resulting owned wire/state/CAP behavior against primary specifications and the current `ircv3_parse`, `irc-proto`, `vinezombie`, and `obby-proto` families, then record a production/dev-only/reference/excluded disposition in `plans/research/003-rust-irc-crate-conformance-results.md`.

M003 remains blocked until that gate completes without an unresolved M003-affecting correctness defect. Router integration remains blocked behind M005.
