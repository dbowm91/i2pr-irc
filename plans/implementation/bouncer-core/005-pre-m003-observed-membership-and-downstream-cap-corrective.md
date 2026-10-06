# Bouncer Core Corrective 005 — Pre-M003 Observed Membership and Downstream CAP Correctness

Status: closed — see `plans/closure/bouncer-core/005-status.md`

Repository baseline: `3de5e66e49826735346a23030374ad96bfdb5a3b`

Corrects current post-Corrective-004 runtime behavior before M003.

Related historical closures:

- `plans/closure/bouncer-core/002-status.md`
- `plans/closure/bouncer-core/004-status.md`

Source roadmap:

- `plans/subsystems/bouncer-core-roadmap.md#M003--durable-multi-network-multi-client-and-history-model`

Research gate that may run in parallel:

- `plans/research/002-rust-irc-crate-conformance-plan.md`

Long-term requirements:

- `plans/000-long-term-specification.md` sections 4.4, 4.5, 6, 7, 8, and 14
- `plans/001-terminology-and-domain-model.md` definitions for DesiredState, ObservedState, DownstreamSession, and Capability mediation
- `plans/002-long-term-roadmap.md` Phases 2-3
- `plans/003-planning-process.md` section 9

Applicable ADR:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`

Primary class: invariant + protocol correctness corrective

## 1. Objective

Close the remaining state/protocol defects that would otherwise become durable M003 behavior:

1. stop treating configured/desirable channels as already observed/joined merely because a JOIN command was sent;
2. make upstream channel membership derive only from server-confirmed state and explicitly classify failed join attempts;
3. make downstream IRCv3 CAP negotiation hold registration until CAP END when the client entered CAP negotiation;
4. correct RPL_NAMREPLY visibility handling and related small registration/state edge cases discovered while tightening those invariants.

This corrective does not add persistence, simultaneous clients, history, new downstream capabilities, or router integration.

## 2. Why this corrective is ready

Corrective 004 successfully repaired the difficult owner/lifecycle problem. Source review of the closed `3de5e66` tree found two remaining correctness issues that do not require a new architecture decision.

### C002-F1 — desired JOIN is currently promoted to observed membership

After upstream registration the runtime sends each configured JOIN and then calls `NetworkState::mark_desired_joined()`.

That method copies `desired_channels` directly into `self_channels`.

Consequences:

- sending `JOIN #room` is treated as proof that the server accepted it;
- a later downstream attach may receive a synthetic self JOIN for a channel whose join was denied;
- standard join-failure numerics are not currently reflected in the observed state;
- M003 persistence could accidentally serialize a model where operator intent and live membership are conflated.

The canonical domain model explicitly separates DesiredState from ObservedState, so this is a corrective defect rather than a new product decision.

### C002-F2 — downstream CAP negotiation does not suspend registration

`DownstreamSession` currently has no CAP-negotiation state.

If a client sends CAP LS and then sends valid NICK/USER, the session becomes ready and emits 001 immediately; CAP END is not required.

This is harmless only while the bouncer advertises no useful downstream capabilities. M003 introduces the foundation for message-tags/server-time/batch/labeled-response/history behavior, so the registration state machine must be correct first.

### C002-F3 — RPL_NAMREPLY visibility symbol handling is incomplete

RFC-style RPL_NAMREPLY uses a visibility symbol before the channel. The runtime currently accepts `=` and `*` but not `@`.

A secret-channel NAMES reply can therefore be ignored even though the rest of its state is valid.

## 3. Corrective invariants

The corrective MUST establish and prove:

1. `desired_channels` is operator intent only.
2. `self_channels` contains only server-confirmed current membership.
3. Sending a JOIN command never mutates observed membership by itself.
4. The bouncer adds its own channel membership only when it receives an authoritative server event confirming that membership, normally a self JOIN.
5. PART/KICK and equivalent authoritative events remove observed membership.
6. A join rejection never creates observed membership.
7. Join-failure tracking is bounded and generation-local; it does not become an infinite retry queue.
8. Desired channels remain desired after a failed attempt unless the operator changes configuration.
9. A fresh upstream generation may attempt desired joins again after successful registration, but the current generation does not busy-loop retry failures.
10. A downstream client that starts pre-registration CAP negotiation cannot receive 001/current-state projection until CAP END and valid NICK/USER have all completed.
11. A downstream client that never starts CAP negotiation may still complete registration with valid NICK/USER.
12. CAP requests are mediated locally; they never cause transparent upstream CAP forwarding.
13. No new downstream capability is advertised in this corrective.
14. RPL_NAMREPLY visibility symbols `=`, `*`, and `@` are accepted according to the IRC server-reply grammar.
15. All existing I2P-only, generation-fencing, no-replay, bounded-queue, no-client-liveness, and state-fidelity guarantees remain intact.

## 4. Scope

### In scope

- remove or replace `mark_desired_joined()` semantics that fabricate observed membership;
- introduce a bounded generation-local desired-join attempt/pending/failure representation if useful;
- update `self_channels` only from authoritative server events;
- classify standard join-failure numerics sufficiently to clear pending attempt state and expose bounded diagnostics/state;
- prove failed desired joins are not projected as joined;
- preserve desired intent across the failure and across a later generation;
- add downstream CAP negotiation state;
- defer downstream welcome/state projection until CAP END when CAP negotiation was started before registration;
- retain the existing empty advertised downstream capability set;
- accept all standard RPL_NAMREPLY visibility symbols;
- update tests/docs/closure/registry/roadmap.

### Explicitly out of scope

- SQLite/history/cursors;
- simultaneous downstream clients;
- IRCv3 labeled-response implementation;
- message-tags/server-time/batch downstream enablement;
- chathistory/read-marker;
- automatic same-generation retry policy for failed desired joins;
- production local listener/authentication;
- SAM/I2CP/Proposal 170/i2pr adapters;
- new upstream capability requests beyond existing M002 behavior;
- M004 CTCP/DCC anonymity work.

## 5. Required production changes

### A. Separate desired join attempts from observed membership

Replace the current post-registration pattern:

~~~text
send JOIN for desired channels
mark every desired channel as joined
~~~

with:

~~~text
send JOIN for desired channels
record bounded generation-local attempt/pending state if needed
wait for authoritative server state
self JOIN -> observed membership
join failure -> failed attempt, not membership
~~~

`NetworkState::self_channels` must never be populated merely by command emission.

If a pending-join structure is introduced:

- key it by the current casemapping/channel identity;
- cap it at `MAX_CHANNELS`;
- reset it on generation replacement;
- clear it on self JOIN;
- clear/mark failed on recognized join rejection;
- never use it as a substitute for `self_channels`.

Do not persist pending attempts in M003 as desired state.

### B. Handle join rejection without mutating operator intent

Recognize the standard join-rejection numerics needed to disambiguate a desired attempt from success, including the RFC-defined channel failures applicable to JOIN such as:

- 403 ERR_NOSUCHCHANNEL;
- 405 ERR_TOOMANYCHANNELS;
- 471 ERR_CHANNELISFULL;
- 473 ERR_INVITEONLYCHAN;
- 474 ERR_BANNEDFROMCHAN;
- 475 ERR_BADCHANNELKEY;
- 476 ERR_BADCHANMASK.

Additional network-specific numerics may remain ordinary server events unless independently specified.

For a recognized failure:

- identify the target channel from the reply only after validating parameter shape;
- never add it to `self_channels`;
- clear any generation-local pending attempt for that channel;
- retain `desired_channels`;
- expose only bounded non-secret diagnostic state if diagnostics are added;
- do not auto-retry within the same generation in this corrective.

A subsequent fresh generation may retry desired channels as it already reconstructs desired state after registration.

### C. Make self JOIN the normal positive membership authority

When an upstream JOIN's source nick matches the current bouncer nick under the negotiated casemapping:

- add the channel to observed `self_channels`;
- clear any pending desired-join attempt;
- ensure its ChannelState exists within normal ceilings.

A downstream client attaching before confirmation must not receive synthetic JOIN/current-channel state for that target.

PART/KICK for the bouncer continue to remove observed membership.

If a server provides an alternative authoritative join confirmation mechanism, support requires explicit evidence; do not infer success from command write completion.

### D. Correct downstream CAP registration state

Add explicit pre-registration CAP state to `DownstreamSession`, conceptually:

- `cap_negotiating: bool`;
- NICK received/valid;
- USER received;
- ready.

Required behavior:

- pre-registration `CAP LS` enters/retains CAP negotiation;
- pre-registration `CAP REQ` is handled locally and retains CAP negotiation;
- `CAP END` exits CAP negotiation;
- registration completes only when NICK + USER are valid and CAP negotiation is not active;
- NICK/USER may arrive before or during CAP;
- if no CAP command started negotiation, NICK + USER can still register immediately;
- `CAP END` before NICK/USER does not register by itself;
- repeated/late CAP messages have deterministic bounded behavior;
- post-registration CAP remains locally mediated and cannot alter upstream capability negotiation.

The currently advertised downstream capability set remains empty. Do not use this corrective to introduce M003 capabilities.

### E. Preserve truthful state projection timing

The 001/005/JOIN/topic/mode/NAMES projection must be emitted exactly once when a downstream session transitions into ready state.

When CAP is active, no 001 or current-state projection may be queued before CAP END.

If current observed state changes while the client is still negotiating CAP, projection uses the current generation-owned state at the moment registration actually completes.

### F. Correct RPL_NAMREPLY visibility parsing

Accept the standard visibility field values `=`, `*`, and `@`.

Do not confuse that channel-visibility symbol with per-member PREFIX symbols.

Malformed/unknown visibility fields may be ignored/fail closed according to the existing state parser policy, but must not corrupt existing member state.

### G. Reconcile state API names/comments

Remove or rename `mark_desired_joined()` so future M003 code cannot mistake desired intent for observed membership.

Document explicitly:

- desired configuration;
- pending generation-local attempt;
- observed self membership.

Do not create a durable schema yet.

## 6. Ordered work packages

### Work package A — Desired/observed join split

Intent:

Remove fabricated observed membership.

Acceptance evidence:

- after desired JOIN bytes are written but before server confirmation, `self_channels` is empty for that channel;
- self JOIN moves it into observed membership;
- downstream projection before confirmation omits it.

### Work package B — Join-failure disposition

Intent:

Make a denied desired join explicit without deleting operator intent.

Acceptance evidence:

- each supported standard numeric clears pending attempt state;
- channel remains absent from observed membership;
- desired configuration remains present;
- no same-generation retry loop occurs;
- a fresh generation attempts desired state again.

### Work package C — Downstream CAP registration state machine

Intent:

Make the bouncer a correct downstream CAP-negotiating server before M003 advertises capabilities.

Acceptance evidence:

- CAP LS -> NICK -> USER does not emit 001;
- CAP END then emits exactly one registration/current-state projection;
- NICK/USER without CAP still registers;
- CAP END before NICK/USER waits for both;
- CAP REQ/NAK remains local and registration waits for CAP END.

### Work package D — NAMES visibility and protocol edge regression

Intent:

Close the small state parsing gap found in review.

Acceptance evidence:

- `353 ... @ #secret :...` is incorporated;
- `=` and `*` remain covered;
- visibility symbol is not stored as a membership prefix.

### Work package E — Broad regression/closure

Intent:

Prove this pre-M003 correction preserves Corrective 004 and M002 guarantees.

Required regression areas:

- zero-client upstream online/liveness;
- downstream detach/reattach same generation;
- truthful CHANTYPES/PREFIX/CHANMODES projection;
- ambiguous-message no replay;
- reconnect/backoff;
- static runtime network boundary;
- Rust 1.88 floor.

## 7. Failure, cancellation, restart, and contention semantics

### Join failure

A rejected desired JOIN is an observation about this generation, not a configuration mutation.

It must not stop the NetworkSupervisor, stop a downstream client, or force reconnect unless the server separately terminates the connection.

### Generation replacement

Pending join-attempt state is discarded. Desired configuration survives and may be attempted after the new generation registers.

Observed membership is rebuilt from new authoritative upstream state.

### Downstream CAP

A downstream EOF/QUIT during CAP negotiation detaches only that client under Corrective 004 semantics.

A malformed CAP message may terminate only that downstream session according to current protocol-failure policy.

No downstream CAP event changes the upstream generation's negotiated capabilities.

### Cancellation

Supervisor stop cancels outstanding local attachment/CAP work through existing ownership. No pending desired-join bookkeeping creates detached tasks or timers.

## 8. Compatibility and migration

There is no durable schema or released stable runtime API.

This corrective may change internal `NetworkState` and `DownstreamSession` APIs.

It must not change the public I2pStreamProvider/LocalAcceptor security boundary.

No data migration is introduced.

## 9. Required tests

### Desired/observed state

- configured desired channel is not observed immediately after JOIN write;
- self JOIN confirms observed membership;
- self PART/KICK removes observed membership;
- each supported join-failure numeric leaves the channel unjoined;
- failed desired join remains in desired intent;
- reconnect/fresh generation attempts desired JOIN again;
- a downstream attach after failed JOIN does not receive a synthetic JOIN.

### Downstream CAP

- no-CAP NICK/USER registration;
- CAP LS -> NICK/USER -> no 001 before END;
- CAP LS -> NICK -> CAP END -> USER -> register once;
- CAP LS -> USER -> NICK -> CAP END -> register once;
- CAP REQ gets local NAK and still waits for END;
- repeated CAP LS/END have deterministic behavior;
- downstream PING still works while CAP negotiation is pending;
- downstream detach during CAP does not affect upstream.

### NAMES/state

- `=`, `*`, and `@` RPL_NAMREPLY visibility forms;
- custom PREFIX remains independent from the visibility field;
- malformed visibility does not corrupt retained membership.

### Regression/security

- full Corrective 004 lifecycle/fault suite;
- static network boundary;
- fuzz smoke;
- secret redaction;
- Rust 1.88 full verification.

## 10. Required verification commands

Closure records exact commands. Expected floor:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh quick
scripts/verify.sh full
scripts/fuzz-smoke.sh
rustup run 1.88.0 sh scripts/verify.sh full
~~~

## 11. Documentation updates

Required:

- `architecture/network-supervisor.md` — desired attempt vs observed join;
- `architecture/downstream-session.md` — CAP negotiation registration gate;
- state-model documentation/comments;
- `plans/subsystems/bouncer-core-roadmap.md`;
- `plans/registry.md`;
- closure record `plans/closure/bouncer-core/005-status.md`.

Historical closure 004 remains unchanged.

## 12. Acceptance criteria

1. Sending configured JOIN does not add the channel to observed membership.
2. Only authoritative upstream confirmation creates observed self membership.
3. Standard join rejection cannot be projected as a successful join.
4. Desired intent survives a failed join without same-generation retry spinning.
5. Fresh generations can reconstruct/attempt desired joins.
6. CAP-negotiating downstream clients receive no welcome/current-state projection before CAP END.
7. Clients that never use CAP still register normally.
8. Downstream CAP remains locally mediated and cannot alter upstream CAP state.
9. RPL_NAMREPLY `@` visibility is parsed alongside `=` and `*`.
10. Corrective 004 lifecycle/state-fidelity guarantees remain green.
11. Rust 1.88 full verification passes.
12. M003 remains blocked until both this corrective closes and Research 002 has a recorded disposition.

## 13. Stop conditions

Stop and register a successor decision instead of widening this corrective if:

- authoritative joined-state cannot be determined from standard IRC events without a larger state protocol redesign;
- correct downstream CAP negotiation requires implementing actual M003 capabilities;
- join retry semantics require persistence/timers beyond generation-local state;
- fixing these defects requires simultaneous downstream clients or SQLite;
- a proposed fix weakens the I2P-only/network-boundary invariant.

## 14. Closure evidence required

`plans/closure/bouncer-core/005-status.md` must include:

- implementation commits;
- disposition of C002-F1 through C002-F3;
- desired/pending/observed state transition matrix;
- join-failure numeric fixture matrix;
- downstream CAP registration transition matrix;
- NAMES visibility fixture matrix;
- Corrective 004 regression evidence;
- exact verification results including Rust 1.88;
- unresolved findings/severity;
- explicit statement that M003 remains blocked until Research 002 is also complete.

## 15. Handoff notes

Do not solve the desired/observed defect by deleting desired intent after a failed JOIN. Persistence needs the opposite separation: operator intent remains durable while observed state remains generation-local.

Do not add downstream IRCv3 capabilities merely to exercise CAP. This corrective establishes the registration gate; M003 will decide which capabilities can truthfully be advertised.
