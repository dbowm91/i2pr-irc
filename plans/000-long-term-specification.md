# i2pr-irc Long-Term Architecture and Product Specification

Status: canonical long-term implementation directive

Companion documents:

- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md

The keywords MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, and MAY are normative.

## 1. Product definition

i2pr-irc is a lightweight persistent IRC bouncer whose upstream network capability is exclusively I2P.

It maintains long-lived IRC network sessions on behalf of one local operator, permits multiple local IRC clients to attach to those sessions, preserves channel and history state across client disconnects, and tolerates I2P path instability without turning transient outages into reconnect storms or state corruption.

The product is not a generic TCP IRC bouncer. I2P-only transport is a security boundary rather than a configuration profile.

Initial deployment:

~~~
local IRC client(s)
        |
 local-only listener
        |
   i2pr-irc core
      /     \
 history   network supervisor(s)
                |
         I2P stream provider
                |
          local I2P router
                |
           I2P IRC service
~~~

Router-specific APIs sit below the I2P stream-provider boundary. IRC protocol state MUST NOT depend on SAM syntax, i2pr private crates, or Proposal 170 wire details.

## 2. Primary goals

i2pr-irc MUST provide:

1. Correct persistent IRC bouncer behavior with a small, auditable runtime.
2. Many independently supervised upstream IRC networks.
3. Multiple local downstream clients with deterministic response routing.
4. Durable history and per-client/read-state behavior for IRCv3 and legacy clients.
5. Modern IRCv3 capability mediation rather than blind byte forwarding.
6. Robust behavior under high latency, stalls, repeated disconnects, path re-establishment, partial I/O, and reconnect bursts.
7. An anonymity-oriented policy removing avoidable local identity leakage and alternate network paths.
8. A router-neutral I2P stream interface with SAM as the first portable adapter.
9. First-party i2pr managed-app integration once its public app API is stable.
10. Optional least-privilege router-control integration only after a concrete product need and stable application authorization contract exist.

## 3. Non-goals

The initial product MUST NOT become:

- a clearnet IRC bouncer;
- a general TCP client or proxy;
- a SOCKS or HTTP CONNECT client;
- a web browser, URL previewer, webhook system, or file-upload service;
- a DCC endpoint;
- an ident server;
- a general router administration console;
- a multi-tenant hosted bouncer;
- a scripting/plugin host comparable to ZNC modules;
- a replacement I2P router, SAM bridge, I2CP implementation, or Proposal 170 implementation.

One local operator per process is sufficient for the first product generation. Multiple named downstream clients and many upstream IRC networks are required. Multi-user hosting is deferred.

## 4. Architectural principles

### 4.1 Structural absence beats secure presets

There is no generic upstream socket API in the bouncer core.

An upstream endpoint is an I2P endpoint. Resolution is performed by an I2P provider. Production upstream code MUST NOT call host DNS or reinterpret an I2P name as a host/IP endpoint.

### 4.2 IRC core and router adapters are separate

The core owns IRC wire semantics, IRC/IRCv3 state, history semantics, client routing, reconnect policy, and anonymity filtering.

A router adapter owns only the mechanism required to obtain an ordered reliable I2P byte stream and narrowly scoped router functions explicitly exposed through an interface.

### 4.3 One upstream owner per network

Each configured Network has one logical NetworkSupervisor that owns its current connection generation, registration/capability state, channel state, liveness, reconnect state, bounded outbound queues, and request correlation.

Downstream clients submit typed intents and do not mutate network state directly.

### 4.4 Persistence is not live protocol authority

Durable storage records configuration, desired state, history, and cursors. Live supervisors remain authoritative for current connection state.

Restart reconciliation distinguishes durable desired state from stale observations of a previous process generation.

### 4.5 Capability mediation is explicit

The upstream capability set SHOULD be stable for a configured bouncer version/policy and MUST NOT vary merely because a different local client connected.

Downstream capabilities are advertised only when the bouncer itself can provide their semantics.

### 4.6 Bounded asynchronous behavior

Every queue, history request, batch, message, line, tag set, pending correlation, reconnect attempt, and diagnostic collection has an explicit bound.

Slow downstream clients cannot cause unbounded upstream buffering. Storage latency cannot starve PING/PONG or registration control traffic.

### 4.7 Failure ambiguity is represented

IRC over a stream does not provide transaction semantics.

If connection failure occurs after bytes for a non-idempotent command may have reached the peer, delivery can be unknown. The bouncer MUST NOT automatically replay user chat or arbitrary commands across a generation boundary unless semantics make replay safe.

### 4.8 Time is injectable

Reconnect, registration, liveness, history ordering assistance, and deterministic fault tests require explicit time ownership. Core state machines SHOULD consume injected clock/timer interfaces.

## 5. Canonical runtime model

~~~
Bouncer
+-- Operator
+-- NetworkCatalog
|   +-- Network
|   |   +-- NetworkSupervisor
|   +-- ...
+-- DownstreamSessions
+-- HistoryStore
+-- ConfigurationStore
+-- I2pStreamProvider
+-- LocalAcceptor
~~~

History writes are ordered by a bouncer-local monotonic sequence. Server time and msgid are preserved as source metadata rather than replacing local ordering.

## 6. IRC and IRCv3 contract

The wire layer MUST enforce the traditional 512-byte non-tag IRC message limit and IRCv3 tag limits, reject framing violations and oversized input before unbounded allocation, preserve unknown commands/numerics/tags when safe, handle partial reads/writes, and encode only valid bounded output.

Initial IRCv3 priorities:

- CAP 302 and cap-notify;
- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message;
- account-tag and account-notify;
- away-notify;
- extended-join;
- chghost;
- invite-notify;
- multi-prefix;
- SASL;
- setname;
- standard replies where useful.

History SHOULD support draft/chathistory and draft/read-marker only behind explicit draft-version handling. The bouncer SHOULD support soju.im/bouncer-networks after the multi-network model is closed. Legacy one-network-per-downstream operation remains required.

## 7. Connection and resilience model

The runtime distinguishes provider unavailable, naming, stream establishment, IRC registration, capability negotiation, SASL, registered/healthy, stalled, disconnected/backoff, and stopping.

Timeouts are phase-specific. A single short generic socket timeout is not acceptable.

Reconnect uses exponential backoff with bounded jitter, a ceiling, and a global attempt budget so many networks do not reconnect simultaneously after router/path disruption.

PING/PONG and registration traffic receive priority over normal chat queues.

Reconnect restores desired channel state deliberately and does not replay arbitrary pre-disconnect traffic.

## 8. History and client synchronization

Each HistoryEvent records network, buffer, local monotonic sequence, receive time, optional server-time, optional upstream msgid, event classification, protocol fields required for replay, and audience metadata.

Per-client cursors are independent and monotonic.

Legacy clients may receive bounded automatic backlog playback. Clients negotiating draft/chathistory SHOULD use query-based history to avoid duplicate replay.

## 9. Anonymity and privacy invariants

The application MUST NOT create an upstream clearnet path.

It MUST NOT derive IRC-visible values from host name, login name, machine ID, OS/router version, local paths, process IDs, or ambient environment values.

DCC is unsupported and must not cause direct connection behavior.

CTCP is mediated. ACTION remains normal chat. Environment-oriented queries such as VERSION, TIME, USERINFO, SOURCE, FINGER, and CLIENTINFO are blocked, suppressed, or answered only according to a documented fixed non-identifying policy.

Client-only tags are privacy-sensitive. Default policy is deny-unrecognized with a reviewed allowlist. CLIENTTAGDENY should reflect policy where applicable.

Logs redact secrets and authentication payloads. Raw protocol logging is disabled by default.

This does not prevent correlation through reused nicks/accounts, channel choice, writing style, message content, or deliberate disclosure.

## 10. Downstream local access

Standalone operation may expose Unix-domain sockets, loopback TCP, and equivalent local IPC on supported platforms. Non-loopback downstream listening is outside the initial contract.

Local TCP requires bouncer authentication.

Future i2pr managed-app operation SHOULD receive local accepted streams from the trusted app runtime rather than gaining general bind/listen authority.

## 11. Router integration

### Portable SAM

SAM is the first portable adapter.

Initial compatibility target is stable SAM 3.1 STREAM functionality for long-lived sessions, naming, and outbound stream creation. More recent features require compatibility analysis.

A SAM session is long-lived rather than recreated for each IRC reconnect.

Standalone SAM endpoints are local-router endpoints only by default.

### i2pr

i2pr is the preferred first-party target.

The bouncer consumes a stable public application capability/SDK and MUST NOT import private router internals.

The future adapter maps I2pStreamProvider onto the managed-app capability channel and LocalAcceptor onto a scoped local-listener/accepted-stream capability when available.

### Proposal 170

Proposal 170 is control plane, not IRC data plane.

The bouncer must be fully useful without it. Any future use is optional and least-privilege. A managed app MUST NOT receive a general router administrator credential.

## 12. Storage

SQLite is the preferred initial durable store for state metadata and history. Exact Rust dependency and async ownership are frozen by the storage milestone after dependency review.

Blocking database calls MUST NOT execute on latency-sensitive Tokio tasks.

Migrations are transactional. Failed migration does not start against a partially upgraded schema.

Secrets are classified separately and never emitted in diagnostics.

## 13. Observability

Diagnostics should expose network state/generation, last disconnect classification, backoff, registration/SASL failure, queue pressure, history-store health, and downstream attachment state without secrets or private destination material.

## 14. Verification

Required evidence includes parser golden vectors, fuzz/property tests, deterministic partial I/O, disconnects throughout registration and steady state, reconnect storms, bounded slow-client tests, response-routing tests, history restart tests, redaction/anonymity tests, static guards against unauthorized resolver/clearnet connectors, cross-router SAM evidence, and i2pr end-to-end evidence before integration claims.

## 15. Completion definition

The initial product generation is complete when local clients can maintain many I2P IRC networks, survive repeated realistic path loss, recover desired channel state, query durable history, use the documented IRCv3 set, and do so without an application clearnet egress path or documented local identity leakage.

i2pr integration is complete only when the same core runs through the public managed-app capability boundary with no private router dependency and equivalent fault/security evidence.
