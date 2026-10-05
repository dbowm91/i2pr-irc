# Bouncer Core Milestone 002 — Single-Network Operational Bouncer

Status: blocked

Repository baseline: 2228103181a60855ff6e76f024298b91aec1b940

Source roadmap:

- plans/subsystems/bouncer-core-roadmap.md#M002--single-network-operational-bouncer

Long-term requirements:

- plans/000-long-term-specification.md sections 1-7, 9-10, 13-14
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md Phase 2
- plans/research/001-bouncer-and-i2p-foundation.md

Applicable ADRs:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md

Primary class: capability

Hard dependency:

- bouncer-core M001 must be evidence-closed.

Current implementation status: remains blocked. The registration-only runtime slice is not an M002 implementation and does not change this dependency.

## 1. Objective

Build the first complete bouncer vertical without introducing a concrete I2P router dependency:

- one configured Network;
- one NetworkSupervisor owning one upstream connection generation at a time;
- one local downstream session supplied through LocalAcceptor;
- upstream IRC registration with CAP 302 and configured SASL;
- downstream IRC-server registration synthesized from authoritative bouncer state;
- bidirectional message flow with explicit routing;
- phase-specific deadlines/liveness;
- bounded reconnect/backoff/jitter;
- stale-generation fencing;
- correct ambiguous-delivery behavior;
- clean cancellation/shutdown.

The milestone is operational only through fake/test I2pStreamProvider and LocalAcceptor implementations. SAM remains out of scope.

## 2. Why this milestone is blocked

M002 depends on contracts M001 has not implemented or closed:

- strict wire parser/encoder;
- I2pEndpoint/I2pStreamProvider;
- LocalAcceptor;
- connection-generation identity;
- injected time;
- deterministic fault streams;
- static network-boundary guards.

Implementation against guessed versions of those contracts would create exactly the coupling M001 is intended to avoid.

Once M001 closes without a high-severity finding, this plan may be refreshed against the closure baseline and marked ready without changing its objective.

## 3. Current implementation evidence

At this planning baseline there is no bouncer runtime.

The canonical model already requires:

- one live state owner per Network;
- stable DesiredState distinct from ObservedState;
- no generic upstream networking;
- capability mediation rather than CAP proxying;
- no blind replay of non-idempotent traffic;
- bounded queues and priorities;
- injected time and deterministic fault qualification.

M001 is responsible for making those lower-level contracts concrete.

## 4. Invariants that must not regress

1. Exactly one logical NetworkSupervisor owns mutable upstream state for the Network.
2. Every event from I/O tasks carries or is associated with ConnectionGeneration.
3. A stale generation cannot change state, resolve a current request, or trigger a current downstream event.
4. DesiredState survives reconnect conceptually; ObservedState is rebuilt fresh after registration.
5. User PRIVMSG/NOTICE/TAGMSG and arbitrary commands are not automatically replayed after ambiguous failure.
6. PING/PONG, registration, CAP, SASL, and shutdown control traffic cannot be starved by normal chat backlog.
7. Every supervisor/downstream queue is bounded with an explicit overload disposition.
8. The upstream CAP request set is bouncer policy and does not vary based solely on the attached downstream client.
9. The bouncer advertises downstream capabilities only when it implements their semantics.
10. No SAM/system DNS/generic TCP is introduced.
11. No spawned task outlives its owner after clean shutdown.

## 5. Scope

### In scope

- production async runtime ownership, expected to use Tokio after dependency review;
- Network configuration required for one network;
- NetworkSupervisor lifecycle/state machine;
- one upstream stream from I2pStreamProvider;
- one local downstream stream from LocalAcceptor;
- upstream IRC registration;
- CAP LS 302 / REQ / ACK/NAK / END;
- SASL PLAIN at minimum if configured, with credentials treated as secrets;
- PASS/NICK/USER ordering according to server/capability requirements;
- ISUPPORT and current identity capture;
- JOIN/PART/NICK/QUIT/KICK/TOPIC/MODE/member state needed to synthesize one client's current view;
- server PING and client PING handling;
- downstream registration and initial current-state projection;
- bounded outbound priority queues;
- phase deadlines;
- reconnect/backoff/jitter;
- graceful stop;
- typed diagnostics/state snapshot;
- deterministic tests using M001 fault provider.

### Explicitly out of scope

- real SAM;
- SQLite/history;
- durable restart configuration beyond minimal fixture/config needed for the vertical;
- multiple upstream networks;
- more than one simultaneous downstream client;
- labeled-response multi-client routing;
- draft chathistory/read-marker;
- DCC/CTCP anonymity qualification;
- arbitrary operator service commands;
- soju.im/bouncer-networks;
- external plugins;
- remote downstream listener.

## 6. Required production changes

### Runtime ownership

Introduce one explicit runtime/composition owner for M002.

Do not put Tokio, task spawning, or transport ownership in the wire crate.

The runtime owns:

- supervisor task;
- upstream read/write tasks or equivalent split;
- downstream session task;
- timers;
- bounded event channels;
- orderly cancellation/join.

Every task must have a documented owner and completion path.

### NetworkSupervisor state machine

Use explicit phases comparable to:

~~~
Disabled / Idle
  -> WaitingForProvider
  -> Connecting
  -> Registering
       -> CapabilityNegotiation
       -> Authenticating
       -> AwaitWelcome
  -> Online
  -> Backoff
  -> Stopping
  -> Stopped
~~~

Exact enum decomposition may differ, but illegal phase transitions must be rejected/diagnosed rather than represented by loose booleans.

Track:

- ConnectionGeneration;
- negotiated upstream caps;
- nick/user/account identity;
- server ISUPPORT/casemapping;
- desired channels;
- observed channels/members/topic/modes at the bounded level required for M002;
- liveness timestamps/deadlines;
- reconnect attempt/backoff state;
- pending safe request state;
- priority/normal queue pressure.

A new generation clears generation-scoped request/observed state before fresh registration.

### Registration and CAP

Implement an upstream registration transaction that:

- requests CAP LS 302;
- accumulates multiline capability advertisement;
- computes a stable bouncer-owned request set;
- handles ACK/NAK explicitly;
- performs SASL only when configured and offered/required by policy;
- sends CAP END deterministically;
- handles welcome/error/auth failure classifications.

Do not request a capability solely because the attached downstream client requests it.

Secrets must be held in a secret-marked type or equivalent redaction-aware boundary and zeroized where a reviewed dependency/policy makes that meaningful; regardless, they cannot enter Debug/display diagnostics.

### SASL

At minimum support SASL PLAIN for interoperability if configured.

Requirements:

- bounded AUTHENTICATE chunks;
- exact base64 behavior;
- no credential logging;
- explicit success/failure numerics;
- no fallback to sending NickServ passwords automatically unless a future feature explicitly owns it;
- no retry loop that hammers an upstream service with bad credentials.

If a reusable SASL crate is considered, dependency review is required.

### Downstream server behavior

The bouncer acts as an IRC server to one local client.

It must:

- parse downstream registration independently;
- authenticate according to a minimal M002 local secret/fixture contract if local TCP-like semantics are represented; actual socket listener/auth UX may remain for a later runtime plan;
- advertise only implemented downstream capabilities;
- synthesize welcome/ISUPPORT/current channel state from the supervisor model;
- route downstream IRC commands as typed UpstreamIntent values;
- return local errors for unsupported/unsafe operations rather than blindly proxying them.

The downstream cannot directly issue CAP to the upstream connection.

### State tracking

M002 needs enough current state to rehydrate an attached client and recover desired channels after reconnect.

At minimum model:

- current network nick/account;
- channel joined/not joined;
- topic;
- bounded membership with nick/prefix modes;
- channel mode representation sufficient to preserve observed state without pretending full semantic support for unknown modes.

ISUPPORT drives casemapping/prefix/channel type semantics where available.

Unknown modes/tokens should be retained or safely represented rather than discarded if doing so is needed for a truthful downstream snapshot.

### Typed intents and replay class

Classify outbound intents at the domain boundary.

At minimum distinguish:

- ephemeral/non-replayable user traffic;
- desired-state reconciliation operations that can be regenerated after fresh registration;
- request/response queries scoped to one generation;
- protocol-control traffic.

Do not implement a generic "retry command" flag supplied by downstream clients.

### Queues and priority

Use bounded queues.

At minimum:

Priority/control:
- PONG;
- CAP/SASL/registration;
- shutdown/QUIT as appropriate;
- essential reconnect state.

Normal:
- user chat;
- JOIN/PART requests;
- queries.

Define overload behavior. It may reject/disconnect a pathological downstream client, but it may not silently allocate without bound.

Do not use one large queue where normal traffic can starve liveness.

### Timeout and liveness policy

Represent distinct configurable bounded durations for:

- provider connect;
- registration;
- CAP/SASL phase;
- online idle probe;
- PONG/dead peer;
- graceful shutdown;
- initial/max reconnect delay.

Defaults should be conservative for anonymity-network conditions but closure must describe them as operational defaults, not protocol claims.

Tests use injected time.

### Reconnect scheduler

M002 only has one Network, but implement the per-network backoff state in a form that M003/M004 can later place behind a global scheduler.

Requirements:

- exponential growth;
- configured cap;
- bounded jitter from an injected deterministic RNG/source;
- reset after a defined stable-online period rather than every brief successful TCP/I2P connect;
- auth/configuration failures may require a different retry disposition from path failures;
- explicit stop cancels retry.

### Failure ambiguity

Track whether an outbound intent was:

- not yet handed to the stream writer;
- partially/possibly written;
- completed locally as a write.

None of these prove server processing.

Across generation loss:

- do not requeue user messages automatically;
- drop/fail generation-scoped queries;
- regenerate desired JOIN state from DesiredState after registration;
- expose a bounded diagnostic for ambiguous user traffic if useful.

Do not invent message acknowledgments absent protocol support.

### Diagnostics

Provide a bounded frontend-neutral snapshot/event vocabulary for:

- phase;
- generation;
- last disconnect class;
- reconnect attempt/current delay;
- capability/SASL failure class;
- queue pressure;
- attached downstream state.

No secrets/raw authentication payloads.

## 7. Ordered work packages

### Work package A — Runtime/event ownership

Intent:

Create one supervised runtime composition with explicit bounded channels and cancellation.

Acceptance evidence:

- task ownership diagram;
- no detached tasks after shutdown;
- queue capacities named/tested.

### Work package B — Upstream registration state machine

Intent:

Reach truthful Online state from an I2P byte stream.

Acceptance evidence:

- CAP 302 golden scenarios;
- SASL success/failure;
- registration timeout/disconnect at each phase;
- stable capability request set.

### Work package C — Live IRC state model

Intent:

Maintain sufficient current state for one downstream client and reconnect reconciliation.

Acceptance evidence:

- nick/channel/member/topic/mode event sequences;
- casemapping/ISUPPORT changes;
- generation reset.

### Work package D — Downstream server session

Intent:

Allow one local stream to behave as an IRC client against the bouncer.

Acceptance evidence:

- downstream registration;
- synthesized current network state;
- command-to-intent routing;
- unsupported command diagnostics.

### Work package E — Liveness, queue priority, reconnect

Intent:

Make the vertical survive bad stream conditions.

Acceptance evidence:

- PONG under normal-queue saturation;
- phase-specific virtual deadlines;
- exponential backoff/jitter;
- stable-period reset;
- no replay of ambiguous chat.

### Work package F — Fault campaign and docs

Intent:

Qualify the complete M002 vertical under deterministic fault injection.

Acceptance evidence:

- disconnect at every registration transition;
- stalls/read segmentation/short writes;
- stale-generation events;
- repeated online/offline cycles;
- clean resource convergence after stop.

## 8. Failure, cancellation, restart, and contention semantics

### Failure

Provider/naming/stream errors, IRC protocol errors, auth failures, registration timeout, online liveness timeout, downstream protocol violation, queue overload, and internal task failure have distinct typed dispositions.

One downstream failure cannot corrupt the NetworkSupervisor.

### Cancellation

Stopping the supervisor cancels pending connect/timers, closes the active stream generation, closes/fails pending downstream requests, and joins child tasks.

No canceled timer/event can fire into a reused generation.

### Restart

Durable process restart is not a capability in M002. The state machine must nevertheless separate DesiredState from ObservedState so M003 can persist only the former plus history/configuration.

### Contention

With one downstream client there is no multi-client request contention, but concurrent upstream reader/writer/supervisor events must be serialized through the owner and tested at generation boundaries.

## 9. Compatibility and migration

No released runtime exists.

M002 may evolve M001 internal APIs, but any material change to M001 closed invariants requires a corrective plan rather than silent weakening.

No durable database schema is introduced.

Configuration format should remain minimal and explicitly unstable unless the milestone chooses to freeze a small schema; if frozen, add strict validation and compatibility notes.

## 10. Required tests

### Unit/state-machine

- every legal/illegal phase transition;
- CAP multiline negotiation;
- ACK/NAK;
- SASL success/failure/unavailable;
- ISUPPORT/casemapping projection;
- liveness deadlines;
- backoff/jitter/reset;
- intent replay classification;
- queue priority/overflow.

### Integration

- fake provider -> upstream scripted IRC -> bouncer -> fake local client;
- downstream registration/current-state projection;
- join/part/message/query basic vertical;
- server-initiated nick/topic/member changes;
- disconnect/reconnect/rejoin;
- downstream detach/reattach during same generation if supported by the one-client model.

### Fault/recovery

- reset/EOF at every registration phase;
- short writes around IRC command boundaries;
- stalled reads during online liveness;
- upstream disconnect immediately before/after user message write;
- stale old-generation reader event after new generation online;
- repeated 100+ connect/fail/reconnect cycles under virtual time;
- shutdown in every phase.

### Security/negative

- no secret in Debug/error/diagnostic;
- downstream cannot force unrequested upstream CAP;
- downstream cannot request a generic endpoint/network change;
- M001 network guard remains green;
- malformed downstream/upstream lines remain bounded.

## 11. Required verification commands

Use M001's closed verification floor plus focused M002 tests.

Expected:

~~~
cargo test -p <core/runtime package> <focused>
scripts/verify.sh quick
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
scripts/verify.sh full
scripts/fuzz-smoke.sh
~~~

Closure records exact commands actually executed.

## 12. Documentation updates

- architecture/network-supervisor.md;
- architecture/downstream-session.md;
- architecture/reconnect-and-liveness.md;
- architecture/capability-mediation.md;
- architecture/security-anonymity.md initial operational subset;
- configuration reference for any frozen settings;
- README implementation-state section;
- AGENTS.md gotchas/verification as needed;
- roadmap/registry.

## 13. Acceptance criteria

1. One Network reaches Online through only I2pStreamProvider.
2. One downstream client registers to the bouncer and receives truthful synthesized current state.
3. Upstream CAP 302 and configured SASL work without secret leakage.
4. Current nick/channel/member/topic/mode state follows representative event sequences.
5. Every I/O event is generation-fenced.
6. Control traffic remains live under bounded normal-queue pressure.
7. Phase-specific timeouts use injected time.
8. Reconnect uses bounded exponential backoff/jitter and does not busy-loop.
9. Authentication/configuration failures have an explicit retry disposition.
10. Ambiguous user traffic is never blindly replayed after reconnect.
11. Desired channels are regenerated only after fresh registration.
12. Shutdown in every phase leaves no owned task running.
13. Deterministic fault campaign passes.
14. No generic upstream network/SAM/SQLite code lands.

## 14. Stop conditions

Stop instead of widening scope if:

- M001 is not closed or its closure has a high-severity unresolved finding;
- correct multi-client behavior becomes necessary to make the one-client vertical work;
- reliable current-state projection requires a durable database;
- upstream server behavior requires a router-specific workaround;
- a generic TCP/DNS connector appears necessary;
- a CAP/SASL requirement cannot be truthfully represented by the frozen wire layer;
- exact reconnect correctness requires global multi-network scheduling rather than leaving a compatible per-network state seam.

## 15. Closure evidence required

- implementation commits;
- M001 dependency closure reference;
- runtime task/queue ownership matrix;
- network state transition matrix;
- CAP/SASL fixtures;
- state reconstruction fixtures;
- generation-fencing tests;
- timeout/backoff virtual-time evidence;
- priority/overload evidence;
- ambiguous-delivery tests;
- shutdown phase matrix;
- repeated deterministic fault campaign results;
- secret-redaction evidence;
- full verification results;
- residual findings and M003 readiness.

## 16. Handoff notes

This plan is intentionally written before M001 closes so its upstream requirements can pressure-test M001's interfaces, but implementation is blocked.

When M001 closes, refresh the repository baseline and any exact type names before changing status to ready. Do not weaken M001's I2P-only boundary to simplify M002.
