# Plan 055 — M011-A — Per-Buffer Retention and No-History

Status: ready
Date: 2026-10-09
Track: M011; authority Research 011, ADR-0008 and existing accepted ADRs.
Baseline: M010 work line; Plan 054 remains independently active.

## Objective and production work
Introduce typed `persistent`, `ephemeral`, and `no-history` policies for canonical (NetworkId, BufferId) with default persistent migration. Ensure every history-ingest, FTS index, legacy backlog, CHATHISTORY, SEARCH, cursor/read-marker, and detached-channel path consults one policy source. Keep ephemeral in capped RAM only and never persist/replay after restart. Stronger privacy transitions transactionally prohibit further persisted ingress before asynchronously bounded purge of existing rows/indexes and clamp cursors safely. Do not assert forensic deletion of WAL, backups, snapshots or SSD media. SQLCipher and opaque OTR remain independent.

## Dependencies and failure contracts
Existing M009/R001 library baseline and ADR-0008; no M010-E/R002 requirement. Preserve one NetworkOwner, bounded queues, original client identities, stable upstream CAP, no generic DNS/TCP/HTTP/proxy and no user-message replay. Use versioned schema migration/rollback and refuse malformed config before any partial change. A Store failure cannot cause silent policy downgrade. Generation cancellation removes stale work and leaves no orphan tasks.

## Verification and acceptance
Tests: old-schema upgrade and rollback; no-history negative SQL/FTS scans; restart+crash midpurge; detached/private buffers; simultaneous client profiles; OTR ciphertext; resource pressure; per-policy query semantics. Acceptance: no forbidden durable bytes after policy commit; bounded memory and migrations; existing live fanout unaffected.
Run focused tests and scripts/verify.sh full on stable and Rust 1.88 where available, plus deterministic fault campaigns and static network-boundary guards. Document limitations, platform blockers and any incomplete product-path qualification; do not claim live compatibility without permitted real-server evidence.

## Stop and closure
Security leak, memory growth, false state/IRCv3 CAP, incorrect history/cursor behavior or unauthorized egress requires numbered corrective. Close only with plans/closure/irc-enhancements/055-status.md: exact implementation commits, requirement-evidence matrix, actual commands, failure/restart review, remaining findings and registry status. Only Plan 055 is ready now.
