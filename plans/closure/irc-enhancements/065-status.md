# Plan 065 — M013-C TLS-over-I2P and SASL EXTERNAL

Status: closed with research deferral; no TLS-over-I2P or SASL EXTERNAL implementation shipped
Implementation commit: none
Closure commit: `a0327dc` — `docs(plans): close M013 compatibility line`
Date: 2026-10-10

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Compare current Rust TLS stack against workspace MSRV | Reviewed current `rustls` 0.23.45 API docs, `tokio-rustls` 0.26.6 docs/metadata, and upstream package manifests. tokio-rustls declares Rust 1.71 and depends on rustls 0.23.27+, compatible with workspace Rust 1.88. |
| Determine whether TLS can wrap the I2P provider stream | `crates/core/src/lib.rs` defines `ByteStream` as `AsyncRead + AsyncWrite + Unpin + Send`, and `I2pStreamProvider::connect` returns `Box<dyn ByteStream>`. tokio-rustls exposes TLS streams over generic async read/write IO, so an adapter is technically feasible while keeping I2P as the only transport. No adapter was implemented. |
| Require authenticated peer identity and client certificate scoping | Rustls supports explicit trust roots and client certificates. Its custom-verifier API is explicitly dangerous and requires correct chain/identity and handshake signature validation. The repository has no per-Network trust identity/pin, client certificate/key provisioning, storage lifecycle, or zeroization contract. These are design prerequisites, not safe defaults to guess. |
| Demonstrate mTLS and SASL EXTERNAL on a controlled I2P product path | NOT RUN: no authorized controlled endpoint, server certificate identity, client certificate, or credentials were available. Fixture-only transport testing cannot meet the plan's product-path interoperability requirement. |
| Preserve fail-closed and network boundary behavior | Existing typed `tls-over-i2p` and `sasl-external` profiles remain rejected before owner construction/dial under Plan 063. No TLS-to-plain fallback, clearnet path, DNS, or generic transport was added. |

## Security, recovery, and limits

Implementation is deferred until an operator-controlled I2P test service and an explicit per-Network server identity/trust-anchor and client-key provisioning contract exist. The library comparison establishes feasibility of an adapter only; it does not establish safe identity policy, SBC memory suitability, deployed compatibility, or mTLS interoperability. No security verifier bypass is acceptable. TLS and SASL EXTERNAL remain unsupported.

## Registry and roadmap disposition

Plan 065 is closed with a research deferral. Plan 066 is unblocked to record the completed M013 implementation and deferral matrix without claiming TLS/EXTERNAL support. Reopen this work as a new plan when the named endpoint and credential/trust prerequisites are available. Plan 054 and Plan 062's independent evidence conditions remain unchanged.
