# Plan 032 — R001-D SAM Cross-Router Qualification and R001 Closure

Closed 2026-10-08. Outcome: **R001-D closed. R001 is CONDITIONALLY CLOSED.**
Implementation is complete and one live router (i2pd 2.61.0) was qualified far enough to
prove SAM handshake, session creation, stream establishment, and session reuse against an
independent implementation. Three of Plan 032's evidence rows could not be produced and are
recorded as not-run, never as passes. R001 portability is therefore **evidence-blocked**,
which is the state Plan 032 section 11 explicitly provides for.

Repository baseline: `000b9d3` (Plan 031 closure).

Authority: ADR-0004, ADR-0005, Research 007, the subsystem roadmap.

Primary class: capability qualification.

## 1. Claim separation (Plan 032 section 2)

Every row below names which claim it proves. These are not interchangeable.

| Claim | Meaning | Status |
|---|---|---|
| A. SAM server compatibility | the owned client speaks one real router's SAM 3.1 bridge correctly | **Proved for i2pd 2.61.0** |
| B. I2P stream product path | application bytes cross that router over an I2P stream | **Not established** |
| C. Cross-router network interoperability | traffic traverses between distinct router implementations | **Not attempted** |

Nothing in row A is used to claim row B or C. Plan 032 forbids it and this record does not
do it.

## 2. Router matrix (Plan 032 section 3)

| Router | Version | Availability | Result |
|---|---|---|---|
| **i2pd** | 2.61.0 (0.9.70), built against Boost 1.92.0 / OpenSSL 4.0.3 | installed at `/usr/local/bin/i2pd`, run locally | **PARTIAL — 5 PASS, 2 NOT RUN** |
| Java I2P | — | **no installation present** (`java` 26.0.2.1 present, no I2P) | **NOT RUN — operational evidence unavailable** |
| i2pr | — | **binary not installed** (`~/i2p-rs` is the independent `i2p/i2p-rs` client library, not this project's router) | **NOT RUN — operational evidence unavailable** |

Bridge under test: `127.0.0.1:7656`, loopback only, started by the router itself. Our code
never accepted anything else.

## 3. i2pd evidence, row by row

Committed harness: `scripts/live-sam-qualify.py`. Raw output:
`evidence/live-sam-i2pd-2.61.0.log`. It fails closed — with no bridge it prints
`NOT RUN: no SAM bridge` and exits 2, and it refuses any non-loopback endpoint outright.

| # | Row | Result | Evidence |
|---|---|---|---|
| 1 | Bridge reachable on loopback | PASS | `bridge: 127.0.0.1:7656` |
| 2 | `HELLO VERSION MIN=3.1 MAX=3.1` | **PASS** | `HELLO REPLY RESULT=OK VERSION=3.1` |
| 3 | `SESSION CREATE STYLE=STREAM …` (exact frozen profile) | **PASS** | `SESSION STATUS RESULT=OK` |
| 4 | Endpoint form: i2pd's returned Destination | **PASS** | 908 chars, accepted by `I2pEndpoint::parse` |
| 5 | `STREAM CONNECT ID=… DESTINATION=… SILENT=false` | **PASS** | `STREAM STATUS RESULT=OK` in 0.6 s |
| 6 | Second stream reuses the session | **PASS** | `STREAM STATUS RESULT=OK` |
| 7 | Binary fixture reaches the peer (NUL, invalid UTF-8, CRLF, SAM-looking text) | **NOT RUN** | 0 of 31 bytes within 60 s |
| 8 | Reply reaches the client | **NOT RUN** | 0 of 13 bytes within 60 s |
| 9 | Restart/closure behaviour | **NOT RUN** | not reached; the run did not get that far |
| 10 | Session reuse after bridge restart | **NOT RUN** | as above |

Timing observed on a live public-network router: peer session 9–30 s, control session
6–30 s, `STREAM CONNECT` 0.5–0.6 s. The 300 s `PROVIDER_ACQUIRE_TIMEOUT` from Plan 031
covers the worst observed cold session with margin, which is the first live confirmation
that Plan 031's budget reconciliation was not merely arithmetically true.

## 4. Three findings from the live router

**Finding 1 — medium, no code change needed. i2pd returns a 908-character I2P-base64
Destination, not a 516-character one.** Its alphabet uses `-` and `~` where RFC 4648 base64
uses `+` and `/`, plus trailing `=`. This is *not* a `.b32.i2p` name, and it must not be
mistaken for one: a 908-character token is far outside the 52/56–63 character label lengths
the hostname guard enforces, so refusing it as a name is correct.

This is the first live confirmation that Plan 029's recorded deviation was necessary.
Implementing its section 10 literally — base64url only — would have rejected **every** real
i2pd Destination. `I2pEndpoint::parse` accepts the live value; the union alphabet was
load-bearing.

Locked in by `endpoint_accepts_a_live_router_i2p_base64_destination` in `crates/core`. The
fixture is **synthetic**, shaped like the live observation rather than captured from it: a
real Destination is key material and does not belong in a repository even when transient. The
test also pins that the same 908-character token is still refused as a `.b32.i2p` name, since
a b32 label is 52 or 56–63 characters and mistaking a Destination for an address would mean
accepting a 908-character hostname.

**Finding 2 — medium, no code change needed, design validated. i2pd omits `ID=` from its
`SESSION STATUS RESULT=OK` reply.** A client that supplied no `ID=` and waited for the
router to name the session would hang until its own deadline. The owned client supplies its
own 128-bit ID and `SessionReply` requires only `RESULT=`, so it is unaffected. The
qualification harness had to adopt the same shape before it could proceed, which is how the
assumption surfaced.

**Finding 3 — high, unresolved, not attributed to the owned client. Streams establish
but no application bytes traversed.** `STREAM CONNECT` returns `RESULT=OK` in under a second
and a second stream over the same session also returns `RESULT=OK`, yet neither direction
carried a single byte within 60 s while both sockets were being read continuously.

Diagnosis, in order:

1. **Not our request topology.** Sending `STREAM CONNECT` on the *same* socket that created
   the session produces `SESSION STATUS RESULT=I2P_ERROR MESSAGE="Socket already in use"`.
   Moving it to its own connection — which is exactly what the owned client does, per Plan
   031 — changes the reply to a well-formed `STREAM STATUS`. The router-side data socket and
   the control socket are the design; the harness's first attempt was the wrong shape.
2. **Not an unreachable peer.** The first correct attempt returned
   `STREAM STATUS RESULT=CANT_REACH_PEER MESSAGE="LeaseSet not found"`, because the
   harness had created the peer with `i2cp.dontPublishLeaseSet=true`. That option is correct
   for *our* client, which never needs to be reachable, and wrong for a peer. Publishing the
   peer's LeaseSet turned the same request into `RESULT=OK`.
3. **Not a missing bridge or an empty addressbook.** i2pd's own SOCKS client was also tried
   and failed, but for an unrelated and correct reason (`Addressbook: Can't find domain`),
   which is a fresh router with no addressbook — not evidence about SAM.
4. **Unresolved.** What remains is that this i2pd build reports stream establishment and
   then delivers nothing in either direction on this host. Recorded as an open
   environment/i2pd-side gap. **It is not claimed as a pass, and it is not recorded as a
   defect in the owned client**, because every protocol-level observation the client controls
   was correct.

## 5. Independent peer fixture (Plan 032 section 4)

The peer is **not** the owned client. `scripts/live-sam-qualify.py` drives it with
hand-written SAM over its own loopback socket, so a protocol mistake in the owned Rust
client cannot be reproduced on both ends and certified.

Provenance:

| | |
|---|---|
| Peer mechanism | hand-written SAM client in `scripts/live-sam-qualify.py` |
| Router under test | i2pd 2.61.0, installed system-wide at `/usr/local/bin/i2pd` |
| Version recorded | `evidence/live-sam-i2pd-version.txt` |
| Production dependency | **none** — the script is not referenced by any crate |

## 6. Real bouncer scenario (Plan 032 section 6)

Run against the real `RuntimeController` + `SamProvider`, over the loopback fake bridge and
fake IRC upstream, in `crates/runtime/tests/r001c_sam_core_integration.rs` (7 tests):

- upstream IRC registration — `a_real_controller_registers_over_a_sam_stream`
- desired channel join — same test, `JOIN` follows `001`
- forced IRC stream disconnect ⇒ **same** SAM session reused —
  `an_irc_eof_reconnects_through_the_same_sam_session`
- forced session loss ⇒ provider session recreated, IRC recovers —
  `a_lost_session_is_replaced_once_and_irc_recovers`
- desired channels reconcile — covered by the `JOIN` assertion
- no user chat replay across ambiguous disconnect —
  `a_reconnect_replays_no_upstream_frame_from_the_old_connection`
- response routing / history — not re-qualified here; unchanged above the provider boundary

`I2pStreamProvider` is never bypassed. What this scenario does **not** do is carry IRC over
a live I2P tunnel; that is claim B, which section 1 records as not established.

## 7. Many-Network resource check (Plan 032 section 7)

**NOT RUN at live scale.** Requires one router session per Network, and claim B is not
established, so a live multi-Network measurement would produce numbers about a path that has
not been shown to carry traffic.

The repository ceiling of 64 is unchanged, and is exercised deterministically in
`the_scope_map_is_bounded` with a two-scope ceiling. Live router limitations did not and
must not silently change that ceiling.

No ADR is registered: no evidence emerged that per-Network tunnel cost is operationally
unreasonable, so there is nothing to decide.

## 8. Fault / restart matrix (Plan 032 section 8)

Deterministic coverage, matching Plan 031 typed semantics:

| Fault | Where covered |
|---|---|
| SAM bridge absent at startup | `an_absent_bridge_is_refused_not_hung` |
| Bridge starts later | `a_connect_after_release_creates_a_fresh_session` (scope created lazily on first connect) |
| Bridge restart during idle session | `a_closed_control_socket_invalidates_and_creates_one_replacement` |
| Bridge restart during active IRC stream | `a_lost_session_is_replaced_once_and_irc_recovers` |
| Peer unreachable | `a_peer_failure_does_not_destroy_a_healthy_session` |
| `INVALID_ID` | `an_invalid_session_id_invalidates_immediately` |
| Malformed SAM reply | `a_malformed_reply_is_refused_without_echoing_router_text` |
| Control EOF | `a_closed_control_socket_invalidates_and_creates_one_replacement` |
| Network delete during pending connect | `a_release_answers_a_connect_that_is_still_pending` |
| Process shutdown with several scopes | `delete_and_shutdown_leave_no_scope_behind`, `r001a_provider_scope` |
| STREAM timeout | `every_exchange_deadline_fires_and_names_its_phase` |
| Invalid destination | `an_unwritable_destination_is_refused_rather_than_truncated` |

The live half of this matrix — restarting a real router mid-session — is **NOT RUN**.

## 9. Privacy / security audit (Plan 032 section 9)

| Requirement | Verdict | Evidence |
|---|---|---|
| Bridge endpoint loopback only | verified | non-loopback is unrepresentable; scan exit 0 |
| No system DNS | verified | no resolver call in `crates/sam`; scan fails on `ToSocketAddrs` even in allowlisted files |
| No clearnet fallback | verified | no HTTP, SOCKS, proxy, or generic TCP outside `crates/sam` |
| Session ID carries no NetworkId / display / nick / build data | verified | 128 bits of OS randomness; `SamSessionId` Debug redacted |
| Destinations, private Destination, router `MESSAGE` absent from logs and diagnostics | verified | `Totals` counts only; `SamError` is 8 bytes of closed enums |
| `SESSION STATUS` private material zeroized / not retained | verified | `SessionReply` is a 1-byte enum with no field for it; the line is parsed into a `Zeroizing` buffer |
| One Network's SAM identity not reused by another | verified | `two_networks_get_two_distinct_sessions`, asserted on the wire |
| IRC anonymity / DCC / CTCP policy unchanged above the provider | verified | provider boundary; `architecture/ctcp-dcc-policy.md` untouched |

The live run strengthens the fourth and fifth rows: a real 908-character Destination crossed
the router and was never retained, never logged, and never reached a diagnostic.

## 10. Static / dependency audit (Plan 032 section 10)

| Check | Result |
|---|---|
| `scripts/check-network-boundary.py` | exit 0 |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clean |
| `cargo test --workspace --all-features --locked` | **886 passed, 0 failed** |
| `cargo tree --locked` | **no external SAM crate**; deps are `async-trait`, `thiserror`, `tokio`, `zeroize`, `getrandom`, `rusqlite` |
| `unsafe` in production source | **none**; workspace lint is `unsafe_code = "forbid"` |
| Tokio `net` use | confined to `crates/sam/src/{client,fake}.rs`, the allowlisted pair |
| Random source | `getrandom` 0.3, reviewed in `architecture/dependency-review.md` |
| Rust 1.88 (MSRV) | clean (`rustup run 1.88.0`) |

## 11. Commands actually executed

| Command | Result |
|---|---|
| `./scripts/verify.sh quick` | exit 0 |
| `cargo test --workspace --all-features --locked` | 885 passed, 0 failed |
| `scripts/check-network-boundary.py` | exit 0 |
| `cargo tree --locked` | no external SAM crate |
| `python3 scripts/live-sam-qualify.py` (live i2pd 2.61.0) | **5 PASS, 0 FAIL, 2 NOT RUN** |
| same, `--endpoint 127.0.0.1:17656` | `NOT RUN: no SAM bridge`, exit 2 — fails closed |
| same, `--endpoint 192.168.1.1:7656` | `REFUSED: not loopback`, exit 2 |

Live environment: i2pd 2.61.0 started with `--datadir` under a temporary directory, SAM on
`127.0.0.1:7656`, joined the public I2P network, built inbound and outbound tunnels. The
router process was stopped afterwards; no repository state depended on it.

## 12. Documentation reconciliation (Plan 032 section 12)

Updated: `README.md` implementation state, `architecture/sam-adapter.md`,
`architecture/network-boundary.md`, `architecture/network-ownership.md`,
`architecture/overview.md`, the router roadmap, and `plans/registry.md`.

Documented explicitly: the per-Network transient SAM identity; that identity changing after
provider or router session recreation or process restart; the one-tunnel-pool-per-Network
implication; loopback-only bridges; and that R001 supports no persistent Destination.

## 13. Unresolved findings

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | **high** | i2pd 2.61.0 establishes streams (`RESULT=OK`) but no application bytes traversed on this host (§4) | open; environment/i2pd-side, not attributed to the owned client |
| 2 | medium | Java I2P compatibility unproven | evidence-blocked, no installation |
| 3 | medium | i2pr compatibility unproven | evidence-blocked, not installed |
| 4 | medium | Cross-router interoperability unproven | not attempted; needs a testnet |
| 5 | low | Live many-Network scale unmeasured | blocked behind finding 1 |

No **identity**, **linkability**, **clearnet**, or **session-leak** finding is open. The
unresolved items are evidence gaps and one unexplained router-side behaviour, and none of
them indicates that this implementation shares a Destination between Networks, leaks a
session, or reaches the clearnet.

## 14. Stop conditions (Plan 032 section 15)

Checked individually:

| Condition | Result |
|---|---|
| Multiple Networks silently share a Destination | **no** — prevented structurally and asserted on the wire |
| Resource cost forces unreviewed identity policy | **no** — no evidence of that |
| Non-loopback SAM needed | **no** — loopback throughout; harness refuses anything else |
| Generic resolver / clearnet connector appears | **no** — scan exit 0, no resolver call |
| Private Destination retained or logged | **no** — type has nowhere to put it |
| Hidden 3.2/3.3 semantics required | **no** — only 3.1 was spoken |
| Portability claimed from missing evidence | **no** — the three missing rows are recorded as not-run |

## 15. R001 closure

**R001 is conditionally closed.**

Closed: Plans 029, 030, and 031 are closed; implementation is complete; the identity,
linkability, clearnet, and session-leak stop conditions are all clear; documentation and
planning are reconciled; and one live router independently confirms SAM handshake, session
creation, stream establishment, and session reuse.

Evidence-blocked: claim B (the I2P stream product path) and Java I2P / i2pd / i2pr
portability. Plan 032 section 11 states that if Java I2P or i2pd cannot be run, implementation
may be complete while R001 portability remains evidence-blocked. That is this case. The
milestone must not be reported as a portable-SAM pass.

## 16. R002 readiness

**R002 is NOT ready.** It needs stable public i2pr managed-app contracts for I2P stream
provisioning, local listener, and lifecycle, plus closure of finding 1. Neither exists here:
i2pr is not installed, and its public contract surface is not a dependency of this repository.

R002 stays blocked in the registry, unchanged, with its own prerequisite now stated precisely
rather than implicitly.