# Standalone M010-C / Plan 052 — Authenticated Registration Handoff and Client Profiles

Status: closed — see `plans/closure/standalone/052-status.md`
Repository baseline for planning: f325e7d5e495e36b1fc7b168c30e4b725756d22c
Primary class: capability + trust-transition invariant
Authority: plans/subsystems/standalone-daemon-roadmap.md; ADR-0003; ADR-0007; Research 010

## 1. Objective

Complete the end-to-end local IRC session path from Plan 051's authenticated socket to existing DownstreamAdmission/NetworkOwner or unbound BouncerServ, preserving registration and IRCv3 state exactly once. Authenticate the Operator, resolve a stable per-client profile to durable ClientId, and determine an explicit or default Network selection without inventing multi-operator roles.

## 2. Readiness

051 must be closed with a bounded authentication checkpoint and local listener authority. Existing DownstreamAdmission::run expects a fresh stream and a trusted ClientId; existing SessionReader stores decoder, pending byte lines, registered nick, USER status, CAP state and writer. StoreHandle::create_client(login) is available; SessionIdAllocator is already distinct from ClientId. Any public API modification should be additive and covered by legacy tests.

## 3. Invariants

- Authenticated operator scope alone permits BouncerServ; profile names are durable cursor identities, not independent users/roles.
- StoreHandle::create_client must not run on unauthenticated login names or failed credential attempts.
- A successfully transferred session has one authenticated ClientId, a fresh SessionId and at most one owner NetworkId; no fallback to a guessed Network.
- CAP replies, CAP ACK/NAK, SASL success, registration welcome, buffered NICK/USER/BOUNCER BIND are neither re-sent nor lost.
- A peer must not switch profile/identity, select two Networks, BIND after registration, or smuggle control commands through pre-auth.
- Clients without bouncer-networks can use a documented configured default Network; clients negotiating it may choose before binding.
- No upstream outgoing IRC traffic, response routing or history leak to another ClientId as a consequence of reconnect or fragmentation.

## 4. Required production changes

Design a typed AuthenticatedRegistration checkpoint consumed by the core's admission/session code, including moved stream read/write ownership or writer handle, decoder/unread byte state, parsed NICK/USER, CAP negotiation status, acknowledged capabilities, optional requested Network selection and authenticated profile identity. Prefer refactoring common SessionReader logic over a second parser, and preserve existing DownstreamAdmission::run for test/mocked already-trusted LocalAcceptor users. Explicit no-double-consume type ownership (private fields, consuming conversion).

Canonical profile syntax must be bounded (for example default plus explicit alphanumeric/_/- label with a fixed length cap), normalized consistently and stored under existing durable clients. One Operator credential may authorize several named clients, but two profiles should have separate history cursor lineage; the Operator can access all via operator controls. Resolve/create ClientId *after* authentication; cap clients at store MAX_CLIENTS. Concurrent first logins with same profile must converge to the same durable ID. Credentials and profiles cannot be interpolated as IRC-visible host/version metadata.

Network selection: use configured default NetworkId for a legacy client when present, otherwise remain unbound control-only; an authenticated soju-capability client may send BOUNCER BIND pre-registration. If profile-specific default mapping is introduced, keep it in bounded process config, not as an inferred network identity; durable NetworkId is authoritative. Reject deleted/unavailable configured networks clearly, never create an upstream network as side effect of attaching.

Check fixed nickname requirement when binding a Network: revalidate against stored/observed record; preserve M007 preferred-nick fallback handling. Preserve conditional capability advertisement per selected Network and refuse rather than falsely ACK unsupported negotiated capabilities.

## 5. Ordered work

A. Specify typed ownership checkpoint with a state transition table and exact wire transcript examples for each auth mode.
B. Add consuming core admission entrypoint and test no double registration/projection or stale writer.
C. Resolve authenticated profile -> stable ClientId under bounded durable worker and allocate fresh SessionId.
D. Integrate network default/unbound and authenticated BIND; retain existing M005 compatibility.
E. Run multi-client protocol/concurrency tests with controlled session and durable store fixtures.

## 6. Failure and restart semantics

Unknown/deleted network: refuse bind without reassigning client to unrelated Network. Controller stopped or store unavailable: close the session with non-secret diagnostics, do not hold an unbounded wait. Profile creation ambiguity: re-read durable profile rather than allocate another ClientId. Network generation loss: retain exactly the existing reconnect/non-replay semantics. On process restart, profile lineage resolves to same durable ClientId while a new SessionId is issued.

## 7. Test matrix

CAP LS -> PASS -> NICK USER; PASS -> CAP LS; CAP LS -> CAP REQ sasl -> AUTHENTICATE PLAIN -> CAP END; NICK/USER before auth; auth and post-auth frames coalesced in one TCP write; arbitrary frame byte split; conditional CAP offered/not offered; one CAP reply per command; no leaked pre-auth BouncerServ; legacy default Network; unbound operator control; two profiles with different cursors; two sessions same profile; concurrent profile first attach; stale/default Network missing; repeated disconnect/reconnect; BIND twice/late.

Use existing deterministic stream/fault suite and real TCP loopback fixtures without requiring a live router. Full boundary, fmt, clippy, tests, fuzz smoke and Rust 1.88 checks are required. No silent CAP downgrade or extra registration numerics allowed.

## 8. Documentation / acceptance / stop

Document normal IRC client fields (server=127.0.0.1, port, PASS or SASL PLAIN, client profile, Network binding), making clear downstream auth differs from upstream IRC network SASL. Accept when at least one PASS-only legacy client and one CAP/SASL client can authenticate and attach deterministically, with preserved state and private independent cursors. Stop and write corrective on any duplicate CAP/projection, wrong ClientId, privilege leak or non-idempotent replay.

## 9. Closure

Write plans/closure/standalone/052-status.md with exact commits, authenticated wire fixtures, profile/cursor evidence and stop/recovery review. Promote 053 only after 052 closes.
