# ADR-0005 — Explicit I2P Provider Scope Release

Status: accepted

Date: 2026-10-07

Related:

- `plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md`
- `plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md`

## Context

ADR-0004 scopes provider connections by durable `NetworkId` so a SAM backend can own one long-lived I2P identity/session per configured Network.

A scoped `connect(network, endpoint)` call is not enough to manage that lifetime. A SAM control socket may intentionally remain alive while the IRC Network is disconnected/backing off. Therefore the adapter cannot infer durable Network deletion from the absence of active IRC streams, and an idle timeout would either leak sessions or destroy the long-lived-session property.

The process-level `RuntimeController` already owns durable Network creation/change/delete and process shutdown. It is the correct component to signal when a provider scope is no longer part of the runtime.

## Decision

Extend the router-neutral provider contract with explicit scope release:

~~~rust
#[async_trait]
pub trait I2pStreamProvider: Send + Sync {
    async fn connect(
        &self,
        network: NetworkId,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, ProviderError>;

    async fn release(&self, network: NetworkId) -> Result<(), ProviderError>;
}
~~~

Semantics:

- `connect` may lazily create provider state for the Network.
- `release` ends provider-owned state for that Network and is idempotent.
- after `release` completes, no provider task, control socket, queued operation or identity for that Network remains live;
- a later `connect` for the same durable Network may create a new provider scope;
- `release` is not called merely because one IRC connection generation ended;
- changing IRC configuration while preserving the same durable Network may preserve the provider scope unless the change explicitly requires provider replacement;
- durable Network deletion calls `release` after the owner is stopped and before the deletion is considered fully quiesced;
- process shutdown stops owners and releases all live provider scopes before returning;
- release failure is bounded and reported, but shutdown/deletion must not hang forever.

Fake/test providers implement the same lifecycle and expose counters so cleanup is qualification evidence.

## Considered alternatives

### Infer lifecycle from open data streams

Rejected. A SAM session should outlive an IRC stream generation.

### Idle timeout

Rejected. It makes identity/tunnel lifetime depend on IRC outage duration and can churn sessions during a long backoff.

### Provider-specific cleanup outside the trait

Rejected. It would make RuntimeController aware of SAM and break adapter replaceability.

### RAII per-Network provider object

Architecturally clean but a larger rewrite of the existing controller/provider API than R001 requires.

Deferred as a possible future API if more router-scoped capabilities are added.

## Consequences

- the provider trait becomes a small lifecycle interface rather than connect-only;
- fake providers and future i2pr managed-app adapters must implement explicit release;
- RuntimeController becomes responsible for calling release at durable scope boundaries;
- long-lived SAM sessions can be retained through IRC reconnects without leaking after deletion.

## Security and reliability implications

- provider cleanup is tied to durable identity, not remote endpoint text;
- deleting a Network destroys its transient SAM identity and tunnel pool;
- a stale release must not destroy a newly recreated scope for another Network;
- release does not expose private Destination material or session IDs;
- release has a bounded deadline and cannot deadlock process shutdown.

## Verification

R001-A must prove:

- IRC generation loss does not call release;
- durable delete calls release exactly once logically;
- repeated release is safe;
- process shutdown releases every live scope;
- no scope/task remains after successful release;
- configuration change preserving NetworkId does not accidentally duplicate a scope;
- fake providers make lifecycle counts observable without carrying endpoint/private material.

## Supersession

None.
