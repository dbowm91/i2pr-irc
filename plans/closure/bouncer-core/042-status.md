# Bouncer Core Corrective 042 — Closure

Status: closed

Plan: plans/implementation/bouncer-core/042-post-m007-monitor-and-adverse-qualification-corrective.md

Repository baseline recorded by the plan: 0852bf74fdcaabf008df8e23c087533bf948972c

M006 and M007 remain historical closures and their closure records are retained
unchanged. Corrective 042 supersedes only the readiness claims those records make about
MONITOR event semantics and adverse-network evidence scope.

## 1. Dispositions

| Finding | Disposition |
|---|---|
| C042-F1 — `730` RPL_MONONLINE treated as a negative snapshot | Fixed. `730` no longer uses absence semantics anywhere. |
| C042-F2 — bare `MONITOR` and positive limits above the local ceiling rejected | Fixed. `MonitorSupport` represents the three spellings; only disabled/unparsable/absent fall back to `ISON`. |
| C042-F3 — destructive Eggchaos evidence only through a generic echo peer | Fixed. Four scenarios now run the production `SamProvider` path through the pinned proxy. |
| Registry/roadmap drift | Fixed. Registry, roadmap, and the bouncer-core current-state text reconciled. |

No stop condition in §15 was reached. In particular the MONITOR fix did not require a
downstream monitor broker, no downstream MONITOR command or new IRCv3 feature was added,
eggchaos did not become a Cargo or production dependency, durable identity semantics were
not changed, and no recovery path replays ambiguous user traffic.

## 2. What the defect actually was

The old rule read one rule out of three different commands:

~~~rust
730 | 303 => free = !preferred_listed
731       => free =  preferred_listed
~~~

`731` names the targets that went offline, so naming the preferred nick is the evidence.
`303` answers a query, so absence from the returned set is the evidence. `730` does
neither: it *reports targets that came online*, so `:srv 730 bot :alice` says alice is
online and says nothing whatsoever about bot. Reading absence there meant any unrelated
online event started a reclaim of a nick the server never commented on.

`730` is now an event with two rules and no absence rule at all: an unrelated `730` is
*nothing*, and a `730` that names the preferred nick *retracts* free evidence that is
already pending, because the nick is demonstrably in use at that moment.

## 3. Command-specific evidence matrix

Enforced in one table, `presence::reclaim_evidence(command, preferred_listed, probe_outstanding)`.

| Command | Preferred nick listed | Result | Meaning |
|---|---|---|---|
| `731` | yes | free evidence | names the targets that went offline |
| `731` | no | nothing | says nothing about the preferred nick |
| `730` | yes | retracts evidence | the nick is in use right now |
| `730` | no | nothing | silence is not a statement |
| `303` | no, probe outstanding | free evidence | absent from the answer to a query we sent |
| `303` | no, no probe outstanding | nothing | an unsolicited reply answers no question we asked |
| `303` | yes | retracts evidence | present in the returned online set |
| anything else | — | nothing | recorded as silence, never as negative evidence |

`303` is gated on the generation having actually sent the `ISON` it answers. The strategy
is recorded on the reclaim attempt when the generation opens it, so a generation that asked
through `MONITOR` has no outstanding query and cannot read absence out of a stray `303`.

## 4. ISUPPORT MONITOR matrix

Enforced in `presence::parse_monitor_support` and `presence::reclaim_strategy`.

| ISUPPORT token | `MonitorSupport` | Strategy |
|---|---|---|
| `MONITOR` | `Unlimited` | `MONITOR` |
| `MONITOR=1` | `Limited(1)` | `MONITOR` |
| `MONITOR=4` | `Limited(4)` | `MONITOR` |
| `MONITOR=100` | `Limited(100)` | `MONITOR` |
| `monitor=4` / `MoNiToR=0` | case-insensitive match | as above |
| `MONITOR=0` | `Disabled` | `ISON` |
| `MONITOR=abc`, `MONITOR=`, `MONITOR=-1` | `Disabled` | `ISON` |
| absent | `Absent` | `ISON` |

`Absent` and `Disabled` both fall back to probing but are kept as distinct values because
they are different facts about the server: one never mentioned the feature, the other
refused to serve it.

`MAX_MONITOR_TARGETS` is now documented for what it is: a ceiling on what one `MONITOR`
request may contain, not on what a server advertises. This bouncer watches exactly one
nick, so a server permitting a hundred has permitted the one that will be asked for.
Rejecting `MONITOR=100` because the local build would never monitor a hundred targets is a
refusal to use an available feature.

## 5. Changed source files

| File | Change |
|---|---|
| `crates/runtime/src/presence.rs` | `MonitorSupport`, `parse_monitor_support`, reworked `reclaim_strategy`, `ReclaimEvidence`, `reclaim_evidence`, strategy recorded on `ReclaimAttempt`, `probe_outstanding`, unit matrix |
| `crates/runtime/src/owner.rs` | evidence handling rewritten against the table; strategy recorded at generation open so `begin_reclaim` no longer re-derives it |
| `crates/runtime/src/state.rs` | `monitor_limit() -> Option<usize>` replaced by `monitor_support() -> MonitorSupport` |
| `crates/runtime/tests/m005c_presence_nick.rs` | full conformance matrix; harness nick parameterised so the server's casemapping can actually be observed |
| `crates/runtime/tests/m007_eggchaos.rs` | four product-path scenarios and a shared fixture |
| `scripts/qualify-m007-eggchaos.py` | evidence-strength sections; per-scenario execution; no dependency on an unrelated `rtk` on `PATH` |
| `qualification/m007/fault-smoke.py` | ports held until immediately before the proxy binds them |
| `qualification/m007/scenarios-v2.md` | new; supersedes v1, which is retained unchanged |
| `architecture/presence-and-nick.md` | event/snapshot distinction and MONITOR interpretation rewritten |

### Tests that would have caught the defect

The new vectors were verified to actually discriminate, by temporarily reintroducing each
old behaviour and confirming the specific test fails:

| Reintroduced behaviour | Test that failed |
|---|---|
| `730`/`303` shared absence rule | `an_unrelated_monitor_online_event_never_claims_the_preferred_nick`, `an_ison_reply_nobody_asked_for_is_not_a_snapshot`, `a_monitor_online_event_is_never_free_evidence`, `an_ison_snapshot_is_evidence_only_as_an_answer_to_an_own_probe` |
| `730` treated as inert (no retraction) | `a_monitor_online_event_retracts_free_evidence_the_server_has_just_withdrawn`, `a_monitor_online_event_is_never_free_evidence` |
| bare `MONITOR` unrecognised, limits above `MAX_MONITOR_TARGETS` rejected | `every_usable_monitor_advertisement_watches_the_preferred_nick`, `reclaim_uses_monitor_for_bare_and_positive_advertisements`, `monitor_token_names_are_matched_case_insensitively` |

The casemapping vector uses the preferred nick `[bot]` rather than a case variant, because
`ascii` casemapping folds case exactly as `rfc1459` does — the only thing that separates
them is the bracket family. With `[bot]` the same `731 bot :{bot}` frame reclaims under
`rfc1459` and does nothing under `ascii`, which is the honest demonstration that the
server's mapping decides nick identity.

## 6. External qualification

| | |
|---|---|
| eggchaos version | 0.2.0 |
| pinned source commit | `b6a277d5ad4267bd602bc15a4333b14322057b90` |
| binary SHA-256 | `b4e8f8588e034f01bd5714dd7202d7775a01ca89710623867e22a5a902f433fe` |

Run with `EGGCHAOS_BIN=<pinned binary> python3 scripts/qualify-m007-eggchaos.py`. The
runner reports NOT RUN, never PASS, when the pinned executable is absent.

### Generic fault smoke — fault-tool capability only

Loopback echo peer, all eight phases PASS: repeated churn (100 cycles), stable, bandwidth,
slow-close, slicing, blackhole, disconnect, stream-loss.

This establishes what the pinned fault tool does. It says nothing about the bouncer.

### Product path — `SamProvider` -> eggchaos -> fake SAM bridge

| Scenario | Outcome |
|---|---|
| A1 latency/jitter/bandwidth/slicing | PASS — 20 s at 10 ms ± 5 ms then 90 s at 50 ms ± 20 ms produced no reconnect; `stream_attempts=1` throughout |
| A2 blackhole | PASS — no premature disconnect in 20 s; liveness ended the generation inside the bounded ceiling; recovery to `generation=2`, `stream_attempts 1->2`, `queue_refusals=0` |
| A3 hard disconnect | PASS — `generation=2`, `session_creations=1`, `session_losses=0`, `stream_attempts=2`, `stream_failures=0` |
| A4 replay disposition | PASS — the replacement generation wrote only `NICK`, `USER`, `CAP`, `JOIN`, and its own `PING` |

A2 is the one that earns its keep. A blackhole produces no EOF, no reset, and no error: the
socket stays nominally open forever. The test asserts *both* halves of that — the
generation must survive a bounded quiet window untouched (a silent transport is not a dead
one, or every idle Network would reconnect forever), and it must then end inside a bounded
ceiling, because nothing else can end it.

A2 is run against a downstream blackhole and healed on the first observation of the loss
rather than on the arrival of the replacement generation. The catalog sleeps about a second
in backoff before it tries again, so leaving a fault armed across that gap kills the
replacement connection too and turns one fault into an unbounded reconnect fight.

### Resource accounting

Asserted from the production provider, per scenario: `live_scopes == 1` throughout,
`healthy_scopes == 1` after recovery, `queue_refusals == 0` (recovery never stampedes the
bounded request queue), `stream_failures == 0` on the clean paths, and after `delete` the
scope returns to baseline within the ceiling.

### What is deliberately not claimed

**The accepted-stream multi-client boundary.** There is no standalone downstream listener in
this repository and Corrective 042 forbids adding one, so sessions cannot be attached
through this path. `sessions_attached` is asserted to be `0` before and after recovery
rather than being quietly claimed as covered. The multi-client claim stays where it is
honest: the in-process owner suites.

**Stream-loss as packet loss.** eggchaos stream-loss drops arbitrary application bytes with
no TCP semantics — no segment, no checksum, no retransmission — so whether a connection
survives depends on which byte was dropped. Dropping one byte of a SAM command line leaves
the client waiting for a reply that can no longer arrive; dropping one byte of an IRC frame
leaves an unparseable prefix. Neither is what real packet loss does to a TCP path, where
the transport retransmits and the application usually never notices. Asserting reconnect
semantics through it would be asserting byte-drop luck rather than bouncer behaviour, so
it is qualified against the generic echo peer and recorded here as a fault-tool capability
only. Loss behaviour is covered instead by A1's `slicing`/`bandwidth` paths and the
in-process `adverse` suite.

### Three fixture corrections worth recording

Each of these presented as "recovery never happens" and none of them was about recovery:

1. **The fake bridge has no background reader.** IRC bytes are only read when the test
   pumps them. A standing in-process server now pumps and answers each new registration
   exactly once — answering repeatedly would re-arm `CAP` negotiation after every `001` and
   never finish registering.
2. **A graceful termination of one direction is not a disconnect.** eggchaos's default
   disconnects half-close, leaving the client's socket half-open: writes vanish with no
   error and no EOF, and the bouncer waits in `registering` forever. A3 uses `hard_reset`.
3. **Ports chosen by binding and closing are not reserved.** They are free for the next
   allocation too, usually one of the test's own outbound connections. The failure then
   surfaces much later as a connection refusal against a fault nobody changed. Ports are
   now held until immediately before the proxy binds them, and the Rust fixture allocates
   its own rather than receiving them across a process spawn.

## 7. Repository verification

All green on the closing tree:

| Check | Result |
|---|---|
| `./scripts/check-network-boundary.py` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS |
| `cargo test --workspace --all-features --locked` | PASS |
| `./scripts/fuzz-smoke.sh` | PASS |
| `./scripts/verify.sh full` (current stable 1.89.0) | PASS |
| `rustup run 1.88.0 sh scripts/verify.sh full` (declared MSRV) | PASS |

The boundary scan is worth a specific note: the first draft of the Eggchaos fixture
reserved its loopback ports with its own `std::net::TcpListener` and the scan correctly
rejected it. Network ownership in this repository is confined to two named files in the
SAM crate, so the fixture now allocates through the fake bridge's own
`absent_endpoint()` helper. A fixture that quietly widens the boundary it is meant to
qualify would have been the wrong outcome for a corrective that exists to tighten claims.

The Eggchaos qualification is external, is not invoked by `verify.sh`, and is absent from
Cargo; it does not gate the MSRV floor.

### One MSRV run was not clean on the first attempt

The first `rustup run 1.88.0 sh scripts/verify.sh full` failed
`m005g_member_state::a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed`.
Re-running the identical command passed with exit 0, and the test passes standalone on both
toolchains.

That test's own comment documents the race: it needs the `JOIN` and `MODE` to be processed
before the client attaches, and says outright that on a slow scheduler "a client [can]
register first, observe a completed run, and pass for the wrong reason -- or fail depending
on which order the two racing events win". It is a pre-existing flake in a file this
corrective does not touch (`m005g_member_state.rs` is unchanged since Plan 039), and it is
unrelated to MONITOR semantics or to the Eggchaos fixture.

It is recorded here rather than quietly dropped because "the suite is green" and "the suite
is green after a retry" are different claims. The flake is a real, known defect in that
test's synchronization and is worth a follow-up corrective of its own; it is out of scope
here because fixing it would mean changing membership-state test synchronization with no
bearing on any claim this corrective makes.

## 8. Planning reconciliation

- `plans/registry.md`: Corrective 042 moved from ready to closed; the active table is empty
  again; the latest-handoff section no longer names a closed plan as the handoff.
- `plans/subsystems/bouncer-core-roadmap.md`: current state records M006/M007 as closed
  and Corrective 042 as the corrective that superseded their affected readiness claims.
- `plans/closure/bouncer-core/035-status.md` and `041-status.md` are retained unchanged.
  035's `730` row is historically accurate for what the code did; 042 supersedes it rather
  than rewriting it.
- `qualification/m007/scenarios-v1.md` is retained unchanged as the Plan 041 evidence
  document; `scenarios-v2.md` is current.

### Nothing was unblocked by this corrective

No successor plan moved to ready. R002 stays blocked on stable public i2pr managed-app
I2P-stream/local-listener/lifecycle contracts, which is an external dependency this
corrective does not touch, and R003 stays research-blocked. Privacy/encryption and the
standalone listener remain intentionally unplanned behind their own research gates — the
accepted-stream boundary this closure explicitly declines to claim is the same boundary
those gates govern, so nothing about closing 042 makes it reachable.

The active table is therefore genuinely empty rather than empty by omission. The one
follow-up this work surfaced is the flaky `m005g_member_state` synchronization described in
§7; it is a test defect rather than a product one, so it belongs in a successor corrective
raised on its own merits rather than as a dependency of anything already open.

## 9. Supersession

M006 and M007 closures remain historical and retained. Corrective 042 supersedes only
these specific readiness claims:

- the `730` interpretation recorded in the 035 closure;
- the adverse-network evidence scope recorded in the 041 closure, which described
  destructive coverage through in-process suites and a generic echo peer and now names the
  product-path scenarios that replaced it;
- the registry handoff that pointed at closed plans.

Everything else those closures established — reclaim boundedness, refusal cooldown,
generation ownership, durable identity, reconnect budgeting — stands unchanged.