# Research 007 — R001 Owned SAM 3.1 Client and Provider-Scope Research

Status: complete for implementation planning

Research date: 2026-10-07

Repository baseline:

- `b61ce0605eaea66236e1888975a7842f22ba1ee0`

Related authority:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`
- `plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md`
- `plans/adrs/ADR-0005-explicit-i2p-provider-scope-release.md`
- `plans/subsystems/i2p-router-integration-roadmap.md`
- `plans/closure/bouncer-core/028-status.md`

Primary external references:

- official SAM v3 documentation: https://www.i2p.net/en/docs/api/samv3/
- i2pr SAM 3.1 roadmap/evidence: https://github.com/dbowm91/i2pr
- Yosemite SAM client: https://github.com/eggstack/yosemite
- i2p-rs client library: https://github.com/i2p/i2p-rs

## 1. Purpose

Freeze the smallest production SAM profile required by R001 and decide whether R001 should depend on an existing Rust SAM client or own that narrow protocol surface while the project's standalone SAM library matures.

## 2. Official SAM findings

The current official SAM documentation identifies SAM 3.1 as the recommended minimum.

For a basic stream client the relevant commands are:

- `HELLO VERSION MIN=3.1 MAX=3.1`;
- `SESSION CREATE STYLE=STREAM ...`;
- `STREAM CONNECT ID=... DESTINATION=...`.

The documentation also lists `DEST GENERATE`, `NAMING LOOKUP`, and `STREAM ACCEPT` for more general applications. R001 does not require them to open outbound IRC streams when the router's STREAM CONNECT path accepts the endpoint forms the bouncer supports.

SAM sessions/tunnel pools are intended to be long-lived. The documentation explicitly cautions against rapidly creating/discarding sessions and recommends specifying tunnel quantities because Java I2P and i2pd use different defaults.

For transient sessions:

- `SIGNATURE_TYPE=7` is the recommended Ed25519 type;
- `i2cp.leaseSetEncType=4` is the conservative ECIES-X25519 encryption profile;
- `i2cp.leaseSetEncType=6,4` is suitable only where newer API support is known.

R001 therefore uses `4` as the portability baseline and does not make MLKEM support a hidden requirement.

The official document states that Java I2P and modern i2pd accept hostnames and b32/b33 forms in `STREAM CONNECT DESTINATION=`. Direct STREAM CONNECT is therefore sufficient for the initial typed `I2pEndpoint` forms. NAMING LOOKUP can remain outside the first production profile unless live router evidence shows a supported endpoint form requires it.

STREAM CONNECT response behavior:

- `RESULT=OK` transitions the socket to raw stream bytes;
- `CANT_REACH_PEER`, `I2P_ERROR`, `INVALID_KEY`, `INVALID_ID`, and `TIMEOUT` are typed failure candidates;
- the router's internal connect timeout is approximately one minute and the SAM documentation warns clients not to impose a shorter STREAM CONNECT response deadline.

This means the bouncer's SAM stream-connect phase needs its own deadline longer than the router's normal attempt window, while still remaining below the outer runtime's bounded connection-attempt budget.

## 3. Endpoint finding

The current `I2pEndpoint` accepts a textual Destination only when it is exactly 516 characters and globally caps endpoints at 516 bytes.

The official SAM documentation says a Base64 Destination is **516 or more** characters depending on signature type.

R001 must therefore correct this type before live SAM integration.

Decision:

- continue accepting canonical hostname, standard b32, extended b32/b33-profile and Base64 Destination forms only;
- increase the application-level endpoint ceiling to an explicit bounded value;
- accept Base64 Destination strings at or above the legacy 516-character floor up to that ceiling;
- keep `I2pEndpoint` opaque/redacted in Debug;
- do not attempt to validate every certificate/signature subtype cryptographically inside the bouncer; the SAM bridge remains authoritative for full Destination validity.

The implementation plan should select one conservative ceiling (recommended 4096 textual bytes, matching the owned SAM control-line envelope) and prove max/max+1 behavior.

## 4. Existing Rust clients

### Yosemite 0.7

Strengths:

- MIT;
- active project;
- Tokio/smol/sync modes;
- explicit STREAM session object;
- returned stream implements Tokio AsyncRead/AsyncWrite;
- current public API supports STREAM, forwarding, datagrams and primary sessions;
- local SAM TCP is hard-coded to loopback with configurable port, which is directionally compatible with R001.

Gaps relative to R001's proof obligations:

- command responses are read until newline into a growable String/Vec without this project's explicit line ceiling;
- no R001-specific command deadlines;
- HELLO does not freeze MIN/MAX to 3.1 in the inspected path;
- broad command/session API exceeds the bouncer's required authority;
- logging/error paths need review against destination/private-material redaction;
- the library's lifecycle API is not the project's explicit `NetworkId` scope/release contract.

Disposition:

- useful future conformance/reference implementation;
- not the R001 production dependency.

### i2p-rs

The public crate remains an early-development API and uses an older dependency stack.

Disposition:

- reference/history only;
- not a production candidate for R001.

### i2pr SAM

The i2pr repository's SAM 3.1 server line is substantially mature at the local SAM protocol/product boundary:

- bounded SAM parsing/state;
- loopback listener;
- SESSION CREATE;
- STREAM CONNECT/ACCEPT;
- NAMING/FORWARD;
- independent external client evidence;
- deterministic slow-reader/slow-writer/fault campaigns.

Its own closure correctly does not broaden this into router-to-router I2P interoperability.

Disposition:

- high-value R001 server/conformance target;
- not a crate dependency;
- do not import private i2pr SAM/router internals.

## 5. Owned client decision

R001 will own the small SAM 3.1 client profile in a dedicated adapter crate.

Recommended package/path:

- path: `crates/sam`;
- package: `i2pr-irc-sam`.

The crate owns generic TCP authority only to a validated loopback `SocketAddr` representing the SAM bridge.

No hostname is accepted for the SAM bridge itself, avoiding host DNS entirely.

Default bridge:

- `127.0.0.1:7656`.

IPv6 loopback may be accepted as an explicit `[::1]:port`.

## 6. Provider-scope finding

The existing provider is process-wide:

~~~rust
async fn connect(&self, endpoint: &I2pEndpoint) -> Result<Box<dyn ByteStream>, ProviderError>;
~~~

A SAM STREAM session owns an I2P Destination.

If the process-wide provider owned one SAM session, all configured IRC Networks would share that I2P Destination and gain avoidable cross-Network linkability.

Decision in ADR-0004:

- provider connections carry `NetworkId`;
- default R001 SAM ownership is one long-lived session per active Network;
- IRC reconnects reuse that session;
- provider/session recreation uses a transient Destination;
- private Destination material is not persisted in R001.

## 7. Provider-release finding

A long-lived provider scope cannot infer durable Network deletion from stream lifetime.

ADR-0005 therefore adds explicit:

~~~rust
release(NetworkId)
~~~

semantics.

`RuntimeController` already owns the correct durable lifecycle boundary.

Expected ordering:

### Network deletion

1. stop/join the Network owner;
2. call provider `release(NetworkId)`;
3. commit/finish durable deletion according to the controller's existing mutation semantics;
4. publish the resulting runtime state.

The exact placement relative to durable removal must preserve existing unknown-commit handling; the implementation plan must not invent a state where durable state says deleted while an old owner can still reconnect.

### Process shutdown

1. stop/join all owners;
2. release every live provider scope;
3. stop remaining process services.

Release is idempotent and bounded.

## 8. SAM session identity

SAM session IDs must be globally unique on a bridge.

Do not serialize:

- durable NetworkId;
- display name;
- IRC nick;
- endpoint;
- package/version.

Use an opaque random session ID generated at session creation.

A small OS-random dependency may be added only after MSRV/license/dependency review. If used, expose randomness behind a tiny injectable source so protocol tests remain deterministic.

The random ID is not a durable user identity.

## 9. Minimal SESSION CREATE policy

Initial owned profile:

~~~text
SESSION CREATE
STYLE=STREAM
ID=<opaque>
DESTINATION=TRANSIENT
SIGNATURE_TYPE=7
i2cp.leaseSetEncType=4
i2cp.dontPublishLeaseSet=true
inbound.quantity=2
outbound.quantity=2
~~~

Rationale:

- Ed25519 avoids the obsolete default;
- ECIES-X25519 is the conservative modern encryption baseline;
- outbound-only bouncer sessions do not need public LeaseSet publication;
- quantity 2 gives deterministic Java/i2pd behavior and limits the cost of per-Network identity.

Do not expose arbitrary SAM/I2CP option passthrough in R001.

Tunnel length remains router/profile default in R001 unless interoperability evidence requires an explicit value.

## 10. SAM control framing

The owned client needs a strict line reader separate from IRC framing.

Recommended initial ceilings:

- control line: 4096 bytes including terminator;
- token count: 64;
- key bytes: 64;
- value bytes: 3072;
- response MESSAGE is parsed only as bounded diagnostic classification and is never emitted raw.

These are application profile limits, not claims about every SAM implementation.

Max/max+1 fixtures are required.

All command keywords/result values are ASCII.

After `STREAM STATUS RESULT=OK`, the stream socket transitions permanently from SAM line mode to opaque raw bytes. No further SAM parsing occurs on that socket.

## 11. Deadlines and cancellation

Separate phases:

- SAM TCP connect;
- HELLO;
- SESSION CREATE;
- STREAM socket TCP connect;
- STREAM HELLO;
- STREAM CONNECT.

STREAM CONNECT must allow the router's documented approximately-one-minute internal connect behavior. The plan should use a bounded value safely above that (for example 90 seconds) and ensure the outer reconnect scheduler remains the final attempt-rate governor.

Dropping a pending `connect()` future must close/drop its socket and remove any queued provider request without leaving a detached task.

Control-session loss is a provider/session event, distinct from ordinary remote IRC EOF.

## 12. Error taxonomy

Map SAM failures into a small adapter taxonomy before mapping to `ProviderError`.

At minimum distinguish internally:

- bridge unavailable/connect failure;
- negotiation/version failure;
- session create failure;
- session lost;
- naming/invalid destination;
- peer unreachable;
- stream timeout;
- stream protocol/malformed reply;
- cancellation;
- local adapter overload.

Do not place raw router MESSAGE text in ProviderError or normal diagnostics.

R001 may initially map several classes to `ProviderError::Unavailable`, `Timeout`, `Cancelled`, or `Failed`, but tests must prove the mapping is deterministic and does not turn a router/session failure into an IRC auth/config failure.

## 13. Session owner architecture

One provider instance owns a bounded map:

~~~text
NetworkId -> SamNetworkScope
              |
              +-- state
              +-- one control/session owner task
              +-- opaque SAM session id
              +-- bounded connect request queue
              +-- generation/epoch
              +-- health snapshot
~~~

The map is bounded by `MAX_SUPERVISED_NETWORKS`.

A Network scope is lazily created on first connect and remains alive across IRC generations.

The owner serializes session establishment/re-establishment. It may process bounded stream-connect requests concurrently only after session state is healthy and only if doing so preserves the bouncer's outer reconnect/concurrency policy.

No SAM session creation stampede may bypass the process-wide `ReconnectScheduler`.

## 14. Router/session restart semantics

When the control session is lost:

- mark the scope unavailable;
- fence the old SAM epoch;
- active stream sockets eventually fail independently;
- no old queued connect may attach to a newly created session unless it still belongs to the current request/Network lifecycle;
- the next allowed provider connect recreates the session under the outer bouncer reconnect budget;
- a new transient Destination is expected.

Do not add an independent aggressive retry loop inside the SAM provider. The existing Network reconnect scheduler remains the attempt-rate authority.

## 15. Static network boundary

Today the repository intentionally carries no production generic upstream TCP authority.

R001 introduces one narrow exception:

- `crates/sam` may use `tokio::net::TcpStream`;
- destination must be a validated loopback `SocketAddr`;
- no resolver API;
- no arbitrary host string;
- no proxy/HTTP/SOCKS path.

Update the boundary script so:

- the exception is path-scoped;
- a planted `TcpStream` use elsewhere still fails;
- a planted non-loopback SAM endpoint path fails tests.

## 16. Cross-router qualification

Final R001 evidence should target all environments available to the project:

- Java I2P;
- i2pd;
- i2pr SAM 3.1.

Evidence rows are per router and per capability.

Missing environment = `not-run/operational gap`, never pass.

Minimum live path:

1. create SAM STREAM session;
2. connect to a controlled I2P streaming echo/IRC fixture;
3. exchange exact bytes;
4. drive a real bouncer IRC registration/session where feasible;
5. interrupt/restart SAM/router path;
6. prove bouncer reconnect semantics remain equivalent.

i2pr's local SAM acceptance evidence is useful but does not substitute for Java/i2pd rows.

## 17. Implementation decomposition

### R001-A / Plan 029 — provider scope and lifecycle foundation

- NetworkId-scoped connect;
- explicit release;
- update fakes/call sites/controller lifecycle;
- fix I2pEndpoint Destination length profile;
- preserve all bouncer-core verification.

### R001-B / Plan 030 — owned SAM 3.1 wire/client foundation

- new dedicated SAM crate;
- loopback endpoint type;
- bounded line/token parser;
- exact 3.1 HELLO;
- SESSION CREATE;
- STREAM CONNECT/raw transition;
- opaque session IDs;
- typed errors;
- fake bridge conformance.

### R001-C / Plan 031 — per-Network SAM provider integration

- bounded NetworkId -> session-owner map;
- long-lived transient sessions;
- provider connect/release implementation;
- router/session loss and cancellation;
- runtime integration;
- deterministic restart/resource tests.

### R001-D / Plan 032 — cross-router qualification and R001 closure

- Java I2P/i2pd/i2pr live evidence where environments exist;
- real bouncer-over-SAM scenario;
- restart/failure campaign;
- privacy/boundary audit;
- docs/registry/roadmap reconciliation;
- R001 closure and R002 readiness decision.

## 18. Readiness

Plan 029 is ready immediately.

Plan 030 is blocked on Plan 029 because the adapter should be built against the final provider scope and endpoint profile.

Plan 031 is blocked on Plan 030.

Plan 032 is blocked on Plan 031 and has operational evidence dependencies for portability claims.

No R001 work requires the standalone SAM library to be complete.
