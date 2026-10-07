# Network supervisor

## Which implementation is live

The Network owner that ships is `owner::NetworkOwner`, driven by `catalog::NetworkSupervisor` as one supervised task per durable `NetworkId`. It accepts typed `SupervisorCommand::Attach` intents and may hold several `DownstreamSession`s at once, each with its own bounded queues and writer task.

`lib.rs::NetworkSupervisor` is the superseded first-generation owner. Corrective 019 gated it behind `#[cfg(test)]`, so it is absent from the production build and its ungated upstream connect can no longer be reached from the shipped public API. It is retained only so its qualification suite keeps compiling. Nothing below about `LocalAcceptor` describes the shipping path.

## Generations, ownership, and teardown

`NetworkOwner::serve` owns one upstream Network across connection generations. It owns provider attempts, the connection-generation counter, backoff, and the upstream generation actor. Attachment is data the generation handles, never a precondition for it.

Each attempt passes the owner's own `NetworkId` to `I2pStreamProvider::connect`, under the existing process-wide reconnect permit. Generation churn never releases the provider scope: the router identity belongs to the durable Network, and releasing it on every failed attempt would churn the identity on every outage. Release happens only on deletion and shutdown, and only through `RuntimeController`. See [provider scope and release](network-boundary.md#provider-scope-and-release).

```
NetworkOwner::serve                               (owns generations + backoff)
└── run_generation(upstream, generation, commands, stop)
    ├── upstream read half        owner loop      registration, liveness, observed state
    ├── upstream write half       owner loop      CAP/SASL + welcome (registration only)
    └── upstream writer task      JoinSet         control(8) + normal(64) bounded queues

    downstream sessions (zero or more, per network)
    ├── attached  -> DownstreamSession { read half, decoder, bounded queues }
    │                                   SessionWriter { exit signal, JoinHandle }
    └── detached  -> session + writer aborted and joined; generation continues
```

The `LocalAcceptor` shape in the legacy supervisor is deliberately not drawn: it accepted
at most one disposable downstream view, which is why that owner was superseded.

Registration happens with no client attached. The generation reaches `Online` on its own, so local-client absence can never prevent or delay upstream registration. Each attempt receives a monotonically increasing `ConnectionGeneration`, which tags outbound intents and liveness tokens; a failed generation is discarded before another provider attempt.

Observed state (`NetworkState`) lives inside the generation actor, so it is retained across every downstream attach and detach and reset only when a generation is discarded. Upstream and downstream stream halves, the decoder, and the session writer task are owned by the generation; nothing is detached. On exit the generation drops the session, aborts and joins the session writer task, then either joins the upstream writer after the final QUIT or aborts it immediately.

## Desired attempt versus observed membership

Three kinds of channel knowledge are kept apart and are never promoted into each other:

| Kind | Owner | Lifetime | Meaning |
|---|---|---|---|
| `desired_channels` | operator configuration | durable across generations | channels the operator asked the bouncer to be on |
| `join_attempts` | one connection generation | discarded when the generation is replaced | a JOIN that was written upstream and is still `Pending`, or that the server rejected with a standard channel-failure numeric |
| `self_channels` | one connection generation | observed only | membership the server has confirmed, normally by echoing a self JOIN |

Writing `JOIN #chan` records a `Pending` attempt and nothing else; command emission is not evidence. A self JOIN whose nick matches the negotiated casemapping is the normal positive authority: it moves the channel into `self_channels`, ensures its `ChannelState` exists within the channel ceiling, and clears the attempt. A self PART or KICK removes observed membership and clears the attempt again. The standard channel-failure numerics `403`, `405`, `471`, `473`, `474`, `475`, and `476` classify a refused attempt, but only after the reply's parameter shape yields a plausible channel; the channel never enters `self_channels`, and `desired_channels` is left untouched. Network-specific numerics stay ordinary server events because nothing specified here can classify them.

Consequences: a downstream client attaching mid-flight cannot receive a synthetic JOIN for an unconfirmed channel, a refused join is visible as bounded non-secret diagnostics (`pending_joins` / `rejected_joins` in the snapshot) rather than as a silent lie, and no same-generation retry loop exists. A failed attempt is an observation about one generation; the next generation re-attempts the same operator intent after its own registration. Durable intent is stored separately from live membership, and it is committed *before* the upstream bytes exist, so a crash can never leave an upstream JOIN without a durable record.

The owner loop uses separate bounded control and normal output queues (8 and 64 frames). The normal queue carries typed `OutboundIntent` values tagged with their generation; the writer discards any intent whose generation does not match its owner. The writer always selects control before queued normal traffic. A refused upstream enqueue is an explicit failure, never a silent drop. An ordinary client command is reported to its originating session and counted in `upstream_rejected`; it is never retried, because it was rejected before admission and a later generation could not know whether writing it again would duplicate it. Committed desired membership is different — the durable intent stands, so it converges through a bounded reconciliation set that carries only channel names, and a generation that cannot hold another entry is deliberately restarted.

A full downstream queue ends that one attachment rather than skipping a frame for it, because an ordered IRC stream cannot be repaired after a gap. The refusal is counted in `fanout_dropped` and the detach in `fanout_detached`; no other client and not the upstream is affected. No client command is ever silently dropped and none is ever retained for replay.

`subscribe_snapshot` exposes phase, generation, current nick and joined channels, reconnect attempt and delay, last error class, upstream and downstream queue depths, downstream attachment state, cumulative attached-session and detach counters, the most recent detach disposition, the upstream event count, bounded history and fanout counters, refused upstream commands, and the deferred desired-membership reconciliation depth. It does not expose raw errors, message payloads, endpoints, or credentials. Both counters are monotonic so a later attach cannot hide an earlier detach.

Configuration validation bounds identities, channels, and SASL credential sizes before the runtime starts. Desired channels are validated conservatively against `#` and `&` before connecting; live channel classification follows the server-advertised `CHANTYPES`. Snapshots and diagnostics never include credential values or payloads.
