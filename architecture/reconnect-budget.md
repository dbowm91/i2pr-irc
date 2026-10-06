# Reconnect budget

Per-Network exponential backoff bounds one Network's retry rate. It does nothing about the
case that actually hurts on a router restart: every Network notices the outage at the same
moment and every Network retries at the same moment. A reconnect herd is a self-inflicted
egress spike, and it is indistinguishable from an attack on the operator's own router.

`crate::reconnect` adds a process-wide second gate in front of that backoff.

## Shape

A `ReconnectScheduler` owns a token bucket and a FIFO waiter queue.

- `MAX_IN_FLIGHT_CONNECTS = 4` — at most four connection attempts may be in flight across
  every Network at once.
- `MAX_CONNECT_BURST = 4` — the bucket's burst capacity. A cold start admits a small group
  immediately rather than serialising every Network behind one probe.
- `CONNECT_TOKEN_INTERVAL = 2s` — one token is returned every two seconds, so the
  sustained rate is bounded no matter how many Networks failed.
- `MAX_RECONNECT_WAITERS = 64` — the queue is bounded. A waiter that cannot be admitted is
  refused rather than retained, so a mass outage cannot grow an unbounded structure.

Admission is a second gate, not a replacement. A Network must still wait out its own
backoff first; the scheduler only decides which eligible Network goes next among those.

## Fairness and cancellation

The queue is FIFO, so a Network that has waited longest connects first and no Network can
starve behind a repeatedly-failing one. Each Network holds at most one waiter position: a
Network that is already queued does not queue again, which is what stops a fast-failing
Network from monopolising the queue.

A `ConnectPermit` is released by `Drop`. Cancellation therefore cannot leak admission: a
Network whose owner is stopped mid-wait, or whose connect future is aborted, releases its
slot without needing to be told. This matters because every cancellation path is an early
return, and a manually-maintained release would be missed on exactly the paths that are
hardest to test.

`classify` turns an attempt outcome into a decision: a terminal failure is not retried
through the budget at all, so a permanently refused endpoint cannot consume a slot or a
token forever.

Jitter is derived from a per-scheduler entropy source rather than the clock, so two
Networks that failed together still diverge and the bucket's bounds hold even when the
timer source is coarse.

## Relationship to backoff

`Backoff` remains per-Network and per-attempt. The scheduler is process-wide. Removing the
scheduler would not reintroduce an unbounded retry: backoff alone is bounded and correct.
Removing backoff and keeping the scheduler would bound the rate but let a single Network
retry too fast. Both are needed, and they answer different questions — backoff bounds *how
often one Network retries*, the scheduler bounds *how many Networks retry at once*.

See `reconnect-and-liveness.md` for the backoff and liveness detail, and
`resource-accounts.md` for the gauges this scheduler publishes.