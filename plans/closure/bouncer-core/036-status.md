# Bouncer Core M006-A / Plan 036 — Closure Status

Status: closed

Plan: `plans/implementation/bouncer-core/036-m006a-registration-downgrade-and-legacy-server-baseline.md`

## Implementation commit

- `975363a` — `feat: support legacy IRC registration without CAP`

## Requirement-to-evidence matrix

| Requirement | Evidence |
|---|---|
| Explicit CAP state and classic no-CAP welcome | `RegistrationCapState` tracks negotiating/supported/unsupported; `a_classic_server_welcomes_without_cap_end` completes on 001 without CAP END |
| 421 CAP downgrade | `unknown_cap_command_then_welcome_is_a_supported_downgrade` |
| CAP with optional capabilities but no configured SASL | `cap_without_sasl_completes_without_authentication_when_none_is_configured` |
| Optional capability NAK is non-fatal | `optional_capability_nak_does_not_abort_registration` |
| Configured SASL remains required, separate from optional CAP requests | `the_production_path_completes_a_sasl_plain_handshake`; observes separate `CAP REQ :sasl` and `CAP REQ :message-tags` |
| Bare SASL offer attempts configured PLAIN | `bare_sasl_capability_attempts_configured_plain_authentication` |
| Missing CAP/SASL, explicit EXTERNAL-only, unsupported CAP, required NAK, auth failure, and early 001 fail closed | `sasl_configured_but_not_offered_is_a_terminal_registration_error`, `configured_sasl_rejects_an_explicit_non_plain_offer`, `configured_sasl_fails_when_cap_is_unsupported`, `configured_sasl_nak_is_a_terminal_registration_error`, `a_refused_sasl_credential_is_a_terminal_registration_error`, and `welcome_before_sasl_success_does_not_complete_registration` |
| Fragmented CAP LS and downstream-independent request set | Existing production tests in `corrective_019` and `m005i_integration`; `the_upstream_registration_is_identical_for_every_client_mix` |
| Plain IRC directly over I2P, with no TLS dependency or alternate egress | `architecture/upstream-registration.md`; static `scripts/check-network-boundary.py`; dependency tree unchanged |
| CAP NEW/DEL remains generation-scoped | Existing `m005f_protocol_polish` capability-change regressions remain green |

## Verification executed

- `rtk cargo test -p i2pr-irc-runtime --test corrective_019 --locked` — passed (19 tests, final targeted run).
- `rtk cargo test -p i2pr-irc-runtime --test m005i_integration --locked` — passed (9 tests).
- `rtk cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — passed.
- `rtk scripts/check-network-boundary.py` — passed.
- `rtk scripts/verify.sh full` — passed on the installed toolchain.
- `rtk rustup run 1.88.0 sh scripts/verify.sh full` — passed.
- `rtk cargo fmt --all -- --check` — passed as part of both full verification runs; final targeted tree was formatted with `rtk cargo fmt --all`.

The full verification commands ran after the production state-machine changes. One final test-only case for CAP-without-SASL was then added and passed in the final 19-test focused suite.

## Security and recovery review

The owner now distinguishes CAP negotiation from CAP unsupported and does not send CAP END after a no-CAP welcome or a 421 CAP response. This is a per-generation decision; no host DNS, generic socket, TLS upgrade, or other egress path was introduced. Without configured SASL, the server may use modern CAP or classic registration and optional capability rejection remains non-fatal. With configured SASL, the owner rejects unsupported/missing mechanisms, a required NAK, authentication failures, and a 001 that precedes SASL success. Explicit mechanism lists must include PLAIN; a bare SASL token is treated as unknown and gets a PLAIN attempt. Required SASL is negotiated before the optional capability request. Authentication payloads remain zeroized and are absent from diagnostics.

Finding: no-CAP registration and optional capability rejection are now supported without weakening configured-SASL policy. No remaining Plan 036 finding was identified.

## Disposition

Plan 036 and M006-A are closed. Plan 037's hard dependency is discharged and Plan 037 is ready. Plan 038 remains gated on Plan 037; Plans 039-041 remain gated on M006 closure and subsequent M007 dependencies.
