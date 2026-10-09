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

The bouncer core is closed through M009, with router integration closed at R001 for this repository. M001/M002 provide the bounded wire/domain/time/fault foundations and the one-network runtime; M003 is the durable multi-Network, multi-client core with transactional SQLite storage, bounded history with private per-client cursors, SessionId-scoped labeled-response routing, and bounded chathistory/read-marker adapters; M004 qualifies anonymity mediation and adverse-network behavior; M005 adds the bounded process `RuntimeController` with pre-bind downstream admission, durable detach/reattach and presence/nick policy, indexed history search, `soju.im/bouncer-networks` mediation, and a local `BouncerServ` administration service. M011 and Plans 055-060 are closed; Plan 060's upstream CHATHISTORY production feature is deferred because the official draft warns against production use. Plan 061 is active for explicitly operator-attested same-Network I2P endpoint failover. See `plans/registry.md` and closure records under `plans/closure/`.

Research lives under plans/research/. Subsystem roadmaps live under plans/subsystems/. Bounded implementation handoffs live under plans/implementation/, and evidence-based completion records live under plans/closure/. Follow `plans/registry.md` for current status. See [architecture/overview.md](architecture/overview.md).

### Implemented core/runtime

- Durable multi-Network, multi-client bouncer engine with one live owner per Network, bounded queues/timers/collections, and no replay of non-idempotent commands across ambiguous disconnects.
- IRC/IRCv3 compatibility across modern, partial, and legacy no-CAP/no-SASL servers (M006): explicit no-CAP registration, fail-closed configured SASL, and mediated account-tag/invite-notify without synthesized state.
- Reconnect, preferred-nick reclaim, phased service actions, and multi-client identity resilience under degraded connectivity, qualified with external Eggchaos campaigns (M007).
- SAM provider for standalone-router integration (R001, closed for this repository): an owned SAM 3.1 client behind `I2pStreamProvider` giving each Network one long-lived transient router-side identity over a loopback bridge only. Corrective 033 qualified exact bidirectional application bytes against i2pd 2.61.0 through a real inbound `STREAM ACCEPT`; broad multi-router SAM conformance is delegated to the dedicated SAM library project. See `plans/closure/router-integration/033-status.md` and `architecture/sam-adapter.md`.
- Optional whole-database SQLCipher encrypted Store under one injected process-level key, with source-preserving migration and rotation (M008).
- OTRv3-transparent opaque transport (M009): the bouncer carries OTR query/AKE/data/fragment payloads byte-exactly, holds no OTR keys/session state/plaintext, keeps OTR-bearing chat non-replayable, and retains ciphertext only.
- Optional same-Network I2P endpoint failover is implemented under Plan 061. It requires explicit local Operator attestations for network equivalence and credential scope; no IRC2P/ILITA deployment equivalence is inferred. Upstream CHATHISTORY recovery remains deferred while its official draft warns against production use.

### Standalone daemon

On supported Unix systems, create a private directory (mode 0700), then initialize and save the one-time Operator token securely:

```sh
mkdir -m 700 "$HOME/.config/i2pr-irc"
cargo run -p i2pr-irc-daemon -- init --config "$HOME/.config/i2pr-irc/daemon.conf"
cargo run -p i2pr-irc-daemon -- --config "$HOME/.config/i2pr-irc/daemon.conf" run
```

Initialization creates an encrypted SQLCipher store by default, an independent random store key, and a random 256-bit Operator token. The token is shown once. Configure an ordinary IRC client for `127.0.0.1:6667`, with password `default:<token>`; profile labels such as `laptop:<token>` create independent durable history identities under the same Operator credential. SASL PLAIN is also supported for local authentication. Use `i2pr-irc status --config <path>` for redacted state details. Plaintext storage requires the explicit `init --config <path> --plaintext` option. Back up the state directory and config together; losing `state/store.key` makes encrypted history unrecoverable. Token rotation/recovery is not provided yet: if the token is lost, retain the data directory and seek an explicit recovery procedure rather than deleting or recreating it. An `.init-incomplete` marker means initialization stopped before completion; preserve the directory for manual recovery. Do not place the token or key in shell history, source control, or routine diagnostics.

The daemon is local-only and upstream IRC remains I2P-only through SAM. Secure init and startup currently support Unix private file modes; other platforms fail explicitly until ACL protection is qualified. Packaging/service installation, keyring/HSM integration, and real-router product-path qualification remain tracked separately in `plans/registry.md` and the standalone roadmap.

Router R002 (i2pr managed-app adapter) remains blocked on stable public i2pr managed-app stream/listener/lifecycle contracts.

## Conformance corpus

`research/irc-conformance/` contains an independently authored IRC/IRCv3 conformance corpus derived from primary specifications, plus a committed runner per owned layer. `cargo test -p i2pr-irc-wire --test conformance` and `cargo test -p i2pr-irc-runtime --test conformance` execute it on every test run. External comparisons and per-candidate dispositions live in `research/irc-conformance/results/` and `plans/research/003-rust-irc-crate-conformance-results.md`.
