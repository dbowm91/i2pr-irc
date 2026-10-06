# Network supervisor

`NetworkSupervisor::serve` owns one upstream Network across connection generations. It owns provider attempts, the connection-generation counter, backoff, and the upstream generation actor. It does not own any local client: a `LocalAcceptor` supplies at most one disposable downstream view at a time, and the generation actor handles attachment as data.

```
NetworkSupervisor::serve                         (owns generations + backoff)
└── run_generation(upstream, generation, acceptor, stop)
    ├── upstream read half        owner loop      registration, liveness, observed state
    ├── upstream write half       owner loop      CAP/SASL + welcome (registration only)
    └── upstream writer task      JoinSet         control(8) + normal(64) bounded queues

    LocalAcceptor (zero-or-one at any instant)
    ├── accepted  -> DownstreamSession { read half, decoder, bounded queues }
    │                                  SessionWriter { exit signal, JoinHandle }
    └── detached  -> session + writer aborted and joined; generation continues
```

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

Consequences: a downstream client attaching mid-flight cannot receive a synthetic JOIN for an unconfirmed channel, a refused join is visible as bounded non-secret diagnostics (`pending_joins` / `rejected_joins` in the snapshot) rather than as a silent lie, and no same-generation retry loop exists. A failed attempt is an observation about one generation; the next generation re-attempts the same operator intent after its own registration, which is the behaviour persistence will need when M003 stores intent separately from live membership.

The owner loop uses separate bounded control and normal output queues (8 and 64 frames). The normal queue carries typed `OutboundIntent` values tagged with their generation; the writer discards any intent whose generation does not match its owner. The writer always selects control before queued normal traffic. A full upstream queue is an explicit failure that ends the generation; a full downstream queue ends only that client. Commands are never silently dropped or retained for replay.

`subscribe_snapshot` exposes phase, generation, current nick and joined channels, reconnect attempt and delay, last error class, upstream and downstream queue depths, downstream attachment state, cumulative attached-session and detach counters, the most recent detach disposition, and the upstream event count. It does not expose raw errors, message payloads, endpoints, or credentials. Both counters are monotonic so a later attach cannot hide an earlier detach.

Configuration validation bounds identities, channels, and SASL credential sizes before the runtime starts. Desired channels are validated conservatively against `#` and `&` before connecting; live channel classification follows the server-advertised `CHANTYPES`. Snapshots and diagnostics never include credential values or payloads.
