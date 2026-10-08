# Plan 037 Closure — Account-Tag and Invite-Notify Mediation

Status: closed

## Outcome

M006-B implements upstream and downstream mediation for `account-tag` and
`invite-notify`. Upstream requests remain a fixed reviewed set. Downstream availability
depends on upstream ACK and is updated on CAP NEW/DEL only for sessions that negotiated
cap-notify. The implementation does not synthesize account tags from cached account
state. Self-targeted INVITE frames retain baseline delivery; third-party invite
notifications reach only sessions that negotiated invite-notify.

## Capability matrix

| Capability | Upstream request | Downstream advertisement | Runtime behavior |
|---|---|---|---|
| `account-tag` | only when offered | only after upstream ACK | preserves an observed `account` tag per session; cached account state never creates one |
| `invite-notify` | only when offered | only after upstream ACK | third-party invitations only to opted-in sessions; self-targeted invitations to all attached sessions |
| `chghost` | not requested | never advertised | remains deferred pending a consistent compatibility projection |
| `extended-monitor` | not requested | never advertised | remains deferred pending a bounded per-session monitor broker |

## Tag filtering matrix

| Session negotiation | Retained tags |
|---|---|
| no tag capability | none |
| `message-tags` | all except `time` and `account` unless their specific capabilities were negotiated |
| `server-time` alone | `time` only |
| `account-tag` alone | `account` only |
| `message-tags` + `server-time` | all except `account` |
| `message-tags` + `account-tag` | all except `time` |
| all three | all observed tags |

## Invite routing matrix

| INVITE target | Any attached session | Session with `invite-notify` |
|---|---:|---:|
| current Network nick | receives | receives |
| another user | withheld | receives |

## Evidence

- Added a five-client integrated account-tag projection test, including account-tag alone,
  generic tags alone, no tags, and server-time without account-tag.
- Verified no tag is synthesized after account-notify state is observed.
- Added invite-notify routing coverage for self and third-party invitations.
- Added CAP NEW/ACK/DEL coverage for account-tag and invite-notify and conditional
  advertisement unit coverage.
- Updated M005-F/M005-G expectations and architecture documentation for the promoted
  capabilities and the two remaining deferred capabilities.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace --all-features`: passed.
- `scripts/verify.sh full`: passed.
- `rustup run 1.88.0 sh scripts/verify.sh full`: passed.

## Dependency disposition

Plan 038's hard dependency on this closure is discharged. No new blocker was found;
Plan 038 is ready. Plans 039–041 remain gated on the already specified sequential
dependencies: M006 closure, Plan 039 closure, and Plan 040 closure respectively.
