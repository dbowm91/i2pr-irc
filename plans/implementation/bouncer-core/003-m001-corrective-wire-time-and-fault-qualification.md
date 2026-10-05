# Bouncer Core Corrective 003 — M001 Wire, Time, and Fault Qualification

Status: active

Repository baseline: `6c7a093152b7062adaf2e3a87c4fe66fdd93c8f3`

Corrects: `plans/implementation/bouncer-core/001-protocol-domain-and-fault-harness-foundation.md`

Prior closure: `plans/closure/bouncer-core/001-status.md` (corrective pass required)

Roadmap: `plans/subsystems/bouncer-core-roadmap.md#M001--protocol-domain-and-deterministic-fault-foundation`

Primary class: invariant + infrastructure

## 1. Objective

Close M001's identified evidence gaps without adding router integration or claiming the M002 capability. Freeze protocol and endpoint behavior against primary specifications, correct the wire codec, provide useful injected timers and an ordered bounded duplex fault harness, and prove the boundaries through regression/property/fuzz and dependency/MSRV evidence.

M002 implementation remains a separate sequential phase. Its existing registration-only runtime slice is unqualified and does not count as completion.

## 2. Authority and research basis

- RFC 2812 §2.3: ordinary IRC messages are octet lines terminated by CRLF and limited to 512 bytes including CRLF.
- IRCv3 Message Tags: tag prefix limit is 8191 bytes including `@` and its following space; the remaining IRC message retains its 512-byte limit. Client-sent and server-added tag data each have a 4094-byte limit. Tag names are opaque/case-sensitive; duplicate keys should resolve to the final occurrence; tag values use UTF-8; empty and missing values are semantically equivalent; escaping has defined handling for invalid and trailing backslashes.
- I2P Naming and Address Book: traditional Base32 names use 52 encoded characters before `.b32.i2p`; encrypted-LeaseSet extended names use 56 or more; full Destinations are Base64 forms.
- Cargo's `rust-version` communicates supported toolchains; the declared 1.88 floor must be exercised, including test/build targets and locked dependencies.

Links are recorded in `architecture/irc-wire.md`, `architecture/network-boundary.md`, and this handoff's closure record. These constraints are parsing/validation only; they do not authorize local or network resolution in core.

## 3. Findings to correct

1. The codec currently applies 8191 bytes to a whole tagged line. Tag prefix and IRC body need separate ceilings.
2. Tag-key parsing rejects some names that the IRCv3 spec requires implementations to preserve as opaque identifiers. Tag-value UTF-8 policy and empty-value semantics are not tested.
3. Encoding a final parameter beginning with `:` can lose data; command forms, invalid output framing, prefix bounds, and all max/max+1 boundaries lack qualification.
4. Endpoint validation does not distinguish standard b32, extended b32, human-readable `.i2p`, and full Destination text robustly. It lacks canonical encoding/length tests.
5. `VirtualClock` advances a number but has no timer scheduling, cancellation, or waiter wakeup.
6. `ScriptedStream` is not duplex. It does not model stalls/reset, and a full buffer returns `WouldBlock` as an error instead of pending bounded backpressure.
7. Fuzz smoke is deterministic random input only; it makes no properties, and the guard positive control does not exercise a complete dependency/source policy.
8. Rust 1.88 and build-script/dependency ownership were not verified and recorded.

## 4. Invariants

- Upstream authority remains only `I2pStreamProvider<I2pEndpoint>`; no generic DNS/TCP/HTTP/proxy path.
- Local downstream acceptance remains a separate capability.
- Every externally controlled buffer/collection stays explicitly bounded before append/allocation.
- IRC stream ordering is preserved; fault injection never reorders or duplicates bytes.
- Timer cancellation and generation replacement cannot deliver stale completions to current state.
- Malformed lines have documented deterministic disposition and cannot silently desynchronize the decoder.
- Unknown well-formed commands, tags, and ISUPPORT tokens remain representable.
- No unsafe Rust is introduced.

## 5. Scope and ordered work packages

### A. Freeze wire and endpoint contracts

- Correctly define ordinary body lines (512 bytes including CRLF) and tag-prefix bytes (up to 8191 including `@` and separator) as independent limits.
- Define directional validation for the 4094-byte client/server tag-data budgets while preserving a direction-independent structural representation.
- Freeze malformed framing, invalid UTF-8 tag-value, duplicate-key, empty/missing value, tag escape, and oversize resynchronization behavior.
- Define accepted `I2pEndpoint` variants and canonical validation: ordinary I2P hostname, traditional b32, extended b32, and Destination text only if supported. Do not resolve names.

Acceptance: docs name exact byte accounting; valid/invalid fixtures cover each endpoint form and both sides of every byte ceiling.

### B. Correct codec and prove its bounds

- Fix tag opaque-key preservation, UTF-8 handling, duplicate semantics, encoder validation, and leading-colon trailing-param round trips.
- Restrict command syntax to ASCII letters or exactly three decimal digits; bound prefix, command, each parameter, tags, and aggregate counts.
- Add golden corpus and every-split-point/concatenated-line tests.
- Add generated valid-message parse/encode/parse properties and arbitrary-byte no-panic/bounded-state assertions.
- Test max and max+1 for untagged line, tag prefix, per-direction tag data, parameter count, tag count, prefix/token size, and endpoint input length.

Acceptance: property/fuzz regressions prove semantic equality or documented error; decoder retained capacity cannot grow with hostile declared length.

### C. Implement cancellable injected timers

- Define a monotonic deadline API and cancellation semantics usable by later runtime code.
- Implement deterministic virtual advancement and wake due waiters without wall-clock sleeps.
- Prove same-deadline ordering policy, cancellation, timer replacement, deadline saturation, and generation fencing.
- If Tokio time is used for runtime tests, keep the core contract independent and test the exact paused-time behavior relied upon.

Acceptance: no M001 scheduling test sleeps on wall time; cancelled timers never fire; repeated virtual scenarios reproduce identically.

### D. Implement ordered bounded duplex faults

- Pair endpoints over bounded directional buffers; reads/writes wake on peer progress.
- Configure short read/write segments, independent stalls, readiness gates, EOF/reset by byte/event boundary, and capacity/backpressure.
- Full capacity yields pending readiness until progress or close, never unbounded growth or a synthetic `WouldBlock` failure.
- Add scripted provider success/failure, requested endpoint records, pending old-generation completion, and compact deterministic reproduction descriptors.
- Ensure write capture and diagnostic storage are bounded and redact payloads by default.

Acceptance: tests prove order, segmentation, stalls/resume, EOF versus reset, close wakeups, bounded pressure, cancellation, and stale-generation fencing.

### E. Harden verification, boundary, and MSRV evidence

- Make positive-control tests invoke the same source/manifest/dependency analysis as production checks.
- Cover build scripts and transitive production dependency ownership; preserve explicit future ownership boundaries rather than globally banning all I/O.
- Run formatting, clippy, tests, quick/full verification, and fuzz smoke on Rust 1.88 as well as the current toolchain.
- Record `cargo tree --workspace` and reviewed production/build/proc-macro dependency set.

Acceptance: forbidden source and dependency fixtures fail; clean workspace passes the guard; both declared Rust version and current version pass the full floor.

### F. Close M001 and refresh M002

- Update architecture docs, README, AGENTS.md, M001 closure, registry, and roadmap with exact evidence and commit IDs.
- M001 closes only if every original acceptance criterion and this corrective matrix is satisfied with no unresolved high-severity issue.
- If M001 closes, refresh M002 baseline/type names and mark it ready. Otherwise register another bounded corrective plan and keep M002 blocked.
- Do not implement M003 or router work from this plan.

## 6. Required test/evidence matrix

| Area | Required evidence |
|---|---|
| Wire ceiling | max/max+1 for tag prefix, body, full line, tokens/counts; spec citations |
| Message semantics | duplicate tags, opaque keys, invalid UTF-8 policy, all tag escapes, unknown commands/numerics, leading-colon parameter |
| Framing | all split points, concatenated lines, NUL/CR/LF, overlong discard then valid line |
| Property/fuzz | no panic, bounded decoder state, parse/encode semantic preservation, reproducible seed/corpus |
| Endpoint | valid standard/extended b32 and selected Destination/name forms; URL/IP/host:port/control/length rejection |
| Time | deadline/advance/cancel/reuse and equal-deadline policy, no wall sleeps |
| Fault streams | partial I/O, ordered bytes, stalls/resume, capacity, EOF/reset, peer close, stale work |
| Static boundary | positive and negative source/dependency fixtures; production tree review |
| Toolchain | Rust 1.88 and current toolchain, exact command outcomes |

Required command floor remains `cargo fmt --all -- --check`, warning-free workspace Clippy, `cargo test --workspace --all-features`, `scripts/verify.sh quick`, `scripts/verify.sh full`, and `scripts/fuzz-smoke.sh`.

## 7. Stop conditions

Stop and register a successor decision if the endpoint specification cannot be implemented without resolution, a safe timer/fault abstraction would require unsafe/platform-specific code, or a dependency forces MSRV above 1.88 without necessity. Do not add SAM, SQLite, listeners, or an operational bouncer to make M001 tests easier.

## 8. Closure evidence

Record exact implementation commits, spec-to-code matrix, endpoint fixtures, byte-ceiling tables, property/fuzz results and seeds, timer cancellation matrix, duplex fault matrix, guard positive-control result, dependency tree, both toolchain results, security/recovery review, unresolved findings, and M002 readiness decision in `plans/closure/bouncer-core/003-status.md`.
