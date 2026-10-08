# Research 008 — M006/M007 IRC Interoperability and Identity Resilience

Status: complete for implementation planning

Research date: 2026-10-07

Repository baseline:

- a5fa47b20b58ddb0e72007eaec72e9fa8e238d8e

Related authority:

- plans/000-long-term-specification.md
- plans/002-long-term-roadmap.md
- plans/subsystems/bouncer-core-roadmap.md
- plans/closure/bouncer-core/022-status.md
- plans/closure/bouncer-core/026-status.md
- plans/closure/bouncer-core/028-status.md
- plans/closure/router-integration/033-status.md

External protocol references reviewed:

- IRCv3 capability negotiation: https://ircv3.net/specs/core/capability-negotiation
- IRCv3 SASL 3.1/3.2: https://ircv3.net/specs/extensions/sasl-3.1 and https://ircv3.net/specs/extensions/sasl-3.2
- IRCv3 account-tag: https://ircv3.net/specs/extensions/account-tag
- IRCv3 invite-notify: https://ircv3.net/specs/extensions/invite-notify
- IRCv3 chghost: https://ircv3.net/specs/extensions/chghost-3.2.html
- IRCv3 MONITOR: https://ircv3.net/specs/core/monitor-3.2.html
- IRCv3 extended-monitor: https://ircv3.net/specs/extensions/extended-monitor
- Modern IRC client protocol: https://modern.ircdocs.horse/
- ZNC keepnick/nickserv/perform sources as behavior references only
- eggchaos README/control-plane as adverse-network qualification substrate

## 1. Purpose

Research M006 and M007 together because capability downgrade, authentication posture, preferred-nick recovery, services actions, netsplit behavior, and multi-client identity projection all meet inside the same upstream generation lifecycle.

The intended product priorities are:

- robust IRC/IRCv3 interoperability over I2P;
- first-class operation against servers with no TLS and no SASL;
- correct preferred-nick recovery after disconnects, collisions, and split-like churn;
- simultaneous multi-client behavior;
- deterministic degraded-network qualification;
- no unnecessary additional router/SAM conformance loops.

This research explicitly does not expand the temporary owned SAM implementation. Corrective 033 already proved exact application-byte transport through the production SamProvider against i2pd 2.61.0. Broader multi-router SAM conformance belongs to the dedicated SAM-library project.

## 2. Immediate pre-existing defect

M005-C implemented MONITOR-assisted preferred-nick reclaim, but both its closure record and live owner code interpret MONITOR numerics backwards.

Current code treats:

- 730 as RPL_MONITOROFFLINE and therefore evidence that the preferred nick is free;
- 731 as the online/no-action case.

The current IRCv3 MONITOR specification defines:

- 730 = RPL_MONONLINE;
- 731 = RPL_MONOFFLINE.

The live code can therefore attempt to reclaim a nick precisely when the server reports it online, and ignore the actual offline notification.

This is a correctness defect in an already-closed feature, not M007 feature work.

Decision:

- register Corrective 035 first;
- M006/M007 implementation stays blocked until Corrective 035 closes;
- preserve the historical Plan 022 closure and supersede only the MONITOR-numeric interpretation through the corrective record.

## 3. M006 registration compatibility findings

### 3.1 No-CAP servers are currently not handled correctly

The upstream registration path always sends:

CAP LS 302
NICK ...
USER ...

and waits for both:

- welcome observed;
- capability negotiation marked finished.

If an older server ignores CAP but still sends 001, welcomed becomes true while cap_finished remains false. The owner continues waiting under CAP_SASL_TIMEOUT and eventually tears down a connection that had actually registered.

Required M006 behavior:

- track whether the server demonstrated CAP support;
- if 001 arrives before any valid CAP response and no SASL credential is configured, treat CAP as unsupported, mark negotiation complete locally, and keep the generation;
- do not send a meaningless CAP END after registration to a server that ignored CAP;
- if a required SASL credential is configured, a 001 reached without successful SASL is a fail-closed registration failure, never a silent downgrade.

ERR_UNKNOWNCOMMAND for CAP before registration should have the same unsupported-CAP disposition when SASL is not required.

### 3.2 Bare sasl capability is valid

Current code requires both:

- the server offers sasl; and
- the CAP value explicitly lists PLAIN.

SASL 3.2 explicitly requires clients to handle a sasl capability with no value.

Required behavior:

- bare sasl means mechanism list unknown; the bouncer may attempt its supported PLAIN mechanism;
- sasl=... with a mechanism list that excludes PLAIN remains incompatible with the current configured credential;
- a configured SASL secret remains required/fail-closed;
- M006 must not automatically fall back to NickServ with the SASL secret.

### 3.3 No-SASL is already a valid product mode

NetworkRecord.sasl is optional. When absent, the product should register successfully without SASL.

M006 should preserve the important distinction:

- SASL absent in configuration: no SASL required; capability absence is normal.
- SASL configured: authentication is required; lack/failure of SASL is terminal for that generation/configuration posture.

No "SASL preferred" mode is required for M006/M007.

Servers without SASL are supported through explicit service actions in M007 rather than by silently repurposing SASL credentials.

### 3.4 Plain IRC over I2P is canonical

The standalone provider returns an already-established I2P byte stream and the IRC runtime speaks ordinary IRC on that stream. There is no TLS layer in the bouncer core.

This is desirable for I2P IRC because in-network services do not uniformly deploy TLS and I2P already supplies the routed encrypted transport.

M006 should explicitly qualify:

- no TLS requirement;
- no STS request or automatic TLS upgrade;
- no failure merely because upstream IRC has no TLS/SASL;
- future TLS-over-I2P, if added, is opt-in and separate from this milestone.

No clearnet fallback or generic TLS connector is introduced.

## 4. M006 IRCv3 capability findings

### account-tag

The earlier M005 deferral rationale assumed supporting account-tag would require synthesizing account tags on messages the upstream did not stamp.

That is unnecessary.

A truthful bouncer can:

- request account-tag only when the server offers it;
- advertise it downstream only when upstream acknowledged it;
- pass an upstream account tag only to sessions that negotiated account-tag;
- strip the account tag from sessions that have message-tags but not account-tag;
- never synthesize an account tag from cached account state.

This preserves the existing "do not fabricate live server claims" rule.

Decision: implement in M006.

### invite-notify

This is additive and manageable.

An ordinary INVITE targeting the bouncer itself remains normal IRC traffic and reaches attached sessions.

An INVITE about another user that the bouncer receives only because invite-notify was enabled should reach only downstream sessions that negotiated invite-notify.

Decision: implement in M006.

### chghost

The upstream capability is straightforward for modern clients, but a bouncer requesting chghost changes what the upstream server sends. A downstream legacy client that did not negotiate chghost then requires the specification's synthetic QUIT/JOIN/MODE compatibility sequence.

The repository deliberately distinguishes observed upstream membership from generated local presentation and currently avoids inventing membership events. Correct fallback also requires sufficient prefix/membership state to restore channel modes truthfully.

Implementing chghost without a dedicated compatibility-projection architecture would either:

- leave legacy clients with stale user/host state;
- synthesize incomplete membership/mode state; or
- make upstream capability negotiation depend on which clients are attached.

All three are worse than withholding it.

Decision: keep chghost explicitly deferred in M006/M007. A future milestone may introduce a reviewed derived-compatibility-projection model.

### extended-monitor

The current bouncer uses MONITOR internally for one preferred nick but does not expose a full per-session downstream MONITOR broker.

extended-monitor requires:

- per-session monitor sets;
- a bounded merged upstream subscription set;
- routing metadata updates only to sessions that monitor the target;
- interaction with account-notify/chghost/setname/away-notify;
- explicit privacy review because the extension expands metadata visibility for users who do not share channels.

Decision: defer extended-monitor. It is not required for preferred-nick recovery.

### Other already-implemented IRCv3

M006 should preserve and requalify current support for:

- CAP 302/cap-notify;
- message-tags/client tag policy;
- server-time;
- batch;
- labeled-response;
- echo-message when upstream supports it;
- standard-replies;
- extended-join;
- account-notify;
- away-notify;
- multi-prefix;
- setname;
- draft/no-implicit-names;
- draft/chathistory/read-marker.

No capability is promoted merely to increase a checklist count.

## 5. M007 service authentication findings

NickServ/services authentication is not standardized.

ZNC's nickserv module demonstrates why hard-coded heuristics are unattractive:

- it matches English service text;
- it supports configurable command patterns;
- it recommends SASL where available.

For this project, parsing service prose would be fragile and would turn remote text into control flow.

The existing constrained RegistrationAction model is a stronger base:

- service-targeted PRIVMSG/NOTICE only;
- bounded;
- redacted;
- replayed intentionally per generation;
- no arbitrary raw quote.

Decision:

- do not parse NickServ prompts;
- do not hard-code Anope/Atheme prose;
- extend the action model with explicit phases/triggers rather than inventing a NickServ protocol.

Proposed phases:

1. PreJoin — service-only messages intended for account identification before desired JOIN replay.
2. PostJoin — existing behavior; all existing stored actions migrate here to preserve semantics.
3. FallbackRecovery — service-only messages emitted only when the generation registered under a fallback nick while keep_nick is enabled.

This supports operator-configured examples such as IDENTIFY, RECOVER, RELEASE, or GHOST without making any one services package canonical.

Secrets remain stored/redacted under the existing registration-action secret treatment.

## 6. M007 preferred-nick findings

### Registration collision classes

Current registration handles 433 and 436 with a bounded fallback sequence, then marks NickExhausted terminal.

For long-running bouncer operation that is too final for a transient identity conflict.

M007 should distinguish:

Permanent/configuration errors:
- 432 erroneous nickname;
- malformed configured nick;
- explicit policy/auth failures.

Transient nickname availability:
- 433 nickname in use;
- 436 collision;
- 437 unavailable resource when the refused target is the attempted nickname and the server uses that numeric for temporary nick unavailability.

Fallback exhaustion for a transient collision should end the generation and retry only after a dedicated long collision cooldown under the global reconnect budget. It must not hot-loop and must not remain terminal forever.

### Correct MONITOR evidence

After Corrective 035:

- 730 means preferred nick online, not reclaim evidence;
- 731 means preferred nick offline, therefore immediate reclaim evidence;
- 303 ISON lists online nicks; preferred nick absent means free.

The NICK command remains a request; only the server's own NICK frame confirms success.

### Reclaim refusal while online

A reclaim NICK may itself receive 433/437.

That must not tear down an otherwise healthy generation.

Required behavior:

- record the failed reclaim;
- clear immediate evidence;
- return to bounded MONITOR/ISON waiting/cooldown;
- never spam repeated NICK writes.

### Explicit downstream NICK

A local client NICK command is explicit Operator intent and should not fight the keep-nick timer.

Recommended semantics:

- forward the NICK as one non-replayable Network-wide command;
- suspend automatic preferred-nick reclaim for the rest of that generation when the requested nick differs from the durable preferred nick;
- if the Operator explicitly requests the preferred nick, normal reclaim policy remains active;
- on the next generation the durable preferred nick is tried again unless the Operator changes durable configuration.

This follows the useful product behavior in ZNC keepnick without changing durable configuration from one attached client.

### Local attachment while upstream holds a fallback nick

Current admission requires the client to claim the exact live upstream nick. That means an Operator whose configured client still uses the preferred nick cannot attach while the bouncer temporarily holds bot_1.

For a persistent bouncer, this is poor multi-client behavior.

M007 should allow the configured preferred nick as a local registration alias while the Network holds a generated fallback nick.

Truthful projection:

1. client claims the configured preferred nick;
2. bouncer accepts because it is the durable alias for that Network;
3. before/with registration projection, the session is told the actual current observed nick through an explicit local NICK transition or equivalent protocol-correct sequence;
4. all subsequent state uses the observed current nick.

Arbitrary aliases remain forbidden.

The implementation must prove a new client never believes the Network holds the preferred nick when it does not.

## 7. Netsplit and reconnect findings

Do not parse human-readable QUIT reasons to detect netsplits.

"Netsplit" behavior should fall out of protocol/state transitions:

- transport EOF/reset => generation ends and reconnects under existing backoff/global budget;
- remote user quits/disappears => observed state changes;
- preferred nick availability => MONITOR/ISON evidence;
- nickname collision => typed transient nick availability state;
- desired channels => re-sent only after fresh registration;
- ambiguous user chat => never replayed;
- service setup actions => intentionally replay according to phase.

This avoids server-brand-specific split strings.

## 8. Multi-client identity requirements

M007 must qualify at least:

- modern + legacy downstream sessions simultaneously;
- one session reconnecting while another stays attached;
- Network nick changing under all sessions;
- preferred nick regained while several sessions are attached;
- one session issuing manual NICK while another is passive;
- one slow/desynchronized session while healthy sessions continue;
- history/query routing during upstream reconnect;
- no session-specific capability changes altering upstream registration fingerprint.

Nick and services state is Network-wide. No per-session nick authority is introduced.

## 9. eggchaos disposition

eggchaos is a strong external qualification substrate and does not need product integration.

Current eggchaos capabilities include deterministic TCP stream:

- latency/jitter;
- bandwidth;
- blackhole;
- byte limit;
- slow-close;
- slicing;
- disconnect;
- stream-loss;
- versioned seeded scenarios.

Recommended topology for M007 qualification:

i2pr-irc SamProvider
    -> loopback eggchaos TCP proxy
    -> fake SAM bridge or local SAM bridge

Once STREAM CONNECT transitions to raw mode, the same TCP connection carries the application stream, so eggchaos can exercise the production socket path through both SAM framing and raw stream phases.

Important boundaries:

- eggchaos remains an external test executable, not a Cargo/production dependency;
- its Rust 1.89 MSRV does not change i2pr-irc's Rust 1.88 floor;
- qualification pins an eggchaos release/revision;
- deterministic in-process testkit remains the mandatory unit/integration substrate;
- eggchaos adds process/socket-boundary evidence rather than replacing existing tests.

No eggchaos repository change is required for M006/M007.

## 10. SAM verification scope correction

Corrective 033 established:

- production SamProvider against i2pd 2.61.0;
- exact bidirectional application bytes;
- two streams from one long-lived SAM session;
- explicit release to zero live scope.

For this repository, that is sufficient evidence for the temporary owned SAM client.

Decision:

- stop treating Java I2P, i2pr, and mixed-router testing as required R001 closure work here;
- mark R001 fully closed on the existing i2pd product-path evidence;
- broad multi-router SAM conformance moves to the dedicated SAM library project;
- R002 remains blocked only on the i2pr managed-app public contracts it actually needs.

## 11. Implementation decomposition

### Corrective 035 — MONITOR numeric conformance

Fix the reversed 730/731 semantics and update closure/docs/tests.

### M006-A / Plan 036 — registration downgrade and legacy-server baseline

Implement:

- no-CAP registration completion;
- required-SASL fail-closed behavior;
- bare sasl capability handling;
- no-SASL success;
- plain IRC-over-I2P baseline;
- capability downgrade matrices.

### M006-B / Plan 037 — account-tag and invite-notify mediation

Promote only those two deferred capabilities with per-session mediation. Keep chghost and extended-monitor explicitly deferred.

### M006-C / Plan 038 — integrated IRCv3/degraded-server qualification and M006 closure

Qualify old/no-CAP/no-SASL and modern IRCv3 servers, simultaneous clients, capability loss/change, and absence of TLS assumptions.

### M007-A / Plan 039 — phased service actions for non-SASL authentication/recovery

Add PreJoin, PostJoin, and FallbackRecovery action phases with schema migration and strict service-message bounds. Existing actions migrate PostJoin.

### M007-B / Plan 040 — preferred-nick/reconnect/multi-client identity resilience

Implement transient collision retry, reclaim refusal cooldown, manual NICK reclaim suspension, preferred-nick local alias attach, and corrected generation-fenced reclaim semantics.

### M007-C / Plan 041 — Eggchaos + multi-client adverse qualification and M007 closure

Run deterministic and external process-boundary fault campaigns, including long stalls/blackholes/disconnects and multiple clients, then close M007.

## 12. Sequencing

Strict handoff sequence:

Corrective 035
  -> Plan 036
  -> Plan 037
  -> Plan 038 / M006 closure
  -> Plan 039
  -> Plan 040
  -> Plan 041 / M007 closure

The sequence is deliberately linear because Plans 036-040 all touch the upstream registration/generation/identity path. Parallel implementation would create competing state machines.

## 13. Explicit non-goals

M006/M007 do not include:

- TLS implementation;
- clearnet support;
- SASL mechanisms beyond the existing PLAIN profile;
- automatic fallback from configured SASL to NickServ;
- parsing NickServ/service prose;
- generic raw perform;
- chghost compatibility synthesis;
- downstream extended-monitor broker;
- OTR/E2EE;
- encrypted database/history;
- plugin ABI;
- extra Java/i2pd/i2pr SAM conformance loops.

Privacy/encryption is a later independent milestone.

## 14. Readiness

Corrective 035 is ready immediately.

All later M006/M007 plans should be registered now with explicit blockers.

No external router or i2pr managed-app API is required for M006/M007.
