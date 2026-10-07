# Router Integration R001-B / Plan 030 — Owned SAM 3.1 Wire/Client Foundation

Status: blocked

Hard dependency:

- plans/closure/router-integration/029-status.md

Authority:

- plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md

Primary class: infrastructure + invariant

## 1. Objective

Add a small project-owned SAM 3.1 STREAM client foundation in a dedicated crate with strict bounds, deadlines, redaction and loopback-only bridge authority.

This plan does not yet implement the process-wide I2pStreamProvider session map. It produces the audited building blocks Plan 031 composes.

## 2. Required crate boundary

Add path crates/sam with package i2pr-irc-sam.

Allowed dependencies:

- i2pr-irc-core;
- Tokio I/O/time/net features;
- thiserror;
- zeroize;
- one reviewed OS-random primitive for opaque SAM session IDs.

No i2pr private crates. No external SAM library production dependency.

## 3. Network authority

SamBridgeEndpoint is a numeric SocketAddr only and ip.is_loopback() must be true.

Default: 127.0.0.1:7656.

Explicit [::1]:port is valid.

Reject non-loopback addresses, hostnames including localhost, URLs, unix-path reinterpretation, and invalid/forbidden ports according to final config policy.

There is no resolver call.

## 4. Owned protocol profile

Implement only:

- HELLO VERSION MIN=3.1 MAX=3.1;
- SESSION CREATE STYLE=STREAM;
- STREAM CONNECT;
- required reply/status parsing;
- line-to-raw stream transition.

Do not implement ACCEPT, FORWARD, DATAGRAM/RAW, PRIMARY/subsessions, generic send_command, arbitrary SAM option passthrough, SAM auth/TLS/remote bridge, persistent Destination import/export, or router control APIs.

NAMING LOOKUP is not required initially because selected Java I2P/i2pd/i2pr STREAM CONNECT profiles accept typed endpoint forms directly. If qualification disproves this, stop and amend the profile explicitly rather than resolving elsewhere.

## 5. SAM line framing

Freeze:

- MAX_SAM_LINE_BYTES = 4096;
- MAX_SAM_TOKENS = 64;
- MAX_SAM_KEY_BYTES = 64;
- MAX_SAM_VALUE_BYTES = 3072.

Properties:

- LF and CRLF accepted according to reference behavior;
- NUL/embedded CR/LF rejected;
- partial reads supported;
- overflow drops until newline without swallowing the next valid line;
- token/options count bounded;
- duplicate required-option behavior explicit by reply type;
- quoted MESSAGE parsed only enough to classify/skip safely;
- router MESSAGE text never appears verbatim in normal diagnostics.

The buffer holding SESSION STATUS is zeroized after parsing because a successful transient session reply may contain private Destination material.

## 6. Typed replies/state

Use typed enums, not an upward string map.

At minimum model:

- HelloOk31;
- HelloNoVersion;
- SessionOk;
- SessionDuplicateId;
- SessionDuplicateDestination;
- SessionError;
- StreamOk;
- StreamCantReachPeer;
- StreamInvalidKey;
- StreamInvalidId;
- StreamTimeout;
- StreamError;
- malformed/unexpected response.

Strict sequencing:

~~~
new socket
  -> hello pending
  -> hello 3.1
  -> session-create pending OR stream-connect pending
  -> session active OR raw stream
~~~

Unexpected responses are typed protocol failure.

## 7. Opaque session IDs

SAM IDs use at least 128 bits of OS randomness, ASCII-safe bounded encoding, and contain no NetworkId/display name/nick/endpoint/process id/timestamp/build string.

Expose randomness behind a tiny injectable source for deterministic tests. Random-source failure is explicit and never falls back to identifying material.

Perform MSRV/license/dependency review before adding the random primitive.

## 8. SESSION CREATE profile

Emit deterministic allowlisted fields:

~~~
SESSION CREATE STYLE=STREAM
ID=<opaque>
DESTINATION=TRANSIENT
SIGNATURE_TYPE=7
i2cp.leaseSetEncType=4
i2cp.dontPublishLeaseSet=true
inbound.quantity=2
outbound.quantity=2
~~~

Serialize as one bounded line. No arbitrary user option fragments.

Do not retain the private Destination returned by the router.

A successful session object retains only opaque session ID, control socket, and local non-secret epoch/health metadata.

## 9. STREAM CONNECT

For each outbound stream:

1. open loopback socket to configured bridge;
2. HELLO 3.1;
3. send STREAM CONNECT ID=<opaque> DESTINATION=<I2pEndpoint> SILENT=false;
4. parse exactly one STREAM STATUS;
5. on RESULT=OK transition the same socket to raw bytes;
6. return a stream compatible with core ByteStream.

After OK no SAM parsing occurs on the data socket.

Never log the complete remote Destination.

## 10. Deadlines

Use a typed/injectable timeout profile.

Starting production values:

- bridge TCP connect: 10 s;
- HELLO: 10 s;
- SESSION CREATE/tunnel build: 120 s;
- stream socket TCP connect: 10 s;
- stream HELLO: 10 s;
- STREAM CONNECT: 90 s.

The stream budget is above the SAM documentation's approximately-one-minute router timeout.

Plan 031 must reconcile the outer runtime provider-connect timeout with the complete cold-session path.

Tests use injected short durations rather than production sleeps.

## 11. Cancellation

Dropping a pending client future drops its owned socket and leaves no detached task. Prefer direct async state machines. The long-lived control watcher belongs to Plan 031.

## 12. Error taxonomy

Define bounded SamError variants for invalid bridge endpoint, bridge unavailable, timeout(phase), unsupported version, session rejected(class), session lost, destination rejected, peer unavailable, stream rejected(class), malformed reply, random unavailable, and cancellation/closed where representable.

No variant stores arbitrary router MESSAGE text, private key, or full Destination.

## 13. Static boundary

Update scripts/check-network-boundary.py so crates/sam is the only upstream adapter path allowed to use Tokio TCP.

Positive controls prove planted TcpStream use in runtime/core fails, resolver APIs remain forbidden, and SamBridgeEndpoint is required before connect.

Dependency review records Tokio net feature enablement and why source-level authority remains path-confined.

## 14. Test bridge

Add deterministic loopback fake SAM bridge in the SAM crate test surface.

Script fragmented replies, CRLF/LF, oversized line then valid line, HELLO OK/NOVERSION/malformed, SESSION outcomes, STREAM outcomes, delayed responses, control close, and exact raw transition including NUL/invalid UTF-8/SAM-looking payload.

The fake is test-only, not a second production SAM server.

## 15. Work packages

A. crate/dependency boundary.
B. bridge endpoint.
C. line/token parser.
D. reply/state model.
E. opaque ID source.
F. session-create client.
G. stream-connect/raw transition.
H. deadlines/cancellation.
I. static guard.
J. fake-bridge suite.
K. docs/closure.

## 16. Acceptance criteria

Plan 030 closes when the crate can create one fake SAM 3.1 STREAM session and open exact raw streams under all framing/error/deadline tests, while exposing no generic SAM command surface and no non-loopback authority.

## 17. Stop conditions

Stop if SAM 3.1 interoperability requires hidden 3.2-only behavior, a supported router requires non-loopback baseline access, session establishment requires retaining private Destination material, a third-party SAM production dependency becomes necessary, or TCP authority cannot be path-confined.

## 18. Closure evidence

Create plans/closure/router-integration/030-status.md with dependency/MSRV review, command transcript matrix, max/max+1 framing, timeout/cancellation matrix, redaction/private-Destination handling, binary raw transition fixture, boundary positive controls, and Plan 031 readiness.
