# Bouncer Core M007-B / Plan 040 Closure

Status: closed

Implementation plan:

- `plans/implementation/bouncer-core/040-m007b-preferred-nick-reconnect-and-multiclient-identity-resilience.md`

Implementation commit: `c53bc8b` (`Implement M007-B nick and identity resilience`).

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Collision classification | `NetworkOwner::run_generation` uses 433/436 as transient availability conflicts, 437 only when parameter 2 case-matches the attempted nick, and 432 as permanent registration rejection. Numeric reason text is not parsed. Tests cover 433, 436, qualified 437, and terminal 432. |
| Exhaustion and scheduler | The bounded fallback ends that generation; the owner waits 15 minutes plus deterministic positive Network/generation jitter (0–3 minutes) before looping through the existing `ReconnectScheduler.acquire` path. It does not mark the Network terminal, and the local wait does not hold a scheduler permit. Tests assert Backoff, `nick-collision-retry`, the 15–18 minute bounds, and no immediate second NICK sequence. |
| Reclaim refusal | Matching 433/436/437 refusals clear free evidence, count the refusal, and set a five-minute generation-local cooldown. The online phase remains online; the test confirms no tight retry. Corrected 731/730/303 behavior and bounded write counts remain covered. |
| Manual NICK | Registered NICK remains `NonReplayable`. An alternate nick suspends reclaim for the generation without changing durable configuration. The server NICK frame is observed by a second client; reconnect starts with the durable preferred nick. |
| Preferred alias | The current nick is accepted, as is the durable preferred alias only when the current observed nick matches a generated fallback already offered in this generation. Admission queues `:<claimed> NICK :<observed>` before projection and updates the transferred reader's registered identity. The end-to-end admission test confirms the transition; the existing arbitrary mismatch test confirms refusal. |
| Diagnostics | Fixed fields report preferred/current nick, fallback, reclaim suspension/cooldown, write/refusal counts, and typed retry classification. No service payload, endpoint, or secret was added. |
| Invariants | Nick state remains owned by `NetworkOwner`; multi-client authority continues through the shared observed Network state and server frame fanout. No user command is replayed across generations, and no generic network transport was added. |

## Verification run

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — passed.
- `cargo test -p i2pr-irc-runtime --test m005c_presence_nick --test m005a_controller_admission` — passed (27 + 28 tests).

## Findings and disposition

No unresolved Plan 040 findings. Plan 041's sole hard dependency, this closure, is now satisfied. Plan 041 is unblocked and becomes the active M007-C qualification and closure plan.
