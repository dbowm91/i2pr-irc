# Bouncer Core M003-E — IRCv3 Chathistory and Read-Marker Adapters

Status: closed — see `plans/closure/bouncer-core/011-status.md`

Blocker cleared:

- `plans/closure/bouncer-core/010-status.md` accepted

Authority:

- ADR-0002
- Research 004
- current primary IRCv3 specifications at implementation time

Primary class: capability

## 1. Objective

Expose the durable generic history/cursor model through isolated current IRCv3 history/read-state adapters without allowing draft syntax to shape the database.

Deliver:

- bounded CHATHISTORY queries;
- history BATCH emission;
- draft/read-marker MARKREAD mapping;
- no-duplicate legacy versus chathistory playback;
- explicit draft/version capability isolation;
- target/query indexes and limits.

## 2. Invariants

1. Database schema identity remains HistoryEventId/BufferId based.
2. Draft command/capability syntax is adapter code only.
3. All history queries have event and byte ceilings.
4. Results are ordered by local HistoryEventId with server-time/msgid preserved as metadata.
5. Read marker moves only forward.
6. Read state is private to this Operator's local clients.
7. A chathistory-negotiating client does not also receive the same automatic legacy backlog.
8. No history query blocks network liveness.
9. Unknown/stale history references fail deterministically.
10. Current draft semantics are version-labelled/documented.

## 3. Scope

Implement the current reviewed draft/chathistory surface needed by ordinary modern clients, preferably:

- LATEST;
- BEFORE;
- AFTER;
- BETWEEN;
- AROUND if bounded index support is ready;
- TARGETS if bounded target enumeration is ready.

Implement the current read-marker capability/command semantics over the durable Buffer read marker.

Do not implement event-playback unless required to make the chosen history payload truthful; otherwise constrain stored replay payload to message event types the negotiated capability set can represent.

## 4. Capability isolation

Centralize capability names/version assumptions in one adapter module.

Do not scatter `draft/...` literals through store/runtime code.

At startup/build documentation, identify which spec revision the adapter implements.

If the draft changes incompatibly, a future wire adapter update should not require schema migration unless semantic data requirements changed.

## 5. Reference resolution

Translate IRCv3 message references into generic store query keys.

Use upstream msgid where available and supported; use timestamp reference parsing according to the current spec.

Resolve ties through local HistoryEventId.

Never expose HistoryEventId directly as an upstream-derived msgid unless an explicit local message-id scheme is separately designed.

## 6. Query execution

History query path:

~~~text
DownstreamSession
  -> typed history request
  -> StoreHandle bounded query
  -> bounded HistoryEvent result
  -> IRCv3 adapter
  -> bounded BATCH/session queue
~~~

A slow query/session cannot create unbounded pending results.

Apply both count and byte caps.

## 7. CHATHISTORY output

Emit replay lines with truthful:

- target;
- message type;
- server-time (upstream value or stored local receive time according to policy);
- msgid only when one is valid/preserved;
- tags allowed by downstream capability/anonymity policy.

Use BATCH framing as required.

Do not include JOIN/PART/NICK/etc. unless event-playback semantics are explicitly implemented.

## 8. Legacy duplication prevention

Session capability state determines one initial synchronization mode:

- legacy client -> bounded automatic backlog using playback cursor;
- chathistory client -> query-driven history, no automatic duplicate backlog.

Changing CAP state after registration must not replay the same initial history twice.

Document cursor behavior for chathistory clients; the legacy playback cursor need not advance merely because a CHATHISTORY query was issued unless the product explicitly treats query delivery as playback state.

## 9. Read marker

Map current MARKREAD input to BufferId and a resolved HistoryEventId.

Monotonic update:

~~~text
new_marker = max(old_marker, resolved_event)
~~~

Report current marker according to the current adapter semantics.

Never move backwards.

A marker referencing pruned history clamps/resolves according to M003-C retention semantics.

Do not send read state upstream.

## 10. Work packages

A. versioned capability adapter;
B. reference parser/resolver;
C. CHATHISTORY typed query mapping;
D. BATCH replay emission;
E. legacy/chathistory synchronization switch;
F. MARKREAD/read-marker adapter;
G. draft isolation/conformance tests.

## 11. Failure/restart

History query failure returns an explicit local error/FAIL according to supported standard-reply semantics; it does not tear down upstream.

Session detach cancels query delivery; store request/result remains bounded.

Restart preserves history/read marker because storage semantics are independent of draft syntax.

## 12. Tests

- every implemented CHATHISTORY subcommand;
- count max/max+1;
- byte limit;
- equal timestamps deterministic;
- missing/unknown msgid;
- pruned reference;
- BATCH formation;
- no legacy duplicate when chathistory negotiated;
- legacy client still receives bounded automatic playback;
- MARKREAD forward;
- MARKREAD backward ignored/clamped;
- two ClientIds see same operator read marker but independent playback cursors;
- read marker survives restart;
- spec capability string is isolated in adapter;
- draft capability disabled cleanly if adapter feature/policy is off.

## 13. Verification/docs

Run full conformance and Rust 1.88. Add history-protocol architecture and `plans/closure/bouncer-core/011-status.md`.

## 14. Acceptance criteria

Modern clients can query bounded durable history and synchronize read state without duplicate legacy replay, while schema semantics remain independent of draft wire syntax.

## 15. Stop conditions

Stop if the current draft changed materially from Research 004 assumptions; refresh protocol research first. Stop if event-playback becomes required for correctness beyond PRIVMSG/NOTICE and register a bounded extension rather than broadening silently.

## 16. Closure evidence

Record exact spec revisions, capability/query matrix, bounds, reference-resolution behavior, duplicate-prevention matrix, read-marker monotonic/restart evidence, verification, and M003-F readiness in `plans/closure/bouncer-core/011-status.md`.
