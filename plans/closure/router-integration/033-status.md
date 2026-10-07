# Plan 033 — R001 Live STREAM ACCEPT Qualification Corrective

Closed 2026-10-07. Outcome: **Corrective 033 closed. Plan 032 Finding 1 is superseded.
R001 remains CONDITIONALLY CLOSED, now blocked only on portability evidence that this
corrective did not produce.**

The corrected harness and the corrected live run produced a full pass against i2pd 2.61.0:
exact bidirectional application bytes crossed an I2P stream between an independently
implemented accepting peer and the production `SamProvider`, on the same provider instance,
with one session creation serving both streams and an explicit release leaving zero scope.

Repository baseline: `4c9ce6f` (Plan 032 closure, R001 conditional closure).

Authority: ADR-0004, ADR-0005, Research 007, the subsystem roadmap, and the SAM v3
specification at <https://www.i2p.net/en/docs/api/samv3/>.

Primary class: qualification corrective.

## 1. Finding disposition

| Finding | Disposition |
|---|---|
| **Q033-F1** — the independent peer never issued `STREAM ACCEPT`, so the harness had no inbound application stream at all | **Confirmed, and the finding it supported is superseded.** The committed harness read and wrote application bytes on the `SESSION CREATE` control socket. SAM has no rule under which that socket carries an inbound stream. The corrected harness arms `STREAM ACCEPT` on a second socket and the inbound stream is delivered there. Plan 032's "0 bytes traversed" measured a topology that was never built. |
| **Q033-F2** — the production owned client was not the connecting implementation under live test | **Closed.** The connecting side is now `crates/sam/examples/live-qualify-probe.rs`, built from the real `i2pr-irc-sam` crate, driving the real `SamProvider` and the real `I2pStreamProvider::connect`. |
| **Q033-F3** — the harness bridge policy was weaker than the production boundary | **Closed.** The harness now accepts numeric loopback only. `localhost`, routable addresses, URLs, wildcards, and malformed forms are refused. |
| **Q033-F4** — stale planning/control metadata after the conditional closure | **Closed.** See section 11. |

### A production defect this corrective found

The plan authorised a bounded production fix *"unless corrected evidence exposes a real
production defect"*. It did.

The owned client's `HELLO` classifier accepted only Java I2P's bare `HELLO OK` and rejected
the specification's canonical reply, which is also what i2pd sends:

```
<- HELLO REPLY RESULT=OK VERSION=3.1
```

Every connect to i2pd failed at the handshake with `ProviderError::Failed`. This was
invisible to the whole deterministic suite because the scripted loopback bridge answers
`HELLO OK`, so the scripted fixture agreed with the narrower rule. Only a real router could
find it.

Fixed in `crates/sam/src/protocol.rs`; `RESULT=NOVERSION` is still a version disagreement and
`RESULT=I2P_ERROR` is still a handshake failure rather than a version one. Regression
coverage added for both reply spellings, both `NOVERSION` spellings, and the `I2P_ERROR`
case. This is the only production code change in the corrective, and it changed no API.

## 2. The corrected topology

The independent peer holds its session on one socket and accepts on another:

~~~
peer control socket
  HELLO VERSION MIN=3.1 MAX=3.1
  SESSION CREATE STYLE=STREAM ID=<peer> DESTINATION=TRANSIENT
    SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4 i2cp.dontPublishLeaseSet=false
    inbound.quantity=2 outbound.quantity=2
  |
  +-- remains open for the session lifetime

peer accept socket          (armed before the connecting side connects)
  HELLO VERSION MIN=3.1 MAX=3.1
  STREAM ACCEPT ID=<peer> SILENT=false
  <- STREAM STATUS RESULT=OK
  |  (wait for the inbound connection)
  <- <connecting-peer-Destination>\n
  |
  +-- raw bidirectional application bytes

connecting side             (production Rust, separate process)
  SamBridgeEndpoint::parse("127.0.0.1:7656")
  SamProvider::with_config(...)
  I2pStreamProvider::connect(NetworkId(1), peer_destination)
  write exact fixture / read exact reply
  second connect on the SAME provider instance
  provider.release(NetworkId(1))
~~~

## 3. Provenance of the two sides

**Independent peer** — `scripts/live-sam-qualify.py`. Hand-written SAM. It shares no code
with the Rust client, so it cannot agree with a bug in it. It never speaks SAM on the
connecting side.

**Production probe** — `crates/sam/examples/live-qualify-probe.rs`. A cargo example built
from the real `i2pr-irc-sam` crate, so `cargo test --workspace` and
`cargo clippy --all-targets` compile it on every run and it cannot silently rot. It holds
one `SamProvider` for the whole run, which is what makes the second stream's session reuse
a measurement rather than an assertion. It emits only counters, booleans, and a hex
encoding of the caller's own fixture; it never prints a Destination, a session ID, or a
router message.

The probe is built by the harness with `cargo build --locked --example live-qualify-probe`
and can be pointed at a prebuilt binary with `--probe`.

## 4. Deterministic ACCEPT and raw-transition matrix

`crates/sam/tests/inbound_accept_qualification.rs` against
`crates/sam/src/fake.rs::FakeSamPeer`, a loopback bridge that serves both directions and is
reachable only behind the `testkit` feature.

| Required coverage | Test | Result |
|---|---|---|
| fake SAM peer `SESSION CREATE` plus separate ACCEPT socket | `an_inbound_accept_on_its_own_socket_receives_the_providers_stream` | PASS |
| ACCEPT status, then peer-Destination line, then raw payload | same | PASS |
| raw bytes coalesced with the peer-Destination newline are preserved | `bytes_coalesced_with_the_destination_line_are_not_lost` | PASS |
| production `SamProvider` connects into that ACCEPT | `an_inbound_accept_on_its_own_socket_receives_the_providers_stream` | PASS |
| bidirectional binary fixture exact | same | PASS |
| same provider/session opens a second stream | `one_session_serves_a_second_stream_without_recreating_it` | PASS |
| release leaves zero scope | `releasing_the_network_scope_leaves_no_live_scope` | PASS |
| malformed ACCEPT status fails closed | `a_refused_accept_status_is_not_treated_as_an_accept` | PASS |
| missing peer-Destination line times out boundedly | `a_missing_destination_line_times_out_boundedly` | PASS |
| oversized peer-Destination prelude refused | `an_oversized_destination_line_is_refused_at_the_ceiling` | PASS |
| bridge hostname refused | `the_bridge_endpoint_is_refused_for_a_host_name` | PASS |
| missing bridge => NOT RUN in the live harness | `scripts/live-sam-qualify.py --endpoint 127.0.0.1:17656` | NOT RUN, exit 2 |

The coalescing test is the sharpest of these. The fixture writes the Destination line and
the first application bytes in a single `write_all`, so they arrive in one segment; a reader
that discards whatever it read past the newline loses the payload while the stream still
looks healthy.

The fake bridge authors the prelude itself and hands it over with the routed connection, so
a test cannot make the transition pass by writing its own Destination line.

## 5. Live i2pd 2.61.0 rerun, row by row

Router: `i2pd version 2.61.0 (0.9.70)`, SAM bound to `127.0.0.1:7656`, HTTP and SOCKS
proxies disabled. Bridge under test: `127.0.0.1:7656`, numeric loopback, opened by the
router. Full output: `evidence/live-sam-i2pd-2.61.0.log`.

| # | Row | Result | Evidence |
|---|---|---|---|
| 1 | Bridge reachable on numeric loopback | PASS | `bridge: 127.0.0.1:7656` |
| 2 | `HELLO`, independent peer | PASS | acknowledged |
| 3 | `SESSION CREATE`, independent peer control socket | PASS | 27.0s, `id_echoed=False`, `destlen=908` |
| 4 | `STREAM ACCEPT` armed on a **separate** socket | PASS | `STREAM STATUS RESULT=OK` |
| 5 | production session creation | PASS | `session_creations=1` |
| 6 | production `STREAM CONNECT` | PASS | 11.7s, `RESULT=OK` |
| 7 | inbound peer-Destination prelude received | PASS | 524 characters |
| 8 | router blank line after the Destination line | PASS | i2pd sent one; exactly one skipped |
| 9 | forward payload exact, 31 bytes | PASS | exact bytes |
| 10 | reverse payload exact, 14 bytes | PASS | exact bytes |
| 11 | second stream over the same provider instance | PASS | 1.8s |
| 12 | second payload exact | PASS | exact bytes |
| 13 | one session creation for two streams | PASS | `session_creations=1 stream_successes=2` |
| 14 | provider release leaves zero scope | PASS | `release_ok=True live_scopes=0` |

`13 passed, 0 failed, 0 not run`.

The fixture is `00 FF FE` + `STREAM STATUS RESULT=OK\r\n` + `\r\n` + `01`: a NUL, an invalid
UTF-8 byte, a bare CRLF, and a complete SAM-looking status line inside the payload. A
framing, text, or raw-transition mistake anywhere shows up as a byte difference.

### Router deviation recorded, not absorbed

i2pd 2.61.0 terminates the peer-Destination line and then writes a **second, empty line**
before any application byte. The specification does not call for that. The peer skips
exactly one blank line, reports it as its own named stage, and the run summary states the
residual ambiguity plainly: on this router a payload whose *first* byte is a newline cannot
be told apart from that blank line, which is why every fixture here begins with NUL. This is
an observation about i2pd 2.61.0, not a claim about Java I2P.

## 6. First-stream and second-stream exact byte evidence

| Stream | Direction | Length | Result |
|---|---|---|---|
| first | production client to accepting peer | 31 bytes | exact |
| first | accepting peer to production client | 14 bytes | exact |
| second | production client to accepting peer | 9 bytes | exact |
| second | accepting peer to production client | 3 bytes | exact |

The probe checks both directions itself and reports `forward_exact` and `reverse_exact`; the
harness re-checks the returned hex against what it sent, so a pass is not a single verdict
from the code under test.

## 7. Provider lifecycle counters

| Counter | Value | Meaning |
|---|---|---|
| `session_creations` | 1 | one SAM session served both streams |
| `session_losses` | 0 | the control socket never ended underneath the scope |
| `stream_attempts` | 2 | |
| `stream_successes` | 2 | |
| `stream_failures` | 0 | |
| `releases` | 1 | explicit, not implied by drop |
| `live_scopes` after release | 0 | nothing retained for a live owner |

## 8. Privacy, boundary, and MSRV verification

| Check | Result |
|---|---|
| No Destination material in `evidence/` or any committed file | PASS — only `destlen` and byte counts are recorded |
| `scripts/check-network-boundary.py` | PASS — including its positive controls |
| No socket authority added outside `crates/sam` | PASS — `src/client.rs` and `src/fake.rs` remain the only two allowlisted files; `crates/sam/examples/` names no socket type at all, so the probe provably cannot bypass the provider |
| Test-only ACCEPT code cannot become a product capability | PASS — `fake.rs` stays behind the `testkit` feature, which no production build enables |
| No production API change | PASS — the production change is one classifier arm in `protocol.rs` |
| No DNS, no clearnet fallback, loopback-only bridge | PASS |
| One transient SAM identity per Network | PASS — unchanged |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS |
| `cargo test --workspace --all-features --locked` | PASS, 0 failures |
| `scripts/verify.sh full` | PASS, including `fuzz-smoke.sh` |
| `rustup run 1.88.0 cargo clippy -p i2pr-irc-sam --all-targets --all-features -- -D warnings` | PASS for every file this corrective touched |

`rustup run 1.88.0 sh scripts/verify.sh full` does **not** pass, and did not pass before this
corrective. It fails on `crates/core/src/lib.rs` with `clippy::uninlined_format_args`, a
lint that fires on the older toolchain. Verified against the untouched baseline by stashing
this corrective's changes and reproducing the same failure. It is recorded here as a
pre-existing gap rather than silently worked around, and fixing `crates/core` is outside
this plan's scope.

## 9. Claim separation, restated against corrected evidence

| Claim | Meaning | Status after this corrective |
|---|---|---|
| A. SAM server compatibility | the owned client speaks one real router's SAM 3.1 bridge correctly | **Proved for i2pd 2.61.0**, with the live handshake now performed by the production client rather than a hand-written equivalent |
| B. I2P stream product path | application bytes cross that router over an I2P stream | **Proved for i2pd 2.61.0** — exact bytes both ways, through a real inbound `STREAM ACCEPT` |
| C. Cross-router network interoperability | traffic traverses between distinct router implementations | **Not attempted** — unchanged |

Claim B was previously "not established" and is now established for i2pd only. It was never
established for any router by the old harness, because the old harness built no inbound
stream.

## 10. Supersession of Plan 032 Finding 1

Plan 032's high-severity stream-delivery finding — that i2pd 2.61.0 established a broken
data stream, established by "0 of 31 bytes" and "0 of 13 bytes" traversing — was an artefact
of the harness. The peer never issued `STREAM ACCEPT`; it read and wrote application bytes
on the `SESSION CREATE` control socket, which SAM does not use for inbound data. The count
was therefore measuring a socket arrangement the specification does not define, not a
router defect.

With the corrected topology the same 31-byte and 13-byte payloads traverse i2pd 2.61.0
exactly. Plan 032's historical record is left intact; this section is the authoritative
interpretation of it.

## 11. Planning reconciliation

- `plans/registry.md`: Corrective 033 moved from active to closed; closed Plans 031 and 032
  removed from the active and blocked implementation sections.
- `plans/subsystems/i2p-router-integration-roadmap.md`: current state and the R001 row
  updated with the corrected evidence and the supersession.
- `plans/closure/router-integration/032-status.md`: a marked erratum added, leaving every
  substantive statement untouched.

**Closure-date erratum.** Plan 032's closure heading reads "Closed 2026-10-08" although the
closure commit landed on 2026-10-07. The heading is left exactly as written and corrected in
place here and in the Plan 032 file as an explicitly marked administrative erratum. No
substantive Plan 032 finding is altered.

## 12. Final R001 status

**R001 remains CONDITIONALLY CLOSED.** What changed is *why* it is conditional.

Before this corrective, R001's product-path evidence was invalid, so its conditional closure
rested on an unverified premise. That premise is now verified for i2pd 2.61.0. R001 is now
conditional solely on portability evidence this corrective did not and could not produce:

- **Java I2P** — not installed. NOT RUN.
- **i2pr** — router binary not installed. NOT RUN.
- **Cross-router interoperability** — not attempted.

No stop condition in Plan 033 section 17 was reached. Nothing required weakening loopback
authority, SAM 3.2/3.3 semantics, inbound ACCEPT in the product, a change to per-Network
identity policy, or persistent Destination storage.

## 13. R002 readiness

**R002 stays blocked**, and its blocker wording is updated only to remove the now-obsolete
"R001's product-path evidence is invalid" reason.

R002's prerequisites are its own: a managed-app interface, an i2pr integration consuming
public managed-app capabilities, and a reviewable data-transfer surface. None of those were
in Corrective 033's scope, and none of them is unblocked by it. The only R001 fact R002
depended on — that the SAM adapter could carry application bytes at all — is now established
for one router rather than assumed from an invalid run.

A successor plan for R002 may be authored on the corrected basis. It must not treat i2pd
2.61.0 as sufficient for a managed-app data-transfer claim.

## 14. What a reader should take from this

The defect this corrective corrected was in the measuring instrument, not in the thing
measured. Two independent signals said so at the time and neither was acted on: a router
answering `CANT_REACH_PEER MESSAGE="LeaseSet not found"` that was traced to
`dontPublishLeaseSet=true`, and a client "passing" against a scripted bridge that answers
`HELLO OK` while the specification's canonical `HELLO REPLY RESULT=OK` went unhandled.

A harness that only ever exercises the shape it agrees with cannot fail. The corrective's
durable value is the rule it now enforces: the peer is implemented independently, the code
under test is the code that ships, and every live stage names which side it measured.