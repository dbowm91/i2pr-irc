# Bouncer Core M004-A — Anonymity Protocol Mediation

Status: closed

Blocker:

- `plans/closure/bouncer-core/014-status.md` accepted

Research authority:

- `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Primary class: invariant + capability

## 1. Objective

Implement a single audited privacy boundary for anonymity-sensitive IRC inputs before the adverse-network campaign.

Deliver:

- CTCP parsing/mediation;
- DCC structural suppression;
- local handling/suppression of metadata queries that would expose attached-client fingerprints;
- downstream client-tag mediation with a conservative default;
- truthful CLIENTTAGDENY behavior;
- stable upstream-visible CTCP/CAP fingerprint independent of attached client;
- environment/secret leakage negative tests.

## 2. Core invariants

1. DCC can never invoke a dial/listen path or reach a local client as an actionable request.
2. CTCP ACTION remains ordinary chat semantics.
3. Upstream metadata queries cannot cause the attached IRC client to auto-reveal its VERSION/TIME/USERINFO/SOURCE/FINGER/CLIENTINFO.
4. Remote-visible CTCP behavior is independent of which local client is attached.
5. No host environment value is used to construct an IRC-visible reply.
6. Unknown client-only tags are denied by default.
7. Client-originated unprefixed tags are removed unless a specific reviewed extension authorizes them.
8. CLIENTTAGDENY accurately describes the policy exposed downstream.
9. Filtering occurs before upstream transmission/downstream exposure, not after logging/history side effects.
10. No anonymity policy introduces alternate network authority.

## 3. CTCP parser

Add a small bounded parser over the text parameter of PRIVMSG/NOTICE.

Requirements:

- detect the `0x01` delimiter;
- accept the commonly tolerated missing final delimiter where appropriate;
- parse command and bounded opaque params;
- reject embedded NUL/CR/LF according to existing wire constraints;
- do not interpret CTCP inside arbitrary mixed plain text;
- preserve ACTION payload bytes within existing message limits;
- return explicit parsed kind / malformed / ordinary-text result.

Do not use regex-based rewriting.

## 4. Directional CTCP policy

### Upstream -> downstream

For incoming PRIVMSG CTCP queries:

- ACTION: fan out as ordinary action/chat.
- PING: bouncer answers privately with the same bounded opaque token; query is not handed to arbitrary local clients.
- DCC: suppress completely.
- VERSION: suppress.
- TIME: suppress.
- USERINFO: suppress.
- SOURCE: suppress.
- FINGER: suppress.
- CLIENTINFO: suppress.
- unknown: suppress.

For incoming CTCP NOTICE replies:

- pass only when they correspond to an operator-initiated safe query and do not violate response-routing/privacy policy;
- DCC never passes;
- unknown unsolicited replies may be suppressed.

### Downstream -> upstream

- ACTION in PRIVMSG: allow.
- ordinary user-initiated CTCP query: allow if it has no alternate-network side effect.
- DCC in PRIVMSG or NOTICE: block.
- PING query/reply: allow under bounded token policy.
- VERSION/TIME/USERINFO/SOURCE/FINGER/CLIENTINFO NOTICE replies: suppress by default to stop local-client fingerprint leakage.
- unknown CTCP NOTICE replies: suppress by default.

Blocking a CTCP frame must not drop an otherwise separate plain message because this implementation does not support mixed CTCP/plain bodies.

## 5. DCC

DCC must be structurally absent:

- no host/port parsing into network types;
- no socket API;
- no file path creation;
- no listener;
- no passive/reverse DCC mode;
- no XDCC/SDCC convenience path.

The parser may recognize the command name only to classify it as blocked.

Strengthen the static boundary guard with a positive control showing that introducing a generic DCC dial/listen primitive fails the check.

## 6. Client-tag policy

Wire live client forwarding through one tag mediator.

Initial M004 policy:

- remove all client-supplied unprefixed tags unless an explicitly implemented extension defines their client submission semantics;
- deny all `+` client-only tags by default;
- emit/advertise `CLIENTTAGDENY=*` when message-tags is exposed downstream;
- do not forward a TAGMSG whose entire semantic payload is removed;
- keep the client-only allowlist empty in this milestone.

Do not permit client `time` or `msgid` merely because they parse.

Any future exemption requires a separate privacy/timing review.

## 7. Capability/fingerprint stability

For a fixed upstream offer, prove identical upstream CAP request/registration behavior for:

- no clients;
- one legacy client;
- one chathistory client;
- one read-marker client;
- several clients with different CAP requests;
- client churn and repeated CAP negotiation.

CTCP policy replies must also be fixed by bouncer policy rather than downstream client brand/version.

## 8. Environment/secret negative tests

Poison test environment with unique sentinels for:

- USER / LOGNAME;
- HOSTNAME;
- HOME;
- TMPDIR;
- PATH-like values where safe.

Exercise:

- registration;
- local errors;
- CAP;
- CTCP;
- history replay;
- store errors;
- reconnect diagnostics.

Assert sentinels never appear in:

- upstream bytes;
- downstream fixed bouncer replies;
- structured snapshots/diagnostics;
- closure fixtures.

Also assert no SASL password/AUTHENTICATE payload/private secret material appears.

## 9. Work packages

A. bounded CTCP parser;
B. upstream privacy mediation;
C. downstream CTCP/DCC mediation;
D. live client-tag mediator + CLIENTTAGDENY;
E. capability/fingerprint stability;
F. environment/secret negative campaign;
G. docs/closure.

## 10. Tests

Include:

- ACTION both directions;
- PING query/reply;
- every blocked metadata query/reply;
- DCC CHAT/SEND/passive-looking forms;
- malformed/missing final CTCP delimiter;
- unknown CTCP;
- several CTCP frames at max/max+1 payload bounds;
- unprefixed client tags removed;
- `+typing` and arbitrary vendor tags denied;
- CLIENTTAGDENY=*` emitted truthfully;
- client-only tag pressure does not enlarge queues;
- poisoned environment does not escape;
- capability fingerprint matrix;
- static no-network/DCC guard.

## 11. Acceptance criteria

M004-A closes only when an attached local client cannot alter the bouncer's remote metadata fingerprint or create a DCC/direct-network path, while ACTION and safe intentional IRC behavior continue to work.

## 12. Stop conditions

Stop if the desired policy requires identifying specific downstream client brands, host environment inspection, or a new socket/network API.

## 13. Closure evidence

Create `plans/closure/bouncer-core/015-status.md` with CTCP matrix, tag matrix, fingerprint matrix, environment/secret evidence, static-boundary evidence and M004-C readiness contribution.
