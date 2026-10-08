# Standalone M010-E / Plan 054 — Standalone Product Integration and M010 Closure

Status: proposed — dependency-gated on Plan 053 closure
Repository baseline for planning: f325e7d5e495e36b1fc7b168c30e4b725756d22c
Primary class: qualification + milestone closure
Authority: plans/subsystems/standalone-daemon-roadmap.md; ADR-0007; Research 010; M008/M009 and R001 closure records

## 1. Objective

Qualify an actual headless standalone i2pr-irc application from normal configuration through authenticated real local IRC-client session, durable Network selection, upstream SAM connectivity, controlled restart, history recovery and safe security/resource failure. Reconcile docs/registry, close M010 only on reproducible product-path evidence. This does not ship release packaging or managed i2pr integration.

## 2. Readiness

Plans 050-053 individually closed with recorded proof; no critical auth/capability/permission defects unresolved. R001 live i2pd 2.61.0 byte transport is already evidence-closed and may be reused as substrate evidence. Broad SAM router portability lives in dedicated i2pr-sam; no repeating i2pd/Java/i2pr combinations.

## 3. Non-negotiable invariants

I2P-only upstream; no DCC/HTTP/DNS/clearnet; nonloopback downstream bind fails. Auth required for all TCP/Unix operator access. ClientId stable per profile, SessionId fresh per attachment. No double CAP replies/registration, wrong-identity history leak or user-chat replay after ambiguous disconnect. Resource ceilings enforced before admission; shutdown drains bounded work and releases SAM sessions/store lock. SQLCipher and OTR remain separate protections; the bouncer never owns OTR keys.

## 4. Test topology and qualification claim

Use the production executable, production auth and listener, actual Store/RuntimeController/SamProvider, and a local mature independent router (i2pd supported). Build an independent IRC server/service on the I2P side, or connect to a known controlled in-network IRC endpoint with permission. Record router version, SAM endpoint, IRC service shape, exact commands/transcripts and which side is real vs fixture. A test-only fake router is acceptable for broad deterministic failure matrix but cannot substitute for final one-real-router product-path evidence.

One legacy IRC client without soju-specific extensions and one modern IRCv3/CAP/SASL client should be exercised end-to-end (or a real client plus independent exact-wire harness for the other). No external router-to-router IRC proof: verify the bouncer's SAM product path once.

## 5. Required integrated scenarios

1. Fresh encrypted init, start, authenticate, configure existing I2P Network and attach; exchange IRC messages, join/part and retain history.
2. CAP-first PASS and SASL registration, selected Network and unbound control-only mode; BouncerServ actions only after trust.
3. Two simultaneous clients with distinct stable profiles; disconnect one and reconnect, independent history/cursor behavior; default and explicit network binding.
4. Router unavailable at start, router restart during steady state, stalled upstream, lost stream, reconnect/recovery with existing global budget and no repeated non-idempotent chat.
5. Process stop/restart while attached: restored desired Networks/channel state, fresh generation and SessionIds, stable ClientIds/history/SQLCipher FTS behavior, no leaked listener/lock.
6. Auth failure, missing/wrong key, unsafe perms/symlink, duplicate daemon, nonloopback bind, oversize/incomplete handshake, concurrent idle handshake flood, stop under full queues, cleanup.
7. Static/network guard negative fixtures and no production HTTP/DCC/generic dial; logging redaction and no host/router/build fingerprint leak.
8. Real-client OTRv3 query/AKE/data/fragment carriage where a suitable independent endpoint client can be configured; if unavailable, mark unrun explicitly and retain M009 opaque-carriage evidence without claiming real-client compatibility.

## 6. Verification commands and evidence retention

Run focused daemon/auth/store tests and exact IRC wire conformance; sh scripts/verify.sh full; rustup run 1.88.0 sh scripts/verify.sh full. Qualify Linux and macOS runtime paths when available; Windows TCP build/runtime and Unix absence must be stated, not guessed. Check intended SBC runtime load qualitatively or with controlled resource counters, and record admission ceiling/budget behavior. Preserve sanitized terminal transcripts, environment/versions and test artifacts without endpoint credentials or private destination leakage.

If live i2pd/IRC service is not available, do not claim M010 complete. Record an operational evidence blocker and defer closure without fabricating a green result.

## 7. Documentation and disposition

Update README to describe real daemon start/stop, client configuration, auth and network binding, supported platforms, SQLCipher provisioning, limits and privacy tradeoffs. Reconcile plans/registry.md, plans/subsystems/standalone-daemon-roadmap.md and plans/002-long-term-roadmap.md with actual closures, not anticipated ones. Do not mark install/service packaging, managed router R002 or future built-in client complete.

## 8. Acceptance and stop conditions

A normal local IRC client can authenticate, use a live I2P IRC Network through the production SAM provider, disconnect/reconnect and preserve the correct durable state across daemon restart; negative authority/resource/security cases pass; exit leaves no orphaned owner, listener, Store worker or lock; all required verification runs pass and source proves no unintended socket authority. Any correctness or security finding gets a numbered corrective before M010 closure. No relax-and-rerun or scope substitution.

## 9. Closure output

Create plans/closure/standalone/054-status.md with implementation commit lineage (050-054), requirement-to-evidence matrix, exact command and external test outputs, platform/MSRV outcomes, anonymity/security review, unresolved limitations, and downstream roadmap disposition. Only then change registry M010 to closed. The next packaging/service milestone and R002 integration remain distinct.
