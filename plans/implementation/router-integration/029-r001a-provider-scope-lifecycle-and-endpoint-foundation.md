# Router Integration R001-A / Plan 029 — Provider Scope, Lifecycle, and Endpoint Foundation

Status: ready for handoff

Repository baseline:

- a8dc46ff2bdfe345453295429c153f58f104a25f

Authority:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md
- plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md
- plans/adrs/ADR-0005-explicit-i2p-provider-scope-release.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md

Primary class: invariant + infrastructure

## 1. Objective

Make the router-neutral provider boundary capable of safely owning one long-lived router scope per durable Network before any SAM-specific production code lands.

This plan scopes I2pStreamProvider::connect by NetworkId, adds explicit idempotent provider-scope release, wires release into RuntimeController durable lifecycle, corrects I2pEndpoint for modern longer Base64 Destinations, and migrates every fake/provider call site without changing IRC behavior.

No production SAM socket is added in Plan 029.

## 2. Readiness

Ready. M005 is closed. ADRs 0004/0005 and Research 007 freeze the ownership/lifecycle contract. No external router is required.

## 3. Current evidence

Current core contract is connect(endpoint) only. RuntimeController stores one Arc provider shared by every Network. NetworkOwner already knows the durable NetworkId and is the only production caller establishing an upstream generation.

Current I2pEndpoint caps all text at 516 bytes and accepts Base64 Destination only at exactly 516 bytes. Official SAM documentation states Base64 Destinations are 516 or more characters depending on key/certificate/signature type.

## 4. Invariants

1. Every provider connect receives exactly one durable NetworkId.
2. NetworkId is scope only and never becomes remote wire identity.
3. Provider release is idempotent.
4. IRC generation ending does not release provider scope.
5. Network delete releases scope only after owner task join.
6. Process shutdown releases all scopes after owner task join.
7. No provider task/session remains after successful release.
8. Release cannot hang delete/shutdown indefinitely.
9. No generic TCP/DNS/upstream socket is introduced here.
10. Fake-provider/fault/reconnect semantics remain unchanged except scope lifecycle observability.
11. I2pEndpoint remains I2P-only and redacted.
12. Rust 1.88 remains the floor.

## 5. Provider contract

Change the trait atomically to:

~~~
async fn connect(
    &self,
    network: NetworkId,
    endpoint: &I2pEndpoint,
) -> Result<Box<dyn ByteStream>, ProviderError>;

async fn release(
    &self,
    network: NetworkId,
) -> Result<(), ProviderError>;
~~~

The Arc forwarding implementation forwards both operations. Do not add an arbitrary closure/callback lifecycle API.

## 6. Runtime migration

NetworkOwner::serve passes self.network to provider.connect under the existing global reconnect permit.

No release call occurs in generation teardown/backoff.

Every fake/test provider records NetworkId for attempts so many-Network tests can assert scope, not only count.

## 7. Delete semantics

Preserve current controller ordering:

1. validate durable Network exists;
2. stop/join owner;
3. publish owner stopped;
4. release provider scope;
5. remove durable Network;
6. remove catalog/record state;
7. publish final state.

If release fails, do not perform durable deletion. Return bounded failure. The durable Network remains configured but stopped and retry may call release again.

If durable removal fails after successful release, the row remains durable and stopped; a later reconcile/start may lazily create a fresh provider scope.

## 8. Shutdown semantics

After each owner is stopped/joined, release that Network scope.

Release failures during shutdown are accounted, do not cause an unbounded wait, and do not prevent attempts to release remaining scopes. Provider drop remains final cleanup.

Introduce a fixed provider-release deadline distinct from IRC connection/register timeouts.

## 9. Change/reconcile semantics

A configuration change preserving NetworkId does not release provider scope merely because the IRC owner is replaced. SAM identity/session is Network-scoped, not IRC-generation-scoped.

A future explicit identity rotation is outside R001-A.

## 10. I2pEndpoint correction

Freeze application endpoint ceiling at 4096 textual bytes.

Base64 Destination acceptance:

- length >= 516 and <= 4096;
- body alphabet A-Z a-z 0-9 - ~;
- optional = padding only at the end;
- at most two padding bytes;
- no whitespace/control/path/host-port syntax;
- no cryptographic Destination validation here.

Hostname and b32/b33-profile forms keep tighter existing bounds. No clearnet endpoint kind is added.

## 11. Work packages

A. provider trait + Arc forwarding.
B. NetworkOwner scoped connect migration.
C. fake/test provider migration with scope observations.
D. RuntimeController release lifecycle.
E. release timeout/error accounting.
F. endpoint length/Base64 correction.
G. static-boundary/regression verification.
H. docs/closure.

## 12. Tests

Provider:

- correct NetworkId for one and many Networks;
- same endpoint on two Networks remains two scopes;
- generation reconnect does not call release;
- delete calls release after owner join;
- repeated release is safe;
- failed release prevents durable delete;
- shutdown attempts every release even if one fails;
- same-Network config change does not force release;
- no fake provider scope remains after successful delete/shutdown.

Endpoint:

- legacy 516-char Destination accepted;
- valid longer Destination accepted;
- 4096 accepted and 4097 rejected;
- legal trailing padding accepted;
- interior/excess padding rejected;
- invalid alphabet rejected;
- hostname/b32 regressions green;
- Debug remains redacted.

Run reconnect scheduler/fault suites, RuntimeController create/change/delete/restart suites, M004/M005 many-Network tests, static network boundary, and Rust 1.88 full verification.

## 13. Compatibility

Internal source-level breaking change only. No SQLite migration. No IRC wire change. No production networking dependency added.

Future managed-i2pr provider must implement the same scope/release semantics.

## 14. Documentation

Update provider/lifecycle architecture and examples. Record that NetworkId is scope and must not appear in remote-visible metadata.

## 15. Acceptance criteria

Plan 029 closes when the repository passes against the scoped provider API, generation churn never releases scope, delete/shutdown do, modern bounded Destinations are accepted, no production SAM socket exists yet, and Rust 1.88/boundary gates are green.

## 16. Stop conditions

Stop for architecture review if release requires SAM-specific knowledge in RuntimeController, correct delete semantics require a second durable writer, NetworkId must be exposed to the router, endpoint correction requires generic DNS, or lifecycle requires unbounded background work.

## 17. Closure evidence

Create plans/closure/router-integration/029-status.md with trait migration matrix, delete/shutdown ordering, provider scope/release counts, endpoint vectors, full verification/MSRV, and explicit Plan 030 readiness.
