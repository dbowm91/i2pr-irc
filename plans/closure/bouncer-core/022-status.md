# Plan 022 — M005-C Presence and Preferred-Nick Policy — Closure

Closed 2026-10-08. Outcome: **closed, no open findings**.

## Outcome

The bouncer answers "is the Operator here?" from per-session classification rather than
a socket count, and answers "is this the nick I wanted?" inside the registration window
with a bounded deterministic sequence that never reads the host. Both policies are
durable and both migrate disabled, so an upgraded binary emits no new upstream traffic
that an Operator did not ask for.

The plan's eight invariants, and where each is answered:

| Invariant | Answered by | Evidence |
| --- | --- | --- |
| `NetworkRecord.nick` stays the preferred nick | never overwritten; only read | `the_fallback_sequence_is_bounded_deterministic_and_distinct` |
| `NetworkState.nick` stays the observed nick | `state.nick = next` on refusal only | `a_nick_collision_is_answered_rather_than_waited_out` |
| Fallback uses no host/environment input | `NickFallback` is a function of nick + `NICKLEN` | `no_host_or_environment_value_can_reach_a_nick_or_an_away_message` |
| Presence aggregates session state, not sockets | `SessionPresence::{Active, Passive}` | `a_passive_session_does_not_clear_auto_away` |
| One passive session cannot prevent auto-away | passive excluded from `away_decision` | `a_passive_session_does_not_clear_auto_away` |
| Manual away cannot be cleared incidentally | owner-scoped, set only by explicit `AWAY` | `an_explicit_manual_away_outranks_every_session_count` |
| Reclaim is rate-bounded and generation-fenced | generation `interval` + local `ReclaimAttempt` | `reclaim_writes_are_capped_per_generation`, `a_replaced_generations_reclaim_state_cannot_act_on_its_replacement` |
| Attachment never changes the upstream CAP fingerprint | negotiation is generation-owned | `the_upstream_capability_fingerprint_does_not_depend_on_attached_clients` |

## What was landed

### Work package A — durable presence/nick policy migration

`crates/store/src/model.rs`, `crates/store/src/schema.rs`, `crates/store/src/ops.rs`,
`crates/store/src/testing.rs`

- `SCHEMA_VERSION` 4 → 5. `NETWORKS_V5` carries `auto_away` and `keep_nick`, each
  `INTEGER NOT NULL DEFAULT 0 CHECK (… IN (0, 1))`.
- `schema_v4()` is retained as a real declaration rather than "v5 minus a column", and
  `create_v4_database` builds it, so the migration fixture cannot drift into a shape
  this build never wrote.
- `migrate_4_to_5` is one `execute_batch` of two `ALTER TABLE … ADD COLUMN` statements
  inside the existing migration transaction.
- `REQUIRED_COLUMNS` gained `("networks","auto_away")` and `("networks","keep_nick")`, so
  the promised-column check runs on both the migration and the open path.
- `policy_flag` is one shared decoder: an unrecognised value is `Corrupt`, never coerced.

Both policies migrate **disabled**. That is the load-bearing decision in this package: an
upgrade must not make a Network start emitting `AWAY` and `NICK` upstream traffic. New
upstream behaviour is something an Operator turns on, not something a binary does to
them.

### Work package B — per-session presence classification

`crates/runtime/src/session.rs`

`SessionPresence::{Active, Passive}` with `DEFAULT = Active`. It is a fact about a
session, inserted at attach and removed where the session goes away — not a counter, so
"slow clients cannot stall presence aggregation" falls out of the shape rather than
requiring a lock.

`MAX_AWAY_TEXT_BYTES = 200`; `AWAY` parsing rejects NUL, CR, and LF and refuses
anything past the ceiling. A rejected frame ends the session rather than forwarding an
Operator string that never met the bound into an IRC-visible field.

### Work package C — draft/pre-away adapter and CAP advertisement

`crates/runtime/src/presence.rs`, `crates/runtime/src/downstream.rs`,
`crates/runtime/src/capability.rs`

`PRE_AWAY_CAPABILITY = "draft/pre-away"` is added to `DOWNSTREAM_ADVERTISED` and
`DOWNSTREAM_PRE_AWAY`. `PASSIVE`/`ACTIVE` are accepted only when the client negotiated
it.

`SessionReader::registration_intent` emits the declaration **first**, then
`RequestProjection`, and `run()` drains all pending registration intents in one turn
rather than emitting one per `select` pass. This is what makes a pre-`CAP END`
declaration mean anything at all: without the drain, the declaration would be processed
after the projection had already claimed the session was active.

### Work package D — manual/automatic away state machine

`crates/runtime/src/presence.rs`, `crates/runtime/src/owner.rs`

- `PresencePolicy { auto_away, keep_nick }` with `from_record` and `LEGACY`.
- `away_decision(policy, manual, active_sessions)`: manual wins, then auto-away, then
  present.
- `PresenceState` is owner-scoped on `NetworkOwner` behind a `std::sync::Mutex`;
  `for_generation()` carries the manual-away and deliberately drops observed upstream
  away state. Manual away therefore survives a reconnect and is re-applied upstream
  after the next successful registration; what the server believed about the previous
  connection is not a fact about this one.
- `note_upstream_away` emits only on transition, so repeated equivalent events are
  idempotent.
- `apply_presence` is evaluated at generation start (after waiting attachments) and after
  every session event — and deliberately **not** on attach.

The attach exclusion is a real finding rather than an oversight. A client that declared
itself passive during registration has not yet been heard from at attach time; evaluating
there would count it as an Operator and then take that away, which is precisely the flap
the draft exists to avoid. The comment is in the code at the decision.

`AUTO_AWAY_TEXT` is a fixed bouncer-owned constant rather than an Operator-configurable
string. The plan offered "a fixed bouncer-owned default" explicitly, and an away message
reaches an IRC-visible field on paths that include upstream re-application after a
reconnect — a configurable string there is a control-character delivery problem waiting
to happen.

### Work package E — registration 433/fallback state

`crates/runtime/src/presence.rs`, `crates/runtime/src/owner.rs`

`"433" | "436"` is handled inside the registration loop, so the bouncer answers in the
same window as the refusal. Sitting until the 180-second registration ceiling would turn
a two-second collision into a thirty-second stall indistinguishable from a dead network.

`NickFallback` produces `bot`, `bot_1`, `bot_2`, `bot_3`, truncated to the advertised
`NICKLEN` (clamped to `[1, 64]`), capped at `MAX_FALLBACK_NICK_ATTEMPTS = 4`. No random
component and no host input. `prime()` is required — registration already offered the
preferred nick, so the first refusal must be answered with a different name.

`RuntimeError::NickExhausted` is terminal: the owner marks the Network terminal in
`serve` and in the gated `lib.rs` supervisor. A sequence already refused once per
candidate will produce identical upstream traffic on every retry while consuming a
process-wide connect permit each time; only configuration or a reconcile changes the
answer.

### Work package F/G — MONITOR reclaim and the bounded schedule

`crates/runtime/src/presence.rs`, `crates/runtime/src/owner.rs`

`reclaim_strategy(monitor_limit)` is driven from the ISUPPORT `MONITOR=<limit>` token
rather than from CAP, so the upstream capability fingerprint stays client-independent.
`MONITOR=0` (advertised and disabled) and any limit above `MAX_MONITOR_TARGETS = 8` both
fall back to a bounded `ISON` probe, because waiting for notifications a disabled feature
never sends would wait forever, and because the bouncer will not watch more nicks than its
own ceiling allows.

The clock is a generation-owned `tokio::time::interval(RECLAIM_INTERVAL)` at 300s, and
nothing else moves it. Reclaim state — `ReclaimAttempt` and the wake signal — is a plain
local in `run_generation` and dies with the connection that created it.

`MAX_RECLAIM_WRITES_PER_GENERATION = 8` bounds how often one generation may claim.

### Latent defect found and fixed

**Reclaim evidence was recorded but could not act on it.** The first implementation set
`attempt.evidence` from the upstream reply and left the 300-second interval as the only
trigger. A server that had just told the bouncer the preferred nick came free would have
had to wait out the interval before the write was permitted — losing a nickname the
bouncer already knew was available, in a feature whose entire purpose is to hold a nick.

Fixed by adding a generation-local `tokio::sync::Notify` that accepted evidence fires and
that the generation's `select` awaits alongside the interval. Evidence moves the *timing*
of a write; it never grants permission the schedule would not have.

### Second defect found and fixed

**`730` and `303` were read as the same kind of list.** The original parser treated a
`RPL_MONITOROFFLINE` nick list as an online list, so a `730` naming the preferred nick
became *negative* evidence — the precise inverse of what it means. `303` lists nicks that
are online (absence is evidence); `730` names the nick that went offline (presence is
evidence). Both are now read from parameter index 1, since both are addressed to the
bouncer, and every command other than those two is recorded as nothing rather than as
negative evidence that would suppress a future write.

This one is worth recording as a class of bug: the two replies are easy to conflate and
the confusion fails silently, because a bouncer that never claims its nick looks exactly
like a bouncer whose policy is off.

### Work package H — tests

`crates/runtime/src/presence.rs` (11 unit tests), `crates/runtime/tests/m005c_presence_nick.rs`
(21 integration tests).

The anonymity check is source-level rather than environmental: `unsafe` is denied at
build, so a test cannot `set_var` a sentinel and read it back. The test instead reads the
fallback and away-text construction *code lines* and asserts that no environment-derived
identifier is interpolated into an IRC-visible field. That is a weaker check than a live
sentinel would be, and it is recorded as such rather than presented as equivalent.

### Work package I — docs

`architecture/presence-and-nick.md` is new. `architecture/storage.md`,
`architecture/downstream-session.md`, `architecture/reconnect-and-liveness.md`, and
`architecture/overview.md` are updated.

## Presence transition matrix

`SessionPresence` is per session; `A` is the count of active sessions.

| Manual away | `auto_away` | Active sessions | Upstream `AWAY` written | Frame |
| --- | --- | --- | --- | --- |
| set, with text | any | any | on transition | `AWAY :<text>` |
| cleared | on | 0 | yes → present | `AWAY` |
| cleared | on | ≥1 | yes → present | `AWAY` |
| cleared | off | any | yes → present | `AWAY` |
| unset | on | 0 | on transition | `AWAY :Bouncer away: no active local client` |
| unset | on | ≥1 | on transition → present | `AWAY` |
| unset | off | any | never | — |
| any | any | any, repeated equivalent event | no | — |

The last row is what "exactly once" means. The bouncer's upstream away state is a function
of the classification, not of how many events happened to arrive.

## Fallback and reclaim bounds

| Quantity | Bound | Constant |
| --- | --- | --- |
| Fallback attempts per registration | 4 | `MAX_FALLBACK_NICK_ATTEMPTS` |
| Fallback nick length | `[1, 64]`, truncated to advertised `NICKLEN` | `DEFAULT_NICK_LENGTH = 9` |
| Reclaim interval | 300s | `RECLAIM_INTERVAL` |
| `MONITOR` targets | 8 | `MAX_MONITOR_TARGETS` |
| Reclaim writes per generation | 8 | `MAX_RECLAIM_WRITES_PER_GENERATION` |
| Away text | 200 bytes | `MAX_AWAY_TEXT_BYTES` |

## Ownership and lifetime matrix

| Thing | Owner | Lifetime | Bound |
| --- | --- | --- | --- |
| Durable presence policy | store worker | process + file | one row per Network |
| Manual away | `NetworkOwner.presence` | process (survives reconnect) | one optional bounded string |
| Observed upstream away | `run_generation` local | one generation | one optional string |
| Per-session presence | `run_generation` map | one generation | ≤ `MAX_SESSIONS_PER_NETWORK` |
| `NickFallback` | `run_generation` local | one generation | ≤ 4 candidates |
| `ReclaimAttempt` | `run_generation` local | one generation | ≤ 8 writes |
| Reclaim wake | `run_generation` local `Notify` | one generation | 1 permit |

The split between owner-scoped manual away and generation-scoped observation is the
whole of the reconnect story: intent survives, observation does not.

## Failure matrix

| Condition | Behaviour |
| --- | --- |
| Store cannot persist a policy change | the change is not claimed; the caller is told |
| Upstream `AWAY`/`NICK` enqueue refused | `upstream-queue-refused` in the snapshot; the frame is not retried as if it had been sent |
| Generation lost | reclaim attempt, evidence, and wake signal are dropped with it |
| Reclaim write ceiling reached | `reclaim-write-ceiling` in the snapshot; no further writes this generation |
| Fallback sequence exhausted | `NickExhausted`, Network marked terminal, `nick exhausted` recorded |
| Zero active, several passive sessions | auto-away is correct and is applied |
| Unbound admission/control session | no upstream away effect; it is never classified |

## Design decisions and deviations

1. **`draft/pre-away` is advertised, not assumed.** It is added to the downstream `CAP
   LS` set and the `DOWNSTREAM_PRE_AWAY` constant, and is honoured only on negotiation.
2. **Presence is not evaluated on attach.** See "Latent defect" — this is a flap
   avoidance decision, not an omission.
3. **Manual away is Network policy, not session state.** A session that disconnects while
   the Operator is away does not un-away them. A session that attaches while the Operator
   is manually away does not un-away them either.
4. **`MONITOR` is read from ISUPPORT, not CAP.** A negotiated capability would make the
   upstream fingerprint depend on negotiation order; an advertised token is a fact about
   the server.
5. **The automatic away text is not configurable.** See work package D.
6. **The anonymity test is structural, not environmental.** See work package H.
7. **`NetworkSnapshot` gained `away` and `active_sessions`.** These are Operator-facing
   facts, not faults. The richer diagnostics surface remains Plan 027's work.
8. **No capability was requested upstream for any of this.** `draft/pre-away` is a
   downstream-only adapter; `pre_away_is_only_honoured_from_a_client_that_negotiated_it`
   and `the_upstream_capability_fingerprint_does_not_depend_on_attached_clients` both pin
   that.

## Limits recorded

1. **The structural anonymity test is weaker than a live sentinel.** `unsafe` is denied
   at build, so the suite cannot set an environment variable and prove it cannot reach an
   IRC field. It proves the construction sites interpolate no such value. A live check
   would need a build that permits `unsafe` in tests, which the workspace denies
   deliberately.
2. **`MAX_MONITOR_TARGETS = 8` is a ceiling on watching, not on the advertised limit.**
   A server advertising `MONITOR=100` gets `ISON` probing instead. That is intentional —
   the bouncer watches exactly one nick — but it does mean a generous server is treated
   the same as one with no `MONITOR` at all.
3. **`NickExhausted` is terminal until configuration changes.** There is no automatic
   retry on a longer backoff. See the reasoning in work package E.
4. **Reclaim writes are requests, not claims.** The bouncer writes `NICK <preferred>` and
   waits for the server's own frame; it never treats its own write as success.
5. **Reclaim does not run while the Network is offline.** A preferred nick that frees up
   while the bouncer is disconnected is reclaimed on the next generation, at most
   `RECLAIM_INTERVAL` after it starts.
6. **Only the preferred nick is ever reclaimed.** MONITOR membership for other nicks is
   not tracked and would not be durable `DesiredState` if it were.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-store` — pass (11 unit + 46 qualification)
- `cargo test -p i2pr-irc-runtime --lib` — pass (203 tests, 11 new in `presence.rs`)
- `cargo test -p i2pr-irc-runtime --test m005c_presence_nick` — pass (21 tests)
- `cargo test --workspace --all-features` — pass
- `./scripts/verify.sh quick` — pass
- `./scripts/verify.sh full` — pass

## Findings

None open. Plan 022 introduced no architectural conflict, no new network boundary, and
no deferred obligation. Two defects were found during implementation and fixed; both are
described above with the reasoning that made the fix the right one.

## M005-D readiness

Plan 023 (M005-D, bouncer networks and local IRC administration) is **unblocked and
dependency-ready**. Everything it needs is landed:

- the `BouncerServ` administration surface this plan's compatibility shorthand has been
  explicitly deferring to, with a typed presence/nick policy already durable per Network
  so an administrative command has something real to change;
- `NetworkSnapshot.away` and `active_sessions`, so an Operator-facing status command can
  report presence without inventing its own accounting;
- a bounded, terminal-on-exhaustion registration path, so `BouncerServ` reconnect and
  network commands have well-defined generations to act on;
- `set_desired_channel_detached` and the `ChannelPolicy` seam from Plan 021, which the
  same administration surface needs for channel commands.

Plan 024 remains gated on Plan 023's closure, and Plans 025-028 remain gated on their
sequential predecessors.