# Plan 057 — M011-C — Local Watch, Keyword and Highlight Rules

Status: proposed — depends on predecessor closure
Date: 2026-10-09
Track: M011; authority Research 011, ADR-0008 and existing accepted ADRs.
Baseline: M010 work line; Plan 054 remains independently active.

## Objective and production work
Provide typed local-only sender/nick/keyword/mention watches with strict rule count, pattern complexity, channel scope, IRC casemapping and cost ceilings. Evaluate inbound live events once, not history replay; suppress duplicate/unsafe catch-up and opaque crypto payload matches. Emit bounded authorized local notifications via existing control/session channel; default payload redacted and no persistence of forbidden private content. Keep per-profile delivery/ack independent; loss/drop is counted, never backpressures NetworkOwner. Explicitly exclude HTTP Web Push, mail, webhooks, scripts and arbitrary shell hooks.

## Dependencies and failure contracts
Plan 56 must have a committed evidence closure before implementation. Preserve one NetworkOwner, bounded queues, original client identities, stable upstream CAP, no generic DNS/TCP/HTTP/proxy and no user-message replay. Use versioned schema migration/rollback and refuse malformed config before any partial change. A Store failure cannot cause silent policy downgrade. Generation cancellation removes stale work and leaves no orphan tasks.

## Verification and acceptance
Tests: malformed/hostile patterns, match storms, single/multiple ClientIds, no-history & OTR, replay non-duplication, denial of unauthenticated subscriptions, no-egress guard. Acceptance: no new network authority or unbounded queues.
Run focused tests and scripts/verify.sh full on stable and Rust 1.88 where available, plus deterministic fault campaigns and static network-boundary guards. Document limitations, platform blockers and any incomplete product-path qualification; do not claim live compatibility without permitted real-server evidence.

## Stop and closure
Security leak, memory growth, false state/IRCv3 CAP, incorrect history/cursor behavior or unauthorized egress requires numbered corrective. Close only with plans/closure/irc-enhancements/057-status.md: exact implementation commits, requirement-evidence matrix, actual commands, failure/restart review, remaining findings and registry status. Only Plan 055 is ready now.
