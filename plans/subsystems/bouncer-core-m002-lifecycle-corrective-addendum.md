# Bouncer Core M002 Post-Closure Corrective Addendum

Status: active

Long-term references:

- `plans/000-long-term-specification.md`
- `plans/001-terminology-and-domain-model.md`
- `plans/002-long-term-roadmap.md`
- `plans/003-planning-process.md`

Related roadmap:

- `plans/subsystems/bouncer-core-roadmap.md`

Related ADR:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`

Historical implementation and closure:

- `plans/implementation/bouncer-core/002-single-network-operational-bouncer.md`
- `plans/closure/bouncer-core/002-status.md`

Repository baseline reviewed:

- `e76561a517563296e2ebb92110a76f3b06cec34b`

## 1. Purpose and ownership boundary

M002 is historically closed against its recorded evidence, but post-closure review found that the delivered runtime does not yet satisfy the stronger product invariant implied by a persistent IRC bouncer: the upstream IRC network session must remain owned and live independently of whether a local downstream client is currently attached.

This corrective line owns only the defects that must be repaired before M003 may persist or multiply the current ownership/state model:

- upstream/downstream lifecycle decoupling;
- sequential downstream detach/reattach while one upstream generation remains alive;
- truthful ISUPPORT-driven channel/member/mode state needed for downstream state synthesis;
- static no-clearnet boundary coverage across the production runtime crate;
- planning/architecture reconciliation for the corrected owner model.

It does not add durable storage, multiple simultaneous clients, SAM, a production listener, router integration, or M003 history behavior.

The historical M002 closure record remains immutable. This addendum becomes the strict current authority for whether M003 may proceed.

## 2. Findings requiring correction

### C001-F1 — Upstream lifetime is coupled to downstream attachment

Current `NetworkSupervisor::serve` connects the upstream provider, then waits on `LocalAcceptor::accept()` before invoking the generation registration/online loop.

Consequences:

- with no local IRC client, the upstream IRC session is not registered and cannot remain resident in channels;
- the bouncer cannot accumulate current state while the user is away;
- local-client availability incorrectly participates in upstream connection-generation progress.

This contradicts the canonical persistent-bouncer product model.

### C001-F2 — Downstream EOF/QUIT terminates the supervisor

The current generation returns success on downstream EOF and downstream `QUIT`. `serve` interprets successful generation completion as terminal success and stops rather than keeping the upstream session online.

Local client detach must be a downstream-session lifecycle event, not an upstream-network shutdown event.

### C001-F3 — State synthesis is not yet sufficiently faithful for persistence

Current runtime state tracks channel modes as a set of mode letters and strips membership prefixes using a hard-coded `~&@%+` set.

It records `CASEMAPPING`, but does not yet make the following ISUPPORT values authoritative:

- `CHANTYPES`;
- `PREFIX=(modes)prefixes`;
- `CHANMODES=A,B,C,D` or an equivalent mode-argument model.

A later client can therefore receive a synthesized `324` or NAMES projection that omits required mode arguments or assumes the wrong membership-prefix vocabulary.

M003 must not persist this lossy representation.

### C001-F4 — The static network-boundary proof omits the runtime owner

`scripts/check-network-boundary.py` currently scans `core` and `wire`. The production upstream state machine now lives in `runtime`.

There is no observed clearnet path today, and the workspace Tokio feature set does not enable `net`, but the claimed static no-clearnet boundary no longer covers the most relevant production crate.

### C001-F5 — Planning text is stale

The main roadmap simultaneously marks M002 closed and says the runtime is still only a registration slice. The registry currently makes M003 planning-eligible without accounting for C001-F1 through C001-F4.

## 3. Corrective invariants

The corrective MUST establish and prove all of the following:

1. `NetworkSupervisor` owns the upstream Network lifecycle independently of local-client attachment.
2. One upstream generation can exist with zero attached downstream clients.
3. A local client can attach after the upstream is already online and receive a truthful bounded state projection.
4. Downstream EOF, local `QUIT`, registration failure, protocol failure, or downstream writer failure detaches that client only; it does not send upstream `QUIT`, close the upstream stream, increment `ConnectionGeneration`, or stop the supervisor.
5. After detach, a later local client can attach to the same live upstream generation.
6. Explicit supervisor/process stop remains the owner of upstream `QUIT` and terminal shutdown.
7. Upstream failure may still terminate the currently attached local session in this corrective; preserving a downstream session through upstream reconnection is deferred unless required for correctness.
8. Upstream liveness/state processing continues while no downstream client is attached.
9. ISUPPORT-derived channel/member/mode semantics are represented truthfully enough that synthesized state never invents or silently drops a required parameter.
10. If mode state becomes ambiguous, the bouncer prefers an explicit unknown/incomplete state or omission over emitting a false `324`.
11. Core, wire, and runtime remain structurally unable to own generic upstream DNS/TCP/HTTP/proxy behavior.
12. Existing generation fencing, bounded queues, secret handling, no-replay semantics, CAP/SASL behavior, and fault-test guarantees do not regress.

## 4. Target ownership model

The corrected runtime should have this logical ownership:

~~~text
NetworkSupervisor
|
+-- upstream generation
|   +-- connect / register
|   +-- upstream reader
|   +-- upstream writer
|   +-- liveness
|   +-- observed IRC state
|   +-- reconnect/backoff
|
+-- downstream attachment owner
    +-- zero or one attached DownstreamSession in C001
    +-- accept next client while upstream remains live
    +-- project current upstream state on attach
    +-- detach locally on EOF/QUIT/failure
~~~

The exact task decomposition is implementation-specific, but downstream session completion MUST be represented as data to the network owner rather than as successful completion of the upstream generation.

## 5. Dependency graph

~~~text
historical M002 closure
        |
        v
C001 upstream lifecycle + state fidelity corrective
        |
        v
M003 durable multi-network / multi-client / history
~~~

C001 is a hard dependency for M003.

M004, M005, and router R001 remain transitively blocked behind M003/M004/M005.

## 6. Corrective milestone

### C001 — Persistent upstream ownership and state-fidelity correction

Class: invariant + capability corrective

Implementation handoff:

- `plans/implementation/bouncer-core/004-m002-persistent-upstream-lifecycle-and-state-fidelity-corrective.md`

Objective:

Repair the M002 runtime so a persistent upstream IRC session exists independently of local client presence, make sequential local reattachment correct on the same generation, and harden current-state semantics/static network-boundary evidence before M003 introduces persistence and multi-client fanout.

Exit conditions:

- upstream reaches and remains Online with no downstream client attached;
- attaching a client does not create a new upstream generation;
- downstream detach followed by reattach preserves the same upstream generation and current state;
- upstream PING/PONG and state updates continue with no client;
- downstream QUIT never becomes upstream QUIT;
- only supervisor stop owns terminal upstream QUIT;
- CHANTYPES/PREFIX and mode-argument semantics are explicit and bounded;
- no synthesized state lies about parameterized channel modes;
- no hard-coded membership-prefix assumption remains where ISUPPORT has supplied a different mapping;
- network-boundary guard covers runtime with positive controls;
- existing M002 no-replay/reconnect/CAP/SASL/security tests remain green;
- docs/registry/main roadmap accurately record C001 as the strict dependency before M003.

## 7. Non-goals

C001 does not implement:

- SQLite or any durable state;
- simultaneous multi-client fanout;
- labeled-response routing;
- chathistory/read-marker;
- a real Unix/TCP local listener;
- SAM/I2CP/Proposal 170/i2pr adapters;
- CTCP/DCC M004 anonymity qualification;
- remote downstream access;
- an operator service/control UI.

## 8. Verification strategy

Required deterministic scenarios include:

- supervisor reaches Online before any accept result is supplied;
- upstream events mutate current state while no client exists;
- first client attaches and receives current state;
- first client EOF detaches only;
- first client `QUIT` detaches only;
- second client attaches to the same ConnectionGeneration;
- upstream remains responsive to PING while zero clients are attached;
- explicit supervisor stop emits at most one upstream QUIT and terminates owned tasks;
- upstream failure still replaces the generation and never replays ambiguous chat;
- custom `CHANTYPES` and `PREFIX` alter parsing/projection behavior;
- parameterized channel-mode fixtures either round-trip truthfully or are explicitly marked incomplete/omitted;
- runtime forbidden-network source/dependency positive controls fail as intended.

## 9. Completion definition

C001 closes only when the repository can truthfully say that the M002 upstream session is persistent across local client absence/detach and that M003 will not be asked to persist a knowingly lossy channel/member mode representation.

Until C001 closes, M003 is blocked.
