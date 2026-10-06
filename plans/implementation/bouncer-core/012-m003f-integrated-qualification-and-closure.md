# Bouncer Core M003-F — Integrated Qualification and M003 Closure

Status: closed — see `plans/closure/bouncer-core/012-status.md`

Blocker cleared:

- `plans/closure/bouncer-core/011-status.md` accepted

Source milestone:

- Bouncer Core M003

Primary class: capability qualification

## 1. Objective

Qualify the complete M003 durable multi-Network/multi-client/history system as one coherent bouncer core and close the M003 roadmap milestone.

This plan is not a feature catch-all. It integrates and stress-tests the contracts delivered by M003-A through M003-E, fixes only bounded defects discovered by that qualification, reconciles documentation/planning, and records the evidence required to unblock M004.

## 2. Invariants under qualification

- one live owner per Network;
- no generic upstream clearnet authority;
- durable DesiredState versus fresh ObservedState;
- bounded store/intent/fanout/history/response queues;
- no network control starvation from storage or clients;
- no ambiguous user-message replay;
- SessionId versus ClientId separation;
- deterministic HistoryEventId ordering;
- monotonic cursors/read marker;
- truthful capability advertisement;
- generation/session fencing of response routes;
- schema migration/restart safety;
- Rust 1.88.

## 3. Qualification matrix

### Multi-network

Run several independent supervisors with mixed:

- healthy;
- registration failure;
- repeated disconnect;
- long backoff;
- stopped/restarted.

Prove one Network's failure/store/history activity does not corrupt another's live state.

### Multi-client

Exercise several SessionIds and ClientIds concurrently:

- fanout;
- independent queue pressure;
- detach/reattach;
- same ClientId replacement attachment;
- concurrent queries;
- stale SessionId completions;
- capability differences.

### Persistence/restart

Exercise:

- clean restart;
- crash-like reopen between durable desired mutation and network observation;
- restart after history writes;
- cursor/read marker survival;
- retention then restart;
- migration/open failure;
- corrupted/incompatible schema disposition.

### Store pressure

Use controlled slow/failing worker fixtures:

- queue full;
- delayed commit;
- failed history append;
- failed desired mutation;
- retention work;
- shutdown with pending work.

Prove PING/PONG/control remains schedulable and no unbounded side queue appears.

### History

Exercise:

- deterministic ordering;
- identical/skewed server times;
- msgid/no-msgid;
- legacy playback;
- chathistory queries;
- no duplicate initial replay;
- cursor ack timing;
- read marker;
- retention/clamping.

### Response routing

Exercise:

- labeled concurrent WHOIS/WHO/NAMES/LIST from several sessions;
- fallback when upstream lacks labels;
- response timeout;
- generation replacement;
- session replacement;
- BATCH multi-response;
- route ceiling.

### IRCv3

Run/extend the durable conformance corpus for:

- message tags;
- server-time;
- BATCH;
- labeled-response;
- echo-message policy;
- current draft chathistory/read-marker adapter.

## 4. Scale/resource bounds

Choose explicit deterministic qualification ceilings rather than claiming unlimited scale.

At minimum include:

- many mostly-idle Networks;
- several clients per Network;
- full bounded fanout queues;
- store queue max/max+1;
- route max/max+1;
- history query max/max+1;
- retention batch max/max+1 where applicable.

Record task/thread/queue counts before and after churn and prove return to bounded steady state.

This is not yet the full adverse-network/reconnect-storm campaign of M004.

## 5. Security/privacy review

Confirm M003 did not introduce:

- DNS/generic TCP/HTTP/proxy egress;
- raw secret diagnostics;
- persisted SessionId/generation;
- ClientId disclosure upstream through labels;
- unreviewed client-only tag forwarding;
- arbitrary SQL/closure execution from network input;
- raw protocol logging by default.

Full CTCP/DCC/anonymity qualification remains M004.

## 6. Corrective handling

If qualification finds a defect that can be fixed without changing architecture or widening capability scope, fix it in this work line and add regression evidence.

If it requires a new durable decision, schema semantic change, major capability expansion, or security-boundary change, stop and register a new corrective/ADR. Do not hide it inside closure.

## 7. Documentation reconciliation

Reconcile:

- canonical docs only where implementation proved clarification is needed without changing direction;
- bouncer-core roadmap current state;
- storage/schema architecture;
- network catalog/session architecture;
- history/cursor semantics;
- capability/response routing;
- draft adapter version;
- dependency review;
- registry.

Remove stale M003-planning language.

## 8. Required verification

Expected final evidence includes:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh full
scripts/fuzz-smoke.sh
rustup run 1.88.0 sh scripts/verify.sh full
cargo tree --locked -e all
~~~

Also run the M003-specific migration/restart/store-pressure/multi-network/multi-client qualification harnesses created by A-E.

## 9. M003 closure criteria

M003 may close only when all roadmap exit conditions are evidenced:

1. many NetworkSupervisors fail/reconnect independently;
2. many downstream clients attach without shared-state races;
3. desired channels survive restart and reconcile after registration;
4. SQLite migrations/restart behavior is proven;
5. history order is deterministic;
6. slow storage cannot starve control traffic;
7. labeled-response routes concurrent replies correctly;
8. bounded command-specific fallback exists without labels;
9. per-client cursors are private/monotonic;
10. server-time/batch/message-tags/echo-message policy is proven;
11. draft chathistory/read-marker are isolated from schema syntax;
12. legacy playback is bounded and does not duplicate negotiated chathistory;
13. dependency/network/secret boundaries remain green;
14. Rust 1.88 passes.

## 10. Closure artifact

Create `plans/closure/bouncer-core/012-status.md` as the M003 closure record.

It must reference closures 007-011 and include:

- implementation commit range;
- final architecture diagrams;
- schema version and migration matrix;
- resource-bound matrix;
- multi-network/client matrix;
- history/cursor/retention matrix;
- response-routing/capability matrix;
- store-pressure/liveness evidence;
- security/dependency review;
- exact verification;
- unresolved findings/severity;
- explicit M004 readiness decision.

## 11. Acceptance

M003 is closed only when the repository can truthfully describe the core as a durable multi-Network, multi-client IRC bouncer with bounded history and modern IRCv3 synchronization semantics while still operating entirely through fake/test I2P stream providers.

Router integration remains out of scope and blocked behind M005.

## 12. Stop conditions

Do not close M003 with:

- a missing predecessor closure;
- conditional migration correctness;
- an unbounded queue;
- history gaps silently represented as complete;
- incorrect cross-client response routing;
- draft syntax embedded as durable schema identity;
- a raised MSRV;
- a newly introduced generic upstream network path.

Register a corrective instead.
