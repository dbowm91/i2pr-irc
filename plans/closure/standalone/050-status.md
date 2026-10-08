# Standalone M010-A / Plan 050 Closure

Status: closed
Implementation commits: `f4eb3a0` (`feat(daemon): add standalone runtime bootstrap`), `2d02012df6d3def8819421242490c8747e029144` (`fix(daemon): refuse missing or unsafe store paths`)
Closure baseline: `f325e7d5e495e36b1fc7b168c30e4b725756d22c`

## Requirement-to-evidence

| Requirement | Evidence | Result |
|---|---|---|
| Dedicated `i2pr-irc` executable and bounded explicit config | `crates/daemon/src/main.rs`; strict version/key/size/value checks; help/version and explicit unsupported command errors | Pass |
| Numeric loopback SAM config and listener not activated | `SamBridgeEndpoint::parse`; listener parsed as numeric `SocketAddr`, loopback only, and refused at run | Pass |
| Exclusive state ownership before Store access | `StateLease` uses `fs2` nonblocking exclusive locking; symlink and writable-path checks; lease contention/release tests | Pass |
| Open existing Store with explicit policy | Run rejects absent, symlink, and non-file Store paths; only pre-provisioned plaintext mode is supported; no key material is read | Pass |
| Compose Store, SamProvider, RuntimeController and restore | Store is opened before controller construction; the unique `serve()` task is supervised; `serve()` remains the restore authority | Pass by implementation; existing RuntimeController restore qualification applies |
| Deterministic stop and lock lifetime | SIGINT/SIGTERM or internal stop requests controller stop; join has a 30 second bound; Store shutdown precedes lease release | Pass |
| Secret-free diagnostics and no upstream identity leakage | Config/parser errors are fixed classifications; no auth or store secrets exist in this phase; no listener/upstream IRC field construction | Pass |
| Boundary scan covers daemon | `scripts/check-network-boundary.py` includes daemon source, manifest and dependency tree; scan and positive controls pass | Pass |
| Rust 1.88 compatibility and dependencies | `architecture/dependency-review.md`; clippy MSRV lint passes on workspace compiler | Pass |

## Commands executed

- `rtk cargo fmt --all` — passed
- `rtk cargo check -p i2pr-irc-daemon --locked` — passed
- `rtk cargo test -p i2pr-irc-daemon --locked` — 3 passed
- `rtk cargo clippy -p i2pr-irc-daemon --all-targets --all-features --locked -- -D warnings` — passed
- `rtk python3 scripts/check-network-boundary.py` — passed, including embedded negative positive-controls

## Security and recovery review

The daemon has no listening or generic network authority. SAM configuration is represented by the SAM crate's loopback-only endpoint type. A missing or unsafe database path fails before SQLite can create a replacement. The advisory lock file is not a PID authority; the kernel lock is held for the state lease lifetime. Store shutdown precedes normal lock release. If runtime join exceeds 30 seconds, the process returns failure and retains ownership until process termination; no clean shutdown is claimed for that timeout.

The lease checks the state directory itself for symlink and world-write, and checks the lock file for symlink and group/world write. Full parent-chain ownership and Windows ACL qualification are not claimed. They are revisited by secure initialization (Plan 053). There is no child-process test of cross-process flock contention yet; the same-file-description contention test and fs2's OS lock are the evidence for this phase.

## Limitations and handoff

Plan 050 supplies infrastructure only. It does not activate a listener, authenticate Operators, provision SQLCipher keys, or make a standalone client connection possible. The runtime uses an explicit existing plaintext test store; encrypted configuration fails closed until Plan 053. Help identifies the non-listening status. No full workspace/MSRV suite was run at this closure point.

Plan 051 is promoted to `ready`: it can build its local listener/auth substrate on the daemon-owned process lease, strict config, supervised runtime, and shutdown boundary. Its CAP/SASL handoff remains private until Plan 052.
