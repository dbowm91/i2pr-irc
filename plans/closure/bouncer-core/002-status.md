# Bouncer Core M002 Closure Record

Status: closed

Source plan: `plans/implementation/bouncer-core/002-single-network-operational-bouncer.md`

Dependency: M001 closed in `plans/closure/bouncer-core/003-status.md`.

Implementation commits:

- `c388316` — Implement single-network operational bouncer
- `a9dc923` — Bound upstream CAP and SASL negotiation time

The previous `blocked` decision in this file described the registration-only repository state at `89f7e30a3c32a5828ff1643c58d52f27e20bd1b2`, before M001's corrective closure. It is superseded by this evidence-based review; that historical decision was correct for its reviewed baseline.

## Requirement-to-evidence matrix

| Requirement | Evidence | Result |
|---|---|---|
| One upstream owner and one local session; I2P provider boundary | `NetworkSupervisor::serve` and `run_generation` in `crates/runtime/src/lib.rs`; injected `I2pStreamProvider` and `LocalAcceptor`; network-boundary guard | Pass. No generic resolver, upstream socket, SAM, or router internals were added. |
| CAP 302, multiline LS, policy-owned requests, configured SASL PLAIN | registration loop; `configured_sasl_plain_completes_without_secret_diagnostics`; `configured_sasl_unavailable_is_a_terminal_registration_error` | Pass. Only configured SASL is requested; unavailable/rejected configured authentication is terminal. |
| Bounded config, wire, state, and queues | `UpstreamConfig::validate`; named capacities and member/channel/ISUPPORT bounds; overflow and priority tests | Pass. Overload is explicit. |
| Downstream registration and truthful current-state projection | `single_client_vertical_registers_routes_and_answers_ping`; `architecture/downstream-session.md` | Pass for the declared single injected local stream: welcome, ISUPPORT, JOIN, topic, modes, and member list are synthesized from observed state. No downstream capabilities are claimed. |
| Typed intents, generation fencing, and ambiguous delivery | `OutboundIntent`, generation comparison in writer, `ambiguous_chat_is_not_replayed_into_replacement_generation`; stale-generation test in testkit | Pass. Chat/query intents are generation-scoped and are not retained across replacement connections; desired JOIN state is regenerated after registration. |
| Liveness, phase deadlines, reconnect, and stable bounds | `CAP_SASL_TIMEOUT`, `REGISTRATION_TIMEOUT`, `CONNECT_TIMEOUT`, online PING/PONG deadline; paused-time registration/CAP-SASL/liveness tests; bounded `Backoff`; 100 provider failures/recovery test | Pass. Timeouts and retry policy use bounded operational defaults; CAP/SASL has its own deadline. |
| Control priority and queue overload | `ready_control_frame_precedes_normal_backlog`; `control_queue_is_separate_and_normal_overflow_is_explicit` | Pass. PONG/control queue is separate from normal traffic. |
| Stop and task ownership | `JoinSet` writer ownership; `stop_cancels_in_progress_registration`; vertical shutdown test; writer timeout | Pass. Owned writer tasks are joined on clean stop and aborted with their generation. |
| Deterministic fault qualification and hostile input limits | testkit reset/EOF/short-I/O/stall/stale-generation tests; runtime recovery and no-replay tests; wire boundary and arbitrary-byte tests; full verification and fuzz-smoke | Pass for the M002 single-network scope. This is deterministic smoke/property qualification, not coverage-guided fuzzing or live-router qualification. |
| Secret/anonymity and product boundary | redacted `Secret` Debug/drop zeroization; temporary SASL buffers use `Zeroizing`; `architecture/security-anonymity.md`; boundary guard | Pass for the M002 implementation boundary. No credentials or auth payloads enter diagnostics. |

## Runtime ownership and state transitions

`serve` owns one supervisor and serially creates generations. Each `run_generation` owns the split upstream/downstream streams, bounded channels, and a `JoinSet` for writer tasks. The supervisor owns connect and registration deadlines, backoff, snapshots, and cancellation. On a failed generation its channels and writer tasks are dropped before the next generation is started. Online writer intents carry their generation and stale intents are discarded.

The operational path is `Idle -> Connecting -> Registering -> Online -> Backoff -> Connecting`, with terminal `Stopping/Stopped` on explicit cancellation and terminal registration disposition for configured authentication rejection. Provider/connect, registration, CAP/SASL, and online liveness have bounded deadlines. Backoff grows exponentially from one to 300 seconds with deterministic bounded jitter and resets only after a stable online interval.

## Security and recovery review

- Production upstream authority remains the injected `I2pStreamProvider`; local downstream access remains an injected `LocalAcceptor` capability.
- One downstream client is supported. It cannot select an endpoint or alter upstream CAP policy.
- IRC output is bounded and validated; unsupported downstream commands receive a local error. Tags are removed when forwarding because message-tags semantics are not advertised downstream.
- Ambiguous user writes are never replayed. Observed channel/member/topic/mode state is generation-local; configured desired channels are joined again only after fresh registration.
- Configuration/authentication failures do not retry indefinitely. Provider, I/O, protocol, and timeout failures use bounded reconnect backoff. Explicit stop interrupts connect, registration, and backoff waits.
- The vertical is not a daemon or router integration. It has no real SAM provider, real local listener, durable state, or live-router qualification; these remain outside M002's declared deliverable boundary.

## Commands executed

All commands below completed successfully on the final M002 tree:

- `rtk cargo fmt --all -- --check`
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk cargo test --workspace --all-features` — 48 tests across 9 suites
- `rtk scripts/verify.sh quick`
- `rtk scripts/verify.sh full`
- `rtk scripts/fuzz-smoke.sh`
- `rtk scripts/check-network-boundary.py`
- `rtk rustup run 1.88.0 sh scripts/verify.sh full`
- `rtk rustup run 1.88.0 sh scripts/fuzz-smoke.sh`

## Findings and roadmap disposition

No M002-blocking security or recovery finding remains. Remaining limitations are the explicit M002 boundary: a single in-memory Network and injected local stream, with no durable storage, production listener, or concrete router adapter. Extensive adverse-network/anonymity qualification remains M004 work.

M002 is closed. Its hard dependency is satisfied, so M003 may now be planned and handed off against this closure baseline. M003 remains unplanned and is therefore recorded as planning-eligible rather than implementation-ready. M004 and M005 remain sequenced behind M003 and M004 respectively. Router R001 remains blocked by the canonical M005 dependency; R002 and R003 retain their separate public-capability/product conditions.
