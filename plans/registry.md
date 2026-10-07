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
| Bouncer core | maintenance corrective active | plans/subsystems/bouncer-core-roadmap.md | Corrective 034 ready; M005 remains closed | M004, Corrective 019, and Plans 020-025 are closed. Research 006 and ADR-0003 freeze M005 control-session/runtime ownership. Plan 020 landed the bounded RuntimeController, pre-bind DownstreamAdmission, one-shot PreparedSession transfer, and schema 3. Plan 021 landed the typed DesiredChannelRecord, schema 4's durable detached flag, and the detach/reattach transitions. Plan 022 landed schema 5's auto_away and keep_nick policy, per-session active/passive classification with draft/pre-away mediation, owner-scoped manual away, a bounded deterministic nick fallback answered inside the registration window, and generation-owned reclaim. Plan 023 landed the soju.im/bouncer-networks draft, the local BouncerServ administration service, controller-allocated netids, a single-sourced I2P attribute profile, and snapshot-derived notification deltas. Plan 024 landed the soju.im/search adapter, schema 6's FTS5 side index and effective_time rule, indexed msgid and timestamp reference lookups, the HistoryPosition model for out-of-window references, and a two-seek AROUND. Plan 025 promoted server-time, standard-replies, cap-notify and draft/no-implicit-names, made echo-message conditional on the upstream negotiation, gave the tag surface and the refusal format a per-session third and dual form, and replaced the static capability list with one advertisement shared by the owner and the reader. Plan 026 accepted extended-join, account-notify, away-notify, multi-prefix and setname as a set whose downstream advertisement is conditional on the upstream acknowledgement, recorded account-tag, chghost, invite-notify and extended-monitor as deliberately deferred with stated reasons, added bounded observed member metadata with a three-state account model, mediated extended JOINs and prefix runs per session across NAMES and routed WHO and WHOIS, and made registration read the Network's live advertisement. Plan 027 landed the bounded secret-free diagnostics surface read from the live owners, the versioned local configuration snapshot format with plan-then-apply import and a stored-credential merge, and the bounded allowlisted registration actions with schema 7 and a replay runner that emits them after every successful generation; it also recorded a pre-existing finding that a generation teardown takes about 120 s to be noticed, which Plan 028 resolved. Plan 028 qualified M005 as one integrated product and **closed the milestone**. The integrated pass found three production defects that eight per-subsystem suites had each correctly passed over: the connect rate limiter could hang, because `ReconnectScheduler::acquire` parked on a notification while the token gate frees on a clock that notifies nothing, so every Network past `MAX_CONNECT_BURST` could stay unconnected forever on a cold start; `ControlSnapshot` answered from memory, because `publish()` ran only from `commit()`, so a Network with two live sessions reported `attached=0 phase=idle` until an unrelated edit happened; and `registration_actions`, `clients` and `network_secrets` were missing from `REQUIRED_TABLES`, so a database declaring the current version without them opened successfully and failed later. Plan 028 also **withdrew** Plan 027's teardown finding as a fixture defect -- `drop_generation` silently matched nothing, so the test measured `LIVENESS_DEADLINE` rather than the bouncer, which ends a generation on end-of-stream immediately. |
| I2P router integration | R001 conditionally closed | plans/subsystems/i2p-router-integration-roadmap.md | Corrective 033 closed | Corrective 033 closed on 2026-10-07 after a full live pass against i2pd 2.61.0: exact bidirectional application bytes crossed an I2P stream between an independently implemented accepting peer and the production SamProvider, on one provider instance, with one session creation serving both streams and an explicit release leaving zero scope. Plan 032's "no application bytes" finding is **superseded** — its peer never issued STREAM ACCEPT, so it measured a socket arrangement SAM does not define. The corrective also found and fixed a real production defect: the owned client's HELLO classifier rejected the specification's canonical `HELLO REPLY RESULT=OK VERSION=3.1`, so every connect to i2pd failed at the handshake; the scripted bridge answered the Java form and agreed with the bug. R001 is now conditional solely on portability evidence: Java I2P and i2pr are NOT RUN, and cross-router interoperability is not attempted. R002 remains blocked on its own managed-app interface prerequisites plus stable public i2pr app I2P-stream/local-listener/lifecycle contracts; R003 remains research-blocked. |

## Active and dependency-ready implementation plans

| Plan | Status | Class | Source | Closure/result |
|---|---|---|---|---|
| Bouncer Core Corrective 034 — Restore Rust 1.88 Repository Verification | ready | verification + maintenance corrective | plans/subsystems/bouncer-core-roadmap.md | future plans/closure/bouncer-core/034-status.md |

## Recently closed implementation plans

| Plan | Status | Class | Source roadmap | Closure |
|---|---|---|---|---|
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
| Router R002 — i2pr Managed-App Adapter | blocked | its own managed-app interface prerequisites, plus stable public i2pr app I2P-stream/local-listener/lifecycle contracts | no implementation handoff yet |

## Unplanned later milestones

R001 is conditionally closed on corrected evidence. Later router milestones remain outside
the current handoff:

- Router R002 — i2pr managed-app adapter, blocked on its own managed-app interface prerequisites plus stable public app stream/listener/lifecycle contracts. It may be authored on the corrected R001 basis: the SAM adapter's ability to carry application bytes is established for i2pd 2.61.0 rather than assumed from an invalid run. It must not treat one router as sufficient for a managed-app data-transfer claim;
- Router R003 — optional scoped Proposal 170/control integration, research-blocked until a concrete product need exists.

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

Important retained conclusions:

- ZNC is a feature-envelope reference, not the target extension architecture.
- soju's persistent multi-network/multi-client/history model is the closer conceptual bouncer reference; implementation remains independent.
- IRCv3 labeled-response is foundational to later multi-client request routing.
- draft/chathistory and draft/read-marker remain draft-isolated wire adapters over internal durable history/cursor semantics.
- SAM 3.1 STREAM is the conservative first portable router target.
- R001 uses a small owned SAM 3.1 client while the standalone SAM library matures; third-party SAM crates are conformance/test references, not production dependencies.
- Provider scope is per durable Network by default so unrelated IRC Networks do not silently share one I2P Destination; transient SAM identity survives IRC reconnects but not provider/router-session recreation or process restart.
- Proposal 170 is not required for the IRC data path.
- i2pr managed-app integration waits for public app-scoped I2P stream and local accepted-stream/listener capabilities; it must not import private router internals.

## Immediate handoff

Implement only:

- plans/implementation/bouncer-core/034-rust-1-88-verification-corrective.md

Corrective 034 restores the repository-wide Rust 1.88 verification floor recorded as a pre-existing gap by Corrective 033. The expected work is behavior-neutral source/test cleanup, beginning with crates/core/src/lib.rs and continuing only if the exact full Rust 1.88 run exposes additional diagnostics.

Do not weaken Clippy, remove -D warnings, change scripts/verify.sh to skip checks, raise the MSRV, or reopen SAM/R001 behavior merely to make the command green.

R001 remains conditionally closed only on Java I2P/i2pr/cross-router evidence. Those are evidence gaps, not Corrective 034 work.

R002 remains blocked on its managed-app/public i2pr interface prerequisites. R003 remains research-blocked.
