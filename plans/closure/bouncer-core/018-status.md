# Bouncer Core M004-D — Integrated Anonymity Qualification and M004 Closure

Status: closed

Implements: `plans/implementation/bouncer-core/018-m004d-integrated-anonymity-qualification-and-closure.md`

Predecessor closures:

- `plans/closure/bouncer-core/014-status.md` — Corrective 014, live multi-client response routing
- `plans/closure/bouncer-core/015-status.md` — M004-A, anonymity protocol mediation
- `plans/closure/bouncer-core/016-status.md` — M004-B, global reconnect budget
- `plans/closure/bouncer-core/017-status.md` — M004-C, adverse-network and resource qualification

Research authority: `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Repository baseline: `cffd8db` ("Close M004-C and unblock M004-D")

Implementation commits:

- `76cd67d` — "Simplify client-tag mediation and cover the fuzz harness with the boundary guard"
- `9d45518` — "Qualify the M004 closure claims that span subsystems"
- `7843c86` — "Document the M004 anonymity and resilience design"

## Objective

Close M004 only after the core can truthfully be described as suitable for anonymity-network
operation at the router-neutral byte-stream boundary.

This milestone added no protocol feature. Its output is integrated evidence, a final
source/dependency review, and reconciled documentation.

## What is new here, and why

M004's individual claims are already defended by the mechanism suites. Restating them in a
closure file would only let a second copy drift. The claims worth closure-level evidence are
the ones that are **cross-cutting**: each can only fail if two subsystems interact badly, so
no single mechanism suite would catch it.

`crates/runtime/tests/qualification.rs` is therefore six tests, not sixty.

## Anonymity protocol matrix

| Claim | Evidence | Result |
|---|---|---|
| DCC cannot invoke network behavior | `a_client_dcc_request_is_blocked_in_every_shape`; `an_upstream_dcc_request_is_suppressed_entirely`; boundary guard `DCC_TOKENS` | Pass. `Ctcp::Dcc` carries no parameter and no code path builds a connection, address, or offer from one. |
| DCC cannot reach a local client as an actionable request | `an_upstream_dcc_request_is_suppressed_entirely`; `a_full_generation_never_leaks...` | Pass. Suppressed before fanout; the composition test re-proves it on a live generation with two attached clients. |
| ACTION remains correct | `an_action_is_ordinary_chat_in_both_directions`; `remote_visible_ctcp_behaviour_is_fixed_by_policy_not_by_client_brand` | Pass. Forwarded as ordinary chat in both directions; `ACTION` in a `NOTICE` is ordinary text, not an action. |
| Reviewed CTCP policy is complete | `the_directional_policy_is_deny_by_default` | Pass. Both directions are allowlists; everything else is refused, including any command word added later. |
| Local client software/version/time/environment cannot leak through CTCP auto-replies | `a_client_metadata_reply_never_reaches_the_upstream_server`; `an_upstream_ctcp_ping_is_answered_by_the_bouncer_itself`; `no_build_router_or_version_string_is_exposed_by_default` | Pass. The auto-answer is a fixed `NOTICE` echoing only the probe's own token. |
| Client-only tags follow explicit deny policy | `client_tags_are_denied_except_the_bouncers_own_label`; `a_flood_of_denied_tags_costs_no_extra_queue_capacity` | Pass. Tag mediation runs on every forwarded frame, not only chat. |
| CLIENTTAGDENY is truthful | `the_advertised_client_tag_deny_is_truthful`; `the_capability_set_and_the_tag_deny_agree_for_every_negotiating_client` | Pass. The advertised `*` is checked against the mediator over all 15 named client-only tags plus one the build has never seen, for every negotiating client mix. |
| No ambient host identity values become IRC-visible | `no_host_environment_value_reaches_the_wire_or_a_diagnostic` | Pass. Real host values are used as sentinels, so a match would be attributable rather than merely suspicious. |
| Secret / redaction tests pass | `a_sasl_secret_never_reaches_a_diagnostic`; `qualification_diagnostics_carry_no_secret_or_endpoint_material`; `no_durable_row_records_a_session_id_or_generation` | Pass. |
| Raw protocol logging is disabled by default | `raw_protocol_logging_is_absent_from_every_production_path` | Pass, structurally. No logging facade is a dependency of any production crate, and no production source writes to stdout or stderr. |

The last row is a structural claim rather than a behavioural one: there is no way to observe
"nothing was logged" from inside a test, so it is proved the only way it can be — by showing
no sink exists.

## Client-tag / CLIENTTAGDENY matrix

| Tag class | Advertised | Enforced | Result |
|---|---|---|---|
| `msgid`, `time`, `server-time`, `account`, `account-tag` | denied (`*`) | stripped | Pass |
| `draft/reply`, `draft/edit`, `draft/delete`, `draft/msgid` | denied (`*`) | stripped | Pass |
| `+typing`, `+react`, `+draft/relaymsg`, `+draft/thread` | denied (`*`) | stripped | Pass |
| `echo`, `no-reply` | denied (`*`) | stripped | Pass |
| `+unheard-of/namespace` (no such tag) | denied (`*`) | stripped | Pass — this is what makes `*` honest rather than a best-effort list |
| `label` | denied (`*`) | retained for the router, translated, never relayed | Pass — the bouncer's own correlation mechanism |
| `batch` (server-originated) | n/a | passed through | Pass — not a client tag |

`label` is the only survivor, and retaining it does not contradict `CLIENTTAGDENY=*`: the
token is consumed by this process and never reaches the server, which is exactly what the
advertisement promises.

## Environment / secret negative matrix

| Surface | Values searched for | Result |
|---|---|---|
| downstream registration and ISUPPORT | `USER`, `LOGNAME`, `HOSTNAME`, `HOME`, `TMPDIR`, `PWD` | Absent |
| upstream bytes | same | Absent |
| downstream replies | same | Absent |
| `NetworkSnapshot` diagnostics | same, plus payload-shaped values | Absent |
| CTCP auto-answer | `i2pr-irc`, `CARGO_PKG_VERSION`, `uname`, `Darwin`, `Linux`, `x86_64`, `aarch64`, `i2pd`, `I2P`, `b32`, `VERSION`, `version=`, ` 0.1.0` | Absent |
| restored configuration (`NetworkCatalog`) | SASL secret | Absent |
| durable rows | SessionId, generation | Absent |

## Upstream fingerprint matrix

| Claim | Evidence | Result |
|---|---|---|
| Upstream CAP request set is stable for the same server offer regardless of downstream clients | `the_upstream_registration_is_identical_for_every_client_mix` | Pass. Registration bytes are byte-identical across no client, one legacy, one tagged, and three concurrently attached. |
| CTCP remote-visible behavior is fixed by bouncer policy, not attached-client brand | `remote_visible_ctcp_behaviour_is_fixed_by_policy_not_by_client_brand` | Pass. A legacy client and a `message-tags`+`batch`+`labeled-response` client produce identical upstream traffic. |
| No build/router/OS/version string is exposed by default | `no_build_router_or_version_string_is_exposed_by_default` | Pass. Checked across the registration, the CTCP auto-answer, the upstream bytes, and the diagnostics. |

The brand comparison excludes the liveness `PING` from the byte-order comparison. That line
comes from a timer, so its position relative to client traffic is a scheduling artifact; it
says nothing about the attached client, and comparing raw byte order would make the test pass
or fail on timing. Its presence is asserted to be brand-independent separately.

## Response-routing matrix

| Claim | Evidence | Result |
|---|---|---|
| Corrective 014 routing is live end to end | `two_clients_query_concurrently_and_each_gets_only_its_own_answer`; `a_full_generation_never_leaks_or_carries_a_fingerprint_across_reconnect` | Pass. |
| Concurrent query replies reach only the requesting SessionId | `concurrent_labeled_queries_from_several_sessions_route_correctly`; `a_full_generation_never_leaks...` | Pass. The bystander receives nothing. |
| Stale sessions/generations cannot receive replies | `a_generation_replacement_discards_every_route`; `a_detached_client_can_reattach_and_is_told_the_truth_again` | Pass. Routes are generation-local by construction, not by timeout. |
| A route closes on its own terminator | `a_full_generation_never_leaks...` | Pass. `response_routes` and `open_batches` settle to zero. |

Correlation requires the server to echo the bouncer's translated label. An unlabelled reply
is a server-initiated frame and fans out by design; that distinction is asserted explicitly
rather than left implicit.

## Reconnect budget / fairness matrix

| Claim | Evidence | Result |
|---|---|---|
| Global scheduler gates startup and retry | 19 tests in `reconnect_budget.rs` | Pass for the production path (`catalog::NetworkSupervisor` over `owner::NetworkOwner`). See UF-015-1 for the legacy supervisor, which is outside it. |
| Connect concurrency bounded | `MAX_IN_FLIGHT_CONNECTS = 4`; burst gate campaign | Pass on the production path. Peak in-flight was 4. |
| Start rate bounded | `CONNECT_TOKEN_INTERVAL = 2s`, `MAX_CONNECT_BURST = 4` | Pass. |
| Fairness / no starvation | FIFO waiter queue, one waiter per Network | Pass. |
| Terminal failures do not consume infinite retries | `classify` returns a terminal decision | Pass. |
| Ambiguous user traffic is never replayed | `a_command_refused_by_the_upstream_queue_is_reported_and_never_replayed`; generation fence in the writer | Pass. |
| Stale generation work cannot mutate current state | `a_generation_replacement_discards_every_route`; intent stamped by the owner | Pass. |
| Slow client/store pressure cannot starve PING/PONG/control | `a_stalled_store_never_starves_control_traffic`; `store_pressure_degrades_storage_only_and_creates_no_side_queue` | Pass. |

## Adverse fault matrix

| Campaign | Shape | Result |
|---|---|---|
| Startup herd | 64 Networks start together | Pass. Concurrency measured, not assumed; in-flight peak 4. |
| Burst rate | repeated connect demand under the token bucket | Pass. Requested and granted counts stay within the burst. |
| Simultaneous outage | every Network fails at once | Pass. |
| Stalled provider | provider accepts then stops reading | Pass. |
| Churn | 3 Networks, 120 attach/detach/fail rounds | Pass. |

Every campaign asserts `current == baseline` after settling. Recorded from the campaigns
themselves in `017-status.md`; the ledger produces those numbers rather than a test asserting
them into existence.

## Durability / restart-crash matrix

| Claim | Evidence | Result |
|---|---|---|
| Restart preserves committed DesiredState | `desired_state_survives_restart_and_no_observed_state_does`; `a_clean_restart_rebuilds_durable_intent_and_no_live_state` | Pass. |
| History/cursors/read markers survive | `migration_preserves_event_ids_cursors_and_read_markers`; `read_marker_and_cursor_survive_retention_and_clamp_monotonically` | Pass. |
| Stale ObservedState is not restored | `desired_state_survives_restart_and_no_observed_state_does` | Pass. |
| Schema v2 valid, migration history intact | 32 store qualification tests incl. v1→v2 fixtures, batched migration, autoincrement monotonicity, and a failed-migration rollback | Pass. |

## Resource baseline / peak / settled

Carried from `017-status.md`, where the campaigns ran. Every campaign settled to baseline.

| Campaign | Baseline | Peak | Settled |
|---|---|---|---|
| startup herd (64 Networks) | `networks 0`, all gauges 0 | `owner_tasks 64`, `in_flight 4`, waiters `<= 64` | equals baseline |
| burst gate (64 Networks) | `networks 0` | `in_flight 4`, `requested 4` | equals baseline |
| shared outage (4 Networks) | `networks 0` | `owner_tasks 4` | equals baseline |
| churn (3 Networks, 120 rounds) | `networks 0` | `owner_tasks 3`, `in_flight <= 4` | equals baseline |
| stall (4 Networks) | `networks 0` | `owner_tasks 4` | equals baseline |

No hidden unbounded structure exists: the ledger stores current-and-peak only, so its own
memory is constant regardless of run time or campaign count. Every queue, batch set, route
table, and waiter queue has an explicit ceiling.

One exception is recorded as UF-018-1 below: `catalog::MAX_TOTAL_SESSIONS` is declared but
neither enforced nor asserted. The session population is still bounded — by
`MAX_SUPERVISED_NETWORKS` (64, enforced at `catalog.rs:212`) times `MAX_SESSIONS_PER_NETWORK`
(64, enforced in `owner.rs`) — but that product is 4096, not the 1024 the unused constant
declares.

## Static network-boundary evidence

`python3 scripts/check-network-boundary.py` exits 0.

The guard covers all five prohibited primitive families — generic TCP, DNS, HTTP, SOCKS or
proxy, and DCC — each with a named positive control, proven by predicate-deletion mutation
testing so a control cannot silently stop testing anything. `crates/fuzz-smoke` was added to
the guarded crate list in this pass: it became a first-party exerciser of production
mediation code when it took a path dependency on `i2pr-irc-runtime`, so it must be guarded
like the rest.

`I2pStreamProvider` remains the sole upstream stream authority. The guard is asserted from
`the_static_network_boundary_guard_passes` rather than restated, so a test cannot drift from
it.

## Dependency and MSRV review

**No third-party production dependency was added during M004.** The entire `Cargo.lock` delta
across the milestone is one line: `i2pr-irc-fuzz-smoke` gaining the first-party path
dependency `i2pr-irc-runtime`. That change landed in M004-C (`a0e13e3`), not in this pass:
`git diff cffd8db HEAD -- Cargo.lock` is empty. It is deliberate — fuzzing a model of the
mediation policy would prove nothing about shipping code.

The full external dependency set is unchanged from M003: `tokio`, `thiserror`, `async-trait`,
`base64`, `zeroize`, and `rusqlite` (M003-era, `bundled`, `default-features = false`).
Transitive `cc`/`pkg-config`/`vcpkg` arrive through `rusqlite`'s bundled build and are not
new.

MSRV is unchanged at **1.88**. The highest `rust-version` among external crates is
`zeroize 1.9.0` at 1.85. `rustup run 1.88.0 sh scripts/verify.sh full` passes.

One MSRV-driven change was required in this pass: `ctcp.rs` needed its format argument
inlined, because the 1.88 clippy run does not apply the `uninlined_format_args` change
automatically. It is a lint fix, not a behaviour change.

## Verification

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass, no warnings |
| `cargo test --workspace --all-features --locked` | pass — 464 tests across 17 binaries |
| `cargo tree --locked -e all` | no new external crate |
| `python3 scripts/check-network-boundary.py` | exit 0 |
| `scripts/fuzz-smoke.sh` | pass — 20,000 generated frames + 10,000 wire iterations |
| `scripts/verify.sh full` | pass |
| `rustup run 1.88.0 sh scripts/verify.sh full` | pass |

Test count is 464 across 17 binaries, up from 458 at the M004-C closure (`cffd8db`). The
six new tests are in `crates/runtime/tests/qualification.rs`. Both figures were measured by
running the workspace suite at those commits, not estimated.

Note: the 017 record states "448 tests passing across 16 test binaries". Re-running the
suite at its own implementation commit (`a0e13e3`) gives **458**, so that figure was already
wrong when written. It is left unmodified because Plan 018 section 4 requires historical
closure records to be preserved rather than rewritten; the discrepancy is recorded here
instead. It does not affect any M004 claim — the count is informational.

## Documentation reconciliation

Four architecture documents existed for M004's design but none for its M004 work. Written:
`ctcp-dcc-policy.md`, `reconnect-budget.md`, `resource-accounts.md`, `response-routing.md`.

Updated: `security-anonymity.md` (its closing paragraph still said CTCP/DCC filtering was
later work), `reconnect-and-liveness.md`, `overview.md`, `README.md`, `plans/registry.md`,
`plans/subsystems/bouncer-core-roadmap.md`.

Removed stale language: the README, registry, and roadmap all still described M004 as "not
yet decomposed" or "ready". Historical closure records 001–017 were **not** rewritten.

The `architecture/` directory was intact throughout. A stale note in the working session
suggested it was missing; it is not, and README's pointer to `architecture/overview.md`
resolves.

## Unresolved findings

| ID | Severity | Finding |
|---|---|---|
| UF-015-1 | medium | The legacy `NetworkSupervisor` at `crates/runtime/src/lib.rs:222` is gated by neither the `ReconnectScheduler` nor the resource ledger. It is a `pub struct` at the crate root with no `cfg` attribute, so it is compiled into the shipped rlib and appears in the generated public API; its `pub fn serve` performs a second unconditional upstream connect at `lib.rs:290`. Only its *call sites* live inside `#[cfg(test)]`, and nothing outside `lib.rs` constructs it — the production path is `catalog::NetworkSupervisor` over `owner::NetworkOwner`. Because the type is public API, "no test constructs it" is not the same as "no production code can". It must be gated or deleted before any promotion to production. |
| UF-017-1 | low | `NetworkOwner::serve` constructs `Backoff` inline (base 1s, cap 300s, jitter 20%) rather than injecting it, so churn campaigns must pin virtual time and advance by the cap. Not a defect; it constrains testability. |
| UF-018-1 | low | `catalog::MAX_TOTAL_SESSIONS` (`catalog.rs:295`) is declared with a doc comment claiming it bounds a catalog's total sessions, but no production code reads it and no test asserts it. The effective bound is `MAX_SUPERVISED_NETWORKS` × `MAX_SESSIONS_PER_NETWORK` = 4096, not the 1024 declared. The population is bounded; this constant is a stale duplicate of a bound that is enforced elsewhere. It should be enforced or deleted. |

The session population is bounded without UF-018-1 (the product of two enforced ceilings),
so none of the three is an unbounded-resource finding. None is blocking, and none is
anonymity-, egress-, herd-, routing-, or resource-bound.

UF-015-1 is the only one that touches an egress path: the legacy supervisor holds a second
ungated upstream connect. It is not reachable from the production entry point and nothing
outside its own test module constructs it, which is why it does not block closure — but it is
recorded at medium rather than low because the type is public API, so the guarantee is "no
production code calls this today" rather than "this cannot be called".

## Post-closure amendment: remedies revised by Corrective 019

A follow-up review of this record found all three findings real and found that **the
remedies proposed above were wrong in two of three cases**. Corrective 019
(`plans/implementation/bouncer-core/019-m004-findings-corrective.md`) owns the corrected work.
The findings are restated here rather than amended in place, so the record as accepted stays
readable and the correction is auditable.

| Finding | Remedy stated above | Corrected remedy | Why it changed |
|---|---|---|---|
| UF-015-1 | "gated or deleted", treated as equal options; severity medium | **Gate, do not delete**; severity low | 25 substantive tests depend on the legacy supervisor, and two behaviours it covers — the SASL PLAIN handshake (`owner.rs:1042-1084`) and the upstream `QUIT` on stop (`owner.rs:1453`) — have **no production-path test coverage at all**. Deleting would trade a cosmetic API wart for a silent coverage regression. Gating is ~10 `#[cfg(test)]` attributes. Severity is low because no production code exists that could call it: the workspace's only binary is `fuzz-smoke`, which references neither it nor `UpstreamConfig`. |
| UF-017-1 | "constrains testability", severity low | **Severity medium; the campaign is vacuous** | `a_stalled_provider_produces_a_bounded_number_of_attempts` advances virtual time once by 600s and measured **4 attempts — exactly one per Network**, sitting on its own lower bound. A spin loop and correct backoff are indistinguishable, which is the precise thing its doc comment claims to rule out. Cause: `tokio::time::advance` performs one poll, so a timer re-armed during it never fires. Repairing the test (looped small advances) measured 4 → 20 attempts with zero production change. |
| UF-018-1 | delete the unenforced constant | **Unchanged; confirmed stronger** | `git log -S` shows the constant has never been read since introduction at `646937e`. The 1024 figure traces only to `plans/closure/bouncer-core/012-status.md:116`, which presented an unenforced declaration as an enforced ceiling. Delete; do not enforce. |

Two further corrections recorded by that review:

- **A false doc comment, previously unrecorded.** `timeout_bounded`'s comment (`lib.rs:881-885`)
  claims a virtual clock cannot expire its deadlines without an explicit `advance()`. It is
  `tokio::time::timeout`; auto-advance fires it — a parked 120s deadline expired in 8
  microseconds of real time. This is load-bearing for the campaign defect above.
- **An overstated claim in the legacy supervisor.** The comment at `lib.rs:800-802` says it
  "shares this policy with the production owner rather than having its own". It shares the
  `ctcp::` module; the *enforcement site* is duplicated logic (`lib.rs:798-831` vs
  `owner.rs:1563`). There is no shared guard.

None of this changes any M004 claim. M004 closed correctly: the findings were real,
recorded honestly, non-blocking, and M004's own invariants were not weakened to reach
closure. What was wrong was the proposed remedy, and that is what Corrective 019 fixes.

Two audit observations were examined and deliberately not changed:

- `crates/store/src/testing.rs:82-84` uses `std::env::temp_dir()` and `std::process::id()` in
  a `pub mod testing` that ships in the store rlib. It is test-support code that no production
  path calls, and removing the module would be a scope expansion beyond M004. Recorded, not
  fixed.
- `crates/runtime/tests/privacy.rs:612` reads real host environment values as sentinels.
  That is intentional: real values make a leak attributable rather than merely suspicious,
  and the assertion is that they are *absent* everywhere.

## Independent audit

An independent read-only audit of this record against the repository checked every cited test
name and assertion, every constant value and type, the test-count figures at both commits, the
dependency and MSRV analysis, the boundary guard, and both unresolved findings. It confirmed
the substance — all cited tests exist and assert what is attributed to them, the constants are
exactly as stated, the 464/17 and 458/16 figures reproduce, and the dependency analysis is
accurate.

It found four overstatements, all corrected above: UF-015-1's "(test-only)" wording (D1), the
blanket "every ceiling is asserted" claim (D2), the Cargo.lock framing relative to the
record's stated baseline (D3), and an off-by-one on the named tag count (D5). The resulting
scoping of the reconnect matrix rows follows from D1 (D4).

Two further observations were informational and are recorded rather than changed:

- `a_command_refused_by_the_upstream_queue_is_reported_and_never_replayed` asserts the
  *reporting* half directly; the *non-replay* half is structural (a refused frame is never
  enqueued), not separately asserted.
- `qualification_diagnostics_carry_no_secret_or_endpoint_material` renders
  `NetworkSnapshot::default()`. The populated-snapshot case is covered by the new
  `a_full_generation_never_leaks...` composition test instead.

One coverage gap it found was real and is fixed: the new logging test scanned manifests and
sources over four crates while omitting `fuzz-smoke`, even though this milestone added
`fuzz-smoke` to the boundary guard's list and it calls production mediation code. The test now
covers it.

## Defects found and fixed during this pass

**1. The fuzz harness was outside the network-boundary guard.** `crates/fuzz-smoke` became a
first-party exerciser of production mediation code when M004-C gave it a path dependency on
`i2pr-irc-runtime`. A first-party crate that calls production code and is not guarded is a
gap in the guard, so it was added to `CRATES`.

**2. Dead mediation surface.** `mediate_client_tags` carried a `negotiated` parameter that
affected nothing, and `TagDisposition::Rejected` could never be constructed. Both were
removed. This changes no behaviour: only `label` survives mediation, and it did so before
and after.

**3. The logging test did not cover `fuzz-smoke`.** Found by the independent audit. The test
claimed to cover "every production path" while scanning four crates, omitting the one crate
this milestone had just made a first-party exerciser of production code. Now scanned.

## Stop conditions

Plan 018 section 8 forbids closing M004 with any of the following. None is present:

| Stop condition | State |
|---|---|
| a known DCC/direct-network path | None. `Dcc` carries no parameter and reaches no sink. |
| client-dependent upstream fingerprint | None. Registration and CTCP visibility are client-independent. |
| environment/secret leakage | None. See the negative matrix. |
| dead response-routing machinery | None. Routes open, route, and close on live traffic. |
| unbounded reconnect admission | None on the production path. Four explicit ceilings. The legacy supervisor in UF-015-1 sits outside them. |
| starvation | None. FIFO fairness proven. |
| task/queue/resource leak | None. Every campaign settles to baseline. |
| hidden replay of ambiguous user traffic | None. Refusals are reported, never retried; intents are generation-stamped. |
| a raised Rust MSRV | None. Still 1.88, verified under 1.88. |
| missing predecessor closure | None. All four M004 predecessor closures accepted. |

## M005 readiness decision

**M005 is planning- and research-eligible.**

Every roadmap exit condition for M004 is evidenced above, and no unresolved
high-severity anonymity, alternate-egress, reconnect-herd, response-routing, or
resource-bound finding remains.

M004 closure authorizes **no** router-specific implementation. Per
`plans/002-long-term-roadmap.md`, router R001 remains blocked until M005 closure, and R002
and R003 remain blocked on stable public i2pr app contracts.

No implementation plan for M005 may be written until the milestone is decomposed against the
repository state this closure produced. When one exists it must be registered in
`plans/registry.md` before implementation begins.