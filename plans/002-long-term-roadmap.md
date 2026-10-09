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

Sequence: M010-A process/bootstrap and exclusive state owner; M010-B bounded listener/authentication; M010-C exact CAP/PASS/SASL registration handoff and stable ClientId; M010-D secure local credential/SQLCipher key provisioning; M010-E production integration and evidence-based closure. Plans 050-053 are closed. Plan 054 is active, with M010 closure gated on a controlled live i2pd-to-IRC product-path qualification.

A standalone product must work independently of i2pr managed-app contracts and Proposal 170. Installer/service packaging remains a later release line. Broad SAM portability matrices remain in the dedicated SAM library project.

## Independent IRC features after the M010 library baseline — M011/M012/M013 (registered, unimplemented)

Research 011 and accepted-for-planning ADR-0008/0009 are the new authority for optional IRC capability extensions. This work can build on the M001-M009 and R001 library baseline without claiming M010 Plan 054's separate live product-path gate has been satisfied or depending on R002 app-runtime contracts.

M011 — privacy and local intelligence (Plans 055–058):
- Per-buffer persistent/ephemeral/no-history storage policy, with migration, bounded purge and FTS/read-marker consistency, not falsely claiming deletion from backups/WAL.
- Soju-inspired bounded relay-detached/reattach-on/detach-after and ZNC-inspired local-only watch/mention notifications. No arbitrary scripting or external push/HTTP.

M012 — I2P IRC connectivity resilience (Plans 059–062):
- Generation-scoped JOIN/service-command pacing beyond the already implemented global reconnect budget; truthful upstream-outage gap evidence distinct from local Store drops.
- Conditional upstream CHATHISTORY retrieval only where actual IRCd offers it; bounded dedup and no speculative user-chat replay. Unavailable server capability is a documented feature deferral, not a reason to downgrade the normal IRC session.
- Verified operator-approved I2P endpoints in one IRC trust domain for optional failover. No implicit network/account equivalence or reuse of credentials across unrelated endpoints.

M013 — ordinary IRC2P/ILITA profiles, IRCv3 decisions and optional inner TLS (Plans 063–066):
- Normal operation remains plain IRC encapsulated in an I2P stream, without TLS. IRC2P-style NickServ identification is available without mandatory SASL. ILITA-style required SASL PLAIN is available when explicitly configured and acknowledged; verify currently deployed mechanisms with authorized live evidence before claiming them.
- Revisit CHGHOST, event playback and message redaction on current specs; adopt only fully mediated semantics and preserve accurate per-client CAP negotiation.
- TLS-over-I2P and certificate-backed SASL EXTERNAL are explicit opt-in and research-gated. TLS server identity verification and client-cert scope must be proved; no raw IRC transmission of certificate material, plaintext downgrade or unrestricted credential sharing.
- A potential future clearnet-directed client-cert exception cannot be implemented by a configuration flag in this product: ADR-0001 structurally forbids clearnet egress. It would require a separate explicit canonical product change, new scoped connector and security approval. No override is authorized by M013.

Dependency order: 055 first ready, 056-066 sequentially proposed/gated. Planned work does not imply implemented functionality. Scope-specific closure records and registry updates are required.

## Phase 7 — i2pr managed-app integration

Begins only after stable written i2pr contracts exist for app-scoped I2P streams, naming as needed, local accepted-stream delivery/listener capability, and required lifecycle/health behavior.

## Phase 8 — Optional scoped control integration

After Proposal 170 and i2pr's app-scoped control adapter stabilize, evaluate concrete bouncer needs. No milestone exists merely to claim Proposal 170 support.

## Cross-phase rule

Clearnet support is not a deferred phase. Adding it requires an explicit canonical product-direction change and ADR.
