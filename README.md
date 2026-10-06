# i2pr-irc

i2pr-irc is a planned Rust IRC bouncer for I2P.

The product boundary is deliberately narrow: upstream IRC traffic is I2P-only. The bouncer does not provide a clearnet IRC connector, system-DNS fallback, generic proxying, DCC, or an HTTP side channel. Its first priority is correct IRC/IRCv3 bouncer behavior under unstable, high-latency stream conditions. Router integration follows through explicit I2P stream-provider interfaces.

Target deployment forms are a standalone local bouncer using a local router through SAM, portable SAM operation against compatible routers, and first-party managed-application integration with i2pr once its public app API is stable. Proposal 170 integration is optional control-plane work, not a data-plane dependency.

Planning authority begins at:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md
- plans/registry.md

## Implementation state

M001's bounded wire/domain/time/fault foundations and M002's one-network, one-local-client runtime are closed. The runtime uses injected I2P and local stream capabilities, with CAP/SASL registration, bounded state and queues, liveness, reconnect, and no-replay handling. It is not a complete daemon or router integration. M003 is closed: the core is a durable multi-Network, multi-client bouncer with transactional SQLite storage, bounded history with private per-client cursors, SessionId-scoped labeled-response routing, and bounded chathistory/read-marker adapters. M004 (anonymity and adverse-network qualification) is unblocked but not yet decomposed into plans. Follow `plans/registry.md` for current status. See [architecture/overview.md](architecture/overview.md) and the milestone records under `plans/closure/bouncer-core/`.

Corrective 013 subsequently closed against M003 and repaired five defects in it. Server-time is a preserved canonical UTC millisecond timestamp rather than integer epoch seconds, with leap seconds preserved and a transactional schema v1→v2 migration; CHATHISTORY and MARKREAD follow the reviewed draft grammar and are reachable from a real client; a command refused by the bounded upstream queue is reported and counted rather than dropped, and committed DesiredState converges through a bounded reconciliation set; and a client that misses a live IRC frame is detached rather than left attached and desynchronized. M003's closure record was preserved unmodified, and one non-blocking finding — live response routing is constructed and expired but never opened on the client-intent path — is recorded in `plans/closure/bouncer-core/013-status.md`.

Research lives under plans/research/. Subsystem roadmaps live under plans/subsystems/. Bounded implementation handoffs live under plans/implementation/, and evidence-based completion records live under plans/closure/.

## Conformance corpus

`research/irc-conformance/` contains an independently authored IRC/IRCv3 conformance corpus derived from primary specifications, plus a committed runner per owned layer. `cargo test -p i2pr-irc-wire --test conformance` and `cargo test -p i2pr-irc-runtime --test conformance` execute it on every test run. External comparisons and per-candidate dispositions live in `research/irc-conformance/results/` and `plans/research/003-rust-irc-crate-conformance-results.md`.
