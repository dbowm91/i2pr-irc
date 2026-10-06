# Bouncer Core M003-C — History Journal, Cursors, and Legacy Playback

Status: blocked

Blocker:

- `plans/closure/bouncer-core/008-status.md` accepted

Authority:

- ADR-0002
- Research 004

Primary class: capability

## 1. Objective

Add durable bounded message history and client synchronization on top of the many-Network/many-session owner model.

Deliver:

- durable Buffer resolution;
- HistoryEvent ingestion with local total order;
- bounded retention;
- per-ClientId playback cursors;
- per-Buffer operator read-marker storage primitive;
- bounded automatic backlog for legacy clients;
- restart/history/store-pressure semantics.

IRCv3 CHATHISTORY/MARKREAD wire commands remain M003-E.

## 2. Invariants

1. HistoryEventId/local sequence is canonical order.
2. server-time/msgid are metadata, not ordering authority.
3. history persistence never authorizes automatic message retransmission upstream.
4. store pressure cannot starve IRC control traffic.
5. client playback cursors are per ClientId + BufferId.
6. read marker is distinct and shared per Buffer for the single Operator.
7. cursors move monotonically.
8. a canceled SessionId cannot advance a cursor after detach.
9. legacy playback is bounded in event count and bytes.
10. retention has a deterministic cursor/read-marker clamping rule.

## 3. Scope

### In scope

- Buffer lookup/create;
- incoming PRIVMSG/NOTICE history initially;
- outbound own-message history policy;
- local receive wall time;
- optional upstream server-time/msgid preservation where available;
- bounded history append batch;
- bounded history queries;
- retention;
- playback cursor;
- read-marker persistence primitive;
- legacy automatic backlog;
- writer acknowledgment required before cursor advance;
- restart tests.

### Out of scope

- CHATHISTORY wire syntax;
- MARKREAD wire syntax;
- labeled-response;
- event-playback for JOIN/PART/etc.;
- FTS/search;
- history export.

## 4. History model

Each stored event includes at minimum:

- HistoryEventId;
- NetworkId;
- BufferId;
- receive wall time;
- optional server-time;
- optional upstream msgid;
- direction/audience;
- stable event class;
- bounded canonical protocol payload required for replay.

Do not persist an opaque Rust enum serialization as the durable schema.

## 5. Buffer resolution

Buffers are stable IDs scoped by NetworkId.

Initial kinds:

- Channel;
- Query/direct-message peer.

Resolve current wire targets using the negotiated casemapping, while preserving the stable BufferId after creation.

If a casemapping change makes two existing durable target identities ambiguous, fail closed/record a diagnostic rather than silently merging histories. Register a successor design if automatic durable rekey/merge is required.

## 6. Ingestion policy

Persist incoming PRIVMSG and NOTICE first.

A message observed from upstream is eligible for one canonical history insert after wire validation and buffer resolution.

Deduplication may use upstream msgid where present, but msgid is not globally trusted as the local primary key. Any dedup index is Network-scoped.

### Own outgoing messages

Preferred policy:

- if upstream echo-message is available later, upstream echo is canonical confirmed history;
- before M003-D, local outgoing PRIVMSG/NOTICE may be stored only with explicit unconfirmed/unknown delivery semantics, or omitted until upstream observation confirms it.

The implementation plan must choose one minimal policy and never label local write completion as confirmed upstream delivery.

## 7. Store pressure

Network owner submits history work through a bounded path and continues servicing control traffic.

On queue full/store failure:

- increment bounded/store-health diagnostics;
- do not block PING/PONG indefinitely;
- do not create an unbounded retry buffer;
- do not advance any client cursor for an event not durably available.

Tests should use a deliberately stalled/failing store fixture.

## 8. Cursor semantics

### Playback cursor

Key: `(ClientId, BufferId)`.

Value: highest HistoryEventId successfully delivered via automatic legacy playback.

Cursor updates use monotonic compare-and-advance semantics.

Playback batch must obtain writer completion/ack before advancing.

A process crash between socket write and cursor commit may duplicate on restart; duplication is preferred to a gap.

### Read marker primitive

Key: BufferId for the one Operator.

Value: monotonic HistoryEventId.

No IRCv3 wire behavior in this plan.

## 9. Legacy playback

After downstream registration/current-state projection, for each applicable Buffer:

- query after the durable client cursor;
- cap events and total bytes;
- emit oldest-to-newest by HistoryEventId;
- do not overflow the session queue;
- advance cursor only after acknowledged writer completion.

A future chathistory-negotiating client must be able to suppress this path; reserve the session capability hook now.

## 10. Retention

Implement configurable bounded retention and a hard safety ceiling.

Delete in bounded chunks.

When retention removes the cursor's target event, cursor/read marker is monotonically clamped to a documented retained boundary so future query semantics remain deterministic.

Retention must not hold the store worker in one arbitrarily large transaction.

## 11. Work packages

A. Buffer model/resolution;
B. HistoryEvent append/query;
C. store-pressure/error disposition;
D. playback/read-marker primitives;
E. session-writer delivery acknowledgment;
F. legacy backlog;
G. retention/restart qualification.

## 12. Failure/restart

History append failure degrades history only; it does not falsify network delivery.

Playback failure/detach leaves cursor at the last acknowledged event.

Restart preserves history/cursors/read marker and reconstructs no live ObservedState.

Store corruption/incompatible schema remains fatal at startup under M003-A.

## 13. Tests

- deterministic HistoryEventId order with identical timestamps;
- restart preserves history order;
- server-time skew/collision does not reorder;
- msgid preserved;
- Buffer stable lookup under casemapping;
- ambiguous mapping does not merge;
- store queue saturation while PING/PONG remains serviced;
- cursor monotonicity;
- detach mid-playback cannot advance beyond acked event;
- crash-window simulation prefers duplicate over gap;
- two ClientIds have independent cursors;
- read marker independent from playback cursor;
- retention bounded and clamping deterministic;
- legacy replay event/byte limits;
- no automatic upstream replay introduced.

## 14. Verification/docs

Full verification/Rust 1.88 plus storage restart fixtures. Add history/cursor architecture and `plans/closure/bouncer-core/009-status.md`.

## 15. Acceptance criteria

Durable bounded history works across restart, per-client legacy playback is independently synchronized, store pressure cannot starve network control traffic, and no draft IRCv3 syntax is embedded as schema authority.

## 16. Stop conditions

Stop if Buffer identity under casemapping cannot be made deterministic without a new durable identity ADR, or if history write acknowledgment requires blocking the NetworkSupervisor read/control loop.

## 17. Closure evidence

Record schema additions/indexes, event-order proof, buffer policy, cursor/read-marker matrix, retention behavior, store-pressure liveness, restart evidence, verification, and M003-D readiness in `plans/closure/bouncer-core/009-status.md`.
