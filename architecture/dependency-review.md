# Dependency review

Rust 1.88 / edition 2024 is the workspace floor. Production dependencies are Tokio (async runtime, synchronization and byte I/O), async-trait (provider object contract), thiserror (typed errors), base64 (SASL PLAIN wire encoding), zeroize (credential and store-key cleanup), and rusqlite (the M003 storage boundary). Wire has no dependencies. `rusqlite 0.40.2` uses `default-features = false` with `bundled-sqlcipher-vendored-openssl`: `libsqlite3-sys 0.38.2` bundles SQLCipher 4.14.0 Community Edition (SQLite 3.51.3, FTS5 enabled) and compiles OpenSSL 3.6.3 from source. This keeps runtime behavior independent of host SQLite/OpenSSL installations, at the cost of a larger native build and binary. SQLCipher CE is BSD-3-Clause; OpenSSL 3 is Apache-2.0; rusqlite/libsqlite3-sys are MIT. The normal dependency tree adds `openssl-sys 0.9.117`; vendored OpenSSL build dependencies remain build-time only and the build does not fetch source at runtime. `cargo tree --workspace` is reviewed, and `scripts/check-network-boundary.py` checks the `core`, `wire`, `store`, `runtime`, and `testkit` normal/build/dev dependency trees with positive controls. The store is included because it introduces a third-party native dependency, and its positive control proves the store's source and manifest scope independently.


## R001-B: the SAM adapter crate

`crates/sam` adds the workspace's only socket authority and its only OS-random dependency.
Both were reviewed before being added.

### Standalone daemon process lease

Plan 050 adds `fs2` 0.4 for nonblocking advisory exclusive state-directory ownership. Its
MSRV is below Rust 1.88 and it uses the platform's file-lock API without an unsafe
first-party boundary. The lock is held by an open file descriptor for the complete
Store/Runtime lifetime; stale file contents are never treated as ownership. The daemon
uses Tokio's existing `net` feature only in the already-authorized SAM crate; daemon
signal handling adds only Tokio's `signal` feature. No listener or generic socket
authority is introduced by this milestone.

### `tokio` `net` feature

The SAM crate enables `tokio`'s `net` feature. The workspace `tokio` already carried
`rt-multi-thread`, `macros`, `sync`, `time`, and `io-util`, so this adds one feature to an
existing dependency rather than a new crate — no new version, no new build surface, no new
transitive dependency.

Source-level authority remains path-confined: enabling the feature makes the symbols
*available*, and `scripts/check-network-boundary.py` is what makes them *unreachable* outside
`crates/sam/src/client.rs` and `crates/sam/src/fake.rs`. The feature is not the boundary; the
scan is. Eight positive controls prove the scan fails when the confinement is narrowed.

`crates/sam` is scanned for the same generic predicates as every other first-party crate —
generic TCP, DNS resolution, HTTP clients, SOCKS/proxy — and its dependency tree is scanned
for the same manifest and transitive names.

### `getrandom` 0.3

The one OS-random dependency in the workspace, and it exists for exactly one reason: a SAM
session ID must carry at least 128 bits of OS randomness and must never be derived from
anything identifying.

| | |
|---|---|
| License | MIT OR Apache-2.0 — matches every other dependency here |
| `rust-version` | 1.63, below this workspace's 1.88 floor |
| Features used | none beyond default |
| Transitive dependencies | none on Linux, macOS, or Windows |

Every backend is `cfg`-gated, and the platforms this workspace builds for resolve to libc
calls or syscalls they already have. Randomness is reached only through a `RandomSource`
trait, so the dependency is one implementation behind an injectable interface rather than
something protocol tests have to route around.

The licence and MSRV were read from the vendored source before the dependency was added:
`~/.cargo/registry/src/*/getrandom-0.3.4/Cargo.toml` and its `LICENSE-APACHE`.

### What this crate does not add

No DNS resolver, no HTTP client, no proxy, no unix socket, and no router administration API.
`SamBridgeEndpoint` can only be constructed from a loopback literal, so non-loopback authority
is unrepresentable rather than merely unused.
