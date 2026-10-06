# Bouncer Core M005-B / Plan 021 — Durable Detached-Channel Policy

Status: blocked

Blocker:

- Plan 020 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: capability

## 1. Objective

Extend durable desired channel state from a plain membership list into a bounded channel policy that supports manual detach/reattach while preserving upstream membership and history.

Detached means hidden from ordinary downstream live state, not parted upstream.

## 2. Invariants

1. Desired membership and detached presentation are separate facts.
2. A detached desired channel remains joined upstream when the upstream accepts membership.
3. History ingestion continues while detached.
4. Detached channel traffic is not ordinarily fanned out or projected to local clients.
5. Reattaching never fabricates membership; it projects only observed truthful state.
6. Ordinary PART still means leave and forget desired membership.
7. Existing per-client history cursors remain the delivery boundary for legacy backlog; detached state does not create a second cursor system.
8. No detach policy introduces external notification/network side effects.

## 3. Storage

Replace the durable desired-channel string model with a typed DesiredChannelRecord carrying at least:

- target;
- position/order;
- detached boolean.

Migrate the Plan 020 schema forward transactionally. Existing desired channels migrate detached=false.

Do not store draft-protocol syntax. NetworkState may consume a typed durable policy projection, but observed membership remains live only.

The first detached-channel tranche is deliberately manual. Defer soju-style relay-detached, reattach-on-highlight and timed auto-detach until there is evidence they are worth their additional timers/highlight semantics.

## 4. Runtime semantics

### Detach

When a joined desired channel becomes detached:

- persist policy first;
- do not send upstream PART;
- emit a local synthetic PART or equivalent truthful transition to attached sessions that currently see the channel;
- stop ordinary live fanout/projection for that channel;
- continue state tracking and history ingestion.

The synthetic transition must be visibly local/bouncer-owned and must not be stored as an upstream HistoryEvent.

### Reattach

When detached becomes false:

- persist policy first;
- if observed membership is present, emit synthetic JOIN plus bounded current topic/mode/NAMES projection;
- if observed membership is absent, allow normal desired-state JOIN reconciliation to establish membership before projection;
- legacy clients receive bounded missed history through existing cursor semantics after projection;
- explicit-chathistory clients do not receive duplicate automatic history.

A session attaching while a channel is detached must not see that channel in its ordinary registration projection.

### Leave

A normal PART removes durable desired membership and, if currently joined, sends upstream PART under the existing persistence-first reconciliation rules.

Optionally accept an exact documented compatibility shorthand such as PART <channel> :detach only if it is unambiguous and tested. BouncerServ in Plan 023 is the primary explicit administration path.

## 5. Multi-client semantics

Detached state is Operator/Network channel policy, not per-session UI state.

One client detaching a channel hides it for every attached session on that Network.

Per-client read/playback cursors remain private. A client that was disconnected throughout the detached interval can still query retained history normally.

## 6. Bounds and failure semantics

- number of desired channel records remains bounded by existing desired-channel ceilings;
- policy mutation uses the bounded store worker;
- store refusal means no live detach transition is applied;
- unknown commit state requires re-read before changing presentation;
- synthetic projection is bounded by existing downstream queues and observed-state ceilings;
- if reattach projection overloads one client, existing client desynchronization/detach policy applies without affecting Network membership.

## 7. Work packages

A. typed desired-channel domain model;
B. schema migration and predecessor tests;
C. persistence-first detach/reattach mutations;
D. projection/fanout filtering;
E. synthetic detach/reattach transitions;
F. legacy cursor-backed reattach backlog;
G. multi-client and restart qualification;
H. documentation/closure.

## 8. Tests

Include:

- existing desired channels migrate attached;
- detach survives process restart;
- detached channel remains upstream-joined;
- detached incoming PRIVMSG is durably stored but not live-fanned out;
- detached channel omitted from fresh client projection;
- reattach projects truthful state before backlog;
- explicit chathistory session gets no duplicate legacy backlog;
- legacy cursor advances only after acknowledged delivery;
- one client detach affects all sessions consistently;
- normal PART still parts upstream and removes desired state;
- unknown store commit re-read determines presentation;
- reconnect rejoins desired detached channel but keeps it hidden downstream;
- resource/queue counts settle after repeated detach/reattach.

## 9. Verification

Run the full Plan 020 verification floor plus focused store migration, history, projection and multi-client tests.

## 10. Documentation

Add/update a detached-channel architecture note, storage schema documentation, history/playback behavior and downstream projection semantics.

## 11. Acceptance criteria

A durable desired channel can be hidden locally without leaving upstream, continue collecting bounded history across reconnect/restart, and be reattached with truthful current state and non-duplicating bounded history.

## 12. Stop conditions

Stop if implementation requires:

- persisting observed membership;
- a second history/cursor model;
- forwarding detached traffic through an external notification channel;
- an unbounded timer or highlight cache;
- treating per-session UI preference as global Operator intent without explicit command semantics.

## 13. Closure evidence

Create plans/closure/bouncer-core/021-status.md with schema migration, upstream/downstream transition matrix, restart evidence, cursor/history evidence, bounds and M005-C readiness.
