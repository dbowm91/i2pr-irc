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

## Post-M005 core enhancement track — M006/M007

This track is independent of the blocked i2pr managed-app work and may proceed while router-app contracts stabilize.

M006 — IRC interoperability and capability downgrade:

- no-CAP/no-SASL server support;
- strict required-SASL semantics when configured;
- plain IRC directly over I2P without a TLS requirement;
- truthful promotion of selected IRCv3 capabilities;
- simultaneous legacy/modern downstream qualification.

M007 — identity and connectivity resilience:

- bounded service-auth/recovery phases for non-SASL networks;
- transient nick-collision retry rather than permanent failure;
- robust preferred-nick reclaim;
- simultaneous-client nick consistency;
- deterministic adverse-network qualification, including external Eggchaos process/socket campaigns.

M006 and M007 are now historically closed.

## Post-M007 privacy track — M008/M009

This track is independent of the blocked i2pr managed-app work.

M008 — encrypted durable state:

- SQLCipher whole-database encryption as an explicit option;
- injected process-level store key rather than a key derived from downstream login;
- encrypted credentials, service-action payloads, history and FTS index together;
- source-preserving plaintext-to-encrypted migration;
- source-preserving key rotation;
- wrong-key fail-closed behavior and platform/MSRV qualification.

M009 — OTRv3 transparent-carriage compatibility:

- OTR remains endpoint-to-endpoint between IRC clients;
- the bouncer carries opaque OTR query/AKE/data/fragment payloads without decrypting or owning crypto state;
- simultaneous bouncer clients do not share bouncer-side OTR state;
- OTR-bearing chat remains non-replayable across ambiguous disconnects;
- retained history contains ciphertext only;
- OTRv4 and built-in-client crypto remain later endpoint work.

ADR-0006 is the encryption-layering authority. Database encryption is not described as E2EE, and OTR is not terminated in the bouncer.

M008 and M009 are closed (Plans 044-048, plus post-M009 Corrective 049).

## Phase 6 — Portable SAM integration

Production SAM 3.1-compatible stream provider, long-lived session ownership, I2P naming, router restart behavior, and one real mature-router application-byte qualification. The temporary in-repo SAM client is not required to repeat Java/i2pd/i2pr or mixed-router conformance; broad SAM portability belongs to the dedicated SAM library project. Proposal 170 is not required.

## Standalone productization after R001 — M010 (registered)

The core is closed through M009 and the portable SAM product path through R001. M010 adds the first executable and authenticated local IRC listener without altering I2P-only upstream transport. This is a separately tracked standalone-daemon subsystem (Research 010, ADR-0007, Plans 050-054).

Sequence: M010-A process/bootstrap and exclusive state owner; M010-B bounded listener/authentication; M010-C exact CAP/PASS/SASL registration handoff and stable ClientId; M010-D secure local credential/SQLCipher key provisioning; M010-E production integration and evidence-based closure. Only 050 is initially ready; later plans are dependency-gated.

A standalone product must work independently of i2pr managed-app contracts and Proposal 170. Installer/service packaging remains a later release line. Broad SAM portability matrices remain in the dedicated SAM library project.

## Phase 7 — i2pr managed-app integration

Begins only after stable written i2pr contracts exist for app-scoped I2P streams, naming as needed, local accepted-stream delivery/listener capability, and required lifecycle/health behavior.

## Phase 8 — Optional scoped control integration

After Proposal 170 and i2pr's app-scoped control adapter stabilize, evaluate concrete bouncer needs. No milestone exists merely to claim Proposal 170 support.

## Cross-phase rule

Clearnet support is not a deferred phase. Adding it requires an explicit canonical product-direction change and ADR.
