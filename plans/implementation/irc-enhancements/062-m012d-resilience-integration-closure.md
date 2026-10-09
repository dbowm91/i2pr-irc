# Plan 062 — M012-D — Integrated Connectivity/Recovery Qualification

Date: 2026-10-09
Authority: Research 011, ADR-0008/0009 and IRC enhancement roadmap.
Baseline: M010 Plan 054 independently active; maintain I2P-only typed I2pStreamProvider, per-Network owner and no generic DNS/TCP/HTTP or proxy path.

Status: active; Plans 059-061 are closed (Plan 060 production catch-up deferred on the official draft's production warning).
Objective: qualify post-registration IRC pacing, gap evidence, conditional upstream CHATHISTORY and approved I2P endpoint failover together.
Work packages:
1. Run deterministic and Eggchaos campaigns: mass reconnect, fifty-channel restore, router blackout, partial IRC registrations, missed upstream messages, unsupported CHATHISTORY, upstream state/auth changes.
2. Prove bounded tasks/timers, no PING starvation, truthful disconnect gaps, no fabricated replay, no unbounded history acquisition, no duplicate upstream user PRIVMSG/NOTICE, and no cross-server secret exposure.
3. Test local no-history/ephemeral behavior during upstream catch-up, multisession cursors, history sorting and all failure classifications.
4. Reconcile accepted/deferred feature inventory; don't use fake-server qualification as evidence for IRC2P/ILITA live support. Preserve separate M010 Plan 054 active status and R002 blocker.
5. Run sh scripts/verify.sh full and Rust 1.88 check, capture platform/operational blockers, updates to README and subsystem roadmap, and exact commit/test evidence.
Stop if resource, auth or privacy invariant fails. Closure: plans/closure/irc-enhancements/062-status.md and registry change only on evidence.

## Verification contract
Run focused protocol and failure tests, stable/MSRV full verification if available, positive and negative static network guard fixtures, migration/restart/queue-pressure checks. Preserve user-message at-most-once-on-ambiguity semantics. Unresolved security defects require numbered corrective, not weakening the goal or inaccurate closure.
