# Bouncer Core Corrective 049 — Closure Status

Status: closed

Implementation commit:

- `9615803` — `fix(sam): reassemble SAM replies split across TCP reads without re-feeding`

## Transient failure identity (Plan 048 handoff)

Plan 048's closure records that the first `rustup run 1.88.0 sh scripts/verify.sh full`
encountered one transient `SessionCreate` timeout in an unrelated SAM fragmented-reply
conformance test, which passed in isolation and on a complete rerun. No retained
full-verification log survived, so the closure wording is treated as the starting
hypothesis rather than established test identity; the hypothesized test is
`a_fragmented_reply_produces_the_same_result` in
`crates/sam/tests/sam31_conformance.rs`.

## Reproduction

The flake reproduced on current stable without any parallel load. A scratch loop with
5-second phase deadlines failed at iteration 17 with:

- `Timeout { phase: StreamConnect }` (the Plan 048 instance named `SessionCreate`;
  same test, same root cause, different phase — see below);
- the fake had received all three requests (`HELLO`, `SESSION CREATE`,
  `STREAM CONNECT`) on one connection, so the reply was written but never accepted.

Instrumented reruns showed the exact stall: the client read the complete 25-byte
`STREAM STATUS RESULT=OK` reply, then waited for more bytes until the phase deadline.
The failure recurred whenever the reply's trailing `CR LF` straddled two socket reads,
which byte-at-a-time fixture writes make likely (~5–8% per exchange pre-fix).

## Root cause: production framing defect (C049-F1)

`SamClient::next_line` (`crates/sam/src/client.rs`) took the `LineReader`'s partial
line back out via `take_partial()` on every loop iteration and re-fed it ahead of
newly read bytes. `LineReader` holds a trailing `CR` back in its `pending_cr` flag
rather than in the partial buffer. When `CR` and `LF` arrived in separate reads, the
re-fed stale bytes arrived while `pending_cr` was still set; the first re-fed byte was
not `LF`, so the held-back `CR` was baked into the line as ordinary content. The
completed line then contained an embedded `CR` and `finish()` refused it as
`EmbeddedControl`. The refused line was silently dropped, `ready` stayed empty, and
the phase stalled to its full deadline (`Timeout`), despite the router having answered
correctly.

Production impact is real and not fixture-specific: any TCP segmentation splitting a
reply terminator — routine under the high-latency/lossy conditions this bouncer is
built for — dropped that reply and stalled the exchange for up to the full phase
deadline (10 s hello, 120 s session-create, 90 s stream-connect). The removed re-feed
also double-counted every framing offset (`fed`, `inflight`) for an O(n²) feed cost
per reply; each socket byte is now fed exactly once, which is also what makes the
`inflight` bound (line ceiling plus one read chunk) actually hold.

## Fix (bounded production correction + deterministic regression)

Changed files (no production timeout, deadline, retry, or scope change):

- `crates/sam/src/client.rs` — `next_line` no longer takes the reader's partial line
  back out; the reader retains its partial line and CR holdback across feeds, and the
  `pending` buffer holds only bytes the reader has not yet seen. Field/method
  documentation updated to the single-feed contract.
- `crates/sam/src/fake.rs` — test-only `Script::split_crlf` mode writes each reply in
  two writes split between the trailing `CR` and `LF` with a brief pause, pinning the
  exact boundary deterministically. `fragment` (byte-at-a-time) is unchanged and still
  proves arbitrary-boundary reassembly.
- `crates/sam/tests/sam31_conformance.rs` — new
  `a_crlf_split_across_reads_produces_the_same_result` regression test.

The pre-fix code fails the new regression test deterministically (hello-phase
deadline, 10 s); the fixed code passes it. No retry/ignore/relaxation mechanism was
introduced, and the SAM fragmented-reply property remains covered twice: arbitrary
splits (`fragment`) and the pinned terminator split (`split_crlf`).

## Requirement-to-evidence matrix

| Requirement | Evidence |
|---|---|
| Transient SAM timeout investigated, not rerun-greened | Reproduced pre-fix (~5–8%/exchange scratch loop; instrumented stall trace); root-caused to client re-feed vs CR holdback; fixed with evidence above. |
| No hidden failure (retry/ignore/relaxed timeout) | No retry, ignore, or production-timeout change in the diff; new test fails pre-fix and passes post-fix. |
| Fragmentation property retained | `a_fragmented_reply_produces_the_same_result` unchanged and green; `two_replies_in_one_segment_are_both_read` and over-long/raw-transition tests green. |
| Targeted stress green | Fragmented test 100/100 current stable and 100/100 Rust 1.88.0; CR/LF-split test 20/20 current and 10/10 Rust 1.88.0; scratch 200-iteration 5 s-deadline loop 200/200 post-fix (file removed after validation). |
| SAM suite under normal parallel load | `cargo test -p i2pr-irc-sam --all-features` green repeatedly (53 lib + 8 inbound + 21 conformance + 17 provider-scope). |
| Full current verification on closure tree | `sh scripts/verify.sh full` passed (boundary check, fmt, clippy `-D warnings`, workspace tests, fuzz smoke). |
| Full Rust 1.88 verification on closure tree | `rustup run 1.88.0 sh scripts/verify.sh full` passed with the same stages, first pass. |
| Registry has no closed plan in the active table | Active table replaced with an explicit empty state; 049 moved to recently-closed. |
| Registry no longer describes M008/M009 as waiting behind Corrective 043 | States M008/M009 closed with no active handoff (already true at registration; confirmed at closure). |
| README reflects implementation through M009 and R001's scope | Opening line and implementation-state section rewritten: core closed through M009, R001 closed for this repository on the i2pd product path with portability delegated to the dedicated SAM library. |
| README still states no finished standalone daemon/listener | Explicit "Not yet a finished standalone product" section (no executable, listener/bootstrap, key provisioning, packaging, or real-client OTR qualification). |
| Roadmaps no longer present M008/M009 as future work | Bouncer roadmap status/table/§11 closed out; long-term roadmap marks M008/M009 closed. |
| Historical closure records unchanged | `git diff` touches no file under `plans/closure/` except the new `049-status.md`. |

## Verification commands executed

- `cargo test -p i2pr-irc-sam --all-features --test sam31_conformance a_fragmented_reply_produces_the_same_result -- --exact` — 100x current stable, 100x `rustup run 1.88.0` (all pass post-fix).
- `cargo test -p i2pr-irc-sam --all-features --test sam31_conformance a_crlf_split_across_reads_produces_the_same_result -- --exact` — 20x current, 10x 1.88.0 (all pass); fails on pre-fix code.
- `cargo test -p i2pr-irc-sam --all-features` — repeated, green.
- `cargo fmt --all -- --check` — green.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — green.
- `cargo test --workspace --all-features --locked` — green (43 suites ok, no failures).
- `sh scripts/verify.sh full` — passed on the closure tree.
- `rustup run 1.88.0 sh scripts/verify.sh full` — passed on the closure tree, first pass.

Docs-only closure-evidence edits after the final code commit did not repeat the 100x
targeted stress, per the plan; the two full verifications above ran on the exact
closure tree.

## Security and recovery review

- No network authority change: loopback-only bridge endpoint, no DNS/TCP/clearnet path touched.
- No secret handling change: session-ID redaction and zeroizing paths untouched; no SASL payload or key material logging added.
- No timeout/deadline relaxation: all production `SamTimeouts` values unchanged.
- No reconnect/replay policy change: ambiguous-disconnect non-replay behavior untouched.
- Bounded-resource posture improved: per-reply feed cost returns to linear and the
  `inflight` bound holds as documented.
- Fixture additions (`split_crlf`, 50 ms pause) are test-only behind the existing
  `testkit` boundary.

## README before/after summary

- Before: "a planned Rust IRC bouncer"; implementation narrative centered on M001–M005
  with Corrective 013 follow-up; R001 "conditionally closed" for missing Java I2P/i2pr
  portability evidence; no mention of M006–M009.
- After: "a Rust IRC bouncer core for I2P, delivered as library crates"; condensed
  M001–M005 lineage; explicit Implemented core/runtime list (durable engine, M006
  downgrade/interop, M007 resilience, R001 provider scope, M008 SQLCipher, M009
  OTRv3-transparent carriage); explicit Not-yet-standalone list; R001 closed for this
  repository with portability delegated; R002 blocked.

## Registry/roadmap reconciliation

- `plans/registry.md`: Bouncer Core row now post-M009 complete with no active plan;
  active table explicitly empty; 049 heads recently-closed; unplanned-milestones and
  handoff sections state no active handoff with standalone work unregistered and R002
  blocked.
- `plans/subsystems/bouncer-core-roadmap.md`: status, §11 completion definition, and
  milestone table closed out for C049 with no open plan.
- `plans/002-long-term-roadmap.md`: M008/M009 marked closed.

## Findings and successor disposition

Finding C049-F1 (transient SAM timeout) is closed as a fixed production framing
defect with a deterministic regression. Findings C049-F2 (stale registry) and C049-F3
(stale README) are closed by the reconciliation above.

No successor implementation plan is registered. Standalone daemon/listener/bootstrap
remains the likely next productization line but requires its own research/planning
gate and is not unblocked by this closure. Router R002 remains independently blocked
on stable public i2pr managed-app contracts; this closure does not change that
blocker. No future plan status required updating beyond emptying the active table:
there was no queued plan waiting on 049.

## Roadmap disposition

Corrective 049 is closed. The Bouncer Core has no open plan; M001–M009 and R001 stand
as historical closures per the registries above.
