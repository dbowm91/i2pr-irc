# Bouncer Core Corrective 006 — Framing Recovery and Casemapping Token Conformance

Status: closed

Closure record: `plans/closure/bouncer-core/006-status.md`

Repository baseline: `bdcd0ef4ec5d4700abfad2d5d37bdf4a35467093` (Corrective 005 closure)

Registered by: `plans/research/002-rust-irc-crate-conformance-plan.md` section 10, after the independently authored conformance vector `WF-004` failed against the owned decoder.

Corrects current post-Corrective-005 wire behavior before M003.

Prior closures:

- `plans/closure/bouncer-core/002-status.md`
- `plans/closure/bouncer-core/004-status.md`
- `plans/closure/bouncer-core/005-status.md`

Source roadmap:

- `plans/subsystems/bouncer-core-roadmap.md#M003--durable-multi-network-multi-client-and-history-model`

Long-term requirements:

- `plans/000-long-term-specification.md` sections 4.4, 6, 7, and 14
- `plans/001-terminology-and-domain-model.md` definitions for ObservedState and Upstream generation
- `plans/002-long-term-roadmap.md` Phase 2
- `plans/003-planning-process.md` section 9

Applicable ADR:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`

Primary class: invariant + protocol correctness corrective

## 1. Objective

Close two conformance defects that the Research 002 conformance corpus found in
the owned wire and state layers:

1. restore total framing recovery in the owned incremental decoder so an over-long
   line is reported and discarded through its own terminating LF and the following
   line is decoded normally;
2. honor the Modern IRC Client Protocol `CASEMAPPING=rfc1459-strict` spelling in
   addition to the older `strict-rfc1459` spelling, so a network that declares
   `~` and `^` to be distinct does not get them folded into one identity.

This corrective adds no capability, no dependency, and no configuration.

## 2. Why this corrective is required

Research 002 authored an independently derived conformance corpus. Vector
`WF-004` states the specification-derived requirement: an over-long line must be
rejected and framing must recover at the next complete line.

The owned `LineDecoder::push` in `crates/wire/src/lib.rs` violates it.

The length ceiling is applied before the line terminator is inspected. For a line
whose terminating LF is the byte that pushes the buffer past the ceiling, the
decoder clears the buffer, reports `TooLong`, and sets `dropping = true`. Because
that LF has already been consumed as part of the over-long line, the decoder then
discards everything up to the *next* LF, which is the end of the following line.

Observed result on the research baseline: a 513-byte line followed by
`PING :after` yields one `TooLong` and **no** decoded line. The following
complete line is silently lost.

### C003-F1 — framing recovery discards one line too many

Consequences for a bouncer:

- a single over-long line silently removes the next real server event, including
  a `PING`, a self `JOIN`, a `PART`, or a liveness probe response;
- a hostile or malfunctioning peer that interleaves over-long lines can
  continuously suppress server state the owner needs to stay truthful;
- observed state can diverge from the network without any bounded or explicit
  signal, which is the same class of defect Corrective 005 removed from channel
  membership;
- M003 persistence would then persist state that the network never reported.

Severity: medium. Memory safety is unaffected — the decoder's buffer ceiling and
`dropping` recovery are correct — and the effect is bounded to losing server
events, not unbounded growth. It is nevertheless a correctness defect in observed
state fidelity, so it must be corrected before persistence work begins.

Not a defect, and recorded here so it is not "fixed" later by mistake: the
decoder deliberately discards the remainder of an input chunk after
`MAX_DECODED_MESSAGES_PER_PUSH` and reports `TooManyMessages`. That is a
separate, correct overload policy.

### C003-F2 — `CASEMAPPING=rfc1459-strict` is not recognized

`NetworkState::apply_isupport_token` recognized only the older RPL_ISUPPORT draft
spelling `strict-rfc1459`. The Modern IRC Client Protocol — which IRCv3 adopted as
its base specification — spells the value `rfc1459-strict`.

A server advertising `CASEMAPPING=rfc1459-strict` therefore fell through to the
`rfc1459` default, which folds `~` together with `^`.

Consequences for a bouncer:

- two distinct identities on such a network are treated as one;
- a self `JOIN`, `PART`, or `NICK` event can be attributed to the wrong identity;
- M003 would persist identity keys derived from a mapping the server explicitly
  did not advertise.

Severity: medium, and low likelihood today: `rfc1459-strict` networks are rare, but
the value is in the current base specification and the failure mode is silent
identity merging rather than a visible error.

Both spellings are accepted. An unrecognized value keeps the documented `rfc1459`
default; inventing strictness for an unknown spelling would be the same class of
silent-assumption defect this corrective exists to remove.

## 3. Corrective invariants

The corrective MUST establish and prove:

1. An over-long line is reported exactly once per line.
2. Discarding an over-long line stops at its own terminating LF.
3. The first complete line after an over-long line is decoded normally.
4. The buffer ceiling that bounds memory is unchanged.
5. The `TooManyMessages` overload policy is unchanged.
6. A line that ends exactly at the ceiling is still accepted.
7. Both strict casemapping spellings produce a strict mapping, and no spelling
   silently produces plain `rfc1459` when the server asked for strictness.
8. An unrecognized `CASEMAPPING` value keeps the documented `rfc1459` default.
9. All existing wire, runtime, boundary, and fuzz-smoke guarantees remain intact.

## 4. Scope

### In scope

- the ordering of the length-ceiling check and the line-terminator check in
  `LineDecoder::push`;
- the accepted `CASEMAPPING` values in `NetworkState::apply_isupport_token`;
- regression coverage for over-long line recovery, including a line whose
  terminator is the overflowing byte, a mid-line overflow, consecutive over-long
  lines, and a partial over-long line split across chunks;
- conformance corpus coverage through `WF-004`;
- test/docs/closure/registry/roadmap reconciliation.

### Explicitly out of scope

- changing any documented ceiling (`MAX_LINE_BYTES`, `MAX_TAG_PREFIX_BYTES`,
  `MAX_TAGGED_LINE_BYTES`, `MAX_DECODED_MESSAGES_PER_PUSH`);
- changing `Message::parse` validation or encoding;
- changing any casemapping semantics beyond accepting the second token spelling;
- adding IRCv3 capabilities, persistence, multi-client routing, or router
  integration;
- refactoring the decoder into a different streaming design.

## 5. Required behavior

| input | expected |
|---|---|
| over-long line, then `PING :after` | one `TooLong`, then `PING :after` decoded |
| over-long line whose terminator overflows the ceiling | one `TooLong`, no loss of the next line |
| two consecutive over-long lines | two `TooLong`, next line decoded |
| over-long line split across chunks | one `TooLong`, framing recovered |
| line of exactly the ceiling | decoded |
| 257 complete lines in one push | 256 decoded, one `TooManyMessages`, remainder discarded |
| `CASEMAPPING=rfc1459-strict` | `bot~` and `bot^` differ; `bot[` and `bot{` match |
| `CASEMAPPING=strict-rfc1459` | identical to the spelling above |
| `CASEMAPPING=rfc7613` or any unknown value | documented `rfc1459` default |

## 6. Required regression tests

- `WF-004` passes against the owned decoder.
- `SV-003` and `SV-003b` pass against the owned state model.
- new wire unit tests assert the byte-exact recovery sequence.
- a new state unit test asserts both strict spellings.
- `crates/wire/tests/conformance.rs` framing suite passes unchanged otherwise.
- the existing fuzz-smoke corpus and all workspace tests still pass.

## 7. Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
./scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
```

## 8. Risks and mitigation

| risk | mitigation |
|---|---|
| the fix reintroduces unbounded buffering | the ceiling check stays on the byte path; a test asserts a 4096-byte line still leaves the buffer empty |
| the fix changes overload semantics | `TooManyMessages` branch is untouched and covered by existing tests |
| the fix is a symptom of a deeper streaming redesign | the change is a two-line ordering change; no redesign is proposed or needed |

## 9. Closure requirements

A closure record at `plans/closure/bouncer-core/006-status.md` recording the
C003-F1 and C003-F2 dispositions, the required-behavior matrix with test names,
the verification commands and results on the Rust 1.88 floor, and the explicit
statement that no other framing or ISUPPORT path changed.