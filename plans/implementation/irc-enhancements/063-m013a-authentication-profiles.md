# Plan 063 — M013-A IRC2P and ILITA Authentication Profiles

Status: proposed; Plan 062/M012 closure required.
Date: 2026-10-09
Primary class: compatibility capability and authentication invariant.
Authority: Research 011, ADR-0009, existing registration/M007 security contracts.
Baseline: M010 standalone work line, Plan 054 separately active.

## Objective
Codify baseline ordinary IRC over I2P with no inner TLS requirement. Support IRC2P-style NickServ authentication without required SASL and ILITA-style explicitly configured required SASL PLAIN. Never infer current server capability from an address or network name.

## Readiness and current evidence
Existing `architecture/upstream-registration.md` handles partial/no CAP and required SASL PLAIN; M007 adds constrained phased NickServ service actions; M010 local auth is separate from upstream IRC authentication. No typed profile currently binds transport and auth policy. Before claiming deployed interoperability gather sanitized, authorized current IRC2P and ILITA CAP/registration evidence. No double-router interoperability exercise is requested.

## Invariants and ordered implementation
1. Introduce independently typed `IrcTransportProfile` (default `plain-i2p`, future explicit `tls-over-i2p`) and `UpstreamAuthProfile` (none, nickserv, sasl-plain, future sasl-external). Reject unsupported combinations before dialing; do not enable reserved TLS/EXTERNAL until Plan 065 qualification.
2. Provide explicit editable profile examples, never hardcoded servers or credentials: IRC2P plain-i2p + phased NickServ when configured, no SASL requirement; ILITA plain-i2p + SASL PLAIN only when configured and server acknowledges it. Existing legacy configurations preserve their current effective mode across upgrade.
3. Keep SASL required semantics fail-closed on missing CAP/SASL, excluded mechanism, NAK, 904-907 and registration-before-auth. No automatic NickServ/none fallback from failed SASL or conversion of NickServ to SASL because capability is advertised.
4. Scope credentials to durable NetworkId and verify the Network's configured service/auth authority; never reuse credentials between unrelated IRC endpoints. Preserve OTR/database/local Operator key separation and redact all auth material from diagnostics, errors and exports.
5. Preserve generation-local replay policy: bounded intentional setup actions in phased order, no resubmission of arbitrary prior user chat, no multiplying authentication attempts on attachment or on repeated failed registrations.
6. Extend typed BouncerServ and configuration migration with validated profile status; updates are transactional and crash-safe. Profile changes require a new generation and cannot silently continue with stale live authentication.

## Fault, restart and compatibility
Failures during CAP, SASL and NickServ phases produce precise redacted dispositions and safe backoff; never fall through to unauthenticated operation. No listener, DNS, generic TCP, SOCKS or outproxy authority is added. Wrong/missing credentials do not start a silent retry storm. Auth mode is independent from TLS and from downstream ClientId identity.

## Verification and acceptance
Test CAP-less/no-SASL IRC2P-style server with service identification and nick collision; partial CAP, CAP LS multi-line, SASL PLAIN accepted/refused, 904-907, wrong credentials, early welcome and reconnect; multiple NetworkIds and local client profiles; migration from configured existing SASL and service actions; negative proof of TLS handshake on plain baseline and generic egress paths. Run `sh scripts/verify.sh full` and Rust 1.88 verification when available; record platform or authorized-live-endpoint blockers.

Acceptance: both baseline profiles are usable without TLS, required SASL fails closed, no cross-network secret exposure, and documentation precisely separates controlled fixture behavior from current deployed IRC2P/ILITA support.

## Stop and closure
Any silent downgrade, credential leak, changed public network boundary, schema loss, or invalid state requires a numbered corrective. Close only through `plans/closure/irc-enhancements/063-status.md` with implementation SHAs, requirement-to-test evidence, commands actually executed, authentication failure matrix and registry disposition.
