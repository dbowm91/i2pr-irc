# Network capability ownership

`I2pEndpoint` has closed kinds for human-readable `.i2p` hostnames, standard `.b32.i2p` names, extended b32 names, and encoded Destinations. It validates/canonicalizes syntax only; it does not resolve names. Standard b32 is 52 Base32 characters; extended b32 is 56–63 characters before the suffix; encoded Destination input is bounded to the documented 516-character form. Endpoint Debug output is redacted.

Upstream bytes can be acquired only through `I2pStreamProvider<I2pEndpoint>`. `LocalAcceptor` is separate and cannot be used as an upstream connector. The workspace includes no SAM implementation, system resolver, generic socket connector, HTTP client, or router administration API. The network guard scans core/wire source including build scripts, crate manifests, and their normal/build dependency trees. Positive controls exercise all three guard checks.

I2P naming background: [I2P Naming and Address Book](https://www.i2p.net/en/docs/overview/naming/). Name validation never grants permission to perform resolution from core.
