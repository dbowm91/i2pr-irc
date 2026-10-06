# Research 006 — M005 Mature-Bouncer and Control-Session Architecture

Status: complete

Repository baseline: 5cb46829061e7483ca66ad2d9022051b4fe01e3a

Source milestone: Bouncer Core M005

## 1. Question

M005 adds mature bouncer behavior after M004 closed the anonymity and adverse-network qualification line. The main architecture question is how a downstream connection can exist before it is bound to one Network, support local bouncer control, and later bind to a selected Network without creating a second owner of upstream state or weakening the bounded-session model.

This research also checks how the remaining M005 feature candidates fit the storage, history, capability, and diagnostics substrate that exists at the M004 closure baseline.

## 2. Sources reviewed

Primary project authority:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md
- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md
- plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md
- plans/closure/bouncer-core/018-status.md
- plans/closure/bouncer-core/019-status.md
- crates/runtime/src/catalog.rs
- crates/runtime/src/session.rs
- crates/runtime/src/downstream.rs
- crates/runtime/src/owner.rs
- crates/runtime/src/capability.rs
- crates/runtime/src/chathistory.rs
- crates/runtime/src/resource.rs
- crates/store/src/model.rs
- crates/store/src/schema.rs
- crates/store/src/worker.rs

External protocol/reference material:

- https://github.com/emersion/soju/blob/master/doc/ext/bouncer-networks.md
- https://github.com/emersion/soju/blob/master/downstream.go
- https://github.com/emersion/soju/blob/master/doc/soju.1.scd
- https://github.com/emersion/soju/blob/master/doc/ext/search.md
- https://ircv3.net/specs/extensions/standard-replies
- https://ircv3.net/specs/extensions/pre-away
- https://ircv3.net/specs/extensions/chathistory
- https://ircv3.net/specs/extensions/read-marker
- https://ircv3.net/specs/extensions/no-implicit-names
- https://ircv3.net/specs/extensions/monitor
- https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs

soju is used only as an interoperability and architecture reference. i2pr-irc implementation remains independent; no source is to be copied.

## 3. Current repository facts

The M004-closed core already has the difficult data-plane properties M005 must preserve:

- one NetworkOwner owns one Network across connection generations;
- SessionTask objects are currently created only after a Network has already been selected;
- NetworkOwner owns each attached SessionTask and its shutdown;
- SessionId is ephemeral and response routing is SessionId-scoped;
- ClientId is durable and supplied by the local downstream boundary;
- NetworkCatalog stores bounded SupervisorHandle values but does not own the provider or supervisor tasks and cannot create a new live Network by itself;
- StoreHandle already supports load, save, and remove Network records, but no runtime control owner serializes those mutations with supervisor start/stop/reconcile;
- durable NetworkId is already exactly the stable local identifier needed for a bouncer-network netid;
- NetworkRecord has endpoint, IRC identity, SASL, and desired channels but no durable display name or mature policy fields;
- downstream CAP is mediated locally and independent of upstream negotiation;
- history, cursors, read markers, response routing, reconnect scheduling, resource accounting, and queue bounds are already in place;
- the core has no generic DNS/TCP connector and I2pEndpoint cannot represent a clearnet endpoint.

The canonical long-term model already places DownstreamSessions beside NetworkCatalog rather than making catalog mutation a Network responsibility. M005 can therefore add a process control/admission layer without changing the one-owner-per-Network invariant.

## 4. soju and bouncer-networks findings

The soju.im/bouncer-networks draft has two connection modes that matter here:

1. a connection may register without selecting a Network and remain a bouncer-control connection;
2. BOUNCER BIND selects a stable netid before registration completes and binds the connection to that Network.

LISTNETWORKS, ADDNETWORK, CHANGENETWORK, DELNETWORK, and network notifications are bouncer-level operations. They are not operations on the currently selected upstream socket.

soju follows the same conceptual split: downstream registration can exist while the network pointer is absent, then a Network is selected before the connection is welcomed. The source is a reference only; the i2pr-irc ownership model is different enough that copying the implementation shape would be inappropriate.

The extension is work-in-progress. Its syntax must therefore live in an isolated adapter, like the existing draft/chathistory adapter, rather than becoming storage schema vocabulary.

## 5. Architecture options

### Option A — permanently catalog-owned bound IRC sessions

Move every SessionTask out of NetworkOwner and let a process-wide session manager own all client sockets forever.

Advantages:

- unbound and bound connections use one lifetime model;
- cross-network control is straightforward.

Disadvantages:

- large M003 ownership change;
- upstream generation loss would need a new cross-owner termination protocol;
- NetworkOwner would no longer own the tasks whose fanout queues and routing lifetime it currently qualifies;
- substantially expands the M005 regression surface before any operator feature lands.

Disposition: reject for M005.

### Option B — pre-bind admission followed by one-time transfer

A process-level admission task owns an accepted local stream only until registration decides whether it is a control-only connection or a Network-bound connection.

For a bound connection, it transfers the existing bounded downstream transport, decoder state, SessionId, ClientId, negotiated capabilities, and validated registration facts exactly once to the selected NetworkOwner.

Advantages:

- preserves NetworkOwner as the sole owner of bound session lifetime and fanout;
- preserves generation-local response routing;
- keeps legacy one-Network attachment behavior;
- limits new ownership to the genuinely new pre-bind/control phase;
- allows a registered unbound connection to remain a local control session.

Disposition: selected.

### Option C — keep every connection pre-bound and emulate bouncer-networks

Pretend the client is bound to a default Network, then reinterpret BOUNCER BIND later.

Disposition: reject. It makes pre-registration Network selection non-truthful and cannot represent a real unbound bouncer control connection.

## 6. Selected process control architecture

M005 adds a bounded RuntimeController and cloneable RuntimeControlHandle.

RuntimeController owns:

- the I2pStreamProvider used to construct Network owners;
- NetworkCatalog;
- the live supervisor registry, including each stop signal and JoinHandle;
- the process-wide reconnect scheduler and resource ledger already shared by owners;
- serialization of durable Network create/change/delete against live owner lifecycle;
- a bounded control command queue;
- a monotonic catalog revision and bounded watch projection for local control notifications.

NetworkOwner remains the exclusive mutable upstream owner. RuntimeController never parses or forwards upstream chat and never owns a Network connection generation.

StoreHandle remains a storage boundary, not the process control plane.

## 7. Downstream admission and transfer

An accepted stream enters DownstreamAdmission with an already established ClientId. That is the current core trust contract: concrete local listener adapters are responsible for authenticating local TCP before presenting a ClientId. M005 does not invent a second authentication database inside the IRC core.

Admission allocates SessionId before any Network is selected.

The admission object owns:

- the read half;
- LineDecoder including any bytes already read past the current complete line;
- the existing bounded downstream writer and its queues;
- registration facts;
- negotiated bouncer-level capabilities;
- optional selected NetworkId;
- a bounded RuntimeControlHandle.

The writer is created at admission time so pre-bind replies cannot block the RuntimeController on a slow local client.

When registration completes with a selected Network, admission constructs PreparedSession and submits a bounded attach command. PreparedSession carries the read half, decoder, writer, session handle, registration state and negotiated capabilities. NetworkOwner then owns that SessionTask exactly as it owns sessions today.

No bytes may be discarded at the transfer boundary. A test must place CAP END, the final registration line, and a post-registration command in one read and prove the post-registration command reaches the bound SessionTask exactly once.

If registration completes without a selected Network, the connection remains a local control session. Channel/user commands are refused locally. It may use supported bouncer-control commands.

BOUNCER BIND is pre-registration only. A registered unbound connection cannot later transform into a bound data connection.

## 8. Bound-session access to process control

A bound session still needs LISTNETWORKS and later bouncer administration without owning RuntimeController.

The selected design is a bounded typed RuntimeControlHandle available to downstream session translation. Bouncer-level commands become typed control requests. They do not become generic NetworkOwner operations and they do not expose StoreHandle to a client task.

The control handle may return bounded structured results. Wire rendering stays in the downstream bouncer-control adapter.

A NetworkOwner may carry the handle so the SessionTask it owns can submit control requests, but the owner does not interpret, persist, or authorize those requests.

## 9. Durable mutation and restart semantics

Dynamic network mutation must converge live state to durable state without pretending SQLite and task startup form one transaction.

Add:

- preflight all limits and validate the full typed NetworkRecord;
- commit the record;
- start the owner;
- if live activation fails after commit, keep the durable record and surface disconnected/error state. Startup reconciliation will retry it. Do not roll back a committed Network because one live activation failed.

Change of connection-sensitive configuration:

- stop/quiesce the current owner first;
- persist the complete candidate record;
- on an error with unknown commit state, re-read;
- whichever complete record is durable becomes authority and is started;
- no old owner continues transmitting while the durable identity is ambiguous.

Delete:

- stop/quiesce the current owner first;
- remove the durable record;
- on unknown commit state, re-read;
- if the record still exists, restart it; if absent, deletion is complete;
- a deleted durable Network must not remain as a ghost live upstream owner.

The controller must have a stop path that does not depend on free capacity in an ordinary command queue.

## 10. Control snapshots and notifications

Diagnostics and privileged control views are different products.

CatalogStatus remains non-secret and excludes endpoint/private destination material.

A local authenticated control projection may include fields needed for bouncer administration, but it must never be logged or reused as generic diagnostics.

The controller publishes a monotonic catalog revision through bounded watch state. soju.im/bouncer-networks-notify can derive deltas per session from the latest bounded Network set. A lagging client never causes an unbounded event backlog.

The process has at most 64 Networks, so a complete control snapshot and diff are explicitly bounded.

## 11. I2P profile for bouncer-networks

The extension was designed around ordinary IRC host/port/TLS connections. i2pr-irc must not import those authority assumptions.

Mapping:

- netid: decimal representation of stable NetworkId;
- name: durable local display name;
- state: connected, connecting, or disconnected from live status;
- error: bounded non-secret last error;
- host: accepted only when the value parses as I2pEndpoint;
- nickname, username, realname: mapped to existing durable IRC identity;
- port: recognized but invalid/not-applicable for this product;
- tls: recognized but invalid/not-applicable for this product;
- pass: recognized but rejected until a reviewed upstream PASS feature exists.

Unsupported standard attributes are rejected explicitly. They are never interpreted by generic URL, DNS, TCP or TLS code.

The extension explicitly permits a bouncer to reject create/change requests. i2pr-irc should therefore prefer a truthful partial management profile over pretending I2P destinations are TCP endpoints.

## 12. M005 feature fit

Detached channels fit the existing desired-channel/history architecture. The durable channel record needs a detached flag, but no second history system. A detached channel remains joined upstream, continues history ingestion, is omitted from normal downstream projection/fanout, and can reuse existing per-client cursors for legacy backlog on reattach.

Auto-away and keep-nick fit the existing configured-versus-observed split. Preferred nick is NetworkRecord.nick; current nick is NetworkState.nick. Presence must count active sessions rather than raw socket count, because draft/pre-away exists specifically for passive history-sync connections.

History search should stay inside the owned SQLite worker. The bundled SQLite used by rusqlite is compiled with SQLITE_ENABLE_FTS5, so a bounded FTS5 sidecar can be implemented without a second search engine. Client text must be converted to a bounded literal/token query rather than exposed as arbitrary FTS syntax or regex.

Richer IRCv3 support should be staged. Low-risk bouncer-provided semantics come before member-state extensions. Unknown or draft syntax stays adapter-isolated.

## 13. Documentation findings

Two architecture documents are stale relative to production:

- architecture/downstream-session.md still says the downstream CAP set is empty and message-tags/batch are not advertised;
- architecture/ircv3.md says server-time and conditional echo-message are already advertised, while production currently withholds both.

These are documentation defects, not runtime defects. They are corrected with this research handoff so M005 plans start from truthful authority.

## 14. Accepted M005 decomposition

The implementation sequence is:

1. Plan 020 / M005-A — runtime control and downstream admission foundation.
2. Plan 021 / M005-B — durable detached-channel policy.
3. Plan 022 / M005-C — presence and preferred-nick policy.
4. Plan 023 / M005-D — bouncer-networks and local BouncerServ administration.
5. Plan 024 / M005-E — indexed history search and CHATHISTORY completion.
6. Plan 025 / M005-F — downstream IRCv3 protocol polish.
7. Plan 026 / M005-G — richer IRCv3 member-state mediation.
8. Plan 027 / M005-H — operator diagnostics, configuration snapshots, and constrained registration actions.
9. Plan 028 / M005-I — integrated qualification and M005 closure.

Only the earliest dependency-ready plan is executable.

## 15. Stop conditions

Re-open architecture review if implementation requires:

- moving ordinary upstream ownership out of NetworkOwner;
- a generic host/port/TLS connector;
- storing soju draft attribute strings as durable schema;
- a second unbounded session/event queue;
- an unbounded history scan or regex engine;
- generic raw command execution;
- giving a downstream session StoreHandle, I2pStreamProvider, or private router authority;
- changing the one-Operator product into multi-user hosting.

## 16. Decision

M005 is ready to be decomposed and Plan 020 is dependency-ready.

The main architecture change is deliberately narrow: add a process RuntimeController and pre-bind DownstreamAdmission, then transfer bound sessions into the existing NetworkOwner model. The remainder of M005 can be built as additive policy/protocol work on top of that boundary.
