# Bouncer Core M007-B / Plan 040 — Preferred-Nick, Reconnect, and Multi-Client Identity Resilience

Status: closed

Hard dependency:

- plans/closure/bouncer-core/039-status.md

Research authority:

- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Primary class: identity state machine + resilience

## 1. Objective

Make preferred-nick behavior robust across transient collisions, server splits/reconnects, non-SASL service workflows, manual NICK commands, and simultaneous downstream clients.

Build on Corrective 035 and Plan 039 rather than adding a second nick state machine.

## 2. Registration collision classification

Handle as transient availability conflicts:

- 433 ERR_NICKNAMEINUSE;
- 436 ERR_NICKCOLLISION;
- 437 when the refused resource is the attempted nickname and the server is using it as temporary nick unavailability.

Handle as permanent/configuration error:

- 432 ERR_ERRONEUSNICKNAME;
- locally invalid configured identity.

Do not parse numeric reason text to decide class.

## 3. Fallback exhaustion

Current NickExhausted is terminal forever until configuration changes.

Replace that behavior for transient collisions.

Required policy:

- bounded deterministic fallback sequence remains;
- if every fallback candidate is refused, end the generation;
- schedule retry using a dedicated collision cooldown under the existing global reconnect scheduler;
- do not immediately cycle through the same candidates again;
- do not mark Network permanently terminal solely because nicks are currently occupied.

Recommended collision retry floor: 15 minutes, with Network-specific deterministic jitter and the existing global scheduler still governing connect admission.

Implemented cooldown: 15 minutes plus deterministic positive jitter up to three minutes. The delay is a pure duration helper, so boundary behavior is directly testable.

The exact cooldown must be injected/testable.

## 4. Reclaim refusal while online

When a keep-nick NICK attempt receives 433/436/qualified-437:

- the online generation remains healthy;
- record the refusal;
- clear immediate free evidence;
- return to MONITOR/ISON waiting;
- apply a reclaim cooldown before another NICK attempt;
- do not consume the entire per-generation write budget in a tight burst.

A malformed/invalid preferred nick remains a configuration error rather than endless reclaim.

## 5. Availability signals

After Corrective 035:

- 731 preferred nick offline => immediate reclaim evidence;
- 730 preferred nick online => no write;
- 303 preferred absent => immediate evidence;
- QUIT/NICK observations involving the preferred nick may be used as additional bounded evidence only if they are based on observed protocol identity, not human-readable quit reasons.

Do not parse netsplit text.

## 6. Explicit local NICK semantics

A registered local client may send NICK.

Network-wide semantics:

- forward as NonReplayable;
- if requested nick differs from durable preferred nick, suspend automatic preferred-nick reclaim for the remainder of this generation;
- manual NICK does not rewrite durable configured preferred nick;
- other attached sessions receive the authoritative server NICK frame normally;
- if requested nick equals preferred nick, normal reclaim state may continue.

On the next generation the durable preferred nick is again the configured target.

This avoids the bouncer fighting an Operator's explicit live action while preserving durable configuration.

## 7. Preferred-nick local registration alias

Current downstream admission requires the local client to claim the exact current upstream nick.

Permit exactly two valid local claims:

- current observed Network nick;
- durable configured preferred nick.

The preferred alias is accepted only when current observed nick is a known generated fallback for that same Network/generation.

After accepting the preferred alias:

- the session's local registration must converge immediately to the actual observed nick;
- emit a protocol-correct local NICK transition or equivalent projection before ordinary live state;
- subsequent SessionReader identity uses the observed nick;
- no session is left believing the upstream currently holds the preferred nick.

Arbitrary aliases remain refused.

## 8. Multi-client nick transition behavior

When the Network nick changes:

- all attached sessions receive one authoritative transition;
- response routes remain SessionId-scoped;
- history/search state remains Network-wide;
- passive/active status does not affect nick authority;
- a slow session that cannot accept the transition follows existing desynchronization detach policy;
- healthy sessions continue.

One client's explicit NICK affects the shared Network identity; no per-session nick exists.

## 9. Reconnect/split behavior

Treat network splits through observable protocol/transport events only.

Scenarios:

- transport EOF/reset => generation replacement;
- preferred nick occupied after reconnect => fallback;
- preferred nick becomes free later => reclaim;
- desired channels rejoin after fresh registration;
- service phases replay intentionally;
- downstream user chat never replays;
- stale generation reclaim evidence cannot act.

## 10. Diagnostics

Add bounded state sufficient to distinguish:

- preferred nick;
- observed current nick;
- fallback active;
- reclaim suspended by operator NICK;
- reclaim cooldown;
- transient collision retry vs permanent registration error;
- reclaim write/refusal counts.

No service payload or endpoint secret.

## 11. Tests

Registration:

- 433/436 fallback;
- qualified 437 fallback;
- 432 terminal;
- fallback exhaustion schedules long retry rather than terminal forever;
- retry uses global scheduler.

Online reclaim:

- corrected 731/303 evidence;
- 433 reclaim refusal leaves generation online;
- cooldown prevents tight NICK loop;
- eventual free evidence regains preferred nick.

Manual NICK:

- manual alternate nick suspends reclaim for generation;
- other client sees transition;
- next generation tries durable preferred again.

Admission:

- client using current fallback nick attaches;
- client using configured preferred alias attaches during generated fallback;
- client is immediately told actual current nick;
- arbitrary third nick refused;
- several clients converge on the same observed nick.

## 12. Acceptance criteria

Plan 040 closes when nickname occupation/split churn can no longer permanently strand the Network or cause reclaim fights, and simultaneous local clients remain truthful about the one observed upstream identity.

## 13. Stop conditions

Stop for ADR if:

- identity needs to become per-session;
- preferred aliases require lying about observed upstream state;
- NickServ prose parsing appears necessary;
- recovery requires replaying ambiguous user traffic;
- a new global mutable identity owner outside NetworkOwner is proposed.

## 14. Closure evidence

Create plans/closure/bouncer-core/040-status.md with:

- numeric/error classification;
- collision retry/cooldown matrix;
- reclaim refusal/recovery transcripts;
- local alias/multi-client matrix;
- reconnect/service-action integration;
- Plan 041 readiness.
