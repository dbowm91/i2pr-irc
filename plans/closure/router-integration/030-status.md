# Plan 030 — R001-B Owned SAM 3.1 Wire/Client Foundation

Closed 2026-10-07. Outcome: **R001-B closed. The repository has a loopback-only SAM 3.1
STREAM client with strict framing, typed replies, opaque identity, bounded deadlines, and
an exact raw transition. One pre-existing test flake was found and fixed. One deadline
mismatch is recorded for Plan 031. No unresolved high-severity finding remains.**

Repository baseline: `351340c` (Plan 029).

Authority: ADR-0004, Research 007.

Primary class: infrastructure + invariant.

## 1. What landed

`crates/sam` (package `i2pr-irc-sam`), six modules and a conformance suite:

| Module | Responsibility |
|---|---|
| `endpoint` | `SamBridgeEndpoint`, the type that makes non-loopback unrepresentable |
| `line` | bounded control-line reader and quote-aware tokenizer |
| `protocol` | typed replies, strict sequencing, the three frozen request lines |
| `session_id` | `SamSessionId` and the injectable `RandomSource` |
| `client` | the three state machines, deadlines, the raw transition |
| `fake` | the test-only loopback bridge |

### Dependency boundary

`i2pr-irc-core`, `tokio` (`net` added), `thiserror`, `zeroize`, `getrandom`. No i2pr private
crate, no external SAM library.

## 2. Network authority

A type, not a check. `SamBridgeEndpoint` is constructible only through its own `parse`, which
accepts a numeric loopback literal:

| Case | Outcome |
|---|---|
| `127.0.0.1:7656`, `127.0.0.53:65535`, `[::1]:7656`, `[0:0:0:0:0:0:0:1]:7656` | accepted |
| `192.168.1.1:7656`, `0.0.0.0:7656`, `[::]:7656`, `[2001:db8::1]:7656` | `NotLoopback` |
| `localhost:7656`, `router.example:7656` | `NotNumeric` |
| `http://127.0.0.1:7656/`, `/var/run/sam.sock` | `NotNumeric` |
| port 0, port 65536, no port, no host, empty | typed refusal |

`localhost` is refused on purpose rather than accepted as a special case. It resolves to
loopback everywhere sane, so accepting it *looks* harmless — but accepting a name means the
resolution decides where to connect, and there is no resolver call in this crate for it to be
wrong about.

Two corrections made while implementing the predicate:

- IPv6 literals need hex digits, so the "is this numeric" check accepts `a-f`. Rejecting them
  would have reported a routable IPv6 address as `NotNumeric`, sending an operator hunting a
  typo that is not there.
- All of `127.0.0.0/8` is loopback, not only `127.0.0.1`.

Evidence: `endpoint::tests`, 8 tests including a max/max+1 port sweep.

## 3. Static boundary

`scripts/check-network-boundary.py` now scans `crates/sam` and holds two of its files —
`src/client.rs` and `src/fake.rs` — exempt from the blanket socket and DCC predicates,
compensated by a narrower predicate in front of them:

- a socket token in **any other** file is a failure;
- `UdpSocket`, `UnixStream`, `UnixListener`, `TcpSocket`, and `ToSocketAddrs` are failures
  **even in the two permitted files**.

The comparison is on the path relative to `crates/sam/src`, so a fixture tree laid out
anywhere is held to the same rule as the real crate.

Eight positive controls, each of which fails if the confinement is narrowed: socket authority
outside the allowlist (TCP and listener), UDP and unix in an allowlisted file, a raw socket,
UDP in the fake, a resolver in an allowlisted file, and a resolver in a non-allowlisted file.

The test-only fake lives inside the crate rather than in `crates/testkit` deliberately: that
way the boundary scan sees it and holds it to the same rule. It binds `127.0.0.1:0` and is
compiled out of production builds behind the `testkit` feature.

## 4. Requirement-to-evidence matrix

Plan 030 section 4 (protocol profile) and section 5 (framing):

| Requirement | Evidence |
|---|---|
| Only `HELLO`, `SESSION CREATE`, `STREAM CONNECT` | `protocol::tests::the_hello_line_is_the_frozen_profile`, `the_session_create_line_is_the_frozen_profile`; `every_request_line_fits_the_line_ceiling`. There is no `send_command` and no option-passthrough parameter, so the profile cannot be extended from a caller. |
| No `ACCEPT`/`FORWARD`/`DATAGRAM`/`RAW`/auth/remote-bridge/import-export/admin | No such symbol exists; the boundary scan holds the crate to loopback TCP only |
| `MAX_SAM_LINE_BYTES = 4096` | `line::tests::the_exact_maximum_line_is_accepted`: `MAX-1` accepted, `MAX` overflows |
| `MAX_SAM_TOKENS = 64` | `options_are_bounded_at_every_ceiling`: `MAX+2` refused, exactly `MAX` accepted |
| `MAX_SAM_KEY_BYTES = 64` | same, `MAX+1` refused and `MAX` accepted |
| `MAX_SAM_VALUE_BYTES = 3072` | same, `MAX+1` refused and `MAX` accepted |
| LF and CRLF both accepted | `both_terminators_are_accepted` |
| NUL / embedded CR rejected | `nul_and_embedded_control_are_refused`; a bare CR is rejected rather than treated as a line break, since accepting it would let a peer inject a line boundary |
| Partial reads supported | `fragmented_input_is_read_identically_to_whole_input` (byte-at-a-time feed equals whole-buffer feed) |
| Overflow drops until newline without swallowing the next line | `an_over_long_line_is_dropped_and_the_next_line_survives`, `an_over_long_reply_is_discarded_and_the_reader_recovers` |
| Duplicate-option behaviour explicit | `duplicate_options_are_preserved_and_countable`; `SamReply::count` exists so a caller *can* be explicit per reply type |
| Quoted `MESSAGE` classified and skipped safely | `a_quoted_message_is_one_value_not_several_options`, `an_unterminated_quote_is_refused` |
| Router `MESSAGE` never verbatim in diagnostics | `SamError` is 8 bytes and holds no text (`sam_error_carries_no_router_text`); `a_malformed_reply_is_refused_without_echoing_router_text` asserts neither `Debug` nor `Display` contains the router's text |
| SESSION STATUS buffer zeroized | Every parsed string is a `Zeroizing`; asserted by `parsed_values_are_wrapped_so_they_are_zeroized`, which takes a `&Zeroizing<String>` and so fails to compile if the wrapper is dropped |
| Typed replies, not a string map | `protocol::tests`: all eleven Plan 030 section 6 variants modelled |
| Strict sequencing | `a_reply_in_the_wrong_phase_is_a_protocol_failure` — six wrong-phase cases; `a_session_id_message_is_not_a_session_status` |
| `SamState::is_raw` only after `RESULT=OK` | `only_a_successful_stream_status_produces_a_raw_socket` |
| IDs ≥128 bits of OS randomness, ASCII-safe, bounded | `a_generated_id_carries_at_least_128_bits`, `a_generated_id_is_the_expected_hex_of_the_source_bytes`, `nothing_identifying_can_reach_a_generated_id` |
| No NetworkId/nick/endpoint/pid/timestamp/build string in an ID | `the_session_id_carries_no_identifying_material` asserts the absence of six specific strings on the wire |
| Randomness injectable | `RandomSource` trait; `FixedRandom` and `BrokenRandom` in the conformance suite |
| Random failure explicit, no fallback | `a_failing_random_source_prevents_the_exchange` → `SamError::RandomUnavailable` |
| Frozen SESSION CREATE fields | `the_session_create_line_is_the_frozen_profile` |
| Private Destination not retained | `a_successful_session_reply_cannot_carry_the_private_destination`: `SessionReply` is 1 byte and has no field |
| Successful session retains only ID, socket, non-secret metadata | The session type is a control socket plus an opaque ID; nothing else is stored |
| Line-to-raw transition | `after_result_ok_nothing_is_parsed_as_sam` |
| Never log a complete Destination | No `Display`/`Debug` on `I2pEndpoint` prints it (Plan 029); `SamRawStream` derives no `Debug` |
| Typed/injectable timeout profile | `SamTimeouts` struct; `SamTimeouts::immediate` |
| Production values 10/10/120/10/10/90 | `the_production_profile_matches_the_planned_values` |
| Stream budget above the router's ~1 minute | `the_stream_deadline_exceeds_the_routers_own_attempt_window` |
| Dropping a future drops its socket, no detached task | No `tokio::spawn` in `client.rs`; every exchange is a plain `async fn` |
| Error taxonomy, no variant stores router text | `error::tests`, 6 tests |

### Raw-transition evidence

`after_result_ok_nothing_is_parsed_as_sam` sends, after `RESULT=OK`, a payload containing a
NUL byte, `0xff 0xfe` (invalid UTF-8), a string that is byte-for-byte
`STREAM STATUS RESULT=OK\r\n`, and a trailing `0x01`. All 32 bytes cross unchanged.

Three separate mechanisms make this hold:

1. `SamState::is_raw` is the only thing that authorises raw reading, and only
   `RESULT=OK` sets it.
2. Nothing is parsed after that point — the socket is handed over directly.
3. Bytes the reader had already consumed *past* the `RESULT=OK` line are transferred to the
   stream rather than dropped. A router may write application bytes in the same TCP segment
   as the acknowledgement, and losing the first bytes of an IRC conversation would be a
   silent, intermittent failure that no protocol test would reliably catch.

Mechanism 3 was found by that test failing. It is the reason `LineReader` now reports the byte
offset at which each line ends.

### Deadline and cancellation matrix

| Phase | Deadline | Exercised by |
|---|---|---|
| bridge connect | 10 s | `an_absent_bridge_is_reported_as_unavailable_and_is_retryable`, `the_bridge_connect_deadline_is_bounded` |
| `HELLO` | 10 s | `every_exchange_deadline_fires_and_names_its_phase` |
| `SESSION CREATE` | 120 s | same |
| `STREAM CONNECT` | 90 s | same |

The deadline test drives the client *to* each phase before removing that phase's deadline.
Timing out the first exchange only proves the first exchange has one; each case starts with
every other phase generous.

**The bridge-connect deadline cannot be exercised against a live loopback listener.** A
loopback connect completes in microseconds however small the deadline, so a test that tried
would be asserting that a working connect fails. That is recorded rather than papered over: the
deadline is asserted as a bounded value, and the failure an operator actually hits — the
router is not running — is tested instead.

## 5. Findings

**Finding 1 — high, fixed here. The raw transition dropped prefetched bytes.** A router that
writes application bytes in the same TCP segment as `RESULT=OK` lost them. Caught by the
conformance test before any router was involved. Fixed by transferring the post-line bytes to
the stream.

**Finding 2 — medium, fixed here. A pre-existing test flake in `m005g_member_state.rs`.**
`a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed` failed
intermittently: **2 failures in 20 baseline runs** at `351340c`. The test's premise — that the
JOIN and MODE were processed *before* the client attached — was never established; it relied
on the attach happening second. A second barrier now establishes the premise explicitly.
**0 failures in 20 runs** afterwards, measured rather than asserted. Not caused by R001-B;
found by running the full suite.

**Finding 3 — medium, recorded for Plan 031. The cold-session path exceeds the runtime's
connect budget.** SAM-side: 10 + 10 + 120 = 140 s. Runtime `CONNECT_TIMEOUT`: 120 s. A cold
first connect would be cut off while the router was still building tunnels, failing a working
Network. Plan 030 section 10 anticipated this and assigned the reconciliation to Plan 031.

Recorded as a test (`the_cold_session_path_exceeds_the_runtime_connect_budget`) that asserts
*both* numbers. A later plan that fixes the budget has to update the test deliberately;
silently changing either side is how the mismatch would have gone unnoticed.

## 6. Dependency and MSRV review

`getrandom` 0.3, read from vendored source before being added
(`~/.cargo/registry/src/*/getrandom-0.3.4/`):

| | |
|---|---|
| License | MIT OR Apache-2.0 — matches every other dependency |
| `rust-version` | 1.63, below the 1.88 floor |
| Transitive dependencies | none on Linux, macOS, or Windows (every backend is `cfg`-gated) |
| Why | the one OS-random need: a session ID must carry ≥128 bits of OS randomness and must never be derived from anything identifying |

`tokio`'s `net` feature adds one feature to an existing dependency, not a new crate. Enabling
a feature makes the symbols *available*; the boundary scan is what makes them *unreachable*.

Both recorded in `architecture/dependency-review.md`.

## 7. Commands actually executed

| Command | Result |
|---|---|
| `./scripts/check-network-boundary.py` | exit 0, including 8 new SAM positive controls |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clean |
| `cargo test --workspace --all-features --locked` | 0 failures |
| `./scripts/fuzz-smoke.sh` (`verify.sh full`) | exit 0 |
| `rustup run 1.88.0 cargo test --workspace --all-features --locked` | 0 failures |
| `cargo test -p i2pr-irc-runtime --test m005g_member_state --release` ×20, before and after | 2/20 → 0/20 |

Test counts: `crates/sam` unit 52, conformance 20.

## 8. Security and privacy review

- **Loopback only, structurally.** Non-loopback is unrepresentable, and the scan fails on a
  resolver in every file including the client.
- **No cross-Network correlation.** A session ID is 128 bits of OS randomness, not the
  `NetworkId`. Two Networks cannot be correlated at the bridge by their session identifiers,
  and no display name, nick, endpoint, process ID, timestamp, or build string appears on the
  wire.
- **No private material retained.** The router's transient Destination is parsed into a
  zeroizing buffer and dropped; `SessionReply` has no field for it.
- **No endpoint disclosure.** `I2pEndpoint` Debug stays redacted; `SamRawStream` derives no
  `Debug`, so a socket handle cannot reach a diagnostic by accident.
- **No router text in operator-facing strings.** `SamError` is 8 bytes and holds only closed
  enums; a free-form `MESSAGE` cannot travel through it.
- **Bounded by construction.** Every phase has a deadline, every collection has a ceiling,
  overflow is a state rather than a growing buffer, and no exchange spawns a task.

## 9. Roadmap disposition

R001-B is closed. Plan 031's hard dependency is satisfied and its status moves from `blocked`
to `ready`.

Plan 032's dependency is Plan 031 and remains blocked.

Plan 030's own stop conditions were not triggered: SAM 3.1 interoperability was not assumed
from hidden 3.2 behaviour (qualification against live routers is Plan 032's work and its
consequences are that plan's), no router required non-loopback baseline access, session
establishment did not require retaining private Destination material, no third-party SAM
production dependency was added, and TCP authority is path-confined with a proven guard.

Plan 030 section 16 acceptance is met: the crate creates a fake SAM 3.1 STREAM session and
opens exact raw streams under all framing, error, and deadline tests, while exposing no
generic SAM command surface and no non-loopback authority.
