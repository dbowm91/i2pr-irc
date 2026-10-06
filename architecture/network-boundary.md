# Network capability ownership

`I2pEndpoint` has closed kinds for human-readable `.i2p` hostnames, standard `.b32.i2p` names, extended b32 names, and encoded Destinations. It validates/canonicalizes syntax only; it does not resolve names. Standard b32 is 52 Base32 characters; extended b32 is 56–63 characters before the suffix; encoded Destination input is bounded to the documented 516-character form. Endpoint Debug output is redacted.

Upstream bytes can be acquired only through `I2pStreamProvider<I2pEndpoint>`. `LocalAcceptor` is separate and cannot be used as an upstream connector. The workspace includes no SAM implementation, system resolver, generic socket connector, HTTP client, or router administration API.

The network guard scans source, build scripts, and crate manifests for every first-party crate that could own network access — `core`, `wire`, `store`, `runtime`, and `testkit` — plus each crate's normal, build, and dev dependency tree. The store is scanned because its SQLite dependency tree is third-party native code. Covering the runtime matters because it owns the upstream connection and the downstream client sockets. Positive controls exercise the same predicates and the same crate scoping against a synthetic fixture tree, including a dedicated store fixture, so a future coverage regression fails the guard instead of silently narrowing the boundary.

A future router or SAM adapter inherits this boundary: it must be a separate crate that consumes managed-app capabilities, it may not add generic host DNS or generic upstream TCP, and adding it requires architecture review.

I2P naming background: [I2P Naming and Address Book](https://www.i2p.net/en/docs/overview/naming/). Name validation never grants permission to perform resolution from core.
