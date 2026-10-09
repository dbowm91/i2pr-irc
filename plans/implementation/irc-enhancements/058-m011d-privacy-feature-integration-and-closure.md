# Plan 058 — M011-D — Integrated Privacy Policy Qualification and M011 Closure

Status: active
Date: 2026-10-09
Class: qualification + closure
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/058-status.md

## Objective
Verify the complete privacy/visibility/watch system against adversarial traffic, upgrades, router faults and multi-client state, then close M011 only on recorded proof.

## Readiness and dependencies
Plans 055–057 closed with evidence; M010 integration only needed for the specific product-path claims being made.
Plans 055-057 are closed with committed evidence at their named closure records. Fresh source review confirms the Store/privacy policy, detached activity, owner-observed watch handling, per-ClientId cursors, and bounded fake-upstream integration surfaces needed for this cross-feature qualification are present. M010 live product-path evidence is independent and does not block core-only testing or closure.

## Existing code/evidence and required invariant
Independent subsystem passing tests do not prove cross-subsystem closure, as M005-I found with reconnect scheduler/snapshot defects.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Build requirement-to-evidence matrix for ADR-0008 and Plans 055–057; enumerate invalid/unsupported combinations.
2. Run integrated two-profile/three-buffer scenarios mixing persistent, no-history, ephemeral, OTR ciphertext, detach/reattach and watch rules during high-latency I2P-path simulation.
3. Prove storage migration, FTS leakage absence, purging/wal caveat, crash/restart, no-history no-index, and failure during DB write/notification fanout.
4. Prove upstream capability fingerprint independence from local attachments, no direct path, no hooks, no network API beyond existing provider; explicit negative/positive static guards.
5. Reconcile README, architecture, roadmap/registry, close Plan 058 in plans/closure/irc-enhancements/058-status.md with commit ranges, executed verification, reproducible fixtures and residual risks.

## Failure, restart, contention, and resource semantics
Any history leak, unbounded allocation, duplicated or fabricated IRC state, or changed egress permission requires a numbered corrective. No retroactive rewriting of closure tests.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Full verify on current toolchain and Rust 1.88; deterministic fault campaigns; Linux/macOS where possible; bounded memory/queue stress; schema fixture chain; cross-client privacy matrix.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
All M011 requirements evidenced and plan registry updated; unsupported live-service claims explicitly omitted.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/058-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
