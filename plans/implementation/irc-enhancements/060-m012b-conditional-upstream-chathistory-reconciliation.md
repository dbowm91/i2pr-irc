# Plan 060 — M012-B — Optional Upstream CHATHISTORY Catch-Up

Status: proposed; feature research-gated
Date: 2026-10-09
Class: capability + security invariant
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/060-status.md

## Objective
Retrieve missed upstream messages only when the active IRCd advertises compatible CHATHISTORY and the Network has a verifiable history anchor.

## Readiness and dependencies
Plan 059 closed; initial implementation can use exact-wire test server, live compatibility claims require authorized supported server.
Only Plan 055 is currently marked ready. Promoting this plan to ready requires proof its named predecessor is closed and a fresh source/dependency check. Separate M010 live product-path blocker does not falsely mark these core-only designs closed.

## Existing code/evidence and required invariant
The current local CHATHISTORY server adapter answers downstream from bouncer Store; it does not imply upstream IRCd supports replay. IRC2P/ILITA capabilities must not be presumed.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Preflight detailed IRCv3 CHATHISTORY/capability negotiation spec, server access control, supported reference types, batch/tag semantics and distinction between local and upstream cursor authority. Write decision findings before feature code.
2. Add optional fixed upstream CAP request policy independent of attached clients, invoked only when offered; no automatic command to unsupported/no-CAP servers; configured required authentication remains fail-closed.
3. Use bounded per-buffer/Network catch-up jobs and pagination, identify a reliable last-seen anchor, propagate server msgid/time without inventing one; report irrecoverable gaps explicitly.
4. Deduplicate against Store using (network, buffer, trusted msgid or anchored bounded fallback), reject cross-buffer ambiguity, order by local event ID, retain source timestamps as metadata. Never re-send outgoing PRIVMSG, re-run perform, or fake confirmation.
5. No-history and ephemeral buffer policies remain authoritative; skip/purge as appropriate without reconstructing forbidden retained data.
6. Serve live traffic while recovery runs; limit bytes, pages, memory, requests, time and retry budget; suppress catch-up if server semantics/protocol do not safely support it.

## Failure, restart, contention, and resource semantics
Unsupported/no-CAP, insufficient source history, missing anchors, server NAK, malformed BATCH or time skew yield typed 'gap unresolved' status; no guessing and no negative impact on ordinary IRC session.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Spec fixture including after/before/around, BATCH and msgid duplicates; unsupported IRCd; partial frames; outage during catch-up; server lost history; privacy modes/FTS; deterministic two-client history; rate/byte bounds.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
Recovery only when directly negotiated and authenticated; otherwise graceful unchanged operation and truthful gaps. Live feature claims conditional on actual supported server evidence.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/060-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
