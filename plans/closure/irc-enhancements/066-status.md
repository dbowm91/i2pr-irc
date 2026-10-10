# Plan 066 — M013-D IRC Interoperability and Authentication Closure

Status: closed; controlled IRC-over-I2P authentication profiles implemented, optional/draft features accurately deferred
Implementation commits: `319ca65` — Plan 063 authentication profiles; no implementation for deferred plans 064/065
Closure commit: pending
Date: 2026-10-10

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Standard non-TLS IRC over I2P profiles | Plan 063 implements per-Network `plain-i2p` with explicit none, NickServ, or required SASL PLAIN. Model/store qualification covers schema 16 migration; BouncerServ and snapshot tests cover explicit edits, persistence and secret-free export. |
| Fail-closed configured SASL and restart/profile semantics | `crates/runtime/tests/corrective_019.rs` covers required SASL negotiation/refusal and terminal 904-907 responses; `m005h_diagnostics.rs` covers invalid snapshot preflight before writes; profile changes use transactional controller persistence and owner generation replacement. No NickServ or unauthenticated fallback follows required-SASL failure. |
| Current deployed IRC2P/ILITA behavior | No authorized live endpoint or sanitized current CAP/auth transcript was available. Support claims remain limited to controlled fixtures; deployed service compatibility is unqualified. Plan 054 remains active for live product-path evidence. |
| CHGHOST, event playback and message-redaction disposition | Plan 064 closes with all three deferred. CHGHOST's legacy membership fallback conflicts with the projection invariant; event-playback and redaction remain draft and the current store lacks event/redaction provenance. No corresponding capability was added. |
| TLS-over-I2P and SASL EXTERNAL disposition | Plan 065 closes with research deferral. Rustls/tokio-rustls fit the MSRV and can wrap the provider's async stream, but no controlled endpoint, per-Network peer-trust identity, or client-key provisioning contract exists. Typed TLS/EXTERNAL values remain rejected. |
| Full verification and network-boundary controls | After the Plan 063 implementation, stable `sh scripts/verify.sh full` and `rustup run 1.88.0 sh scripts/verify.sh full` completed through workspace/doc tests, static positive/negative network-boundary controls and release fuzz smoke without failures. The full scripts include formatting, Clippy with warnings denied, and all-feature tests. The four pinned Eggchaos cases remain ignored as documented by Plan 062; its external campaign has not run. No source code changed after these verification runs. |
| Documentation and registry alignment | README, upstream-registration/storage/operator-surfaces architecture, Plan 063-066 handoffs, M013 roadmap and `plans/registry.md` now distinguish implemented profiles, draft/spec deferrals, research deferral and outstanding external evidence. No claim of TLS, EXTERNAL, CHGHOST, event playback, or redaction support is made. |

## Security, recovery, and limits

Implemented profiles remain plain IRC over the typed I2P provider. Authentication policy is explicit and scoped by durable NetworkId; secrets do not appear in exports or status; required SASL is fail-closed. No generic DNS/TCP, clearnet route, HTTP, SOCKS, DCC, TLS fallback or automatic auth downgrade was introduced. Plan 061 failover remains operator-attested and does not claim deployed endpoint equivalence.

M013 does not establish live IRC2P/ILITA compatibility, TLS/EXTERNAL support, draft feature readiness, or router-blackout qualification. Plan 054 live IRC product-path evidence, Plan 062's pinned Eggchaos campaign, and R002's managed-app contracts remain independently open. Reopen only through a new bounded plan when its documented prerequisites are met.

## Registry and roadmap disposition

Plan 066 is closed and M013 is dispositioned. Plan 063 is implemented; Plan 064 is closed with CHGHOST/event-playback/redaction deferred; Plan 065 is closed with TLS/EXTERNAL research-deferred. The M013 work line has no active successor. External evidence conditions on Plans 054 and 062 remain active and separate.
