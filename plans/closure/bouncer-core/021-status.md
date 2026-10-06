# Plan 021 — M005-B Durable Detached-Channel Policy — Closure

Closed 2026-10-06. Outcome: **closed, no open findings**.

## Outcome

A desired channel can be hidden from every attached session while the bouncer stays
joined upstream and keeps collecting bounded history. The decision is durable, ordered,
and explicit; it survives a restart and a reconnect; and reattaching restores truthful
current state and non-duplicating bounded history through the client's existing cursor
rather than through a second history model.

The plan's four hard requirements, and where each is answered:

| Requirement | Answered by | Evidence |
| --- | --- | --- |
| Durable desired intent unchanged | `DesiredChannelRecord`; `set_desired_channel_detached` is a single `UPDATE` | `desire_durable_order_is_untouched_by_detaching` |
| No hidden real departure | No upstream `PART` in either direction | `neither_policy_direction_writes_an_upstream_part` |
| Live projection/fanout excluded, history unaffected | `NetworkState::visible_channels`, `detached_fanout`, history block untouched | `a_detached_channel_stays_joined_upstream_and_keeps_collecting_history` |
| Reattach truthful + non-duplicating | `reveal_channel`, deferred reveal on confirmed self `JOIN`, unchanged legacy cursor | `reattaching_projects_truthful_state_before_any_history` |

## What was landed

### Work package A — typed desired-channel domain model

`crates/store/src/model.rs`

- `DesiredChannelRecord { target, position, detached }` replaces `Vec<String>` on
  `NetworkRecord`.
- `attached_channels(&[S])` builds an ordinary attached list, so configuration and test
  fixtures state channels rather than inventing position numbering.
- `DesiredChannelRecord::after` places a new channel at `max(position) + 1` — the same
  position the store's own SQL assigns — so an in-memory record and a reloaded one agree.
- `with_detached` replaces only the presentation flag; the durable order never moves.
- `validate()` now enforces: bounded count, per-record shape, `position <
  MAX_DESIRED_CHANNELS`, strictly increasing positions, and Rfc1459 casemap-unique
  targets. Two tests state each new rule.

The model makes the three facts explicit: which channel, in what order, and whether it
is presented. The previous string list could express only the first.

### Work package B — schema version 4

`crates/store/src/schema.rs`, `crates/store/src/testing.rs`

- `SCHEMA_VERSION` 3 → 4. `SCHEMA_TABLES` split into `SCHEMA_HEAD`,
  `DESIRED_CHANNELS_V3`, `DESIRED_CHANNELS_V4`, and the unchanged middle;
  `compose(desired_channels, networks, history, tail)` takes four parts.
- `schema_v3()` is retained as a real declaration, not "v4 minus a column", so the
  migration fixture cannot drift into a shape this build never wrote.
- `migrate_3_to_4` is one `ALTER TABLE desired_channels ADD COLUMN detached INTEGER NOT
  NULL DEFAULT 0 CHECK (detached IN (0, 1))` inside the existing migration transaction.
- `REQUIRED_TABLES` gained `desired_channels`; new `verify_promised_columns` checks
  `desired_channels.detached`, and runs on both the migration and open paths.

The column check is the real reason version 4 exists as a promise rather than a number.
A table that survived a migration without its flag would be served as though every
channel were attached — the failure mode is invisible in the data and total in its
effect.

### Work package C — persistence-first detach/reattach

`crates/store/src/ops.rs`, `crates/store/src/worker.rs`, `crates/runtime/src/owner.rs`

- `set_desired_channel_detached(connection, network, channel, detached) -> Result<bool>`
  commits a single `UPDATE` and reports whether a row changed.
- `StoreRequest::SetDesiredChannelDetached` and `StoreHandle::set_desired_channel_detached`
  expose it through the bounded worker.
- `NetworkOwner::apply_detach` commits first and only then changes presentation.
  `NetworkOwner::apply_reattach` commits first and then either projects or joins.

`false` distinguishes "the policy is now this" from "there was nothing to change", so
the caller can say so rather than assume success.

### Work package D — projection and fanout exclusion

`crates/runtime/src/state.rs`, `crates/runtime/src/projection.rs`, `crates/runtime/src/owner.rs`

- `NetworkState` carries `detached: BTreeSet<Vec<u8>>` (casemapped identities) and
  `pending_reveal: BTreeSet<Vec<u8>>`.
- `visible_channels()` is the single accessor for downstream-visible membership;
  `projection::project`, `initial_read_markers`, and `deliver_legacy_backlog` all read it
  or filter by it.
- `projection::project_channel` is the per-channel block extracted from `project`, reused
  verbatim by the reattach reveal so a reattached channel looks exactly like one that was
  attached when the client connected.
- `deliver_legacy_backlog` filters buffers by `is_detached_key`. History buffers are keyed
  by casemapped identity and include detached channels, because a detached channel
  collects history exactly as an attached one does; every replay path therefore filters
  explicitly rather than trusting the buffer list.
- `detached_fanout` withholds channel-scoped lines about a detached channel and redacts
  `QUIT` channel lists.

### Work package E — synthetic transitions

`crates/runtime/src/projection.rs`

`projection::detach_line` and `reattach_join_line` both use the bouncer's reserved
`:bouncer` prefix — never the Operator's nickname and never a participant upstream. A
frame attributed to a real person would be a false statement about an event that did not
occur.

The transitions are written to the session control queue and are never durable
`HistoryEvent`s: they record a local presentation decision.

### Work package F — compatibility shorthand

`crates/runtime/src/session.rs`, `crates/runtime/src/catalog.rs`

`PART <channel> :detach` and `PART <channel> :attach`, requiring exactly two parameters.
Anything else — a real `PART` with a part message, or any longer form — is an ordinary
part. Guessing at near-misses would let a client that meant to leave a channel silently
change the bouncer's whole presentation policy.

`BouncerServ` in Plan 023 remains the primary explicit administration path; the shorthand
exists so the policy is reachable before it.

### Work package G — generation and reconnect

`crates/runtime/src/owner.rs`

`NetworkOwner::durable_desired_policy` re-reads durable intent at each generation and
`NetworkState::new` is built from it. A detach or join performed mid-generation is
durable intent like any other, and taking the owner's birth record would drop it.

## Latent defect found and fixed

**In-process reconnect restored the owner's birth record, not durable intent.**
`run_generation` built `NetworkState` from `self.context.record.desired_channels`, which
is the `Arc<NetworkRecord>` captured when the owner was constructed. A channel joined
during generation 1 — by an ordinary `JOIN`, or detached by this plan — was therefore not
re-sent on the reconnect that replaced generation 1, even though it was durable intent.

The same latent gap now also applies to the detached flag, so it is fixed rather than
recorded: `durable_desired_policy()` is awaited once per generation before registration,
and falls back to the birth record (with `durable-channel-policy-unreadable` in the
snapshot) if the store cannot answer. A reconnect restores exactly what is on disk.

Qualified by `a_reconnect_rejoins_a_detached_channel_and_keeps_it_hidden` and
`a_detach_survives_a_process_restart_and_is_reapplied_without_being_asked`.

## Ownership and lifetime matrix

| Thing | Owner | Lifetime | Bound |
| --- | --- | --- | --- |
| Durable detach flag | store worker | process + file | one row per (network, casemaped target) |
| Generation-local detached set | `NetworkState` | one generation | ≤ `MAX_DESIRED_CHANNELS` |
| Pending reveal set | `NetworkState` | one generation | ≤ `MAX_DESIRED_CHANNELS` |
| Desired policy read | `NetworkOwner` | one per generation | one await, before registration |
| Synthetic transition | session writer | one session | control queue, `CONTROL_QUEUE_CAPACITY` |

## Failure matrix

| Store outcome | Durable effect | Live presentation | Client told |
| --- | --- | --- | --- |
| `Ok(true)` | applied | transition emitted | synthetic `PART`/`JOIN` |
| `Ok(false)` | none | none | "does not hold that channel" |
| `Err`, `RolledBack` | none | none | "could not persist that request" |
| `Err`, `Unknown` | unknown | re-read decides | "re-read durable state" or "could not confirm" |

The `Unknown` row is the one that matters. Presentation follows the durable flag, not the
outcome this process observed. Guessing would make what a client sees depend on which of
two outcomes this process happened to see first — and the other outcome is what a
reconnect sees.

## Reattach paths

| Observed membership | Written upstream | Projected |
| --- | --- | --- |
| present | nothing | `:bouncer JOIN` + topic/mode/NAMES, once |
| absent | `JOIN <channel>` | nothing until the authoritative self `JOIN` arrives |

Projecting at request time would tell a client it had joined a channel the bouncer had
not. Projecting never would leave it told that it had joined a channel it never saw. The
`pending_reveal` flag is cleared as it is consumed, so the reveal happens exactly once.

## Withholding and redaction

| Line | Action |
| --- | --- |
| `PRIVMSG`/`NOTICE` to a detached channel | withheld |
| `JOIN`/`PART`/`KICK`/`MODE`/`TOPIC`/`INVITE` naming a detached channel | withheld |
| `QUIT :#secret,#open` with `#secret` detached | rewritten to `:open`, tags dropped |
| `QUIT :#secret` with `#secret` detached | withheld; nothing is left to say |
| line naming both a detached and a visible channel | withheld whole |
| line mentioning no detached channel | unchanged |

Redaction rather than whole-frame suppression for `QUIT` is deliberate: suppressing it
would silently desynchronize the visible channels it also concerns. Withholding a frame
that names both is deliberate in the other direction: a client must never see a frame
whose meaning depends on a channel it may not see.

## Multi-client semantics

One client's detach changes the Network's policy, so **every** attached session receives
the synthetic `PART`. A client that stayed silent would keep a channel it can no longer
see, with no explanation for it disappearing. Reattach is symmetric.

Per-client read and playback cursors stay private throughout: a client that was
disconnected for the entire detached interval returns with its cursor where it left it
and can query the retained history normally, because the history was never withheld —
only the live view was.

## Design decisions and deviations

1. **`ChannelPolicy` is a trait**, for the same reason Plan 020's `DurableNetworks` is
   one. The decision that has to be qualified is what an owner does when a detach
   commit's outcome cannot be determined, and a bare `StoreHandle` cannot be made to
   answer ambiguously without corrupting a real database. Production passes
   `StoreChannelPolicy`, which is one hop from the owner to the bounded worker.
2. **The registration ceiling stays where it was.** Unchanged from Plan 020.
3. **The shorthand is `PART <channel> :detach` / `:attach`**, exact-match, two parameters
   only. The plan permitted a compatibility shorthand and deferred the explicit
   administration path to Plan 023.
4. **Existing rows migrate to attached.** The only safe default: a channel joined before
   this build existed has been presented to clients this whole time.
5. **`NetworkSnapshot` gained `detached_channels`, `channels_detached`, and
   `channels_reattached`.** These are policy, not faults, and a reader must be able to
   tell "the bouncer is not in this room" apart from "the Operator asked for this room to
   be hidden". Richer diagnostics remain Plan 027's work.
6. **Backlog on reattach is not delivered.** See limits.

## Limits recorded

1. **Reattaching restores visibility, not replay.** The plan's own wording — "through the
   client's existing cursor rather than through a second history model" — is satisfied by
   the client using its ordinary cursor; the owner does not push legacy backlog on
   reattach. A legacy client therefore sees truthful current state and can fetch what it
   missed the ordinary way. The alternative would need either a second cursor system or a
   duplicate-delivery risk, and neither is worth the cost of one convenience.
2. **Backlog for detached channels is filtered at the legacy path only.** The legacy
   path is the only automatic replay. A client that negotiated `draft/chathistory` always
   fetches through the ordinary cursor, which is unaffected by detaching.
3. **Redaction drops tags on a rewritten `QUIT`.** A tag is optional in every direction,
   and a server-chosen tag value may itself name a channel.
4. **`detached_channels` reports casemapped identities.** The stored spelling is the
   Operator's; the snapshot is keyed by casemaped identity so it cannot be mistaken for a
   different channel.
5. **The compatibility shorthand has no authorization of its own.** A session that can
   send `PART` can detach. Plan 023's `BouncerServ` is where administrative authority
   gets an explicit, bounded shape; nothing here is a general administrator credential.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-store` — pass (11 unit + 46 qualification, 9 new)
- `cargo test -p i2pr-irc-runtime --test m005b_detached_policy` — pass (16 tests)
- `cargo test --workspace --all-features` — pass
- `./scripts/verify.sh quick` — pass

## Findings

None open. Plan 021 introduced no architectural conflict, no new boundary, and no
deferred obligation.

## M005-C readiness

Plan 022 (M005-C, presence and preferred-nick policy) is **unblocked and
dependency-ready**. Everything it needs is landed:

- typed `DesiredChannelRecord` and the `ChannelPolicy` seam, so per-network presentation
  policy has one durable model and one injection point;
- `visible_channels()` and the `detached_fanout` filter, which any nick or away state
  must compose with rather than bypass;
- the reattach reveal machinery, which is the same "membership changed, project it once"
  shape a preferred nick needs.

Plan 023 remains gated on Plan 022's closure, and Plans 024-028 remain gated on their
sequential predecessors.