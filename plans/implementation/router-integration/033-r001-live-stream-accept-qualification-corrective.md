# Router Integration Corrective 033 — Repair Live SAM STREAM Qualification

Status: ready for handoff

Repository baseline:

- 4c9ce6fce1c298611a8e761a640b1f9a6b56e766

Corrects evidence from:

- plans/closure/router-integration/032-status.md
- scripts/live-sam-qualify.py

Authority:

- plans/adrs/ADR-0004-network-scoped-provider-and-owned-sam31-client.md
- plans/adrs/ADR-0005-explicit-i2p-provider-scope-release.md
- plans/research/007-r001-owned-sam31-client-and-provider-scope.md
- official SAM v3 documentation: https://www.i2p.net/en/docs/api/samv3/

Primary class: qualification corrective

## 1. Objective

Repair the live R001 qualification harness so its evidence actually exercises:

1. a valid inbound SAM STREAM topology on the independent peer side; and
2. the production owned Rust SamProvider on the connecting side.

Then rerun the live i2pd qualification, reclassify the current high-severity stream-delivery finding from valid evidence, and reconcile R001/R002 planning state without changing production SAM code unless corrected evidence exposes a real production defect.

Corrective 033 is an evidence-correctness pass first.

## 2. Why the corrective is required

### Q033-F1 — the independent peer never issued STREAM ACCEPT

The committed harness creates the peer STREAM session and then treats the SESSION CREATE control socket as if it were the inbound application stream.

Current shape:

~~~
peer control socket
  HELLO
  SESSION CREATE
  then read/write application bytes here
~~~

SAM STREAM does not work that way.

The current SAM specification requires an inbound application to:

1. keep its STREAM session alive on the session/control socket;
2. open a second SAM socket;
3. perform HELLO on that second socket;
4. issue STREAM ACCEPT ID=<peer-session-id> SILENT=false;
5. receive STREAM STATUS RESULT=OK;
6. wait for the incoming connection;
7. receive the connecting peer's Destination line when SILENT=false;
8. only then treat the remainder of that ACCEPT socket as raw bidirectional application bytes.

Therefore the current "0 bytes traversed" result is not evidence that i2pd established a broken data stream. The harness never created an inbound stream socket.

Severity: high because this is the only evidence supporting R001 Finding 1.

### Q033-F2 — the production owned client is not the connecting implementation under live test

The current Python harness hand-builds:

- the peer SESSION CREATE;
- the client SESSION CREATE;
- the client STREAM CONNECT.

This independently checks the general SAM request shape against i2pd, which is useful, but it does not directly prove that the production Rust SamProvider performs the live connect and raw-byte transition correctly.

A live R001 product-path qualification must keep the peer independent while using the actual production implementation under test on the connecting side.

Severity: high because Plan 032's product-path claim is about the owned adapter, not only a hand-written equivalent.

### Q033-F3 — the harness bridge-address policy is weaker than the production boundary

The Python harness accepts localhost as a bridge host.

The production SamBridgeEndpoint intentionally accepts numeric loopback only so no resolver participates in router authority.

The live qualification harness should use the same security boundary and accept only numeric loopback addresses.

Severity: low, but it weakens the claimed qualification boundary.

### Q033-F4 — planning/control metadata is stale after conditional closure

Current active planning still lists:

- closed Plan 031 under Active and dependency-ready;
- closed Plan 032 under Blocked implementation plans.

The Plan 032 closure heading also says Closed 2026-10-08 although the closure commit landed on 2026-10-07.

The historical Plan 032 evidence record must not be rewritten to pretend the harness defect was known at closure. Corrective 033 becomes the authoritative interpretation of that evidence.

Severity: administrative but must be reconciled.

## 3. Corrective invariants

1. The independent peer remains implemented separately from the production Rust SAM client.
2. The peer uses a real STREAM ACCEPT socket; the SESSION CREATE control socket is never treated as application data.
3. The production connecting side uses the actual owned Rust SamProvider / production SAM client path.
4. The connecting side exercises the same NetworkId-scoped session lifecycle used by the bouncer.
5. The accepted stream consumes the SAM status and peer-Destination prelude before binary payload comparison.
6. Binary payload comparison is exact and includes NUL, invalid UTF-8, CR/LF, and SAM-looking text.
7. A second application stream reuses the same live production SAM session.
8. The test bridge endpoint is numeric loopback only.
9. Missing router/bridge remains NOT RUN and cannot become PASS.
10. A live failure is not attributed to i2pd, the environment, or production code until the corrected topology identifies which side failed.
11. No captured private/public Destination material is committed to the repository.
12. Production network authority remains confined to crates/sam.
13. R002 remains blocked until Corrective 033 closes and its separate managed-app interface prerequisites are satisfied.

## 4. Correct independent-peer topology

The independent side must use this shape:

~~~
peer control socket
  HELLO VERSION MIN=3.1 MAX=3.1
  SESSION CREATE STYLE=STREAM ID=<peer-id> DESTINATION=TRANSIENT ...
  |
  +-- remains open for session lifetime

peer accept socket
  HELLO VERSION MIN=3.1 MAX=3.1
  STREAM ACCEPT ID=<peer-id> SILENT=false
  <- STREAM STATUS RESULT=OK
  |
  | wait for incoming connect
  <- <connecting-peer-Destination>\n
  |
  +-- raw bidirectional application stream
~~~

The ACCEPT socket must be established before the production client attempts STREAM CONNECT so the receiving side is ready.

For SAM 3.1 the harness needs only one pending accept at a time.

## 5. Production-side live probe

Add a live qualification entry point that uses the actual production Rust implementation.

Acceptable forms:

- an ignored integration test;
- an example/tool compiled from the real i2pr-irc-sam crate;
- a small qualification binary outside normal product installation.

Requirements:

- construct real SamBridgeEndpoint;
- construct real SamProvider;
- use a fixed test-only NetworkId;
- parse the peer Destination through real I2pEndpoint;
- call the real I2pStreamProvider::connect;
- write/read exact binary payload;
- keep the same SamProvider alive for a second connect so session reuse is real;
- call release(NetworkId) before exit;
- expose only machine-readable PASS/FAIL stage results and bounded non-secret diagnostics.

Do not copy the production SAM state machine into the probe.

## 6. Orchestration

The Python harness may remain the outer orchestrator and independent peer.

Recommended sequence:

1. validate numeric loopback endpoint;
2. probe bridge;
3. create peer session and obtain Destination;
4. open first ACCEPT socket and wait for STREAM STATUS RESULT=OK;
5. launch/instruct the Rust production-side probe;
6. wait for incoming peer-Destination line on ACCEPT socket;
7. exchange exact binary fixture in both directions;
8. close first accepted stream;
9. open second ACCEPT socket;
10. tell the same Rust process/provider to connect again;
11. exchange a second small fixture;
12. assert provider diagnostics show one session creation and two stream successes;
13. release Network scope;
14. summarize PASS/FAIL/NOT RUN.

The second connect must not create a new SamProvider process.

## 7. Raw transition correctness

The peer ACCEPT side with SILENT=false receives protocol text before application data.

The harness must consume, in order:

1. STREAM STATUS RESULT=OK;
2. after an incoming connection arrives, one bounded Destination/info line;
3. raw application bytes.

Do not start the raw pump before both protocol lines are consumed.

Preserve any bytes already read beyond the newline, just as the production client preserves post-status bytes on CONNECT.

## 8. Bridge endpoint qualification

Remove localhost acceptance from the live harness.

Accept:

- 127.0.0.0/8 numeric IPv4 loopback;
- ::1 numeric IPv6 loopback using unambiguous bracket/port parsing.

Reject:

- hostnames;
- routable IPv4/IPv6;
- wildcard addresses;
- URLs;
- malformed host:port forms.

No resolver call.

## 9. Evidence outcomes

Corrective 033 must support three truthful outcomes.

### Outcome A — corrected i2pd product path passes

If exact bytes traverse in both directions using independent ACCEPT plus production SamProvider:

- close current R001 Finding 1 as a qualification-harness defect;
- mark i2pd claim B, I2P stream product path, PASS;
- preserve Java I2P and i2pr as NOT RUN if still unavailable;
- preserve cross-router interoperability as unproven;
- R001 may remain conditionally closed solely on remaining portability evidence unless all required rows are also completed.

### Outcome B — production SamProvider fails while an independent hand-written CONNECT succeeds

This is evidence of a production adapter defect.

- localize the failing phase;
- fix only if bounded and clearly within Corrective 033;
- add regression coverage;
- otherwise stop and register a successor implementation corrective.

Do not close Corrective 033 while knowingly leaving an untracked production defect.

### Outcome C — both production and independent CONNECT fail against a correct ACCEPT topology

This is credible router/environment evidence rather than the current invalid result.

- record exact router version/config and stage;
- keep R001 conditionally closed/evidence-blocked;
- do not attribute cause beyond evidence;
- Corrective 033 may close if the qualification harness itself is proven correct and the remaining environment finding is accurately registered.

## 10. Regression tests

Add deterministic coverage for the corrected topology independent of live i2pd:

- fake SAM peer SESSION CREATE plus separate ACCEPT socket;
- ACCEPT status then peer-Destination line then raw payload;
- raw bytes immediately following the peer-Destination newline are preserved;
- production SamProvider connects into that ACCEPT;
- bidirectional binary fixture exact;
- same provider/session opens a second stream;
- release leaves zero scope;
- malformed ACCEPT status fails closed;
- missing peer-Destination line times out boundedly;
- oversized peer-Destination prelude refused;
- bridge hostname refused;
- missing bridge => NOT RUN in live harness.

## 11. Live evidence rerun

Rerun at minimum against the same i2pd 2.61.0 environment if available.

Record:

- router version;
- HELLO;
- peer SESSION CREATE;
- ACCEPT readiness;
- production session creation;
- production STREAM CONNECT;
- inbound peer-Destination prelude received;
- forward payload exactness;
- reverse payload exactness;
- second stream/session reuse;
- provider release;
- elapsed times.

If Java I2P or i2pr become available during the corrective, the same corrected harness may add rows, but their absence does not block correction of the i2pd evidence defect.

## 12. Planning and historical-evidence reconciliation

Do not rewrite Plan 032 substantive historical findings as though they were known at closure.

At Corrective 033 closure:

- create plans/closure/router-integration/033-status.md;
- explicitly supersede the interpretation of Plan 032 Finding 1;
- record Plan 032 closure-date typo as an erratum;
- move closed Plans 031/032 out of active/blocked registry sections;
- make Corrective 033 the active handoff until closed;
- update router roadmap current-state and R001 row with corrected evidence;
- update R002 blocker wording from the corrected R001 disposition.

The Plan 032 file itself may receive only a narrowly identified administrative date correction if repository convention permits; otherwise record the erratum exclusively in closure 033.

## 13. Scope

### In scope

- live qualification harness topology;
- independent STREAM ACCEPT peer;
- production Rust client/provider-side live probe;
- exact binary exchange;
- same-session second connect;
- numeric loopback enforcement in qualification tooling;
- corrected i2pd evidence;
- bounded production fix if corrected evidence reveals a small local SAM defect;
- planning/control-surface reconciliation.

### Out of scope

- adding STREAM ACCEPT to production client API;
- inbound bouncer listening;
- R002 managed-app integration;
- persistent Destinations;
- Java I2P installation as a hard dependency;
- cross-router testnet construction;
- changing per-Network identity policy;
- replacing the owned SAM client with the standalone SAM library.

## 14. Compatibility and security

No production API change is required merely to repair qualification.

If a production bug is found, changes must preserve:

- loopback-only bridge authority;
- no DNS;
- no clearnet fallback;
- one transient SAM identity per Network;
- no Destination/session-id logging;
- Rust 1.88;
- no external SAM production dependency.

Test-only independent ACCEPT code must not become a production inbound-network capability.

## 15. Verification

Expected minimum:

~~~
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Plus:

- deterministic corrected ACCEPT qualification suite;
- corrected live i2pd run where the environment is available;
- fail-closed missing-bridge run;
- non-loopback/hostname refusal runs.

## 16. Acceptance criteria

Corrective 033 closes only when:

1. the peer uses a real STREAM ACCEPT socket;
2. the connecting side uses the real production SamProvider path;
3. exact bidirectional application bytes are measured or a corrected, attributable failure is recorded;
4. session reuse is measured using one persistent provider instance;
5. live bridge policy is numeric-loopback-only;
6. no private Destination is committed/logged;
7. Plan 032 Finding 1 is reclassified from corrected evidence;
8. any real production defect found is fixed or separately registered;
9. registry/roadmap status is truthful;
10. R001/R002 readiness is updated from corrected evidence rather than the invalid harness result.

## 17. Stop conditions

Stop and register a successor corrective or ADR if:

- production CONNECT only works by weakening loopback-only authority;
- a fix requires SAM 3.2/3.3 semantics for the baseline;
- the production adapter must implement inbound ACCEPT for bouncer operation;
- corrected evidence shows per-Network identity ownership is incompatible with a supported router;
- a production fix requires persistent private Destination storage;
- cross-router claims would require evidence not available in the current environment.

## 18. Closure evidence

Create plans/closure/router-integration/033-status.md containing:

- Q033-F1 through Q033-F4 disposition;
- exact corrected topology;
- independent-peer provenance;
- production-probe provenance;
- deterministic ACCEPT/raw-transition matrix;
- live i2pd row-by-row rerun;
- first-stream and second-stream exact byte evidence;
- provider session creation/reuse/release counts;
- privacy/network-boundary/MSRV verification;
- Plan 032 Finding 1 supersession statement;
- closure-date erratum;
- final R001 status;
- explicit R002 readiness disposition.
