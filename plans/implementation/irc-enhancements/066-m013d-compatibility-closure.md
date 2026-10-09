# Plan 066 — M013-D IRC Interoperability and Authentication Closure

Status: proposed — depends on Plans 063–064 closure and Plan 065 shipped-or-deferred disposition.
Date: 2026-10-09
Authority: Research 011, ADR-0009 and IRC enhancement roadmap.

## Objective and work
1. Qualify standard non-TLS IRC over I2P with two explicit authentication profiles: IRC2P-style NickServ/no mandatory SASL, and ILITA-style operator-configured SASL PLAIN. Preserve strict configured SASL failure behavior and correct client state/history on restart.
2. Where permission and external service access exist, record sanitized current IRC2P/ILITA CAP and authentication behavior. Otherwise clearly distinguish controlled-fixture support from deployed-network validation.
3. Record the accept/defer status of CHGHOST, event playback and redaction based on Plan 064 spec review and conformance tests.
4. If Plan 065 shipped, prove inner TLS peer identity verification, certificate scoping and SASL EXTERNAL strictly inside I2P. If it was research-deferred, explicitly omit claims that EXTERNAL or TLS is supported.
5. Reconcile README, roadmap, ADR references, architecture docs and registry. Preserve M010 Plan 054 and R002 statuses unless independently changed.

## Test and closure
Run complete stable and Rust 1.88 verification where available; mixed legacy/modern CAP clients, authenticated/unauthenticated network profiles, connection stalls and restarts, local privacy policy, and no-clearnet static guard controls. Auth downgrade, key disclosure, false CAP advertisement, history corruption or resource unboundedness block closure and trigger a corrective.
Commit plans/closure/irc-enhancements/066-status.md with exact commit/test evidence, required commands, unsupported features, platform/live evidence limitations and the final registered status.
