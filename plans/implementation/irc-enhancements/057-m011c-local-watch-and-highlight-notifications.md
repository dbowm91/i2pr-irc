# Plan 057 — M011-C — ZNC-Style Local Watch and Highlight Rules

Status: ready
Date: 2026-10-09
Class: capability + security invariant
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/057-status.md

## Objective
Offer local-only typed mention/keyword/sender watches usable by future i2pr console without a scripting, HTTP or push-delivery host.

## Readiness and dependencies
Plan 056 closed; Plan 055 privacy gating available.
Plan 056 closed in implementation commit `2ebf576`. Fresh source check confirms owner-observed parsed channel messages, negotiated casemapping, OTR detection, per-buffer privacy policy, stable ClientId cursors, and authenticated bounded BouncerServ/session control paths are available. There is no existing event-driven watch service or notification queue; those are this plan's implementation scope. M010 live product-path qualification is independent and does not block this core-only work.

## Existing code/evidence and required invariant
History FTS5 search is indexed; there is no event-driven rule service or local notification queue. Existing downstream sessions and BouncerServ are already authenticated.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Define bounded watch rules with NetworkId/BufferId scopes, sender/nickname/keyword match shapes, explicit IRC casemap and literal/bounded-regex cost limits. Disable capture or inspect of OTR payloads and other opaque crypto fragments.
2. Evaluate once on owner-observed inbound events with clear duplicate/cross-attachment semantics; do not trigger on history replay, outgoing ambiguous traffic or upstream catch-up duplicates without deterministic deduplication.
3. Expose local notification events via existing typed control/session channels, including fresh sequence IDs, redacted default payloads and bounded optional previews subject to buffer privacy policy.
4. Support durable rules and bounded acknowledgement per stable ClientId where appropriate, while never persisting no-history content, notification text, derived plaintext search terms or unbounded backlog.
5. Match rate limits, queue overflow/disposition counters, coalescing/dedup, and fair fanout so slow downstreams cannot pressure network owners. Auth failure and unauthenticated sockets cannot subscribe.
6. Do not add network-backed notifications, SMTP, Web Push, URL previews, arbitary scripts, shell hooks, filesystem exports or a second local daemon.

## Failure, restart, contention, and resource semantics
Rule parser reject malformed inputs early. On rule changes or restart, do not replay old unacknowledged matched plaintext as a new message. Event drop is counted and exposed without secret content. Loss of consumer never affects IRC operations.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Rule grammar fuzz, 1000-match bursts bounded, simultaneous clients/ClientId isolation, privacy modes, OTR, malformed Unicode/casemapping, coalescing, restart, no replay, no-egress positive static fixtures.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
Usable local highlight/watch with bounded tasks/storage, no new network authority and no disclosure of non-retained plaintext.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/057-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
