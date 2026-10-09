# Plan 055 — M011-A — Per-Buffer Retention, Ephemeral Storage, and No-History Policy

Status: ready
Date: 2026-10-09
Class: invariant + capability
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/055-status.md

## Objective
Give each canonical (NetworkId, BufferId) a typed persistent/ephemeral/no-history policy, defaulting old databases to persistent, without breaking live IRC, history integrity, or encryption boundaries.

## Readiness and dependencies
M001–M009, R001, ADR-0008; no M010-E or R002 dependency for core work.
Only Plan 055 is currently marked ready. Promoting this plan to ready requires proof its named predecessor is closed and a fresh source/dependency check. Separate M010 live product-path blocker does not falsely mark these core-only designs closed.

## Existing code/evidence and required invariant
Current Store retains chat in SQLite+FTS5; history journal uses per-ClientId cursors; detached buffers are ingested; opaque OTR messages can be retained as ciphertext. No per-buffer retention exists.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Define typed `HistoryPrivacyPolicy` with `persistent`, `ephemeral`, `no-history`, bounded optional age/count/byte ceilings and explicit default inheritance; ensure channel/PM identities use correct IRC casemapping and stable BufferId.
2. Add transactional schema migration with versioned fixtures and rollback, record explicit overrides and preserve existing data/cursors. A newly created unknown buffer adopts the chosen policy before first history ingest.
3. Insert a policy gate **before durable ingestion and FTS derivation**. For no-history avoid appending event, search terms, metadata-bearing payload or ciphertext anywhere durable. For ephemeral use bounded memory-only ring, explicit zero-on-eviction and process-exit loss; never claim persistence/CHATHISTORY on restart.
4. Update legacy backlog, CHATHISTORY, SEARCH, read marker/cursor paths and detached-channel ingest; define truthful empty/refusal/ephemeral responses and no fake upstream msgid.
5. Policy transitions to stricter modes schedule bounded transactional purge of SQLite rows+FTS+marker/cursor references (including per-buffer accepted-event count); crash/restart resumes outstanding purge. Existing copies in WAL/backups/snapshots cannot be guaranteed physically erased.
6. Keep live fanout, liveness and upstream message routing independent from history policy; no extra user-chat replay. Guard against reindexing during migration or import of no-history rows.

## Failure, restart, contention, and resource semantics
Storage queue overload must not fall back to unencrypted/unbounded history. Ephemeral limits cannot starve upstream control. Crash between policy commit and purge leaves a pending state that refuses new durable writes and resumes deletion. Reject malformed/unknown policy values without changing existing policy.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Versioned old→new schema fixture matrix; per-buffer persistence/FTS negative tests; OTR ciphertext path; detached buffers; per-profile cursor clamping; crash during purge; repeated low-memory ingest; slow Store; server-time/msgid; restart loss of ephemeral; rollback; static no-egress guards.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
No-history writes no payload/FTS after policy commit; ephemeral is strictly memory-bounded; persistent previous defaults preserved; every query/projection uses identical policy; privacy guarantees state limitations honestly.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/055-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
