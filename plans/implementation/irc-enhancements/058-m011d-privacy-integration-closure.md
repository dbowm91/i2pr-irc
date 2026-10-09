# Plan 058 — M011-D — Integrated Privacy Qualification and Closure

Status: proposed — Plans 055–057 closed
Date: 2026-10-09
Primary class: qualification + closure
Authority: plans/research/011-irc-privacy-resilience-authentication.md; plans/subsystems/irc-privacy-resilience-roadmap.md; ADR-0008/0009 and existing ADRs.

## Objective and readiness
Deliver m011-d — integrated privacy qualification and closure after Plans 055–057 closed. This is not a claim of completion or live-network compatibility. Current baseline: M010 Plan 054 independently active. Keep one owner per Network and I2P-only provider authority.

## Ordered production work packages
Integrate per-buffer retention, history/FTS, detached activity, watch notifications and two stable ClientId profiles. Exercise persistent/ephemeral/no-history under process restart, mixed OTR ciphertext and plain text, retention purge, searches, concurrent detach/reattach, slow clients, queue pressure, router stalls and outage recovery. Ensure notifications never leak non-retained text, and privacy transitions have durable ordering with migration fixtures. Re-run static guards proving no network authority was added.

## Failure/restart/compatibility constraints
All work uses bounded typed inputs, queue depths, timeouts, memory and fair cancellation with generation fences. No unexpected queued user-chat retransmission, false IRCv3 advertisement, silent authentication downgrade, raw I2P endpoint disclosure or new generic host outbound socket. On missing upstream features, return explicit unsupported status and continue safe baseline; on security ambiguity fail closed. Durable changes require migrations and recovery under interrupted writes.

## Verification and acceptance
Full current+MSRV verification, bounded-resource adversarial scenarios, migration/purge crash simulation, privacy negative inspection, exact requirement-to-evidence matrix. All vulnerabilities, false history or resource bounds unresolved require corrective.
Run focused tests, sh scripts/verify.sh full, and rustup run 1.88.0 sh scripts/verify.sh full if available. Record missing platform/live-network evidence as a blocker of that specific claim. Maintain static no-clearnet checks with negative/positive controls.

## Stop/closure evidence
Any incorrect upstream state, false history, leaked credential, unbounded pressure or egress-boundary violation triggers a numbered corrective. Commit plans/closure/irc-enhancements/058-status.md with implementation SHAs, test matrix, executed commands and failures, compatibility, security review, and registry update. Promote only once predecessor closure and baseline reinspection prove readiness.
