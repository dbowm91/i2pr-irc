# Network capability ownership

`I2pEndpoint` has closed kinds for human-readable `.i2p` hostnames, standard `.b32.i2p` names, extended b32 names, and encoded Destinations. It validates/canonicalizes syntax only; it does not resolve names. Standard b32 is 52 Base32 characters; extended b32 is 56–63 characters before the suffix. Endpoint Debug output is redacted for every form, including a Destination, which is key material.

Every form shares one application ceiling of 4096 textual bytes, and each form keeps its own tighter bounds inside it. Hostnames are capped at 67 characters with labels of at most 63. A raw Base64 Destination is 516–4096 characters, may carry at most two `=` pad characters and only at the end, and is not required to be block-aligned unless it is padded. The Destination floor is the shortest real Destination; the ceiling is the same 4096 as any other endpoint, so no second, tighter number exists for a router adapter to have to know.

A Destination's shape is decided here and its reachability is not. Whether the bytes name a service that answers is a router question, so an unreachable Destination is a well-formed endpoint that fails at connect time like any other unreachable target, rather than a configuration error the Operator is asked to fix. The accepted alphabet is the union of standard Base64 (`A-Z a-z 0-9 + /`) and the base64url variant (`- ~`), because real Destinations use `+` and `/` and accepting only the narrower set would reject every Destination a real router can produce. Destination validity is decided by the router adapter, never by the bouncer.

Upstream bytes can be acquired only through `I2pStreamProvider`. `LocalAcceptor` is separate and cannot be used as an upstream connector. The workspace includes no system resolver, generic socket connector, HTTP client, or router administration API.

As of R001-B there is exactly one SAM implementation: `crates/sam`, the only crate permitted to open a socket, and only to a loopback bridge. See [the owned SAM 3.1 adapter](sam-adapter.md).

## Provider scope and release

Every provider call carries the durable `NetworkId` it belongs to:

~~~rust
async fn connect(&self, network: NetworkId, endpoint: &I2pEndpoint)
    -> Result<Box<dyn ByteStream>, ProviderError>;
async fn release(&self, network: NetworkId) -> Result<(), ProviderError>;
~~~

Scope is what makes "these two Networks do not share a router resource" a checkable claim. One provider instance serves every configured Network, so an unscoped call would leave an adapter unable to tell which Network's session an attempt belonged to, and unable to release only the one it was asked for.

`NetworkId` is local scope and nothing more. It is never written to a remote-visible field, never sent to a router, and never allowed to influence a Destination, nickname, or any other IRC-visible value.

Release is explicit because a router scope outlives any single IRC connection. A SAM control socket may legitimately stay open while its IRC Network is disconnected or backing off, so an adapter cannot infer deletion from the absence of active streams, and an idle timeout would either leak the scope or destroy the long-lived-session property that scoping exists to provide. `RuntimeController` calls `release` because it is the only component that knows when a durable Network has gone:

- ending an IRC connection generation never releases;
- changing configuration while preserving the `NetworkId` never releases, because the router identity belongs to the Network rather than to a generation;
- deleting a Network releases **after** the owner task has joined and **before** the durable row is forgotten, so a failed release leaves a Network that is configured but stopped and can be retried, and no release can race a connect still inside the provider;
- shutdown releases every configured scope, including those with no live owner, and one failure does not prevent the remaining attempts.

Releasing an unknown or already-released Network is a no-op, which is what makes a retry after a timeout converge rather than wedging a Network permanently undeletable. Release is bounded by `PROVIDER_RELEASE_TIMEOUT`, deliberately far below the connect budget because it runs where a caller is already blocked and shutdown has no timeout of its own.

The network guard scans source, build scripts, and crate manifests for every first-party crate that could own network access — `core`, `wire`, `store`, `runtime`, `sam`, and `testkit` — plus each crate's normal, build, and dev dependency tree. The store is scanned because its SQLite dependency tree is third-party native code. Covering the runtime matters because it owns the upstream connection and the downstream client sockets. Positive controls exercise the same predicates and the same crate scoping against a synthetic fixture tree, including a dedicated store fixture, so a future coverage regression fails the guard instead of silently narrowing the boundary.

A future router or SAM adapter inherits this boundary: it must be a separate crate that consumes managed-app capabilities, it may not add generic host DNS or generic upstream TCP, and adding it requires architecture review.

I2P naming background: [I2P Naming and Address Book](https://www.i2p.net/en/docs/overview/naming/). Name validation never grants permission to perform resolution from core.
