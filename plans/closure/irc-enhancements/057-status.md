# Plan 057 — M011-C Closure Status

Status: closed
Implementation commit: `2f652b7` — `feat(runtime): add bounded local watch notifications`
Closure commit: pending
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Bounded durable rules scoped to networks or stable buffers | Schema v13 adds `watch_rules`, migrated empty from v12. A network has at most 128 rules; IDs are 1–128; terms are at most 128 bytes. Scoped rules bind both canonical target and stable `BufferId`, with network/kind validation and cascade deletion. `watch_rules_round_trip_and_v12_migration_starts_empty` covers migration, persistence, scope, and redacted Debug. |
| Literal matching with IRC casemapping and OTR exclusion | Owner evaluates inbound parsed PRIVMSG/NOTICE only, after buffer resolution, with negotiated casemapping. Keyword matching is literal; sender matching is casemap-aware. Self-originated and OTR messages are excluded. `local_watch_emits_redacted_bounded_hit_metadata_and_skips_otr` covers scope, metadata redaction, and OTR exclusion; `watch_rules` unit tests cover casemapping and stable-buffer isolation. |
| Bounded/fair matching and rate behavior | Terms and rule count are capped; the message body is folded once; at most eight hits are emitted per message; two-second coalescing and a rotating rule cursor bound repeat work and avoid fixed-order starvation. Watch limiter tests include a 1000-event burst and multi-rule fairness. |
| Local authenticated rule management | Typed controller operations serialize add/delete/clear, assign bounded IDs, validate scoped buffers, persist before notifying the owner, and reconcile ambiguous Store commits. Authenticated BouncerServ exposes `WATCH LIST/ADD/DELETE/CLEAR`; `the_local_service_administers_presence_and_channel_policy` covers durable administration. |
| Redacted bounded best-effort notification | A hit sends only a fresh process-unique sequence and rule ID through the existing bounded session output queue. Message text and destination are omitted. No-consumer and queue-full outcomes increment `watch_dropped`; delivered per-session hits increment `watch_hits`. Diagnostics contain counts only. |
| No new egress or upstream behavior | Implementation uses the existing NetworkOwner and local control/session paths. It adds no subscription socket, webhook, script hook, HTTP, DNS, or network API. Existing static boundary checks are included in full verification. |

## Scope, restart, and privacy limits

Rule configuration is durable; limiter state and sequence IDs are process-local. Restart does not generate old notifications. There is no notification text storage, notification backlog, preview, or acknowledgement protocol: delivery is immediate and best effort through the already bounded per-session queue, and loss is counted. This intentionally narrows the plan's optional acknowledgement/preview language to avoid creating retained plaintext or another queue. WATCH LIST returns configured terms only to the authenticated local Operator. Rule terms and targets are redacted in Debug and diagnostics. Storage encryption has the same deployment guarantees and limits as the rest of the Store.

Only owner-observed parsed inbound messages can match; history replay, outgoing traffic, self echoes, and OTR fragments cannot trigger a hit. A scoped rule follows its stable BufferId through display-name changes and does not match a different buffer that later reuses the same name. Matching never causes JOIN/PART or upstream sends. Queue pressure cannot block the NetworkOwner.

Tests use deterministic local/fake upstreams. They do not establish deployed IRC2P/ILITA capability or a live i2pd product path. Plan 054's independent live-service evidence requirement is unchanged.

## Verification

Passed on implementation tree `2f652b7`:

- `rtk cargo fmt --all -- --check`
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk sh scripts/verify.sh full`
- `rtk rustup run 1.88.0 sh scripts/verify.sh full`
- Focused watch limiter, Store migration/round-trip, BouncerServ administration, and owner notification/privacy integration tests
- `rtk git diff --check`

Both full verification runs exited successfully and include all-feature workspace tests, repository privacy/network-boundary checks, and release fuzz smoke. Four pre-existing pinned EggChaos tests remain ignored by their annotations; the explicit pinned qualification target was not run and is not claimed. No real upstream server or live router qualification was performed.

## Registry and next-plan disposition

Plan 057 is closed. Plan 058 is promoted to ready after checking the three predecessor closure records and confirming its runtime/store integration surfaces are present. Plan 054 remains independently active pending controlled live IRC-over-i2pd product-path evidence. Plans 059 and later remain gated pending the M011 integration disposition and their own dependency checks.
