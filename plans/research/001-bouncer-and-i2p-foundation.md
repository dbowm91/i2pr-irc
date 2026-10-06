# Research 001 — IRC bouncer and I2P foundation

Research date: 2026-10-05

Purpose: establish the external protocol, prior-art, runtime, security, and router-integration evidence needed to plan i2pr-irc without copying another bouncer's implementation.

This record is research evidence, not implementation authority. Canonical decisions live in plans/000-long-term-specification.md and accepted ADRs.

## 1. Sources reviewed

### Bouncer prior art

ZNC:
- https://github.com/znc/znc
- https://wiki.znc.in/
- Apache-2.0 licensed project.

soju:
- https://soju.im/
- active upstream is codeberg.org/emersion/soju; the GitHub repository is an archived mirror as of April 2026.
- https://github.com/emersion/soju
- AGPLv3.

The soju GitHub mirror remains useful as source-history/reference evidence but implementation code must not be copied into a differently licensed project. Protocol behavior should be implemented from IRC/IRCv3 specifications and independently designed tests.

### IRCv3

Reviewed:
- https://ircv3.net/specs/extensions/capability-negotiation.html
- https://ircv3.net/specs/extensions/message-tags.html
- https://ircv3.net/specs/extensions/labeled-response.html
- https://ircv3.net/specs/extensions/server-time.html
- https://ircv3.net/specs/extensions/chathistory.html
- https://ircv3.net/specs/extensions/read-marker
- https://ircv3.net/irc/

### I2P

Reviewed:
- SAM v3: https://geti2p.net/en/docs/api/samv3
- I2P specification index: https://i2p.net/en/docs/specs
- Proposal 170: https://www.i2p.net/en/proposals/170-i2pcontrol-expansion/

### i2pr

Reviewed current i2pr architecture and the active native-app planning line:
- docs/architecture/i2pr-api.md
- plans/subsystems/i2pcontrol-proposal-170-roadmap.md
- branch codex/plan-345-native-app-runtime
- its managed-app v1 reference/ADR and Plan 349 corrective.

### Rust ecosystem

Reviewed:
- irc 1.1.0 / irc-proto 1.1.0 on docs.rs;
- rusqlite 0.40.x;
- tokio-rusqlite 0.8.x.

Version observations are research-time facts, not dependency pins.

## 2. ZNC lessons

ZNC demonstrates the mature operator feature envelope expected from a bouncer:

- persistent upstream networks/channels;
- multiple downstream clients;
- replay/buffers;
- SASL and authentication support;
- detached channels;
- reconnect/connect-delay controls;
- administrative control surfaces;
- keep-nick/reclaim and perform-style automation;
- extensibility.

The useful lesson is feature coverage, not architecture cloning.

i2pr-irc should not initially adopt ZNC's broad native/interpreted module model. An anonymity-oriented bundled application benefits from a smaller trusted code and egress surface. Extensions that can execute arbitrary code or introduce network/filesystem side channels would directly conflict with the project threat model.

## 3. soju lessons

soju is the stronger conceptual reference for the bouncer state model.

Relevant behavior includes:

- multiple persistent upstream networks;
- multiple downstream clients per user;
- per-client backlog state;
- channel persistence;
- detached channels;
- auto-away behavior;
- SQLite-backed message storage;
- BouncerServ administrative commands;
- soju.im/bouncer-networks for exposing multiple networks through one downstream connection;
- strong IRCv3 integration.

The current project should borrow these product concepts but implement independently.

A particularly useful design lesson is that history belongs to the bouncer as durable structured state, not only as a rolling line buffer. Per-client cursor semantics should therefore exist below IRCv3 read-marker/chathistory wire syntax.

## 4. IRCv3 foundation

### Capability negotiation

CAP 302 is the appropriate baseline. It provides capability values, multiline CAP replies, and cap-notify.

The bouncer is both an IRC client upstream and an IRC server downstream. Capability sets cannot simply be proxied.

Upstream negotiation should be driven by a stable bouncer policy. Downstream advertisement should reflect semantics the bouncer can actually provide.

This reduces fingerprint coupling between the user's local client and the upstream IRC service.

### Message tags and wire limits

The traditional non-tag portion of an IRC message remains limited to 512 bytes including CRLF.

IRCv3 message-tags defines a separate tag region up to 8191 bytes in the combined server/client case, with individual client/server tag contributions bounded. The parser must enforce these byte ceilings before unbounded allocation.

Unknown well-formed tags are part of forward-compatible protocol state, but client-only tags are also untrusted privacy-bearing data. The anonymity policy should default-deny unreviewed client-only tags while the wire parser remains capable of representing them.

### Labeled response

labeled-response is foundational for a multi-client bouncer because it allows replies such as WHOIS/errors to be routed back to the downstream session that originated a request.

When upstream supports labels, the bouncer should namespace/translate labels to prevent collisions between downstream clients and then restore each client's original label on reply.

Fallback routing for servers without labeled-response must be explicitly bounded and command-specific. It cannot rely on one global FIFO heuristic for all commands.

### History

draft/chathistory treats a bouncer as a server-side history provider and depends on batch/server-time/message-tags for full behavior.

A client negotiating chathistory should not simultaneously receive the same automatic backlog behavior intended for legacy clients.

The spec is still draft and must remain isolated behind a versioned adapter so storage schema does not encode current draft syntax.

### Read marker

draft/read-marker exists specifically to synchronize read position across multiple clients of the same user.

The durable model should therefore hold monotonic per-client/per-buffer cursors independently of MARKREAD syntax.

## 5. Correct bouncer state model

A bouncer cannot be a line proxy. It has to synthesize server state downstream from a persistent upstream session.

Minimum authoritative per-network state includes:

- registration generation and server identity;
- current nick/user/account state;
- CAP availability and negotiated capabilities;
- ISUPPORT tokens and casemapping;
- channel prefixes/types and mode semantics;
- joined channels;
- channel topic/modes;
- membership/nick/account/away observations;
- desired persistent channels;
- pending labeled/fallback request correlations;
- liveness/reconnect state.

Downstream registration must be generated from this model rather than replaying a stale upstream welcome burst verbatim.

## 6. Adverse-network requirements

I2P changes timeout/recovery assumptions.

The implementation should have distinct deadlines for:

- router/provider availability;
- I2P naming;
- I2P stream establishment;
- IRC registration;
- CAP/SASL exchange;
- steady-state liveness;
- graceful shutdown.

The first release should not freeze guessed timeout numbers as protocol requirements. It should freeze the model and bounded configurable ranges, then calibrate defaults with interoperability/fault evidence.

Reconnect requires:

- exponential backoff;
- bounded randomized jitter;
- per-network state;
- a process-wide attempt budget;
- reset rules after a sufficiently stable connection;
- priority for control traffic;
- no network-wide synchronized retry after router restart.

### Delivery ambiguity

If a stream disconnects after bytes have been accepted locally for write, the bouncer may not know whether the IRC server processed the command.

Safe policy:
- never auto-replay PRIVMSG/NOTICE/TAGMSG or arbitrary user commands across a connection generation merely because no response was observed;
- desired-state operations such as rejoin are reconstructed from durable desired state after fresh registration;
- request correlations are generation-scoped;
- UI/diagnostics may report unknown delivery where useful.

This is more important than attempting an illusion of exactly-once semantics IRC cannot provide.

## 7. Deterministic fault harness

The core should be testable without an I2P router.

The fault stream must model failures that an ordered reliable stream can actually expose:

- arbitrary segmentation of reads/writes;
- short writes;
- delayed reads/writes;
- stalls;
- EOF/reset at selected byte/event boundaries;
- bounded backpressure;
- router/provider unavailable before connection;
- connection generations replaced while old tasks still complete.

It should not invent packet reordering or duplicate bytes inside one reliable stream.

Inject time so retry/liveness tests do not sleep in wall-clock time.

This harness should become a reusable qualification tool throughout the project rather than a one-off test fixture.

## 8. Anonymity-specific IRC policy

### No alternate egress

Production upstream interfaces are I2P-only.

No system DNS, generic host/IP connector, SOCKS, HTTP CONNECT, URL preview, webhook, ident, or file upload callback is needed.

Standalone SAM is allowed to connect only to the configured local router endpoint.

### DCC

DCC intentionally creates a direct secondary connection and exchanges endpoint information. It conflicts with the product model.

Both outgoing and incoming DCC negotiation should be blocked or transformed into a local diagnostic. No DCC payload may invoke a connector.

### CTCP

ACTION is ordinary chat semantics.

Environment-oriented CTCP commands can expose client/version/time/environment information. The bouncer should mediate them with a strict allowlist. VERSION/TIME/USERINFO/SOURCE/FINGER/CLIENTINFO should not reach environment-dependent client logic by default.

CTCP PING requires a deliberate policy because it can provide timing/linkability information even though it is not a direct host-information leak.

### IRC identity defaults

USER/realname/nick defaults must be explicit configuration or non-identifying constants, never host-derived.

### Capability fingerprint

The project should not pretend to be another bouncer, but it should avoid changing its upstream CAP request set based on which downstream client happens to be attached.

## 9. SAM integration findings

Official I2P guidance describes SAM as the recommended protocol for non-Java applications and identifies SAM 3.1 as stable. The documentation also notes that i2pd does not support most 3.2/3.3 features.

The portable baseline should therefore target the SAM 3.1 STREAM subset first.

Relevant operations are:

- HELLO VERSION;
- NAMING LOOKUP as required;
- SESSION CREATE STYLE=STREAM;
- STREAM CONNECT.

STREAM ACCEPT is not required for the initial local-client bouncer but is relevant if a later milestone exposes the downstream bouncer itself as an I2P service.

SAM sessions/tunnel pools are intended to be long-lived. The adapter should own a durable session and create many IRC streams through it rather than create/discard a SAM session for every reconnect.

## 10. Proposal 170 findings

Proposal 170 is currently Open and expands I2PControl with router/service/addressbook information and mutations.

It is administrative control, not transport.

i2pr's own roadmap still distinguishes its historically qualified profile from full canonical Proposal 170 conformance and has active/blocked successor work around canonical wire and final interoperability evidence.

Conclusion: i2pr-irc must not depend on Proposal 170 for basic IRC.

A later optional control adapter may use a completed/scoped surface for a concrete need, but direct general router administration would conflict with the managed-app security model.

## 11. i2pr integration findings

Current i2pr already has a substantial runtime-neutral SAM 3.1 protocol layer under i2pr-api and daemon-owned listeners/runtime composition.

The active managed-native-app line establishes a separate-process capability model with direct host networking denied in the secured profile. Its app protocol is still infrastructure-only and Plan 349 corrects message direction/reply semantics, broker reservation, and network-policy defects before downstream stability.

For i2pr-irc this implies:

1. Do not depend on i2pr internal SAM types now.
2. Build against I2pStreamProvider.
3. Use the portable SAM adapter for early cross-router integration.
4. Add an i2pr-specific adapter only when app-scoped I2P streams have a public stable contract.
5. The app runtime also needs a scoped local accepted-stream/listener facility for ordinary local IRC clients; using an unsafe direct-network profile merely to bind localhost would weaken the intended model.
6. Proposal 170, if consumed, must be through a narrow app authorization adapter rather than a general administrator token.

## 12. Rust dependency findings

### IRC libraries

irc-proto 1.1.0 provides Tokio IRC codec/message types and may reduce parser work.

However, a bouncer needs strict bounds, both server/client roles, complete tag preservation, unknown extension preservation, and security-reviewed allocation behavior. Milestone 001 should run a bounded behavioral probe before adopting it.

If it cannot satisfy lossless/strict requirements without substantial wrapping, an owned small wire crate is preferable to importing a higher-level IRC client library.

The high-level irc crate is oriented toward IRC clients/bots and carries functionality the bouncer core does not need. It should not be the default architectural dependency.

### SQLite

rusqlite 0.40.x is current and mature. tokio-rusqlite 0.8.x provides an async handle backed by a dedicated connection thread.

The desired ownership model is one bounded database worker/connection path rather than blocking SQLite operations on Tokio network tasks. The storage milestone should benchmark and review the exact dependency rather than pinning from this research record.

### Supply chain

Because the product is security-sensitive and intends to remain lightweight, every production dependency should have an explicit reason. Prefer small libraries with no build-time network behavior. Review build scripts/proc macros and pin Cargo.lock for application releases.

## 13. Recommended subsystem sequence

Bouncer core:

1. strict protocol/domain/fault foundation;
2. single-network/single-downstream operational vertical;
3. multi-network/multi-client persistence/history;
4. anonymity/adverse-network qualification;
5. mature operator feature set.

Router integration:

1. portable SAM;
2. i2pr managed-app adapter after public capability stabilization;
3. optional scoped Proposal 170 integration only after a concrete need.

## 14. Deferred and subsequently resolved questions

The original foundation research intentionally deferred several decisions. Current status:

- MSRV is now frozen at Rust 1.88 by the M001 closure.
- M001 selected an owned byte-oriented wire codec rather than `irc-proto`; that production decision is closed. `plans/research/002-rust-irc-crate-conformance-plan.md` now asks a narrower question: whether current IRC crates should be conformance oracles, dev-only dependencies, references, or justify a separately planned migration.
- the exact SQLite async wrapper remains an M003 decision;
- local authentication scheme and OS peer-credential integration remain deferred;
- default reconnect/timeout values still require measured live-I2P evidence;
- remote downstream IRC-over-I2P remains a possible later inbound-stream milestone;
- the exact safe client-only tag set remains M004 work;
- extracting the deterministic fault stream remains unjustified without a second independent consumer.

The parser decision is therefore not reopened merely by Research 002. A production replacement requires new evidence and a separately registered migration/corrective plan.
