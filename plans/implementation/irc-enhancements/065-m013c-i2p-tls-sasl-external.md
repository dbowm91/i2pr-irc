# Plan 065 — M013-C Optional TLS-over-I2P and SASL EXTERNAL

Status: closed with research deferral after bounded preflight; TLS/EXTERNAL implementation remains unshipped and unsupported.
Date: 2026-10-09
Authority: Research 011, ADR-0009 and ADR-0001.

## Objective
Add optional authenticated TLS layered **inside an existing I2P stream**, then optional certificate-backed SASL EXTERNAL. The non-TLS IRC2P NickServ and ILITA SASL PLAIN paths remain primary.

## Prerequisite research
Compare actively maintained Rust TLS implementations for Rust 1.88 compatibility, memory impact on SBCs, TLS peer identity/pinning, supported client certificates, secret sourcing, license/security maintenance and ability to wrap an I2pStreamProvider stream. Confirm a controlled test endpoint can demonstrate TLS client-certificate exchange and SASL negotiation. If this cannot be established, record a named deferral; do not expose a partially working setting.

## Preflight result and disposition (2026-10-10)

Current primary crate metadata and APIs were reviewed: `rustls` 0.23.45 docs and current upstream 0.24 development metadata; `tokio-rustls` 0.26.6 metadata/API. The current tokio-rustls release declares Rust 1.71 MSRV and depends on rustls 0.23.27 or newer, within this workspace's Rust 1.88 MSRV. Tokio's TLS stream wraps a generic async read/write stream; the repository's provider returns `Box<dyn ByteStream>`, where `ByteStream` is `AsyncRead + AsyncWrite + Unpin + Send`, so the adapter shape is technically feasible. Rustls supports configured roots and client certificates; a custom verifier uses explicitly dangerous APIs and would require careful signature verification, so it is not an acceptable shortcut for endpoint pinning.

DEFER implementation. The repository has no explicit per-Network TLS trust anchor/server identity field, client-certificate/key source, lifecycle/zeroization contract, or controlled TLS+SASL EXTERNAL endpoint. No authorized endpoint or credentials were supplied, and fixture-only TLS cannot establish the plan's required product-path interoperability. Implementing an exposed profile before the identity and provisioning contracts exist would create a setting with no verifiable safe configuration. Existing typed `tls-over-i2p` and `sasl-external` values remain rejected before dialing. Reopen with a new bounded implementation plan once an operator-controlled I2P test service, certificate authority/pinning contract and per-Network key provisioning path are available.

Reviewed sources: [rustls current client configuration](https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html), [rustls verifier API](https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html), [tokio-rustls API](https://docs.rs/tokio-rustls/latest/tokio_rustls/), [rustls upstream package metadata](https://raw.githubusercontent.com/rustls/rustls/main/rustls/Cargo.toml), and [tokio-rustls upstream package metadata](https://raw.githubusercontent.com/rustls/tokio-rustls/main/Cargo.toml).

## Ordered implementation
1. Add explicit transport enum `plain-i2p` (default) vs `tls-over-i2p`. Both obtain their stream from the typed I2P provider. No DNS, generic TCP, SOCKS or HTTP fallback. Do not infer TLS from port or destination name.
2. Use authenticated TLS server identity, with explicit scoped certificate/SPKI pin or rigorously validated service identity. Refuse unverified peers and never provide an insecure-verification switch.
3. Introduce separate per-Network credential source for a client certificate and private key, not SQLCipher StoreKey, local Operator secret, NickServ password or OTR identity. No key/cert in config export, logs, BouncerServ response or IRC message.
4. Present the client certificate only within the inner TLS handshake to a trusted I2P endpoint, and negotiate SASL EXTERNAL only when authentication configuration explicitly requires it and the upstream advertises a usable mechanism. The EXTERNAL IRC command alone does not authenticate a certificate.
5. If TLS/auth fails, fail closed; never switch to plain IRC, SASL PLAIN or a different I2P endpoint without separate explicit configuration.
6. A clearnet certificate override **does not exist** in this I2P-only product. Supporting one would first require changing canonical product direction and an independent security ADR and connector design; this plan neither implements nor permits that.

## Testing and failure semantics
Exercise successful and failing mutual TLS with a controlled I2P stream fixture, incorrect server pin, missing client certificate, wrong mechanism, aborted handshake, reconnect, mismatched per-Network certificates and Rust 1.88 builds. Assert certificate bytes cannot appear in IRC frames, config exports or logs. Retain static proof of no clearnet path with positive guard controls. Time and memory bounds apply to TLS handshakes; canceled generation cleans up private material.

## Closure
The research deferral is recorded in `plans/closure/irc-enhancements/065-status.md`; do not advertise TLS/EXTERNAL support. Any insecure verification or unauthorized egress requires a corrective rather than weakening the boundary.
