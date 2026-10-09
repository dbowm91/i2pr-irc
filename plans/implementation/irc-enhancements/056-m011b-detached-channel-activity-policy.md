# Plan 056 — M011-B — Activity-Aware Detached Channels and Reattachment

Status: proposed
Date: 2026-10-09
Class: capability + security invariant
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/056-status.md

## Objective
Add soju-inspired activity and notification-based detach/reattach while preserving upstream membership and per-client state fidelity.

## Readiness and dependencies
Plan 055 closed; existing M005 detached-channel owner model remains authoritative.
Only Plan 055 is currently marked ready. Promoting this plan to ready requires proof its named predecessor is closed and a fresh source/dependency check. Separate M010 live product-path blocker does not falsely mark these core-only designs closed.

## Existing code/evidence and required invariant
M005-B already has a durable detached flag with PART :detach semantics; passive/active downstream presence and fixed owner generation are implemented.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

## Production scope and ordered work packages
1. Add bounded typed policies for `relay-detached` (none/mentions/all), `reattach-on` (off/message/mention), `detach-after` (off or bounded duration), and explicit observed activity criteria. Defaults are off/no behavior change on migration.
2. Evaluate activity on canonical owner-observed message events after IRC parsing; honor channel casemap and exact nick mention boundaries. Never treat OTR ciphertext fragments as human-readable text.
3. Keep one owner-owned monotonic timer per bounded channel policy and a heap/timer wheel rather than per-message unbounded tasks; input rate cannot force repeated upstream commands.
4. A detach is only a local visibility policy; neither timeout nor mention sends JOIN/PART to server. Reattach produces one consistent downstream projection followed by privacy-eligible history without cursor duplication.
5. Define simultaneous profile notification behavior and cross-client reattach scope; avoid silently changing existing per-client/private cursors or Operator-wide network presence.
6. BouncerServ exposes bounded typed inspect/set/disable controls and diagnostics that avoid message bodies, private destinations and usernames.

## Failure, restart, contention, and resource semantics
On generation reset, rebuild timers from durable policy and fresh monotonic state; never reuse stale observed activity as proof; if visibility transitions fail to commit, do not publish them; flood matches coalesce with caps.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Detached visibility vs upstream membership, single/multi-client, initial replay state, simultaneous mention+timeout, partial IO/queue pressure, cancellation and restart, nick casemap, OTR uninterpreted, memory bounds, no repeated JOIN/PART.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
Timers and matching are bounded; no accidental upstream membership change; attach projection/read state remain exact under disconnection, multiple clients and upgrades.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/056-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
