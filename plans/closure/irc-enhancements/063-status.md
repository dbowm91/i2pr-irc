# Plan 063 — M013-A IRC2P and ILITA Authentication Profiles

Status: closed; controlled profile contract implemented; deployed IRC2P/ILITA interoperability remains unclaimed
Implementation commit: `319ca65` — `feat(runtime): add explicit upstream auth profiles`
Closure commit: `f90e378` — `docs(plans): close Plan 063 and start M013-B`
Date: 2026-10-10

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Explicit per-Network transport and authentication profiles | `crates/store/src/model.rs` defines typed `IrcTransportProfile` and `UpstreamAuthProfile`; schema 16 stores them by durable NetworkId. Only `plain-i2p` with `none`, `nickserv`, or `sasl-plain` is accepted. TLS-over-I2P and SASL EXTERNAL are rejected before owner construction. |
| Preserve existing behavior across migration | The 15-to-16 migration maps configured SASL credentials to required SASL PLAIN, otherwise a NickServ-targeted setup action to NickServ, otherwise none. Existing Networks receive the plain-I2P transport. Migration qualification is in `crates/store/tests/qualification.rs`. |
| Operator status and edits are typed and transactional | `AUTH STATUS/SET` reports and updates non-secret profile state. SASL credential set selects required SASL PLAIN; reset clears it and returns required SASL to none. Invalid profile/credential combinations are refused. BouncerServ integration coverage is in `crates/runtime/tests/m005d_bouncer_networks.rs`. |
| Snapshots do not disclose secrets or erase profiles on old imports | Snapshot v5 exports only profile names. v1-v4 imports preserve existing profile fields; reserved profiles are rejected. Invalid SASL profile imports are preflighted before any Network write. Coverage is in `crates/runtime/src/config_snapshot.rs` tests and `crates/runtime/tests/m005h_diagnostics.rs`. |
| IRC2P-style no-SASL registration and NickServ setup remain possible | Production-owner fixtures cover a CAP-less server, NickServ setup and nick collision in `crates/runtime/tests/corrective_019.rs`. Setup actions remain post-registration and bounded. |
| Required SASL never falls back after failure | SASL requires acknowledged capability/mechanism and a configured credential. Registration-before-auth, missing support, and numerics 904-907 are terminal; tests in `corrective_019.rs` exercise refusal paths. NickServ actions are emitted only after registration succeeds, so failed required-SASL registration cannot fall through to them. |
| Scope, generation and privacy invariants | Credentials remain in the per-Network secret store; diagnostics and exports expose no credential bytes. Profile changes flow through controller persistence and owner generation replacement. No DNS, generic TCP, or other network authority was added. |
| Formatting, lint, tests and network-boundary verification | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, stable `sh scripts/verify.sh full`, and Rust 1.88 `sh scripts/verify.sh full` completed without failures. The full scripts include workspace/doc tests, static network-boundary positive/negative controls, and release fuzz smoke. Plan 062's four separately annotated Eggchaos cases subsequently passed their pinned external qualification target. |

## Security, recovery, and limits

The profile is an explicit operator choice and does not infer server policy from a destination or display name. Required SASL is fail-closed. SASL credentials remain scoped to a durable NetworkId and are not exported. Reserved TLS and EXTERNAL values cannot reach the dialer. Profile changes apply to a new connection generation; ambiguous user messages are not replayed.

Evidence is controlled fixture evidence only. No authorized live IRC2P/ILITA transcript was collected, so this closure does not claim current deployment compatibility. Plan 054 remains independently active for live IRC-over-I2P evidence. Plan 062's pinned Eggchaos product-path campaign passed; real router-outage and deployed-service behavior remain separate.

## Registry and roadmap disposition

Plan 063 is closed. Plan 064 is active for an IRCv3 feature disposition based on current primary specifications. Plan 065 may begin its bounded TLS-stack and controlled-endpoint preflight after Plan 064 closes. Plan 066 remains sequenced after the M013 dispositions. Plan 054's live-service evidence and R002's managed-app contract conditions remain independent; Plan 062's pinned Eggchaos product-path qualification has since passed.
