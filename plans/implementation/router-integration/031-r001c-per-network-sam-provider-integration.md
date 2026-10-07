# Router Integration R001-C / Plan 031 — Per-Network SAM Provider Integration

Status: closed

Closure: plans/closure/router-integration/031-status.md

Hard dependency:

- plans/closure/router-integration/030-status.md

Authority:

- plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md
- plans/adrs/ADR-0005-explicit-i2p-provider-scope-release.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md
- plans/implementation/router-integration/029-r001a-provider-scope-lifecycle-and-endpoint-foundation.md
- plans/implementation/router-integration/030-r001b-owned-sam31-wire-client-foundation.md

Primary class: integration capability

## 1. Objective

Compose the owned SAM 3.1 client into production I2pStreamProvider with one bounded, long-lived, transient SAM STREAM session per active durable Network.

Prove that IRC reconnects reuse the Network's SAM session, router/SAM session loss causes bounded recreation rather than user-traffic replay, provider release tears down the scope, and the process-wide reconnect scheduler remains the attempt-rate authority.

## 2. Architecture

One provider instance owns a bounded synchronous map:

~~~
NetworkId -> ScopeEntry
             +-- bounded command sender
             +-- stop signal
             +-- join handle
             +-- redacted health snapshot
~~~

Each ScopeEntry owns one async SamScopeOwner task:

~~~
SamScopeOwner
  +-- opaque SAM session id
  +-- control socket
  +-- SAM epoch
  +-- bounded connect-request queue
  +-- control-socket loss watcher/state
~~~

The map ceiling is MAX_SUPERVISED_NETWORKS.

Never hold a map mutex guard across await.

## 3. Lazy creation and reuse

The first provider.connect(network, endpoint) may create the Network scope and establish the SAM session.

Subsequent IRC generation connects reuse that healthy session.

A durable Network without a live owner does not require a pre-created SAM session.

There is no independent provider auto-retry loop.

## 4. Outer connection deadline

The current runtime wraps provider connect in a 120-second ceiling. That is too short for a cold SAM path that may include tunnel construction plus the router's approximately-one-minute STREAM CONNECT timeout.

Reconcile this explicitly.

Recommended production contract:

- outer provider-connect ceiling: 300 s;
- Plan 030 inner phase deadlines remain individually bounded;
- outer cancellation propagates into SAM request/socket cleanup;
- process-wide reconnect permit remains held for exactly this provider attempt.

Rename the runtime constant if needed so it describes provider acquisition rather than generic TCP.

Do not let SAM silently exceed the outer deadline.

## 5. Scope request queue

Use a small explicit bound:

- SAM_SCOPE_REQUEST_CAPACITY = 4.

The current owner should normally have one outstanding request, but the hard bound protects future/control races.

Queue full maps to typed provider overload/failure without creating another scope.

## 6. Session establishment

For unhealthy/new scope:

1. create opaque session ID;
2. connect to loopback SAM bridge;
3. HELLO 3.1;
4. SESSION CREATE fixed R001 profile;
5. increment local SAM epoch;
6. publish healthy redacted snapshot;
7. execute pending STREAM CONNECT.

Only one SESSION CREATE may be in flight per Network.

Different Networks remain constrained by the existing global reconnect scheduler because owners call provider only after admission.

## 7. Stream connect

For a healthy scope:

- open fresh SAM data socket;
- HELLO 3.1;
- STREAM CONNECT with scope session ID;
- return raw stream.

A STREAM result that means the session ID no longer exists invalidates the current epoch before returning failure.

Peer-specific CANT_REACH_PEER/TIMEOUT does not destroy an otherwise healthy SAM session.

## 8. Control-session loss

After SESSION CREATE, observe the control socket for EOF/error with bounded reads.

SAM 3.1 has no required PING/PONG keepalive.

On control loss:

- mark current epoch/session unhealthy;
- do not autonomously recreate immediately;
- fail outstanding not-yet-returned connect requests with typed session loss;
- let NetworkOwner apply its existing backoff/global scheduler;
- the next admitted connect lazily creates a new transient session.

INVALID_ID on STREAM CONNECT also invalidates the session even if EOF has not yet been observed.

## 9. Returned stream ownership

After STREAM STATUS OK, the data stream belongs to the Network connection generation.

Provider release need not own/track returned streams because RuntimeController releases only after stopping/joining the Network owner, which has dropped its generation stream.

Tests must prove that ordering.

Do not add a second hidden stream registry.

## 10. release(NetworkId)

Implement release as:

1. remove/mark scope closing under short map lock;
2. prevent new requests entering old scope;
3. signal owner stop;
4. await join under fixed release deadline;
5. drop control socket/session;
6. remove redacted diagnostics.

Repeated release with no scope is success.

A connect racing release must either be cancelled/refused in old scope or occur after scope removal only when the Network runtime legitimately still exists. RuntimeController delete ordering should make recreation impossible during deletion.

## 11. Error mapping

Map SamError to ProviderError deliberately:

- bridge unavailable / session lost / peer unavailable -> Unavailable;
- phase timeout / STREAM TIMEOUT -> Timeout;
- cancellation/release -> Cancelled;
- malformed/unsupported/session protocol/random-source failure -> Failed.

Do not map SAM/session failures to IRC Registration/Nick/SASL terminal classes.

Keep richer classes only in redacted provider diagnostics if useful.

## 12. Provider diagnostics

Expose bounded secret-free counters/snapshot:

- live scopes;
- healthy scopes;
- session creations;
- session losses;
- stream connect attempts/success/failure;
- releases;
- current/peak queued requests;
- numeric SAM epoch if needed.

Never expose session nickname, public/private Destination, remote endpoint text, router MESSAGE, or raw SAM command/reply.

## 13. Deterministic tests

Identity/session lifecycle:

- two Networks => two SESSION CREATEs and distinct opaque IDs;
- one Network across 100 IRC reconnects => one SAM session while bridge remains healthy;
- peer CANT_REACH does not recreate session;
- STREAM INVALID_ID invalidates and next attempt creates exactly one new session;
- control EOF invalidates;
- release destroys scope;
- delete/shutdown leave zero scopes.

Bounds:

- scope count bounded by runtime Network ceiling;
- request queue max/max+1;
- concurrent first connect cannot create duplicate scope owner;
- no task leak after churn;
- no map lock held across await.

Cancellation:

- cancel during bridge TCP;
- HELLO;
- SESSION CREATE;
- stream HELLO;
- STREAM CONNECT;
- release while pending;
- outer timeout using injected short timing.

Core integration:

- actual RuntimeController<SamProvider>;
- fake SAM bridge + fake IRC upstream over returned raw stream;
- IRC registration succeeds;
- forced IRC EOF reconnects through same SAM session;
- forced SAM session loss creates new session then reconnects IRC;
- no ambiguous user-message replay;
- reconnect admission counts remain correct.

## 14. Network boundary

Production scan must show generic TCP only under crates/sam, no resolver/HTTP/SOCKS/proxy, and runtime/core accessing only I2pStreamProvider.

## 15. Acceptance criteria

Plan 031 closes when deterministic production composition proves per-Network session ownership, reuse across IRC reconnects, recreation after SAM loss, release/cleanup, bounded resources, and no external-router portability claim yet.

## 16. Stop conditions

Stop if one SAM session cannot safely serve repeated STREAM CONNECTs, correct liveness requires hidden 3.2-only behavior, SAM syntax leaks into runtime/core, session recreation bypasses global reconnect admission, or resource ownership becomes unbounded.

## 17. Closure evidence

Create plans/closure/router-integration/031-status.md with per-Network/session matrix, reuse/recreation counts, error mapping, release/task/resource baseline-peak-settled evidence, full bouncer verification and Rust 1.88, and Plan 032 readiness.
