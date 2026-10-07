# Dependency review

Rust 1.88 / edition 2024 is the workspace floor. Production dependencies are Tokio (async runtime, synchronization and byte I/O), async-trait (provider object contract), thiserror (typed errors), base64 (SASL PLAIN wire encoding), zeroize (credential drop cleanup), and rusqlite (the M003 storage boundary). Wire has no dependencies. `rusqlite 0.40.2` is declared `default-features = false, features = ["bundled"]`, which pins the SQLite build so supported binary behavior does not depend on a host library; the linked test binary shows no system SQLite linkage. Its transitive tree is `hashlink` (removed; unused in 0.40) plus `bitflags`, `fallible-iterator`, `smallvec`, `libsqlite3-sys`, and the `cc`/`pkg-config`/`vcpkg` build-time crates. `libsqlite3-sys` build scripts run locally only and perform no network access. `tokio-rusqlite` was rejected in ADR-0002 because its worker uses an unbounded crossbeam request channel; the reviewed store instead owns a bounded Tokio MPSC queue. Core uses async-trait and Tokio byte-I/O traits; its monotonic clock/timer contract is implemented independently with standard-library futures/wakers. No IRC client/codec, network client, resolver, or storage dependency is present. `cargo tree --workspace` is reviewed, and `scripts/check-network-boundary.py` checks the `core`, `wire`, `store`, `runtime`, and `testkit` normal/build/dev dependency trees with positive controls. The store is included because it introduces a third-party native dependency, and its positive control proves the store's source and manifest scope independently.


## R001-B: the SAM adapter crate

`crates/sam` adds the workspace's only socket authority and its only OS-random dependency.
Both were reviewed before being added.

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
