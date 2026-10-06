# Bouncer Core Corrective 006 Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/006-framing-recovery-corrective.md`

Registered by: `plans/research/002-rust-irc-crate-conformance-plan.md` section 10, after its independently authored conformance corpus failed against the owned implementation.

Prior closure: `plans/closure/bouncer-core/005-status.md` (Corrective 005 / C002)

Repository baseline reviewed: `bdcd0ef4ec5d4700abfad2d5d37bdf4a35467093` (Corrective 005 closure)

Implementation commit:

- `e2ba4ac` — Recover framing at the over-long line's own terminator and honor the strict casemapping spelling

## Finding disposition

| Finding | Before | After | Evidence |
|---|---|---|---|
| C003-F1 framing recovery discards one line too many | `LineDecoder::push` applied the length ceiling before inspecting the line terminator, so when the overflowing byte *was* the LF, `dropping` stayed set and the following complete line was silently discarded. A 513-byte line followed by `PING :after` yielded one `TooLong` and no decoded line | an over-long line is discarded through its own terminating LF; the next complete line decodes normally | `WF-004`, `an_over_long_line_is_discarded_through_its_own_terminator_only`, `a_mid_line_overflow_discards_through_the_pending_terminator`, `consecutive_over_long_lines_cost_exactly_one_error_each`, `an_over_long_line_split_across_chunks_still_recovers`, `a_tagged_over_long_line_recovers_at_its_own_terminator` |
| C003-F2 `CASEMAPPING=rfc1459-strict` unrecognized | `apply_isupport_token` matched only `strict-rfc1459`, so a server advertising the Modern IRC Client Protocol spelling fell through to the `rfc1459` default and folded `~` together with `^`, merging two identities the server had declared distinct | both `rfc1459-strict` and `strict-rfc1459` select the strict mapping; an unrecognized value still keeps the documented `rfc1459` default | `SV-003`, `SV-003b`, `both_strict_casemapping_spellings_are_honored` |

C003-F1 was found by this project's own corpus; C003-F2 was found while comparing the owned
state model against `obby-proto` and `vinezombie`, both of which use the Modern IRC
`rfc1459-strict` spelling. The spec disagreement is real and is recorded rather than normalized
away: the older RPL_ISUPPORT draft spells it `strict-rfc1459`, the Modern IRC Client Protocol
spells it `rfc1459-strict`, and the owned runtime had implemented only the first.

## Required-behavior matrix

| # | Required behavior | Evidence | Result |
|---|---|---|---|
| 1 | an over-long line is reported exactly once per line | `WF-012`, `consecutive_over_long_lines_cost_exactly_one_error_each` | pass |
| 2 | discarding an over-long line stops at its own terminating LF | `an_over_long_line_is_discarded_through_its_own_terminator_only` | pass |
| 3 | the first complete line after an over-long line is decoded | `WF-004`, `an_over_long_line_is_discarded_through_its_own_terminator_only` | pass |
| 4 | the buffer ceiling that bounds memory is unchanged | `a_rejected_line_never_leaves_the_decoder_unframed`, `deterministic_arbitrary_bytes_never_panic_or_exceed_decoder_bound` | pass |
| 5 | the `TooManyMessages` overload policy is unchanged | `WF-014`, `line_decoder_bounds_outputs_per_push` | pass |
| 6 | a line ending exactly at the ceiling is still accepted | `WV-010`, `ordinary_max_and_max_plus_one` | pass |
| 7 | both strict spellings produce a strict mapping | `SV-003`, `SV-003b`, `both_strict_casemapping_spellings_are_honored` | pass |
| 8 | an unrecognized `CASEMAPPING` value keeps the documented `rfc1459` default | `both_strict_casemapping_spellings_are_honored` | pass |
| 9 | all existing wire, runtime, boundary, and fuzz-smoke guarantees remain intact | full verification below; no other framing or ISUPPORT path was touched | pass |

The change to `LineDecoder::push` is one line: `self.dropping = *byte != b'\n';`. No ceiling
was changed, no other branch was reordered, and `Message::parse` was not touched. The
casemapping change adds one alternative to one `match` arm.

## Verification commands and results

Executed on the final tree of this corrective (macOS, `rustc 1.89.0 (29483883e 2025-08-04)`, `cargo 1.89.0`):

| Command | Result |
|---|---|
| `./scripts/check-network-boundary.py` | pass (exit 0) |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | pass, no warnings |
| `cargo test --workspace --all-features --locked` | pass: 126 tests (core 6, wire 20 + 7 conformance, runtime 78 + 3 conformance, testkit 12), 0 failed |
| `./scripts/verify.sh full` (guard + fmt + clippy + tests + `scripts/fuzz-smoke.sh`) | pass (exit 0) |
| `rustup run 1.88.0 sh scripts/verify.sh full` | pass (exit 0) on the declared floor `rustc 1.88.0` |

## Conformance corpus

This corrective added the durable external conformance corpus described in
`research/irc-conformance/README.md`: 32 message vectors, 14 framing vectors, 28 state
vectors, and 15 capability vectors, all independently authored from primary specifications
and committed with their spec citations. Two committed runners execute them against the owned
implementation on every `cargo test`:

- `crates/wire/tests/conformance.rs`
- `crates/runtime/tests/conformance.rs`

External comparisons were run in temporary projects outside this workspace, so no candidate
crate entered this repository's `Cargo.lock`. Only summarized outputs are committed under
`research/irc-conformance/results/`.

## Unresolved findings and severity

No C003 finding remains open. Recorded limitations:

| item | severity | disposition |
| --- | --- | --- |
| framing recovery after an over-long line now yields exactly one error and full recovery, but a peer that floods over-long lines still forces the owner to discard a line each time | low | bounded, explicit, and no worse than any framing implementation that enforces a line ceiling; the liveness deadline still bounds the session |
| both strict casemapping spellings are accepted, but a network advertising some future casemapping (for example `rfc7613`) is treated as `rfc1459` | low | matches the specification's documented default; inventing an unknown mapping would be a silent assumption |
| `obby-proto` and `irc-proto` retain every duplicate tag key rather than collapsing to the final value | informational | representation difference only; the specification says only one value per key is transmitted |
| `irc-proto` merges parameters beyond the fifteenth | informational | external-crate observation recorded in `research/irc-conformance/results/irc-proto.md`; no upstream patch proposed, no code copied |

## M003 readiness decision

Corrective 006 is closed, so C003-F1 and C003-F2 no longer block persistence work. The owned
wire layer and state model now pass the full conformance corpus, and the two gates the roadmap
placed on M003 — Corrective 005 closure and the Research 002 disposition — are both satisfied.

The handoff boundary for M003 is extended by this corrective: persistence may not treat a
recovered framing position as if a line were lost, and it must key identities with a folded
identity type that honours both strict casemapping spellings rather than an ad hoc string
comparison.