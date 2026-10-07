# Router Integration R001-D / Plan 032 — SAM Cross-Router Qualification and R001 Closure

Status: closed

Hard dependency:

- plans/closure/router-integration/031-status.md (met)

Closure: plans/closure/router-integration/032-status.md

Operational dependencies:

- available Java I2P, i2pd, and i2pr SAM test environments for claims made.

Primary class: capability qualification

## 1. Objective

Qualify the owned SAM 3.1 provider against independent router SAM servers and close R001 without conflating SAM API compatibility with router-to-router I2P interoperability.

This plan owns live evidence, restart/failure qualification, privacy/boundary review, documentation reconciliation, and R002 readiness disposition.

## 2. Claim separation

Every evidence row identifies which claim it proves.

SAM server compatibility:
the owned client correctly talks to one router implementation's SAM 3.1 bridge.

I2P stream product path:
the resulting stream exchanges exact application bytes through that router.

Cross-router network interoperability:
traffic traverses between distinct router implementations over an I2P network/testnet.

Do not infer the third from the first two.

i2pr local self-composition/SAM acceptance is useful protocol/product evidence but is not automatically router-to-router interoperability.

## 3. Router matrix

Target where environments exist:

- Java I2P;
- i2pd;
- i2pr.

For each record:

- exact version/revision;
- bridge endpoint/config;
- HELLO;
- SESSION CREATE;
- STREAM CONNECT;
- endpoint forms tested;
- exact byte transfer;
- restart/closure behavior;
- result classification.

Missing environment must be recorded as not-run / operational evidence unavailable, never pass.

## 4. Independent peer fixture

Do not make the owned production client test itself as both sides.

Use an independent test-side mechanism, such as:

- the project's standalone SAM library once usable;
- a pinned reviewed Yosemite revision;
- router-native test tunnel;
- another independently implemented SAM client used by i2pr.

Any test-only dependency/source is pinned and provenance recorded. It must not become a production dependency.

## 5. Minimum live compatibility sequence

Per router:

1. start/verify local SAM bridge;
2. create owned transient STREAM session;
3. create independent peer destination;
4. owned client connects by full Destination;
5. exchange binary fixture containing NUL, LF/CRLF, invalid UTF-8, and SAM-looking text;
6. repeat with supported b32 and hostname forms;
7. close stream but keep session;
8. open second stream and prove session reuse;
9. close/restart bridge/router and prove old session unavailable;
10. restore bridge/router and prove later bouncer attempt creates a new transient session.

## 6. Real bouncer scenario

Run actual RuntimeController + SamProvider against a controlled IRC-over-I2P fixture.

Evidence:

- upstream IRC registration;
- capability negotiation;
- desired channel join;
- downstream client attachment;
- message exchange;
- forced IRC stream disconnect => same SAM session reused;
- forced SAM/router session loss => provider session recreated under existing backoff;
- desired channels reconcile;
- no user chat replay across ambiguous disconnect;
- response routing/history remain correct.

Do not bypass I2pStreamProvider.

## 7. Many-Network resource check

Because R001 deliberately uses one SAM session/tunnel pool per active Network, qualify representative scale.

At minimum record 1, 2, 8, and the largest practical local-router count in the test environment.

The repository hard ceiling remains 64; live router limitations do not silently change that code ceiling.

Record non-secret bounded observations such as session count, connect latency buckets, failures/timeouts, and optional process/router resource observations.

If per-Network tunnel cost is operationally unreasonable, stop before R001 closure and register a new ADR for explicit identity grouping/resource policy. Do not silently collapse to a shared Destination.

## 8. Fault/restart matrix

Live where practical, deterministic otherwise:

- SAM bridge absent at startup;
- bridge starts later;
- bridge restart during idle session;
- bridge restart during STREAM CONNECT;
- bridge restart during active IRC stream;
- peer unreachable;
- STREAM timeout;
- invalid destination;
- malformed SAM reply via deterministic fixture;
- control EOF;
- Network delete during pending connect;
- process shutdown with several scopes.

Assertions must match Plan 031 typed provider semantics.

## 9. Privacy/security audit

Verify:

- bridge endpoint loopback only;
- no system DNS;
- no clearnet fallback;
- session ID contains no NetworkId/display/nick/build data;
- complete Destinations/private Destination/router MESSAGE absent from logs/diagnostics;
- SESSION STATUS private material zeroized/not retained;
- one Network SAM identity is not reused by another;
- IRC anonymity/DCC/CTCP policy remains unchanged above provider boundary.

## 10. Static/dependency audit

Run production source boundary scanner, dependency-tree review, unsafe review, Tokio net-use path review, random-source dependency review, and Rust 1.88.

No external SAM production crate may appear.

## 11. Portability disposition

Minimum recommended closure bar for a portable SAM claim:

- Java I2P SAM compatibility passed;
- i2pd SAM compatibility passed;
- deterministic adapter/fault suite passed;
- one actual bouncer-over-I2P controlled scenario passed;
- i2pr row recorded truthfully according to available router/network evidence.

If Java I2P or i2pd cannot be run, implementation may be complete but R001 portability remains evidence-blocked.

## 12. Documentation reconciliation

Update README standalone operation, router-adapter architecture, SAM bridge configuration, security model/network-boundary exception, troubleshooting/error classes, router roadmap, and registry.

Document:

- per-Network transient SAM identity;
- identity changes after provider/router-session recreation/process restart;
- one tunnel pool per active Network implication;
- local bridge only;
- no persistent Destination support in R001.

## 13. Verification

Run full repository floor plus live qualification scripts:

~~~
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo tree --locked -e all
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Live scripts fail closed when requested prerequisites are missing rather than recording pass.

## 14. Acceptance criteria

R001 closes only when Plans 029-031 are closed, required Java/i2pd portability evidence passed or milestone remains explicitly evidence-blocked, real bouncer behavior through SAM is demonstrated, no unresolved high-severity identity/linkability/clearnet/session-leak finding remains, and docs/planning are reconciled.

## 15. Stop conditions

Do not close if multiple Networks silently share a Destination, resource cost forces unreviewed identity-policy change, non-loopback SAM is needed for baseline, generic resolver/clearnet connector appears, private Destination is retained/logged, hidden 3.2/3.3 semantics are required, or portability is claimed from missing evidence.

## 16. Closure evidence

Create plans/closure/router-integration/032-status.md containing implementation commit ranges for 029-031, router/version matrix, independent-peer provenance, secret-free bouncer-over-SAM result summary, session reuse/recreation matrix, resource observations, restart/fault matrix, privacy/boundary/dependency/MSRV evidence, unresolved findings/severity, explicit R001 closure, and explicit R002 readiness decision against current i2pr public managed-app contracts.
