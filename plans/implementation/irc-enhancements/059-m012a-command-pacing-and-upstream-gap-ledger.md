# Plan 059 — M012-A — IRC Command Pacing and Connection-Gap Evidence

Status: closed
Date: 2026-10-09
Class: invariant + capability
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/059-status.md

## Objective
Prevent bursty post-registration JOIN/service commands and expose truthful periods when the bouncer could not observe upstream IRC traffic.

## Readiness and dependencies
M011 closed; R001 provider already exists; no R002 requirement.
Plan 058 is closed with committed M011 evidence. Fresh source review confirms a single generation-owned NetworkOwner, typed/bounded upstream command queues, ordered bounded registration actions, generation fencing, the process-wide reconnect scheduler, and sanitized owner diagnostics are available. The shared post-registration pacing policy and durable connection-gap ledger remain this plan's implementation scope. M010 live product-path evidence and i2pr R002 are independent and do not block core-only work.

## Existing code/evidence and required invariant
Existing `ReconnectScheduler` gates connection establishment but `architecture/reconnect-and-liveness.md` does not provide one shared post-registration command pacing policy.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Introduce bounded per-Network generation-scoped command pacing with separate classes for liveness/control, required authentication, JOIN recovery and optional operator setup. Keep PING/PONG exempt from starvation; never silently queue user PRIVMSG/NOTICE for ambiguous cross-generation replay.
2. Preserve dependency ordering: NickServ/pre-join actions, desired JOINs, post-join actions, fallback reclaim. Avoid command fairness starvation across many active Networks and channels.
3. Persist only high-level connection-gap intervals/dispositions and bounded monotonic ordering anchors, not untrusted timestamp guesses or private message payloads. Explicitly distinguish upstream outage from local Store ingestion loss and user-intentional no-history.
4. Expose sanitized gap status to diagnostics and clients where semantically sound without inventing protocol messages or claiming missing history content.
5. Allow schedule limits to be operator-configured only within safe bounds; do not let client attach alter upstream fingerprint/scheduling behavior.

## Failure, restart, contention, and resource semantics
Cancellation on generation change releases permits and drops unsafe commands; observed gaps close only on actual registration/stream evidence; rollback avoids 'healthy' state when PONG deadlines fail.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Mass restore of >50 channels, Eggchaos latency stalls, simultaneous Networks, priorities, timeout/fairness and cancellation, gaps around restart, clock skew and storage failure; no speculative replay; negative egress guards.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
Recovery setup is limited to ten commands per second process-wide with FIFO turn-taking across Networks; liveness/control bypass the recovery gate. Outage records use a bounded sequence and monotonic elapsed duration, are separate from Store/history loss counters, and unknown intervals across process restart remain explicitly interrupted. The production rate is a fixed safe default; no operator knob was added without a registered configuration contract.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/059-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
