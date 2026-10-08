# Bouncer Core M007-A / Plan 039 — Phased Service Actions for Non-SASL Authentication and Recovery

Status: closed

Hard dependency:

- plans/closure/bouncer-core/038-status.md

Research authority:

- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Primary class: durable policy + security

## 1. Objective

Support IRC networks that authenticate identity through NickServ/services rather than SASL without parsing service prose or introducing arbitrary raw perform commands.

Extend the existing constrained RegistrationAction model with explicit execution phases.

## 2. Non-goals

Do not:

- parse NickServ notices/prompts;
- assume Anope, Atheme, or one services package;
- automatically reuse SASL credentials;
- support raw arbitrary commands;
- execute shell/plugins;
- auto-detect service names;
- claim that sending IDENTIFY proves authentication succeeded.

This is an operator-configured setup/recovery mechanism.

## 3. Action phases

Introduce:

- PreJoin;
- PostJoin;
- FallbackRecovery.

### PreJoin

Runs after upstream registration completes and before desired JOIN replay.

Use case: IDENTIFY or other service login that should precede joining channels.

### PostJoin

Runs after desired JOIN replay.

This preserves current Plan 027 behavior.

All existing durable RegistrationAction rows migrate to PostJoin.

### FallbackRecovery

Runs once per generation only when:

- registration completed under a generated fallback nick;
- keep_nick is enabled;
- at least one FallbackRecovery action is configured.

Use cases: operator-specified RECOVER, RELEASE, GHOST, or service-specific recovery sequence.

This phase does not itself assert that the preferred nick became available.

## 4. Allowed actions

Retain the existing closed typed action authority.

Permitted classes remain narrowly bounded:

- service-targeted PRIVMSG;
- service-targeted NOTICE if already supported by the action type;
- self MODE only where existing model permits it.

Do not add NICK/JOIN/PART/QUIT/CAP/AUTHENTICATE/OPER/raw line variants.

For FallbackRecovery, prefer service message actions only; self MODE has no identity-recovery purpose.

## 5. Durable schema

Add an action phase column/enum.

Migration:

- existing rows => PostJoin;
- no action duplication;
- stable ordering within each phase;
- total actions and total bytes remain bounded by existing ceilings unless a measured reason requires a new reviewed bound.

Schema migration must be transactional and tested from schema 7.

Config snapshot import/export must represent phase without exposing payload secrets.

## 6. Secret handling

Registration-action payload remains secret-classified/redacted.

Requirements:

- Debug redacted;
- diagnostics contain counts only;
- config snapshot continues to omit secret payload;
- error messages never echo action text;
- closure fixtures use synthetic non-secret values.

No database encryption is added here; that belongs to the later privacy/encryption milestone.

## 7. Generation execution order

Required sequence:

1. IRC registration succeeds;
2. PreJoin actions;
3. desired JOIN replay;
4. PostJoin actions;
5. if current nick is a generated fallback and keep_nick enabled, FallbackRecovery actions;
6. normal online loop.

A generation that dies during any phase:

- never resumes at the failed action index;
- next successful generation starts its applicable phase from the beginning;
- no downstream user traffic is replayed.

This is intentional setup replay.

## 8. Queue/failure semantics

Action enqueue/send failure:

- bounded;
- visible in diagnostics;
- terminates or degrades the setup phase according to existing registration-action semantics;
- never silently claims success.

FallbackRecovery actions execute at most once per generation. Reclaim retries later in the same generation do not replay service recovery actions automatically.

## 9. Operator surfaces

Extend BouncerServ/control/config syntax with explicit phase.

No free-form trigger expressions.

Example conceptual configuration:

- phase=pre-join target=NickServ text=IDENTIFY ...
- phase=fallback-recovery target=NickServ text=RECOVER ...

Do not bake these literal services commands into production defaults.

## 10. Tests

- schema 7 -> new schema migration;
- old actions become PostJoin;
- PreJoin before first JOIN;
- PostJoin after JOIN;
- FallbackRecovery only under fallback + keep_nick;
- no FallbackRecovery when preferred nick obtained;
- FallbackRecovery once per generation;
- reconnect intentionally replays applicable setup phases;
- action failure/redaction;
- config snapshot phase roundtrip without payload;
- forbidden command matrix remains closed;
- several clients cannot trigger duplicate action phases.

## 11. Acceptance criteria

Plan 039 closes when an Operator can configure safe service authentication/recovery sequencing for a non-SASL network without service-text parsing, raw command execution, or secret leakage.

## 12. Closure evidence

Create plans/closure/bouncer-core/039-status.md with:

- migration matrix;
- phase ordering transcript;
- security/deny matrix;
- redaction evidence;
- reconnect replay matrix;
- Plan 040 readiness.

Closure record: `plans/closure/bouncer-core/039-status.md`.
