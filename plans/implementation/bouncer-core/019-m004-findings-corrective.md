# Bouncer Core Corrective 019 — Close M004 Findings and Restore Sole-Owner Evidence

Status: ready for handoff

Repository baseline: `f708011` ("Correct M004 closure record after independent audit")

Owns unresolved findings:

- UF-015-1 from `plans/closure/bouncer-core/018-status.md` (revised: gate, do not delete)
- UF-017-1 from `plans/closure/bouncer-core/018-status.md` (revised: severity raised, cause corrected)
- UF-018-1 from `plans/closure/bouncer-core/018-status.md` (confirmed as recorded)

Research authority:

- `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Historical implementation authority:

- `plans/closure/bouncer-core/015-status.md`
- `plans/closure/bouncer-core/018-status.md`

Primary class: invariant + testability corrective

## 1. Objective

M004 closed cleanly with three non-blocking findings recorded. Independent review of that
closure record showed all three are real and that **the remedies the record proposed are
wrong in two of the three cases**. This corrective closes them correctly and repairs the
evidence damage that leaving them open causes.

Nothing here changes protocol behaviour, the network boundary, or any dependency. The work
is: remove a duplicate network owner from the shipped public API, delete an unenforced
stale constant, repair one vacuous campaign, and port two production behaviours that are
currently tested only by the code being removed.

The last item is the most important. It is not cleanup.

## 2. Required invariants

1. Exactly one Network owner implementation ships. `catalog::NetworkSupervisor` over
   `owner::NetworkOwner` is it.
2. The shipped public API exposes no second upstream-connect path.
3. No production behaviour loses its only test coverage.
4. Every externally controlled collection still has an explicit, enforced ceiling.
5. No test asserts a bound it cannot distinguish from the failure it claims to detect.
6. No change widens the I2P-only network boundary.
7. No third-party dependency is added and MSRV stays 1.88.

## 3. Scope

### In scope

- gate the legacy supervisor and its exclusive types out of the production build;
- delete `catalog::MAX_TOTAL_SESSIONS`;
- correct the stale ceiling row in `plans/closure/bouncer-core/012-status.md`;
- repair the stalled-provider campaign so it can actually detect a spin;
- correct the `timeout_bounded` doc comment;
- port the SASL handshake and upstream-QUIT tests onto the production path;
- annotate the stale "shared policy" comment in the legacy supervisor;
- update the registry, roadmap, and `018-status.md` findings table.

### Out of scope

- injecting `Backoff` into `NetworkOwner::serve` (see §6);
- deleting the legacy supervisor and its 25 tests (see §4);
- any M004 invariant change;
- M005 scope of any kind;
- router integration.

## 4. UF-015-1 — gate the legacy supervisor, do not delete it

The closure record offered "gated or deleted" as equal options. They are not equal, and the
difference was never measured.

### What ships today

`crates/runtime/src/lib.rs` declares `NetworkSupervisor` at line 222 with **no `cfg`
attribute**. The only `cfg(test)` in the file is the `#[cfg(test)]` attribute at line 1047. The type and
its `impl` are therefore compiled into the shipped rlib and appear in the generated public
API, and `pub async fn serve` performs an unconditional `I2pStreamProvider::connect` at
line 290 — outside both `ReconnectScheduler` admission and `ResourceLedger` accounting.

Only the *call sites* are test-only. Nothing outside `lib.rs` constructs it. The workspace's
only binary is `i2pr-irc-fuzz-smoke`, which references neither `NetworkSupervisor` nor
`UpstreamConfig`. The risk is therefore entirely prospective — but a public API that
contains a second ungated connect path is exactly the thing `ADR-0001` says must not exist,
so it should not remain reachable by accident.

### Why deletion is the wrong remedy

25 substantive tests depend on the legacy supervisor, 9 of them under
`#[tokio::test(start_paused = true)]`. Two behaviours those tests cover **have no coverage
on the production path at all**:

| Behaviour | Production implementation | Production test coverage |
|---|---|---|
| SASL PLAIN upstream handshake | `owner.rs:1042-1084` exists and is live | **none** — no integration test drives `AUTHENTICATE`; only `lib.rs:2190-2195` does |
| Bounded upstream `QUIT` on explicit stop | `owner.rs:1453` sends `QUIT :Bouncer shutting down` | **none** — no test anywhere asserts it |

Deleting the supervisor would trade a cosmetic API wart for a real, silent coverage
regression on the shipping path. That is the wrong trade. Gate instead.

### Required change

Add `#[cfg(test)]` to these items in `crates/runtime/src/lib.rs`:

- `AcceptFuture` (line 214);
- `pub struct NetworkSnapshot` (line 186) — legacy-only; `owner.rs:541` defines its own;
- `pub struct UpstreamConfig` (line 130) and `impl UpstreamConfig` (line 138) — zero external
  users;
- `pub struct NetworkSupervisor` (line 222) and `impl<P: I2pStreamProvider>` (line 227).

These items must **remain ungated** because production code depends on them:

| Item | Production user |
|---|---|
| `timeout_bounded` (886) | `owner.rs:973`, `owner.rs:2449` |
| `error_class` (903) | `owner.rs:919` |
| `valid_client_nick` (919) | `session.rs:447`, `downstream.rs:480`, `state.rs:648` |
| `next_queued_frame` (946) | `downstream.rs:190` |
| `next_upstream_frame` (955) | `downstream.rs:243` |
| `write_frame` (970) | `owner.rs:1150,1152`, `downstream.rs:193,245` |
| `Backoff` (1021) | `owner.rs:815` |

`queue_control` (933) also stays: it has no production user, but the test
`control_queue_is_separate_and_normal_overflow_is_explicit` (line 1238) uses it and does not
depend on the supervisor. Deleting it would break a live test.

Leave in place the private helpers the supervisor uses (`apply_upstream_line` 784,
`next_accept`, `read_client`, `client_writer_exit`, `next_intent_frame`, `send`). They become
dead code once the supervisor is gated, and `clippy -D warnings` will surface them; removing
them is part of this corrective, but only after the build is green without them.

### Resulting invariant

"No production code calls the legacy supervisor" becomes "the legacy supervisor does not
exist in the production build". That is the distinction the closure record failed to draw.

## 5. UF-018-1 — delete `MAX_TOTAL_SESSIONS`

Confirmed as recorded, and stronger: `git log -S` shows the constant has **never been read
since it was introduced** at `646937e`. It is declaration-only from birth.

The 1024 figure appears in exactly one place outside its definition:
`plans/closure/bouncer-core/012-status.md:116`, in a table headed *"Every externally
controlled quantity has an explicit ceiling"*. That row presents an unenforced declaration
as an enforced ceiling. No canonical document ever specified a process-wide session cap, so
there is no design intent to recover.

### The real bound

| Factor | Value | Enforcement |
|---|---|---|
| Supervised Networks | 64 | `catalog.rs:210-215` |
| Sessions per Network | 64 | `owner.rs:1492` (`Err(QueueOverloaded)`), re-applied `owner.rs:1195` |
| Queued inbound attaches per Network | 256 | `catalog.rs:36`, via `try_send` `catalog.rs:88-94` |

Product: **4096** concurrent sessions, 16384 queued attaches. Every term is finite and
explicitly ceilinged.

### Required change

- delete `catalog::MAX_TOTAL_SESSIONS` (`catalog.rs:293-295`);
- correct the stale row at `plans/closure/bouncer-core/012-status.md:116` so a future
  auditor cannot re-derive a false bound from it.

Do **not** enforce 1024. Doing so would be a behaviour change that could refuse legitimate
attaches at a number with no product requirement behind it. If a 1024 process-wide cap is
wanted as policy, that is a new milestone decision with a stated rationale.

## 6. UF-017-1 — repair the vacuous campaign

The closure record described this as "constrains testability". That understates it. The
campaign `a_stalled_provider_produces_a_bounded_number_of_attempts` (`adverse.rs:585`) does
not test what it claims.

### Measured defect

It advances virtual time once by 600s and asserts `NETWORKS <= attempts <= NETWORKS * 32`.
Measured against the real crate: **4 attempts — exactly one per Network**, sitting precisely
on the test's own lower bound and 32x below its upper bound per Network.

Its doc comment claims the test pins virtual time so that "fast" and "at the scheduled rate"
stop being distinguishable. A hot spin loop and correct backoff are currently
indistinguishable in exactly this test — the thing it says it rules out.

### Cause

`tokio::time::advance` performs a single poll. A timer re-armed during that poll never
fires, so the chain `connect → backoff → retry` advances one link per `advance()` call, not
one per virtual second. One call exercises exactly one attempt.

### Required change

In `crates/runtime/tests/adverse.rs`, replace the single `advance(600s)` with a loop of small
advances, and tighten the bound.

Verified during planning: advancing `1000 x 600ms` instead of once by 600s takes attempts
from **4 to 20** (5 per Network) with **zero production changes**, and the test still runs
in well under a second. That is a real signal that a spin would not produce.

The upper bound must be tightened to match the measured backoff schedule (approximately
`NETWORKS * 6`), with a comment recording the measured value and why. A bound loose enough
to admit a spin is not a bound.

### Also correct

`timeout_bounded`'s doc comment (`lib.rs:881-885`) claims "a virtual monotonic clock must
not be able to expire them without the test actually advancing it". This is false. It is
literally `tokio::time::timeout`, fully governed by the pausable runtime clock, and
auto-advance fires it with no `advance()` call — measured: a parked 120s deadline expired in
8 microseconds of real time. This matters beyond documentation, because auto-advance rather
than backoff is what produces the single attempt in the campaign above. Correct the comment
to state the actual behaviour.

### Deliberately not doing

Injecting `Backoff` into `NetworkOwner::serve` is the principled follow-up and would enable
schedule assertions, `jitter_percent: 0` determinism, and deterministic `stable_online`
verification. It is a strictly larger change than the defect warrants — it touches a
shipping constructor's signature. Repairing the test is the correct fix at this severity.
Record the injection as a candidate for M005's "bounded history search / operator
diagnostics" work if a test there needs the schedule pinned.

## 7. Port the two uncovered production behaviours

This is the substantive part of the corrective and is required regardless of §4's
disposition.

### SASL PLAIN upstream handshake

`owner.rs:1042-1084` implements the `AUTHENTICATE` exchange, but no integration test drives
it. Port the coverage from `lib.rs:2165`/`lib.rs:2208` to a production-path test in
`crates/runtime/tests/` that asserts:

- the bouncer sends `AUTHENTICATE PLAIN` upstream when configured with SASL;
- it answers `AUTHENTICATE +` with the base64 credential;
- a rejected or absent credential yields a bounded terminal registration error;
- no credential material reaches a diagnostic, a snapshot, or downstream fanout.

The last assertion already exists in `privacy.rs:666` for a restored configuration; keep it
and add the wire-level coverage it does not provide.

### Bounded upstream `QUIT` on explicit stop

`owner.rs:1453` sends `QUIT :Bouncer shutting down`. No test asserts it. Add a
production-path test that asserts:

- an explicit stop writes exactly one `QUIT` upstream;
- no queued user traffic reaches the network after that fence (the property
  `network-ownership.md` already claims);
- a failed generation is aborted instead, and sends no `QUIT`.

## 8. Documentation corrections

- Annotate the comment at `lib.rs:800-802`, which says the legacy supervisor "shares this
  policy with the production owner rather than having its own". It shares the `ctcp::`
  module, but the *enforcement site* is duplicated logic (`lib.rs:798-831` vs
  `owner.rs:1563`). The comment implies a shared guard that does not exist. Once the
  supervisor is gated this becomes moot for production, but the test build still has two
  copies and the comment must not overstate.
- Correct `plans/closure/bouncer-core/012-status.md:116` per §5.
- Update the findings table in `plans/closure/bouncer-core/018-status.md` to record that the
  stated remedies were wrong and the corrected ones applied here. Do not rewrite 018's
  matrices; amend only its findings section.
- Register this plan in `plans/registry.md` and `plans/subsystems/bouncer-core-roadmap.md`.

## 9. Required tests

- the legacy supervisor, `UpstreamConfig`, legacy `NetworkSnapshot`, and `AcceptFuture` are
  absent from the production build (assert via a `#[cfg(not(test))]`-guarded check or by
  `cargo doc` output, whichever is cheaper and honest);
- the 25 legacy tests still compile and pass under `#[cfg(test)]`;
- `MAX_TOTAL_SESSIONS` no longer exists and no source references it;
- stalled-provider campaign produces a measurable attempt count strictly above the lower
  bound and within the tightened ceiling;
- a simulated spin loop would fail the stalled-provider bound (mutation check);
- SASL handshake, accepted and rejected, on the production path;
- no SASL material in diagnostics, snapshot, or downstream bytes;
- explicit stop writes one upstream `QUIT` and nothing after it;
- failed generation writes no `QUIT`;
- `scripts/check-network-boundary.py` still exits 0;
- `scripts/verify.sh full` and `rustup run 1.88.0 sh scripts/verify.sh full` green;
- `Cargo.lock` unchanged.

## 10. Acceptance criteria

Corrective 019 closes only when:

1. the shipped public API contains exactly one Network owner and one upstream-connect path;
2. SASL and upstream-QUIT behaviour are covered on the production path, so the legacy
   tests are redundant rather than load-bearing;
3. the stalled-provider campaign can distinguish a backoff schedule from a spin;
4. no unenforced constant remains presented as an enforced ceiling.

Criterion 2 is the one that matters most. A cosmetic API cleanup that silently drops
production coverage has made the codebase worse while looking better.

## 11. Stop conditions

Stop and register a new design if:

- porting the SASL test reveals the production handshake is behaviourally different from the
  legacy one — that is a defect in the shipping path and needs its own corrective, not a
  quiet test rewrite;
- gating the legacy supervisor requires changing any signature a production caller depends
  on;
- removing the legacy supervisor's private helpers proves any of them is load-bearing;
- tightening the stalled-provider bound requires changing production timing behaviour.

## 12. Closure evidence

Create `plans/closure/bouncer-core/019-status.md` including:

- the gated-items table from §4 with before/after reachability;
- the ported SASL and QUIT test names and what each asserts;
- the measured stalled-provider attempt count before and after the repair;
- the `MAX_TOTAL_SESSIONS` deletion and the real enforced bound with file:line evidence;
- the full test-count delta;
- exact verification commands;
- the disposition of each of UF-015-1, UF-017-1, UF-018-1;
- an explicit statement of whether this unblocks or changes M005 planning, which is
  expected to be neither.