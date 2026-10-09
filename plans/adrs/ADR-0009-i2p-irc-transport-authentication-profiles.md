# ADR-0009 — I2P IRC Authentication Profiles and Optional Inner TLS
Status: accepted for planning (2026-10-09); conditional TLS/EXTERNAL implementation remains gated.
Authority: ADR-0001, ADR-0004/0005, ADR-0006/0007, Research 011.
Supersedes: none.

## Problem
IRC2P commonly requires NickServ-style identification without SASL; ILITA is expected to accept SASL PLAIN; both normally use IRC directly over encrypted I2P streams without TLS. Future in-network services may provide TLS and client-certificate authentication. A generic `tls=true` option coupled to a generic TCP connector is unacceptable.

## Decision
1. All upstream connections remain through a typed I2pEndpoint, I2pStreamProvider and (for standalone) loopback-only SAM. No DNS, public TCP, SOCKS, outproxy, HTTP or clearnet connector. The no-clearnet guard continues to fail if these are introduced. Inner TLS, where enabled, wraps the already-established I2P byte stream.
2. Treat **transport** (`plain-i2p` default, optional `tls-over-i2p`) and **authentication** (`none`, `nickserv`, `sasl-plain`, optional `sasl-external`) as independent typed per-Network configuration. Reject invalid combinations before any credential reaches I/O. Do not use an unauthenticated or cross-Network fallback if a configured authentication method fails.
3. IRC2P suggested profile: `plain-i2p + nickserv`, with configurable constrained service steps and no required SASL. ILITA suggested profile: `plain-i2p + sasl-plain` when an operator configures credentials and the server actually negotiates PLAIN. Do not invent a SASL credential or silently convert a configured requirement into non-SASL login.
4. Never infer real network capabilities from names. Provide illustrative profiles, not silently enabled hardcoded endpoints, credentials, autojoin or service commands. Live authorized CAP/registration evidence is required to claim a real-network matrix.
5. EXTERNAL is only eligible with explicitly authorized TLS-over-I2P and an actual client certificate/private key presented to an authenticated in-network TLS peer, followed by an upstream `sasl` offer allowing EXTERNAL. `AUTHENTICATE EXTERNAL` alone is not authentication. A cert is NEVER emitted as IRC payload, included in diagnostic/export, or reused across unrelated NetworkIds implicitly. Cert key material is separate from SQLCipher StoreKey, Operator token, SASL PLAIN and OTR keys.
6. TLS server identity verification MUST succeed (explicit trust/pin or rigorously verified identity); unverified/hostname-check-disabled TLS and plaintext downgrades are forbidden. Cert key read errors, expired pins, mechanism NAK, TLS alerts and unknown peer identity are typed redacted failures. No automatic retry with PLAIN or plain IRC from EXTERNAL.
7. A clearnet-directed client-cert override is **not implemented or authorized**. Even a hypothetical user setting does not add clearnet authority under ADR-0001. If a future product explicitly wishes to support certificate disclosure to a clearnet server, it first requires a separate canonical product-direction change, new ADR, explicit per-endpoint user authorization, and scoped clearnet connector. This planning line neither grants nor implements that escape hatch.
8. TLS and EXTERNAL are independently optional; absence of a live I2P EXTERNAL server does not block baseline IRC2P/ILITA compatibility, but a shipped EXTERNAL feature requires independent deterministic TLS+cert+SASL end-to-end proof and at least an I2P-provider-realistic qualification.

## Rejected alternatives
Clearnet override flag inside the existing I2P-only product; trusting `.i2p` suffix for TLS verification; `sasl-external` on bare IRC; implicitly carrying client cert on SAM; auto-mutating a Network to alternate SASL modes; generic `host:port` fallback.

## Verification
Typed profile matrix, secrets redaction, static no-clearnet checks and positive controls, simulated CAP fragmentation/NAK, cert transmission only within authenticated TLS handshake, wrong host/pin, client-cert absent, reconnect/reset, multiple NetworkId identities, Linux/macOS/MSRV and controlled I2P/SAM product path where accessible.
