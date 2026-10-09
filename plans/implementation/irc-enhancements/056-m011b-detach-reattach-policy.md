# Plan 056 — M011-B — Activity-Aware Detachment

Status: proposed — depends on predecessor closure
Date: 2026-10-09
Track: M011; authority Research 011, ADR-0008 and existing accepted ADRs.
Baseline: M010 work line; Plan 054 remains independently active.

## Objective and production work
Add bounded typed `relay-detached` (none/mentions/all), `reattach-on` (off/message/mention), and `detach-after` (off/bounded duration) policy with stable default-off migration. Owner-observed events drive casefold-aware activity and one coalesced timer schedule. Never infer human-readable mentions from opaque OTR ciphertext. Detach/reattach changes **local visibility**, never upstream JOIN/PART. Reattach projects observed state once, follows history privacy, preserves ClientId cursors and does not duplicate backlog. Expose typed BouncerServ administration.

## Dependencies and failure contracts
Plan 55 must have a committed evidence closure before implementation. Preserve one NetworkOwner, bounded queues, original client identities, stable upstream CAP, no generic DNS/TCP/HTTP/proxy and no user-message replay. Use versioned schema migration/rollback and refuse malformed config before any partial change. A Store failure cannot cause silent policy downgrade. Generation cancellation removes stale work and leaves no orphan tasks.

## Verification and acceptance
Tests: concurrent timeout/mention, idle burst, router reconnect, multi-client visibility and projection, encrypted/no-history buffers, slow downstream, no upstream churn. Acceptance: bounded actions and truthful replay.
Run focused tests and scripts/verify.sh full on stable and Rust 1.88 where available, plus deterministic fault campaigns and static network-boundary guards. Document limitations, platform blockers and any incomplete product-path qualification; do not claim live compatibility without permitted real-server evidence.

## Stop and closure
Security leak, memory growth, false state/IRCv3 CAP, incorrect history/cursor behavior or unauthorized egress requires numbered corrective. Close only with plans/closure/irc-enhancements/056-status.md: exact implementation commits, requirement-evidence matrix, actual commands, failure/restart review, remaining findings and registry status. Only Plan 055 is ready now.
