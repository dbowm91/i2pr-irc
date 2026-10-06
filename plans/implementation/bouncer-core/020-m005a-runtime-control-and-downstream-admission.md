# Bouncer Core M005-A / Plan 020 — Runtime Control and Downstream Admission Foundation

Status: ready

Repository baseline: 44c65a7328d29f8a03775c41c4d3cf99c30fa1f3

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Architecture authority:

- plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md
- architecture/control-session.md

Primary class: infrastructure + invariant

## 1. Objective

Add the process-level ownership needed by M005 without moving ordinary upstream or bound-session ownership out of NetworkOwner.

Deliver:

- one bounded RuntimeController that owns process-level Network lifecycle;
- a cloneable bounded RuntimeControlHandle for typed control requests;
- startup restoration that constructs exactly one live owner for each eligible durable Network;
- dynamic Network create/change/delete/reconcile with explicit durable/live convergence;
- DownstreamAdmission for an accepted local stream before Network selection;
- a one-time PreparedSession transfer into a selected NetworkOwner;
- a registered unbound control-only session state;
- a bounded process control/status snapshot with monotonic revision;
- legacy direct pre-bound attachment compatibility.

Do not advertise soju.im/bouncer-networks in this plan. Plan 020 builds the authority and lifetime model that Plan 023 will expose.

## 2. Readiness

Ready.

M004 and Corrective 019 are closed. Research 006 and ADR-0003 freeze the ownership decision needed by this plan. No router integration is required.

The baseline already provides:

- StoreHandle typed Network load/save/remove operations;
- NetworkCatalog, SupervisorHandle and ResourceLedger;
- NetworkOwner with one-owner-per-Network generation ownership;
- SessionId and ClientId separation;
- bounded downstream writer queues and LineDecoder;
- process-wide ReconnectScheduler;
- static no-clearnet/network-boundary verification.

## 3. Invariants

1. NetworkOwner remains the only mutable upstream owner for one Network.
2. A bound downstream session is ultimately owned by exactly one NetworkOwner.
3. An admission session may be unbound, but it cannot carry ordinary upstream IRC traffic until transfer succeeds.
4. A SessionId is allocated once and never changes during admission-to-bound transfer.
5. Buffered bytes, CAP state and registration facts survive transfer exactly once.
6. A session cannot bind to two Networks and cannot BIND after registration completion.
7. Runtime control is typed and bounded; no client task receives StoreHandle, I2pStreamProvider, raw catalog internals or a generic network API.
8. Durable Network state remains restart authority.
9. A deleted Network cannot retain a live ghost owner.
10. A configuration update cannot leave an old owner transmitting while durable commit state is unknown.
11. Process snapshots and diagnostics contain no credentials or private I2P destination material.
12. Existing direct pre-bound attachment remains supported.

## 4. Scope

### RuntimeController

Add one process controller that owns:

- the I2pStreamProvider handle used to construct NetworkOwner instances;
- NetworkCatalog;
- the live task registry for supervised Networks;
- stop handles and joins for those owners;
- ReconnectScheduler and ResourceLedger sharing;
- a bounded control request queue;
- a monotonic catalog/control revision;
- startup restore, create, change, delete, reconcile and orderly process stop.

The controller must not parse ordinary upstream IRC commands and must not become a replacement Network owner.

A stop path must remain reachable even if the ordinary control queue is saturated. Use a dedicated stop/watch mechanism or an equivalent bounded side channel.

### Durable display name

The canonical Network domain already includes a display name, while NetworkRecord currently lacks one. Add a bounded durable display name in this plan so later bouncer-network and administration protocols do not derive human identity from endpoints.

Migrate schema 2 to schema 3 transactionally.

Requirements:

- existing rows receive a deterministic non-secret fallback derived only from NetworkId, such as network-<id>;
- display names are bounded and validated;
- uniqueness is not Network identity; NetworkId remains canonical;
- a display-name collision is allowed only if every control API remains unambiguous by NetworkId. If implementation needs name lookup, either reject duplicate names explicitly or require NetworkId for mutation.

Do not expose endpoint text in the fallback name.

### DownstreamAdmission

Refactor the current bound-session construction so the accepted stream can exist before a Network is selected.

Admission owns:

- SessionId and ClientId;
- read half;
- LineDecoder, including buffered unread bytes;
- bounded writer task and queues;
- NICK/USER/CAP registration facts;
- negotiated bouncer-level capability set;
- optional selected NetworkId;
- RuntimeControlHandle.

An accepted local stream already carries a trusted/authenticated ClientId from LocalAcceptor. This plan does not create a second user database. Concrete loopback TCP adapters remain responsible for canonical local authentication before core admission.

### PreparedSession transfer

Define a one-shot transfer object containing the state a bound SessionTask needs.

Transfer requirements:

- no new socket or copied byte stream;
- no decoder reset;
- no writer respawn;
- no SessionId change;
- no duplicated registration projection;
- no command after the registration boundary may be lost or interpreted by both admission and NetworkOwner.

NetworkOwner takes ownership after successful bounded attach. On refusal, admission terminates explicitly rather than holding a half-bound session.

### Unbound control session

A session that finishes registration without a selected Network may remain connected as local control only.

It may:

- PING/PONG;
- CAP operations supported by the control surface;
- bouncer-control operations added in later plans;
- QUIT.

It must refuse channel/user/upstream commands deterministically. It creates no I2P stream.

## 5. Dynamic Network mutation semantics

### Startup

Load the complete durable Network set first, validate bounds, then construct owners. Each durable Network gets at most one live owner.

A single Network that cannot activate must surface a disconnected/error state without causing a second owner or silently deleting its durable configuration.

### Create

1. validate complete candidate and global limits;
2. persist candidate;
3. start one owner;
4. if activation fails after a confirmed commit, keep durable DesiredState and expose a disconnected/error state.

Do not roll back confirmed durable configuration merely because the current live activation failed.

### Change

For fields requiring reconnect:

1. quiesce/stop the current owner;
2. persist the complete candidate record;
3. if commit state is unknown, re-read durable state;
4. start exactly the record proven durable.

For a rejected/rolled-back mutation, restart the previous durable record.

### Delete

1. quiesce/stop the live owner;
2. remove durable state;
3. on unknown commit state, re-read;
4. if the record survives, restart it; if absent, deletion is complete.

A durable deletion and a live owner may never disagree indefinitely.

## 6. Process control/status model

Add a bounded process control projection separate from generic diagnostics.

The projection should provide only the fields later control protocols need, for example:

- NetworkId;
- display name;
- connected/connecting/disconnected state;
- bounded error class;
- configured preferred nick and non-secret identity fields where appropriate;
- revision.

Do not put endpoint/private destination or credential data into CatalogStatus merely because an authenticated control view needs it.

Publish complete bounded snapshots via watch-like latest-state semantics rather than an unbounded event log. With MAX_SUPERVISED_NETWORKS=64, complete snapshots and per-session diffs are finite.

## 7. Work packages

A. schema 2→3 Network display-name migration;
B. RuntimeController and live-owner registry;
C. startup restore and typed mutation requests;
D. DownstreamAdmission and early writer ownership;
E. PreparedSession one-shot transfer into NetworkOwner;
F. unbound control-only registration state;
G. bounded revisioned control snapshot;
H. legacy attachment compatibility and documentation;
I. closure evidence.

## 8. Failure, restart and contention semantics

- controller control-queue full: explicit overload, never secondary queue;
- admission output queue full: terminate or explicit bounded refusal according to existing downstream disposition semantics;
- selected Network absent/stopped: explicit bind/attach failure, no fallback Network;
- owner start fails after durable create: record remains durable and visible as disconnected/error;
- unknown store commit: re-read before choosing owner state;
- controller shutdown: stop all admissions/control sessions and owners with owned joins;
- session transfer canceled: object is consumed or explicitly terminated; it cannot be retried into another owner;
- process restart: no admission/control connection survives; durable Network configuration does.

## 9. Compatibility

The existing direct path that already knows NetworkId must continue to work.

Do not remove the existing SupervisorHandle attachment semantics until the new path has equivalent tests. It may become an adapter onto PreparedSession after equivalence is proven.

No protocol capability is newly advertised merely because the internal controller exists.

MSRV remains 1.88 unless a separately reviewed dependency forces a decision.

## 10. Tests

Required focused tests include:

- unbound admission completes registration without creating an upstream connection;
- selected admission binds exactly once;
- late/repeated bind is refused;
- missing NetworkId is refused without fallback;
- CAP END, final registration fact and a post-registration command delivered in one read preserve that command exactly once across transfer;
- SessionId and ClientId are unchanged across transfer;
- writer queue/backpressure remains bounded before and after transfer;
- generation loss still owns bound-session teardown;
- startup creates one owner per durable Network;
- create activation failure keeps confirmed durable config;
- change with simulated unknown commit re-reads and starts exactly durable state;
- delete with simulated unknown commit either restores surviving record or leaves no owner when deleted;
- controller queue saturation is explicit;
- controller stop works under ordinary queue saturation;
- control snapshot revision is monotonic and bounded;
- display-name migration preserves every v2 Network and does not expose endpoint text;
- legacy direct pre-bound attach has equivalent registration/fanout behavior;
- static no-clearnet guard remains green.

Add a source-level or type-level regression test proving RuntimeController does not expose a generic dial/listen operation.

## 11. Verification

Run at minimum:

- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets --all-features -- -D warnings
- cargo test -p i2pr-irc-store
- cargo test -p i2pr-irc-runtime
- cargo test --workspace --all-features
- scripts/check-network-boundary.py
- scripts/verify.sh full
- rustup run 1.88.0 sh scripts/verify.sh full

## 12. Documentation

Update:

- architecture/control-session.md with exact landed ownership;
- architecture/network-ownership.md;
- architecture/downstream-session.md;
- architecture/storage.md for schema 3;
- architecture/overview.md;
- plans/subsystems/bouncer-core-roadmap.md;
- plans/registry.md at closure.

Do not rewrite historical M003/M004 closure records.

## 13. Acceptance criteria

Plan 020 closes only when a local stream can exist truthfully before Network selection, transfer once without losing protocol state into exactly one NetworkOwner, remain unbound for local control without creating upstream authority, and dynamically mutate Network DesiredState while live owner state converges deterministically under success, failure and unknown store commit outcomes.

## 14. Stop conditions

Stop for architecture review if implementation requires:

- moving ordinary bound-session fanout/lifetime out of NetworkOwner;
- adding a second upstream owner;
- exposing StoreHandle or I2pStreamProvider to a downstream task;
- adding a generic TCP/DNS/URL connector;
- accepting an unauthenticated loopback TCP socket as Operator identity;
- dropping buffered bytes at transfer;
- an unbounded control/notification queue.

## 15. Closure evidence

Create plans/closure/bouncer-core/020-status.md with:

- implementation commit range;
- ownership/lifetime matrix;
- transfer byte-preservation evidence;
- dynamic create/change/delete failure matrix;
- schema 2→3 migration evidence;
- queue/resource bounds;
- legacy compatibility evidence;
- full verification results;
- findings and M005-B readiness decision.
