# Plan 031 — R001-C Per-Network SAM Provider Integration

Closed 2026-10-08. Outcome: **R001-C closed. The repository has a process-wide SAM scope map
that gives one durable Network one long-lived router-side identity, with an observable
control socket, a bounded request queue, a deliberate error taxonomy, and cumulative
diagnostics that survive a release. Five defects were found and fixed, three of them in
code this plan wrote. The connect-budget mismatch Plan 030 recorded is reconciled. No
unresolved high-severity finding remains.**

Repository baseline: `175a2fe` (Plan 030 closure, plus the `__pycache__` removal).

Authority: ADR-0004 (one router identity per Network), ADR-0005 (bounded, retryable
release), Research 007, the subsystem roadmap.

Primary class: infrastructure + invariant.

## 1. What landed

`crates/sam/src/provider.rs`, exporting `SamProvider`, and two qualification suites.

| Piece | Responsibility |
|---|---|
| `SamProvider` | the process-wide `I2pStreamProvider`, one scope per `NetworkId` |
| `ScopeEntry` | one scope's request sender, stop signal, join handle, liveness |
| `Totals` | the cumulative ledger, owned by the provider so it outlives its scopes |
| `ScopeHealth` | per-scope liveness and epoch, dropped with the scope |
| `scope_task` | the single owner of one Network's session |
| `map_error` | the SAM-to-provider taxonomy, deliberately coarse |

`crates/runtime` gained `i2pr-irc-sam` as a dependency and a dev-dependency with the
`testkit` feature, so the core-integration qualification drives the real provider.

## 2. The scope, and why it is shaped this way

One task per Network, not a shared pool. A SAM `STREAM CONNECT` names a session ID, and a
session belongs to one router-side I2P identity. Two Networks sharing one would let an
observer at the bridge correlate their IRC conversations, which is the whole reason ADR-0004
scopes the provider by `NetworkId`.

The map holds senders, stop signals, and join handles — never an `await`. A lock held across
an await is how one Network's slow connect stalls another's, and this map sits on the path
of every connect in the process. `ensure_scope` decides under the lock and spawns outside it.

**There is deliberately no release request.** `release` removes the map entry, which drops
the request sender, which closes the channel, which ends the task's receive loop — and
separately signals the stop flag so a task parked mid-exchange stops immediately rather than
after its deadline. A release message would be a third path to the same outcome, and three
paths to one event is where they start to disagree.

**The scope ceiling and release deadline are constructor arguments**, not imports. The
runtime depends on this crate, so importing its constants here would be a cycle. Passing the
value means the number the runtime enforces is literally the runtime's
(`Provider::with_limits`).

## 3. Deadline reconciliation (Plan 030 Finding 3)

Plan 030 recorded that the cold-session path is 140 s against a 120 s runtime budget, and
assigned the fix here.

| | Before | After |
|---|---|---|
| Runtime outer ceiling | `CONNECT_TIMEOUT` = 120 s | `PROVIDER_ACQUIRE_TIMEOUT` = 300 s |
| SAM cold path | 140 s | 140 s |
| Margin | **−20 s, a cold first connect could not finish** | +160 s |

The name changed with the value, deliberately: the constant no longer describes "connecting",
it describes acquiring a scoped stream, and a caller reading the old name would assume a
tighter bound than the one that exists. `PROVIDER_RELEASE_TIMEOUT` stays at 15 s, well under
the new ceiling so a delete can never wait on a release for longer than a connect.

The budget is asserted, not asserted-by-test alone: `the_cold_session_path_fits_inside_the_
runtime_acquire_budget` checks 140 s < 300 s and that two consecutive cold attempts fit
inside one budget, so the runtime cannot admit a second attempt it has no time to finish.

## 4. Session invalidation

Three events mean the router no longer knows this Network's identity. All three retire the
session, and each creates exactly one replacement on the next connect.

| Event | Detected by | Evidence |
|---|---|---|
| `INVALID_ID` on a stream | `invalidates_session`, synchronously, before the failure is returned | `an_invalid_session_id_invalidates_immediately` |
| The control socket ends | a watcher on the control socket, at the moment the end-of-stream arrives | `a_closed_control_socket_invalidates_and_creates_one_replacement` |
| `release` | dropping the map entry | `release_destroys_the_scope_and_is_idempotent` |

A peer-level failure is **not** in that list. `CANT_REACH_PEER` and a stream timeout say the
peer is unavailable, not that the identity is gone; tearing the session down there would
churn the router identity on every IRC outage, which is the exact failure the long-lived
session property exists to prevent
(`a_peer_failure_does_not_destroy_a_healthy_session`).

## 5. Requirement-to-evidence matrix

| # | Requirement | Evidence | Test |
|---|---|---|---|
| 1 | Two Networks ⇒ two SESSION CREATEs, distinct IDs | `sam_provider_scope` | `two_networks_get_two_distinct_sessions` |
| 2 | One Network, 100 reconnects ⇒ one session | wire, not a counter | `one_network_across_many_reconnects_keeps_one_session` |
| 3 | Peer `CANT_REACH` does not recreate the session | `acquire` | `a_peer_failure_does_not_destroy_a_healthy_session` |
| 4 | `INVALID_ID` invalidates; next attempt makes exactly one | `acquire` | `an_invalid_session_id_invalidates_immediately` |
| 5 | Control EOF invalidates | `watch_control` | `a_closed_control_socket_invalidates_and_creates_one_replacement` |
| 6 | Release destroys the scope; repeat and unknown are no-ops | `release` | `release_destroys_the_scope_and_is_idempotent` |
| 7 | Release of one Network leaves the others alone | `BTreeMap::remove` | `releasing_one_network_leaves_the_others_alone` |
| 8 | A connect after release starts a new lifetime, new identity | wire | `a_connect_after_release_creates_a_fresh_session` |
| 9 | Delete and shutdown leave zero scopes | controller lifecycle | `r001c_sam_core_integration::delete_and_shutdown_leave_no_scope_behind` |
| 10 | Scope count bounded | `max_scopes` | `the_scope_map_is_bounded` |
| 11 | Request queue bounded at max, refuses past it | `try_send` | `the_request_queue_is_bounded_and_refuses_rather_than_growing` |
| 12 | Concurrent first connects make one owner | `ensure_scope` under the lock | `concurrent_first_connects_create_one_scope` |
| 13 | No task leak after churn | counts return to baseline | `diagnostics_settle_after_churn` |
| 14 | No map lock held across an await | structural; the map holds only senders and handles | `provider.rs` §"Why the map is a plain `Mutex`" |
| 15 | Release interrupts a pending connect | `select!` around `acquire` | `a_release_answers_a_connect_that_is_still_pending` |
| 16 | An abandoned connect leaves the scope usable | task teardown | `an_abandoned_connect_leaves_the_scope_usable` |
| 17 | Real `RuntimeController<SamProvider>` | `r001c_sam_core_integration` | all 7 tests in that file |
| 18 | Fake IRC upstream over the returned raw stream | `fake::FakeIrcPeer` | `a_real_controller_registers_over_a_sam_stream` |
| 19 | IRC registration succeeds | `JOIN` after `001` | `a_real_controller_registers_over_a_sam_stream` |
| 20 | Forced IRC EOF reconnects through the same session | wire: one SESSION CREATE, two STREAM CONNects | `an_irc_eof_reconnects_through_the_same_sam_session` |
| 21 | Forced session loss ⇒ one new session, then IRC recovers | wire: two SESSION CREATEs, distinct IDs | `a_lost_session_is_replaced_once_and_irc_recovers` |
| 22 | No ambiguous replay across a reconnect | per-stream byte diff | `a_reconnect_replays_no_upstream_frame_from_the_old_connection` |
| 23 | Reconnect admission counts stay correct | `ProcessDiagnostics` | `reconnect_admission_counts_stay_correct_across_a_reconnect` |
| 24 | Diagnostics are secret-free | `Totals` has nowhere to put them | `diagnostics_are_secret_free`, `provider::tests` |
| 25 | An absent bridge is refused, not hung | `map_error` | `an_absent_bridge_is_refused_not_hung` |
| 26 | SAM errors map to bounded provider classes | `map_error` | `sam_errors_map_to_bounded_provider_classes` |

Requirement 20's evidence is the wire itself. The failure trace for
`an_irc_eof_reconnects_through_the_same_sam_session` during development recorded:

```
HELLO / SESSION CREATE ID=5bc27b3a… / HELLO / STREAM CONNECT ID=5bc27b3a…
HELLO / STREAM CONNECT ID=5bc27b3a…
```

One `SESSION CREATE`, two `STREAM CONNECT`s, one session ID. That is the claim, in the bytes.

## 6. Findings

**Finding 1 — high, fixed here. Cumulative diagnostics were erased by the release they
should have survived.** `diagnostics()` aggregated over the live map entries, so deleting a
Network zeroed every counter describing it. An Operator asking "what did this Network cost
before I deleted it?" got an answer of zero. Split into a provider-owned `Totals` ledger and
per-scope `ScopeHealth` liveness. The distinction is now also visible in the struct:
`max_epoch` is explicitly a live-scope reading, while `session_creations` is cumulative.

**Finding 2 — high, fixed here. Release did not interrupt an in-flight connect.** The stop
signal was only selected around the receive, so a release during a connect parked on a silent
router waited out the release deadline and reported `Timeout` — for a Network that was
already gone. `acquire` is now inside the same `select!`, and the pending caller's `oneshot`
closes rather than blocking to its own deadline.

**Finding 3 — medium, fixed here. A queue refusal was counted as a stream failure.** A
saturated queue never reached the router, but reporting it in `stream_failures` would send an
Operator looking for a failing router instead of a saturated budget. Split out as
`queue_refusals`.

**Finding 4 — medium, fixed here. `peak_queued` bounded nothing.** It counted requests being
*processed*, and one scope task processes one request at a time, so it read 1 under every
possible load. Now the high-water of the queue depth the owner actually sees
(`receiver.len() + 1`), which is the number the ceiling is about.

**Finding 5 — medium, fixed here. A test fixture ran the script dry and reported a
ten-second stall as a failure.** `Script::healthy(n)` scripted `n + 1` `HELLO`s, which is
correct for exactly one connect and wrong for every later one, because a connect is *two*
sockets and each says `HELLO`. Several tests therefore waited out the full hello deadline
instead of failing on a missing reply, and the provider suite took 120 s to report three real
failures. Corrected to `2 × connects`, with `Script::scoped(networks, streams)` for the
multi-Network cases the single-verb script cannot express. **120 s → 0.03 s.**

**Recorded, not fixed — control-socket EOF cannot be observed without a watcher.** Plan 031
section 13 lists "control EOF invalidates" as a required test. Before this plan the provider
held its control socket without ever reading it, so a router that discarded the session left
the scope reusing an identity that no longer existed and every later connect failed
`INVALID_ID` until something else invalidated it. A watcher task now reads the idle control
socket and clears a validity flag. It lives in `client.rs`, not `provider.rs`, so the network
boundary scan keeps holding the provider to the same rule as everything else (§7).

## 7. Network authority

Unchanged from Plan 030, and re-verified under the new code. `SamProvider` holds **no
socket**: the only socket-adjacent call it makes is `SamClient::into_socket`, handed straight
to `client::watch_control_socket`. `scripts/check-network-boundary.py` still passes with the
allowlist at `src/client.rs` and `src/fake.rs` only — a third file would have been the signal
that the provider had started owning TCP, which is the thing the scan exists to prevent.

## 8. Diagnostics: what an operator can learn and what they cannot

`SamDiagnostics` is counters and high-water marks only. `Totals` has nowhere to put a session
ID, a Destination, a nickname, a `NetworkId`, or a router message, which is what makes it safe
to project without a filter. `diagnostics_are_secret_free` renders the whole struct and
asserts none of those strings appear.

Deliberately absent: anything that would let two Networks be told apart. An operator can see
that *a* scope churned, not which one.

## 9. Commands actually executed

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `./scripts/check-network-boundary.py` | exit 0 (via `verify.sh quick`) |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clean (via `verify.sh quick`) |
| `cargo test --workspace --all-features --locked` | **885 passed, 0 failed** |
| `rustup run 1.88.0 cargo test -p i2pr-irc-sam --all-features --locked` | 0 failures (MSRV floor) |

Test counts: `crates/sam` unit 55, conformance 20, provider scope 17, core integration 7.

Note on suite runtime: the provider scope suite took **120 s** before Finding 5 and **0.03 s**
after. A slow suite is usually a missing reply being waited out rather than an assertion
failing, which is why the fix was to correct the fixture rather than to raise a timeout.

## 10. Security and privacy review

- **No cross-Network correlation.** One `NetworkId`, one session ID, one router-side
  identity. Proven on the wire (Finding 5's evidence and requirement 20/21) rather than by
  counting.
- **No replay across a connection generation.** The core-integration suite diffs each
  stream's bytes and requires the replacement to contain only a fresh registration. This is
  the repo's implementation posture about ambiguous delivery, checked where the generation
  actually changes.
- **No secret reaches a diagnostic or the wire.** `Totals` counts only. `SamError` remains 8
  bytes of closed enums. `SamRawStream` still derives no `Debug`.
- **Bounded.** Scope count, queue depth, request channels, the fixture's stream registry, and
  every wait in every test have explicit ceilings. Overflow is a refusal with its own
  counter, never a growing buffer.
- **Recovery.** A refused or failed connect leaves the Network configured and retryable; the
  session is replaced rather than poisoned; a release either completes or reports, and the
  controller's delete ordering from Plan 029 still holds (release failure skips durable
  removal).

## 11. Roadmap disposition

R001-C is closed.

Plan 032's hard dependency is Plan 031 and is now **unblocked**; its status moves from
`blocked` to `ready`. What Plan 032 must still do is stated as its own work and is *not*
claimed here: qualification against live Java I2P, i2pd, and i2pr. Nothing in this closure
substitutes for that evidence, and no test in this plan required a router to exist.

Plan 031's own stop conditions were not triggered: no stop condition applied to the provider
scope, the error taxonomy stayed coarse as the plan required, and the provider learned no SAM
syntax the runtime has to understand.