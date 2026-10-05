# i2pr-irc Long-Term Roadmap

Status: canonical sequencing directive

## Phase 0 — Planning and contract foundation

Freeze the I2P-only product boundary, terminology, external research, planning/closure conventions, and initial ADR.

Exit: first implementation milestone is dependency-ready.

## Phase 1 — Protocol/domain/fault-test foundation

Establish Rust workspace and verification floor; bounded IRC/IRCv3 wire substrate; stable domain IDs; I2P-only endpoint/stream-provider types; local stream abstraction; injected time; deterministic stream-fault harness; static network/dependency guards.

No user-visible bouncer capability is claimed.

## Phase 2 — Minimal correct bouncer vertical

One Network, one NetworkSupervisor, one local downstream client, IRC registration/CAP/SASL, bidirectional relay, current-state tracking, phase-specific reconnect/liveness, clean shutdown, stale-generation fencing, and no clearnet/system-DNS path.

Exit requires repeated disconnect/reconnect evidence.

## Phase 3 — Durable multi-network / multi-client bouncer

Many supervisors and downstream clients; SQLite; desired network/channel state; history; state reconstruction; labeled-response routing; per-client cursors; server-time/batch/echo-message; draft chathistory/read-marker; bounded legacy backlog.

## Phase 4 — Anonymity and adverse-network qualification

CTCP policy, DCC rejection, client-tag allowlist, stable upstream capability policy, secret/log redaction, bounded slow-client behavior, global reconnect budget, high-latency/stall/path-loss testing, reconnect storms across many networks, restart consistency, and static proof against generic upstream clearnet/DNS.

## Phase 5 — Mature bouncer feature set

Persistent/detached channels, auto-away, keep-nick/reclaim, constrained perform commands, IRC-service administration, soju.im/bouncer-networks, richer IRCv3 mediation, bounded history search, configuration ergonomics, and diagnostics.

Arbitrary ZNC-style native/interpreted modules remain out of scope.

## Phase 6 — Portable SAM integration

Production SAM 3.1-compatible stream provider, long-lived session ownership, I2P naming, router restart behavior, and cross-router interoperability evidence. Proposal 170 is not required.

## Phase 7 — i2pr managed-app integration

Begins only after stable written i2pr contracts exist for app-scoped I2P streams, naming as needed, local accepted-stream delivery/listener capability, and required lifecycle/health behavior.

## Phase 8 — Optional scoped control integration

After Proposal 170 and i2pr's app-scoped control adapter stabilize, evaluate concrete bouncer needs. No milestone exists merely to claim Proposal 170 support.

## Cross-phase rule

Clearnet support is not a deferred phase. Adding it requires an explicit canonical product-direction change and ADR.
