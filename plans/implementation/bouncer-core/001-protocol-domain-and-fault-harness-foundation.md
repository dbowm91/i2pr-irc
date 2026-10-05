# Bouncer Core Milestone 001 — Protocol, Domain, and Deterministic-Fault Foundation

Status: closed

Closure record: `plans/closure/bouncer-core/003-status.md` (corrective qualification)

Repository baseline: 7276d2f8f6ec62c3c8a9023b027bbdaf90528f02

Source roadmap:

- plans/subsystems/bouncer-core-roadmap.md#M001--protocol-domain-and-deterministic-fault-foundation

Long-term requirements:

- plans/000-long-term-specification.md sections 1-7, 9, and 14
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md Phase 1
- plans/research/001-bouncer-and-i2p-foundation.md

Applicable ADRs:

- plans/adrs/ADR-0001-i2p-only-upstream-and-router-adapter-boundary.md

Primary class: invariant + infrastructure

## 1. Objective

Create the minimum production-quality Rust foundation on which later bouncer behavior can safely depend:

- workspace/toolchain/verification floor;
- strict bounded IRC/IRCv3 wire representation;
- canonical IDs and I2P-only endpoint types;
- runtime-neutral I2pStreamProvider and LocalAcceptor contracts;
- injected time primitives required by reconnect/liveness state;
- deterministic ordered-stream fault harness;
- static no-clearnet/no-host-resolution guards;
- fuzz/property coverage for hostile wire input.

This milestone must not implement SAM, SQLite, a real listener, a production network supervisor, or a user-visible bouncer.

## 2. Why this milestone is ready

Hard dependencies are closed:

- canonical product/specification/terminology/planning documents exist;
- ADR-0001 is accepted;
- bouncer-core roadmap M001 is registered;
- external research identifies the relevant IRC/IRCv3 bounds and router abstraction without requiring a live router.

No unresolved router/API decision is required for this milestone because all networking is represented by traits/fakes.

Current related workspace policy:

- i2pr currently uses Rust 1.88 / edition 2024;
- CodeGG uses explicit workspace package/dependency policy and static boundary checks.

To minimize future i2pr SDK friction, this milestone should target Rust 1.88 or explain in closure why a higher MSRV is required by a reviewed dependency. The MSRV decision is part of this milestone and becomes repository policy once closed.

## 3. Current implementation evidence

At the baseline there is no Cargo workspace or Rust production code.

Existing authority:

- I2pEndpoint is I2P-only by definition;
- I2pStreamProvider is the only upstream data-plane abstraction;
- LocalAcceptor is a separate downstream-local abstraction;
- upstream host DNS/TCP are prohibited;
- non-idempotent IRC operations may have unknown delivery across disconnect;
- core time must be injectable;
- every externally influenced line/queue/collection is bounded.

Research indicates irc-proto 1.1.0 is a possible low-level codec dependency, but suitability for strict bouncer/server use is unproven. The high-level irc client crate is not an architectural dependency.

## 4. Invariants that must not regress

1. No production core crate can resolve host DNS or open a generic upstream TCP/UDP socket.
2. I2pEndpoint cannot encode a generic URL, IP socket address, or host/port endpoint variant.
3. LocalAcceptor cannot be used as an upstream connector.
4. I2pStreamProvider cannot expose arbitrary router administration.
5. Wire input is bounded before allocation proportional to attacker-controlled declared length.
6. Unknown well-formed commands, numerics, tags, and ISUPPORT values remain representable where forwarding/recording requires them.
7. Invalid framing cannot desynchronize subsequent lines silently.
8. Time-dependent domain behavior can be tested without wall-clock sleeps.
9. Fault injection models an ordered reliable byte stream; it does not invent packet reordering/duplication inside a stream.
10. No unsafe Rust is required for the foundation.
11. Dependency breadth is justified and minimal.

## 5. Scope

### In scope

- root Cargo workspace and Cargo.lock;
- repository MSRV/edition/lint policy;
- workspace dependency ownership rules;
- likely crates/modules for wire, core contracts, and testkit;
- parser/encoder and bounded incremental line codec;
- IRCv3 tags;
- prefix/command/params/trailing representation;
- typed or validated IRC casemapping/name primitives needed by later state;
- stable opaque IDs: NetworkId, ClientId, ConnectionGeneration and similar minimum M001 identities;
- validated I2pEndpoint forms;
- I2pStreamProvider trait and error vocabulary;
- LocalAcceptor/local-stream trait contract without a socket implementation;
- Clock/Timer or equivalent injected-time boundary;
- deterministic duplex/fault stream testkit;
- static architecture guards;
- fuzz target(s);
- architecture docs and verification scripts.

### Explicitly out of scope

- SAM or I2CP;
- Proposal 170;
- i2pr crate dependency;
- Tokio TCP listener/connect calls;
- system resolver;
- SQLite;
- configuration file UX;
- IRC registration state machine beyond what is necessary to exercise wire/domain primitives;
- SASL execution;
- downstream authentication;
- actual IRC server/client connection;
- history;
- reconnect policy implementation;
- anonymity CTCP/DCC filtering beyond types needed to avoid losing wire information.

## 6. Required production changes

### Workspace and dependency policy

Create a small workspace with clear ownership boundaries. A suitable starting decomposition is:

- i2pr-irc-wire — protocol values, bounded parsing/encoding, no runtime/network/storage;
- i2pr-irc-core — domain IDs, endpoint/provider/local-stream/time contracts, no concrete router;
- i2pr-irc-testkit — deterministic stream/time/provider fixtures used by repository tests;
- optional root package only if needed for repository verification; it must not pretend to be an operational daemon.

Exact naming may vary if equivalent boundaries are clearer.

Workspace requirements:

- Cargo.lock committed;
- rust-version explicitly frozen;
- edition explicitly frozen;
- unsafe_code denied/forbidden in foundation crates;
- dependencies use minimal feature sets;
- repeated dependency versions/default-feature policy owned at workspace level;
- release/profile decisions remain minimal until measured need.

Review every production dependency, including build scripts and proc macros.

### Wire substrate decision gate

Before adopting irc-proto, write focused qualification tests/probes for:

- 512-byte non-tag message rule including CRLF;
- IRCv3 message-tag region ceiling;
- partial line reads;
- NUL/CR/LF handling;
- empty/malformed prefix/command/params;
- maximum parameter count;
- tag escaping/unescaping;
- unknown command/numeric/tag preservation;
- invalid UTF-8 policy;
- parse/encode behavior for bouncer forwarding;
- absence of hidden unbounded buffering.

Decision:

- adopt/wrap irc-proto only if the bouncer can enforce all required bounds and preservation semantics at its own boundary without depending on high-level client assumptions;
- otherwise implement a small owned wire crate from IRC/IRCv3 specifications.

Record the decision and evidence in architecture documentation. A new ADR is not required unless the decision establishes a public compatibility contract beyond this subsystem.

Do not add the high-level irc client crate merely to obtain its connection/runtime model.

### Wire representation

Freeze exact named ceilings in one module.

At minimum represent:

- optional tags;
- optional source/prefix;
- command/numeric without an exhaustive enum that discards unknown commands;
- bounded parameters and final trailing parameter;
- raw/validated token forms needed for faithful re-encoding;
- message direction-independent wire semantics.

Parsing must be incremental and consume exact complete lines. Oversize input enters a deterministic error/discard state; it must not retain an attacker-controlled unbounded line.

Specify UTF-8 policy explicitly. IRC is byte-oriented historically, while modern client behavior expects text. Do not silently use String everywhere if that can make malformed remote bytes impossible to classify/reject deterministically.

### IRC casemapping/name primitives

Add only the primitives needed to prevent later state bugs:

- ASCII/RFC1459/strict-RFC1459 casemapping behavior as advertised by ISUPPORT;
- validated/bounded channel/nick identifiers or normalized comparison keys;
- no host/environment-derived defaults.

Do not implement full channel state.

### Core identities and endpoint

IDs must be stable typed values rather than interchangeable strings.

I2pEndpoint must have a closed initial representation that can distinguish supported I2P naming/destination forms without a Generic(String) escape hatch interpreted by host networking.

Validation should reject:

- URL schemes;
- host:port syntax;
- IP socket-address syntax;
- control/NUL/whitespace smuggling;
- overlong input.

It may retain a normalized I2P hostname/base32/Destination text form according to the external specification. Do not perform resolution in core.

### Provider/local-stream contracts

I2pStreamProvider should expose a small async connect operation over I2pEndpoint plus typed failure classification and cancellation semantics.

Do not expose:

- SocketAddr;
- ToSocketAddrs;
- generic host strings;
- DNS resolver handles;
- raw router-admin client.

LocalAcceptor is separate and yields a local client stream/metadata abstraction. M001 provides only contract/fakes.

Use the minimum trait/object/generic shape needed for Tokio-compatible byte streams later while keeping deterministic fake streams easy.

### Time

Introduce a small monotonic Clock/Timer/Sleeper abstraction suitable for:

- deadlines;
- reconnect eligibility;
- liveness;
- deterministic virtual advancement.

Wall-clock timestamps used later for history are a separate concept from monotonic scheduling.

### Deterministic fault harness

Implement an ordered reliable duplex stream fixture configurable to:

- segment reads;
- segment writes / report short writes;
- delay readiness under virtual/injected control;
- stall one direction;
- EOF/reset after selected byte or scripted event boundaries;
- impose bounded capacity/backpressure;
- record writes without secrets by default;
- replace connection generations while stale work remains pending.

Provide a FakeI2pStreamProvider that returns scripted outcomes/streams and records requested I2pEndpoint values.

The testkit must be deterministic by seed/script and produce a compact failure reproduction descriptor.

### Static guards

Add a script invoked by routine verification that checks core/wire crates for prohibited networking/resolution ownership.

At minimum flag unexpected production references/dependencies such as:

- std::net connection/listener/resolution surfaces;
- tokio::net TCP/UDP in core/wire;
- ToSocketAddrs;
- common HTTP clients/resolvers;
- generic proxy client dependencies.

The guard must have an automated positive control proving it detects forbidden examples. Avoid a brittle text grep that is trivial to evade; combine manifest/dependency checks and source checks appropriate to the repository size.

The future SAM adapter/local-listener areas will be explicit allowlisted owners, not reasons to weaken the core guard globally.

### Documentation

Add architecture docs for:

- crate/dependency graph;
- wire bounds/preservation policy;
- network capability ownership;
- time/fault model;
- dependency review.

## 7. Ordered work packages

### Work package A — Workspace and boundary skeleton

Intent:

Create the repository's executable verification and dependency foundation before protocol code.

Required changes:

- workspace manifests;
- MSRV/edition/lints;
- fmt/clippy/test scripts;
- crate boundaries;
- static dependency guard skeleton + positive control.

Acceptance evidence:

- workspace builds with no operational networking;
- guard catches the fixture and accepts production skeleton;
- dependency tree is recorded/reviewed.

### Work package B — Wire dependency qualification and contract freeze

Intent:

Resolve owned parser versus irc-proto from tests, not convenience.

Required changes:

- conformance probe/golden corpus;
- decision record;
- named byte/count ceilings;
- malformed input taxonomy.

Acceptance evidence:

- each selection criterion has a result;
- no unresolved parser ownership decision remains before full wire implementation.

### Work package C — Strict wire codec

Intent:

Provide one bounded parse/encode authority.

Required changes:

- incremental line reader/decoder;
- message/tag/prefix/command/param types;
- tag escapes;
- unknown-value preservation;
- exact max/max+1 behavior.

Acceptance evidence:

- golden vectors;
- segmented-read matrix;
- malformed/oversize matrix;
- round-trip/property coverage.

### Work package D — Domain and capability boundary

Intent:

Freeze identity and I2P-only authority before runtime work.

Required changes:

- IDs/generation;
- I2pEndpoint;
- I2pStreamProvider;
- LocalAcceptor contract;
- typed provider errors;
- casemapping primitives.

Acceptance evidence:

- compile-time/type tests where useful;
- invalid endpoint suite;
- no generic network target in the public core surface.

### Work package E — Time and deterministic fault testkit

Intent:

Make future reconnect behavior reproducible.

Required changes:

- monotonic test clock/timer;
- scripted duplex stream;
- FakeI2pStreamProvider;
- stale generation scenarios;
- compact reproduction descriptors.

Acceptance evidence:

- deterministic repeated runs;
- no wall-clock sleep in fault-harness tests;
- partial/short/stall/reset cases.

### Work package F — Fuzz/static/docs closure floor

Intent:

Turn the foundation into an enforceable contract.

Required changes:

- fuzz target(s);
- static guards integrated into verify;
- architecture docs;
- routine verification script.

Acceptance evidence:

- fuzz smoke completes without panic/OOM;
- guard positive control passes;
- broad repository floor is green.

## 8. Failure, cancellation, restart, and contention semantics

M001 has no daemon restart state.

Trait semantics must nevertheless define:

- cancellation of a pending I2P connect returns control without leaving a hidden owned task in core;
- a stream error is terminal for that stream generation;
- stale ConnectionGeneration values are comparable/fenceable;
- short reads/writes are normal stream behavior;
- EOF is distinct from malformed IRC input;
- virtual-time timers can be canceled without later firing into a reused identity;
- bounded fault-stream capacity has deterministic backpressure rather than memory growth.

No exactly-once send guarantee is introduced.

## 9. Compatibility and migration

There is no prior released API or database.

M001 may choose APIs freely within canonical invariants.

The closure must freeze:

- repository MSRV/edition;
- public crate boundary intended for M002;
- wire limits;
- endpoint forms;
- provider/local-stream contract.

Before a public release, these may still be corrected through normal corrective planning rather than compatibility shims.

## 10. Required tests

### Focused unit tests

- every parser token/message bound max and max+1;
- tag escape/unescape valid/invalid;
- IRC command/numeric and unknown command;
- casemapping vectors;
- endpoint valid/invalid;
- ID/generation ordering/equality;
- timer cancellation/advance.

### Integration/property tests

- all split points of representative IRC lines;
- multiple concatenated lines;
- oversize line followed by valid line according to frozen resynchronization policy;
- parse -> encode -> parse invariants;
- fake provider scripted success/failure;
- short writes and bounded backpressure;
- stale generation completion.

### Security/negative tests

- URL/IP/host:port endpoint rejection;
- static guard forbidden dependency/source fixtures;
- NUL/control framing;
- oversized tag/message input;
- allocation does not scale beyond frozen ceilings for malformed declared input.

### Fuzz

At least wire decoder/parser and tag parser, with bounded input corpus/limits and smoke command documented.

## 11. Required verification commands

The implementation may refine command names while preserving this floor. Closure must record exact commands actually run.

Expected floor:

~~~
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
scripts/verify.sh quick
scripts/verify.sh full
scripts/fuzz-smoke.sh
~~~

If all-features would intentionally include external/live-router tests in the future, the verification contract must be revised before that occurs; M001 has no such features.

## 12. Documentation updates

Required:

- README implementation-state section;
- architecture/overview.md;
- architecture/irc-wire.md;
- architecture/network-boundary.md;
- architecture/testing.md;
- dependency-maintenance or equivalent dependency review record;
- AGENTS.md quick-start commands once they exist;
- source roadmap and registry status.

## 13. Acceptance criteria

1. A fresh checkout can execute the documented quick verification floor.
2. Wire parsing/encoding is strict, bounded, segmented-I/O safe, and preserves required unknown IRC/IRCv3 information.
3. IRCv3 tag and traditional IRC line ceilings are named and max/max+1 tested.
4. The parser dependency decision is evidence-backed and closed.
5. The core public endpoint/provider surface cannot express a generic clearnet connection.
6. No core/wire production dependency owns host DNS, generic socket connection, HTTP, or proxy behavior.
7. Static guards fail on an intentional forbidden-network positive control.
8. Core scheduling time is injectable.
9. The deterministic fault stream reproduces partial I/O, stalls, resets/EOF, backpressure, and stale-generation completion without wall sleeps.
10. Fuzz smoke covers hostile wire decoders.
11. MSRV/edition/dependency policy is documented.
12. No SAM/router/listener/storage/bouncer capability is falsely claimed.

## 14. Stop conditions

Stop and report/register a successor decision rather than broadening M001 if:

- no existing async I/O trait shape can satisfy both deterministic test streams and later Tokio integration without unsafe/platform-specific code;
- irc-proto behavior would require weakening a frozen wire bound/preservation invariant;
- correct I2P endpoint validation requires implementing router naming/resolution;
- static no-clearnet enforcement requires banning the future LocalAcceptor/SAM adapter rather than preserving an ownership boundary;
- a dependency introduces a materially larger runtime/network surface than this plan;
- repository MSRV must rise above i2pr's current 1.88 solely for convenience;
- implementation starts requiring SQLite, SAM, or a real listener to demonstrate correctness.

## 15. Closure evidence required

The closure record must include:

- implementation commits;
- exact workspace/MSRV/edition/dependency set;
- parser dependency qualification matrix and decision;
- dependency tree/review;
- wire ceiling table with source specification references;
- golden/property/max+1 test evidence;
- segmented I/O evidence;
- endpoint rejection matrix;
- static network-boundary guard positive-control evidence;
- deterministic fault-harness scenario matrix;
- fuzz smoke result;
- exact quick/full verification results;
- invariant review;
- unresolved findings and severity;
- M002 readiness decision.

## 16. Handoff notes

This milestone is intentionally foundation-heavy. Do not "make it useful" by opening sockets or adding SAM.

Prefer owned small types and explicit limits over broad generic abstractions.

The deterministic fault harness is a first-class product-development asset. Design it for reuse by M002-M004, but do not extract it to a separate repository without a second independent consumer.
