# i2pr-irc

i2pr-irc is a Rust IRC bouncer for I2P, with a Rust library core and a process bootstrap.

The product boundary is deliberately narrow: upstream IRC traffic is I2P-only. The bouncer does not provide a clearnet IRC connector, system-DNS fallback, generic proxying, DCC, or an HTTP side channel. Its first priority is correct IRC/IRCv3 bouncer behavior under unstable, high-latency stream conditions. Router integration follows through explicit I2P stream-provider interfaces.

Target deployment forms are a standalone local bouncer using a local router through SAM, portable SAM operation against compatible routers, and first-party managed-application integration with i2pr once its public app API is stable. Proposal 170 integration is optional control-plane work, not a data-plane dependency.

Planning authority begins at:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md
- plans/registry.md

## Implementation state

The bouncer core is closed through M009, with router integration closed at R001 for this repository. M001/M002 provide the bounded wire/domain/time/fault foundations and the one-network runtime; M003 is the durable multi-Network, multi-client core with transactional SQLite storage (schema version 8), bounded history with private per-client cursors, SessionId-scoped labeled-response routing, and bounded chathistory/read-marker adapters; M004 qualifies anonymity mediation and adverse-network behavior; M005 adds the bounded process `RuntimeController` with pre-bind downstream admission, durable detach/reattach and presence/nick policy, indexed history search, `soju.im/bouncer-networks` mediation, and a local `BouncerServ` administration service. See `plans/closure/bouncer-core/028-status.md` and the earlier milestone records under `plans/closure/bouncer-core/`.

Research lives under plans/research/. Subsystem roadmaps live under plans/subsystems/. Bounded implementation handoffs live under plans/implementation/, and evidence-based completion records live under plans/closure/. Follow `plans/registry.md` for current status. See [architecture/overview.md](architecture/overview.md).

### Implemented core/runtime

- Durable multi-Network, multi-client bouncer engine with one live owner per Network, bounded queues/timers/collections, and no replay of non-idempotent commands across ambiguous disconnects.
- IRC/IRCv3 compatibility across modern, partial, and legacy no-CAP/no-SASL servers (M006): explicit no-CAP registration, fail-closed configured SASL, and mediated account-tag/invite-notify without synthesized state.
- Reconnect, preferred-nick reclaim, phased service actions, and multi-client identity resilience under degraded connectivity, qualified with external Eggchaos campaigns (M007).
- SAM provider for standalone-router integration (R001, closed for this repository): an owned SAM 3.1 client behind `I2pStreamProvider` giving each Network one long-lived transient router-side identity over a loopback bridge only. Corrective 033 qualified exact bidirectional application bytes against i2pd 2.61.0 through a real inbound `STREAM ACCEPT`; broad multi-router SAM conformance is delegated to the dedicated SAM library project. See `plans/closure/router-integration/033-status.md` and `architecture/sam-adapter.md`.
- Optional whole-database SQLCipher encrypted Store under one injected process-level key, with source-preserving migration and rotation (M008).
- OTRv3-transparent opaque transport (M009): the bouncer carries OTR query/AKE/data/fragment payloads byte-exactly, holds no OTR keys/session state/plaintext, keeps OTR-bearing chat non-replayable, and retains ciphertext only.

### Standalone daemon bootstrap status

- `cargo run -p i2pr-irc-daemon -- --help` exposes the bootstrap CLI. Plan 050 adds an executable that takes an exclusive state lease, opens an explicitly configured existing plaintext store, restores RuntimeController ownership and stops on SIGINT/SIGTERM.
- The listener/authentication and canonical registration handoff are implemented, including stable per-profile ClientIds and unbound control sessions. The CLI does not activate them until Plan 053 provisions private credentials and the encrypted store key.
- This bootstrap is for a deliberately pre-provisioned plaintext test store only. Secure encrypted initialization and key provisioning are not available until Plan 053. It is not yet a usable standalone IRC bouncer.
- No production store-key provisioning UX (environment/file/keyring/HSM).
- No packaging, service, or install layer.
- No real-client OTR interoperability qualification through a product listener; that waits on the future production listener.

The standalone M010 productization milestone is in progress (Research 010, ADR-0007, Plans 050-054). Plans 050-052 establish the process, authentication and runtime handoff. The executable remains non-listening until secure credential and store-key provisioning is complete. Track sequential handoffs in `plans/registry.md`.

Router R002 (i2pr managed-app adapter) remains blocked on stable public i2pr managed-app stream/listener/lifecycle contracts.

## Conformance corpus

`research/irc-conformance/` contains an independently authored IRC/IRCv3 conformance corpus derived from primary specifications, plus a committed runner per owned layer. `cargo test -p i2pr-irc-wire --test conformance` and `cargo test -p i2pr-irc-runtime --test conformance` execute it on every test run. External comparisons and per-candidate dispositions live in `research/irc-conformance/results/` and `plans/research/003-rust-irc-crate-conformance-results.md`.
