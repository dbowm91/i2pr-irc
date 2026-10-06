# Bouncer Core M005-C / Plan 022 — Presence and Preferred-Nick Policy

Status: blocked

Blocker:

- Plan 021 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Primary class: capability

## 1. Objective

Add mature Operator presence and preferred-nickname behavior without deriving identity from the host environment or treating socket count as human presence.

Deliver:

- configurable auto-away;
- draft/pre-away mediation for passive/background downstream connections;
- explicit manual-away precedence;
- explicit 433 nickname-collision handling;
- bounded deterministic fallback nick selection;
- configurable keep-nick/reclaim;
- MONITOR-assisted reclaim when supported and bounded fallback probing otherwise.

## 2. Invariants

1. NetworkRecord.nick remains the preferred configured nick.
2. NetworkState.nick remains the current observed nick.
3. Fallback nick generation uses only configured policy and protocol state, never hostname/login/process/environment data.
4. Presence is aggregated from session presence state, not merely attached socket count.
5. One passive history-sync connection cannot prevent auto-away.
6. Manual Operator AWAY state cannot be accidentally cleared by an unrelated session attaching/detaching.
7. Nick reclaim traffic is rate-bounded and generation-fenced.
8. Client attachment never changes the upstream CAP request fingerprint.

## 3. Durable policy

Extend durable Network policy with explicit fields for at least:

- auto_away enabled;
- keep_nick enabled;
- optional bounded automatic away text or a fixed bouncer-owned default.

Migration defaults must preserve previous behavior. Existing Networks should not begin emitting new upstream AWAY/NICK traffic merely because the binary was upgraded; migrate both policies disabled unless canonical product direction is deliberately amended.

Do not persist live away/current-nick observation.

## 4. Presence state

Track per-session presence as active or passive.

draft/pre-away is a downstream capability and adapter. When negotiated, the draft's pre-registration/passive semantics classify that session without forwarding a client-specific fingerprint upstream.

Define deterministic precedence:

1. explicit manual AWAY with text establishes Operator manual-away;
2. explicit manual AWAY clear removes manual-away;
3. when no manual-away exists, auto-away derives from whether any active session exists;
4. passive sessions do not count as active;
5. an unbound control-only session does not make any Network present.

Only transitions emit upstream AWAY state. Repeated equivalent events are idempotent.

On reconnect, current process presence policy is re-applied after successful registration rather than restoring stale observed away state from SQLite.

## 5. Nick collision and fallback

Handle ERR_NICKNAMEINUSE and related registration collision replies explicitly.

A collision must not sit until generic registration timeout.

Define a bounded deterministic fallback sequence from the configured nick, constrained by the server's known/default nick length and IRC nick grammar. Do not use random host-derived suffixes.

The number of registration fallback attempts is explicitly capped. Exhaustion becomes a typed terminal registration failure until configuration/reconcile changes.

## 6. Keep-nick/reclaim

When current nick differs from preferred nick and keep_nick is enabled:

- use MONITOR availability notifications when the server advertises a usable MONITOR limit;
- otherwise use a bounded low-frequency ISON probe or equivalent standard query;
- never poll faster because a client attaches;
- send NICK preferred only when policy has evidence it may succeed or on the bounded schedule;
- authoritative upstream NICK confirms success.

Reclaim timers/tasks are generation-owned and disappear on replacement.

MONITOR membership itself is not durable DesiredState; keep-nick policy is.

## 7. Work packages

A. durable presence/nick policy migration;
B. per-session presence classification;
C. draft/pre-away adapter and CAP advertisement;
D. manual/automatic away state machine;
E. registration 433/fallback state;
F. MONITOR-based keep-nick;
G. bounded fallback reclaim schedule;
H. restart/multi-client/anonymity tests;
I. docs/closure.

## 8. Failure and contention semantics

- policy store failure: do not claim a setting changed;
- upstream AWAY/NICK enqueue refusal: expose diagnostic and retry only where the operation is defined as safe desired-state reconciliation;
- generation loss discards pending reclaim probes;
- slow clients cannot stall presence aggregation;
- an owner with zero active sessions but several passive sessions may become auto-away;
- an unbound admission/control session has no upstream away effect.

## 9. Tests

Include:

- manual AWAY set/clear precedence across several clients;
- active/passive transitions with draft/pre-away;
- last active detach triggers auto-away exactly once;
- first active attach clears only automatic away, not an explicit manual-away policy unless specified;
- passive history-only session does not clear auto-away;
- reconnect re-applies current presence state;
- 433 uses bounded fallback rather than timeout;
- fallback nick grammar/max-length tests;
- no environment sentinel can enter fallback nick;
- keep-nick disabled emits no reclaim traffic;
- MONITOR path reclaims preferred nick;
- non-MONITOR fallback is rate-bounded under virtual time;
- stale generation reclaim completion cannot mutate replacement generation;
- capability fingerprint remains client-independent.

## 10. Verification

Run full workspace verification, privacy/environment campaigns and virtual-time liveness tests in addition to focused presence/nick tests.

## 11. Documentation

Add presence/nickname policy architecture and update configuration/reconnect/downstream capability documentation.

## 12. Acceptance criteria

The bouncer represents Operator presence consistently across many local clients and can survive/recover nickname collisions without host-derived identity, retry storms or client-dependent upstream behavior.

## 13. Stop conditions

Stop if implementation requires ambient host identity, per-client upstream CAP negotiation, unbounded reclaim polling, or storing live current nick/away observation as durable authority.

## 14. Closure evidence

Create plans/closure/bouncer-core/022-status.md with presence transition matrix, fallback/reclaim virtual-time evidence, anonymity/fingerprint checks and M005-D readiness.
