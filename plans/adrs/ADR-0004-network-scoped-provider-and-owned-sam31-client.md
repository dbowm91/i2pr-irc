# ADR-0004 — Network-Scoped I2P Provider Identity and Owned SAM 3.1 Client

Status: accepted

Date: 2026-10-07

Related:

- `plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md`
- `plans/subsystems/i2p-router-integration-roadmap.md`
- `plans/closure/bouncer-core/028-status.md`

## Context

R001 is the first production router integration for i2pr-irc.

The existing core deliberately exposes only:

~~~rust
I2pStreamProvider::connect(&I2pEndpoint)
~~~

and `RuntimeController` shares one provider instance across every configured Network.

A SAM STREAM session owns an I2P Destination/tunnel pool. If one provider-level SAM session were shared by every Network, unrelated configured IRC Networks would originate from the same I2P Destination and become linkable at the I2P identity layer.

The project also evaluated existing Rust SAM client libraries. A maintained library such as Yosemite is useful as future conformance/reference material, but R001 needs only a narrow SAM 3.1 STREAM subset and has stricter requirements around bounded framing, cancellation, redaction, local-router authority, explicit version negotiation and session ownership.

The project is also developing a broader standalone SAM library separately. R001 must not wait on that library or bind its core API to the library's eventual shape.

## Decision drivers

- preserve the I2P-only upstream boundary;
- avoid accidental cross-Network I2P identity reuse;
- keep SAM syntax below the router-adapter boundary;
- own all bounds, deadlines and redaction required by this application;
- keep the first production SAM surface intentionally small;
- allow later replacement by the standalone SAM library without changing bouncer semantics;
- avoid adding durable private-destination storage before a concrete need exists.

## Considered options

### A. One process-wide SAM STREAM session

Simple and resource-efficient, but every configured IRC Network shares one I2P Destination.

Rejected as the default because it creates avoidable cross-Network linkability and the existing provider interface cannot express another policy.

### B. One SAM STREAM session per Network

Each durable Network owns an independent SAM identity/session. IRC reconnects reuse that session. Session recreation occurs only when the SAM/router session itself is lost or the Network is stopped/replaced.

Selected.

### C. Configurable identity groups

Allows several Networks to deliberately share one SAM Destination.

Potentially useful later, but creates a new durable identity-group concept and operator policy surface.

Deferred.

### D. Depend immediately on an external Rust SAM library

Reduces code ownership, but current candidates expose broader APIs and do not by themselves freeze this project's bounds, deadlines, error taxonomy and identity policy.

Rejected for R001. External implementations remain interoperability/conformance inputs.

### E. Wait for the project's standalone SAM library

Would avoid duplicate protocol code but unnecessarily blocks the first real router adapter.

Rejected. R001 owns a deliberately narrow client that can later be replaced behind the same provider contract.

## Decision

### Provider scope

Change the router-neutral provider request so connection authority includes the durable Network scope:

~~~rust
async fn connect(
    &self,
    network: NetworkId,
    endpoint: &I2pEndpoint,
) -> Result<Box<dyn ByteStream>, ProviderError>;
~~~

The `NetworkId` is provider scope, not a remote wire identifier. It must not be serialized into SAM commands, session IDs, logs or remote-visible metadata.

Fake providers and future managed-i2pr providers receive the same scope so tests and adapters preserve identical semantics.

### SAM session ownership

The standalone SAM adapter owns at most one live SAM 3.1 STREAM session per active durable Network.

For a given Network:

- the SAM session is long-lived;
- IRC connection generations reuse it;
- concurrent `connect()` calls are serialized/bounded behind one session owner;
- SAM/router session loss invalidates every stream/session generation tied to it;
- recreation uses a fresh transient Destination in R001;
- deleting/stopping/replacing the Network tears down the session;
- no session is created merely because a Network record exists if its owner is not active.

The total live SAM session count is therefore bounded by the existing supervised-Network ceiling.

### Destination persistence

R001 uses transient SAM Destinations.

Private destination material is not persisted in the bouncer database in R001.

A later requirement for identity continuity across bouncer/router restarts requires a new ADR covering secret storage, migration, export/import behavior and operational implications.

### Owned protocol profile

R001 implements only the SAM 3.1 subset required by the bouncer:

- loopback/local-router TCP connection;
- `HELLO VERSION MIN=3.1 MAX=3.1`;
- `SESSION CREATE STYLE=STREAM ... DESTINATION=TRANSIENT`;
- `NAMING LOOKUP` only when the adapter must resolve an endpoint before STREAM CONNECT;
- `STREAM CONNECT`;
- bounded status/reply parsing;
- transition to opaque ordered stream bytes.

R001 does not implement:

- ACCEPT;
- FORWARD;
- DATAGRAM/RAW;
- PRIMARY/subsessions;
- arbitrary SAM command execution;
- arbitrary tunnel/session option passthrough;
- remote SAM endpoint support;
- persistent Destination import/export.

### Local-router authority

The SAM adapter may use generic TCP only inside its dedicated adapter crate and only to an explicitly validated loopback SAM endpoint.

No generic upstream host resolver is added.

The adapter never falls back from SAM/I2P to clearnet IRC.

## Consequences

Positive:

- unrelated IRC Networks do not share an I2P Destination by default;
- SAM lifecycle remains independent of IRC connection-generation lifecycle;
- the bouncer owns all parsing/resource/privacy properties it claims;
- later standalone-library adoption is a backend replacement rather than a core redesign;
- no new durable secret is introduced.

Costs:

- one active Network may imply one I2P tunnel pool;
- a process with many active Networks can therefore be resource-heavy even though it remains bounded;
- the small SAM client is code this repository must maintain until it is replaced;
- router/session restart changes the transient I2P Destination.

## Compatibility and migration

The provider signature change is an internal source-level migration.

All fake/test providers and runtime call sites must be updated atomically.

No SQLite schema migration is required.

No IRC wire behavior changes.

The future standalone SAM library may replace the owned protocol implementation only if it can preserve this ADR's provider/session/identity/error/boundary semantics.

## Security and reliability implications

- `NetworkId` must never be exposed to the router as a stable SAM session nickname without an opaque derivation.
- SAM session IDs must be bounded opaque process-local values.
- private Destination replies, complete remote Destinations and authentication material are redacted.
- reply lines are length/token/value bounded before allocation growth.
- command phases have explicit deadlines.
- dropping a pending provider future must cancel or detach it without spawning unowned work.
- session-loss and stream-connect failure are distinguishable enough for the existing reconnect policy to classify them.
- no failed SAM operation can trigger a clearnet fallback.

## Verification

R001 must prove:

- separate Networks use separate SAM sessions/identities;
- repeated IRC reconnects for one Network reuse its live SAM session;
- SAM session loss forces bounded recreation rather than per-IRC-connect session churn;
- live session count never exceeds active supervised Networks;
- fake provider tests still preserve all core semantics after the provider signature change;
- local-router endpoint validation rejects non-loopback hosts;
- static network-boundary checks allow generic TCP only in the dedicated SAM adapter;
- no NetworkId/private Destination/endpoint secret appears in diagnostics or remote-visible SAM metadata.

## Supersession

None.
