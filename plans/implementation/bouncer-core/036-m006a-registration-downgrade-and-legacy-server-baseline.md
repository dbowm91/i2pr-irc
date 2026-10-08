# Bouncer Core M006-A / Plan 036 — Registration Downgrade and Legacy-Server Baseline

Status: closed

Hard dependency:

- plans/closure/bouncer-core/035-status.md

Research authority:

- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Primary class: protocol compatibility + invariant

## 1. Objective

Make upstream registration correct on both modern IRCv3 servers and older/minimal IRC servers, with no TLS or SASL assumption.

Deliver:

- graceful no-CAP registration;
- correct bare-sasl handling;
- strict distinction between optional-no-SASL and configured-required-SASL;
- explicit plain IRC-over-I2P baseline;
- bounded capability downgrade behavior;
- preserved upstream fingerprint independence from downstream clients.

## 2. Required registration modes

### Mode A — no SASL configured

The Network must be able to register when the server provides:

- full CAP 302;
- CAP but no sasl;
- CAP with a bare sasl token;
- no CAP implementation at all;
- 421 ERR_UNKNOWNCOMMAND for CAP;
- a minimal classic IRC welcome path.

No authentication is required in this mode.

### Mode B — SASL configured

Configured SASL remains required.

The Network must fail closed if:

- the server proves CAP unsupported;
- sasl is absent;
- sasl= value excludes PLAIN;
- CAP REQ sasl is NAKed;
- authentication returns 904/905/906/907;
- welcome 001 arrives without successful SASL.

Do not silently downgrade a configured SASL credential into an unauthenticated connection.

Do not repurpose the SASL secret for NickServ.

## 3. No-CAP registration state

Replace the current implicit "welcomed && cap_finished" dependency with an explicit registration capability state.

Suggested state:

- Unknown;
- Negotiating;
- Supported;
- Unsupported.

Behavior:

1. send CAP LS 302, NICK, USER as today;
2. valid CAP LS/ACK/NAK proves CAP support;
3. ERR_UNKNOWNCOMMAND for CAP proves unsupported;
4. 001 before any valid CAP response proves unsupported for this generation when no SASL credential is configured;
5. unsupported + no required SASL completes capability negotiation locally;
6. unsupported + required SASL fails registration.

Do not send CAP END to a server that never demonstrated CAP support.

A malformed CAP line is not automatically "CAP unsupported"; keep existing bounded protocol/error policy.

## 4. Bare SASL capability

IRCv3 SASL 3.2 requires clients to handle sasl without a capability value.

Required behavior:

- offered token "sasl" => mechanism set unknown, attempt PLAIN if configured;
- offered token "sasl=PLAIN,..." => PLAIN supported;
- offered token with no PLAIN => configured SASL cannot proceed;
- mechanism comparison is ASCII case-insensitive;
- capability value remains bounded under current capability-value ceiling.

Do not infer EXTERNAL or add another SASL mechanism in M006.

## 5. Capability request behavior

Non-SASL capabilities remain opportunistic.

A server NAKing a request containing optional capabilities must not terminate an otherwise usable generation unless required SASL was part of the refused set.

If necessary, split required SASL negotiation from optional foundational capability request so one optional capability cannot cause a strict auth failure by sharing a single CAP REQ.

Preferred shape:

1. request required SASL separately when configured;
2. request optional reviewed capabilities as a separate bounded request;
3. complete CAP once each required decision is settled.

Do not make requests depend on attached downstream clients.

## 6. Plain IRC-over-I2P baseline

Document and test that:

- I2pStreamProvider returns the transport stream;
- IRC runs directly over it;
- no TLS connector is required;
- no STS/HSTS-style auto-upgrade is attempted;
- lack of server TLS is not a failure;
- lack of SASL is not a failure when no SASL credential is configured.

TLS-over-I2P remains future opt-in work, not M006.

This plan must not add generic clearnet or TLS socket authority.

## 7. Failure classification

Permanent/configuration-like:

- configured SASL cannot be satisfied;
- server rejects authentication;
- invalid nickname/identity configuration.

Retryable/transient transport behavior remains owned by the existing supervisor/reconnect scheduler.

No-CAP is a compatibility property, not an error.

## 8. Required tests

Registration transcripts:

- full modern CAP + no SASL;
- no CAP response + 001;
- 421 CAP + later 001;
- bare sasl + successful PLAIN;
- sasl=PLAIN + successful PLAIN;
- sasl=EXTERNAL only + configured credential => fail;
- no sasl + configured credential => fail;
- CAP unsupported + configured credential => fail;
- optional CAP NAK without SASL => continue;
- required SASL NAK => fail;
- 001 before SASL success => fail;
- fragmented CAP LS continuation;
- cap-notify/new/del regression.

Fingerprint:

- zero/one/many downstream clients yield identical upstream request set for a fixed server offer.

Transport:

- fake I2P stream with ordinary plaintext IRC only;
- no TLS dependency/import added;
- network boundary stays green.

## 9. Work packages

A. explicit CAP support state;
B. no-CAP completion path;
C. split required SASL vs optional capability negotiation;
D. bare-sasl support;
E. failure classification/diagnostics;
F. degraded-server transcript corpus;
G. docs/closure.

## 10. Acceptance criteria

M006-A closes when an IRC server can omit CAP, TLS, and SASL entirely and the bouncer still registers correctly, while a Network explicitly configured to require SASL never silently downgrades.

## 11. Stop conditions

Stop for new planning if:

- support requires TLS implementation;
- a server needs arbitrary pre-registration raw commands;
- a second SASL mechanism becomes necessary for the baseline;
- no-CAP handling would require client-dependent upstream behavior.

## 12. Closure evidence

Create plans/closure/bouncer-core/036-status.md with:

- registration state matrix;
- SASL mechanism matrix;
- no-CAP transcripts;
- network-boundary/dependency review;
- Rust 1.88/current verification;
- explicit Plan 037 readiness.
