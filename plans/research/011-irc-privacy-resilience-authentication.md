# Research 011 — Post-M010 IRC Privacy, Reliability, and Network Authentication

Status: completed planning research (2026-10-09); observational network compatibility remains a live qualification requirement.
Baseline: `work/plans-050-054-m010-standalone` at the time of planning; branch `work/plans-055-066-irc-privacy-resilience`.
Authority: plans/000-long-term-specification.md, plans/002-long-term-roadmap.md, ADR-0001/0002/0003/0006/0007.

## Questions

Which ZNC/soju/IRCv3 mechanisms add operator value under I2P anonymity constraints and degraded connectivity? Which are already implemented? What should be deferred? How should IRC2P, ILITA and possible future TLS/CertFP networks be represented without an accidental clearnet or credential-identity escape?

## Source-derived baseline in this repository

- M001–M009 and R001 are closed; M010 Plans 050–053 are closed, Plan 054 remains active, blocked on controlled real i2pd-to-IRC product-path evidence. Do not reclassify M010 through this workstream.
- M003/M005 already supply per-(ClientId,BufferId) history cursors; SQLite/FTS; bounded CHATHISTORY and search; detached channels; auto-away; nick reclaim; constrained phased NickServ actions; soju bouncer-networks; standard IRCv3 mediation; and process-global reconnect scheduling.
- M008 SQLCipher is whole-database optional encryption, NOT per-buffer retention policy. M009 OTRv3 carriage is opaque, NOT an endpoint crypto implementation. Local outgoing chat has no speculative delivery history; upstream echo is the only confirmed outgoing event.
- `architecture/history.md` states only PRIVMSG/NOTICE are recorded, dropped-on-pressure history is counted, and detached-channel traffic is still stored. `architecture/chathistory.md` has local history adapters but does not implement upstream missed-history retrieval.
- `architecture/upstream-registration.md` confirms SASL PLAIN is supported, is explicitly required/fail-closed when configured, and CAP-less servers work without SASL. `architecture/operator-surfaces.md` confirms phased NickServ service messages exist. `architecture/sam-adapter.md` requires typed I2P endpoint + loopback SAM and long-lived transient per-Network session.
- `plans/subsystems/standalone-daemon-roadmap.md` and `plans/registry.md` establish M010 as separate from core enhancements.

## External specifications and references (independent implementation, not code donor)

- soju upstream manual: https://github.com/emersion/soju/blob/master/doc/soju.1.scd — `relay-detached`, `reattach-on`, `detach-after`, `detach-on` and multiclient retention semantics.
- IRCv3 CHATHISTORY and optional event playback: https://ircv3.net/specs/extensions/chathistory.html — work in progress; upstream access only after actual CAP/ISUPPORT negotiation and authorization.
- IRCv3 SASL 3.2: https://ircv3.net/specs/extensions/sasl-3.2 — mechanism offer/selection/NAK and `AUTHENTICATE` state.
- SASL EXTERNAL: https://www.rfc-editor.org/rfc/rfc4422.html#appendix-A — EXTERNAL uses an externally established credential, usually TLS client certificates for IRC CertFP. `AUTHENTICATE EXTERNAL` by itself does not present a TLS client certificate and does not authenticate the client.
- IRCv3 CHGHOST: https://ircv3.net/specs/extensions/chghost-3.2.html — legacy-client fallback requires careful membership handling.
- IRCv3 message redaction: https://ircv3.net/specs/extensions/message-redaction — draft, display-level not forensic erasure.
- I2P IRC guide: https://i2p.net/en/docs/applications/irc/ — IRC2P/ILITA destination examples, default non-TLS I2P transport. These examples do NOT independently prove current live SASL capabilities or their server-service equivalence.

### Plan 060 CHATHISTORY preflight (2026-10-09)

Reviewed the current official [IRCv3 CHATHISTORY specification](https://ircv3.net/specs/extensions/chathistory.html) and [BATCH specification](https://ircv3.net/specs/extensions/batch). CHATHISTORY remains work in progress and explicitly warns against production use, so this repository may only attempt the exact `draft/chathistory` name when the active IRCd offers and ACKs it; it must never infer support from a network profile or downstream client capability. Full support depends on upstream `batch`, `server-time`, and `message-tags`; `MSGREFTYPES` governs whether `msgid=` or `timestamp=` anchors are accepted. A successful history response uses a `chathistory` batch with a canonical target, each content line carries the `batch` tag, and the batch must close. The batch reference is opaque and case-sensitive. `draft/event-playback` is a separate optional feature and is not needed for message-only catch-up.

The spec permits variable response counts and implementation-defined ordering; timestamps can skew across servers. Recovery therefore needs bounded pages, a stable previously retained `msgid` anchor when available, local HistoryEventId order, and explicit unresolved-gap status when references or messages are unavailable. Channel membership/authorization and direct-message account identity remain server policy; a numeric or empty reply is not proof that the history was complete. A compliant server should refuse inaccessible history with standard errors such as `INVALID_TARGET` or `MESSAGE_ERROR`. Local no-history and ephemeral policies remain stronger than any upstream replay offer.

This review is enough to implement a conditional protocol adapter behind direct upstream capability negotiation and exact-wire fixtures. It does not establish that IRC2P or ILITA currently advertises/authorizes CHATHISTORY; no live transcript was collected, and no deployed-network support claim is made.

## Compatibility profiles and confidence

**IRC2P:** intended default is plain IRC carried via I2P, CAP absent/partial tolerated, SASL not required, and optional credentialed NickServ identification/reclaim via the existing constrained phased service actions. This is operator-specified interoperability intent; validate with authorized server responses, do not assert every federated node behaves identically. No TLS or SASL fallback automatically activated.

**ILITA:** intended default is plain IRC carried via I2P, with explicitly configured SASL PLAIN required (fail-closed) when account credentials are provisioned. Confirm the actual `CAP LS 302` mechanisms and `AUTHENTICATE` result with an authorized current endpoint before claiming live conformance. Absence/NAK of configured SASL must not silently fall back to NickServ or unauthenticated operation. If a particular endpoint does not advertise SASL, expose a clear incompatible-profile error and allow explicit operator reconfiguration.

**Other/newer I2P IRC services:** per-network explicit transport and auth profile. `plain-i2p` is default; possible `tls-over-i2p` is explicitly opt-in. Proposed `sasl-external` requires both a negotiated SASL EXTERNAL mechanism and authenticated TLS client credential exchange to an explicitly approved I2P server.

Public documentation evidence confirms IRC2P/ILITA I2P endpoint conventions and ordinary non-TLS IRC, but does not establish live SASL/EXTERNAL matrices. Record capability transcripts without credentials or raw I2P Destinations.

## Prioritized findings

F011-1 Per-buffer persistence is the strongest privacy gap: `persistent`, `ephemeral` and `no-history` must govern ingress, FTS, history queries, cursors/read markers, detached-channel ingestion, backup/export and migration, not just playback. SQLCipher protects storage confidentiality, not retention. Never promise physical erase from WAL, filesystem snapshots, SSDs or backups.

F011-2 Detached-channel automation should be pure, bounded policy triggered by observed owner events; no spontaneous JOIN/PART or additional router calls. Mention matching must be IRC-casefold-aware and avoid parsing encrypted OTR bodies as ordinary search terms.

F011-3 Persistent watch rules/notifications must be local-only, no web push, email, HTTP, outproxy or scripting host. Do not allow an untrusted IRC client to choose arbitrary executable hooks. Notifications must be bounded, redacted as configured, and scoped to authenticated Operator/ClientId sessions.

F012-1 Global dial throttling does not throttle per-generation registration/service/JOIN command bursts. Separate queue budgets, priority and generation-fenced scheduling; preserve liveness and no replay of ambiguous user commands.

F012-2 The bouncer's retained history is **not** upstream server history during I2P outage. Gap markers should state evidence and uncertainty without pretending missing content exists. Optional catch-up needs server-advertised capability, bounded pages, stable cursor reconciliation, duplicate/ambiguous msgid handling, timestamp skew limits, channels/PM authorization, and hard absence fallback. Production demonstration depends on a server that actually supports it; unsupported networks must remain healthy.

F012-3 Multi-endpoint failover may only switch among verified servers belonging to the same authenticated IRC network/trust domain. I2P labels do not prove federation; no automatic cross-network migration. Keep one logical Network owner and one active SAM stream at a time.

F013-1 CHGHOST, event playback and redaction remain deferred unless precise spec+storage+legacy-client semantics can be safely advertised. Do not misrepresent redaction as erasure.

F013-2 TLS-over-I2P is an optional **inner** protocol, independent from I2P stream encryption. Never auto-enable TLS from an endpoint name or `sasl=EXTERNAL` offer. Verify TLS peer identity via explicit pinned certificate/SPKI or a validated identity policy that actually works for .i2p names; avoid downgrade/insecure verification flags. A client certificate/key stays in a separately authorized secret source, is not in config export/diagnostics, and is never sent on an unencrypted IRC application stream.

F013-3 No clearnet TLS/IRC egress, even with user-supplied `host` or cert path. Current canonical direction (ADR-0001) structurally forbids it; a hypothetical future *specific, explicit* user override for clearnet certificate use needs a separate product-direction change, threat model, ADR, and new connector authority. No such override or connector is approved or created by this planning line.

## Sequencing

M011 Plans 055–058: per-buffer privacy, detached-channel behavior, local watch, integrated closure.
M012 Plans 059–062: IRC pacing/gap evidence, conditional upstream catch-up, verified failover, integrated closure.
M013 Plans 063–066: network auth profiles, cautious IRCv3 compatibility, optional I2P-only TLS+EXTERNAL, integrated closure. Plan 065 requires its own implementation preflight and may remain deferred if adequate authenticated TLS material/testing cannot be obtained. M013 closure must truthfully disposition it; unsupported mechanisms are not advertised.

All milestones can be designed without R002. No generic ZNC module host, HTTP Web Push, DCC, clearnet path, or i2pr private internals.
