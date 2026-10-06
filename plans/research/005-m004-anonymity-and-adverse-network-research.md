# Research 005 — M004 anonymity and adverse-network qualification

Status: complete for implementation planning

Research date: 2026-10-06

Repository baseline:

- `11e49d286fc2a442ffbf06ca8d0517338a155b3a`

Related authority:

- `plans/000-long-term-specification.md`
- `plans/001-terminology-and-domain-model.md`
- `plans/subsystems/bouncer-core-roadmap.md`
- `plans/closure/bouncer-core/013-status.md`

Primary external protocol references reviewed:

- IRCv3 message-tags / CLIENTTAGDENY: https://ircv3.net/specs/extensions/message-tags.html
- current IRCv3 registry: https://ircv3.net/registry
- Modern IRC CTCP reference: https://modern.ircdocs.horse/ctcp
- Modern IRC DCC reference: https://modern.ircdocs.horse/dcc

## 1. Purpose

Resolve the implementation boundaries required to turn M004 into bounded handoffs.

M004 is the first milestone allowed to claim that the router-neutral core is suitable for anonymity-network operation. The work therefore has two kinds of obligations:

1. implement the missing anonymity-sensitive policy boundaries; and
2. qualify the already-bounded runtime under correlated, I2P-like adverse conditions.

The research covers:

- the unresolved M003-D live response-routing gap;
- CTCP/DCC policy;
- downstream client-tag mediation;
- fixed upstream-visible fingerprint policy;
- environment/secret leakage tests;
- process-wide reconnect-storm control;
- deterministic many-Network fault campaigns;
- resource recovery and closure evidence.

## 2. Current baseline

Corrective 013 closed the M003 time/history and queue-integrity defects.

The core already has:

- structural I2P-only upstream authority through `I2pStreamProvider`;
- one live owner per Network;
- explicit bounded control/normal/session/store/history queues;
- downstream overload detachment;
- bounded DesiredState reconciliation;
- durable history and restart behavior;
- per-Network exponential backoff with jitter;
- static no-generic-network boundary checks;
- generation fencing;
- a deterministic ordered-stream testkit.

Those are M004 foundations rather than work to replace.

## 3. Pre-M004 correctness prerequisite: UF-013-1

Corrective 013 records one unresolved non-blocking finding:

> live response routing is constructed and expired per generation but `ResponseRouter::route()` is never called on the client-intent path.

Current consequence:

- WHOIS/WHO/NAMES/LIST from one local client are sent upstream;
- replies return through ordinary multi-client fanout;
- the bouncer does not actually isolate query replies to the requesting SessionId;
- the M003-D route machinery is therefore not live end to end.

This is not an anonymity-policy feature, but M004's qualification strategy explicitly includes multi-client concurrent query behavior. Carrying dead correlation code into a fault campaign would make the campaign misleading.

Decision:

- register Corrective 014 as a prerequisite to M004-A/B implementation;
- reuse the existing ResponseRouter architecture;
- do not redesign one-owner-per-Network.

## 4. CTCP and DCC findings

CTCP is carried in the text body of IRC `PRIVMSG` and `NOTICE`.

DCC is initiated through CTCP and then creates direct client-to-client connections bypassing the IRC server. Its query includes a host/address and port. That conflicts directly with this project's no-alternate-egress invariant.

Current runtime behavior has no CTCP/DCC mediator. An upstream CTCP query can therefore be fanned to an attached local IRC client, which may auto-reply with its own software/version/time metadata or offer/accept DCC.

### Required M004 policy

M004 needs a parsed protocol policy rather than substring filtering.

Inbound upstream -> local-client behavior:

- CTCP ACTION: preserve as ordinary chat/action.
- CTCP PING: answer locally at the bouncer boundary with the opaque bounded token; do not fan the query to arbitrary local clients.
- CTCP DCC: suppress completely; no local actionable DCC request.
- VERSION/TIME/USERINFO/SOURCE/FINGER/CLIENTINFO: suppress by default rather than allowing an attached client to auto-answer.
- unknown CTCP: suppress by default.
- malformed CTCP: deterministic fail-closed/suppress disposition without tearing down a healthy Network unless the enclosing IRC line is itself malformed.

Downstream client -> upstream behavior:

- ACTION: permit.
- ordinary CTCP query in PRIVMSG: may be permitted when it does not itself create alternate network authority; user-controlled queries are not host-environment disclosure.
- DCC: block in both PRIVMSG and NOTICE, regardless of parameters.
- environment-bearing metadata replies in NOTICE — VERSION/TIME/USERINFO/SOURCE/FINGER/CLIENTINFO — suppress so an attached client cannot expose its own fingerprint.
- PING query/reply: permit under strict bounded opaque-token rules.
- unknown CTCP replies: default deny/suppress.
- normal non-CTCP PRIVMSG/NOTICE remains unaffected.

This distinguishes deliberate operator traffic from automatic local-client fingerprint leakage.

## 5. Client-tag findings

IRCv3 message-tags distinguishes:

- unprefixed tags, which carry server/extension-defined meaning; and
- client-only tags prefixed with `+`, which are explicitly untrusted.

The specification states that unprefixed tags received from clients must be removed before relaying unless a specific extension defines client submission behavior.

`CLIENTTAGDENY` communicates a server's client-only tag filtering policy. `CLIENTTAGDENY=*` means all client-only tags are blocked.

Current repo:

- contains `mediate_client_tags()`;
- does not call it from the live `SessionReader::translate_registered()` forwarding path;
- that helper currently tolerates client-supplied unprefixed `time` and `msgid`, which is not an acceptable default server-side policy;
- live downstream capability advertisement is split between older and newer capability surfaces and needs reconciliation before claiming message-tags semantics.

Decision for M004:

- wire tag mediation into the live path;
- remove unprefixed client tags unless a specifically reviewed extension permits them;
- begin with an empty client-only allowlist;
- advertise `CLIENTTAGDENY=*` when the downstream capability surface exposes message-tags;
- keep TAGMSG unsupported unless a useful allowed client-only tag exists; forwarding a tag-only message after stripping every tag is not meaningful;
- future client-only tags require explicit privacy/timing review.

Typing tags are a concrete example of why: they expose `active`, `paused`, and `done` timing state and therefore should not be enabled accidentally.

## 6. Upstream fingerprint invariants

The upstream capability set is already generated from one fixed reviewed policy and server offers, not from attached clients.

M004 must qualify this under:

- no client attached;
- legacy client;
- history/read-marker clients;
- several clients with different CAP requests;
- repeated downstream CAP renegotiation;
- client churn.

The exact upstream CAP request/fingerprint for the same server offer must remain identical.

The same rule applies to CTCP policy: remote-visible CTCP behavior must not depend on whether Halloy, WeeChat, irssi, or another client is currently attached.

Do not include:

- package version;
- git hash;
- OS;
- Rust version;
- router implementation/version;
- local hostname/login/path/process identifiers

in upstream-visible fixed replies.

Default metadata-query suppression is preferable to inventing a unique "i2pr-irc/x.y" fingerprint.

## 7. Environment and secret leakage

The canonical specification already prohibits environment-derived identity.

M004 should prove the negative property by poisoning the process environment with unique sentinels for common sources:

- USER/LOGNAME;
- HOSTNAME;
- HOME;
- TMPDIR;
- PATH-like local paths;
- other supported-platform equivalents where tests can set them safely.

Then exercise:

- registration;
- CAP;
- errors;
- CTCP;
- history replay;
- diagnostics;
- store errors;
- reconnect failures.

No sentinel may appear on either IRC wire or bounded diagnostics.

Also assert absence of:

- SASL password;
- AUTHENTICATE base64 payload;
- private destination/endpoint material where classified secret;
- raw SQLite error payloads containing secret/config values;
- raw protocol logs by default.

## 8. Reconnect-storm findings

Each Network currently owns independent exponential backoff.

The algorithm is individually bounded, but there is no process-wide admission control before `I2pStreamProvider::connect()`.

Current jitter entropy is also derived from generation-level state in a way that can correlate Networks with the same generation/attempt sequence.

A simultaneous router/path outage can therefore make many Networks retry at roughly the same time.

### Required architecture

Add a process-level reconnect scheduler above Network owners, most naturally owned by/alongside `NetworkCatalog`.

Conceptual shape:

~~~text
NetworkOwner
   |
request connect permit
   v
ReconnectScheduler
   +-- bounded one-waiter-per-Network set
   +-- fair admission order
   +-- max in-flight connect attempts
   +-- bounded start-rate/burst budget
   |
   v
I2pStreamProvider::connect()
~~~

All initial startup connects and reconnects pass through the same scheduler.

The scheduler is process-local and not persisted.

Per-Network exponential backoff remains. The global scheduler is an additional herd-control layer, not a replacement.

Jitter entropy must mix at least:

- NetworkId;
- ConnectionGeneration/attempt number;
- deterministic test seed/entropy source.

This keeps tests reproducible without giving every Network the same sequence.

### Failure classification

Retryable:

- provider unavailable/transient failure;
- connect timeout;
- EOF/reset;
- generation transport loss;
- other explicitly transient path failures.

Terminal-until-config-change:

- invalid durable configuration;
- authentication/SASL rejection;
- explicit server registration policy rejection where retrying unchanged credentials would only create traffic.

The scheduler must not let terminal misconfiguration consume retry budget forever.

## 9. Reconnect scheduler bounds

The production constants/config should be explicit and test-injectable.

The implementation plan should freeze reasonable conservative defaults, but M004 qualification cares about properties more than one magic number:

- in-flight connects <= configured hard ceiling;
- queued waiters <= MAX_SUPERVISED_NETWORKS;
- at most one pending waiter per Network;
- starts per interval <= configured rate/burst;
- FIFO or equivalently provable starvation-free fairness;
- stop/cancel removes a waiter;
- a permit is released on every connect completion/error/cancellation;
- startup of many Networks is budgeted;
- no busy-spin when no token/permit is available.

## 10. Adverse-network fault model

The bouncer consumes an ordered reliable byte stream supplied by the I2P transport/provider boundary.

Therefore deterministic M004 stream faults should include:

- arbitrary segmentation;
- short reads;
- short writes;
- delayed reads/writes;
- complete stalls;
- bounded backpressure;
- EOF;
- reset;
- provider unavailable;
- connect timeout;
- stale generation completion;
- delayed/missing liveness response.

Do not invent byte reordering or duplicate bytes *inside* a reliable ordered stream. Those belong below the I2P stream abstraction and would test the wrong contract.

## 11. Many-Network campaign

Qualification should include the maximum supervised-Network ceiling where practical, including simultaneous:

- startup;
- provider failure;
- reconnect;
- recovery.

Assertions:

- actual concurrent `connect()` calls never exceed global ceiling;
- attempt-start rate never exceeds budget;
- every non-terminal Network eventually receives a fair admission opportunity under virtual time;
- one terminal Network stops consuming admissions;
- one Network's backoff does not mutate another's;
- recovery does not create a second herd.

The fake provider needs global attempt instrumentation so the test proves actual concurrency rather than inferring from timestamps.

## 12. Slow-client/store combined campaigns

Corrective 013 already established:

- live downstream frame refusal => detach only that SessionId;
- history-ingest pressure => bounded best-effort drop/count;
- DesiredState queue pressure => bounded reconcile/restart;
- upstream command refusal => explicit local failure.

M004 should combine them.

Examples:

- one stopped-reading client + one healthy client + upstream burst + stalled SQLite;
- several stalled clients on several Networks during reconnect recovery;
- retention work while upstream messages arrive;
- history queue full while PING/PONG remains due;
- store mutation failure during client JOIN while other Networks remain healthy.

No new lossless buffering abstraction is required.

## 13. Crash/restart qualification

Use deterministic reopen/restart fixtures around:

- durable desired JOIN/PART commit;
- history append;
- client playback cursor;
- read marker;
- schema v2;
- retention/clamping.

Required properties:

- no stale ObservedState restored;
- committed DesiredState survives;
- history order remains by HistoryEventId;
- cursor/read marker never point to a reused event identity;
- an interrupted operation has the already-defined explicit commit-state semantics;
- no ambiguous user message is replayed automatically.

## 14. Resource-recovery evidence

M004 closure should observe bounded process state before, during, and after campaigns.

At minimum expose/test:

- Network task count;
- session task count;
- reconnect waiters;
- in-flight connects;
- store queue depth;
- upstream normal/control depths;
- session queue depths;
- response routes;
- batch count;
- desired reconcile depth;
- history ingest depth or accounted outcome.

After churn settles, counts must return to the expected live baseline.

Do not require exact allocator heap-byte measurements unless a deterministic regression harness already exists; bound owned collections/tasks/queues directly.

## 15. Static boundary hardening

The existing network-boundary script remains M004 evidence.

Strengthen positive controls so production code is rejected if it acquires:

- `tokio::net::TcpStream`;
- `std::net::TcpStream`;
- DNS resolver APIs/crates;
- HTTP client ownership;
- generic proxy/SOCKS/CONNECT machinery;
- DCC dial/listen implementation.

DCC should be impossible not merely by policy but because no production API can honor the supplied host/port.

## 16. Implementation decomposition

### Corrective 014 — live multi-client response-routing completion

Close UF-013-1.

- wire GenerationQuery through ResponseRouter;
- labeled-response translation when available;
- command-specific bounded fallback otherwise;
- deliver correlated replies only to requesting SessionId;
- clean route on failed queue admission, timeout, session detach and generation replacement;
- reconcile capability advertisement with what the live path actually implements.

### M004-A / Plan 015 — anonymity protocol mediation

- CTCP parser/policy;
- DCC structural block;
- metadata-reply filtering;
- live client-tag mediation;
- CLIENTTAGDENY policy;
- fixed upstream-visible behavior;
- environment/secret negative tests;
- stable upstream fingerprint qualification.

### M004-B / Plan 016 — global reconnect scheduling

- process-level ReconnectScheduler;
- initial-connect/reconnect admission;
- concurrency + rate budget;
- independent deterministic jitter;
- retryable/terminal failure classification;
- fair cancellation-safe bounded waiters.

M004-A and M004-B may proceed independently after Corrective 014 closes.

### M004-C / Plan 017 — adverse-network/resource qualification

- testkit attempt instrumentation;
- many-Network reconnect storms;
- combined slow-client/store/upstream faults;
- malformed/privacy-bearing protocol campaigns;
- restart/crash campaigns;
- resource steady-state evidence.

Depends on M004-A and M004-B.

### M004-D / Plan 018 — integrated anonymity qualification and closure

- final privacy/security audit;
- static boundary positive controls;
- all exit-condition matrices;
- roadmap/docs/registry reconciliation;
- M004 closure and M005 readiness decision.

## 17. Readiness

No new third-party production dependency is required by this research.

Corrective 014 is ready immediately.

Plans 015 and 016 should be registered now but blocked on Corrective 014 so M004 qualification does not build on a known multi-client correctness gap.

Plan 017 depends on both 015 and 016 closure.

Plan 018 depends on 017 closure.

If implementation of CTCP/DCC, reconnect scheduling, or routing would require replacing one-owner-per-Network supervision or widening generic network authority, stop and register a new ADR instead.
