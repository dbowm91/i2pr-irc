# Plan 038 Closure — Integrated IRC Interoperability Qualification and M006 Closure

Status: closed

## Server profile matrix

| Profile | Evidence | Result |
|---|---|---|
| Legacy/minimal: no CAP, SASL, MONITOR, tags, or labeled-response | production-owner classic welcome and `421 CAP` downgrade tests; Plan 036 tests | Online after 001 without CAP END; no SASL required when no credential is configured |
| Transitional: CAP 302, partial offers, bare sasl, optional NAK, selected member/tag capabilities | production-owner SASL/optional negotiation tests, multi-line fragmented CAP LS test, CAP NEW/DEL integration test | Offers accumulate to final LS; only reviewed capabilities are requested; optional NAK is non-fatal; configured SASL remains fail-closed |
| Modern: account-tag, invite-notify, cap-notify, history and member-state capabilities | M005-F/M005-G and M006 integrated suites | Capability state is mediated per session and updates are truthful |

## Downstream client profile matrix

| Client | Negotiated profile | Qualification |
|---|---|---|
| Legacy | no CAP | Receives baseline chat and self-target INVITE, without tags or third-party invite notifications |
| Basic IRCv3 | message-tags + server-time | Receives permitted tags but no account tag or third-party invite notification |
| Modern IRCv3 | reviewed current set including account-tag and invite-notify | Receives observed account tags and third-party invite notifications |
| History/passive | message-tags, CHATHISTORY/read-marker, pre-away | Shares the same Network generation without changing its upstream capability policy |

All four profiles run simultaneously on one generation in
`legacy_basic_modern_and_history_clients_share_one_generation_truthfully`. Existing
routing, history, account/member-state, detached-channel, reconnect, and session isolation
suites cover concurrent WHOIS/NAMES, history and reattachment behaviors.

## Authentication and downgrade matrix

| Upstream/auth configuration | Result |
|---|---|
| no CAP, no configured SASL | 001 completes registration; CAP END is not sent |
| 421 CAP, no configured SASL | treated as unsupported CAP and 001 completes registration |
| CAP without SASL, no configured SASL | completes registration without AUTHENTICATE |
| optional capability NAK, no configured SASL | remains online with declined optional semantics disabled |
| configured SASL absent/unsupported/NAKed/rejected | registration fails closed |
| configured SASL receives early 001 | registration does not complete before 903 |
| configured SASL bare offer | attempts configured PLAIN flow |

## Capability and privacy disposition

- `account-tag` and `invite-notify` are requested only from the fixed reviewed upstream
  set, conditionally advertised after ACK, and truthfully added/removed through cap-notify.
- `chghost` and `extended-monitor` remain absent from upstream requests and downstream
  advertisement.
- Plain IRC bytes over the I2P stream remain the transport profile. There is no TLS
  assumption or additional egress path.
- Account tags are forwarded only from observed upstream frames. No cached state is used
  to synthesize tags.
- Third-party invites reach only opted-in sessions; invites addressed to the Network nick
  reach every attached session.
- SASL payload secrecy, no-identifier defaults, and the I2P-only network boundary remain
  covered by existing qualification tests.

## Verification

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace --all-features`: passed.
- `scripts/check-network-boundary.py`: passed through full verification.
- `scripts/fuzz-smoke.sh`: passed through full verification.
- `scripts/verify.sh full`: passed.
- `rustup run 1.88.0 sh scripts/verify.sh full`: passed on rerun. The first run hit a
  timing-sensitive pre-existing M005-E retention test; its isolated rerun passed on both
  toolchains and the full MSRV rerun passed without a repository change to that test.

## M006 closure and dependency disposition

M006 is closed. The legacy/no-CAP/no-SASL and modern IRCv3 profiles are usable under
deterministic tests, configured SASL does not silently downgrade, and no unsupported
capability is advertised. No unresolved high-severity protocol or multi-client finding
was identified in this qualification. Plan 039 is unblocked and ready; Plan 040 remains
blocked on Plan 039 closure, and Plan 041 remains blocked on Plan 040 closure.
