# I2P Router Integration Roadmap

Status: proposed / blocked on bouncer-core M005

Long-term references:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/research/001-bouncer-and-i2p-foundation.md

Related ADRs:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md

## 1. Purpose and ownership boundary

This workstream connects the already-correct bouncer core to actual I2P routers.

It owns adapters below I2pStreamProvider, and later optional RouterControlProvider integrations.

It does not own IRC parsing/state/history/reconnect semantics.

Integration order is deliberately:

1. portable SAM;
2. i2pr managed-app capability adapter once public contracts stabilize;
3. optional narrowly scoped Proposal 170 integration only for concrete bouncer requirements.

## 2. Work classification

### Invariants

- router adapters cannot add a generic upstream clearnet connector;
- SAM sessions are router data-plane adapters, not application-wide socket authority;
- i2pr integration uses public app capabilities, never private router internals;
- Proposal 170 is optional control plane;
- a managed app never receives a general router administrator credential;
- router-specific errors map to typed provider failures without leaking private material.

### Infrastructure

- SAM transport/session owner;
- router capability adapters;
- cross-router test harness;
- i2pr app SDK adapter;
- optional scoped control adapter.

### Capabilities

- live I2P IRC via compatible SAM routers;
- managed first-party execution under i2pr;
- optional bounded router inspection/control if later justified.

### Polish

- router-specific diagnostics and migration ergonomics.

## 3. Non-goals

This workstream does not implement SAM/I2CP/Proposal 170 itself.

It does not fork the bouncer core by router.

It does not require Proposal 170 for IRC connectivity.

It does not use clearnet as a fallback when the router is unavailable.

## 4. Current state

No production adapter exists in i2pr-irc.

External/current evidence:

- official I2P documentation presents SAM as the normal non-Java application protocol;
- SAM 3.1 is the conservative portable baseline and newer feature coverage differs among routers;
- SAM STREAM provides the required ordered reliable data path;
- SAM sessions/tunnel pools should be long-lived rather than created per IRC reconnect;
- Proposal 170 remains an Open control-plane proposal;
- i2pr has a substantial SAM 3.1 implementation but its managed-app runtime/API is still evolving and the current app contract line is explicitly infrastructure-only.

The project therefore has enough written interface evidence to design I2pStreamProvider, but not enough stable i2pr app surface to write the native adapter now.

## 5. Target architecture

Standalone:

~~~
bouncer core
    |
I2pStreamProvider
    |
SAM adapter
    |
local SAM control/data connections
    |
Java I2P / i2pd / i2pr
~~~

Future managed i2pr:

~~~
bouncer core
   |               \
I2pStreamProvider   LocalAcceptor
   |                 |
i2pr app SDK / inherited capability channel
             |
       trusted app runtime
             |
         i2pr router
~~~

Optional later control:

~~~
bouncer operator feature
      |
narrow RouterControlProvider
      |
scoped app-authorized Proposal 170 adapter
~~~

## 6. Dependency graph

~~~
bouncer-core M005
     |
     v
R001 portable SAM adapter + interoperability
     |
     +----------------------+
     |                      |
     v                      v
R002 i2pr managed app       R003 optional scoped control research
    integration             (only if concrete need exists)
     |
     v
managed-app qualification/release
~~~

Dependency classes:

- R001 hard-depends on bouncer-core M005 by canonical phase order.
- R002 hard-depends on R001's provider semantics and interface-depends on stable written i2pr app contracts for I2P streams/local accepted streams/lifecycle.
- R003 has no automatic implementation eligibility; it hard-depends on a concrete product use case and interface-depends on a stable scoped i2pr/Proposal-170 contract.
- Cross-router live fixtures are operational dependencies for portability claims.

## 7. Milestones

### R001 — Portable SAM stream provider and cross-router qualification

Class: capability + integration

Objective:

Implement the conservative SAM 3.1 STREAM adapter as the first production I2pStreamProvider and prove bouncer behavior survives router/path restarts without changing core semantics.

Hard dependency:

- bouncer-core M005 closed.

Required behavior:

- local-router endpoint validation;
- HELLO negotiation;
- long-lived SAM STREAM session ownership;
- I2P naming/destination handling;
- outbound STREAM CONNECT;
- bounded SAM command/reply framing;
- provider restart/reconnect classification;
- no SAM-session recreation for every IRC reconnect;
- no generic DNS/clearnet fallback.

Interoperability target:

At least the routers available to the project among Java I2P, i2pd, and i2pr. A missing test environment is an operational evidence gap and must not be reported as successful portability.

Exit:

A real IRC session through SAM survives deterministic and live router interruptions with equivalent core recovery semantics.

### R002 — i2pr managed-app adapter

Class: integration capability

Objective:

Run the same bouncer core as a secured first-party i2pr managed app using only the public application API.

Hard dependencies:

- R001 provider semantics closed;
- stable i2pr application contracts exist for required capabilities.

Required i2pr interfaces before planning may become ready:

- app-scoped outbound I2P streams;
- naming resolution if not intrinsic to stream request;
- local accepted-stream/listener delivery suitable for local IRC clients;
- bounded app lifecycle/health/shutdown;
- stable app identity/config storage semantics needed by packaging.

Constraints:

- no direct import of private i2pr router crates;
- no UnsafeDirect merely to get a localhost listener;
- no generic brokered clearnet capability;
- no administrator Proposal 170 token.

Exit:

Standalone and managed-app adapters pass the same bouncer fault/conformance suite plus i2pr sandbox/capability qualification.

### R003 — Optional scoped Proposal 170/control integration

Class: research before capability

Objective:

Only after a concrete operator workflow requires router control, determine whether a small RouterControlProvider surface improves the product.

Examples that might justify research:

- bounded router/service health used for diagnostics;
- explicit addressbook operation not already available through the data-plane naming capability;
- app-owned service inspection/control.

Non-justifications:

- adding Proposal 170 merely for feature branding;
- obtaining arbitrary router configuration authority;
- using control-plane methods to replace SAM/app data-plane streams.

Dependencies:

- concrete user-visible requirement;
- Proposal 170 semantics relevant to that requirement are stable;
- i2pr exposes an app-scoped authorization model for those exact operations.

Exit:

Either no integration is needed, or a new bounded implementation plan/ADR freezes an operation-level least-privilege contract.

## 8. Cross-cutting requirements

### Security

- local SAM endpoints are explicitly configured and locally scoped by default;
- router credentials/private destinations never enter normal logs;
- adapter errors cannot trick the core into falling back to another transport;
- app sandbox/network guarantees remain truthful.

### Concurrency/recovery

- one adapter/session owner coordinates long-lived SAM session state;
- IRC reconnects do not stampede SAM session creation;
- router restart is distinguishable from IRC server refusal/auth failure;
- stale provider streams are fenced by ConnectionGeneration.

### Compatibility

- SAM 3.1 is the initial common baseline;
- router-specific enhancements are optional capabilities, not silent requirements;
- no claim of support for a router/version without live or authoritative fixture evidence.

### Observability

Expose bounded typed provider diagnostics: unavailable, negotiation failure, naming failure, stream connect failure, session lost, app-capability denied, lifecycle stop.

## 9. Verification strategy

R001:

- fake SAM protocol fixtures;
- malformed/oversized reply tests;
- local-endpoint policy negative tests;
- router restart/fault injection;
- live cross-router matrix where environments exist;
- no-DNS/no-clearnet static checks.

R002:

- app capability denial tests;
- sandbox direct-network negative evidence owned jointly with i2pr qualification;
- local-listener delivery;
- lifecycle/restart/update;
- same core bouncer scenarios under standalone and managed adapter.

R003:

- authorization negative tests;
- no general admin token;
- exact operation matrix;
- no control-plane dependency for normal IRC.

## 10. Risks and decision points

Risks:

- treating i2pr's current internal SAM layer as a public SDK;
- binding too early to an app contract still under corrective work;
- using Proposal 170 as a convenience dependency;
- remote SAM configuration widening host-network authority;
- conflating router and IRC reconnect policy.

ADRs are required if the project considers remote SAM endpoints by default, direct I2CP instead of SAM/app capabilities, or a durable RouterControlProvider public API.

## 11. Completion definition

Router integration is complete for the initial product when the bouncer is qualified through the portable SAM adapter and through the stable secured i2pr managed-app API, without core forks, private router dependencies, or clearnet fallback.

Proposal 170 is not required for completion unless a later canonical product requirement explicitly adds a scoped use case.

## 12. Milestone status

| Milestone | Status | Implementation plan | Closure record | Blockers |
|---|---|---|---|---|
| R001 | blocked | future | future | bouncer-core M005 |
| R002 | blocked | future | future | R001 + stable i2pr app stream/listener/lifecycle contracts |
| R003 | research-blocked | future only if justified | future | concrete product need + stable scoped control contract |
