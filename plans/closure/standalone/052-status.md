# Standalone M010-C / Plan 052 Closure

Status: closed
Implementation commits: `b196781cd8467b5df144d18f34678f2fdc8a5418`, `42c3338a8228bf38448897872d50b8a02ee48b0f`
Predecessor closures: `plans/closure/standalone/050-status.md`, `plans/closure/standalone/051-status.md`

## Requirement-to-evidence

| Requirement | Evidence | Result |
|---|---|---|
| Consume authenticated state and stream exactly once | `AuthenticatedCheckpoint::into_parts`; private checkpoint ownership; canonical session reader receives transferred unread bytes | Pass |
| Avoid replaying auth/CAP responses or registration bytes | Authenticator consumes PASS/SASL/CAP and transfers only acknowledged capabilities; NICK/USER and post-auth lines are deferred to canonical `SessionReader` once | Pass |
| PASS-only and CAP/SASL local authentication modes | Listener wire fixtures cover PASS and PLAIN; production local pipeline exercises CAP LS/REQ, PASS, NICK/USER, CAP END and canonical welcome | Pass |
| Stable profile identity, fresh attachment identity | Profile normalized to lowercase then stored with unique `StoreHandle::create_client`; separate profile IDs and concurrent same-profile admission covered; SessionId allocated per attachment | Pass |
| Resolve default Network before durable profile creation; no guessed Network | `admit_authenticated` verifies configured `NetworkId` through `RuntimeControlHandle::network_record`; absent ID errors before `create_client`, regression test proves no profile side effect | Pass |
| Preserve unbound control mode and supported negotiated capabilities | Unbound admission produces the canonical bouncer welcome; runtime validates transferred capability state and seeds the canonical reader | Pass |
| Bound/cancel admission tasks and avoid detached writers | Listener's bounded handoff channel and semaphore; admission task ceiling; cancellation drains/aborts admitted tasks; `AbortOnDrop` and writer Drop abort child tasks | Pass |
| Drain a final stale-identity numeric before closing a refused attachment | `PreparedSession::refuse` and `ClientWiring::respond_and_close`; regression exercised by runtime test `a_detached_client_can_reattach_and_is_told_the_truth_again` | Pass; corrective follow-up `42c3338` |
| I2P-only upstream boundary | Updated source guard scans daemon production source, allows only exact loopback bind/accept authority, and rejects unauthorized socket authority in positive controls | Pass |

## Commands executed

- `rtk cargo fmt --all` — passed
- `rtk cargo test -p i2pr-irc-daemon --locked` — 13 passed
- `rtk cargo clippy -p i2pr-irc-daemon --all-targets --all-features --locked -- -D warnings` — passed
- `rtk cargo test -p i2pr-irc-runtime --lib --locked` — 285 passed
- `rtk cargo clippy -p i2pr-irc-runtime --lib --all-features --locked -- -D warnings` — passed
- `rtk python3 scripts/check-network-boundary.py` — passed with positive controls

## Security, recovery, and scope review

Credential verification precedes `admit_authenticated`; profile persistence occurs only after the configured default Network is verified. Concurrent profile creation converges through the durable unique profile index. A failed/missing Network does not silently attach to another Network. The original NICK/USER lines remain in the one stream transfer and are parsed by the normal session machinery, so the lightweight auth parser does not project a second registration. Writer/session child tasks are aborted on parent cancellation.

Plan 052 closes the authenticated handoff and profile lineage interfaces; it does not enable the CLI listener, since no durable secure credential verifier exists until Plan 053. CAP/SASL local auth is distinct from upstream network SASL. Real router and IRC service qualification remains Plan 054.

## Handoff

Plan 053 is promoted to active: it can supply the protected Operator verifier and independent store key needed to enable the CLI listener with encrypted-by-default initialization. Plan 054 remains dependency-gated on Plan 053 closure.
