# ADR-0008 — Per-Buffer Privacy and Local Alert Policy
Status: accepted for planning (2026-10-09); implementation requires evidence-backed closure.
Authority: ADR-0002, ADR-0003, ADR-0006, Research 011.

## Context
The store durably indexes IRC payloads even for detached channels. SQLCipher encrypts the whole store but does not prevent persistence or backups; opaque OTRv3 payloads are ciphertext, not non-retention. Multiple local clients share one Operator but have different ClientIds and history cursors.

## Decision
1. Introduce an explicit per-BufferId durable policy with modes `persistent`, `ephemeral`, `no-history`; migration defaults to `persistent` preserving current behavior. Optional bounds for retained count, age and bytes are enforced by Store, not merely runtime. Policy identifies a channel/PM buffer canonically using existing IRC case mapping and NetworkId; unknown buffers use a conservative documented per-Network default. Changing to `no-history` is prospective and triggers bounded cleanup of retained events, FTS terms and affected cursor/marker metadata.
2. `ephemeral` uses a fixed-memory, per-network/per-buffer budget held outside SQLCipher/SQLite. It is never exported, indexed, backed up, copied into durable diagnostic output, or promised across restart. `no-history` records no chat payload at all. Live delivery remains unaffected. Preserve user-command non-replay and truthfully distinguish a disconnected upstream gap from intentional non-retention.
3. Retention, deletion and query surfaces (legacy replay, CHATHISTORY, SEARCH, read markers, detached channels, cursors, OTR ciphertext, snapshots) must implement one consistent policy. Never claim physical erasure from WAL/SSD/snapshots/backups; disclose durability limits.
4. Detach/reattach automation is a presentation policy, never permission to issue an upstream PART/JOIN. Default automated relay, reattach and timers disabled on migration. Detached-channel data is only retained as permitted by the buffer's history policy.
5. Watch/match notification actions are typed and local-only. The bouncer never opens a web push, email, HTTP, webhook, executable/script host or new network path. All rule counts, match work, pending events, per-client fanout and notification outputs have strict bounds and secret-content controls. Never interpret OTR ciphertext as plaintext.
6. No change to trusted Operator boundary or ClientId lineage. Explicit per-client visibility and notification acknowledgement must not leak one profile's private state to another.

## Rejected alternatives
Encryption alone as privacy policy; unbounded regex/notifications; silently clearing FTS while retaining history; treating redaction as forensic erasure; automatic notifications via external services; event-driven upstream JOIN/PART.

## Acceptance
Implement Plans 055–058 with schema upgrade fixture matrix, restart/crash/purge tests, no-history ingestion denial, count/budget assertions, OTR opaque handling, multiple downstream profiles, and static outbound-authority guards. Supersede via new ADR, do not edit this decision to hide defects.
