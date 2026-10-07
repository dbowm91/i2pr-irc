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
| Bouncer core | M005 closed | plans/subsystems/bouncer-core-roadmap.md | Plans 020-028 closed; M005 complete | M004, Corrective 019, and Plans 020-025 are closed. Research 006 and ADR-0003 freeze M005 control-session/runtime ownership. Plan 020 landed the bounded RuntimeController, pre-bind DownstreamAdmission, one-shot PreparedSession transfer, and schema 3. Plan 021 landed the typed DesiredChannelRecord, schema 4's durable detached flag, and the detach/reattach transitions. Plan 022 landed schema 5's auto_away and keep_nick policy, per-session active/passive classification with draft/pre-away mediation, owner-scoped manual away, a bounded deterministic nick fallback answered inside the registration window, and generation-owned reclaim. Plan 023 landed the soju.im/bouncer-networks draft, the local BouncerServ administration service, controller-allocated netids, a single-sourced I2P attribute profile, and snapshot-derived notification deltas. Plan 024 landed the soju.im/search adapter, schema 6's FTS5 side index and effective_time rule, indexed msgid and timestamp reference lookups, the HistoryPosition model for out-of-window references, and a two-seek AROUND. Plan 025 promoted server-time, standard-replies, cap-notify and draft/no-implicit-names, made echo-message conditional on the upstream negotiation, gave the tag surface and the refusal format a per-session third and dual form, and replaced the static capability list with one advertisement shared by the owner and the reader. Plan 026 accepted extended-join, account-notify, away-notify, multi-prefix and setname as a set whose downstream advertisement is conditional on the upstream acknowledgement, recorded account-tag, chghost, invite-notify and extended-monitor as deliberately deferred with stated reasons, added bounded observed member metadata with a three-state account model, mediated extended JOINs and prefix runs per session across NAMES and routed WHO and WHOIS, and made registration read the Network's live advertisement. Plan 027 landed the bounded secret-free diagnostics surface read from the live owners, the versioned local configuration snapshot format with plan-then-apply import and a stored-credential merge, and the bounded allowlisted registration actions with schema 7 and a replay runner that emits them after every successful generation; it also recorded a pre-existing finding that a generation teardown takes about 120 s to be noticed, which Plan 028 resolved. Plan 028 qualified M005 as one integrated product and **closed the milestone**. The integrated pass found three production defects that eight per-subsystem suites had each correctly passed over: the connect rate limiter could hang, because `ReconnectScheduler::acquire` parked on a notification while the token gate frees on a clock that notifies nothing, so every Network past `MAX_CONNECT_BURST` could stay unconnected forever on a cold start; `ControlSnapshot` answered from memory, because `publish()` ran only from `commit()`, so a Network with two live sessions reported `attached=0 phase=idle` until an unrelated edit happened; and `registration_actions`, `clients` and `network_secrets` were missing from `REQUIRED_TABLES`, so a database declaring the current version without them opened successfully and failed later. Plan 028 also **withdrew** Plan 027's teardown finding as a fixture defect -- `drop_generation` silently matched nothing, so the test measured `LIVENESS_DEADLINE` rather than the bouncer, which ends a generation on end-of-stream immediately. |
| I2P router integration | planning-ready | plans/subsystems/i2p-router-integration-roadmap.md | R001 portable SAM adapter | M005 is closed and the R001 hard dependency is discharged. The router roadmap now marks R001 planning-ready; no bounded R001 implementation handoff exists yet. R002 still waits on stable public i2pr app I2P-stream/local-listener/lifecycle contracts. R003 still requires a concrete product need plus stable scoped control semantics. |

## Active and dependency-ready implementation plans

**None.** No registered implementation plan is dependency-ready.

Bouncer core has no open plan: all 28 plans under `plans/implementation/bouncer-core/` carry `Status: closed`, and both M004 and M005 are complete. The only dependency-eligible unit is router **R001**, but it exists solely as roadmap prose in `plans/subsystems/i2p-router-integration-roadmap.md` §7 — there is no `plans/implementation/router-integration/` directory and no R001 plan file. Authoring that plan is the next planning step; the milestone is not yet a handoff artifact and must not be started from the roadmap alone.

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
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

**None.** No registered implementation plan carries a named hard or interface dependency.

The router milestones are gated on external conditions rather than on any registered plan, and are tracked in their own roadmap: R002 waits on stable public i2pr app contracts for I2P streams, local accepted streams, and lifecycle; R003 requires a concrete product need plus stable scoped Proposal 170 semantics. See plans/subsystems/i2p-router-integration-roadmap.md.

## Unplanned later milestones

M004 is fully closed, Plans 020-028 are closed, and M005 is complete. No M005 plan remains open.

The later router milestones remain intentionally outside this handoff:

- Router R001 — portable SAM adapter/cross-router qualification, eligible under its own prerequisites now that M005 is closed;
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

**Implement nothing from `plans/implementation/bouncer-core/`.** Every registered bouncer-core plan is closed and both milestones are complete.

Closure state:

- Plans 020-026 closed with no open findings; Plans 027 and 028 closed with the findings they recorded resolved, withdrawn, or explicitly stated as bounded. Closure records are at `plans/closure/bouncer-core/{020..028}-status.md`.
- M005 closure is recorded at `plans/closure/bouncer-core/028-status.md`. That record found and fixed three production defects the per-subsystem suites had missed and withdrew the finding Plan 027 carried in.
- No unresolved high-severity finding remains. Two low-severity boundaries are stated rather than fixed, and both are permitted: a whole-process `DIAG` reports at most two Networks (a consequence of the bounded control queue, and `DIAG NETWORK` always delivers one Network in full), and configuration import is not transactional across Networks (nothing is written until the whole snapshot validates).

The next unit of work is **planning, not implementation**: author the R001 portable SAM adapter plan under `plans/implementation/router-integration/`. No such plan exists yet. The R001 milestone in `plans/subsystems/i2p-router-integration-roadmap.md` §7 is roadmap prose, not a handoff artifact — `plans/003-planning-process.md` requires a bounded plan carrying readiness and dependencies, current evidence, invariants, scope, ordered work packages, failure and restart semantics, compatibility, tests, verification, documentation, acceptance criteria, stop conditions, and closure evidence before any of it is executed.

Do not begin R001 implementation from the long-term roadmap or from the R001 milestone prose alone. R001 must not add a generic upstream TCP or DNS connector, a clearnet fallback, or any route other than `I2pStreamProvider`; see `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`. R002 and R003 are authorized by nothing in this registry.
