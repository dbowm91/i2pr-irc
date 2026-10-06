# Identity model

The bouncer keeps durable identity and live attachment identity strictly apart. Conflating them is how a late reply ends up delivered to the wrong connection.

| Identity | Scope | Durable | Answers |
|---|---|---|---|
| `NetworkId` | one upstream network | yes | which network |
| `ClientId` | one client lineage | yes | which lineage owns playback/read state |
| `BufferId` | one buffer within a network | yes | which conversation |
| `HistoryEventId` | one retained event | yes | canonical history position |
| `SessionId` | one live attachment | **no** | which connection owns CAP state, queues, and routes |
| `ConnectionGeneration` | one upstream connection | **no** | which upstream attempt this is |

## SessionId versus ClientId

`ClientId` is durable lineage: it survives detach and reattach, and it owns the playback cursor and read state. `SessionId` is allocated locally at attachment time from a bounded allocator, is never persisted, and is never restored.

A `ClientId` that reconnects always receives a fresh `SessionId`. That is the mechanism that prevents a late reply scoped to a previous attachment from being delivered to its replacement — matching `ClientId` is not sufficient grounds for delivery.

Allocation never wraps. Once `MAX_SESSION_IDS` is reached it reports exhaustion instead, because a reused `SessionId` would make an old response route indistinguishable from a current one.

## Wall time versus monotonic time

`Clock` is monotonic and drives reconnect backoff and liveness, where a jump would be a defect. `WallClock` is civil time and supplies a durable history event's receive timestamp, where monotonic time is useless because it does not survive a restart. They are separate interfaces so neither property is weakened.

`WallTime` is bounded to a representable window and validated with unsigned comparison, so a corrupt durable row fails validation instead of overflowing.

Canonical history order is `HistoryEventId`, never a timestamp. `received_at` and any upstream `server-time` are metadata: skewed, missing, or repeated values cannot reorder retained history.