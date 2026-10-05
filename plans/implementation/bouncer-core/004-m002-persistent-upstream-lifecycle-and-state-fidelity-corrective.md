# Bouncer Core Corrective 004 — M002 Persistent Upstream Lifecycle and State Fidelity

Status: closed

Closure record: `plans/closure/bouncer-core/004-status.md`

Repository baseline: `e76561a517563296e2ebb92110a76f3b06cec34b`

Corrects:

- `plans/implementation/bouncer-core/002-single-network-operational-bouncer.md`
- `plans/closure/bouncer-core/002-status.md`

Corrective roadmap:

- `plans/subsystems/bouncer-core-m002-lifecycle-corrective-addendum.md#C001--persistent-upstream-ownership-and-state-fidelity-correction`

Parent roadmap:

- `plans/subsystems/bouncer-core-roadmap.md`

Long-term requirements:

- `plans/000-long-term-specification.md` sections 1, 4.3, 5, 7, 8, 10, and 14
- `plans/001-terminology-and-domain-model.md` definitions for NetworkSupervisor, ConnectionGeneration, DownstreamSession, DesiredState, and ObservedState
- `plans/002-long-term-roadmap.md` Phases 2-3
- `plans/003-planning-process.md` section 9

Applicable ADR:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`

Primary class: invariant + capability corrective

## 1. Objective

Correct the M002 runtime before M003 introduces durable state and simultaneous clients.

The delivered outcome must make the upstream IRC Network lifecycle independent of local-client attachment while preserving M002's already-correct CAP/SASL, reconnect, generation-fencing, bounded-queue, liveness, secret-handling, and no-ambiguous-replay behavior.

The same corrective must replace lossy/hard-coded channel/member state assumptions with a bounded ISUPPORT-driven model sufficient for truthful downstream state synthesis, and must expand the static no-clearnet guard across the production runtime owner.

## 2. Why this corrective is ready

The defects are present in the reviewed `e76561a` tree and require no unresolved product decision.

The canonical architecture already says:

- the bouncer maintains long-lived upstream IRC sessions on behalf of a local operator;
- NetworkSupervisor is the exclusive live owner of one Network across downstream attachment changes;
- DownstreamSession is one attached local client, not the Network lifetime;
- persistence must distinguish desired state from live observations;
- generic upstream networking remains prohibited.

No ADR is needed because this plan restores the accepted architecture rather than changing it.

M003 is blocked until this corrective closes.

## 3. Current implementation evidence

At the reviewed baseline:

- `NetworkSupervisor::serve` obtains an upstream stream and then blocks on `LocalAcceptor::accept()` before calling `run_generation`;
- upstream registration therefore depends on a local client being present;
- downstream EOF returns `Ok(())` from the generation;
- downstream `QUIT` also returns `Ok(())`;
- `serve` interprets `Ok(())` from `run_generation` as terminal success and stops the NetworkSupervisor;
- upstream and downstream writer tasks are owned by the same generation-local `JoinSet`;
- channel modes are stored as `BTreeSet<char>`, losing mode parameters;
- `member_nick` strips the hard-coded prefix set `~&@%+`;
- `CASEMAPPING` is interpreted, while `CHANTYPES`, `PREFIX`, and `CHANMODES` do not authoritatively drive state;
- `scripts/check-network-boundary.py` scans `core` and `wire`, but not `runtime`.

There is no evidence of a current clearnet path. This corrective is about restoring architectural ownership and making the static proof match the production surface.

## 4. Invariants that must not regress

1. I2pStreamProvider remains the only upstream stream authority.
2. LocalAcceptor remains a downstream-only capability.
3. One NetworkSupervisor owns at most one current upstream ConnectionGeneration.
4. Downstream attach/detach does not itself create or destroy a ConnectionGeneration.
5. Stale generation events cannot mutate the current generation.
6. Non-idempotent user traffic is never replayed across generation loss.
7. Control traffic remains separately bounded/prioritized.
8. CAP/SASL policy remains bouncer-owned and secret-safe.
9. Every client/upstream queue and state collection remains explicitly bounded.
10. Explicit process/supervisor stop is the only local action that deliberately sends upstream QUIT.
11. Current-state synthesis must prefer omission/unknown state over false state.
12. No generic upstream DNS/TCP/HTTP/proxy ownership is introduced.

## 5. Scope

### In scope

- refactor NetworkSupervisor ownership so upstream connect/register/online state does not wait on downstream acceptance;
- allow zero or one attached downstream session at a time in this corrective;
- allow sequential local clients to detach/reattach while the same upstream generation remains online;
- continue reading upstream, maintaining state, and serving PING/PONG with no downstream attached;
- distinguish downstream-session completion from upstream-generation completion;
- preserve or improve orderly shutdown/task ownership;
- introduce bounded ISUPPORT-derived `CHANTYPES`, `PREFIX`, and mode-argument semantics;
- make downstream channel/member/mode projection truthful;
- extend network-boundary static checks to `runtime`;
- add regression/fault tests for the findings;
- reconcile architecture docs, roadmap, registry, and closure evidence.

### Explicitly out of scope

- simultaneous downstream clients;
- SQLite/history/cursors;
- labeled-response;
- chathistory/read-marker;
- production Unix/TCP LocalAcceptor implementation;
- SAM/I2CP/Proposal 170/i2pr integration;
- CTCP/DCC anonymity qualification;
- keeping a local downstream connection alive across an upstream generation loss;
- remote downstream clients;
- generic plugin/module support.

## 6. Required production changes

### A. Separate upstream generation ownership from downstream session ownership

Refactor the runtime so upstream registration happens immediately after I2pStreamProvider succeeds, independent of LocalAcceptor.

A valid shape is:

~~~text
serve
  -> connect/register upstream generation
  -> online network owner
       |-- upstream reader/liveness/writer
       |-- zero-or-one DownstreamSession
       |-- accept next client when detached
  -> upstream failure/backoff/new generation
~~~

The implementation may use an actor/event loop, owned child tasks, or an equivalent bounded structure.

The critical contract is:

- `accept()` is not a predecessor of upstream registration;
- a downstream session completion is an event handled by the online Network owner;
- the upstream generation remains owned until upstream failure, explicit stop, or another upstream-terminal condition occurs.

### B. Define downstream detach semantics

Introduce an explicit downstream-session disposition, conceptually distinguishing:

- graceful local detach/QUIT;
- downstream EOF;
- downstream protocol violation;
- downstream queue overload;
- downstream writer/read error;
- supervisor stop;
- upstream generation loss.

For local detach/failure:

- close only that downstream session;
- clear downstream-attached diagnostics;
- keep upstream stream/read/write/liveness/state tasks alive;
- return to accepting a subsequent local session;
- do not send upstream QUIT;
- do not increment ConnectionGeneration.

For supervisor stop:

- stop accepting clients;
- terminate the attached client;
- send at most one best-effort bounded upstream QUIT;
- join/abort all generation-owned tasks deterministically;
- enter Stopped.

For upstream loss:

- terminate the current attached client if preserving it is not implemented in this corrective;
- tear down the generation;
- preserve DesiredState;
- use existing bounded backoff/new generation behavior;
- do not replay ambiguous user traffic.

### C. Make current state independent of a specific downstream client

Move/retain observed IRC state under the Network generation owner, not inside one DownstreamSession lifetime.

At minimum:

- current network nick/account data already supported;
- joined channels;
- topic state;
- membership;
- channel mode state;
- ISUPPORT/casemapping metadata.

A newly attached downstream session receives a point-in-time bounded projection from this state.

A client that detaches must not erase observed upstream state.

### D. Replace hard-coded membership-prefix behavior

Parse and bound `PREFIX=(modes)prefixes` from ISUPPORT.

Requirements:

- validate mode/prefix cardinality;
- bound the total number of prefix mappings;
- retain an explicit default only until the server provides PREFIX;
- use the current mapping when parsing NAMES membership prefixes;
- apply membership-mode changes consistently where supported;
- do not silently interpret arbitrary leading punctuation as a membership prefix;
- do not expose a hard-coded `~&@%+` assumption after a server supplied another mapping.

### E. Make channel-type semantics explicit

Parse and bound `CHANTYPES`.

Use the current server-advertised channel types when classifying channel targets/state.

The configured DesiredState may remain conservatively validated before connection, but live ObservedState must not assume only `#` and `&` after the server advertises a different CHANTYPES set.

Do not broaden upstream endpoint/network authority.

### F. Make channel-mode projection truthful

The current `BTreeSet<char>` representation is insufficient because IRC channel modes may require parameters.

Implement a bounded representation driven by `CHANMODES=A,B,C,D` and `PREFIX` membership modes, or use an equally truthful model.

Required semantics:

- preserve mode arguments required to reconstruct a truthful `324` projection;
- distinguish membership modes from channel modes;
- bound mode letters, list entries, and argument lengths/counts;
- apply `MODE +.../-...` only when parameter-consumption semantics are known;
- if an unknown/malformed delta makes the current projection ambiguous, mark the affected mode snapshot incomplete and omit/qualify synthesized `324` rather than emitting false state;
- an authoritative server mode snapshot may restore complete state.

Do not implement a full IRC daemon mode engine if a smaller truthful bouncer representation suffices.

### G. Preserve upstream liveness with zero downstream clients

Regression-prove that:

- upstream PING is answered with no local client;
- bouncer liveness PING/PONG continues with no local client;
- JOIN/NICK/TOPIC/MODE/member events update observed state with no local client;
- state accumulated while detached appears when a later client attaches.

### H. Extend the no-clearnet static boundary

Update `scripts/check-network-boundary.py` so the production `runtime` crate is covered by the same source/manifest/dependency checks as `core` and `wire`.

Preserve positive controls using the same predicates as production scanning.

Do not globally ban future networking adapters. The later SAM and concrete LocalAcceptor implementations should live in explicitly owned adapter crates/areas and receive narrowly documented allowlists when they exist.

At this corrective baseline, `runtime` itself must not own generic socket/resolver/HTTP/proxy dependencies.

### I. Reconcile documentation and planning

Update at least:

- `architecture/network-supervisor.md`;
- `architecture/downstream-session.md`;
- `architecture/reconnect-and-liveness.md`;
- `architecture/security-anonymity.md`;
- `architecture/network-boundary.md` if needed;
- `plans/subsystems/bouncer-core-roadmap.md`;
- `plans/registry.md`.

Remove the stale "registration-only slice" statement after the corrective evidence is complete.

Do not rewrite the historical M002 closure to conceal the post-closure defect.

## 7. Ordered work packages

### Work package A — Network/client lifecycle split

Intent:

Make the upstream Network owner persistent with zero downstream clients.

Required changes:

- move accept/attach out of upstream-registration prerequisite;
- represent downstream-session completion separately;
- preserve upstream tasks/state after downstream detach;
- keep one-client-at-a-time limit explicit.

Acceptance evidence:

- upstream reaches Online without a downstream accept;
- downstream EOF/QUIT leaves generation unchanged and Online;
- later attach uses the same generation.

### Work package B — Detach/reattach state projection

Intent:

Make local clients disposable views over persistent upstream state.

Required changes:

- retain observed state after detach;
- accept another client;
- synthesize its current view from the retained state;
- keep queue/session cleanup bounded.

Acceptance evidence:

- state changes while detached appear on reattach;
- detached client's queues/tasks are gone;
- upstream reader/liveness remain active.

### Work package C — ISUPPORT/member/mode fidelity

Intent:

Prevent M003 from persisting an already-lossy state model.

Required changes:

- CHANTYPES;
- PREFIX mapping;
- CHANMODES or equivalent mode-argument ownership;
- bounded truthful NAMES/324 projection;
- incomplete/unknown mode disposition.

Acceptance evidence:

- custom PREFIX/CHANTYPES fixtures;
- parameterized mode fixtures;
- malformed/unknown mode fixtures prove no false 324 synthesis.

### Work package D — Static boundary expansion

Intent:

Make the no-clearnet proof cover the actual production runtime.

Required changes:

- scan runtime source/manifests/transitive dependencies;
- preserve positive controls;
- document future adapter ownership rather than weakening current checks.

Acceptance evidence:

- intentional forbidden runtime source/dependency fixture is detected;
- production workspace passes.

### Work package E — Fault and shutdown regression campaign

Intent:

Prove the lifecycle split does not regress M002 recovery semantics.

Required scenarios:

- no-client upstream registration;
- detach by EOF;
- detach by QUIT;
- downstream malformed input/overload;
- reattach same generation;
- upstream PING and local liveness with zero clients;
- upstream failure while detached;
- upstream failure while attached;
- explicit stop with and without attached client;
- ambiguous chat remains unreplayed;
- repeated attach/detach cycles remain bounded.

### Work package F — Closure and planning reconciliation

Intent:

Make C001 the strict authority and unblock M003 only with evidence.

Required changes:

- closure record `plans/closure/bouncer-core/004-status.md`;
- roadmap/registry status update;
- architecture docs;
- exact command/test evidence.

If any high-severity lifecycle/state-fidelity finding remains, keep M003 blocked and register another bounded corrective.

## 8. Failure, cancellation, restart, and contention semantics

### Downstream failure

Downstream EOF, QUIT, protocol violation, queue overload, or I/O error affects only the current DownstreamSession unless it reveals a shared internal invariant violation.

The Network owner must remain online and accept another client.

### Upstream failure

Upstream EOF/reset/protocol/liveness failure remains generation-terminal.

Current downstream may be disconnected in this corrective. Desired channels are reconstructed only after a fresh upstream registration. Non-idempotent traffic is not replayed.

### Cancellation

Explicit supervisor stop cancels accept, active downstream work, liveness, and upstream I/O. Child tasks are joined or deliberately aborted by their owner.

A canceled downstream session cannot later emit commands into the still-live generation.

### Restart

No durable process restart semantics are added here. The corrected in-memory ownership/state model must be suitable for M003 persistence without depending on one downstream session.

### Contention

Only one downstream is attached at a time. An accept result racing with current-session teardown must have one deterministic owner/disposition; no two sessions may concurrently mutate the M002 Network owner.

## 9. Compatibility and migration

No released stable runtime API or database schema exists.

Internal runtime APIs may change to restore the canonical owner model.

Preserve public/core contracts from M001 unless a defect requires a separately documented corrective.

No data migration is introduced.

## 10. Required tests

### Lifecycle tests

- Online before LocalAcceptor yields a client;
- attach after Online;
- EOF -> detached -> same upstream generation;
- QUIT -> detached -> same upstream generation;
- second client reattaches same generation;
- 100+ attach/detach cycles remain bounded;
- explicit stop is the only local path that sends upstream QUIT.

### State tests

- upstream JOIN/PART/NICK/QUIT/TOPIC/MODE while detached;
- later client sees resulting state;
- custom CASEMAPPING/PREFIX/CHANTYPES;
- membership mode changes;
- parameterized channel modes;
- incomplete/unknown mode state does not emit a misleading 324.

### Recovery tests

- upstream reset while no client;
- upstream reset while client attached;
- stale detached-session command/completion cannot affect current client/generation;
- ambiguous user message is not replayed.

### Liveness tests

- server PING answered with no client;
- generation PING/PONG timeout remains active with no client;
- normal downstream backlog cannot starve upstream control traffic when attached.

### Boundary/security tests

- runtime forbidden-source positive control;
- runtime forbidden-dependency positive control;
- production runtime has no generic upstream socket/resolver/HTTP/proxy path;
- secrets remain absent from diagnostics.

## 11. Required verification commands

Closure must record exact commands actually run. Expected minimum:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh quick
scripts/verify.sh full
scripts/fuzz-smoke.sh
rtk rustup run 1.88.0 sh scripts/verify.sh full
~~~

Add focused runtime tests before broad sweeps.

## 12. Documentation updates

Required:

- network supervisor ownership/task diagram;
- downstream attach/detach semantics;
- ISUPPORT/channel/member/mode state semantics;
- no-client liveness behavior;
- static network-boundary ownership;
- roadmap/registry current status.

## 13. Acceptance criteria

1. NetworkSupervisor reaches Online and remains connected with zero local clients.
2. Local client attachment is not required for upstream registration.
3. Downstream EOF/QUIT does not terminate the upstream generation or supervisor.
4. A later client attaches to the same generation and receives current bounded state.
5. Upstream liveness and state processing continue while detached.
6. Only explicit supervisor stop deliberately emits upstream QUIT.
7. Existing upstream-loss reconnect/no-replay behavior still passes.
8. PREFIX/CHANTYPES drive membership/channel interpretation after advertisement.
9. Parameterized channel modes are preserved truthfully or explicitly treated as incomplete; no false 324 is synthesized.
10. Network-boundary guard covers runtime and its dependency tree with positive controls.
11. Queue/task counts remain bounded across repeated attach/detach.
12. Full verification passes on the declared Rust 1.88 floor.
13. Planning/docs no longer describe M003 as eligible before this corrective closes.

## 14. Stop conditions

Stop and report/register a successor decision rather than broadening scope if:

- correct detach semantics require simultaneous multi-client architecture;
- a faithful mode model requires a full IRC-server mode engine rather than a bounded bouncer representation;
- preserving an attached downstream across upstream reconnect becomes necessary to satisfy the canonical contract;
- runtime decoupling requires a generic socket/listener or router-specific implementation;
- network-boundary enforcement can only be satisfied by weakening the I2P-only invariant;
- M003 persistence/history becomes necessary to make the corrective work.

## 15. Closure evidence required

`plans/closure/bouncer-core/004-status.md` must include:

- implementation commits;
- owner/task lifecycle diagram;
- before/after disposition of C001-F1 through C001-F5;
- attach/detach/generation matrix;
- no-client upstream liveness evidence;
- ISUPPORT/PREFIX/CHANTYPES/CHANMODES fixture matrix;
- truthful/incomplete mode-projection evidence;
- static runtime-boundary positive-control evidence;
- existing M002 regression results;
- repeated attach/detach resource-bounded evidence;
- exact verification commands/results including Rust 1.88;
- unresolved findings and severity;
- explicit M003 readiness decision.

## 16. Handoff notes

Preserve the useful M002 implementation. This is an ownership correction, not a rewrite mandate.

Do not solve downstream detach by restarting the upstream generation faster; the invariant is that the upstream generation survives local-client absence.

Do not use M003 storage as a crutch for current-state ownership. The in-memory owner model must be correct before persistence is introduced.
