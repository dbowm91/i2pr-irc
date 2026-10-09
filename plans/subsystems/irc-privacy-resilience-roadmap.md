# IRC Privacy, Resilience and Compatibility Roadmap
Status: M011 closed; M012-D integrated closure in progress; Plans 055-061 are closed (060 production feature deferred).
Research: plans/research/011-irc-privacy-resilience-authentication.md
ADRs: ADR-0008 and ADR-0009; all prior accepted ADRs remain binding.
Baseline: work/plans-050-054-m010-standalone (M010-E Plan 054 active).

## Ownership and product constraints
This track extends existing NetworkOwner, Store, history, BouncerServ and IRC capability mediation; it does not create a second owner, a plugin interpreter, new router implementation, generic network stack, external notification delivery or nonlocal listener. `I2pStreamProvider` remains the only upstream authority. Feature switches default to the safer/legacy-compatible behavior at upgrade.

M010 standalone qualification and R002 managed-app interfaces remain independent. Core deterministic implementation can proceed without R002, while production live-network compatibility claims require authorized real I2P router/service evidence. Never re-test generic Java ↔ i2pd interoperability merely to close application features.

## Dependency graph
M010 library baseline (M001–M009/R001 and M010 Plans 050–053 closed; 054 active)
  -> M011-A / 055 buffer privacy + migrations [closed]
  -> M011-B / 056 detached activity policy [closed]
  -> M011-C / 057 local watch and notification [closed]
  -> M011-D / 058 integration/closure [closed]
  -> M012-A / 059 IRC pacing and upstream gap records [closed]
  -> M012-B / 060 conditional upstream history reconciliation [closed; production implementation deferred by draft warning]
  -> M012-C / 061 verified same-network I2P endpoint failover [closed; operator attestation required]
  -> M012-D / 062 resilience closure [active]
  -> M013-A / 063 named network auth profiles [proposed after 062]
  -> M013-B / 064 IRCv3 member/redaction/event-playback decision and bounded implementation [proposed after 063; unsafe pieces deferred]
  -> M013-C / 065 optional TLS-over-I2P and SASL EXTERNAL [research-gated after 064]
  -> M013-D / 066 interoperability closure [proposed after conditional 065 disposition]

## Milestone exits
M011: privacy mode controls every durable/in-memory ingestion and query path, including FTS/OTR/detached, with migration, resource and multi-client tests; notifications remain local, bounded and off by default.

M012: no connection storm, no JOIN/service command burst beyond bounds, truthful gap visibility, optional server-supported upstream catch-up that never fabricates history or replays chat, and failover only across operator-declared/verified trust-equivalent I2P servers. Plan 059 closes pacing and gap evidence. Plans 060 and 061 may remain conditional/deferred if server capability or network-equivalence evidence is unavailable.

M013: IRC2P NickServ/no-SASL and ILITA explicitly configured SASL PLAIN profiles work without TLS and fail closed on incompatible authentication; IRCv3 capability claims are truthful; optional TLS-over-I2P/EXTERNAL proves certificate identity handling within I2P or is recorded as deferred, never falsely shipped. There is no clearnet override.

## Cross-cutting evidence
Every implementation plan requires bounded queues, timers and memory, failure/restart semantics, schema backward migration, profile scoping, test matrix for I2P unavailability/long latency/connection churn, ported Rust MSRV verification, security/credential redaction, static network-boundary negative/positive controls, and per-plan closure in plans/closure/irc-enhancements/NNN-status.md.
For live IRC2P/ILITA claims, collect sanitized evidence from a permitted endpoint without storing private destinations/credentials. A fake server is valid for protocol correctness but not for claims of deployed capability support.

## Explicitly excluded
Clearnet, outproxy, DCC, HTTP web push, arbitrary executable hooks, generic ZNC modules, hardcoded IRC passwords or certificates, blind failover between unrelated IRC networks, speculative outgoing message replay, OTR termination inside the bouncer, and treating message redaction as guaranteed erasure.
