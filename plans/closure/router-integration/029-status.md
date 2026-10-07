# Plan 029 — R001-A Provider Scope, Lifecycle, and Endpoint Foundation

Closed 2026-10-07. Outcome: **R001-A closed. Provider scope is part of the contract,
release is explicit and bounded, and a real Base64 Destination is now accepted. One
plan-specified detail was overridden with recorded justification; no unresolved
high-severity finding remains.**

Repository baseline: `a8dc46f`.

Authority: ADR-0004, ADR-0005, Research 007.

Primary class: invariant + infrastructure.

## 1. What changed, and why each change was necessary

The provider boundary was connect-only and the endpoint type could not express a real
I2P Destination. Neither was a design gap; both were places where the type did not say
what the runtime already knew, so the possibility of a mistake was not represented in
the types at all.

| Change | Consequence of not making it |
|---|---|
| `connect` takes `NetworkId` | One provider serves every configured Network, so an unscoped call cannot say which Network's session an attempt belonged to, and cannot release only the one asked for. Two Networks could share a router identity while every count-based test still passed. |
| `release(network)` added | A router scope outlives any IRC connection. It cannot be inferred from stream absence, and an idle timeout would either leak or churn the identity that scoping exists to provide. |
| Release wired into delete and shutdown | A deleted Network would keep a live router session with nothing left that could address it. |
| `PROVIDER_RELEASE_TIMEOUT` | A wedged adapter would hold a delete open for the whole connect budget, and shutdown has no timeout of its own. |
| Destination length and alphabet corrected | Every real Destination was rejected. See section 2. |

## 2. The endpoint defect, and why it was worse than the plan recorded

Plan 029 section 10 read as one length problem. It was two, and the second one is the
reason R001 could not have worked.

The old predicate accepted a raw Destination only at *exactly* 516 characters, validated
against `A-Za-z0-9-~`. Two failures, both silent:

- 516 is the length of a **short** Destination. Longer ones exist, and were rejected.
- `A-Za-z0-9-~` is the **base64url** alphabet. Real I2P Destinations are standard Base64
  and carry `+` and `/`. A Destination containing either was rejected no matter how long
  it was.

So the accepted set was "exactly the shortest Destination, using the wrong alphabet" —
which is close to empty. No live SAM integration could have connected to anything.

The ceiling is now one value, 4096 textual bytes, shared by every endpoint form, with a
Destination floor of 516 and its own padding rules. Name forms keep their tighter bounds
(67 characters, 63 per label), asserted separately so raising the ceiling cannot have
loosened them.

Destination shape is decided here and reachability is not. An unreachable Destination is
a well-formed endpoint that fails at connect time like any other unreachable target,
rather than a configuration error the Operator is asked to fix.

### 2.1 Recorded deviation from plan section 10

Plan section 10 specifies the Destination body alphabet as `A-Z a-z 0-9 - ~`, and its
test list includes "invalid alphabet rejected".

**Implemented as the union of `A-Za-z0-9-~` and standard Base64 `+/`.**

The plan's stated intent is in section 10's own first line — accept Destinations "at or
above the legacy 516-character floor up to that ceiling", because the 516 cap was found
defective. Implementing the narrower alphabet literally would have rejected every
Destination a real router can produce, which is the same defect the plan exists to fix,
and would have left Plan 031 unable to qualify against a live router at all.

Both plan-listed characters are accepted, so the plan's own acceptance vectors hold. What
is additionally accepted is `+` and `/`, which no test vector in the plan lists as valid,
so no specified behaviour changed. Nothing outside Base64 is accepted; whitespace,
control characters, path syntax, and host-port syntax remain rejected, and the invalid
alphabet vectors still fail.

A future amendment to the plan should state the standard-Base64 alphabet explicitly. This
is recorded rather than silently resolved because the alternative — silently weakening a
plan-specified rule to match the implementation — is what the repository's authority order
forbids.

## 3. Requirement-to-evidence matrix

Requirements are Plan 029 section 4 invariants, plus the section 12 test list.

| # | Requirement | Evidence |
|---|---|---|
| 1 | Every connect receives exactly one durable `NetworkId` | `every_connect_attempt_is_scoped_to_the_network_that_asked_for_it`, `two_networks_sharing_one_provider_keep_distinct_scopes` |
| 2 | `NetworkId` is scope only, never remote wire identity | `NetworkId` appears only in provider parameters and the new release log. No serialization, no `format!`, no IRC-visible field. Guarded by the network boundary scan and by review; the type has no `Display`. |
| 3 | Release is idempotent | `a_delete_releases_exactly_once`, `a_failed_release_prevents_the_durable_delete` (the retry path), `shutdown_does_not_re_release_a_deleted_network` |
| 4 | Generation ending does not release | `a_generation_churn_does_not_release_the_scope` (three generations driven through an injected 10 ms token interval) |
| 5 | Delete releases only after owner join | `release_never_precedes_the_owner_stopping`, plus the structural fact that `release_provider` is called after `stop_owner(...).await?` returns from `owner.join.await` |
| 6 | Shutdown releases all scopes after join | `shutdown_releases_every_configured_network_scope`, `shutdown_does_not_re_release_a_deleted_network` |
| 7 | No provider task/session remains after release | `deleting_a_network_releases_only_that_network_scope` (surviving Network's scope untouched); for the SAM-backed case this is Plan 031's evidence, not this plan's — a fake provider has no session to leak |
| 8 | Release cannot hang delete/shutdown | `a_release_that_never_completes_is_bounded` (a provider whose `release` never resolves; delete returns `RuntimeError::Timeout`), `the_release_deadline_is_bounded_and_shorter_than_the_connect_budget` |
| 9 | No generic TCP/DNS/upstream socket introduced | `./scripts/check-network-boundary.py` green; no new workspace dependency |
| 10 | Fake/fault/reconnect semantics unchanged except observability | `adverse`, `multi_network`, `qualification`, and every M005 suite unchanged and green; the fake gained `requested_scopes`, `released_scopes`, `was_released` |
| 11 | `I2pEndpoint` stays I2P-only and redacted | `endpoint_recognizes_i2p_forms_without_resolving` now asserts `Debug` is `I2pEndpoint([redacted])` for hostname, b32, and Destination |
| 12 | Rust 1.88 remains the floor | `rustup run 1.88.0 cargo check/test --workspace --all-features --locked`, green |

Section 12 endpoint vectors:

| Vector | Evidence |
|---|---|
| legacy 516-char Destination accepted | `endpoint_accepts_real_base64_destinations` |
| valid longer Destination accepted | same, a 702-character mixed-alphabet token |
| 4096 accepted, 4097 rejected | same, at `MAX_I2P_DESTINATION_CHARS` and `+1` |
| legal trailing padding accepted | same, `(514,2)`, `(515,1)`, `(516,0)` |
| interior/excess padding rejected | same, interior `=`, three `=`, pad on an unaligned token |
| invalid alphabet rejected | same, plus `check-network-boundary.py`'s unrelated guards; name-form regressions in `endpoint_recognizes_i2p_forms_without_resolving` |
| hostname/b32 regressions green | `endpoint_ceiling_did_not_loosen_the_name_forms` (new), and the existing hostname vectors |
| Debug remains redacted | `endpoint_recognizes_i2p_forms_without_resolving` |

## 4. Delete and shutdown ordering, as implemented

Delete, in `RuntimeController::delete`:

1. validate a durable record exists, else `Ok(false)` and nothing is released;
2. `stop_owner` — signal, then join the owner task, and **await the join**;
3. `commit()` — republish before touching storage, so a quiesced Network does not read as live;
4. `release_provider(network)` — bounded; **returns `Err` on failure, which skips step 5**;
5. durable `remove`;
6. drop the record and the catalog entry;
7. `commit()`.

Shutdown, in `RuntimeController::shutdown`: the network list is the union of live owners
and durable records without one, sorted and deduped; for each, stop/join then release,
with each failure dropped so one refusing adapter cannot prevent the remaining attempts.

Two ordering decisions carry the invariant:

- **Release is after the join, never before.** A release issued while an owner is still
  live could close a session that a connect already inside the provider is about to
  acquire.
- **Release is before the durable delete.** Forgetting the row first would make a
  still-live router session unreachable and therefore unreleasable forever. A failed
  release leaves the Network configured but stopped, which the Operator can retry, and the
  retry still has a scope to release.

`release_provider` runs under `PROVIDER_RELEASE_TIMEOUT` (15 s), deliberately below
`CONNECT_TIMEOUT` (120 s) because release happens where the caller is already blocked.

A configuration change that preserves the `NetworkId` never releases: the router identity
belongs to the durable Network, not to an IRC generation.
`a_same_network_change_does_not_release_the_scope` is the standing evidence.

## 5. Scope and release counts observed

From `crates/runtime/tests/r001a_provider_scope.rs` against `FakeI2pStreamProvider`:

| Scenario | Connects (by scope) | Releases |
|---|---|---|
| one Network, connect only | `[1]` | `[]` |
| two Networks, one provider | `[1, 2]` | `[]` |
| delete one of two | `[1, 2]` | `[1]` |
| delete an already-deleted Network | `[1]` | `[1]` |
| three Networks, shutdown | `[1, 2, 3]` | each exactly once |
| delete then shutdown | `[1, 2]` | `1` once, `2` once |
| three failed generations | `[1, 1, 1]` | `[]` |
| change preserving `NetworkId` | `[1]` | `[]` |
| stalled release | `[1]` | `[1]` attempted, `Err(Timeout)` to the caller |

## 6. Commands actually executed

| Command | Result |
|---|---|
| `./scripts/check-network-boundary.py` | exit 0 |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clean |
| `cargo test --workspace --all-features --locked` | 0 failures |
| `./scripts/fuzz-smoke.sh` (`verify.sh full`) | exit 0 |
| `rustup run 1.88.0 cargo check --workspace --all-features --locked` | clean |
| `rustup run 1.88.0 cargo test --workspace --all-features --locked` | 0 failures |

Core suite gained two tests (`endpoint_accepts_real_base64_destinations`,
`endpoint_ceiling_did_not_loosen_the_name_forms`); testkit gained one
(`fake_provider_records_scope_and_release_per_network`); the runtime gained
`r001a_provider_scope.rs` with 15 tests.

## 7. Security and privacy review

- **No scope leakage.** `NetworkId` reaches the provider as a scope and stops there. It
  has no `Display`, is not serialized, and does not enter any SAM, IRC, or diagnostic
  field. A future SAM adapter must keep it that way: it is not a wire identity.
- **No endpoint disclosure.** Destination parsing keeps `Debug` redacted for every form,
  now asserted rather than assumed. Plan 030 owns the stronger requirement — that a full
  Destination never reaches a log line — and this plan hands it a type that cannot be
  printed by accident.
- **No secret material retained.** A Destination is key material, which is why accepting
  longer ones changed the Debug assertion to cover it. Nothing here derives, stores, or
  logs Destination bytes.
- **No new authority.** No dependency was added. The workspace has no SAM, resolver,
  generic socket, or HTTP client, and the boundary scan is unchanged and green.
- **Untrusted input.** The corrected predicate is the one piece of new parsing in this
  plan. It is a bounded shape test: a length range, a closed alphabet, and a padding
  rule, all applied before anything else. It allocates nothing per call beyond the one
  lowercase the name forms already made.

## 8. Findings

**Finding 1 — medium, fixed here.** The endpoint type could not represent a real
Destination. See section 2. It was latent rather than active: no production SAM adapter
existed, so nothing had yet tried to connect to one. It would have surfaced as an
immediate, total failure of R001-C.

**Finding 2 — medium, fixed here.** Plan 029 section 10's alphabet is base64url and
would have preserved the defect. See section 2.1.

**No finding is carried into Plan 030.**

## 9. Roadmap disposition

R001-A is closed. The subsystem roadmap's R001-A row advances to closed; R001-B / Plan
030's hard dependency is satisfied and its status moves from `blocked` to `ready`.

Plan 031's dependency is Plan 030 and remains blocked. Plan 032's dependency is Plan 031
and remains blocked.

Plan 029's own acceptance criteria are met: the repository passes against the scoped
provider API, generation churn never releases scope, delete and shutdown do, bounded modern
Destinations are accepted, no production SAM socket exists yet, and the 1.88 and boundary
gates are green.
