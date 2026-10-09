# Plan 061 — M012-C — Verified Same-Network I2P Endpoint Failover

Date: 2026-10-09
Authority: Research 011, ADR-0008/0009 and IRC enhancement roadmap.
Baseline: M010 Plan 054 independently active; maintain I2P-only typed I2pStreamProvider, per-Network owner and no generic DNS/TCP/HTTP or proxy path.

Status: proposed; depends on Plan 060 closure and a review of same-network equivalence.
Objective: add bounded operator-authorized alternates for a single durable I2P IRC NetworkId, preserving one NetworkOwner, at most one active stream/generation, and same logical network identity.
Work packages:
1. Record researched evidence on IRC federation, services/authentication/account differences and limits to proving trust equivalence. Require explicit per-endpoint membership approval, never infer network equivalence from a .i2p name.
2. Add a bounded typed I2P endpoint set and migrate single-endpoint configs without altering behavior. Fail over by classified fault + cooldown, preserving global scheduler, cancellation, generation fencing and one active upstream.
3. Reject alternate destination credentials unless per-endpoint approval matches: SASL PLAIN, NickServ and future TLS/CertFP secrets have explicit exposure scope. Prevent sending credentials to arbitrary/unverified IRC services.
4. Reconcile per-server CAP, casemapping, channel membership and upstream history anchors; disable state-sharing/catch-up if trust equivalence or protocol identity is uncertain. Avoid cross-network retention contamination.
5. Expose sanitized endpoint choice, failover reason and cooldown in Operator diagnostics, not raw I2P Destinations or passwords.
Tests: old-schema compatibility, wrong network, all endpoints offline, alternating failures, stale generations, partial registration, credential pin mismatch, same-server recovery, resource bounds, no extra clearnet authority.
Closure: plans/closure/irc-enhancements/061-status.md with concrete tests, actual commands, commits, risk/failure analysis and registry update.

## Verification contract
Run focused protocol and failure tests, stable/MSRV full verification if available, positive and negative static network guard fixtures, migration/restart/queue-pressure checks. Preserve user-message at-most-once-on-ambiguity semantics. Unresolved security defects require numbered corrective, not weakening the goal or inaccurate closure.
