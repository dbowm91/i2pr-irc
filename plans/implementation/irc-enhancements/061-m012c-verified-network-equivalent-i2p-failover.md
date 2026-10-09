# Plan 061 — M012-C — Verified I2P Server Endpoint Failover

Status: active; operator-declared trust equivalence is required and does not establish federation fact
Date: 2026-10-09
Class: capability + security invariant
Subsystem: plans/subsystems/irc-privacy-resilience-roadmap.md
Research: plans/research/011-irc-privacy-resilience-authentication.md
Architecture: ADR-0008 and ADR-0009, plus pre-existing accepted ADRs.
Repository handoff branch: work/plans-055-066-irc-privacy-resilience
Planning baseline: standalone M010 work line (Plans 050-053 closed, 054 active).
Closure evidence location: plans/closure/irc-enhancements/061-status.md

## Objective
Support operator-approved alternate I2P endpoints for one federated IRC network without automatic cross-network trust/identity changes.

## Readiness and dependencies
Plan 060 is formally closed with production catch-up deferred because the official draft warns against production use. Fresh source review confirms `NetworkRecord` currently has one typed `I2pEndpoint`, one `NetworkOwner` reconnects through the `NetworkId`-scoped `I2pStreamProvider`, and the Store already validates configuration transactionally. Core failover can proceed only with explicit per-Network operator attestation that endpoints are trust-equivalent and credential scope is valid; the application cannot infer federation from `.i2p` names. No deployed IRC2P/ILITA equivalence is asserted. Separate M010 live product-path blocker does not falsely mark this core design closed.

Current official I2P documentation identifies a published IRC2P federation list and a separate ILITA endpoint list; Research 011 records the source and the limits of that evidence. This is sufficient to reject cross-network defaults, but not to automatically enable any alternate. Every endpoint group needs explicit operator attestation.

## Existing code/evidence and required invariant
Current durable Network has a single validated I2P endpoint; one NetworkOwner owns reconnect generations. Matching .i2p suffix does not prove equivalent network or NickServ account.
Maintain one NetworkOwner per Network, bounded memory/queues/timers, SessionId/ClientId separation, stable upstream CAP request independent of attached downstream clients, generation-fenced reconnect, no replay of non-idempotent chat, and typed I2P-only upstream authority. Preserve downstream local Operator authentication and SQLCipher/OTR separation.

### Implementation limits
The primary endpoint remains first priority. At most seven alternates are stored (eight endpoints total); alternates must be hostname or standard/extended `.b32.i2p` forms no longer than 240 bytes so the versioned config snapshot can carry one per bounded IRC line. Raw destinations remain valid for a primary endpoint but are refused as alternates. A version-15 Store migration adds two cascading tables; older rows migrate with no alternate group. Config snapshot version 4 records an explicit disabled policy or both Operator attestations plus the ordered alternates; imports of versions 1-3 preserve the stored failover policy. A failed provider attempt, timeout, protocol failure, or I/O disconnect rotates to the next endpoint on the next generation under the existing reconnect backoff. Registration rejection is terminal and never sends credentials to another endpoint. Process restart returns selection to the configured primary. Diagnostics expose only the selected priority index and fixed error class, never endpoint bytes.

## Production scope and ordered work packages
1. Research how IRC2P/ILITA server federation and authorization would actually be proven; define explicit `EndpointGroup` tied to one logical NetworkId with typed .i2p endpoints, labels, stable priority and operator-supplied trusted-network equivalence declaration.
2. Introduce bounded endpoint list and deterministic/manual failover policy triggered only by classified failures and cooldown; one active endpoint/stream per Network generation and shared NetworkId identity.
3. Preserve per-endpoint TLS pin/auth trust policy rather than assuming one cert covers all; never send one network's SASL/CertFP credentials to an unverified alternate. Require specific equivalence and credential authorization before dialing or selecting an alternate.
4. Account for service-prefix, advertised capabilities, network/channel case mapping, history-server identity and user account differences; disable shared state/catch-up when equivalence cannot be established.
5. Expose sanitized currently selected endpoint index, last error class and cooldown; no raw Destination in broadcast diagnostics.
6. Preserve same I2P stream provider and loopback SAM; no generic dialer, clearnet fallback, DNS or port parsing.

## Failure, restart, contention, and resource semantics
All endpoints fail => controlled offline/backoff without fallback to unrelated host. A mismatch or untrusted endpoint is a configuration refusal. Credential scope/pin mismatch ends attempt before IRC registration. Rollback preserves original single-endpoint config.
Explicitly define ceilings before implementation, account for overflow, and preserve prior behavior for existing profiles on migrations.

## Compatibility, security, and documentation
No host DNS, clearnet outbound, arbitrary proxy, HTTP/webhooks, DCC, or generic ZNC script/module host. Preserve existing CAP/no-CAP/SASL PLAIN behavior and operator-controlled authentication where applicable. All new secrets and diagnostics require structural redaction. Update relevant architecture reference, README, config docs, research findings and roadmap status when evidence exists; do not rewrite historical closure claims.

## Verification and adversarial evidence
Typed endpoint validation, 100% failure rotation, same NetworkId one owner, no parallel hidden streams, wrong-network transcript/fingerprint, split-brain, mismatched TLS pin, service auth, stale generation, bounded attempts; migration fixtures.
Run focused tests, `sh scripts/verify.sh full`, and `rustup run 1.88.0 sh scripts/verify.sh full` where installed; document environment blockers instead of claiming green. Cover deterministic partial I/O, router stall/restart and low-resource SBC budgets. Real-world server capability claims require permitted/sanitized live IRC transcripts; a fake test server cannot prove IRC2P/ILITA deployments.

## Acceptance criteria
Failover only for explicitly authorized trust-equivalent endpoints; no unintended credential disclosure or cross-network state mixing.

## Stop conditions and closure record
Stop and register a numbered corrective on unbounded behavior, material anonymity/credential leak, unauthorized connector, false IRCv3 advertisement, incorrect history integrity, or incompatible migrations. Produce `plans/closure/irc-enhancements/061-status.md` with exact implementation and closure SHAs, test names, commands actually executed, requirement-to-evidence matrix, security and restart analysis, upstream capability qualifications, open findings and registry/roadmap disposition. Do not claim closure until evidence is committed.
