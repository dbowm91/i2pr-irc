# Standalone M010-B / Plan 051 — Local Listener and Bounded Authentication

Status: proposed — dependency-gated on Plan 050 closure
Repository baseline for planning: f325e7d5e495e36b1fc7b168c30e4b725756d22c
Primary class: security invariant + infrastructure
Authority: plans/subsystems/standalone-daemon-roadmap.md; ADR-0007; Research 010

## 1. Objective

Introduce real bounded loopback/Unix socket acceptance and the local Operator authentication state machine, without granting unauthenticated callers a ClientId or operator control. The result is a tested private authenticated-session substrate; Plan 052 owns integration with the existing downstream registration/session model.

## 2. Readiness

Not ready until 050's daemon/process lease, runtime startup/stop and config model are closed. ADR-0007 accepted. Public tests may use an injected fake credential verifier; production credential provisioning waits on 053. Do not introduce a default password or bypass for test convenience.

## 3. Invariants

- Accept numeric IPv4/IPv6 loopback only; reject 0.0.0.0, ::, remotely routed addresses, hostnames and local-domain coercion; no non-loopback bind/fallback.
- Unix socket optional/Unix-target-only, private directory/socket, symlink/stale-socket safe handling and no automatic auth bypass.
- No Controller requests, BouncerServ, durable ClientId creation, upstream reads/writes or history until authenticated.
- Authentication consumes bounded input, time, attempts, decode work and resident memory; scheduling admission is nonblocking and resource-permit gated before spawn.
- No raw auth frame, token, username, authcid or SASL secret in log/debug/diagnostic errors.
- Failed or cancelled pre-auth sessions cannot leave a socket/writer/registration task alive.

## 4. Required changes

Add concrete local listener modules only under crates/daemon (or a tightly bounded standalone-local adapter crate justified in the closure). The listener task owns TCP/Unix accept, an auth coordinator, handshake deadlines, and cancellation. Introduce a credential verification interface over OS-generated high-entropy secrets; 053 will implement persisted credential sources. Test verification must use explicit fixtures; human passwords are out of scope.

Implement IRCv3-aware pre-auth parsing of PASS and SASL PLAIN. Permit CAP LS/REQ/END and NICK/USER ordering before successful authentication as protocol requires, but never permit control or ordinary upstream commands. Return bounded CAP LS/NAK based on unconditional bouncer capabilities and sasl=PLAIN; dynamically conditional upstream capabilities must not be misadvertised before an authenticated Network is selected. For SASL PLAIN validate bounded base64 decoding, authzid/authcid shape, chunk count, sequence and failure numeric; reject mixed/conflicting PASS and SASL. For PASS use one documented token/profile grammar; do not treat upstream SASL secret as the local credential.

The pre-auth parser/writer must expose a movable state checkpoint, not a transformed copy of an already-consumed IRC transcript. The initial checkpoint may be private until 052. If existing SessionReader must be refactored to prevent double CAP replies, preserve the *single canonical* IRC registration implementation. Any refactoring needed to cross an API boundary must be planned and covered before enabling public listener operation.

Update scripts/check-network-boundary.py and manifest scanning to include crates/daemon: allow exactly local socket bind/accept source modules; no outgoing TCP socket, resolver, HTTP/SOCKS, UDP or DCC APIs. Add positive controls that prove an unauthorized module, nonloopback bind or generic dial fails the guard. Raw string token scanning alone is not sufficient; complement with typed address validation tests.

## 5. Ordered work

A. Fix listener configuration grammar, default loopback and explicit unsupported-platform error.
B. Introduce bounded accept loop, semaphore/permit admission and precise shutdown cancellation.
C. Implement shared byte/line-framing + one logical preauth registration state machine, with CAP-first, PASS and SASL PLAIN.
D. Introduce secret verification and typed untrusted->authenticated session transition; no control handle reachable earlier.
E. Narrow static guard exceptions and test both permitted and denied socket authority.

## 6. Adverse and restart behavior

Idle accepted sockets time out before the existing DownstreamAdmission registration deadline. Flooded sockets consume at most configured permits; overload closes/refuses promptly without queueing unbounded tasks. Bad auth, repeated credentials, malformed SASL and truncated frames give bounded failures. On shutdown, stop accept first and cancel/close pending handshake sockets before controller stop. A disconnected half-authenticated session has no persistence side effects. Listener bind failure does not leave the controller running without the explicitly requested service (unless an opt-in headless mode is documented).

## 7. Tests and verification

PASS first; CAP LS before PASS; CAP REQ sasl and fragmented AUTHENTICATE PLAIN; NICK/USER before auth; double auth attempt; wrong secret; oversized frame/chunks; client stalls; coalesced PASS+registration bytes; multiple concurrent idle sockets; permit exhaustion; shutdown race; unsafe Unix socket path; nonloopback addresses; IPv4/IPv6 loopback. Capture exact wire responses to prove no CAP double-response within 051's preauth stage.

Run targeted tests, boundary guard with positive controls, full workspace verification on supported toolchains and Rust 1.88. On unsupported OS, mark the Unix-specific tests skipped with platform reason, not green. No live router necessary.

## 8. Docs / acceptance / stop

Document local authentication model and unsupported remote mode, no password or implicit trust for Unix, and the provisional substrate status. Accept only if no unauthenticated stream reaches privileged code and all failure paths are demonstrably bounded. Stop if CAP/SASL ordering cannot be expressed without re-answering messages or if guard enforcement requires a generic socket/network exception.

## 9. Closure

Record commits, test commands, denial matrix, concurrency/resource ceilings, security review, platform qualifications, and any remaining registration handoff requirements in plans/closure/standalone/051-status.md. Promote 052 only after 051 closes.
