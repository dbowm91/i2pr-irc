# Plan 060 — M012-B Capability-Gated Upstream History Recovery

Status: proposed — Plan 059 closure required. Upstream live-support claims research-gated.
Date: 2026-10-09
Authority: Research 011; M012 roadmap; ADR-0008/0009.

## Objective and current evidence
Local CHATHISTORY currently serves the bouncer's SQLite history, not missed content while its own upstream IRC connection is down. Add safe optional server-side recovery; unsupported I2P IRC networks must operate normally.

## Ordered work
1. Record primary-spec and current network-capability findings for upstream CHATHISTORY, CAP/BATCH/msgid/timestamp, authentication and optional history grants. Do not assume IRC2P/ILITA has CHATHISTORY.
2. Build fixed owner-scoped upstream capability requests only when offered, never changed by local clients. Maintain bounded recovery work per buffer after reconnection.
3. Use retained trustworthy last-seen references for paged recovery, count max pages/events/bytes/timeouts and dedup against canonical history without inventing msgids. Preserve local HistoryEventId ordering, server-time as metadata, cursor semantics.
4. Route recovered events through per-buffer privacy policy: no-history never writes payload or FTS, ephemeral remains memory-only; encrypted OTR remains opaque.
5. Report explicit unresolved gap for unsupported/NAK/expired history/missing anchors/malformed batches; never resend user PRIVMSG/NOTICE, falsely mark user traffic delivered, or stall PING/PONG.
6. Preserve existing CAP-less and SASL-required profiles and client-independent upstream CAP fingerprint.

## Failure and evidence
Connection loss mid-recovery cancels generation tasks; bounded resumable cursor/checkpoint does not create duplicate or missing acknowledged stored events. Malformed frames are rejected with bounded allocation, no exposure of raw credentials or I2P destinations. Test supported/unsupported CAP, fragmented BATCH, duplicate msgid, skewed timestamps, no-history, OTR, reconnect and two independent downstream clients. Run full stable and Rust 1.88 verification where available; collect real capability transcript only with authorized I2P service.

## Closure
Stop on fabricated history, client leakage or unbounded resource use. Create plans/closure/irc-enhancements/060-status.md with source/commit evidence, tests executed, limitations and registry update; never claim live deployed compatibility from only a fixture.
