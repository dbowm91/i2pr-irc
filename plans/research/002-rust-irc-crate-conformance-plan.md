# Research 002 — Rust IRC Crate Conformance and Reuse Decision Plan

Status: closed

Result record: `plans/research/003-rust-irc-crate-conformance-results.md`

Research baseline: `3de5e66e49826735346a23030374ad96bfdb5a3b`

Related implementation gate:

- `plans/implementation/bouncer-core/005-pre-m003-observed-membership-and-downstream-cap-corrective.md`

Source roadmap:

- `plans/subsystems/bouncer-core-roadmap.md#M003--durable-multi-network-multi-client-and-history-model`

Prior foundation research:

- `plans/research/001-bouncer-and-i2p-foundation.md`

Current owned wire authority:

- `architecture/irc-wire.md`
- `crates/wire/`
- `crates/runtime/src/state.rs`
- `crates/runtime/src/downstream.rs`

## 1. Objective

Determine, with reproducible protocol evidence, whether current maintained Rust IRC crates should be:

1. production dependencies;
2. dev/research-only differential conformance oracles;
3. behavioral references only; or
4. excluded.

The research must also use independently authored protocol vectors to cross-check i2pr-irc's owned wire/state/CAP implementation before M003 makes IRCv3 capability, history, and persistence semantics more expensive to change.

The default outcome is not migration. The current owned wire layer remains authoritative unless this research demonstrates a concrete compatibility, maintenance, licensing, MSRV, and security advantage without weakening existing invariants.

## 2. Why this research is required before M003

M001 deliberately chose an owned wire codec after finding that a bouncer requires:

- separate bounded IRC body and IRCv3 tag budgets;
- byte-oriented ordinary fields;
- bounded incremental framing under hostile input;
- unknown command/tag preservation;
- deterministic malformed-input disposition;
- no hidden networking ownership.

Since that decision, additional actively maintained protocol crates have become relevant, particularly `ircv3_parse` and `obby-proto`.

M003 is the first milestone that intends to add substantial downstream IRCv3 behavior: labeled-response, server-time, batch, history, cursors, and multi-client routing. This is the correct point to verify that the owned implementation is not diverging from current ecosystem interpretations and that no low-level crate now provides a materially better substrate.

The research is a gate on M003 planning/implementation disposition, not an authorization to change dependencies automatically.

## 3. Candidate baseline

Research starts with these candidates and records exact versions/commits at execution time.

### A. ircv3_parse

Repository: `m3idnotfree/ircv3_parse`

Research-time baseline:

- version 4.0.0;
- release/commit activity through 2026-03-03;
- MIT OR Apache-2.0;
- Rust 1.78 MSRV;
- zero-copy parser focus;
- normal parser surface is text/`&str` oriented rather than the byte-oriented policy currently owned by i2pr-irc.

Initial classification: strongest candidate for a compatible dev-only differential parser, but production adoption requires proof that text-only assumptions do not weaken i2pr-irc's wire contract.

### B. irc / irc-proto

Repository: `aatxe/irc`

Research-time baseline:

- `irc` 1.1.0;
- active maintenance through 2026-01-01;
- Rust 1.80 MSRV;
- MPL-2.0;
- high-level `irc` crate is explicitly client/network oriented and includes optional TLS/proxy/network ownership;
- low-level `irc-proto` is the only part relevant to wire-level comparison.

Initial classification: mature interoperability reference; high-level client crate is not a production fit for the bouncer core.

### C. vinezombie

Repository: `vinezombie/vinezombie`

Research-time baseline:

- version 0.3.1;
- last observed commit 2025-04-13;
- Rust 1.70 MSRV;
- EUPL-1.2;
- modular IRCv3 framework with protocol/capability/state ideas relevant to a bouncer.

Initial classification: useful design/conformance reference, but maintenance and license compatibility require explicit review before any dependency proposal.

### D. obby-proto / obby-client

Repository: `obbyworld/obby-client`

Research-time baseline:

- version 0.3.0;
- active release on 2026-09-07;
- Rust 1.90 MSRV;
- GPL-3.0-or-later;
- sans-I/O `obby-proto` owns IRCv3 message parsing, tags, casemapping, ISUPPORT and mode arity;
- `obby-client` adds capability/SASL/state/history/reconnect behavior.

Initial classification: particularly valuable modern behavioral/reference oracle, but not a default production dependency because its license and MSRV conflict with the current MIT/Apache, Rust-1.88 workspace constraints.

## 4. Research invariants

1. IRC/IRCv3 primary specifications remain canonical. Agreement among libraries does not override a specification.
2. No external crate becomes a production dependency merely because it passes a subset of conformance vectors.
3. GPL/EUPL source or test code must not be copied into i2pr-irc.
4. Independently author test vectors from protocol specifications and observed behavior descriptions.
5. Do not add an incompatible-license crate to the production workspace or Cargo.lock merely for convenience.
6. The research must not raise i2pr-irc's Rust 1.88 MSRV.
7. The research must not weaken the owned byte-oriented/malformed-input/security contract to match a library that assumes valid UTF-8.
8. No high-level crate may introduce generic TCP/DNS/TLS/proxy authority into core/runtime.
9. Draft IRCv3 behavior must be version-labelled and not treated as stable solely because a crate implements it.
10. Disagreements are recorded, not normalized away.

## 5. Questions to answer

### Maintenance and provenance

For each candidate record:

- current published version;
- current repository/default branch;
- latest meaningful commit/release;
- license;
- MSRV;
- unsafe/build-script/proc-macro footprint;
- mandatory/default dependencies;
- whether it owns sockets/resolvers/TLS/proxies;
- project stability statements;
- bus-factor/maintenance signal sufficient for this project's risk model.

### Wire behavior

Compare:

- 512-byte IRC body accounting;
- IRCv3 tag-prefix/tag-data limits;
- CRLF/framing;
- NUL/embedded CR/LF;
- parameter count;
- leading-colon trailing params;
- unknown commands/numerics;
- opaque tag keys;
- tag escaping and invalid escape behavior;
- duplicate tags;
- empty/missing tag values;
- invalid UTF-8 in ordinary fields and tag values;
- partial/incremental input expectations;
- parse/encode round trips.

### IRC state primitives

Compare:

- ASCII/RFC1459/strict-RFC1459 casemapping;
- ISUPPORT token representation;
- `CHANTYPES`;
- `PREFIX`;
- `CHANMODES` mode-argument arity;
- RPL_NAMREPLY visibility symbols;
- membership prefixes/multi-prefix;
- channel mode application and incomplete/unknown state behavior.

### Registration/capabilities

Compare/reference:

- client CAP 302 flow;
- server-side CAP LS/REQ/ACK/NAK/END state;
- registration suspension during CAP;
- cap-notify;
- SASL PLAIN sequencing/chunking;
- capability values;
- post-registration CAP;
- stable upstream capability-set behavior applicable to bouncers.

### M003-facing IRCv3 semantics

Research current implementation approaches for:

- message-tags;
- server-time;
- batch;
- labeled-response;
- echo-message;
- standard replies;
- draft/chathistory;
- draft/read-marker;
- soju.im/bouncer-networks where relevant.

The purpose is to identify reusable representation/conformance ideas before designing persistence and multi-client routing, not to import another client's architecture.

## 6. Conformance corpus

Create an independently authored corpus derived from primary specifications.

Suggested layout if committed:

~~~text
research/irc-conformance/
  README.md
  vectors/
    wire-valid.*
    wire-invalid.*
    tags.*
    isupport.*
    modes.*
    cap.*
    sasl.*
    names.*
  results/
    ircv3-parse.md
    irc-proto.md
    vinezombie.md
    obby-proto.md
    i2pr-irc.md
~~~

The exact serialization may be JSON/TOML/text fixtures if that makes byte-exact cases unambiguous.

Every vector needs:

- source specification section;
- exact input bytes/text;
- expected structural interpretation or expected rejection;
- whether UTF-8 validity is relevant;
- stable versus draft status;
- reason it matters to a bouncer.

Do not copy fixture bodies from incompatible-license repositories. Recreate cases from specifications.

## 7. Execution strategy

### A. Primary-spec baseline

Before comparing crates, freeze expected behavior from:

- RFC IRC grammar where still applicable;
- IRCv3 current specifications;
- current draft versions for draft-only features;
- documented IRC server reply semantics used by the runtime.

Record ambiguity where specifications intentionally leave behavior implementation-defined.

### B. Candidate harnesses

Prefer isolated temporary/research harnesses over production dependencies.

For compatible-license candidates, a committed dev/research harness MAY be proposed only if:

- it remains outside production dependency ownership;
- it works on Rust 1.88;
- network/default features are disabled;
- the main no-clearnet boundary remains green.

For candidates with incompatible project licensing or higher MSRV:

- do not add them to the main workspace;
- run externally/temporarily if needed;
- commit only independently authored input vectors and summarized observed outputs;
- record exact version/commit/toolchain used.

### C. Differential comparison

For each vector classify each implementation result as:

- agrees with spec/i2pr-irc;
- external crate stricter;
- external crate more permissive;
- i2pr-irc stricter;
- i2pr-irc more permissive;
- representation mismatch;
- cannot represent due to UTF-8/API assumptions;
- draft/version mismatch;
- library bug/uncertain requiring source/spec review.

Do not make majority vote a correctness rule.

### D. State/capability comparison

Where a crate owns higher-level state rather than a pure parser, use scenario traces instead of direct type equality.

Example trace:

~~~text
CAP LS 302
NICK bot
USER user 0 * :real
(no 001 yet)
CAP END
=> registration completes
~~~

Likewise test desired JOIN/write versus self-JOIN confirmation and join rejection as scenario semantics, even if the external library is client-only and cannot express the bouncer side directly.

## 8. Production-dependency decision criteria

A candidate may be proposed as a production dependency only if all of these are true:

- license is accepted for the repository's intended MIT OR Apache-2.0 distribution strategy;
- MSRV <= the project floor or a separately approved project-wide MSRV change exists;
- no generic network authority enters wire/core/runtime through mandatory/default features;
- byte/framing/tag semantics satisfy or exceed current safety invariants;
- unknown IRC/IRCv3 extensions remain representable;
- server-side/bouncer use does not require fighting a client-only abstraction;
- maintenance signal is adequate;
- dependency/transitive/build-script surface is smaller or materially better than the owned implementation;
- replacement removes more maintenance/security risk than it adds;
- migration has a bounded compatibility plan.

If any criterion fails, classify the crate as dev/research-only or reference-only.

## 9. Expected likely dispositions

These are hypotheses to test, not predetermined conclusions:

- `ircv3_parse`: compatible dev-only differential oracle; production replacement uncertain because of text-oriented parsing.
- `irc-proto`: mature interoperability/reference source; likely not enough benefit to replace the qualified owned wire codec.
- `vinezombie`: behavioral/capability reference only unless maintenance/license review strongly changes the assessment.
- `obby-proto`: modern high-value reference oracle; likely reference-only due to GPL-3.0-or-later and Rust 1.90.
- owned `i2pr-irc-wire`: likely retained as production authority with external conformance vectors improving confidence.

Research must be willing to reject these hypotheses if evidence differs.

## 10. Required result artifact

Complete the research in a new result record:

- `plans/research/003-rust-irc-crate-conformance-results.md`

It must include:

- research date;
- exact candidate versions/commit SHAs;
- maintenance/license/MSRV/dependency matrix;
- protocol vector sources;
- differential behavior matrix;
- every i2pr-irc discrepancy found;
- whether each discrepancy is a bug, deliberate stricter policy, or unresolved ambiguity;
- candidate disposition: production/dev-only/reference/excluded;
- supply-chain/security assessment;
- M003 implications;
- explicit recommendation on whether to retain or replace any owned layer.

If research identifies an i2pr-irc correctness defect beyond Corrective 005, M003 remains blocked and a new corrective plan is required.

## 11. Relationship to Corrective 005

Research 002 and Corrective 005 MAY execute in parallel.

Corrective 005 uses primary IRC/IRCv3 semantics as its implementation authority and must not wait for a library to tell it what is correct.

Research 002 must include the final Corrective-005 behavior in the comparison before it closes.

M003 may become implementation-planning ready only when:

1. Corrective 005 has a closed evidence record with no high-severity blocker; and
2. Research 003 results record a clear dependency/conformance disposition with no unresolved correctness defect that affects M003 design.

## 12. Stop conditions

Stop and escalate rather than silently changing architecture if:

- a candidate reveals a primary-spec interpretation that materially contradicts the current canonical wire contract;
- production adoption would require changing repository licensing strategy;
- production adoption would require raising MSRV above 1.88;
- an external crate introduces hidden networking/build-time execution inconsistent with the threat model;
- the correct behavior for a foundational IRCv3 draft is genuinely ambiguous and M003 would persist that ambiguity.

## 13. Completion definition

Research 002 is complete when the result record gives a reproducible, source-backed disposition for all four candidate families and all discrepancies relevant to M003, without changing production dependencies implicitly.

The likely useful output is a durable external conformance corpus and a reasoned decision that either validates continued ownership of `i2pr-irc-wire` or justifies a separately planned migration.
