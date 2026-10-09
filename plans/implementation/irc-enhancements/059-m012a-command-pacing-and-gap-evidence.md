# Plan 059 — M012-A — IRC Pacing and Upstream Gap Evidence

Status: proposed — M011 closed
Date: 2026-10-09
Primary class: capability and resilience invariant
Authority: plans/research/011-irc-privacy-resilience-authentication.md; plans/subsystems/irc-privacy-resilience-roadmap.md; ADR-0008/0009 and existing ADRs.

## Objective and readiness
Deliver m012-a — irc pacing and upstream gap evidence after M011 closed. This is not a claim of completion or live-network compatibility. Current baseline: M010 Plan 054 independently active. Keep one owner per Network and I2P-only provider authority.

## Ordered production work packages
Implement a bounded, generation-owned IRC command pacer: registration/PING/PONG highest priority, next required NickServ authentication, then configured JOIN restoration and permitted service actions. Preserve established prejoin/join/postjoin ordering and intentional recovery semantics; never requeue non-idempotent PRIVMSG/NOTICE after ambiguous disconnect. Make scheduling independent of local client attachments. Independently persist bounded outage/gap metadata: observed offline/online transitions, not speculative content; distinguish local history ingestion drops and deliberate no-history. Typed gap diagnostics must redact destinations, messages and credentials.

## Failure/restart/compatibility constraints
All work uses bounded typed inputs, queue depths, timeouts, memory and fair cancellation with generation fences. No unexpected queued user-chat retransmission, false IRCv3 advertisement, silent authentication downgrade, raw I2P endpoint disclosure or new generic host outbound socket. On missing upstream features, return explicit unsupported status and continue safe baseline; on security ambiguity fail closed. Durable changes require migrations and recovery under interrupted writes.

## Verification and acceptance
Cold start with >50 desired channels, many Networks, Eggchaos timing/lost stream, expired registration, command partial-write, shutdown/cancellation, fairness and PING starvation tests. Assert ceilings and no fabricated missed messages.
Run focused tests, sh scripts/verify.sh full, and rustup run 1.88.0 sh scripts/verify.sh full if available. Record missing platform/live-network evidence as a blocker of that specific claim. Maintain static no-clearnet checks with negative/positive controls.

## Stop/closure evidence
Any incorrect upstream state, false history, leaked credential, unbounded pressure or egress-boundary violation triggers a numbered corrective. Commit plans/closure/irc-enhancements/059-status.md with implementation SHAs, test matrix, executed commands and failures, compatibility, security review, and registry update. Promote only once predecessor closure and baseline reinspection prove readiness.
