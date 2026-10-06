# Bouncer Core M005-D / Plan 023 — Bouncer Networks and Local IRC Administration

Status: ready

Blocker:

- Plan 022 closure accepted

Research authority:

- plans/research/006-m005-mature-bouncer-and-control-session-research.md

Architecture authority:

- plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md
- architecture/control-session.md

Primary class: capability

## 1. Objective

Expose the M005 process-control model through interoperable local IRC surfaces:

- soju.im/bouncer-networks;
- soju.im/bouncer-networks-notify;
- a small local BouncerServ administration service;
- standard-reply framing for local bouncer-control errors where appropriate.

The implementation is independently authored from protocol documents and reference behavior.

## 2. Protocol isolation

Place bouncer-networks parsing/rendering in a dedicated adapter module.

The extension is work-in-progress. Do not store its message-tag attribute strings as the durable schema contract.

NetworkId is canonical netid and must remain stable over Network lifetime.

BOUNCER BIND is accepted only before registration completes.

A registered unbound session remains an unbound control connection; it cannot BIND late.

Legacy clients may continue connecting directly to a configured Network without negotiating the extension.

## 3. I2P-safe attribute profile

Recognize the standard attribute names so clients receive explicit errors rather than silent reinterpretation.

Supported mapping:

- name -> durable Network display name;
- state -> read-only live connected/connecting/disconnected projection;
- error -> read-only bounded non-secret error;
- host -> typed I2pEndpoint only;
- nickname -> preferred nick;
- username -> IRC username;
- realname -> IRC realname.

Recognized but rejected/not applicable in this product generation:

- port;
- tls;
- pass until a separately reviewed upstream PASS feature exists.

host must pass I2pEndpoint validation before any durable mutation. No hostname from this extension may reach system DNS, generic TCP, URL parsing or TLS setup.

Unknown and read-only attributes receive explicit FAIL BOUNCER responses.

## 4. BOUNCER commands

Implement:

- BIND;
- LISTNETWORKS;
- ADDNETWORK;
- CHANGENETWORK;
- DELNETWORK.

All mutations go through RuntimeControlHandle and inherit Plan 020 durable/live convergence.

LISTNETWORKS returns a bounded batch.

ADD/CHANGE cannot bypass global Network count, identity, endpoint, policy or secret bounds.

DEL must not leave a ghost owner.

## 5. Notifications

Advertise soju.im/bouncer-networks-notify only when initial and change notifications are complete.

At negotiation/registration send a bounded initial Network batch.

For later changes, derive per-session notifications from monotonic bounded controller snapshots/revisions. Do not allocate an unbounded process event log.

If a control session falls behind, reconcile it from the current bounded snapshot rather than replaying an arbitrary backlog of catalog events.

## 6. BouncerServ subset

Add a fixed, non-environment-derived local IRC service identity.

Initial commands should cover only typed operations already owned by RuntimeControlHandle:

- help;
- network status/list;
- network create;
- network update;
- network delete;
- channel status;
- channel detach;
- channel attach;
- presence/nick policy status and bounded updates;
- SASL status/set/reset if secret handling can reuse StoredSecret without rendering the value.

Do not implement:

- shell execution;
- network quote/raw IRC line;
- file access;
- HTTP/Web hooks;
- plugin/module execution;
- router administration;
- multi-user administration.

Service parsing must be bounded and must not use a general shell interpreter merely to imitate another bouncer's command syntax.

## 7. Authorization

The initial product has one local Operator.

Runtime control is available only to a session admitted with the trusted/authenticated ClientId supplied by the local access boundary. This plan does not broaden listeners to non-loopback or create hosted multi-user semantics.

If later listener work requires a distinct authorization role beyond authenticated ClientId, stop and make that an explicit architecture decision.

## 8. CAP and standard replies

Advertise bouncer-networks capabilities only after their complete semantics are live.

Implement the IRCv3 standard-replies capability if the downstream surface can truthfully use FAIL/WARN/NOTE generically; otherwise use the bouncer-networks draft's required FAIL forms without falsely advertising broader semantics and leave capability promotion to Plan 025.

CAP remains locally mediated and never alters upstream negotiation.

## 9. Work packages

A. bouncer-networks adapter;
B. CAP advertisement and BIND registration integration;
C. LIST/ADD/CHANGE/DELETE typed control bridge;
D. I2P attribute validation profile;
E. bounded notify snapshots/deltas;
F. BouncerServ typed command parser/service identity;
G. multi-client/unbound/bound interoperability matrix;
H. anonymity/network-boundary qualification;
I. docs/closure.

## 10. Tests

Include:

- netid stability across restart;
- unbound LISTNETWORKS;
- pre-registration BIND and late BIND refusal;
- invalid netid;
- ADD/CHANGE host accepts valid .i2p/b32/Destination and rejects clearnet host/URL;
- port/tls/pass explicit refusal;
- read-only state/error mutation refusal;
- Network count and input max/max+1;
- notify initial batch and add/change/delete deltas;
- slow notification client does not grow process memory;
- bound and unbound control sessions can administer via typed control;
- BouncerServ commands cannot inject extra IRC lines;
- BouncerServ cannot execute raw command/host path;
- SASL secret never appears in service responses/diagnostics if set/reset is included;
- network delete has no surviving owner;
- static network-boundary positive controls remain green;
- interoperability transcript tests derived from the published draft, not reference source code.

## 11. Verification

Run full verification plus the static network/anonymity guards and deterministic multi-network tests.

## 12. Documentation

Add a versioned bouncer-networks support/profile document stating the exact draft revision and I2P-specific attribute disposition.

Document BouncerServ as local Operator control, not an IRC network service.

## 13. Acceptance criteria

A modern compatible IRC client can discover, select and administer I2P IRC Networks through one local bouncer connection without any generic clearnet authority, while legacy pre-bound clients remain supported and all process mutation flows through the typed controller.

## 14. Stop conditions

Stop if interoperability appears to require:

- generic host/port/TLS networking;
- auto-creating clearnet Networks from usernames;
- retaining draft attribute maps as durable schema;
- a permanently global owner for bound session traffic;
- a raw network-quote escape hatch.

## 15. Closure evidence

Create plans/closure/bouncer-core/023-status.md with draft conformance transcripts, I2P attribute matrix, authorization/bounds evidence, notification pressure tests and M005-E readiness.
